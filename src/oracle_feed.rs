use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use chrono::Utc;
use dashmap::DashMap;
use futures::StreamExt;
use parking_lot::RwLock;
use polymarket_client_sdk_v2::rtds::Client as RtdsClient;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, warn};

pub const ORACLE_BUFFER_WINDOW_MS: i64 = 10 * 60 * 1000;

/// Default Binance USD-M futures combined-stream base (single source of truth —
/// `lib.rs` and `clear_hot_patch` must reference these, not duplicate the literals).
pub const DEFAULT_FUTURES_WS_BASE: &str = "wss://fstream.binance.com/stream?streams=";
/// Default Binance spot combined-stream base, used only as an explicit fallback.
pub const DEFAULT_SPOT_WS_BASE: &str = "wss://stream.binance.com:9443/stream?streams=";
pub const ORACLE_MAX_SAMPLES_PER_SYMBOL: usize = 2000;
pub const ORACLE_STALE_MS: i64 = 10_000;
pub const FAST_LEAD_MIN_SAMPLE_MS: i64 = 50;
pub const FAST_LEAD_MAX_SAMPLES: usize = 12_000;
pub const SUPPORTED_CHAINLINK_SYMBOLS: &[&str] = &[
    "btc/usd",
    "eth/usd",
    "sol/usd",
    "xrp/usd",
    "doge/usd",
    "bnb/usd",
    "hype/usd",
];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OracleSample {
    pub timestamp_ms: i64,
    pub price: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleSnapshot {
    pub symbol: String,
    pub spot: f64,
    pub twap_60s: f64,
    pub last_timestamp_ms: i64,
    pub stale: bool,
    pub binance_mid: Option<f64>,
    pub binance_ts_ms: Option<i64>,
    pub fast_spot: Option<f64>,
    pub fast_ts_ms: Option<i64>,
    pub fast_wire_lag_ms: Option<i64>,
    /// Shared, immutable history slices. `Arc` rather than `Vec` so the render loop
    /// (up to 3 calls/frame at 8 ms) never deep-copies 12 000 samples per call.
    #[serde(skip)]
    pub fast_history: Arc<Vec<(i64, f64)>>,
    #[serde(skip)]
    pub history: Arc<Vec<(i64, f64)>>,
}

#[derive(Clone)]
struct HistoryCacheEntry {
    version: u64,
    history: Arc<Vec<(i64, f64)>>,
    fast_history: Arc<Vec<(i64, f64)>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OracleEvent {
    pub symbol: String,
}

pub fn normalize_timestamp_ms(ts: i64) -> i64 {
    if ts > 0 && ts < 1_000_000_000_000 {
        ts.saturating_mul(1000)
    } else {
        ts
    }
}

/// Time-weighted average price over `[now_ms - 60_000, now_ms]` using piecewise-constant
/// Chainlink oracle updates (seeded by the latest sample at or before `now_ms - 60_000`).
pub fn compute_twap_60s(samples: &VecDeque<OracleSample>, now_ms: i64) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let window_start = now_ms - 60_000;
    let seed = samples
        .iter()
        .rfind(|s| s.timestamp_ms <= window_start)
        .copied();

    let mut prev_t = window_start;
    let mut prev_price = seed.map(|s| s.price);
    let mut weighted_sum = 0.0_f64;
    let mut total_dt = 0_i64;

    for s in samples {
        if s.timestamp_ms <= window_start || s.timestamp_ms > now_ms {
            continue;
        }
        if let Some(p) = prev_price {
            let dt = (s.timestamp_ms - prev_t).max(0);
            weighted_sum += p * (dt as f64);
            total_dt += dt;
        }
        prev_t = s.timestamp_ms;
        prev_price = Some(s.price);
    }

    if let Some(p) = prev_price {
        let tail_dt = (now_ms - prev_t).max(0);
        weighted_sum += p * (tail_dt as f64);
        total_dt += tail_dt;
    }

    if total_dt > 0 {
        Some(weighted_sum / (total_dt as f64))
    } else {
        samples.back().map(|s| s.price)
    }
}

pub struct OracleFeed {
    samples: Arc<DashMap<String, VecDeque<OracleSample>>>,
    /// USD-M **futures** bookTicker mid, `(price, venue_ts_ms)`. Basis is calibrated
    /// against this series only.
    binance_leads: Arc<DashMap<String, (f64, i64)>>,
    /// **Spot** bookTicker mid, used only as an explicit fallback. Kept separate so a
    /// perp premium is never applied to a spot price (or vice versa).
    spot_leads: Arc<DashMap<String, (f64, i64)>>,
    binance_basis: Arc<DashMap<String, f64>>,
    binance_raw_history: Arc<DashMap<String, VecDeque<OracleSample>>>,
    spot_raw_history: Arc<DashMap<String, VecDeque<OracleSample>>>,
    fast_wire_lags: Arc<DashMap<String, i64>>,
    /// Best (lowest) observed `local_recv_ms - venue_event_ms` per symbol. Wire lag is
    /// reported as the excess over this baseline, which makes the badge independent of
    /// absolute wall-clock skew on the user's machine.
    wire_offset_min: Arc<DashMap<String, i64>>,
    last_binance_emit_ms: Arc<DashMap<String, i64>>,
    /// Memoized `(version, history, fast_history)` per symbol so repeated snapshot()
    /// calls between ticks reuse the same `Arc` instead of rebuilding the vectors.
    history_cache: Arc<DashMap<String, HistoryCacheEntry>>,
    backfilled_symbols: Arc<RwLock<HashSet<String>>>,
    watched: Arc<RwLock<HashSet<String>>>,
    watch_tx: mpsc::UnboundedSender<HashSet<String>>,
    events_tx: broadcast::Sender<OracleEvent>,
    chainlink_disconnected: Arc<AtomicBool>,
    futures_ws_base: Arc<RwLock<String>>,
    spot_ws_base: Arc<RwLock<String>>,
    endpoint_gen: Arc<AtomicU64>,
}

/// A Binance bookTicker tick's source venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinanceVenue {
    Futures,
    Spot,
}

/// A fast (Binance) tick older than this is not shown as a live lead at all — the
/// websocket died and we must not keep painting the last price as current.
pub const FAST_LEAD_STALE_MS: i64 = 5_000;

impl OracleFeed {
    pub fn spawn() -> Arc<Self> {
        let (feed, watch_rx) = Self::build();
        tokio::spawn(run_oracle_coordinator(feed.clone(), watch_rx));
        feed
    }

    pub fn new_detached() -> Arc<Self> {
        let (feed, _rx) = Self::build();
        feed
    }

