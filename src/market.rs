use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSummary {
    pub id: String,
    pub question: String,
    #[serde(default)]
    pub question_zh: Option<String>,
    pub slug: String,
    pub event_slug: String,
    pub event_title: String,
    pub condition_id: String,
    pub outcomes: Vec<String>,
    #[serde(default)]
    pub outcomes_zh: Vec<String>,
    pub token_ids: Vec<String>,
    pub outcome_prices: Vec<f64>,
    pub tick_size: f64,
    pub min_order_size: f64,
    pub volume_24hr: f64,
    pub liquidity: f64,
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
    pub last_trade_price: Option<f64>,
    pub one_day_change: Option<f64>,
    pub neg_risk: bool,
    pub fees_enabled: bool,
    pub accepting_orders: bool,
    #[serde(default)]
    pub event_start_time: Option<String>,
    pub end_date: Option<String>,
    pub image: Option<String>,
    pub category: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortTimeframeInfo {
    pub coin: String,
    pub chainlink_symbol: String,
    pub binance_symbol: String,
    pub interval_mins: u32,
    pub variant: &'static str,
    pub slot_ts: Option<i64>,
}

/// Seconds left in a 5m / 15m slot: prefers the market's own `end_date`, then the
/// parsed slug slot timestamp (`slot_ts + interval`).
pub fn short_timeframe_countdown_secs(
    end_date: Option<&str>,
    slot_ts: Option<i64>,
    interval_mins: u32,
) -> Option<i64> {
    let end_ts = end_date
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp())
        .or_else(|| slot_ts.map(|s| s + i64::from(interval_mins) * 60))?;
    Some((end_ts - chrono::Utc::now().timestamp()).max(0))
}

pub fn short_timeframe_slot(now_secs: i64, interval_secs: i64) -> i64 {
    if interval_secs <= 0 {
        return now_secs;
    }
    now_secs.div_euclid(interval_secs) * interval_secs
}

pub fn deterministic_updown_slug(coin: &str, interval_mins: u32, now_secs: i64) -> String {
    let interval_secs = i64::from(interval_mins.max(1)) * 60;
    let slot = short_timeframe_slot(now_secs, interval_secs);
    format!("{}-updown-{}m-{slot}", coin.to_ascii_lowercase(), interval_mins)
}

pub fn next_updown_slug(slug: &str, now_secs: i64) -> Option<String> {
    let lower = slug.trim().to_ascii_lowercase();
    let (coin, rest, interval_mins) = if let Some((c, r)) = lower.split_once("-updown-5m-") {
        (c, r, 5_u32)
    } else if let Some((c, r)) = lower.split_once("-updown-15m-") {
        (c, r, 15_u32)
    } else {
        return None;
    };
    let slot = rest.split('-').next()?.parse::<i64>().ok()?;
    let step = i64::from(interval_mins) * 60;
    let next_slot = (slot + step).max(short_timeframe_slot(now_secs, step));
    Some(format!("{coin}-updown-{interval_mins}m-{next_slot}"))
}

fn detect_crypto_coin(text_lower: &str) -> Option<&'static str> {
    if text_lower.contains("btc-updown-") || text_lower.contains("bitcoin") || has_word(text_lower, "btc") {
        Some("btc")
    } else if text_lower.contains("eth-updown-") || text_lower.contains("ethereum") || has_word(text_lower, "eth") {
        Some("eth")
    } else if text_lower.contains("sol-updown-") || text_lower.contains("solana") || has_word(text_lower, "sol") {
        Some("sol")
    } else if text_lower.contains("xrp-updown-") || has_word(text_lower, "xrp") {
        Some("xrp")
    } else if text_lower.contains("doge-updown-") || has_word(text_lower, "doge") {
        Some("doge")
    } else if text_lower.contains("bnb-updown-") || has_word(text_lower, "bnb") {
        Some("bnb")
    } else if text_lower.contains("hype-updown-") || has_word(text_lower, "hype") {
        Some("hype")
    } else {
        None
    }
}

fn has_word(haystack: &str, word: &str) -> bool {
    haystack
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|tok| tok == word)
}

pub fn extract_short_timeframe_info(m: &MarketSummary) -> Option<ShortTimeframeInfo> {
    extract_short_timeframe_info_raw(&m.slug, &m.event_slug, &m.question)
}

