use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use k256::ecdsa::signature::Verifier as _;
use reqwest::{Client, StatusCode};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{
    APP_VERSION, AppConfig, BUILD_TARGET, validate_polygon_rpc_url, validate_public_https_url,
    validate_public_wss_url, write_private_atomic,
};

pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
pub const MAX_HOT_PATCH_BYTES: usize = 512 * 1024;
pub const MAX_BINARY_BYTES: usize = 150 * 1024 * 1024;
pub const HOT_PATCH_ASSET_NAME: &str = "hot-patch.json";
pub const HOT_PATCH_RELEASE_TAG: &str = "hot-patch";
pub const CHECKSUM_ASSET_NAME: &str = "SHA256SUMS.txt";
pub const CHECKSUM_SIG_ASSET_NAME: &str = "SHA256SUMS.txt.sig";

/// Detached-signature release key (33-byte compressed secp256k1 SEC1, hex).
///
/// SHA-256 of the payload only protects against *transport corruption*: the
/// checksum rides in the same mutable release as the payload, so anyone able to
/// publish to the release can also publish a matching checksum. A detached
/// signature over `SHA256SUMS.txt`, verified against this build-time key, is the
/// only thing that makes the channel trustworthy.
///
/// Set at build time, e.g.
/// `POLYDESK_RELEASE_PUBKEY=02ab…cargo build --release`.
/// While unset the client still works, but logs a warning on every verification
/// attempt because the channel is then unsigned.
pub const RELEASE_PUBKEY_HEX: Option<&str> = option_env!("POLYDESK_RELEASE_PUBKEY");

fn decode_signature_bytes(raw: &str) -> Result<Vec<u8>> {
    let trimmed = raw.trim();
    let unhexed = hex::decode(trimmed.trim_start_matches("0x"));
    if let Ok(bytes) = unhexed {
        return Ok(bytes);
    }
    base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(trimmed))
        .context("release signature is neither hex nor base64")
}

/// Verify a detached secp256k1 signature over the exact `SHA256SUMS.txt` bytes.
///
/// Accepts a 64-byte `r||s` signature or DER, hex- or base64-encoded.
pub fn verify_manifest_signature(checksums: &[u8], signature_blob: &str) -> Result<()> {
    static WARNED_UNSIGNED: std::sync::Once = std::sync::Once::new();
    let Some(pubkey_hex) = RELEASE_PUBKEY_HEX.filter(|s| !s.trim().is_empty()) else {
        WARNED_UNSIGNED.call_once(|| {
            tracing::warn!(
                "POLYDESK_RELEASE_PUBKEY is not set — update channel runs WITHOUT detached \
                 signature verification (SHA-256 only). Set it for production builds."
            );
        });
        return Ok(());
    };
    verify_signature_with_key(pubkey_hex, checksums, signature_blob)
}

/// Core verifier, parameterized on the key so it is unit-testable without rebuilding
/// the crate with a compile-time release key.
pub fn verify_signature_with_key(
    pubkey_hex: &str,
    checksums: &[u8],
    signature_blob: &str,
) -> Result<()> {
    let key_bytes = hex::decode(pubkey_hex.trim().trim_start_matches("0x"))
        .context("POLYDESK_RELEASE_PUBKEY is not valid hex")?;
    let verifying_key = k256::ecdsa::VerifyingKey::from_sec1_bytes(&key_bytes)
        .context("POLYDESK_RELEASE_PUBKEY is not a valid secp256k1 public key")?;

    let sig_bytes = decode_signature_bytes(signature_blob)?;
    let signature = k256::ecdsa::Signature::from_slice(&sig_bytes)
        .or_else(|_| k256::ecdsa::Signature::from_der(&sig_bytes))
        .context("release signature is not a valid secp256k1 signature")?;
    // Normalize S to low-S so standard OpenSSL `openssl dgst -sha256 -sign`
    // signatures (~50% high-S on secp256k1) verify reliably under k256.
    let signature = signature.normalize_s().unwrap_or(signature);

    verifying_key
        .verify(checksums, &signature)
        .context("SHA256SUMS.txt signature does not match the embedded release key")?;
    Ok(())
}

