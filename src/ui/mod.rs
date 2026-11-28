pub mod chart;
pub mod ladder;
pub mod modals;
pub mod theme;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{
    Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, Frame, Key, Margin,
    RichText, ScrollArea, Stroke, TextEdit, Vec2,
};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use polydesk::CoreEngine;
use polydesk::alerts::{AlertCondition, AlertTriggeredEvent, PriceAlert};
use polydesk::automation::{AutomationRule, CopyTarget, CreateCopyTargetRequest, CreateRuleRequest, RuleKind};
use polydesk::book::{estimate_builder_fee_usd, estimate_protocol_fee_usd, GreenUpQuote, SweepQuote};
use polydesk::config::HARDCODED_BUILDER_LABEL;
use polydesk::ctf::{CtfAction, CtfRequest, CtfTxResult};
use polydesk::execution::{
    AccountState, DepositAddressesInfo, OrderRequest, ScaleEntryRequest, TradingMode, align_to_tick,
    plan_sibling_hedge_legs,
};
use polydesk::i18n::{Lang, tr};
use polydesk::market::{
    EventSiblings, MarketSummary, PricePoint, WhaleFlowSummary, deterministic_updown_slug,
    extract_short_timeframe_info, next_updown_slug,
};
use polydesk::news::{
    NewsEdition, NewsFeedResult, NewsWindow, build_market_news_query, format_relative_age,
};
use polydesk::odds::{OddsFormat, format_odds, format_size, format_usd};
use polydesk::paper::{PaperOrder, PaperPosition};
use polydesk::translate::Translator;
use polydesk::updater::{
    HotPatchManifest, StagedColdUpdate, UpdateStatus, check_github_updates, compute_update_kind,
    download_and_stage_cold_update, relaunch_exe,
};
use polydesk::vault::{GeneratedWallet, WalletSignatureType, validate_profile_name};

