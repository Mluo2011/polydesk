use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
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

enum FeedCmd {
    Subscribe(Vec<String>),
    #[allow(dead_code)]
    Unsubscribe(Vec<String>),
}

pub struct MarketFeed {
    books: Arc<DashMap<String, FastOrderBook>>,
    tape: Arc<Mutex<VecDeque<TapeEntry>>>,
    subscribed: Arc<Mutex<HashSet<String>>>,
    cmd_tx: mpsc::UnboundedSender<FeedCmd>,
    events_tx: broadcast::Sender<FeedEvent>,
    whale_threshold: Arc<Mutex<f64>>,
}

impl MarketFeed {
    pub fn spawn(ws_url: String, telemetry: Arc<Telemetry>, whale_threshold_usd: f64) -> Arc<Self> {
        let books = Arc::new(DashMap::new());
        let tape = Arc::new(Mutex::new(VecDeque::with_capacity(200)));
        let subscribed = Arc::new(Mutex::new(HashSet::new()));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (events_tx, _) = broadcast::channel(2048);
        let whale_threshold = Arc::new(Mutex::new(whale_threshold_usd));

        let feed = Arc::new(Self {
            books: books.clone(),
            tape: tape.clone(),
            subscribed: subscribed.clone(),
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

    pub fn subscribe(&self, asset_ids: Vec<String>) {
        let mut sub = self.subscribed.lock();
        let mut to_add = Vec::new();
        for id in asset_ids {
            if !id.is_empty() && sub.insert(id.clone()) {
                to_add.push(id);
            }
        }
        drop(sub);
        if !to_add.is_empty() {
            let _ = self.cmd_tx.send(FeedCmd::Subscribe(to_add));
        }
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
        let mut entry = self
            .books
            .entry(asset_id.to_string())
            .or_insert_with(|| FastOrderBook::new(asset_id.to_string(), market_id.to_string(), tick_size));
        entry.set_tick_size(tick_size);
        entry.apply_snapshot(bids, asks, ts_ms, hash);
        if let Some(lt) = last_trade {
            entry.last_trade_price = Some(lt);
        }
        let view = entry.to_view(60);
        let _ = self.events_tx.send(FeedEvent::Book { view: view.clone() });
        view
    }

    pub fn get_book_view(&self, asset_id: &str, levels: usize) -> Option<BookView> {
        self.books.get(asset_id).map(|b| b.to_view(levels))
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
}

#[allow(clippy::too_many_arguments)]
async fn run_ws_loop(
    ws_url: String,
    books: Arc<DashMap<String, FastOrderBook>>,
    tape: Arc<Mutex<VecDeque<TapeEntry>>>,
    subscribed: Arc<Mutex<HashSet<String>>>,
    mut cmd_rx: mpsc::UnboundedReceiver<FeedCmd>,
    events_tx: broadcast::Sender<FeedEvent>,
    telemetry: Arc<Telemetry>,
    whale_threshold: Arc<Mutex<f64>>,
) {
    let mut backoff_ms = 250u64;

    loop {
        // Connect with disable_nagle = true (TCP_NODELAY) for minimum packet latency
        match connect_async_with_config(&ws_url, None, true).await {
            Ok((ws_stream, _)) => {
                info!(url = %ws_url, "Connected to Polymarket CLOB market WebSocket (TCP_NODELAY)");
                backoff_ms = 250;
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
            let view = book.to_view(60);
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
                        view: book.to_view(60),
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
                let view = book.to_view(60);
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
                    view: book.to_view(60),
                });
            }
        }
        _ => {}
    }
}