    fn build() -> (Arc<Self>, mpsc::UnboundedReceiver<HashSet<String>>) {
        let (watch_tx, watch_rx) = mpsc::unbounded_channel();
        let (events_tx, _) = broadcast::channel(256);
        let feed = Arc::new(Self {
            samples: Arc::new(DashMap::new()),
            binance_leads: Arc::new(DashMap::new()),
            spot_leads: Arc::new(DashMap::new()),
            binance_basis: Arc::new(DashMap::new()),
            binance_raw_history: Arc::new(DashMap::new()),
            spot_raw_history: Arc::new(DashMap::new()),
            fast_wire_lags: Arc::new(DashMap::new()),
            wire_offset_min: Arc::new(DashMap::new()),
            last_binance_emit_ms: Arc::new(DashMap::new()),
            history_cache: Arc::new(DashMap::new()),
            backfilled_symbols: Arc::new(RwLock::new(HashSet::new())),
            watched: Arc::new(RwLock::new(HashSet::new())),
            watch_tx,
            events_tx,
            chainlink_disconnected: Arc::new(AtomicBool::new(false)),
            futures_ws_base: Arc::new(RwLock::new(DEFAULT_FUTURES_WS_BASE.to_string())),
            spot_ws_base: Arc::new(RwLock::new(DEFAULT_SPOT_WS_BASE.to_string())),
            endpoint_gen: Arc::new(AtomicU64::new(0)),
        });
        (feed, watch_rx)
    }

    /// Hot-reload Binance live-lead WebSocket base URLs without restarting the app.
    pub fn update_ws_endpoints(&self, futures_base: Option<&str>, spot_base: Option<&str>) {
        let mut changed = false;
        if let Some(raw) = futures_base {
            let trimmed = raw.trim();
            if trimmed.starts_with("wss://") {
                let mut g = self.futures_ws_base.write();
                if *g != trimmed {
                    *g = trimmed.to_string();
                    changed = true;
                }
            }
        }
        if let Some(raw) = spot_base {
            let trimmed = raw.trim();
            if trimmed.starts_with("wss://") {
                let mut g = self.spot_ws_base.write();
                if *g != trimmed {
                    *g = trimmed.to_string();
                    changed = true;
                }
            }
        }
        if changed {
            self.endpoint_gen.fetch_add(1, Ordering::Relaxed);
            let current = self.watched.read().clone();
            if !current.is_empty() {
                let _ = self.watch_tx.send(current);
            }
        }
    }