use self::chart::{
    ChartCache, ChartMode, IndicatorConfig, OracleChartData, OracleChartSmoothState,
    ShareOverlayData, ShortChartView, draw_oracle_chart, draw_price_chart,
    draw_synced_share_chart, format_oracle_usd,
};
use self::ladder::{LadderAction, LadderState, draw_price_ladder};
use self::theme::{ThemePalette, apply_theme, ghost_button, seg_item};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutMode {
    Single,
    Multi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveModal {
    WalletAndDeposit,
    SettingsAndUpdates,
    CtfConfirm,
    OrderConfirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Trading,
    ChartsSpeed,
    Appearance,
    NetworkUpdates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletSetupTab {
    CreateNew,
    ImportExisting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrderChoice {
    Gtc,
    PostOnly,
    Fak,
    Fok,
    TimedFok,
    Sweep,
}

impl OrderChoice {
    pub(crate) fn from_config_str(s: &str) -> Self {
        match s.trim().to_ascii_uppercase().as_str() {
            "GTC" => Self::Gtc,
            "POST_ONLY" | "POSTONLY" => Self::PostOnly,
            "FAK" => Self::Fak,
            "FOK" => Self::Fok,
            "TIMED_FOK" | "TIMEDFOK" => Self::TimedFok,
            _ => Self::Sweep,
        }
    }

    pub(crate) fn to_config_str(self) -> &'static str {
        match self {
            Self::Gtc => "GTC",
            Self::PostOnly => "POST_ONLY",
            Self::Fak => "FAK",
            Self::Fok => "FOK",
            Self::TimedFok => "TIMED_FOK",
            Self::Sweep => "SWEEP",
        }
    }

    pub(crate) fn label(self, lang: Lang) -> &'static str {
        match self {
            Self::Gtc => tr(lang, "Limit (GTC)", "限价单 (GTC)"),
            Self::PostOnly => tr(lang, "Maker (Post-Only)", "只做 Maker"),
            Self::Fak => tr(lang, "FAK Immediate", "立即成交剩余撤单 (FAK)"),
            Self::Fok => tr(lang, "Fill-or-Kill (FOK)", "全成或全撤 (FOK)"),
            Self::TimedFok => tr(lang, "Timed FOK", "限时全成"),
            Self::Sweep => tr(lang, "Market Sweep", "市价扫盘"),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PendingOrderConfirm {
    pub market: MarketSummary,
    pub outcome_idx: usize,
    pub outcome_name: String,
    /// Asset id the quote was built from — used to detect a moved book before submit.
    pub asset_id: String,
    /// When the quote was captured. A stale confirmation must never execute silently.
    pub created_at: Instant,
    pub side: String,
    pub raw_price: f64,
    pub est_shares: f64,
    pub est_usd: f64,
    pub best_price: Option<f64>,
    pub best_level_shares: Option<f64>,
    pub vwap_price: Option<f64>,
    pub worst_price: Option<f64>,
    pub levels_consumed: usize,
    pub slippage_bps: f64,
    pub filled_completely: bool,
    pub fee_usd: f64,
    pub choice: OrderChoice,
    pub max_notional_usd: Option<f64>,
    pub is_limit: bool,
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
    News,
    Automation,
    Alerts,
    WhalesTape,
    CopyTrade,
    Fees,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingLiveExit {
    SwitchToPaper,
    LockWallet,
    SwitchProfile(String),
}

pub enum UiMsg {
    MarketsLoaded { markets: Vec<MarketSummary>, append: bool },
    PresetMarketsLoaded(Vec<MarketSummary>),
    RolloverMarketLoaded(MarketSummary),
    OpenPriceLoaded { condition_id: String, open_price: f64 },
    OpenPriceFailed { condition_id: String },
    HistoryLoaded {
        asset_id: String,
        interval: &'static str,
        req_gen: u64,
        result: Result<Vec<PricePoint>, String>,
    },
    WhalesLoaded(String, WhaleFlowSummary),
    SiblingsLoaded(String, EventSiblings),
    NewsLoaded {
        condition_id: String,
        req_gen: u64,
        result: Result<NewsFeedResult, String>,
    },
    TranslationsReady,
    RulesUpdated,
    AlertsUpdated(Vec<PriceAlert>),
    AlertTriggered(AlertTriggeredEvent),
    AccountUpdated(AccountState),
    OrderFinished { asset_id: String },
    CtfFinished(Result<CtfTxResult, String>),
    WalletCreated {
        wallet: GeneratedWallet,
        account: AccountState,
        deposit: Option<DepositAddressesInfo>,
    },
    WalletRevealed(GeneratedWallet),
    DepositLoaded(DepositAddressesInfo),
    UpdateChecked {
        status: UpdateStatus,
        is_auto: bool,
    },
    ColdUpdateStaged {
        result: Result<StagedColdUpdate, String>,
        auto_relaunch: bool,
    },
    Toast { text: String, is_err: bool },
}

#[derive(Clone)]
pub(crate) struct UiSender {
    tx: mpsc::UnboundedSender<UiMsg>,
    ctx: Context,
}

impl UiSender {
    pub(crate) fn send(&self, msg: UiMsg) {
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }

    pub(crate) fn send_silent(&self, msg: UiMsg) {
        let _ = self.tx.send(msg);
    }
}

pub struct PolyDeskDesktop {
    pub(crate) core: Arc<CoreEngine>,
    pub(crate) rt: Arc<Runtime>,
    pub(crate) tx: UiSender,
    rx: mpsc::UnboundedReceiver<UiMsg>,

    pub(crate) lang: Lang,
    pub(crate) light_mode: bool,
    pub(crate) odds_fmt: OddsFormat,
    pub(crate) modal: Option<ActiveModal>,
    pub(crate) account: AccountState,
    pub(crate) update_status: Option<UpdateStatus>,
    pub(crate) auto_check_updates: bool,
    pub(crate) auto_apply_hot_updates: bool,
    pub(crate) auto_stage_cold_updates: bool,
    pub(crate) update_check_in_flight: bool,
    pub(crate) cold_update_in_flight: bool,
    pub(crate) staged_cold_update: Option<StagedColdUpdate>,
    pub(crate) active_hot_patch: Option<HotPatchManifest>,
    pub(crate) dismissed_announcement_id: Option<String>,
    last_update_check: Instant,
    pub(crate) deposit_info: Option<DepositAddressesInfo>,

    show_markets_rail: bool,
    markets: Vec<MarketSummary>,
    markets_offset: usize,
    loading_more_markets: bool,
    search_query: String,
    category: String,
    watchlist_set: HashSet<String>,
    active_market: Option<MarketSummary>,
    active_outcome_idx: usize,
    siblings: Option<EventSiblings>,
    history: Vec<PricePoint>,
    share_live_ticks: Vec<(i64, f64)>,
    history_req_gen: u64,
    history_loading: bool,
    history_error: Option<String>,
    chart_interval: &'static str,
    chart_mode: ChartMode,
    short_chart_view: ShortChartView,
    indicators: IndicatorConfig,
    chart_cache: ChartCache,
    oracle_chart_state: OracleChartSmoothState,
    whale_flow: Option<WhaleFlowSummary>,
    news_query_input: String,
    news_auto_query: String,
    news_window: NewsWindow,
    news_edition: NewsEdition,
    news_req_gen: u64,
    news_loading: bool,
    news_error: Option<String>,
    news_feed: Option<NewsFeedResult>,
    ladder_state: LadderState,

    order_in_flight: HashSet<String>,
    order_in_flight_since: HashMap<String, Instant>,

    layout_mode: LayoutMode,
    pinned_markets: Vec<MarketSummary>,
    grid_ladders: HashMap<String, LadderState>,
    grid_outcome_idx: HashMap<String, usize>,
    grid_stake_inputs: HashMap<String, String>,
    crypto_open_prices: HashMap<String, f64>,
    crypto_open_fetching: HashSet<String>,
    auto_next_round: bool,
    last_rollover_cond: Option<String>,
    start_slug_pending: Option<String>,

    cached_alerts: Vec<PriceAlert>,
    alert_cond_kind: u8,
    alert_thresh_cents: f64,
    alert_sound: bool,
    unread_alerts: usize,
    alert_banner: Option<String>,

    cached_rules: Vec<AutomationRule>,
    cached_copy_targets: Vec<CopyTarget>,
    stake_presets: Vec<f64>,

    // Quick Trade state
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
    pub(crate) confirm_delete_vault: bool,
    pub(crate) delete_vault_confirm_input: String,
    pub(crate) pending_live_exit: Option<PendingLiveExit>,
    pub(crate) ctf_amount_input: String,
    pub(crate) pending_ctf: Option<CtfRequest>,
    pub(crate) ctf_in_flight: bool,
    pub(crate) last_ctf_result: Option<CtfTxResult>,
    pub(crate) new_profile_input: String,
    pub(crate) rename_profile_input: String,
    pub(crate) renaming_profile: bool,
    pub(crate) polygon_rpc_input: String,

    pub(crate) repaint_gap_atomic: Arc<AtomicU64>,
    pub(crate) settings_tab: SettingsTab,
    pub(crate) pending_order_confirm: Option<PendingOrderConfirm>,
    pub(crate) confirm_before_order: bool,
    pub(crate) confirm_on_high_slippage: bool,
    pub(crate) confirm_ladder_orders: bool,
    pub(crate) slippage_warn_bps: f64,
    pub(crate) quick_buy_presets: Vec<f64>,
    pub(crate) ui_repaint_ms: u64,
    pub(crate) live_tick_ms: u64,
    pub(crate) short_chart_bucket_secs: i64,
    pub(crate) primary_btc_live_lead: bool,
    pub(crate) short_chart_window_secs: i64,
    pub(crate) show_oracle_fast_lead: bool,
    pub(crate) show_oracle_cl_ref: bool,
    pub(crate) show_oracle_twap: bool,
    pub(crate) show_oracle_lag_badge: bool,
    pub(crate) show_line_guide: bool,
    pub(crate) ui_zoom: f32,

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
        let ui_zoom = cfg.ui_zoom.clamp(0.85, 1.25);
        apply_theme(&cc.egui_ctx, light_mode);
        cc.egui_ctx.set_zoom_factor(ui_zoom);
        install_fonts(&cc.egui_ctx);

        let (raw_tx, rx) = mpsc::unbounded_channel();
        let tx = UiSender {
            tx: raw_tx,
            ctx: cc.egui_ctx.clone(),
        };
        let account = rt.block_on(core.execution.account_state());
        let watchlist_set: HashSet<String> = cfg.watchlist.iter().cloned().collect();
        let cached_alerts = core.alerts.list();
        let cached_rules = core.automation.list_rules();
        let cached_copy_targets = core.automation.list_copy_targets();
        let ui_repaint_ms = cfg.ui_repaint_ms.clamp(4, 50);
        let repaint_gap_atomic = Arc::new(AtomicU64::new(ui_repaint_ms));
        let default_short_chart_view = match std::env::var("POLYDIRECT_START_CHART").as_deref() {
            Ok("overlay") => ShortChartView::Overlay,
            Ok("spot") => ShortChartView::Spot,
            Ok("shares") => ShortChartView::Shares,
            Ok("both") => ShortChartView::Both,
            _ => match cfg.default_short_chart_view.as_str() {
                "overlay" => ShortChartView::Overlay,
                "spot" | "oracle" => ShortChartView::Spot,
                "shares" => ShortChartView::Shares,
                _ => ShortChartView::Both,
            },
        };
        let initial_modal = match std::env::var("POLYDIRECT_START_MODAL").as_deref() {
            Ok("settings") => Some(ActiveModal::SettingsAndUpdates),
            Ok("wallet") => Some(ActiveModal::WalletAndDeposit),
            _ => None,
        };
        let initial_settings_tab = match std::env::var("POLYDIRECT_START_SETTINGS_TAB").as_deref() {
            Ok("network") | Ok("updates") => SettingsTab::NetworkUpdates,
            Ok("charts") => SettingsTab::ChartsSpeed,
            Ok("appearance") => SettingsTab::Appearance,
            _ => SettingsTab::Trading,
        };
        let active_hot_patch = core.current_hot_patch();
        let quick_buy_presets = if cfg.quick_buy_presets.is_empty() {
            vec![1.0, 10.0, 50.0, 100.0]
        } else {
            cfg.quick_buy_presets.clone()
        };

        let mut app = Self {
            core: core.clone(),
            rt: rt.clone(),
            tx: tx.clone(),
            rx,
            lang: Lang::from_code(&cfg.language),
            light_mode,
            odds_fmt: OddsFormat::from_str_code(&cfg.odds_format),
            modal: initial_modal,
            account,
            update_status: None,
            auto_check_updates: cfg.auto_check_updates,
            auto_apply_hot_updates: cfg.auto_apply_hot_updates,
            auto_stage_cold_updates: cfg.auto_stage_cold_updates,
            update_check_in_flight: false,
            cold_update_in_flight: false,
            staged_cold_update: None,
            active_hot_patch,
            dismissed_announcement_id: None,
            last_update_check: Instant::now(),
            deposit_info: None,
            show_markets_rail: true,
            markets: Vec::new(),
            markets_offset: 0,
            loading_more_markets: false,
            search_query: String::new(),
            category: "all".into(),
            watchlist_set,
            active_market: None,
            active_outcome_idx: 0,
            siblings: None,
            history: Vec::new(),
            share_live_ticks: Vec::new(),
            history_req_gen: 0,
            history_loading: false,
            history_error: None,
            chart_interval: "1d",
            chart_mode: ChartMode::Line,
            short_chart_view: default_short_chart_view,
            indicators: IndicatorConfig::default(),
            chart_cache: ChartCache::default(),
            oracle_chart_state: OracleChartSmoothState::default(),
            whale_flow: None,
            news_query_input: String::new(),
            news_auto_query: String::new(),
            news_window: NewsWindow::Day24h,
            news_edition: if cfg.language.eq_ignore_ascii_case("zh") {
                NewsEdition::Zh
            } else {
                NewsEdition::En
            },
            news_req_gen: 0,
            news_loading: false,
            news_error: None,
            news_feed: None,
            ladder_state: LadderState::default(),
            order_in_flight: HashSet::new(),
            order_in_flight_since: HashMap::new(),
            layout_mode: LayoutMode::Single,
            pinned_markets: Vec::new(),
            grid_ladders: HashMap::new(),
            grid_outcome_idx: HashMap::new(),
            grid_stake_inputs: HashMap::new(),
            crypto_open_prices: HashMap::new(),
            crypto_open_fetching: HashSet::new(),
            auto_next_round: false,
            last_rollover_cond: None,
            start_slug_pending: std::env::var("POLYDIRECT_START_SLUG")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            cached_alerts,
            alert_cond_kind: 0,
            alert_thresh_cents: 60.0,
            alert_sound: true,
            unread_alerts: 0,
            alert_banner: None,
            cached_rules,
            cached_copy_targets,
            stake_presets: cfg.stake_presets.clone(),
            quick_buy: true,
            quick_limit_mode: false,
            quick_amount_input: "25".into(),
            quick_limit_cents: 50.0,
            order_choice: OrderChoice::from_config_str(&cfg.default_order_type),
            stake_value: 25.0,
            stake_in_usd: true,
            timed_fok_ms: 1500,
            right_tab: RightTab::QuickTrade,
            deck_tab: if std::env::var("POLYDIRECT_START_DECK").as_deref() == Ok("news") {
                DeckTab::News
            } else {
                DeckTab::Positions
            },
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
            confirm_delete_vault: false,
            delete_vault_confirm_input: String::new(),
            pending_live_exit: None,
            ctf_amount_input: "10".into(),
            pending_ctf: None,
            ctf_in_flight: false,
            last_ctf_result: None,
            new_profile_input: String::new(),
            rename_profile_input: String::new(),
            renaming_profile: false,
            polygon_rpc_input: cfg.polygon_rpc_url.clone(),
            repaint_gap_atomic: repaint_gap_atomic.clone(),
            settings_tab: initial_settings_tab,
            pending_order_confirm: None,
            confirm_before_order: cfg.confirm_before_order,
            confirm_on_high_slippage: cfg.confirm_on_high_slippage,
            confirm_ladder_orders: cfg.confirm_ladder_orders,
            slippage_warn_bps: cfg.slippage_warn_bps.max(1.0),
            quick_buy_presets,
            ui_repaint_ms,
            live_tick_ms: cfg.live_tick_ms.clamp(50, 1000),
            short_chart_bucket_secs: cfg.short_chart_bucket_secs.clamp(1, 10),
            primary_btc_live_lead: cfg.primary_btc_live_lead,
            short_chart_window_secs: match cfg.short_chart_window_secs {
                60 | 180 | 300 => cfg.short_chart_window_secs,
                _ => 180,
            },
            show_oracle_fast_lead: cfg.show_oracle_fast_lead,
            show_oracle_cl_ref: cfg.show_oracle_cl_ref,
            show_oracle_twap: cfg.show_oracle_twap,
            show_oracle_lag_badge: cfg.show_oracle_lag_badge,
            show_line_guide: false,
            ui_zoom,
            toasts: Vec::new(),
            last_account_poll: Instant::now(),
        };

        // Task A (M9 + Oracle): Dedicated WS feed + Chainlink oracle repaint coalescer
        let ctx_rep = cc.egui_ctx.clone();
        let core_feed = core.clone();
        let repaint_gap_ref = repaint_gap_atomic;
        rt.spawn(async move {
            let mut feed_rx = core_feed.feed.subscribe_events();
            let mut oracle_rx = core_feed.oracle.subscribe_events();
            let init_gap = Duration::from_millis(repaint_gap_ref.load(Ordering::Relaxed).clamp(4, 50));
            let mut last_feed_repaint = Instant::now() - init_gap;
            let mut pending_flush: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;

            loop {
                let tick_received = tokio::select! {
                    _ = async {
                        if let Some(sleep) = pending_flush.as_mut() {
                            sleep.as_mut().await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                    }, if pending_flush.is_some() => {
                        pending_flush = None;
                        last_feed_repaint = Instant::now();
                        ctx_rep.request_repaint();
                        false
                    }
                    res = feed_rx.recv() => match res {
                        Ok(_) => true,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                    res = oracle_rx.recv() => match res {
                        Ok(_) => true,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => false,
                    }
                };
                if tick_received {
                    let min_gap = Duration::from_millis(repaint_gap_ref.load(Ordering::Relaxed).clamp(4, 50));
                    let now = Instant::now();
                    let elapsed = now.saturating_duration_since(last_feed_repaint);
                    if elapsed >= min_gap {
                        pending_flush = None;
                        last_feed_repaint = now;
                        ctx_rep.request_repaint();
                    } else if pending_flush.is_none() {
                        pending_flush = Some(Box::pin(tokio::time::sleep(min_gap - elapsed)));
                    }
                }
            }
        });

        // Task B (M9 + Alerts): Dedicated unstarved execution, automation & alert event forwarder
        let tx_ev = tx.clone();
        let core_ev = core.clone();
        rt.spawn(async move {
            let mut exec_rx = core_ev.execution.subscribe_events();
            let mut auto_rx = core_ev.automation.subscribe_events();
            let mut alert_rx = core_ev.alerts.subscribe_events();

            loop {
                tokio::select! {
                    res = exec_rx.recv() => {
                        match res {
                            Ok(ev) => {
                                match ev {
                                    polydesk::execution::ExecEvent::AccountUpdated => {
                                        let acc = core_ev.execution.account_state().await;
                                        tx_ev.send(UiMsg::AccountUpdated(acc));
                                    }
                                    polydesk::execution::ExecEvent::ModeChanged { .. } => {
                                        let acc = core_ev.execution.account_state().await;
                                        tx_ev.send(UiMsg::AccountUpdated(acc));
                                        tx_ev.send(UiMsg::RulesUpdated);
                                    }
                                    polydesk::execution::ExecEvent::OrderAck { ack } => {
                                        let msg = format!(
                                            "{} {:.1} @ {:.1}¢ · {} ({:.0}µs / {:.1}ms)",
                                            ack.side, ack.size, ack.price * 100.0, ack.status, ack.sign_us, ack.rtt_ms
                                        );
                                        tx_ev.send(UiMsg::Toast { text: msg, is_err: ack.error.is_some() });
                                    }
                                    polydesk::execution::ExecEvent::Fill { fill } => {
                                        tx_ev.send(UiMsg::Toast {
                                            text: format!("FILLED {} {:.1} @ {:.1}¢", fill.side, fill.shares, fill.price * 100.0),
                                            is_err: false,
                                        });
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                if core_ev.execution.mode() == TradingMode::Live {
                                    let _ = core_ev.execution.refresh_live_account().await;
                                }
                                let acc = core_ev.execution.account_state().await;
                                tx_ev.send(UiMsg::AccountUpdated(acc));
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    res = auto_rx.recv() => {
                        match res {
                            Ok(ev) => {
                                match ev {
                                    polydesk::automation::AutoEvent::RuleTriggered { message, is_err, .. }
                                    | polydesk::automation::AutoEvent::CopyExecuted { message, is_err, .. } => {
                                        tx_ev.send(UiMsg::Toast { text: message, is_err });
                                        tx_ev.send(UiMsg::RulesUpdated);
                                    }
                                    polydesk::automation::AutoEvent::RulesUpdated => {
                                        tx_ev.send(UiMsg::RulesUpdated);
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                tx_ev.send(UiMsg::RulesUpdated);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    res = alert_rx.recv() => {
                        match res {
                            Ok(ev) => tx_ev.send(UiMsg::AlertTriggered(ev)),
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                tx_ev.send(UiMsg::AlertsUpdated(core_ev.alerts.list()));
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {}
                        }
                    }
                }
            }
        });

        let alert_assets: Vec<String> = app
            .cached_alerts
            .iter()
            .filter(|a| a.active && !a.asset_id.is_empty())
            .map(|a| a.asset_id.clone())
            .collect();
        if !alert_assets.is_empty() {
            app.core.feed.subscribe(alert_assets);
        }
        app.sync_pinned_feed_assets();
        app.trigger_load_markets();
        if !cfg.github_repo.is_empty() && cfg.auto_check_updates {
            app.trigger_check_update(true);
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
        open_orders_for(&self.account)
    }

    fn all_positions(&self) -> &[PaperPosition] {
        positions_for(&self.account)
    }

    fn active_position(&self) -> Option<&PaperPosition> {
        active_position_for(&self.account, self.active_asset_id())
    }

    fn compute_shares(&self, price: f64) -> f64 {
        if !self.stake_in_usd {
            self.stake_value.max(1.0)
        } else {
            ((self.stake_value / price.max(0.005)) * 100.0).round() / 100.0
        }
    }

    // ---------- Bilingual Display & Async Actions ----------

    fn display_text(&self, en: &str) -> String {
        display_text_with(self.lang, &self.core.translator, en)
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
                tx.send(UiMsg::TranslationsReady);
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
                Ok(ms) => { tx.send(UiMsg::MarketsLoaded { markets: ms, append }); }
                Err(e) => { tx.send(UiMsg::Toast { text: format!("Markets error: {e}"), is_err: true }); }
            }
        });
    }

    fn collect_pinned_feed_assets(&self) -> HashSet<String> {
        let mut pinned = HashSet::new();
        if let Some(ref m) = self.active_market {
            for tid in &m.token_ids {
                if !tid.is_empty() {
                    pinned.insert(tid.clone());
                }
            }
        }
        for pm in &self.pinned_markets {
            for tid in &pm.token_ids {
                if !tid.is_empty() {
                    pinned.insert(tid.clone());
                }
            }
        }
        for p in &self.account.paper.positions {
            if p.shares > 1e-6 {
                pinned.insert(p.asset_id.clone());
            }
        }
        for p in &self.account.live_positions {
            if p.shares > 1e-6 {
                pinned.insert(p.asset_id.clone());
            }
        }
        for o in &self.account.paper.open_orders {
            pinned.insert(o.asset_id.clone());
        }
        for o in &self.account.live_open_orders {
            pinned.insert(o.asset_id.clone());
        }
        for r in &self.cached_rules {
            if r.active {
                pinned.insert(r.asset_id.clone());
            }
        }
        for a in &self.cached_alerts {
            if a.active && !a.asset_id.is_empty() {
                pinned.insert(a.asset_id.clone());
            }
        }
        pinned
    }

    fn sync_pinned_feed_assets(&self) {
        self.core
            .feed
            .set_pinned_assets(self.collect_pinned_feed_assets());
    }

    fn sync_oracle_watches(&self) {
        let mut syms = HashSet::new();
        if let Some(ref m) = self.active_market
            && let Some(info) = extract_short_timeframe_info(m)
        {
            syms.insert(info.chainlink_symbol);
        }
        for pm in &self.pinned_markets {
            if let Some(info) = extract_short_timeframe_info(pm) {
                syms.insert(info.chainlink_symbol);
            }
        }
        self.core.oracle.set_watched_symbols(syms);
    }

    fn pin_market_to_grid(&mut self, market: MarketSummary) {
        if !insert_pinned_market(&mut self.pinned_markets, market.clone()) {
            self.push_toast(
                tr(self.lang, "Split grid holds up to 4 markets (unpin one first)", "分屏最多固定 4 个市场，请先取消固定一个").into(),
                true,
            );
            return;
        }
        self.core.feed.subscribe(market.token_ids.clone());
        let core_w = self.core.clone();
        let tids = market.token_ids.clone();
        self.rt.spawn(async move { core_w.execution.prewarm_market(&tids).await; });
        if let Some(aid) = market.token_ids.first().cloned() {
            let core_b = self.core.clone();
            let cid = market.condition_id.clone();
            self.rt.spawn(async move {
                if let Ok((bids, asks, ts_tick, lt, ts, hash)) = core_b.market_client.fetch_book_rest(&aid).await {
                    core_b.feed.seed_book(&aid, &cid, ts_tick, &bids, &asks, lt, ts, hash);
                }
            });
        }
        let mut ls = LadderState::default();
        ls.reset_for_market(market.tick_size.max(0.001));
        self.grid_ladders.entry(market.condition_id.clone()).or_insert(ls);
        self.sync_pinned_feed_assets();
        self.sync_oracle_watches();
        self.maybe_fetch_open_price(&market);
    }

    fn unpin_market(&mut self, condition_id: &str) {
        let removed_tokens: Vec<String> = self
            .pinned_markets
            .iter()
            .filter(|m| m.condition_id == condition_id)
            .flat_map(|m| m.token_ids.iter().cloned())
            .filter(|t| !t.is_empty())
            .collect();
        self.pinned_markets.retain(|m| m.condition_id != condition_id);
        self.grid_ladders.remove(condition_id);
        self.grid_outcome_idx.remove(condition_id);
        self.grid_stake_inputs.remove(condition_id);
        let still_needed = self.collect_pinned_feed_assets();
        self.core.feed.set_pinned_assets(still_needed.clone());
        let to_unsub: Vec<String> = removed_tokens
            .into_iter()
            .filter(|t| !still_needed.contains(t))
            .collect();
        if !to_unsub.is_empty() {
            self.core.feed.unsubscribe(to_unsub);
        }
        self.sync_oracle_watches();
    }

    fn maybe_fetch_open_price(&mut self, m: &MarketSummary) {
        let Some(info) = extract_short_timeframe_info(m) else {
            return;
        };
        if self.crypto_open_prices.contains_key(&m.condition_id)
            || self.crypto_open_fetching.contains(&m.condition_id)
        {
            return;
        }
        let step_secs = i64::from(info.interval_mins) * 60;
        let start_iso = m.event_start_time.clone().or_else(|| {
            info.slot_ts
                .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
                .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        });
        let end_iso = m.end_date.clone().or_else(|| {
            info.slot_ts
                .and_then(|s| chrono::DateTime::from_timestamp(s + step_secs, 0))
                .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        });
        let (Some(start_iso), Some(end_iso)) = (start_iso, end_iso) else {
            return;
        };
        self.crypto_open_fetching.insert(m.condition_id.clone());
        let core = self.core.clone();
        let tx = self.tx.clone();
        let cid = m.condition_id.clone();
        let sym = info.coin.to_ascii_uppercase();
        let variant = info.variant;
        self.rt.spawn(async move {
            match core
                .market_client
                .fetch_crypto_open_price(&sym, &start_iso, &end_iso, variant)
                .await
            {
                Ok(open_price) => tx.send(UiMsg::OpenPriceLoaded {
                    condition_id: cid,
                    open_price,
                }),
                Err(_) => tx.send(UiMsg::OpenPriceFailed { condition_id: cid }),
            }
        });
    }

    fn trigger_load_5m_4up(&mut self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            match core.market_client.fetch_short_timeframe_markets("5m", 16).await {
                Ok(ms) => {
                    let mut chosen = Vec::with_capacity(4);
                    for coin in ["btc", "eth", "sol", "xrp"] {
                        if let Some(m) = ms.iter().find(|cand| {
                            extract_short_timeframe_info(cand)
                                .is_some_and(|i| i.coin == coin && i.interval_mins == 5)
                        }) {
                            chosen.push(m.clone());
                        }
                    }
                    if chosen.is_empty() {
                        chosen = ms.into_iter().take(4).collect();
                    }
                    tx.send(UiMsg::PresetMarketsLoaded(chosen));
                }
                Err(e) => tx.send(UiMsg::Toast {
                    text: format!("5m 4-Up load error: {e}"),
                    is_err: true,
                }),
            }
        });
    }

    fn trigger_next_round(&mut self, current: &MarketSummary) {
        let info = extract_short_timeframe_info(current);
        let now_secs = chrono::Utc::now().timestamp();
        let next_slug = next_updown_slug(&current.slug, now_secs).or_else(|| {
            info.as_ref().map(|i| {
                deterministic_updown_slug(
                    &i.coin,
                    i.interval_mins,
                    now_secs + i64::from(i.interval_mins) * 60,
                )
            })
        });
        let Some(next_slug) = next_slug else {
            return;
        };
        let cur_cid = current.condition_id.clone();
        let core = self.core.clone();
        let tx = self.tx.clone();
        let l = self.lang;
        self.rt.spawn(async move {
            if let Ok(Some(m)) = core.market_client.fetch_market_by_slug(&next_slug).await
                && m.condition_id != cur_cid
            {
                tx.send(UiMsg::RolloverMarketLoaded(m));
                return;
            }
            if let Some(i) = info {
                let tag = if i.interval_mins == 15 { "15m" } else { "5m" };
                if let Ok(ms) = core.market_client.fetch_short_timeframe_markets(tag, 16).await
                    && let Some(m) = ms.into_iter().find(|cand| {
                        cand.condition_id != cur_cid
                            && extract_short_timeframe_info(cand).is_some_and(|ci| ci.coin == i.coin)
                    })
                {
                    tx.send(UiMsg::RolloverMarketLoaded(m));
                    return;
                }
            }
            tx.send(UiMsg::Toast {
                text: tr(l, "Next round market not live on Gamma yet", "下一轮市场暂未上线").into(),
                is_err: true,
            });
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
        self.share_live_ticks.clear();
        self.chart_cache.clear();
        self.oracle_chart_state.reset();
        self.history_error = None;
        self.history_loading = false;
        self.last_rollover_cond = None;

        self.core.feed.subscribe(market.token_ids.clone());
        self.sync_pinned_feed_assets();
        self.sync_oracle_watches();
        self.maybe_fetch_open_price(&market);
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
                tx_wh.send(UiMsg::WhalesLoaded(cid_wh, wf));
            }
        });

        if !market.event_slug.is_empty() {
            let core_s = self.core.clone();
            let tx_s = self.tx.clone();
            let eslug = market.event_slug.clone();
            self.rt.spawn(async move {
                if let Ok(sib) = core_s.market_client.fetch_event_siblings(&eslug).await {
                    tx_s.send(UiMsg::SiblingsLoaded(eslug, sib));
                }
            });
        }

        let auto_q = build_market_news_query(&market);
        if auto_q != self.news_auto_query || self.news_feed.is_none() {
            self.news_auto_query = auto_q.clone();
            self.news_query_input = auto_q;
            self.news_feed = None;
            self.news_error = None;
            self.trigger_load_news(false);
        }
    }

    fn trigger_load_news(&mut self, force_refresh: bool) {
        let Some(cid) = self.active_market.as_ref().map(|m| m.condition_id.clone()) else {
            return;
        };
        let q = {
            let trimmed = self.news_query_input.trim();
            if trimmed.is_empty() {
                self.news_auto_query.clone()
            } else {
                trimmed.to_string()
            }
        };
        if q.trim().is_empty() {
            return;
        }
        self.news_req_gen = self.news_req_gen.wrapping_add(1);
        let req_gen = self.news_req_gen;
        self.news_loading = true;
        self.news_error = None;
        let core = self.core.clone();
        let tx = self.tx.clone();
        let window = self.news_window;
        let edition = self.news_edition;
        self.rt.spawn(async move {
            let result = core
                .news
                .fetch_news(&q, window, edition, force_refresh)
                .await
                .map_err(|e| e.to_string());
            tx.send(UiMsg::NewsLoaded {
                condition_id: cid,
                req_gen,
                result,
            });
        });
    }

    fn trigger_load_history(&mut self, aid: &str) {
        self.history_req_gen = self.history_req_gen.wrapping_add(1);
        self.history_loading = true;
        self.history_error = None;
        let req_gen = self.history_req_gen;
        let core = self.core.clone();
        let tx = self.tx.clone();
        let tok = aid.to_string();
        let stf_info = self
            .active_market
            .as_ref()
            .and_then(extract_short_timeframe_info);
        let is_short_tf = stf_info.is_some();
        let slot_start_ts = stf_info.and_then(|i| i.slot_ts);
        let iv = if is_short_tf && self.short_chart_view != ShortChartView::Shares {
            "1h"
        } else {
            self.chart_interval
        };
        let fid = if iv == "1h" { 1 } else if iv == "6h" { 2 } else { 10 };
        self.rt.spawn(async move {
            let result = if let Some(st) = slot_start_ts
                && iv == "1h"
            {
                core.market_client
                    .fetch_price_history_since(&tok, st - 60, 1)
                    .await
                    .map_err(|e| e.to_string())
            } else {
                core.market_client
                    .fetch_price_history(&tok, iv, fid)
                    .await
                    .map_err(|e| e.to_string())
            };
            tx.send(UiMsg::HistoryLoaded {
                asset_id: tok,
                interval: iv,
                req_gen,
                result,
            });
        });
    }

    /// Place an order from a hotkey. Always honours the user's global
    /// `confirm_before_order` setting — a single keystroke must not be able to skip
    /// the slippage preview and the confirmation gate.
    fn place_order(&mut self, side: &str, raw_price: f64, choice: OrderChoice) {
        let Some(m) = self.active_market.clone() else { return; };
        let tick_ticks = ((m.tick_size * 10_000.0).round() as usize).max(1);
        let price = align_to_tick(raw_price, tick_ticks);
        let size = self.compute_shares(price);
        let usd_cap = (choice == OrderChoice::Sweep
            && side.eq_ignore_ascii_case("BUY")
            && self.stake_in_usd)
            .then_some(self.stake_value);
        let outcome_idx = self.active_outcome_idx;
        self.submit_order_with_confirm_for_market(
            &m,
            outcome_idx,
            side,
            price,
            size,
            choice,
            usd_cap,
            confirm_required(self.confirm_before_order, false),
        );
    }

    /// Immediate placement used only where the user has explicitly opted out of
    /// per-order confirmation (single-click ladder/DOM mode).
    fn place_order_immediate(&mut self, side: &str, raw_price: f64, choice: OrderChoice) {
        let Some(m) = self.active_market.clone() else { return; };
        let tick_ticks = ((m.tick_size * 10_000.0).round() as usize).max(1);
        let price = align_to_tick(raw_price, tick_ticks);
        let size = self.compute_shares(price);
        let usd_cap = (choice == OrderChoice::Sweep
            && side.eq_ignore_ascii_case("BUY")
            && self.stake_in_usd)
            .then_some(self.stake_value);
        let outcome_idx = self.active_outcome_idx;
        self.place_order_for_market(&m, outcome_idx, side, price, size, choice, usd_cap);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_order_confirm_for_market(
        &self,
        m: &MarketSummary,
        outcome_idx: usize,
        side: &str,
        raw_price: f64,
        shares: f64,
        choice: OrderChoice,
        max_notional_usd: Option<f64>,
    ) -> Option<PendingOrderConfirm> {
        let aid = m
            .token_ids
            .get(outcome_idx)
            .or_else(|| m.token_ids.first())?
            .clone();
        let outcome_name = m
            .outcomes
            .get(outcome_idx)
            .cloned()
            .unwrap_or_else(|| if outcome_idx == 0 { "Yes".into() } else { "No".into() });
        let is_buy = side.eq_ignore_ascii_case("BUY");
        let is_limit = matches!(choice, OrderChoice::Gtc | OrderChoice::PostOnly);
        let top_book = self.core.feed.get_book_view(&aid, 1);
        let (best_price, best_level_shares) = if let Some(ref bv) = top_book {
            if is_buy {
                (bv.best_ask.or(bv.mid), bv.asks.first().map(|l| l.1))
            } else {
                (bv.best_bid.or(bv.mid), bv.bids.first().map(|l| l.1))
            }
        } else {
            (None, None)
        };
        let sweep = if !is_limit {
            let sweep_limit = Some(if is_buy {
                (raw_price * 1.05).min(0.999)
            } else {
                (raw_price * 0.95).max(0.0001)
            });
            let target_usd = if is_buy { max_notional_usd } else { None };
            let target_shares = if target_usd.is_some() { None } else { Some(shares) };
            self.core
                .execution
                .preview_sweep(&aid, is_buy, target_usd, target_shares, sweep_limit)
        } else {
            None
        };
        let (est_shares, est_usd, vwap_price, worst_price, levels_consumed, slippage_bps, filled_completely) =
            if let Some(ref sq) = sweep
                && sq.total_shares > 0.0
            {
                let sh = if is_buy { sq.total_shares } else { shares };
                let usd = sq.total_notional_usd;
                (
                    sh,
                    usd,
                    Some(sq.vwap),
                    Some(sq.worst_price),
                    sq.levels_consumed,
                    sq.slippage_bps,
                    sq.filled_completely,
                )
            } else {
                let tick_ticks = ((m.tick_size * 10_000.0).round() as usize).max(1);
                let px = align_to_tick(raw_price, tick_ticks);
                let usd = shares * px;
                (shares, usd, Some(px), Some(px), 1, 0.0, true)
            };
        let fee_px = vwap_price.unwrap_or(raw_price);
        let is_maker = choice == OrderChoice::PostOnly;
        let p_fee = estimate_protocol_fee_usd(est_shares, fee_px, is_maker, m.fees_enabled);
        let b_fee = estimate_builder_fee_usd(
            est_usd,
            is_maker,
            self.account.builder_maker_bps,
            self.account.builder_taker_bps,
        );
        Some(PendingOrderConfirm {
            market: m.clone(),
            outcome_idx,
            outcome_name,
            asset_id: aid,
            created_at: Instant::now(),
            side: if is_buy { "BUY".into() } else { "SELL".into() },
            raw_price,
            est_shares,
            est_usd,
            best_price,
            best_level_shares,
            vwap_price,
            worst_price,
            levels_consumed,
            slippage_bps,
            filled_completely,
            fee_usd: p_fee + b_fee,
            choice,
            max_notional_usd,
            is_limit,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_order_with_confirm_for_market(
        &mut self,
        m: &MarketSummary,
        outcome_idx: usize,
        side: &str,
        raw_price: f64,
        shares: f64,
        choice: OrderChoice,
        max_notional_usd: Option<f64>,
        force_confirm: bool,
    ) {
        if force_confirm
            && let Some(req) = self.build_order_confirm_for_market(
                m,
                outcome_idx,
                side,
                raw_price,
                shares,
                choice,
                max_notional_usd,
            )
        {
            self.pending_order_confirm = Some(req);
            self.modal = Some(ActiveModal::OrderConfirm);
            return;
        }
        self.place_order_for_market(m, outcome_idx, side, raw_price, shares, choice, max_notional_usd);
    }

    pub(crate) fn confirm_and_execute_pending_order(&mut self) {
        let Some(p) = self.pending_order_confirm.clone() else {
            self.modal = None;
            return;
        };

        // Re-quote check: the modal can sit open indefinitely, and the book moves.
        // Submitting the captured price/size then is a silent slippage event at best
        // and an unfillable order at worst.
        let tick = p.market.tick_size.max(0.001);
        let current_px = self.core.feed.get_top_of_book(&p.asset_id).and_then(
            |(bid, ask, mid)| {
                if p.side.eq_ignore_ascii_case("BUY") {
                    ask.or(mid)
                } else {
                    bid.or(mid)
                }
            },
        );
        if should_requote_pending_order(
            p.is_limit,
            p.side.eq_ignore_ascii_case("BUY"),
            p.created_at.elapsed(),
            tick,
            current_px,
            p.best_price,
        ) {
            let refresh_px = if p.is_limit {
                p.raw_price
            } else {
                current_px.unwrap_or(p.raw_price)
            };
            match self.build_order_confirm_for_market(
                &p.market,
                p.outcome_idx,
                &p.side,
                refresh_px,
                p.est_shares,
                p.choice,
                p.max_notional_usd,
            ) {
                Some(fresh) => {
                    self.pending_order_confirm = Some(fresh);
                    self.modal = Some(ActiveModal::OrderConfirm);
                }
                None => {
                    self.pending_order_confirm = None;
                    self.modal = None;
                }
            }
            self.push_toast(
                tr(
                    self.lang,
                    "Quote moved or expired — review the refreshed numbers and confirm again",
                    "报价已变动或过期，请核对待确认的最新数字后重新确认",
                )
                .into(),
                true,
            );
            return;
        }

        self.pending_order_confirm = None;
        self.modal = None;
        // Submit the *worst* price the user was shown, not the optimistic top-of-book
        // price the sweep was priced against, so the executed order matches the preview.
        let submit_px = if p.is_limit {
            p.raw_price
        } else {
            p.worst_price.unwrap_or(p.raw_price)
        };
        self.place_order_for_market(
            &p.market,
            p.outcome_idx,
            &p.side,
            submit_px,
            p.est_shares,
            p.choice,
            p.max_notional_usd,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn place_order_for_market(
        &mut self,
        m: &MarketSummary,
        outcome_idx: usize,
        side: &str,
        raw_price: f64,
        shares: f64,
        choice: OrderChoice,
        max_notional_usd: Option<f64>,
    ) {
        let Some(aid) = m
            .token_ids
            .get(outcome_idx)
            .or_else(|| m.token_ids.first())
            .cloned()
        else {
            return;
        };
        let outcome_name = m
            .outcomes
            .get(outcome_idx)
            .cloned()
            .unwrap_or_else(|| if outcome_idx == 0 { "Yes".into() } else { "No".into() });
        let tick_ticks = ((m.tick_size * 10_000.0).round() as usize).max(1);
        let price = align_to_tick(raw_price, tick_ticks);
        let mut size = (shares * 100.0).round() / 100.0;
        // The preview may have priced the sweep against a deeper book. Never submit a
        // notional larger than the cap the user reviewed and approved.
        size = clamp_buy_notional(size, price, max_notional_usd);
        if size < 0.01 { return; }

        if !begin_order_in_flight(
            &mut self.order_in_flight,
            &mut self.order_in_flight_since,
            &aid,
            Instant::now(),
        ) {
            self.push_toast(
                tr(self.lang, "Order already in flight for this outcome", "该方向已有订单正在提交").into(),
                true,
            );
            return;
        }

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
            asset_id: aid.clone(),
            condition_id: m.condition_id.clone(),
            market_question: m.question.clone(),
            outcome: outcome_name,
            side: side.to_string(),
            order_type,
            post_only,
            price,
            size,
            expiration_secs: None,
            timed_fok_ms: (choice == OrderChoice::TimedFok).then_some(self.timed_fok_ms),
            max_slippage_bps: (choice == OrderChoice::Sweep).then_some(500.0),
            fees_enabled: Some(m.fees_enabled),
            max_notional_usd,
        };
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            if let Err(e) = core.execution.place_order(req).await {
                tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
            }
            tx.send(UiMsg::OrderFinished { asset_id: aid });
        });
    }

    fn trigger_green_up(&mut self, strategy: &'static str) {
        let Some(aid) = self.active_asset_id().map(|s| s.to_string()) else { return; };
        let opp = self.opposite_asset_id().map(|s| s.to_string());
        self.trigger_green_up_for(aid, opp, strategy);
    }

    fn trigger_green_up_for(&mut self, aid: String, opp: Option<String>, strategy: &'static str) {
        if !begin_order_in_flight(
            &mut self.order_in_flight,
            &mut self.order_in_flight_since,
            &aid,
            Instant::now(),
        ) {
            return;
        }
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            if let Err(e) = core.execution.execute_green_up(&aid, opp.as_deref(), strategy).await {
                tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
            }
            tx.send(UiMsg::OrderFinished { asset_id: aid });
        });
    }

    fn trigger_flatten(&mut self) {
        let Some(pos) = self.active_position().cloned() else { return; };
        let Some(m) = self.active_market.clone() else { return; };
        if !begin_order_in_flight(
            &mut self.order_in_flight,
            &mut self.order_in_flight_since,
            &pos.asset_id,
            Instant::now(),
        ) {
            return;
        }
        let px = self
            .core
            .feed
            .get_top_of_book(&pos.asset_id)
            .and_then(|(b, _, m)| b.or(m))
            .unwrap_or(0.01);
        let aid = pos.asset_id.clone();
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
                max_notional_usd: None,
            }).await {
                tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
            }
            tx.send(UiMsg::OrderFinished { asset_id: aid });
        });
    }

    pub(crate) fn trigger_set_mode(&mut self, mode: TradingMode) {
        self.pending_live_exit = (mode == TradingMode::Paper).then_some(PendingLiveExit::SwitchToPaper);
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            match core.execution.set_mode(mode).await {
                Ok(()) => {
                    let acc = core.execution.account_state().await;
                    tx.send(UiMsg::AccountUpdated(acc));
                    let msg = match mode {
                        TradingMode::Paper => "Switched to PAPER mode",
                        TradingMode::Live => "Switched to LIVE mode",
                    };
                    tx.send(UiMsg::Toast { text: msg.into(), is_err: false });
                }
                Err(e) => {
                    let acc = core.execution.account_state().await;
                    tx.send(UiMsg::AccountUpdated(acc));
                    tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
                }
            }
        });
    }

    pub(crate) fn trigger_create_new_wallet(&mut self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let pass = std::mem::take(&mut self.vault_pass_input);
        let persist = self.wallet_persist_vault;
        self.rt.spawn(async move {
            match core.execution.create_and_unlock_new_wallet(pass, persist).await {
                Ok((wallet, account, deposit)) => {
                    tx.send(UiMsg::WalletCreated { wallet, account, deposit });
                    tx.send(UiMsg::Toast {
                        text: "Created new local wallet & fetched USDT/USDC deposit addresses!".into(),
                        is_err: false,
                    });
                }
                Err(e) => {
                    tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
                }
            }
        });
    }

    pub(crate) fn trigger_reveal_backup(&mut self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let mut pass = std::mem::take(&mut self.reveal_pass_input);
        self.rt.spawn(async move {
            let res = core.execution.reveal_vault_backup(&pass);
            zeroize::Zeroize::zeroize(&mut pass);
            match res {
                Ok(w) => {
                    tx.send(UiMsg::WalletRevealed(w));
                }
                Err(e) => {
                    tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
                }
            }
        });
    }

    pub(crate) fn trigger_connect_wallet(&mut self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let pk = std::mem::take(&mut self.wallet_pk_input);
        let funder = (!self.wallet_funder_input.trim().is_empty()).then(|| self.wallet_funder_input.clone());
        let st = self.wallet_sig_type;
        let pass = std::mem::take(&mut self.vault_pass_input);
        let persist = self.wallet_persist_vault;
        self.rt.spawn(async move {
            match core.execution.save_and_unlock_wallet(pk, funder, st, pass, persist).await {
                Ok(acc) => {
                    tx.send(UiMsg::AccountUpdated(acc));
                    tx.send(UiMsg::Toast { text: "Live Polymarket CLOB V2 wallet unlocked!".into(), is_err: false });
                }
                Err(e) => { tx.send(UiMsg::Toast { text: e.to_string(), is_err: true }); }
            }
        });
    }

    pub(crate) fn trigger_unlock_vault(&mut self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let mut pass = std::mem::take(&mut self.vault_pass_input);
        self.rt.spawn(async move {
            let res = core.execution.unlock_existing_vault(&pass).await;
            zeroize::Zeroize::zeroize(&mut pass);
            match res {
                Ok(acc) => {
                    tx.send(UiMsg::AccountUpdated(acc));
                    tx.send(UiMsg::Toast { text: "Vault unlocked — LIVE active".into(), is_err: false });
                }
                Err(e) => { tx.send(UiMsg::Toast { text: e.to_string(), is_err: true }); }
            }
        });
    }

    pub(crate) fn trigger_lock_wallet(&mut self) {
        self.pending_live_exit = Some(PendingLiveExit::LockWallet);
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            match core.execution.lock_wallet().await {
                Ok(acc) => {
                    tx.send(UiMsg::AccountUpdated(acc));
                    tx.send(UiMsg::Toast { text: "Wallet locked (PAPER mode)".into(), is_err: false });
                }
                Err(e) => {
                    let acc = core.execution.account_state().await;
                    tx.send(UiMsg::AccountUpdated(acc));
                    tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
                }
            }
        });
    }

    pub(crate) fn trigger_force_lock_wallet(&self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let acc = core.execution.force_lock_wallet().await;
            tx.send(UiMsg::AccountUpdated(acc));
            tx.send(UiMsg::Toast {
                text: "Keys force-locked — live orders may still be active on Polymarket".into(),
                is_err: true,
            });
        });
    }

    pub(crate) fn trigger_retry_cancel_all_live(&self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let pending = self.pending_live_exit.clone();
        self.rt.spawn(async move {
            match core.execution.cancel_all_live().await {
                Ok(n) => {
                    match pending {
                        Some(PendingLiveExit::LockWallet) => {
                            if let Err(e) = core.execution.lock_wallet().await {
                                let acc = core.execution.account_state().await;
                                tx.send(UiMsg::AccountUpdated(acc));
                                tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
                                return;
                            }
                        }
                        Some(PendingLiveExit::SwitchToPaper) => {
                            if let Err(e) = core.execution.set_mode(TradingMode::Paper).await {
                                let acc = core.execution.account_state().await;
                                tx.send(UiMsg::AccountUpdated(acc));
                                tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
                                return;
                            }
                        }
                        Some(PendingLiveExit::SwitchProfile(target)) => {
                            if let Err(e) = core.execution.switch_vault_profile(&target).await {
                                let acc = core.execution.account_state().await;
                                tx.send(UiMsg::AccountUpdated(acc));
                                tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
                                return;
                            }
                        }
                        None => {}
                    }
                    let acc = core.execution.account_state().await;
                    tx.send(UiMsg::AccountUpdated(acc));
                    tx.send(UiMsg::Toast {
                        text: format!("Cancelled {n} live order(s) on Polymarket"),
                        is_err: false,
                    });
                }
                Err(e) => {
                    let acc = core.execution.account_state().await;
                    tx.send(UiMsg::AccountUpdated(acc));
                    tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
                }
            }
        });
    }

    pub(crate) fn trigger_execute_ctf(&mut self, req: CtfRequest) {
        if self.ctf_in_flight {
            return;
        }
        self.ctf_in_flight = true;
        self.last_ctf_result = None;
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let res = core.execution.execute_ctf(req).await;
            let acc = core.execution.account_state().await;
            tx.send(UiMsg::AccountUpdated(acc));
            tx.send(UiMsg::CtfFinished(res.map_err(|e| e.to_string())));
        });
    }

    pub(crate) fn build_ctf_request_for_condition(
        &self,
        condition_id: &str,
        action: CtfAction,
        amount: f64,
    ) -> Option<CtfRequest> {
        let positions = positions_for(&self.account);
        let matching: Vec<&PaperPosition> = positions
            .iter()
            .filter(|p| p.condition_id == condition_id)
            .collect();
        let market_opt = self
            .active_market
            .iter()
            .chain(self.markets.iter())
            .chain(self.pinned_markets.iter())
            .find(|m| m.condition_id == condition_id && m.token_ids.len() >= 2);

        if let Some(m) = market_opt {
            let tid0 = m.token_ids[0].clone();
            let tid1 = m.token_ids[1].clone();
            let out0 = m
                .outcomes
                .first()
                .cloned()
                .unwrap_or_else(|| "Yes".to_string());
            let out1 = m
                .outcomes
                .get(1)
                .cloned()
                .unwrap_or_else(|| "No".to_string());
            let yes_shares = matching
                .iter()
                .find(|p| p.asset_id == tid0)
                .map(|p| p.shares)
                .unwrap_or(0.0);
            let no_shares = matching
                .iter()
                .find(|p| p.asset_id == tid1)
                .map(|p| p.shares)
                .unwrap_or(0.0);
            return Some(CtfRequest {
                action,
                condition_id: condition_id.to_string(),
                market_question: m.question.clone(),
                yes_asset_id: tid0,
                no_asset_id: tid1,
                yes_outcome: out0,
                no_outcome: out1,
                amount: if amount > 0.0 {
                    amount
                } else {
                    yes_shares + no_shares
                },
                yes_shares,
                no_shares,
                neg_risk: m.neg_risk,
            });
        }

        let first = matching.first().copied()?;
        let second = matching
            .iter()
            .copied()
            .find(|p| p.asset_id != first.asset_id);
        Some(CtfRequest {
            action,
            condition_id: condition_id.to_string(),
            market_question: first.market_question.clone(),
            yes_asset_id: first.asset_id.clone(),
            no_asset_id: second.map(|s| s.asset_id.clone()).unwrap_or_default(),
            yes_outcome: first.outcome.clone(),
            no_outcome: second
                .map(|s| s.outcome.clone())
                .unwrap_or_else(|| "No".into()),
            amount: if amount > 0.0 {
                amount
            } else {
                first.shares + second.map(|s| s.shares).unwrap_or(0.0)
            },
            yes_shares: first.shares,
            no_shares: second.map(|s| s.shares).unwrap_or(0.0),
            neg_risk: false,
        })
    }

    pub(crate) fn trigger_switch_vault_profile(&mut self, target: String) {
        let trimmed = target.trim().to_string();
        if let Err(e) = validate_profile_name(&trimmed) {
            self.push_toast(e.to_string(), true);
            return;
        }
        self.revealed_wallet = None;
        self.confirm_delete_vault = false;
        self.delete_vault_confirm_input.clear();
        self.renaming_profile = false;
        self.pending_live_exit = Some(PendingLiveExit::SwitchProfile(trimmed.clone()));
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            match core.execution.switch_vault_profile(&trimmed).await {
                Ok(acc) => {
                    tx.send(UiMsg::AccountUpdated(acc));
                    tx.send(UiMsg::Toast {
                        text: format!("Switched active vault profile to '{trimmed}'"),
                        is_err: false,
                    });
                }
                Err(e) => {
                    let acc = core.execution.account_state().await;
                    tx.send(UiMsg::AccountUpdated(acc));
                    tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
                }
            }
        });
    }

    pub(crate) fn trigger_rename_vault_profile(&mut self, old_name: String, new_name: String) {
        let trimmed = new_name.trim().to_string();
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            match core.execution.rename_vault_profile(&old_name, &trimmed).await {
                Ok(acc) => {
                    tx.send(UiMsg::AccountUpdated(acc));
                    tx.send(UiMsg::Toast {
                        text: format!("Renamed vault profile '{old_name}' -> '{trimmed}'"),
                        is_err: false,
                    });
                }
                Err(e) => {
                    tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
                }
            }
        });
    }

    pub(crate) fn trigger_wipe_vault(&mut self) {
        self.confirm_delete_vault = false;
        self.delete_vault_confirm_input.clear();
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            match core.execution.wipe_vault().await {
                Ok(acc) => {
                    tx.send(UiMsg::AccountUpdated(acc));
                    tx.send(UiMsg::Toast { text: "Vault deleted".into(), is_err: false });
                }
                Err(e) => {
                    let acc = core.execution.account_state().await;
                    tx.send(UiMsg::AccountUpdated(acc));
                    tx.send(UiMsg::Toast { text: e.to_string(), is_err: true });
                }
            }
        });
    }

    pub(crate) fn trigger_sync_allowance(&self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            match core.execution.sync_balance_allowance().await {
                Ok(acc) => {
                    tx.send(UiMsg::AccountUpdated(acc));
                    tx.send(UiMsg::Toast { text: "Synced live pUSD balance & allowance!".into(), is_err: false });
                }
                Err(e) => { tx.send(UiMsg::Toast { text: e.to_string(), is_err: true }); }
            }
        });
    }

    pub(crate) fn trigger_fetch_deposit(&self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let addr = self.deposit_wallet_input.clone();
        self.rt.spawn(async move {
            match core.execution.fetch_deposit_addresses(&addr).await {
                Ok(info) => { tx.send(UiMsg::DepositLoaded(info)); }
                Err(e) => { tx.send(UiMsg::Toast { text: e.to_string(), is_err: true }); }
            }
        });
    }

    pub(crate) fn trigger_check_update(&mut self, is_auto: bool) {
        if self.update_check_in_flight {
            return;
        }
        let repo = self.github_repo_input.trim().to_string();
        if repo.is_empty() {
            return;
        }
        self.update_check_in_flight = true;
        self.last_update_check = Instant::now();
        let core = self.core.clone();
        let tx = self.tx.clone();
        let (applied_rev, applied_id) = match core.current_hot_patch() {
            Some(p) => (p.revision, Some(p.patch_id)),
            None => (0, None),
        };
        self.rt.spawn(async move {
            let st = check_github_updates(
                core.market_client.http(),
                &repo,
                applied_rev,
                applied_id.as_deref(),
            )
            .await;
            tx.send(UiMsg::UpdateChecked { status: st, is_auto });
        });
    }

    pub(crate) fn apply_verified_hot_patch(&mut self, patch: HotPatchManifest) {
        let l = self.lang;
        match self.core.apply_hot_patch(patch.clone()) {
            Ok(summary) => {
                self.active_hot_patch = Some(patch.clone());
                self.dismissed_announcement_id = None;
                if let Some(ref mut st) = self.update_status {
                    st.hot_patch_available = false;
                    st.applied_hot_revision = summary.revision;
                    st.applied_hot_patch_id = Some(summary.patch_id.clone());
                    st.available_hot_patch_id = None;
                    st.available_hot_revision = None;
                    st.available_hot_summary = None;
                    st.available_hot_summary_zh = None;
                    st.verified_hot_patch = None;
                    st.kind = compute_update_kind(st.cold_update_available, false);
                    st.update_available = st.cold_update_available;
                }
                let msg_summary = if l == Lang::Zh {
                    patch
                        .summary_zh
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or(&patch.summary)
                } else {
                    &patch.summary
                };
                let note = if msg_summary.trim().is_empty() {
                    summary.patch_id.as_str()
                } else {
                    msg_summary.trim()
                };
                self.push_toast(
                    format!(
                        "{} (#{}, {}): {}",
                        tr(l, "Hot-updated live (no restart)", "热更新已实时生效（无需重启）"),
                        summary.revision,
                        summary.patch_id,
                        note
                    ),
                    false,
                );
            }
            Err(e) => {
                self.push_toast(e.to_string(), true);
            }
        }
    }

    pub(crate) fn reset_hot_patch(&mut self) {
        let l = self.lang;
        match self.core.clear_hot_patch() {
            Ok(()) => {
                self.active_hot_patch = None;
                self.dismissed_announcement_id = None;
                if let Some(ref mut st) = self.update_status {
                    st.applied_hot_revision = 0;
                    st.applied_hot_patch_id = None;
                }
                self.push_toast(
                    tr(
                        l,
                        "Cleared cached hot patch & restored baseline config",
                        "已清除热更新缓存并恢复默认配置",
                    )
                    .into(),
                    false,
                );
            }
            Err(e) => self.push_toast(e.to_string(), true),
        }
    }

    pub(crate) fn trigger_stage_cold_update(&mut self, auto_relaunch: bool) {
        if self.cold_update_in_flight {
            return;
        }
        let repo = self.github_repo_input.trim().to_string();
        if repo.is_empty() {
            return;
        }
        self.cold_update_in_flight = true;
        let core = self.core.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let res = download_and_stage_cold_update(core.market_client.http(), &repo)
                .await
                .map_err(|e| e.to_string());
            tx.send(UiMsg::ColdUpdateStaged {
                result: res,
                auto_relaunch,
            });
        });
    }

    pub(crate) fn try_relaunch_staged_update(&mut self) {
        let l = self.lang;
        if !self.order_in_flight.is_empty() || self.ctf_in_flight {
            self.push_toast(
                tr(
                    l,
                    "Wait for active order / CTF tx to finish before restarting",
                    "请等待当前订单或链上交易完成后再重启更新",
                )
                .into(),
                true,
            );
            return;
        }
        let Some(ref staged) = self.staged_cold_update else {
            return;
        };
        match relaunch_exe(&staged.exe_path) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                self.push_toast(
                    format!(
                        "{}: {e}",
                        tr(l, "Failed launching updated binary", "启动新版本程序失败")
                    ),
                    true,
                );
            }
        }
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
                        // One-shot startup slug helper (cleared once PresetMarketsLoaded picks it).
                        if self.start_slug_pending.is_some() {
                            self.trigger_load_5m_4up();
                        }
                        self.select_market(first, 0);
                    }
                    self.trigger_translate_visible();
                }
                UiMsg::PresetMarketsLoaded(ms) => {
                    if ms.is_empty() {
                        self.start_slug_pending = None;
                        self.push_toast(
                            tr(self.lang, "No active 5m crypto markets found right now", "当前暂无活跃 5m 加密货币市场").into(),
                            true,
                        );
                    } else if let Some(needle) = self.start_slug_pending.take()
                        && let Some(pick) = ms
                            .iter()
                            .find(|m| m.slug.to_ascii_lowercase().contains(&needle.to_ascii_lowercase()))
                            .cloned()
                    {
                        self.select_market(pick, 0);
                        self.trigger_translate_visible();
                    } else {
                        self.pinned_markets.clear();
                        self.grid_ladders.clear();
                        self.grid_outcome_idx.clear();
                        for m in ms.into_iter().take(4) {
                            self.pin_market_to_grid(m);
                        }
                        self.layout_mode = LayoutMode::Multi;
                        self.trigger_translate_visible();
                        self.push_toast(
                            tr(self.lang, "Loaded 5m 4-Up Split Grid (BTC / ETH / SOL / XRP)", "已加载 5m 四联分屏 (BTC / ETH / SOL / XRP)").into(),
                            false,
                        );
                    }
                }
                UiMsg::RolloverMarketLoaded(m) => {
                    let msg_q = m.question.clone();
                    self.select_market(m, self.active_outcome_idx);
                    self.trigger_translate_visible();
                    self.push_toast(format!("→ {msg_q}"), false);
                }
                UiMsg::OpenPriceLoaded { condition_id, open_price } => {
                    self.crypto_open_fetching.remove(&condition_id);
                    self.crypto_open_prices.insert(condition_id, open_price);
                }
                UiMsg::OpenPriceFailed { condition_id } => {
                    self.crypto_open_fetching.remove(&condition_id);
                }
                UiMsg::HistoryLoaded {
                    asset_id,
                    interval,
                    req_gen,
                    result,
                } => {
                    let allow_iv = interval == self.chart_interval
                        || (interval == "1h"
                            && self
                                .active_market
                                .as_ref()
                                .and_then(extract_short_timeframe_info)
                                .is_some());
                    if req_gen == self.history_req_gen
                        && self.active_asset_id() == Some(asset_id.as_str())
                        && allow_iv
                    {
                        self.history_loading = false;
                        match result {
                            Ok(mut pts) => {
                                // For 5m/15m markets, trim dead pre-open 50c history so the active round fills the chart
                                if let Some(info) = self.active_market.as_ref().and_then(extract_short_timeframe_info)
                                    && let Some(slot_ts) = info.slot_ts
                                    && pts.len() > 2
                                {
                                    let cutoff = slot_ts - 60;
                                    let first_live = pts.iter().position(|p| p.t >= cutoff);
                                    if let Some(idx) = first_live {
                                        let keep_from = idx.saturating_sub(1).min(pts.len() - 2);
                                        pts.drain(0..keep_from);
                                    }
                                }
                                self.history = pts;
                                self.history_error = None;
                            }
                            Err(e) => {
                                self.history.clear();
                                self.history_error = Some(e.clone());
                                self.push_toast(format!("Chart history error: {e}"), true);
                            }
                        }
                    }
                }
                UiMsg::WhalesLoaded(cid, wf) => {
                    if self.active_market.as_ref().is_some_and(|m| m.condition_id == cid) {
                        self.whale_flow = Some(wf);
                    }
                }
                UiMsg::NewsLoaded {
                    condition_id,
                    req_gen,
                    result,
                } => {
                    if req_gen == self.news_req_gen
                        && self.active_market.as_ref().is_some_and(|m| m.condition_id == condition_id)
                    {
                        self.news_loading = false;
                        match result {
                            Ok(feed) => {
                                self.news_error = None;
                                self.news_feed = Some(feed);
                            }
                            Err(e) => {
                                self.news_error = Some(e);
                            }
                        }
                    }
                }
                UiMsg::SiblingsLoaded(eslug, sib) => {
                    if self.active_market.as_ref().is_some_and(|m| m.event_slug == eslug) && sib.outcomes.len() >= 2 {
                        self.siblings = Some(sib);
                        self.trigger_translate_visible();
                    }
                }
                UiMsg::TranslationsReady => {}
                UiMsg::RulesUpdated => {
                    self.cached_rules = self.core.automation.list_rules();
                    self.cached_copy_targets = self.core.automation.list_copy_targets();
                    self.sync_pinned_feed_assets();
                }
                UiMsg::AlertsUpdated(list) => {
                    self.cached_alerts = list;
                    self.sync_pinned_feed_assets();
                }
                UiMsg::AlertTriggered(ev) => {
                    self.cached_alerts = self.core.alerts.list();
                    self.sync_pinned_feed_assets();
                    self.unread_alerts = self.unread_alerts.saturating_add(1);
                    let cond_txt = match ev.alert.condition {
                        AlertCondition::CrossAbove(th) => format!("≥ {:.1}¢", th * 100.0),
                        AlertCondition::CrossBelow(th) => format!("≤ {:.1}¢", th * 100.0),
                        AlertCondition::SpreadAbove(th) => format!("Spread ≥ {:.1}¢", th * 100.0),
                    };
                    let msg = format!(
                        "{}: {} ({}) {} · {:.1}¢",
                        tr(self.lang, "ALERT", "预警触发"),
                        ev.alert.market_question.chars().take(36).collect::<String>(),
                        ev.alert.outcome,
                        cond_txt,
                        ev.observed_value * 100.0,
                    );
                    self.alert_banner = Some(msg.clone());
                    self.push_toast(msg, false);
                    if ev.alert.sound_enabled {
                        use std::io::Write;
                        let _ = std::io::stdout().write_all(b"\x07");
                    }
                }
                UiMsg::AccountUpdated(acc) => {
                    if acc.mode == TradingMode::Paper && acc.live_orders_warning.is_none() {
                        self.pending_live_exit = None;
                    }
                    self.account = acc;
                    self.sync_pinned_feed_assets();
                }
                UiMsg::OrderFinished { asset_id } => {
                    end_order_in_flight(
                        &mut self.order_in_flight,
                        &mut self.order_in_flight_since,
                        &asset_id,
                    );
                }
                UiMsg::CtfFinished(res) => {
                    self.ctf_in_flight = false;
                    match res {
                        Ok(tx_res) => {
                            let summary = tx_res.summary.clone();
                            self.last_ctf_result = Some(tx_res);
                            self.pending_ctf = None;
                            self.push_toast(summary, false);
                        }
                        Err(err) => {
                            self.push_toast(err, true);
                        }
                    }
                }
                UiMsg::WalletCreated { wallet, account, deposit } => {
                    self.deposit_wallet_input = wallet.address.clone();
                    self.revealed_wallet = Some(wallet);
                    self.account = account;
                    if let Some(dep) = deposit {
                        self.deposit_info = Some(dep);
                    }
                    self.vault_pass_input.clear();
                    self.sync_pinned_feed_assets();
                }
                UiMsg::WalletRevealed(w) => {
                    self.revealed_wallet = Some(w);
                    self.reveal_pass_input.clear();
                }
                UiMsg::DepositLoaded(dep) => { self.deposit_info = Some(dep); }
                UiMsg::UpdateChecked { status, is_auto } => {
                    self.update_check_in_flight = false;
                    let hot_patch_to_apply = if status.hot_patch_available && self.auto_apply_hot_updates {
                        status.verified_hot_patch.clone()
                    } else {
                        None
                    };
                    let cold_ver_to_stage = if status.cold_update_available
                        && is_auto
                        && self.auto_stage_cold_updates
                        && !self.cold_update_in_flight
                    {
                        status.latest_version.clone().filter(|ver| {
                            self.staged_cold_update
                                .as_ref()
                                .is_none_or(|s| s.version != *ver)
                        })
                    } else {
                        None
                    };
                    self.update_status = Some(status);
                    if let Some(patch) = hot_patch_to_apply {
                        self.apply_verified_hot_patch(patch);
                    }
                    if cold_ver_to_stage.is_some() {
                        self.trigger_stage_cold_update(false);
                    }
                }
                UiMsg::ColdUpdateStaged { result, auto_relaunch } => {
                    self.cold_update_in_flight = false;
                    let l = self.lang;
                    match result {
                        Ok(staged) => {
                            let ver = staged.version.clone();
                            self.staged_cold_update = Some(staged);
                            if auto_relaunch {
                                self.try_relaunch_staged_update();
                            } else {
                                self.push_toast(
                                    format!(
                                        "{} v{ver} — {}",
                                        tr(l, "Cold update verified & staged", "冷更新已校验并预装"),
                                        tr(l, "click 'Restart' when ready", "准备好时点击“重启生效”即可"),
                                    ),
                                    false,
                                );
                            }
                        }
                        Err(e) => {
                            self.push_toast(e, true);
                        }
                    }
                }
                UiMsg::Toast { text, is_err } => { self.push_toast(text, is_err); }
            }
        }
        self.toasts.retain(|(_, _, t0)| t0.elapsed() < Duration::from_secs(4));
        prune_stale_in_flight(
            &mut self.order_in_flight,
            &mut self.order_in_flight_since,
            Instant::now(),
            Duration::from_secs(15),
        );

        if let Some(aid) = self.active_asset_id()
            && let Some((best_b, best_a, Some(mid))) = self.core.feed.get_top_of_book(aid)
        {
            let spread_ok = best_b.zip(best_a).is_none_or(|(b, a)| (a - b) <= 0.18);
            let now_ms = chrono::Utc::now().timestamp_millis();
            let live_tick_ms = self.live_tick_ms.clamp(50, 1000) as i64;
            let push_live = (0.001..=0.999).contains(&mid)
                && spread_ok
                && self.share_live_ticks.last().is_none_or(|&(lt, lp)| {
                    now_ms - lt >= live_tick_ms || (mid - lp).abs() > 1e-6
                });
            if push_live {
                self.share_live_ticks.push((now_ms, mid));
                let cutoff = now_ms - polydesk::oracle_feed::ORACLE_BUFFER_WINDOW_MS;
                while self.share_live_ticks.first().is_some_and(|&(t, _)| t < cutoff) {
                    self.share_live_ticks.remove(0);
                }
                if self.share_live_ticks.len() > 2000 {
                    let excess = self.share_live_ticks.len() - 2000;
                    self.share_live_ticks.drain(0..excess);
                }
            }
            if let Some(last) = self.history.last_mut() {
                let now_sec = now_ms / 1000;
                let is_short_tf = self
                    .active_market
                    .as_ref()
                    .and_then(extract_short_timeframe_info)
                    .is_some();
                let bucket_secs = if is_short_tf {
                    self.short_chart_bucket_secs.clamp(1, 10)
                } else {
                    60
                };
                if now_sec - last.t < bucket_secs {
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
                tx.send_silent(UiMsg::AccountUpdated(acc));
            });
        }

        if self.auto_check_updates
            && !self.update_check_in_flight
            && !self.github_repo_input.trim().is_empty()
            && self.last_update_check.elapsed() >= Duration::from_secs(1800)
        {
            self.trigger_check_update(true);
        }
    }

    fn handle_hotkeys(&mut self, ctx: &Context) {
        if ctx.egui_wants_keyboard_input() || self.modal.is_some() { return; }
        // Ctrl/Cmd/Alt combinations are host shortcuts, never trading shortcuts. Without
        // this a Cmd+S style save chord could fire a live sell.
        if ctx.input(|i| i.modifiers.command || i.modifiers.ctrl || i.modifiers.alt) {
            return;
        }
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

        if !space && !o && !l && !b && !s && !x && !g && !f && digit.is_none() {
            return;
        }

        let top = (space || b || s)
            .then(|| self.active_asset_id().and_then(|a| self.core.feed.get_top_of_book(a)))
            .flatten();

        if space {
            self.ladder_state.recenter_mid(top.and_then(|(_, _, m)| m));
        }
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
        let asset_busy = self
            .active_asset_id()
            .is_some_and(|aid| self.order_in_flight.contains(aid));
        if b && !asset_busy && let Some((best_bid, best_ask, _)) = top {
            if shift && let Some(ask) = best_ask {
                self.place_order("BUY", ask, OrderChoice::Fak);
            } else if let Some(bid) = best_bid {
                self.place_order("BUY", bid, self.order_choice);
            }
        }
        if s && !asset_busy && let Some((best_bid, best_ask, _)) = top {
            if shift && let Some(bid) = best_bid {
                self.place_order("SELL", bid, OrderChoice::Fak);
            } else if let Some(ask) = best_ask {
                self.place_order("SELL", ask, self.order_choice);
            }
        }
        if x {
            let core = self.core.clone();
            let tx = self.tx.clone();
            if shift {
                self.rt.spawn(async move {
                    match core.execution.cancel_all().await {
                        Ok(n) => tx.send(UiMsg::Toast {
                            text: format!("Cancelled {n} order(s)"),
                            is_err: false,
                        }),
                        Err(e) => tx.send(UiMsg::Toast {
                            text: format!("Cancel failed: {e}"),
                            is_err: true,
                        }),
                    }
                });
            } else if let Some(aid) = self.active_asset_id().map(|s| s.to_string()) {
                self.rt.spawn(async move {
                    if let Err(e) = core.execution.cancel_market(&aid).await {
                        tx.send(UiMsg::Toast {
                            text: format!("Cancel market failed: {e}"),
                            is_err: true,
                        });
                    }
                });
            }
        }
        if g && !asset_busy { self.trigger_green_up("close"); }
        if f && !asset_busy { self.trigger_flatten(); }
        if let Some(idx) = digit
            && let Some(&v) = self.stake_presets.get(idx)
        {
            self.stake_value = v;
        }
    }

    fn switch_active_outcome(&mut self, idx: usize, px: Option<f64>) {
        let tick = self.active_market.as_ref().map(|m| m.tick_size).unwrap_or(0.01);
        self.active_outcome_idx = idx;
        self.ladder_state.reset_for_market(tick);
        self.history.clear();
        self.share_live_ticks.clear();
        self.chart_cache.clear();
        self.oracle_chart_state.reset();
        self.history_error = None;
        if let Some(p) = px {
            self.quick_limit_cents = (p * 100.0).round().clamp(1.0, 99.0);
        }
        if let Some(aid) = self.active_asset_id().map(|s| s.to_string()) {
            self.trigger_load_history(&aid);
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
        let modal_open = self.modal.is_some();
        let telem = self.core.telemetry.snapshot();
        let active_aid = self.active_asset_id().map(|s| s.to_string());
        let active_top = active_aid.as_deref().and_then(|a| self.core.feed.get_top_of_book(a));
        let active_book = if self.right_tab == RightTab::Ladder {
            active_aid.as_deref().and_then(|a| self.core.feed.get_book_view(a, 60))
        } else {
            None
        };

        // 1. CARBON PRECISION 46px TOP BAR (brand emblem, segmented workspace switcher, borderless inline telemetry)
        let top_frame = Frame::NONE
            .fill(pal.bg_panel)
            .inner_margin(Margin::symmetric(14, 5));
        let open_order_count = open_orders_for(&self.account).len();

        let top_resp = egui::Panel::top("top_bar")
            .exact_size(46.0)
            .frame(top_frame)
            .show(ui, |ui| {
                if modal_open {
                    ui.disable();
                }
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;

                    if ghost_button(
                        ui,
                        pal,
                        RichText::new(if self.show_markets_rail { "◀" } else { "☰" })
                            .size(12.5)
                            .color(pal.text_secondary),
                    )
                    .on_hover_text(tr(l, "Toggle Markets", "展开/折叠市场栏"))
                    .clicked()
                    {
                        self.show_markets_rail = !self.show_markets_rail;
                    }

                    ThemePalette::draw_brand_mark(ui, pal, telem.ws_connected);
                    ui.label(
                        RichText::new("PolyDirect")
                            .font(ThemePalette::semibold(14.5))
                            .color(pal.text_primary),
                    );

                    // Soft borderless Mode Pill (PAPER / LIVE)
                    let is_live = self.account.mode == TradingMode::Live;
                    let mode_col = if is_live { pal.green } else { pal.amber };
                    let mode_bg = ThemePalette::tint(mode_col, if pal.light { 24 } else { 26 });
                    let mode_lbl = if is_live { tr(l, "● LIVE", "● 实盘") } else { tr(l, "● PAPER", "● 模拟盘") };
                    if ui
                        .add(
                            egui::Button::new(RichText::new(mode_lbl).font(ThemePalette::semibold(10.5)).color(mode_col))
                                .fill(mode_bg)
                                .stroke(Stroke::NONE)
                                .corner_radius(CornerRadius::same(99)),
                        )
                        .clicked()
                    {
                        self.modal = Some(ActiveModal::WalletAndDeposit);
                    }

                    // C1: Visible warning badge whenever live orders remain cached or warning active (even in Paper mode)
                    let live_ord_count = self.account.live_open_orders.len();
                    if live_ord_count > 0 || self.account.live_orders_warning.is_some() {
                        let warn_col = if self.account.live_orders_warning.is_some() || !is_live {
                            pal.red
                        } else {
                            pal.amber
                        };
                        let badge_txt = if live_ord_count > 0 {
                            format!("{} ({})", tr(l, "LIVE ORDERS", "实盘挂单"), live_ord_count)
                        } else {
                            tr(l, "LIVE WARNING", "实盘警告").to_string()
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(badge_txt)
                                        .font(ThemePalette::semibold(10.0))
                                        .color(ThemePalette::contrast_on(warn_col)),
                                )
                                .fill(warn_col)
                                .stroke(Stroke::NONE)
                                .corner_radius(CornerRadius::same(6)),
                            )
                            .on_hover_text(tr(
                                l,
                                "Live orders may still be active on Polymarket — click to manage",
                                "实盘挂单可能仍在 Polymarket 生效 — 点击管理",
                            ))
                            .clicked()
                        {
                            self.modal = Some(ActiveModal::WalletAndDeposit);
                        }
                    }

                    v_hairline(ui, pal, 16.0);

                    // Recessed Segmented Layout Switcher + Ghost 5m 4-Up shortcut
                    pal.pill_container().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if seg_item(ui, pal, self.layout_mode == LayoutMode::Single, tr(l, "Single", "单屏")).clicked() {
                                self.layout_mode = LayoutMode::Single;
                            }
                            let multi_lbl = format!("{} ({})", tr(l, "Multi", "分屏"), self.pinned_markets.len());
                            if seg_item(ui, pal, self.layout_mode == LayoutMode::Multi, &multi_lbl).clicked() {
                                self.layout_mode = LayoutMode::Multi;
                            }
                        });
                    });

                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(tr(l, "5m 4-Up", "5m 四联"))
                                    .font(ThemePalette::semibold(11.5))
                                    .color(pal.blue),
                            )
                            .fill(ThemePalette::tint(pal.blue, if pal.light { 16 } else { 22 }))
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(6)),
                        )
                        .on_hover_text(tr(l, "Pin 5m BTC / ETH / SOL / XRP into 2x2 grid", "一键铺开 5分钟 BTC/ETH/SOL/XRP 四联分屏"))
                        .clicked()
                    {
                        self.trigger_load_5m_4up();
                    }

                    if self.unread_alerts > 0
                        && ui
                            .add(
                                egui::Button::new(
                                    RichText::new(format!("{} ({})", tr(l, "Alerts", "预警"), self.unread_alerts))
                                        .font(ThemePalette::semibold(10.0))
                                        .color(ThemePalette::contrast_on(pal.amber)),
                                )
                                .fill(pal.amber)
                                .stroke(Stroke::NONE)
                                .corner_radius(CornerRadius::same(6)),
                            )
                            .clicked()
                    {
                        self.layout_mode = LayoutMode::Single;
                        self.deck_tab = DeckTab::Alerts;
                        self.unread_alerts = 0;
                    }

                    v_hairline(ui, pal, 16.0);

                    // Clean Inline Account Telemetry Strip (no extra bounding box)
                    let cash = if is_live { self.account.live_balance_usd.unwrap_or(0.0) } else { self.account.paper.cash_usd };
                    let mut pos_val = 0.0;
                    let mut upnl = 0.0;
                    for p in self.all_positions() {
                        let mk = self
                            .core
                            .feed
                            .get_top_of_book(&p.asset_id)
                            .and_then(|(b, _, m)| m.or(b))
                            .unwrap_or(p.avg_entry);
                        pos_val += p.shares * mk;
                        upnl += p.shares * (mk - p.avg_entry);
                    }
                    let portfolio = cash + pos_val;
                    let pnl = if is_live { upnl } else { self.account.paper.realized_pnl_usd + upnl };
                    let pnl_col = if pnl >= 0.0 { pal.green_text } else { pal.red_text };
                    let clob_ms = if telem.warm_ping_ms.p50 > 0.0 { telem.warm_ping_ms.p50 } else { telem.order_rtt_ms.last };
                    let clob_col = if clob_ms <= 0.0 {
                        pal.text_muted
                    } else if clob_ms < 60.0 {
                        pal.green_text
                    } else if clob_ms < 200.0 {
                        pal.amber
                    } else {
                        pal.text_secondary
                    };

                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 7.0;
                        ui.label(RichText::new(tr(l, "Equity", "总资产")).size(11.0).color(pal.text_muted));
                        ui.label(RichText::new(format_usd(portfolio, false)).monospace().strong().size(12.5).color(pal.text_primary));
                        v_hairline(ui, pal, 12.0);
                        ui.label(RichText::new(tr(l, "Cash", "余额")).size(11.0).color(pal.text_muted));
                        ui.label(RichText::new(format_usd(cash, false)).monospace().strong().size(12.5).color(pal.green_text));
                        v_hairline(ui, pal, 12.0);
                        ui.label(RichText::new(format_usd(pnl, true)).monospace().strong().size(12.0).color(pnl_col));
                        v_hairline(ui, pal, 12.0);
                        ui.label(RichText::new(format!("● {:.0}ms", clob_ms.min(999.0))).monospace().size(10.5).color(clob_col));
                    })
                    .response
                    .on_hover_text(format!(
                        "WS lag: {:.0} ms | CLOB RTT: {:.0} ms | Book apply: {:.0} µs | EIP-712 Sign: {:.0} µs | WS msgs: {}",
                        telem.ws_lag_ms.p50, clob_ms, telem.book_apply_us.p50, telem.sign_us.last, telem.ws_msgs_total
                    ));

                    // Right utility & action cluster (right-to-left)
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;

                        let cxl_fill = if open_order_count > 0 {
                            ThemePalette::tint(pal.red, if pal.light { 22 } else { 28 })
                        } else {
                            Color32::TRANSPARENT
                        };
                        let cxl_col = if open_order_count > 0 { pal.red_text } else { pal.text_muted };
                        let cxl_txt = if open_order_count > 0 {
                            format!("{} ({})", tr(l, "Cancel All", "全撤"), open_order_count)
                        } else {
                            tr(l, "Cancel All", "全撤").to_string()
                        };
                        if ui
                            .add(
                                egui::Button::new(RichText::new(cxl_txt).font(ThemePalette::semibold(11.5)).color(cxl_col))
                                    .fill(cxl_fill)
                                    .stroke(Stroke::NONE)
                                    .corner_radius(CornerRadius::same(6)),
                            )
                            .clicked()
                        {
                            let core = self.core.clone();
                            let tx = self.tx.clone();
                            self.rt.spawn(async move {
                                match core.execution.cancel_all().await {
                                    Ok(n) => tx.send(UiMsg::Toast {
                                        text: format!("Cancelled {n} order(s)"),
                                        is_err: false,
                                    }),
                                    Err(e) => tx.send(UiMsg::Toast {
                                        text: format!("Cancel All failed: {e}"),
                                        is_err: true,
                                    }),
                                }
                            });
                        }

                        // High-contrast Primary Deposit / Wallet CTA
                        if ui
                            .add(
                                egui::Button::new(RichText::new(tr(l, "+ Deposit / Wallet", "+ 充值 / 钱包")).font(ThemePalette::semibold(11.5)).color(pal.cta_fg))
                                    .fill(pal.cta_bg)
                                    .stroke(Stroke::NONE)
                                    .corner_radius(CornerRadius::same(7)),
                            )
                            .clicked()
                        {
                            self.modal = Some(ActiveModal::WalletAndDeposit);
                        }

                        if let Some(staged) = self.staged_cold_update.clone() {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(format!(
                                            "↻ {} v{}",
                                            tr(l, "Restart", "重启更新"),
                                            staged.version
                                        ))
                                        .font(ThemePalette::semibold(11.0))
                                        .color(pal.green_text),
                                    )
                                    .fill(ThemePalette::tint(pal.green, if pal.light { 24 } else { 32 }))
                                    .stroke(Stroke::NONE)
                                    .corner_radius(CornerRadius::same(6)),
                                )
                                .on_hover_text(tr(
                                    l,
                                    "Cold binary update verified & staged — click to restart cleanly",
                                    "冷更新二进制已校验就绪 — 点击安全重启并切换至新版",
                                ))
                                .clicked()
                            {
                                self.try_relaunch_staged_update();
                            }
                        } else if let Some(hot_patch) = self
                            .update_status
                            .as_ref()
                            .filter(|u| u.hot_patch_available)
                            .and_then(|u| u.verified_hot_patch.clone())
                            && ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(tr(l, "⚡ Hot Patch", "⚡ 一键热更新"))
                                            .font(ThemePalette::semibold(11.0))
                                            .color(pal.cyan),
                                    )
                                    .fill(ThemePalette::tint(pal.cyan, if pal.light { 20 } else { 28 }))
                                    .stroke(Stroke::NONE)
                                    .corner_radius(CornerRadius::same(6)),
                                )
                                .on_hover_text(tr(
                                    l,
                                    "Apply verified live patch immediately without restarting",
                                    "立即应用 SHA-256 校验热更新，无需重启客户端",
                                ))
                                .clicked()
                            {
                                self.apply_verified_hot_patch(hot_patch);
                            }

                        let upd_dot = self.update_status.as_ref().is_some_and(|u| u.update_available);
                        let settings_lbl = if self.staged_cold_update.is_some() || upd_dot {
                            tr(l, "Updates ↑", "更新 ↑")
                        } else {
                            tr(l, "Settings", "设置")
                        };
                        if ghost_button(
                            ui,
                            pal,
                            RichText::new(settings_lbl).size(11.5).color(
                                if self.staged_cold_update.is_some() || upd_dot {
                                    pal.green_text
                                } else {
                                    pal.text_secondary
                                },
                            ),
                        )
                        .on_hover_text(tr(l, "Settings, Auto-Updates & Keyboard Hotkeys", "设置、热/冷更新与快捷键"))
                        .clicked()
                        {
                            if self.staged_cold_update.is_some() || upd_dot {
                                self.settings_tab = SettingsTab::NetworkUpdates;
                            }
                            self.modal = Some(ActiveModal::SettingsAndUpdates);
                        }

                        let theme_lbl = if self.light_mode { tr(l, "Dark", "深色") } else { tr(l, "Light", "浅色") };
                        if ghost_button(
                            ui,
                            pal,
                            RichText::new(theme_lbl).size(11.5).color(pal.text_secondary),
                        )
                        .on_hover_text(tr(l, "Switch Light / Dark theme", "切换浅色 / 深色主题"))
                        .clicked()
                        {
                            self.light_mode = !self.light_mode;
                            apply_theme(&ctx, self.light_mode);
                            let mode_str = if self.light_mode { "light" } else { "dark" }.to_string();
                            let _ = self.core.execution.update_config_and_save(|cfg| cfg.theme = mode_str);
                        }

                        if ghost_button(
                            ui,
                            pal,
                            RichText::new(if l == Lang::En { "中文" } else { "EN" })
                                .size(11.5)
                                .color(pal.text_secondary),
                        )
                        .clicked()
                        {
                            self.lang = self.lang.toggle();
                            let c = self.lang.as_code().to_string();
                            let _ = self.core.execution.update_config_and_save(|cfg| cfg.language = c);
                            self.trigger_translate_visible();
                        }

                        if ghost_button(
                            ui,
                            pal,
                            RichText::new(self.odds_fmt.short_label())
                                .monospace()
                                .size(11.5)
                                .color(pal.text_secondary),
                        )
                        .on_hover_text(tr(l, "Odds format (O)", "切换赔率格式 (O)"))
                        .clicked()
                        {
                            self.odds_fmt = self.odds_fmt.cycle();
                            let c = self.odds_fmt.as_str_code().to_string();
                            let _ = self.core.execution.update_config_and_save(|cfg| cfg.odds_format = c);
                        }
                    });
                });
            })
            .response;

        ui.painter().line_segment(
            [
                egui::pos2(top_resp.rect.left(), top_resp.rect.bottom() - 0.5),
                egui::pos2(top_resp.rect.right(), top_resp.rect.bottom() - 0.5),
            ],
            Stroke::new(1.0, pal.border),
        );

        // Dismissible hot-patch advisory banner if the active verified hot patch includes an announcement
        if let Some(ann) = self
            .active_hot_patch
            .as_ref()
            .and_then(|p| p.announcement.as_ref())
            .filter(|a| self.dismissed_announcement_id.as_deref() != Some(a.id.as_str()))
            .cloned()
        {
            let accent = match ann.severity.trim().to_ascii_lowercase().as_str() {
                "critical" => pal.red,
                "warn" | "warning" => pal.amber,
                _ => pal.blue,
            };
            let ann_frame = Frame::NONE
                .fill(ThemePalette::tint(accent, if pal.light { 20 } else { 28 }))
                .inner_margin(Margin::symmetric(12, 5));
            egui::Panel::top("hot_announcement_banner")
                .frame(ann_frame)
                .show(ui, |ui| {
                    if modal_open {
                        ui.disable();
                    }
                    ui.horizontal_wrapped(|ui| {
                        let title = if l == Lang::Zh {
                            ann.title_zh
                                .as_deref()
                                .filter(|s| !s.trim().is_empty())
                                .unwrap_or(&ann.title)
                        } else {
                            &ann.title
                        };
                        let body = if l == Lang::Zh {
                            ann.message_zh
                                .as_deref()
                                .filter(|s| !s.trim().is_empty())
                                .unwrap_or(&ann.message)
                        } else {
                            &ann.message
                        };
                        ui.label(RichText::new(format!("⚡ {title}:")).strong().small().color(accent));
                        ui.label(RichText::new(body).small().color(pal.text_primary));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("×").clicked() {
                                self.dismissed_announcement_id = Some(ann.id.clone());
                            }
                            if let Some(ref url) = ann.url {
                                ui.hyperlink_to(tr(l, "Details ↗", "详情 ↗"), url);
                            }
                        });
                    });
                });
        }

        // C1: Explicit warning banner beneath top bar when cancel verification failed or live orders remain in Paper mode
        if self.account.live_orders_warning.is_some()
            || (self.account.mode == TradingMode::Paper && !self.account.live_open_orders.is_empty())
        {
            let warn_frame = Frame::NONE
                .fill(ThemePalette::tint(pal.red, if pal.light { 24 } else { 36 }))
                .inner_margin(Margin::symmetric(12, 6));
            egui::Panel::top("live_warning_banner")
                .frame(warn_frame)
                .show(ui, |ui| {
                    if modal_open {
                        ui.disable();
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(tr(
                                l,
                                "LIVE ORDERS MAY STILL BE ACTIVE ON POLYMARKET",
                                "警告：实盘挂单可能仍在 POLYMARKET 生效",
                            ))
                            .strong()
                            .size(12.0)
                            .color(pal.red_text),
                        );
                        if let Some(ref w) = self.account.live_orders_warning {
                            ui.label(RichText::new(w).small().color(pal.text_primary));
                        } else {
                            ui.label(
                                RichText::new(format!(
                                    "({} {} {})",
                                    tr(l, "cached:", "缓存："),
                                    self.account.live_open_orders.len(),
                                    tr(l, "open", "笔挂单")
                                ))
                                .small()
                                .color(pal.text_primary),
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if self.account.live_connected {
                                if ui
                                    .small_button(tr(
                                        l,
                                        "Force Lock Keys (orders stay on exchange)",
                                        "强制锁定私钥（挂单保留在交易所）",
                                    ))
                                    .clicked()
                                {
                                    self.revealed_wallet = None;
                                    self.trigger_force_lock_wallet();
                                }
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new(tr(l, "Retry Cancel All", "重试全撤实盘"))
                                                .strong()
                                                .small()
                                                .color(pal.red_text),
                                        )
                                        .stroke(Stroke::new(1.0, pal.red)),
                                    )
                                    .clicked()
                                {
                                    self.trigger_retry_cancel_all_live();
                                }
                            } else {
                                if ui.small_button(tr(l, "Open Wallet", "打开钱包")).clicked() {
                                    self.modal = Some(ActiveModal::WalletAndDeposit);
                                }
                                if self.account.live_open_orders.is_empty()
                                    && ui.small_button(tr(l, "Dismiss", "忽略")).clicked()
                                {
                                    self.core.execution.clear_live_warning_if_empty();
                                }
                            }
                        });
                    });
                });
        }

        // 2. REFINED LEFT MARKET RAIL (276px · recessed search pill · tonal filters · calm scannable rows)
        if self.show_markets_rail {
            let rail_frame = Frame::NONE
                .fill(pal.bg_panel)
                .inner_margin(Margin::ZERO);

            let rail_resp = egui::Panel::left("markets_panel")
                .exact_size(276.0)
                .resizable(false)
                .frame(rail_frame)
                .show(ui, |ui| {
                    if modal_open {
                        ui.disable();
                    }
                    let hdr_resp = Frame::NONE
                        .inner_margin(Margin::symmetric(10, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            let has_q = !self.search_query.is_empty();
                            Frame::NONE
                                .fill(pal.bg_sub)
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::symmetric(9, 5))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        let clear_w = if has_q { 22.0 } else { 0.0 };
                                        let resp = ui.add(
                                            TextEdit::singleline(&mut self.search_query)
                                                .frame(Frame::NONE)
                                                .hint_text(tr(l, "Search markets (EN / 中文)...", "搜索市场 (中/英文)..."))
                                                .desired_width((ui.available_width() - clear_w).max(120.0)),
                                        );
                                        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                            self.trigger_load_markets();
                                        }
                                        if has_q
                                            && ghost_button(ui, pal, RichText::new("×").size(12.0).color(pal.text_muted))
                                                .on_hover_text(tr(l, "Clear", "清空"))
                                                .clicked()
                                        {
                                            self.search_query.clear();
                                            self.trigger_load_markets();
                                        }
                                    });
                                });

                            ui.add_space(7.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::new(4.0, 4.0);
                                for (id, en, zh) in [
                                    ("all", "All", "全部"),
                                    ("5m", "5m", "5m"),
                                    ("15m", "15m", "15m"),
                                    ("watch", "★ Watch", "★ 自选"),
                                    ("crypto", "Crypto", "加密"),
                                    ("sports", "Sports", "体育"),
                                    ("politics", "Politics", "政治"),
                                    ("finance", "Finance", "金融"),
                                    ("tech", "Tech", "科技"),
                                ] {
                                    let active = self.category == id;
                                    let fill = if active {
                                        ThemePalette::tint(pal.blue, if pal.light { 24 } else { 32 })
                                    } else {
                                        Color32::TRANSPARENT
                                    };
                                    let col = if active { pal.blue } else { pal.text_muted };
                                    if ui
                                        .add(
                                            egui::Button::new(RichText::new(tr(l, en, zh)).size(11.0).color(col))
                                                .fill(fill)
                                                .stroke(Stroke::NONE)
                                                .corner_radius(CornerRadius::same(99)),
                                        )
                                        .clicked()
                                    {
                                        self.category = id.into();
                                        self.trigger_load_markets();
                                    }
                                }
                            });
                        })
                        .response;

                    ui.painter().line_segment(
                        [
                            egui::pos2(hdr_resp.rect.left(), hdr_resp.rect.bottom() - 0.5),
                            egui::pos2(hdr_resp.rect.right(), hdr_resp.rect.bottom() - 0.5),
                        ],
                        Stroke::new(1.0, pal.border),
                    );

                    let sib_reserve = if self.siblings.is_some() { 204.0 } else { 0.0 };
                    let mut clicked_market: Option<MarketSummary> = None;
                    let mut toggle_star: Option<String> = None;
                    let mut toggle_pin: Option<MarketSummary> = None;
                    let mut want_load_more = false;

                    let watch_indices: Vec<usize> = if self.category == "watch" {
                        self.markets
                            .iter()
                            .enumerate()
                            .filter_map(|(i, m)| self.watchlist_set.contains(&m.condition_id).then_some(i))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let visible_count = if self.category == "watch" {
                        watch_indices.len()
                    } else {
                        self.markets.len()
                    };
                    let has_load_more = !matches!(self.category.as_str(), "watch" | "5m" | "15m")
                        && self.search_query.trim().is_empty()
                        && !self.markets.is_empty();
                    let total_rows = visible_count + usize::from(has_load_more);
                    const ROW_H: f32 = 68.0;

                    let scroll_h = (ui.available_height() - sib_reserve).max(160.0);
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ScrollArea::vertical()
                            .id_salt("mkt_scroll")
                            .max_height(scroll_h)
                            .auto_shrink([false, false])
                            .show_rows(ui, ROW_H, total_rows, |ui, row_range| {
                                for row_idx in row_range {
                                    if row_idx == visible_count {
                                        ui.allocate_ui_with_layout(
                                            Vec2::new(ui.available_width(), ROW_H),
                                            egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                                            |ui| {
                                                let lbl = if self.loading_more_markets {
                                                    tr(l, "Loading more markets...", "正在加载更多市场...")
                                                } else {
                                                    tr(l, "Load More Markets (+100)", "加载更多市场 (+100)")
                                                };
                                                if ui
                                                    .add_sized(
                                                        Vec2::new((ui.available_width() - 24.0).max(100.0), 30.0),
                                                        egui::Button::new(RichText::new(lbl).small().color(pal.text_secondary))
                                                            .fill(pal.bg_sub)
                                                            .stroke(Stroke::NONE)
                                                            .corner_radius(CornerRadius::same(6)),
                                                    )
                                                    .clicked()
                                                    && !self.loading_more_markets
                                                    && !modal_open
                                                {
                                                    want_load_more = true;
                                                }
                                            },
                                        );
                                        continue;
                                    }

                                    let m_idx = if self.category == "watch" {
                                        watch_indices[row_idx]
                                    } else {
                                        row_idx
                                    };
                                    let m = &self.markets[m_idx];
                                    let sel = self
                                        .active_market
                                        .as_ref()
                                        .is_some_and(|a| a.condition_id == m.condition_id);
                                    let starred = self.watchlist_set.contains(&m.condition_id);
                                    let display_q = self.display_text(&m.question);

                                    let sense = if modal_open {
                                        egui::Sense::hover()
                                    } else {
                                        egui::Sense::click()
                                    };
                                    let (row_rect, row_resp) = ui.allocate_exact_size(
                                        Vec2::new(ui.available_width(), ROW_H),
                                        sense,
                                    );
                                    let star_rect = egui::Rect::from_min_size(
                                        egui::pos2(row_rect.right() - 26.0, row_rect.top() + 8.0),
                                        Vec2::new(18.0, 18.0),
                                    );
                                    let star_resp = ui.interact(
                                        star_rect,
                                        ui.make_persistent_id(("mkt_star", &m.condition_id)),
                                        sense,
                                    );
                                    let pin_rect = egui::Rect::from_min_size(
                                        egui::pos2(row_rect.right() - 56.0, row_rect.top() + 8.5),
                                        Vec2::new(28.0, 17.0),
                                    );
                                    let pin_resp = ui.interact(
                                        pin_rect,
                                        ui.make_persistent_id(("mkt_pin", &m.condition_id)),
                                        sense,
                                    );
                                    let is_pinned = self
                                        .pinned_markets
                                        .iter()
                                        .any(|pm| pm.condition_id == m.condition_id);

                                    let any_hovered = !modal_open
                                        && (row_resp.hovered() || star_resp.hovered() || pin_resp.hovered());
                                    let row_hovered = !modal_open
                                        && row_resp.hovered()
                                        && !star_resp.hovered()
                                        && !pin_resp.hovered();
                                    if any_hovered {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }

                                    let painter = ui.painter();
                                    if sel {
                                        painter.rect_filled(row_rect, 0.0, pal.row_selected_tint);
                                        painter.rect_filled(
                                            egui::Rect::from_min_max(
                                                row_rect.left_top(),
                                                egui::pos2(row_rect.left() + 2.5, row_rect.bottom()),
                                            ),
                                            0.0,
                                            pal.blue,
                                        );
                                    } else if row_hovered {
                                        painter.rect_filled(row_rect, 0.0, pal.row_hover_tint);
                                    }

                                    painter.line_segment(
                                        [
                                            egui::pos2(row_rect.left() + 12.0, row_rect.bottom() - 0.5),
                                            egui::pos2(row_rect.right() - 12.0, row_rect.bottom() - 0.5),
                                        ],
                                        Stroke::new(1.0, ThemePalette::tint(pal.border, 150)),
                                    );

                                    let show_actions = starred || is_pinned || any_hovered;
                                    if starred || any_hovered {
                                        let star_col = if starred {
                                            pal.amber
                                        } else if !modal_open && star_resp.hovered() {
                                            pal.text_secondary
                                        } else {
                                            pal.text_muted
                                        };
                                        painter.text(
                                            star_rect.center(),
                                            egui::Align2::CENTER_CENTER,
                                            "★",
                                            egui::FontId::proportional(11.5),
                                            star_col,
                                        );
                                    }

                                    if is_pinned || any_hovered {
                                        let pin_bg = if is_pinned {
                                            ThemePalette::tint(pal.blue, if pal.light { 24 } else { 36 })
                                        } else {
                                            pal.bg_chip
                                        };
                                        let pin_col = if is_pinned {
                                            pal.blue
                                        } else if !modal_open && pin_resp.hovered() {
                                            pal.text_primary
                                        } else {
                                            pal.text_muted
                                        };
                                        painter.rect_filled(pin_rect, CornerRadius::same(4), pin_bg);
                                        painter.text(
                                            pin_rect.center(),
                                            egui::Align2::CENTER_CENTER,
                                            if is_pinned { "PIN" } else { "+G" },
                                            egui::FontId::monospace(9.0),
                                            pin_col,
                                        );
                                    }

                                    // Clean left-aligned Market Title (max 2 lines, ellipsis)
                                    let right_reserve = if show_actions { 64.0 } else { 14.0 };
                                    let title_w = (row_rect.width() - 12.0 - right_reserve).max(60.0);
                                    let mut job = egui::text::LayoutJob::single_section(
                                        display_q.clone(),
                                        egui::TextFormat {
                                            font_id: ThemePalette::semibold(12.0),
                                            color: pal.text_primary,
                                            ..Default::default()
                                        },
                                    );
                                    job.wrap = egui::text::TextWrapping {
                                        max_width: title_w,
                                        max_rows: 2,
                                        overflow_character: Some('…'),
                                        ..Default::default()
                                    };
                                    let galley = painter.layout_job(job);
                                    painter.galley(
                                        egui::pos2(row_rect.left() + 12.0, row_rect.top() + 8.0),
                                        galley,
                                        pal.text_primary,
                                    );

                                    // Calm 2.5px probability bar at bottom + tabular odds row
                                    let yp = m.outcome_prices.first().copied().or(m.last_trade_price);
                                    let np = m.outcome_prices.get(1).copied().or_else(|| yp.map(|y| 1.0 - y));
                                    let bar_y = row_rect.bottom() - 8.5;
                                    let bar_r = egui::Rect::from_min_max(
                                        egui::pos2(row_rect.left() + 12.0, bar_y),
                                        egui::pos2(row_rect.right() - 12.0, bar_y + 2.5),
                                    );
                                    let frac = yp.unwrap_or(0.5).clamp(0.03, 0.97) as f32;
                                    painter.rect_filled(bar_r, CornerRadius::same(2), pal.bg_chip);
                                    painter.rect_filled(
                                        egui::Rect::from_min_max(
                                            bar_r.left_top(),
                                            egui::pos2(bar_r.left() + bar_r.width() * frac, bar_r.bottom()),
                                        ),
                                        CornerRadius::same(2),
                                        if sel { pal.blue } else { pal.green },
                                    );

                                    // Metadata row above bar: odds left, volume right
                                    let y_txt = format_odds(yp, self.odds_fmt, m.tick_size);
                                    let n_txt = format_odds(np, self.odds_fmt, m.tick_size);
                                    let meta_y = row_rect.bottom() - 18.0;
                                    let px_font = egui::FontId::monospace(10.5);
                                    let mut x = row_rect.left() + 12.0;
                                    let g_yes = painter.layout_no_wrap(y_txt, px_font.clone(), pal.green_text);
                                    let yes_w = g_yes.size().x;
                                    painter.galley(egui::pos2(x, meta_y - g_yes.size().y * 0.5), g_yes, pal.green_text);
                                    x += yes_w + 4.0;
                                    let g_slash = painter.layout_no_wrap("/".into(), px_font.clone(), pal.text_muted);
                                    let slash_w = g_slash.size().x;
                                    painter.galley(egui::pos2(x, meta_y - g_slash.size().y * 0.5), g_slash, pal.text_muted);
                                    x += slash_w + 4.0;
                                    let g_no = painter.layout_no_wrap(n_txt, px_font, pal.text_secondary);
                                    painter.galley(egui::pos2(x, meta_y - g_no.size().y * 0.5), g_no, pal.text_secondary);

                                    let metric_txt = if m.volume_24hr > 0.0 {
                                        format!("{} Vol", format_usd(m.volume_24hr, false))
                                    } else if m.liquidity > 0.0 {
                                        format!("{} Liq", format_usd(m.liquidity, false))
                                    } else {
                                        tr(l, "Live", "新盘").to_string()
                                    };
                                    painter.text(
                                        egui::pos2(row_rect.right() - 12.0, meta_y),
                                        egui::Align2::RIGHT_CENTER,
                                        metric_txt,
                                        egui::FontId::monospace(10.0),
                                        pal.text_muted,
                                    );

                                    if self.lang == Lang::Zh && display_q != m.question {
                                        row_resp.clone().on_hover_text(format!("{display_q}\n{}", m.question));
                                    }

                                    if !modal_open {
                                        if star_resp.clicked() {
                                            toggle_star = Some(m.condition_id.clone());
                                        } else if pin_resp.clicked() {
                                            toggle_pin = Some(m.clone());
                                        } else if row_resp.clicked() && !star_resp.hovered() && !pin_resp.hovered() {
                                            clicked_market = Some(m.clone());
                                        }
                                    }
                                }
                            });
                    });

                    if want_load_more {
                        self.markets_offset += 100;
                        self.loading_more_markets = true;
                        self.trigger_load_markets_page(self.markets_offset, true);
                    }

                    if let Some(cid) = toggle_star {
                        if self.watchlist_set.contains(&cid) {
                            self.watchlist_set.remove(&cid);
                        } else {
                            self.watchlist_set.insert(cid.clone());
                        }
                        let _ = self.core.execution.update_config_and_save(|c| {
                            if let Some(pos) = c.watchlist.iter().position(|x| x == &cid) {
                                c.watchlist.remove(pos);
                            } else {
                                c.watchlist.push(cid);
                            }
                        });
                    }
                    if let Some(m) = toggle_pin {
                        if self.pinned_markets.iter().any(|pm| pm.condition_id == m.condition_id) {
                            self.unpin_market(&m.condition_id);
                        } else {
                            self.pin_market_to_grid(m);
                        }
                    }
                    if let Some(m) = clicked_market {
                        self.select_market(m, 0);
                    }

                    let mut clicked_sibling: Option<MarketSummary> = None;
                    let mut hedge_outs: Option<Vec<_>> = None;
                    if let Some(ref sib) = self.siblings {
                        let preview_outs: Vec<_> = sib.outcomes.iter().take(8).cloned().collect();
                        let hedge_est_usd = plan_sibling_hedge_legs(
                            &preview_outs,
                            self.stake_value,
                            self.stake_in_usd,
                        )
                        .ok()
                        .map(|legs| legs.iter().map(|(_, p, s)| p * s).sum::<f64>());

                        Frame::NONE.inner_margin(Margin::same(10)).show(ui, |ui| {
                            pal.surface_card().show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(tr(l, "Siblings", "关联子市场")).strong().small().color(pal.text_secondary));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        let ec = if sib.dutch_book_edge_bps > 0.0 { pal.green } else { pal.text_muted };
                                        ui.label(RichText::new(format!("Σ{:.1}¢ ({:+.0}bp)", sib.sum_yes_asks * 100.0, sib.dutch_book_edge_bps)).monospace().small().color(ec));
                                    });
                                });
                                ScrollArea::vertical().id_salt("sib_scroll").max_height(88.0).show(ui, |ui| {
                                    for o in &preview_outs {
                                        let sib_lbl = display_text_with(l, &self.core.translator, &o.group_title);
                                        ui.horizontal(|ui| {
                                            if ui.selectable_label(self.active_market.as_ref().is_some_and(|m| m.condition_id == o.condition_id), &sib_lbl).on_hover_text(&o.group_title).clicked() && !modal_open {
                                                clicked_sibling = Some(MarketSummary {
                                                    id: o.market_id.clone(), question: o.question.clone(), question_zh: None, slug: sib.event_slug.clone(),
                                                    event_slug: sib.event_slug.clone(), event_title: sib.event_title.clone(),
                                                    condition_id: o.condition_id.clone(), outcomes: vec!["Yes".into(), "No".into()], outcomes_zh: Vec::new(),
                                                    token_ids: vec![o.yes_token_id.clone(), o.no_token_id.clone()],
                                                    outcome_prices: vec![o.yes_price, o.no_price], tick_size: o.tick_size,
                                                    min_order_size: 5.0, volume_24hr: 0.0, liquidity: o.liquidity,
                                                    best_bid: o.best_bid, best_ask: o.best_ask, last_trade_price: Some(o.yes_price),
                                                    one_day_change: None, neg_risk: sib.neg_risk, fees_enabled: true,
                                                    accepting_orders: true, event_start_time: None, end_date: None, image: None, category: "other".into(),
                                                });
                                            }
                                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                ui.label(RichText::new(format_odds(o.best_ask.or(Some(o.yes_price)), self.odds_fmt, o.tick_size)).monospace().small().color(pal.green));
                                            });
                                        });
                                    }
                                });
                                let hedge_btn_lbl = match hedge_est_usd {
                                    Some(cost) => format!(
                                        "{} ({})",
                                        tr(l, "Auto-Hedge Siblings", "一键对冲子市场"),
                                        format_usd(cost, false)
                                    ),
                                    None => tr(l, "Auto-Hedge Siblings", "一键对冲子市场").to_string(),
                                };
                                let hedge_tip = if self.stake_in_usd {
                                    format!(
                                        "{}: {}",
                                        tr(l, "Total notional capped at", "总投入封顶"),
                                        format_usd(self.stake_value, false)
                                    )
                                } else {
                                    format!(
                                        "{:.2} {} / {}",
                                        self.stake_value,
                                        tr(l, "shares", "份"),
                                        tr(l, "leg", "腿")
                                    )
                                };
                                if ui
                                    .button(RichText::new(hedge_btn_lbl).small().color(pal.amber))
                                    .on_hover_text(hedge_tip)
                                    .clicked()
                                    && !modal_open
                                {
                                    hedge_outs = Some(preview_outs);
                                }
                            });
                        });
                    }
                    if let Some(syn) = clicked_sibling {
                        self.select_market(syn, 0);
                    }
                    if let Some(outs) = hedge_outs {
                        let core = self.core.clone();
                        let tx = self.tx.clone();
                        let stake_val = self.stake_value;
                        let stake_in_usd = self.stake_in_usd;
                        self.rt.spawn(async move {
                            match core.execution.execute_sibling_hedge(&outs, stake_val, stake_in_usd).await {
                                Ok(rep) => {
                                    let is_err = rep.failed > 0;
                                    let text = if rep.failed > 0 {
                                        format!("Auto-Hedge: {} placed, {} failed", rep.placed.len(), rep.failed)
                                    } else {
                                        format!("Auto-Hedge fired {} legs", rep.placed.len())
                                    };
                                    tx.send(UiMsg::Toast { text, is_err });
                                }
                                Err(e) => { tx.send(UiMsg::Toast { text: e.to_string(), is_err: true }); }
                            }
                        });
                    }
                })
                .response;

            ui.painter().line_segment(
                [
                    egui::pos2(rail_resp.rect.right() - 0.5, rail_resp.rect.top()),
                    egui::pos2(rail_resp.rect.right() - 0.5, rail_resp.rect.bottom()),
                ],
                Stroke::new(1.0, pal.border),
            );
        }

        // 3. RIGHT EXECUTION COLUMN (368px — Underline Tabs: Buy/Sell | Ladder | Pro)
        if self.layout_mode == LayoutMode::Single {
        let right_frame = Frame::NONE
            .fill(pal.bg_panel)
            .inner_margin(Margin::same(12));

        let right_resp = egui::Panel::right("exec_panel")
            .exact_size(368.0)
            .resizable(false)
            .frame(right_frame)
            .show(ui, |ui| {
                if modal_open {
                    ui.disable();
                }
                ui.set_max_width(344.0);
                // Clean underline tabs
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if underline_tab(ui, pal, self.right_tab == RightTab::QuickTrade, tr(l, "Buy / Sell", "快捷买卖")).clicked() {
                        self.right_tab = RightTab::QuickTrade;
                    }
                    if underline_tab(ui, pal, self.right_tab == RightTab::Ladder, tr(l, "Ladder", "盘口")).clicked() {
                        self.right_tab = RightTab::Ladder;
                    }
                    if underline_tab(ui, pal, self.right_tab == RightTab::ProOrders, tr(l, "Pro", "条件单")).clicked() {
                        self.right_tab = RightTab::ProOrders;
                    }
                    if self.right_tab == RightTab::Ladder {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if seg_item(ui, pal, self.ladder_state.auto_center, tr(l, "Auto", "跟踪")).clicked() {
                                self.ladder_state.auto_center = !self.ladder_state.auto_center;
                            }
                            if ghost_button(ui, pal, RichText::new(tr(l, "Center", "居中")).small().color(pal.text_secondary)).clicked() {
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
                let (sep_r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), egui::Sense::hover());
                ui.painter().line_segment(
                    [egui::pos2(sep_r.left(), sep_r.center().y), egui::pos2(sep_r.right(), sep_r.center().y)],
                    Stroke::new(1.0, pal.border),
                );
                ui.add_space(8.0);

                match self.right_tab {
                    RightTab::QuickTrade => {
                        ScrollArea::vertical().id_salt("quick_trade_scroll").show(ui, |ui| {
                            self.draw_quick_trade_panel(ui, active_top);
                        });
                    }
                    RightTab::Ladder => {
                        // Fixed-height Controls Card (ZERO reflow or height jitter on ticks)
                        pal.surface_card().show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            pal.pill_container().show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    for (ch, lbl, tip) in [
                                        (OrderChoice::Gtc, "GTC", "Good-Til-Cancelled"),
                                        (OrderChoice::PostOnly, "Post", "Post-Only Maker"),
                                        (OrderChoice::Fak, "FAK", "Fill-And-Kill"),
                                        (OrderChoice::Fok, "FOK", "Fill-Or-Kill"),
                                        (OrderChoice::TimedFok, "tFOK", "Timed Fill-Or-Kill"),
                                        (OrderChoice::Sweep, "Sweep", "Depth Sweep"),
                                    ] {
                                        if seg_item(ui, pal, self.order_choice == ch, lbl)
                                            .on_hover_text(tip)
                                            .clicked()
                                        {
                                            self.order_choice = ch;
                                        }
                                    }
                                    if self.order_choice == OrderChoice::TimedFok {
                                        ui.add(egui::DragValue::new(&mut self.timed_fok_ms).range(150..=60000).suffix("ms"));
                                    }
                                });
                            });

                            ui.add_space(6.0);

                            // Stake presets (using cached stake_presets)
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                if ui
                                    .add(
                                        egui::Button::new(RichText::new(if self.stake_in_usd { "$" } else { "SH" }).monospace().small())
                                            .fill(pal.bg_sub)
                                            .stroke(Stroke::NONE)
                                            .corner_radius(CornerRadius::same(6)),
                                    )
                                    .clicked()
                                {
                                    self.stake_in_usd = !self.stake_in_usd;
                                }
                                ui.add(egui::DragValue::new(&mut self.stake_value).range(1.0..=100_000.0).speed(1.0));
                                for &v in &self.stake_presets {
                                    let txt = if self.stake_in_usd { format!("${v:.0}") } else { format!("{v:.0}") };
                                    if seg_item(ui, pal, (self.stake_value - v).abs() < 0.01, &txt).clicked() {
                                        self.stake_value = v;
                                    }
                                }
                            });

                            ui.add_space(6.0);

                            // Quick Actions (tonal borderless buttons)
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 5.0;
                                if ui.add(
                                    egui::Button::new(RichText::new(tr(l, "Buy Bid", "买一")).strong().size(11.5).color(pal.green_text))
                                        .fill(ThemePalette::tint(pal.green, if pal.light { 20 } else { 26 }))
                                        .stroke(Stroke::NONE)
                                        .corner_radius(CornerRadius::same(6)),
                                ).clicked()
                                    && let Some(p) = active_book.as_ref().and_then(|b| b.best_bid.or(b.mid))
                                {
                                    self.place_order("BUY", p, self.order_choice);
                                }
                                if ui.add(
                                    egui::Button::new(RichText::new(tr(l, "Sell Ask", "卖一")).strong().size(11.5).color(pal.red_text))
                                        .fill(ThemePalette::tint(pal.red, if pal.light { 20 } else { 26 }))
                                        .stroke(Stroke::NONE)
                                        .corner_radius(CornerRadius::same(6)),
                                ).clicked()
                                    && let Some(p) = active_book.as_ref().and_then(|b| b.best_ask.or(b.mid))
                                {
                                    self.place_order("SELL", p, self.order_choice);
                                }
                                if ui.add(
                                    egui::Button::new(RichText::new(tr(l, "Green-Up", "锁盈")).size(11.5).color(pal.amber))
                                        .fill(pal.bg_sub)
                                        .stroke(Stroke::NONE)
                                        .corner_radius(CornerRadius::same(6)),
                                ).clicked() {
                                    self.trigger_green_up("close");
                                }
                                if ui.add(
                                    egui::Button::new(RichText::new(tr(l, "Flat", "平仓")).size(11.5).color(pal.text_secondary))
                                        .fill(pal.bg_sub)
                                        .stroke(Stroke::NONE)
                                        .corner_radius(CornerRadius::same(6)),
                                ).clicked() {
                                    self.trigger_flatten();
                                }
                                if ui.add(
                                    egui::Button::new(RichText::new(tr(l, "Cxl", "撤单")).size(11.5).color(pal.text_secondary))
                                        .fill(pal.bg_sub)
                                        .stroke(Stroke::NONE)
                                        .corner_radius(CornerRadius::same(6)),
                                ).clicked()
                                    && let Some(ref aid) = active_aid
                                {
                                    let core = self.core.clone();
                                    let aid_c = aid.clone();
                                    self.rt.spawn(async move { let _ = core.execution.cancel_market(&aid_c).await; });
                                }
                            });

                            ui.add_space(6.0);

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

                        let open_orders = open_orders_for(&self.account);
                        let active_pos = active_position_for(&self.account, active_aid.as_deref());
                        let mode_rules: Vec<AutomationRule> = self
                            .cached_rules
                            .iter()
                            .filter(|r| r.mode == self.account.mode)
                            .cloned()
                            .collect();
                        let ladder_act = draw_price_ladder(
                            ui,
                            l,
                            pal,
                            &mut self.ladder_state,
                            self.odds_fmt,
                            active_aid.as_deref(),
                            active_book.as_ref(),
                            open_orders,
                            active_pos,
                            &mode_rules,
                            !modal_open,
                        );
                        if !modal_open && let Some(act) = ladder_act {
                            match act {
                                LadderAction::PlaceOrder { side, price, post_only_override } => {
                                    let choice = if post_only_override { OrderChoice::PostOnly } else { self.order_choice };
                                    // The ladder is a one-click surface, but the global
                                    // "confirm before order" setting still wins — otherwise a
                                    // user who enabled it silently gets unconfirmed live fills.
                                    if confirm_required(self.confirm_ladder_orders, self.confirm_before_order)
                                        && let Some(m) = self.active_market.clone()
                                    {
                                        let tick_ticks = ((m.tick_size * 10_000.0).round() as usize).max(1);
                                        let px = align_to_tick(price, tick_ticks);
                                        let sh = self.compute_shares(px);
                                        let usd_cap = (choice == OrderChoice::Sweep
                                            && side.eq_ignore_ascii_case("BUY")
                                            && self.stake_in_usd)
                                            .then_some(self.stake_value);
                                        let out_idx = self.active_outcome_idx;
                                        self.submit_order_with_confirm_for_market(&m, out_idx, side, px, sh, choice, usd_cap, true);
                                    } else {
                                        self.place_order_immediate(side, price, choice);
                                    }
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
            })
            .response;

        ui.painter().line_segment(
            [
                egui::pos2(right_resp.rect.left() + 0.5, right_resp.rect.top()),
                egui::pos2(right_resp.rect.left() + 0.5, right_resp.rect.bottom()),
            ],
            Stroke::new(1.0, pal.border),
        );
        }

        // 4. CENTER STAGE (Market Title + Oracle Strip + Chart + Multi-Grid + Alerts/Deck)
        let center_frame = Frame::NONE
            .fill(pal.bg_canvas)
            .inner_margin(Margin::same(12));
        let mut need_1hz_repaint = false;

        egui::CentralPanel::default().frame(center_frame).show(ui, |ui| {
            if modal_open {
                ui.disable();
            }

            if let Some(banner_txt) = self.alert_banner.clone() {
                Frame::NONE
                    .fill(ThemePalette::tint(pal.amber, if pal.light { 22 } else { 26 }))
                    .stroke(Stroke::new(1.0, pal.amber))
                    .corner_radius(CornerRadius::same(4))
                    .inner_margin(Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&banner_txt).strong().small().color(pal.amber));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("×").clicked() {
                                    self.alert_banner = None;
                                }
                                if ui.small_button(tr(l, "View Alerts", "查看预警")).clicked() {
                                    self.layout_mode = LayoutMode::Single;
                                    self.deck_tab = DeckTab::Alerts;
                                    self.unread_alerts = 0;
                                    self.alert_banner = None;
                                }
                            });
                        });
                    });
                ui.add_space(6.0);
            }

            if self.layout_mode == LayoutMode::Multi {
                self.draw_multi_grid_stage(ui, &mut need_1hz_repaint);
                return;
            }

            let mut outcome_switch: Option<(usize, Option<f64>)> = None;
            let active_m_opt = self.active_market.clone();
            let stf = active_m_opt.as_ref().and_then(extract_short_timeframe_info);
            let hero_odds = active_m_opt
                .as_ref()
                .map(|m| {
                    let live_px = active_top
                        .and_then(|(_, a, mid)| mid.or(a))
                        .or_else(|| m.outcome_prices.get(self.active_outcome_idx).copied())
                        .unwrap_or(0.5);
                    format_odds(Some(live_px), self.odds_fmt, m.tick_size)
                })
                .unwrap_or_default();
            if let Some(ref m) = active_m_opt {
                if stf.is_some() {
                    self.maybe_fetch_open_price(m);
                }
                let title_disp = display_text_with(l, &self.core.translator, &m.question);
                let title_tip = if self.lang == Lang::Zh && title_disp != m.question {
                    format!("{title_disp}\n{}", m.question)
                } else {
                    m.question.clone()
                };

                // Band 1: Pure left-to-right Market Title Bar + View / Timeframe Switchers
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let total_w = ui.available_width();
                    let badge_w = if let Some(ref info) = stf {
                        let resp = Frame::NONE
                            .fill(ThemePalette::tint(pal.blue, if pal.light { 22 } else { 28 }))
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(99))
                            .inner_margin(Margin::symmetric(8, 3))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!("{}M LIVE", info.interval_mins))
                                        .font(ThemePalette::semibold(10.0))
                                        .color(pal.blue),
                                );
                            })
                            .response;
                        resp.rect.width() + 8.0
                    } else {
                        0.0
                    };

                    // Pre-calculate compact right controls width so title truncates cleanly in pure LTR
                    let controls_w = if stf.is_some() {
                        if self.short_chart_view != ShortChartView::Shares { 378.0 } else { 244.0 }
                    } else {
                        136.0
                    };
                    let title_w = (total_w - badge_w - controls_w).max(90.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(title_w, 26.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&title_disp)
                                        .font(ThemePalette::semibold(16.0))
                                        .color(pal.text_primary),
                                )
                                .truncate(),
                            )
                            .on_hover_text(title_tip);
                        },
                    );

                    if stf.is_some() && self.short_chart_view != ShortChartView::Shares {
                        pal.pill_container().show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                for (secs, lbl) in [(60_i64, "1M"), (180_i64, "3M"), (300_i64, "5M")] {
                                    if seg_item(ui, pal, self.short_chart_window_secs == secs, lbl).clicked() {
                                        self.short_chart_window_secs = secs;
                                        let _ = self
                                            .core
                                            .execution
                                            .update_config_and_save(|c| c.short_chart_window_secs = secs);
                                    }
                                }
                            });
                        });
                    }

                    pal.pill_container().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if let Some(ref info) = stf {
                                let coin_up = info.coin.to_ascii_uppercase();
                                let coin_lbl = if l == Lang::Zh {
                                    format!("{coin_up} 币价")
                                } else {
                                    coin_up
                                };
                                for (view, lbl) in [
                                    (ShortChartView::Both, tr(l, "Both", "双图").to_string()),
                                    (ShortChartView::Overlay, tr(l, "Overlay", "叠加").to_string()),
                                    (ShortChartView::Spot, coin_lbl),
                                    (ShortChartView::Shares, tr(l, "Shares", "份额").to_string()),
                                ] {
                                    if seg_item(ui, pal, self.short_chart_view == view, &lbl).clicked() {
                                        self.short_chart_view = view;
                                    }
                                }
                            } else {
                                if seg_item(ui, pal, self.chart_mode == ChartMode::Candles, tr(l, "Candles", "K线")).clicked() {
                                    self.chart_mode = ChartMode::Candles;
                                }
                                if seg_item(ui, pal, self.chart_mode == ChartMode::Line, tr(l, "Line", "折线")).clicked() {
                                    self.chart_mode = ChartMode::Line;
                                }
                            }
                        });
                    });
                });

                ui.add_space(7.0);

                let live_px = active_top
                    .and_then(|(_, a, mid)| mid.or(a))
                    .or_else(|| m.outcome_prices.get(self.active_outcome_idx).copied())
                    .unwrap_or(0.5);
                let first_hist = self.history.first().map(|p| p.p).unwrap_or(live_px);
                let delta_pts = (live_px - first_hist) * 100.0;
                let beat_opt = self.crypto_open_prices.get(&m.condition_id).copied();
                let oracle_snap = stf
                    .as_ref()
                    .and_then(|i| self.core.oracle.snapshot(&i.chainlink_symbol));

                // Band 2: Unified Hero Telemetry & Outcome Card
                let draw_outcome_pair = |ui: &mut egui::Ui, outcome_switch: &mut Option<(usize, Option<f64>)>| {
                    let mut out_probs: [Option<f64>; 2] = [None, None];
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for (idx, name) in m.outcomes.iter().take(2).enumerate() {
                                let tid = m.token_ids.get(idx).map(|s| s.as_str());
                                let px = tid
                                    .and_then(|t| self.core.feed.get_top_of_book(t))
                                    .and_then(|(_, a, mid)| mid.or(a))
                                    .or_else(|| m.outcome_prices.get(idx).copied());
                                out_probs[idx] = px;
                                let col = if idx == 0 { pal.green } else { pal.red };
                                let active = self.active_outcome_idx == idx;
                                let fill = if active {
                                    ThemePalette::tint(col, if pal.light { 24 } else { 32 })
                                } else {
                                    pal.bg_sub
                                };
                                let stroke = if active {
                                    Stroke::new(1.2, col)
                                } else {
                                    Stroke::NONE
                                };
                                let disp_name = display_text_with(l, &self.core.translator, name);
                                let max_chars = if polydesk::translate::contains_cjk(&disp_name) { 4 } else { 7 };
                                let short_name: String = if disp_name.chars().count() > max_chars {
                                    format!("{}…", disp_name.chars().take(max_chars - 1).collect::<String>())
                                } else {
                                    disp_name.clone()
                                };
                                let lbl = format!("{short_name} {}", format_odds(px, self.odds_fmt, m.tick_size));
                                let tip = if self.lang == Lang::Zh && disp_name != *name {
                                    format!("{disp_name} ({name})")
                                } else {
                                    name.clone()
                                };
                                let resp = ui
                                    .add_sized(
                                        Vec2::new(92.0, 28.0),
                                        egui::Button::new(
                                            RichText::new(lbl)
                                                .monospace()
                                                .strong()
                                                .size(11.5)
                                                .color(if active { col } else { pal.text_primary }),
                                        )
                                        .fill(fill)
                                        .stroke(stroke)
                                        .corner_radius(CornerRadius::same(7)),
                                    )
                                    .on_hover_text(tip);
                                if resp.clicked() && !active {
                                    *outcome_switch = Some((idx, px));
                                }
                            }
                        });
                        let p0 = out_probs[0].unwrap_or(0.5).clamp(0.02, 0.98) as f32;
                        let (bar_r, _) = ui.allocate_exact_size(Vec2::new(190.0, 3.0), egui::Sense::hover());
                        let p = ui.painter();
                        p.rect_filled(bar_r, CornerRadius::same(2), ThemePalette::tint(pal.red, if pal.light { 80 } else { 95 }));
                        p.rect_filled(
                            egui::Rect::from_min_max(
                                bar_r.left_top(),
                                egui::pos2(bar_r.left() + bar_r.width() * p0, bar_r.bottom()),
                            ),
                            CornerRadius::same(2),
                            pal.green,
                        );
                    });
                };

                if let Some(ref info) = stf {
                    need_1hz_repaint = true;
                    let secs_left = market_countdown_secs(m, info.slot_ts, info.interval_mins);
                    let mut want_next = false;
                    let mut auto_rollover = false;
                    if self.auto_next_round
                        && secs_left == Some(0)
                        && self.last_rollover_cond.as_deref() != Some(&m.condition_id)
                    {
                        auto_rollover = true;
                    }
                    let is_stale = oracle_snap.as_ref().is_none_or(|s| s.stale);

                    pal.surface_card()
                        .inner_margin(Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing.y = 8.0;

                            // Row 1: Hero Price + Delta Pill left; Countdown + Next/Auto right
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 10.0;

                                let (spot_txt, spot_col, hero_btc) = match oracle_snap.as_ref() {
                                    Some(s) => {
                                        let px = if self.primary_btc_live_lead && !is_stale {
                                            s.fast_spot.unwrap_or(s.spot)
                                        } else {
                                            s.spot
                                        };
                                        (
                                            format_oracle_usd(px),
                                            if is_stale && !self.primary_btc_live_lead {
                                                pal.text_muted
                                            } else {
                                                match beat_opt {
                                                    Some(b) if px >= b => pal.green,
                                                    Some(_) => pal.red,
                                                    None => pal.text_primary,
                                                }
                                            },
                                            Some(px),
                                        )
                                    }
                                    None => ("—".to_string(), pal.text_muted, None),
                                };
                                ui.label(RichText::new(spot_txt).monospace().strong().size(24.0).color(spot_col));

                                if let (Some(px), Some(b)) = (hero_btc, beat_opt) {
                                    let diff = px - b;
                                    let pct = if b.abs() > f64::EPSILON { diff / b * 100.0 } else { 0.0 };
                                    let dcol = if is_stale {
                                        pal.text_muted
                                    } else if diff >= 0.0 {
                                        pal.green_text
                                    } else {
                                        pal.red_text
                                    };
                                    let dfill = if is_stale {
                                        pal.bg_sub
                                    } else {
                                        ThemePalette::tint(if diff >= 0.0 { pal.green } else { pal.red }, if pal.light { 22 } else { 26 })
                                    };
                                    Frame::NONE
                                        .fill(dfill)
                                        .corner_radius(CornerRadius::same(6))
                                        .inner_margin(Margin::symmetric(7, 3))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new(format!(
                                                    "{}{:.2} ({}{:.2}%)",
                                                    if diff >= 0.0 { "+" } else { "-" },
                                                    diff.abs(),
                                                    if pct >= 0.0 { "+" } else { "-" },
                                                    pct.abs()
                                                ))
                                                .monospace()
                                                .strong()
                                                .size(11.0)
                                                .color(dcol),
                                            );
                                        });
                                }

                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.spacing_mut().item_spacing.x = 7.0;
                                    ui.checkbox(
                                        &mut self.auto_next_round,
                                        RichText::new(tr(l, "Auto", "自动")).small().color(pal.text_secondary),
                                    );
                                    if ui
                                        .add(
                                            egui::Button::new(RichText::new(tr(l, "Next →", "下轮 →")).small().color(pal.text_primary))
                                                .fill(pal.bg_sub)
                                                .stroke(Stroke::NONE)
                                                .corner_radius(CornerRadius::same(6)),
                                        )
                                        .clicked()
                                    {
                                        want_next = true;
                                    }
                                    if let Some(rem) = secs_left {
                                        let mm = rem / 60;
                                        let ss = rem % 60;
                                        let total_s = i64::from(info.interval_mins.max(1)) * 60;
                                        let frac = (1.0 - (rem as f32 / total_s as f32)).clamp(0.0, 1.0);
                                        let c_col = if rem <= 30 { pal.red } else if rem <= 60 { pal.amber } else { pal.text_primary };
                                        ui.allocate_ui(Vec2::new(52.0, 26.0), |ui| {
                                            ui.vertical_centered(|ui| {
                                                ui.spacing_mut().item_spacing.y = 2.0;
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(format!("{mm:02}:{ss:02}"))
                                                            .monospace()
                                                            .strong()
                                                            .size(13.5)
                                                            .color(c_col),
                                                    )
                                                    .wrap_mode(egui::TextWrapMode::Extend),
                                                );
                                                let (pb, _) = ui.allocate_exact_size(Vec2::new(48.0, 2.5), egui::Sense::hover());
                                                ui.painter().rect_filled(pb, CornerRadius::same(2), pal.bg_chip);
                                                ui.painter().rect_filled(
                                                    egui::Rect::from_min_max(pb.left_top(), egui::pos2(pb.left() + pb.width() * frac, pb.bottom())),
                                                    CornerRadius::same(2),
                                                    c_col,
                                                );
                                            });
                                        });
                                    }
                                });
                            });

                            // Row 2: Borderless Stacked Micro-Metrics left + Outcome Pair right
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 12.0;

                                let draw_metric = |ui: &mut egui::Ui, label: &str, val: String, col: Color32| {
                                    ui.vertical(|ui| {
                                        ui.spacing_mut().item_spacing.y = 1.0;
                                        ui.label(RichText::new(label).size(9.5).color(pal.text_muted));
                                        ui.label(RichText::new(val).monospace().strong().size(12.0).color(col));
                                    });
                                };

                                draw_metric(
                                    ui,
                                    tr(l, "PRICE TO BEAT", "目标基准价"),
                                    beat_opt.map(format_oracle_usd).unwrap_or_else(|| "—".into()),
                                    pal.amber,
                                );

                                v_hairline(ui, pal, 24.0);

                                let (tw_val, tw_col) = if is_stale {
                                    (
                                        oracle_snap
                                            .as_ref()
                                            .map(|s| format!("{} · {}", format_oracle_usd(s.twap_60s), tr(l, "STALE", "中断")))
                                            .unwrap_or_else(|| tr(l, "RECONNECT", "重连中").to_string()),
                                        pal.text_muted,
                                    )
                                } else if let Some(ref s) = oracle_snap {
                                    let dcol = match beat_opt {
                                        Some(b) if s.twap_60s >= b => pal.green_text,
                                        Some(_) => pal.red_text,
                                        None => pal.text_primary,
                                    };
                                    (format_oracle_usd(s.twap_60s), dcol)
                                } else {
                                    ("—".into(), pal.text_muted)
                                };
                                draw_metric(ui, "60s TWAP", tw_val, tw_col);

                                if self.primary_btc_live_lead
                                    && let Some(ref s) = oracle_snap
                                {
                                    v_hairline(ui, pal, 24.0);
                                    draw_metric(ui, "CL SETTLE", format_oracle_usd(s.spot), pal.blue);
                                }

                                if let Some(ref s) = oracle_snap
                                    && !is_stale
                                    && let Some(bm) = s.binance_mid
                                    && ui.available_width() > 290.0
                                {
                                    v_hairline(ui, pal, 24.0);
                                    draw_metric(ui, "BINANCE", format_oracle_usd(bm), pal.text_secondary);
                                }

                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    draw_outcome_pair(ui, &mut outcome_switch);
                                });
                            });
                        });
                    if auto_rollover {
                        self.last_rollover_cond = Some(m.condition_id.clone());
                        self.trigger_next_round(m);
                    } else if want_next {
                        self.trigger_next_round(m);
                    }
                } else {
                    pal.surface_card()
                        .inner_margin(Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 10.0;
                                let hero_col = if delta_pts >= 0.0 { pal.green } else { pal.red };
                                ui.label(
                                    RichText::new(&hero_odds)
                                        .monospace()
                                        .strong()
                                        .size(23.0)
                                        .color(hero_col),
                                );
                                Frame::NONE
                                    .fill(ThemePalette::tint(hero_col, if pal.light { 22 } else { 26 }))
                                    .corner_radius(CornerRadius::same(6))
                                    .inner_margin(Margin::symmetric(7, 3))
                                    .show(ui, |ui| {
                                        ui.label(
                                            RichText::new(format!("{}{:.1}%", if delta_pts >= 0.0 { "+" } else { "" }, delta_pts))
                                                .monospace()
                                                .strong()
                                                .size(11.5)
                                                .color(if delta_pts >= 0.0 { pal.green_text } else { pal.red_text }),
                                        );
                                    });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    draw_outcome_pair(ui, &mut outcome_switch);
                                });
                            });
                        });
                }
            }
            if let Some((idx, px)) = outcome_switch {
                self.switch_active_outcome(idx, px);
            }

            ui.add_space(6.0);

            // Sleek Indicators + Segmented Timeframe bar (pure LTR, tight pills)
            if stf.is_none() || self.short_chart_view == ShortChartView::Shares {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    let total_w = ui.available_width();
                    let right_w = if stf.is_some() { 368.0 } else { 250.0 };
                    let left_w = (total_w - right_w - 6.0).max(160.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(left_w, 26.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(RichText::new(tr(l, "Indicators", "指标")).small().color(pal.text_muted));
                            for (flag, name, col) in [
                                (&mut self.indicators.ema9, "EMA9", pal.ind_ema),
                                (&mut self.indicators.sma20, "SMA20", pal.ind_sma),
                                (&mut self.indicators.bollinger, "BB(20,2)", pal.ind_bb),
                                (&mut self.indicators.vwap, "VWAP", pal.ind_vwap),
                                (&mut self.indicators.rsi14, "RSI(14)", pal.amber),
                            ] {
                                let active = *flag;
                                let fill = if active {
                                    ThemePalette::tint(col, if pal.light { 24 } else { 30 })
                                } else {
                                    Color32::TRANSPARENT
                                };
                                if ui
                                    .add(
                                        egui::Button::new(RichText::new(name).monospace().size(11.0).color(if active { col } else { pal.text_muted }))
                                            .fill(fill)
                                            .stroke(Stroke::NONE)
                                            .corner_radius(CornerRadius::same(6)),
                                    )
                                    .clicked()
                                {
                                    *flag = !*flag;
                                }
                            }
                        },
                    );

                    if stf.is_some() {
                        pal.pill_container().show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                if seg_item(ui, pal, self.chart_mode == ChartMode::Candles, tr(l, "Candles", "K线")).clicked() {
                                    self.chart_mode = ChartMode::Candles;
                                }
                                if seg_item(ui, pal, self.chart_mode == ChartMode::Line, tr(l, "Line", "折线")).clicked() {
                                    self.chart_mode = ChartMode::Line;
                                }
                            });
                        });
                    }

                    pal.pill_container().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            for iv in ["1h", "6h", "1d", "1w", "1m", "max"] {
                                if seg_item(ui, pal, self.chart_interval == iv, &iv.to_uppercase()).clicked() {
                                    self.chart_interval = iv;
                                    self.history.clear();
                                    self.chart_cache.clear();
                                    self.history_error = None;
                                    if let Some(aid) = self.active_asset_id().map(|s| s.to_string()) {
                                        self.trigger_load_history(&aid);
                                    }
                                }
                            }
                        });
                    });
                });

                ui.add_space(5.0);
            }

            if stf.is_some() && self.short_chart_view != ShortChartView::Shares {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;

                    let live_tip = tr(
                        l,
                        "Live Binance USD-M Futures price calibrated to Chainlink USD (~80–100ms). Moves in real time with share odds; green above Target Price, red below.",
                        "基于币安 USD-M 合约的零延迟校准实时币价（约 80–100ms），与份额同步波动；高于目标价显示绿色，低于显示红色。",
                    );
                    draw_line_key_chip(ui, pal, pal.green, false, tr(l, "Live BTC", "实时币价"), live_tip, None);

                    let beat_tip = tr(
                        l,
                        "Price to Beat (dashed amber): Round opening target price. Settling at/above this line pays Up ($1); below pays Down ($1).",
                        "目标基准价（琥珀虚线）：本轮开盘锁定价格；到期高于基准价 Up 胜 ($1)，低于则 Down 胜 ($1)。",
                    );
                    draw_line_key_chip(ui, pal, pal.amber, true, tr(l, "Price to Beat", "目标基准价"), beat_tip, None);

                    if self.short_chart_view == ShortChartView::Overlay {
                        let sh_tip = tr(
                            l,
                            "Active outcome share price (cyan): left axis shows quiet ¢/%, right edge shows live Share pill. Step line = slot history; solid = live ticks.",
                            "当前选定方向的实时份额走势（青色）：左轴显示美分刻度，右侧直接标注实时份额价格，阶梯线为本轮历史、实线为实时逐笔。",
                        );
                        draw_line_key_chip(ui, pal, pal.cyan, false, tr(l, "Shares (Step/Live)", "份额走势 (青线)"), sh_tip, None);
                    }

                    if self.short_chart_view != ShortChartView::Overlay {
                        let cl_tip = tr(
                            l,
                            "Official Polymarket Chainlink settlement oracle (~1–2s upstream delay). Used to settle the round at expiry. Click to show/hide.",
                            "Polymarket 官方 Chainlink 结算预言机线（上游约 1–2s 延迟），到期结算唯一依据。点击可显示/隐藏。",
                        );
                        if draw_line_key_chip(
                            ui,
                            pal,
                            pal.blue,
                            false,
                            tr(l, "Chainlink", "Chainlink 结算"),
                            cl_tip,
                            Some(self.show_oracle_cl_ref),
                        ) {
                            self.show_oracle_cl_ref = !self.show_oracle_cl_ref;
                            let v = self.show_oracle_cl_ref;
                            let _ = self.core.execution.update_config_and_save(|c| c.show_oracle_cl_ref = v);
                        }

                        let tw_tip = tr(
                            l,
                            "60-second time-weighted average price (TWAP60) of Chainlink updates. Smooths noisy spikes. Click to show/hide.",
                            "Chainlink 60秒时间加权均价线 (TWAP60)，用于平滑短时毛刺。点击可显示/隐藏。",
                        );
                        if draw_line_key_chip(
                            ui,
                            pal,
                            pal.ind_sma,
                            false,
                            "60s TWAP",
                            tw_tip,
                            Some(self.show_oracle_twap),
                        ) {
                            self.show_oracle_twap = !self.show_oracle_twap;
                            let v = self.show_oracle_twap;
                            let _ = self.core.execution.update_config_and_save(|c| c.show_oracle_twap = v);
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let g_col = if self.show_line_guide { pal.blue } else { pal.text_muted };
                        let g_fill = if self.show_line_guide {
                            ThemePalette::tint(pal.blue, if pal.light { 22 } else { 28 })
                        } else {
                            Color32::TRANSPARENT
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(tr(l, "? Line Guide", "? 线条说明"))
                                        .small()
                                        .color(g_col),
                                )
                                .fill(g_fill)
                                .stroke(Stroke::NONE)
                                .corner_radius(CornerRadius::same(6)),
                            )
                            .clicked()
                        {
                            self.show_line_guide = !self.show_line_guide;
                        }
                    });
                });

                if self.show_line_guide {
                    ui.add_space(4.0);
                    Frame::NONE
                        .fill(pal.bg_sub)
                        .stroke(Stroke::new(1.0, pal.border))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(10, 7))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(tr(l, "Chart Line Guide (Right Axis = USD · Overlay Left Axis = Share ¢ Scale, Right Pill = Live Share)", "图表线条说明（右轴 = 美元币价 · 叠加左轴 = 份额刻度，右轴标签 = 实时份额）"))
                                        .strong()
                                        .small()
                                        .color(pal.text_primary),
                                );
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("×").clicked() {
                                        self.show_line_guide = false;
                                    }
                                });
                            });
                            ui.add_space(4.0);
                            egui::Grid::new("line_guide_grid").num_columns(3).spacing([10.0, 4.0]).show(ui, |ui| {
                                let rows: Vec<(Color32, bool, &str, &str, &str)> = vec![
                                    (
                                        pal.green,
                                        false,
                                        tr(l, "Live BTC / Coin (Green/Red)", "实时币价 (绿/红主线)"),
                                        tr(l, "Right ($)", "右轴 ($)"),
                                        tr(l, "Binance Futures price calibrated to Chainlink (~80–100ms). Leads Chainlink so it moves in sync with Polymarket share odds.", "币安合约实时价差校准线（约 80–100ms），领先 Chainlink 结算源，与份额盘口同步。"),
                                    ),
                                    (
                                        pal.amber,
                                        true,
                                        tr(l, "Price to Beat (Dashed Amber)", "目标基准价 (琥珀虚线)"),
                                        tr(l, "Right ($)", "右轴 ($)"),
                                        tr(l, "Opening target for this round. Expiry price >= Beat -> Up wins $1; < Beat -> Down wins $1.", "本轮开盘基准价；到期结算 >= 基准价 Up 获胜赔付 $1，否则 Down 胜。"),
                                    ),
                                    (
                                        pal.blue,
                                        false,
                                        tr(l, "Chainlink Settle (Blue)", "Chainlink 结算 (蓝线)"),
                                        tr(l, "Right ($)", "右轴 ($)"),
                                        tr(l, "Official Polymarket Chainlink RTDS oracle (~1.5–2s upstream delay) used for final settlement.", "Polymarket 官方 Chainlink 结算预言机（约 1.5–2s 延迟），决定最终结算价。"),
                                    ),
                                    (
                                        pal.ind_sma,
                                        false,
                                        tr(l, "60s TWAP (Gold)", "60秒均价线 (金线)"),
                                        tr(l, "Right ($)", "右轴 ($)"),
                                        tr(l, "Rolling 60-second time-weighted average of Chainlink to show short-term trend.", "Chainlink 60秒时间加权平均价格，辅助判断短时趋势。"),
                                    ),
                                    (
                                        pal.cyan,
                                        false,
                                        tr(l, "Shares Overlay (Cyan)", "叠加份额走势 (青线)"),
                                        tr(l, "Left ¢ / Right Pill", "左刻度 / 右标签"),
                                        tr(l, "Selected outcome mid price: horizontal step holds show intra-slot history, solid curve shows live WS ticks, and the right-edge pill tracks the live price.", "所选方向实时份额：阶梯线为本轮历史、实线为实时逐笔，右侧直接标出当前份额价格。"),
                                    ),
                                ];
                                for (col, dashed, name, axis, desc) in rows {
                                    draw_line_key_chip(ui, pal, col, dashed, name, desc, None);
                                    ui.label(RichText::new(axis).monospace().small().color(pal.text_muted));
                                    ui.label(RichText::new(desc).small().color(pal.text_secondary));
                                    ui.end_row();
                                }
                            });
                        });
                }
                ui.add_space(4.0);
            }

            let rem_h = ui.available_height();
            let chart_h = if stf.is_some() && self.short_chart_view == ShortChartView::Both {
                ((rem_h - 10.0) * 0.64).floor().clamp(265.0, 510.0)
            } else {
                ((rem_h - 10.0) * 0.56).floor().clamp(210.0, 480.0)
            };
            let tick_sz = self.active_market.as_ref().map(|m| m.tick_size).unwrap_or(0.01);
            let active_pos = active_position_for(&self.account, active_aid.as_deref());
            let chart_rules: Vec<AutomationRule> = self
                .cached_rules
                .iter()
                .filter(|r| r.mode == self.account.mode)
                .cloned()
                .collect();
            let share_mode = if self.chart_mode == ChartMode::Oracle {
                ChartMode::Line
            } else {
                self.chart_mode
            };

            if let Some(ref info) = stf {
                let cid = self.active_market.as_ref().map(|m| m.condition_id.clone()).unwrap_or_default();
                let snap = self.core.oracle.snapshot(&info.chainlink_symbol);
                let beat = self.crypto_open_prices.get(&cid).copied();
                let oname = self
                    .active_market
                    .as_ref()
                    .and_then(|m| m.outcomes.get(self.active_outcome_idx))
                    .map(|s| display_text_with(l, &self.core.translator, s))
                    .unwrap_or_else(|| tr(l, "Yes", "是").to_string());
                let live_px = active_top
                    .and_then(|(_, a, mid)| mid.or(a))
                    .or_else(|| {
                        self.active_market
                            .as_ref()
                            .and_then(|m| m.outcome_prices.get(self.active_outcome_idx).copied())
                    })
                    .unwrap_or(0.5);

                let now_ms = chrono::Utc::now().timestamp_millis();
                let window_ms = self.short_chart_window_secs.clamp(60, 300) * 1000;
                let max_sample_t = snap
                    .as_ref()
                    .map(|s| {
                        s.history
                            .last()
                            .map(|x| x.0)
                            .unwrap_or(now_ms)
                            .max(s.fast_history.last().map(|x| x.0).unwrap_or(i64::MIN))
                    })
                    .unwrap_or(now_ms);
                let sync_max_t = now_ms.max(max_sample_t);
                let sync_min_t = sync_max_t - window_ms;

                let mut share_timeline_ms: Vec<(i64, f64)> = Vec::new();
                if matches!(self.short_chart_view, ShortChartView::Both | ShortChartView::Overlay) {
                    let slot_start_ms = info.slot_ts.map(|s| s * 1000).unwrap_or(i64::MIN);
                    let first_live_t = self.share_live_ticks.first().map(|&(t, _)| t).unwrap_or(i64::MAX);
                    for pt in &self.history {
                        let t_ms = pt.t * 1000;
                        if t_ms >= slot_start_ms && t_ms < first_live_t {
                            share_timeline_ms.push((t_ms, pt.p));
                        }
                    }
                    share_timeline_ms.extend_from_slice(&self.share_live_ticks);
                }

                let data_owned = snap.as_ref().map(|s| OracleChartData {
                    symbol: &s.symbol,
                    samples: &s.history,
                    fast_samples: &s.fast_history,
                    fast_spot: s.fast_spot,
                    fast_wire_lag_ms: s.fast_wire_lag_ms,
                    now_ms: sync_max_t,
                    window_ms,
                    primary_live_lead: self.primary_btc_live_lead,
                    twap_60s: Some(s.twap_60s),
                    price_to_beat: beat,
                    stale: s.stale,
                    last_timestamp_ms: s.last_timestamp_ms,
                    show_twap: self.show_oracle_twap,
                    show_fast_lead: self.show_oracle_fast_lead,
                    show_cl_ref: self.show_oracle_cl_ref,
                    show_lag: self.show_oracle_lag_badge,
                    share_overlay: if self.short_chart_view == ShortChartView::Overlay {
                        Some(ShareOverlayData {
                            label: &oname,
                            samples_ms: &share_timeline_ms,
                            latest_price: live_px,
                            odds_fmt: self.odds_fmt,
                            tick_size: tick_sz,
                        })
                    } else {
                        None
                    },
                });

                match self.short_chart_view {
                    ShortChartView::Both => {
                        let gap = 6.0;
                        let share_h = ((chart_h - gap) * 0.34).floor().clamp(96.0, 165.0);
                        let oracle_h = (chart_h - gap - share_h).max(145.0);
                        draw_oracle_chart(ui, l, pal, data_owned.as_ref(), &mut self.oracle_chart_state, oracle_h);
                        ui.add_space(gap);

                        let share_badge = format!("{} · {} {}", tr(l, "SHARES", "份额"), oname, hero_odds);
                        draw_synced_share_chart(
                            ui,
                            l,
                            pal,
                            Some(&share_badge),
                            &share_timeline_ms,
                            live_px,
                            sync_min_t,
                            sync_max_t,
                            self.odds_fmt,
                            tick_sz,
                            active_pos,
                            &chart_rules,
                            active_aid.as_deref().unwrap_or(""),
                            share_h,
                        );
                    }
                    ShortChartView::Overlay | ShortChartView::Spot => {
                        draw_oracle_chart(ui, l, pal, data_owned.as_ref(), &mut self.oracle_chart_state, chart_h);
                    }
                    ShortChartView::Shares => {
                        draw_price_chart(
                            ui,
                            l,
                            pal,
                            active_aid.as_deref().unwrap_or(""),
                            self.chart_interval,
                            &self.history,
                            self.history_loading,
                            self.history_error.as_deref(),
                            share_mode,
                            self.indicators,
                            self.odds_fmt,
                            tick_sz,
                            active_top.map(|(b, a, _)| (b, a)),
                            active_pos,
                            &chart_rules,
                            &mut self.chart_cache,
                            None,
                            chart_h,
                        );
                    }
                }
            } else {
                draw_price_chart(
                    ui,
                    l,
                    pal,
                    active_aid.as_deref().unwrap_or(""),
                    self.chart_interval,
                    &self.history,
                    self.history_loading,
                    self.history_error.as_deref(),
                    share_mode,
                    self.indicators,
                    self.odds_fmt,
                    tick_sz,
                    active_top.map(|(b, a, _)| (b, a)),
                    active_pos,
                    &chart_rules,
                    &mut self.chart_cache,
                    None,
                    chart_h,
                );
            }

            ui.add_space(8.0);

            // Bottom Workspace Card with underline tabs
            let deck_h = ui.available_height().max(120.0);
            pal.surface_card().show(ui, |ui| {
                ui.set_min_height((deck_h - 22.0).max(96.0));
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let pos_n = self.all_positions().len();
                    let ord_n = self.all_open_orders().len();
                    let news_n = self.news_feed.as_ref().map(|f| f.items.len()).unwrap_or(0);
                    let rule_n = self.cached_rules.iter().filter(|r| r.active).count();
                    let alert_lbl = if self.unread_alerts > 0 {
                        format!("{} ({})", tr(l, "Alerts", "预警"), self.unread_alerts)
                    } else {
                        tr(l, "Alerts", "预警").to_string()
                    };
                    let fmt_cnt = |base: &str, n: usize| {
                        if n > 0 { format!("{base} {n}") } else { base.to_string() }
                    };
                    for (tab, label) in [
                        (DeckTab::Positions, fmt_cnt(tr(l, "Positions", "持仓"), pos_n)),
                        (DeckTab::Orders, fmt_cnt(tr(l, "Open Orders", "挂单"), ord_n)),
                        (DeckTab::News, fmt_cnt(tr(l, "News", "新闻"), news_n)),
                        (DeckTab::Automation, fmt_cnt(tr(l, "Auto Rules", "条件单"), rule_n)),
                        (DeckTab::Alerts, alert_lbl),
                        (DeckTab::WhalesTape, tr(l, "Whales & Tape", "鲸鱼/逐笔").to_string()),
                        (DeckTab::CopyTrade, tr(l, "Copy-Trade", "跟单").to_string()),
                        (DeckTab::Fees, tr(l, "Fills & Fees", "成交/费率").to_string()),
                    ] {
                        if underline_tab(ui, pal, self.deck_tab == tab, &label).clicked() {
                            self.deck_tab = tab;
                            if tab == DeckTab::Alerts {
                                self.unread_alerts = 0;
                            } else if tab == DeckTab::News
                                && self.news_feed.is_none()
                                && !self.news_loading
                                && self.active_market.is_some()
                            {
                                self.trigger_load_news(false);
                            }
                        }
                    }
                });
                let (sep_r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), egui::Sense::hover());
                ui.painter().line_segment(
                    [egui::pos2(sep_r.left(), sep_r.center().y), egui::pos2(sep_r.right(), sep_r.center().y)],
                    Stroke::new(1.0, pal.border),
                );
                ui.add_space(6.0);
                ScrollArea::vertical().id_salt("deck_scroll").show(ui, |ui| {
                    self.draw_bottom_deck(ui, active_aid.as_deref());
                });
            });
        });

        // Modals & Toasts
        self.render_modals(&ctx);
        if !self.toasts.is_empty() {
            egui::Area::new(egui::Id::NULL.with("toast_area"))
                .order(egui::Order::Tooltip)
                .interactable(false)
                .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-16.0, -16.0))
                .show(&ctx, |ui| {
                    for (txt, is_err, _) in &self.toasts {
                        let stroke_col = if *is_err { pal.red } else { pal.green };
                        Frame::NONE
                            .fill(pal.tooltip_bg)
                            .stroke(Stroke::new(1.0, stroke_col))
                            .inner_margin(Margin::symmetric(12, 8))
                            .corner_radius(CornerRadius::same(6))
                            .show(ui, |ui| {
                                ui.label(RichText::new(txt).monospace().small().color(pal.tooltip_fg));
                            });
                    }
                });
        }

        // Event-driven repaint: fast toast expiry while toasts exist, 1Hz tick on timed markets, 2s idle heartbeat otherwise.
        if !self.toasts.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(250));
        } else if need_1hz_repaint {
            ctx.request_repaint_after(Duration::from_secs(1));
        } else {
            ctx.request_repaint_after(Duration::from_secs(2));
        }
    }
}

