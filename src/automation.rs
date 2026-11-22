use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{info, warn};
use uuid::Uuid;

use crate::execution::{ExecutionEngine, OrderRequest};
use crate::market::MarketClient;
use crate::ws_feed::{FeedEvent, MarketFeed};

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
    pub status: String,
    pub created_at_ms: i64,
    pub last_fired_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyTarget {
    pub id: String,
    pub wallet: String,
    pub label: String,
    pub multiplier: f64,      // e.g. 0.1 = copy 10% of whale share size
    pub max_order_usd: f64,   // hard cap per copied trade
    pub max_slippage_bps: f64,
    pub enabled: bool,
    pub copied_count: u32,
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
    RuleTriggered { rule_id: String, message: String },
    CopyExecuted { wallet: String, message: String },
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

    pub fn create_rule(&self, req: CreateRuleRequest) -> AutomationRule {
        let id = format!("rule-{}", &Uuid::new_v4().to_string()[..8]);
        let now = Utc::now().timestamp_millis();

        // Ensure market feed is subscribed to this asset so triggers stay hot even if user switches chart!
        self.feed.subscribe(vec![req.asset_id.clone()]);

        let current_ref = self
            .feed
            .get_book_clone(&req.asset_id)
            .and_then(|b| {
                if req.side.eq_ignore_ascii_case("SELL") {
                    b.best_bid().or(b.last_trade_price)
                } else {
                    b.best_ask().or(b.last_trade_price)
                }
            });

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
        let mut to_fire: Vec<(AutomationRule, f64)> = Vec::new();
        let mut changed = false;

        {
            let mut rules = self.rules.lock();
            for rule in rules.values_mut() {
                if !rule.active || rule.asset_id != asset_id {
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

                if should_trigger {
                    rule.active = false;
                    rule.status = "TRIGGERED".into();
                    rule.last_fired_ms = Some(Utc::now().timestamp_millis());
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
                    })
                    .await;

                let msg = match res {
                    Ok(ack) => {
                        if let Some(r) = eng.rules.lock().get_mut(&rule.id) {
                            r.status = format!("EXECUTED ({})", ack.status);
                            r.remaining_size = (r.remaining_size - ack.filled_size).max(0.0);
                        }
                        format!(
                            "{:?} triggered @ {:.2}¢ -> {} ({:.1}ms)",
                            rule.kind,
                            exec_px * 100.0,
                            ack.status,
                            ack.rtt_ms
                        )
                    }
                    Err(e) => {
                        if let Some(r) = eng.rules.lock().get_mut(&rule.id) {
                            r.status = format!("ERROR: {e}");
                        }
                        format!("{:?} trigger error: {e}", rule.kind)
                    }
                };
                info!(rule_id = %rule.id, "{msg}");
                let _ = eng.events_tx.send(AutoEvent::RuleTriggered {
                    rule_id: rule.id,
                    message: msg,
                });
                let _ = eng.events_tx.send(AutoEvent::RulesUpdated);
            });
        }
    }

    fn evaluate_drip_feeds(self: &Arc<Self>) {
        let now = Utc::now().timestamp_millis();
        let mut slices_to_fire: Vec<(AutomationRule, f64)> = Vec::new();

        {
            let mut rules = self.rules.lock();
            for rule in rules.values_mut() {
                if !rule.active || rule.kind != RuleKind::DripFeed {
                    continue;
                }
                let interval = rule.interval_ms.unwrap_or(5000) as i64;
                let total_slices = rule.slices_total.unwrap_or(5);
                if rule.slices_done >= total_slices || rule.remaining_size <= 1e-4 {
                    rule.active = false;
                    rule.status = "COMPLETED".into();
                    continue;
                }
                let due = rule.last_fired_ms.is_none_or(|last| now - last >= interval);
                if due {
                    let clip = (rule.total_size / (total_slices as f64)).min(rule.remaining_size);
                    rule.slices_done += 1;
                    rule.remaining_size = (rule.remaining_size - clip).max(0.0);
                    rule.last_fired_ms = Some(now);
                    if rule.slices_done >= total_slices {
                        rule.active = false;
                        rule.status = "COMPLETED".into();
                    } else {
                        rule.status = format!("DRIP {}/{}", rule.slices_done, total_slices);
                    }
                    slices_to_fire.push((rule.clone(), clip));
                }
            }
        }

        if !slices_to_fire.is_empty() {
            let _ = self.events_tx.send(AutoEvent::RulesUpdated);
        }

        for (rule, clip) in slices_to_fire {
            let eng = self.clone();
            tokio::spawn(async move {
                let px = eng
                    .feed
                    .get_book_clone(&rule.asset_id)
                    .and_then(|b| {
                        if rule.side == "BUY" {
                            b.best_ask()
                        } else {
                            b.best_bid()
                        }
                    })
                    .unwrap_or(rule.trigger_price);
                // Respect user limit cap on drip-feed
                let ok_px = if rule.side == "BUY" {
                    px <= rule.trigger_price
                } else {
                    px >= rule.trigger_price
                };
                if !ok_px {
                    return;
                }
                let _ = eng
                    .execution
                    .place_order(OrderRequest {
                        asset_id: rule.asset_id,
                        condition_id: rule.condition_id,
                        market_question: rule.market_question,
                        outcome: rule.outcome,
                        side: rule.side,
                        order_type: "FAK".into(),
                        post_only: false,
                        price: px,
                        size: clip,
                        expiration_secs: None,
                        timed_fok_ms: None,
                        max_slippage_bps: Some(rule.max_slippage_bps),
                        fees_enabled: Some(true),
                    })
                    .await;
            });
        }
    }

    pub fn list_copy_targets(&self) -> Vec<CopyTarget> {
        self.copy_targets.lock().values().cloned().collect()
    }

    pub fn add_copy_target(&self, req: CreateCopyTargetRequest) -> CopyTarget {
        let id = format!("copy-{}", &Uuid::new_v4().to_string()[..8]);
        let target = CopyTarget {
            id: id.clone(),
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
            last_seen_ts: Utc::now().timestamp(),
        };
        self.copy_targets.lock().insert(id, target.clone());
        let _ = self.events_tx.send(AutoEvent::RulesUpdated);
        target
    }

    pub fn toggle_copy_target(&self, id: &str) -> bool {
        let mut m = self.copy_targets.lock();
        if let Some(t) = m.get_mut(id) {
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
        let targets: Vec<CopyTarget> = self
            .copy_targets
            .lock()
            .values()
            .filter(|t| t.enabled)
            .cloned()
            .collect();

        for target in targets {
            let Ok(trades) = self.market_client.fetch_wallet_trades(&target.wallet, 5).await else {
                continue;
            };
            for tr in trades {
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

                // Compute slippage-bounded price
                let slip = target.max_slippage_bps / 10_000.0;
                let exec_px = if tr.side.eq_ignore_ascii_case("BUY") {
                    (tr.price * (1.0 + slip)).min(0.99)
                } else {
                    (tr.price * (1.0 - slip)).max(0.01)
                };

                self.feed.subscribe(vec![tr.token_id.clone()]);
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
                    })
                    .await
                {
                    Ok(ack) => {
                        if let Some(t) = self.copy_targets.lock().get_mut(&target.id) {
                            t.copied_count += 1;
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
                        });
                        let _ = self.events_tx.send(AutoEvent::RulesUpdated);
                    }
                    Err(e) => {
                        warn!("Copy-trade execution skipped for {}: {e}", target.label);
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

    #[tokio::test]
    async fn trailing_stop_ratchets_and_triggers() {
        let cfg = Arc::new(RwLock::new(AppConfig::default()));
        let telemetry = Arc::new(Telemetry::new());
        let feed = MarketFeed::spawn("ws://127.0.0.1:1".into(), telemetry.clone(), 500.0);
        let mc = Arc::new(MarketClient::new(
            "https://gamma-api.polymarket.com".into(),
            "https://clob.polymarket.com".into(),
            "https://data-api.polymarket.com".into(),
        ).unwrap());
        let exec = ExecutionEngine::new(cfg, None, mc.clone(), feed.clone(), telemetry);
        let auto = AutomationEngine::spawn(exec.clone(), feed.clone(), mc);

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

        // Price rallies to 0.70 -> trailing stop should ratchet peak to 0.70, trigger to 0.65
        auto.evaluate_book_tick("tokA", Some(0.70), Some(0.71), Some(0.70));
        let r_after = auto.list_rules().into_iter().find(|r| r.id == rule.id).unwrap();
        assert!(r_after.active);
        assert!((r_after.trigger_price - 0.65).abs() < 1e-6);

        // Price pulls back to 0.64 -> triggers sell!
        feed.seed_book("tokA", "condA", 0.01, &[(0.64, 500.0)], &[(0.65, 500.0)], Some(0.64), 2, None);
        auto.evaluate_book_tick("tokA", Some(0.64), Some(0.65), Some(0.64));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let r_final = auto.list_rules().into_iter().find(|r| r.id == rule.id).unwrap();
        assert!(!r_final.active);
    }
}