    /// Update watched Chainlink symbols (e.g. `{"btc/usd", "eth/usd"}`).
    /// Passing an empty set stops active WebSocket tasks so zero connections stay open when idle.
    pub fn set_watched_symbols(&self, symbols: HashSet<String>) {
        let normalized: HashSet<String> = symbols
            .into_iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| SUPPORTED_CHAINLINK_SYMBOLS.contains(&s.as_str()))
            .collect();
        {
            let mut guard = self.watched.write();
            if *guard == normalized {
                return;
            }
            *guard = normalized.clone();
        }
        let _ = self.watch_tx.send(normalized);
    }

    pub fn watched_count(&self) -> usize {
        self.watched.read().len()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<OracleEvent> {
        self.events_tx.subscribe()
    }

    pub fn record_chainlink_tick(&self, symbol: &str, raw_ts_ms: i64, price: f64) {
        if !price.is_finite() || price <= 0.0 {
            return;
        }
        let sym = symbol.trim().to_ascii_lowercase();
        if !SUPPORTED_CHAINLINK_SYMBOLS.contains(&sym.as_str()) {
            return;
        }
        let ts_ms = normalize_timestamp_ms(raw_ts_ms);
        let mut q = self.samples.entry(sym.clone()).or_insert_with(VecDeque::new);
        if let Some(last) = q.back_mut()
            && ts_ms < last.timestamp_ms
        {
            return;
        }
        if q.back().is_some_and(|last| last.timestamp_ms == ts_ms) {
            if let Some(last) = q.back_mut() {
                last.price = price;
            }
        } else {
            q.push_back(OracleSample {
                timestamp_ms: ts_ms,
                price,
            });
        }
        let cutoff = ts_ms - ORACLE_BUFFER_WINDOW_MS;
        while q.front().is_some_and(|s| s.timestamp_ms < cutoff) {
            q.pop_front();
        }
        while q.len() > ORACLE_MAX_SAMPLES_PER_SYMBOL {
            q.pop_front();
        }
        drop(q);
        // Time-aligned basis calibration: compare Chainlink(cl_ts) against Binance(cl_ts)
        // so Exchange/Oracle lag does not distort the spread during sharp price spikes.
        let aligned_binance = self
            .binance_raw_history
            .get(&sym)
            .and_then(|rq| lookup_price_at(&rq, ts_ms, 30_000))
            .or_else(|| {
                self.binance_leads
                    .get(&sym)
                    .filter(|b| (ts_ms - b.1).abs() <= 30_000)
                    .map(|b| b.0)
            });
        if let Some(bp_at_cl) = aligned_binance {
            let raw_basis = price - bp_at_cl;
            self.binance_basis
                .entry(sym.clone())
                .and_modify(|prev| *prev = 0.20 * raw_basis + 0.80 * *prev)
                .or_insert(raw_basis);
        }
        self.chainlink_disconnected.store(false, Ordering::Relaxed);
        let _ = self.events_tx.send(OracleEvent { symbol: sym });
    }

    pub fn record_binance_tick(&self, binance_sym: &str, price: f64, raw_ts_ms: i64) {
        self.record_binance_tick_with_wire_lag(binance_sym, price, raw_ts_ms, None);
    }

    /// Futures tick (basis-calibrated lead).
    pub fn record_binance_tick_with_wire_lag(
        &self,
        binance_sym: &str,
        price: f64,
        raw_ts_ms: i64,
        event_ts_ms: Option<i64>,
    ) {
        self.record_binance_tick_from(
            binance_sym,
            price,
            raw_ts_ms,
            event_ts_ms,
            BinanceVenue::Futures,
        );
    }

    pub fn record_binance_tick_from(
        &self,
        binance_sym: &str,
        price: f64,
        raw_ts_ms: i64,
        event_ts_ms: Option<i64>,
        venue: BinanceVenue,
    ) {
        if !price.is_finite() || price <= 0.0 {
            return;
        }
        let Some(cl_sym) = binance_to_chainlink_symbol(binance_sym) else {
            return;
        };
        let ts_ms = normalize_timestamp_ms(raw_ts_ms);
        // Venue time, not local receive time: bucketing on the local clock shifts with
        // NTP drift and misaligns the basis window against Chainlink timestamps.
        let venue_ts = event_ts_ms
            .map(normalize_timestamp_ms)
            .filter(|t| *t > 0 && (*t - ts_ms).abs() <= 60_000)
            .unwrap_or(ts_ms);
        if let Some(ev_ms) = event_ts_ms {
            let ev_ms = normalize_timestamp_ms(ev_ms);
            // Raw offset folds together clock skew + network latency. Track the best
            // observed offset and report the *excess*, which removes the (constant)
            // skew term. A machine whose clock is 5 s fast then reports ~0 ms, not
            // a permanent "10 000 ms, feed broken".
            let raw_offset = ts_ms.saturating_sub(ev_ms);
            let baseline = {
                let mut e = self.wire_offset_min.entry(cl_sym.to_string()).or_insert(raw_offset);
                // Creep upward slowly so the baseline can follow real clock drift.
                *e = (*e + 1).min(raw_offset);
                *e
            };
            let lag = raw_offset.saturating_sub(baseline).clamp(0, 10_000);
            self.fast_wire_lags
                .entry(cl_sym.to_string())
                .and_modify(|prev| *prev = ((lag * 3) + (*prev * 7)) / 10)
                .or_insert(lag);
        }
        match venue {
            BinanceVenue::Futures => {
                self.binance_leads
                    .insert(cl_sym.to_string(), (price, venue_ts));
            }
            BinanceVenue::Spot => {
                self.spot_leads
                    .insert(cl_sym.to_string(), (price, venue_ts));
            }
        }
        let bucket_ts = venue_ts - venue_ts.rem_euclid(FAST_LEAD_MIN_SAMPLE_MS);
        let history = match venue {
            BinanceVenue::Futures => &self.binance_raw_history,
            BinanceVenue::Spot => &self.spot_raw_history,
        };
        let mut rq = history
            .entry(cl_sym.to_string())
            .or_insert_with(VecDeque::new);
        if rq.back().is_some_and(|last| bucket_ts < last.timestamp_ms) {
            // Ignore out-of-order ticks.
        } else if rq
            .back()
            .is_some_and(|last| last.timestamp_ms == bucket_ts)
        {
            if let Some(last) = rq.back_mut() {
                last.price = price;
            }
        } else {
            rq.push_back(OracleSample {
                timestamp_ms: bucket_ts,
                price,
            });
        }
        let cutoff = bucket_ts - ORACLE_BUFFER_WINDOW_MS;
        while rq.front().is_some_and(|s| s.timestamp_ms < cutoff) {
            rq.pop_front();
        }
        while rq.len() > FAST_LEAD_MAX_SAMPLES {
            rq.pop_front();
        }
        drop(rq);

        let is_watched = {
            let w = self.watched.read();
            w.contains(cl_sym)
        };
        if is_watched {
            let mut should_emit = true;
            if let Some(prev) = self.last_binance_emit_ms.get(cl_sym)
                && venue_ts - *prev < 8
            {
                should_emit = false;
            }
            if should_emit {
                self.last_binance_emit_ms
                    .insert(cl_sym.to_string(), venue_ts);
                let _ = self.events_tx.send(OracleEvent {
                    symbol: cl_sym.to_string(),
                });
            }
        }
    }

    /// Merge pure Binance USD-M Futures history into `binance_raw_history` without touching Spot.
    pub fn merge_futures_backfill(&self, cl_sym: &str, samples: Vec<OracleSample>) {
        if samples.is_empty() {
            return;
        }
        let mut rq = self
            .binance_raw_history
            .entry(cl_sym.to_string())
            .or_insert_with(VecDeque::new);
        let earliest_existing = rq.front().map(|s| s.timestamp_ms).unwrap_or(i64::MAX);
        let now_cutoff = Utc::now().timestamp_millis() - ORACLE_BUFFER_WINDOW_MS;
        let mut older: Vec<OracleSample> = samples
            .into_iter()
            .filter(|s| s.price.is_finite() && s.price > 0.0 && s.timestamp_ms >= now_cutoff && s.timestamp_ms < earliest_existing)
            .collect();
        if older.is_empty() {
            return;
        }
        older.sort_by_key(|s| s.timestamp_ms);
        older.dedup_by_key(|s| s.timestamp_ms);
        for s in older.into_iter().rev() {
            rq.push_front(s);
        }
        while rq.len() > FAST_LEAD_MAX_SAMPLES {
            rq.pop_front();
        }
        drop(rq);
        let _ = self.events_tx.send(OracleEvent {
            symbol: cl_sym.to_string(),
        });
    }

    /// Merge Binance **spot** 1 s klines into the spot fallback series.
    ///
    /// These are never mixed with `binance_raw_history` (futures): the basis is a perp
    /// premium, so applying it to spot data (or vice versa) silently shifts the whole
    /// fast-lead line.
    pub fn merge_binance_backfill(&self, cl_sym: &str, samples: Vec<OracleSample>) {
        if samples.is_empty() {
            return;
        }
        let mut rq = self
            .spot_raw_history
            .entry(cl_sym.to_string())
            .or_insert_with(VecDeque::new);
        let earliest_existing = rq.front().map(|s| s.timestamp_ms).unwrap_or(i64::MAX);
        let now_cutoff = Utc::now().timestamp_millis() - ORACLE_BUFFER_WINDOW_MS;
        let mut older: Vec<OracleSample> = samples
            .into_iter()
            .filter(|s| s.price.is_finite() && s.price > 0.0 && s.timestamp_ms >= now_cutoff && s.timestamp_ms < earliest_existing)
            .collect();
        if older.is_empty() {
            return;
        }
        older.sort_by_key(|s| s.timestamp_ms);
        older.dedup_by_key(|s| s.timestamp_ms);
        for s in older.into_iter().rev() {
            rq.push_front(s);
        }
        while rq.len() > FAST_LEAD_MAX_SAMPLES {
            rq.pop_front();
        }
        drop(rq);
        let _ = self.events_tx.send(OracleEvent {
            symbol: cl_sym.to_string(),
        });
    }

    pub fn snapshot(&self, symbol: &str) -> Option<OracleSnapshot> {
        self.snapshot_at(symbol, Utc::now().timestamp_millis())
    }

    pub fn snapshot_at(&self, symbol: &str, now_ms: i64) -> Option<OracleSnapshot> {
        let sym = symbol.trim().to_ascii_lowercase();
        let binance = self.binance_leads.get(&sym).map(|e| *e);
        let spot = self.spot_leads.get(&sym).map(|e| *e);
        let basis_opt = self.binance_basis.get(&sym).map(|e| *e);

        // Venue selection: a fresh futures tick always wins (it is the basis-calibrated
        // series that matches how Chainlink is quoted). Spot is an explicit fallback
        // and is NEVER basis-adjusted, because the basis is a perp premium.
        //
        // Freshness is measured against venue time, so subtract the per-symbol
        // `local_recv - venue_event` baseline first. Without that, a workstation whose
        // clock runs fast would classify a perfectly live feed as dead.
        let skew_ms = self.wire_offset_min.get(&sym).map(|e| *e).unwrap_or(0);
        let is_fresh = |ts: i64| {
            now_ms
                .saturating_sub(ts)
                .saturating_sub(skew_ms)
                <= FAST_LEAD_STALE_MS
        };
        let fut_fresh = binance.filter(|(_, ts)| is_fresh(*ts));
        let spot_fresh = spot.filter(|(_, ts)| is_fresh(*ts));
        let (fast_spot, fast_ts_ms, using_spot) = match (fut_fresh, spot_fresh) {
            (Some((bp, ts)), _) => {
                let adjusted = basis_opt.map(|b| bp + b).unwrap_or(bp);
                (
                    (adjusted.is_finite() && adjusted > 0.0).then_some(adjusted),
                    Some(ts),
                    false,
                )
            }
            (None, Some((sp, ts))) => (
                (sp.is_finite() && sp > 0.0).then_some(sp),
                Some(ts),
                true,
            ),
            (None, None) => (None, None, false),
        };
        // ---- memoized history vectors -------------------------------------------------
        // Version is a cheap, order-sensitive fingerprint of the selected series + basis.
        let cl_version = self
            .samples
            .get(&sym)
            .map(|q| {
                let last_ts = q.back().map(|s| s.timestamp_ms).unwrap_or(0);
                ((q.len() as u64) << 40) ^ (last_ts as u64)
            })
            .unwrap_or(0);
        let active_fast_hist = if using_spot {
            &self.spot_raw_history
        } else {
            &self.binance_raw_history
        };
        let fast_version = active_fast_hist
            .get(&sym)
            .map(|q| {
                let last_ts = q.back().map(|s| s.timestamp_ms).unwrap_or(0);
                ((q.len() as u64) << 40) ^ (last_ts as u64)
            })
            .unwrap_or(0)
            ^ basis_opt.map(|b| b.to_bits().rotate_left(17)).unwrap_or(0)
            ^ (using_spot as u64) << 63;
        let version = cl_version.rotate_left(7) ^ fast_version;

        // NOTE: bind the cache hit to an owned value first. Matching directly on
        // `self.history_cache.get(..)` would hold the shard read guard across the
        // `insert` below and deadlock on every cache miss.
        let cached_hit = self
            .history_cache
            .get(&sym)
            .filter(|entry| entry.version == version)
            .map(|entry| (entry.history.clone(), entry.fast_history.clone()));

        let (history, fast_history) = match cached_hit {
            Some(hit) => hit,
            None => {
                let fast: Arc<Vec<(i64, f64)>> = Arc::new(match active_fast_hist.get(&sym) {
                    Some(q) => q
                        .iter()
                        .filter_map(|s| {
                            // Spot series is never basis-adjusted.
                            let adj = if using_spot {
                                s.price
                            } else {
                                s.price + basis_opt.unwrap_or(0.0)
                            };
                            (adj.is_finite() && adj > 0.0).then_some((s.timestamp_ms, adj))
                        })
                        .collect(),
                    None => Vec::new(),
                });
                let cl: Arc<Vec<(i64, f64)>> = Arc::new(
                    self.samples
                        .get(&sym)
                        .map(|q| q.iter().map(|s| (s.timestamp_ms, s.price)).collect())
                        .unwrap_or_default(),
                );
                if self.history_cache.len() > 64 {
                    self.history_cache.clear();
                }
                self.history_cache.insert(
                    sym.clone(),
                    HistoryCacheEntry {
                        version,
                        history: cl.clone(),
                        fast_history: fast.clone(),
                    },
                );
                (cl, fast)
            }
        };

        if let Some(q) = self.samples.get(&sym)
            && let Some(last) = q.back()
        {
            let stale = now_ms.saturating_sub(last.timestamp_ms) > ORACLE_STALE_MS
                || self.chainlink_disconnected.load(Ordering::Relaxed);
            let twap_end = if stale { last.timestamp_ms } else { now_ms };
            let twap_60s = compute_twap_60s(&q, twap_end).unwrap_or(last.price);
            return Some(OracleSnapshot {
                symbol: sym.clone(),
                spot: last.price,
                twap_60s,
                last_timestamp_ms: last.timestamp_ms,
                stale,
                binance_mid: binance.map(|(p, _)| p),
                binance_ts_ms: binance.map(|(_, t)| t),
                fast_spot,
                fast_ts_ms,
                fast_wire_lag_ms: fast_spot
                    .and_then(|_| self.fast_wire_lags.get(&sym).map(|e| *e)),
                fast_history,
                history,
            });
        }
        let _ = &history;
        let (bp, bt) = binance.or(spot)?;
        let anchor = fast_spot.unwrap_or(bp);
        Some(OracleSnapshot {
            symbol: sym.clone(),
            // No Chainlink data yet: report the same basis-adjusted fast price that the
            // chart draws, so the label and the series never disagree.
            spot: anchor,
            twap_60s: anchor,
            last_timestamp_ms: bt,
            stale: true,
            binance_mid: Some(bp),
            binance_ts_ms: Some(bt),
            fast_spot,
            fast_ts_ms,
            fast_wire_lag_ms: fast_spot
                .and_then(|_| self.fast_wire_lags.get(&sym).map(|e| *e)),
            fast_history,
            history,
        })
    }
}

