use std::collections::HashSet;

use dashmap::DashMap;
use reqwest::Client;
use serde_json::Value;

/// Fast, non-blocking bilingual (EN <-> 简体中文) translator & Polymarket search expander
/// shared across Linux, Windows 10/11, macOS, and iOS/iPadOS.
///
/// Only translates public Polymarket market titles/outcomes and search queries —
/// NEVER touches private keys, wallet addresses, or order payloads.
pub struct Translator {
    http: Client,
    en_to_zh: DashMap<String, String>,
    zh_to_en: DashMap<String, String>,
    /// Keys that were injected by a hot patch, so they can be unwound exactly.
    hot_en_keys: DashMap<String, ()>,
    hot_zh_keys: DashMap<String, ()>,
}

impl Translator {
    pub fn new(http: Client) -> Self {
        Self {
            http,
            en_to_zh: DashMap::new(),
            zh_to_en: DashMap::new(),
            hot_en_keys: DashMap::new(),
            hot_zh_keys: DashMap::new(),
        }
    }

    /// Apply verified hot-patch bilingual overrides immediately in memory (zero restart).
    ///
    /// SECURITY: outcome-bearing vocabulary (Yes/No/Up/Down/Over/Under/…) is owned by
    /// the compiled glossary and can NEVER be overridden. Otherwise a one-line manifest
    /// (`{"Up":"Down"}`) would relabel the two sides of a binary market and make the
    /// user buy the opposite token.
    pub fn apply_hot_overrides(
        &self,
        en_to_zh: &std::collections::HashMap<String, String>,
        zh_to_en: &std::collections::HashMap<String, String>,
    ) -> usize {
        let mut applied = 0usize;
        for (en, zh) in en_to_zh {
            let k = en.trim();
            let v = zh.trim();
            if k.is_empty() || v.is_empty() || Self::is_reserved_label(k) {
                continue;
            }
            self.en_to_zh.insert(k.to_string(), v.to_string());
            self.hot_en_keys.insert(k.to_string(), ());
            applied += 1;
        }
        for (zh, en) in zh_to_en {
            let k = zh.trim();
            let v = en.trim();
            if k.is_empty() || v.is_empty() || Self::is_reserved_label(k) {
                continue;
            }
            self.zh_to_en.insert(k.to_string(), v.to_string());
            self.hot_zh_keys.insert(k.to_string(), ());
            applied += 1;
        }
        applied
    }

