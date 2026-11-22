use serde::{Deserialize, Serialize};

/// Precision scale: 1 tick = 0.0001 ($0.0001 = 0.01 cents).
/// Valid Polymarket probabilities live in [0.0001 .. 0.9999] -> ticks 1..=9999.
pub const MAX_TICK: usize = 10_000;
/// Share precision: 1_000_000 micro-shares = 1.0 share (matches 6-decimal ERC-1155 / pUSD).
pub const SIZE_SCALE: f64 = 1_000_000.0;

#[inline]
pub fn price_to_tick(price: f64) -> Option<usize> {
    if !price.is_finite() || price <= 0.0 || price >= 1.0 {
        return None;
    }
    let t = (price * 10_000.0).round() as isize;
    if t >= 1 && (t as usize) < MAX_TICK {
        Some(t as usize)
    } else {
        None
    }
}

#[inline]
pub fn tick_to_price(tick: usize) -> f64 {
    (tick as f64) / 10_000.0
}

#[inline]
pub fn size_to_micro(size: f64) -> u64 {
    if !size.is_finite() || size <= 0.0 {
        0
    } else {
        (size * SIZE_SCALE).round() as u64
    }
}

#[inline]
pub fn micro_to_size(micro: u64) -> f64 {
    (micro as f64) / SIZE_SCALE
}