impl PolyDeskDesktop {
    fn draw_bottom_deck(&mut self, ui: &mut egui::Ui, active_aid: Option<&str>) {
        let l = self.lang;
        let pal = self.palette();
        match self.deck_tab {
            DeckTab::Positions => {
                let mut pos_actions: Vec<(String, &'static str)> = Vec::new();
                let mut redeem_cond: Option<String> = None;
                let modal_open = self.modal.is_some();
                let is_live = self.account.mode == TradingMode::Live;
                let is_eoa = self.account.live_connected
                    && self.account.signature_type == Some(WalletSignatureType::Eoa);
                {
                    let positions = positions_for(&self.account);
                    if positions.is_empty() {
                        ui.add_space(18.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new(tr(l, "No open positions", "暂无持仓")).strong().size(13.0).color(pal.text_secondary));
                            ui.label(RichText::new(tr(l, "Use the ticket or Price Ladder on the right to enter a trade.", "在右侧快捷下单或点击盘口价格即可开仓。")).small().color(pal.text_muted));
                        });
                        return;
                    }
                    egui::Grid::new("pos_grid").striped(true).num_columns(7).spacing([14.0, 5.0]).show(ui, |ui| {
                        for h in [tr(l, "Market", "市场"), tr(l, "Side", "方向"), tr(l, "Shares", "份数"), tr(l, "Avg", "均价"), tr(l, "Mark", "现价"), tr(l, "uP&L", "浮盈"), tr(l, "Actions", "操作")] {
                            ui.label(RichText::new(h).strong().small().color(pal.text_muted));
                        }
                        ui.end_row();
                        for p in positions {
                            let mk = self
                                .core
                                .feed
                                .get_top_of_book(&p.asset_id)
                                .and_then(|(b, _, m)| b.or(m))
                                .unwrap_or(p.avg_entry);
                            let upnl = p.shares * (mk - p.avg_entry);
                            let pq = display_text_with(l, &self.core.translator, &p.market_question);
                            let po = display_text_with(l, &self.core.translator, &p.outcome);
                            ui.label(pq.chars().take(36).collect::<String>()).on_hover_text(&p.market_question);
                            ui.label(po);
                            ui.label(RichText::new(format!("{:.2}", p.shares)).monospace());
                            ui.label(RichText::new(format_odds(Some(p.avg_entry), self.odds_fmt, 0.01)).monospace());
                            ui.label(RichText::new(format_odds(Some(mk), self.odds_fmt, 0.01)).monospace());
                            let pc = if upnl >= 0.0 { pal.green } else { pal.red };
                            ui.label(RichText::new(format_usd(upnl, true)).monospace().strong().color(pc));
                            let busy = modal_open || self.order_in_flight.contains(&p.asset_id);
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!busy, egui::Button::new(tr(l, "Cash Out", "锁盈")).small()).clicked() {
                                    pos_actions.push((p.asset_id.clone(), "close"));
                                }
                                if ui.add_enabled(!busy, egui::Button::new(tr(l, "Free-Roll", "抽本")).small()).clicked() {
                                    pos_actions.push((p.asset_id.clone(), "free_roll"));
                                }
                                if is_live && !p.condition_id.is_empty() {
                                    let r_btn = ui.add_enabled(
                                        !modal_open && !self.ctf_in_flight && is_eoa,
                                        egui::Button::new(tr(l, "Redeem", "赎回")).small(),
                                    );
                                    if !is_eoa {
                                        r_btn.on_hover_text(tr(
                                            l,
                                            "Direct on-chain Redeem requires an EOA wallet (use Polymarket Portfolio for Proxy/Safe)",
                                            "链上赎回仅限 EOA 钱包，Proxy/Safe 钱包请前往 Polymarket 官网资产页",
                                        ));
                                    } else if r_btn.clicked() {
                                        redeem_cond = Some(p.condition_id.clone());
                                    }
                                }
                            });
                            ui.end_row();
                        }
                    });
                }
                for (aid, strat) in pos_actions {
                    self.trigger_green_up_for(aid, None, strat);
                }
                if let Some(cid) = redeem_cond
                    && let Some(req) = self.build_ctf_request_for_condition(&cid, CtfAction::Redeem, 0.0)
                {
                    self.pending_ctf = Some(req);
                    self.last_ctf_result = None;
                    self.modal = Some(ActiveModal::CtfConfirm);
                }
            }
            DeckTab::Orders => {
                let orders = open_orders_for(&self.account);
                if orders.is_empty() {
                    ui.add_space(18.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(tr(l, "No open resting orders", "暂无活跃挂单")).strong().size(13.0).color(pal.text_secondary));
                        ui.label(RichText::new(tr(l, "Limit and GTC orders you place will appear here for 1-click cancel.", "限价或 GTC 挂单将显示在此处，支持一键撤单。")).small().color(pal.text_muted));
                    });
                    return;
                }
                egui::Grid::new("ord_grid").striped(true).num_columns(6).spacing([14.0, 5.0]).show(ui, |ui| {
                    for h in [tr(l, "Side", "方向"), tr(l, "Price", "价格"), tr(l, "Size", "数量"), "Type", tr(l, "Market", "市场"), ""] {
                        ui.label(RichText::new(h).strong().small().color(pal.text_muted));
                    }
                    ui.end_row();
                    for o in orders {
                        let sc = if o.side == "BUY" { pal.green } else { pal.red };
                        ui.label(RichText::new(&o.side).monospace().strong().color(sc));
                        ui.label(RichText::new(format_odds(Some(o.price), self.odds_fmt, 0.01)).monospace());
                        ui.label(RichText::new(format!("{:.1} / {:.1}", o.remaining_size, o.original_size)).monospace());
                        ui.label(format!("{}{}", o.order_type, if o.post_only { " PO" } else { "" }));
                        let oq = display_text_with(l, &self.core.translator, &o.market_question);
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
            DeckTab::News => {
                if self.active_market.is_none() {
                    ui.add_space(18.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(tr(l, "No market selected", "未选择市场")).strong().size(13.0).color(pal.text_secondary));
                        ui.label(RichText::new(tr(l, "Select a market on the left to load related live headlines.", "在左侧选择任意市场即可加载相关实时新闻。")).small().color(pal.text_muted));
                    });
                    return;
                }

                let mut trigger_fetch: Option<bool> = None;
                let mut reset_auto = false;
                let mut expand_window: Option<NewsWindow> = None;
                let modal_open = self.modal.is_some();

                // Pure LTR News Controls Toolbar
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let total_w = ui.available_width();
                    let right_controls_w = 278.0_f32;
                    let search_box_w = (total_w - right_controls_w).clamp(130.0, 320.0);
                    let show_reset = !self.news_auto_query.is_empty()
                        && self.news_query_input.trim() != self.news_auto_query.trim();

                    Frame::NONE
                        .fill(pal.bg_sub)
                        .corner_radius(CornerRadius::same(7))
                        .inner_margin(Margin::symmetric(8, 4))
                        .show(ui, |ui| {
                            ui.set_width(search_box_w);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                let inner_w = (ui.available_width() - if show_reset { 24.0 } else { 4.0 }).max(80.0);
                                let resp = ui.add(
                                    TextEdit::singleline(&mut self.news_query_input)
                                        .frame(Frame::NONE)
                                        .hint_text(tr(l, "Topic or keyword...", "输入关键词搜索新闻..."))
                                        .desired_width(inner_w),
                                );
                                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                    trigger_fetch = Some(true);
                                }
                                if show_reset
                                    && ghost_button(ui, pal, RichText::new("↺").small().color(pal.text_muted))
                                        .on_hover_text(tr(l, "Reset to active market query", "恢复当前市场默认关键词"))
                                        .clicked()
                                {
                                    reset_auto = true;
                                }
                            });
                        });

                    pal.pill_container().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            for (win, lbl) in [
                                (NewsWindow::Day24h, "24h"),
                                (NewsWindow::Week7d, "7d"),
                                (NewsWindow::All, tr(l, "All", "全部")),
                            ] {
                                if seg_item(ui, pal, self.news_window == win, lbl).clicked() && self.news_window != win {
                                    self.news_window = win;
                                    trigger_fetch = Some(false);
                                }
                            }
                        });
                    });

                    pal.pill_container().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            for (ed, lbl) in [(NewsEdition::En, "EN"), (NewsEdition::Zh, "中文")] {
                                if seg_item(ui, pal, self.news_edition == ed, lbl).clicked() && self.news_edition != ed {
                                    self.news_edition = ed;
                                    trigger_fetch = Some(false);
                                }
                            }
                        });
                    });

                    let ref_lbl = if self.news_loading {
                        tr(l, "...", "加载中")
                    } else {
                        tr(l, "↻ Refresh", "↻ 刷新")
                    };
                    if ghost_button(
                        ui,
                        pal,
                        RichText::new(ref_lbl)
                            .small()
                            .color(if self.news_loading { pal.text_muted } else { pal.text_secondary }),
                    )
                    .clicked()
                        && !self.news_loading
                    {
                        trigger_fetch = Some(true);
                    }
                });

                if reset_auto {
                    self.news_query_input = self.news_auto_query.clone();
                    trigger_fetch = Some(false);
                }

                ui.add_space(6.0);

                if self.news_loading && self.news_feed.is_none() {
                    ui.add_space(14.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "{} \"{}\"...",
                                tr(l, "Fetching headlines for", "正在获取相关新闻"),
                                self.news_query_input.trim()
                            ))
                            .small()
                            .color(pal.text_muted),
                        );
                    });
                } else if let Some(ref err) = self.news_error
                    && self.news_feed.as_ref().is_none_or(|f| f.items.is_empty())
                {
                    ui.add_space(12.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(tr(l, "Unable to load headlines", "新闻加载失败")).strong().size(12.5).color(pal.red_text));
                        ui.label(RichText::new(err).small().color(pal.text_muted));
                        ui.add_space(4.0);
                        if ui
                            .add(
                                egui::Button::new(RichText::new(tr(l, "Retry", "重试")).small().color(pal.text_primary))
                                    .fill(pal.bg_sub)
                                    .stroke(Stroke::NONE)
                                    .corner_radius(CornerRadius::same(6)),
                            )
                            .clicked()
                        {
                            trigger_fetch = Some(true);
                        }
                    });
                } else if let Some(ref feed) = self.news_feed {
                    if feed.items.is_empty() {
                        ui.add_space(12.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} \"{}\"",
                                    tr(l, "No recent headlines for", "暂无近期新闻："),
                                    feed.query
                                ))
                                .size(12.5)
                                .color(pal.text_secondary),
                            );
                            let next_win = match self.news_window {
                                NewsWindow::Day24h => Some((NewsWindow::Week7d, tr(l, "Expand to 7d", "扩展至 7天"))),
                                NewsWindow::Week7d => Some((NewsWindow::All, tr(l, "Search All Time", "搜索全部时间"))),
                                NewsWindow::All => None,
                            };
                            if let Some((w, lbl)) = next_win {
                                ui.add_space(4.0);
                                if ui
                                    .add(
                                        egui::Button::new(RichText::new(lbl).small().color(pal.blue))
                                            .fill(ThemePalette::tint(pal.blue, if pal.light { 16 } else { 24 }))
                                            .stroke(Stroke::NONE)
                                            .corner_radius(CornerRadius::same(6)),
                                    )
                                    .clicked()
                                {
                                    expand_window = Some(w);
                                }
                            }
                        });
                    } else {
                        let now_ts = chrono::Utc::now().timestamp();
                        let avail_w = ui.available_width();
                        for (idx, item) in feed.items.iter().enumerate() {
                            let (age_txt, is_fresh) = format_relative_age(item.published_ts, now_ts, l);
                            let row_h = 46.0_f32;
                            let sense = if modal_open { egui::Sense::hover() } else { egui::Sense::click() };
                            let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail_w, row_h), sense);
                            let hovered = !modal_open && resp.hovered();
                            if hovered {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            let p = ui.painter();
                            if hovered {
                                p.rect_filled(rect, CornerRadius::same(7), pal.row_hover_tint);
                            }
                            if idx + 1 < feed.items.len() {
                                p.line_segment(
                                    [
                                        egui::pos2(rect.left() + 6.0, rect.bottom() - 0.5),
                                        egui::pos2(rect.right() - 6.0, rect.bottom() - 0.5),
                                    ],
                                    Stroke::new(1.0, ThemePalette::tint(pal.border, 135)),
                                );
                            }

                            // Headline (truncated cleanly to single scannable line; hover shows full headline + URL)
                            let title_w = (rect.width() - 28.0).max(120.0);
                            let mut job = egui::text::LayoutJob::single_section(
                                item.title.clone(),
                                egui::TextFormat {
                                    font_id: ThemePalette::semibold(12.2),
                                    color: if hovered { pal.blue } else { pal.text_primary },
                                    ..Default::default()
                                },
                            );
                            job.wrap = egui::text::TextWrapping {
                                max_width: title_w,
                                max_rows: 1,
                                overflow_character: Some('…'),
                                ..Default::default()
                            };
                            let galley = p.layout_job(job);
                            p.galley(egui::pos2(rect.left() + 8.0, rect.top() + 7.0), galley, pal.text_primary);

                            // Bottom metadata row: Source badge + fresh/muted timestamp left; ↗ right on hover
                            let meta_y = rect.bottom() - 12.0;
                            let mut mx = rect.left() + 8.0;
                            if !item.source.is_empty() {
                                let sg = p.layout_no_wrap(item.source.clone(), egui::FontId::proportional(10.0), pal.text_secondary);
                                let pill_w = sg.size().x + 10.0;
                                let pill_r = egui::Rect::from_min_size(
                                    egui::pos2(mx, meta_y - 7.5),
                                    Vec2::new(pill_w, 15.0),
                                );
                                p.rect_filled(pill_r, CornerRadius::same(4), pal.bg_sub);
                                p.galley(
                                    egui::pos2(pill_r.left() + 5.0, pill_r.center().y - sg.size().y * 0.5),
                                    sg,
                                    pal.text_secondary,
                                );
                                mx += pill_w + 8.0;
                            }
                            if !age_txt.is_empty() {
                                let age_col = if is_fresh { pal.green_text } else { pal.text_muted };
                                p.text(
                                    egui::pos2(mx, meta_y),
                                    egui::Align2::LEFT_CENTER,
                                    age_txt,
                                    egui::FontId::monospace(10.0),
                                    age_col,
                                );
                            }
                            if hovered {
                                p.text(
                                    egui::pos2(rect.right() - 10.0, rect.center().y),
                                    egui::Align2::RIGHT_CENTER,
                                    "↗",
                                    egui::FontId::proportional(12.0),
                                    pal.blue,
                                );
                            }

                            let tip = format!("{}\n{}", item.title, item.url);
                            let clicked = resp.on_hover_text(tip).clicked();
                            if clicked && !modal_open {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(&item.url));
                            }
                        }

                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(format!(
                                "{} · {} {}",
                                feed.provider,
                                feed.items.len(),
                                tr(l, "headlines", "条相关新闻")
                            ))
                            .monospace()
                            .size(10.0)
                            .color(pal.text_muted),
                        );
                    }
                }

                if let Some(w) = expand_window {
                    self.news_window = w;
                    trigger_fetch = Some(false);
                }
                if let Some(force) = trigger_fetch {
                    self.trigger_load_news(force);
                }
            }
            DeckTab::Automation => {
                let mut clear_done = false;
                let mut cancel_rule_id: Option<String> = None;
                ui.horizontal(|ui| {
                    if ui.small_button(tr(l, "Clear Done", "清理已完成")).clicked() {
                        clear_done = true;
                    }
                });
                if self.cached_rules.is_empty() {
                    ui.label(RichText::new(tr(l, "No armed SL / TP / Trailing / Drip rules. Open Pro Orders on the right panel to create one.", "暂无条件单，请在右侧「高级条件单」中部署止损/止盈/移动止损。")).color(pal.text_muted));
                } else {
                    let cur_mode = self.account.mode;
                    egui::Grid::new("rule_grid").striped(true).num_columns(7).spacing([14.0, 5.0]).show(ui, |ui| {
                        for r in &self.cached_rules {
                            let same_mode = r.mode == cur_mode;
                            let mode_lbl = if r.mode == TradingMode::Live { "LIVE" } else { "PAPER" };
                            let mode_col = if !same_mode {
                                pal.text_muted
                            } else if r.mode == TradingMode::Live {
                                pal.amber
                            } else {
                                pal.blue
                            };
                            ui.label(RichText::new(mode_lbl).monospace().small().strong().color(mode_col));
                            ui.label(RichText::new(format!("{:?}", r.kind)).monospace().color(if same_mode { pal.blue } else { pal.text_muted }));
                            ui.label(&r.side);
                            ui.label(RichText::new(format_odds(Some(r.trigger_price), self.odds_fmt, 0.01)).monospace());
                            ui.label(RichText::new(format!("{:.1}", r.remaining_size)).monospace());
                            let st_col = if r.status.starts_with("ERR") || r.status.starts_with("FAILED") {
                                pal.red
                            } else if same_mode {
                                pal.text_primary
                            } else {
                                pal.text_muted
                            };
                            ui.label(RichText::new(&r.status).small().color(st_col));
                            if r.active {
                                if ui.small_button(tr(l, "Cancel", "取消")).clicked() {
                                    cancel_rule_id = Some(r.id.clone());
                                }
                            } else {
                                ui.label("");
                            }
                            ui.end_row();
                        }
                    });
                }
                if clear_done {
                    self.core.automation.delete_inactive_rules();
                    self.cached_rules = self.core.automation.list_rules();
                }
                if let Some(id) = cancel_rule_id {
                    self.core.automation.cancel_rule(&id);
                    self.cached_rules = self.core.automation.list_rules();
                }
            }
            DeckTab::Alerts => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(tr(l, "New Alert:", "新增预警:")).small().color(pal.text_muted));
                    let cond_lbl = match self.alert_cond_kind {
                        0 => tr(l, "Cross Above (≥)", "上穿 (≥)"),
                        1 => tr(l, "Cross Below (≤)", "下穿 (≤)"),
                        _ => tr(l, "Spread ≥", "点差 ≥"),
                    };
                    egui::ComboBox::from_id_salt("alert_cond_cb")
                        .width(126.0)
                        .selected_text(cond_lbl)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.alert_cond_kind, 0, tr(l, "Cross Above (≥)", "上穿 (≥)"));
                            ui.selectable_value(&mut self.alert_cond_kind, 1, tr(l, "Cross Below (≤)", "下穿 (≤)"));
                            ui.selectable_value(&mut self.alert_cond_kind, 2, tr(l, "Spread ≥", "点差 ≥"));
                        });
                    ui.add(
                        egui::DragValue::new(&mut self.alert_thresh_cents)
                            .range(0.1..=99.9)
                            .speed(0.5)
                            .suffix("¢"),
                    );
                    ui.checkbox(&mut self.alert_sound, tr(l, "Bell", "响铃"));
                    let can_add = self.active_market.is_some() && active_aid.is_some();
                    if ui
                        .add_enabled(can_add, egui::Button::new(tr(l, "Add Alert", "添加预警")))
                        .clicked()
                        && let (Some(m), Some(aid)) = (self.active_market.clone(), active_aid.map(|s| s.to_string()))
                    {
                        let th = (self.alert_thresh_cents / 100.0).clamp(0.001, 0.999);
                        let cond = match self.alert_cond_kind {
                            0 => AlertCondition::CrossAbove(th),
                            1 => AlertCondition::CrossBelow(th),
                            _ => AlertCondition::SpreadAbove(th),
                        };
                        let alert = PriceAlert::new(
                            aid.clone(),
                            m.question,
                            self.active_outcome_name(),
                            cond,
                            self.alert_sound,
                        );
                        let list = self.core.alerts.add(alert);
                        let list_save = list.clone();
                        let _ = self.core.execution.update_config_and_save(|c| c.price_alerts = list_save);
                        self.cached_alerts = list;
                        self.core.feed.subscribe(vec![aid]);
                        self.sync_pinned_feed_assets();
                    }
                });
                ui.add_space(6.0);
                if self.cached_alerts.is_empty() {
                    ui.label(
                        RichText::new(tr(
                            l,
                            "No price/spread alerts configured. Alerts are read-only notifications and never place orders.",
                            "暂无价格或点差预警。预警仅作提醒，绝不会自动下单。",
                        ))
                        .color(pal.text_muted),
                    );
                } else {
                    let mut jump_asset: Option<String> = None;
                    let mut rearm_id: Option<String> = None;
                    let mut del_id: Option<String> = None;
                    egui::Grid::new("alert_grid")
                        .striped(true)
                        .num_columns(6)
                        .spacing([12.0, 5.0])
                        .show(ui, |ui| {
                            for h in [
                                tr(l, "Market", "市场"),
                                tr(l, "Outcome", "方向"),
                                tr(l, "Condition", "条件"),
                                tr(l, "Status", "状态"),
                                "",
                                "",
                            ] {
                                ui.label(RichText::new(h).strong().small().color(pal.text_muted));
                            }
                            ui.end_row();
                            for a in &self.cached_alerts {
                                let aq = display_text_with(l, &self.core.translator, &a.market_question);
                                ui.label(aq.chars().take(32).collect::<String>())
                                    .on_hover_text(&a.market_question);
                                ui.label(self.display_text(&a.outcome));
                                let cond_s = match a.condition {
                                    AlertCondition::CrossAbove(th) => format!("≥ {:.1}¢", th * 100.0),
                                    AlertCondition::CrossBelow(th) => format!("≤ {:.1}¢", th * 100.0),
                                    AlertCondition::SpreadAbove(th) => format!("Spread ≥ {:.1}¢", th * 100.0),
                                };
                                ui.label(RichText::new(cond_s).monospace());
                                let (st_s, st_c) = if a.active {
                                    (tr(l, "ARMED", "监控中"), pal.green)
                                } else {
                                    (tr(l, "TRIGGERED", "已触发"), pal.amber)
                                };
                                ui.label(RichText::new(st_s).monospace().small().strong().color(st_c));
                                if ui.small_button(tr(l, "Jump", "跳转")).clicked() {
                                    jump_asset = Some(a.asset_id.clone());
                                }
                                ui.horizontal(|ui| {
                                    if !a.active && ui.small_button(tr(l, "Re-arm", "重设")).clicked() {
                                        rearm_id = Some(a.id.clone());
                                    }
                                    if ui.small_button("×").clicked() {
                                        del_id = Some(a.id.clone());
                                    }
                                });
                                ui.end_row();
                            }
                        });
                    if let Some(aid) = jump_asset {
                        let found = self
                            .markets
                            .iter()
                            .chain(self.pinned_markets.iter())
                            .find_map(|m| {
                                m.token_ids
                                    .iter()
                                    .position(|t| t == &aid)
                                    .map(|idx| (m.clone(), idx))
                            });
                        if let Some((m, idx)) = found {
                            self.select_market(m, idx);
                        } else {
                            self.push_toast(tr(l, "Market not currently in loaded list", "当前市场列表中未找到该市场").into(), true);
                        }
                    }
                    if let Some(id) = rearm_id {
                        let list = self.core.alerts.rearm(&id);
                        let list_save = list.clone();
                        let _ = self.core.execution.update_config_and_save(|c| c.price_alerts = list_save);
                        self.cached_alerts = list;
                        if let Some(rearmed) = self.cached_alerts.iter().find(|a| a.id == id && a.active) {
                            self.core.feed.subscribe(vec![rearmed.asset_id.clone()]);
                        }
                        self.sync_pinned_feed_assets();
                    }
                    if let Some(id) = del_id {
                        let list = self.core.alerts.remove(&id);
                        let list_save = list.clone();
                        let _ = self.core.execution.update_config_and_save(|c| c.price_alerts = list_save);
                        self.cached_alerts = list;
                        self.sync_pinned_feed_assets();
                    }
                }
            }
            DeckTab::WhalesTape => {
                let mut copy_req: Option<(String, String)> = None;
                ui.columns(2, |cols| {
                    if let Some(ref wf) = self.whale_flow {
                        let fc = if wf.net_flow_usd >= 0.0 { pal.green } else { pal.red };
                        cols[0].horizontal(|ui| {
                            ui.label(RichText::new(tr(l, "Net Whale Flow", "鲸鱼净流向")).strong());
                            ui.label(RichText::new(format_usd(wf.net_flow_usd, true)).monospace().strong().color(fc));
                        });
                        cols[0].label(RichText::new(format!("YES {}  vs  NO {}", format_usd(wf.yes_whale_vol_usd, false), format_usd(wf.no_whale_vol_usd, false))).monospace().small().color(pal.text_muted));
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

                    cols[1].label(RichText::new(tr(l, "Top Market Holders", "本市场大户 (一键跟单)")).strong().small());
                    if let Some(ref wf) = self.whale_flow {
                        for h in wf.top_holders.iter().take(10) {
                            let lbl = if !h.name.is_empty() {
                                h.name.clone()
                            } else if !h.pseudonym.is_empty() {
                                h.pseudonym.clone()
                            } else {
                                h.proxy_wallet.chars().take(10).collect()
                            };
                            cols[1].horizontal(|ui| {
                                ui.label(RichText::new(&lbl).small());
                                ui.label(RichText::new(format!("{} {}", if h.outcome_index == 0 { "Y" } else { "N" }, format_size(h.amount))).monospace().small().color(pal.text_muted));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button(tr(l, "Copy", "跟单")).clicked() {
                                        copy_req = Some((h.proxy_wallet.clone(), lbl.clone()));
                                    }
                                });
                            });
                        }
                    }
                });
                if let Some((wallet, lbl)) = copy_req {
                    self.core.automation.add_copy_target(CreateCopyTargetRequest {
                        wallet,
                        label: lbl.clone(),
                        multiplier: 0.05,
                        max_order_usd: 25.0,
                        max_slippage_bps: Some(300.0),
                    });
                    self.cached_copy_targets = self.core.automation.list_copy_targets();
                    self.push_toast(format!("Now copy-trading {lbl}"), false);
                }
            }
            DeckTab::CopyTrade => {
                let mut add_target = false;
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut self.copy_wallet).hint_text("0xWhaleWallet...").desired_width(220.0));
                    ui.add(TextEdit::singleline(&mut self.copy_label).hint_text(tr(l, "Label", "备注")).desired_width(90.0));
                    ui.add(egui::DragValue::new(&mut self.copy_mult).range(0.01..=5.0).speed(0.05).suffix("x"));
                    ui.add(egui::DragValue::new(&mut self.copy_max_usd).range(1.0..=10_000.0).prefix("$"));
                    if ui.button(tr(l, "Add Copy", "添加跟单")).clicked() && !self.copy_wallet.trim().is_empty() {
                        add_target = true;
                    }
                });
                if add_target {
                    self.core.automation.add_copy_target(CreateCopyTargetRequest {
                        wallet: self.copy_wallet.clone(),
                        label: self.copy_label.clone(),
                        multiplier: self.copy_mult,
                        max_order_usd: self.copy_max_usd,
                        max_slippage_bps: Some(300.0),
                    });
                    self.copy_wallet.clear();
                    self.cached_copy_targets = self.core.automation.list_copy_targets();
                }

                ui.add_space(6.0);
                let mut toggle_id: Option<String> = None;
                let mut remove_id: Option<String> = None;
                for c in &self.cached_copy_targets {
                    let same_mode = c.mode == self.account.mode;
                    let mode_lbl = if c.mode == TradingMode::Live { "LIVE" } else { "PAPER" };
                    let mode_col = if !same_mode {
                        pal.text_muted
                    } else if c.mode == TradingMode::Live {
                        pal.amber
                    } else {
                        pal.blue
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(mode_lbl).monospace().small().strong().color(mode_col));
                        ui.label(RichText::new(&c.label).color(if same_mode { pal.text_primary } else { pal.text_muted }));
                        ui.label(RichText::new(format!("{}x · max ${:.0} · {} hits", c.multiplier, c.max_order_usd, c.copied_count)).monospace().small().color(pal.text_muted));
                        if c.failed_count > 0 {
                            let err_lbl = ui.label(RichText::new(format!("{} err", c.failed_count)).monospace().small().color(pal.red));
                            if let Some(ref err) = c.last_error {
                                err_lbl.on_hover_text(err);
                            }
                        }
                        if ui.small_button(if c.enabled { "ON" } else { "OFF" }).clicked() {
                            toggle_id = Some(c.id.clone());
                        }
                        if ui.small_button("×").clicked() {
                            remove_id = Some(c.id.clone());
                        }
                    });
                }
                if let Some(id) = toggle_id {
                    self.core.automation.toggle_copy_target(&id);
                    self.cached_copy_targets = self.core.automation.list_copy_targets();
                }
                if let Some(id) = remove_id {
                    self.core.automation.remove_copy_target(&id);
                    self.cached_copy_targets = self.core.automation.list_copy_targets();
                }
            }
            DeckTab::Fees => {
                let telem = self.core.telemetry.snapshot();
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("{} · {}bp Taker / {}bp Maker", HARDCODED_BUILDER_LABEL, self.account.builder_taker_bps, self.account.builder_maker_bps)).monospace().small().color(pal.blue));
                    ui.label(RichText::new(format!("| {}: {}", tr(l, "Volume", "成交额"), format_usd(telem.routed_volume_usd, false))).monospace().small());
                    ui.label(RichText::new(format!("| {}: {}", tr(l, "Terminal Fee", "终端费"), format_usd(telem.total_builder_fees_usd, false))).monospace().small().color(pal.amber));
                });
                ui.add_space(6.0);
                for rec in telem.recent_fee_ledger.iter().take(20) {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("[{}] {} {}", rec.mode, rec.side, format_usd(rec.notional_usd, false))).monospace().small());
                        ui.label(RichText::new(format!("Proto {} · Terminal({}bp) {}", format_usd(rec.protocol_fee_usd, false), rec.builder_fee_bps, format_usd(rec.builder_fee_usd, false))).monospace().small().color(pal.text_muted));
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
            ui.label(RichText::new(tr(l, "Conditional Exit (Rust Engine · Microsecond Trigger)", "引擎条件单（本地微秒触发）")).strong().color(pal.text_primary));
            ui.add_space(6.0);
            egui::Grid::new("adv_rule_g").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
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
            if ui.button(RichText::new(tr(l, "Arm Engine Rule", "部署条件单")).strong().color(pal.amber)).clicked()
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
                self.cached_rules = self.core.automation.list_rules();
                self.push_toast("Armed conditional rule in Rust engine".into(), false);
            }
        });

        ui.add_space(8.0);

        // 2. Scale Entry Batch Ladder
        pal.surface_card().show(ui, |ui| {
            ui.label(RichText::new(tr(l, "Scale Entry (Batch Ladder)", "区间阶梯批量挂单")).strong().color(pal.text_primary));
            ui.add_space(6.0);
            egui::Grid::new("scale_g").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
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
                if ui.button(RichText::new(tr(l, "Deploy Scale Ladder", "批量铺单")).strong().color(pal.blue)).clicked()
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
                            Ok(rep) => {
                                let is_err = rep.failed > 0;
                                let text = if rep.failed > 0 {
                                    let first_err = rep.errors.first().map(|s| format!(" ({s})")).unwrap_or_default();
                                    format!("Scale Entry: {} deployed, {} failed{first_err}", rep.placed.len(), rep.failed)
                                } else {
                                    format!("Deployed {} rungs", rep.placed.len())
                                };
                                tx.send(UiMsg::Toast { text, is_err });
                            }
                            Err(e) => { tx.send(UiMsg::Toast { text: e.to_string(), is_err: true }); }
                        }
                    });
                }
            });
        });

        ui.add_space(8.0);

        // 3. Green-Up & Sweep Quotes
        if let Some(aid) = self.active_asset_id().map(|s| s.to_string()) {
            let opp = self.opposite_asset_id().map(|s| s.to_string());
            if let Some(g) = self.core.execution.preview_green_up(&aid, opp.as_deref()) {
                let g: GreenUpQuote = g;
                let busy = self.order_in_flight.contains(&aid);
                pal.surface_card().show(ui, |ui| {
                    ui.label(RichText::new(tr(l, "Green-Up / Trade-Out", "一键锁盈策略")).strong().color(pal.green));
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("Cash Out: {} @ {}", format_usd(g.close_locked_pnl, true), format_odds(Some(g.close_price), self.odds_fmt, 0.01))).monospace());
                    if g.can_free_roll {
                        ui.label(RichText::new(format!("Free-Roll: keep {:.1} risk-free shares", g.free_roll_remaining_shares)).monospace().small().color(pal.green));
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!busy, egui::Button::new(tr(l, "Cash Out", "市价锁盈"))).clicked() { self.trigger_green_up("close"); }
                        if g.can_free_roll && ui.add_enabled(!busy, egui::Button::new(tr(l, "Free-Roll", "抽本留利润"))).clicked() { self.trigger_green_up("free_roll"); }
                        if g.hedge_no_price.is_some() && ui.add_enabled(!busy, egui::Button::new(tr(l, "Hedge Opp", "对冲反边"))).clicked() { self.trigger_green_up("hedge_opposite"); }
                    });
                });
                ui.add_space(8.0);
            }

            if let Some(s) = self.core.execution.preview_sweep(&aid, true, Some(self.stake_value), None, None) {
                let s: SweepQuote = s;
                pal.surface_card().show(ui, |ui| {
                    ui.label(RichText::new(format!("{} (${:.0})", tr(l, "Live Sweep Simulator", "扫盘成本预估"), self.stake_value)).strong());
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("VWAP {} · Slip {:.0}bp · {} levels", format_odds(Some(s.vwap), self.odds_fmt, 0.01), s.slippage_bps, s.levels_consumed)).monospace().small().color(pal.text_muted));
                });
                ui.add_space(8.0);
            }
        }

        // 4. CTF Split / Merge / 1-Click Redeem
        if let Some(m) = self.active_market.clone()
            && m.token_ids.len() >= 2
        {
            let is_live = self.account.mode == TradingMode::Live;
            let is_eoa = self.account.live_connected
                && self.account.signature_type == Some(WalletSignatureType::Eoa);
            let direct_ok = !is_live || is_eoa;
            let tid0 = m.token_ids[0].clone();
            let tid1 = m.token_ids[1].clone();
            let out0 = m
                .outcomes
                .first()
                .cloned()
                .unwrap_or_else(|| "Yes".to_string());
            let out1 = m
                .outcomes
                .get(1)
                .cloned()
                .unwrap_or_else(|| "No".to_string());
            let yes_held = self
                .core
                .execution
                .get_position(&tid0)
                .map(|p| p.shares)
                .unwrap_or(0.0);
            let no_held = self
                .core
                .execution
                .get_position(&tid1)
                .map(|p| p.shares)
                .unwrap_or(0.0);
            let full_pairs = yes_held.min(no_held);

            let mut ctf_click: Option<(CtfAction, f64)> = None;
            let mut ctf_bad_amt = false;
            pal.surface_card().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(tr(l, "CTF Split / Merge / Redeem", "CTF 拆分 / 合并 / 赎回"))
                            .strong()
                            .color(pal.blue),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (badge, col) = if !is_live {
                            ("PAPER", pal.amber)
                        } else if is_eoa {
                            ("LIVE · EOA", pal.green)
                        } else {
                            ("PROXY / SAFE", pal.red)
                        };
                        ui.label(RichText::new(badge).monospace().small().strong().color(col));
                    });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "{}: {:.2} · {}: {:.2} · {}: {:.2} pairs",
                        out0,
                        yes_held,
                        out1,
                        no_held,
                        tr(l, "Mergeable", "可合并"),
                        full_pairs,
                    ))
                    .monospace()
                    .small()
                    .color(pal.text_muted),
                );
                if is_live && !self.account.live_connected {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(tr(
                            l,
                            "Unlock an EOA wallet to submit on-chain CTF transactions.",
                            "请先解锁 EOA 本地钱包后再提交链上 CTF 交易。",
                        ))
                        .small()
                        .color(pal.amber),
                    );
                } else if is_live && !is_eoa {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(tr(
                            l,
                            "Direct CTF Split/Merge/Redeem is disabled for Proxy / GnosisSafe wallets (custodied on-contract).",
                            "Proxy / Safe 智能合约钱包已禁用直接链上 CTF 拆分/合并/赎回，请前往官网资产页操作：",
                        ))
                        .small()
                        .color(pal.red_text),
                    );
                    ui.hyperlink_to(
                        tr(l, "Open Polymarket Portfolio ↗", "前往 Polymarket 资产页 ↗"),
                        "https://polymarket.com/portfolio",
                    );
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(tr(l, "Amount ($ / pairs):", "数量 ($ / 对):"));
                    ui.add(TextEdit::singleline(&mut self.ctf_amount_input).desired_width(76.0));
                });
                ui.add_space(6.0);
                let busy = self.modal.is_some() || self.ctf_in_flight;
                let parsed_amt = self.ctf_amount_input.trim().parse::<f64>().ok();
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(!busy && direct_ok, egui::Button::new(tr(l, "Split ($ → YES+NO)", "拆分 ($ → 双边)")))
                        .clicked()
                    {
                        match parsed_amt.filter(|v| v.is_finite() && *v > 0.0) {
                            Some(amt) => ctf_click = Some((CtfAction::Split, amt)),
                            None => ctf_bad_amt = true,
                        }
                    }
                    if ui
                        .add_enabled(!busy && direct_ok, egui::Button::new(tr(l, "Merge (YES+NO → $)", "合并 (双边 → $)")))
                        .clicked()
                    {
                        match parsed_amt.filter(|v| v.is_finite() && *v > 0.0) {
                            Some(amt) => ctf_click = Some((CtfAction::Merge, amt)),
                            None => ctf_bad_amt = true,
                        }
                    }
                    let redeem_resp = ui.add_enabled(
                        !busy && is_live && is_eoa,
                        egui::Button::new(RichText::new(tr(l, "1-Click Redeem", "一键赎回")).color(pal.green)),
                    );
                    if !is_live {
                        redeem_resp.on_hover_text(tr(
                            l,
                            "On-chain Redeem is LIVE EOA only (use Cash Out in Paper mode)",
                            "链上赎回仅限实盘 EOA 钱包，模拟盘请使用市价锁盈",
                        ));
                    } else if redeem_resp.clicked() {
                        ctf_click = Some((CtfAction::Redeem, yes_held + no_held));
                    }
                });
            });
            if ctf_bad_amt {
                self.push_toast(
                    tr(l, "Enter a positive CTF amount", "请输入大于 0 的数量").into(),
                    true,
                );
            } else if let Some((action, amt)) = ctf_click {
                self.pending_ctf = Some(CtfRequest {
                    action,
                    condition_id: m.condition_id,
                    market_question: m.question,
                    yes_asset_id: tid0,
                    no_asset_id: tid1,
                    yes_outcome: out0,
                    no_outcome: out1,
                    amount: amt,
                    yes_shares: yes_held,
                    no_shares: no_held,
                    neg_risk: m.neg_risk,
                });
                self.last_ctf_result = None;
                self.modal = Some(ActiveModal::CtfConfirm);
            }
        }
    }

    /// 1-Click Quick Buy / Sell ticket — enter amount, inspect payout & fees, execute.
    fn draw_quick_trade_panel(&mut self, ui: &mut egui::Ui, active_top: Option<(Option<f64>, Option<f64>, Option<f64>)>) {
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
        let active_aid_owned = self.active_asset_id().map(|s| s.to_string());
        let in_flight = active_aid_owned.as_deref().is_some_and(|a| self.order_in_flight.contains(a));
        let modal_open = self.modal.is_some();

        pal.surface_card().show(ui, |ui| {
            ui.set_width(ui.available_width());

            // 1. Crisp Recessed Segmented Buy / Sell Control
            pal.pill_container().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let half_w = ((ui.available_width() - 4.0) * 0.5).max(110.0);
                    let buy_fill = if self.quick_buy {
                        ThemePalette::tint(pal.green, if pal.light { 26 } else { 36 })
                    } else {
                        Color32::TRANSPARENT
                    };
                    let buy_stroke = if self.quick_buy {
                        Stroke::new(1.0, ThemePalette::tint(pal.green, 150))
                    } else {
                        Stroke::NONE
                    };
                    let buy_col = if self.quick_buy { pal.green_text } else { pal.text_muted };
                    if ui.add_sized(
                        Vec2::new(half_w, 33.0),
                        egui::Button::new(RichText::new(tr(l, "Buy", "买入")).font(ThemePalette::semibold(13.5)).color(buy_col))
                            .fill(buy_fill)
                            .stroke(buy_stroke)
                            .corner_radius(CornerRadius::same(6)),
                    ).clicked() {
                        self.quick_buy = true;
                    }

                    let sell_fill = if !self.quick_buy {
                        ThemePalette::tint(pal.red, if pal.light { 24 } else { 34 })
                    } else {
                        Color32::TRANSPARENT
                    };
                    let sell_stroke = if !self.quick_buy {
                        Stroke::new(1.0, ThemePalette::tint(pal.red, 150))
                    } else {
                        Stroke::NONE
                    };
                    let sell_col = if !self.quick_buy { pal.red_text } else { pal.text_muted };
                    if ui.add_sized(
                        Vec2::new(half_w, 33.0),
                        egui::Button::new(RichText::new(tr(l, "Sell", "卖出")).font(ThemePalette::semibold(13.5)).color(sell_col))
                            .fill(sell_fill)
                            .stroke(sell_stroke)
                            .corner_radius(CornerRadius::same(6)),
                    ).clicked() {
                        self.quick_buy = false;
                    }
                });
            });

            ui.add_space(10.0);

            // 2. Order Type: Tight Recessed Market | Limit Toggle
            ui.horizontal(|ui| {
                let avail_w = ui.available_width();
                let seg_w = 124.0;
                ui.allocate_ui_with_layout(
                    Vec2::new((avail_w - seg_w - 6.0).max(80.0), 26.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.label(RichText::new(tr(l, "Order Type", "订单类型")).size(11.5).color(pal.text_secondary));
                    },
                );
                pal.pill_container().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if seg_item(ui, pal, !self.quick_limit_mode, tr(l, "Market", "市价成交")).clicked() {
                            self.quick_limit_mode = false;
                        }
                        if seg_item(ui, pal, self.quick_limit_mode, tr(l, "Limit", "限价单")).clicked() {
                            self.quick_limit_mode = true;
                            if let Some((_, a, mid)) = active_top
                                && let Some(px) = mid.or(a)
                            {
                                self.quick_limit_cents = (px * 100.0).round().clamp(1.0, 99.0);
                            }
                        }
                    });
                });
            });

            ui.add_space(10.0);

            // 3. Explicitly Labeled Outcome Cards (Outcome + Ask/Bid side + Bold Odds)
            let side_tag = if self.quick_limit_mode {
                tr(l, "Mid", "中价")
            } else if self.quick_buy {
                tr(l, "Ask", "卖价")
            } else {
                tr(l, "Bid", "买价")
            };
            ui.label(RichText::new(tr(l, "Outcome", "选择方向")).size(11.5).color(pal.text_secondary));
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let n = m.outcomes.len().clamp(1, 2);
                let btn_w = ((ui.available_width() - 8.0) / n as f32).max(110.0);
                for (idx, name) in m.outcomes.iter().take(2).enumerate() {
                    let tid = m.token_ids.get(idx).map(|s| s.as_str());
                    let px = tid
                        .and_then(|t| self.core.feed.get_top_of_book(t))
                        .and_then(|(b, a, mid)| {
                            if self.quick_limit_mode {
                                mid.or(a).or(b)
                            } else if self.quick_buy {
                                a.or(mid)
                            } else {
                                b.or(mid)
                            }
                        })
                        .or_else(|| m.outcome_prices.get(idx).copied());
                    let active = self.active_outcome_idx == idx;
                    let col = if idx == 0 { pal.green } else { pal.red };
                    let disp_name = self.display_text(name);
                    let max_c = if polydesk::translate::contains_cjk(&disp_name) { 6 } else { 10 };
                    let short: String = if disp_name.chars().count() > max_c {
                        format!("{}…", disp_name.chars().take(max_c - 1).collect::<String>())
                    } else {
                        disp_name.clone()
                    };

                    let (card_r, resp) = ui.allocate_exact_size(Vec2::new(btn_w, 56.0), egui::Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    let fill = if active {
                        ThemePalette::tint(col, if pal.light { 24 } else { 30 })
                    } else if resp.hovered() {
                        pal.bg_chip
                    } else {
                        pal.bg_sub
                    };
                    let p = ui.painter();
                    p.rect_filled(card_r, CornerRadius::same(9), fill);
                    if active {
                        p.rect_stroke(card_r, CornerRadius::same(9), Stroke::new(1.5, col), egui::StrokeKind::Inside);
                    }
                    p.text(
                        egui::pos2(card_r.left() + 11.0, card_r.top() + 14.0),
                        egui::Align2::LEFT_CENTER,
                        short,
                        ThemePalette::semibold(12.0),
                        if active { col } else { pal.text_secondary },
                    );
                    p.text(
                        egui::pos2(card_r.right() - 10.0, card_r.top() + 14.0),
                        egui::Align2::RIGHT_CENTER,
                        side_tag,
                        egui::FontId::monospace(9.5),
                        pal.text_muted,
                    );
                    p.text(
                        egui::pos2(card_r.left() + 11.0, card_r.bottom() - 16.0),
                        egui::Align2::LEFT_CENTER,
                        format_odds(px, self.odds_fmt, m.tick_size),
                        egui::FontId::monospace(16.0),
                        if active { pal.text_primary } else { pal.text_secondary },
                    );
                    let clicked = resp.clicked();
                    let _ = resp.on_hover_text(&disp_name);
                    if clicked && !active {
                        self.switch_active_outcome(idx, px);
                    }
                }
            });

            ui.add_space(10.0);

            // 4. Optional Limit Price Stepper
            if self.quick_limit_mode {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tr(l, "Limit Price", "限价价格")).size(11.5).color(pal.text_secondary));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let step_c = (m.tick_size * 100.0).max(0.1);
                        if ui
                            .add(egui::Button::new(" + ").fill(pal.bg_chip).stroke(Stroke::NONE).corner_radius(CornerRadius::same(5)))
                            .clicked()
                        {
                            self.quick_limit_cents = (self.quick_limit_cents + step_c).min(99.5);
                        }
                        ui.add(
                            egui::DragValue::new(&mut self.quick_limit_cents)
                                .range(0.5..=99.5)
                                .speed(0.5)
                                .suffix("¢"),
                        );
                        if ui
                            .add(egui::Button::new(" − ").fill(pal.bg_chip).stroke(Stroke::NONE).corner_radius(CornerRadius::same(5)))
                            .clicked()
                        {
                            self.quick_limit_cents = (self.quick_limit_cents - step_c).max(0.5);
                        }
                    });
                });
                ui.add_space(8.0);
            }

            // 5. Recessed Amount Input + Clickable Balance + Instant Presets
            ui.horizontal(|ui| {
                if self.quick_buy {
                    ui.label(RichText::new(tr(l, "Amount (USD)", "买入金额 ($)")).strong().size(12.0).color(pal.text_secondary));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ghost_button(
                            ui,
                            pal,
                            RichText::new(format!("{}: {}", tr(l, "Bal", "可用"), format_usd(cash, false)))
                                .monospace()
                                .small()
                                .color(pal.text_muted),
                        )
                        .clicked()
                        {
                            self.quick_amount_input = format!("{:.2}", cash.floor().max(0.0));
                        }
                    });
                } else {
                    ui.label(RichText::new(tr(l, "Shares to Sell", "卖出份数")).strong().size(12.0).color(pal.text_secondary));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ghost_button(
                            ui,
                            pal,
                            RichText::new(format!("{}: {:.2} {}", tr(l, "Owned", "持仓"), avail_shares, sh_unit))
                                .monospace()
                                .small()
                                .color(pal.text_muted),
                        )
                        .clicked()
                        {
                            self.quick_amount_input = format!("{:.2}", avail_shares);
                        }
                    });
                }
            });
            ui.add_space(5.0);

            Frame::NONE
                .fill(pal.bg_sub)
                .stroke(Stroke::new(1.0, pal.border))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(10, 7))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(
                            RichText::new(if self.quick_buy { "$" } else { "SH" })
                                .monospace()
                                .strong()
                                .size(15.5)
                                .color(pal.text_muted),
                        );
                        ui.add_sized(
                            Vec2::new((ui.available_width() - 4.0).max(120.0), 24.0),
                            TextEdit::singleline(&mut self.quick_amount_input)
                                .frame(Frame::NONE)
                                .font(egui::FontId::monospace(18.0))
                                .hint_text(if self.quick_buy { "25" } else { "10" }),
                        );
                    });
                });

            ui.add_space(7.0);

            let parsed_amt = self.quick_amount_input.trim().parse::<f64>().unwrap_or(0.0).max(0.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                if self.quick_buy {
                    let presets: Vec<f64> = if self.quick_buy_presets.is_empty() {
                        vec![1.0, 10.0, 50.0, 100.0]
                    } else {
                        self.quick_buy_presets.iter().take(4).copied().collect()
                    };
                    let btn_count = (presets.len() + 1) as f32;
                    let chip_w = ((ui.available_width() - 5.0 * (btn_count - 1.0)) / btn_count).max(46.0);
                    for add_v in presets {
                        if ui.add_sized(
                            Vec2::new(chip_w, 27.0),
                            egui::Button::new(RichText::new(format!("+${add_v:.0}")).monospace().small().color(pal.text_secondary))
                                .fill(pal.bg_sub)
                                .stroke(Stroke::NONE)
                                .corner_radius(CornerRadius::same(6)),
                        ).clicked() {
                            let next = (parsed_amt + add_v).min(100_000.0);
                            self.quick_amount_input = format!("{next:.0}");
                        }
                    }
                    if ui.add_sized(
                        Vec2::new(chip_w, 27.0),
                        egui::Button::new(RichText::new("Max").monospace().small().color(pal.blue))
                            .fill(ThemePalette::tint(pal.blue, if pal.light { 16 } else { 24 }))
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(6)),
                    ).clicked() {
                        self.quick_amount_input = format!("{:.2}", cash.floor().max(0.0));
                    }
                } else {
                    let chip_w = ((ui.available_width() - 15.0) / 4.0).max(58.0);
                    for (pct, lbl) in [(0.25, "25%"), (0.50, "50%"), (0.75, "75%"), (1.0, "100%")] {
                        if ui.add_sized(
                            Vec2::new(chip_w, 27.0),
                            egui::Button::new(RichText::new(lbl).monospace().small().color(pal.text_secondary))
                                .fill(pal.bg_sub)
                                .stroke(Stroke::NONE)
                                .corner_radius(CornerRadius::same(6)),
                        ).clicked() {
                            self.quick_amount_input = format!("{:.2}", avail_shares * pct);
                        }
                    }
                }
            });

            ui.add_space(10.0);

            // 6. Payout & Fee Breakdown Box + Book-Walk Slippage Check (Buy & Sell)
            let top_book = active_aid_owned
                .as_deref()
                .and_then(|aid| self.core.feed.get_book_view(aid, 1));
            let best_level_shares = if self.quick_buy {
                top_book.as_ref().and_then(|b| b.asks.first().map(|l| l.1))
            } else {
                top_book.as_ref().and_then(|b| b.bids.first().map(|l| l.1))
            };
            let exec_px = if self.quick_limit_mode {
                (self.quick_limit_cents / 100.0).clamp(0.005, 0.995)
            } else if self.quick_buy {
                active_top
                    .and_then(|(_, a, mid)| a.or(mid))
                    .or_else(|| m.outcome_prices.get(self.active_outcome_idx).copied())
                    .unwrap_or(0.5)
            } else {
                active_top
                    .and_then(|(b, _, mid)| b.or(mid))
                    .or_else(|| m.outcome_prices.get(self.active_outcome_idx).copied())
                    .unwrap_or(0.5)
            };

            let sweep_quote = if !self.quick_limit_mode && parsed_amt > 0.0 {
                if self.quick_buy {
                    let sweep_limit = (exec_px * 1.05).min(0.999);
                    active_aid_owned
                        .as_deref()
                        .and_then(|aid| self.core.execution.preview_sweep(aid, true, Some(parsed_amt), None, Some(sweep_limit)))
                } else {
                    let sweep_limit = (exec_px * 0.95).max(0.0001);
                    active_aid_owned
                        .as_deref()
                        .and_then(|aid| self.core.execution.preview_sweep(aid, false, None, Some(parsed_amt), Some(sweep_limit)))
                }
            } else {
                None
            };
            let (display_px, est_shares, est_usd) = if self.quick_buy {
                if let Some(ref sq) = sweep_quote
                    && sq.total_shares > 0.0
                {
                    (sq.vwap, sq.total_shares, sq.total_notional_usd.min(parsed_amt))
                } else {
                    let sh = if exec_px > 0.001 { parsed_amt / exec_px } else { 0.0 };
                    (exec_px, sh, parsed_amt)
                }
            } else if let Some(ref sq) = sweep_quote
                && sq.total_shares > 0.0
            {
                (sq.vwap, parsed_amt, sq.total_notional_usd)
            } else {
                (exec_px, parsed_amt, parsed_amt * exec_px)
            };
            let worst_case_usd = parsed_amt;
            let max_notional_usd = (self.quick_buy && !self.quick_limit_mode).then_some(parsed_amt);
            let is_maker = self.quick_limit_mode;
            let p_fee = estimate_protocol_fee_usd(est_shares, display_px, is_maker, m.fees_enabled);
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

            let thin_book_warn = sweep_quote.as_ref().is_some_and(|sq| {
                !sq.filled_completely || sq.levels_consumed > 1 || sq.slippage_bps >= self.slippage_warn_bps
            });

            Frame::NONE
                .fill(pal.bg_sub)
                .stroke(if thin_book_warn { Stroke::new(1.0, pal.amber) } else { Stroke::NONE })
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    if let Some(top_sz) = best_level_shares {
                        let exceeding = !self.quick_limit_mode && est_shares > top_sz + 0.01;
                        ui.horizontal(|ui| {
                            let lbl = if self.quick_buy {
                                tr(l, "Top Ask Depth", "卖一挂单量")
                            } else {
                                tr(l, "Top Bid Depth", "买一挂单量")
                            };
                            ui.label(RichText::new(lbl).size(11.5).color(pal.text_secondary));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(
                                    RichText::new(format!("{:.1} {} @ {}", top_sz, sh_unit, format_odds(Some(exec_px), self.odds_fmt, m.tick_size)))
                                        .monospace()
                                        .size(11.5)
                                        .color(if exceeding { pal.amber } else { pal.text_primary }),
                                );
                            });
                        });
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(tr(l, "Avg Fill Price", "预计成交均价")).size(11.5).color(pal.text_secondary));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let slip_tag = sweep_quote
                                .as_ref()
                                .filter(|sq| sq.slippage_bps >= 0.5)
                                .map(|sq| format!(" (+{:.0}bp)", sq.slippage_bps))
                                .unwrap_or_default();
                            let px_col = if sweep_quote.as_ref().is_some_and(|sq| sq.slippage_bps >= self.slippage_warn_bps) {
                                pal.amber
                            } else {
                                pal.text_primary
                            };
                            ui.label(
                                RichText::new(format!("{}{}", format_odds(Some(display_px), self.odds_fmt, m.tick_size), slip_tag))
                                    .monospace()
                                    .strong()
                                    .size(12.0)
                                    .color(px_col),
                            );
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(tr(l, "Shares", "预计份数")).size(11.5).color(pal.text_secondary));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(format!("{:.2} {}", est_shares, sh_unit)).monospace().strong().size(12.0));
                        });
                    });
                    if self.quick_buy {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(tr(l, "Max Cost", "最大花费（封顶）")).size(11.5).color(pal.text_secondary));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let slip_note = sweep_quote
                                    .as_ref()
                                    .filter(|sq| !self.quick_limit_mode && sq.slippage_bps > 0.5)
                                    .map(|sq| format!(" (+{:.0}bp)", sq.slippage_bps))
                                    .unwrap_or_default();
                                let capped_col = if slip_note.is_empty() { pal.text_primary } else { pal.amber };
                                ui.label(
                                    RichText::new(format!(
                                        "{}{}",
                                        format_usd(if self.quick_limit_mode { est_usd } else { worst_case_usd }, false),
                                        slip_note
                                    ))
                                    .monospace()
                                    .size(11.5)
                                    .color(capped_col),
                                );
                            });
                        });
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{} ({} {}bp)", tr(l, "Fees", "费率"), HARDCODED_BUILDER_LABEL, b_bps)).size(11.5).color(pal.text_secondary));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(format_usd(p_fee + b_fee, false)).monospace().size(11.5).color(pal.text_muted));
                        });
                    });
                    if let Some(ref sq) = sweep_quote
                        && thin_book_warn
                    {
                        ui.add_space(4.0);
                        let is_unfilled = !sq.filled_completely;
                        let warn_col = if is_unfilled { pal.red } else { pal.amber };
                        let warn_msg = if is_unfilled {
                            if l == Lang::Zh {
                                format!("深度不足：盘口仅可成交 {:.1} 份（{:.0}bp 滑点），将穿过 {} 档至 {}", sq.total_shares, sq.slippage_bps, sq.levels_consumed, format_odds(Some(sq.worst_price), self.odds_fmt, m.tick_size))
                            } else {
                                format!("Thin book: only {:.1} sh fillable across {} lvl(s) up to {} (+{:.0}bp)", sq.total_shares, sq.levels_consumed, format_odds(Some(sq.worst_price), self.odds_fmt, m.tick_size), sq.slippage_bps)
                            }
                        } else if l == Lang::Zh {
                            format!("滑点提醒：将扫过 {} 档盘口至 {}（+{:.0}bp，均价 {}）", sq.levels_consumed, format_odds(Some(sq.worst_price), self.odds_fmt, m.tick_size), sq.slippage_bps, format_odds(Some(sq.vwap), self.odds_fmt, m.tick_size))
                        } else {
                            format!("Price impact: walks {} levels to {} (+{:.0}bp, avg {})", sq.levels_consumed, format_odds(Some(sq.worst_price), self.odds_fmt, m.tick_size), sq.slippage_bps, format_odds(Some(sq.vwap), self.odds_fmt, m.tick_size))
                        };
                        Frame::NONE
                            .fill(ThemePalette::tint(warn_col, if pal.light { 18 } else { 24 }))
                            .stroke(Stroke::new(1.0, ThemePalette::tint(warn_col, 140)))
                            .corner_radius(CornerRadius::same(4))
                            .inner_margin(Margin::symmetric(8, 5))
                            .show(ui, |ui| {
                                ui.label(RichText::new(warn_msg).small().color(if is_unfilled { pal.red_text } else { pal.amber }));
                            });
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        let lbl = if self.quick_buy { tr(l, "To Win (if settles Yes)", "获胜预期回报") } else { tr(l, "Est. Receive", "预计到账") };
                        ui.label(RichText::new(lbl).strong().size(12.5).color(pal.text_primary));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if self.quick_buy {
                                Frame::NONE
                                    .fill(ThemePalette::tint(pal.green, if pal.light { 24 } else { 28 }))
                                    .corner_radius(CornerRadius::same(5))
                                    .inner_margin(Margin::symmetric(6, 2))
                                    .show(ui, |ui| {
                                        ui.label(
                                            RichText::new(format!("+{:.0}%", roi_pct))
                                                .monospace()
                                                .strong()
                                                .size(11.0)
                                                .color(pal.green_text),
                                        );
                                    });
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(format_usd(to_win, false))
                                        .monospace()
                                        .strong()
                                        .size(14.5)
                                        .color(pal.green_text),
                                );
                            } else {
                                ui.label(
                                    RichText::new(format_usd(to_win, false))
                                        .monospace()
                                        .strong()
                                        .size(14.5)
                                        .color(pal.text_primary),
                                );
                            }
                        });
                    });
                });

            ui.add_space(10.0);

            // 7. 1-Click Buy / Sell Action Button with guaranteed contrast
            let out_disp = self.display_text(&self.active_outcome_name());
            let action_lbl = if in_flight {
                tr(l, "Submitting...", "下单中...").to_string()
            } else if self.quick_buy {
                format!("{} {} · {}", tr(l, "Buy", "立即买入"), out_disp, format_usd(est_usd, false))
            } else {
                format!("{} {} · {:.1} {}", tr(l, "Sell", "立即卖出"), out_disp, est_shares, sh_unit)
            };
            let btn_bg = if self.quick_buy { pal.green } else { pal.red };
            let btn_txt = ThemePalette::contrast_on(btn_bg);

            let submit_resp = ui
                .add_enabled_ui(!in_flight && !modal_open, |ui| {
                    ui.add_sized(
                        Vec2::new(ui.available_width(), 42.0),
                        egui::Button::new(RichText::new(action_lbl).font(ThemePalette::semibold(14.0)).color(btn_txt))
                            .fill(btn_bg)
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(8)),
                    )
                })
                .inner;
            if submit_resp.clicked() {
                if est_shares < 0.1 {
                    self.push_toast(tr(l, "Enter a valid amount first", "请先输入有效金额或份数").into(), true);
                } else {
                    let side = if self.quick_buy { "BUY" } else { "SELL" };
                    let choice = if self.quick_limit_mode { OrderChoice::Gtc } else { OrderChoice::Sweep };
                    let confirm_req = self.confirm_before_order || (self.confirm_on_high_slippage && thin_book_warn);
                    let out_idx = self.active_outcome_idx;
                    self.submit_order_with_confirm_for_market(
                        &m,
                        out_idx,
                        side,
                        exec_px,
                        est_shares,
                        choice,
                        max_notional_usd,
                        confirm_req,
                    );
                }
            }
        });

        // Quick Cash-Out / Position Summary Card right beneath if user holds this market
        if let Some(pos) = active_pos {
            ui.add_space(8.0);
            let mk = active_top.and_then(|(b, _, mid)| b.or(mid)).unwrap_or(pos.avg_entry);
            let upnl = pos.shares * (mk - pos.avg_entry);
            let pcol = if upnl >= 0.0 { pal.green } else { pal.red };
            pal.surface_card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tr(l, "Your Position", "当前持仓")).strong().size(12.5));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format_usd(upnl, true)).monospace().strong().size(13.0).color(pcol));
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
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(
                        !in_flight && !modal_open,
                        egui::Button::new(RichText::new(tr(l, "Cash Out All", "一键全部卖出锁盈")).strong().color(pal.amber)),
                    ).clicked() {
                        self.trigger_green_up("close");
                    }
                    if ui.add_enabled(
                        !in_flight && !modal_open,
                        egui::Button::new(tr(l, "Free-Roll", "抽本留利润")),
                    ).clicked() {
                        self.trigger_green_up("free_roll");
                    }
                });
            });
        }
    }

    fn draw_multi_grid_stage(&mut self, ui: &mut egui::Ui, need_1hz_repaint: &mut bool) {
        let l = self.lang;
        let pal = self.palette();
        let modal_open = self.modal.is_some();

        if self.pinned_markets.is_empty() {
            ui.centered_and_justified(|ui| {
                pal.surface_card().show(ui, |ui| {
                    ui.set_max_width(460.0);
                    ui.label(
                        RichText::new(tr(
                            l,
                            "Multi-Market Split Grid (Up to 4 Pinned Markets)",
                            "多市场联动分屏（最多固定 4 个市场）",
                        ))
                        .strong()
                        .size(15.0)
                        .color(pal.text_primary),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(tr(
                            l,
                            "Click +G on any market in the left rail to pin it here with an independent live ladder & quick ticket, or load the 5m BTC/ETH/SOL/XRP 4-Up grid in 1 click.",
                            "点击左侧市场行右上角的 +G 按钮即可固定最多 4 个市场并排交易，或一键铺开 5 分钟 BTC/ETH/SOL/XRP 四联盘口。",
                        ))
                        .small()
                        .color(pal.text_muted),
                    );
                    ui.add_space(10.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(tr(l, "Load 5m 4-Up (BTC / ETH / SOL / XRP)", "一键加载 5m 四联 (BTC/ETH/SOL/XRP)"))
                                    .strong()
                                    .color(pal.cta_fg),
                            )
                            .fill(pal.cta_bg)
                            .stroke(Stroke::new(1.0, pal.cta_border))
                            .corner_radius(CornerRadius::same(4)),
                        )
                        .clicked()
                    {
                        self.trigger_load_5m_4up();
                    }
                });
            });
            return;
        }

        let pinned = self.pinned_markets.clone();
        let rows = if pinned.len() <= 2 { 1 } else { 2 };
        let gap = 8.0;
        let tile_w = ((ui.available_width() - gap) * 0.5).max(320.0);
        let tile_h = ((ui.available_height() - gap * (rows as f32 - 1.0)) / rows as f32).max(290.0);
        let mode_rules: Vec<AutomationRule> = self
            .cached_rules
            .iter()
            .filter(|r| r.mode == self.account.mode)
            .cloned()
            .collect();

        let mut expand_req: Option<(MarketSummary, usize)> = None;
        let mut unpin_cid: Option<String> = None;
        let mut order_req: Option<(MarketSummary, usize, &'static str, f64, f64, OrderChoice, Option<f64>)> = None;
        let mut cancel_req: Option<(String, &'static str, f64)> = None;

        ui.spacing_mut().item_spacing = Vec2::new(gap, gap);
        for row_chunk in pinned.chunks(2) {
            ui.horizontal(|ui| {
                for m in row_chunk {
                    let cid = m.condition_id.clone();
                    let out_idx = *self.grid_outcome_idx.get(&cid).unwrap_or(&0);
                    let aid_opt = m
                        .token_ids
                        .get(out_idx)
                        .or_else(|| m.token_ids.first())
                        .cloned();
                    let active_book = aid_opt.as_deref().and_then(|a| self.core.feed.get_book_view(a, 60));
                    let top = active_book
                        .as_ref()
                        .map(|b| (b.best_bid, b.best_ask, b.mid))
                        .or_else(|| aid_opt.as_deref().and_then(|a| self.core.feed.get_top_of_book(a)));
                    let in_flight = aid_opt
                        .as_deref()
                        .is_some_and(|a| self.order_in_flight.contains(a));

                    if extract_short_timeframe_info(m).is_some() {
                        *need_1hz_repaint = true;
                        self.maybe_fetch_open_price(m);
                    }

                    ui.allocate_ui_with_layout(
                        Vec2::new(tile_w, tile_h),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            pal.surface_card().show(ui, |ui| {
                                ui.set_width(tile_w - 22.0);
                                ui.set_height(tile_h - 22.0);

                                // Header: Title + Expand + Unpin
                                ui.horizontal(|ui| {
                                    let q_disp = display_text_with(l, &self.core.translator, &m.question);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&q_disp)
                                                .strong()
                                                .size(12.5)
                                                .color(pal.text_primary),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(&m.question);
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.small_button("×").on_hover_text(tr(l, "Unpin", "取消固定")).clicked() {
                                            unpin_cid = Some(cid.clone());
                                        }
                                        if ui.small_button("↗").on_hover_text(tr(l, "Expand to Single view", "放大至单屏")).clicked() {
                                            expand_req = Some((m.clone(), out_idx));
                                        }
                                    });
                                });

                                // Outcome pills + live odds
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    for (idx, oname) in m.outcomes.iter().take(2).enumerate() {
                                        let tid = m.token_ids.get(idx).map(|s| s.as_str());
                                        let opx = tid
                                            .and_then(|t| self.core.feed.get_top_of_book(t))
                                            .and_then(|(_, a, mid)| mid.or(a))
                                            .or_else(|| m.outcome_prices.get(idx).copied());
                                        let col = if idx == 0 { pal.green } else { pal.red };
                                        let selected = out_idx == idx;
                                        let disp = display_text_with(l, &self.core.translator, oname);
                                        let lbl = format!("{} {}", disp, format_odds(opx, self.odds_fmt, m.tick_size));
                                        if ui
                                            .add(
                                                egui::Button::new(RichText::new(lbl).monospace().small().strong().color(col))
                                                    .fill(if selected {
                                                        ThemePalette::tint(col, if pal.light { 24 } else { 32 })
                                                    } else {
                                                        pal.bg_sub
                                                    })
                                                    .stroke(Stroke::new(1.0, if selected { col } else { pal.border }))
                                                    .corner_radius(CornerRadius::same(3)),
                                            )
                                            .clicked()
                                        {
                                            self.grid_outcome_idx.insert(cid.clone(), idx);
                                        }
                                    }
                                });

                                // Compact Oracle readout for 5m / 15m tiles
                                if let Some(info) = extract_short_timeframe_info(m) {
                                    let beat = self.crypto_open_prices.get(&cid).copied();
                                    let snap = self.core.oracle.snapshot(&info.chainlink_symbol);
                                    let rem = market_countdown_secs(m, info.slot_ts, info.interval_mins);
                                    ui.horizontal_wrapped(|ui| {
                                        ui.spacing_mut().item_spacing.x = 5.0;
                                        let b_txt = beat.map(format_oracle_usd).unwrap_or_else(|| "—".into());
                                        ui.label(
                                            RichText::new(format!("Beat {b_txt}"))
                                                .monospace()
                                                .small()
                                                .color(pal.amber),
                                        );
                                        if snap.as_ref().is_none_or(|s| s.stale) {
                                            ui.label(
                                                RichText::new(tr(l, "STALE / RECONNECTING", "中断 / 重连中"))
                                                    .monospace()
                                                    .small()
                                                    .color(pal.text_muted),
                                            );
                                        } else if let Some(ref s) = snap {
                                            let col = match beat {
                                                Some(b) if s.twap_60s >= b => pal.green,
                                                Some(_) => pal.red,
                                                None => pal.text_primary,
                                            };
                                            // Coin spot next to the Beat so 5m/15m tiles lead with the
                                            // underlying price instead of only the share odds.
                                            ui.label(
                                                RichText::new(format_oracle_usd(s.spot))
                                                    .monospace()
                                                    .small()
                                                    .strong()
                                                    .color(pal.text_primary),
                                            );
                                            ui.label(
                                                RichText::new(format!("TWAP {}", format_oracle_usd(s.twap_60s)))
                                                    .monospace()
                                                    .small()
                                                    .strong()
                                                    .color(col),
                                            );
                                        }
                                        if let Some(sec) = rem {
                                            let c_col = if sec <= 30 { pal.red } else { pal.text_muted };
                                            ui.label(
                                                RichText::new(format!("· {:02}:{:02}", sec / 60, sec % 60))
                                                    .monospace()
                                                    .small()
                                                    .strong()
                                                    .color(c_col),
                                            );
                                        }
                                    });
                                }

                                // Compact Quick Ticket Strip (protected by order_in_flight & max_notional_usd)
                                let stake_str = self
                                    .grid_stake_inputs
                                    .entry(cid.clone())
                                    .or_insert_with(|| "25".to_string());
                                let mut usd_amt = stake_str.trim().parse::<f64>().unwrap_or(25.0).max(1.0);
                                let best_ask = top
                                    .and_then(|(_, a, mid)| a.or(mid))
                                    .or_else(|| m.outcome_prices.get(out_idx).copied())
                                    .unwrap_or(0.5);
                                let best_bid = top
                                    .and_then(|(b, _, mid)| b.or(mid))
                                    .or_else(|| m.outcome_prices.get(out_idx).copied())
                                    .unwrap_or(0.5);
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    ui.label(RichText::new("$").monospace().small().color(pal.text_muted));
                                    ui.add(TextEdit::singleline(stake_str).desired_width(42.0).font(egui::FontId::monospace(11.5)));
                                    usd_amt = stake_str.trim().parse::<f64>().unwrap_or(usd_amt).max(1.0);
                                    for v in [10.0, 25.0, 50.0] {
                                        if ui.small_button(format!("${v:.0}")).clicked() {
                                            *stake_str = format!("{v:.0}");
                                            usd_amt = v;
                                        }
                                    }
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        let buy_lbl = if in_flight { "…" } else { tr(l, "Buy", "买") };
                                        let sell_lbl = if in_flight { "…" } else { tr(l, "Sell", "卖") };
                                        let sell_shares = (usd_amt / best_bid.max(0.01)).max(1.0);
                                        if ui
                                            .add_enabled(
                                                !modal_open && !in_flight,
                                                egui::Button::new(
                                                    RichText::new(sell_lbl)
                                                        .small()
                                                        .strong()
                                                        .color(ThemePalette::contrast_on(pal.red)),
                                                )
                                                .fill(pal.red),
                                            )
                                            .clicked()
                                        {
                                            order_req = Some((
                                                m.clone(),
                                                out_idx,
                                                "SELL",
                                                best_bid,
                                                sell_shares,
                                                OrderChoice::Fak,
                                                None,
                                            ));
                                        }
                                        let buy_shares = (usd_amt / best_ask.max(0.01)).max(1.0);
                                        if ui
                                            .add_enabled(
                                                !modal_open && !in_flight,
                                                egui::Button::new(
                                                    RichText::new(buy_lbl)
                                                        .small()
                                                        .strong()
                                                        .color(ThemePalette::contrast_on(pal.green)),
                                                )
                                                .fill(pal.green),
                                            )
                                            .clicked()
                                        {
                                            order_req = Some((
                                                m.clone(),
                                                out_idx,
                                                "BUY",
                                                best_ask,
                                                buy_shares,
                                                OrderChoice::Sweep,
                                                Some(usd_amt),
                                            ));
                                        }
                                    });
                                });

                                ui.add_space(4.0);

                                // Compact GPU 1-Click Price Ladder inside remaining tile height
                                let ladder_state = self.grid_ladders.entry(cid.clone()).or_insert_with(|| {
                                    let mut ls = LadderState::default();
                                    ls.reset_for_market(m.tick_size.max(0.001));
                                    ls
                                });
                                let open_orders = open_orders_for(&self.account);
                                let active_pos = active_position_for(&self.account, aid_opt.as_deref());
                                if let Some(act) = draw_price_ladder(
                                    ui,
                                    l,
                                    pal,
                                    ladder_state,
                                    self.odds_fmt,
                                    aid_opt.as_deref(),
                                    active_book.as_ref(),
                                    open_orders,
                                    active_pos,
                                    &mode_rules,
                                    !modal_open,
                                ) {
                                    match act {
                                        LadderAction::PlaceOrder { side, price, post_only_override } => {
                                            let choice = if post_only_override {
                                                OrderChoice::PostOnly
                                            } else {
                                                self.order_choice
                                            };
                                            let shares = if side == "BUY" {
                                                (usd_amt / price.max(0.005)).max(1.0)
                                            } else {
                                                usd_amt.max(1.0)
                                            };
                                            let cap = (choice == OrderChoice::Sweep && side == "BUY").then_some(usd_amt);
                                            order_req = Some((m.clone(), out_idx, side, price, shares, choice, cap));
                                        }
                                        LadderAction::CancelLevel { side, price } => {
                                            if let Some(aid) = aid_opt {
                                                cancel_req = Some((aid, side, price));
                                            }
                                        }
                                    }
                                }
                            });
                        },
                    );
                }
            });
        }

        if let Some(cid) = unpin_cid {
            self.unpin_market(&cid);
        }
        if let Some((m, idx)) = expand_req {
            self.select_market(m, idx);
            self.layout_mode = LayoutMode::Single;
        }
        if let Some((m, out_idx, side, px, shares, choice, cap)) = order_req {
            let need_confirm = confirm_required(self.confirm_ladder_orders, self.confirm_before_order);
            self.submit_order_with_confirm_for_market(&m, out_idx, side, px, shares, choice, cap, need_confirm);
        }
        if let Some((aid, side, price)) = cancel_req {
            let core = self.core.clone();
            self.rt.spawn(async move {
                let _ = core.execution.cancel_at_price(&aid, Some(side), price).await;
            });
        }
    }
}

