pub mod chart;
pub mod ladder;
pub mod modals;
pub mod theme;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{
    Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, Frame, Key, Margin,
    RichText, ScrollArea, Stroke, TextEdit, Vec2,
};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use polydesk::CoreEngine;
use polydesk::automation::{AutomationRule, CopyTarget, CreateCopyTargetRequest, CreateRuleRequest, RuleKind};
use polydesk::book::{estimate_builder_fee_usd, estimate_protocol_fee_usd, GreenUpQuote, SweepQuote};
use polydesk::config::HARDCODED_BUILDER_LABEL;
use polydesk::execution::{
    AccountState, DepositAddressesInfo, OrderRequest, ScaleEntryRequest, TradingMode, align_to_tick,
};
use polydesk::i18n::{Lang, tr};
use polydesk::market::{EventSiblings, MarketSummary, PricePoint, WhaleFlowSummary};
use polydesk::odds::{OddsFormat, format_odds, format_size, format_usd};
use polydesk::paper::{PaperOrder, PaperPosition};
use polydesk::updater::{UpdateStatus, check_github_release, download_and_apply_update};
use polydesk::vault::{GeneratedWallet, WalletSignatureType};

use self::chart::{ChartMode, IndicatorConfig, draw_price_chart};
use self::ladder::{LadderAction, LadderState, draw_price_ladder};
use self::theme::{ThemePalette, apply_theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveModal {
    WalletAndDeposit,
    SettingsAndUpdates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletSetupTab {
    CreateNew,
    ImportExisting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrderChoice {
    Gtc,
    PostOnly,
    Fak,
    Fok,
    TimedFok,
    Sweep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RightTab {
    QuickTrade,
    Ladder,
    ProOrders,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeckTab {
    Positions,
    Orders,
    Automation,
    WhalesTape,
    CopyTrade,
    Fees,
}

pub enum UiMsg {
    MarketsLoaded { markets: Vec<MarketSummary>, append: bool },
    HistoryLoaded(String, Vec<PricePoint>),
    WhalesLoaded(String, WhaleFlowSummary),
    SiblingsLoaded(String, EventSiblings),
    TranslationsReady,
    AccountUpdated(AccountState),
    WalletCreated {
        wallet: GeneratedWallet,
        account: AccountState,
        deposit: Option<DepositAddressesInfo>,
    },
    WalletRevealed(GeneratedWallet),
    DepositLoaded(DepositAddressesInfo),
    UpdateChecked(UpdateStatus),
    Toast { text: String, is_err: bool },
}

pub struct PolyDeskDesktop {
    pub(crate) core: Arc<CoreEngine>,
    pub(crate) rt: Arc<Runtime>,
    pub(crate) tx: mpsc::UnboundedSender<UiMsg>,
    rx: mpsc::UnboundedReceiver<UiMsg>,

    pub(crate) lang: Lang,
    pub(crate) light_mode: bool,
    pub(crate) odds_fmt: OddsFormat,
    pub(crate) modal: Option<ActiveModal>,
    pub(crate) account: AccountState,
    pub(crate) update_status: Option<UpdateStatus>,
    pub(crate) deposit_info: Option<DepositAddressesInfo>,

    show_markets_rail: bool,
    markets: Vec<MarketSummary>,
    markets_offset: usize,
    loading_more_markets: bool,
    search_query: String,
    category: String,
    active_market: Option<MarketSummary>,
    active_outcome_idx: usize,
    siblings: Option<EventSiblings>,
    history: Vec<PricePoint>,
    chart_interval: &'static str,
    chart_mode: ChartMode,
    indicators: IndicatorConfig,
    whale_flow: Option<WhaleFlowSummary>,
    ladder_state: LadderState,

    // Polymarket-style 1-click Quick Trade state
    quick_buy: bool,
    quick_limit_mode: bool,
    quick_amount_input: String,
    quick_limit_cents: f64,

    order_choice: OrderChoice,
    stake_value: f64,
    stake_in_usd: bool,
    timed_fok_ms: u64,
    right_tab: RightTab,
    deck_tab: DeckTab,

    // Pro order form state
    rule_kind: RuleKind,
    rule_side_sell: bool,
    rule_trigger_cents: f64,
    rule_trail_cents: f64,
    rule_size: f64,
    rule_slices: u32,
    rule_interval_sec: u64,

    scale_buy: bool,
    scale_start_cents: f64,
    scale_end_cents: f64,
    scale_size: f64,
    scale_steps: usize,
    scale_post_only: bool,

    copy_wallet: String,
    copy_label: String,
    copy_mult: f64,
    copy_max_usd: f64,

    pub(crate) paper_reset_input: String,
    pub(crate) wallet_setup_tab: WalletSetupTab,
    pub(crate) wallet_sig_type: WalletSignatureType,
    pub(crate) wallet_pk_input: String,
    pub(crate) wallet_funder_input: String,
    pub(crate) vault_pass_input: String,
    pub(crate) reveal_pass_input: String,
    pub(crate) revealed_wallet: Option<GeneratedWallet>,
    pub(crate) wallet_persist_vault: bool,
    pub(crate) deposit_wallet_input: String,
    pub(crate) github_repo_input: String,

    toasts: Vec<(String, bool, Instant)>,
    last_account_poll: Instant,
}

impl PolyDeskDesktop {
    pub(crate) fn palette(&self) -> ThemePalette {
        ThemePalette::from_light(self.light_mode)
    }

    pub fn new(cc: &eframe::CreationContext<'_>, core: Arc<CoreEngine>, rt: Arc<Runtime>) -> Self {
        let cfg = core.config.read().clone();
        let light_mode = cfg.theme.eq_ignore_ascii_case("light");
        apply_theme(&cc.egui_ctx, light_mode);
        install_cjk_fonts(&cc.egui_ctx);

        let (tx, rx) = mpsc::unbounded_channel();
        let account = rt.block_on(core.execution.account_state());

        let app = Self {
            core: core.clone(),
            rt: rt.clone(),
            tx: tx.clone(),
            rx,
            lang: Lang::from_code(&cfg.language),
            light_mode,
            odds_fmt: OddsFormat::from_str_code(&cfg.odds_format),
            modal: None,
            account,
            update_status: None,
            deposit_info: None,
            show_markets_rail: true,
            markets: Vec::new(),
            markets_offset: 0,
            loading_more_markets: false,
            search_query: String::new(),
            category: "all".into(),
            active_market: None,
            active_outcome_idx: 0,
            siblings: None,
            history: Vec::new(),
            chart_interval: "1d",
            chart_mode: ChartMode::Line,
            indicators: IndicatorConfig::default(),
            whale_flow: None,
            ladder_state: LadderState::default(),
            quick_buy: true,
            quick_limit_mode: false,
            quick_amount_input: "25".into(),
            quick_limit_cents: 50.0,
            order_choice: OrderChoice::Gtc,
            stake_value: 25.0,
            stake_in_usd: true,
            timed_fok_ms: 1500,
            right_tab: RightTab::QuickTrade,
            deck_tab: DeckTab::Positions,
            rule_kind: RuleKind::StopLoss,
            rule_side_sell: true,
            rule_trigger_cents: 45.0,
            rule_trail_cents: 3.0,
            rule_size: 25.0,
            rule_slices: 5,
            rule_interval_sec: 5,
            scale_buy: true,
            scale_start_cents: 49.0,
            scale_end_cents: 44.0,
            scale_size: 50.0,
            scale_steps: 5,
            scale_post_only: true,
            copy_wallet: String::new(),
            copy_label: String::new(),
            copy_mult: 0.1,
            copy_max_usd: 25.0,
            paper_reset_input: "1000".into(),
            wallet_setup_tab: WalletSetupTab::CreateNew,
            wallet_sig_type: WalletSignatureType::Eoa,
            wallet_pk_input: String::new(),
            wallet_funder_input: String::new(),
            vault_pass_input: String::new(),
            reveal_pass_input: String::new(),
            revealed_wallet: None,
            wallet_persist_vault: true,
            deposit_wallet_input: String::new(),
            github_repo_input: cfg.github_repo.clone(),
            toasts: Vec::new(),
            last_account_poll: Instant::now(),
        };

        let ctx_rep = cc.egui_ctx.clone();
        let tx_ev = tx.clone();
        let core_ev = core.clone();
        rt.spawn(async move {
            let mut exec_rx = core_ev.execution.subscribe_events();
            let mut auto_rx = core_ev.automation.subscribe_events();
            let mut feed_rx = core_ev.feed.subscribe_events();
            loop {
                tokio::select! {
                    Ok(_) = feed_rx.recv() => { ctx_rep.request_repaint(); }
                    Ok(ev) = exec_rx.recv() => {
                        match ev {
                            polydesk::execution::ExecEvent::AccountUpdated => {
                                let acc = core_ev.execution.account_state().await;
                                let _ = tx_ev.send(UiMsg::AccountUpdated(acc));
                            }
                            polydesk::execution::ExecEvent::OrderAck { ack } => {
                                let msg = format!(
                                    "{} {:.1} @ {:.1}¢ · {} ({:.0}µs / {:.1}ms)",
                                    ack.side, ack.size, ack.price * 100.0, ack.status, ack.sign_us, ack.rtt_ms
                                );
                                let _ = tx_ev.send(UiMsg::Toast { text: msg, is_err: ack.error.is_some() });
                            }
                            polydesk::execution::ExecEvent::Fill { fill } => {
                                let _ = tx_ev.send(UiMsg::Toast {
                                    text: format!("FILLED {} {:.1} @ {:.1}¢", fill.side, fill.shares, fill.price * 100.0),
                                    is_err: false,
                                });
                            }
                        }
                        ctx_rep.request_repaint();
                    }
                    Ok(ev) = auto_rx.recv() => {
                        match ev {
                            polydesk::automation::AutoEvent::RuleTriggered { message, .. }
                            | polydesk::automation::AutoEvent::CopyExecuted { message, .. } => {
                                let _ = tx_ev.send(UiMsg::Toast { text: message, is_err: false });
                            }
                            _ => {}
                        }
                        ctx_rep.request_repaint();
                    }
                }
            }
        });

        app.trigger_load_markets();
        if !cfg.github_repo.is_empty() && cfg.auto_check_updates {
            app.trigger_check_update();
        }
        app
    }

    pub(crate) fn push_toast(&mut self, text: String, is_err: bool) {
        self.toasts.push((text, is_err, Instant::now()));
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    fn active_asset_id(&self) -> Option<&str> {
        let m = self.active_market.as_ref()?;
        m.token_ids.get(self.active_outcome_idx).or_else(|| m.token_ids.first()).map(|s| s.as_str())
    }

    fn opposite_asset_id(&self) -> Option<&str> {
        let m = self.active_market.as_ref()?;
        if m.token_ids.len() < 2 { return None; }
        let idx = if self.active_outcome_idx == 0 { 1 } else { 0 };
        m.token_ids.get(idx).map(|s| s.as_str())
    }

    fn active_outcome_name(&self) -> String {
        let Some(m) = self.active_market.as_ref() else { return "Yes".into(); };
        m.outcomes.get(self.active_outcome_idx).cloned().unwrap_or_else(|| if self.active_outcome_idx == 0 { "Yes".into() } else { "No".into() })
    }

    fn all_open_orders(&self) -> &[PaperOrder] {
        if self.account.mode == TradingMode::Live { &self.account.live_open_orders } else { &self.account.paper.open_orders }
    }

    fn all_positions(&self) -> &[PaperPosition] {
        if self.account.mode == TradingMode::Live { &self.account.live_positions } else { &self.account.paper.positions }
    }

    fn active_position(&self) -> Option<&PaperPosition> {
        let aid = self.active_asset_id()?;
        self.all_positions().iter().find(|p| p.asset_id == aid && p.shares > 1e-6)
    }

    fn compute_shares(&self, price: f64) -> f64 {
        if !self.stake_in_usd {
            self.stake_value.max(1.0)
        } else {
            ((self.stake_value / price.max(0.005)) * 100.0).round() / 100.0
        }
    }

    // ---------- Bilingual Display & Async Actions ----------

    /// Returns auto-translated Simplified Chinese in 中文 mode (instant O(1) cache hit)
    /// or original English in EN mode.
    fn display_text(&self, en: &str) -> String {
        if self.lang == Lang::Zh {
            self.core
                .translator
                .get_zh(en)
                .unwrap_or_else(|| en.to_string())
        } else {
            en.to_string()
        }
    }

    fn trigger_translate_visible(&self) {
        if self.lang != Lang::Zh {
            return;
        }
        let mut texts = Vec::with_capacity(self.markets.len() * 2 + 24);
        for m in &self.markets {
            texts.push(m.question.clone());
            for o in &m.outcomes {
                texts.push(o.clone());
            }
        }
        if let Some(ref m) = self.active_market {
            texts.push(m.question.clone());
            for o in &m.outcomes {
                texts.push(o.clone());
            }
        }
        if let Some(ref sib) = self.siblings {
            for o in &sib.outcomes {
                texts.push(o.group_title.clone());
                texts.push(o.question.clone());
            }
        }
        for p in self.all_positions() {
            texts.push(p.market_question.clone());
            texts.push(p.outcome.clone());
        }
        for o in self.all_open_orders() {
            texts.push(o.market_question.clone());
        }

        if texts.is_empty() {
            return;
        }
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let added = core.translator.translate_en_to_zh_batch(&texts).await;
            if added > 0 {
                let _ = tx.send(UiMsg::TranslationsReady);
            }
        });
    }

    fn trigger_load_markets(&self) {
        self.trigger_load_markets_page(0, false);
    }

    fn trigger_load_markets_page(&self, offset: usize, append: bool) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let q = self.search_query.clone();
        let cat = if self.category == "watch" { "all".into() } else { self.category.clone() };
        self.rt.spawn(async move {
            match core
                .market_client
                .fetch_markets_page(
                    (!q.trim().is_empty()).then_some(q.as_str()),
                    Some(cat.as_str()),
                    100,
                    offset,
                    &core.translator,
                    false,
                )
                .await
            {
                Ok(ms) => { let _ = tx.send(UiMsg::MarketsLoaded { markets: ms, append }); }
                Err(e) => { let _ = tx.send(UiMsg::Toast { text: format!("Markets error: {e}"), is_err: true }); }
            }
        });
    }

    fn select_market(&mut self, market: MarketSummary, outcome_idx: usize) {
        let tick = market.tick_size.max(0.001);
        let init_px = market.outcome_prices.get(outcome_idx).copied().or(market.last_trade_price).unwrap_or(0.50);
        let init_cents = (init_px * 100.0).clamp(2.0, 98.0);
        self.rule_trigger_cents = (init_cents - 5.0).max(1.0);
        self.scale_start_cents = (init_cents - 1.0).max(1.0);
        self.scale_end_cents = (init_cents - 6.0).max(0.5);
        self.quick_limit_cents = init_cents.round().clamp(1.0, 99.0);
        self.ladder_state.reset_for_market(tick);
        self.active_outcome_idx = outcome_idx;
        self.active_market = Some(market.clone());
        self.siblings = None;
        self.whale_flow = None;
        self.history.clear();

        self.core.feed.subscribe(market.token_ids.clone());
        let core_w = self.core.clone();
        let tids = market.token_ids.clone();
        self.rt.spawn(async move { core_w.execution.prewarm_market(&tids).await; });

        if let Some(aid) = self.active_asset_id().map(|s| s.to_string()) {
            let core_b = self.core.clone();
            let cid = market.condition_id.clone();
            let aid_c = aid.clone();
            self.rt.spawn(async move {
                if let Ok((bids, asks, ts_tick, lt, ts, hash)) = core_b.market_client.fetch_book_rest(&aid_c).await {
                    core_b.feed.seed_book(&aid_c, &cid, ts_tick, &bids, &asks, lt, ts, hash);
                }
            });
            self.trigger_load_history(&aid);
        }

        let core_wh = self.core.clone();
        let tx_wh = self.tx.clone();
        let cid_wh = market.condition_id.clone();
        self.rt.spawn(async move {
            let thresh = core_wh.config.read().whale_threshold_usd;
            if let Ok(wf) = core_wh.market_client.fetch_whale_flow(&cid_wh, thresh).await {
                let _ = tx_wh.send(UiMsg::WhalesLoaded(cid_wh, wf));
            }
        });

        if !market.event_slug.is_empty() {
            let core_s = self.core.clone();
            let tx_s = self.tx.clone();
            let eslug = market.event_slug.clone();
            self.rt.spawn(async move {
                if let Ok(sib) = core_s.market_client.fetch_event_siblings(&eslug).await {
                    let _ = tx_s.send(UiMsg::SiblingsLoaded(eslug, sib));
                }
            });
        }
    }

    fn trigger_load_history(&self, aid: &str) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let tok = aid.to_string();
        let iv = self.chart_interval;
        let fid = if iv == "1h" { 1 } else if iv == "6h" { 2 } else { 10 };
        self.rt.spawn(async move {
            if let Ok(pts) = core.market_client.fetch_price_history(&tok, iv, fid).await {
                let _ = tx.send(UiMsg::HistoryLoaded(tok, pts));
            }
        });
    }

    fn place_order(&self, side: &str, raw_price: f64, choice: OrderChoice) {
        let Some(m) = self.active_market.as_ref() else { return; };
        let tick_ticks = ((m.tick_size * 10_000.0).round() as usize).max(1);
        let price = align_to_tick(raw_price, tick_ticks);
        let size = self.compute_shares(price);
        self.place_order_exact(side, price, size, choice);
    }

    fn place_order_exact(&self, side: &str, raw_price: f64, shares: f64, choice: OrderChoice) {
        let Some(m) = self.active_market.as_ref() else { return; };
        let Some(aid) = self.active_asset_id() else { return; };
        let tick_ticks = ((m.tick_size * 10_000.0).round() as usize).max(1);
        let price = align_to_tick(raw_price, tick_ticks);
        let size = (shares * 100.0).round() / 100.0;
        if size < 0.01 { return; }
        let post_only = choice == OrderChoice::PostOnly;
        let order_type = match choice {
            OrderChoice::Gtc | OrderChoice::PostOnly => "GTC",
            OrderChoice::Fak => "FAK",
            OrderChoice::Fok => "FOK",
            OrderChoice::TimedFok => "TIMED_FOK",
            OrderChoice::Sweep => "SWEEP",
        }
        .to_string();

        let req = OrderRequest {
            asset_id: aid.to_string(),
            condition_id: m.condition_id.clone(),
            market_question: m.question.clone(),
            outcome: self.active_outcome_name(),
            side: side.to_string(),
            order_type,
            post_only,
            price,
            size,
            expiration_secs: None,
            timed_fok_ms: (choice == OrderChoice::TimedFok).then_some(self.timed_fok_ms),
            max_slippage_bps: (choice == OrderChoice::Sweep).then_some(500.0),
            fees_enabled: Some(m.fees_enabled),
        };
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            if let Err(e) = core.execution.place_order(req).await {
                let _ = tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
            }
        });
    }

    fn trigger_green_up(&self, strategy: &'static str) {
        let Some(aid) = self.active_asset_id().map(|s| s.to_string()) else { return; };
        let opp = self.opposite_asset_id().map(|s| s.to_string());
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            if let Err(e) = core.execution.execute_green_up(&aid, opp.as_deref(), strategy).await {
                let _ = tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
            }
        });
    }

    fn trigger_flatten(&self) {
        let Some(pos) = self.active_position().cloned() else { return; };
        let Some(m) = self.active_market.clone() else { return; };
        let px = self.core.feed.get_book_view(&pos.asset_id, 5).and_then(|b| b.best_bid.or(b.mid)).unwrap_or(0.01);
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            if let Err(e) = core.execution.place_order(OrderRequest {
                asset_id: pos.asset_id,
                condition_id: pos.condition_id,
                market_question: pos.market_question,
                outcome: pos.outcome,
                side: "SELL".into(),
                order_type: "SWEEP".into(),
                post_only: false,
                price: px,
                size: pos.shares,
                expiration_secs: None,
                timed_fok_ms: None,
                max_slippage_bps: Some(600.0),
                fees_enabled: Some(m.fees_enabled),
            }).await {
                let _ = tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
            }
        });
    }

    pub(crate) fn trigger_create_new_wallet(&self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let pass = self.vault_pass_input.clone();
        let persist = self.wallet_persist_vault;
        self.rt.spawn(async move {
            match core.execution.create_and_unlock_new_wallet(pass, persist).await {
                Ok((wallet, account, deposit)) => {
                    let _ = tx.send(UiMsg::WalletCreated { wallet, account, deposit });
                    let _ = tx.send(UiMsg::Toast {
                        text: "Created new local wallet & fetched USDT/USDC deposit addresses!".into(),
                        is_err: false,
                    });
                }
                Err(e) => {
                    let _ = tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
                }
            }
        });
    }

    pub(crate) fn trigger_reveal_backup(&self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let pass = self.reveal_pass_input.clone();
        self.rt.spawn(async move {
            match core.execution.reveal_vault_backup(&pass) {
                Ok(w) => {
                    let _ = tx.send(UiMsg::WalletRevealed(w));
                }
                Err(e) => {
                    let _ = tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
                }
            }
        });
    }

    pub(crate) fn trigger_connect_wallet(&self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let pk = self.wallet_pk_input.clone();
        let funder = (!self.wallet_funder_input.trim().is_empty()).then(|| self.wallet_funder_input.clone());
        let st = self.wallet_sig_type;
        let pass = self.vault_pass_input.clone();
        let persist = self.wallet_persist_vault;
        self.rt.spawn(async move {
            match core.execution.save_and_unlock_wallet(pk, funder, st, pass, persist).await {
                Ok(acc) => {
                    let _ = tx.send(UiMsg::AccountUpdated(acc));
                    let _ = tx.send(UiMsg::Toast { text: "Live Polymarket CLOB V2 wallet unlocked!".into(), is_err: false });
                }
                Err(e) => { let _ = tx.send(UiMsg::Toast { text: e.to_string(), is_err: true }); }
            }
        });
    }

    pub(crate) fn trigger_unlock_vault(&self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let pass = self.vault_pass_input.clone();
        self.rt.spawn(async move {
            match core.execution.unlock_existing_vault(&pass).await {
                Ok(acc) => {
                    let _ = tx.send(UiMsg::AccountUpdated(acc));
                    let _ = tx.send(UiMsg::Toast { text: "Vault unlocked — LIVE active".into(), is_err: false });
                }
                Err(e) => { let _ = tx.send(UiMsg::Toast { text: e.to_string(), is_err: true }); }
            }
        });
    }

    pub(crate) fn trigger_lock_wallet(&self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let acc = core.execution.lock_wallet().await;
            let _ = tx.send(UiMsg::AccountUpdated(acc));
        });
    }

    pub(crate) fn trigger_wipe_vault(&self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            match core.execution.wipe_vault().await {
                Ok(acc) => {
                    let _ = tx.send(UiMsg::AccountUpdated(acc));
                    let _ = tx.send(UiMsg::Toast { text: "Vault deleted".into(), is_err: false });
                }
                Err(e) => { let _ = tx.send(UiMsg::Toast { text: e.to_string(), is_err: true }); }
            }
        });
    }

    pub(crate) fn trigger_sync_allowance(&self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            match core.execution.sync_balance_allowance().await {
                Ok(acc) => {
                    let _ = tx.send(UiMsg::AccountUpdated(acc));
                    let _ = tx.send(UiMsg::Toast { text: "Synced live pUSD balance & allowance!".into(), is_err: false });
                }
                Err(e) => { let _ = tx.send(UiMsg::Toast { text: e.to_string(), is_err: true }); }
            }
        });
    }

    pub(crate) fn trigger_fetch_deposit(&self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let addr = self.deposit_wallet_input.clone();
        self.rt.spawn(async move {
            match core.execution.fetch_deposit_addresses(&addr).await {
                Ok(info) => { let _ = tx.send(UiMsg::DepositLoaded(info)); }
                Err(e) => { let _ = tx.send(UiMsg::Toast { text: e.to_string(), is_err: true }); }
            }
        });
    }

    pub(crate) fn trigger_check_update(&self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let repo = self.github_repo_input.clone();
        self.rt.spawn(async move {
            let st = check_github_release(core.market_client.http(), &repo).await;
            let _ = tx.send(UiMsg::UpdateChecked(st));
        });
    }

    pub(crate) fn trigger_apply_update(&self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let repo = self.github_repo_input.clone();
        self.rt.spawn(async move {
            match download_and_apply_update(core.market_client.http(), &repo).await {
                Ok(ver) => {
                    let _ = tx.send(UiMsg::Toast { text: format!("Verified SHA-256 & updated to v{ver}! Restarting..."), is_err: false });
                    tokio::time::sleep(Duration::from_millis(600)).await;
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = std::process::Command::new(exe).spawn();
                    }
                    std::process::exit(0);
                }
                Err(e) => { let _ = tx.send(UiMsg::Toast { text: e.to_string(), is_err: true }); }
            }
        });
    }

    fn drain_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                UiMsg::MarketsLoaded { markets: ms, append } => {
                    self.loading_more_markets = false;
                    let need_select = self.active_market.is_none();
                    if append {
                        for m in ms {
                            if !self.markets.iter().any(|x| x.condition_id == m.condition_id) {
                                self.markets.push(m);
                            }
                        }
                    } else {
                        self.markets = ms;
                        self.markets_offset = 0;
                    }
                    if need_select && let Some(first) = self.markets.first().cloned() {
                        self.select_market(first, 0);
                    }
                    self.trigger_translate_visible();
                }
                UiMsg::HistoryLoaded(tok, pts) => {
                    if self.active_asset_id() == Some(tok.as_str()) { self.history = pts; }
                }
                UiMsg::WhalesLoaded(cid, wf) => {
                    if self.active_market.as_ref().is_some_and(|m| m.condition_id == cid) { self.whale_flow = Some(wf); }
                }
                UiMsg::SiblingsLoaded(eslug, sib) => {
                    if self.active_market.as_ref().is_some_and(|m| m.event_slug == eslug) && sib.outcomes.len() >= 2 {
                        self.siblings = Some(sib);
                        self.trigger_translate_visible();
                    }
                }
                UiMsg::TranslationsReady => {}
                UiMsg::AccountUpdated(acc) => { self.account = acc; }
                UiMsg::WalletCreated { wallet, account, deposit } => {
                    self.deposit_wallet_input = wallet.address.clone();
                    self.revealed_wallet = Some(wallet);
                    self.account = account;
                    if let Some(dep) = deposit {
                        self.deposit_info = Some(dep);
                    }
                    self.vault_pass_input.clear();
                }
                UiMsg::WalletRevealed(w) => {
                    self.revealed_wallet = Some(w);
                    self.reveal_pass_input.clear();
                }
                UiMsg::DepositLoaded(dep) => { self.deposit_info = Some(dep); }
                UiMsg::UpdateChecked(st) => { self.update_status = Some(st); }
                UiMsg::Toast { text, is_err } => { self.push_toast(text, is_err); }
            }
        }
        self.toasts.retain(|(_, _, t0)| t0.elapsed() < Duration::from_secs(4));

        if let Some(aid) = self.active_asset_id()
            && let Some(b) = self.core.feed.get_book_view(aid, 5)
            && let Some(mid) = b.mid
        {
            let now_sec = chrono::Utc::now().timestamp();
            if let Some(last) = self.history.last_mut() {
                // Coalesce live ticks into 60s buckets so chart stays clean with true time spacing!
                if now_sec - last.t < 60 {
                    last.p = mid;
                } else {
                    self.history.push(PricePoint { t: now_sec, p: mid });
                }
            }
        }

        if self.last_account_poll.elapsed() > Duration::from_secs(3) {
            self.last_account_poll = Instant::now();
            let core = self.core.clone();
            let tx = self.tx.clone();
            self.rt.spawn(async move {
                let acc = core.execution.account_state().await;
                let _ = tx.send(UiMsg::AccountUpdated(acc));
            });
        }
    }

    fn handle_hotkeys(&mut self, ctx: &Context) {
        if ctx.egui_wants_keyboard_input() || self.modal.is_some() { return; }
        let book = self.active_asset_id().and_then(|a| self.core.feed.get_book_view(a, 5));
        let (shift, space, o, l, b, s, x, g, f, digit) = ctx.input(|i| {
            let d = if i.key_pressed(Key::Num1) { Some(0) }
                else if i.key_pressed(Key::Num2) { Some(1) }
                else if i.key_pressed(Key::Num3) { Some(2) }
                else if i.key_pressed(Key::Num4) { Some(3) }
                else if i.key_pressed(Key::Num5) { Some(4) }
                else { None };
            (i.modifiers.shift, i.key_pressed(Key::Space), i.key_pressed(Key::O), i.key_pressed(Key::L),
             i.key_pressed(Key::B), i.key_pressed(Key::S), i.key_pressed(Key::X), i.key_pressed(Key::G),
             i.key_pressed(Key::F), d)
        });

        if space { self.ladder_state.recenter(book.as_ref()); }
        if o {
            self.odds_fmt = self.odds_fmt.cycle();
            let code = self.odds_fmt.as_str_code().to_string();
            let _ = self.core.execution.update_config_and_save(|c| c.odds_format = code);
        }
        if l {
            self.lang = self.lang.toggle();
            let code = self.lang.as_code().to_string();
            let _ = self.core.execution.update_config_and_save(|c| c.language = code);
            self.trigger_translate_visible();
        }
        if b && let Some(ref bk) = book {
            if shift && let Some(ask) = bk.best_ask { self.place_order("BUY", ask, OrderChoice::Fak); }
            else if let Some(bid) = bk.best_bid { self.place_order("BUY", bid, self.order_choice); }
        }
        if s && let Some(ref bk) = book {
            if shift && let Some(bid) = bk.best_bid { self.place_order("SELL", bid, OrderChoice::Fak); }
            else if let Some(ask) = bk.best_ask { self.place_order("SELL", ask, self.order_choice); }
        }
        if x {
            let core = self.core.clone();
            if shift {
                self.rt.spawn(async move { let _ = core.execution.cancel_all().await; });
                self.push_toast("Cancelled all open orders".into(), false);
            } else if let Some(aid) = self.active_asset_id().map(|s| s.to_string()) {
                self.rt.spawn(async move { let _ = core.execution.cancel_market(&aid).await; });
            }
        }
        if g { self.trigger_green_up("close"); }
        if f { self.trigger_flatten(); }
        if let Some(idx) = digit {
            let presets = self.core.config.read().stake_presets.clone();
            if let Some(&v) = presets.get(idx) { self.stake_value = v; }
        }
    }
}

