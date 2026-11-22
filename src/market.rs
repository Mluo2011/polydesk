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
    pub end_date: Option<String>,
    pub image: Option<String>,
    pub category: String,
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
            .user_agent("PolyDesk/0.1 (low-latency-terminal)")
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
        let raw_q = query.unwrap_or("").trim();
        let expanded_queries = if raw_q.is_empty() {
            Vec::new()
        } else {
            translator.expand_search_queries(raw_q).await
        };
        let cat_lower = category.unwrap_or("all").trim().to_lowercase();

        let fetch_limit = if !raw_q.is_empty()
            || (!cat_lower.is_empty() && cat_lower != "all")
        {
            (limit * 4).clamp(80, 200)
        } else {
            limit.clamp(10, 100)
        };

        let url = format!(
            "{}/markets?active=true&closed=false&limit={}&order=volume24hr&ascending=false",
            self.gamma_host, fetch_limit
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
                if !expanded_queries.is_empty() {
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

                    let matched = expanded_queries.iter().any(|cand| {
                        let cl = cand.to_lowercase();
                        if hay_en.contains(&cl) || (!hay_zh.is_empty() && hay_zh.contains(&cl)) {
                            return true;
                        }
                        // Token-wise match for multi-word English queries (e.g. "fed interest rates")
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
                    });
                    if !matched {
                        continue;
                    }
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

        // Also search Gamma /public-search using the best English queries (works for BOTH EN & ZH searches!)
        if !expanded_queries.is_empty() && out.len() < 12 {
            let gamma_terms: Vec<&str> = expanded_queries
                .iter()
                .map(|s| s.as_str())
                .filter(|s| !crate::translate::contains_cjk(s) && !s.is_empty())
                .take(2)
                .collect();

            for term in gamma_terms {
                let search_url = format!(
                    "{}/public-search?q={}",
                    self.gamma_host,
                    urlencoding_simple(&term.to_lowercase())
                );
                if let Ok(resp) = self.http.get(&search_url).send().await
                    && let Ok(val) = resp.json::<Value>().await
                    && let Some(events) = val.get("events").and_then(|e| e.as_array())
                {
                    for ev in events {
                        let ev_slug = ev.get("slug").and_then(|s| s.as_str()).unwrap_or("").to_string();
                        let ev_title = ev.get("title").and_then(|s| s.as_str()).unwrap_or("").to_string();
                        if let Some(markets) = ev.get("markets").and_then(|m| m.as_array()) {
                            for mv in markets {
                                if let Some(mut m) = parse_gamma_market(mv) {
                                    if m.event_slug.is_empty() {
                                        m.event_slug = ev_slug.clone();
                                    }
                                    if m.event_title.is_empty() {
                                        m.event_title = ev_title.clone();
                                    }
                                    if m.accepting_orders
                                        && !m.token_ids.is_empty()
                                        && !out.iter().any(|x| x.condition_id == m.condition_id)
                                    {
                                        out.push(m);
                                        if out.len() >= limit {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        if out.len() >= limit {
                            break;
                        }
                    }
                }
                if out.len() >= limit {
                    break;
                }
            }
        }

        if populate_zh && !out.is_empty() {
            let mut texts = Vec::with_capacity(out.len() * 3);
            for m in &out {
                texts.push(m.question.clone());
                for o in &m.outcomes {
                    texts.push(o.clone());
                }
            }
            translator.translate_en_to_zh_batch(&texts).await;
            for m in &mut out {
                m.question_zh = translator.get_zh(&m.question);
                m.outcomes_zh = m
                    .outcomes
                    .iter()
                    .map(|o| translator.get_zh(o).unwrap_or_else(|| o.clone()))
                    .collect();
            }
        }

        Ok(out)
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
        "trump", "biden", "election", "senate", "house", "president", "fed ", "interest rate",
        "fomc", "cpi", "tariff", "china", "russia", "ukraine", "israel", "iran", "prime minister",
        "ceasefire", "xi jinping", "nato",
    ]
    .iter()
    .any(|k| text.contains(k))
    {
        return "politics".to_string();
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

    let (event_slug, event_title) = v
        .get("events")
        .and_then(|x| x.as_array())
        .and_then(|a| a.first())
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
        .and_then(|x| x.as_f64())
        .or_else(|| v.get("volumeNum").and_then(|x| x.as_f64()))
        .unwrap_or(0.0);
    let liquidity = v
        .get("liquidityNum")
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0);

    let category = infer_category(&question, &slug, &event_title);

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
        end_date: v
            .get("endDate")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        image: v
            .get("icon")
            .or_else(|| v.get("image"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        category,
    })
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