async fn read_response_capped(
    mut resp: reqwest::Response,
    max_bytes: usize,
    label: &str,
) -> Result<Vec<u8>> {
    if let Some(len) = resp.content_length()
        && len > max_bytes as u64
    {
        return Err(anyhow!(
            "{label} exceeds maximum allowed size ({len} > {max_bytes} bytes)"
        ));
    }
    let initial = resp
        .content_length()
        .and_then(|n| usize::try_from(n).ok())
        .map(|n| n.min(max_bytes).min(1024 * 1024))
        .unwrap_or(0);
    let mut buf = Vec::with_capacity(initial);
    while let Some(chunk) = resp.chunk().await? {
        if buf.len().saturating_add(chunk.len()) > max_bytes {
            return Err(anyhow!(
                "{label} exceeded maximum allowed download size ({max_bytes} bytes)"
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateKind {
    UpToDate,
    HotPatch,
    ColdBinary,
    HotAndCold,
}

impl Default for UpdateKind {
    fn default() -> Self {
        Self::UpToDate
    }
}

pub fn compute_update_kind(cold_available: bool, hot_available: bool) -> UpdateKind {
    match (cold_available, hot_available) {
        (true, true) => UpdateKind::HotAndCold,
        (false, true) => UpdateKind::HotPatch,
        (true, false) => UpdateKind::ColdBinary,
        (false, false) => UpdateKind::UpToDate,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotAnnouncement {
    pub id: String,
    #[serde(default = "default_info_severity")]
    pub severity: String,
    pub title: String,
    #[serde(default)]
    pub title_zh: Option<String>,
    pub message: String,
    #[serde(default)]
    pub message_zh: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

fn default_info_severity() -> String {
    "info".to_string()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotEndpointOverrides {
    #[serde(default)]
    pub gamma_host: Option<String>,
    #[serde(default)]
    pub clob_host: Option<String>,
    #[serde(default)]
    pub data_host: Option<String>,
    #[serde(default)]
    pub ws_market_url: Option<String>,
    #[serde(default)]
    pub polygon_rpc_url: Option<String>,
    #[serde(default)]
    pub binance_futures_ws_base: Option<String>,
    #[serde(default)]
    pub binance_spot_ws_base: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotPatchManifest {
    pub schema_version: u32,
    pub patch_id: String,
    pub revision: u64,
    pub min_app_version: String,
    #[serde(default)]
    pub max_app_version: Option<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub summary_zh: Option<String>,
    #[serde(default)]
    pub announcement: Option<HotAnnouncement>,
    #[serde(default)]
    pub translations_en_to_zh: HashMap<String, String>,
    #[serde(default)]
    pub search_aliases_zh_to_en: HashMap<String, String>,
    #[serde(default)]
    pub endpoints: Option<HotEndpointOverrides>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotPatchCompatibility {
    Compatible,
    RequiresColdUpdate { min_version: String },
    SupersededByNewerApp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotPatchApplySummary {
    pub patch_id: String,
    pub revision: u64,
    pub translations_applied: usize,
    pub endpoints_updated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedColdUpdate {
    pub version: String,
    pub exe_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateStatus {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub kind: UpdateKind,
    pub cold_update_available: bool,
    pub hot_patch_available: bool,
    pub hot_patch_requires_cold: bool,
    pub applied_hot_revision: u64,
    pub applied_hot_patch_id: Option<String>,
    pub available_hot_patch_id: Option<String>,
    pub available_hot_revision: Option<u64>,
    pub available_hot_summary: Option<String>,
    pub available_hot_summary_zh: Option<String>,
    pub verified_hot_patch: Option<HotPatchManifest>,
    pub checked_at_ms: i64,
    pub release_url: Option<String>,
    pub release_notes: Option<String>,
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
    pub checksum_url: Option<String>,
    #[serde(default)]
    pub checksum_sig_url: Option<String>,
    #[serde(default)]
    pub hot_patch_error: Option<String>,
    pub repo: String,
    pub target: String,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    body: Option<String>,
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

pub fn platform_asset_suffix() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows-x86_64.exe"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "macos-arm64"
        } else {
            "macos-x86_64"
        }
    } else if cfg!(target_arch = "aarch64") {
        "linux-aarch64"
    } else {
        "linux-x86_64"
    }
}

pub fn expected_asset_name() -> String {
    format!("polydirect-{}", platform_asset_suffix())
}

fn find_matching_asset<'a>(assets: &'a [GhAsset], suffix: &str) -> Option<&'a GhAsset> {
    let candidates = [
        (format!("polydirect-{suffix}"), false),
        (format!("polydirect-{suffix}"), true),
        (format!("polydesk-{suffix}"), false),
        (format!("polydesk-{suffix}"), true),
    ];
    for (needle, ci) in candidates {
        if let Some(found) = assets.iter().find(|a| {
            if ci {
                a.name.eq_ignore_ascii_case(&needle)
            } else {
                a.name == needle
            }
        }) {
            return Some(found);
        }
    }
    None
}

/// Remove the `*.old` rollback copy left behind by a previous self-update.
///
/// Called once at startup: reaching this point proves the replacement binary can
/// launch, which is the signal to drop the rollback copy kept by
/// `download_and_stage_cold_update`.
pub fn cleanup_previous_update() {
    confirm_healthy_update();
}

fn is_valid_github_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let valid_seg = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    valid_seg(owner) && valid_seg(name)
}

fn is_trusted_github_asset_url(url: &str) -> bool {
    url.starts_with("https://github.com/")
        || url.starts_with("https://objects.githubusercontent.com/")
        || url.starts_with("https://release-assets.githubusercontent.com/")
}

fn is_safe_patch_id(id: &str) -> bool {
    let trimmed = id.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 64
        && trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Hosts a hot patch is allowed to point the client at.
///
/// SECURITY: a hot patch is delivered over an unsigned, mutable release channel, so
/// "any public HTTPS host" is not an acceptable constraint — it lets a compromised
/// release redirect CLOB authentication (L1/L2 headers) and signed orders to an
/// attacker. Only infrastructure we already trust at build time may be selected.
/// Matching is exact-host or a dot-prefixed subdomain of an entry here.
pub const TRUSTED_ENDPOINT_HOSTS: &[&str] = &[
    "polymarket.com",
    "clob.polymarket.com",
    "gamma-api.polymarket.com",
    "data-api.polymarket.com",
    "ws-subscriptions-clob.polymarket.com",
    "binance.com",
    "fstream.binance.com",
    "stream.binance.com",
    "data-stream.binance.vision",
    "polygon-bor-rpc.publicnode.com",
    "polygon-rpc.com",
    "polygon.llamarpc.com",
    "rpc.ankr.com",
    "polygon-mainnet.g.alchemy.com",
    "polygon-mainnet.infura.io",
];

/// Hosts a hot-patch announcement may link to. Anything else is rendered as plain
/// text (see `src/ui/mod.rs`), so a compromised manifest cannot turn the in-app
/// banner into a phishing jump-off page.
pub const TRUSTED_ANNOUNCEMENT_HOSTS: &[&str] = &[
    "appdeck.org",
    "polydirect.appdeck.org",
    "polymarket.com",
    "docs.polymarket.com",
    "github.com",
];

fn host_in_allowlist(host: &str, allowlist: &[&str]) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if h.is_empty() {
        return false;
    }
    allowlist.iter().any(|entry| {
        let e = entry.to_ascii_lowercase();
        h == e || h.ends_with(&format!(".{e}"))
    })
}

fn require_trusted_endpoint_host(raw: &str, label: &str) -> Result<()> {
    let parsed = url::Url::parse(raw.trim()).with_context(|| format!("Invalid {label} URL"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("{label} URL must specify a valid host"))?;
    if !host_in_allowlist(host, TRUSTED_ENDPOINT_HOSTS) {
        return Err(anyhow!(
            "{label} host '{host}' is not a trusted Polymarket/Binance/RPC endpoint; \
             refusing a hot patch that redirects traffic off-network"
        ));
    }
    Ok(())
}

/// Belt-and-braces SSRF guard for hot-patch endpoints: the syntax check in
/// `config::validate_public_url` cannot see DNS, so a hostname that resolves to
/// loopback / RFC1918 / CGNAT / ULA would otherwise pass. This closes the
/// DNS-rebinding gap for everything a hot patch can select.
pub async fn verify_hot_patch_endpoints_resolved(patch: &HotPatchManifest) -> Result<()> {
    let Some(ep) = &patch.endpoints else {
        return Ok(());
    };
    let candidates: [(&str, Option<&str>); 7] = [
        ("Gamma host", ep.gamma_host.as_deref()),
        ("CLOB host", ep.clob_host.as_deref()),
        ("Data host", ep.data_host.as_deref()),
        ("Market WS", ep.ws_market_url.as_deref()),
        ("Polygon RPC", ep.polygon_rpc_url.as_deref()),
        ("Binance Futures WS", ep.binance_futures_ws_base.as_deref()),
        ("Binance Spot WS", ep.binance_spot_ws_base.as_deref()),
    ];
    for (label, raw) in candidates {
        let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let parsed = url::Url::parse(raw).with_context(|| format!("Invalid {label} URL"))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow!("{label} URL must specify a valid host"))?;
        // Only resolve when the host is still a domain after the allowlist check;
        // literal IPs were already rejected by `validate_public_url`.
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            if crate::config::is_disallowed_ip(ip) {
                return Err(anyhow!("{label} points at private/local IP {ip}"));
            }
            continue;
        }
        let port = parsed.port_or_known_default().unwrap_or(443);
        let addrs: Vec<std::net::IpAddr> = tokio::net::lookup_host((host, port))
            .await
            .with_context(|| format!("{label} host '{host}' failed DNS resolution"))?
            .map(|sa| sa.ip())
            .collect();
        crate::config::verify_resolved_rpc_ips(host, &addrs)
            .map_err(|e| anyhow!("{label}: {e}"))?;
    }
    Ok(())
}

pub fn validate_hot_patch_manifest(
    patch: &HotPatchManifest,
    current_app_version: &str,
) -> Result<HotPatchCompatibility> {
    if patch.schema_version != 1 {
        return Err(anyhow!(
            "Unsupported hot-patch schema_version {} (expected 1)",
            patch.schema_version
        ));
    }
    if !is_safe_patch_id(&patch.patch_id) {
        return Err(anyhow!(
            "Invalid hot-patch patch_id {:?} (must be 1..=64 ASCII safe chars)",
            patch.patch_id
        ));
    }
    if patch.revision == 0 {
        return Err(anyhow!("Hot-patch revision must be >= 1"));
    }

    let cur_ver = Version::parse(current_app_version.trim_start_matches('v'))
        .with_context(|| format!("invalid current app version {current_app_version:?}"))?;
    let min_ver = Version::parse(patch.min_app_version.trim_start_matches('v'))
        .with_context(|| format!("invalid min_app_version {:?}", patch.min_app_version))?;
    let max_ver = match patch.max_app_version.as_deref() {
        Some(raw) if !raw.trim().is_empty() => Some(
            Version::parse(raw.trim().trim_start_matches('v'))
                .with_context(|| format!("invalid max_app_version {raw:?}"))?,
        ),
        _ => None,
    };

    if let Some(ann) = &patch.announcement
        && let Some(raw_url) = ann.url.as_deref()
        && !raw_url.trim().is_empty()
    {
        let clean = validate_public_https_url(raw_url, "Hot-patch announcement")?;
        let host = url::Url::parse(&clean)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default();
        if !host_in_allowlist(&host, TRUSTED_ANNOUNCEMENT_HOSTS) {
            return Err(anyhow!(
                "Hot-patch announcement host '{host}' is not allowlisted; refusing to render a \
                 remote-controlled outbound link"
            ));
        }
    }

    if let Some(ep) = &patch.endpoints {
        if let Some(v) = ep.gamma_host.as_deref().filter(|s| !s.trim().is_empty()) {
            validate_public_https_url(v, "Hot-patch Gamma host")?;
            require_trusted_endpoint_host(v, "Hot-patch Gamma host")?;
        }
        if let Some(v) = ep.clob_host.as_deref().filter(|s| !s.trim().is_empty()) {
            let clean = validate_public_https_url(v, "Hot-patch CLOB host")?;
            // The executing client is built once and authenticated sessions are
            // host-scoped, so a hot patch must never be able to move order
            // submission. Only the canonical production CLOB is accepted.
            let host = url::Url::parse(&clean)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_default();
            let canonical_clob = host.trim().trim_end_matches('.').to_ascii_lowercase();
            if canonical_clob != "clob.polymarket.com" {
                return Err(anyhow!(
                    "Hot patches may not change clob_host (got '{host}') — order routing is \
                     not remotely configurable"
                ));
            }
        }
        if let Some(v) = ep.data_host.as_deref().filter(|s| !s.trim().is_empty()) {
            validate_public_https_url(v, "Hot-patch Data host")?;
            require_trusted_endpoint_host(v, "Hot-patch Data host")?;
        }
        if let Some(v) = ep.ws_market_url.as_deref().filter(|s| !s.trim().is_empty()) {
            validate_public_wss_url(v, "Hot-patch Market WS")?;
            require_trusted_endpoint_host(v, "Hot-patch Market WS")?;
        }
        if let Some(v) = ep.polygon_rpc_url.as_deref().filter(|s| !s.trim().is_empty()) {
            validate_polygon_rpc_url(v)?;
            require_trusted_endpoint_host(v, "Hot-patch Polygon RPC")?;
        }
        if let Some(v) = ep
            .binance_futures_ws_base
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            validate_public_wss_url(v, "Hot-patch Binance Futures WS")?;
            require_trusted_endpoint_host(v, "Hot-patch Binance Futures WS")?;
        }
        if let Some(v) = ep
            .binance_spot_ws_base
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            validate_public_wss_url(v, "Hot-patch Binance Spot WS")?;
            require_trusted_endpoint_host(v, "Hot-patch Binance Spot WS")?;
        }
    }

    if cur_ver < min_ver {
        return Ok(HotPatchCompatibility::RequiresColdUpdate {
            min_version: min_ver.to_string(),
        });
    }
    if max_ver.is_some_and(|mx| cur_ver > mx) {
        return Ok(HotPatchCompatibility::SupersededByNewerApp);
    }

    Ok(HotPatchCompatibility::Compatible)
}

/// Reject a cached manifest that another local principal could have written.
///
/// The cached file is the only state that is applied without a network round-trip,
/// so it must be a plain file we own with 0600 semantics.
fn cached_patch_is_trustworthy(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o077 != 0 {
            return false;
        }
        // Compare against the config directory we own, avoiding a libc dependency.
        if let Some(dir) = path.parent()
            && let Ok(dir_meta) = fs::metadata(dir)
            && dir_meta.uid() != meta.uid()
        {
            return false;
        }
    }
    true
}

pub fn load_cached_hot_patch(custom_config: Option<&Path>) -> Option<HotPatchManifest> {
    let path = AppConfig::hot_patch_path(custom_config);
    if !cached_patch_is_trustworthy(&path) {
        tracing::warn!(
            path = %path.display(),
            "Ignoring cached hot patch: not a regular 0600 file owned by this user"
        );
        return None;
    }
    let raw = fs::read_to_string(&path).ok()?;
    let manifest: HotPatchManifest = serde_json::from_str(&raw).ok()?;
    match validate_hot_patch_manifest(&manifest, APP_VERSION) {
        Ok(HotPatchCompatibility::Compatible) => Some(manifest),
        Ok(HotPatchCompatibility::RequiresColdUpdate { .. })
        | Ok(HotPatchCompatibility::SupersededByNewerApp)
        | Err(_) => {
            let _ = fs::remove_file(&path);
            None
        }
    }
}

pub fn save_cached_hot_patch(
    custom_config: Option<&Path>,
    patch: &HotPatchManifest,
) -> Result<()> {
    let path = AppConfig::hot_patch_path(custom_config);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(patch)?;
    write_private_atomic(&path, &bytes)
        .with_context(|| format!("failed saving hot patch at {}", path.display()))
}

pub fn clear_cached_hot_patch(custom_config: Option<&Path>) -> Result<()> {
    let path = AppConfig::hot_patch_path(custom_config);
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("failed removing hot patch at {}", path.display()))?;
    }
    Ok(())
}

async fn fetch_release_endpoint(
    http: &Client,
    url: &str,
    allow_404: bool,
) -> std::result::Result<Option<GhRelease>, String> {
    let resp = http
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("GitHub API request failed: {e}"))?;

    if allow_404 && resp.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("GitHub returned HTTP {}", resp.status()));
    }
    let rel_bytes = read_response_capped(resp, MAX_MANIFEST_BYTES, "GitHub release JSON")
        .await
        .map_err(|e| format!("Failed reading GitHub release JSON: {e}"))?;
    let rel: GhRelease = serde_json::from_slice(&rel_bytes)
        .map_err(|e| format!("Failed parsing GitHub release JSON: {e}"))?;
    Ok(Some(rel))
}

