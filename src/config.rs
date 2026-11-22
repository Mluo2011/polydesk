use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUILD_TARGET: &str = env!("POLYDESK_TARGET");
pub const DEFAULT_GITHUB_REPO: &str = env!("POLYDESK_DEFAULT_GITHUB_REPO");

/// Hardcoded official Polymarket CLOB V2 Builder Code for Dronepilot.
/// Non-editable at runtime; live Maker/Taker fee rates are fetched directly from
/// Polymarket's CLOB endpoint (`https://clob.polymarket.com/fees/builder-fees/{code}`).
pub const HARDCODED_BUILDER_CODE: &str =
    "0x8b258cc9a239e2ebf724bd4097bcf3dbecd2d53ca28a7a74959fe48087135e5d";
pub const HARDCODED_BUILDER_LABEL: &str = "Dronepilot";

fn default_clob_host() -> String {
    "https://clob.polymarket.com".to_string()
}

fn default_ws_market_url() -> String {
    "wss://ws-subscriptions-clob.polymarket.com/ws/market".to_string()
}

fn default_gamma_host() -> String {
    "https://gamma-api.polymarket.com".to_string()
}

fn default_data_host() -> String {
    "https://data-api.polymarket.com".to_string()
}

fn default_language() -> String {
    "en".to_string()
}

fn default_odds_format() -> String {
    "cents".to_string()
}

fn default_stake_presets() -> Vec<f64> {
    vec![5.0, 10.0, 25.0, 50.0, 100.0]
}

fn default_order_type() -> String {
    "GTC".to_string()
}

fn default_whale_threshold() -> f64 {
    500.0
}

fn default_paper_balance() -> f64 {
    1000.0
}

fn default_true() -> bool {
    true
}

fn default_builder_taker_bps() -> u32 {
    0
}

fn default_builder_maker_bps() -> u32 {
    0
}

pub fn default_hotkeys() -> HashMap<String, String> {
    HashMap::from([
        ("join_bid".into(), "KeyB".into()),
        ("join_ask".into(), "KeyS".into()),
        ("cross_buy".into(), "Shift+KeyB".into()),
        ("cross_sell".into(), "Shift+KeyS".into()),
        ("cancel_market".into(), "KeyX".into()),
        ("panic_cancel_all".into(), "Shift+KeyX".into()),
        ("green_up".into(), "KeyG".into()),
        ("flatten".into(), "KeyF".into()),
        ("recenter".into(), "Space".into()),
        ("cycle_odds".into(), "KeyO".into()),
        ("toggle_lang".into(), "KeyL".into()),
        ("stake_1".into(), "Digit1".into()),
        ("stake_2".into(), "Digit2".into()),
        ("stake_3".into(), "Digit3".into()),
        ("stake_4".into(), "Digit4".into()),
        ("stake_5".into(), "Digit5".into()),
    ])
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_clob_host")]
    pub clob_host: String,
    #[serde(default = "default_ws_market_url")]
    pub ws_market_url: String,
    #[serde(default = "default_gamma_host")]
    pub gamma_host: String,
    #[serde(default = "default_data_host")]
    pub data_host: String,

    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_odds_format")]
    pub odds_format: String,
    #[serde(default = "default_stake_presets")]
    pub stake_presets: Vec<f64>,
    #[serde(default = "default_order_type")]
    pub default_order_type: String,
    #[serde(default = "default_whale_threshold")]
    pub whale_threshold_usd: f64,
    #[serde(default = "default_paper_balance")]
    pub paper_starting_balance: f64,

    #[serde(default)]
    pub github_repo: String,
    #[serde(default = "default_true")]
    pub auto_check_updates: bool,

    /// Polymarket CLOB V2 Builder Code (32-byte 0x-prefixed hex string).
    /// Configured via https://polymarket.com/settings?tab=builder so matched orders
    /// settle builder fees natively on-chain (capped by Polymarket at 100 bps taker / 50 bps maker).
    #[serde(default)]
    pub builder_code: String,
    #[serde(default = "default_builder_taker_bps")]
    pub builder_taker_bps: u32,
    #[serde(default = "default_builder_maker_bps")]
    pub builder_maker_bps: u32,
    #[serde(default)]
    pub builder_label: String,

    #[serde(default)]
    pub enable_heartbeats: bool,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub watchlist: Vec<String>,
    #[serde(default = "default_hotkeys")]
    pub hotkeys: HashMap<String, String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            clob_host: default_clob_host(),
            ws_market_url: default_ws_market_url(),
            gamma_host: default_gamma_host(),
            data_host: default_data_host(),
            language: default_language(),
            odds_format: default_odds_format(),
            stake_presets: default_stake_presets(),
            default_order_type: default_order_type(),
            whale_threshold_usd: default_whale_threshold(),
            paper_starting_balance: default_paper_balance(),
            github_repo: DEFAULT_GITHUB_REPO.to_string(),
            auto_check_updates: true,
            builder_code: HARDCODED_BUILDER_CODE.to_string(),
            builder_taker_bps: default_builder_taker_bps(),
            builder_maker_bps: default_builder_maker_bps(),
            builder_label: HARDCODED_BUILDER_LABEL.to_string(),
            enable_heartbeats: false,
            access_token: None,
            watchlist: Vec::new(),
            hotkeys: default_hotkeys(),
        }
    }
}