fn draw_line_key_chip(
    ui: &mut egui::Ui,
    pal: ThemePalette,
    color: Color32,
    dashed: bool,
    label: &str,
    tooltip: &str,
    toggle: Option<bool>,
) -> bool {
    let enabled = toggle.unwrap_or(true);
    let draw_col = if enabled { color } else { pal.text_muted };
    let text_col = if enabled { pal.text_secondary } else { pal.text_muted };
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(11.0),
        text_col,
    );
    let w = 13.0 + 5.0 + galley.size().x + 14.0;
    let sense = if toggle.is_some() {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 20.0), sense);
    let bg = if resp.hovered() && toggle.is_some() {
        ThemePalette::tint(color, if pal.light { 24 } else { 32 })
    } else if enabled && toggle == Some(true) {
        ThemePalette::tint(color, if pal.light { 16 } else { 22 })
    } else {
        Color32::TRANSPARENT
    };
    let p = ui.painter();
    if bg != Color32::TRANSPARENT {
        p.rect_filled(rect, CornerRadius::same(99), bg);
    }
    let sy = rect.center().y;
    let sx0 = rect.left() + 7.0;
    if dashed {
        p.line_segment([egui::pos2(sx0, sy), egui::pos2(sx0 + 5.0, sy)], Stroke::new(2.0, draw_col));
        p.line_segment([egui::pos2(sx0 + 8.0, sy), egui::pos2(sx0 + 13.0, sy)], Stroke::new(2.0, draw_col));
    } else {
        p.line_segment([egui::pos2(sx0, sy), egui::pos2(sx0 + 13.0, sy)], Stroke::new(2.2, draw_col));
    }
    p.galley(
        egui::pos2(sx0 + 17.0, rect.center().y - galley.size().y * 0.5),
        galley,
        text_col,
    );
    let clicked = resp.clicked();
    let _ = resp.on_hover_text(tooltip);
    clicked
}