/// Fetch, signature-verify and SHA-256-verify a release's hot-patch manifest.
///
/// Returns `Ok(None)` when the release simply does not carry a hot patch.
/// Returns `Err` when a patch **is** present but fails signature or checksum
/// verification — a tamper attempt must be visible to the operator, never folded
/// into "you are up to date".
async fn fetch_verified_hot_patch_from_release(
    http: &Client,
    rel: &GhRelease,
) -> Result<Option<HotPatchManifest>> {
    let Some(patch_asset) = rel
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(HOT_PATCH_ASSET_NAME))
    else {
        return Ok(None);
    };
    let checksum_asset = rel
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(CHECKSUM_ASSET_NAME))
        .ok_or_else(|| {
            anyhow!(
                "Release {} ships {HOT_PATCH_ASSET_NAME} without {CHECKSUM_ASSET_NAME}",
                rel.tag_name
            )
        })?;
    let signature_asset = rel
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(CHECKSUM_SIG_ASSET_NAME));

    if !is_trusted_github_asset_url(&patch_asset.browser_download_url)
        || !is_trusted_github_asset_url(&checksum_asset.browser_download_url)
    {
        return Err(anyhow!(
            "Release {} points {HOT_PATCH_ASSET_NAME} at a non-GitHub host",
            rel.tag_name
        ));
    }

    let sig_fut = async {
        match signature_asset {
            Some(a) if is_trusted_github_asset_url(&a.browser_download_url) => {
                http.get(&a.browser_download_url).send().await.ok()
            }
            _ => None,
        }
    };
    let (cs_res, hp_res, sig_res) = tokio::join!(
        http.get(&checksum_asset.browser_download_url).send(),
        http.get(&patch_asset.browser_download_url).send(),
        sig_fut
    );

    let cs_resp = cs_res
        .context("downloading SHA256SUMS.txt")?
        .error_for_status()
        .context("SHA256SUMS.txt returned an error status")?;
    let hp_resp = hp_res
        .context("downloading hot-patch.json")?
        .error_for_status()
        .context("hot-patch.json returned an error status")?;

    // The allowlist only covers the URL GitHub's API reported. If the request was
    // redirected anywhere else, the body is no longer attributable to GitHub.
    if !is_trusted_github_asset_url(cs_resp.url().as_str())
        || !is_trusted_github_asset_url(hp_resp.url().as_str())
    {
        return Err(anyhow!(
            "Release asset redirect left github.com ({} / {})",
            cs_resp.url(),
            hp_resp.url()
        ));
    }

    let cs_bytes = read_response_capped(cs_resp, MAX_MANIFEST_BYTES, CHECKSUM_ASSET_NAME).await?;
    let hp_bytes = read_response_capped(hp_resp, MAX_HOT_PATCH_BYTES, HOT_PATCH_ASSET_NAME).await?;

    // 1) Detached signature over the exact checksum bytes (trust root).
    if RELEASE_PUBKEY_HEX.is_some_and(|k| !k.trim().is_empty()) {
        let sig_resp = sig_res
            .ok_or_else(|| anyhow!("Release publishes a release key but no {CHECKSUM_SIG_ASSET_NAME}"))?
            .error_for_status()
            .context("SHA256SUMS.txt.sig returned an error status")?;
        let sig_bytes =
            read_response_capped(sig_resp, 4096, CHECKSUM_SIG_ASSET_NAME).await?;
        let sig_text = String::from_utf8(sig_bytes).context("signature asset is not UTF-8")?;
        verify_manifest_signature(&cs_bytes, &sig_text)?;
    } else if let Some(resp) = sig_res
        && let Ok(bytes) = read_response_capped(resp, 4096, CHECKSUM_SIG_ASSET_NAME).await
        && let Ok(text) = String::from_utf8(bytes)
    {
        let _ = decode_signature_bytes(&text);
    }

    let cs_text = String::from_utf8(cs_bytes).context("SHA256SUMS.txt is not valid UTF-8")?;

    // 2) SHA-256 of the manifest against the (now trusted) checksum list,
    //    BEFORE the JSON is parsed.
    verify_sha256_manifest(&cs_text, &patch_asset.name, &hp_bytes)?;

    let manifest = serde_json::from_slice::<HotPatchManifest>(&hp_bytes)
        .context("hot-patch.json is not a valid manifest")?;
    Ok(Some(manifest))
}

