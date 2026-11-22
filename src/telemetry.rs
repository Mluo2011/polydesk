use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

const RING_CAP: usize = 256;

#[derive(Debug, Default)]
struct MetricRing {
    samples: VecDeque<f64>,
}

impl MetricRing {
    fn push(&mut self, val: f64) {
        if !val.is_finite() {
            return;
        }
        if self.samples.len() >= RING_CAP {
            self.samples.pop_front();
        }
        self.samples.push_back(val);
    }

    fn stats(&self) -> LatencyStats {
        if self.samples.is_empty() {
            return LatencyStats::default();
        }
        let last = *self.samples.back().unwrap_or(&0.0);
        let mut sorted: Vec<f64> = self.samples.iter().copied().collect();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let n = sorted.len();
        let p50 = sorted[n / 2];
        let p95 = sorted[((n as f64) * 0.95).floor().min((n - 1) as f64) as usize];
        LatencyStats { last, p50, p95 }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct LatencyStats {
    pub last: f64,
    pub p50: f64,
    pub p95: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuilderFeeRecord {
    pub id: String,
    pub timestamp_ms: i64,
    pub mode: String,
    pub market_question: String,
    pub side: String,
    pub notional_usd: f64,
    pub is_maker: bool,
    pub protocol_fee_usd: f64,
    pub builder_fee_bps: u32,
    pub builder_fee_usd: f64,
    pub builder_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetrySnapshot {
    pub ws_connected: bool,
    pub ws_msgs_total: u64,
    pub ws_reconnects: u64,
    pub orders_sent: u64,
    pub orders_filled: u64,
    pub uptime_secs: u64,
    pub ws_lag_ms: LatencyStats,
    pub book_apply_us: LatencyStats,
    pub sign_us: LatencyStats,
    pub order_rtt_ms: LatencyStats,
    pub warm_ping_ms: LatencyStats,
    pub routed_volume_usd: f64,
    pub total_protocol_fees_usd: f64,
    pub total_builder_fees_usd: f64,
    pub recent_fee_ledger: Vec<BuilderFeeRecord>,
}

pub struct Telemetry {
    started: Instant,
    ws_connected: AtomicBool,
    ws_msgs_total: AtomicU64,
    ws_reconnects: AtomicU64,
    orders_sent: AtomicU64,
    orders_filled: AtomicU64,
    ws_lag_ms: Mutex<MetricRing>,
    book_apply_us: Mutex<MetricRing>,
    sign_us: Mutex<MetricRing>,
    order_rtt_ms: Mutex<MetricRing>,
    warm_ping_ms: Mutex<MetricRing>,
    ledger: Mutex<LedgerState>,
}

#[derive(Default)]
struct LedgerState {
    routed_volume_usd: f64,
    total_protocol_fees_usd: f64,
    total_builder_fees_usd: f64,
    records: VecDeque<BuilderFeeRecord>,
}

impl Telemetry {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            ws_connected: AtomicBool::new(false),
            ws_msgs_total: AtomicU64::new(0),
            ws_reconnects: AtomicU64::new(0),
            orders_sent: AtomicU64::new(0),
            orders_filled: AtomicU64::new(0),
            ws_lag_ms: Mutex::new(MetricRing::default()),
            book_apply_us: Mutex::new(MetricRing::default()),
            sign_us: Mutex::new(MetricRing::default()),
            order_rtt_ms: Mutex::new(MetricRing::default()),
            warm_ping_ms: Mutex::new(MetricRing::default()),
            ledger: Mutex::new(LedgerState::default()),
        }
    }

    pub fn set_ws_connected(&self, connected: bool) {
        self.ws_connected.store(connected, Ordering::Relaxed);
    }

    pub fn inc_ws_msg(&self) {
        self.ws_msgs_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_ws_reconnect(&self) {
        self.ws_reconnects.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_order_sent(&self) {
        self.orders_sent.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_order_filled(&self) {
        self.orders_filled.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_ws_lag_ms(&self, ms: f64) {
        // Clamp negative clock skew lightly so display stays readable
        self.ws_lag_ms.lock().push(ms.max(0.0));
    }

    pub fn record_book_apply_us(&self, us: f64) {
        self.book_apply_us.lock().push(us);
    }

    pub fn record_sign_us(&self, us: f64) {
        self.sign_us.lock().push(us);
    }

    pub fn record_order_rtt_ms(&self, ms: f64) {
        self.order_rtt_ms.lock().push(ms);
    }

    pub fn record_warm_ping_ms(&self, ms: f64) {
        self.warm_ping_ms.lock().push(ms);
    }

    pub fn record_fee(&self, rec: BuilderFeeRecord) {
        let mut l = self.ledger.lock();
        l.routed_volume_usd += rec.notional_usd;
        l.total_protocol_fees_usd += rec.protocol_fee_usd;
        l.total_builder_fees_usd += rec.builder_fee_usd;
        if l.records.len() >= 100 {
            l.records.pop_front();
        }
        l.records.push_back(rec);
    }

    pub fn snapshot(&self) -> TelemetrySnapshot {
        let l = self.ledger.lock();
        TelemetrySnapshot {
            ws_connected: self.ws_connected.load(Ordering::Relaxed),
            ws_msgs_total: self.ws_msgs_total.load(Ordering::Relaxed),
            ws_reconnects: self.ws_reconnects.load(Ordering::Relaxed),
            orders_sent: self.orders_sent.load(Ordering::Relaxed),
            orders_filled: self.orders_filled.load(Ordering::Relaxed),
            uptime_secs: self.started.elapsed().as_secs(),
            ws_lag_ms: self.ws_lag_ms.lock().stats(),
            book_apply_us: self.book_apply_us.lock().stats(),
            sign_us: self.sign_us.lock().stats(),
            order_rtt_ms: self.order_rtt_ms.lock().stats(),
            warm_ping_ms: self.warm_ping_ms.lock().stats(),
            routed_volume_usd: l.routed_volume_usd,
            total_protocol_fees_usd: l.total_protocol_fees_usd,
            total_builder_fees_usd: l.total_builder_fees_usd,
            recent_fee_ledger: l.records.iter().rev().cloned().collect(),
        }
    }
}