impl eframe::App for PolyDeskDesktop {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_messages();
        self.handle_hotkeys(&ctx);

        let l = self.lang;
        let pal = self.palette();
        let telem = self.core.telemetry.snapshot();
        let active_aid = self.active_asset_id().map(|s| s.to_string());
        let active_book = active_aid.as_deref().and_then(|a| self.core.feed.get_book_view(a, 60));

        // 1. MODERN 58px TOP BAR WITH PROMINENT PORTFOLIO & CASH BALANCE (Light + Dark!)
        let top_frame = Frame::NONE
            .fill(pal.bg_panel)
            .stroke(Stroke::new(1.0, pal.border))
            .inner_margin(Margin::symmetric(16, 8));

        egui::Panel::top("top_bar").exact_size(58.0).frame(top_frame).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;

                if ui.button(if self.show_markets_rail { "◀" } else { "▶" }).on_hover_text(tr(l, "Toggle Markets", "展开/折叠市场栏")).clicked() {
                    self.show_markets_rail = !self.show_markets_rail;
                }

                let ws_col = if telem.ws_connected { pal.green } else { pal.red };
                ui.label(RichText::new("●").color(ws_col).size(11.0));
                ui.label(RichText::new("PolyDesk").strong().size(17.5).color(pal.text_primary));