impl AppConfig {
    pub fn config_dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("polydesk")
    }

    pub fn config_path(custom: Option<&PathBuf>) -> PathBuf {
        custom
            .cloned()
            .unwrap_or_else(|| Self::config_dir().join("config.toml"))
    }

    pub fn load(custom: Option<&PathBuf>) -> Result<Self> {
        let path = Self::config_path(custom);
        if !path.exists() {
            let mut cfg = Self::default();
            cfg.apply_env_overrides();
            return Ok(cfg);
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed reading config at {}", path.display()))?;
        let mut cfg: Self = toml::from_str(&raw)
            .with_context(|| format!("failed parsing config TOML at {}", path.display()))?;
        cfg.sanitize();
        cfg.apply_env_overrides();
        Ok(cfg)
    }

    pub fn save(&self, custom: Option<&PathBuf>) -> Result<()> {
        let path = Self::config_path(custom);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut copy = self.clone();
        copy.sanitize();
        let text = toml::to_string_pretty(&copy)?;
        fs::write(&path, text)
            .with_context(|| format!("failed writing config at {}", path.display()))?;
        Ok(())
    }

    pub fn sanitize(&mut self) {
        // Hardcode Builder Code & label — cannot be altered via config.toml or UI
        self.builder_code = HARDCODED_BUILDER_CODE.to_string();
        self.builder_label = HARDCODED_BUILDER_LABEL.to_string();
        self.builder_taker_bps = self.builder_taker_bps.min(100);
        self.builder_maker_bps = self.builder_maker_bps.min(50);
        if self.language != "zh" {
            self.language = "en".to_string();
        }
        if self.github_repo.is_empty() && !DEFAULT_GITHUB_REPO.is_empty() {
            self.github_repo = DEFAULT_GITHUB_REPO.to_string();
        }
        if self.stake_presets.is_empty() {
            self.stake_presets = default_stake_presets();
        }
        if self.hotkeys.is_empty() {
            self.hotkeys = default_hotkeys();
        }
    }

    pub fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("POLYDESK_CLOB_HOST")
            && !v.trim().is_empty()
        {
            self.clob_host = v.trim().to_string();
        }
        if let Ok(v) = std::env::var("POLYDESK_GITHUB_REPO")
            && !v.trim().is_empty()
        {
            self.github_repo = v.trim().to_string();
        }
        if let Ok(v) = std::env::var("POLYDESK_TOKEN")
            && !v.trim().is_empty()
        {
            self.access_token = Some(v.trim().to_string());
        }
        if let Ok(v) = std::env::var("POLYDESK_LANG")
            && v.trim().eq_ignore_ascii_case("zh")
        {
            self.language = "zh".to_string();
        }
        if std::env::args().any(|a| a == "--zh" || a == "--cn") {
            self.language = "zh".to_string();
        }
        // Always enforce hardcoded builder code even after env overrides
        self.builder_code = HARDCODED_BUILDER_CODE.to_string();
        self.builder_label = HARDCODED_BUILDER_LABEL.to_string();
    }

    pub fn is_builder_locked(&self) -> bool {
        true
    }
}