fn lookup_price_at(q: &VecDeque<OracleSample>, target_ts_ms: i64, max_age_ms: i64) -> Option<f64> {
    if q.is_empty() {
        return None;
    }
    let pos = q.partition_point(|s| s.timestamp_ms <= target_ts_ms);
    if pos == 0 {
        let first = q.front()?;
        return ((first.timestamp_ms - target_ts_ms).abs() <= max_age_ms).then_some(first.price);
    }
    if pos == q.len() {
        let last = q.back()?;
        return ((target_ts_ms - last.timestamp_ms).abs() <= max_age_ms).then_some(last.price);
    }
    let prev = &q[pos - 1];
    let next = &q[pos];
    let gap = next.timestamp_ms - prev.timestamp_ms;
    if gap > 0 && gap <= 5_000 {
        let frac = ((target_ts_ms - prev.timestamp_ms) as f64 / gap as f64).clamp(0.0, 1.0);
        return Some(prev.price + (next.price - prev.price) * frac);
    }
    let d_prev = (target_ts_ms - prev.timestamp_ms).abs();
    let d_next = (next.timestamp_ms - target_ts_ms).abs();
    if d_prev <= d_next {
        (d_prev <= max_age_ms).then_some(prev.price)
    } else {
        (d_next <= max_age_ms).then_some(next.price)
    }
}

fn binance_to_chainlink_symbol(sym: &str) -> Option<&'static str> {
    match sym.trim().to_ascii_lowercase().as_str() {
        "btcusdt" => Some("btc/usd"),
        "ethusdt" => Some("eth/usd"),
        "solusdt" => Some("sol/usd"),
        "xrpusdt" => Some("xrp/usd"),
        "dogeusdt" => Some("doge/usd"),
        "bnbusdt" => Some("bnb/usd"),
        "hypeusdt" => Some("hype/usd"),
        _ => None,
    }
}

fn chainlink_to_binance_symbol(sym: &str) -> Option<&'static str> {
    match sym.trim().to_ascii_lowercase().as_str() {
        "btc/usd" => Some("BTCUSDT"),
        "eth/usd" => Some("ETHUSDT"),
        "sol/usd" => Some("SOLUSDT"),
        "xrp/usd" => Some("XRPUSDT"),
        "doge/usd" => Some("DOGEUSDT"),
        "bnb/usd" => Some("BNBUSDT"),
        _ => None,
    }
}