/// Same detection as [`extract_short_timeframe_info`], but from the raw strings so
/// FFI hosts (iOS) can identify 5m / 15m Up-Down markets without a full summary.
pub fn extract_short_timeframe_info_raw(
    slug: &str,
    event_slug: &str,
    question: &str,
) -> Option<ShortTimeframeInfo> {
    let slug_l = slug.to_ascii_lowercase();
    let ev_slug_l = event_slug.to_ascii_lowercase();
    let q_l = question.to_ascii_lowercase();

    let (interval_mins, variant, slot_ts) = if slug_l.contains("-updown-5m") || ev_slug_l.contains("-updown-5m") {
        let slot = slug_l
            .split("-updown-5m-")
            .nth(1)
            .or_else(|| ev_slug_l.split("-updown-5m-").nth(1))
            .and_then(|s| s.split('-').next()?.parse::<i64>().ok());
        (5_u32, "fiveminute", slot)
    } else if slug_l.contains("-updown-15m") || ev_slug_l.contains("-updown-15m") {
        let slot = slug_l
            .split("-updown-15m-")
            .nth(1)
            .or_else(|| ev_slug_l.split("-updown-15m-").nth(1))
            .and_then(|s| s.split('-').next()?.parse::<i64>().ok());
        (15_u32, "fifteen", slot)
    } else if (q_l.contains("up or down") || q_l.contains("updown"))
        && (q_l.contains("5 min") || q_l.contains("5m") || q_l.contains(":05") || q_l.contains("5-min"))
    {
        (5_u32, "fiveminute", None)
    } else if (q_l.contains("up or down") || q_l.contains("updown"))
        && (q_l.contains("15 min") || q_l.contains("15m") || q_l.contains("15-min"))
    {
        (15_u32, "fifteen", None)
    } else {
        return None;
    };

    let combined = format!("{slug_l} {ev_slug_l} {q_l}");
    let coin = detect_crypto_coin(&combined)?;
    Some(ShortTimeframeInfo {
        coin: coin.to_string(),
        chainlink_symbol: format!("{coin}/usd"),
        binance_symbol: format!("{coin}usdt"),
        interval_mins,
        variant,
        slot_ts,
    })
}