pub async fn check_github_updates(
    http: &Client,
    repo: &str,
    applied_hot_revision: u64,
    applied_hot_patch_id: Option<&str>,
) -> UpdateStatus {
    let repo_clean = repo.trim().trim_matches('/');
    let checked_at_ms = chrono::Utc::now().timestamp_millis();
    let base = UpdateStatus {
        current_version: APP_VERSION.to_string(),
        latest_version: None,
        update_available: false,
        kind: UpdateKind::UpToDate,
        cold_update_available: false,
        hot_patch_available: false,
        hot_patch_requires_cold: false,
        applied_hot_revision,
        applied_hot_patch_id: applied_hot_patch_id.map(str::to_string),
        available_hot_patch_id: None,
        available_hot_revision: None,
        available_hot_summary: None,
        available_hot_summary_zh: None,
        verified_hot_patch: None,
        checked_at_ms,
        release_url: None,
        release_notes: None,
        asset_name: Some(expected_asset_name()),
        asset_url: None,
        checksum_url: None,
        checksum_sig_url: None,
        hot_patch_error: None,
        repo: repo_clean.to_string(),
        target: BUILD_TARGET.to_string(),
        error: None,
    };

    if !is_valid_github_repo(repo_clean) {
        return UpdateStatus {
            error: Some("GitHub repo invalid or not configured (expected owner/repo)".into()),
            ..base
        };
    }

    let latest_url = format!("https://api.github.com/repos/{repo_clean}/releases/latest");
    let hot_url =
        format!("https://api.github.com/repos/{repo_clean}/releases/tags/{HOT_PATCH_RELEASE_TAG}");

    let (latest_res, hot_res) = tokio::join!(
        fetch_release_endpoint(http, &latest_url, false),
        fetch_release_endpoint(http, &hot_url, true)
    );

    let rel = match latest_res {
        Ok(Some(r)) => r,
        Ok(None) => {
            return UpdateStatus {
                error: Some("GitHub returned no latest release".into()),
                ..base
            };
        }
        Err(err) => {
            return UpdateStatus {
                error: Some(err),
                ..base
            };
        }
    };

    let latest_str = rel.tag_name.trim_start_matches('v').to_string();
    let current_ver = Version::parse(APP_VERSION).unwrap_or_else(|_| Version::new(0, 1, 0));
    let latest_ver = Version::parse(&latest_str).ok();
    let cold_update_available = latest_ver.as_ref().is_some_and(|lv| lv > &current_ver);

    let suffix = platform_asset_suffix();
    let matched_asset = find_matching_asset(&rel.assets, suffix);
    let want_asset = matched_asset
        .map(|a| a.name.clone())
        .unwrap_or_else(expected_asset_name);
    let asset_url = matched_asset.map(|a| a.browser_download_url.clone());
    let checksum_url = rel
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(CHECKSUM_ASSET_NAME))
        .map(|a| a.browser_download_url.clone());
    let checksum_sig_url = rel
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(CHECKSUM_SIG_ASSET_NAME))
        .map(|a| a.browser_download_url.clone());

    // Only the dedicated rolling tag is a hot-patch channel. Scanning the versioned
    // release as well let a manifest staged for the wrong channel be applied, and
    // broke the "versioned release == immutable binaries" invariant.
    let mut candidate_releases = Vec::new();
    let mut hot_patch_error: Option<String> = None;
    match hot_res {
        Ok(Some(hot_rel)) => candidate_releases.push(hot_rel),
        // A 403/429 on the hot-patch tag used to be indistinguishable from "no hot
        // patch", silently disabling the channel behind NAT/CGNAT rate limits.
        Ok(None) => {}
        Err(err) => {
            hot_patch_error = Some(format!("Hot-patch channel unavailable: {err}"));
        }
    }
    let hot_channel_present = !candidate_releases.is_empty();

    let mut best_hot: Option<HotPatchManifest> = None;
    let mut hot_patch_requires_cold = false;

    for cand_rel in &candidate_releases {
        match fetch_verified_hot_patch_from_release(http, cand_rel).await {
            Ok(Some(manifest)) => match validate_hot_patch_manifest(&manifest, APP_VERSION) {
                Ok(HotPatchCompatibility::Compatible) => {
                    // Reject DNS-level SSRF targets (loopback/RFC1918/CGNAT/ULA) that a
                    // syntax-only host check cannot see.
                    if let Err(e) = verify_hot_patch_endpoints_resolved(&manifest).await {
                        hot_patch_error = Some(e.to_string());
                        continue;
                    }
                    if manifest.revision > applied_hot_revision
                        && best_hot
                            .as_ref()
                            .is_none_or(|b| manifest.revision > b.revision)
                    {
                        best_hot = Some(manifest);
                    }
                }
                Ok(HotPatchCompatibility::RequiresColdUpdate { .. }) => {
                    hot_patch_requires_cold = true;
                }
                Ok(HotPatchCompatibility::SupersededByNewerApp) => {}
                Err(e) => {
                    hot_patch_error = Some(format!("Hot patch rejected: {e}"));
                }
            },
            Ok(None) => {}
            Err(e) => {
                // Signature / checksum failure: surface it, do not hide it behind
                // an "up to date" verdict.
                hot_patch_error = Some(format!("Hot patch failed verification: {e}"));
            }
        }
    }

    let hot_patch_available = best_hot.is_some();
    let kind = compute_update_kind(cold_update_available, hot_patch_available);
    let update_available = cold_update_available || hot_patch_available;
    let _ = hot_channel_present;

    let latest_rel = Some(&rel);

    UpdateStatus {
        latest_version: Some(latest_str),
        update_available,
        kind,
        cold_update_available,
        hot_patch_available,
        hot_patch_requires_cold,
        available_hot_patch_id: best_hot.as_ref().map(|h| h.patch_id.clone()),
        available_hot_revision: best_hot.as_ref().map(|h| h.revision),
        available_hot_summary: best_hot.as_ref().and_then(|h| {
            (!h.summary.trim().is_empty()).then(|| h.summary.clone())
        }),
        available_hot_summary_zh: best_hot
            .as_ref()
            .and_then(|h| h.summary_zh.clone())
            .filter(|s| !s.trim().is_empty()),
        verified_hot_patch: best_hot,
        release_url: latest_rel.as_ref().map(|r| r.html_url.clone()),
        release_notes: latest_rel.and_then(|r| r.body.clone()),
        asset_name: Some(want_asset),
        asset_url,
        checksum_url,
        checksum_sig_url,
        hot_patch_error,
        ..base
    }
}