    /// True for strings whose meaning is a trading outcome. Hot patches may not touch
    /// these; the compiled glossary is the single source of truth.
    fn is_reserved_label(s: &str) -> bool {
        if outcome_glossary_zh(s).is_some() {
            return true;
        }
        matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "buy" | "sell" | "long" | "short" | "bid" | "ask" | "win" | "lose" | "winning"
        )
    }

    /// Remove every override injected by a hot patch, restoring the pre-patch cache.
    pub fn clear_hot_overrides(&self) {
        let en_keys: Vec<String> = self.hot_en_keys.iter().map(|e| e.key().clone()).collect();
        for k in en_keys {
            self.en_to_zh.remove(&k);
        }
        self.hot_en_keys.clear();
        let zh_keys: Vec<String> = self.hot_zh_keys.iter().map(|e| e.key().clone()).collect();
        for k in zh_keys {
            self.zh_to_en.remove(&k);
        }
        self.hot_zh_keys.clear();
    }

    pub fn hot_override_count(&self) -> usize {
        self.hot_en_keys.len() + self.hot_zh_keys.len()
    }

    /// Outcome-name translation with the compiled glossary taking priority over every
    /// cache entry, including anything a hot patch injected. Used for YES/NO, Up/Down,
    /// Over/Under and other labels whose inversion would change the traded token.
    pub fn get_zh_outcome(&self, text: &str) -> Option<String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Some(exact) = outcome_glossary_zh(trimmed) {
            return Some(exact.to_string());
        }
        self.get_zh(trimmed)
    }

    /// Lock-free O(1) lookup for UI rendering (never blocks the frame loop).
    pub fn get_zh(&self, text: &str) -> Option<String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        if contains_cjk(trimmed) {
            return Some(trimmed.to_string());
        }
        if let Some(ov) = self.en_to_zh.get(trimmed) {
            return Some(ov.value().clone());
        }
        if let Some(exact) = outcome_glossary_zh(trimmed) {
            return Some(exact.to_string());
        }
        None
    }

    /// Translate a batch of English market titles / outcomes / sibling titles to Simplified Chinese.
    /// Deduplicates, skips already-cached strings, and batches ~14 lines per HTTPS call.
    /// Returns how many new translations were added to the cache.
    pub async fn translate_en_to_zh_batch(&self, texts: &[String]) -> usize {
        let mut pending = Vec::new();
        let mut seen = HashSet::new();

        for raw in texts {
            let t = raw.trim();
            if t.is_empty()
                || contains_cjk(t)
                || outcome_glossary_zh(t).is_some()
                || self.en_to_zh.contains_key(t)
                || !seen.insert(t.to_string())
            {
                continue;
            }
            // Skip pure numeric/punctuation labels like "37.5" or "$100"
            if !t.chars().any(|c| c.is_ascii_alphabetic()) {
                continue;
            }
            pending.push(t.to_string());
        }

        if pending.is_empty() {
            return 0;
        }

        let mut added = 0usize;
        for chunk in pending.chunks(14) {
            let clean_lines: Vec<String> = chunk
                .iter()
                .map(|s| s.replace(['\r', '\n'], " "))
                .collect();
            let joined = clean_lines.join("\n");

            if let Some(translated) = self.google_translate(&joined, "en", "zh-CN").await {
                let parts: Vec<&str> = translated.lines().collect();
                if parts.len() == chunk.len() {
                    for (orig, zh) in chunk.iter().zip(parts.iter()) {
                        let zh_clean = zh.trim();
                        if !zh_clean.is_empty() {
                            self.en_to_zh.insert(orig.clone(), zh_clean.to_string());
                            added += 1;
                        }
                    }
                    continue;
                }
            }

            // Fallback: translate items individually if batch line alignment differed
            for orig in chunk {
                if let Some(zh) = self.translate_single(orig, "en", "zh-CN").await {
                    self.en_to_zh.insert(orig.clone(), zh);
                    added += 1;
                }
            }
        }

        added
    }

    /// Expand a user search query (English, Chinese, or mixed) into English search terms
    /// so Polymarket Gamma API & local filtering find matching markets in BOTH languages.
    pub async fn expand_search_queries(&self, raw_query: &str) -> Vec<String> {
        let q = raw_query.trim();
        if q.is_empty() {
            return Vec::new();
        }

        let mut out = Vec::new();
        let mut push_unique = |s: String| {
            let cleaned = s.trim().to_string();
            if !cleaned.is_empty()
                && !out.iter().any(|x: &String| x.eq_ignore_ascii_case(&cleaned))
            {
                out.push(cleaned);
            }
        };

        push_unique(q.to_string());

        if contains_cjk(q) {
            // 1. Instant local Polymarket CN -> EN domain glossary
            let glossary_hits = expand_cn_glossary(q);
            for term in &glossary_hits {
                push_unique(term.clone());
            }

            // 2. Live ZH -> EN translation (cached) for any city, team, politician, event, etc.
            let translated = if let Some(cached) = self.zh_to_en.get(q) {
                Some(cached.value().clone())
            } else if let Some(en) = self.translate_single(q, "zh-CN", "en").await {
                self.zh_to_en.insert(q.to_string(), en.clone());
                Some(en)
            } else {
                None
            };

            if let Some(en) = translated {
                let norm = normalize_en_search(&en);
                push_unique(norm.clone());
                // Also add Polymarket shorthand aliases (e.g., "Federal Reserve" -> "Fed")
                for alias in english_polymarket_aliases(&norm) {
                    push_unique(alias);
                }
            }
        } else {
            // Also normalize common English shorthand (e.g., "btc" -> "bitcoin", "eth" -> "ethereum")
            for alias in english_polymarket_aliases(q) {
                push_unique(alias);
            }
        }

        out
    }

    async fn translate_single(&self, text: &str, sl: &str, tl: &str) -> Option<String> {
        if let Some(t) = self.google_translate(text, sl, tl).await {
            let clean = t.trim().to_string();
            if !clean.is_empty() {
                return Some(clean);
            }
        }
        self.mymemory_translate(text, sl, tl).await
    }

    async fn google_translate(&self, text: &str, sl: &str, tl: &str) -> Option<String> {
        let url = format!(
            "https://translate.googleapis.com/translate_a/single?client=gtx&sl={}&tl={}&dt=t&q={}",
            sl,
            tl,
            pct_encode(text)
        );
        let resp = self
            .http
            .get(&url)
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let val: Value = resp.json().await.ok()?;
        let arr = val.get(0)?.as_array()?;
        let mut combined = String::new();
        for seg in arr {
            if let Some(part) = seg.get(0).and_then(|s| s.as_str()) {
                combined.push_str(part);
            }
        }
        let trimmed = combined.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    async fn mymemory_translate(&self, text: &str, sl: &str, tl: &str) -> Option<String> {
        let url = format!(
            "https://api.mymemory.translated.net/get?q={}&langpair={}|{}",
            pct_encode(text),
            sl,
            tl
        );
        let resp = self.http.get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let val: Value = resp.json().await.ok()?;
        let t = val
            .get("responseData")
            .and_then(|d| d.get("translatedText"))
            .and_then(|s| s.as_str())?
            .trim()
            .to_string();
        (!t.is_empty()).then_some(t)
    }
}

