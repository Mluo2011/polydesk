use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{info, warn};
use uuid::Uuid;

use crate::execution::{ExecEvent, ExecutionEngine, OrderRequest, TradingMode, align_to_tick};
use crate::market::MarketClient;
use crate::ws_feed::{FeedEvent, MarketFeed};

const MAX_RULE_RETRIES: u32 = 5;

fn retry_backoff_ms(retry_count: u32) -> i64 {
    (1000_i64 << retry_count.min(4)).min(30_000)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    StopLoss,
    TakeProfit,
    TrailingStop,
    DripFeed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRuleRequest {
    pub kind: RuleKind,
    pub asset_id: String,
    pub condition_id: String,
    pub market_question: String,
    pub outcome: String,
    pub side: String,          // "BUY" | "SELL"
    pub size: f64,             // total shares
    pub trigger_price: f64,    // for SL/TP, or limit cap for DripFeed
    pub trail_offset: Option<f64>,   // for TrailingStop (e.g., 0.03 = 3 cents)
    pub interval_ms: Option<u64>,    // for DripFeed
    pub slices: Option<u32>,         // for DripFeed
    pub max_slippage_bps: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationRule {
    pub id: String,
    #[serde(default)]
    pub mode: TradingMode,
    pub kind: RuleKind,
    pub asset_id: String,
    pub condition_id: String,
    pub market_question: String,
    pub outcome: String,
    pub side: String,
    pub total_size: f64,
    pub remaining_size: f64,
    pub trigger_price: f64,
    pub trail_offset: Option<f64>,
    pub peak_price: Option<f64>,
    pub interval_ms: Option<u64>,
    pub slices_total: Option<u32>,
    pub slices_done: u32,
    pub max_slippage_bps: f64,
    pub active: bool,
    #[serde(default)]
    pub in_flight: bool,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(default)]
    pub next_retry_ms: Option<i64>,
    pub status: String,
    pub created_at_ms: i64,
    pub last_fired_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyTarget {
    pub id: String,
    #[serde(default)]
    pub mode: TradingMode,
    pub wallet: String,
    pub label: String,
    pub multiplier: f64,      // e.g. 0.1 = copy 10% of whale share size
    pub max_order_usd: f64,   // hard cap per copied trade
    pub max_slippage_bps: f64,
    pub enabled: bool,
    pub copied_count: u32,
    #[serde(default)]
    pub failed_count: u32,
    #[serde(default)]
    pub last_error: Option<String>,
    pub last_seen_ts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCopyTargetRequest {
    pub wallet: String,
    pub label: String,
    pub multiplier: f64,
    pub max_order_usd: f64,
    pub max_slippage_bps: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutoEvent {
    RulesUpdated,
    RuleTriggered { rule_id: String, message: String, is_err: bool },
    CopyExecuted { wallet: String, message: String, is_err: bool },
}

pub struct AutomationEngine {
    rules: Mutex<HashMap<String, AutomationRule>>,
    copy_targets: Mutex<HashMap<String, CopyTarget>>,
    seen_copy_keys: Mutex<HashSet<String>>,
    execution: Arc<ExecutionEngine>,
    feed: Arc<MarketFeed>,
    market_client: Arc<MarketClient>,
    events_tx: broadcast::Sender<AutoEvent>,
}

impl AutomationEngine {
    pub fn spawn(
        execution: Arc<ExecutionEngine>,
        feed: Arc<MarketFeed>,
        market_client: Arc<MarketClient>,
    ) -> Arc<Self> {
        let (events_tx, _) = broadcast::channel(256);
        let engine = Arc::new(Self {
            rules: Mutex::new(HashMap::new()),
            copy_targets: Mutex::new(HashMap::new()),
            seen_copy_keys: Mutex::new(HashSet::new()),
            execution,
            feed,
            market_client,
            events_tx,
        });

        // 1. Sub-millisecond WS tick trigger evaluator (ZERO UI round-trip!)
        let e1 = engine.clone();
        tokio::spawn(async move {
            let mut rx = e1.feed.subscribe_events();
            loop {
                match rx.recv().await {
                    Ok(FeedEvent::Book { view }) => {
                        e1.evaluate_book_tick(
                            &view.asset_id,
                            view.best_bid,
                            view.best_ask,
                            view.last_trade_price,
                        );
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                    _ => {}
                }
            }
        });

        // 2. Drip-Feed / TWAP scheduler loop (250ms resolution)
        let e2 = engine.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                e2.evaluate_drip_feeds();
            }
        });

        // 3. Smart-Money / Whale Copy-Trade watcher loop (polls Data API v2 every 3s for active targets)
        let e3 = engine.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(3));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                e3.poll_copy_targets().await;
            }
        });

        // 4. Mode-isolation guard: pause mismatched rules & copy targets on Paper <-> Live switch
        let e4 = engine.clone();
        tokio::spawn(async move {
            let mut rx = e4.execution.subscribe_events();
            loop {
                match rx.recv().await {
                    Ok(ExecEvent::ModeChanged { mode }) => {
                        e4.pause_mismatched_mode(mode);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        e4.pause_mismatched_mode(e4.execution.mode());
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    _ => {}
                }
            }
        });

        engine
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<AutoEvent> {
        self.events_tx.subscribe()
    }

    pub fn list_rules(&self) -> Vec<AutomationRule> {
        let mut out: Vec<AutomationRule> = self.rules.lock().values().cloned().collect();
        out.sort_by_key(|r| -r.created_at_ms);
        out
    }

    pub fn pause_mismatched_mode(&self, active_mode: TradingMode) -> (usize, usize) {
        let mut paused_rules = 0usize;
        {
            let mut rules = self.rules.lock();
            for r in rules.values_mut() {
                if r.active && r.mode != active_mode {
                    r.active = false;
                    r.in_flight = false;
                    r.next_retry_ms = None;
                    r.status = format!("PAUSED ({:?} mode)", active_mode);
                    paused_rules += 1;
                }
            }
        }
        let mut paused_targets = 0usize;
        {
            let mut targets = self.copy_targets.lock();
            for t in targets.values_mut() {
                if t.enabled && t.mode != active_mode {
                    t.enabled = false;
                    paused_targets += 1;
                }
            }
        }
        if paused_rules > 0 || paused_targets > 0 {
            let msg = format!(
                "Paused {paused_rules} rule(s) and {paused_targets} copy target(s) after switching to {active_mode:?} mode"
            );
            warn!("{msg}");
            let _ = self.events_tx.send(AutoEvent::RuleTriggered {
                rule_id: "mode-switch".into(),
                message: msg,
                is_err: true,
            });
            let _ = self.events_tx.send(AutoEvent::RulesUpdated);
        } else {
            let _ = self.events_tx.send(AutoEvent::RulesUpdated);
        }
        (paused_rules, paused_targets)
    }

    pub fn create_rule(&self, req: CreateRuleRequest) -> AutomationRule {
        let id = format!("rule-{}", &Uuid::new_v4().to_string()[..8]);
        let now = Utc::now().timestamp_millis();
        let mode = self.execution.mode();

        // Ensure market feed is subscribed to this asset so triggers stay hot even if user switches chart!
        self.feed.subscribe(vec![req.asset_id.clone()]);

        let current_ref = self
            .feed
            .with_book(&req.asset_id, |b| {
                if req.side.eq_ignore_ascii_case("SELL") {
                    b.best_bid().or(b.last_trade_price)
                } else {
                    b.best_ask().or(b.last_trade_price)
                }
            })
            .flatten();

        let (peak_price, trigger_price) = if req.kind == RuleKind::TrailingStop {
            let off = req.trail_offset.unwrap_or(0.03).clamp(0.001, 0.50);
            let peak = current_ref.unwrap_or(req.trigger_price);
            let trig = if req.side.eq_ignore_ascii_case("SELL") {
                (peak - off).max(0.001)
            } else {
                (peak + off).min(0.999)
            };
            (Some(peak), trig)
        } else {
            (None, req.trigger_price.clamp(0.001, 0.999))
        };

        let rule = AutomationRule {
            id: id.clone(),
            mode,
            kind: req.kind,
            asset_id: req.asset_id,
            condition_id: req.condition_id,
            market_question: req.market_question,
            outcome: req.outcome,
            side: req.side.to_uppercase(),
            total_size: req.size.max(1.0),
            remaining_size: req.size.max(1.0),
            trigger_price,
            trail_offset: req.trail_offset,
            peak_price,
            interval_ms: req.interval_ms.map(|m| m.clamp(500, 3_600_000)),
            slices_total: req.slices.map(|s| s.clamp(2, 100)),
            slices_done: 0,
            max_slippage_bps: req.max_slippage_bps.unwrap_or(400.0),
            active: true,
            in_flight: false,
            retry_count: 0,
            next_retry_ms: None,
            status: "ARMED".into(),
            created_at_ms: now,
            last_fired_ms: None,
        };

        self.rules.lock().insert(id, rule.clone());
        let _ = self.events_tx.send(AutoEvent::RulesUpdated);
        rule
    }

    pub fn cancel_rule(&self, id: &str) -> bool {
        let mut rules = self.rules.lock();
        let removed = if let Some(r) = rules.get_mut(id) {
            r.active = false;
            r.in_flight = false;
            r.next_retry_ms = None;
            r.status = "CANCELLED".into();
            true
        } else {
            false
        };
        drop(rules);
        if removed {
            let _ = self.events_tx.send(AutoEvent::RulesUpdated);
        }
        removed
    }

    pub fn delete_inactive_rules(&self) {
        self.rules.lock().retain(|_, r| r.active);
        let _ = self.events_tx.send(AutoEvent::RulesUpdated);
    }

    fn evaluate_book_tick(
        self: &Arc<Self>,
        asset_id: &str,
        best_bid: Option<f64>,
        best_ask: Option<f64>,
        last_trade: Option<f64>,
    ) {
        let current_mode = self.execution.mode();
        let now_ms = Utc::now().timestamp_millis();
        let mut to_fire: Vec<(AutomationRule, f64)> = Vec::new();
        let mut changed = false;

        {
            let mut rules = self.rules.lock();
            for rule in rules.values_mut() {
                if !rule.active
                    || rule.in_flight
                    || rule.mode != current_mode
                    || rule.asset_id != asset_id
                {
                    continue;
                }
                let is_sell = rule.side == "SELL";
                let ref_px = if is_sell {
                    best_bid.or(last_trade)
                } else {
                    best_ask.or(last_trade)
                };
                let Some(px) = ref_px else { continue };

                let should_trigger = match rule.kind {
                    RuleKind::StopLoss => {
                        if is_sell {
                            px <= rule.trigger_price
                        } else {
                            px >= rule.trigger_price
                        }
                    }
                    RuleKind::TakeProfit => {
                        if is_sell {
                            px >= rule.trigger_price
                        } else {
                            px <= rule.trigger_price
                        }
                    }
                    RuleKind::TrailingStop => {
                        let off = rule.trail_offset.unwrap_or(0.03);
                        let peak = rule.peak_price.unwrap_or(px);
                        if is_sell {
                            if px > peak {
                                rule.peak_price = Some(px);
                                rule.trigger_price = (px - off).max(0.001);
                                changed = true;
                            }
                            px <= rule.trigger_price
                        } else {
                            if px < peak {
                                rule.peak_price = Some(px);
                                rule.trigger_price = (px + off).min(0.999);
                                changed = true;
                            }
                            px >= rule.trigger_price
                        }
                    }
                    RuleKind::DripFeed => false,
                };

                if should_trigger && rule.next_retry_ms.is_none_or(|t| now_ms >= t) {
                    rule.in_flight = true;
                    rule.status = "EXECUTING".into();
                    rule.last_fired_ms = Some(now_ms);
                    to_fire.push((rule.clone(), px));
                    changed = true;
                }
            }
        }

        if changed {
            let _ = self.events_tx.send(AutoEvent::RulesUpdated);
        }

        for (rule, exec_px) in to_fire {
            let eng = self.clone();
            tokio::spawn(async move {
                if eng.execution.mode() != rule.mode {
                    if let Some(r) = eng.rules.lock().get_mut(&rule.id) {
                        r.in_flight = false;
                        r.active = false;
                        r.status = "PAUSED (mode changed)".into();
                    }
                    let _ = eng.events_tx.send(AutoEvent::RulesUpdated);
                    return;
                }

                let res = eng
                    .execution
                    .place_order(OrderRequest {
                        asset_id: rule.asset_id.clone(),
                        condition_id: rule.condition_id.clone(),
                        market_question: rule.market_question.clone(),
                        outcome: rule.outcome.clone(),
                        side: rule.side.clone(),
                        order_type: "SWEEP".into(),
                        post_only: false,
                        price: exec_px,
                        size: rule.remaining_size,
                        expiration_secs: None,
                        timed_fok_ms: None,
                        max_slippage_bps: Some(rule.max_slippage_bps),
                        fees_enabled: Some(true),
                        max_notional_usd: None,
                    })
                    .await;

                let (msg, is_err) = match res {
                    Ok(ack) => {
                        let filled = ack.filled_size.max(0.0);
                        let mut out_err = false;
                        if let Some(r) = eng.rules.lock().get_mut(&rule.id) {
                            r.in_flight = false;
                            if r.active {
                                r.remaining_size = (r.remaining_size - filled).max(0.0);
                                if r.remaining_size <= 1e-6 {
                                    r.remaining_size = 0.0;
                                    r.active = false;
                                    r.retry_count = 0;
                                    r.next_retry_ms = None;
                                    r.status = format!("EXECUTED ({})", ack.status);
                                } else if filled > 1e-6 {
                                    r.active = true;
                                    r.retry_count = 0;
                                    r.next_retry_ms = Some(Utc::now().timestamp_millis() + 500);
                                    r.status = format!("PARTIAL ({:.2} rem)", r.remaining_size);
                                } else {
                                    r.retry_count = r.retry_count.saturating_add(1);
                                    if r.retry_count < MAX_RULE_RETRIES {
                                        r.active = true;
                                        r.next_retry_ms = Some(
                                            Utc::now().timestamp_millis() + retry_backoff_ms(r.retry_count),
                                        );
                                        r.status = format!(
                                            "UNFILLED · RETRY {}/{}",
                                            r.retry_count, MAX_RULE_RETRIES
                                        );
                                    } else {
                                        r.active = false;
                                        r.next_retry_ms = None;
                                        r.status = "ERROR (unfilled max retries)".into();
                                    }
                                    out_err = true;
                                }
                            }
                        }
                        (
                            format!(
                                "{:?} @ {:.2}¢ -> {} (filled {:.2}, {:.1}ms)",
                                rule.kind,
                                exec_px * 100.0,
                                ack.status,
                                filled,
                                ack.rtt_ms
                            ),
                            out_err,
                        )
                    }
                    Err(e) => {
                        let mut retries_left = false;
                        let mut retry_n = 0;
                        if let Some(r) = eng.rules.lock().get_mut(&rule.id) {
                            r.in_flight = false;
                            if r.active {
                                r.retry_count = r.retry_count.saturating_add(1);
                                retry_n = r.retry_count;
                                if r.retry_count < MAX_RULE_RETRIES {
                                    r.active = true;
                                    r.next_retry_ms = Some(
                                        Utc::now().timestamp_millis() + retry_backoff_ms(r.retry_count),
                                    );
                                    r.status = format!(
                                        "RETRY {}/{}: {e}",
                                        r.retry_count, MAX_RULE_RETRIES
                                    );
                                    retries_left = true;
                                } else {
                                    r.active = false;
                                    r.next_retry_ms = None;
                                    r.status = format!("ERROR: {e}");
                                }
                            }
                        }
                        let text = if retries_left {
                            format!("{:?} failed ({e}); retry {retry_n}/{MAX_RULE_RETRIES} scheduled", rule.kind)
                        } else {
                            format!("{:?} error (disarmed): {e}", rule.kind)
                        };
                        (text, true)
                    }
                };
                info!(rule_id = %rule.id, is_err, "{msg}");
                let _ = eng.events_tx.send(AutoEvent::RuleTriggered {
                    rule_id: rule.id,
                    message: msg,
                    is_err,
                });
                let _ = eng.events_tx.send(AutoEvent::RulesUpdated);
            });
        }
    }

    fn evaluate_drip_feeds(self: &Arc<Self>) {
        let current_mode = self.execution.mode();
        let now = Utc::now().timestamp_millis();
        let mut slices_to_fire: Vec<(AutomationRule, f64, u32)> = Vec::new();
        let mut changed = false;

        {
            let mut rules = self.rules.lock();
            for rule in rules.values_mut() {
                if !rule.active
                    || rule.in_flight
                    || rule.mode != current_mode
                    || rule.kind != RuleKind::DripFeed
                {
                    continue;
                }
                let interval = rule.interval_ms.unwrap_or(5000) as i64;
                let total_slices = rule.slices_total.unwrap_or(5);
                if rule.slices_done >= total_slices || rule.remaining_size <= 1e-4 {
                    rule.active = false;
                    rule.status = "COMPLETED".into();
                    changed = true;
                    continue;
                }
                if rule.next_retry_ms.is_some_and(|t| now < t) {
                    continue;
                }
                let due = rule.last_fired_ms.is_none_or(|last| now - last >= interval);
                if due {
                    let clip = (rule.total_size / (total_slices as f64)).min(rule.remaining_size);
                    rule.in_flight = true;
                    rule.last_fired_ms = Some(now);
                    rule.status = format!("DRIP {}/{} (sending)", rule.slices_done + 1, total_slices);
                    slices_to_fire.push((rule.clone(), clip, total_slices));
                    changed = true;
                }
            }
        }

        if changed {
            let _ = self.events_tx.send(AutoEvent::RulesUpdated);
        }

        for (rule, clip, total_slices) in slices_to_fire {
            let eng = self.clone();
            tokio::spawn(async move {
                if eng.execution.mode() != rule.mode {
                    if let Some(r) = eng.rules.lock().get_mut(&rule.id) {
                        r.in_flight = false;
                        r.active = false;
                        r.status = "PAUSED (mode changed)".into();
                    }
                    let _ = eng.events_tx.send(AutoEvent::RulesUpdated);
                    return;
                }

                let px = eng
                    .feed
                    .with_book(&rule.asset_id, |b| {
                        if rule.side == "BUY" {
                            b.best_ask()
                        } else {
                            b.best_bid()
                        }
                    })
                    .flatten()
                    .unwrap_or(rule.trigger_price);
                // Respect user limit cap on drip-feed
                let ok_px = if rule.side == "BUY" {
                    px <= rule.trigger_price
                } else {
                    px >= rule.trigger_price
                };
                if !ok_px {
                    if let Some(r) = eng.rules.lock().get_mut(&rule.id) {
                        r.in_flight = false;
                        r.status = format!("WAITING LIMIT ({:.1}¢)", rule.trigger_price * 100.0);
                    }
                    let _ = eng.events_tx.send(AutoEvent::RulesUpdated);
                    return;
                }

                let res = eng
                    .execution
                    .place_order(OrderRequest {
                        asset_id: rule.asset_id.clone(),
                        condition_id: rule.condition_id.clone(),
                        market_question: rule.market_question.clone(),
                        outcome: rule.outcome.clone(),
                        side: rule.side.clone(),
                        order_type: "FAK".into(),
                        post_only: false,
                        price: px,
                        size: clip,
                        expiration_secs: None,
                        timed_fok_ms: None,
                        max_slippage_bps: Some(rule.max_slippage_bps),
                        fees_enabled: Some(true),
                        max_notional_usd: None,
                    })
                    .await;

                match res {
                    Ok(ack) => {
                        if ack.filled_size <= 1e-6 && ack.status == "CANCELLED" {
                            if let Some(r) = eng.rules.lock().get_mut(&rule.id) {
                                r.in_flight = false;
                                if r.active {
                                    r.retry_count = r.retry_count.saturating_add(1);
                                    if r.retry_count < MAX_RULE_RETRIES {
                                        r.next_retry_ms = Some(
                                            Utc::now().timestamp_millis() + retry_backoff_ms(r.retry_count),
                                        );
                                        r.status = format!(
                                            "DRIP UNFILLED · RETRY {}/{}",
                                            r.retry_count, MAX_RULE_RETRIES
                                        );
                                    } else {
                                        r.active = false;
                                        r.status = "ERROR (unfilled max retries)".into();
                                    }
                                }
                            }
                            let _ = eng.events_tx.send(AutoEvent::RuleTriggered {
                                rule_id: rule.id,
                                message: "Drip slice unfilled (FAK cancelled)".into(),
                                is_err: true,
                            });
                        } else if let Some(r) = eng.rules.lock().get_mut(&rule.id) {
                            r.in_flight = false;
                            if r.active {
                                r.slices_done += 1;
                                r.remaining_size = (r.remaining_size - ack.filled_size).max(0.0);
                                r.retry_count = 0;
                                r.next_retry_ms = None;
                                if r.slices_done >= total_slices || r.remaining_size <= 1e-4 {
                                    r.active = false;
                                    r.status = "COMPLETED".into();
                                } else {
                                    r.status = format!("DRIP {}/{}", r.slices_done, total_slices);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        if let Some(r) = eng.rules.lock().get_mut(&rule.id) {
                            r.in_flight = false;
                            if r.active {
                                r.retry_count = r.retry_count.saturating_add(1);
                                if r.retry_count < MAX_RULE_RETRIES {
                                    r.next_retry_ms = Some(
                                        Utc::now().timestamp_millis() + retry_backoff_ms(r.retry_count),
                                    );
                                    r.status = format!(
                                        "DRIP RETRY {}/{}: {e}",
                                        r.retry_count, MAX_RULE_RETRIES
                                    );
                                } else {
                                    r.active = false;
                                    r.status = format!("ERROR: {e}");
                                }
                            }
                        }
                        let _ = eng.events_tx.send(AutoEvent::RuleTriggered {
                            rule_id: rule.id,
                            message: format!("Drip-feed error: {e}"),
                            is_err: true,
                        });
                    }
                }
                let _ = eng.events_tx.send(AutoEvent::RulesUpdated);
            });
        }
    }

    pub fn list_copy_targets(&self) -> Vec<CopyTarget> {
        self.copy_targets.lock().values().cloned().collect()
    }

    pub fn add_copy_target(&self, req: CreateCopyTargetRequest) -> CopyTarget {
        let id = format!("copy-{}", &Uuid::new_v4().to_string()[..8]);
        let mode = self.execution.mode();
        let target = CopyTarget {
            id: id.clone(),
            mode,
            wallet: req.wallet.trim().to_lowercase(),
            label: if req.label.trim().is_empty() {
                req.wallet.trim().chars().take(10).collect()
            } else {
                req.label.trim().to_string()
            },
            multiplier: req.multiplier.clamp(0.001, 10.0),
            max_order_usd: req.max_order_usd.clamp(1.0, 100_000.0),
            max_slippage_bps: req.max_slippage_bps.unwrap_or(300.0).clamp(10.0, 2000.0),
            enabled: true,
            copied_count: 0,
            failed_count: 0,
            last_error: None,
            last_seen_ts: Utc::now().timestamp(),
        };
        self.copy_targets.lock().insert(id, target.clone());
        let _ = self.events_tx.send(AutoEvent::RulesUpdated);
        target
    }

    pub fn toggle_copy_target(&self, id: &str) -> bool {
        let current_mode = self.execution.mode();
        let mut m = self.copy_targets.lock();
        if let Some(t) = m.get_mut(id) {
            if !t.enabled && t.mode != current_mode {
                let msg = format!(
                    "Cannot enable {:?} copy target '{}' while in {:?} mode",
                    t.mode, t.label, current_mode
                );
                let _ = self.events_tx.send(AutoEvent::CopyExecuted {
                    wallet: t.wallet.clone(),
                    message: msg,
                    is_err: true,
                });
                return false;
            }
            t.enabled = !t.enabled;
            let _ = self.events_tx.send(AutoEvent::RulesUpdated);
            return true;
        }
        false
    }

    pub fn remove_copy_target(&self, id: &str) -> bool {
        let removed = self.copy_targets.lock().remove(id).is_some();
        if removed {
            let _ = self.events_tx.send(AutoEvent::RulesUpdated);
        }
        removed
    }

    async fn poll_copy_targets(self: &Arc<Self>) {
        let current_mode = self.execution.mode();
        let targets: Vec<CopyTarget> = self
            .copy_targets
            .lock()
            .values()
            .filter(|t| t.enabled && t.mode == current_mode)
            .cloned()
            .collect();

        for target in targets {
            let Ok(trades) = self.market_client.fetch_wallet_trades(&target.wallet, 5).await else {
                continue;
            };
            for tr in trades {
                if self.execution.mode() != target.mode {
                    break;
                }
                if tr.timestamp <= target.last_seen_ts || tr.token_id.is_empty() {
                    continue;
                }
                let dedup = format!("{}:{}:{}:{}", tr.proxy_wallet, tr.token_id, tr.timestamp, tr.size);
                {
                    let mut seen = self.seen_copy_keys.lock();
                    if !seen.insert(dedup) {
                        continue;
                    }
                    if seen.len() > 2000 {
                        seen.clear();
                    }
                }

                // Scale & cap order size
                let scaled_shares = tr.size * target.multiplier;
                let max_shares = target.max_order_usd / tr.price.max(0.01);
                let final_shares = scaled_shares.min(max_shares).floor();
                if final_shares < 1.0 {
                    continue;
                }

                self.feed.subscribe(vec![tr.token_id.clone()]);

                // Compute slippage-bounded price aligned strictly to token tick size
                let slip = target.max_slippage_bps / 10_000.0;
                let raw_px = if tr.side.eq_ignore_ascii_case("BUY") {
                    (tr.price * (1.0 + slip)).min(0.999)
                } else {
                    (tr.price * (1.0 - slip)).max(0.001)
                };
                let tick_ticks = self.feed.get_tick_size_ticks(&tr.token_id).unwrap_or(100);
                let exec_px = align_to_tick(raw_px, tick_ticks);
                let max_notional_usd = tr
                    .side
                    .eq_ignore_ascii_case("BUY")
                    .then_some(target.max_order_usd);

                match self
                    .execution
                    .place_order(OrderRequest {
                        asset_id: tr.token_id.clone(),
                        condition_id: tr.condition_id.clone(),
                        market_question: tr.title.clone(),
                        outcome: tr.outcome.clone(),
                        side: tr.side.clone(),
                        order_type: "FAK".into(),
                        post_only: false,
                        price: exec_px,
                        size: final_shares,
                        expiration_secs: None,
                        timed_fok_ms: None,
                        max_slippage_bps: Some(target.max_slippage_bps),
                        fees_enabled: Some(true),
                        max_notional_usd,
                    })
                    .await
                {
                    Ok(ack) => {
                        if ack.status == "CANCELLED" && ack.filled_size <= 1e-6 {
                            let msg = format!(
                                "Copy {} {} {:.0}sh failed (FAK cancelled, no fill)",
                                target.label, tr.side, final_shares
                            );
                            if let Some(t) = self.copy_targets.lock().get_mut(&target.id) {
                                t.failed_count += 1;
                                t.last_error = Some(msg.clone());
                                t.last_seen_ts = t.last_seen_ts.max(tr.timestamp);
                            }
                            let _ = self.events_tx.send(AutoEvent::CopyExecuted {
                                wallet: target.wallet.clone(),
                                message: msg,
                                is_err: true,
                            });
                            let _ = self.events_tx.send(AutoEvent::RulesUpdated);
                        } else {
                            if let Some(t) = self.copy_targets.lock().get_mut(&target.id) {
                                t.copied_count += 1;
                                t.last_error = None;
                                t.last_seen_ts = t.last_seen_ts.max(tr.timestamp);
                            }
                            let msg = format!(
                                "Copied {} {} {:.0}sh @ {:.1}¢ ({})",
                                target.label,
                                tr.side,
                                final_shares,
                                tr.price * 100.0,
                                ack.status
                            );
                            let _ = self.events_tx.send(AutoEvent::CopyExecuted {
                                wallet: target.wallet.clone(),
                                message: msg,
                                is_err: false,
                            });
                            let _ = self.events_tx.send(AutoEvent::RulesUpdated);
                        }
                    }
                    Err(e) => {
                        let msg = format!("Copy-trade failed for {}: {e}", target.label);
                        warn!("{msg}");
                        if let Some(t) = self.copy_targets.lock().get_mut(&target.id) {
                            t.failed_count += 1;
                            t.last_error = Some(e.to_string());
                        }
                        let _ = self.events_tx.send(AutoEvent::CopyExecuted {
                            wallet: target.wallet.clone(),
                            message: msg,
                            is_err: true,
                        });
                        let _ = self.events_tx.send(AutoEvent::RulesUpdated);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::telemetry::Telemetry;
    use parking_lot::RwLock;

    fn make_test_stack() -> (Arc<ExecutionEngine>, Arc<MarketFeed>, Arc<AutomationEngine>) {
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
        let exec = ExecutionEngine::new(cfg, None, mc.clone(), feed.clone(), telemetry);
        let auto = AutomationEngine::spawn(exec.clone(), feed.clone(), mc);
        (exec, feed, auto)
    }

    #[tokio::test]
    async fn trailing_stop_ratchets_and_triggers() {
        let (exec, feed, auto) = make_test_stack();

        // Seed book + buy 100 paper shares at 0.50
        feed.seed_book("tokA", "condA", 0.01, &[(0.49, 500.0)], &[(0.50, 500.0)], Some(0.50), 1, None);
        exec.place_order(OrderRequest {
            asset_id: "tokA".into(),
            condition_id: "condA".into(),
            market_question: "Q".into(),
            outcome: "Yes".into(),
            side: "BUY".into(),
            order_type: "FAK".into(),
            post_only: false,
            price: 0.50,
            size: 100.0,
            expiration_secs: None,
            timed_fok_ms: None,
            max_slippage_bps: None,
            fees_enabled: Some(true),
            max_notional_usd: None,
        }).await.unwrap();

        // Arm a 5c trailing stop on SELL
        let rule = auto.create_rule(CreateRuleRequest {
            kind: RuleKind::TrailingStop,
            asset_id: "tokA".into(),
            condition_id: "condA".into(),
            market_question: "Q".into(),
            outcome: "Yes".into(),
            side: "SELL".into(),
            size: 100.0,
            trigger_price: 0.44,
            trail_offset: Some(0.05),
            interval_ms: None,
            slices: None,
            max_slippage_bps: Some(500.0),
        });
        assert!(rule.active);
        assert_eq!(rule.mode, TradingMode::Paper);

        // Price rallies to 0.70 -> trailing stop should ratchet peak to 0.70, trigger to 0.65
        auto.evaluate_book_tick("tokA", Some(0.70), Some(0.71), Some(0.70));
        let r_after = auto.list_rules().into_iter().find(|r| r.id == rule.id).unwrap();
        assert!(r_after.active);
        assert!((r_after.trigger_price - 0.65).abs() < 1e-6);

        // Price pulls back to 0.64 -> triggers sell!
        feed.seed_book("tokA", "condA", 0.01, &[(0.64, 500.0)], &[(0.65, 500.0)], Some(0.64), 2, None);
        auto.evaluate_book_tick("tokA", Some(0.64), Some(0.65), Some(0.64));
        tokio::time::sleep(Duration::from_millis(80)).await;
        let r_final = auto.list_rules().into_iter().find(|r| r.id == rule.id).unwrap();
        assert!(!r_final.active);
        assert!(!r_final.in_flight);
    }

    #[tokio::test]
    async fn paper_rule_isolated_from_live_mode_and_pause_mismatched() {
        let (exec, feed, auto) = make_test_stack();
        let mut rx = auto.subscribe_events();
        feed.seed_book("tokIso", "condIso", 0.01, &[(0.50, 100.0)], &[(0.52, 100.0)], Some(0.51), 1, None);

        let rule = auto.create_rule(CreateRuleRequest {
            kind: RuleKind::StopLoss,
            asset_id: "tokIso".into(),
            condition_id: "condIso".into(),
            market_question: "Iso?".into(),
            outcome: "Yes".into(),
            side: "SELL".into(),
            size: 10.0,
            trigger_price: 0.45,
            trail_offset: None,
            interval_ms: None,
            slices: None,
            max_slippage_bps: Some(300.0),
        });
        let target = auto.add_copy_target(CreateCopyTargetRequest {
            wallet: "0x1111111111111111111111111111111111111111".into(),
            label: "Whale".into(),
            multiplier: 0.1,
            max_order_usd: 10.0,
            max_slippage_bps: None,
        });
        assert_eq!(rule.mode, TradingMode::Paper);
        assert_eq!(target.mode, TradingMode::Paper);

        // Switch execution mode to Live -> Paper rule must NOT fire on tick below trigger
        exec.set_mode_for_test(TradingMode::Live);
        auto.evaluate_book_tick("tokIso", Some(0.30), Some(0.32), Some(0.30));
        tokio::time::sleep(Duration::from_millis(40)).await;
        let r_still = auto.list_rules().into_iter().find(|r| r.id == rule.id).unwrap();
        assert!(r_still.active);
        assert!(!r_still.in_flight);
        assert!(r_still.last_fired_ms.is_none());

        // Attempting to re-enable mismatched copy target while in Live mode is refused
        let (paused_r, paused_t) = auto.pause_mismatched_mode(TradingMode::Live);
        assert_eq!(paused_r, 1);
        assert_eq!(paused_t, 1);
        assert!(!auto.toggle_copy_target(&target.id));

        // Drain events and confirm an is_err: true mode warning was emitted
        let mut saw_err = false;
        while let Ok(ev) = rx.try_recv() {
            if let AutoEvent::RuleTriggered { is_err: true, .. } = ev {
                saw_err = true;
            }
        }
        assert!(saw_err);

        // Legacy deserialization without `mode` defaults safely to Paper
        let legacy_rule: AutomationRule = serde_json::from_str(
            r#"{"id":"r1","kind":"stop_loss","asset_id":"a","condition_id":"c","market_question":"q","outcome":"Yes","side":"SELL","total_size":10.0,"remaining_size":10.0,"trigger_price":0.4,"trail_offset":null,"peak_price":null,"interval_ms":null,"slices_total":null,"slices_done":0,"max_slippage_bps":300.0,"active":true,"status":"ARMED","created_at_ms":1,"last_fired_ms":null}"#,
        ).unwrap();
        assert_eq!(legacy_rule.mode, TradingMode::Paper);
        let legacy_copy: CopyTarget = serde_json::from_str(
            r#"{"id":"c1","wallet":"0x1","label":"w","multiplier":0.1,"max_order_usd":10.0,"max_slippage_bps":300.0,"enabled":true,"copied_count":0,"last_seen_ts":1}"#,
        ).unwrap();
        assert_eq!(legacy_copy.mode, TradingMode::Paper);
    }

    #[tokio::test]
    async fn failed_stop_loss_stays_active_with_backoff_and_emits_error() {
        let (_exec, feed, auto) = make_test_stack();
        let mut rx = auto.subscribe_events();
        // Zero shares held, so SELL stop-loss fails with insufficient shares
        feed.seed_book("tokFail", "condFail", 0.01, &[(0.40, 100.0)], &[(0.42, 100.0)], Some(0.41), 1, None);
        let rule = auto.create_rule(CreateRuleRequest {
            kind: RuleKind::StopLoss,
            asset_id: "tokFail".into(),
            condition_id: "condFail".into(),
            market_question: "Fail?".into(),
            outcome: "Yes".into(),
            side: "SELL".into(),
            size: 25.0,
            trigger_price: 0.45,
            trail_offset: None,
            interval_ms: None,
            slices: None,
            max_slippage_bps: Some(400.0),
        });

        auto.evaluate_book_tick("tokFail", Some(0.40), Some(0.42), Some(0.40));
        tokio::time::sleep(Duration::from_millis(80)).await;

        let r = auto.list_rules().into_iter().find(|x| x.id == rule.id).unwrap();
        assert!(r.active, "failed execution must keep stop-loss active");
        assert!(!r.in_flight);
        assert_eq!(r.retry_count, 1);
        assert!(r.next_retry_ms.is_some());
        assert!(r.status.starts_with("RETRY 1/"));

        let mut saw_err = false;
        while let Ok(ev) = rx.try_recv() {
            if let AutoEvent::RuleTriggered { rule_id, is_err: true, .. } = ev
                && rule_id == rule.id
            {
                saw_err = true;
            }
        }
        assert!(saw_err);
    }

    #[tokio::test]
    async fn partial_fill_stop_loss_keeps_remaining_size_active() {
        let (exec, feed, auto) = make_test_stack();
        // Buy 100 shares at 0.50
        feed.seed_book("tokPart", "condPart", 0.01, &[(0.49, 500.0)], &[(0.50, 500.0)], Some(0.50), 1, None);
        exec.place_order(OrderRequest {
            asset_id: "tokPart".into(),
            condition_id: "condPart".into(),
            market_question: "Part?".into(),
            outcome: "Yes".into(),
            side: "BUY".into(),
            order_type: "FAK".into(),
            post_only: false,
            price: 0.50,
            size: 100.0,
            expiration_secs: None,
            timed_fok_ms: None,
            max_slippage_bps: None,
            fees_enabled: Some(false),
            max_notional_usd: None,
        }).await.unwrap();

        let rule = auto.create_rule(CreateRuleRequest {
            kind: RuleKind::StopLoss,
            asset_id: "tokPart".into(),
            condition_id: "condPart".into(),
            market_question: "Part?".into(),
            outcome: "Yes".into(),
            side: "SELL".into(),
            size: 100.0,
            trigger_price: 0.45,
            trail_offset: None,
            interval_ms: None,
            slices: None,
            max_slippage_bps: Some(500.0),
        });

        // Book drops to 0.44 with only 40 shares on bid -> partial fill
        feed.seed_book("tokPart", "condPart", 0.01, &[(0.44, 40.0)], &[(0.46, 100.0)], Some(0.44), 2, None);
        auto.evaluate_book_tick("tokPart", Some(0.44), Some(0.46), Some(0.44));
        tokio::time::sleep(Duration::from_millis(80)).await;

        let r = auto.list_rules().into_iter().find(|x| x.id == rule.id).unwrap();
        assert!(r.active, "partial fill must keep remaining size active");
        assert!(!r.in_flight);
        assert!((r.remaining_size - 60.0).abs() < 1e-6);
        assert!(r.status.starts_with("PARTIAL"));
    }
}