fn display_text_with(lang: Lang, translator: &Translator, en: &str) -> String {
    if lang == Lang::Zh {
        // Glossary-first: outcome labels must never be shadowed by a cached/hot-patch
        // translation, or a market's two sides could be rendered swapped.
        translator.get_zh_outcome(en).unwrap_or_else(|| en.to_string())
    } else {
        en.to_string()
    }
}

fn open_orders_for(account: &AccountState) -> &[PaperOrder] {
    if account.mode == TradingMode::Live {
        &account.live_open_orders
    } else {
        &account.paper.open_orders
    }
}

fn positions_for(account: &AccountState) -> &[PaperPosition] {
    if account.mode == TradingMode::Live {
        &account.live_positions
    } else {
        &account.paper.positions
    }
}

fn active_position_for<'a>(account: &'a AccountState, aid: Option<&str>) -> Option<&'a PaperPosition> {
    let aid = aid?;
    positions_for(account)
        .iter()
        .find(|p| p.asset_id == aid && p.shares > 1e-6)
}

fn v_hairline(ui: &mut egui::Ui, pal: ThemePalette, height: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, height), egui::Sense::hover());
    ui.painter().line_segment(
        [egui::pos2(rect.center().x, rect.top()), egui::pos2(rect.center().x, rect.bottom())],
        Stroke::new(1.0, pal.border),
    );
}