/// Ultra-low-latency O(1) fixed-tick L2 orderbook.
/// Zero heap allocations on delta updates.
#[derive(Clone)]
pub struct FastOrderBook {
    pub asset_id: String,
    pub market_id: String,
    pub tick_size_ticks: usize, // e.g. 100 = 0.01, 10 = 0.001, 1 = 0.0001
    pub bids: Box<[u64; MAX_TICK + 1]>,
    pub asks: Box<[u64; MAX_TICK + 1]>,
    pub best_bid_tick: Option<usize>,
    pub best_ask_tick: Option<usize>,
    pub last_trade_price: Option<f64>,
    pub last_trade_side: Option<String>,
    pub last_timestamp_ms: i64,
    pub sequence: u64,
    pub hash: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LadderLevel {
    pub price: f64,
    pub tick: usize,
    pub bid_size: f64,
    pub ask_size: f64,
    pub cum_bid: f64,
    pub cum_ask: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookView {
    pub asset_id: String,
    pub market_id: String,
    pub tick_size: f64,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    pub mid: Option<f64>,
    pub spread: Option<f64>,
    pub imbalance: f64, // -1.0 (all ask) .. +1.0 (all bid) across top 5 levels
    pub last_trade_price: Option<f64>,
    pub last_trade_side: Option<String>,
    pub timestamp_ms: i64,
    pub sequence: u64,
    pub bids: Vec<(f64, f64)>, // top levels descending (price, size)
    pub asks: Vec<(f64, f64)>, // top levels ascending (price, size)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepQuote {
    pub side: String,
    pub total_shares: f64,
    pub total_notional_usd: f64,
    pub vwap: f64,
    pub worst_price: f64,
    pub best_price: f64,
    pub slippage_bps: f64,
    pub levels_consumed: usize,
    pub filled_completely: bool,
    pub legs: Vec<(f64, f64)>, // (price, shares)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GreenUpQuote {
    pub shares: f64,
    pub avg_entry: f64,
    pub close_price: f64,
    pub close_locked_pnl: f64,
    pub close_cash_out: f64,
    pub free_roll_shares_to_sell: f64,
    pub free_roll_remaining_shares: f64,
    pub can_free_roll: bool,
    pub hedge_no_price: Option<f64>,
    pub hedge_no_locked_pnl: Option<f64>,
    pub recommended_action: String,
}

impl FastOrderBook {
    pub fn new(asset_id: String, market_id: String, tick_size: f64) -> Self {
        let tick_size_ticks = ((tick_size * 10_000.0).round() as usize).clamp(1, 500);
        Self {
            asset_id,
            market_id,
            tick_size_ticks,
            bids: Box::new([0u64; MAX_TICK + 1]),
            asks: Box::new([0u64; MAX_TICK + 1]),
            best_bid_tick: None,
            best_ask_tick: None,
            last_trade_price: None,
            last_trade_side: None,
            last_timestamp_ms: 0,
            sequence: 0,
            hash: String::new(),
        }
    }

    pub fn set_tick_size(&mut self, tick_size: f64) {
        self.tick_size_ticks = ((tick_size * 10_000.0).round() as usize).clamp(1, 500);
    }

    pub fn apply_snapshot(
        &mut self,
        bids: &[(f64, f64)],
        asks: &[(f64, f64)],
        timestamp_ms: i64,
        hash: Option<String>,
    ) {
        self.bids.fill(0);
        self.asks.fill(0);
        let mut best_bid: Option<usize> = None;
        let mut best_ask: Option<usize> = None;

        for &(p, s) in bids {
            if let Some(t) = price_to_tick(p) {
                let m = size_to_micro(s);
                if m > 0 {
                    self.bids[t] = m;
                    best_bid = Some(best_bid.map_or(t, |cur| cur.max(t)));
                }
            }
        }
        for &(p, s) in asks {
            if let Some(t) = price_to_tick(p) {
                let m = size_to_micro(s);
                if m > 0 {
                    self.asks[t] = m;
                    best_ask = Some(best_ask.map_or(t, |cur| cur.min(t)));
                }
            }
        }

        self.best_bid_tick = best_bid;
        self.best_ask_tick = best_ask;
        self.last_timestamp_ms = timestamp_ms;
        if let Some(h) = hash {
            self.hash = h;
        }
        self.sequence = self.sequence.wrapping_add(1);
    }

    #[inline]
    pub fn apply_delta(&mut self, is_bid: bool, price: f64, size: f64, timestamp_ms: i64) {
        let Some(t) = price_to_tick(price) else {
            return;
        };
        let m = size_to_micro(size);
        if is_bid {
            self.bids[t] = m;
            if m > 0 {
                self.best_bid_tick = Some(self.best_bid_tick.map_or(t, |cur| cur.max(t)));
            } else if self.best_bid_tick == Some(t) {
                self.best_bid_tick = (1..t).rev().find(|&i| self.bids[i] > 0);
            }
        } else {
            self.asks[t] = m;
            if m > 0 {
                self.best_ask_tick = Some(self.best_ask_tick.map_or(t, |cur| cur.min(t)));
            } else if self.best_ask_tick == Some(t) {
                self.best_ask_tick = ((t + 1)..MAX_TICK).find(|&i| self.asks[i] > 0);
            }
        }
        if timestamp_ms > 0 {
            self.last_timestamp_ms = timestamp_ms;
        }
        self.sequence = self.sequence.wrapping_add(1);
    }

    pub fn best_bid(&self) -> Option<f64> {
        self.best_bid_tick.map(tick_to_price)
    }

    pub fn best_ask(&self) -> Option<f64> {
        self.best_ask_tick.map(tick_to_price)
    }

    pub fn mid(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => Some((b + a) * 0.5),
            (Some(b), None) => Some(b),
            (None, Some(a)) => Some(a),
            (None, None) => self.last_trade_price,
        }
    }

    pub fn spread(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) if a >= b => Some(a - b),
            _ => None,
        }
    }

    pub fn to_view(&self, max_levels: usize) -> BookView {
        let mut bids = Vec::with_capacity(max_levels);
        if let Some(start) = self.best_bid_tick {
            for t in (1..=start).rev() {
                let m = self.bids[t];
                if m > 0 {
                    bids.push((tick_to_price(t), micro_to_size(m)));
                    if bids.len() >= max_levels {
                        break;
                    }
                }
            }
        }

        let mut asks = Vec::with_capacity(max_levels);
        if let Some(start) = self.best_ask_tick {
            for t in start..MAX_TICK {
                let m = self.asks[t];
                if m > 0 {
                    asks.push((tick_to_price(t), micro_to_size(m)));
                    if asks.len() >= max_levels {
                        break;
                    }
                }
            }
        }

        let bid_top5: f64 = bids.iter().take(5).map(|(_, s)| *s).sum();
        let ask_top5: f64 = asks.iter().take(5).map(|(_, s)| *s).sum();
        let imbalance = if bid_top5 + ask_top5 > 0.0 {
            (bid_top5 - ask_top5) / (bid_top5 + ask_top5)
        } else {
            0.0
        };

        BookView {
            asset_id: self.asset_id.clone(),
            market_id: self.market_id.clone(),
            tick_size: tick_to_price(self.tick_size_ticks),
            best_bid: self.best_bid(),
            best_ask: self.best_ask(),
            mid: self.mid(),
            spread: self.spread(),
            imbalance,
            last_trade_price: self.last_trade_price,
            last_trade_side: self.last_trade_side.clone(),
            timestamp_ms: self.last_timestamp_ms,
            sequence: self.sequence,
            bids,
            asks,
        }
    }

    /// Simulate sweeping resting liquidity across multiple price levels.
    /// `is_buy = true` sweeps asks upward; `is_buy = false` sweeps bids downward.
    pub fn simulate_sweep(
        &self,
        is_buy: bool,
        target_usd: Option<f64>,
        target_shares: Option<f64>,
        limit_price: Option<f64>,
    ) -> SweepQuote {
        let mut remaining_usd = target_usd.unwrap_or(f64::INFINITY);
        let mut remaining_shares = target_shares.unwrap_or(f64::INFINITY);
        let mut total_shares = 0.0;
        let mut total_usd = 0.0;
        let mut best_price = 0.0;
        let mut worst_price = 0.0;
        let mut legs = Vec::new();

        if is_buy {
            let max_t = limit_price.and_then(price_to_tick).unwrap_or(MAX_TICK - 1);
            if let Some(start) = self.best_ask_tick {
                for t in start..=max_t {
                    let avail = micro_to_size(self.asks[t]);
                    if avail <= 0.0 {
                        continue;
                    }
                    let p = tick_to_price(t);
                    if best_price == 0.0 {
                        best_price = p;
                    }
                    let cap_by_usd = remaining_usd / p;
                    let take = avail.min(remaining_shares).min(cap_by_usd);
                    if take <= 1e-6 {
                        break;
                    }
                    let cost = take * p;
                    total_shares += take;
                    total_usd += cost;
                    remaining_shares -= take;
                    remaining_usd -= cost;
                    worst_price = p;
                    legs.push((p, take));
                    if remaining_shares <= 1e-6 || remaining_usd <= 1e-6 {
                        break;
                    }
                }
            }
        } else {
            let min_t = limit_price.and_then(price_to_tick).unwrap_or(1);
            if let Some(start) = self.best_bid_tick {
                for t in (min_t..=start).rev() {
                    let avail = micro_to_size(self.bids[t]);
                    if avail <= 0.0 {
                        continue;
                    }
                    let p = tick_to_price(t);
                    if best_price == 0.0 {
                        best_price = p;
                    }
                    let cap_by_usd = remaining_usd / p;
                    let take = avail.min(remaining_shares).min(cap_by_usd);
                    if take <= 1e-6 {
                        break;
                    }
                    let proceeds = take * p;
                    total_shares += take;
                    total_usd += proceeds;
                    remaining_shares -= take;
                    remaining_usd -= proceeds;
                    worst_price = p;
                    legs.push((p, take));
                    if remaining_shares <= 1e-6 || remaining_usd <= 1e-6 {
                        break;
                    }
                }
            }
        }

        let vwap = if total_shares > 0.0 {
            total_usd / total_shares
        } else {
            0.0
        };
        let slippage_bps = if best_price > 0.0 && vwap > 0.0 {
            ((vwap - best_price).abs() / best_price) * 10_000.0
        } else {
            0.0
        };
        let filled_completely = match (target_usd, target_shares) {
            (Some(_), _) => remaining_usd <= 0.01,
            (None, Some(_)) => remaining_shares <= 1e-4,
            (None, None) => false,
        };

        SweepQuote {
            side: if is_buy { "BUY".into() } else { "SELL".into() },
            total_shares,
            total_notional_usd: total_usd,
            vwap,
            worst_price,
            best_price,
            slippage_bps,
            levels_consumed: legs.len(),
            filled_completely,
            legs,
        }
    }
}