pub fn parse_crypto_open_price_json(body: &str) -> Result<f64> {
    let val: Value = serde_json::from_str(body).context("invalid crypto-price JSON")?;
    let raw = val
        .get("openPrice")
        .or_else(|| val.get("open_price"))
        .ok_or_else(|| anyhow::anyhow!("missing openPrice field"))?;
    let px = match raw {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .ok_or_else(|| anyhow::anyhow!("invalid openPrice value"))?;
    if !px.is_finite() || px <= 0.0 {
        return Err(anyhow::anyhow!("non-positive openPrice: {px}"));
    }
    Ok(px)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiblingOutcome {
    pub market_id: String,
    pub condition_id: String,
    pub question: String,
    pub group_title: String,
    #[serde(default)]
    pub group_title_zh: Option<String>,
    pub yes_token_id: String,
    pub no_token_id: String,
    pub yes_price: f64,
    pub no_price: f64,
    pub best_ask: Option<f64>,
    pub best_bid: Option<f64>,
    pub liquidity: f64,
    pub tick_size: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSiblings {
    pub event_slug: String,
    pub event_title: String,
    pub neg_risk: bool,
    pub sum_yes_asks: f64,
    pub dutch_book_edge_bps: f64,
    pub outcomes: Vec<SiblingOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricePoint {
    pub t: i64,
    pub p: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HolderInfo {
    pub proxy_wallet: String,
    pub name: String,
    pub pseudonym: String,
    pub token_id: String,
    pub outcome_index: i32,
    pub amount: f64,
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhaleTrade {
    pub proxy_wallet: String,
    pub name: String,
    pub pseudonym: String,
    pub side: String,
    pub token_id: String,
    pub condition_id: String,
    pub outcome: String,
    pub outcome_index: i32,
    pub size: f64,
    pub price: f64,
    pub notional_usd: f64,
    pub timestamp: i64,
    pub title: String,
    pub slug: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhaleFlowSummary {
    pub condition_id: String,
    pub top_holders: Vec<HolderInfo>,
    pub recent_trades: Vec<WhaleTrade>,
    pub yes_whale_vol_usd: f64,
    pub no_whale_vol_usd: f64,
    pub net_flow_usd: f64,
}

#[derive(Clone)]
pub struct MarketClient {
    http: Client,
    gamma_host: String,
    clob_host: String,
    data_host: String,
}

impl MarketClient {
    pub fn new(gamma_host: String, clob_host: String, data_host: String) -> Result<Self> {
        let http = Client::builder()
            .pool_max_idle_per_host(8)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)
            .tcp_keepalive(Duration::from_secs(30))
            .timeout(Duration::from_secs(12))
            .user_agent(concat!(
                "PolyDirect/",
                env!("CARGO_PKG_VERSION"),
                " (low-latency-terminal)"
            ))
            .build()?;

        Ok(Self {
            http,
            gamma_host: gamma_host.trim_end_matches('/').to_string(),
            clob_host: clob_host.trim_end_matches('/').to_string(),
            data_host: data_host.trim_end_matches('/').to_string(),
        })
    }

    pub fn http(&self) -> &Client {
        &self.http
    }

    pub async fn fetch_markets(
        &self,
        query: Option<&str>,
        category: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MarketSummary>> {
        let tr = crate::translate::Translator::new(self.http.clone());
        self.fetch_markets_bilingual(query, category, limit, &tr, false)
            .await
    }

    /// Fetch active markets with full English AND Chinese search support + optional Chinese translation.
    pub async fn fetch_markets_bilingual(
        &self,
        query: Option<&str>,
        category: Option<&str>,
        limit: usize,
        translator: &crate::translate::Translator,
        populate_zh: bool,
    ) -> Result<Vec<MarketSummary>> {
        self.fetch_markets_page(query, category, limit, 0, translator, populate_zh)
            .await
    }

    /// Paginated active markets fetch across ALL Polymarket markets (supports EN + ZH search & offset paging).
    pub async fn fetch_markets_page(
        &self,
        query: Option<&str>,
        category: Option<&str>,
        limit: usize,
        offset: usize,
        translator: &crate::translate::Translator,
        populate_zh: bool,
    ) -> Result<Vec<MarketSummary>> {
        let raw_q = query.unwrap_or("").trim();
        let expanded_queries = if raw_q.is_empty() {
            Vec::new()
        } else {
            translator.expand_search_queries(raw_q).await
        };
        let cat_lower = category.unwrap_or("all").trim().to_lowercase();

        if cat_lower == "5m" || cat_lower == "15m" {
            let mut out = self.fetch_short_timeframe_markets(&cat_lower, limit).await?;
            if !expanded_queries.is_empty() {
                out.retain(|m| market_matches_queries(m, &expanded_queries, translator));
            }
            if populate_zh && !out.is_empty() {
                populate_market_translations(&mut out, translator).await;
            }
            return Ok(out);
        }

        let fetch_limit = if !raw_q.is_empty()
            || (!cat_lower.is_empty() && cat_lower != "all")
        {
            (limit * 3).clamp(120, 250)
        } else {
            limit.clamp(20, 150)
        };

        let url = format!(
            "{}/markets?active=true&closed=false&limit={}&offset={}&order=volume24hr&ascending=false",
            self.gamma_host, fetch_limit, offset
        );

        let raw: Vec<Value> = self
            .http
            .get(&url)
            .send()
            .await
            .context("gamma /markets request failed")?
            .error_for_status()?
            .json()
            .await?;

        let mut out = Vec::with_capacity(limit);
        for v in raw {
            if let Some(m) = parse_gamma_market(&v) {
                if !m.accepting_orders || m.token_ids.is_empty() {
                    continue;
                }
                if !expanded_queries.is_empty()
                    && !market_matches_queries(&m, &expanded_queries, translator)
                {
                    continue;
                }
                if !cat_lower.is_empty() && cat_lower != "all" && m.category != cat_lower {
                    continue;
                }
                out.push(m);
                if out.len() >= limit {
                    break;
                }
            }
        }

        // Always query Gamma /public-search when search query is non-empty (works for BOTH EN & ZH searches)
        if !expanded_queries.is_empty() {
            let gamma_terms: Vec<&str> = expanded_queries
                .iter()
                .map(|s| s.as_str())
                .filter(|s| !crate::translate::contains_cjk(s) && !s.is_empty())
                .take(2)
                .collect();

            for term in gamma_terms {
                let search_url = format!(
                    "{}/public-search?q={}&events_status=active",
                    self.gamma_host,
                    urlencoding_simple(&term.to_lowercase())
                );
                if let Ok(resp) = self.http.get(&search_url).send().await
                    && let Ok(val) = resp.json::<Value>().await
                    && let Some(events) = val.get("events").and_then(|e| e.as_array())
                {
                    for m in parse_events_array_into_markets(events) {
                        if !cat_lower.is_empty() && cat_lower != "all" && m.category != cat_lower {
                            continue;
                        }
                        if !out.iter().any(|x| x.condition_id == m.condition_id) {
                            out.push(m);
                            if out.len() >= limit {
                                break;
                            }
                        }
                    }
                }
                if out.len() >= limit {
                    break;
                }
            }

            // If query looks like 5m/15m/updown or BTC/ETH/SOL/XRP, surface active 5m/15m rounds at top
            let raw_lower = raw_q.to_lowercase();
            if query_wants_short_timeframe(&expanded_queries, &raw_lower)
                && (cat_lower.is_empty() || cat_lower == "all" || cat_lower == "crypto")
            {
                let want_5m = raw_lower.contains("5m") || raw_lower.contains("5min")
                    || expanded_queries.iter().any(|s| {
                        let sl = s.to_lowercase();
                        sl.contains("5m") || sl.contains("5min")
                    });
                let want_15m = raw_lower.contains("15m")
                    || expanded_queries.iter().any(|s| s.to_lowercase().contains("15m"));
                let (m5_res, m15_res) = match (want_5m, want_15m) {
                    (true, false) => (self.fetch_short_timeframe_markets("5m", 16).await.unwrap_or_default(), Vec::new()),
                    (false, true) => (Vec::new(), self.fetch_short_timeframe_markets("15m", 16).await.unwrap_or_default()),
                    _ => {
                        let (a, b) = tokio::join!(
                            self.fetch_short_timeframe_markets("5m", 12),
                            self.fetch_short_timeframe_markets("15m", 12),
                        );
                        (a.unwrap_or_default(), b.unwrap_or_default())
                    }
                };
                let mut front: Vec<MarketSummary> = Vec::new();
                for cand in m5_res.into_iter().chain(m15_res) {
                    if !cand.accepting_orders || cand.token_ids.is_empty() {
                        continue;
                    }
                    let coin_filter = detect_crypto_coin(&raw_lower);
                    let matches_coin = match (coin_filter, extract_short_timeframe_info(&cand)) {
                        (Some(c), Some(info)) => info.coin == c,
                        _ => market_matches_queries(&cand, &expanded_queries, translator) || raw_lower.contains("5m") || raw_lower.contains("15m") || raw_lower.contains("updown") || raw_lower.contains("up or down"),
                    };
                    if matches_coin && !front.iter().any(|x| x.condition_id == cand.condition_id) {
                        front.push(cand);
                    }
                }
                if !front.is_empty() {
                    for existing in out {
                        if !front.iter().any(|x| x.condition_id == existing.condition_id) {
                            front.push(existing);
                        }
                    }
                    front.truncate(limit);
                    out = front;
                }
            }
        }

        if populate_zh && !out.is_empty() {
            populate_market_translations(&mut out, translator).await;
        }

        Ok(out)
    }

    pub async fn fetch_market_by_slug(&self, slug: &str) -> Result<Option<MarketSummary>> {
        let url = format!("{}/events?slug={}", self.gamma_host, urlencoding_simple(slug));
        let events: Vec<Value> = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("gamma /events?slug={slug} failed"))?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_events_array_into_markets(&events).into_iter().next())
    }

    pub async fn fetch_short_timeframe_markets(
        &self,
        tag_slug: &str,
        limit: usize,
    ) -> Result<Vec<MarketSummary>> {
        let interval_mins: u32 = if tag_slug.trim().eq_ignore_ascii_case("15m") {
            15
        } else {
            5
        };
        let interval_secs = i64::from(interval_mins) * 60;
        let now_dt = chrono::Utc::now();
        let now_secs = now_dt.timestamp();
        let now_iso = now_dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        let mut out: Vec<MarketSummary> = Vec::new();
        let tags = if interval_mins == 15 {
            ["15m", "15M"]
        } else {
            ["5m", "5M"]
        };

        for tag in tags {
            let url = format!(
                "{}/events?active=true&closed=false&tag_slug={}&end_date_min={}&order=endDate&ascending=true&limit={}",
                self.gamma_host,
                urlencoding_simple(tag),
                urlencoding_simple(&now_iso),
                limit.clamp(8, 50)
            );
            if let Ok(resp) = self.http.get(&url).send().await
                && let Ok(events) = resp.json::<Vec<Value>>().await
            {
                for m in parse_events_array_into_markets(&events) {
                    let same_tf = extract_short_timeframe_info(&m)
                        .is_some_and(|info| info.interval_mins == interval_mins);
                    if same_tf && !out.iter().any(|x| x.condition_id == m.condition_id) {
                        out.push(m);
                    }
                }
            }
            if !out.is_empty() {
                break;
            }
        }

        // Deterministic slug fallback for top 4 crypto Up/Down coins (`btc`, `eth`, `sol`, `xrp`)
        let fallback_coins = ["btc", "eth", "sol", "xrp"];
        let missing_coins: Vec<&str> = fallback_coins
            .into_iter()
            .filter(|&coin| {
                !out.iter().any(|m| {
                    extract_short_timeframe_info(m)
                        .is_some_and(|i| i.coin == coin && i.interval_mins == interval_mins)
                })
            })
            .collect();

        if !missing_coins.is_empty() {
            let futs = missing_coins.into_iter().map(|coin| async move {
                let s1 = deterministic_updown_slug(coin, interval_mins, now_secs);
                if let Ok(Some(m1)) = self.fetch_market_by_slug(&s1).await {
                    let expired = m1
                        .end_date
                        .as_deref()
                        .and_then(|e| chrono::DateTime::parse_from_rfc3339(e).ok())
                        .is_some_and(|dt| dt.timestamp() <= now_secs);
                    if !expired {
                        return Some(m1);
                    }
                }
                let s2 = deterministic_updown_slug(coin, interval_mins, now_secs + interval_secs);
                self.fetch_market_by_slug(&s2).await.ok().flatten()
            });
            for maybe_m in futures::future::join_all(futs).await {
                if let Some(m) = maybe_m
                    && !out.iter().any(|x| x.condition_id == m.condition_id)
                {
                    out.push(m);
                }
            }
        }

        out.sort_by(|a, b| a.end_date.cmp(&b.end_date).then_with(|| a.slug.cmp(&b.slug)));
        out.truncate(limit);
        Ok(out)
    }

    pub async fn fetch_crypto_open_price(
        &self,
        symbol: &str,
        start_iso: &str,
        end_iso: &str,
        variant: &str,
    ) -> Result<f64> {
        let url = format!(
            "https://polymarket.com/api/crypto/crypto-price?symbol={}&eventStartTime={}&variant={}&endDate={}",
            urlencoding_simple(&symbol.trim().to_ascii_uppercase()),
            urlencoding_simple(start_iso),
            urlencoding_simple(variant),
            urlencoding_simple(end_iso),
        );
        let body = self
            .http
            .get(&url)
            .send()
            .await
            .context("crypto-price request failed")?
            .error_for_status()?
            .text()
            .await?;
        parse_crypto_open_price_json(&body)
    }

    /// Fetch the “Price to Beat” (slot open price) for a 5m / 15m Up-Down slot
    /// directly from the slot timestamp, formatting the ISO window internally.
    pub async fn fetch_crypto_open_price_for_slot(
        &self,
        coin: &str,
        slot_ts: i64,
        interval_mins: u32,
        variant: &str,
    ) -> Result<f64> {
        let step = i64::from(interval_mins.max(1)) * 60;
        let start = chrono::DateTime::from_timestamp(slot_ts, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid slot timestamp {slot_ts}"))?
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let end = chrono::DateTime::from_timestamp(slot_ts + step, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid slot end timestamp"))?
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        self.fetch_crypto_open_price(coin, &start, &end, variant)
            .await
    }

    pub async fn fetch_event_siblings(&self, event_slug: &str) -> Result<EventSiblings> {
        let url = format!(
            "{}/events?slug={}",
            self.gamma_host,
            urlencoding_simple(event_slug)
        );
        let events: Vec<Value> = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let ev = events
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("Event not found: {event_slug}"))?;

        let event_title = ev
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or(event_slug)
            .to_string();
        let neg_risk = ev.get("negRisk").and_then(|v| v.as_bool()).unwrap_or(false);

        let mut outcomes = Vec::new();
        if let Some(markets) = ev.get("markets").and_then(|v| v.as_array()) {
            for mv in markets {
                if let Some(m) = parse_gamma_market(mv) {
                    if !m.accepting_orders || m.token_ids.len() < 2 {
                        continue;
                    }
                    let group_title = mv
                        .get("groupItemTitle")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&m.question)
                        .to_string();
                    let yes_price = *m.outcome_prices.first().unwrap_or(&0.0);
                    let no_price = *m.outcome_prices.get(1).unwrap_or(&(1.0 - yes_price));
                    outcomes.push(SiblingOutcome {
                        market_id: m.id,
                        condition_id: m.condition_id,
                        question: m.question,
                        group_title,
                        group_title_zh: None,
                        yes_token_id: m.token_ids[0].clone(),
                        no_token_id: m.token_ids[1].clone(),
                        yes_price,
                        no_price,
                        best_ask: m.best_ask,
                        best_bid: m.best_bid,
                        liquidity: m.liquidity,
                        tick_size: m.tick_size,
                    });
                }
            }
        }

        // Sort highest probability first
        outcomes.sort_by(|a, b| b.yes_price.total_cmp(&a.yes_price));

        let sum_yes_asks: f64 = outcomes
            .iter()
            .map(|o| o.best_ask.unwrap_or(o.yes_price))
            .sum();
        let dutch_book_edge_bps = if sum_yes_asks > 0.0 && outcomes.len() >= 2 {
            ((1.0 - sum_yes_asks) / sum_yes_asks) * 10_000.0
        } else {
            0.0
        };

        Ok(EventSiblings {
            event_slug: event_slug.to_string(),
            event_title,
            neg_risk,
            sum_yes_asks,
            dutch_book_edge_bps,
            outcomes,
        })
    }

    pub async fn fetch_book_rest(&self, token_id: &str) -> Result<(Vec<(f64, f64)>, Vec<(f64, f64)>, f64, Option<f64>, i64, Option<String>)> {
        let url = format!("{}/book?token_id={}", self.clob_host, token_id);
        let v: Value = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let parse_side = |key: &str| -> Vec<(f64, f64)> {
            v.get(key)
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|lvl| {
                            let p = lvl.get("price")?.as_str()?.parse::<f64>().ok()?;
                            let s = lvl.get("size")?.as_str()?.parse::<f64>().ok()?;
                            Some((p, s))
                        })
                        .collect()
                })
                .unwrap_or_default()
        };

        let bids = parse_side("bids");
        let asks = parse_side("asks");
        let tick_size = v
            .get("tick_size")
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.01);
        let last_trade = v
            .get("last_trade_price")
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse::<f64>().ok());
        let ts = v
            .get("timestamp")
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        let hash = v.get("hash").and_then(|x| x.as_str()).map(|s| s.to_string());

        Ok((bids, asks, tick_size, last_trade, ts, hash))
    }

    pub async fn fetch_price_history(
        &self,
        token_id: &str,
        interval: &str,
        fidelity: u32,
    ) -> Result<Vec<PricePoint>> {
        let interval_clean = match interval {
            "1h" | "6h" | "1d" | "1w" | "1m" | "max" => interval,
            _ => "1d",
        };
        let url = format!(
            "{}/prices-history?market={}&interval={}&fidelity={}",
            self.clob_host, token_id, interval_clean, fidelity
        );
        let v: Value = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let mut pts = Vec::new();
        if let Some(arr) = v.get("history").and_then(|x| x.as_array()) {
            for item in arr {
                if let (Some(t), Some(p)) = (
                    item.get("t").and_then(|x| x.as_i64()),
                    item.get("p").and_then(|x| x.as_f64()),
                ) {
                    pts.push(PricePoint { t, p });
                }
            }
        }
        Ok(pts)
    }

    /// Fetch top holders & recent whale trades via Polymarket Data API v2.
    pub async fn fetch_whale_flow(
        &self,
        condition_id: &str,
        whale_threshold_usd: f64,
    ) -> Result<WhaleFlowSummary> {
        let holders_url = format!(
            "{}/v2/holders?condition={}&limit=20",
            self.data_host, condition_id
        );
        let trades_url = format!(
            "{}/v2/trades?condition={}&limit=60",
            self.data_host, condition_id
        );

        let (holders_res, trades_res) = tokio::join!(
            self.http.get(&holders_url).send(),
            self.http.get(&trades_url).send()
        );

        let mut top_holders = Vec::new();
        if let Ok(resp) = holders_res
            && let Ok(val) = resp.json::<Value>().await
            && let Some(groups) = val.get("data").and_then(|d| d.as_array())
        {
            for g in groups {
                let tok = g
                    .get("token_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(hs) = g.get("holders").and_then(|x| x.as_array()) {
                    for h in hs {
                        top_holders.push(HolderInfo {
                            proxy_wallet: h
                                .get("proxy_wallet")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            name: h
                                .get("name")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            pseudonym: h
                                .get("pseudonym")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            token_id: h
                                .get("token_id")
                                .and_then(|x| x.as_str())
                                .unwrap_or(&tok)
                                .to_string(),
                            outcome_index: h
                                .get("outcome_index")
                                .and_then(|x| x.as_i64())
                                .unwrap_or(0) as i32,
                            amount: h.get("amount").and_then(|x| x.as_f64()).unwrap_or(0.0),
                            verified: h.get("verified").and_then(|x| x.as_bool()).unwrap_or(false),
                        });
                    }
                }
            }
        }
        top_holders.sort_by(|a, b| b.amount.total_cmp(&a.amount));
        top_holders.truncate(20);

        let mut recent_trades = Vec::new();
        let mut yes_whale_vol_usd = 0.0;
        let mut no_whale_vol_usd = 0.0;

        if let Ok(resp) = trades_res
            && let Ok(val) = resp.json::<Value>().await
            && let Some(arr) = val.get("data").and_then(|d| d.as_array())
        {
            for tr in arr {
                if let Some(wt) = parse_data_v2_trade(tr) {
                    if wt.notional_usd >= whale_threshold_usd {
                        let is_buy = wt.side.eq_ignore_ascii_case("BUY");
                        if (wt.outcome_index == 0 && is_buy) || (wt.outcome_index == 1 && !is_buy) {
                            yes_whale_vol_usd += wt.notional_usd;
                        } else {
                            no_whale_vol_usd += wt.notional_usd;
                        }
                    }
                    recent_trades.push(wt);
                }
            }
        }

        Ok(WhaleFlowSummary {
            condition_id: condition_id.to_string(),
            top_holders,
            recent_trades,
            yes_whale_vol_usd,
            no_whale_vol_usd,
            net_flow_usd: yes_whale_vol_usd - no_whale_vol_usd,
        })
    }

    /// Fetch latest trades for a specific tracked wallet (used by Copy-Trade engine).
    pub async fn fetch_wallet_trades(&self, wallet: &str, limit: usize) -> Result<Vec<WhaleTrade>> {
        let url = format!(
            "{}/v2/trades?user={}&limit={}",
            self.data_host,
            wallet.trim(),
            limit.clamp(1, 50)
        );
        let val: Value = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let mut out = Vec::new();
        if let Some(arr) = val.get("data").and_then(|d| d.as_array()) {
            for tr in arr {
                if let Some(wt) = parse_data_v2_trade(tr) {
                    out.push(wt);
                }
            }
        }
        Ok(out)
    }

    /// Fetch live positions for an authenticated wallet from Data API v2.
    pub async fn fetch_wallet_positions(&self, wallet: &str) -> Result<Vec<Value>> {
        let url = format!("{}/v2/positions?user={}&limit=100", self.data_host, wallet);
        let val: Value = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(val
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default())
    }
}

