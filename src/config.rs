use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use url::{Host, Url};

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

fn default_theme() -> String {
    "dark".to_string()
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

pub fn default_polygon_rpc_url() -> String {
    "https://polygon-bor-rpc.publicnode.com".to_string()
}

fn is_disallowed_ipv4(ip: Ipv4Addr) -> bool {
    let oct = ip.octets();
    let is_cgnat = oct[0] == 100 && (oct[1] & 0xc0) == 64;
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || is_cgnat
}

fn is_disallowed_ipv6(ip: Ipv6Addr) -> bool {
    let seg0 = ip.segments()[0];
    let is_ula = (seg0 & 0xfe00) == 0xfc00;
    let is_link_local = (seg0 & 0xffc0) == 0xfe80;
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || is_ula
        || is_link_local
        || ip.to_ipv4_mapped().is_some_and(is_disallowed_ipv4)
}

pub fn is_disallowed_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_disallowed_ipv4(v4),
        IpAddr::V6(v6) => is_disallowed_ipv6(v6),
    }
}

pub fn verify_resolved_rpc_ips(host: &str, ips: &[IpAddr]) -> Result<()> {
    if ips.is_empty() {
        return Err(anyhow!("Polygon RPC host '{host}' did not resolve to any IP"));
    }
    for &ip in ips {
        if is_disallowed_ip(ip) {
            return Err(anyhow!(
                "Polygon RPC host '{host}' resolved to private/local IP '{ip}', which is not permitted"
            ));
        }
    }
    Ok(())
}

/// Resolves a validated HTTPS Polygon RPC URL and rejects any DNS record pointing to
/// loopback, RFC1918 private, link-local, ULA, or CGNAT IP addresses.
pub async fn verify_polygon_rpc_dns(clean_rpc: &str) -> Result<()> {
    let parsed = Url::parse(clean_rpc).context("Invalid Polygon RPC URL")?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("Polygon RPC URL missing host"))?;
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs: Vec<IpAddr> = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("Failed resolving Polygon RPC host '{}'", redact_rpc_url(clean_rpc)))?
        .map(|sa| sa.ip())
        .collect();
    verify_resolved_rpc_ips(host, &addrs)
}

pub fn validate_polygon_rpc_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let parsed = Url::parse(trimmed).context("Invalid Polygon RPC URL")?;
    if parsed.scheme() != "https" {
        return Err(anyhow!("Polygon RPC URL must use https://"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(anyhow!("Polygon RPC URL must not embed credentials"));
    }
    match parsed.host() {
        Some(Host::Domain(dom)) => {
            let lower = dom.trim_end_matches('.').to_ascii_lowercase();
            if lower.is_empty()
                || lower == "localhost"
                || lower.ends_with(".localhost")
                || lower.ends_with(".local")
                || lower.ends_with(".internal")
            {
                return Err(anyhow!("Polygon RPC host '{lower}' is not permitted"));
            }
            if let Ok(ip) = lower.parse::<IpAddr>() {
                let bad = match ip {
                    IpAddr::V4(v4) => is_disallowed_ipv4(v4),
                    IpAddr::V6(v6) => is_disallowed_ipv6(v6),
                };
                if bad {
                    return Err(anyhow!("Private/local Polygon RPC IP '{ip}' is not permitted"));
                }
            }
        }
        Some(Host::Ipv4(ip)) => {
            if is_disallowed_ipv4(ip) {
                return Err(anyhow!("Private/local Polygon RPC IP '{ip}' is not permitted"));
            }
        }
        Some(Host::Ipv6(ip)) => {
            if is_disallowed_ipv6(ip) {
                return Err(anyhow!("Private/local Polygon RPC IP '{ip}' is not permitted"));
            }
        }
        None => return Err(anyhow!("Polygon RPC URL must specify a valid host")),
    }
    Ok(trimmed.to_string())
}

pub fn redact_rpc_url(raw: &str) -> String {
    match Url::parse(raw.trim()) {
        Ok(u) => match u.host_str() {
            Some(host) if !host.is_empty() => match u.port() {
                Some(port) => format!("https://{host}:{port}"),
                None => format!("https://{host}"),
            },
            _ => "https://<invalid-rpc>".to_string(),
        },
        Err(_) => "https://<invalid-rpc>".to_string(),
    }
}

pub fn default_vault_profile() -> String {
    "default".to_string()
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
    #[serde(default = "default_theme")]
    pub theme: String,
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
    #[serde(default, skip_serializing)]
    pub access_token: Option<String>,
    #[serde(default = "default_polygon_rpc_url")]
    pub polygon_rpc_url: String,
    #[serde(default = "default_vault_profile")]
    pub active_vault_profile: String,
    #[serde(default)]
    pub watchlist: Vec<String>,
    #[serde(default)]
    pub price_alerts: Vec<crate::alerts::PriceAlert>,
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
            theme: default_theme(),
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
            polygon_rpc_url: default_polygon_rpc_url(),
            active_vault_profile: default_vault_profile(),
            watchlist: Vec::new(),
            price_alerts: Vec::new(),
            hotkeys: default_hotkeys(),
        }
    }
}

