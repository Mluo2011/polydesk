use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "threshold", rename_all = "snake_case")]
pub enum AlertCondition {
    CrossAbove(f64),
    CrossBelow(f64),
    SpreadAbove(f64),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceAlert {
    pub id: String,
    pub asset_id: String,
    pub market_question: String,
    pub outcome: String,
    pub condition: AlertCondition,
    pub active: bool,
    #[serde(default = "default_true")]
    pub sound_enabled: bool,
    #[serde(default)]
    pub triggered_at_ms: Option<i64>,
}

impl PriceAlert {
    pub fn new(
        asset_id: String,
        market_question: String,
        outcome: String,
        condition: AlertCondition,
        sound_enabled: bool,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            asset_id,
            market_question,
            outcome,
            condition,
            active: true,
            sound_enabled,
            triggered_at_ms: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AlertTriggeredEvent {
    pub alert: PriceAlert,
    pub observed_value: f64,
}

pub struct AlertEngine {
    alerts: RwLock<Vec<PriceAlert>>,
    active_assets: RwLock<HashSet<String>>,
    events_tx: broadcast::Sender<AlertTriggeredEvent>,
}

impl AlertEngine {
    pub fn new(initial: Vec<PriceAlert>) -> Arc<Self> {
        let active_assets: HashSet<String> = initial
            .iter()
            .filter(|a| a.active)
            .map(|a| a.asset_id.clone())
            .collect();
        let (events_tx, _) = broadcast::channel(128);
        Arc::new(Self {
            alerts: RwLock::new(initial),
            active_assets: RwLock::new(active_assets),
            events_tx,
        })
    }

    pub fn list(&self) -> Vec<PriceAlert> {
        self.alerts.read().clone()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<AlertTriggeredEvent> {
        self.events_tx.subscribe()
    }

    pub fn add(&self, alert: PriceAlert) -> Vec<PriceAlert> {
        let mut guard = self.alerts.write();
        guard.push(alert);
        Self::rebuild_active_set(&guard, &self.active_assets);
        guard.clone()
    }

    pub fn rearm(&self, id: &str) -> Vec<PriceAlert> {
        let mut guard = self.alerts.write();
        if let Some(a) = guard.iter_mut().find(|a| a.id == id) {
            a.active = true;
            a.triggered_at_ms = None;
        }
        Self::rebuild_active_set(&guard, &self.active_assets);
        guard.clone()
    }

    pub fn remove(&self, id: &str) -> Vec<PriceAlert> {
        let mut guard = self.alerts.write();
        guard.retain(|a| a.id != id);
        Self::rebuild_active_set(&guard, &self.active_assets);
        guard.clone()
    }

    /// Evaluate all active alerts for `asset_id` in O(1) when no active alerts target it.
    /// Read-only notification only; never triggers orders.
    pub fn evaluate_book(
        &self,
        asset_id: &str,
        best_bid: Option<f64>,
        best_ask: Option<f64>,
        mid: Option<f64>,
        now_ms: i64,
    ) -> Vec<AlertTriggeredEvent> {
        if !self.active_assets.read().contains(asset_id) {
            return Vec::new();
        }
        let mark = mid.or(best_ask).or(best_bid);
        let spread = match (best_bid, best_ask) {
            (Some(b), Some(a)) if a >= b => Some(a - b),
            _ => None,
        };

        let mut fired = Vec::new();
        let mut guard = self.alerts.write();
        for a in guard.iter_mut() {
            if !a.active || a.asset_id != asset_id {
                continue;
            }
            let observed = match a.condition {
                AlertCondition::CrossAbove(th) => mark.filter(|&p| p >= th),
                AlertCondition::CrossBelow(th) => mark.filter(|&p| p <= th),
                AlertCondition::SpreadAbove(th) => spread.filter(|&s| s >= th),
            };
            if let Some(val) = observed {
                a.active = false;
                a.triggered_at_ms = Some(now_ms);
                let ev = AlertTriggeredEvent {
                    alert: a.clone(),
                    observed_value: val,
                };
                fired.push(ev.clone());
                let _ = self.events_tx.send(ev);
            }
        }
        if !fired.is_empty() {
            Self::rebuild_active_set(&guard, &self.active_assets);
        }
        fired
    }

    fn rebuild_active_set(alerts: &[PriceAlert], active_assets: &RwLock<HashSet<String>>) {
        let mut set = active_assets.write();
        set.clear();
        for a in alerts {
            if a.active {
                set.insert(a.asset_id.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_and_spread_alerts_fire_once_and_rearm() {
        let eng = AlertEngine::new(vec![
            PriceAlert::new(
                "tok-1".into(),
                "Q1".into(),
                "Yes".into(),
                AlertCondition::CrossAbove(0.65),
                false,
            ),
            PriceAlert::new(
                "tok-1".into(),
                "Q1".into(),
                "Yes".into(),
                AlertCondition::CrossBelow(0.35),
                false,
            ),
            PriceAlert::new(
                "tok-2".into(),
                "Q2".into(),
                "Yes".into(),
                AlertCondition::SpreadAbove(0.05),
                false,
            ),
        ]);

        // Unrelated asset skips in O(1)
        assert!(eng.evaluate_book("tok-other", Some(0.9), Some(0.99), Some(0.95), 1000).is_empty());

        // Mid inside neutral band triggers nothing
        assert!(eng.evaluate_book("tok-1", Some(0.49), Some(0.51), Some(0.50), 1001).is_empty());

        // Cross above fires CrossAbove and disarms it so duplicate ticks don't re-fire
        let f1 = eng.evaluate_book("tok-1", Some(0.65), Some(0.67), Some(0.66), 1002);
        assert_eq!(f1.len(), 1);
        assert!(matches!(f1[0].alert.condition, AlertCondition::CrossAbove(_)));
        assert!(eng.evaluate_book("tok-1", Some(0.67), Some(0.69), Some(0.68), 1003).is_empty());

        // Cross below fires CrossBelow
        let f2 = eng.evaluate_book("tok-1", Some(0.33), Some(0.35), Some(0.34), 1004);
        assert_eq!(f2.len(), 1);
        assert!(matches!(f2[0].alert.condition, AlertCondition::CrossBelow(_)));

        // Re-arm first alert
        let above_id = f1[0].alert.id.clone();
        eng.rearm(&above_id);
        let f3 = eng.evaluate_book("tok-1", Some(0.69), Some(0.71), Some(0.70), 1005);
        assert_eq!(f3.len(), 1);

        // Spread above 5c fires cleanly
        let f_sp = eng.evaluate_book("tok-2", Some(0.40), Some(0.47), Some(0.435), 1006);
        assert_eq!(f_sp.len(), 1);
        assert!(matches!(f_sp[0].alert.condition, AlertCondition::SpreadAbove(_)));
        assert!((f_sp[0].observed_value - 0.07).abs() < 1e-9);
    }
}