                // Mode badge (PAPER / LIVE)
                let is_live = self.account.mode == TradingMode::Live;
                let mode_col = if is_live { pal.green } else { pal.amber };
                let mode_bg = if is_live { Color32::from_rgba_unmultiplied(16, 185, 129, 26) } else { Color32::from_rgba_unmultiplied(245, 158, 11, 24) };
                if ui.add(
                    egui::Button::new(RichText::new(if is_live { tr(l, "LIVE", "实盘") } else { tr(l, "PAPER", "模拟盘") }).strong().size(11.0).color(mode_col))
                        .fill(mode_bg)
                        .stroke(Stroke::new(1.0, mode_col))
                        .corner_radius(CornerRadius::same(16)),
                ).clicked() {
                    self.modal = Some(ActiveModal::WalletAndDeposit);
                }

                // Prominent Polymarket-Style Top Balance Bar: Portfolio | Cash | P&L (locked widths -> zero jitter!)
                let cash = if is_live { self.account.live_balance_usd.unwrap_or(0.0) } else { self.account.paper.cash_usd };
                let mut pos_val = 0.0;
                let mut upnl = 0.0;
                for p in self.all_positions() {
                    let mk = self.core.feed.get_book_view(&p.asset_id, 1).and_then(|b| b.mid.or(b.best_bid)).unwrap_or(p.avg_entry);
                    pos_val += p.shares * mk;
                    upnl += p.shares * (mk - p.avg_entry);
                }
                let portfolio = cash + pos_val;
                let pnl = if is_live { upnl } else { self.account.paper.realized_pnl_usd + upnl };
                let pnl_col = if pnl >= 0.0 { pal.green } else { pal.red };