pub fn contains_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(
            c as u32,
            0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
        )
    })
}

fn outcome_glossary_zh(s: &str) -> Option<&'static str> {
    match s.to_ascii_lowercase().as_str() {
        "yes" => Some("是"),
        "no" => Some("否"),
        "up" => Some("涨"),
        "down" => Some("跌"),
        "over" => Some("大于"),
        "under" => Some("小于"),
        "draw" | "tie" => Some("平局"),
        "higher" => Some("更高"),
        "lower" => Some("更低"),
        _ => None,
    }
}

fn expand_cn_glossary(q: &str) -> Vec<String> {
    const TABLE: &[(&str, &[&str])] = &[
        ("比特币", &["bitcoin", "btc"]),
        ("大饼", &["bitcoin"]),
        ("以太坊", &["ethereum", "eth"]),
        ("以太", &["ethereum"]),
        ("索拉纳", &["solana"]),
        ("瑞波", &["xrp"]),
        ("狗狗币", &["dogecoin"]),
        ("加密", &["crypto"]),
        ("美联储", &["fed", "federal reserve"]),
        ("联储", &["fed"]),
        ("降息", &["decrease interest rates", "rate cut", "interest rates"]),
        ("加息", &["increase interest rates", "rate hike", "interest rates"]),
        ("利率", &["interest rates", "fed"]),
        ("通胀", &["inflation", "cpi"]),
        ("衰退", &["recession"]),
        ("关税", &["tariff"]),
        ("特朗普", &["trump"]),
        ("川普", &["trump"]),
        ("拜登", &["biden"]),
        ("哈里斯", &["harris"]),
        ("万斯", &["vance"]),
        ("马斯克", &["musk", "elon"]),
        ("总统", &["president"]),
        ("大选", &["election"]),
        ("国会", &["congress", "senate"]),
        ("中国", &["china"]),
        ("美国", &["united states", "us"]),
        ("台湾", &["taiwan"]),
        ("乌克兰", &["ukraine"]),
        ("俄乌", &["ukraine", "russia"]),
        ("俄罗斯", &["russia"]),
        ("伊朗", &["iran"]),
        ("以色列", &["israel"]),
        ("停火", &["ceasefire"]),
        ("战争", &["war"]),
        ("英伟达", &["nvidia"]),
        ("特斯拉", &["tesla"]),
        ("苹果", &["apple"]),
        ("微软", &["microsoft"]),
        ("谷歌", &["google"]),
        ("亚马逊", &["amazon"]),
        ("黄金", &["gold"]),
        ("原油", &["oil"]),
        ("石油", &["oil"]),
        ("美股", &["s&p", "nasdaq"]),
        ("标普", &["s&p 500"]),
        ("纳斯达克", &["nasdaq"]),
        ("湖人", &["lakers"]),
        ("凯尔特人", &["celtics"]),
        ("勇士", &["warriors"]),
        ("尼克斯", &["knicks"]),
        ("掘金", &["nuggets"]),
        ("雷霆", &["thunder"]),
        ("热火", &["heat"]),
        ("雄鹿", &["bucks"]),
        ("独行侠", &["mavericks"]),
        ("太阳", &["suns"]),
        ("快船", &["clippers"]),
        ("火箭", &["rockets"]),
        ("马刺", &["spurs"]),
        ("欧冠", &["champions league"]),
        ("英超", &["premier league"]),
        ("西甲", &["la liga"]),
        ("世界杯", &["world cup"]),
        ("足球", &["soccer", "fc"]),
        ("篮球", &["nba", "basketball"]),
        ("网球", &["tennis"]),
        ("拳击", &["boxing"]),
        ("电竞", &["esports"]),
        ("冠军", &["win", "champion"]),
    ];

    let mut matched = Vec::new();
    let mut parts = Vec::new();
    for &(cn, ens) in TABLE {
        if q.contains(cn) {
            if let Some(&primary) = ens.first() {
                parts.push(primary);
            }
            for &e in ens {
                if !matched.iter().any(|x: &String| x == e) {
                    matched.push(e.to_string());
                }
            }
        }
    }
    if parts.len() >= 2 {
        matched.insert(0, parts.join(" "));
    }
    matched
}