async fn run_oracle_coordinator(
    feed: Arc<OracleFeed>,
    mut watch_rx: mpsc::UnboundedReceiver<HashSet<String>>,
) {
    let mut chainlink_task: Option<JoinHandle<()>> = None;
    let mut binance_task: Option<JoinHandle<()>> = None;
    let mut active_streams: Vec<String> = Vec::new();
    let mut active_ep_gen: u64 = feed.endpoint_gen.load(Ordering::Relaxed);

    while let Some(watched) = watch_rx.recv().await {
        let want_active = !watched.is_empty();
        let cur_ep_gen = feed.endpoint_gen.load(Ordering::Relaxed);
        if want_active {
            if chainlink_task.is_none() {
                let f1 = feed.clone();
                chainlink_task = Some(tokio::spawn(run_chainlink_loop(f1)));
            }
            let mut want_streams: Vec<String> = watched
                .iter()
                .filter_map(|s| {
                    chainlink_to_binance_symbol(s)
                        .map(|b| format!("{}@bookTicker", b.to_ascii_lowercase()))
                })
                .collect();
            want_streams.sort();
            want_streams.dedup();
            if !want_streams.is_empty()
                && (binance_task.is_none()
                    || want_streams != active_streams
                    || cur_ep_gen != active_ep_gen)
            {
                if let Some(h) = binance_task.take() {
                    h.abort();
                }
                active_streams = want_streams.clone();
                active_ep_gen = cur_ep_gen;
                let f2 = feed.clone();
                binance_task = Some(tokio::spawn(run_binance_lead_loop(f2, want_streams)));
            }
            for sym in watched {
                let inserted = {
                    let mut bf = feed.backfilled_symbols.write();
                    bf.insert(sym.clone())
                };
                if inserted {
                    let f = feed.clone();
                    tokio::spawn(backfill_binance_klines(f, sym));
                }
            }
        } else {
            if let Some(h) = chainlink_task.take() {
                h.abort();
            }
            if let Some(h) = binance_task.take() {
                h.abort();
            }
            active_streams.clear();
            feed.chainlink_disconnected.store(false, Ordering::Relaxed);
        }
    }

    if let Some(h) = chainlink_task {
        h.abort();
    }
    if let Some(h) = binance_task {
        h.abort();
    }
}

async fn run_chainlink_loop(feed: Arc<OracleFeed>) {
    let mut backoff_ms = 500_u64;
    loop {
        let client = RtdsClient::default();
        match client.subscribe_chainlink_prices(None) {
            Ok(stream) => {
                feed.chainlink_disconnected.store(false, Ordering::Relaxed);
                let mut stream = Box::pin(stream);
                backoff_ms = 500;
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(cp) => {
                            if let Some(val) = cp.value.to_f64() {
                                feed.record_chainlink_tick(&cp.symbol, cp.timestamp, val);
                            }
                        }
                        Err(e) => {
                            debug!("RTDS Chainlink stream item error: {e}");
                        }
                    }
                }
                feed.chainlink_disconnected.store(true, Ordering::Relaxed);
            }
            Err(e) => {
                feed.chainlink_disconnected.store(true, Ordering::Relaxed);
                warn!("RTDS Chainlink subscribe failed: {e}");
            }
        }
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(15_000);
    }
}

/// Outcome of one Binance websocket session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WsSession {
    /// TCP/TLS connect failed — the endpoint is unusable, fall back.
    ConnectFailed,
    /// Connected, but the session ended before any tick arrived.
    ConnectedNoData,
    /// Connected and delivered at least one tick.
    ConnectedWithData,
}

async fn consume_binance_ws(feed: &Arc<OracleFeed>, url: &str, venue: BinanceVenue) -> WsSession {
    let Ok((ws, _)) = connect_async_with_config(url, None, true).await else {
        return WsSession::ConnectFailed;
    };
    let (_, mut read) = ws.split();
    let mut received_any = false;
    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(txt)) => {
                if let Ok(val) = serde_json::from_str::<Value>(&txt) {
                    let data = val.get("data").unwrap_or(&val);
                    let sym = data.get("s").and_then(|s| s.as_str()).unwrap_or("");
                    let bid = data
                        .get("b")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<f64>().ok());
                    let ask = data
                        .get("a")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<f64>().ok());
                    let event_ts = data.get("E").and_then(|v| v.as_i64());
                    if let (Some(b), Some(a)) = (bid, ask)
                        && b > 0.0
                        && a >= b
                    {
                        received_any = true;
                        let now_ms = Utc::now().timestamp_millis();
                        feed.record_binance_tick_from(
                            sym,
                            (b + a) * 0.5,
                            now_ms,
                            event_ts,
                            venue,
                        );
                    }
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }
    if received_any {
        WsSession::ConnectedWithData
    } else {
        WsSession::ConnectedNoData
    }
}

async fn run_binance_lead_loop(feed: Arc<OracleFeed>, streams: Vec<String>) {
    let joined = if streams.is_empty() {
        "btcusdt@bookTicker".to_string()
    } else {
        streams.join("/")
    };
    let (fut_base, spot_base) = (
        feed.futures_ws_base.read().clone(),
        feed.spot_ws_base.read().clone(),
    );
    let build_ws_url = |base: &str| {
        if base.contains("streams=") {
            format!("{base}{joined}")
        } else if base.ends_with('/') {
            format!("{base}stream?streams={joined}")
        } else {
            format!("{base}/stream?streams={joined}")
        }
    };
    let futures_url = build_ws_url(&fut_base);
    let spot_url = build_ws_url(&spot_base);
    let mut backoff_ms = 250_u64;
    loop {
        // Futures is the primary venue. Only fall back to spot when the futures
        // *connection* fails — a normal server-side rotation must not silently switch
        // the price source, because spot and perp are different series (M7/L5).
        let fut = consume_binance_ws(&feed, &futures_url, BinanceVenue::Futures).await;
        match fut {
            WsSession::ConnectFailed => {
                match consume_binance_ws(&feed, &spot_url, BinanceVenue::Spot).await {
                    WsSession::ConnectFailed => {
                        backoff_ms = (backoff_ms * 2).min(15_000);
                    }
                    _ => backoff_ms = 250,
                }
            }
            WsSession::ConnectedNoData => {
                // Endpoint reachable but silent: keep the (small) backoff so a
                // misbehaving server cannot spin us.
                backoff_ms = (backoff_ms * 2).min(15_000);
            }
            WsSession::ConnectedWithData => backoff_ms = 250,
        }
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
    }
}