pub async fn check_github_release(http: &Client, repo: &str) -> UpdateStatus {
    check_github_updates(http, repo, 0, None).await
}

pub fn verify_sha256_manifest(manifest: &str, asset_name: &str, bytes: &[u8]) -> Result<()> {
    let actual = hex::encode(Sha256::digest(bytes));
    let mut matches: Vec<String> = Vec::new();
    for line in manifest.lines() {
        let mut parts = line.split_whitespace();
        let Some(hash) = parts.next() else { continue };
        let Some(name) = parts.next() else { continue };
        let name = name.trim_start_matches('*');
        if name != asset_name {
            continue;
        }
        // A filename in a checksum manifest must be a bare file name. Reject
        // traversal/absolute forms so a crafted manifest cannot describe a
        // different file that happens to share the suffix.
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            return Err(anyhow!(
                "SHA256SUMS.txt contains an unsafe asset name {name:?}"
            ));
        }
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(anyhow!(
                "SHA256SUMS.txt contains a malformed hash for {asset_name}: {hash:?}"
            ));
        }
        matches.push(hash.to_ascii_lowercase());
    }

    let expected = match matches.len() {
        0 => return Err(anyhow!("Asset {asset_name} not listed in SHA256SUMS.txt")),
        1 => matches.remove(0),
        _ => {
            // Ambiguous manifest: an attacker (or a botched merge) can otherwise
            // shadow the real entry by prepending a line with a matching name.
            matches.sort();
            matches.dedup();
            if matches.len() > 1 {
                return Err(anyhow!(
                    "SHA256SUMS.txt lists {asset_name} with {} conflicting hashes — refusing",
                    matches.len()
                ));
            }
            matches.remove(0)
        }
    };

    if actual != expected {
        return Err(anyhow!(
            "SHA-256 mismatch! expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

/// Download release binary, verify SHA-256 download integrity against GitHub Release
/// `SHA256SUMS.txt` over HTTPS (not detached code signing), atomically replace self on disk,
/// and return the new version plus the exact executable path captured BEFORE swap (safe on Linux).
/// Verify that `dir` can actually be written to before we spend bandwidth on a
/// ~150 MB download that can only fail at the final rename.
fn probe_writable_dir(dir: &Path) -> Result<()> {
    let probe = dir.join(format!(".polydesk-write-probe-{}", std::process::id()));
    let res = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe);
    match res {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(anyhow!(
            "Install directory {} is not writable ({e}); reinstall to a per-user location \
             or run the package manager instead of an in-app update",
            dir.display()
        )),
    }
}

/// Write the staged binary without following symlinks and flush it to stable
/// storage before the swap, so a crash cannot activate a half-written executable.
fn write_staged_binary(staged: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    // `create_new` refuses to follow a pre-existing symlink planted in the install
    // directory (which would otherwise let a local attacker redirect a 150 MB write).
    let open = |path: &Path| -> std::io::Result<std::fs::File> {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o700);
        }
        opts.open(path)
    };

    let mut file = match open(staged) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Only remove a *regular* leftover from a previous failed attempt; never
            // delete a symlink or directory that we did not create.
            let meta = fs::symlink_metadata(staged)
                .with_context(|| format!("failed inspecting {}", staged.display()))?;
            if !meta.file_type().is_file() {
                return Err(anyhow!(
                    "Refusing to overwrite non-regular file at {} — remove it manually",
                    staged.display()
                ));
            }
            fs::remove_file(staged)?;
            open(staged).context("failed creating staged update binary")?
        }
        Err(e) => return Err(e).context("failed creating staged update binary"),
    };

    file.write_all(bytes)
        .context("failed writing staged update binary")?;
    file.sync_all()
        .context("failed flushing staged update binary")?;
    drop(file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(staged, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// fsync the directory containing `path` so the rename survives a power loss.
fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && let Ok(dir) = fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
}

/// Download release binary, verify the detached signature over `SHA256SUMS.txt`
/// (when a release key is compiled in) and the SHA-256 of the binary itself, then
/// atomically activate it while keeping a rollback copy until the new build has
/// been observed healthy.
pub async fn download_and_stage_cold_update(
    http: &Client,
    repo: &str,
) -> Result<StagedColdUpdate> {
    let status = check_github_release(http, repo).await;
    if let Some(err) = status.error {
        return Err(anyhow!(err));
    }
    if !status.cold_update_available {
        return Err(anyhow!(
            "Already on latest binary version ({})",
            status.current_version
        ));
    }
    let asset_name = status
        .asset_name
        .ok_or_else(|| anyhow!("Unknown target asset"))?;
    let asset_url = status
        .asset_url
        .ok_or_else(|| anyhow!("Release is missing asset {asset_name}"))?;
    let checksum_url = status.checksum_url.clone().ok_or_else(|| {
        anyhow!("Release is missing SHA256SUMS.txt (refusing unverified binary)")
    })?;

    if !is_trusted_github_asset_url(&asset_url) || !is_trusted_github_asset_url(&checksum_url) {
        return Err(anyhow!("Refusing untrusted non-GitHub release asset URL"));
    }

    // Fail before the download, not after it.
    let current_exe =
        std::env::current_exe().context("cannot determine current executable path")?;
    let parent = current_exe
        .parent()
        .ok_or_else(|| anyhow!("invalid executable directory"))?;
    if cfg!(target_os = "macos") && current_exe.components().any(|c| c.as_os_str() == "Contents") {
        return Err(anyhow!(
            "Refusing in-place self-replace inside a macOS .app bundle — it invalidates the \
             code signature. Download the new build from GitHub Releases instead."
        ));
    }
    probe_writable_dir(parent)?;

    let checksum_resp = http
        .get(&checksum_url)
        .send()
        .await?
        .error_for_status()?;
    if !is_trusted_github_asset_url(checksum_resp.url().as_str()) {
        return Err(anyhow!(
            "Checksum redirect left trusted GitHub hosts ({})",
            checksum_resp.url()
        ));
    }
    let checksum_bytes = read_response_capped(checksum_resp, MAX_MANIFEST_BYTES, CHECKSUM_ASSET_NAME)
        .await?;

    if RELEASE_PUBKEY_HEX.is_some_and(|k| !k.trim().is_empty()) {
        let sig_url = status.checksum_sig_url.clone().ok_or_else(|| {
            anyhow!("Release publishes a release key but no {CHECKSUM_SIG_ASSET_NAME}")
        })?;
        if !is_trusted_github_asset_url(&sig_url) {
            return Err(anyhow!("Refusing untrusted signature URL"));
        }
        let sig_resp = http.get(&sig_url).send().await?.error_for_status()?;
        if !is_trusted_github_asset_url(sig_resp.url().as_str()) {
            return Err(anyhow!(
                "Signature redirect left trusted GitHub hosts ({})",
                sig_resp.url()
            ));
        }
        let sig_bytes =
            read_response_capped(sig_resp, 4096, CHECKSUM_SIG_ASSET_NAME).await?;
        let sig_text = String::from_utf8(sig_bytes).context("signature asset is not UTF-8")?;
        verify_manifest_signature(&checksum_bytes, &sig_text)?;
    }

    let checksum_text =
        String::from_utf8(checksum_bytes).context("SHA256SUMS.txt is not valid UTF-8")?;

    let bin_resp = http.get(&asset_url).send().await?.error_for_status()?;
    if !is_trusted_github_asset_url(bin_resp.url().as_str()) {
        return Err(anyhow!(
            "Binary redirect left trusted GitHub hosts ({})",
            bin_resp.url()
        ));
    }
    let bytes = read_response_capped(bin_resp, MAX_BINARY_BYTES, "Release binary").await?;

    verify_sha256_manifest(&checksum_text, &asset_name, &bytes)?;

    let staged = parent.join(format!("{asset_name}.staged"));
    // On Windows the running .exe is locked for deletion but may be renamed; on Unix
    // it can be unlinked. Keep the rollback copy on both platforms until the next
    // successful start (see `confirm_healthy_update`).
    let backup = PathBuf::from(format!("{}.old", current_exe.display()));

    write_staged_binary(&staged, &bytes)?;

    if backup.exists() {
        let _ = fs::remove_file(&backup);
    }

    fs::rename(&current_exe, &backup).context("failed moving current executable to backup")?;
    if let Err(e) = fs::rename(&staged, &current_exe) {
        // Roll back, and do not leave a 150 MB artefact behind for the next attempt.
        let restored = fs::rename(&backup, &current_exe);
        let _ = fs::remove_file(&staged);
        return Err(anyhow!(
            "failed activating staged binary: {e} (rollback {})",
            if restored.is_ok() {
                "succeeded"
            } else {
                "FAILED — restore the .old file manually"
            }
        ));
    }
    sync_parent_dir(&current_exe);

    // Backup is intentionally kept: `confirm_healthy_update()` removes it once the
    // replacement process has proven it can start.
    Ok(StagedColdUpdate {
        version: status.latest_version.unwrap_or_else(|| "latest".into()),
        exe_path: current_exe,
    })
}

/// Directory that owns the `*.old` rollback copy for the running executable.
pub fn rollback_backup_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .map(|exe| PathBuf::from(format!("{}.old", exe.display())))
}

