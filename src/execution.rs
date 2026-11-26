use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
use polymarket_client_sdk_v2::clob::types::response::OpenOrderResponse;
use polymarket_client_sdk_v2::clob::types::{AssetType, OrderStatusType, OrderType, Side, SignatureType};
use polymarket_client_sdk_v2::clob::{Client as ClobClient, Config as ClobConfig};
use polymarket_client_sdk_v2::types::{Address, B256, Decimal, U256};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{info, warn};
use zeroize::Zeroize;

use crate::book::{
    calculate_green_up, estimate_builder_fee_usd, estimate_protocol_fee_usd, GreenUpQuote,
    SweepQuote,
};
use crate::config::{AppConfig, HARDCODED_BUILDER_CODE};
use crate::ctf::{
    CtfAction, CtfRequest, CtfTxResult, ensure_eoa_for_direct_ctf, execute_ctf_live,
};
use crate::market::{MarketClient, SiblingOutcome};
use crate::paper::{PaperEngine, PaperFill, PaperOrder, PaperOrderParams, PaperPosition, PaperStateSnapshot};
use crate::telemetry::{BuilderFeeRecord, Telemetry};
use crate::vault::{
    DEFAULT_VAULT_PROFILE, GeneratedWallet, MIN_NEW_VAULT_PASSWORD_LEN, VaultMetadata,
    VaultProfileEntry, VaultSecret, WalletSignatureType, delete_vault_profile,
    generate_local_wallet, list_vault_profiles, load_encrypted_vault,
    migrate_legacy_single_vault, parse_key_or_mnemonic,
    rename_vault_profile as vault_rename_profile, save_encrypted_vault, validate_profile_name,
    vault_metadata, vault_path_for_profile,
};
use crate::ws_feed::MarketFeed;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TradingMode {
    #[default]
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
    #[serde(default)]
    pub max_notional_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchOrderReport {
    pub placed: Vec<OrderAck>,
    pub failed: usize,
    pub errors: Vec<String>,
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
    pub active_vault_profile: String,
    pub vault_profiles: Vec<VaultProfileEntry>,
    pub live_balance_usd: Option<f64>,
    pub builder_code: String,
    pub builder_maker_bps: u32,
    pub builder_taker_bps: u32,
    pub builder_fee_locked: bool,
    pub heartbeats_enabled: bool,
    pub paper: PaperStateSnapshot,
    pub live_open_orders: Vec<PaperOrder>,
    pub live_positions: Vec<PaperPosition>,
    pub live_orders_warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecEvent {
    AccountUpdated,
    ModeChanged { mode: TradingMode },
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
    base_dir: PathBuf,
    active_profile: RwLock<String>,
    ctf_in_flight: AtomicBool,
    order_gen: AtomicU64,
    in_flight_live_orders: AtomicUsize,
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
    live_cancel_warning: RwLock<Option<String>>,
}

struct InFlightLiveOrderGuard<'a>(&'a AtomicUsize);

impl Drop for InFlightLiveOrderGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl ExecutionEngine {
    pub fn new(
        config: Arc<RwLock<AppConfig>>,
        config_path: Option<PathBuf>,
        market_client: Arc<MarketClient>,
        feed: Arc<MarketFeed>,
        telemetry: Arc<Telemetry>,
    ) -> Arc<Self> {
        let (starting_bal, clob_host, active_profile_raw) = {
            let c = config.read();
            (
                c.paper_starting_balance,
                c.clob_host.clone(),
                c.active_vault_profile.clone(),
            )
        };
        let public_clob = ClobClient::new(&clob_host, ClobConfig::default()).ok();
        let (events_tx, _) = broadcast::channel(512);

        let base_dir = config_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(PathBuf::from)
            .unwrap_or_else(AppConfig::config_dir);
        if let Err(e) = migrate_legacy_single_vault(&base_dir) {
            warn!("Legacy single-vault migration failed (will retry on next launch): {e}");
        }
        let active_profile = validate_profile_name(&active_profile_raw)
            .unwrap_or_else(|_| DEFAULT_VAULT_PROFILE.to_string());

        let engine = Arc::new(Self {
            config,
            config_path,
            base_dir,
            active_profile: RwLock::new(active_profile),
            ctf_in_flight: AtomicBool::new(false),
            order_gen: AtomicU64::new(1),
            in_flight_live_orders: AtomicUsize::new(0),
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
            live_cancel_warning: RwLock::new(None),
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

    pub fn active_vault_profile(&self) -> String {
        self.active_profile.read().clone()
    }

    fn current_vault_path(&self) -> PathBuf {
        let prof = self.active_profile.read().clone();
        vault_path_for_profile(&self.base_dir, &prof).unwrap_or_else(|_| {
            vault_path_for_profile(&self.base_dir, DEFAULT_VAULT_PROFILE)
                .unwrap_or_else(|_| self.base_dir.join("vaults").join("default.json"))
        })
    }

    fn enter_live_order_barrier(&self, captured_gen: u64) -> Result<InFlightLiveOrderGuard<'_>> {
        self.in_flight_live_orders.fetch_add(1, Ordering::SeqCst);
        let guard = InFlightLiveOrderGuard(&self.in_flight_live_orders);
        if self.order_gen.load(Ordering::SeqCst) != captured_gen
            || self.mode() != TradingMode::Live
        {
            return Err(anyhow!(
                "Live order aborted: mode or wallet state changed"
            ));
        }
        Ok(guard)
    }

    async fn fence_and_drain_live_orders(&self) -> Result<()> {
        self.order_gen.fetch_add(1, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let n = self.in_flight_live_orders.load(Ordering::SeqCst);
            if n == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "Timed out waiting for {n} in-flight live order(s) to finish before cancel"
                ));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn prepare_live_exit(&self, action_label: &str) -> Result<()> {
        if let Err(e) = self.fence_and_drain_live_orders().await {
            let msg = format!("LIVE ORDERS MAY STILL BE ACTIVE ON POLYMARKET: {e}");
            *self.live_cancel_warning.write() = Some(msg);
            let _ = self.events_tx.send(ExecEvent::AccountUpdated);
            return Err(anyhow!(
                "Refused {action_label} — LIVE ORDERS MAY STILL BE ACTIVE ON POLYMARKET: {e}"
            ));
        }
        if let Err(e) = self.cancel_all_live_and_verify().await {
            let msg = format!("LIVE ORDERS MAY STILL BE ACTIVE ON POLYMARKET: {e}");
            *self.live_cancel_warning.write() = Some(msg);
            let _ = self.events_tx.send(ExecEvent::AccountUpdated);
            return Err(anyhow!(
                "Refused {action_label} — LIVE ORDERS MAY STILL BE ACTIVE ON POLYMARKET: {e}"
            ));
        }
        Ok(())
    }

    pub fn mode(&self) -> TradingMode {
        *self.mode.read()
    }

    pub async fn set_mode(&self, mode: TradingMode) -> Result<()> {
        let cur = *self.mode.read();
        if cur == mode {
            return Ok(());
        }
        if mode == TradingMode::Live {
            if self.live.read().await.is_none() {
                return Err(anyhow!(
                    "Unlock or connect a wallet vault before switching to LIVE mode"
                ));
            }
            *self.mode.write() = TradingMode::Live;
            let _ = self.events_tx.send(ExecEvent::ModeChanged { mode: TradingMode::Live });
            let _ = self.events_tx.send(ExecEvent::AccountUpdated);
            return Ok(());
        }

        // Transitioning Live -> Paper: fence new orders, drain in-flight, cancel all, verify 0 remain
        self.prepare_live_exit("switch to Paper mode").await?;

        *self.mode.write() = TradingMode::Paper;
        let _ = self.events_tx.send(ExecEvent::ModeChanged { mode: TradingMode::Paper });
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        Ok(())
    }

    pub async fn cancel_all_live(&self) -> Result<usize> {
        match self.cancel_all_live_and_verify().await {
            Ok(n) => Ok(n),
            Err(e) => {
                let msg = format!("LIVE ORDERS MAY STILL BE ACTIVE ON POLYMARKET: {e}");
                *self.live_cancel_warning.write() = Some(msg);
                let _ = self.events_tx.send(ExecEvent::AccountUpdated);
                Err(e)
            }
        }
    }

    pub fn clear_live_warning_if_empty(&self) {
        if self.live_orders_cache.read().is_empty() {
            *self.live_cancel_warning.write() = None;
            let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        }
    }

    async fn cancel_all_live_and_verify(&self) -> Result<usize> {
        let sess_opt = self.live.read().await.clone();
        let Some(sess) = sess_opt else {
            let cached = self.live_orders_cache.read().len();
            if cached > 0 {
                return Err(anyhow!(
                    "Wallet is locked/offline with {cached} cached live order(s) that could not be verified cancelled"
                ));
            }
            *self.live_cancel_warning.write() = None;
            return Ok(0);
        };

        let res = sess
            .client
            .cancel_all_orders()
            .await
            .context("Failed calling Polymarket CLOB cancel_all_orders")?;

        let page = sess
            .client
            .orders(&OrdersRequest::default(), None)
            .await
            .context("Failed verifying remaining live orders after cancel_all")?;
        let mapped = map_live_orders(page.data);
        let remaining = mapped.len();
        *self.live_orders_cache.write() = mapped;
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);

        if !res.not_canceled.is_empty() || remaining > 0 {
            let msg = format!(
                "LIVE ORDERS MAY STILL BE ACTIVE ON POLYMARKET ({remaining} remaining, {} failed to cancel)",
                res.not_canceled.len()
            );
            *self.live_cancel_warning.write() = Some(msg.clone());
            let _ = self.events_tx.send(ExecEvent::AccountUpdated);
            return Err(anyhow!(msg));
        }

        *self.live_cancel_warning.write() = None;
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        Ok(res.canceled.len())
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
        mut password: String,
        persist_vault: bool,
    ) -> Result<(GeneratedWallet, AccountState, Option<DepositAddressesInfo>)> {
        if persist_vault && password.chars().count() < MIN_NEW_VAULT_PASSWORD_LEN {
            password.zeroize();
            return Err(anyhow!(
                "Vault passphrase must be at least {MIN_NEW_VAULT_PASSWORD_LEN} characters"
            ));
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
            let vault_path = self.current_vault_path();
            let save_res = save_encrypted_vault(&vault_path, &password, &secret, &hint);
            password.zeroize();
            save_res?;
        } else {
            password.zeroize();
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
        let vault_path = self.current_vault_path();
        let secret = load_encrypted_vault(&vault_path, password)?;
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
        mut private_key_or_phrase: String,
        funder: Option<String>,
        signature_type: WalletSignatureType,
        mut password: String,
        persist_vault: bool,
    ) -> Result<AccountState> {
        let parsed = parse_key_or_mnemonic(&private_key_or_phrase);
        private_key_or_phrase.zeroize();
        let (mut pk_clean, mut mnemonic_opt) = match parsed {
            Ok(v) => v,
            Err(e) => {
                password.zeroize();
                return Err(e);
            }
        };
        if persist_vault && password.chars().count() < MIN_NEW_VAULT_PASSWORD_LEN {
            pk_clean.zeroize();
            if let Some(ref mut m) = mnemonic_opt {
                m.zeroize();
            }
            password.zeroize();
            return Err(anyhow!(
                "Vault passphrase must be at least {MIN_NEW_VAULT_PASSWORD_LEN} characters"
            ));
        }
        let signer_res = PrivateKeySigner::from_str(&pk_clean)
            .context("Invalid Polygon private key hex");
        let signer = match signer_res {
            Ok(s) => s.with_chain_id(Some(POLYGON)),
            Err(e) => {
                pk_clean.zeroize();
                if let Some(ref mut m) = mnemonic_opt {
                    m.zeroize();
                }
                password.zeroize();
                return Err(e);
            }
        };
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
                mnemonic: mnemonic_opt.take(),
                funder: funder_clean.clone(),
                signature_type,
                label: Some("Primary".into()),
            };
            let vault_path = self.current_vault_path();
            let save_res = save_encrypted_vault(&vault_path, &password, &secret, &hint);
            pk_clean.zeroize();
            password.zeroize();
            drop(secret);
            save_res?;
        } else {
            pk_clean.zeroize();
            if let Some(ref mut m) = mnemonic_opt {
                m.zeroize();
            }
            password.zeroize();
        }

        self.authenticate_live(signer, funder_clean, signature_type)
            .await?;
        Ok(self.account_state().await)
    }

    pub async fn unlock_existing_vault(&self, password: &str) -> Result<AccountState> {
        let vault_path = self.current_vault_path();
        let secret = load_encrypted_vault(&vault_path, password)?;
        let signer = PrivateKeySigner::from_str(secret.private_key.trim())
            .context("Vault contains invalid private key")?
            .with_chain_id(Some(POLYGON));
        self.authenticate_live(signer, secret.funder.clone(), secret.signature_type)
            .await?;
        Ok(self.account_state().await)
    }

    pub async fn lock_wallet(&self) -> Result<AccountState> {
        let had_live_session = self.live.read().await.is_some();
        if had_live_session {
            self.prepare_live_exit("wallet lock").await?;
            *self.live.write().await = None;
            *self.mode.write() = TradingMode::Paper;
            self.live_orders_cache.write().clear();
            self.live_positions_cache.write().clear();
            *self.live_balance_cache.write() = None;
            *self.live_cancel_warning.write() = None;
        } else {
            self.order_gen.fetch_add(1, Ordering::SeqCst);
            *self.mode.write() = TradingMode::Paper;
            self.live_positions_cache.write().clear();
            *self.live_balance_cache.write() = None;
            if self.live_orders_cache.read().is_empty() {
                *self.live_cancel_warning.write() = None;
            } else if self.live_cancel_warning.read().is_none() {
                *self.live_cancel_warning.write() = Some(
                    "LIVE ORDERS MAY STILL BE ACTIVE ON POLYMARKET (wallet offline)".to_string(),
                );
            }
        }
        let _ = self.events_tx.send(ExecEvent::ModeChanged { mode: TradingMode::Paper });
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        Ok(self.account_state().await)
    }

    /// Escape hatch: drop signing keys from memory immediately even when offline/unreachable,
    /// leaving `live_orders_cache` intact and surfacing an explicit exchange-orders warning.
    pub async fn force_lock_wallet(&self) -> AccountState {
        self.order_gen.fetch_add(1, Ordering::SeqCst);
        *self.live.write().await = None;
        *self.mode.write() = TradingMode::Paper;
        self.live_positions_cache.write().clear();
        *self.live_balance_cache.write() = None;
        *self.live_cancel_warning.write() = Some(
            "LIVE ORDERS MAY STILL BE ACTIVE ON POLYMARKET (keys force-locked while offline)"
                .to_string(),
        );
        let _ = self.events_tx.send(ExecEvent::ModeChanged { mode: TradingMode::Paper });
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        self.account_state().await
    }

    pub async fn wipe_vault(&self) -> Result<AccountState> {
        if self.live.read().await.is_some() {
            self.lock_wallet().await?;
        }
        let prof = self.active_profile.read().clone();
        delete_vault_profile(&self.base_dir, &prof)?;
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        Ok(self.account_state().await)
    }

    pub async fn switch_vault_profile(&self, target_profile: &str) -> Result<AccountState> {
        let safe_target = validate_profile_name(target_profile)?;
        if safe_target == *self.active_profile.read() {
            return Ok(self.account_state().await);
        }

        let must_cancel = self.live.read().await.is_some()
            || self.mode() == TradingMode::Live
            || !self.live_orders_cache.read().is_empty();
        if must_cancel {
            self.prepare_live_exit("vault profile switch").await?;
        }

        let prev_live = self.live.write().await.take();
        drop(prev_live);
        *self.mode.write() = TradingMode::Paper;
        self.live_orders_cache.write().clear();
        self.live_positions_cache.write().clear();
        *self.live_balance_cache.write() = None;
        *self.live_cancel_warning.write() = None;

        *self.active_profile.write() = safe_target.clone();
        self.update_config_and_save(|c| c.active_vault_profile = safe_target.clone())?;

        let _ = self.events_tx.send(ExecEvent::ModeChanged {
            mode: TradingMode::Paper,
        });
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        Ok(self.account_state().await)
    }

    pub async fn rename_vault_profile(&self, old_name: &str, new_name: &str) -> Result<AccountState> {
        let old_safe = validate_profile_name(old_name)?;
        let new_safe = validate_profile_name(new_name)?;
        if old_safe == new_safe {
            return Ok(self.account_state().await);
        }
        let old_path = vault_path_for_profile(&self.base_dir, &old_safe)?;
        let new_path = vault_path_for_profile(&self.base_dir, &new_safe)?;
        if old_path.exists() {
            vault_rename_profile(&self.base_dir, &old_safe, &new_safe)?;
        } else if new_path.exists() {
            return Err(anyhow!("Vault profile '{new_safe}' already exists"));
        }
        if *self.active_profile.read() == old_safe {
            *self.active_profile.write() = new_safe.clone();
            self.update_config_and_save(|c| c.active_vault_profile = new_safe.clone())?;
        }
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        Ok(self.account_state().await)
    }

    pub async fn execute_ctf(&self, req: CtfRequest) -> Result<CtfTxResult> {
        struct FlagReset<'a>(&'a AtomicBool);
        impl Drop for FlagReset<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        if self
            .ctf_in_flight
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(anyhow!("Another CTF transaction is already in progress"));
        }
        let _guard = FlagReset(&self.ctf_in_flight);

        match self.mode() {
            TradingMode::Paper => match req.action {
                CtfAction::Split => {
                    self.paper.split_position(
                        &req.condition_id,
                        &req.market_question,
                        &req.yes_asset_id,
                        &req.no_asset_id,
                        &req.yes_outcome,
                        &req.no_outcome,
                        req.amount,
                    )?;
                    let _ = self.events_tx.send(ExecEvent::AccountUpdated);
                    Ok(CtfTxResult {
                        action: CtfAction::Split,
                        mode_label: "PAPER".into(),
                        tx_hash: None,
                        polygonscan_url: None,
                        summary: format!(
                            "Paper Split: ${:.2} -> {:.2} {} + {:.2} {}",
                            req.amount, req.amount, req.yes_outcome, req.amount, req.no_outcome
                        ),
                    })
                }
                CtfAction::Merge => {
                    self.paper
                        .merge_positions(&req.yes_asset_id, &req.no_asset_id, req.amount)?;
                    let _ = self.events_tx.send(ExecEvent::AccountUpdated);
                    Ok(CtfTxResult {
                        action: CtfAction::Merge,
                        mode_label: "PAPER".into(),
                        tx_hash: None,
                        polygonscan_url: None,
                        summary: format!(
                            "Paper Merge: {:.2} {} + {:.2} {} -> ${:.2}",
                            req.amount, req.yes_outcome, req.amount, req.no_outcome, req.amount
                        ),
                    })
                }
                CtfAction::Redeem => Err(anyhow!(
                    "On-chain CTF Redeem is available in LIVE mode only (use Cash Out in Paper mode)"
                )),
            },
            TradingMode::Live => {
                let sess = self
                    .live
                    .read()
                    .await
                    .clone()
                    .ok_or_else(|| anyhow!("Unlock an EOA wallet vault first"))?;
                ensure_eoa_for_direct_ctf(sess.signature_type)?;
                let rpc_url = self.config.read().polygon_rpc_url.clone();
                let res =
                    execute_ctf_live(&rpc_url, sess.signer.clone(), sess.signature_type, &req).await?;
                let _ = self.refresh_live_account().await;
                let _ = self.events_tx.send(ExecEvent::AccountUpdated);
                Ok(res)
            }
        }
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
        let _ = self.events_tx.send(ExecEvent::ModeChanged { mode: TradingMode::Live });
        let _ = self.events_tx.send(ExecEvent::AccountUpdated);
        let short_wallet = truncate_address(&wallet_address);
        info!(wallet = %short_wallet, "Live Polymarket CLOB V2 wallet authenticated");
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
            let mapped = map_live_orders(page.data);
            if mapped.is_empty() {
                *self.live_cancel_warning.write() = None;
            }
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

        let vault_path = self.current_vault_path();
        let active_vault_profile = self.active_profile.read().clone();
        let vault_profiles = list_vault_profiles(&self.base_dir);

        AccountState {
            mode,
            live_connected,
            wallet_address,
            funder_address,
            signature_type,
            vault: vault_metadata(&vault_path),
            active_vault_profile,
            vault_profiles,
            live_balance_usd: *self.live_balance_cache.read(),
            builder_code: cfg.builder_code.clone(),
            builder_maker_bps: maker_bps,
            builder_taker_bps: taker_bps,
            builder_fee_locked: cfg.is_builder_locked(),
            heartbeats_enabled: hb,
            paper: self.paper.snapshot(),
            live_open_orders: self.live_orders_cache.read().clone(),
            live_positions: self.live_positions_cache.read().clone(),
            live_orders_warning: self.live_cancel_warning.read().clone(),
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
        self.feed
            .with_book(asset_id, |book| book.simulate_sweep(is_buy, usd, shares, limit_price))
    }

    pub fn preview_green_up(&self, asset_id: &str, opposite_asset_id: Option<&str>) -> Option<GreenUpQuote> {
        let pos = self.get_position(asset_id)?;
        let best_bid = self
            .feed
            .with_book(asset_id, |book| book.best_bid().or(book.last_trade_price))
            .flatten()?;
        let opp_ask = opposite_asset_id
            .and_then(|oid| self.feed.with_book(oid, |b| b.best_ask()).flatten());
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
            let sweep_outcome = self.feed.with_book(&req.asset_id, |book| {
                let max_slip = req.max_slippage_bps.unwrap_or(500.0) / 10_000.0;
                let ref_px = if is_buy {
                    book.best_ask().unwrap_or(req.price)
                } else {
                    book.best_bid().unwrap_or(req.price)
                };
                let cap = if is_buy {
                    (ref_px * (1.0 + max_slip)).min(0.9999)
                } else {
                    (ref_px * (1.0 - max_slip)).max(0.0001)
                };
                if is_buy
                    && let Some(usd_cap) = req.max_notional_usd.filter(|v| v.is_finite() && *v > 0.0)
                {
                    let quote = book.simulate_sweep(true, Some(usd_cap), None, Some(cap));
                    if quote.total_shares <= 1e-6 || quote.worst_price <= 0.0 {
                        return Err(anyhow!("Insufficient ask liquidity within slippage cap"));
                    }
                    let aligned_worst = align_to_tick(quote.worst_price, book.tick_size_ticks);
                    let max_sh_at_worst = ((usd_cap / aligned_worst) * 100.0).floor() / 100.0;
                    let capped_shares = (quote.total_shares.min(max_sh_at_worst) * 100.0).floor() / 100.0;
                    if capped_shares < 0.01 {
                        return Err(anyhow!("Order notional ${usd_cap:.2} is too small at {aligned_worst:.4}"));
                    }
                    Ok((Some(aligned_worst), Some(capped_shares)))
                } else {
                    let quote = book.simulate_sweep(is_buy, None, Some(req.size), Some(cap));
                    let px = if quote.worst_price > 0.0 {
                        Some(align_to_tick(quote.worst_price, book.tick_size_ticks))
                    } else {
                        None
                    };
                    Ok((px, None))
                }
            });

            match sweep_outcome {
                Some(Ok((px_opt, sh_opt))) => {
                    if let Some(p) = px_opt {
                        effective.price = p;
                    }
                    if let Some(s) = sh_opt {
                        effective.size = s;
                    }
                }
                Some(Err(e)) => return Err(e),
                None => {
                    if is_buy
                        && let Some(usd_cap) = req.max_notional_usd.filter(|v| v.is_finite() && *v > 0.0)
                    {
                        let tick_ticks = self.feed.get_tick_size_ticks(&req.asset_id).unwrap_or(100);
                        effective.price = align_to_tick(effective.price, tick_ticks);
                        let max_sh = ((usd_cap / effective.price) * 100.0).floor() / 100.0;
                        effective.size = (effective.size.min(max_sh) * 100.0).floor() / 100.0;
                        if effective.size < 0.01 {
                            return Err(anyhow!("Order notional ${usd_cap:.2} is too small"));
                        }
                    }
                }
            }

            effective.order_type = "FAK".into();
            effective.post_only = false;
        } else if is_buy
            && let Some(usd_cap) = req.max_notional_usd.filter(|v| v.is_finite() && *v > 0.0)
        {
            let tick_ticks = self.feed.get_tick_size_ticks(&req.asset_id).unwrap_or(100);
            effective.price = align_to_tick(effective.price, tick_ticks);
            let max_sh = ((usd_cap / effective.price) * 100.0).floor() / 100.0;
            effective.size = (effective.size.min(max_sh) * 100.0).floor() / 100.0;
            if effective.size < 0.01 {
                return Err(anyhow!("Order notional ${usd_cap:.2} is too small"));
            }
        }

        // Handle TIMED_FOK: place GTC and schedule exact millisecond auto-cancel
        let timed_fok_ms = if otype == "TIMED_FOK" {
            effective.order_type = "GTC".into();
            Some(req.timed_fok_ms.unwrap_or(1500).clamp(150, 60_000))
        } else {
            None
        };

        let order_gen_snapshot = self.order_gen.load(Ordering::SeqCst);
        let ack = match mode {
            TradingMode::Paper => self.place_order_paper(&effective)?,
            TradingMode::Live => self.place_order_live(&effective, order_gen_snapshot).await?,
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
        let expires_at_ms = req
            .expiration_secs
            .filter(|&s| s > 0)
            .map(|s| Utc::now().timestamp_millis() + s * 1000);

        let params = PaperOrderParams {
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
        };
        let res = match self
            .feed
            .with_book(&req.asset_id, |b| self.paper.place_order(params.clone(), Some(b)))
        {
            Some(r) => r?,
            None => self.paper.place_order(params, None)?,
        };

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

    async fn place_order_live(&self, req: &OrderRequest, captured_gen: u64) -> Result<OrderAck> {
        let _barrier = self.enter_live_order_barrier(captured_gen)?;
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

        if !req.price.is_finite() || !(0.0001..=0.9999).contains(&req.price) {
            return Err(anyhow!("Order price out of valid range (0.01¢–99.99¢)"));
        }
        if !req.size.is_finite() || req.size <= 0.0 {
            return Err(anyhow!("Order size must be greater than zero"));
        }

        let tick_ticks = match self.feed.get_tick_size_ticks(&req.asset_id) {
            Some(t) => t,
            None => sess
                .client
                .tick_size(token_id)
                .await
                .ok()
                .and_then(|ts| ts.minimum_tick_size.as_decimal().to_f64())
                .map(|f| ((f * 10_000.0).round() as usize).clamp(1, 500))
                .unwrap_or(100),
        };
        let aligned_price = align_to_tick(req.price, tick_ticks);

        let price_dec = Decimal::from_f64(aligned_price)
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

        if self.order_gen.load(Ordering::SeqCst) != captured_gen || self.mode() != TradingMode::Live {
            return Err(anyhow!(
                "Live order aborted before submission: mode or wallet changed"
            ));
        }
        let t_post = Instant::now();
        let resp = sess
            .client
            .post_order(signed)
            .await
            .context("CLOB POST /order failed")?;
        let rtt_ms = t_post.elapsed().as_secs_f64() * 1000.0;
        self.telemetry.inc_order_sent();
        self.telemetry.record_order_rtt_ms(rtt_ms);

        let status_str = format!("{:?}", resp.status).to_uppercase();
        validate_post_order_response(resp.success, resp.error_msg.as_deref(), &status_str)?;

        let notional = aligned_price * req.size;
        let is_maker = matches!(order_type, OrderType::GTC | OrderType::GTD) && req.post_only;
        let b_bps = if is_maker {
            sess.builder_maker_bps
        } else {
            sess.builder_taker_bps
        };
        let b_fee = estimate_builder_fee_usd(notional, is_maker, sess.builder_maker_bps, sess.builder_taker_bps);
        let p_fee = estimate_protocol_fee_usd(req.size, aligned_price, is_maker, req.fees_enabled.unwrap_or(true));

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

        let raw_filled = match side {
            Side::Buy => resp.taking_amount.to_f64().unwrap_or(0.0),
            Side::Sell => resp.making_amount.to_f64().unwrap_or(0.0),
            _ => resp.taking_amount.to_f64().unwrap_or(0.0),
        };
        let filled_size = if raw_filled <= 1e-6 && matches!(resp.status, OrderStatusType::Matched) {
            req.size
        } else {
            raw_filled.clamp(0.0, req.size)
        };

        // Refresh open orders asynchronously after fill/placement
        let _ = self.refresh_live_account().await;

        Ok(OrderAck {
            mode: TradingMode::Live,
            order_id: resp.order_id,
            status: status_str,
            side: req.side.to_uppercase(),
            price: aligned_price,
            size: req.size,
            filled_size,
            sign_us,
            rtt_ms,
            builder_fee_bps: b_bps,
            estimated_builder_fee_usd: b_fee,
            error: None,
        })
    }

    /// Scale Entry: distribute N limit orders evenly across [start_price .. end_price].
    pub async fn place_scale_entry(self: &Arc<Self>, req: ScaleEntryRequest) -> Result<BatchOrderReport> {
        let steps = req.steps.clamp(2, 15);
        let tick_ticks = self
            .feed
            .get_tick_size_ticks(&req.asset_id)
            .unwrap_or(100);
        let clip = (req.total_size / (steps as f64) * 100.0).floor() / 100.0;
        if clip < 1.0 {
            return Err(anyhow!("Clip size per rung ({clip:.2}) is too small"));
        }

        let mut placed = Vec::with_capacity(steps);
        let mut errors = Vec::new();
        for i in 0..steps {
            let frac = (i as f64) / ((steps - 1) as f64);
            let raw_p = req.start_price + (req.end_price - req.start_price) * frac;
            let p = align_to_tick(raw_p, tick_ticks);
            match self
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
                    max_notional_usd: None,
                })
                .await
            {
                Ok(ack) => placed.push(ack),
                Err(e) => errors.push(format!("rung {} ({:.1}¢): {e}", i + 1, p * 100.0)),
            }
        }
        if placed.is_empty() && !errors.is_empty() {
            return Err(anyhow!("All scale rungs failed: {}", errors.join("; ")));
        }
        Ok(BatchOrderReport {
            placed,
            failed: errors.len(),
            errors,
        })
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
                    max_notional_usd: None,
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
                    max_notional_usd: None,
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
                    max_notional_usd: None,
                })
                .await
            }
        }
    }

    /// Sibling-Market Auto-Hedge / Dutch-Book: splits USD budget (when `stake_in_usd`)
    /// or places equal shares across all selected sibling outcomes, reporting partial failures.
    pub async fn execute_sibling_hedge(
        self: &Arc<Self>,
        outcomes: &[SiblingOutcome],
        stake_value: f64,
        stake_in_usd: bool,
    ) -> Result<BatchOrderReport> {
        let legs = plan_sibling_hedge_legs(outcomes, stake_value, stake_in_usd)?;
        let mut placed = Vec::with_capacity(legs.len());
        let mut errors = Vec::new();
        for (o, px, shares) in legs {
            let max_notional_usd = stake_in_usd.then_some(shares * px);
            match self
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
                    max_notional_usd,
                })
                .await
            {
                Ok(ack) => {
                    if ack.status == "CANCELLED" && ack.filled_size <= 1e-6 {
                        errors.push(format!("{}: FAK cancelled (no fill)", o.group_title));
                    } else {
                        placed.push(ack);
                    }
                }
                Err(e) => errors.push(format!("{}: {e}", o.group_title)),
            }
        }
        if placed.is_empty() && !errors.is_empty() {
            return Err(anyhow!("All sibling hedge legs failed: {}", errors.join("; ")));
        }
        Ok(BatchOrderReport {
            placed,
            failed: errors.len(),
            errors,
        })
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
            TradingMode::Paper => {
                let n = self.paper.cancel_all();
                let _ = self.events_tx.send(ExecEvent::AccountUpdated);
                n
            }
            TradingMode::Live => self.cancel_all_live().await?,
        };
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

    #[cfg(test)]
    pub(crate) fn set_mode_for_test(&self, mode: TradingMode) {
        *self.mode.write() = mode;
    }

    #[cfg(test)]
    pub(crate) fn seed_live_orders_cache_for_test(&self, orders: Vec<PaperOrder>) {
        *self.live_orders_cache.write() = orders;
    }
}