                pal.pill_frame().show(ui, |ui| {
                    ui.set_width(356.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            ui.label(RichText::new(tr(l, "Portfolio", "总资产")).small().color(pal.text_muted));
                            ui.label(RichText::new(format!("{:>9}", format_usd(portfolio, false))).monospace().strong().size(13.5).color(pal.text_primary));
                        });
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            ui.label(RichText::new(tr(l, "Cash", "余额")).small().color(pal.text_muted));
                            ui.label(RichText::new(format!("{:>9}", format_usd(cash, false))).monospace().strong().size(13.5).color(pal.green));
                        });
                        ui.label(RichText::new(format!("{:>8}", format_usd(pnl, true))).monospace().strong().size(12.5).color(pnl_col));
                    });
                });

                // Subtle compact latency indicator
                let clob_ms = if telem.warm_ping_ms.p50 > 0.0 { telem.warm_ping_ms.p50 } else { telem.order_rtt_ms.last };
                let clob_col = if clob_ms <= 0.0 { pal.text_muted } else if clob_ms < 60.0 { pal.green } else if clob_ms < 200.0 { pal.amber } else { pal.text_muted };
                Frame::NONE
                    .fill(pal.bg_sub)
                    .stroke(Stroke::new(1.0, pal.border))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_width(136.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            ui.label(RichText::new(format!("WS {:>3.0}ms", telem.ws_lag_ms.p50.min(999.0))).monospace().small().color(pal.text_muted));
                            ui.label(RichText::new("·").color(pal.border_strong));
                            ui.label(RichText::new(format!("{:>3.0}ms", clob_ms.min(999.0))).monospace().small().color(clob_col));
                        });
                    })
                    .response
                    .on_hover_text(format!(
                        "CLOB RTT: {:.0} ms | Book apply: {:.0} µs | EIP-712 Sign: {:.0} µs | WS msgs: {}",
                        clob_ms, telem.book_apply_us.p50, telem.sign_us.last, telem.ws_msgs_total
                    ));

                // Right actions — sleek modern buttons including 1-click Light / Dark mode toggle!
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 7.0;

                    if ui.add(
                        egui::Button::new(RichText::new(tr(l, "Cancel All", "全撤")).strong().size(12.0).color(pal.red_text))
                            .fill(Color32::from_rgba_unmultiplied(244, 63, 94, if pal.light { 22 } else { 26 }))
                            .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(244, 63, 94, 110)))
                            .corner_radius(CornerRadius::same(9)),
                    ).clicked() {
                        let core = self.core.clone();
                        self.rt.spawn(async move { let _ = core.execution.cancel_all().await; });
                    }

                    let upd_dot = self.update_status.as_ref().is_some_and(|u| u.update_available);
                    let settings_lbl = if upd_dot {
                        tr(l, "Updates ↑", "更新 ↑")
                    } else {
                        tr(l, "Settings", "设置")
                    };
                    if ui.button(RichText::new(settings_lbl).size(12.0).color(if upd_dot { pal.green } else { pal.text_secondary })).on_hover_text(tr(l, "Updates & Keyboard Hotkeys", "自动更新与快捷键")).clicked() {
                        self.modal = Some(ActiveModal::SettingsAndUpdates);
                    }

                    // Primary vibrant blue Deposit / Wallet button
                    if ui.add(
                        egui::Button::new(RichText::new(tr(l, "+ Deposit / Wallet", "+ 充值 / 钱包")).strong().size(12.5).color(Color32::WHITE))
                            .fill(pal.blue)
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(9)),
                    ).clicked() {
                        self.modal = Some(ActiveModal::WalletAndDeposit);
                    }

                    // 1-Click Light / Dark Mode Switcher!
                    let theme_lbl = if self.light_mode {
                        tr(l, "Dark", "深色")
                    } else {
                        tr(l, "Light", "浅色")
                    };
                    if ui.button(RichText::new(theme_lbl).size(12.0)).on_hover_text(tr(l, "Switch Light / Dark theme", "切换浅色 / 深色主题")).clicked() {
                        self.light_mode = !self.light_mode;
                        apply_theme(&ctx, self.light_mode);
                        let mode_str = if self.light_mode { "light" } else { "dark" }.to_string();
                        let _ = self.core.execution.update_config_and_save(|cfg| cfg.theme = mode_str);
                    }

                    if ui.button(if l == Lang::En { "中文" } else { "EN" }).clicked() {
                        self.lang = self.lang.toggle();
                        let c = self.lang.as_code().to_string();
                        let _ = self.core.execution.update_config_and_save(|cfg| cfg.language = c);
                        self.trigger_translate_visible();
                    }

                    if ui.button(RichText::new(self.odds_fmt.short_label()).monospace()).on_hover_text(tr(l, "Odds format (O)", "切换赔率格式 (O)")).clicked() {
                        self.odds_fmt = self.odds_fmt.cycle();
                        let c = self.odds_fmt.as_str_code().to_string();
                        let _ = self.core.execution.update_config_and_save(|cfg| cfg.odds_format = c);
                    }
                });
            });
        });

        // 2. COLLAPSIBLE LEFT MARKET RAIL (Modern rounded cards + ALL Polymarket pagination!)
        if self.show_markets_rail {
            let rail_frame = Frame::NONE
                .fill(pal.bg_panel)
                .stroke(Stroke::new(1.0, pal.border))
                .inner_margin(Margin::same(12));

            egui::Panel::left("markets_panel").exact_size(286.0).resizable(false).frame(rail_frame).show(ui, |ui| {
                ui.set_max_width(262.0);
                ui.horizontal(|ui| {
                    let has_q = !self.search_query.is_empty();
                    let extra_w = if has_q { 78.0 } else { 54.0 };
                    let resp = ui.add(
                        TextEdit::singleline(&mut self.search_query)
                            .hint_text(tr(l, "Search all markets (EN / 中文)...", "搜索全站市场 (中/英文)..."))
                            .desired_width((ui.available_width() - extra_w).max(120.0)),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        self.trigger_load_markets();
                    }
                    if has_q && ui.small_button("×").on_hover_text(tr(l, "Clear", "清空")).clicked() {
                        self.search_query.clear();
                        self.trigger_load_markets();
                    }
                    if ui.button(tr(l, "Go", "搜索")).clicked() {
                        self.trigger_load_markets();
                    }
                });

                ui.add_space(7.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(4.0, 5.0);
                    for (id, en, zh) in [
                        ("all", "All", "全部"),
                        ("sports", "Sports", "体育"),
                        ("crypto", "Crypto", "加密"),
                        ("politics", "Politics", "政治"),
                        ("finance", "Finance", "金融"),
                        ("tech", "Tech", "科技"),
                        ("watch", "★", "★"),
                    ] {
                        if ui.selectable_label(self.category == id, RichText::new(tr(l, en, zh)).small()).clicked() {
                            self.category = id.into();
                            self.trigger_load_markets();
                        }
                    }
                });

                ui.add_space(8.0);
                let sib_reserve = if self.siblings.is_some() { 210.0 } else { 0.0 };
                let wl: HashSet<String> = self.core.config.read().watchlist.iter().cloned().collect();
                let mut clicked_market: Option<MarketSummary> = None;
                let mut toggle_star: Option<String> = None;
                let mut want_load_more = false;

                ScrollArea::vertical().id_salt("mkt_scroll").max_height((ui.available_height() - sib_reserve).max(160.0)).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 7.0;
                    for m in &self.markets {
                        if self.category == "watch" && !wl.contains(&m.condition_id) { continue; }
                        let sel = self.active_market.as_ref().is_some_and(|a| a.condition_id == m.condition_id);
                        let fill = if sel {
                            Color32::from_rgba_unmultiplied(46, 124, 246, if pal.light { 24 } else { 26 })
                        } else {
                            pal.bg_sub
                        };
                        let stroke = if sel { pal.blue } else { pal.border };

                        Frame::NONE
                            .fill(fill)
                            .stroke(Stroke::new(1.0, stroke))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::same(10))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let starred = wl.contains(&m.condition_id);
                                    if ui.add(egui::Button::new(RichText::new("★").color(if starred { pal.amber } else { pal.text_muted })).frame(false)).clicked() {
                                        toggle_star = Some(m.condition_id.clone());
                                    }
                                    let display_q = self.display_text(&m.question);
                                    let q_resp = ui.add(
                                        egui::Label::new(RichText::new(&display_q).size(12.5).color(pal.text_primary))
                                            .wrap()
                                            .sense(egui::Sense::click()),
                                    );
                                    let q_resp = if self.lang == Lang::Zh && display_q != m.question {
                                        q_resp.on_hover_text(format!("{display_q}\n{}", m.question))
                                    } else {
                                        q_resp
                                    };
                                    if q_resp.clicked() {
                                        clicked_market = Some(m.clone());
                                    }
                                });
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    let yp = m.outcome_prices.first().copied().or(m.last_trade_price);
                                    let np = m.outcome_prices.get(1).copied().or_else(|| yp.map(|y| 1.0 - y));
                                    ui.label(RichText::new(format_odds(yp, self.odds_fmt, m.tick_size)).monospace().strong().size(12.0).color(pal.green));
                                    ui.label(RichText::new("/").color(pal.text_muted));
                                    ui.label(RichText::new(format_odds(np, self.odds_fmt, m.tick_size)).monospace().strong().size(12.0).color(pal.red));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(RichText::new(format!("{} Vol", format_usd(m.volume_24hr, false))).monospace().small().color(pal.text_muted));
                                    });
                                });
                            });
                    }

                    // Paginated Load More across ALL active Polymarket markets
                    if self.category != "watch" && self.search_query.trim().is_empty() && !self.markets.is_empty() {
                        ui.add_space(4.0);
                        let lbl = if self.loading_more_markets {
                            tr(l, "Loading more markets...", "正在加载更多市场...")
                        } else {
                            tr(l, "Load More Markets (+100)", "加载更多市场 (+100)")
                        };
                        if ui.add_sized(
                            Vec2::new(ui.available_width(), 30.0),
                            egui::Button::new(RichText::new(lbl).small().color(pal.text_secondary))
                                .fill(pal.bg_chip)
                                .corner_radius(CornerRadius::same(8)),
                        ).clicked() && !self.loading_more_markets {
                            want_load_more = true;
                        }
                    }
                });

                if want_load_more {
                    self.markets_offset += 100;
                    self.loading_more_markets = true;
                    self.trigger_load_markets_page(self.markets_offset, true);
                }

                if let Some(cid) = toggle_star {
                    let _ = self.core.execution.update_config_and_save(|c| {
                        if let Some(pos) = c.watchlist.iter().position(|x| x == &cid) { c.watchlist.remove(pos); }
                        else { c.watchlist.push(cid); }
                    });
                }
                if let Some(m) = clicked_market { self.select_market(m, 0); }

                if let Some(sib) = self.siblings.clone() {
                    ui.add_space(8.0);
                    pal.surface_card().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(tr(l, "Siblings", "关联子市场")).strong().small().color(Color32::from_rgb(56, 189, 248)));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let ec = if sib.dutch_book_edge_bps > 0.0 { Color32::from_rgb(16, 185, 129) } else { Color32::GRAY };
                                ui.label(RichText::new(format!("Σ{:.1}¢ ({:+.0}bp)", sib.sum_yes_asks * 100.0, sib.dutch_book_edge_bps)).monospace().small().color(ec));
                            });
                        });
                        ScrollArea::vertical().id_salt("sib_scroll").max_height(88.0).show(ui, |ui| {
                            for o in sib.outcomes.iter().take(8) {
                                let sib_lbl = self.display_text(&o.group_title);
                                ui.horizontal(|ui| {
                                    if ui.selectable_label(self.active_market.as_ref().is_some_and(|m| m.condition_id == o.condition_id), &sib_lbl).on_hover_text(&o.group_title).clicked() {
                                        let syn = MarketSummary {
                                            id: o.market_id.clone(), question: o.question.clone(), question_zh: None, slug: sib.event_slug.clone(),
                                            event_slug: sib.event_slug.clone(), event_title: sib.event_title.clone(),
                                            condition_id: o.condition_id.clone(), outcomes: vec!["Yes".into(), "No".into()], outcomes_zh: Vec::new(),
                                            token_ids: vec![o.yes_token_id.clone(), o.no_token_id.clone()],
                                            outcome_prices: vec![o.yes_price, o.no_price], tick_size: o.tick_size,
                                            min_order_size: 5.0, volume_24hr: 0.0, liquidity: o.liquidity,
                                            best_bid: o.best_bid, best_ask: o.best_ask, last_trade_price: Some(o.yes_price),
                                            one_day_change: None, neg_risk: sib.neg_risk, fees_enabled: true,
                                            accepting_orders: true, end_date: None, image: None, category: "other".into(),
                                        };
                                        self.select_market(syn, 0);
                                    }
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(RichText::new(format_odds(o.best_ask.or(Some(o.yes_price)), self.odds_fmt, o.tick_size)).monospace().small().color(Color32::from_rgb(16, 185, 129)));
                                    });
                                });
                            }
                        });
                        if ui.button(RichText::new(tr(l, "Auto-Hedge Siblings", "一键对冲子市场")).small().color(Color32::from_rgb(251, 191, 36))).clicked() {
                            let core = self.core.clone();
                            let tx = self.tx.clone();
                            let outs: Vec<_> = sib.outcomes.iter().take(8).cloned().collect();
                            let sh = self.stake_value.max(5.0);
                            self.rt.spawn(async move {
                                match core.execution.execute_sibling_hedge(&outs, sh).await {
                                    Ok(acks) => { let _ = tx.send(UiMsg::Toast { text: format!("Auto-Hedge fired {} legs", acks.len()), is_err: false }); }
                                    Err(e) => { let _ = tx.send(UiMsg::Toast { text: e.to_string(), is_err: true }); }
                                }
                            });
                        }
                    });
                }
            });
        }

        // 3. SPACIOUS MODERN RIGHT EXECUTION COLUMN (426px — Quick Buy/Sell | Ladder | Pro Orders!)
        let right_frame = Frame::NONE
            .fill(pal.bg_panel)
            .stroke(Stroke::new(1.0, pal.border))
            .inner_margin(Margin::same(14));

        egui::Panel::right("exec_panel").exact_size(426.0).resizable(false).frame(right_frame).show(ui, |ui| {
            ui.set_max_width(398.0);
            // Clean 3-Way Segmented Switcher: Quick Trade | Price Ladder | Pro Orders
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.selectable_value(&mut self.right_tab, RightTab::QuickTrade, RichText::new(tr(l, "Buy / Sell", "快捷买卖")).strong().size(12.5));
                ui.selectable_value(&mut self.right_tab, RightTab::Ladder, RichText::new(tr(l, "Ladder", "盘口")).strong().size(12.5));
                ui.selectable_value(&mut self.right_tab, RightTab::ProOrders, RichText::new(tr(l, "Pro", "条件单")).strong().size(12.5));
                if self.right_tab == RightTab::Ladder {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.selectable_label(self.ladder_state.auto_center, tr(l, "Auto", "跟踪")).clicked() {
                            self.ladder_state.auto_center = !self.ladder_state.auto_center;
                        }
                        if ui.small_button(tr(l, "Center", "居中")).clicked() {
                            self.ladder_state.recenter(active_book.as_ref());
                        }
                        egui::ComboBox::from_id_salt("tick_step_cb")
                            .width(50.0)
                            .selected_text(format!("{}¢", self.ladder_state.step * 100.0))
                            .show_ui(ui, |ui| {
                                for s in [0.001, 0.005, 0.01, 0.05] {
                                    if ui.selectable_value(&mut self.ladder_state.step, s, format!("{}¢", s * 100.0)).clicked() {
                                        self.ladder_state.recenter(active_book.as_ref());
                                    }
                                }
                            });
                    });
                }
            });

            ui.add_space(10.0);

            match self.right_tab {
                RightTab::QuickTrade => {
                    ScrollArea::vertical().id_salt("quick_trade_scroll").show(ui, |ui| {
                        self.draw_quick_trade_panel(ui, active_book.as_ref());
                    });
                }
                RightTab::Ladder => {
                    // Fixed-height Controls Card (ZERO reflow or height jitter on ticks!)
                    pal.surface_card().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        // Row 1: Order Types (single non-wrapping line)
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for (ch, lbl, tip) in [
                                (OrderChoice::Gtc, "GTC", "Good-Til-Cancelled"),
                                (OrderChoice::PostOnly, "Post", "Post-Only Maker"),
                                (OrderChoice::Fak, "FAK", "Fill-And-Kill"),
                                (OrderChoice::Fok, "FOK", "Fill-Or-Kill"),
                                (OrderChoice::TimedFok, "tFOK", "Timed Fill-Or-Kill"),
                                (OrderChoice::Sweep, "Sweep", "Depth Sweep"),
                            ] {
                                ui.selectable_value(&mut self.order_choice, ch, RichText::new(lbl).monospace().size(11.0))
                                    .on_hover_text(tip);
                            }
                            if self.order_choice == OrderChoice::TimedFok {
                                ui.add(egui::DragValue::new(&mut self.timed_fok_ms).range(150..=60000).suffix("ms"));
                            }
                        });

                        ui.add_space(6.0);

                        // Row 2: Stake presets
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            if ui.button(if self.stake_in_usd { "$" } else { "SH" }).clicked() {
                                self.stake_in_usd = !self.stake_in_usd;
                            }
                            ui.add(egui::DragValue::new(&mut self.stake_value).range(1.0..=100_000.0).speed(1.0));
                            let presets = self.core.config.read().stake_presets.clone();
                            for v in presets {
                                let txt = if self.stake_in_usd { format!("${v:.0}") } else { format!("{v:.0}") };
                                if ui.selectable_label((self.stake_value - v).abs() < 0.01, RichText::new(txt).monospace()).clicked() {
                                    self.stake_value = v;
                                }
                            }
                        });

                        ui.add_space(6.0);

                        // Row 3: Quick Actions
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            if ui.add(egui::Button::new(RichText::new(tr(l, "Buy Bid", "买一")).strong().color(Color32::from_rgb(16, 185, 129))).fill(Color32::from_rgba_unmultiplied(16, 185, 129, 24))).clicked()
                                && let Some(p) = active_book.as_ref().and_then(|b| b.best_bid.or(b.mid))
                            {
                                self.place_order("BUY", p, self.order_choice);
                            }
                            if ui.add(egui::Button::new(RichText::new(tr(l, "Sell Ask", "卖一")).strong().color(Color32::from_rgb(244, 63, 94))).fill(Color32::from_rgba_unmultiplied(244, 63, 94, 24))).clicked()
                                && let Some(p) = active_book.as_ref().and_then(|b| b.best_ask.or(b.mid))
                            {
                                self.place_order("SELL", p, self.order_choice);
                            }
                            if ui.button(RichText::new(tr(l, "Green-Up", "锁盈")).color(Color32::from_rgb(251, 191, 36))).clicked() {
                                self.trigger_green_up("close");
                            }
                            if ui.button(tr(l, "Flat", "平仓")).clicked() {
                                self.trigger_flatten();
                            }
                            if ui.button(tr(l, "Cxl", "撤单")).clicked()
                                && let Some(ref aid) = active_aid
                            {
                                let core = self.core.clone();
                                let aid_c = aid.clone();
                                self.rt.spawn(async move { let _ = core.execution.cancel_market(&aid_c).await; });
                            }
                        });

                        ui.add_space(6.0);

                        // Fixed single-line Spread & Transparent Fee readout (truncated — NEVER wraps or expands!)
                        let ref_px = active_book.as_ref().and_then(|b| b.mid).unwrap_or(0.5);
                        let est_sh = self.compute_shares(ref_px);
                        let is_maker = matches!(self.order_choice, OrderChoice::Gtc | OrderChoice::PostOnly);
                        let fees_on = self.active_market.as_ref().map(|m| m.fees_enabled).unwrap_or(true);
                        let p_fee = estimate_protocol_fee_usd(est_sh, ref_px, is_maker, fees_on);
                        let b_fee = estimate_builder_fee_usd(
                            est_sh * ref_px,
                            is_maker,
                            self.account.builder_maker_bps,
                            self.account.builder_taker_bps,
                        );
                        let b_bps = if is_maker { self.account.builder_maker_bps } else { self.account.builder_taker_bps };
                        let spread_str = active_book.as_ref().and_then(|b| b.spread).map(|s| format!("{:.1}¢", s * 100.0)).unwrap_or_else(|| "—".into());

                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "{} {:>5} · {:>6.1} {}  |  {} {} + {}({}bp) {}",
                                    tr(l, "Spread", "点差"),
                                    spread_str,
                                    est_sh,
                                    tr(l, "shares", "份"),
                                    tr(l, "Proto", "平台"),
                                    format_usd(p_fee, false),
                                    HARDCODED_BUILDER_LABEL,
                                    b_bps,
                                    format_usd(b_fee, false),
                                ))
                                .monospace()
                                .small()
                                .color(pal.text_muted),
                            )
                            .truncate(),
                        );
                    });

                    ui.add_space(8.0);

                    let active_orders: Vec<PaperOrder> = active_aid
                        .as_deref()
                        .map(|a| self.all_open_orders().iter().filter(|o| o.asset_id == a).cloned().collect())
                        .unwrap_or_default();
                    let active_rules: Vec<AutomationRule> = active_aid
                        .as_deref()
                        .map(|a| self.core.automation.list_rules().into_iter().filter(|r| r.asset_id == a && r.active).collect())
                        .unwrap_or_default();
                    let active_pos = self.active_position().cloned();

                    if let Some(act) = draw_price_ladder(
                        ui,
                        l,
                        pal,
                        &mut self.ladder_state,
                        self.odds_fmt,
                        active_book.as_ref(),
                        &active_orders,
                        active_pos.as_ref(),
                        &active_rules,
                    ) {
                        match act {
                            LadderAction::PlaceOrder { side, price, post_only_override } => {
                                self.place_order(side, price, if post_only_override { OrderChoice::PostOnly } else { self.order_choice });
                            }
                            LadderAction::CancelLevel { side, price } => {
                                if let Some(ref aid) = active_aid {
                                    let core = self.core.clone();
                                    let aid_c = aid.clone();
                                    self.rt.spawn(async move { let _ = core.execution.cancel_at_price(&aid_c, Some(side), price).await; });
                                }
                            }
                        }
                    }
                }
                RightTab::ProOrders => {
                    ScrollArea::vertical().id_salt("pro_orders_scroll").show(ui, |ui| {
                        self.draw_pro_orders_panel(ui);
                    });
                }
            }
        });

        // 4. MODERN CENTER STAGE (Polymarket-style Hero Header + Gradient Chart + Clean Bottom Tabs)
        let center_frame = Frame::NONE
            .fill(pal.bg_canvas)
            .inner_margin(Margin::same(16));

        egui::CentralPanel::default().frame(center_frame).show(ui, |ui| {
            // Hero Market Title + Big Live Probability Readout (modern Polymarket feel!)
            if let Some(m) = self.active_market.clone() {
                let title_disp = self.display_text(&m.question);
                let title_tip = if self.lang == Lang::Zh && title_disp != m.question {
                    format!("{title_disp}\n{}", m.question)
                } else {
                    m.question.clone()
                };
                ui.add(
                    egui::Label::new(RichText::new(&title_disp).strong().size(18.5).color(pal.text_primary))
                        .truncate(),
                )
                .on_hover_text(title_tip);

                ui.add_space(6.0);

                // Polymarket-style Hero Live Odds + Delta + Outcome Pills
                let live_px = active_book
                    .as_ref()
                    .and_then(|b| b.mid.or(b.best_ask).or(b.last_trade_price))
                    .or_else(|| m.outcome_prices.get(self.active_outcome_idx).copied())
                    .unwrap_or(0.5);
                let first_hist = self.history.first().map(|p| p.p).unwrap_or(live_px);
                let delta_pts = (live_px - first_hist) * 100.0;
                let hero_col = if delta_pts >= 0.0 { pal.green } else { pal.red };

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    // Big clean probability readout (fixed width -> zero jitter)
                    ui.add_sized(
                        Vec2::new(98.0, 30.0),
                        egui::Label::new(
                            RichText::new(format_odds(Some(live_px), self.odds_fmt, m.tick_size))
                                .monospace()
                                .strong()
                                .size(24.0)
                                .color(hero_col),
                        ),
                    );
                    ui.add_sized(
                        Vec2::new(72.0, 22.0),
                        egui::Label::new(
                            RichText::new(format!("{}{:.1}%", if delta_pts >= 0.0 { "+" } else { "" }, delta_pts))
                                .monospace()
                                .strong()
                                .size(12.5)
                                .color(hero_col),
                        ),
                    );

                    for (idx, name) in m.outcomes.iter().enumerate() {
                        let tid = m.token_ids.get(idx).map(|s| s.as_str());
                        let px = tid
                            .and_then(|t| self.core.feed.get_book_view(t, 1))
                            .and_then(|b| b.mid.or(b.best_ask))
                            .or_else(|| m.outcome_prices.get(idx).copied());
                        let col = if idx == 0 { pal.green } else { pal.red };
                        let active = self.active_outcome_idx == idx;
                        let fill = if active {
                            if idx == 0 { Color32::from_rgba_unmultiplied(16, 185, 129, if pal.light { 30 } else { 34 }) } else { Color32::from_rgba_unmultiplied(244, 63, 94, if pal.light { 26 } else { 34 }) }
                        } else {
                            pal.bg_card
                        };
                        let stroke = if active { col } else { pal.border };
                        let disp_name = self.display_text(name);
                        let max_chars = if polydesk::translate::contains_cjk(&disp_name) { 6 } else { 10 };
                        let short_name: String = if disp_name.chars().count() > max_chars {
                            format!("{}…", disp_name.chars().take(max_chars - 1).collect::<String>())
                        } else {
                            disp_name.clone()
                        };
                        let lbl = format!("{short_name}  {:>5}", format_odds(px, self.odds_fmt, m.tick_size));
                        let tip = if self.lang == Lang::Zh && disp_name != *name {
                            format!("{disp_name} ({name})")
                        } else {
                            name.clone()
                        };
                        let resp = ui.add_sized(
                            Vec2::new(134.0, 30.0),
                            egui::Button::new(RichText::new(lbl).monospace().strong().size(12.5).color(col))
                                .fill(fill)
                                .stroke(Stroke::new(1.0, stroke))
                                .corner_radius(CornerRadius::same(9)),
                        ).on_hover_text(tip);
                        if resp.clicked() && !active {
                            self.active_outcome_idx = idx;
                            self.ladder_state.reset_for_market(m.tick_size);
                            if let Some(p) = px {
                                self.quick_limit_cents = (p * 100.0).round().clamp(1.0, 99.0);
                            }
                            if let Some(aid) = self.active_asset_id().map(|s| s.to_string()) {
                                self.trigger_load_history(&aid);
                            }
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.selectable_value(&mut self.chart_mode, ChartMode::Candles, tr(l, "Candles", "K线"));
                        ui.selectable_value(&mut self.chart_mode, ChartMode::Line, tr(l, "Line", "折线"));
                    });
                });
            }

            ui.add_space(6.0);

            // Indicators + Timeframe bar (single fixed non-wrapping row — zero collision even with long team names!)
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new(tr(l, "Indicators", "指标")).small().color(Color32::from_rgb(100, 116, 139)));
                ui.toggle_value(&mut self.indicators.ema9, RichText::new("EMA9").monospace().small().color(Color32::from_rgb(56, 189, 248)));
                ui.toggle_value(&mut self.indicators.sma20, RichText::new("SMA20").monospace().small().color(Color32::from_rgb(251, 191, 36)));
                ui.toggle_value(&mut self.indicators.bollinger, RichText::new("BB(20,2)").monospace().small().color(Color32::from_rgb(129, 140, 248)));
                ui.toggle_value(&mut self.indicators.vwap, RichText::new("VWAP").monospace().small().color(Color32::from_rgb(192, 132, 252)));
                ui.toggle_value(&mut self.indicators.rsi14, RichText::new("RSI(14)").monospace().small().color(Color32::from_rgb(251, 191, 36)));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for iv in ["max", "1m", "1w", "1d", "6h", "1h"] {
                        if ui.selectable_label(self.chart_interval == iv, RichText::new(iv.to_uppercase()).monospace().small()).clicked() {
                            self.chart_interval = iv;
                            if let Some(aid) = self.active_asset_id().map(|s| s.to_string()) {
                                self.trigger_load_history(&aid);
                            }
                        }
                    }
                });
            });

            ui.add_space(6.0);

            // Stable chart height derived cleanly from remaining space (ZERO tick bounce!)
            let rem_h = ui.available_height();
            let chart_h = ((rem_h - 10.0) * 0.56).floor().clamp(210.0, 480.0);
            let tick_sz = self.active_market.as_ref().map(|m| m.tick_size).unwrap_or(0.01);
            let active_rules: Vec<AutomationRule> = active_aid
                .as_deref()
                .map(|a| self.core.automation.list_rules().into_iter().filter(|r| r.asset_id == a && r.active).collect())
                .unwrap_or_default();
            draw_price_chart(
                ui,
                l,
                pal,
                &self.history,
                self.chart_mode,
                self.indicators,
                self.odds_fmt,
                tick_sz,
                active_book.as_ref(),
                self.active_position(),
                &active_rules,
                chart_h,
            );

            ui.add_space(10.0);

            // Fixed-height Bottom Workspace Card (fills remaining height cleanly without bouncing)
            let deck_h = ui.available_height().max(120.0);
            pal.surface_card().show(ui, |ui| {
                ui.set_min_height((deck_h - 24.0).max(96.0));
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.selectable_value(&mut self.deck_tab, DeckTab::Positions, RichText::new(tr(l, "Positions", "持仓")).strong());
                    ui.selectable_value(&mut self.deck_tab, DeckTab::Orders, RichText::new(tr(l, "Open Orders", "挂单")).strong());
                    ui.selectable_value(&mut self.deck_tab, DeckTab::Automation, RichText::new(tr(l, "Auto Rules", "条件单")).strong());
                    ui.selectable_value(&mut self.deck_tab, DeckTab::WhalesTape, RichText::new(tr(l, "Whales & Tape", "鲸鱼/逐笔")).strong());
                    ui.selectable_value(&mut self.deck_tab, DeckTab::CopyTrade, RichText::new(tr(l, "Copy-Trade", "跟单")).strong());
                    ui.selectable_value(&mut self.deck_tab, DeckTab::Fees, RichText::new(tr(l, "Fills & Fees", "成交/费率")).strong());
                });
                ui.separator();
                ScrollArea::vertical().id_salt("deck_scroll").show(ui, |ui| {
                    self.draw_bottom_deck(ui, active_aid.as_deref());
                });
            });
        });

        // Modals & Toasts
        self.render_modals(&ctx);
        if !self.toasts.is_empty() {
            egui::Area::new(egui::Id::NULL.with("toast_area"))
                .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-16.0, -16.0))
                .show(&ctx, |ui| {
                    for (txt, is_err, _) in &self.toasts {
                        let stroke_col = if *is_err { pal.red } else { pal.green };
                        Frame::NONE
                            .fill(pal.tooltip_bg)
                            .stroke(Stroke::new(1.2, stroke_col))
                            .inner_margin(Margin::symmetric(12, 8))
                            .corner_radius(CornerRadius::same(9))
                            .show(ui, |ui| {
                                ui.label(RichText::new(txt).monospace().small().color(pal.tooltip_fg));
                            });
                    }
                });
        }

        ctx.request_repaint_after(Duration::from_millis(65));
    }
}

