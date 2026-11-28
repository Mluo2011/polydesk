use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use dashmap::DashMap;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::i18n::{Lang, tr};
use crate::market::{MarketSummary, extract_short_timeframe_info};

const MAX_RSS_BYTES: usize = 512 * 1024;
const NEWS_CACHE_TTL: Duration = Duration::from_secs(180);
const MAX_NEWS_CACHE_ENTRIES: usize = 64;
const DEFAULT_MAX_NEWS_ITEMS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NewsWindow {
    Day24h,
    Week7d,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NewsEdition {
    En,
    Zh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsItem {
    pub title: String,
    pub url: String,
    pub source: String,
    pub published_ts: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsFeedResult {
    pub query: String,
    pub provider: &'static str,
    pub items: Vec<NewsItem>,
    pub fetched_at_ts: i64,
}

/// Isolated async RSS news client for per-market headline context.
/// Uses its own `reqwest::Client` pool so headline fetches never contend with CLOB requests.
pub struct NewsClient {
    http: Client,
    cache: DashMap<(String, NewsWindow, NewsEdition), (Instant, NewsFeedResult)>,
}

impl NewsClient {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::limited(4))
            .user_agent(concat!("PolyDirect/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed building news HTTP client")?;
        Ok(Self {
            http,
            cache: DashMap::new(),
        })
    }

    pub async fn fetch_news(
        &self,
        query: &str,
        window: NewsWindow,
        edition: NewsEdition,
        force_refresh: bool,
    ) -> Result<NewsFeedResult> {
        let clean_q: String = query.split_whitespace().collect::<Vec<_>>().join(" ");
        if clean_q.is_empty() {
            return Ok(NewsFeedResult {
                query: String::new(),
                provider: "Google News",
                items: Vec::new(),
                fetched_at_ts: Utc::now().timestamp(),
            });
        }

        let cache_key = (clean_q.to_ascii_lowercase(), window, edition);
        if !force_refresh
            && let Some(hit) = self.cache.get(&cache_key)
            && hit.0.elapsed() < NEWS_CACHE_TTL
        {
            return Ok(hit.1.clone());
        }

        let g_query = match window {
            NewsWindow::Day24h => format!("{clean_q} when:1d"),
            NewsWindow::Week7d => format!("{clean_q} when:7d"),
            NewsWindow::All => clean_q.clone(),
        };
        let (hl, gl, ceid) = match edition {
            NewsEdition::En => ("en-US", "US", "US:en"),
            NewsEdition::Zh => ("zh-CN", "CN", "CN:zh-Hans"),
        };

        let mut last_err: Option<anyhow::Error> = None;
        match self
            .fetch_rss_limited(
                "https://news.google.com/rss/search",
                &[("q", g_query.as_str()), ("hl", hl), ("gl", gl), ("ceid", ceid)],
            )
            .await
        {
            Ok(xml) => {
                let items = parse_rss_items(&xml, DEFAULT_MAX_NEWS_ITEMS);
                if !items.is_empty() {
                    let res = NewsFeedResult {
                        query: clean_q,
                        provider: "Google News",
                        items,
                        fetched_at_ts: Utc::now().timestamp(),
                    };
                    self.insert_cache(cache_key, res.clone());
                    return Ok(res);
                }
            }
            Err(e) => {
                last_err = Some(e);
            }
        }

        // Fallback: Bing News RSS
        let setlang = match edition {
            NewsEdition::En => "en-us",
            NewsEdition::Zh => "zh-hans",
        };
        match self
            .fetch_rss_limited(
                "https://www.bing.com/news/search",
                &[("q", clean_q.as_str()), ("format", "rss"), ("setlang", setlang)],
            )
            .await
        {
            Ok(xml) => {
                let items = parse_rss_items(&xml, DEFAULT_MAX_NEWS_ITEMS);
                let res = NewsFeedResult {
                    query: clean_q,
                    provider: "Bing News",
                    items,
                    fetched_at_ts: Utc::now().timestamp(),
                };
                if !res.items.is_empty() {
                    self.insert_cache(cache_key, res.clone());
                }
                Ok(res)
            }
            Err(e) => {
                if let Some(prev) = last_err {
                    Err(anyhow!("{prev}; fallback failed: {e}"))
                } else {
                    Err(e)
                }
            }
        }
    }

    async fn fetch_rss_limited(&self, base_url: &str, params: &[(&str, &str)]) -> Result<String> {
        let mut resp = self
            .http
            .get(base_url)
            .query(params)
            .send()
            .await?
            .error_for_status()?;
        let mut buf: Vec<u8> = Vec::with_capacity(16 * 1024);
        while let Some(chunk) = resp.chunk().await? {
            let rem = MAX_RSS_BYTES.saturating_sub(buf.len());
            if rem == 0 {
                break;
            }
            let take = chunk.len().min(rem);
            buf.extend_from_slice(&chunk[..take]);
            if buf.len() >= MAX_RSS_BYTES {
                break;
            }
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    fn insert_cache(&self, key: (String, NewsWindow, NewsEdition), val: NewsFeedResult) {
        if self.cache.len() >= MAX_NEWS_CACHE_ENTRIES {
            let mut oldest_key: Option<(String, NewsWindow, NewsEdition)> = None;
            let mut oldest_time = Instant::now();
            for entry in &self.cache {
                if entry.value().0 <= oldest_time {
                    oldest_time = entry.value().0;
                    oldest_key = Some(entry.key().clone());
                }
            }
            if let Some(k) = oldest_key {
                self.cache.remove(&k);
            } else {
                self.cache.clear();
            }
        }
        self.cache.insert(key, (Instant::now(), val));
    }
}

/// Derive a crisp search query for a Polymarket market (crypto short-TF slots map to
/// spot coin keywords; other markets strip betting/date noise from `question`).
pub fn build_market_news_query(m: &MarketSummary) -> String {
    if let Some(info) = extract_short_timeframe_info(m) {
        return match info.coin.as_str() {
            "btc" => "Bitcoin BTC crypto".into(),
            "eth" => "Ethereum ETH crypto".into(),
            "sol" => "Solana SOL crypto".into(),
            "xrp" => "XRP Ripple crypto".into(),
            "doge" => "Dogecoin DOGE crypto".into(),
            "bnb" => "BNB Binance crypto".into(),
            "hype" => "Hyperliquid HYPE crypto".into(),
            other => format!("{} crypto", other.to_ascii_uppercase()),
        };
    }

    let q_clean = clean_market_title_for_query(&m.question);
    let q_words = q_clean.split_whitespace().count();
    if q_words >= 2 {
        return q_clean;
    }
    let e_clean = clean_market_title_for_query(&m.event_title);
    if !e_clean.is_empty() {
        return e_clean;
    }
    q_clean
}

fn clean_market_title_for_query(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    let lower = s.to_ascii_lowercase();
    for prefix in ["spread:", "moneyline:", "handicap:", "total:", "o/u:"] {
        if lower.starts_with(prefix) {
            s = s[prefix.len()..].trim().to_string();
            break;
        }
    }

    // Remove parenthesized spreads like (-5.5) or (+3.5)
    let mut no_parens = String::with_capacity(s.len());
    let mut in_paren = false;
    let mut paren_buf = String::new();
    for ch in s.chars() {
        if ch == '(' {
            in_paren = true;
            paren_buf.clear();
        } else if ch == ')' && in_paren {
            in_paren = false;
            let trimmed = paren_buf.trim();
            let looks_like_spread = trimmed.starts_with('+')
                || trimmed.starts_with('-')
                || trimmed.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '+' || c == '-');
            if !looks_like_spread && !trimmed.is_empty() {
                no_parens.push(' ');
                no_parens.push_str(trimmed);
            }
        } else if in_paren {
            paren_buf.push(ch);
        } else {
            no_parens.push(ch);
        }
    }

    let mut words: Vec<String> = Vec::new();
    for raw_tok in no_parens.split_whitespace() {
        let tok = raw_tok
            .trim_matches(|c: char| matches!(c, '?' | '!' | ',' | ';' | '"' | ':'))
            .trim_end_matches('.');
        if tok.is_empty() || is_iso_date(tok) {
            continue;
        }
        let low = tok.to_ascii_lowercase();
        if words.is_empty()
            && matches!(
                low.as_str(),
                "will" | "does" | "do" | "is" | "are" | "can" | "could" | "did" | "has" | "have"
            )
        {
            continue;
        }
        if matches!(
            low.as_str(),
            "the"
                | "a"
                | "an"
                | "be"
                | "by"
                | "on"
                | "in"
                | "at"
                | "of"
                | "to"
                | "for"
                | "after"
                | "before"
                | "next"
                | "win"
                | "wins"
                | "match"
                | "vs"
                | "o/u"
        ) {
            continue;
        }
        words.push(tok.to_string());
        if words.len() >= 8 {
            break;
        }
    }

    let mut out = String::new();
    for w in words {
        let extra = if out.is_empty() { w.len() } else { w.len() + 1 };
        if out.len() + extra > 80 {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&w);
    }
    out
}

fn is_iso_date(tok: &str) -> bool {
    let b = tok.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[0..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..10].iter().all(u8::is_ascii_digit)
}

/// Zero-dependency RSS `<item>` parser with XML entity unescaping, strict `http`/`https`
/// URL validation, publisher suffix deduplication, and reverse-chronological sorting.
pub fn parse_rss_items(xml: &str, max_items: usize) -> Vec<NewsItem> {
    if max_items == 0 || xml.is_empty() {
        return Vec::new();
    }
    let lower = xml.to_ascii_lowercase();
    let mut items: Vec<NewsItem> = Vec::new();
    let mut seen_titles: HashSet<String> = HashSet::new();
    let mut pos = 0usize;

    while let Some(rel_start) = lower[pos..].find("<item") {
        let tag_start = pos + rel_start;
        let after_lt = tag_start + 5;
        let Some(next_ch) = lower.as_bytes().get(after_lt).copied() else {
            break;
        };
        if !matches!(next_ch, b'>' | b' ' | b'\t' | b'\r' | b'\n') {
            pos = after_lt;
            continue;
        }
        let Some(rel_end) = lower[tag_start..].find("</item>") else {
            break;
        };
        let block_end = tag_start + rel_end;
        let block = &xml[tag_start..block_end];
        let block_low = &lower[tag_start..block_end];
        pos = block_end + 7;

        let raw_link = extract_tag_inner(block, block_low, "link").unwrap_or_default();
        let link_clean = sanitize_text(&decode_xml_entities(&unwrap_cdata(&raw_link)));
        let Ok(parsed_url) = Url::parse(&link_clean) else {
            continue;
        };
        if !matches!(parsed_url.scheme(), "http" | "https") {
            continue;
        }

        let source_raw = extract_tag_inner(block, block_low, "source")
            .or_else(|| extract_tag_inner(block, block_low, "news:source"))
            .unwrap_or_default();
        let source = sanitize_text(&decode_xml_entities(&unwrap_cdata(&source_raw)));

        let title_raw = extract_tag_inner(block, block_low, "title").unwrap_or_default();
        let mut title = sanitize_text(&decode_xml_entities(&unwrap_cdata(&title_raw)));
        if title.is_empty() {
            continue;
        }
        if !source.is_empty() {
            let suffix = format!(" - {source}");
            if title.len() > suffix.len() && title.ends_with(&suffix) {
                title.truncate(title.len() - suffix.len());
                title = title.trim().to_string();
            }
        }
        if title.is_empty() {
            continue;
        }

        let dedup_key: String = title
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .take(64)
            .collect();
        if !dedup_key.is_empty() && !seen_titles.insert(dedup_key) {
            continue;
        }

        let pub_raw = extract_tag_inner(block, block_low, "pubdate").unwrap_or_default();
        let pub_clean = sanitize_text(&decode_xml_entities(&unwrap_cdata(&pub_raw)));
        let published_ts = parse_rss_pubdate(&pub_clean);

        items.push(NewsItem {
            title,
            url: parsed_url.to_string(),
            source,
            published_ts,
        });
    }

    items.sort_by(|a, b| match (b.published_ts, a.published_ts) {
        (Some(tb), Some(ta)) => tb.cmp(&ta),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    items.truncate(max_items);
    items
}

fn parse_rss_pubdate(raw: &str) -> Option<i64> {
    if raw.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(raw) {
        return Some(dt.timestamp());
    }
    // Fallback if a publisher miscomputes the RFC-2822 weekday prefix
    let without_wd = raw
        .split_once(',')
        .map(|(_, rest)| rest.trim())
        .unwrap_or(raw);
    let normalized = if let Some(prefix) = without_wd.strip_suffix(" GMT") {
        format!("{prefix} +0000")
    } else if let Some(prefix) = without_wd.strip_suffix(" UTC") {
        format!("{prefix} +0000")
    } else {
        without_wd.to_string()
    };
    chrono::DateTime::parse_from_str(&normalized, "%d %b %Y %H:%M:%S %z")
        .ok()
        .map(|dt| dt.timestamp())
}

fn extract_tag_inner(orig: &str, lower: &str, tag: &str) -> Option<String> {
    let open_pat = format!("<{tag}");
    let close_pat = format!("</{tag}>");
    let mut search_from = 0usize;
    while let Some(rel) = lower[search_from..].find(&open_pat) {
        let start = search_from + rel;
        let after_name = start + open_pat.len();
        let next = lower.as_bytes().get(after_name).copied()?;
        if !matches!(next, b'>' | b' ' | b'\t' | b'\r' | b'\n' | b'/') {
            search_from = after_name;
            continue;
        }
        let gt_rel = orig[start..].find('>')?;
        let gt_idx = start + gt_rel;
        if gt_idx > 0 && orig.as_bytes()[gt_idx - 1] == b'/' {
            return Some(String::new());
        }
        let inner_start = gt_idx + 1;
        let close_rel = lower[inner_start..].find(&close_pat)?;
        return Some(orig[inner_start..inner_start + close_rel].to_string());
    }
    None
}

fn unwrap_cdata(s: &str) -> String {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("<![CDATA[")
        && let Some(inner) = rest.strip_suffix("]]>")
    {
        return inner.to_string();
    }
    t.to_string()
}

fn decode_xml_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'&'
            && let Some(semi_rel) = s[i + 1..].find(';')
            && semi_rel <= 12
        {
            let entity = &s[i + 1..i + 1 + semi_rel];
            let decoded = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                    u32::from_str_radix(&entity[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if entity.starts_with('#') => entity[1..]
                    .parse::<u32>()
                    .ok()
                    .and_then(char::from_u32),
                _ => None,
            };
            if let Some(ch) = decoded {
                out.push(ch);
                i += semi_rel + 2;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn sanitize_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = true;
    for ch in s.chars() {
        if ch.is_control() || ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    if prev_space && out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Format relative headline age cleanly in EN / 中文 along with whether it's `< 1h` fresh.
pub fn format_relative_age(published_ts: Option<i64>, now_ts: i64, lang: Lang) -> (String, bool) {
    let Some(ts) = published_ts else {
        return (String::new(), false);
    };
    let age = (now_ts - ts).max(0);
    if age < 60 {
        (tr(lang, "just now", "刚刚").to_string(), true)
    } else if age < 3600 {
        let m = age / 60;
        let lbl = if lang == Lang::Zh {
            format!("{m}分钟前")
        } else {
            format!("{m}m ago")
        };
        (lbl, true)
    } else if age < 86_400 {
        let h = age / 3600;
        let lbl = if lang == Lang::Zh {
            format!("{h}小时前")
        } else {
            format!("{h}h ago")
        };
        (lbl, false)
    } else {
        let d = age / 86_400;
        let lbl = if lang == Lang::Zh {
            format!("{d}天前")
        } else {
            format!("{d}d ago")
        };
        (lbl, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_market(slug: &str, question: &str, event_title: &str) -> MarketSummary {
        MarketSummary {
            id: "1".into(),
            question: question.into(),
            question_zh: None,
            slug: slug.into(),
            event_slug: slug.into(),
            event_title: event_title.into(),
            condition_id: "cond1".into(),
            outcomes: vec!["Yes".into(), "No".into()],
            outcomes_zh: Vec::new(),
            token_ids: vec!["t0".into(), "t1".into()],
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
            category: "all".into(),
        }
    }

    #[test]
    fn build_market_news_query_handles_crypto_sports_macro_and_fallback() {
        let btc_5m = dummy_market(
            "btc-updown-5m-1795810800",
            "Bitcoin Up or Down - November 27, 3:45PM-3:50PM ET",
            "",
        );
        assert_eq!(build_market_news_query(&btc_5m), "Bitcoin BTC crypto");

        let fed = dummy_market(
            "fed-dec-2026",
            "Will the Fed decrease interest rates by 25 bps after the December 2026 meeting?",
            "",
        );
        let fed_q = build_market_news_query(&fed);
        assert!(fed_q.contains("Fed") && fed_q.contains("interest") && fed_q.contains("rates"), "got {fed_q}");
        assert!(!fed_q.starts_with("Will"));

        let spread = dummy_market(
            "broncos-steelers-spread",
            "Spread: Broncos (-5.5) vs. Steelers on 2026-11-27",
            "Broncos vs Steelers",
        );
        assert_eq!(build_market_news_query(&spread), "Broncos Steelers");

        let short_q = dummy_market("taipei-mayor", "Win?", "Taipei Mayor Election 2026");
        assert_eq!(build_market_news_query(&short_q), "Taipei Mayor Election 2026");
    }

    #[test]
    fn parse_rss_items_parses_google_and_bing_strips_suffix_dedups_and_rejects_bad_schemes() {
        let rss = r#"<?xml version="1.0" encoding="UTF-8"?>
        <rss version="2.0" xmlns:news="http://www.bing.com/schema/news">
          <channel>
            <item>
              <title><![CDATA[Bitcoin tops $60,000 as ETF flows surge - Reuters]]></title>
              <link>https://www.reuters.com/markets/bitcoin-60k</link>
              <pubDate>Fri, 27 Nov 2026 20:00:00 GMT</pubDate>
              <source url="https://www.reuters.com">Reuters</source>
            </item>
            <item>
              <title>Malicious Javascript Link - Evil</title>
              <link>javascript:alert(1)</link>
              <pubDate>Fri, 27 Nov 2026 21:00:00 GMT</pubDate>
              <source>Evil</source>
            </item>
            <item>
              <title>Local File Link - Bad</title>
              <link>file:///etc/passwd</link>
              <pubDate>Fri, 27 Nov 2026 21:05:00 GMT</pubDate>
            </item>
            <item>
              <title>Bitcoin tops $60,000 as ETF flows surge!</title>
              <link>https://mirror.example.com/dup</link>
              <pubDate>Fri, 27 Nov 2026 19:00:00 GMT</pubDate>
            </item>
            <item>
              <title>Fed signals patience on December rate cut &amp; outlook</title>
              <link>https://apnews.com/article/fed-december</link>
              <pubDate>Fri, 27 Nov 2026 20:30:00 GMT</pubDate>
              <news:Source>AP News</news:Source>
            </item>
          </channel>
        </rss>"#;

        let items = parse_rss_items(rss, 20);
        assert_eq!(items.len(), 2);
        // Sorted newest-first: AP News (20:30) before Reuters (20:00)
        assert!(items[0].published_ts.is_some());
        assert!(items[0].published_ts > items[1].published_ts);
        assert_eq!(items[0].source, "AP News");
        assert_eq!(items[0].title, "Fed signals patience on December rate cut & outlook");
        assert_eq!(items[1].source, "Reuters");
        assert_eq!(items[1].title, "Bitcoin tops $60,000 as ETF flows surge");
    }

    #[test]
    fn format_relative_age_formats_en_and_zh() {
        let now = 1_795_810_000_i64;
        assert_eq!(format_relative_age(Some(now - 20), now, Lang::En), ("just now".into(), true));
        assert_eq!(format_relative_age(Some(now - 20), now, Lang::Zh), ("刚刚".into(), true));
        assert_eq!(format_relative_age(Some(now - 900), now, Lang::En), ("15m ago".into(), true));
        assert_eq!(format_relative_age(Some(now - 900), now, Lang::Zh), ("15分钟前".into(), true));
        assert_eq!(format_relative_age(Some(now - 7200), now, Lang::En), ("2h ago".into(), false));
        assert_eq!(format_relative_age(Some(now - 172800), now, Lang::Zh), ("2天前".into(), false));
        assert_eq!(format_relative_age(None, now, Lang::En), (String::new(), false));
    }
}