fn english_polymarket_aliases(q: &str) -> Vec<String> {
    let low = q.to_lowercase();
    let mut out = Vec::new();
    if low.contains("federal reserve") {
        out.push(low.replace("federal reserve", "fed"));
        out.push("fed".into());
    }
    if low == "btc" {
        out.push("bitcoin".into());
    } else if low == "eth" {
        out.push("ethereum".into());
    } else if low == "sol" {
        out.push("solana".into());
    }
    out
}

fn normalize_en_search(s: &str) -> String {
    s.trim()
        .trim_matches(|c: char| matches!(c, '?' | '!' | '.' | ',' | '"' | '\''))
        .to_string()
}

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            _ => {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_detection_and_glossary_work() {
        assert!(contains_cjk("比特币"));
        assert!(contains_cjk("Fed 降息"));
        assert!(!contains_cjk("Will Bitcoin hit $100k?"));

        let tr = Translator::new(Client::new());
        assert_eq!(tr.get_zh("Yes").as_deref(), Some("是"));
        assert_eq!(tr.get_zh("No").as_deref(), Some("否"));
        assert_eq!(tr.get_zh("Up").as_deref(), Some("涨"));

        let hits = expand_cn_glossary("美联储 降息");
        assert!(hits.iter().any(|h| h.contains("fed")));
        assert!(hits.iter().any(|h| h.contains("interest rates")));
    }

    // ---- §4.12: a hot patch must never be able to invert an outcome label ---------

    #[test]
    fn hot_overrides_cannot_relabel_trading_outcomes() {
        let tr = Translator::new(Client::new());
        let applied = tr.apply_hot_overrides(
            &std::collections::HashMap::from([
                ("Up".to_string(), "Down".to_string()),
                ("Yes".to_string(), "No".to_string()),
                ("No".to_string(), "Yes".to_string()),
                ("Buy".to_string(), "Sell".to_string()),
                ("Long".to_string(), "Short".to_string()),
            ]),
            &std::collections::HashMap::new(),
        );
        assert_eq!(applied, 0, "reserved outcome labels must be ignored");
        assert_eq!(tr.get_zh("Up").as_deref(), Some("涨"));
        assert_eq!(tr.get_zh("Yes").as_deref(), Some("是"));
        assert_eq!(tr.get_zh("No").as_deref(), Some("否"));
        assert_eq!(tr.get_zh_outcome("Up").as_deref(), Some("涨"));

        // A legitimate (non-outcome) override still applies and is reversible.
        let applied = tr.apply_hot_overrides(
            &std::collections::HashMap::from([(
                "Will BTC close above 100k?".to_string(),
                "BTC 会收于 10 万以上吗？".to_string(),
            )]),
            &std::collections::HashMap::new(),
        );
        assert_eq!(applied, 1);
        assert_eq!(
            tr.get_zh("Will BTC close above 100k?").as_deref(),
            Some("BTC 会收于 10 万以上吗？")
        );
        assert_eq!(tr.hot_override_count(), 1);

        // Clearing restores the ambient cache exactly.
        tr.clear_hot_overrides();
        assert_eq!(tr.hot_override_count(), 0);
        assert!(tr.get_zh("Will BTC close above 100k?").is_none());
        assert_eq!(tr.get_zh("Up").as_deref(), Some("涨"));
    }

    #[test]
    fn outcome_lookup_beats_any_cached_translation() {
        let tr = Translator::new(Client::new());
        // Even if something wrote a wrong entry straight into the cache, the
        // outcome-safe lookup must win.
        tr.en_to_zh.insert("Down".into(), "涨".into());
        assert_eq!(tr.get_zh("Down").as_deref(), Some("涨"));
        assert_eq!(tr.get_zh_outcome("Down").as_deref(), Some("跌"));
    }
}