fn parse_fapi_agg_trades(trades: &[Value], out: &mut Vec<OracleSample>) -> Option<i64> {
    let mut earliest_raw_ms: Option<i64> = None;
    for tr in trades {
        let ts = tr.get("T").and_then(|v| v.as_i64());
        let px = tr
            .get("p")
            .and_then(|v| v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64()));
        if let (Some(t), Some(p)) = (ts, px)
            && p.is_finite()
            && p > 0.0
        {
            let norm_t = normalize_timestamp_ms(t);
            earliest_raw_ms = Some(match earliest_raw_ms {
                Some(prev) => prev.min(norm_t),
                None => norm_t,
            });
            let bucket = norm_t - norm_t.rem_euclid(250);
            if out.last().is_some_and(|s| s.timestamp_ms == bucket) {
                if let Some(last) = out.last_mut() {
                    last.price = p;
                }
            } else {
                out.push(OracleSample {
                    timestamp_ms: bucket,
                    price: p,
                });
            }
        }
    }
    earliest_raw_ms
}

async fn backfill_binance_klines(feed: Arc<OracleFeed>, cl_sym: String) {
    let Some(bin_sym) = chainlink_to_binance_symbol(&cl_sym) else {
        return;
    };
    let fut_klines_url = format!(
        "https://fapi.binance.com/fapi/v1/klines?symbol={bin_sym}&interval=1m&limit=10"
    );
    let fut_trades_url = format!(
        "https://fapi.binance.com/fapi/v1/aggTrades?symbol={bin_sym}&limit=1000"
    );
    let spot_klines_url = format!(
        "https://api.binance.com/api/v3/klines?symbol={bin_sym}&interval=1s&limit=300"
    );
    let ok = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(client) => {
            let (fk_res, ft_res, sk_res) = tokio::join!(
                client.get(&fut_klines_url).send(),
                client.get(&fut_trades_url).send(),
                client.get(&spot_klines_url).send(),
            );

            // 1) Pure USD-M Futures series: fetch 2 pages of aggTrades (~5–7 minutes of
            // 250ms sub-second ticks) so 1M/3M/5M views render smoothly with zero 1m ramps.
            let mut page1_trades: Vec<Value> = Vec::new();
            if let Ok(resp) = ft_res
                && resp.status().is_success()
                && let Ok(trades) = resp.json::<Vec<Value>>().await
            {
                page1_trades = trades;
            }
            let earliest_raw_ms = page1_trades
                .first()
                .and_then(|v| v.get("T"))
                .and_then(|v| v.as_i64())
                .map(normalize_timestamp_ms);
            let mut page2_trades: Vec<Value> = Vec::new();
            if let Some(t1) = earliest_raw_ms
                && t1 > 1
            {
                let page2_url = format!(
                    "https://fapi.binance.com/fapi/v1/aggTrades?symbol={bin_sym}&limit=1000&endTime={}",
                    t1 - 1
                );
                if let Ok(resp2) = client.get(&page2_url).send().await
                    && resp2.status().is_success()
                    && let Ok(trades2) = resp2.json::<Vec<Value>>().await
                {
                    page2_trades = trades2;
                }
            }

            let mut fut_samples: Vec<OracleSample> = Vec::new();
            let _ = parse_fapi_agg_trades(&page2_trades, &mut fut_samples);
            let _ = parse_fapi_agg_trades(&page1_trades, &mut fut_samples);

            let earliest_trade = fut_samples.first().map(|s| s.timestamp_ms).unwrap_or(i64::MAX);
            let now_ms = Utc::now().timestamp_millis();
            if let Ok(resp) = fk_res
                && resp.status().is_success()
                && let Ok(rows) = resp.json::<Vec<Vec<Value>>>().await
            {
                let mut candle_samples = Vec::with_capacity(rows.len());
                for row in rows {
                    let ts = row.get(6).or_else(|| row.first()).and_then(|v| v.as_i64());
                    let close = row
                        .get(4)
                        .and_then(|v| v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64()));
                    if let (Some(t), Some(p)) = (ts, close)
                        && p.is_finite()
                        && p > 0.0
                    {
                        let nt = normalize_timestamp_ms(t);
                        // Exclude both trades already covered by aggTrades and the still-open
                        // current minute's future closeTime.
                        if nt < earliest_trade && nt <= now_ms {
                            candle_samples.push(OracleSample {
                                timestamp_ms: nt,
                                price: p,
                            });
                        }
                    }
                }
                candle_samples.extend(fut_samples);
                fut_samples = candle_samples;
            }
            let fut_ok = !fut_samples.is_empty();
            if fut_ok {
                feed.merge_futures_backfill(&cl_sym, fut_samples);
            }

            // 2) Pure Spot 1 s klines kept strictly isolated in `spot_raw_history`
            let mut spot_ok = false;
            if let Ok(resp) = sk_res
                && resp.status().is_success()
                && let Ok(rows) = resp.json::<Vec<Vec<Value>>>().await
            {
                let mut parsed = Vec::with_capacity(rows.len());
                for row in rows {
                    let ts = row.first().and_then(|v| v.as_i64());
                    let close = row
                        .get(4)
                        .and_then(|v| v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64()));
                    if let (Some(t), Some(p)) = (ts, close)
                        && p.is_finite()
                        && p > 0.0
                    {
                        parsed.push(OracleSample {
                            timestamp_ms: normalize_timestamp_ms(t),
                            price: p,
                        });
                    }
                }
                spot_ok = !parsed.is_empty();
                feed.merge_binance_backfill(&cl_sym, parsed);
            }

            fut_ok || spot_ok
        }
        Err(_) => false,
    };
    if !ok {
        feed.backfilled_symbols.write().remove(&cl_sym);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twap_60s_computes_time_weighted_step_average_and_evicts_old_samples() {
        let mut q = VecDeque::new();
        let now_ms = 1_000_000;
        // 100.0 held for 45s ([window_start, window_start + 45_000]),
        // then 104.0 held for 15s ([window_start + 45_000, now_ms]) => 101.0
        q.push_back(OracleSample {
            timestamp_ms: now_ms - 70_000,
            price: 100.0,
        });
        q.push_back(OracleSample {
            timestamp_ms: now_ms - 15_000,
            price: 104.0,
        });
        let twap = compute_twap_60s(&q, now_ms).unwrap();
        assert!((twap - 101.0).abs() < 1e-9, "got {twap}");

        // Single sample held up to now_ms returns its price
        let single = VecDeque::from([OracleSample {
            timestamp_ms: now_ms - 5_000,
            price: 95_000.5,
        }]);
        assert!((compute_twap_60s(&single, now_ms).unwrap() - 95_000.5).abs() < 1e-9);

        // Rolling 10-minute buffer eviction on detached OracleFeed
        let feed = OracleFeed::new_detached();
        feed.record_chainlink_tick("BTC/USD", 1_000_000, 90_000.0);
        feed.record_chainlink_tick("btc/usd", 1_000_000 + ORACLE_BUFFER_WINDOW_MS + 10_000, 91_000.0);
        let snap = feed
            .snapshot_at("btc/usd", 1_000_000 + ORACLE_BUFFER_WINDOW_MS + 10_000)
            .unwrap();
        assert!(!snap.stale);
        assert_eq!(snap.history.len(), 1);
        assert!((snap.spot - 91_000.0).abs() < 1e-9);
    }

    #[test]
    fn oracle_snapshot_marks_stale_and_does_not_extrapolate_twap() {
        let feed = OracleFeed::new_detached();
        let base_ms: i64 = 1_700_000_000_000;
        feed.record_chainlink_tick("eth/usd", base_ms + 100_000, 100.0);
        feed.record_chainlink_tick("eth/usd", base_ms + 130_000, 110.0);

        // Fresh 5s after last tick -> not stale, TWAP blends 100.0 (55s) and 110.0 (5s)
        let fresh = feed.snapshot_at("eth/usd", base_ms + 135_000).unwrap();
        assert!(!fresh.stale);
        assert!(fresh.twap_60s > 100.0 && fresh.twap_60s < 110.0);

        // Stale > 10s after last tick -> marked stale and TWAP freezes at last_timestamp_ms
        // Expected TWAP at (base_ms + 130_000) is 100.0 (only 100.0 held over [base_ms+70k, base_ms+130k])
        let stale1 = feed.snapshot_at("eth/usd", base_ms + 140_001).unwrap();
        let stale2 = feed.snapshot_at("eth/usd", base_ms + 250_000).unwrap();
        assert!(stale1.stale);
        assert!(stale2.stale);
        assert!((stale1.twap_60s - 100.0).abs() < 1e-9, "got {}", stale1.twap_60s);
        assert!((stale2.twap_60s - 100.0).abs() < 1e-9, "got {}", stale2.twap_60s);
    }

    #[test]
    fn basis_adjusted_fast_lead_tracks_binance_and_preserves_chainlink_stale() {
        let feed = OracleFeed::new_detached();
        let base_ms: i64 = 1_700_000_000_000;
        // Binance at 90_010, Chainlink settles +10 higher at 90_020 -> initial basis = +10
        feed.record_binance_tick("BTCUSDT", 90_010.0, base_ms);
        feed.record_chainlink_tick("btc/usd", base_ms + 50, 90_020.0);
        // Next Binance ticks are adjusted by +10 basis
        feed.record_binance_tick("BTCUSDT", 90_030.0, base_ms + 300);
        feed.record_binance_tick("BTCUSDT", 90_040.0, base_ms + 600);

        let snap = feed.snapshot_at("btc/usd", base_ms + 800).unwrap();
        assert!(!snap.stale);
        assert!((snap.fast_spot.unwrap() - 90_050.0).abs() < 1e-6);
        assert_eq!(snap.fast_history.len(), 3);

        // If Chainlink goes silent > 10s while Binance keeps streaming, `stale` must stay true
        feed.record_binance_tick("BTCUSDT", 90_060.0, base_ms + 12_000);
        let stale_snap = feed.snapshot_at("btc/usd", base_ms + 12_100).unwrap();
        assert!(stale_snap.stale);

        // Regression test: rapid 20ms Binance ticks across >400ms must create multiple buckets
        let feed2 = OracleFeed::new_detached();
        feed2.record_binance_tick("ETHUSDT", 3_000.0, base_ms - 50);
        feed2.record_chainlink_tick("eth/usd", base_ms - 20, 2_995.0);
        for step in 0..=20 {
            feed2.record_binance_tick("ETHUSDT", 3_000.0 + step as f64, base_ms + step * 20);
        }
        let eth_snap = feed2.snapshot_at("eth/usd", base_ms + 420).unwrap();
        assert!(
            eth_snap.fast_history.len() >= 3,
            "expected >= 3 fast buckets from 20ms ticks across 400ms, got {}",
            eth_snap.fast_history.len()
        );
    }

    #[test]
    fn snapshot_reuses_cached_history_arcs() {
        let feed = OracleFeed::new_detached();
        let base_ms: i64 = 1_700_000_000_000;
        for i in 0..50 {
            feed.record_binance_tick("BTCUSDT", 60_000.0 + i as f64, base_ms + i * 50);
        }
        feed.record_chainlink_tick("btc/usd", base_ms + 50, 60_010.0);

        let a = feed.snapshot_at("btc/usd", base_ms + 3_000).unwrap();
        let b = feed.snapshot_at("btc/usd", base_ms + 3_001).unwrap();
        // No new ticks between the two calls, so the render loop must not rebuild the
        // 50-sample vectors: it should hand back the same allocation.
        assert!(
            std::sync::Arc::ptr_eq(&a.fast_history, &b.fast_history),
            "fast_history must be memoized, not re-cloned per frame"
        );
        assert!(std::sync::Arc::ptr_eq(&a.history, &b.history));

        // A new tick invalidates the cache and produces fresh data.
        feed.record_binance_tick("BTCUSDT", 60_100.0, base_ms + 3_100);
        let c = feed.snapshot_at("btc/usd", base_ms + 3_200).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&b.fast_history, &c.fast_history));
        assert!(c.fast_history.len() >= a.fast_history.len());
    }

    #[test]
    fn binance_ticks_never_clear_chainlink_stale_flag() {
        let feed = OracleFeed::new_detached();
        let base_ms: i64 = 1_700_000_000_000;
        feed.record_chainlink_tick("eth/usd", base_ms, 3_000.0);
        // Chainlink silent for 30s while "Binance" keeps streaming on BOTH venues.
        for i in 0..20 {
            feed.record_binance_tick_from(
                "ETHUSDT",
                3_010.0 + i as f64,
                base_ms + 1_000 + i * 100,
                None,
                BinanceVenue::Futures,
            );
            feed.record_binance_tick_from(
                "ETHUSDT",
                3_009.0 + i as f64,
                base_ms + 1_000 + i * 100,
                None,
                BinanceVenue::Spot,
            );
        }
        let snap = feed.snapshot_at("eth/usd", base_ms + 30_000).unwrap();
        assert!(
            snap.stale,
            "N6: a fast Binance feed must never resurrect a stale Chainlink settlement feed"
        );
        // And the settlement spot itself stays frozen at the last Chainlink print.
        assert!((snap.spot - 3_000.0).abs() < 1e-9);
    }

    #[test]
    fn spot_fallback_is_never_basis_adjusted_and_does_not_merge_with_futures() {
        let feed = OracleFeed::new_detached();
        let base_ms: i64 = 1_700_000_000_000;
        // Establish a +$10 perp premium from the futures venue.
        feed.record_binance_tick_from(
            "BTCUSDT",
            60_010.0,
            base_ms,
            None,
            BinanceVenue::Futures,
        );
        feed.record_chainlink_tick("btc/usd", base_ms + 10, 60_020.0);

        // Futures dies; spot keeps publishing 60_000 (no premium).
        for i in 0..5 {
            feed.record_binance_tick_from(
                "BTCUSDT",
                60_000.0,
                base_ms + 6_000 + i * 100,
                None,
                BinanceVenue::Spot,
            );
        }
        let snap = feed.snapshot_at("btc/usd", base_ms + 6_500).unwrap();
        assert!(
            (snap.fast_spot.unwrap() - 60_000.0).abs() < 1e-9,
            "spot fallback must not inherit the perp basis, got {:?}",
            snap.fast_spot
        );
        // Nothing from the futures series leaked into the spot series.
        assert!(
            snap.fast_history.iter().all(|(_, p)| (p - 60_000.0).abs() < 1e-9),
            "futures samples must not be merged into the spot fallback series"
        );
    }

    #[test]
    fn fast_lead_freshness_absorbs_local_clock_skew() {
        let feed = OracleFeed::new_detached();
        let base_ms: i64 = 1_700_000_000_000;
        // The local clock claims "now" is 4 s ahead of venue time. A live feed must
        // still count as live; only genuine silence may hide it.
        let skew = 4_000;
        for i in 0..5 {
            let venue_ts = base_ms + i * 100;
            feed.record_binance_tick_from(
                "BTCUSDT",
                60_000.0 + i as f64,
                venue_ts + skew,
                Some(venue_ts),
                BinanceVenue::Futures,
            );
        }
        let live = feed.snapshot_at("btc/usd", base_ms + 400 + skew).unwrap();
        assert!(
            live.fast_spot.is_some(),
            "a 4 s clock skew must not make a live feed look dead"
        );

        // 20 s of real silence on top of the same skew must still hide it.
        let dead = feed
            .snapshot_at("btc/usd", base_ms + 20_000 + skew)
            .unwrap();
        assert!(dead.fast_spot.is_none());
    }

    #[test]
    fn stale_fast_lead_is_hidden_rather_than_painted_forever() {
        let feed = OracleFeed::new_detached();
        let base_ms: i64 = 1_700_000_000_000;
        feed.record_binance_tick_from(
            "BTCUSDT",
            60_000.0,
            base_ms,
            Some(base_ms),
            BinanceVenue::Futures,
        );
        // 30 s later the websocket is dead: fast_spot must disappear, not freeze.
        let snap = feed.snapshot_at("btc/usd", base_ms + 30_000).unwrap();
        assert!(snap.fast_spot.is_none());
        assert!(snap.fast_ts_ms.is_none());
    }

    #[test]
    fn time_aligned_basis_does_not_drift_during_lagged_chainlink_spike() {
        let feed = OracleFeed::new_detached();
        let base_ms: i64 = 1_700_000_000_000;
        // Static spread between Chainlink and Binance is exactly -$5.00
        feed.record_binance_tick("BTCUSDT", 60_000.0, base_ms);
        feed.record_chainlink_tick("btc/usd", base_ms, 59_995.0);

        // Over next 2 seconds Binance jumps +$50 to 60_050.0
        feed.record_binance_tick("BTCUSDT", 60_025.0, base_ms + 1_000);
        feed.record_binance_tick("BTCUSDT", 60_050.0, base_ms + 2_000);

        // Chainlink now delivers a 2s-old quote (cl_ts = base_ms + 500 -> Binance was ~60_012.50, so Chainlink is ~60_007.50)
        feed.record_chainlink_tick("btc/usd", base_ms + 500, 60_007.5);

        let snap = feed.snapshot_at("btc/usd", base_ms + 2_050).unwrap();
        // Basis must stay -$5.00 cleanly, NOT drift by -$42.50! So fast_spot tracks full +$50 spike -> 60_045.0
        assert!(
            (snap.fast_spot.unwrap() - 60_045.0).abs() < 0.5,
            "expected ~60045.0 with time-aligned basis, got {:?}",
            snap.fast_spot
        );
    }

    #[test]
    fn wire_lag_is_skew_invariant_and_50ms_buckets_record_cleanly() {
        let feed = OracleFeed::new_detached();
        let base_ms: i64 = 1_700_000_000_000;
        // The local clock is consistently 90 ms ahead of the venue. That is clock
        // skew, not network latency, and must NOT be reported as wire lag.
        feed.record_binance_tick_with_wire_lag("BTCUSDT", 61_000.0, base_ms, Some(base_ms - 90));
        feed.record_chainlink_tick("btc/usd", base_ms, 60_995.0);
        feed.record_binance_tick_with_wire_lag(
            "BTCUSDT",
            61_005.0,
            base_ms + 60,
            Some(base_ms + 60 - 90),
        );
        feed.record_binance_tick_with_wire_lag(
            "BTCUSDT",
            61_010.0,
            base_ms + 120,
            Some(base_ms + 120 - 90),
        );
        let snap = feed.snapshot_at("btc/usd", base_ms + 130).unwrap();
        assert_eq!(
            snap.fast_wire_lag_ms,
            Some(0),
            "constant clock skew must not be reported as wire lag"
        );
        assert!(snap.fast_history.len() >= 3, "got {}", snap.fast_history.len());

        // A genuinely delayed packet (190 ms beyond the baseline offset) must show up
        // as ~190 ms of excess latency, not as the raw 280 ms offset.
        for i in 0..12 {
            let ts = base_ms + 200 + i * 50;
            feed.record_binance_tick_with_wire_lag(
                "BTCUSDT",
                61_010.0,
                ts,
                Some(ts - 90 - 190),
            );
        }
        let snap2 = feed.snapshot_at("btc/usd", base_ms + 1_000).unwrap();
        let lag = snap2.fast_wire_lag_ms.unwrap();
        assert!(
            (100..=220).contains(&lag),
            "expected ~190 ms of excess latency, got {lag}"
        );
    }

    #[test]
    fn futures_backfill_seeds_futures_history_without_touching_spot() {
        let feed = OracleFeed::new_detached();
        let now_ms = Utc::now().timestamp_millis();
        feed.merge_futures_backfill(
            "btc/usd",
            vec![
                OracleSample { timestamp_ms: now_ms - 60_000, price: 61_050.0 },
                OracleSample { timestamp_ms: now_ms - 120_000, price: 61_000.0 },
                OracleSample { timestamp_ms: now_ms - 60_000, price: 61_050.0 },
                OracleSample { timestamp_ms: now_ms - 30_000, price: f64::NAN },
            ],
        );
        feed.record_binance_tick_from("BTCUSDT", 61_100.0, now_ms, Some(now_ms), BinanceVenue::Futures);
        feed.record_chainlink_tick("btc/usd", now_ms, 61_090.0);

        let snap = feed.snapshot_at("btc/usd", now_ms + 10).unwrap();
        assert_eq!(snap.fast_history.len(), 3);
        assert!(snap.fast_history[0].0 < snap.fast_history[1].0);
        assert!(snap.fast_history[1].0 < snap.fast_history[2].0);
        assert!(feed.spot_raw_history.get("btc/usd").is_none());
    }
}