pub(crate) fn write_private_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let mut tmp_os = path.as_os_str().to_os_string();
    tmp_os.push(".tmp");
    let tmp_path = PathBuf::from(tmp_os);

    let write_res: Result<()> = (|| {
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp_path)
                .with_context(|| format!("failed opening temp file at {}", tmp_path.display()))?;
            f.write_all(bytes)
                .with_context(|| format!("failed writing temp file at {}", tmp_path.display()))?;
            f.sync_all()
                .with_context(|| format!("failed syncing temp file at {}", tmp_path.display()))?;
            drop(f);
            let _ = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600));
        }
        #[cfg(not(unix))]
        {
            use std::io::Write;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)
                .with_context(|| format!("failed opening temp file at {}", tmp_path.display()))?;
            f.write_all(bytes)
                .with_context(|| format!("failed writing temp file at {}", tmp_path.display()))?;
            f.sync_all()
                .with_context(|| format!("failed syncing temp file at {}", tmp_path.display()))?;
        }
        Ok(())
    })();

    if let Err(e) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    if let Err(e) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e).with_context(|| format!("failed renaming temp file to {}", path.display()));
    }

    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && let Ok(dir) = fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }

    Ok(())
}

fn env_non_empty(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        std::env::var(k)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

impl AppConfig {
    /// Returns `~/.config/polydirect/` (or OS config dir), automatically migrating
    /// `config.toml` and `vault.json` from legacy `polydesk/` when `polydirect/` does not yet exist.
    pub fn config_dir() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        Self::config_dir_in(&base)
    }

    pub(crate) fn migrate_legacy_config_dir(base: &std::path::Path) -> Result<bool> {
        let new_dir = base.join("polydirect");
        let legacy_dir = base.join("polydesk");
        if new_dir.exists() || !legacy_dir.exists() {
            return Ok(false);
        }

        let staging_dir = base.join("polydirect.migrating");
        if staging_dir.exists() {
            fs::remove_dir_all(&staging_dir).with_context(|| {
                format!(
                    "failed clearing stale migration directory at {}",
                    staging_dir.display()
                )
            })?;
        }

        let stage_res: Result<()> = (|| {
            fs::create_dir_all(&staging_dir).with_context(|| {
                format!("failed creating staging directory at {}", staging_dir.display())
            })?;
            for filename in ["config.toml", "vault.json"] {
                let src = legacy_dir.join(filename);
                if src.exists() {
                    let dst = staging_dir.join(filename);
                    let bytes = fs::read(&src)
                        .with_context(|| format!("failed reading legacy file at {}", src.display()))?;
                    write_private_atomic(&dst, &bytes)?;
                }
            }
            let legacy_vaults = legacy_dir.join("vaults");
            if legacy_vaults.is_dir() {
                let new_vaults = staging_dir.join("vaults");
                fs::create_dir_all(&new_vaults).with_context(|| {
                    format!("failed creating staging vaults dir at {}", new_vaults.display())
                })?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(&new_vaults, fs::Permissions::from_mode(0o700));
                }
                let entries = fs::read_dir(&legacy_vaults).with_context(|| {
                    format!("failed reading legacy vaults dir at {}", legacy_vaults.display())
                })?;
                for entry in entries {
                    let entry = entry.with_context(|| {
                        format!(
                            "failed iterating legacy vaults dir at {}",
                            legacy_vaults.display()
                        )
                    })?;
                    let src = entry.path();
                    if !src.is_file() {
                        continue;
                    }
                    if src.extension().and_then(|s| s.to_str()) != Some("json") {
                        continue;
                    }
                    let Some(stem) = src.file_stem().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    let Ok(safe_stem) = crate::vault::validate_profile_name(stem) else {
                        continue;
                    };
                    let dst = new_vaults.join(format!("{safe_stem}.json"));
                    if dst.exists() {
                        continue;
                    }
                    let bytes = fs::read(&src).with_context(|| {
                        format!("failed reading legacy profile at {}", src.display())
                    })?;
                    write_private_atomic(&dst, &bytes)?;
                }
            }
            fs::rename(&staging_dir, &new_dir).with_context(|| {
                format!(
                    "failed promoting migrated config dir from {} to {}",
                    staging_dir.display(),
                    new_dir.display()
                )
            })?;
            #[cfg(unix)]
            if let Ok(dir) = fs::File::open(base) {
                let _ = dir.sync_all();
            }
            Ok(())
        })();

        if let Err(e) = stage_res {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(e);
        }
        Ok(true)
    }

    pub fn config_dir_in(base: &std::path::Path) -> PathBuf {
        let new_dir = base.join("polydirect");
        let legacy_dir = base.join("polydesk");
        if let Err(e) = Self::migrate_legacy_config_dir(base) {
            tracing::warn!("Legacy config directory migration failed (keeping {}): {e}", legacy_dir.display());
            return legacy_dir;
        }
        new_dir
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
        write_private_atomic(&path, text.as_bytes())
            .with_context(|| format!("failed saving config at {}", path.display()))
    }

    pub fn sanitize(&mut self) {
        // Hardcode Builder Code & label — cannot be altered via config.toml or UI
        self.builder_code = HARDCODED_BUILDER_CODE.to_string();
        self.builder_label = HARDCODED_BUILDER_LABEL.to_string();
        self.builder_taker_bps = self.builder_taker_bps.min(100);
        self.builder_maker_bps = self.builder_maker_bps.min(50);
        // Enforce encrypted TLS/WSS endpoints (prevent accidental downgrade to plaintext HTTP/WS)
        if !self.clob_host.starts_with("https://") {
            self.clob_host = default_clob_host();
        }
        if !self.gamma_host.starts_with("https://") {
            self.gamma_host = default_gamma_host();
        }
        if !self.data_host.starts_with("https://") {
            self.data_host = default_data_host();
        }
        if !self.ws_market_url.starts_with("wss://") {
            self.ws_market_url = default_ws_market_url();
        }
        match validate_polygon_rpc_url(&self.polygon_rpc_url) {
            Ok(valid) => self.polygon_rpc_url = valid,
            Err(_) => self.polygon_rpc_url = default_polygon_rpc_url(),
        }
        match crate::vault::validate_profile_name(&self.active_vault_profile) {
            Ok(canon) => self.active_vault_profile = canon,
            Err(_) => self.active_vault_profile = default_vault_profile(),
        }
        if self.language != "zh" {
            self.language = "en".to_string();
        }
        if self.theme != "light" {
            self.theme = "dark".to_string();
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
        if let Some(v) = env_non_empty(&["POLYDIRECT_CLOB_HOST", "POLYDESK_CLOB_HOST"]) {
            self.clob_host = v;
        }
        if let Some(v) = env_non_empty(&["POLYDIRECT_GITHUB_REPO", "POLYDESK_GITHUB_REPO"]) {
            self.github_repo = v;
        }
        if let Some(v) = env_non_empty(&["POLYDIRECT_TOKEN", "POLYDESK_TOKEN"]) {
            self.access_token = Some(v);
        }
        if let Some(v) =
            env_non_empty(&["POLYDIRECT_POLYGON_RPC_URL", "POLYDESK_POLYGON_RPC_URL"])
            && let Ok(valid) = validate_polygon_rpc_url(&v)
        {
            self.polygon_rpc_url = valid;
        }
        if let Some(v) = env_non_empty(&["POLYDIRECT_LANG", "POLYDESK_LANG"])
            && v.eq_ignore_ascii_case("zh")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_omits_access_token_and_preserves_builder_and_permissions() {
        let dir = std::env::temp_dir().join(format!("polydesk-cfg-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        let mut cfg = AppConfig::default();
        cfg.access_token = Some("super-secret-token-value".into());
        cfg.builder_code = "0x0000".into();
        cfg.builder_label = "Tampered".into();
        cfg.save(Some(&path)).unwrap();

        let saved = fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("super-secret-token-value"));
        assert!(!saved.contains("access_token"));
        assert!(!dir.join("config.toml.tmp").exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        let loaded = AppConfig::load(Some(&path)).unwrap();
        assert_eq!(loaded.builder_code, HARDCODED_BUILDER_CODE);
        assert_eq!(loaded.builder_label, HARDCODED_BUILDER_LABEL);
        assert!(loaded.access_token.is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_dir_in_migrates_legacy_polydesk_without_resurrecting_after_upgrade() {
        let base = std::env::temp_dir().join(format!("polydirect-mig-test-{}", uuid::Uuid::new_v4()));
        let legacy = base.join("polydesk");
        fs::create_dir_all(&legacy).unwrap();

        let mut old_cfg = AppConfig::default();
        old_cfg.language = "zh".into();
        old_cfg.watchlist = vec!["cond-123".into()];
        old_cfg.save(Some(&legacy.join("config.toml"))).unwrap();
        fs::write(legacy.join("vault.json"), "{\"version\":1,\"hint\":\"legacy\"}").unwrap();

        let resolved = AppConfig::config_dir_in(&base);
        assert_eq!(resolved, base.join("polydirect"));
        assert!(resolved.join("config.toml").exists());
        assert!(resolved.join("vault.json").exists());
        // Legacy directory preserved as non-destructive backup
        assert!(legacy.join("vault.json").exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let cfg_mode = fs::metadata(resolved.join("config.toml")).unwrap().permissions().mode() & 0o777;
            let vault_mode = fs::metadata(resolved.join("vault.json")).unwrap().permissions().mode() & 0o777;
            assert_eq!(cfg_mode, 0o600);
            assert_eq!(vault_mode, 0o600);
        }

        let loaded = AppConfig::load(Some(&resolved.join("config.toml"))).unwrap();
        assert_eq!(loaded.language, "zh");
        assert_eq!(loaded.watchlist, vec!["cond-123".to_string()]);
        assert_eq!(loaded.builder_code, HARDCODED_BUILDER_CODE);
        assert_eq!(loaded.builder_label, HARDCODED_BUILDER_LABEL);

        // Removing vault.json inside polydirect/ must NOT re-copy from legacy polydesk/
        fs::remove_file(resolved.join("vault.json")).unwrap();
        let resolved_again = AppConfig::config_dir_in(&base);
        assert_eq!(resolved_again, resolved);
        assert!(!resolved_again.join("vault.json").exists());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn polygon_rpc_url_validation_and_redaction() {
        assert_eq!(
            validate_polygon_rpc_url("https://polygon-bor-rpc.publicnode.com").unwrap(),
            "https://polygon-bor-rpc.publicnode.com"
        );
        assert_eq!(
            validate_polygon_rpc_url("https://polygon-mainnet.g.alchemy.com/v2/secret-key?foo=1")
                .unwrap(),
            "https://polygon-mainnet.g.alchemy.com/v2/secret-key?foo=1"
        );

        for bad in [
            "",
            "http://polygon-bor-rpc.publicnode.com",
            "https://localhost/rpc",
            "https://sub.localhost/rpc",
            "https://node.local/rpc",
            "https://rpc.internal/v1",
            "https://127.0.0.1/rpc",
            "https://127.0.1.1:8545",
            "https://10.0.0.1/rpc",
            "https://172.16.0.5/rpc",
            "https://192.168.1.1/rpc",
            "https://169.254.1.1/rpc",
            "https://[::1]/rpc",
            "https://[fe80::1]/rpc",
            "https://[fc00::1]/rpc",
            "https://user:pass@polygon-mainnet.g.alchemy.com/v2/abc",
        ] {
            assert!(
                validate_polygon_rpc_url(bad).is_err(),
                "expected rejection for RPC URL {bad:?}"
            );
        }

        assert_eq!(
            redact_rpc_url("https://user:pw@polygon-mainnet.g.alchemy.com/v2/my-secret-key?x=1"),
            "https://polygon-mainnet.g.alchemy.com"
        );
        assert_eq!(
            redact_rpc_url("https://rpc.example.com:8443/secret-path"),
            "https://rpc.example.com:8443"
        );
        assert_eq!(redact_rpc_url("not-a-url"), "https://<invalid-rpc>");

        // DNS-resolved IP SSRF guard rejects private/loopback answers (e.g. 127.0.0.1.nip.io)
        assert!(verify_resolved_rpc_ips("127.0.0.1.nip.io", &[IpAddr::V4(Ipv4Addr::LOCALHOST)]).is_err());
        assert!(verify_resolved_rpc_ips("internal.example", &[IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))]).is_err());
        assert!(verify_resolved_rpc_ips("empty.example", &[]).is_err());
        assert!(verify_resolved_rpc_ips("polygon-bor-rpc.publicnode.com", &[IpAddr::V4(Ipv4Addr::new(104, 18, 10, 1))]).is_ok());
    }

    #[test]
    fn staged_config_migration_cleans_up_on_error_and_retries_cleanly() {
        let base = std::env::temp_dir().join(format!(
            "polydirect-staged-cfg-test-{}",
            uuid::Uuid::new_v4()
        ));
        let legacy = base.join("polydesk");
        fs::create_dir_all(&legacy).unwrap();
        let mut old_cfg = AppConfig::default();
        old_cfg.language = "zh".into();
        old_cfg.save(Some(&legacy.join("config.toml"))).unwrap();
        // Force mid-migration failure: create a directory where vault.json file is expected
        fs::create_dir_all(legacy.join("vault.json")).unwrap();

        assert!(AppConfig::migrate_legacy_config_dir(&base).is_err());
        assert_eq!(AppConfig::config_dir_in(&base), legacy);
        assert!(!base.join("polydirect").exists());
        assert!(!base.join("polydirect.migrating").exists());

        // Fix vault.json and retry — migration must cleanly succeed now
        fs::remove_dir(legacy.join("vault.json")).unwrap();
        fs::write(legacy.join("vault.json"), "{\"version\":1}").unwrap();

        let resolved = AppConfig::config_dir_in(&base);
        assert_eq!(resolved, base.join("polydirect"));
        assert!(resolved.join("config.toml").exists());
        assert!(resolved.join("vault.json").exists());
        assert!(!base.join("polydirect.migrating").exists());

        let _ = fs::remove_dir_all(&base);
    }
}