fn underline_tab(ui: &mut egui::Ui, pal: ThemePalette, selected: bool, label: &str) -> egui::Response {
    let font = if selected {
        ThemePalette::semibold(12.5)
    } else {
        egui::FontId::proportional(12.5)
    };
    let job = egui::text::LayoutJob::single_section(
        label.to_string(),
        egui::TextFormat {
            font_id: font,
            color: Color32::PLACEHOLDER,
            ..Default::default()
        },
    );
    let galley = ui.painter().layout_job(job);
    let pad_x = 9.0;
    let tab_h = 28.0;
    let desired = Vec2::new(galley.size().x + pad_x * 2.0, tab_h);
    let (rect, resp) = ui.allocate_exact_size(desired, egui::Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let text_col = if selected {
        pal.text_primary
    } else if resp.hovered() {
        pal.text_secondary
    } else {
        pal.text_muted
    };
    let text_pos = egui::pos2(
        rect.left() + pad_x,
        rect.center().y - galley.size().y * 0.5 - 1.0,
    );
    ui.painter().galley(text_pos, galley, text_col);
    if selected {
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left() + 4.0, rect.bottom() - 2.5),
                egui::pos2(rect.right() - 4.0, rect.bottom()),
            ),
            CornerRadius::same(2),
            pal.blue,
        );
    }
    resp
}