impl PolyDeskDesktop {
    fn draw_bottom_deck(&mut self, ui: &mut egui::Ui, active_aid: Option<&str>) {
        let l = self.lang;
        let pal = self.palette();
        match self.deck_tab {
            DeckTab::Positions => {
                let positions = self.all_positions().to_vec();
                if positions.is_empty() {
                    ui.label(RichText::new(tr(l, "No open positions — click any Bid or Ask cell on the Price Ladder to trade.", "暂无持仓 — 点击右侧盘口深度格即可下单。")).color(Color32::from_rgb(100, 116, 139)));
                    return;
                }
                egui::Grid::new("pos_grid").striped(true).num_columns(7).spacing([14.0, 6.0]).show(ui, |ui| {
                    for h in [tr(l, "Market", "市场"), tr(l, "Side", "方向"), tr(l, "Shares", "份数"), tr(l, "Avg", "均价"), tr(l, "Mark", "现价"), tr(l, "uP&L", "浮盈"), tr(l, "Actions", "操作")] {
                        ui.label(RichText::new(h).strong().small().color(Color32::from_rgb(100, 116, 139)));
                    }
                    ui.end_row();
                    for p in positions {
                        let mk = self.core.feed.get_book_view(&p.asset_id, 1).and_then(|b| b.best_bid.or(b.mid)).unwrap_or(p.avg_entry);
                        let upnl = p.shares * (mk - p.avg_entry);
                        let pq = self.display_text(&p.market_question);
                        let po = self.display_text(&p.outcome);
                        ui.label(pq.chars().take(36).collect::<String>()).on_hover_text(&p.market_question);
                        ui.label(po);
                        ui.label(RichText::new(format!("{:.2}", p.shares)).monospace());
                        ui.label(RichText::new(format_odds(Some(p.avg_entry), self.odds_fmt, 0.01)).monospace());
                        ui.label(RichText::new(format_odds(Some(mk), self.odds_fmt, 0.01)).monospace());
                        let pc = if upnl >= 0.0 { Color32::from_rgb(16, 185, 129) } else { Color32::from_rgb(244, 63, 94) };
                        ui.label(RichText::new(format_usd(upnl, true)).monospace().strong().color(pc));
                        ui.horizontal(|ui| {
                            if ui.small_button(tr(l, "Cash Out", "锁盈")).clicked() {
                                let core = self.core.clone();
                                let aid = p.asset_id.clone();
                                self.rt.spawn(async move { let _ = core.execution.execute_green_up(&aid, None, "close").await; });
                            }
                            if ui.small_button(tr(l, "Free-Roll", "抽本")).clicked() {
                                let core = self.core.clone();
                                let aid = p.asset_id.clone();
                                self.rt.spawn(async move { let _ = core.execution.execute_green_up(&aid, None, "free_roll").await; });
                            }
                        });
                        ui.end_row();
                    }
                });
            }
            DeckTab::Orders => {
                let orders = self.all_open_orders().to_vec();
                if orders.is_empty() {
                    ui.label(RichText::new(tr(l, "No active resting orders.", "暂无挂单。")).color(Color32::from_rgb(100, 116, 139)));
                    return;
                }
                egui::Grid::new("ord_grid").striped(true).num_columns(6).spacing([14.0, 6.0]).show(ui, |ui| {
                    for h in [tr(l, "Side", "方向"), tr(l, "Price", "价格"), tr(l, "Size", "数量"), "Type", tr(l, "Market", "市场"), ""] {
                        ui.label(RichText::new(h).strong().small().color(Color32::from_rgb(100, 116, 139)));
                    }
                    ui.end_row();
                    for o in orders {
                        let sc = if o.side == "BUY" { Color32::from_rgb(16, 185, 129) } else { Color32::from_rgb(244, 63, 94) };
                        ui.label(RichText::new(&o.side).monospace().strong().color(sc));
                        ui.label(RichText::new(format_odds(Some(o.price), self.odds_fmt, 0.01)).monospace());
                        ui.label(RichText::new(format!("{:.1} / {:.1}", o.remaining_size, o.original_size)).monospace());
                        ui.label(format!("{}{}", o.order_type, if o.post_only { " PO" } else { "" }));
                        let oq = self.display_text(&o.market_question);
                        ui.label(oq.chars().take(34).collect::<String>()).on_hover_text(&o.market_question);
                        if ui.small_button(tr(l, "Cancel", "撤单")).clicked() {
                            let core = self.core.clone();
                            let id = o.id.clone();
                            self.rt.spawn(async move { let _ = core.execution.cancel_order(&id).await; });
                        }
                        ui.end_row();
                    }
                });
            }
            DeckTab::Automation => {
                let rules = self.core.automation.list_rules();
                ui.horizontal(|ui| {
                    if ui.small_button(tr(l, "Clear Done", "清理已完成")).clicked() {
                        self.core.automation.delete_inactive_rules();
                    }
                });
                if rules.is_empty() {
                    ui.label(RichText::new(tr(l, "No armed SL / TP / Trailing / Drip rules. Open Pro Orders on the right panel to create one.", "暂无条件单，请在右侧「高级条件单」中部署止损/止盈/移动止损。")).color(Color32::from_rgb(100, 116, 139)));
                    return;
                }
                egui::Grid::new("rule_grid").striped(true).num_columns(6).spacing([14.0, 6.0]).show(ui, |ui| {
                    for r in rules {
                        ui.label(RichText::new(format!("{:?}", r.kind)).monospace().color(Color32::from_rgb(56, 189, 248)));
                        ui.label(&r.side);
                        ui.label(RichText::new(format_odds(Some(r.trigger_price), self.odds_fmt, 0.01)).monospace());
                        ui.label(RichText::new(format!("{:.1}", r.remaining_size)).monospace());
                        ui.label(&r.status);
                        if r.active && ui.small_button(tr(l, "Cancel", "取消")).clicked() {
                            self.core.automation.cancel_rule(&r.id);
                        }
                        ui.end_row();
                    }
                });
            }
            DeckTab::WhalesTape => {
                ui.columns(2, |cols| {
                    // Left column: Net Whale Flow + Live Tape
                    if let Some(ref wf) = self.whale_flow {
                        let fc = if wf.net_flow_usd >= 0.0 { Color32::from_rgb(16, 185, 129) } else { Color32::from_rgb(244, 63, 94) };
                        cols[0].horizontal(|ui| {
                            ui.label(RichText::new(tr(l, "Net Whale Flow", "鲸鱼净流向")).strong());
                            ui.label(RichText::new(format_usd(wf.net_flow_usd, true)).monospace().strong().color(fc));
                        });
                        cols[0].label(RichText::new(format!("YES {}  vs  NO {}", format_usd(wf.yes_whale_vol_usd, false), format_usd(wf.no_whale_vol_usd, false))).monospace().small().color(Color32::GRAY));
                        cols[0].add_space(6.0);
                    }
                    cols[0].label(RichText::new(tr(l, "Live Trade Tape", "实时逐笔成交")).strong().small());
                    for tr_e in self.core.feed.recent_tape(active_aid, 18) {
                        let c = if tr_e.side == "BUY" { pal.green } else { pal.red };
                        cols[0].horizontal(|ui| {
                            ui.label(RichText::new(&tr_e.side).monospace().small().color(c));
                            ui.label(RichText::new(format_odds(Some(tr_e.price), self.odds_fmt, 0.01)).monospace().small());
                            ui.label(RichText::new(format_usd(tr_e.notional_usd, false)).monospace().small().color(if tr_e.is_whale { pal.amber } else { pal.text_secondary }));
                        });
                    }

                    // Right column: Top Holders (1-Click Copy)
                    cols[1].label(RichText::new(tr(l, "Top Market Holders", "本市场大户 (一键跟单)")).strong().small());
                    if let Some(wf) = self.whale_flow.clone() {
                        for h in wf.top_holders.iter().take(10) {
                            let lbl = if !h.name.is_empty() { h.name.clone() } else if !h.pseudonym.is_empty() { h.pseudonym.clone() } else { h.proxy_wallet.chars().take(10).collect() };
                            cols[1].horizontal(|ui| {
                                ui.label(RichText::new(&lbl).small());
                                ui.label(RichText::new(format!("{} {}", if h.outcome_index == 0 { "Y" } else { "N" }, format_size(h.amount))).monospace().small().color(Color32::GRAY));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button(tr(l, "Copy", "跟单")).clicked() {
                                        self.core.automation.add_copy_target(CreateCopyTargetRequest {
                                            wallet: h.proxy_wallet.clone(), label: lbl.clone(),
                                            multiplier: 0.05, max_order_usd: 25.0, max_slippage_bps: Some(300.0),
                                        });
                                        self.push_toast(format!("Now copy-trading {lbl}"), false);
                                    }
                                });
                            });
                        }
                    }
                });
            }
            DeckTab::CopyTrade => {
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut self.copy_wallet).hint_text("0xWhaleWallet...").desired_width(220.0));
                    ui.add(TextEdit::singleline(&mut self.copy_label).hint_text(tr(l, "Label", "备注")).desired_width(90.0));
                    ui.add(egui::DragValue::new(&mut self.copy_mult).range(0.01..=5.0).speed(0.05).suffix("x"));
                    ui.add(egui::DragValue::new(&mut self.copy_max_usd).range(1.0..=10_000.0).prefix("$"));
                    if ui.button(tr(l, "Add Copy", "添加跟单")).clicked() && !self.copy_wallet.trim().is_empty() {
                        self.core.automation.add_copy_target(CreateCopyTargetRequest {
                            wallet: self.copy_wallet.clone(), label: self.copy_label.clone(),
                            multiplier: self.copy_mult, max_order_usd: self.copy_max_usd, max_slippage_bps: Some(300.0),
                        });
                        self.copy_wallet.clear();
                    }
                });
                ui.add_space(6.0);
                let targets: Vec<CopyTarget> = self.core.automation.list_copy_targets();
                for c in targets {
                    ui.horizontal(|ui| {
                        ui.label(&c.label);
                        ui.label(RichText::new(format!("{}x · max ${:.0} · {} hits", c.multiplier, c.max_order_usd, c.copied_count)).monospace().small().color(Color32::GRAY));
                        if ui.small_button(if c.enabled { "ON" } else { "OFF" }).clicked() { self.core.automation.toggle_copy_target(&c.id); }
                        if ui.small_button("×").clicked() { self.core.automation.remove_copy_target(&c.id); }
                    });
                }
            }
            DeckTab::Fees => {
                let telem = self.core.telemetry.snapshot();
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("{} · {}bp Taker / {}bp Maker", HARDCODED_BUILDER_LABEL, self.account.builder_taker_bps, self.account.builder_maker_bps)).monospace().small().color(pal.cyan));
                    ui.label(RichText::new(format!("| {}: {}", tr(l, "Volume", "成交额"), format_usd(telem.routed_volume_usd, false))).monospace().small());
                    ui.label(RichText::new(format!("| {}: {}", tr(l, "Terminal Fee", "终端费"), format_usd(telem.total_builder_fees_usd, false))).monospace().small().color(pal.amber));
                });
                ui.add_space(6.0);
                for rec in telem.recent_fee_ledger.iter().take(20) {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("[{}] {} {}", rec.mode, rec.side, format_usd(rec.notional_usd, false))).monospace().small());
                        ui.label(RichText::new(format!("Proto {} · Terminal({}bp) {}", format_usd(rec.protocol_fee_usd, false), rec.builder_fee_bps, format_usd(rec.builder_fee_usd, false))).monospace().small().color(Color32::GRAY));
                    });
                }
            }
        }
    }

    fn draw_pro_orders_panel(&mut self, ui: &mut egui::Ui) {
        let l = self.lang;
        let pal = self.palette();

        // 1. Engine-Side Conditional Rules (SL / TP / Trailing / Drip)
        pal.surface_card().show(ui, |ui| {
            ui.label(RichText::new(tr(l, "Conditional Exit (Rust Engine · Microsecond Trigger)", "引擎条件单（本地微秒触发）")).strong().color(pal.cyan));
            ui.add_space(6.0);
            egui::Grid::new("adv_rule_g").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label(tr(l, "Rule Type", "策略类型"));
                egui::ComboBox::from_id_salt("rk_cb").width(200.0).selected_text(format!("{:?}", self.rule_kind)).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.rule_kind, RuleKind::StopLoss, tr(l, "Stop-Loss (SL)", "止损 SL"));
                    ui.selectable_value(&mut self.rule_kind, RuleKind::TakeProfit, tr(l, "Take-Profit (TP)", "止盈 TP"));
                    ui.selectable_value(&mut self.rule_kind, RuleKind::TrailingStop, tr(l, "Trailing Stop", "移动止损"));
                    ui.selectable_value(&mut self.rule_kind, RuleKind::DripFeed, tr(l, "Drip-Feed / TWAP", "时间分批"));
                });
                ui.end_row();

                ui.label(tr(l, "Side", "方向"));
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.rule_side_sell, true, "SELL");
                    ui.selectable_value(&mut self.rule_side_sell, false, "BUY");
                });
                ui.end_row();

                ui.label(tr(l, "Trigger Price (¢)", "触发价 (¢)"));
                ui.add(egui::DragValue::new(&mut self.rule_trigger_cents).range(0.1..=99.9).speed(0.5));
                ui.end_row();

                ui.label(tr(l, "Size (Shares)", "份数"));
                ui.add(egui::DragValue::new(&mut self.rule_size).range(1.0..=100_000.0));
                ui.end_row();

                if self.rule_kind == RuleKind::TrailingStop {
                    ui.label(tr(l, "Trail Offset (¢)", "回撤距离 (¢)"));
                    ui.add(egui::DragValue::new(&mut self.rule_trail_cents).range(0.5..=50.0).speed(0.5));
                    ui.end_row();
                }
                if self.rule_kind == RuleKind::DripFeed {
                    ui.label(tr(l, "Slices / Interval", "批数 / 间隔(秒)"));
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut self.rule_slices).range(2..=100));
                        ui.add(egui::DragValue::new(&mut self.rule_interval_sec).range(1..=3600).suffix("s"));
                    });
                    ui.end_row();
                }
            });
            ui.add_space(8.0);
            if ui.button(RichText::new(tr(l, "Arm Engine Rule", "部署条件单")).strong().color(Color32::from_rgb(251, 191, 36))).clicked()
                && let (Some(m), Some(aid)) = (self.active_market.as_ref(), self.active_asset_id())
            {
                self.core.automation.create_rule(CreateRuleRequest {
                    kind: self.rule_kind.clone(),
                    asset_id: aid.to_string(),
                    condition_id: m.condition_id.clone(),
                    market_question: m.question.clone(),
                    outcome: self.active_outcome_name(),
                    side: if self.rule_side_sell { "SELL".into() } else { "BUY".into() },
                    size: self.rule_size,
                    trigger_price: self.rule_trigger_cents / 100.0,
                    trail_offset: (self.rule_kind == RuleKind::TrailingStop).then_some(self.rule_trail_cents / 100.0),
                    interval_ms: (self.rule_kind == RuleKind::DripFeed).then_some(self.rule_interval_sec * 1000),
                    slices: (self.rule_kind == RuleKind::DripFeed).then_some(self.rule_slices),
                    max_slippage_bps: Some(400.0),
                });
                self.push_toast("Armed conditional rule in Rust engine".into(), false);
            }
        });

        ui.add_space(10.0);

        // 2. Scale Entry Batch Ladder
        pal.surface_card().show(ui, |ui| {
            ui.label(RichText::new(tr(l, "Scale Entry (Batch Ladder)", "区间阶梯批量挂单")).strong().color(pal.cyan));
            ui.add_space(6.0);
            egui::Grid::new("scale_g").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label(tr(l, "Side", "方向"));
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.scale_buy, true, "BUY");
                    ui.selectable_value(&mut self.scale_buy, false, "SELL");
                });
                ui.end_row();
                ui.label(tr(l, "Start → End (¢)", "起始 → 结束价 (¢)"));
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.scale_start_cents).range(0.5..=99.5).speed(0.5));
                    ui.label("→");
                    ui.add(egui::DragValue::new(&mut self.scale_end_cents).range(0.5..=99.5).speed(0.5));
                });
                ui.end_row();
                ui.label(tr(l, "Total Shares / Rungs", "总份数 / 档数"));
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.scale_size).range(5.0..=100_000.0));
                    ui.add(egui::DragValue::new(&mut self.scale_steps).range(2..=15));
                });
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.scale_post_only, "Post-Only");
                if ui.button(RichText::new(tr(l, "Deploy Scale Ladder", "批量铺单")).strong().color(Color32::from_rgb(56, 189, 248))).clicked()
                    && let (Some(m), Some(aid)) = (self.active_market.clone(), self.active_asset_id().map(|s| s.to_string()))
                {
                    let req = ScaleEntryRequest {
                        asset_id: aid, condition_id: m.condition_id, market_question: m.question,
                        outcome: self.active_outcome_name(), side: if self.scale_buy { "BUY".into() } else { "SELL".into() },
                        start_price: self.scale_start_cents / 100.0, end_price: self.scale_end_cents / 100.0,
                        total_size: self.scale_size, steps: self.scale_steps, post_only: self.scale_post_only,
                        fees_enabled: Some(m.fees_enabled),
                    };
                    let core = self.core.clone();
                    let tx = self.tx.clone();
                    self.rt.spawn(async move {
                        match core.execution.place_scale_entry(req).await {
                            Ok(acks) => { let _ = tx.send(UiMsg::Toast { text: format!("Deployed {} rungs", acks.len()), is_err: false }); }
                            Err(e) => { let _ = tx.send(UiMsg::Toast { text: e.to_string(), is_err: true }); }
                        }
                    });
                }
            });
        });

        ui.add_space(10.0);

        // 3. Green-Up & Sweep Quotes
        if let Some(aid) = self.active_asset_id() {
            if let Some(g) = self.core.execution.preview_green_up(aid, self.opposite_asset_id()) {
                let g: GreenUpQuote = g;
                pal.surface_card().show(ui, |ui| {
                    ui.label(RichText::new(tr(l, "Green-Up / Trade-Out", "一键锁盈策略")).strong().color(pal.green));
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("Cash Out: {} @ {}", format_usd(g.close_locked_pnl, true), format_odds(Some(g.close_price), self.odds_fmt, 0.01))).monospace());
                    if g.can_free_roll {
                        ui.label(RichText::new(format!("Free-Roll: keep {:.1} risk-free shares", g.free_roll_remaining_shares)).monospace().small().color(pal.green));
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr(l, "Cash Out", "市价锁盈")).clicked() { self.trigger_green_up("close"); }
                        if g.can_free_roll && ui.button(tr(l, "Free-Roll", "抽本留利润")).clicked() { self.trigger_green_up("free_roll"); }
                        if g.hedge_no_price.is_some() && ui.button(tr(l, "Hedge Opp", "对冲反边")).clicked() { self.trigger_green_up("hedge_opposite"); }
                    });
                });
                ui.add_space(10.0);
            }

            if let Some(s) = self.core.execution.preview_sweep(aid, true, Some(self.stake_value), None, None) {
                let s: SweepQuote = s;
                pal.surface_card().show(ui, |ui| {
                    ui.label(RichText::new(format!("{} (${:.0})", tr(l, "Live Sweep Simulator", "扫盘成本预估"), self.stake_value)).strong());
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("VWAP {} · Slip {:.0}bp · {} levels", format_odds(Some(s.vwap), self.odds_fmt, 0.01), s.slippage_bps, s.levels_consumed)).monospace().small().color(pal.text_muted));
                });
            }
        }
    }

    /// Polymarket-style 1-Click Quick Buy / Sell Card — enter amount, see instant payout & fees, click Buy/Sell!
    fn draw_quick_trade_panel(&mut self, ui: &mut egui::Ui, active_book: Option<&polydesk::book::BookView>) {
        let l = self.lang;
        let pal = self.palette();
        let sh_unit = tr(l, "shares", "份");
        let Some(m) = self.active_market.clone() else {
            ui.label(RichText::new(tr(l, "Select any market on the left to start trading.", "请在左侧选择任意市场开始交易。")).color(pal.text_muted));
            return;
        };

        let is_live = self.account.mode == TradingMode::Live;
        let cash = if is_live { self.account.live_balance_usd.unwrap_or(0.0) } else { self.account.paper.cash_usd };
        let active_pos = self.active_position().cloned();
        let avail_shares = active_pos.as_ref().map(|p| p.shares).unwrap_or(0.0);

        pal.surface_card().show(ui, |ui| {
            ui.set_width(ui.available_width());

            // 1. Big Buy / Sell Segmented Switcher
            ui.horizontal(|ui| {
                let half_w = ((ui.available_width() - 8.0) * 0.5).max(120.0);
                let buy_fill = if self.quick_buy { Color32::from_rgba_unmultiplied(16, 185, 129, if pal.light { 28 } else { 36 }) } else { pal.bg_chip };
                let buy_stroke = if self.quick_buy { pal.green } else { pal.border };
                let buy_col = if self.quick_buy { pal.green } else { pal.text_muted };
                if ui.add_sized(
                    Vec2::new(half_w, 34.0),
                    egui::Button::new(RichText::new(tr(l, "Buy", "买入")).strong().size(14.0).color(buy_col))
                        .fill(buy_fill)
                        .stroke(Stroke::new(1.2, buy_stroke))
                        .corner_radius(CornerRadius::same(9)),
                ).clicked() {
                    self.quick_buy = true;
                }

                let sell_fill = if !self.quick_buy { Color32::from_rgba_unmultiplied(244, 63, 94, if pal.light { 24 } else { 36 }) } else { pal.bg_chip };
                let sell_stroke = if !self.quick_buy { pal.red } else { pal.border };
                let sell_col = if !self.quick_buy { pal.red } else { pal.text_muted };
                if ui.add_sized(
                    Vec2::new(half_w, 34.0),
                    egui::Button::new(RichText::new(tr(l, "Sell", "卖出")).strong().size(14.0).color(sell_col))
                        .fill(sell_fill)
                        .stroke(Stroke::new(1.2, sell_stroke))
                        .corner_radius(CornerRadius::same(9)),
                ).clicked() {
                    self.quick_buy = false;
                }
            });

            ui.add_space(10.0);

            // 2. Order Type Pill: Market vs Limit
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr(l, "Order Type", "订单类型")).small().color(pal.text_muted));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.selectable_label(self.quick_limit_mode, RichText::new(tr(l, "Limit", "限价单")).small()).clicked() {
                        self.quick_limit_mode = true;
                        if let Some(px) = active_book.and_then(|b| b.mid.or(b.best_ask)) {
                            self.quick_limit_cents = (px * 100.0).round().clamp(1.0, 99.0);
                        }
                    }
                    if ui.selectable_label(!self.quick_limit_mode, RichText::new(tr(l, "Market", "市价成交")).small()).clicked() {
                        self.quick_limit_mode = false;
                    }
                });
            });

            ui.add_space(10.0);

            // 3. Big Side-by-Side Outcome Cards
            ui.label(RichText::new(tr(l, "Outcome", "选择方向")).small().color(pal.text_muted));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let n = m.outcomes.len().clamp(1, 2);
                let btn_w = ((ui.available_width() - 8.0) / n as f32).max(120.0);
                for (idx, name) in m.outcomes.iter().take(2).enumerate() {
                    let tid = m.token_ids.get(idx).map(|s| s.as_str());
                    let px = tid
                        .and_then(|t| self.core.feed.get_book_view(t, 1))
                        .and_then(|b| if self.quick_buy { b.best_ask.or(b.mid) } else { b.best_bid.or(b.mid) })
                        .or_else(|| m.outcome_prices.get(idx).copied());
                    let active = self.active_outcome_idx == idx;
                    let col = if idx == 0 { pal.green } else { pal.red };
                    let fill = if active {
                        if idx == 0 { Color32::from_rgba_unmultiplied(16, 185, 129, if pal.light { 30 } else { 42 }) } else { Color32::from_rgba_unmultiplied(244, 63, 94, if pal.light { 26 } else { 42 }) }
                    } else {
                        pal.bg_chip
                    };
                    let stroke = if active { col } else { pal.border };
                    let disp_name = self.display_text(name);
                    let max_c = if polydesk::translate::contains_cjk(&disp_name) { 7 } else { 12 };
                    let short: String = if disp_name.chars().count() > max_c {
                        format!("{}…", disp_name.chars().take(max_c - 1).collect::<String>())
                    } else {
                        disp_name.clone()
                    };
                    let lbl = format!("{short}\n{}", format_odds(px, self.odds_fmt, m.tick_size));
                    if ui.add_sized(
                        Vec2::new(btn_w, 48.0),
                        egui::Button::new(RichText::new(lbl).monospace().strong().size(13.0).color(if active { col } else { pal.text_primary }))
                            .fill(fill)
                            .stroke(Stroke::new(if active { 1.5 } else { 1.0 }, stroke))
                            .corner_radius(CornerRadius::same(10)),
                    ).on_hover_text(&disp_name).clicked() && !active {
                        self.active_outcome_idx = idx;
                        self.ladder_state.reset_for_market(m.tick_size);
                        if let Some(p) = px {
                            self.quick_limit_cents = (p * 100.0).round().clamp(1.0, 99.0);
                        }
                        if let Some(aid) = self.active_asset_id().map(|s| s.to_string()) {
                            self.trigger_load_history(&aid);
                        }
                    }
                }
            });

            ui.add_space(12.0);

            // 4. Optional Limit Price Stepper
            if self.quick_limit_mode {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tr(l, "Limit Price", "限价价格")).small().color(pal.text_muted));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let step_c = (m.tick_size * 100.0).max(0.1);
                        if ui.button(" + ").clicked() {
                            self.quick_limit_cents = (self.quick_limit_cents + step_c).min(99.5);
                        }
                        ui.add(
                            egui::DragValue::new(&mut self.quick_limit_cents)
                                .range(0.5..=99.5)
                                .speed(0.5)
                                .suffix("¢"),
                        );
                        if ui.button(" − ").clicked() {
                            self.quick_limit_cents = (self.quick_limit_cents - step_c).max(0.5);
                        }
                    });
                });
                ui.add_space(10.0);
            }

            // 5. Big Clean Amount Input + Instant Preset Chips
            ui.horizontal(|ui| {
                if self.quick_buy {
                    ui.label(RichText::new(tr(l, "Amount (USD)", "买入金额 ($)")).strong().size(13.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("{}: {}", tr(l, "Balance", "可用"), format_usd(cash, false))).monospace().small().color(pal.text_muted));
                    });
                } else {
                    ui.label(RichText::new(tr(l, "Shares to Sell (1 share = $1 if wins)", "卖出份数 (每份赢了赔 $1)")).strong().size(12.5));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("{}: {:.2} {}", tr(l, "Owned", "持仓"), avail_shares, sh_unit)).monospace().small().color(pal.text_muted));
                    });
                }
            });
            ui.add_space(6.0);

            ui.add_sized(
                Vec2::new(ui.available_width(), 38.0),
                TextEdit::singleline(&mut self.quick_amount_input)
                    .font(egui::FontId::monospace(18.0))
                    .hint_text(if self.quick_buy { "25" } else { "10" }),
            );

            ui.add_space(6.0);

            let parsed_amt = self.quick_amount_input.trim().parse::<f64>().unwrap_or(0.0).max(0.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if self.quick_buy {
                    let chip_w = ((ui.available_width() - 24.0) / 5.0).max(52.0);
                    for add_v in [1.0, 10.0, 50.0, 100.0] {
                        if ui.add_sized(
                            Vec2::new(chip_w, 28.0),
                            egui::Button::new(RichText::new(format!("+${add_v:.0}")).monospace().small())
                                .fill(pal.bg_chip)
                                .corner_radius(CornerRadius::same(7)),
                        ).clicked() {
                            let next = (parsed_amt + add_v).min(100_000.0);
                            self.quick_amount_input = format!("{next:.0}");
                        }
                    }
                    if ui.add_sized(
                        Vec2::new(chip_w, 28.0),
                        egui::Button::new(RichText::new("Max").monospace().small().color(pal.blue))
                            .fill(pal.bg_chip)
                            .corner_radius(CornerRadius::same(7)),
                    ).clicked() {
                        self.quick_amount_input = format!("{:.2}", cash.floor().max(0.0));
                    }
                } else {
                    let chip_w = ((ui.available_width() - 18.0) / 4.0).max(60.0);
                    for (pct, lbl) in [(0.25, "25%"), (0.50, "50%"), (0.75, "75%"), (1.0, "100%")] {
                        if ui.add_sized(
                            Vec2::new(chip_w, 28.0),
                            egui::Button::new(RichText::new(lbl).monospace().small())
                                .fill(pal.bg_chip)
                                .corner_radius(CornerRadius::same(7)),
                        ).clicked() {
                            self.quick_amount_input = format!("{:.2}", avail_shares * pct);
                        }
                    }
                }
            });

            ui.add_space(12.0);

            // 6. Clean Payout & Fee Breakdown Box
            let exec_px = if self.quick_limit_mode {
                (self.quick_limit_cents / 100.0).clamp(0.005, 0.995)
            } else if self.quick_buy {
                active_book
                    .and_then(|b| b.best_ask.or(b.mid))
                    .or_else(|| m.outcome_prices.get(self.active_outcome_idx).copied())
                    .unwrap_or(0.5)
            } else {
                active_book
                    .and_then(|b| b.best_bid.or(b.mid))
                    .or_else(|| m.outcome_prices.get(self.active_outcome_idx).copied())
                    .unwrap_or(0.5)
            };

            let (est_shares, est_usd) = if self.quick_buy {
                let sh = if exec_px > 0.001 { parsed_amt / exec_px } else { 0.0 };
                (sh, parsed_amt)
            } else {
                (parsed_amt, parsed_amt * exec_px)
            };
            let is_maker = self.quick_limit_mode;
            let p_fee = estimate_protocol_fee_usd(est_shares, exec_px, is_maker, m.fees_enabled);
            let b_fee = estimate_builder_fee_usd(
                est_usd,
                is_maker,
                self.account.builder_maker_bps,
                self.account.builder_taker_bps,
            );
            let b_bps = if is_maker { self.account.builder_maker_bps } else { self.account.builder_taker_bps };
            let to_win = if self.quick_buy { est_shares } else { est_usd };
            let net_profit = if self.quick_buy { (to_win - est_usd - p_fee - b_fee).max(0.0) } else { 0.0 };
            let roi_pct = if self.quick_buy && est_usd > 0.01 { (net_profit / est_usd) * 100.0 } else { 0.0 };

            Frame::NONE
                .fill(pal.bg_sub)
                .stroke(Stroke::new(1.0, pal.border))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(tr(l, "Avg Price", "预计均价")).small().color(pal.text_muted));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(format_odds(Some(exec_px), self.odds_fmt, m.tick_size)).monospace().strong());
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(tr(l, "Shares", "预计份数")).small().color(pal.text_muted));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(format!("{:.2} {}", est_shares, sh_unit)).monospace().strong());
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{} ({} {}bp)", tr(l, "Fees", "费率"), HARDCODED_BUILDER_LABEL, b_bps)).small().color(pal.text_muted));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(format_usd(p_fee + b_fee, false)).monospace().small().color(pal.text_muted));
                        });
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        let lbl = if self.quick_buy { tr(l, "To Win (if settles Yes)", "获胜预期回报") } else { tr(l, "Est. Receive", "预计到账") };
                        ui.label(RichText::new(lbl).strong().size(13.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if self.quick_buy {
                                ui.label(
                                    RichText::new(format!("{} (+{:.0}%)", format_usd(to_win, false), roi_pct))
                                        .monospace()
                                        .strong()
                                        .size(15.0)
                                        .color(pal.green),
                                );
                            } else {
                                ui.label(
                                    RichText::new(format_usd(to_win, false))
                                        .monospace()
                                        .strong()
                                        .size(15.0)
                                        .color(pal.text_primary),
                                );
                            }
                        });
                    });
                });

            ui.add_space(12.0);

            // 7. HUGE 1-CLICK BUY / SELL ACTION BUTTON
            let out_disp = self.display_text(&self.active_outcome_name());
            let action_lbl = if self.quick_buy {
                format!("{} {} · {}", tr(l, "Buy", "立即买入"), out_disp, format_usd(est_usd, false))
            } else {
                format!("{} {} · {:.1} {}", tr(l, "Sell", "立即卖出"), out_disp, est_shares, sh_unit)
            };
            let btn_bg = if self.quick_buy {
                if pal.light { Color32::from_rgb(5, 150, 105) } else { Color32::from_rgb(0, 198, 140) }
            } else {
                pal.red
            };
            let btn_txt = if self.quick_buy && !pal.light { Color32::from_rgb(6, 16, 14) } else { Color32::WHITE };

            if ui.add_sized(
                Vec2::new(ui.available_width(), 44.0),
                egui::Button::new(RichText::new(action_lbl).strong().size(15.0).color(btn_txt))
                    .fill(btn_bg)
                    .stroke(Stroke::NONE)
                    .corner_radius(CornerRadius::same(11)),
            ).clicked() {
                if est_shares < 0.1 {
                    self.push_toast(tr(l, "Enter a valid amount first", "请先输入有效金额或份数").into(), true);
                } else {
                    let side = if self.quick_buy { "BUY" } else { "SELL" };
                    let choice = if self.quick_limit_mode { OrderChoice::Gtc } else { OrderChoice::Sweep };
                    self.place_order_exact(side, exec_px, est_shares, choice);
                }
            }
        });

        // Quick Cash-Out / Position Summary Card right beneath if user holds this market
        if let Some(pos) = active_pos {
            ui.add_space(10.0);
            let mk = active_book.and_then(|b| b.best_bid.or(b.mid)).unwrap_or(pos.avg_entry);
            let upnl = pos.shares * (mk - pos.avg_entry);
            let pcol = if upnl >= 0.0 { pal.green } else { pal.red };
            pal.surface_card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tr(l, "Your Position", "当前持仓")).strong().size(13.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format_usd(upnl, true)).monospace().strong().size(13.5).color(pcol));
                    });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "{} · {:.2} {} @ {} -> {}",
                        self.display_text(&pos.outcome),
                        pos.shares,
                        sh_unit,
                        format_odds(Some(pos.avg_entry), self.odds_fmt, 0.01),
                        format_odds(Some(mk), self.odds_fmt, 0.01)
                    ))
                    .monospace()
                    .small()
                    .color(pal.text_muted),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new(tr(l, "Cash Out All", "一键全部卖出锁盈")).strong().color(pal.amber)).clicked() {
                        self.trigger_green_up("close");
                    }
                    if ui.button(tr(l, "Free-Roll", "抽本留利润")).clicked() {
                        self.trigger_green_up("free_roll");
                    }
                });
            });
        }
    }
}