/// Polymarket dynamic taker fee formula (CLOB V2):
/// Fee = shares * category_rate * p * (1 - p), where default crypto/sports curve peaks at p=0.5.
/// If market has feesEnabled=false, protocol fee is 0.
pub fn estimate_protocol_fee_usd(shares: f64, price: f64, is_maker: bool, fees_enabled: bool) -> f64 {
    if is_maker || !fees_enabled || shares <= 0.0 || price <= 0.0 || price >= 1.0 {
        return 0.0;
    }
    // Conservative standard curve factor (0.0625 -> ~1.56% max at p=0.50, tapering to ~0 near 0/1)
    shares * 0.0625 * price * (1.0 - price)
}

/// Estimate transparent Builder Fee (stacked on top of protocol fee per CLOB V2 rules).
pub fn estimate_builder_fee_usd(notional_usd: f64, is_maker: bool, maker_bps: u32, taker_bps: u32) -> f64 {
    let bps = if is_maker { maker_bps.min(50) } else { taker_bps.min(100) };
    notional_usd * (bps as f64) / 10_000.0
}

/// Desk-style Green-Up / Trade-Out / Free-Roll calculator for a binary outcome position.
pub fn calculate_green_up(
    shares: f64,
    avg_entry: f64,
    best_bid: f64,
    opposite_best_ask: Option<f64>,
) -> GreenUpQuote {
    let cost = shares * avg_entry;
    let close_cash_out = shares * best_bid;
    let close_locked_pnl = close_cash_out - cost;

    let can_free_roll = best_bid > avg_entry && best_bid > 0.0 && shares > 0.0;
    let free_roll_shares_to_sell = if can_free_roll {
        (cost / best_bid).min(shares)
    } else {
        0.0
    };
    let free_roll_remaining_shares = (shares - free_roll_shares_to_sell).max(0.0);

    let (hedge_no_price, hedge_no_locked_pnl) = match opposite_best_ask {
        Some(a_no) if a_no > 0.0 && a_no < 1.0 => {
            // Buying `shares` of NO at `a_no` guarantees $1.00/share payout at resolution:
            // Locked P&L = shares * (1 - avg_entry - a_no)
            let pnl = shares * (1.0 - avg_entry - a_no);
            (Some(a_no), Some(pnl))
        }
        _ => (None, None),
    };

    let recommended_action = match hedge_no_locked_pnl {
        Some(h_pnl) if h_pnl > close_locked_pnl + 0.01 => "hedge_opposite".to_string(),
        _ => "sell_close".to_string(),
    };

    GreenUpQuote {
        shares,
        avg_entry,
        close_price: best_bid,
        close_locked_pnl,
        close_cash_out,
        free_roll_shares_to_sell,
        free_roll_remaining_shares,
        can_free_roll,
        hedge_no_price,
        hedge_no_locked_pnl,
        recommended_action,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_book_snapshot_delta_and_sweep() {
        let mut book = FastOrderBook::new("1".into(), "0xabc".into(), 0.01);
        book.apply_snapshot(
            &[(0.48, 100.0), (0.47, 200.0)],
            &[(0.50, 50.0), (0.52, 100.0), (0.55, 100.0)],
            1000,
            Some("h1".into()),
        );
        assert_eq!(book.best_bid(), Some(0.48));
        assert_eq!(book.best_ask(), Some(0.50));
        assert!((book.mid().unwrap() - 0.49).abs() < 1e-6);

        // Remove best ask -> should promote 0.52 cleanly
        book.apply_delta(false, 0.50, 0.0, 1001);
        assert_eq!(book.best_ask(), Some(0.52));

        // Add tighter bid
        book.apply_delta(true, 0.49, 75.0, 1002);
        assert_eq!(book.best_bid(), Some(0.49));

        // Sweep $80 across asks: 100 @ 0.52 ($52) + ~50.909 @ 0.55 ($28)
        let q = book.simulate_sweep(true, Some(80.0), None, Some(0.60));
        assert_eq!(q.levels_consumed, 2);
        assert!(q.filled_completely);
        assert!((q.total_notional_usd - 80.0).abs() < 1e-3);
        assert!(q.vwap > 0.52 && q.vwap < 0.55);
    }

    #[test]
    fn green_up_and_free_roll_math() {
        // Bought 100 YES @ 0.40 ($40 cost), now bid is 0.80 ($80 value -> +$40 locked profit)
        let g = calculate_green_up(100.0, 0.40, 0.80, Some(0.18));
        assert!((g.close_locked_pnl - 40.0).abs() < 1e-6);
        assert!(g.can_free_roll);
        assert!((g.free_roll_shares_to_sell - 50.0).abs() < 1e-6);
        assert!((g.free_roll_remaining_shares - 50.0).abs() < 1e-6);
        // Hedge NO @ 0.18 => 100 * (1 - 0.40 - 0.18) = $42 > $40 -> recommends hedge_opposite!
        assert!((g.hedge_no_locked_pnl.unwrap() - 42.0).abs() < 1e-6);
        assert_eq!(g.recommended_action, "hedge_opposite");
    }
}
