use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
pub const ORACLE_MAX_SAMPLES_PER_SYMBOL: usize = 2000;
pub const ORACLE_STALE_MS: i64 = 10_000;
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
    pub history: Vec<(i64, f64)>,
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
    binance_leads: Arc<DashMap<String, (f64, i64)>>,
    watched: Arc<RwLock<HashSet<String>>>,
    watch_tx: mpsc::UnboundedSender<HashSet<String>>,
    events_tx: broadcast::Sender<OracleEvent>,
    chainlink_disconnected: Arc<AtomicBool>,
}

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
            watched: Arc::new(RwLock::new(HashSet::new())),
            watch_tx,
            events_tx,
            chainlink_disconnected: Arc::new(AtomicBool::new(false)),
        });
        (feed, watch_rx)
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
        self.chainlink_disconnected.store(false, Ordering::Relaxed);
        let _ = self.events_tx.send(OracleEvent { symbol: sym });
    }

    pub fn record_binance_tick(&self, binance_sym: &str, price: f64, raw_ts_ms: i64) {
        if !price.is_finite() || price <= 0.0 {
            return;
        }
        let Some(cl_sym) = binance_to_chainlink_symbol(binance_sym) else {
            return;
        };
        let ts_ms = normalize_timestamp_ms(raw_ts_ms);
        self.binance_leads
            .insert(cl_sym.to_string(), (price, ts_ms));
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
        if let Some(q) = self.samples.get(&sym)
            && let Some(last) = q.back()
        {
            let stale = now_ms.saturating_sub(last.timestamp_ms) > ORACLE_STALE_MS
                || self.chainlink_disconnected.load(Ordering::Relaxed);
            let twap_end = if stale { last.timestamp_ms } else { now_ms };
            let twap_60s = compute_twap_60s(&q, twap_end).unwrap_or(last.price);
            let history = q.iter().map(|s| (s.timestamp_ms, s.price)).collect();
            return Some(OracleSnapshot {
                symbol: sym,
                spot: last.price,
                twap_60s,
                last_timestamp_ms: last.timestamp_ms,
                stale,
                binance_mid: binance.map(|(p, _)| p),
                binance_ts_ms: binance.map(|(_, t)| t),
                history,
            });
        }
        binance.map(|(bp, bt)| OracleSnapshot {
            symbol: sym,
            spot: bp,
            twap_60s: bp,
            last_timestamp_ms: bt,
            stale: true,
            binance_mid: Some(bp),
            binance_ts_ms: Some(bt),
            history: Vec::new(),
        })
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

async fn run_oracle_coordinator(
    feed: Arc<OracleFeed>,
    mut watch_rx: mpsc::UnboundedReceiver<HashSet<String>>,
) {
    let mut chainlink_task: Option<JoinHandle<()>> = None;
    let mut binance_task: Option<JoinHandle<()>> = None;

    while let Some(watched) = watch_rx.recv().await {
        let want_active = !watched.is_empty();
        if want_active && chainlink_task.is_none() {
            let f1 = feed.clone();
            chainlink_task = Some(tokio::spawn(run_chainlink_loop(f1)));
            let f2 = feed.clone();
            binance_task = Some(tokio::spawn(run_binance_lead_loop(f2)));
        } else if !want_active {
            if let Some(h) = chainlink_task.take() {
                h.abort();
            }
            if let Some(h) = binance_task.take() {
                h.abort();
            }
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

async fn run_binance_lead_loop(feed: Arc<OracleFeed>) {
    let url = "wss://stream.binance.com:9443/stream?streams=btcusdt@bookTicker/ethusdt@bookTicker/solusdt@bookTicker/xrpusdt@bookTicker/dogeusdt@bookTicker/bnbusdt@bookTicker";
    let mut backoff_ms = 1_500_u64;
    loop {
        match connect_async_with_config(url, None, false).await {
            Ok((ws, _)) => {
                backoff_ms = 1_500;
                let (_, mut read) = ws.split();
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
                                if let (Some(b), Some(a)) = (bid, ask)
                                    && b > 0.0
                                    && a >= b
                                {
                                    let now_ms = Utc::now().timestamp_millis();
                                    feed.record_binance_tick(sym, (b + a) * 0.5, now_ms);
                                }
                            }
                        }
                        Ok(Message::Close(_)) | Err(_) => break,
                        _ => {}
                    }
                }
            }
            Err(_) => {
                // Silent graceful fallback if Binance is geo-blocked or unreachable
            }
        }
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(30_000);
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
}