/// Called once the app is up and healthy: deletes the `*.old` rollback copy.
pub fn confirm_healthy_update() {
    if let Some(backup) = rollback_backup_path()
        && backup.exists()
    {
        let _ = fs::remove_file(&backup);
    }
}

pub async fn download_and_apply_update(http: &Client, repo: &str) -> Result<String> {
    download_and_stage_cold_update(http, repo)
        .await
        .map(|s| s.version)
}

/// Safely spawn the already-activated replacement executable using the path captured BEFORE file swap.
pub fn relaunch_exe(exe_path: &Path) -> Result<()> {
    // Reject symlinks and non-regular files: the swap target is a file we wrote
    // ourselves, so anything else at this path means the install directory was
    // tampered with between activation and relaunch.
    let meta = fs::symlink_metadata(exe_path)
        .with_context(|| format!("Updated executable missing at {}", exe_path.display()))?;
    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        return Err(anyhow!(
            "Refusing to launch {} — it is not a regular executable file",
            exe_path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(anyhow!(
                "Refusing to launch {} — it is not executable",
                exe_path.display()
            ));
        }
    }
    std::process::Command::new(exe_path)
        .spawn()
        .with_context(|| format!("failed spawning updated executable at {}", exe_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_hot_patch() -> HotPatchManifest {
        HotPatchManifest {
            schema_version: 1,
            patch_id: "0.3.2-hot.1".into(),
            revision: 3,
            min_app_version: "0.3.0".into(),
            max_app_version: Some("0.9.0".into()),
            summary: "Fix BTC/USD chart label translation".into(),
            summary_zh: Some("修复 BTC/USD 图表中文译文".into()),
            announcement: Some(HotAnnouncement {
                id: "ann-1".into(),
                severity: "info".into(),
                title: "Maintenance Notice".into(),
                title_zh: Some("维护通知".into()),
                message: "Live feeds operating normally.".into(),
                message_zh: Some("实时数据源运行正常。".into()),
                url: Some("https://polydirect.appdeck.org".into()),
            }),
            translations_en_to_zh: HashMap::from([("Up".into(), "看涨".into())]),
            search_aliases_zh_to_en: HashMap::from([("比特币".into(), "bitcoin".into())]),
            endpoints: Some(HotEndpointOverrides {
                gamma_host: Some("https://gamma-api.polymarket.com".into()),
                clob_host: Some("https://clob.polymarket.com".into()),
                data_host: Some("https://data-api.polymarket.com".into()),
                ws_market_url: Some("wss://ws-subscriptions-clob.polymarket.com/ws/market".into()),
                polygon_rpc_url: Some("https://polygon-bor-rpc.publicnode.com".into()),
                binance_futures_ws_base: Some("wss://fstream.binance.com/stream?streams=".into()),
                binance_spot_ws_base: Some("wss://stream.binance.com:9443/stream?streams=".into()),
            }),
        }
    }

    #[test]
    fn sha256_manifest_verification_works() {
        let data = b"polydirect-test-binary";
        let hash = hex::encode(Sha256::digest(data));
        let manifest = format!("{hash}  polydirect-linux-x86_64\n0000  other-file\n");
        assert!(verify_sha256_manifest(&manifest, "polydirect-linux-x86_64", data).is_ok());
        assert!(verify_sha256_manifest(&manifest, "polydirect-linux-x86_64", b"tampered").is_err());
        assert!(verify_sha256_manifest(&manifest, "missing-bin", data).is_err());
        assert!(expected_asset_name().starts_with("polydirect-"));
        assert!(is_valid_github_repo("Mluo2011/polydirect"));
        assert!(is_valid_github_repo("Mluo2011/polydesk"));
        assert!(!is_valid_github_repo("../evil/repo"));
        assert!(!is_valid_github_repo("owner/repo?foo=bar"));
        assert!(is_trusted_github_asset_url("https://github.com/Mluo2011/polydirect/releases/download/v0.2.5/polydirect-linux-x86_64"));
        assert!(is_trusted_github_asset_url("https://github.com/Mluo2011/polydesk/releases/download/v0.2.5/polydesk-linux-x86_64"));
        assert!(!is_trusted_github_asset_url("http://github.com/Mluo2011/polydirect/releases/download/v0.2.5/bin"));
        assert!(!is_trusted_github_asset_url("https://evil.example/bin"));

        let legacy_only = vec![
            GhAsset {
                name: "PolyDesk-linux-x86_64".into(),
                browser_download_url: "https://github.com/Mluo2011/polydesk/releases/download/v0.2.4/PolyDesk-linux-x86_64".into(),
            },
            GhAsset {
                name: "polydesk-linux-x86_64".into(),
                browser_download_url: "https://github.com/Mluo2011/polydesk/releases/download/v0.2.5/polydesk-linux-x86_64".into(),
            },
        ];
        assert_eq!(
            find_matching_asset(&legacy_only, "linux-x86_64").map(|a| a.name.as_str()),
            Some("polydesk-linux-x86_64")
        );

        let mixed = vec![
            GhAsset {
                name: "polydesk-linux-x86_64".into(),
                browser_download_url: "https://github.com/Mluo2011/polydesk/releases/download/v0.2.5/polydesk-linux-x86_64".into(),
            },
            GhAsset {
                name: "PolyDirect-linux-x86_64".into(),
                browser_download_url: "https://github.com/Mluo2011/polydirect/releases/download/v0.2.6/PolyDirect-linux-x86_64".into(),
            },
            GhAsset {
                name: "polydirect-linux-x86_64".into(),
                browser_download_url: "https://github.com/Mluo2011/polydirect/releases/download/v0.2.6/polydirect-linux-x86_64".into(),
            },
        ];
        assert_eq!(
            find_matching_asset(&mixed, "linux-x86_64").map(|a| a.name.as_str()),
            Some("polydirect-linux-x86_64")
        );

        assert_eq!(
            find_matching_asset(&mixed[..2], "linux-x86_64").map(|a| a.name.as_str()),
            Some("PolyDirect-linux-x86_64")
        );
    }

    #[test]
    fn hot_patch_manifest_validation_and_compat_gating() {
        let base = sample_hot_patch();
        assert_eq!(
            validate_hot_patch_manifest(&base, "0.3.2").unwrap(),
            HotPatchCompatibility::Compatible
        );
        assert_eq!(
            validate_hot_patch_manifest(&base, "0.2.9").unwrap(),
            HotPatchCompatibility::RequiresColdUpdate {
                min_version: "0.3.0".into(),
            }
        );
        assert_eq!(
            validate_hot_patch_manifest(&base, "1.0.0").unwrap(),
            HotPatchCompatibility::SupersededByNewerApp
        );

        // SHA-256 verification catches tampered hot-patch bytes before parse
        let json_bytes = serde_json::to_vec(&base).unwrap();
        let hash = hex::encode(Sha256::digest(&json_bytes));
        let manifest = format!("{hash}  hot-patch.json\n");
        assert!(verify_sha256_manifest(&manifest, HOT_PATCH_ASSET_NAME, &json_bytes).is_ok());
        assert!(verify_sha256_manifest(&manifest, HOT_PATCH_ASSET_NAME, b"{}").is_err());

        // Reject unsafe schemes, local/private IPs, localhost, and embedded credentials
        for bad_https in [
            "http://clob.polymarket.com",
            "https://localhost/api",
            "https://127.0.0.1/api",
            "https://10.0.0.1/api",
            "https://user:pass@clob.polymarket.com",
        ] {
            let mut p = sample_hot_patch();
            p.endpoints.as_mut().unwrap().clob_host = Some(bad_https.into());
            assert!(
                validate_hot_patch_manifest(&p, "0.3.2").is_err(),
                "expected rejection for https endpoint {bad_https:?}"
            );
        }

        for bad_wss in [
            "ws://fstream.binance.com/stream?streams=",
            "wss://localhost/ws",
            "wss://127.0.0.1:9443/ws",
            "wss://10.0.0.5/ws",
            "wss://user:pw@fstream.binance.com/stream?streams=",
        ] {
            let mut p = sample_hot_patch();
            p.endpoints.as_mut().unwrap().ws_market_url = Some(bad_wss.into());
            assert!(
                validate_hot_patch_manifest(&p, "0.3.2").is_err(),
                "expected rejection for wss endpoint {bad_wss:?}"
            );
        }

        let mut bad_schema = sample_hot_patch();
        bad_schema.schema_version = 2;
        assert!(validate_hot_patch_manifest(&bad_schema, "0.3.2").is_err());
    }

    #[test]
    fn hot_patch_cache_roundtrip_and_clear() {
        let dir = std::env::temp_dir().join(format!("polydirect-hp-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("config.toml");

        let mut patch = sample_hot_patch();
        patch.min_app_version = "0.1.0".into();
        patch.max_app_version = None;
        save_cached_hot_patch(Some(&cfg_path), &patch).unwrap();
        assert!(dir.join("hot_patch.json").exists());

        let loaded = load_cached_hot_patch(Some(&cfg_path)).unwrap();
        assert_eq!(loaded, patch);

        clear_cached_hot_patch(Some(&cfg_path)).unwrap();
        assert!(!dir.join("hot_patch.json").exists());
        assert!(load_cached_hot_patch(Some(&cfg_path)).is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_kind_classification() {
        assert_eq!(compute_update_kind(false, false), UpdateKind::UpToDate);
        assert_eq!(compute_update_kind(false, true), UpdateKind::HotPatch);
        assert_eq!(compute_update_kind(true, false), UpdateKind::ColdBinary);
        assert_eq!(compute_update_kind(true, true), UpdateKind::HotAndCold);
    }

    // ---- §4.1 / §4.3: manifest ambiguity, path names, host allowlist ----------------

    #[test]
    fn sha256_manifest_rejects_ambiguous_and_unsafe_names() {
        let data = b"payload";
        let good = hex::encode(Sha256::digest(data));

        // Duplicate, conflicting entries for the same asset.
        let two_hashes = format!("{good}  hot-patch.json\n{}  hot-patch.json\n", "0".repeat(64));
        assert!(
            verify_sha256_manifest(&two_hashes, "hot-patch.json", data).is_err(),
            "conflicting duplicate hashes must be rejected"
        );

        // Duplicate but identical entries are harmless.
        let same_twice = format!("{good}  hot-patch.json\n{good}  hot-patch.json\n");
        assert!(verify_sha256_manifest(&same_twice, "hot-patch.json", data).is_ok());

        // Traversal / absolute-path style names.
        for bad in ["../hot-patch.json", "/etc/hot-patch.json", "dir/hot-patch.json"] {
            let m = format!("{good}  {bad}\n");
            assert!(
                verify_sha256_manifest(&m, bad, data).is_err(),
                "unsafe asset name {bad:?} must be rejected"
            );
        }

        // Malformed hash.
        let m = "zzzz  hot-patch.json\n".to_string();
        assert!(verify_sha256_manifest(&m, "hot-patch.json", data).is_err());
    }

    #[test]
    fn hot_patch_endpoints_must_be_allowlisted() {
        let mut p = sample_hot_patch();
        p.endpoints.as_mut().unwrap().clob_host =
            Some("https://clob.polymarket.example".into());
        assert!(
            validate_hot_patch_manifest(&p, "0.3.2").is_err(),
            "clob_host must not be redirectable"
        );

        for bad in [
            "https://gamma.polymarket.example",
            "https://rebind.attacker.tld",
            "https://10.0.0.5/api",
        ] {
            let mut p = sample_hot_patch();
            p.endpoints.as_mut().unwrap().gamma_host = Some(bad.into());
            assert!(
                validate_hot_patch_manifest(&p, "0.3.2").is_err(),
                "gamma_host {bad:?} must be rejected"
            );
        }

        // Allowlisted subdomains still work.
        let mut p = sample_hot_patch();
        p.endpoints.as_mut().unwrap().gamma_host = Some("https://gamma-api.polymarket.com".into());
        assert!(validate_hot_patch_manifest(&p, "0.3.2").is_ok());

        assert!(host_in_allowlist("gamma-api.polymarket.com", TRUSTED_ENDPOINT_HOSTS));
        assert!(host_in_allowlist("eu.api.binance.com", TRUSTED_ENDPOINT_HOSTS));
        assert!(!host_in_allowlist("evilmopolymarket.com", TRUSTED_ENDPOINT_HOSTS));
        assert!(!host_in_allowlist("polymarket.com.evil.tld", TRUSTED_ENDPOINT_HOSTS));
    }

    #[test]
    fn announcement_urls_must_be_allowlisted() {
        let mut p = sample_hot_patch();
        p.announcement.as_mut().unwrap().url = Some("https://polydirect-wallet-verify.tld".into());
        assert!(
            validate_hot_patch_manifest(&p, "0.3.2").is_err(),
            "phishing announcement targets must be rejected"
        );
        let mut ok = sample_hot_patch();
        ok.announcement.as_mut().unwrap().url = Some("https://polydirect.appdeck.org/notes".into());
        assert!(validate_hot_patch_manifest(&ok, "0.3.2").is_ok());
    }

    #[tokio::test]
    async fn endpoint_dns_guard_rejects_private_literals() {
        let mut p = sample_hot_patch();
        p.endpoints.as_mut().unwrap().gamma_host = Some("https://10.0.0.5/api".into());
        assert!(verify_hot_patch_endpoints_resolved(&p).await.is_err());

        let mut p = sample_hot_patch();
        p.endpoints.as_mut().unwrap().polygon_rpc_url = Some("https://127.0.0.1/rpc".into());
        assert!(verify_hot_patch_endpoints_resolved(&p).await.is_err());
    }

    // ---- §4.4: detached signature round trip ----------------------------------------

    #[test]
    fn release_signature_roundtrip_and_tamper_detection() {
        use k256::ecdsa::signature::Signer as _;

        let signing_key = k256::ecdsa::SigningKey::from_bytes(&[7u8; 32].into()).unwrap();
        let pubkey_hex = hex::encode(signing_key.verifying_key().to_encoded_point(true).as_bytes());

        let checksums = b"d2c4f0...  hot-patch.json\n";
        let sig: k256::ecdsa::Signature = signing_key.sign(checksums);
        let sig_hex = hex::encode(sig.to_bytes());

        assert!(verify_signature_with_key(&pubkey_hex, checksums, &sig_hex).is_ok());

        // Any byte of the manifest changing must break verification.
        let tampered = b"d2c4f0...  hot-patch.json\n";
        assert!(
            verify_signature_with_key(&pubkey_hex, &tampered[..tampered.len() - 1], &sig_hex)
                .is_err()
        );
        // A signature from a different key must be rejected.
        let other = k256::ecdsa::SigningKey::from_bytes(&[9u8; 32].into()).unwrap();
        let other_sig: k256::ecdsa::Signature = other.sign(checksums);
        assert!(
            verify_signature_with_key(&pubkey_hex, checksums, &hex::encode(other_sig.to_bytes()))
                .is_err()
        );
        // Malformed key material fails closed.
        assert!(verify_signature_with_key("zz", checksums, &sig_hex).is_err());
    }

    #[test]
    fn release_signature_accepts_high_s_der_signature() {
        use k256::ecdsa::signature::Signer as _;
        use k256::elliptic_curve::scalar::IsHigh as _;

        let signing_key = k256::ecdsa::SigningKey::from_bytes(&[11u8; 32].into()).unwrap();
        let pubkey_hex = hex::encode(signing_key.verifying_key().to_encoded_point(true).as_bytes());
        let checksums = b"abc123  polydirect-linux-x86_64\n";
        let sig: k256::ecdsa::Signature = signing_key.sign(checksums);
        let (r, s) = sig.split_scalars();
        let s_high = k256::NonZeroScalar::new(-*s).unwrap();
        let sig_high = k256::ecdsa::Signature::from_scalars(r, s_high).unwrap();
        assert!(bool::from(sig_high.s().is_high()));

        let der_b64 = base64::engine::general_purpose::STANDARD.encode(sig_high.to_der().as_bytes());
        assert!(verify_signature_with_key(&pubkey_hex, checksums, &der_b64).is_ok());
        assert!(verify_signature_with_key(&pubkey_hex, b"tampered", &der_b64).is_err());
    }

    // ---- §4.11: cached patch file trustworthiness -----------------------------------

    #[cfg(unix)]
    #[test]
    fn cached_hot_patch_rejects_loose_permissions_and_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let dir = std::env::temp_dir().join(format!("polydirect-hp-perm-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml");
        let mut patch = sample_hot_patch();
        patch.min_app_version = "0.1.0".into();
        patch.max_app_version = None;
        save_cached_hot_patch(Some(&cfg), &patch).unwrap();
        let hp = dir.join("hot_patch.json");
        assert!(load_cached_hot_patch(Some(&cfg)).is_some());

        // World-readable cache must be ignored.
        fs::set_permissions(&hp, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_cached_hot_patch(Some(&cfg)).is_none());
        fs::set_permissions(&hp, fs::Permissions::from_mode(0o600)).unwrap();

        // Symlinked cache must be ignored.
        let target = dir.join("elsewhere.json");
        fs::rename(&hp, &target).unwrap();
        symlink(&target, &hp).unwrap();
        assert!(load_cached_hot_patch(Some(&cfg)).is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_apply_is_off_by_default_and_migrated_off() {
        assert!(!AppConfig::default().auto_apply_hot_updates);
        // Simulate a v0.3.2-draft config.toml that still carries `true`.
        let mut cfg = AppConfig {
            auto_apply_hot_updates: true,
            ..AppConfig::default()
        };
        cfg.sanitize();
        assert!(!cfg.auto_apply_hot_updates, "legacy true must be migrated off");
    }

    // ---- §4.7/§4.8: staged-binary write must not follow planted links ---------------

    #[cfg(unix)]
    #[test]
    fn write_staged_binary_refuses_to_follow_a_planted_symlink() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!("polydesk-stage-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("victim.txt");
        fs::write(&victim, b"do not overwrite me").unwrap();
        let staged = dir.join("polydirect-linux-x86_64.staged");
        symlink(&victim, &staged).unwrap();

        let err = write_staged_binary(&staged, b"evil payload").unwrap_err();
        assert!(
            err.to_string().contains("non-regular") || err.to_string().contains("exists"),
            "unexpected error: {err}"
        );
        assert_eq!(fs::read(&victim).unwrap(), b"do not overwrite me");

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn write_staged_binary_replaces_stale_regular_file_with_executable_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("polydesk-stage-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let staged = dir.join("polydirect-linux-x86_64.staged");
        fs::write(&staged, b"previous failed attempt").unwrap();

        write_staged_binary(&staged, b"new binary bytes").unwrap();
        assert_eq!(fs::read(&staged).unwrap(), b"new binary bytes");
        assert_eq!(
            fs::metadata(&staged).unwrap().permissions().mode() & 0o777,
            0o755
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn writability_probe_detects_a_read_only_install_dir() {
        let dir = std::env::temp_dir().join(format!("polydesk-probe-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        assert!(probe_writable_dir(&dir).is_ok());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
            // Running as root would bypass the permission check; skip in that case.
            if std::fs::write(dir.join("probe"), b"x").is_err() {
                assert!(probe_writable_dir(&dir).is_err());
            }
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn read_response_capped_enforces_byte_limits() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        async fn serve_once(response: &'static [u8]) -> String {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                if let Ok((mut sock, _)) = listener.accept().await {
                    let mut buf = [0u8; 512];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock.write_all(response).await;
                    let _ = sock.shutdown().await;
                }
            });
            format!("http://{addr}/test")
        }

        let http = Client::new();

        // 1. Below limit succeeds
        let url_ok = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").await;
        let resp_ok = http.get(&url_ok).send().await.unwrap();
        let bytes = read_response_capped(resp_ok, 16, "small").await.unwrap();
        assert_eq!(bytes, b"hello");

        // 2. Oversized via Content-Length rejected
        let url_cl = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\nConnection: close\r\n\r\n0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").await;
        let resp_cl = http.get(&url_cl).send().await.unwrap();
        assert!(read_response_capped(resp_cl, 16, "cl-over").await.is_err());

        // 3. Oversized without Content-Length (chunked / streamed) rejected while reading
        let url_chunked = serve_once(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n14\r\n01234567890123456789\r\n0\r\n\r\n",
        )
        .await;
        let resp_chunked = http.get(&url_chunked).send().await.unwrap();
        assert!(read_response_capped(resp_chunked, 16, "stream-over").await.is_err());
    }
}