fn install_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "ibm_plex_sans".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../assets/fonts/IBMPlexSans-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        "ibm_plex_sans_sb".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../assets/fonts/IBMPlexSans-SemiBold.ttf"
        ))),
    );
    fonts.font_data.insert(
        "ibm_plex_mono".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../assets/fonts/IBMPlexMono-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        "ibm_plex_mono_med".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../assets/fonts/IBMPlexMono-Medium.ttf"
        ))),
    );

    let sb_extra: Vec<String> = {
        let prop = fonts.families.entry(FontFamily::Proportional).or_default();
        prop.insert(0, "ibm_plex_sans".into());
        prop.iter().skip(1).cloned().collect()
    };

    let mono = fonts.families.entry(FontFamily::Monospace).or_default();
    mono.insert(0, "ibm_plex_mono_med".into());
    mono.insert(1, "ibm_plex_mono".into());

    let mut sb_list = vec!["ibm_plex_sans_sb".into(), "ibm_plex_sans".into()];
    sb_list.extend(sb_extra);
    fonts
        .families
        .insert(FontFamily::Name("semibold".into()), sb_list);

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
            let mut prop_fd = FontData::from_owned(bytes.clone());
            prop_fd.index = prop_idx;
            fonts.font_data.insert("cjk_sc".into(), Arc::new(prop_fd));
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push("cjk_sc".into());
            fonts
                .families
                .entry(FontFamily::Name("semibold".into()))
                .or_default()
                .push("cjk_sc".into());
            if mono_idx != prop_idx {
                let mut mono_fd = FontData::from_owned(bytes);
                mono_fd.index = mono_idx;
                fonts.font_data.insert("cjk_sc_mono".into(), Arc::new(mono_fd));
                fonts
                    .families
                    .entry(FontFamily::Monospace)
                    .or_default()
                    .push("cjk_sc_mono".into());
            } else {
                fonts
                    .families
                    .entry(FontFamily::Monospace)
                    .or_default()
                    .push("cjk_sc".into());
            }
            break;
        }
    }

    ctx.set_fonts(fonts);
}

