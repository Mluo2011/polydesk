use std::collections::HashMap;

use anyhow::{Result, anyhow};
use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::book::{FastOrderBook, estimate_builder_fee_usd, estimate_protocol_fee_usd};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperOrder {
    pub id: String,
    pub asset_id: String,
    pub condition_id: String,
    pub market_question: String,
    pub outcome: String,
    pub side: String,       // "BUY" | "SELL"
    pub order_type: String, // "GTC" | "GTD" | "FAK" | "FOK"
    pub post_only: bool,
    pub price: f64,
    pub original_size: f64,
    pub remaining_size: f64,
    pub filled_size: f64,
    pub avg_fill_price: f64,
    pub status: String, // "OPEN" | "FILLED" | "PARTIAL" | "CANCELLED" | "REJECTED"
    pub created_at_ms: i64,
    pub expires_at_ms: Option<i64>,
    pub fees_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperPosition {
    pub asset_id: String,
    pub condition_id: String,
    pub market_question: String,
    pub outcome: String,
    pub shares: f64,
    pub avg_entry: f64,
    pub realized_pnl: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperFill {
    pub id: String,
    pub order_id: String,
    pub asset_id: String,
    pub market_question: String,
    pub outcome: String,
    pub side: String,
    pub price: f64,
    pub shares: f64,
    pub notional_usd: f64,
    pub is_maker: bool,
    pub protocol_fee_usd: f64,
    pub builder_fee_bps: u32,
    pub builder_fee_usd: f64,
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperStateSnapshot {
    pub cash_usd: f64,
    pub reserved_usd: f64,
    pub realized_pnl_usd: f64,
    pub total_protocol_fees_usd: f64,
    pub total_builder_fees_usd: f64,
    pub open_orders: Vec<PaperOrder>,
    pub positions: Vec<PaperPosition>,
    pub recent_fills: Vec<PaperFill>,
}

struct InnerPaper {
    cash_usd: f64,
    realized_pnl_usd: f64,
    total_protocol_fees_usd: f64,
    total_builder_fees_usd: f64,
    orders: HashMap<String, PaperOrder>,
    positions: HashMap<String, PaperPosition>,
    fills: Vec<PaperFill>,
}

pub struct PaperEngine {
    inner: Mutex<InnerPaper>,
}

#[derive(Debug, Clone)]
pub struct PaperOrderParams {
    pub asset_id: String,
    pub condition_id: String,
    pub market_question: String,
    pub outcome: String,
    pub side: String,
    pub order_type: String,
    pub post_only: bool,
    pub price: f64,
    pub size: f64,
    pub expires_at_ms: Option<i64>,
    pub fees_enabled: bool,
    pub builder_maker_bps: u32,
    pub builder_taker_bps: u32,
}

#[derive(Debug, Clone)]
pub struct PaperPlaceResult {
    pub order: PaperOrder,
    pub new_fills: Vec<PaperFill>,
}

impl PaperEngine {
    pub fn new(starting_balance: f64) -> Self {
        Self {
            inner: Mutex::new(InnerPaper {
                cash_usd: starting_balance.max(1.0),
                realized_pnl_usd: 0.0,
                total_protocol_fees_usd: 0.0,
                total_builder_fees_usd: 0.0,
                orders: HashMap::new(),
                positions: HashMap::new(),
                fills: Vec::new(),
            }),
        }
    }

    pub fn reset(&self, balance: f64) {
        let mut s = self.inner.lock();
        s.cash_usd = balance.max(1.0);
        s.realized_pnl_usd = 0.0;
        s.total_protocol_fees_usd = 0.0;
        s.total_builder_fees_usd = 0.0;
        s.orders.clear();
        s.positions.clear();
        s.fills.clear();
    }

    pub fn snapshot(&self) -> PaperStateSnapshot {
        let s = self.inner.lock();
        let mut open_orders: Vec<PaperOrder> = s
            .orders
            .values()
            .filter(|o| o.status == "OPEN" || o.status == "PARTIAL")
            .cloned()
            .collect();
        open_orders.sort_by_key(|o| -o.created_at_ms);

        let reserved_usd: f64 = open_orders
            .iter()
            .filter(|o| o.side == "BUY")
            .map(|o| o.remaining_size * o.price)
            .sum();

        let positions: Vec<PaperPosition> = s
            .positions
            .values()
            .filter(|p| p.shares > 1e-6)
            .cloned()
            .collect();

        let recent_fills = s.fills.iter().rev().take(80).cloned().collect();

        PaperStateSnapshot {
            cash_usd: s.cash_usd,
            reserved_usd,
            realized_pnl_usd: s.realized_pnl_usd,
            total_protocol_fees_usd: s.total_protocol_fees_usd,
            total_builder_fees_usd: s.total_builder_fees_usd,
            open_orders,
            positions,
            recent_fills,
        }
    }

    pub fn position(&self, asset_id: &str) -> Option<PaperPosition> {
        self.inner
            .lock()
            .positions
            .get(asset_id)
            .filter(|p| p.shares > 1e-6)
            .cloned()
    }

    pub fn place_order(
        &self,
        params: PaperOrderParams,
        book: Option<&FastOrderBook>,
    ) -> Result<PaperPlaceResult> {
        if params.price <= 0.0 || params.price >= 1.0 {
            return Err(anyhow!("Order price must be between 0.0001 and 0.9999"));
        }
        if params.size <= 0.0 {
            return Err(anyhow!("Order size must be positive"));
        }

        let is_buy = params.side.eq_ignore_ascii_case("BUY");
        let order_type = params.order_type.to_uppercase();
        let now_ms = Utc::now().timestamp_millis();
        let order_id = format!("paper-{}", &Uuid::new_v4().to_string()[..12]);

        let mut s = self.inner.lock();

        // Check available balance / shares
        if is_buy {
            let reserved: f64 = s
                .orders
                .values()
                .filter(|o| (o.status == "OPEN" || o.status == "PARTIAL") && o.side == "BUY")
                .map(|o| o.remaining_size * o.price)
                .sum();
            let free_cash = s.cash_usd - reserved;
            let needed = params.size * params.price;
            if needed > free_cash + 1e-6 {
                return Err(anyhow!(
                    "Insufficient paper balance: need ${needed:.2}, available ${free_cash:.2}"
                ));
            }
        } else {
            let held = s
                .positions
                .get(&params.asset_id)
                .map(|p| p.shares)
                .unwrap_or(0.0);
            let reserved_shares: f64 = s
                .orders
                .values()
                .filter(|o| {
                    (o.status == "OPEN" || o.status == "PARTIAL")
                        && o.side == "SELL"
                        && o.asset_id == params.asset_id
                })
                .map(|o| o.remaining_size)
                .sum();
            let free_shares = held - reserved_shares;
            if params.size > free_shares + 1e-6 {
                return Err(anyhow!(
                    "Insufficient shares to sell: need {:.2}, available {:.2}",
                    params.size,
                    free_shares.max(0.0)
                ));
            }
        }

        // Determine if order crosses resting book immediately
        let crosses = match book {
            Some(b) if is_buy => b.best_ask().is_some_and(|a| params.price >= a),
            Some(b) => b.best_bid().is_some_and(|bid| params.price <= bid),
            None => false,
        };

        if params.post_only && crosses {
            return Err(anyhow!(
                "Post-Only rejected: price {:.4} would cross the spread",
                params.price
            ));
        }

        let mut order = PaperOrder {
            id: order_id.clone(),
            asset_id: params.asset_id.clone(),
            condition_id: params.condition_id.clone(),
            market_question: params.market_question.clone(),
            outcome: params.outcome.clone(),
            side: if is_buy { "BUY".into() } else { "SELL".into() },
            order_type: order_type.clone(),
            post_only: params.post_only,
            price: params.price,
            original_size: params.size,
            remaining_size: params.size,
            filled_size: 0.0,
            avg_fill_price: 0.0,
            status: "OPEN".into(),
            created_at_ms: now_ms,
            expires_at_ms: params.expires_at_ms,
            fees_enabled: params.fees_enabled,
        };

        let mut new_fills = Vec::new();

        // Execute immediate crossing fill against live L2 book levels
        if crosses && let Some(b) = book {
            let sweep = b.simulate_sweep(is_buy, None, Some(params.size), Some(params.price));
            if order_type == "FOK" && sweep.total_shares + 1e-4 < params.size {
                order.status = "CANCELLED".into();
                return Ok(PaperPlaceResult { order, new_fills });
            }

            for (fill_price, fill_qty) in sweep.legs {
                let fill = apply_fill_locked(
                    &mut s,
                    &mut order,
                    fill_price,
                    fill_qty,
                    false,
                    params.builder_maker_bps,
                    params.builder_taker_bps,
                    now_ms,
                );
                new_fills.push(fill);
            }
        } else if order_type == "FOK" || order_type == "FAK" {
            // Non-crossing FOK/FAK cancels immediately
            order.status = "CANCELLED".into();
            return Ok(PaperPlaceResult { order, new_fills });
        }

        if order.remaining_size <= 1e-6 {
            order.remaining_size = 0.0;
            order.status = "FILLED".into();
        } else if order_type == "FAK" || order_type == "FOK" {
            order.remaining_size = 0.0;
            order.status = if order.filled_size > 0.0 {
                "FILLED".into()
            } else {
                "CANCELLED".into()
            };
        } else {
            order.status = if order.filled_size > 0.0 {
                "PARTIAL".into()
            } else {
                "OPEN".into()
            };
            s.orders.insert(order.id.clone(), order.clone());
        }

        Ok(PaperPlaceResult { order, new_fills })
    }

    /// Called on every live WS book/trade tick to fill resting paper GTC/GTD limit orders
    /// when the real market crosses their price!
    pub fn on_market_tick(
        &self,
        asset_id: &str,
        best_bid: Option<f64>,
        best_ask: Option<f64>,
        last_trade: Option<f64>,
        builder_maker_bps: u32,
        builder_taker_bps: u32,
    ) -> Vec<PaperFill> {
        let now_ms = Utc::now().timestamp_millis();
        let mut s = self.inner.lock();
        let mut triggered_ids = Vec::new();
        let mut expired_ids = Vec::new();

        for (id, o) in &s.orders {
            if o.asset_id != asset_id || (o.status != "OPEN" && o.status != "PARTIAL") {
                continue;
            }
            if let Some(exp) = o.expires_at_ms
                && now_ms >= exp
            {
                expired_ids.push(id.clone());
                continue;
            }
            let should_fill = if o.side == "BUY" {
                best_ask.is_some_and(|a| a <= o.price) || last_trade.is_some_and(|lt| lt < o.price)
            } else {
                best_bid.is_some_and(|b| b >= o.price) || last_trade.is_some_and(|lt| lt > o.price)
            };
            if should_fill {
                triggered_ids.push(id.clone());
            }
        }

        for id in expired_ids {
            if let Some(o) = s.orders.get_mut(&id) {
                o.status = "CANCELLED".into();
                o.remaining_size = 0.0;
            }
        }

        let mut fills = Vec::new();
        for id in triggered_ids {
            if let Some(mut order) = s.orders.remove(&id) {
                let qty = order.remaining_size;
                let fill_price = order.price;
                if qty > 1e-6 {
                    let fill = apply_fill_locked(
                        &mut s,
                        &mut order,
                        fill_price,
                        qty,
                        true,
                        builder_maker_bps,
                        builder_taker_bps,
                        now_ms,
                    );
                    order.remaining_size = 0.0;
                    order.status = "FILLED".into();
                    fills.push(fill);
                }
            }
        }
        fills
    }

    pub fn cancel_order(&self, order_id: &str) -> bool {
        let mut s = self.inner.lock();
        if let Some(o) = s.orders.get_mut(order_id) {
            if o.status == "OPEN" || o.status == "PARTIAL" {
                o.status = "CANCELLED".into();
                o.remaining_size = 0.0;
                return true;
            }
        }
        false
    }

    pub fn cancel_at_price(&self, asset_id: &str, side: Option<&str>, price: f64) -> usize {
        let mut s = self.inner.lock();
        let mut count = 0;
        for o in s.orders.values_mut() {
            if o.asset_id == asset_id
                && (o.status == "OPEN" || o.status == "PARTIAL")
                && (o.price - price).abs() < 5e-5
                && side.is_none_or(|sd| o.side.eq_ignore_ascii_case(sd))
            {
                o.status = "CANCELLED".into();
                o.remaining_size = 0.0;
                count += 1;
            }
        }
        count
    }

    pub fn cancel_market(&self, asset_id: &str) -> usize {
        let mut s = self.inner.lock();
        let mut count = 0;
        for o in s.orders.values_mut() {
            if o.asset_id == asset_id && (o.status == "OPEN" || o.status == "PARTIAL") {
                o.status = "CANCELLED".into();
                o.remaining_size = 0.0;
                count += 1;
            }
        }
        count
    }

    pub fn cancel_all(&self) -> usize {
        let mut s = self.inner.lock();
        let mut count = 0;
        for o in s.orders.values_mut() {
            if o.status == "OPEN" || o.status == "PARTIAL" {
                o.status = "CANCELLED".into();
                o.remaining_size = 0.0;
                count += 1;
            }
        }
        count
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_fill_locked(
    s: &mut InnerPaper,
    order: &mut PaperOrder,
    price: f64,
    shares: f64,
    is_maker: bool,
    builder_maker_bps: u32,
    builder_taker_bps: u32,
    now_ms: i64,
) -> PaperFill {
    let notional = shares * price;
    let proto_fee = estimate_protocol_fee_usd(shares, price, is_maker, order.fees_enabled);
    let builder_bps = if is_maker {
        builder_maker_bps.min(50)
    } else {
        builder_taker_bps.min(100)
    };
    let builder_fee = estimate_builder_fee_usd(notional, is_maker, builder_maker_bps, builder_taker_bps);

    s.total_protocol_fees_usd += proto_fee;
    s.total_builder_fees_usd += builder_fee;

    let prev_notional = order.filled_size * order.avg_fill_price;
    order.filled_size += shares;
    order.remaining_size = (order.remaining_size - shares).max(0.0);
    order.avg_fill_price = (prev_notional + notional) / order.filled_size.max(1e-9);

    if order.side == "BUY" {
        s.cash_usd -= notional + proto_fee + builder_fee;
        let pos = s
            .positions
            .entry(order.asset_id.clone())
            .or_insert_with(|| PaperPosition {
                asset_id: order.asset_id.clone(),
                condition_id: order.condition_id.clone(),
                market_question: order.market_question.clone(),
                outcome: order.outcome.clone(),
                shares: 0.0,
                avg_entry: 0.0,
                realized_pnl: 0.0,
            });
        let new_shares = pos.shares + shares;
        pos.avg_entry = (pos.shares * pos.avg_entry + notional) / new_shares.max(1e-9);
        pos.shares = new_shares;
    } else {
        s.cash_usd += notional - proto_fee - builder_fee;
        if let Some(pos) = s.positions.get_mut(&order.asset_id) {
            let closed = shares.min(pos.shares);
            let pnl = closed * (price - pos.avg_entry) - proto_fee - builder_fee;
            pos.shares = (pos.shares - closed).max(0.0);
            pos.realized_pnl += pnl;
            s.realized_pnl_usd += pnl;
        }
    }

    let fill = PaperFill {
        id: format!("fill-{}", &Uuid::new_v4().to_string()[..10]),
        order_id: order.id.clone(),
        asset_id: order.asset_id.clone(),
        market_question: order.market_question.clone(),
        outcome: order.outcome.clone(),
        side: order.side.clone(),
        price,
        shares,
        notional_usd: notional,
        is_maker,
        protocol_fee_usd: proto_fee,
        builder_fee_bps: builder_bps,
        builder_fee_usd: builder_fee,
        timestamp_ms: now_ms,
    };

    if s.fills.len() >= 500 {
        s.fills.remove(0);
    }
    s.fills.push(fill.clone());
    fill
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_buy_sell_and_resting_fill() {
        let engine = PaperEngine::new(1000.0);
        let mut book = FastOrderBook::new("tok1".into(), "cond1".into(), 0.01);
        book.apply_snapshot(&[(0.48, 200.0)], &[(0.50, 200.0)], 1, None);

        // Aggressive FAK buy at 0.50 for 100 shares -> fills immediately at 0.50 ($50 + fees)
        let r = engine
            .place_order(
                PaperOrderParams {
                    asset_id: "tok1".into(),
                    condition_id: "cond1".into(),
                    market_question: "Test?".into(),
                    outcome: "Yes".into(),
                    side: "BUY".into(),
                    order_type: "FAK".into(),
                    post_only: false,
                    price: 0.50,
                    size: 100.0,
                    expires_at_ms: None,
                    fees_enabled: true,
                    builder_maker_bps: 5,
                    builder_taker_bps: 15,
                },
                Some(&book),
            )
            .unwrap();
        assert_eq!(r.order.status, "FILLED");
        assert_eq!(r.new_fills.len(), 1);
        assert!(r.new_fills[0].builder_fee_usd > 0.0);

        // Resting GTC sell at 0.60
        let r2 = engine
            .place_order(
                PaperOrderParams {
                    asset_id: "tok1".into(),
                    condition_id: "cond1".into(),
                    market_question: "Test?".into(),
                    outcome: "Yes".into(),
                    side: "SELL".into(),
                    order_type: "GTC".into(),
                    post_only: true,
                    price: 0.60,
                    size: 100.0,
                    expires_at_ms: None,
                    fees_enabled: true,
                    builder_maker_bps: 5,
                    builder_taker_bps: 15,
                },
                Some(&book),
            )
            .unwrap();
        assert_eq!(r2.order.status, "OPEN");

        // Tick crosses 0.60 -> fills resting sell as maker
        let fills = engine.on_market_tick("tok1", Some(0.60), Some(0.61), Some(0.60), 5, 15);
        assert_eq!(fills.len(), 1);
        assert!(fills[0].is_maker);
        let snap = engine.snapshot();
        assert!(snap.realized_pnl_usd > 9.0);
    }
}
