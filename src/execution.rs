use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::signers::Signer as _;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use polymarket_client_sdk_v2::POLYGON;
use polymarket_client_sdk_v2::auth::Normal;
use polymarket_client_sdk_v2::auth::state::Authenticated;
use polymarket_client_sdk_v2::bridge::Client as BridgeClient;
use polymarket_client_sdk_v2::bridge::types::DepositRequest;
use polymarket_client_sdk_v2::clob::types::request::{
    BalanceAllowanceRequest, CancelMarketOrderRequest, OrdersRequest, UpdateBalanceAllowanceRequest,
};
use polymarket_client_sdk_v2::clob::types::{AssetType, OrderType, Side, SignatureType};
use polymarket_client_sdk_v2::clob::{Client as ClobClient, Config as ClobConfig};
use polymarket_client_sdk_v2::types::{Address, B256, Decimal, U256};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::info;

use crate::book::{
    calculate_green_up, estimate_builder_fee_usd, estimate_protocol_fee_usd, GreenUpQuote,
    SweepQuote,
};
use crate::config::{AppConfig, HARDCODED_BUILDER_CODE};
use crate::market::{MarketClient, SiblingOutcome};
use crate::paper::{PaperEngine, PaperFill, PaperOrder, PaperOrderParams, PaperPosition, PaperStateSnapshot};
use crate::telemetry::{BuilderFeeRecord, Telemetry};
use crate::vault::{
    GeneratedWallet, VaultMetadata, VaultSecret, WalletSignatureType, default_vault_path,
    delete_vault, generate_local_wallet, load_encrypted_vault, parse_key_or_mnemonic,
    save_encrypted_vault, vault_metadata,
};
use crate::ws_feed::MarketFeed;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TradingMode {
    Paper,
    Live,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    pub asset_id: String,
    pub condition_id: String,
    pub market_question: String,
    pub outcome: String,
    pub side: String,       // "BUY" | "SELL"
    pub order_type: String, // "GTC" | "GTD" | "FAK" | "FOK" | "TIMED_FOK" | "SWEEP"
    pub post_only: bool,
    pub price: f64,
    pub size: f64, // shares
    pub expiration_secs: Option<i64>,
    pub timed_fok_ms: Option<u64>,
    pub max_slippage_bps: Option<f64>,
    pub fees_enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaleEntryRequest {
    pub asset_id: String,
    pub condition_id: String,
    pub market_question: String,
    pub outcome: String,
    pub side: String,
    pub start_price: f64,
    pub end_price: f64,
    pub total_size: f64,
    pub steps: usize,
    pub post_only: bool,
    pub fees_enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderAck {
    pub mode: TradingMode,
    pub order_id: String,
    pub status: String,
    pub side: String,
    pub price: f64,
    pub size: f64,
    pub filled_size: f64,
    pub sign_us: f64,
    pub rtt_ms: f64,
    pub builder_fee_bps: u32,
    pub estimated_builder_fee_usd: f64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositAddressesInfo {
    pub target_wallet: String,
    pub evm_address: String,
    #[serde(default)]
    pub tron_address: Option<String>,
    pub solana_address: String,
    pub btc_address: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountState {
    pub mode: TradingMode,
    pub live_connected: bool,
    pub wallet_address: Option<String>,
    pub funder_address: Option<String>,
    pub signature_type: Option<WalletSignatureType>,
    pub vault: VaultMetadata,
    pub live_balance_usd: Option<f64>,
    pub builder_code: String,
    pub builder_maker_bps: u32,
    pub builder_taker_bps: u32,
    pub builder_fee_locked: bool,
    pub heartbeats_enabled: bool,
    pub paper: PaperStateSnapshot,
    pub live_open_orders: Vec<PaperOrder>,
    pub live_positions: Vec<PaperPosition>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecEvent {
    AccountUpdated,
    OrderAck { ack: OrderAck },
    Fill { fill: PaperFill },
}

struct LiveSession {
    client: ClobClient<Authenticated<Normal>>,
    signer: PrivateKeySigner,
    wallet_address: String,
    funder_address: Option<String>,
    signature_type: WalletSignatureType,
    builder_code: Option<B256>,
    builder_maker_bps: u32,
    builder_taker_bps: u32,
    heartbeats_enabled: bool,
}

pub struct ExecutionEngine {
    config: Arc<RwLock<AppConfig>>,
    config_path: Option<PathBuf>,
    vault_path: PathBuf,
    mode: RwLock<TradingMode>,
    paper: Arc<PaperEngine>,
    live: tokio::sync::RwLock<Option<Arc<LiveSession>>>,
    public_clob: RwLock<Option<ClobClient<polymarket_client_sdk_v2::auth::state::Unauthenticated>>>,
    market_client: Arc<MarketClient>,
    feed: Arc<MarketFeed>,
    telemetry: Arc<Telemetry>,
    events_tx: broadcast::Sender<ExecEvent>,
    live_orders_cache: RwLock<Vec<PaperOrder>>,
    live_positions_cache: RwLock<Vec<PaperPosition>>,
    live_balance_cache: RwLock<Option<f64>>,
}

impl ExecutionEngine {
    pub fn new(
        config: Arc<RwLock<AppConfig>>,
        config_path: Option<PathBuf>,
        market_client: Arc<MarketClient>,
        feed: Arc<MarketFeed>,
        telemetry: Arc<Telemetry>,
    ) -> Arc<Self> {
        let starting_bal = config.read().paper_starting_balance;
        let clob_host = config.read().clob_host.clone();
        let public_clob = ClobClient::new(&clob_host, ClobConfig::default()).ok();
        let (events_tx, _) = broadcast::channel(512);

        let vault_path = config_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|dir| dir.join("vault.json"))
            .unwrap_or_else(default_vault_path);

        let engine = Arc::new(Self {
            config,
            config_path,
            vault_path,
            mode: RwLock::new(TradingMode::Paper),
            paper: Arc::new(PaperEngine::new(starting_bal)),
            live: tokio::sync::RwLock::new(None),
            public_clob: RwLock::new(public_clob),
            market_client,
            feed,
            telemetry,
            events_tx,
            live_orders_cache: RwLock::new(Vec::new()),
            live_positions_cache: RwLock::new(Vec::new()),
            live_balance_cache: RwLock::new(None),
        });

        // Spawn persistent HTTP/2 keep-alive pre-warmer so TCP+TLS stays hot to clob.polymarket.com
        let eng_warm = engine.clone();
        tokio::spawn(async move {
            eng_warm.sync_public_builder_fee_rate().await;
            eng_warm.run_keepalive_loop().await;
        });

        // Spawn paper resting-order fill matcher fed directly from MarketFeed WS ticks
        let eng_paper = engine.clone();
        tokio::spawn(async move {
            let mut rx = eng_paper.feed.subscribe_events();
            loop {
                match rx.recv().await {
                    Ok(crate::ws_feed::FeedEvent::Book { view }) => {
                        let (maker_bps, taker_bps, bcode) = {
                            let cfg = eng_paper.config.read();
                            (cfg.builder_maker_bps, cfg.builder_taker_bps, cfg.builder_code.clone())
                        };
                        let fills = eng_paper.paper.on_market_tick(
                            &view.asset_id,
                            view.best_bid,
                            view.best_ask,
                            view.last_trade_price,
                            maker_bps,
                            taker_bps,
                        );
                        if !fills.is_empty() {
                            for f in fills {
                                eng_paper.telemetry.inc_order_filled();
                                eng_paper.telemetry.record_fee(BuilderFeeRecord {
                                    id: f.id.clone(),
                                    timestamp_ms: f.timestamp_ms,
                                    mode: "paper".into(),
                                    market_question: f.market_question.clone(),
                                    side: f.side.clone(),
                                    notional_usd: f.notional_usd,
                                    is_maker: f.is_maker,
                                    protocol_fee_usd: f.protocol_fee_usd,
                                    builder_fee_bps: f.builder_fee_bps,
                                    builder_fee_usd: f.builder_fee_usd,
                                    builder_code: bcode.clone(),
                                });
                                let _ = eng_paper.events_tx.send(ExecEvent::Fill { fill: f });
                            }
                            let _ = eng_paper.events_tx.send(ExecEvent::AccountUpdated);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                    _ => {}
                }
            }
        });

        // Continuous 4-second live account sync so ALL open orders, positions, and balances
        // placed from ANY device (iPad, Windows, Linux, or polymarket.com) appear automatically!
        let eng_sync = engine.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(4));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                if eng_sync.live.read().await.is_some() {
                    let _ = eng_sync.refresh_live_account().await;
                }
            }
        });

        engine
    }

    /// Fetch the live on-chain Builder Fee schedule directly from Polymarket CLOB at startup
    /// so the displayed Maker/Taker fee always matches your Polymarket Builder Dashboard automatically.
    async fn sync_public_builder_fee_rate(&self) {
        let host = self.config.read().clob_host.clone();
        let bcode_clean = HARDCODED_BUILDER_CODE;
        let url = format!("{}/fees/builder-fees/{bcode_clean}", host.trim_end_matches('/'));
        if let Ok(resp) = self.market_client.http().get(&url).send().await
            && resp.status().is_success()
            && let Ok(val) = resp.json::<serde_json::Value>().await
        {
            let m_bps = val
                .get("builder_maker_fee_rate_bps")
                .and_then(|v| v.as_u64())
                .map(|v| (v as u32).min(50));
            let t_bps = val
                .get("builder_taker_fee_rate_bps")
                .and_then(|v| v.as_u64())
                .map(|v| (v as u32).min(100));
            if let (Some(m), Some(t)) = (m_bps, t_bps) {
                {
                    let mut cfg = self.config.write();
                    cfg.builder_maker_bps = m;
                    cfg.builder_taker_bps = t;
                }
                info!(builder_code = bcode_clean, maker_bps = m, taker_bps = t, "Synced live Polymarket CLOB V2 builder fee schedule");
                let _ = self.events_tx.send(ExecEvent::AccountUpdated);
            }
        }
    }

    async fn run_keepalive_loop(self: Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_secs(12));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let host = self.config.read().clob_host.clone();
            let url = format!("{}/time", host.trim_end_matches('/'));
            let t0 = Instant::now();
            if let Ok(resp) = self.market_client.http().get(&url).send().await
                && resp.status().is_success()
            {
                let _ = resp.bytes().await;
                self.telemetry
                    .record_warm_ping_ms(t0.elapsed().as_secs_f64() * 1000.0);
            }
            // Also keep authenticated CLOB client's reqwest pool warm if live session exists
            let live_opt = self.live.read().await.clone();
            if let Some(sess) = live_opt {
                let t1 = Instant::now();
                if sess.client.server_time().await.is_ok() {
                    self.telemetry
                        .record_warm_ping_ms(t1.elapsed().as_secs_f64() * 1000.0);
                }
            }
        }
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<ExecEvent> {
        self.events_tx.subscribe()
    }

    pub fn mode(&self) -> TradingMode {
        *self.mode.read()
    }

    pub fn set_mode(&self, mode: TradingMode) -> Result<()> {
        if mode == TradingMode::Live {
            // Check asynchronously isn't needed if we just verify via try_read
            let has_live = self
                .live
                .try_read()
                .map(|g| g.is_some())
                .unwrap_or(false);
            if !has_live {
                return Err(anyhow!(
                    "Unlock or connect a wallet vault before switching to LIVE mode"
                ));
            }
        }
        *self.mode.write() = mode;
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        Ok(())
    }

    pub fn reset_paper(&self, balance: f64) {
        self.paper.reset(balance);
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
    }

    /// Pre-warm SDK DashMap caches (tick_size, neg_risk, fee_rate_bps) immediately upon
    /// selecting a market so order signing requires ZERO extra REST round-trips!
    pub async fn prewarm_market(&self, token_ids: &[String]) {
        let parsed: Vec<U256> = token_ids
            .iter()
            .filter_map(|s| U256::from_str(s).ok())
            .collect();
        if parsed.is_empty() {
            return;
        }
        let live_opt = self.live.read().await.clone();
        if let Some(sess) = live_opt {
            for tid in parsed {
                let c = sess.client.clone();
                tokio::spawn(async move {
                    let _ = tokio::join!(
                        c.tick_size(tid),
                        c.neg_risk(tid),
                        c.fee_rate_bps(tid),
                    );
                });
            }
        } else if let Some(pub_c) = self.public_clob.read().clone() {
            for tid in parsed {
                let c = pub_c.clone();
                tokio::spawn(async move {
                    let _ = tokio::join!(c.tick_size(tid), c.neg_risk(tid));
                });
            }
        }
    }

    /// Generate a brand-new random local BIP-39 wallet (12-word secret phrase + private key + Polygon address)
    /// completely on-device, encrypt it into the local vault, unlock Polymarket CLOB V2,
    /// and immediately fetch direct USDT/USDC Polymarket Bridge deposit addresses for it.
    pub async fn create_and_unlock_new_wallet(
        &self,
        password: String,
        persist_vault: bool,
    ) -> Result<(GeneratedWallet, AccountState, Option<DepositAddressesInfo>)> {
        if persist_vault && password.len() < 6 {
            return Err(anyhow!("Vault passphrase must be at least 6 characters"));
        }
        let generated = generate_local_wallet()?;
        let signer = PrivateKeySigner::from_str(&generated.private_key)
            .context("Generated private key invalid")?
            .with_chain_id(Some(POLYGON));

        if persist_vault {
            let hint = format!(
                "{}...{}",
                &generated.address[..6],
                &generated.address[generated.address.len() - 4..]
            );
            let secret = VaultSecret {
                private_key: generated.private_key.clone(),
                mnemonic: generated.mnemonic.clone(),
                funder: None,
                signature_type: WalletSignatureType::Eoa,
                label: Some("Local Generated".into()),
            };
            save_encrypted_vault(&self.vault_path, &password, &secret, &hint)?;
        }

        self.authenticate_live(signer, None, WalletSignatureType::Eoa)
            .await?;
        let deposit = self.fetch_deposit_addresses(&generated.address).await.ok();
        let acc = self.account_state().await;
        Ok((generated, acc, deposit))
    }

    /// Decrypt the local encrypted vault (requiring user's vault passphrase) so they can
    /// re-view / back up their on-device private key & 12-word secret phrase offline.
    pub fn reveal_vault_backup(&self, password: &str) -> Result<GeneratedWallet> {
        let secret = load_encrypted_vault(&self.vault_path, password)?;
        let signer = PrivateKeySigner::from_str(secret.private_key.trim())
            .context("Vault contains invalid private key")?;
        Ok(GeneratedWallet {
            address: format!("{:#x}", signer.address()),
            private_key: secret.private_key.clone(),
            mnemonic: secret.mnemonic.clone(),
        })
    }

    pub async fn save_and_unlock_wallet(
        &self,
        private_key_or_phrase: String,
        funder: Option<String>,
        signature_type: WalletSignatureType,
        password: String,
        persist_vault: bool,
    ) -> Result<AccountState> {
        let (pk_clean, mnemonic_opt) = parse_key_or_mnemonic(&private_key_or_phrase)?;
        let signer = PrivateKeySigner::from_str(&pk_clean)
            .context("Invalid Polygon private key hex")?
            .with_chain_id(Some(POLYGON));
        let wallet_addr = format!("{:#x}", signer.address());

        let funder_clean = funder
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        if persist_vault {
            let hint = format!(
                "{}...{}",
                &wallet_addr[..6],
                &wallet_addr[wallet_addr.len() - 4..]
            );
            let secret = VaultSecret {
                private_key: pk_clean.clone(),
                mnemonic: mnemonic_opt,
                funder: funder_clean.clone(),
                signature_type,
                label: Some("Primary".into()),
            };
            save_encrypted_vault(&self.vault_path, &password, &secret, &hint)?;
        }

        self.authenticate_live(signer, funder_clean, signature_type)
            .await?;
        Ok(self.account_state().await)
    }

    pub async fn unlock_existing_vault(&self, password: &str) -> Result<AccountState> {
        let secret = load_encrypted_vault(&self.vault_path, password)?;
        let signer = PrivateKeySigner::from_str(secret.private_key.trim())
            .context("Vault contains invalid private key")?
            .with_chain_id(Some(POLYGON));
        self.authenticate_live(signer, secret.funder.clone(), secret.signature_type)
            .await?;
        Ok(self.account_state().await)
    }

    pub async fn lock_wallet(&self) -> AccountState {
        *self.live.write().await = None;
        *self.mode.write() = TradingMode::Paper;
        self.live_orders_cache.write().clear();
        self.live_positions_cache.write().clear();
        *self.live_balance_cache.write() = None;
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        self.account_state().await
    }

    pub async fn wipe_vault(&self) -> Result<AccountState> {
        delete_vault(&self.vault_path)?;
        Ok(self.lock_wallet().await)
    }

    async fn authenticate_live(
        &self,
        signer: PrivateKeySigner,
        funder: Option<String>,
        sig_type: WalletSignatureType,
    ) -> Result<()> {
        let (clob_host, cfg_maker, cfg_taker, heartbeats) = {
            let cfg = self.config.read();
            (
                cfg.clob_host.clone(),
                cfg.builder_maker_bps,
                cfg.builder_taker_bps,
                cfg.enable_heartbeats,
            )
        };

        // Always enforce hardcoded Dronepilot Builder Code — non-overridable!
        let parsed_bcode = Some(
            B256::from_str(HARDCODED_BUILDER_CODE)
                .expect("HARDCODED_BUILDER_CODE must be valid 32-byte hex"),
        );

        let clob_cfg = match (parsed_bcode, heartbeats) {
            (Some(bc), true) => ClobConfig::builder()
                .use_server_time(true)
                .builder_code(bc)
                .heartbeat_interval(Duration::from_secs(5))
                .build(),
            (Some(bc), false) => ClobConfig::builder()
                .use_server_time(true)
                .builder_code(bc)
                .build(),
            (None, true) => ClobConfig::builder()
                .use_server_time(true)
                .heartbeat_interval(Duration::from_secs(5))
                .build(),
            (None, false) => ClobConfig::builder().use_server_time(true).build(),
        };

        let unauth = ClobClient::new(&clob_host, clob_cfg)
            .context("Failed creating Polymarket CLOB V2 client")?;

        let sdk_sig_type = match sig_type {
            WalletSignatureType::Eoa => SignatureType::Eoa,
            WalletSignatureType::Proxy => SignatureType::Proxy,
            WalletSignatureType::GnosisSafe => SignatureType::GnosisSafe,
            WalletSignatureType::Poly1271 => SignatureType::Poly1271,
        };

        let mut auth_builder = unauth
            .authentication_builder(&signer)
            .signature_type(sdk_sig_type);

        if let Some(ref f) = funder {
            let addr = Address::from_str(f).context("Invalid funder/proxy address")?;
            auth_builder = auth_builder.funder(addr);
        }

        let client = auth_builder
            .authenticate()
            .await
            .context("Polymarket L1/L2 CLOB authentication failed")?;

        // Query live on-chain Builder Fee rate if builder code is configured!
        let (mut maker_bps, mut taker_bps) = (cfg_maker, cfg_taker);
        if let Some(bc) = parsed_bcode {
            if let Ok(rate) = client.builder_fee_rate(bc).await {
                maker_bps = rate.builder_maker_fee_rate_bps;
                taker_bps = rate.builder_taker_fee_rate_bps;
                info!(
                    builder_code = %bc,
                    maker_bps,
                    taker_bps,
                    "Loaded live Polymarket V2 builder fee schedule"
                );
            }
        }

        let wallet_address = format!("{:#x}", signer.address());
        let sess = Arc::new(LiveSession {
            client,
            signer,
            wallet_address: wallet_address.clone(),
            funder_address: funder,
            signature_type: sig_type,
            builder_code: parsed_bcode,
            builder_maker_bps: maker_bps,
            builder_taker_bps: taker_bps,
            heartbeats_enabled: heartbeats,
        });

        *self.live.write().await = Some(sess.clone());
        *self.mode.write() = TradingMode::Live;

        // Refresh live balance, open orders & positions in background
        let _ = self.refresh_live_account().await;
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        info!(wallet = %wallet_address, "Live Polymarket CLOB V2 wallet authenticated");
        Ok(())
    }

    pub async fn refresh_live_account(&self) -> Result<()> {
        let Some(sess) = self.live.read().await.clone() else {
            return Ok(());
        };

        // 1. Balance & allowance (collateral pUSD / USDC)
        let bal_req = BalanceAllowanceRequest::builder()
            .asset_type(AssetType::Collateral)
            .build();
        if let Ok(bal) = sess.client.balance_allowance(bal_req).await {
            // Polymarket CLOB returns collateral (pUSD / USDC.e) in 6-decimal base units (1_000_000 = $1.00)
            let normalized = if bal.balance.scale() == 0 {
                bal.balance.to_f64().unwrap_or(0.0) / 1_000_000.0
            } else {
                bal.balance.to_f64().unwrap_or(0.0)
            };
            *self.live_balance_cache.write() = Some(normalized);
        }

        // 2. Open orders
        if let Ok(page) = sess.client.orders(&OrdersRequest::default(), None).await {
            let mapped: Vec<PaperOrder> = page
                .data
                .into_iter()
                .map(|o| {
                    let orig = o.original_size.to_f64().unwrap_or(0.0);
                    let matched = o.size_matched.to_f64().unwrap_or(0.0);
                    PaperOrder {
                        id: o.id,
                        asset_id: o.asset_id.to_string(),
                        condition_id: format!("{:#x}", o.market),
                        market_question: o.outcome.clone(),
                        outcome: o.outcome,
                        side: match o.side {
                            Side::Buy => "BUY".into(),
                            _ => "SELL".into(),
                        },
                        order_type: format!("{:?}", o.order_type),
                        post_only: false,
                        price: o.price.to_f64().unwrap_or(0.0),
                        original_size: orig,
                        remaining_size: (orig - matched).max(0.0),
                        filled_size: matched,
                        avg_fill_price: o.price.to_f64().unwrap_or(0.0),
                        status: format!("{:?}", o.status).to_uppercase(),
                        created_at_ms: o.created_at.timestamp_millis(),
                        expires_at_ms: None,
                        fees_enabled: true,
                    }
                })
                .collect();
            *self.live_orders_cache.write() = mapped;
        }

        // 3. Positions from Data API v2
        let query_addr = sess
            .funder_address
            .as_deref()
            .unwrap_or(&sess.wallet_address);
        if let Ok(pos_vals) = self.market_client.fetch_wallet_positions(query_addr).await {
            let mut positions = Vec::new();
            for pv in pos_vals {
                let shares = pv
                    .get("current_size")
                    .or_else(|| pv.get("size"))
                    .and_then(|x| x.as_f64())
                    .unwrap_or(0.0);
                if shares <= 1e-6 {
                    continue;
                }
                positions.push(PaperPosition {
                    asset_id: pv
                        .get("token_id")
                        .or_else(|| pv.get("asset"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    condition_id: pv
                        .get("condition_id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    market_question: pv
                        .get("title")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    outcome: pv
                        .get("outcome")
                        .and_then(|x| x.as_str())
                        .unwrap_or("Yes")
                        .to_string(),
                    shares,
                    avg_entry: pv
                        .get("avg_price")
                        .and_then(|x| x.as_f64())
                        .unwrap_or(0.0),
                    realized_pnl: pv
                        .get("realized_pnl")
                        .and_then(|x| x.as_f64())
                        .unwrap_or(0.0),
                });
            }
            *self.live_positions_cache.write() = positions;
        }

        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        Ok(())
    }

    pub async fn account_state(&self) -> AccountState {
        let cfg = self.config.read().clone();
        let live_opt = self.live.read().await.clone();
        let mode = *self.mode.read();

        let (live_connected, wallet_address, funder_address, signature_type, maker_bps, taker_bps, hb) =
            if let Some(s) = live_opt {
                (
                    true,
                    Some(s.wallet_address.clone()),
                    s.funder_address.clone(),
                    Some(s.signature_type),
                    s.builder_maker_bps,
                    s.builder_taker_bps,
                    s.heartbeats_enabled,
                )
            } else {
                (
                    false,
                    None,
                    None,
                    None,
                    cfg.builder_maker_bps,
                    cfg.builder_taker_bps,
                    cfg.enable_heartbeats,
                )
            };

        AccountState {
            mode,
            live_connected,
            wallet_address,
            funder_address,
            signature_type,
            vault: vault_metadata(&self.vault_path),
            live_balance_usd: *self.live_balance_cache.read(),
            builder_code: cfg.builder_code.clone(),
            builder_maker_bps: maker_bps,
            builder_taker_bps: taker_bps,
            builder_fee_locked: cfg.is_builder_locked(),
            heartbeats_enabled: hb,
            paper: self.paper.snapshot(),
            live_open_orders: self.live_orders_cache.read().clone(),
            live_positions: self.live_positions_cache.read().clone(),
        }
    }

    pub fn get_position(&self, asset_id: &str) -> Option<PaperPosition> {
        match self.mode() {
            TradingMode::Paper => self.paper.position(asset_id),
            TradingMode::Live => self
                .live_positions_cache
                .read()
                .iter()
                .find(|p| p.asset_id == asset_id && p.shares > 1e-6)
                .cloned(),
        }
    }

    pub fn preview_sweep(&self, asset_id: &str, is_buy: bool, usd: Option<f64>, shares: Option<f64>, limit_price: Option<f64>) -> Option<SweepQuote> {
        let book = self.feed.get_book_clone(asset_id)?;
        Some(book.simulate_sweep(is_buy, usd, shares, limit_price))
    }

    pub fn preview_green_up(&self, asset_id: &str, opposite_asset_id: Option<&str>) -> Option<GreenUpQuote> {
        let pos = self.get_position(asset_id)?;
        let book = self.feed.get_book_clone(asset_id)?;
        let best_bid = book.best_bid().or(book.last_trade_price)?;
        let opp_ask = opposite_asset_id
            .and_then(|oid| self.feed.get_book_clone(oid))
            .and_then(|b| b.best_ask());
        Some(calculate_green_up(pos.shares, pos.avg_entry, best_bid, opp_ask))
    }

    /// Place a single order (GTC, GTD, FAK, FOK, Post-Only, Timed-FOK, or Sweep).
    pub async fn place_order(self: &Arc<Self>, req: OrderRequest) -> Result<OrderAck> {
        let mode = self.mode();
        let otype = req.order_type.to_uppercase();
        let is_buy = req.side.eq_ignore_ascii_case("BUY");

        // Handle SWEEP: compute worst acceptable price across live L2 book and submit aggressive FAK
        let mut effective = req.clone();
        if otype == "SWEEP" {
            if let Some(book) = self.feed.get_book_clone(&req.asset_id) {
                let max_slip = req.max_slippage_bps.unwrap_or(500.0) / 10_000.0;
                let ref_px = if is_buy {
                    book.best_ask().unwrap_or(req.price)
                } else {
                    book.best_bid().unwrap_or(req.price)
                };
                let cap = if is_buy {
                    (ref_px * (1.0 + max_slip)).min(0.999)
                } else {
                    (ref_px * (1.0 - max_slip)).max(0.001)
                };
                let quote = book.simulate_sweep(is_buy, None, Some(req.size), Some(cap));
                if quote.worst_price > 0.0 {
                    effective.price = align_to_tick(quote.worst_price, book.tick_size_ticks);
                }
            }
            effective.order_type = "FAK".into();
            effective.post_only = false;
        }

        // Handle TIMED_FOK: place GTC and schedule exact millisecond auto-cancel
        let timed_fok_ms = if otype == "TIMED_FOK" {
            effective.order_type = "GTC".into();
            Some(req.timed_fok_ms.unwrap_or(1500).clamp(150, 60_000))
        } else {
            None
        };

        let ack = match mode {
            TradingMode::Paper => self.place_order_paper(&effective)?,
            TradingMode::Live => self.place_order_live(&effective).await?,
        };

        if let Some(ms) = timed_fok_ms
            && (ack.status == "OPEN" || ack.status == "LIVE" || ack.status == "PARTIAL")
        {
            let eng = self.clone();
            let oid = ack.order_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                let _ = eng.cancel_order(&oid).await;
            });
        }

        let _ = self.events_tx.send(ExecEvent::OrderAck { ack: ack.clone() });
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        Ok(ack)
    }

    fn place_order_paper(&self, req: &OrderRequest) -> Result<OrderAck> {
        let t0 = Instant::now();
        let (maker_bps, taker_bps, bcode) = {
            let cfg = self.config.read();
            (cfg.builder_maker_bps, cfg.builder_taker_bps, cfg.builder_code.clone())
        };
        let book = self.feed.get_book_clone(&req.asset_id);
        let expires_at_ms = req
            .expiration_secs
            .filter(|&s| s > 0)
            .map(|s| Utc::now().timestamp_millis() + s * 1000);

        let res = self.paper.place_order(
            PaperOrderParams {
                asset_id: req.asset_id.clone(),
                condition_id: req.condition_id.clone(),
                market_question: req.market_question.clone(),
                outcome: req.outcome.clone(),
                side: req.side.clone(),
                order_type: req.order_type.clone(),
                post_only: req.post_only,
                price: req.price,
                size: req.size,
                expires_at_ms,
                fees_enabled: req.fees_enabled.unwrap_or(true),
                builder_maker_bps: maker_bps,
                builder_taker_bps: taker_bps,
            },
            book.as_ref(),
        )?;

        let elapsed_us = t0.elapsed().as_secs_f64() * 1_000_000.0;
        self.telemetry.inc_order_sent();
        self.telemetry.record_sign_us(elapsed_us);
        self.telemetry.record_order_rtt_ms(elapsed_us / 1000.0);

        let mut total_bfee = 0.0;
        for f in res.new_fills {
            total_bfee += f.builder_fee_usd;
            self.telemetry.inc_order_filled();
            self.telemetry.record_fee(BuilderFeeRecord {
                id: f.id.clone(),
                timestamp_ms: f.timestamp_ms,
                mode: "paper".into(),
                market_question: f.market_question.clone(),
                side: f.side.clone(),
                notional_usd: f.notional_usd,
                is_maker: f.is_maker,
                protocol_fee_usd: f.protocol_fee_usd,
                builder_fee_bps: f.builder_fee_bps,
                builder_fee_usd: f.builder_fee_usd,
                builder_code: bcode.clone(),
            });
            let _ = self.events_tx.send(ExecEvent::Fill { fill: f });
        }

        let is_maker = res.order.filled_size <= 1e-6;
        let b_bps = if is_maker { maker_bps } else { taker_bps };
        if total_bfee == 0.0 {
            total_bfee = estimate_builder_fee_usd(req.price * req.size, is_maker, maker_bps, taker_bps);
        }

        Ok(OrderAck {
            mode: TradingMode::Paper,
            order_id: res.order.id,
            status: res.order.status,
            side: res.order.side,
            price: res.order.price,
            size: res.order.original_size,
            filled_size: res.order.filled_size,
            sign_us: elapsed_us,
            rtt_ms: elapsed_us / 1000.0,
            builder_fee_bps: b_bps,
            estimated_builder_fee_usd: total_bfee,
            error: None,
        })
    }

    async fn place_order_live(&self, req: &OrderRequest) -> Result<OrderAck> {
        let sess = self
            .live
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow!("Live wallet not unlocked"))?;

        let token_id = U256::from_str(&req.asset_id).context("Invalid token_id U256")?;
        let side = if req.side.eq_ignore_ascii_case("BUY") {
            Side::Buy
        } else {
            Side::Sell
        };
        let order_type = match req.order_type.to_uppercase().as_str() {
            "FOK" => OrderType::FOK,
            "FAK" => OrderType::FAK,
            "GTD" => OrderType::GTD,
            _ => OrderType::GTC,
        };

        let price_dec = Decimal::from_f64(req.price)
            .ok_or_else(|| anyhow!("Invalid price"))?
            .round_dp(4)
            .normalize();
        let size_dec = Decimal::from_f64(req.size)
            .ok_or_else(|| anyhow!("Invalid size"))?
            .round_dp(2)
            .normalize();

        let t_sign = Instant::now();
        let mut builder = sess
            .client
            .limit_order()
            .token_id(token_id)
            .side(side)
            .price(price_dec)
            .size(size_dec)
            .order_type(order_type.clone());

        if req.post_only && matches!(order_type, OrderType::GTC | OrderType::GTD) {
            builder = builder.post_only(true);
        }
        if matches!(order_type, OrderType::GTD) {
            let exp_secs = req.expiration_secs.unwrap_or(300).max(65);
            let exp_dt: DateTime<Utc> = Utc::now() + chrono::Duration::seconds(exp_secs);
            builder = builder.expiration(exp_dt);
        }
        if let Some(bc) = sess.builder_code {
            builder = builder.builder_code(bc);
        }

        let signable = builder.build().await.context("Failed building V2 order")?;
        let signed = sess
            .client
            .sign(&sess.signer, signable)
            .await
            .context("Failed EIP-712 order signing")?;
        let sign_us = t_sign.elapsed().as_secs_f64() * 1_000_000.0;
        self.telemetry.record_sign_us(sign_us);

        let t_post = Instant::now();
        let resp = sess
            .client
            .post_order(signed)
            .await
            .context("CLOB POST /order failed")?;
        let rtt_ms = t_post.elapsed().as_secs_f64() * 1000.0;
        self.telemetry.inc_order_sent();
        self.telemetry.record_order_rtt_ms(rtt_ms);

        let notional = req.price * req.size;
        let is_maker = matches!(order_type, OrderType::GTC | OrderType::GTD) && req.post_only;
        let b_bps = if is_maker {
            sess.builder_maker_bps
        } else {
            sess.builder_taker_bps
        };
        let b_fee = estimate_builder_fee_usd(notional, is_maker, sess.builder_maker_bps, sess.builder_taker_bps);
        let p_fee = estimate_protocol_fee_usd(req.size, req.price, is_maker, req.fees_enabled.unwrap_or(true));

        if resp.success {
            self.telemetry.record_fee(BuilderFeeRecord {
                id: resp.order_id.clone(),
                timestamp_ms: Utc::now().timestamp_millis(),
                mode: "live".into(),
                market_question: req.market_question.clone(),
                side: req.side.to_uppercase(),
                notional_usd: notional,
                is_maker,
                protocol_fee_usd: p_fee,
                builder_fee_bps: b_bps,
                builder_fee_usd: b_fee,
                builder_code: sess.builder_code.map(|b| format!("{b:#x}")).unwrap_or_default(),
            });
        }

        // Refresh open orders asynchronously after fill/placement
        let _ = self.refresh_live_account().await;

        Ok(OrderAck {
            mode: TradingMode::Live,
            order_id: resp.order_id,
            status: format!("{:?}", resp.status).to_uppercase(),
            side: req.side.to_uppercase(),
            price: req.price,
            size: req.size,
            filled_size: resp.taking_amount.to_f64().unwrap_or(0.0),
            sign_us,
            rtt_ms,
            builder_fee_bps: b_bps,
            estimated_builder_fee_usd: b_fee,
            error: resp.error_msg,
        })
    }

    /// Scale Entry: distribute N limit orders evenly across [start_price .. end_price].
    pub async fn place_scale_entry(self: &Arc<Self>, req: ScaleEntryRequest) -> Result<Vec<OrderAck>> {
        let steps = req.steps.clamp(2, 15);
        let tick_ticks = self
            .feed
            .get_book_clone(&req.asset_id)
            .map(|b| b.tick_size_ticks)
            .unwrap_or(100);
        let clip = (req.total_size / (steps as f64) * 100.0).floor() / 100.0;
        if clip < 1.0 {
            return Err(anyhow!("Clip size per rung ({clip:.2}) is too small"));
        }

        let mut acks = Vec::with_capacity(steps);
        for i in 0..steps {
            let frac = (i as f64) / ((steps - 1) as f64);
            let raw_p = req.start_price + (req.end_price - req.start_price) * frac;
            let p = align_to_tick(raw_p, tick_ticks);
            let ack = self
                .place_order(OrderRequest {
                    asset_id: req.asset_id.clone(),
                    condition_id: req.condition_id.clone(),
                    market_question: req.market_question.clone(),
                    outcome: req.outcome.clone(),
                    side: req.side.clone(),
                    order_type: "GTC".into(),
                    post_only: req.post_only,
                    price: p,
                    size: clip,
                    expiration_secs: None,
                    timed_fok_ms: None,
                    max_slippage_bps: None,
                    fees_enabled: req.fees_enabled,
                })
                .await?;
            acks.push(ack);
        }
        Ok(acks)
    }

    /// One-click Green-Up (instant cash-out / free-roll principal recovery / opposite hedge).
    pub async fn execute_green_up(
        self: &Arc<Self>,
        asset_id: &str,
        opposite_asset_id: Option<&str>,
        strategy: &str, // "close" | "free_roll" | "hedge_opposite"
    ) -> Result<OrderAck> {
        let pos = self
            .get_position(asset_id)
            .ok_or_else(|| anyhow!("No open position for this outcome"))?;
        let quote = self
            .preview_green_up(asset_id, opposite_asset_id)
            .ok_or_else(|| anyhow!("No live orderbook available for Green-Up"))?;

        match strategy {
            "free_roll" => {
                if !quote.can_free_roll || quote.free_roll_shares_to_sell < 1.0 {
                    return Err(anyhow!("Position is not in profit enough for a Free-Roll"));
                }
                self.place_order(OrderRequest {
                    asset_id: asset_id.to_string(),
                    condition_id: pos.condition_id,
                    market_question: pos.market_question,
                    outcome: pos.outcome,
                    side: "SELL".into(),
                    order_type: "SWEEP".into(),
                    post_only: false,
                    price: quote.close_price,
                    size: quote.free_roll_shares_to_sell,
                    expiration_secs: None,
                    timed_fok_ms: None,
                    max_slippage_bps: Some(300.0),
                    fees_enabled: Some(true),
                })
                .await
            }
            "hedge_opposite" => {
                let opp_id = opposite_asset_id
                    .ok_or_else(|| anyhow!("Opposite outcome token ID required"))?;
                let opp_ask = quote
                    .hedge_no_price
                    .ok_or_else(|| anyhow!("No live ask on opposite outcome"))?;
                self.place_order(OrderRequest {
                    asset_id: opp_id.to_string(),
                    condition_id: pos.condition_id,
                    market_question: pos.market_question,
                    outcome: "Opposite Hedge".into(),
                    side: "BUY".into(),
                    order_type: "SWEEP".into(),
                    post_only: false,
                    price: opp_ask,
                    size: pos.shares,
                    expiration_secs: None,
                    timed_fok_ms: None,
                    max_slippage_bps: Some(300.0),
                    fees_enabled: Some(true),
                })
                .await
            }
            _ => {
                self.place_order(OrderRequest {
                    asset_id: asset_id.to_string(),
                    condition_id: pos.condition_id,
                    market_question: pos.market_question,
                    outcome: pos.outcome,
                    side: "SELL".into(),
                    order_type: "SWEEP".into(),
                    post_only: false,
                    price: quote.close_price,
                    size: pos.shares,
                    expiration_secs: None,
                    timed_fok_ms: None,
                    max_slippage_bps: Some(500.0),
                    fees_enabled: Some(true),
                })
                .await
            }
        }
    }

    /// Sibling-Market Auto-Hedge / Dutch-Book: buys `shares` of every selected sibling outcome!
    pub async fn execute_sibling_hedge(
        self: &Arc<Self>,
        outcomes: &[SiblingOutcome],
        shares: f64,
    ) -> Result<Vec<OrderAck>> {
        if outcomes.len() < 2 {
            return Err(anyhow!("Need at least 2 sibling outcomes to auto-hedge"));
        }
        let mut acks = Vec::with_capacity(outcomes.len());
        for o in outcomes {
            let px = o.best_ask.unwrap_or(o.yes_price).clamp(0.001, 0.999);
            let ack = self
                .place_order(OrderRequest {
                    asset_id: o.yes_token_id.clone(),
                    condition_id: o.condition_id.clone(),
                    market_question: o.question.clone(),
                    outcome: o.group_title.clone(),
                    side: "BUY".into(),
                    order_type: "FAK".into(),
                    post_only: false,
                    price: px,
                    size: shares,
                    expiration_secs: None,
                    timed_fok_ms: None,
                    max_slippage_bps: Some(150.0),
                    fees_enabled: Some(true),
                })
                .await?;
            acks.push(ack);
        }
        Ok(acks)
    }

    pub async fn cancel_order(&self, order_id: &str) -> Result<usize> {
        let count = match self.mode() {
            TradingMode::Paper => usize::from(self.paper.cancel_order(order_id)),
            TradingMode::Live => {
                let sess = self
                    .live
                    .read()
                    .await
                    .clone()
                    .ok_or_else(|| anyhow!("Live wallet not unlocked"))?;
                let res = sess.client.cancel_order(order_id).await?;
                let _ = self.refresh_live_account().await;
                res.canceled.len()
            }
        };
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        Ok(count)
    }

    pub async fn cancel_at_price(&self, asset_id: &str, side: Option<&str>, price: f64) -> Result<usize> {
        let count = match self.mode() {
            TradingMode::Paper => self.paper.cancel_at_price(asset_id, side, price),
            TradingMode::Live => {
                let ids: Vec<String> = self
                    .live_orders_cache
                    .read()
                    .iter()
                    .filter(|o| {
                        o.asset_id == asset_id
                            && (o.price - price).abs() < 5e-5
                            && side.is_none_or(|sd| o.side.eq_ignore_ascii_case(sd))
                    })
                    .map(|o| o.id.clone())
                    .collect();
                if ids.is_empty() {
                    0
                } else {
                    let sess = self
                        .live
                        .read()
                        .await
                        .clone()
                        .ok_or_else(|| anyhow!("Live wallet not unlocked"))?;
                    let refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
                    let res = sess.client.cancel_orders(&refs).await?;
                    let _ = self.refresh_live_account().await;
                    res.canceled.len()
                }
            }
        };
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        Ok(count)
    }

    pub async fn cancel_market(&self, asset_id: &str) -> Result<usize> {
        let count = match self.mode() {
            TradingMode::Paper => self.paper.cancel_market(asset_id),
            TradingMode::Live => {
                let sess = self
                    .live
                    .read()
                    .await
                    .clone()
                    .ok_or_else(|| anyhow!("Live wallet not unlocked"))?;
                let tid = U256::from_str(asset_id).context("Invalid asset_id")?;
                let req = CancelMarketOrderRequest::builder().asset_id(tid).build();
                let res = sess.client.cancel_market_orders(&req).await?;
                let _ = self.refresh_live_account().await;
                res.canceled.len()
            }
        };
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        Ok(count)
    }

    pub async fn cancel_all(&self) -> Result<usize> {
        let count = match self.mode() {
            TradingMode::Paper => self.paper.cancel_all(),
            TradingMode::Live => {
                let sess = self
                    .live
                    .read()
                    .await
                    .clone()
                    .ok_or_else(|| anyhow!("Live wallet not unlocked"))?;
                let res = sess.client.cancel_all_orders().await?;
                let _ = self.refresh_live_account().await;
                res.canceled.len()
            }
        };
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        Ok(count)
    }

    /// Fetch official Polymarket Bridge deposit addresses (EVM USDT/USDC, Tron TRC-20 USDT, Solana, BTC)
    /// that automatically convert deposited USDT / USDC / crypto 1:1 into Polygon pUSD for `wallet_addr`.
    pub async fn fetch_deposit_addresses(&self, wallet_addr: &str) -> Result<DepositAddressesInfo> {
        let addr = Address::from_str(wallet_addr.trim())
            .context("Invalid Polygon wallet address (expected 0x...)")?;
        let target_hex = format!("{addr:#x}");

        // Query Polymarket Bridge directly with X-Builder-Code so we receive ALL chains including Tron TRC-20 USDT!
        if let Ok(resp) = self
            .market_client
            .http()
            .post("https://bridge.polymarket.com/deposit")
            .header("X-Builder-Code", HARDCODED_BUILDER_CODE)
            .json(&serde_json::json!({ "address": &target_hex }))
            .send()
            .await
            && resp.status().is_success()
            && let Ok(val) = resp.json::<serde_json::Value>().await
            && let Some(a) = val.get("address")
        {
            let evm = a.get("evm").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let svm = a.get("svm").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let btc = a.get("btc").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let tron = a
                .get("tron")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            if !evm.is_empty() {
                return Ok(DepositAddressesInfo {
                    target_wallet: target_hex,
                    evm_address: evm,
                    tron_address: tron,
                    solana_address: svm,
                    btc_address: btc,
                    note: val.get("note").and_then(|v| v.as_str()).map(|s| s.to_string()),
                });
            }
        }

        // Fallback to official SDK BridgeClient
        let client = BridgeClient::default();
        let req = DepositRequest::builder().address(addr).build();
        let resp = client
            .deposit(&req)
            .await
            .context("Polymarket Bridge deposit address request failed")?;
        Ok(DepositAddressesInfo {
            target_wallet: target_hex,
            evm_address: format!("{:#x}", resp.address.evm),
            tron_address: None,
            solana_address: resp.address.svm,
            btc_address: resp.address.btc,
            note: resp.note,
        })
    }

    /// Ask Polymarket CLOB to re-index on-chain collateral balance & ERC-20 allowance immediately
    /// after depositing real pUSD / USDC.e.
    pub async fn sync_balance_allowance(&self) -> Result<AccountState> {
        let sess = self
            .live
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow!("Unlock a live wallet first"))?;
        let req = UpdateBalanceAllowanceRequest::builder()
            .asset_type(AssetType::Collateral)
            .build();
        sess.client
            .update_balance_allowance(req)
            .await
            .context("CLOB update_balance_allowance failed")?;
        self.refresh_live_account().await?;
        Ok(self.account_state().await)
    }

    pub fn update_config_and_save<F: FnOnce(&mut AppConfig)>(&self, f: F) -> Result<AppConfig> {
        let mut cfg = self.config.write();
        f(&mut cfg);
        cfg.sanitize();
        cfg.save(self.config_path.as_ref())?;
        self.feed.set_whale_threshold(cfg.whale_threshold_usd);
        let cloned = cfg.clone();
        drop(cfg);
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        Ok(cloned)
    }
}

pub fn align_to_tick(price: f64, tick_size_ticks: usize) -> f64 {
    let step = tick_size_ticks.max(1) as f64;
    let t = ((price * 10_000.0) / step).round() * step;
    (t / 10_000.0).clamp(0.0001, 0.9999)
}
