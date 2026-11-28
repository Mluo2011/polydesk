use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::book::{BookView, FastOrderBook};
use crate::telemetry::Telemetry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapeEntry {
    pub asset_id: String,
    pub market_id: String,
    pub price: f64,
    pub size: f64,
    pub notional_usd: f64,
    pub side: String,
    pub timestamp_ms: i64,
    pub is_whale: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FeedEvent {
    Book {
        view: BookView,
    },
    Trade {
        entry: TapeEntry,
    },
    Status {
        ws_connected: bool,
    },
}

pub const MAX_INACTIVE_BOOKS: usize = 64;

enum FeedCmd {
    Subscribe(Vec<String>),
    Unsubscribe(Vec<String>),
    SetWsUrl(String),
}

pub struct MarketFeed {
    books: Arc<DashMap<String, FastOrderBook>>,
    tape: Arc<Mutex<VecDeque<TapeEntry>>>,
    subscribed: Arc<Mutex<HashSet<String>>>,
    book_last_access: Arc<Mutex<HashMap<String, u64>>>,
    access_clock: Arc<AtomicU64>,
    pinned_assets: Arc<Mutex<HashSet<String>>>,
    cmd_tx: mpsc::UnboundedSender<FeedCmd>,
    events_tx: broadcast::Sender<FeedEvent>,
    whale_threshold: Arc<Mutex<f64>>,
}

impl MarketFeed {
    pub fn spawn(ws_url: String, telemetry: Arc<Telemetry>, whale_threshold_usd: f64) -> Arc<Self> {
        let books = Arc::new(DashMap::new());
        let tape = Arc::new(Mutex::new(VecDeque::with_capacity(200)));
        let subscribed = Arc::new(Mutex::new(HashSet::new()));
        let book_last_access = Arc::new(Mutex::new(HashMap::new()));
        let access_clock = Arc::new(AtomicU64::new(1));
        let pinned_assets = Arc::new(Mutex::new(HashSet::new()));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (events_tx, _) = broadcast::channel(2048);
        let whale_threshold = Arc::new(Mutex::new(whale_threshold_usd));

        let feed = Arc::new(Self {
            books: books.clone(),
            tape: tape.clone(),
            subscribed: subscribed.clone(),
            book_last_access,
            access_clock,
            pinned_assets,
            cmd_tx,
            events_tx: events_tx.clone(),
            whale_threshold: whale_threshold.clone(),
        });

        tokio::spawn(run_ws_loop(
            ws_url,
            books,
            tape,
            subscribed,
            cmd_rx,
            events_tx,
            telemetry,
            whale_threshold,
        ));

        feed
    }

    pub fn touch_book(&self, asset_id: &str) {
        if asset_id.is_empty() {
            return;
        }
        let seq = self.access_clock.fetch_add(1, Ordering::Relaxed);
        self.book_last_access
            .lock()
            .insert(asset_id.to_string(), seq);
    }

    pub fn subscribe(&self, asset_ids: Vec<String>) {
        let mut to_add = Vec::new();
        {
            let mut sub = self.subscribed.lock();
            for id in &asset_ids {
                if !id.is_empty() && sub.insert(id.clone()) {
                    to_add.push(id.clone());
                }
            }
        }
        for id in &asset_ids {
            self.touch_book(id);
        }
        if !to_add.is_empty() {
            let _ = self.cmd_tx.send(FeedCmd::Subscribe(to_add));
        }
        let pinned = self.pinned_assets.lock().clone();
        self.evict_inactive_books(&pinned, MAX_INACTIVE_BOOKS);
    }

    pub fn unsubscribe(&self, asset_ids: Vec<String>) {
        let mut removed = Vec::new();
        {
            let mut sub = self.subscribed.lock();
            for id in &asset_ids {
                if !id.is_empty() && sub.remove(id) {
                    removed.push(id.clone());
                }
            }
        }
        {
            let mut lru = self.book_last_access.lock();
            for id in &asset_ids {
                lru.remove(id);
            }
        }
        for id in &asset_ids {
            self.books.remove(id);
        }
        if !removed.is_empty() {
            let _ = self.cmd_tx.send(FeedCmd::Unsubscribe(removed));
        }
    }

    pub fn set_pinned_assets(&self, pinned: HashSet<String>) {
        {
            let mut p = self.pinned_assets.lock();
            *p = pinned.clone();
        }
        self.evict_inactive_books(&pinned, MAX_INACTIVE_BOOKS);
    }

    pub fn evict_inactive_books(&self, pinned: &HashSet<String>, max_inactive: usize) -> usize {
        let inactive_keys: Vec<String> = self
            .books
            .iter()
            .map(|entry| entry.key().clone())
            .filter(|k| !pinned.contains(k))
            .collect();

        if inactive_keys.len() <= max_inactive {
            return 0;
        }

        let mut ranked: Vec<(u64, String)> = {
            let lru = self.book_last_access.lock();
            inactive_keys
                .into_iter()
                .map(|k| (lru.get(&k).copied().unwrap_or(0), k))
                .collect()
        };
        ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        let evict_count = ranked.len() - max_inactive;
        let to_evict: Vec<String> = ranked.into_iter().take(evict_count).map(|(_, k)| k).collect();

        for k in &to_evict {
            self.books.remove(k);
        }
        {
            let mut lru = self.book_last_access.lock();
            for k in &to_evict {
                lru.remove(k);
            }
        }
        let unsub_ids: Vec<String> = {
            let mut sub = self.subscribed.lock();
            to_evict
                .iter()
                .filter(|k| sub.remove(*k))
                .cloned()
                .collect()
        };
        if !unsub_ids.is_empty() {
            let _ = self.cmd_tx.send(FeedCmd::Unsubscribe(unsub_ids));
        }

        evict_count
    }

    pub fn set_whale_threshold(&self, usd: f64) {
        *self.whale_threshold.lock() = usd.max(1.0);
    }

    pub fn seed_book(
        &self,
        asset_id: &str,
        market_id: &str,
        tick_size: f64,
        bids: &[(f64, f64)],
        asks: &[(f64, f64)],
        last_trade: Option<f64>,
        ts_ms: i64,
        hash: Option<String>,
    ) -> BookView {
        self.touch_book(asset_id);
        let view = {
            let mut entry = self
                .books
                .entry(asset_id.to_string())
                .or_insert_with(|| {
                    FastOrderBook::new(asset_id.to_string(), market_id.to_string(), tick_size)
                });
            entry.set_tick_size(tick_size);
            entry.apply_snapshot(bids, asks, ts_ms, hash);
            if let Some(lt) = last_trade {
                entry.last_trade_price = Some(lt);
            }
            entry.to_view(60)
        };
        let pinned = self.pinned_assets.lock().clone();
        self.evict_inactive_books(&pinned, MAX_INACTIVE_BOOKS);
        let _ = self.events_tx.send(FeedEvent::Book { view: view.clone() });
        view
    }

    pub fn with_book<R>(&self, asset_id: &str, f: impl FnOnce(&FastOrderBook) -> R) -> Option<R> {
        self.touch_book(asset_id);
        self.books.get(asset_id).map(|b| f(&*b))
    }

    pub fn get_tick_size_ticks(&self, asset_id: &str) -> Option<usize> {
        self.books.get(asset_id).map(|b| b.tick_size_ticks)
    }

    pub fn get_book_view(&self, asset_id: &str, levels: usize) -> Option<BookView> {
        self.books.get(asset_id).map(|b| b.to_view(levels))
    }

    /// Zero-allocation O(1) top-of-book lookup returning `(best_bid, best_ask, mid)`.
    #[inline]
    pub fn get_top_of_book(&self, asset_id: &str) -> Option<(Option<f64>, Option<f64>, Option<f64>)> {
        self.books
            .get(asset_id)
            .map(|b| (b.best_bid(), b.best_ask(), b.mid()))
    }

    pub fn get_book_clone(&self, asset_id: &str) -> Option<FastOrderBook> {
        self.books.get(asset_id).map(|b| b.clone())
    }

    pub fn recent_tape(&self, asset_id: Option<&str>, limit: usize) -> Vec<TapeEntry> {
        let t = self.tape.lock();
        t.iter()
            .rev()
            .filter(|e| asset_id.is_none_or(|a| e.asset_id == a))
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<FeedEvent> {
        self.events_tx.subscribe()
    }

    pub fn update_ws_url(&self, new_url: String) {
        let trimmed = new_url.trim().to_string();
        if trimmed.starts_with("wss://") {
            let _ = self.cmd_tx.send(FeedCmd::SetWsUrl(trimmed));
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_ws_loop(
    mut ws_url: String,
    books: Arc<DashMap<String, FastOrderBook>>,
    tape: Arc<Mutex<VecDeque<TapeEntry>>>,
    subscribed: Arc<Mutex<HashSet<String>>>,
    mut cmd_rx: mpsc::UnboundedReceiver<FeedCmd>,
    events_tx: broadcast::Sender<FeedEvent>,
    telemetry: Arc<Telemetry>,
    whale_threshold: Arc<Mutex<f64>>,
) {
    let mut backoff_ms = 150u64;

    loop {
        let mut reconnect_now = false;
        // Connect with disable_nagle = true (TCP_NODELAY) for minimum packet latency
        match connect_async_with_config(&ws_url, None, true).await {
            Ok((ws_stream, _)) => {
                info!(url = %ws_url, "Connected to Polymarket CLOB market WebSocket (TCP_NODELAY)");
                backoff_ms = 150;
                telemetry.set_ws_connected(true);
                let _ = events_tx.send(FeedEvent::Status { ws_connected: true });

                let (mut write, mut read) = ws_stream.split();

                // Re-subscribe all tracked assets immediately on connect
                let initial: Vec<String> = subscribed.lock().iter().cloned().collect();
                if !initial.is_empty() {
                    let msg = json!({
                        "assets_ids": initial,
                        "type": "market"
                    });
                    if let Err(e) = write.send(Message::Text(msg.to_string().into())).await {
                        warn!("Initial WS subscribe failed: {e}");
                    }
                }

                let mut ping_interval = tokio::time::interval(Duration::from_secs(10));
                ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

                loop {
                    tokio::select! {
                        biased;
                        msg_opt = read.next() => {
                            match msg_opt {
                                Some(Ok(Message::Text(text))) => {
                                    let s = text.as_str();
                                    if s == "PONG" || s.is_empty() {
                                        continue;
                                    }
                                    let t0 = Instant::now();
                                    telemetry.inc_ws_msg();
                                    handle_ws_payload(
                                        s,
                                        &books,
                                        &tape,
                                        &events_tx,
                                        &telemetry,
                                        *whale_threshold.lock(),
                                    );
                                    telemetry.record_book_apply_us(t0.elapsed().as_secs_f64() * 1_000_000.0);
                                }
                                Some(Ok(Message::Ping(p))) => {
                                    let _ = write.send(Message::Pong(p)).await;
                                }
                                Some(Ok(Message::Close(_))) | None => {
                                    warn!("Polymarket WS closed by peer");
                                    break;
                                }
                                Some(Err(e)) => {
                                    warn!("Polymarket WS read error: {e}");
                                    break;
                                }
                                _ => {}
                            }
                        }
                        cmd_opt = cmd_rx.recv() => {
                            match cmd_opt {
                                Some(FeedCmd::Subscribe(ids)) => {
                                    let payload = json!({
                                        "assets_ids": ids,
                                        "operation": "subscribe"
                                    });
                                    if let Err(e) = write.send(Message::Text(payload.to_string().into())).await {
                                        warn!("WS dynamic subscribe failed: {e}");
                                        break;
                                    }
                                }
                                Some(FeedCmd::Unsubscribe(ids)) => {
                                    let payload = json!({
                                        "assets_ids": ids,
                                        "operation": "unsubscribe"
                                    });
                                    let _ = write.send(Message::Text(payload.to_string().into())).await;
                                }
                                Some(FeedCmd::SetWsUrl(new_url)) => {
                                    if new_url != ws_url {
                                        info!(old = %ws_url, new = %new_url, "Hot-reloading Polymarket market WebSocket URL");
                                        ws_url = new_url;
                                        backoff_ms = 150;
                                        reconnect_now = true;
                                        break;
                                    }
                                }
                                None => return,
                            }
                        }
                        _ = ping_interval.tick() => {
                            if let Err(e) = write.send(Message::Text("PING".into())).await {
                                warn!("WS heartbeat PING failed: {e}");
                                break;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Polymarket WS connect error: {e}");
            }
        }

        telemetry.set_ws_connected(false);
        telemetry.inc_ws_reconnect();
        let _ = events_tx.send(FeedEvent::Status { ws_connected: false });
        if reconnect_now {
            continue;
        }
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(5_000);
    }
}

fn handle_ws_payload(
    raw: &str,
    books: &DashMap<String, FastOrderBook>,
    tape: &Mutex<VecDeque<TapeEntry>>,
    events_tx: &broadcast::Sender<FeedEvent>,
    telemetry: &Telemetry,
    whale_threshold_usd: f64,
) {
    let parsed: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            debug!("Ignoring non-JSON WS frame: {e}");
            return;
        }
    };

    match parsed {
        Value::Array(items) => {
            for item in items {
                process_single_event(&item, books, tape, events_tx, telemetry, whale_threshold_usd);
            }
        }
        Value::Object(_) => {
            process_single_event(&parsed, books, tape, events_tx, telemetry, whale_threshold_usd);
        }
        _ => {}
    }
}

fn parse_ts(v: &Value) -> i64 {
    v.get("timestamp")
        .and_then(|x| {
            x.as_i64()
                .or_else(|| x.as_str().and_then(|s| s.parse::<i64>().ok()))
        })
        .unwrap_or_else(|| Utc::now().timestamp_millis())
}

fn parse_f64_field(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| {
        x.as_f64()
            .or_else(|| x.as_str().and_then(|s| s.parse::<f64>().ok()))
    })
}

fn process_single_event(
    v: &Value,
    books: &DashMap<String, FastOrderBook>,
    tape: &Mutex<VecDeque<TapeEntry>>,
    events_tx: &broadcast::Sender<FeedEvent>,
    telemetry: &Telemetry,
    whale_threshold_usd: f64,
) {
    let ev_type = v.get("event_type").and_then(|x| x.as_str()).unwrap_or("");
    let ts_ms = parse_ts(v);
    let now_ms = Utc::now().timestamp_millis();
    if ts_ms > 1_000_000_000_000 {
        telemetry.record_ws_lag_ms((now_ms - ts_ms) as f64);
    }

    match ev_type {
        "book" => {
            let Some(asset_id) = v.get("asset_id").and_then(|x| x.as_str()) else {
                return;
            };
            let market_id = v.get("market").and_then(|x| x.as_str()).unwrap_or("");
            let parse_levels = |k: &str| -> Vec<(f64, f64)> {
                v.get(k)
                    .and_then(|x| x.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|lvl| {
                                Some((parse_f64_field(lvl, "price")?, parse_f64_field(lvl, "size")?))
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let bids = parse_levels("bids");
            let asks = parse_levels("asks");
            let hash = v.get("hash").and_then(|x| x.as_str()).map(|s| s.to_string());

            let mut book = books
                .entry(asset_id.to_string())
                .or_insert_with(|| FastOrderBook::new(asset_id.to_string(), market_id.to_string(), 0.01));
            book.apply_snapshot(&bids, &asks, ts_ms, hash);
            let view = book.to_top_view();
            let _ = events_tx.send(FeedEvent::Book { view });
        }
        "price_change" => {
            let market_id = v.get("market").and_then(|x| x.as_str()).unwrap_or("");
            let mut touched: HashSet<String> = HashSet::new();

            if let Some(changes) = v.get("price_changes").and_then(|x| x.as_array()) {
                for ch in changes {
                    let Some(asset_id) = ch.get("asset_id").and_then(|x| x.as_str()) else {
                        continue;
                    };
                    let Some(price) = parse_f64_field(ch, "price") else {
                        continue;
                    };
                    let size = parse_f64_field(ch, "size").unwrap_or(0.0);
                    let side = ch.get("side").and_then(|x| x.as_str()).unwrap_or("BUY");
                    let is_bid = side.eq_ignore_ascii_case("BUY");

                    let mut book = books.entry(asset_id.to_string()).or_insert_with(|| {
                        FastOrderBook::new(asset_id.to_string(), market_id.to_string(), 0.01)
                    });
                    book.apply_delta(is_bid, price, size, ts_ms);
                    touched.insert(asset_id.to_string());
                }
            }

            for asset_id in touched {
                if let Some(book) = books.get(&asset_id) {
                    let _ = events_tx.send(FeedEvent::Book {
                        view: book.to_top_view(),
                    });
                }
            }
        }
        "last_trade_price" => {
            let Some(asset_id) = v.get("asset_id").and_then(|x| x.as_str()) else {
                return;
            };
            let market_id = v.get("market").and_then(|x| x.as_str()).unwrap_or("");
            let Some(price) = parse_f64_field(v, "price") else {
                return;
            };
            let size = parse_f64_field(v, "size").unwrap_or(0.0);
            let side = v
                .get("side")
                .and_then(|x| x.as_str())
                .unwrap_or("BUY")
                .to_uppercase();

            if let Some(mut book) = books.get_mut(asset_id) {
                book.last_trade_price = Some(price);
                book.last_trade_side = Some(side.clone());
                let view = book.to_top_view();
                let _ = events_tx.send(FeedEvent::Book { view });
            }

            let notional = price * size;
            let entry = TapeEntry {
                asset_id: asset_id.to_string(),
                market_id: market_id.to_string(),
                price,
                size,
                notional_usd: notional,
                side,
                timestamp_ms: ts_ms,
                is_whale: notional >= whale_threshold_usd,
            };
            {
                let mut t = tape.lock();
                if t.len() >= 250 {
                    t.pop_front();
                }
                t.push_back(entry.clone());
            }
            let _ = events_tx.send(FeedEvent::Trade { entry });
        }
        "tick_size_change" => {
            if let (Some(asset_id), Some(new_tick)) = (
                v.get("asset_id").and_then(|x| x.as_str()),
                parse_f64_field(v, "new_tick_size"),
            ) && let Some(mut book) = books.get_mut(asset_id)
            {
                book.set_tick_size(new_tick);
                let _ = events_tx.send(FeedEvent::Book {
                    view: book.to_top_view(),
                });
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn top_of_book_zero_alloc_lookup() {
        let telemetry = Arc::new(Telemetry::new());
        let feed = MarketFeed::spawn("ws://127.0.0.1:1".into(), telemetry, 500.0);

        feed.seed_book(
            "tok1",
            "cond1",
            0.01,
            &[(0.48, 100.0)],
            &[(0.50, 100.0)],
            Some(0.49),
            1,
            None,
        );

        assert_eq!(
            feed.get_top_of_book("tok1"),
            Some((Some(0.48), Some(0.50), Some(0.49)))
        );
        assert!(feed.get_top_of_book("missing").is_none());
    }

    #[tokio::test]
    async fn top_of_book_matches_book_view_edge_cases() {
        let telemetry = Arc::new(Telemetry::new());
        let feed = MarketFeed::spawn("ws://127.0.0.1:1".into(), telemetry, 500.0);

        let check_equiv = |asset: &str| {
            let top = feed.get_top_of_book(asset).unwrap();
            let view = feed.get_book_view(asset, 1).unwrap();
            assert_eq!(top, (view.best_bid, view.best_ask, view.mid), "mismatch on {asset}");
        };

        // 1. Empty book
        feed.seed_book("empty", "c1", 0.01, &[], &[], None, 1, None);
        check_equiv("empty");
        assert_eq!(feed.get_top_of_book("empty"), Some((None, None, None)));

        // 2. Bid-only book (one-sided)
        feed.seed_book("bid_only", "c2", 0.01, &[(0.42, 50.0)], &[], None, 2, None);
        check_equiv("bid_only");
        assert_eq!(feed.get_top_of_book("bid_only"), Some((Some(0.42), None, Some(0.42))));

        // 3. Ask-only book (one-sided)
        feed.seed_book("ask_only", "c3", 0.01, &[], &[(0.61, 80.0)], None, 3, None);
        check_equiv("ask_only");
        assert_eq!(feed.get_top_of_book("ask_only"), Some((None, Some(0.61), Some(0.61))));

        // 4. Crossed book (best_bid > best_ask)
        feed.seed_book("crossed", "c4", 0.01, &[(0.55, 20.0)], &[(0.51, 30.0)], None, 4, None);
        check_equiv("crossed");
        assert_eq!(
            feed.get_top_of_book("crossed"),
            Some((Some(0.55), Some(0.51), Some(0.53)))
        );
    }

    #[tokio::test]
    async fn evict_inactive_books_preserves_pinned_and_lru() {
        let telemetry = Arc::new(Telemetry::new());
        let feed = MarketFeed::spawn("ws://127.0.0.1:1".into(), telemetry, 500.0);

        for i in 0..5 {
            let id = format!("tok_{i}");
            feed.subscribe(vec![id.clone()]);
            feed.seed_book(&id, "c", 0.01, &[(0.45, 10.0)], &[(0.55, 10.0)], None, i, None);
        }
        // Touch tok_0 via with_book so it becomes most recently used among unpinned
        let got = feed.with_book("tok_0", |b| b.best_bid()).flatten();
        assert_eq!(got, Some(0.45));

        let pinned: HashSet<String> = ["tok_1".to_string()].into_iter().collect();
        let evicted = feed.evict_inactive_books(&pinned, 2);
        assert_eq!(evicted, 2);

        // tok_1 (pinned), tok_0 (recently touched), tok_4 (latest seeded) remain; tok_2 & tok_3 evicted
        assert!(feed.get_top_of_book("tok_1").is_some());
        assert!(feed.get_top_of_book("tok_0").is_some());
        assert!(feed.get_top_of_book("tok_4").is_some());
        assert!(feed.get_top_of_book("tok_2").is_none());
        assert!(feed.get_top_of_book("tok_3").is_none());
    }
}