fn map_live_orders(data: Vec<OpenOrderResponse>) -> Vec<PaperOrder> {
    data.into_iter()
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
        .collect()
}

pub fn truncate_address(addr: &str) -> String {
    let trimmed = addr.trim();
    let count = trimmed.chars().count();
    if count <= 12 {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(6).collect();
    let tail: String = trimmed.chars().skip(count - 4).collect();
    format!("{head}…{tail}")
}

pub fn validate_post_order_response(
    success: bool,
    error_msg: Option<&str>,
    status_label: &str,
) -> Result<()> {
    let err_trimmed = error_msg.map(str::trim).filter(|s| !s.is_empty());
    if !success || err_trimmed.is_some() {
        let reason = err_trimmed.unwrap_or(status_label);
        return Err(anyhow!("CLOB order rejected: {reason}"));
    }
    Ok(())
}

pub fn plan_sibling_hedge_legs(
    outcomes: &[SiblingOutcome],
    stake_value: f64,
    stake_in_usd: bool,
) -> Result<Vec<(SiblingOutcome, f64, f64)>> {
    if outcomes.len() < 2 {
        return Err(anyhow!("Need at least 2 sibling outcomes to auto-hedge"));
    }
    if !stake_value.is_finite() || stake_value <= 0.0 {
        return Err(anyhow!("Stake amount must be positive"));
    }
    let n = outcomes.len() as f64;
    let mut legs = Vec::with_capacity(outcomes.len());
    if stake_in_usd {
        let usd_per_leg = stake_value / n;
        let mut remaining_usd = stake_value;
        for o in outcomes {
            let tick_ticks = ((o.tick_size * 10_000.0).round() as usize).clamp(1, 500);
            let px = align_to_tick(o.best_ask.unwrap_or(o.yes_price), tick_ticks);
            let leg_budget = usd_per_leg.min(remaining_usd).max(0.0);
            let shares = ((leg_budget / px) * 100.0).floor() / 100.0;
            if shares < 0.01 {
                return Err(anyhow!(
                    "USD stake ${stake_value:.2} across {} legs is too small for leg '{}'",
                    outcomes.len(),
                    o.group_title
                ));
            }
            remaining_usd = (remaining_usd - shares * px).max(0.0);
            legs.push((o.clone(), px, shares));
        }
    } else {
        let shares = (stake_value * 100.0).floor() / 100.0;
        if shares < 0.01 {
            return Err(anyhow!("Share count must be at least 0.01"));
        }
        for o in outcomes {
            let tick_ticks = ((o.tick_size * 10_000.0).round() as usize).clamp(1, 500);
            let px = align_to_tick(o.best_ask.unwrap_or(o.yes_price), tick_ticks);
            legs.push((o.clone(), px, shares));
        }
    }
    Ok(legs)
}

pub fn align_to_tick(price: f64, tick_size_ticks: usize) -> f64 {
    let step = tick_size_ticks.clamp(1, 9_999);
    let min_tick = step;
    let max_tick = (9_999 / step) * step;
    let raw_ticks = if price.is_nan() || price <= 0.0 {
        min_tick as f64
    } else if price >= 1.0 {
        max_tick as f64
    } else {
        ((price * 10_000.0) / (step as f64)).round() * (step as f64)
    };
    let clamped = (raw_ticks.round() as usize).clamp(min_tick, max_tick);
    (clamped as f64) / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::Telemetry;

    fn test_engine() -> Arc<ExecutionEngine> {
        let cfg = Arc::new(RwLock::new(AppConfig::default()));
        let telemetry = Arc::new(Telemetry::new());
        let feed = MarketFeed::spawn("ws://127.0.0.1:1".into(), telemetry.clone(), 500.0);
        let mc = Arc::new(
            MarketClient::new(
                "https://gamma-api.polymarket.com".into(),
                "https://clob.polymarket.com".into(),
                "https://data-api.polymarket.com".into(),
            )
            .unwrap(),
        );
        ExecutionEngine::new(cfg, None, mc, feed, telemetry)
    }

    #[test]
    fn align_to_tick_respects_boundaries_and_never_returns_off_tick() {
        assert!((align_to_tick(0.995, 100) - 0.99).abs() < 1e-9);
        assert!((align_to_tick(1.5, 100) - 0.99).abs() < 1e-9);
        assert!((align_to_tick(0.001, 100) - 0.01).abs() < 1e-9);
        assert!((align_to_tick(-0.5, 100) - 0.01).abs() < 1e-9);
        assert!((align_to_tick(f64::NAN, 100) - 0.01).abs() < 1e-9);
        assert!((align_to_tick(0.9999, 10) - 0.999).abs() < 1e-9);
        assert!((align_to_tick(0.9999, 1) - 0.9999).abs() < 1e-9);
        assert!((align_to_tick(0.98, 500) - 0.95).abs() < 1e-9);
    }

    #[test]
    fn validate_post_order_response_rejects_failures_and_error_msgs() {
        assert!(validate_post_order_response(true, None, "LIVE").is_ok());
        assert!(validate_post_order_response(true, Some("  "), "LIVE").is_ok());
        assert!(validate_post_order_response(false, None, "UNMATCHED").is_err());
        assert!(validate_post_order_response(true, Some("insufficient balance"), "LIVE").is_err());
        assert!(validate_post_order_response(false, Some("crosses book"), "CANCELED").is_err());
    }

    #[test]
    fn plan_sibling_hedge_legs_respects_usd_budget_and_shares_mode() {
        let mk = |name: &str, ask: f64| SiblingOutcome {
            market_id: name.into(),
            condition_id: format!("cond-{name}"),
            question: format!("{name}?"),
            group_title: name.into(),
            group_title_zh: None,
            yes_token_id: format!("tok-{name}"),
            no_token_id: format!("no-{name}"),
            yes_price: ask,
            no_price: 1.0 - ask,
            best_ask: Some(ask),
            best_bid: Some((ask - 0.01).max(0.01)),
            liquidity: 1000.0,
            tick_size: 0.01,
        };
        let outs = vec![mk("A", 0.33), mk("B", 0.34), mk("C", 0.31)];

        // USD mode: total notional strictly <= $30.00
        let usd_legs = plan_sibling_hedge_legs(&outs, 30.0, true).unwrap();
        assert_eq!(usd_legs.len(), 3);
        let total_notional: f64 = usd_legs.iter().map(|(_, p, s)| p * s).sum();
        assert!(total_notional <= 30.0 + 1e-9, "notional {total_notional} > 30");

        // Shares mode: equal shares per leg
        let sh_legs = plan_sibling_hedge_legs(&outs, 12.0, false).unwrap();
        assert!(sh_legs.iter().all(|(_, _, s)| (*s - 12.0).abs() < 1e-9));
    }

    #[tokio::test]
    async fn sweep_buy_caps_notional_at_target_usd() {
        let exec = test_engine();
        exec.feed.seed_book(
            "tok_sweep",
            "cond_sweep",
            0.01,
            &[(0.38, 100.0)],
            &[(0.40, 50.0), (0.55, 100.0), (0.70, 100.0)],
            Some(0.40),
            1,
            None,
        );

        let initial_cash = exec.account_state().await.paper.cash_usd;
        let ack = exec
            .place_order(OrderRequest {
                asset_id: "tok_sweep".into(),
                condition_id: "cond_sweep".into(),
                market_question: "Sweep test".into(),
                outcome: "Yes".into(),
                side: "BUY".into(),
                order_type: "SWEEP".into(),
                post_only: false,
                price: 0.40,
                size: 125.0, // naive share count at 0.40 for $50 -> without cap would cost way more across 0.55!
                expiration_secs: None,
                timed_fok_ms: None,
                max_slippage_bps: Some(5000.0),
                fees_enabled: Some(false),
                max_notional_usd: Some(50.0),
            })
            .await
            .unwrap();

        assert!(ack.size * ack.price <= 50.0 + 1e-6);
        let after_cash = exec.account_state().await.paper.cash_usd;
        let spent = initial_cash - after_cash;
        assert!(spent <= 50.0 + 1e-6, "spent ${spent} exceeded $50 cap");
        assert!(spent > 0.0);
    }

    #[tokio::test]
    async fn scale_entry_reports_partial_failures() {
        let exec = test_engine();
        exec.reset_paper(5.0);
        exec.feed.seed_book(
            "tok_scale",
            "cond_scale",
            0.01,
            &[(0.40, 100.0)],
            &[(0.60, 100.0)],
            Some(0.50),
            1,
            None,
        );

        // 4 rungs of 5 shares: prices 0.20 ($1), 0.30 ($1.50), 0.40 ($2), 0.50 ($2.50)
        // With $5.00 cash, first 3 rungs cost $4.50 and succeed, 4th rung fails!
        let report = exec
            .place_scale_entry(ScaleEntryRequest {
                asset_id: "tok_scale".into(),
                condition_id: "cond_scale".into(),
                market_question: "Scale?".into(),
                outcome: "Yes".into(),
                side: "BUY".into(),
                start_price: 0.20,
                end_price: 0.50,
                total_size: 20.0,
                steps: 4,
                post_only: false,
                fees_enabled: Some(false),
            })
            .await
            .unwrap();

        assert_eq!(report.placed.len(), 3);
        assert_eq!(report.failed, 1);
        assert_eq!(report.errors.len(), 1);
    }

    #[tokio::test]
    async fn live_switch_and_force_lock_preserve_cached_live_orders_on_failure() {
        let exec = test_engine();

        // Cannot switch to Live without unlocked wallet
        assert!(exec.set_mode(TradingMode::Live).await.is_err());

        // Seed cached live order & set mode to Live for test
        exec.seed_live_orders_cache_for_test(vec![PaperOrder {
            id: "ord-live-1".into(),
            asset_id: "tok1".into(),
            condition_id: "cond1".into(),
            market_question: "Live?".into(),
            outcome: "Yes".into(),
            side: "BUY".into(),
            order_type: "GTC".into(),
            post_only: true,
            price: 0.50,
            original_size: 10.0,
            remaining_size: 10.0,
            filled_size: 0.0,
            avg_fill_price: 0.50,
            status: "LIVE".into(),
            created_at_ms: 1,
            expires_at_ms: None,
            fees_enabled: true,
        }]);
        exec.set_mode_for_test(TradingMode::Live);

        // Attempting Live -> Paper when live session/cancel verification fails refuses switch & preserves cache
        let err = exec.set_mode(TradingMode::Paper).await.unwrap_err();
        assert!(err.to_string().contains("LIVE ORDERS MAY STILL BE ACTIVE"));
        assert_eq!(exec.mode(), TradingMode::Live);
        let st = exec.account_state().await;
        assert_eq!(st.live_open_orders.len(), 1);
        assert!(st.live_orders_warning.is_some());

        // Force lock switches to Paper while retaining live_orders_cache & warning
        let locked_st = exec.force_lock_wallet().await;
        assert_eq!(locked_st.mode, TradingMode::Paper);
        assert!(!locked_st.live_connected);
        assert_eq!(locked_st.live_open_orders.len(), 1);
        assert!(locked_st.live_orders_warning.is_some());
    }

    #[test]
    fn truncate_address_formats_safely() {
        assert_eq!(
            truncate_address("0x1234567890abcdef1234567890abcdef1234abcd"),
            "0x1234…abcd"
        );
        assert_eq!(truncate_address("0x1234"), "0x1234");
    }

    #[tokio::test]
    async fn switch_vault_profile_refuses_silent_switch_when_live_cancel_fails() {
        let dir = std::env::temp_dir().join(format!(
            "polydirect-exec-profile-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("config.toml");

        let cfg = Arc::new(RwLock::new(AppConfig::default()));
        let telemetry = Arc::new(Telemetry::new());
        let feed = MarketFeed::spawn("ws://127.0.0.1:1".into(), telemetry.clone(), 500.0);
        let mc = Arc::new(
            MarketClient::new(
                "https://gamma-api.polymarket.com".into(),
                "https://clob.polymarket.com".into(),
                "https://data-api.polymarket.com".into(),
            )
            .unwrap(),
        );
        let exec = ExecutionEngine::new(cfg, Some(cfg_path), mc, feed, telemetry);

        // Traversal/invalid profile name is rejected
        assert!(exec.switch_vault_profile("../evil").await.is_err());

        // Switching cleanly in Paper mode updates profile & persists config
        let st = exec.switch_vault_profile("alt_wallet").await.unwrap();
        assert_eq!(st.active_vault_profile, "alt_wallet");
        assert_eq!(exec.active_vault_profile(), "alt_wallet");

        // Seed live order cache + switch mode to Live: attempting profile switch must fail (C1)
        exec.seed_live_orders_cache_for_test(vec![PaperOrder {
            id: "ord-live-prof-1".into(),
            asset_id: "tok1".into(),
            condition_id: "cond1".into(),
            market_question: "Live?".into(),
            outcome: "Yes".into(),
            side: "BUY".into(),
            order_type: "GTC".into(),
            post_only: true,
            price: 0.50,
            original_size: 10.0,
            remaining_size: 10.0,
            filled_size: 0.0,
            avg_fill_price: 0.50,
            status: "LIVE".into(),
            created_at_ms: 1,
            expires_at_ms: None,
            fees_enabled: true,
        }]);
        exec.set_mode_for_test(TradingMode::Live);

        let err = exec.switch_vault_profile("default").await.unwrap_err();
        assert!(err.to_string().contains("LIVE ORDERS MAY STILL BE ACTIVE"));
        assert_eq!(exec.active_vault_profile(), "alt_wallet");
        assert_eq!(exec.mode(), TradingMode::Live);
        let after = exec.account_state().await;
        assert_eq!(after.live_open_orders.len(), 1);
        assert!(after.live_orders_warning.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn live_order_barrier_rejects_stale_gen_and_lock_preserves_force_locked_cache() {
        let exec = test_engine();
        exec.seed_live_orders_cache_for_test(vec![PaperOrder {
            id: "ord-live-barrier-1".into(),
            asset_id: "tok1".into(),
            condition_id: "cond1".into(),
            market_question: "Live?".into(),
            outcome: "Yes".into(),
            side: "BUY".into(),
            order_type: "GTC".into(),
            post_only: true,
            price: 0.50,
            original_size: 10.0,
            remaining_size: 10.0,
            filled_size: 0.0,
            avg_fill_price: 0.50,
            status: "LIVE".into(),
            created_at_ms: 1,
            expires_at_ms: None,
            fees_enabled: true,
        }]);
        exec.set_mode_for_test(TradingMode::Live);

        let g0 = exec.order_gen.load(Ordering::SeqCst);
        {
            let _guard = exec.enter_live_order_barrier(g0).unwrap();
            assert_eq!(exec.in_flight_live_orders.load(Ordering::SeqCst), 1);
        }
        assert_eq!(exec.in_flight_live_orders.load(Ordering::SeqCst), 0);

        // Bump generation (as Live exit / lock does) -> stale generation rejected & count stays 0
        exec.order_gen.fetch_add(1, Ordering::SeqCst);
        assert!(exec.enter_live_order_barrier(g0).is_err());
        assert_eq!(exec.in_flight_live_orders.load(Ordering::SeqCst), 0);

        // Force lock preserves cached orders and warning
        let forced = exec.force_lock_wallet().await;
        assert_eq!(forced.live_open_orders.len(), 1);
        assert!(forced.live_orders_warning.is_some());

        // Subsequent lock_wallet() when live is already None must NOT erase cached live orders or warning
        let locked = exec.lock_wallet().await.unwrap();
        assert_eq!(locked.live_open_orders.len(), 1);
        assert!(locked.live_orders_warning.is_some());
    }
}