fn parse_data_v2_trade(tr: &Value) -> Option<WhaleTrade> {
    let size = tr.get("size").and_then(|x| x.as_f64())?;
    let price = tr.get("price").and_then(|x| x.as_f64())?;
    Some(WhaleTrade {
        proxy_wallet: tr
            .get("proxy_wallet")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        name: tr
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        pseudonym: tr
            .get("pseudonym")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        side: tr
            .get("side")
            .and_then(|x| x.as_str())
            .unwrap_or("BUY")
            .to_string(),
        token_id: tr
            .get("token_id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        condition_id: tr
            .get("condition_id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        outcome: tr
            .get("outcome")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        outcome_index: tr
            .get("outcome_index")
            .and_then(|x| x.as_i64())
            .unwrap_or(0) as i32,
        size,
        price,
        notional_usd: size * price,
        timestamp: tr.get("timestamp").and_then(|x| x.as_i64()).unwrap_or(0),
        title: tr
            .get("title")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        slug: tr
            .get("slug")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

fn parse_string_list(val: Option<&Value>) -> Vec<String> {
    match val {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect(),
        Some(Value::String(s)) => serde_json::from_str::<Vec<String>>(s).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn parse_f64_list(val: Option<&Value>) -> Vec<f64> {
    parse_string_list(val)
        .into_iter()
        .filter_map(|s| s.parse::<f64>().ok())
        .collect()
}

fn infer_category(question: &str, slug: &str, event_title: &str) -> String {
    let text = format!("{question} {slug} {event_title}").to_lowercase();
    if [
        "vs.", "vs ", "nba", "nfl", "nhl", "mlb", "ufc", "f1", "premier league", "champions league",
        "celtics", "lakers", "spread:", "o/u ", "ncaa", "tennis", "soccer", "world cup", "mvp",
    ]
    .iter()
    .any(|k| text.contains(k))
    {
        return "sports".to_string();
    }
    if [
        "bitcoin", "btc", "ethereum", "eth", "solana", "sol", "crypto", "xrp", "doge", "fdv",
        "token", "binance", "coinbase", "microstrategy", "airdrop",
    ]
    .iter()
    .any(|k| text.contains(k))
    {
        return "crypto".to_string();
    }
    if [
        "trump", "biden", "election", "senate", "house", "president", "tariff", "china", "russia",
        "ukraine", "israel", "iran", "prime minister", "ceasefire", "xi jinping", "nato", "war",
        "congress", "governor", "mayor", "parliament", "supreme court",
    ]
    .iter()
    .any(|k| text.contains(k))
    {
        return "politics".to_string();
    }
    if [
        "fed ", "fomc", "interest rate", "cpi", "inflation", "gdp", "recession", "s&p", "nasdaq",
        "stock", "gold", "oil", "crude", "nvidia", "tesla", "apple", "ipo", "earnings", "treasury",
    ]
    .iter()
    .any(|k| text.contains(k))
    {
        return "finance".to_string();
    }
    if [
        "openai", "gpt", "claude", "gemini", "ai ", "anthropic", "spacex", "starship", "gta",
        "tiktok", "youtube", "twitter", "x.com", "apple", "google", "microsoft", "meta ", "elon",
    ]
    .iter()
    .any(|k| text.contains(k))
    {
        return "tech".to_string();
    }
    "other".to_string()
}

fn parse_gamma_market(v: &Value) -> Option<MarketSummary> {
    let id = v.get("id").and_then(|x| x.as_str())?.to_string();
    let question = v.get("question").and_then(|x| x.as_str())?.to_string();
    let slug = v
        .get("slug")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let condition_id = v
        .get("conditionId")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();

    let first_ev = v
        .get("events")
        .and_then(|x| x.as_array())
        .and_then(|a| a.first());

    let (event_slug, event_title) = first_ev
        .map(|ev| {
            (
                ev.get("slug")
                    .and_then(|s| s.as_str())
                    .unwrap_or(&slug)
                    .to_string(),
                ev.get("title")
                    .and_then(|s| s.as_str())
                    .unwrap_or(&question)
                    .to_string(),
            )
        })
        .unwrap_or_else(|| (slug.clone(), question.clone()));

    let outcomes = parse_string_list(v.get("outcomes"));
    let token_ids = parse_string_list(v.get("clobTokenIds"));
    let outcome_prices = parse_f64_list(v.get("outcomePrices"));

    let tick_size = v
        .get("orderPriceMinTickSize")
        .and_then(|x| x.as_f64())
        .unwrap_or(0.01);
    let min_order_size = v
        .get("orderMinSize")
        .and_then(|x| x.as_f64())
        .unwrap_or(5.0);
    let volume_24hr = v
        .get("volume24hr")
        .and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|s| s.parse::<f64>().ok())))
        .or_else(|| v.get("volumeNum").and_then(|x| x.as_f64()))
        .or_else(|| {
            v.get("volume")
                .or_else(|| v.get("volumeClob"))
                .and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|s| s.parse::<f64>().ok())))
        })
        .unwrap_or(0.0);
    let liquidity = v
        .get("liquidityNum")
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0);

    let category = infer_category(&question, &slug, &event_title);

    let event_start_time = v
        .get("eventStartTime")
        .or_else(|| v.get("startDate"))
        .or_else(|| first_ev.and_then(|ev| ev.get("startTime").or_else(|| ev.get("startDate"))))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let end_date = v
        .get("endDate")
        .or_else(|| first_ev.and_then(|ev| ev.get("endDate")))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());

    Some(MarketSummary {
        id,
        question,
        question_zh: None,
        slug,
        event_slug,
        event_title,
        condition_id,
        outcomes,
        outcomes_zh: Vec::new(),
        token_ids,
        outcome_prices,
        tick_size,
        min_order_size,
        volume_24hr,
        liquidity,
        best_bid: v.get("bestBid").and_then(|x| x.as_f64()),
        best_ask: v.get("bestAsk").and_then(|x| x.as_f64()),
        last_trade_price: v.get("lastTradePrice").and_then(|x| x.as_f64()),
        one_day_change: v.get("oneDayPriceChange").and_then(|x| x.as_f64()),
        neg_risk: v.get("negRisk").and_then(|x| x.as_bool()).unwrap_or(false),
        fees_enabled: v
            .get("feesEnabled")
            .and_then(|x| x.as_bool())
            .unwrap_or(true),
        accepting_orders: v
            .get("acceptingOrders")
            .and_then(|x| x.as_bool())
            .unwrap_or(true),
        event_start_time,
        end_date,
        image: v
            .get("icon")
            .or_else(|| v.get("image"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        category,
    })
}

fn parse_events_array_into_markets(events: &[Value]) -> Vec<MarketSummary> {
    let mut out = Vec::new();
    for ev in events {
        let ev_slug = ev.get("slug").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let ev_title = ev.get("title").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let ev_start = ev
            .get("startTime")
            .or_else(|| ev.get("startDate"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        let ev_end = ev
            .get("endDate")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        if let Some(markets) = ev.get("markets").and_then(|m| m.as_array()) {
            for mv in markets {
                if let Some(mut m) = parse_gamma_market(mv) {
                    if m.event_slug.is_empty() {
                        m.event_slug = ev_slug.clone();
                    }
                    if m.event_title.is_empty() {
                        m.event_title = ev_title.clone();
                    }
                    if m.event_start_time.is_none() {
                        m.event_start_time = ev_start.clone();
                    }
                    if m.end_date.is_none() {
                        m.end_date = ev_end.clone();
                    }
                    if m.accepting_orders && !m.token_ids.is_empty() {
                        out.push(m);
                    }
                }
            }
        }
    }
    out
}

fn market_matches_queries(
    m: &MarketSummary,
    expanded_queries: &[String],
    translator: &crate::translate::Translator,
) -> bool {
    let hay_en = format!(
        "{} {} {} {} {}",
        m.question.to_lowercase(),
        m.slug.to_lowercase(),
        m.event_title.to_lowercase(),
        m.category.to_lowercase(),
        m.outcomes.join(" ").to_lowercase()
    );
    let hay_zh = translator
        .get_zh(&m.question)
        .unwrap_or_default()
        .to_lowercase();

    expanded_queries.iter().any(|cand| {
        let cl = cand.to_lowercase();
        if hay_en.contains(&cl) || (!hay_zh.is_empty() && hay_zh.contains(&cl)) {
            return true;
        }
        if !crate::translate::contains_cjk(&cl) {
            let toks: Vec<&str> = cl
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| {
                    w.len() >= 2
                        && !matches!(
                            *w,
                            "the" | "will" | "in" | "of" | "to" | "by" | "for" | "on" | "at" | "and" | "or"
                        )
                })
                .collect();
            if toks.len() >= 2 && toks.iter().all(|t| hay_en.contains(t)) {
                return true;
            }
        }
        false
    })
}

fn query_wants_short_timeframe(expanded_queries: &[String], raw_lower: &str) -> bool {
    const KEYS: &[&str] = &[
        "5m", "5min", "15m", "updown", "up or down", "btc", "bitcoin", "eth", "ethereum", "sol",
        "solana", "xrp",
    ];
    if KEYS.iter().any(|k| raw_lower.contains(k)) {
        return true;
    }
    expanded_queries.iter().any(|cand| {
        let cl = cand.to_lowercase();
        KEYS.iter().any(|k| cl.contains(k))
    })
}

async fn populate_market_translations(
    out: &mut [MarketSummary],
    translator: &crate::translate::Translator,
) {
    let mut texts = Vec::with_capacity(out.len() * 3);
    for m in out.iter() {
        texts.push(m.question.clone());
        for o in &m.outcomes {
            texts.push(o.clone());
        }
    }
    translator.translate_en_to_zh_batch(&texts).await;
    for m in out.iter_mut() {
        m.question_zh = translator.get_zh(&m.question);
        m.outcomes_zh = m
            .outcomes
            .iter()
            .map(|o| translator.get_zh(o).unwrap_or_else(|| o.clone()))
            .collect();
    }
}

fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push_str("%20"),
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_market(slug: &str, question: &str) -> MarketSummary {
        MarketSummary {
            id: "1".into(),
            question: question.into(),
            question_zh: None,
            slug: slug.into(),
            event_slug: slug.into(),
            event_title: question.into(),
            condition_id: "0xabc".into(),
            outcomes: vec!["Up".into(), "Down".into()],
            outcomes_zh: Vec::new(),
            token_ids: vec!["t1".into(), "t2".into()],
            outcome_prices: vec![0.5, 0.5],
            tick_size: 0.01,
            min_order_size: 5.0,
            volume_24hr: 0.0,
            liquidity: 0.0,
            best_bid: None,
            best_ask: None,
            last_trade_price: None,
            one_day_change: None,
            neg_risk: false,
            fees_enabled: true,
            accepting_orders: true,
            event_start_time: None,
            end_date: None,
            image: None,
            category: "crypto".into(),
        }
    }

    #[test]
    fn short_timeframe_slot_slug_and_symbol_extraction_work() {
        assert_eq!(short_timeframe_slot(1_764_100_123, 300), 1_764_099_900);
        assert_eq!(short_timeframe_slot(1_764_100_323, 300), 1_764_100_200);
        assert_eq!(short_timeframe_slot(1_764_100_323, 900), 1_764_099_900);
        assert_eq!(
            deterministic_updown_slug("BTC", 5, 1_764_100_123),
            "btc-updown-5m-1764099900"
        );
        assert_eq!(
            next_updown_slug("btc-updown-5m-1764099900", 1_764_100_123).as_deref(),
            Some("btc-updown-5m-1764100200")
        );
        // Expired old slot advances at least to current active slot
        assert_eq!(
            next_updown_slug("eth-updown-15m-1764000000", 1_764_100_323).as_deref(),
            Some("eth-updown-15m-1764099900")
        );

        let info5 = extract_short_timeframe_info(&stub_market(
            "sol-updown-5m-1764099900",
            "Solana Up or Down - 5 minutes",
        ))
        .unwrap();
        assert_eq!(info5.coin, "sol");
        assert_eq!(info5.chainlink_symbol, "sol/usd");
        assert_eq!(info5.binance_symbol, "solusdt");
        assert_eq!(info5.interval_mins, 5);
        assert_eq!(info5.variant, "fiveminute");
        assert_eq!(info5.slot_ts, Some(1_764_099_900));

        let info15 = extract_short_timeframe_info(&stub_market(
            "xrp-updown-15m-1764099600",
            "XRP Up or Down - 15m",
        ))
        .unwrap();
        assert_eq!(info15.coin, "xrp");
        assert_eq!(info15.chainlink_symbol, "xrp/usd");
        assert_eq!(info15.interval_mins, 15);
        assert_eq!(info15.variant, "fifteen");
    }

    #[test]
    fn crypto_open_price_json_parses_number_or_string_and_rejects_bad_payloads() {
        assert!((parse_crypto_open_price_json(r#"{"openPrice":91234.56}"#).unwrap() - 91234.56).abs() < 1e-9);
        assert!((parse_crypto_open_price_json(r#"{"openPrice":"3140.25"}"#).unwrap() - 3140.25).abs() < 1e-9);
        assert!(parse_crypto_open_price_json(r#"{"openPrice":0}"#).is_err());
        assert!(parse_crypto_open_price_json(r#"{"other":123}"#).is_err());
    }
}