/// A per-order confirmation is required if *either* the global setting or the
/// surface-specific setting asks for it. The global switch must never be weaker than
/// a per-surface switch, otherwise "confirm every order" silently doesn't.
fn confirm_required(global_confirm: bool, surface_confirm: bool) -> bool {
    global_confirm || surface_confirm
}

/// How long a captured order preview may be trusted before it must be re-quoted.
pub(crate) const QUOTE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(8);

/// True when the confirmation modal's captured quote is no longer safe to execute:
/// either it is older than [`QUOTE_MAX_AGE`], or the touch has moved by at least half a
/// tick (prices are tick-aligned, so any real move is >= 1 tick; the half-tick
/// threshold only absorbs float noise such as `0.43 - 0.42 == 0.0099999…`).
#[allow(dead_code)]
fn quote_is_stale(
    age: std::time::Duration,
    tick: f64,
    current_px: Option<f64>,
    prev_px: Option<f64>,
) -> bool {
    if age > QUOTE_MAX_AGE {
        return true;
    }
    match (current_px, prev_px) {
        (Some(cur), Some(prev)) => (cur - prev).abs() > tick.max(0.0001) * 0.5 + 1e-9,
        _ => false,
    }
}

/// Modal execution re-quote policy:
/// - Limit (`GTC` / `PostOnly`) orders stay at the user's explicit limit price and never block.
/// - Market / sweep / FAK / FOK orders re-quote only after [`QUOTE_MAX_AGE`] (8 s) OR if the
///   touch moved *adversely* by >= 2 ticks (1-tick 5m noise or favorable improvement proceeds).
fn should_requote_pending_order(
    is_limit: bool,
    is_buy: bool,
    age: std::time::Duration,
    tick: f64,
    current_px: Option<f64>,
    prev_px: Option<f64>,
) -> bool {
    if is_limit {
        return false;
    }
    if age > QUOTE_MAX_AGE {
        return true;
    }
    match (current_px, prev_px) {
        (Some(cur), Some(prev)) => {
            let adverse = if is_buy { cur - prev } else { prev - cur };
            adverse > tick.max(0.0001) * 1.5 + 1e-9
        }
        _ => false,
    }
}

/// Clamp a BUY size so `size * price` never exceeds the reviewed notional cap.
/// Sizes are quoted to 0.01 share precision; a cap smaller than one tick-lot leaves
/// the size untouched (the caller rejects `< 0.01` itself).
fn clamp_buy_notional(size: f64, price: f64, max_notional_usd: Option<f64>) -> f64 {
    let Some(cap) = max_notional_usd.filter(|c| c.is_finite() && *c > 0.0) else {
        return size;
    };
    if price <= 0.0 {
        return size;
    }
    let max_size = ((cap / price) * 100.0).floor() / 100.0;
    if max_size >= 0.01 && size > max_size {
        max_size
    } else {
        size
    }
}

fn begin_order_in_flight(
    in_flight: &mut std::collections::HashSet<String>,
    since: &mut std::collections::HashMap<String, Instant>,
    asset_id: &str,
    now: Instant,
) -> bool {
    if in_flight.contains(asset_id) {
        return false;
    }
    in_flight.insert(asset_id.to_string());
    since.insert(asset_id.to_string(), now);
    true
}

fn end_order_in_flight(
    in_flight: &mut std::collections::HashSet<String>,
    since: &mut std::collections::HashMap<String, Instant>,
    asset_id: &str,
) {
    in_flight.remove(asset_id);
    since.remove(asset_id);
}

fn prune_stale_in_flight(
    in_flight: &mut std::collections::HashSet<String>,
    since: &mut std::collections::HashMap<String, Instant>,
    now: Instant,
    max_age: Duration,
) {
    let stale: Vec<String> = since
        .iter()
        .filter_map(|(aid, started)| {
            (now.saturating_duration_since(*started) >= max_age).then(|| aid.clone())
        })
        .collect();
    for aid in stale {
        in_flight.remove(&aid);
        since.remove(&aid);
    }
}

fn insert_pinned_market(pinned: &mut Vec<MarketSummary>, market: MarketSummary) -> bool {
    if pinned.iter().any(|m| m.condition_id == market.condition_id) {
        return true;
    }
    if pinned.len() >= 4 {
        return false;
    }
    pinned.push(market);
    true
}

fn market_countdown_secs(
    m: &MarketSummary,
    slot_ts: Option<i64>,
    interval_mins: u32,
) -> Option<i64> {
    polydesk::market::short_timeframe_countdown_secs(
        m.end_date.as_deref(),
        slot_ts,
        interval_mins,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_grid_market(id: &str) -> MarketSummary {
        MarketSummary {
            id: id.into(),
            question: format!("Market {id}"),
            question_zh: None,
            slug: format!("btc-updown-5m-{id}"),
            event_slug: format!("btc-updown-5m-{id}"),
            event_title: format!("Market {id}"),
            condition_id: format!("cond-{id}"),
            outcomes: vec!["Up".into(), "Down".into()],
            outcomes_zh: Vec::new(),
            token_ids: vec![format!("yes-{id}"), format!("no-{id}")],
            outcome_prices: vec![0.5, 0.5],
            tick_size: 0.01,
            min_order_size: 5.0,
            volume_24hr: 0.0,
            liquidity: 0.0,
            best_bid: None,
            best_ask: None,
            last_trade_price: None,
            one_day_change: None,
            neg_risk: false,
            fees_enabled: true,
            accepting_orders: true,
            event_start_time: None,
            end_date: None,
            image: None,
            category: "crypto".into(),
        }
    }

    #[test]
    fn order_in_flight_prevents_duplicates_and_unlocks_on_finish_or_timeout() {
        let mut in_flight = std::collections::HashSet::new();
        let mut since = std::collections::HashMap::new();
        let t0 = Instant::now();

        assert!(begin_order_in_flight(&mut in_flight, &mut since, "asset-a", t0));
        // Duplicate submit on the same asset while in-flight is rejected.
        assert!(!begin_order_in_flight(
            &mut in_flight,
            &mut since,
            "asset-a",
            t0 + Duration::from_millis(50)
        ));
        // Independent asset can still submit concurrently.
        assert!(begin_order_in_flight(
            &mut in_flight,
            &mut since,
            "asset-b",
            t0 + Duration::from_secs(10)
        ));

        // Explicit completion unlocks the asset.
        end_order_in_flight(&mut in_flight, &mut since, "asset-a");
        assert!(begin_order_in_flight(
            &mut in_flight,
            &mut since,
            "asset-a",
            t0 + Duration::from_secs(11)
        ));

        // Timeout (15s) clears asset-b (started at t0 + 10s, pruned at t0 + 25s) while keeping asset-a (started at t0 + 11s).
        prune_stale_in_flight(
            &mut in_flight,
            &mut since,
            t0 + Duration::from_secs(25),
            Duration::from_secs(15),
        );
        assert!(!in_flight.contains("asset-b"));
        assert!(!since.contains_key("asset-b"));
        assert!(in_flight.contains("asset-a"));
        assert!(begin_order_in_flight(
            &mut in_flight,
            &mut since,
            "asset-b",
            t0 + Duration::from_secs(25)
        ));
    }

    // ---- §4.13/§4.14/§4.15: confirmation-gate, staleness and notional invariants ----

    #[test]
    fn global_confirm_setting_always_wins_over_per_surface_setting() {
        // Global on, ladder off -> must still confirm (this was C3/M3).
        assert!(confirm_required(true, false));
        assert!(confirm_required(false, true));
        assert!(confirm_required(true, true));
        // Only when BOTH are off may an order be submitted without a dialog.
        assert!(!confirm_required(false, false));
    }

    #[test]
    fn order_quote_is_stale_after_timeout_or_one_tick_move() {
        let tick = 0.01;
        // Fresh quote, no movement.
        assert!(!quote_is_stale(
            Duration::from_millis(100),
            tick,
            Some(0.42),
            Some(0.42)
        ));
        // Same price, but the modal sat open past the max age.
        assert!(quote_is_stale(
            QUOTE_MAX_AGE + Duration::from_millis(1),
            tick,
            Some(0.42),
            Some(0.42)
        ));
        // Book moved by a full tick in either direction.
        assert!(quote_is_stale(
            Duration::from_millis(50),
            tick,
            Some(0.43),
            Some(0.42)
        ));
        assert!(quote_is_stale(
            Duration::from_millis(50),
            tick,
            Some(0.41),
            Some(0.42)
        ));
        // Sub-tick noise does not trip it (would make the dialog unusable).
        assert!(!quote_is_stale(
            Duration::from_millis(50),
            tick,
            Some(0.4235),
            Some(0.42)
        ));
        // Missing data on either side is not treated as stale.
        assert!(!quote_is_stale(Duration::ZERO, tick, None, Some(0.42)));
        assert!(!quote_is_stale(Duration::ZERO, tick, Some(0.42), None));
    }

    #[test]
    fn pending_order_requote_skips_limits_and_favorable_moves() {
        let tick = 0.01;
        // Limit orders keep the user's explicit price even after timeout or touch move.
        assert!(!should_requote_pending_order(
            true,
            true,
            QUOTE_MAX_AGE + Duration::from_secs(30),
            tick,
            Some(0.60),
            Some(0.42)
        ));
        // Favorable move for BUY (price dropped) or SELL (price rose) never blocks confirmation.
        assert!(!should_requote_pending_order(false, true, Duration::from_secs(2), tick, Some(0.40), Some(0.42)));
        assert!(!should_requote_pending_order(false, false, Duration::from_secs(2), tick, Some(0.44), Some(0.42)));
        // 1-tick micro-noise on 5m markets does not block; >= 2 ticks adverse triggers re-quote.
        assert!(!should_requote_pending_order(false, true, Duration::from_secs(2), tick, Some(0.43), Some(0.42)));
        assert!(should_requote_pending_order(false, true, Duration::from_secs(2), tick, Some(0.44), Some(0.42)));
        assert!(should_requote_pending_order(false, false, Duration::from_secs(2), tick, Some(0.40), Some(0.42)));
    }

    #[test]
    fn submitted_buy_notional_never_exceeds_the_reviewed_cap() {
        // 200 shares @ 0.52 = $104 against a $100 cap -> clamp to 192.30 shares.
        let size = clamp_buy_notional(200.0, 0.52, Some(100.0));
        assert!(size * 0.52 <= 100.0 + 1e-9, "notional {} exceeded cap", size * 0.52);
        assert_eq!(size, 192.30);

        // Under the cap: untouched.
        assert_eq!(clamp_buy_notional(10.0, 0.52, Some(100.0)), 10.0);

        // No cap / nonsensical cap: untouched.
        assert_eq!(clamp_buy_notional(10.0, 0.52, None), 10.0);
        assert_eq!(clamp_buy_notional(10.0, 0.52, Some(f64::NAN)), 10.0);
        assert_eq!(clamp_buy_notional(10.0, 0.52, Some(0.0)), 10.0);

        // Cap smaller than one 0.01-share lot leaves the size for the caller to reject.
        assert_eq!(clamp_buy_notional(10.0, 0.52, Some(0.001)), 10.0);

        // Property check across a grid: never exceeds the cap, never grows the order.
        for cap in [1.0f64, 5.0, 25.0, 100.0, 1_000.0] {
            for px in [0.01f64, 0.05, 0.25, 0.50, 0.95, 0.99] {
                for sh in [0.01f64, 1.0, 37.0, 250.0, 5_000.0] {
                    let out = clamp_buy_notional(sh, px, Some(cap));
                    assert!(out <= sh + 1e-9);
                    assert!(out * px <= cap + 1e-9, "cap={cap} px={px} sh={sh} -> {out}");
                }
            }
        }
    }

    #[tokio::test]
    async fn multi_grid_caps_at_four_and_retains_pinned_books_under_eviction() {
        let mut pinned = Vec::new();
        for i in 1..=4 {
            assert!(insert_pinned_market(&mut pinned, stub_grid_market(&i.to_string())));
        }
        // Duplicate pin succeeds idempotently without growing past 4
        assert!(insert_pinned_market(&mut pinned, stub_grid_market("2")));
        assert_eq!(pinned.len(), 4);
        // 5th distinct market rejected
        assert!(!insert_pinned_market(&mut pinned, stub_grid_market("5")));
        assert_eq!(pinned.len(), 4);

        let telem = Arc::new(polydesk::telemetry::Telemetry::new());
        let feed = polydesk::ws_feed::MarketFeed::spawn("wss://invalid.local/ws".into(), telem, 500.0);
        let mut pin_set = HashSet::new();
        for m in &pinned {
            for tid in &m.token_ids {
                pin_set.insert(tid.clone());
                feed.seed_book(tid, &m.condition_id, 0.01, &[(0.49, 100.0)], &[(0.51, 100.0)], None, 1000, Some("h".into()));
            }
        }
        feed.set_pinned_assets(pin_set.clone());

        for k in 0..80 {
            let aid = format!("unpinned-{k}");
            feed.seed_book(&aid, "cond-other", 0.01, &[(0.45, 50.0)], &[(0.55, 50.0)], None, 2000, Some("h".into()));
        }
        feed.evict_inactive_books(&pin_set, 16);

        for m in &pinned {
            for tid in &m.token_ids {
                assert!(feed.get_top_of_book(tid).is_some(), "pinned book {tid} was evicted");
            }
        }
    }
}