fn install_cjk_fonts(ctx: &Context) {
    let candidates: &[(&str, u32, u32)] = &[
        ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 2, 7),
        ("/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc", 2, 7),
        ("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc", 0, 1),
        ("/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc", 0, 1),
        ("/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf", 0, 0),
        (r"C:\Windows\Fonts\msyh.ttc", 0, 0),
        (r"C:\Windows\Fonts\msyh.ttf", 0, 0),
        (r"C:\Windows\Fonts\simhei.ttf", 0, 0),
        ("/System/Library/Fonts/PingFang.ttc", 2, 2),
        ("/System/Library/Fonts/STHeiti Light.ttc", 0, 0),
    ];

    for &(path, prop_idx, mono_idx) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = FontDefinitions::default();
            let mut prop_fd = FontData::from_owned(bytes.clone());
            prop_fd.index = prop_idx;
            fonts.font_data.insert("cjk_sc".into(), Arc::new(prop_fd));
            fonts.families.entry(FontFamily::Proportional).or_default().push("cjk_sc".into());
            if mono_idx != prop_idx {
                let mut mono_fd = FontData::from_owned(bytes);
                mono_fd.index = mono_idx;
                fonts.font_data.insert("cjk_sc_mono".into(), Arc::new(mono_fd));
                fonts.families.entry(FontFamily::Monospace).or_default().push("cjk_sc_mono".into());
            } else {
                fonts.families.entry(FontFamily::Monospace).or_default().push("cjk_sc".into());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
}
