use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{APP_VERSION, BUILD_TARGET};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateStatus {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub release_url: Option<String>,
    pub release_notes: Option<String>,
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
    pub checksum_url: Option<String>,
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

pub fn expected_asset_name() -> String {
    if cfg!(target_os = "windows") {
        "polydesk-windows-x86_64.exe".to_string()
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "polydesk-macos-arm64".to_string()
        } else {
            "polydesk-macos-x86_64".to_string()
        }
    } else if cfg!(target_arch = "aarch64") {
        "polydesk-linux-aarch64".to_string()
    } else {
        "polydesk-linux-x86_64".to_string()
    }
}

/// Remove leftover `.old` executable left behind by a previous Windows self-update.
pub fn cleanup_previous_update() {
    if let Ok(exe) = std::env::current_exe() {
        let old = PathBuf::from(format!("{}.old", exe.display()));
        if old.exists() {
            let _ = fs::remove_file(old);
        }
    }
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

pub async fn check_github_release(http: &Client, repo: &str) -> UpdateStatus {
    let repo_clean = repo.trim().trim_matches('/');
    let base = UpdateStatus {
        current_version: APP_VERSION.to_string(),
        latest_version: None,
        update_available: false,
        release_url: None,
        release_notes: None,
        asset_name: Some(expected_asset_name()),
        asset_url: None,
        checksum_url: None,
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

    let url = format!("https://api.github.com/repos/{repo_clean}/releases/latest");
    let resp = match http
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return UpdateStatus {
                error: Some(format!("GitHub API request failed: {e}")),
                ..base
            };
        }
    };

    if !resp.status().is_success() {
        return UpdateStatus {
            error: Some(format!("GitHub returned HTTP {}", resp.status())),
            ..base
        };
    }

    let rel: GhRelease = match resp.json().await {
        Ok(r) => r,
        Err(e) => {
            return UpdateStatus {
                error: Some(format!("Failed parsing GitHub release JSON: {e}")),
                ..base
            };
        }
    };

    let latest_str = rel.tag_name.trim_start_matches('v').to_string();
    let current_ver = Version::parse(APP_VERSION).unwrap_or_else(|_| Version::new(0, 1, 0));
    let latest_ver = Version::parse(&latest_str).ok();
    let update_available = latest_ver.as_ref().is_some_and(|lv| lv > &current_ver);

    let want_asset = expected_asset_name();
    let asset_url = rel
        .assets
        .iter()
        .find(|a| a.name == want_asset)
        .map(|a| a.browser_download_url.clone());
    let checksum_url = rel
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case("SHA256SUMS.txt"))
        .map(|a| a.browser_download_url.clone());

    UpdateStatus {
        latest_version: Some(latest_str),
        update_available,
        release_url: Some(rel.html_url),
        release_notes: rel.body,
        asset_name: Some(want_asset),
        asset_url,
        checksum_url,
        ..base
    }
}

pub fn verify_sha256_manifest(manifest: &str, asset_name: &str, bytes: &[u8]) -> Result<()> {
    let actual = hex::encode(Sha256::digest(bytes));
    let expected = manifest
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            Some((hash.to_lowercase(), name))
        })
        .find(|(_, name)| *name == asset_name)
        .map(|(h, _)| h)
        .ok_or_else(|| anyhow!("Asset {asset_name} not listed in SHA256SUMS.txt"))?;

    if actual != expected {
        return Err(anyhow!(
            "SHA-256 mismatch! expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

/// Download release binary, verify SHA-256 against `SHA256SUMS.txt`, and atomically replace self.
pub async fn download_and_apply_update(http: &Client, repo: &str) -> Result<String> {
    let status = check_github_release(http, repo).await;
    if let Some(err) = status.error {
        return Err(anyhow!(err));
    }
    if !status.update_available {
        return Err(anyhow!(
            "Already on latest version ({})",
            status.current_version
        ));
    }
    let asset_name = status
        .asset_name
        .ok_or_else(|| anyhow!("Unknown target asset"))?;
    let asset_url = status
        .asset_url
        .ok_or_else(|| anyhow!("Release is missing asset {asset_name}"))?;
    let checksum_url = status
        .checksum_url
        .ok_or_else(|| anyhow!("Release is missing SHA256SUMS.txt (refusing unverified binary)"))?;

    if !is_trusted_github_asset_url(&asset_url) || !is_trusted_github_asset_url(&checksum_url) {
        return Err(anyhow!("Refusing untrusted non-GitHub release asset URL"));
    }

    let checksum_text = http
        .get(&checksum_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let bytes = http
        .get(&asset_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    verify_sha256_manifest(&checksum_text, &asset_name, &bytes)?;

    let current_exe = std::env::current_exe().context("cannot determine current executable path")?;
    let parent = current_exe
        .parent()
        .ok_or_else(|| anyhow!("invalid executable directory"))?;
    let staged = parent.join(format!("{asset_name}.staged"));
    let backup = PathBuf::from(format!("{}.old", current_exe.display()));

    fs::write(&staged, &bytes).context("failed writing staged update binary")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
    }

    if backup.exists() {
        let _ = fs::remove_file(&backup);
    }

    // Cross-platform atomic swap (works even on Windows where running .exe is locked for deletion,
    // because Windows allows renaming a running executable to .old).
    fs::rename(&current_exe, &backup).context("failed moving current executable to backup")?;
    if let Err(e) = fs::rename(&staged, &current_exe) {
        let _ = fs::rename(&backup, &current_exe);
        return Err(anyhow!("failed activating staged binary: {e}"));
    }

    #[cfg(unix)]
    {
        let _ = fs::remove_file(&backup);
    }

    Ok(status.latest_version.unwrap_or_else(|| "latest".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_manifest_verification_works() {
        let data = b"polydesk-test-binary";
        let hash = hex::encode(Sha256::digest(data));
        let manifest = format!("{hash}  polydesk-linux-x86_64\n0000  other-file\n");
        assert!(verify_sha256_manifest(&manifest, "polydesk-linux-x86_64", data).is_ok());
        assert!(verify_sha256_manifest(&manifest, "polydesk-linux-x86_64", b"tampered").is_err());
        assert!(verify_sha256_manifest(&manifest, "missing-bin", data).is_err());
        assert!(is_valid_github_repo("Mluo2011/polydesk"));
        assert!(!is_valid_github_repo("../evil/repo"));
        assert!(!is_valid_github_repo("owner/repo?foo=bar"));
        assert!(is_trusted_github_asset_url("https://github.com/Mluo2011/polydesk/releases/download/v0.2.5/polydesk-linux-x86_64"));
        assert!(!is_trusted_github_asset_url("http://github.com/Mluo2011/polydesk/releases/download/v0.2.5/bin"));
        assert!(!is_trusted_github_asset_url("https://evil.example/bin"));
    }
}
