use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use alloy::signers::local::coins_bip39::{English, Mnemonic};
use alloy::signers::local::{MnemonicBuilder, PrivateKeySigner};
use anyhow::{Context, Result, anyhow};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::config::AppConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalletSignatureType {
    Eoa,
    Proxy,
    GnosisSafe,
    Poly1271,
}

impl Default for WalletSignatureType {
    fn default() -> Self {
        Self::Eoa
    }
}

/// Newly generated (or user-revealed) local BIP-39 wallet details.
/// Generated 100% locally via OS CSPRNG; never transmitted to any server.
#[derive(Clone, Serialize, Deserialize)]
pub struct GeneratedWallet {
    pub address: String,
    pub private_key: String,
    pub mnemonic: Option<String>,
}

impl std::fmt::Debug for GeneratedWallet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeneratedWallet")
            .field("address", &self.address)
            .field("private_key", &"[REDACTED]")
            .field("mnemonic", &self.mnemonic.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl Drop for GeneratedWallet {
    fn drop(&mut self) {
        self.private_key.zeroize();
        if let Some(ref mut m) = self.mnemonic {
            m.zeroize();
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct VaultSecret {
    pub private_key: String,
    #[serde(default)]
    pub mnemonic: Option<String>,
    pub funder: Option<String>,
    pub signature_type: WalletSignatureType,
    pub label: Option<String>,
}

impl Drop for VaultSecret {
    fn drop(&mut self) {
        self.private_key.zeroize();
        if let Some(ref mut m) = self.mnemonic {
            m.zeroize();
        }
    }
}

/// Generate a brand-new random local Ethereum/Polygon wallet on-device using OS CSPRNG:
/// - Standard 12-word BIP-39 English Secret Recovery Phrase
/// - Standard MetaMask/Rabby BIP-44 derivation (`m/44'/60'/0'/0/0`)
/// - 66-character `0x...` hex private key & `0x...` Polygon address
pub fn generate_local_wallet() -> Result<GeneratedWallet> {
    let mut rng = rand::rngs::OsRng;
    let mnemonic = Mnemonic::<English>::new_with_count(&mut rng, 12)
        .map_err(|e| anyhow!("Failed generating 12-word BIP-39 mnemonic: {e}"))?;
    let phrase = mnemonic.to_phrase();
    let signer = MnemonicBuilder::<English>::default()
        .phrase(phrase.as_str())
        .build()
        .map_err(|e| anyhow!("Failed deriving wallet from mnemonic: {e}"))?;
    let address = format!("{:#x}", signer.address());
    let private_key = format!("0x{}", hex::encode(signer.to_bytes()));
    Ok(GeneratedWallet {
        address,
        private_key,
        mnemonic: Some(phrase),
    })
}

/// Parse user input as EITHER a `0x...` hex private key OR a 12/24-word BIP-39 secret phrase.
/// Returns `(normalized_0x_private_key, optional_mnemonic_phrase)`.
pub fn parse_key_or_mnemonic(raw: &str) -> Result<(String, Option<String>)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("Enter a private key (0x...) or a 12/24-word secret phrase"));
    }
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.len() >= 12 {
        let normalized_phrase = words.join(" ");
        let signer = MnemonicBuilder::<English>::default()
            .phrase(normalized_phrase.as_str())
            .build()
            .map_err(|e| anyhow!("Invalid BIP-39 secret recovery phrase: {e}"))?;
        let pk = format!("0x{}", hex::encode(signer.to_bytes()));
        return Ok((pk, Some(normalized_phrase)));
    }
    let signer = PrivateKeySigner::from_str(trimmed)
        .context("Invalid Polygon private key hex (expected 64 hex chars / 0x..., or a 12-word secret phrase)")?;
    let pk = format!("0x{}", hex::encode(signer.to_bytes()));
    Ok((pk, None))
}

pub const DEFAULT_VAULT_PROFILE: &str = "default";
pub const MIN_NEW_VAULT_PASSWORD_LEN: usize = 12;
const VAULT_VERSION_V1: u8 = 1;
const VAULT_VERSION_V2: u8 = 2;
const V2_ARGON2_M_COST_KIB: u32 = 64 * 1024;
const V2_ARGON2_T_COST: u32 = 3;
const V2_ARGON2_P_COST: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct VaultKdfParams {
    m_cost_kib: u32,
    t_cost: u32,
    p_cost: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncryptedVaultFile {
    version: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kdf: Option<VaultKdfParams>,
    salt_b64: String,
    nonce_b64: String,
    ciphertext_b64: String,
    address_hint: String,
    signature_type: WalletSignatureType,
    funder: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultMetadata {
    pub exists: bool,
    pub address_hint: Option<String>,
    pub signature_type: Option<WalletSignatureType>,
    pub funder: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultProfileEntry {
    pub name: String,
    pub metadata: VaultMetadata,
}

const WINDOWS_RESERVED_PROFILE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

pub fn validate_profile_name(name: &str) -> Result<String> {
    if name.is_empty() || name.len() > 32 {
        return Err(anyhow!(
            "Vault profile name must be 1-32 characters (letters, digits, '_' or '-')"
        ));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(anyhow!(
            "Vault profile name may only contain letters, digits, '_' and '-'"
        ));
    }
    let lower = name.to_ascii_lowercase();
    if WINDOWS_RESERVED_PROFILE_NAMES.contains(&lower.as_str()) {
        return Err(anyhow!(
            "Vault profile name '{name}' is a reserved system device name"
        ));
    }
    Ok(lower)
}

fn has_case_insensitive_profile(
    base: &Path,
    target_lower: &str,
    ignore_stem_lower: Option<&str>,
) -> bool {
    let Ok(entries) = fs::read_dir(vaults_dir(base)) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let lower = stem.to_ascii_lowercase();
        if ignore_stem_lower.is_some_and(|ign| ign == lower) {
            continue;
        }
        if lower == target_lower {
            return true;
        }
    }
    false
}

fn find_case_insensitive_profile_paths(base: &Path, target_lower: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(vaults_dir(base)) else {
        return Vec::new();
    };
    let mut matches = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem.to_ascii_lowercase() == target_lower {
            matches.push(path);
        }
    }
    matches
}

pub fn vaults_dir(base: &Path) -> PathBuf {
    base.join("vaults")
}

/// Resolves the canonical lowercase vault path for `profile`, automatically renaming
/// a single pre-existing mixed-case file (e.g. `Alpha.json` -> `alpha.json` on Linux)
/// and failing cleanly if conflicting case variants both exist.
pub fn vault_path_for_profile(base: &Path, profile: &str) -> Result<PathBuf> {
    let safe = validate_profile_name(profile)?;
    let vdir = vaults_dir(base);
    let canonical = vdir.join(format!("{safe}.json"));
    let matches = find_case_insensitive_profile_paths(base, &safe);
    if matches.len() > 1 {
        return Err(anyhow!(
            "Conflicting vault profile files for '{safe}' in {} — resolve duplicate case variants before unlocking",
            vdir.display()
        ));
    }
    if let Some(existing) = matches.first()
        && existing != &canonical
    {
        fs::rename(existing, &canonical).with_context(|| {
            format!(
                "failed normalizing legacy mixed-case vault profile from {} to {}",
                existing.display(),
                canonical.display()
            )
        })?;
        #[cfg(unix)]
        if let Ok(dir) = fs::File::open(&vdir) {
            let _ = dir.sync_all();
        }
    }
    Ok(canonical)
}

fn ensure_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Non-destructively migrates `<base>/vault.json` into `<base>/vaults/default.json`
/// when `<base>/vaults/` does not yet exist, staging in `vaults.migrating` first.
pub fn migrate_legacy_single_vault(base: &Path) -> Result<bool> {
    let vdir = vaults_dir(base);
    let legacy = base.join("vault.json");
    if vdir.exists() || !legacy.is_file() {
        return Ok(false);
    }
    let staging_dir = base.join("vaults.migrating");
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir).with_context(|| {
            format!(
                "failed removing stale vault staging dir at {}",
                staging_dir.display()
            )
        })?;
    }

    let stage_res: Result<()> = (|| {
        ensure_private_dir(&staging_dir)?;
        let dst = staging_dir.join(format!("{DEFAULT_VAULT_PROFILE}.json"));
        let bytes = fs::read(&legacy)
            .with_context(|| format!("failed reading legacy vault at {}", legacy.display()))?;
        crate::config::write_private_atomic(&dst, &bytes)
            .with_context(|| format!("failed migrating legacy vault to {}", dst.display()))?;
        fs::rename(&staging_dir, &vdir).with_context(|| {
            format!(
                "failed promoting migrated vaults dir from {} to {}",
                staging_dir.display(),
                vdir.display()
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

pub fn list_vault_profiles(base: &Path) -> Vec<VaultProfileEntry> {
    let vdir = vaults_dir(base);
    let Ok(entries) = fs::read_dir(&vdir) else {
        return Vec::new();
    };
    let mut stems = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Ok(canon) = validate_profile_name(stem) {
            stems.push((canon, path));
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (canon, orig_path) in stems {
        if !seen.insert(canon.clone()) {
            continue;
        }
        let resolved_path = vault_path_for_profile(base, &canon).unwrap_or(orig_path);
        out.push(VaultProfileEntry {
            name: canon,
            metadata: vault_metadata(&resolved_path),
        });
    }
    out.sort_by(|a, b| {
        let a_def = a.name == DEFAULT_VAULT_PROFILE;
        let b_def = b.name == DEFAULT_VAULT_PROFILE;
        b_def.cmp(&a_def).then_with(|| a.name.cmp(&b.name))
    });
    out
}

pub fn rename_vault_profile(base: &Path, old_name: &str, new_name: &str) -> Result<()> {
    let old_safe = validate_profile_name(old_name)?;
    let new_safe = validate_profile_name(new_name)?;
    if old_safe == new_safe {
        return Ok(());
    }
    let old_path = vault_path_for_profile(base, &old_safe)?;
    let new_path = vault_path_for_profile(base, &new_safe)?;
    if !old_path.exists() {
        return Err(anyhow!("Vault profile '{old_safe}' does not exist"));
    }
    if new_path.exists() || has_case_insensitive_profile(base, &new_safe, Some(&old_safe)) {
        return Err(anyhow!("Vault profile '{new_safe}' already exists"));
    }
    ensure_private_dir(&vaults_dir(base))?;
    fs::rename(&old_path, &new_path).with_context(|| {
        format!(
            "failed renaming vault profile from {} to {}",
            old_path.display(),
            new_path.display()
        )
    })?;
    #[cfg(unix)]
    if let Ok(dir) = fs::File::open(vaults_dir(base)) {
        let _ = dir.sync_all();
    }
    Ok(())
}

pub fn delete_vault_profile(base: &Path, profile: &str) -> Result<()> {
    let path = vault_path_for_profile(base, profile)?;
    delete_vault(&path)
}

pub fn default_vault_path() -> PathBuf {
    AppConfig::config_dir().join("vault.json")
}

pub fn default_vault_path_in(base: &Path) -> PathBuf {
    AppConfig::config_dir_in(base).join("vault.json")
}

fn derive_key_v1(password: &str, salt: &[u8]) -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow!("argon2 key derivation failed: {e}"))?;
    Ok(key)
}

fn derive_key_v2(
    password: &str,
    salt: &[u8],
    m_cost_kib: u32,
    t_cost: u32,
    p_cost: u32,
) -> Result<[u8; 32]> {
    if !(8 * 1024..=512 * 1024).contains(&m_cost_kib)
        || !(1..=20).contains(&t_cost)
        || !(1..=8).contains(&p_cost)
    {
        return Err(anyhow!("Corrupted vault file (invalid KDF parameters)"));
    }
    let params = Params::new(m_cost_kib, t_cost, p_cost, Some(32))
        .map_err(|e| anyhow!("invalid argon2 params: {e}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow!("argon2id key derivation failed: {e}"))?;
    Ok(key)
}

pub fn vault_metadata(path: &Path) -> VaultMetadata {
    if !path.exists() {
        return VaultMetadata {
            exists: false,
            address_hint: None,
            signature_type: None,
            funder: None,
        };
    }
    match fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<EncryptedVaultFile>(&s).ok())
    {
        Some(f) => VaultMetadata {
            exists: true,
            address_hint: Some(f.address_hint),
            signature_type: Some(f.signature_type),
            funder: f.funder,
        },
        None => VaultMetadata {
            exists: true,
            address_hint: None,
            signature_type: None,
            funder: None,
        },
    }
}

pub fn save_encrypted_vault(
    path: &Path,
    password: &str,
    secret: &VaultSecret,
    address_hint: &str,
) -> Result<()> {
    if password.chars().count() < MIN_NEW_VAULT_PASSWORD_LEN {
        return Err(anyhow!(
            "Vault passphrase must be at least {MIN_NEW_VAULT_PASSWORD_LEN} characters"
        ));
    }
    if let Some(parent) = path.parent() {
        if parent.file_name().and_then(|s| s.to_str()) == Some("vaults") {
            ensure_private_dir(parent)?;
        } else {
            fs::create_dir_all(parent)?;
        }
    }

    let mut salt = [0u8; 16];
    let mut nonce_bytes = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);

    let kdf = VaultKdfParams {
        m_cost_kib: V2_ARGON2_M_COST_KIB,
        t_cost: V2_ARGON2_T_COST,
        p_cost: V2_ARGON2_P_COST,
    };
    let mut key = derive_key_v2(password, &salt, kdf.m_cost_kib, kdf.t_cost, kdf.p_cost)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| anyhow!("cipher init failed: {e}"))?;
    key.zeroize();

    let mut plaintext = serde_json::to_vec(secret)?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_ref())
        .map_err(|e| anyhow!("vault encryption failed: {e}"))?;
    plaintext.zeroize();

    let payload = EncryptedVaultFile {
        version: VAULT_VERSION_V2,
        kdf: Some(kdf),
        salt_b64: B64.encode(salt),
        nonce_b64: B64.encode(nonce_bytes),
        ciphertext_b64: B64.encode(ciphertext),
        address_hint: address_hint.to_string(),
        signature_type: secret.signature_type,
        funder: secret.funder.clone(),
    };

    let serialized = serde_json::to_string_pretty(&payload)?;
    crate::config::write_private_atomic(path, serialized.as_bytes())
        .with_context(|| format!("failed saving encrypted vault at {}", path.display()))
}

pub fn load_encrypted_vault(path: &Path, password: &str) -> Result<VaultSecret> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("vault not found at {}", path.display()))?;
    let file: EncryptedVaultFile = serde_json::from_str(&raw)?;

    let salt = B64.decode(&file.salt_b64)?;
    let nonce_bytes = B64.decode(&file.nonce_b64)?;
    let ciphertext = B64.decode(&file.ciphertext_b64)?;

    if salt.len() < 8 {
        return Err(anyhow!("Corrupted vault file (invalid salt length)"));
    }
    if nonce_bytes.len() != 12 {
        return Err(anyhow!("Corrupted vault file (invalid AES-GCM nonce length)"));
    }

    let mut key = match (file.version, file.kdf) {
        (VAULT_VERSION_V1, None) => derive_key_v1(password, &salt)?,
        (VAULT_VERSION_V2, Some(kdf)) => {
            derive_key_v2(password, &salt, kdf.m_cost_kib, kdf.t_cost, kdf.p_cost)?
        }
        (VAULT_VERSION_V2, None) => derive_key_v2(
            password,
            &salt,
            V2_ARGON2_M_COST_KIB,
            V2_ARGON2_T_COST,
            V2_ARGON2_P_COST,
        )?,
        _ => return Err(anyhow!("Unsupported vault version ({})", file.version)),
    };
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| anyhow!("cipher init failed: {e}"))?;
    key.zeroize();

    let mut plaintext = cipher
        .decrypt(Nonce::from_slice(&nonce_bytes), ciphertext.as_ref())
        .map_err(|_| anyhow!("Invalid vault passphrase or corrupted vault file"))?;

    let secret: VaultSecret = serde_json::from_slice(&plaintext)?;
    plaintext.zeroize();
    Ok(secret)
}

pub fn delete_vault(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_roundtrip_and_wrong_password() {
        let dir = std::env::temp_dir().join(format!("polydesk-vault-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vault.json");

        let secret = VaultSecret {
            private_key: "0x1111111111111111111111111111111111111111111111111111111111111111".into(),
            mnemonic: Some("test phrase words".into()),
            funder: Some("0x2222222222222222222222222222222222222222".into()),
            signature_type: WalletSignatureType::GnosisSafe,
            label: Some("test".into()),
        };

        save_encrypted_vault(&path, "super-secret-pass", &secret, "0x1234...5678").unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("1111111111111111"));
        assert!(!raw.contains("test phrase words"));

        let unlocked = load_encrypted_vault(&path, "super-secret-pass").unwrap();
        assert_eq!(unlocked.private_key, secret.private_key);
        assert_eq!(unlocked.mnemonic, secret.mnemonic);
        assert_eq!(unlocked.signature_type, WalletSignatureType::GnosisSafe);

        assert!(load_encrypted_vault(&path, "wrong-password").is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn generate_wallet_and_rederive_from_mnemonic() {
        let w = generate_local_wallet().unwrap();
        assert!(w.address.starts_with("0x") && w.address.len() == 42);
        assert!(w.private_key.starts_with("0x") && w.private_key.len() == 66);
        let phrase = w.mnemonic.as_deref().unwrap();
        assert_eq!(phrase.split_whitespace().count(), 12);

        let (pk_from_phrase, m) = parse_key_or_mnemonic(phrase).unwrap();
        assert_eq!(pk_from_phrase, w.private_key);
        assert_eq!(m.as_deref(), Some(phrase));

        let (pk_direct, m2) = parse_key_or_mnemonic(&w.private_key).unwrap();
        assert_eq!(pk_direct, w.private_key);
        assert!(m2.is_none());

        // Verify Debug formatting NEVER leaks private key or mnemonic
        let dbg = format!("{w:?}");
        assert!(!dbg.contains(&w.private_key));
        assert!(!dbg.contains(phrase));
        assert!(dbg.contains("[REDACTED]"));

        // Verify true CSPRNG uniqueness across multiple generated wallets
        let mut addrs = std::collections::HashSet::new();
        let mut phrases = std::collections::HashSet::new();
        for _ in 0..5 {
            let g = generate_local_wallet().unwrap();
            addrs.insert(g.address.clone());
            phrases.insert(g.mnemonic.clone().unwrap());
        }
        assert_eq!(addrs.len(), 5);
        assert_eq!(phrases.len(), 5);
    }

    #[test]
    fn corrupted_nonce_returns_error_not_panic() {
        let dir = std::env::temp_dir().join(format!("polydesk-vault-bad-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vault.json");
        fs::write(
            &path,
            r#"{"version":1,"salt_b64":"AAAAAAAAAAAAAAAAAAAAAA==","nonce_b64":"AAAA","ciphertext_b64":"AAAA","address_hint":"0x1","signature_type":"eoa","funder":null}"#,
        )
        .unwrap();
        assert!(load_encrypted_vault(&path, "password123").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_short_password_for_new_vault() {
        let dir = std::env::temp_dir().join(format!("polydesk-vault-short-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vault.json");
        let secret = VaultSecret {
            private_key: "0x1111111111111111111111111111111111111111111111111111111111111111".into(),
            mnemonic: None,
            funder: None,
            signature_type: WalletSignatureType::Eoa,
            label: None,
        };
        assert!(save_encrypted_vault(&path, "short1", &secret, "0x1").is_err());
        assert!(save_encrypted_vault(&path, "11charslong", &secret, "0x1").is_err());
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_save_creates_v2_and_permissions() {
        let dir = std::env::temp_dir().join(format!("polydesk-vault-v2-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vault.json");
        let secret = VaultSecret {
            private_key: "0x1111111111111111111111111111111111111111111111111111111111111111".into(),
            mnemonic: Some("legacy words".into()),
            funder: None,
            signature_type: WalletSignatureType::Eoa,
            label: Some("v2".into()),
        };

        save_encrypted_vault(&path, "twelve-chars", &secret, "0x1234...abcd").unwrap();
        assert!(!dir.join("vault.json.tmp").exists());
        let raw = fs::read_to_string(&path).unwrap();
        let file: EncryptedVaultFile = serde_json::from_str(&raw).unwrap();
        assert_eq!(file.version, VAULT_VERSION_V2);
        let kdf = file.kdf.unwrap();
        assert_eq!(kdf.m_cost_kib, 64 * 1024);
        assert_eq!(kdf.t_cost, 3);
        assert_eq!(kdf.p_cost, 1);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        let unlocked = load_encrypted_vault(&path, "twelve-chars").unwrap();
        assert_eq!(unlocked.private_key, secret.private_key);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_v1_short_password_vault_remains_unlockable() {
        let dir = std::env::temp_dir().join(format!("polydesk-vault-v1-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vault.json");

        let secret = VaultSecret {
            private_key: "0x2222222222222222222222222222222222222222222222222222222222222222".into(),
            mnemonic: Some("alpha beta gamma".into()),
            funder: None,
            signature_type: WalletSignatureType::Eoa,
            label: Some("legacy".into()),
        };

        let salt = [7u8; 16];
        let nonce = [9u8; 12];
        let mut key = derive_key_v1("abc123", &salt).unwrap();
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        key.zeroize();
        let plaintext = serde_json::to_vec(&secret).unwrap();
        let ciphertext = cipher.encrypt(Nonce::from_slice(&nonce), plaintext.as_ref()).unwrap();

        let legacy_file = EncryptedVaultFile {
            version: VAULT_VERSION_V1,
            kdf: None,
            salt_b64: B64.encode(salt),
            nonce_b64: B64.encode(nonce),
            ciphertext_b64: B64.encode(ciphertext),
            address_hint: "0x2222...2222".into(),
            signature_type: WalletSignatureType::Eoa,
            funder: None,
        };
        fs::write(&path, serde_json::to_string_pretty(&legacy_file).unwrap()).unwrap();

        let unlocked = load_encrypted_vault(&path, "abc123").unwrap();
        assert_eq!(unlocked.private_key, secret.private_key);
        assert_eq!(unlocked.mnemonic, secret.mnemonic);
        assert!(load_encrypted_vault(&path, "wrong6").is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_polydesk_vault_migrates_to_polydirect_and_unlocks() {
        let base = std::env::temp_dir().join(format!("polydirect-vault-mig-{}", uuid::Uuid::new_v4()));
        let legacy_dir = base.join("polydesk");
        fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_path = legacy_dir.join("vault.json");

        let secret = VaultSecret {
            private_key: "0x3333333333333333333333333333333333333333333333333333333333333333".into(),
            mnemonic: Some("legacy phrase words".into()),
            funder: None,
            signature_type: WalletSignatureType::Proxy,
            label: Some("old".into()),
        };

        let salt = [11u8; 16];
        let nonce = [13u8; 12];
        let mut key = derive_key_v1("oldpw6", &salt).unwrap();
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        key.zeroize();
        let plaintext = serde_json::to_vec(&secret).unwrap();
        let ciphertext = cipher.encrypt(Nonce::from_slice(&nonce), plaintext.as_ref()).unwrap();

        let legacy_file = EncryptedVaultFile {
            version: VAULT_VERSION_V1,
            kdf: None,
            salt_b64: B64.encode(salt),
            nonce_b64: B64.encode(nonce),
            ciphertext_b64: B64.encode(ciphertext),
            address_hint: "0x3333...3333".into(),
            signature_type: WalletSignatureType::Proxy,
            funder: None,
        };
        fs::write(&legacy_path, serde_json::to_string_pretty(&legacy_file).unwrap()).unwrap();

        let migrated_vault_path = default_vault_path_in(&base);
        assert_eq!(migrated_vault_path, base.join("polydirect").join("vault.json"));
        assert!(migrated_vault_path.exists());

        let unlocked = load_encrypted_vault(&migrated_vault_path, "oldpw6").unwrap();
        assert_eq!(unlocked.private_key, secret.private_key);
        assert_eq!(unlocked.mnemonic, secret.mnemonic);
        assert_eq!(unlocked.signature_type, WalletSignatureType::Proxy);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn profile_name_validation_rejects_traversal_and_invalid_chars() {
        assert_eq!(validate_profile_name("default").unwrap(), "default");
        assert_eq!(validate_profile_name("scalp_01-ARB").unwrap(), "scalp_01-arb");
        assert_eq!(validate_profile_name("Alpha").unwrap(), "alpha");
        assert_eq!(
            validate_profile_name(&"a".repeat(32)).unwrap(),
            "a".repeat(32)
        );

        for bad in [
            "",
            "../evil",
            "..",
            ".",
            "a/b",
            "a\\b",
            "has space",
            "bad.name",
            "bad$name",
            "CON",
            "con",
            "PRN",
            "AUX",
            "nul",
            "NUL",
            "COM1",
            "com9",
            "LPT1",
            "lpt9",
            &"a".repeat(33),
        ] {
            assert!(
                validate_profile_name(bad).is_err(),
                "expected invalid profile name: {bad:?}"
            );
            assert!(
                vault_path_for_profile(Path::new("/tmp"), bad).is_err(),
                "expected path resolution rejection for {bad:?}"
            );
        }
    }

    #[test]
    fn case_insensitive_profile_collision_and_staged_vault_migration_cleanup() {
        let dir = std::env::temp_dir().join(format!(
            "polydirect-profile-edge-test-{}",
            uuid::Uuid::new_v4()
        ));
        let vdir = vaults_dir(&dir);
        fs::create_dir_all(&vdir).unwrap();

        let secret = VaultSecret {
            private_key: "0x4444444444444444444444444444444444444444444444444444444444444444".into(),
            mnemonic: None,
            funder: None,
            signature_type: WalletSignatureType::Eoa,
            label: None,
        };
        // Simulate pre-existing mixed-case profile file on disk + another profile "beta"
        save_encrypted_vault(&vdir.join("Alpha.json"), "strong-passphrase-12", &secret, "0x4444...4444").unwrap();
        let beta_path = vault_path_for_profile(&dir, "beta").unwrap();
        save_encrypted_vault(&beta_path, "strong-passphrase-12", &secret, "0x4444...4444").unwrap();

        // Attempting to rename "beta" -> "alpha" (or "ALPHA") must reject case-insensitive duplicate
        assert!(rename_vault_profile(&dir, "beta", "alpha").is_err());
        assert!(rename_vault_profile(&dir, "beta", "ALPHA").is_err());

        // Staged vault migration cleans up leftover `vaults.migrating`
        let mig_dir = std::env::temp_dir().join(format!(
            "polydirect-vault-stage-test-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&mig_dir).unwrap();
        fs::write(mig_dir.join("vault.json"), "{\"version\":1}").unwrap();
        // Pre-seed a stale `vaults.migrating` file/dir; verify migration clears & completes cleanly
        let stale = mig_dir.join("vaults.migrating");
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("leftover.json"), "bad").unwrap();
        assert!(migrate_legacy_single_vault(&mig_dir).unwrap());
        assert!(!stale.exists());
        assert!(mig_dir.join("vaults").join("default.json").exists());

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&mig_dir);
    }

    #[test]
    fn legacy_single_vault_migrates_to_default_profile_and_unlocks() {
        let dir = std::env::temp_dir().join(format!(
            "polydirect-profile-mig-test-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        let legacy_path = dir.join("vault.json");

        let secret = VaultSecret {
            private_key: "0x4444444444444444444444444444444444444444444444444444444444444444".into(),
            mnemonic: Some("legacy single vault phrase".into()),
            funder: None,
            signature_type: WalletSignatureType::Eoa,
            label: Some("legacy-main".into()),
        };

        let salt = [17u8; 16];
        let nonce = [19u8; 12];
        let mut key = derive_key_v1("oldpw6", &salt).unwrap();
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        key.zeroize();
        let plaintext = serde_json::to_vec(&secret).unwrap();
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_ref())
            .unwrap();

        let legacy_file = EncryptedVaultFile {
            version: VAULT_VERSION_V1,
            kdf: None,
            salt_b64: B64.encode(salt),
            nonce_b64: B64.encode(nonce),
            ciphertext_b64: B64.encode(ciphertext),
            address_hint: "0x4444...4444".into(),
            signature_type: WalletSignatureType::Eoa,
            funder: None,
        };
        fs::write(&legacy_path, serde_json::to_string_pretty(&legacy_file).unwrap()).unwrap();

        assert!(migrate_legacy_single_vault(&dir).unwrap());
        // Non-destructive + idempotent
        assert!(legacy_path.exists());
        assert!(!migrate_legacy_single_vault(&dir).unwrap());

        let profiles = list_vault_profiles(&dir);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, DEFAULT_VAULT_PROFILE);

        let default_path = vault_path_for_profile(&dir, DEFAULT_VAULT_PROFILE).unwrap();
        let unlocked = load_encrypted_vault(&default_path, "oldpw6").unwrap();
        assert_eq!(unlocked.private_key, secret.private_key);
        assert_eq!(unlocked.mnemonic, secret.mnemonic);

        // Save a new v2 profile and test rename + delete
        let sec_path = vault_path_for_profile(&dir, "alpha").unwrap();
        save_encrypted_vault(&sec_path, "strong-passphrase-12", &secret, "0x4444...4444").unwrap();
        rename_vault_profile(&dir, "alpha", "beta").unwrap();
        let names: Vec<String> = list_vault_profiles(&dir)
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["default".to_string(), "beta".to_string()]);
        delete_vault_profile(&dir, "beta").unwrap();
        assert_eq!(list_vault_profiles(&dir).len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mixed_case_legacy_profile_normalizes_and_conflicts_fail_closed() {
        let dir = std::env::temp_dir().join(format!(
            "polydirect-mixed-case-prof-{}",
            uuid::Uuid::new_v4()
        ));
        let vdir = vaults_dir(&dir);
        fs::create_dir_all(&vdir).unwrap();

        let secret = VaultSecret {
            private_key: "0x5555555555555555555555555555555555555555555555555555555555555555".into(),
            mnemonic: None,
            funder: None,
            signature_type: WalletSignatureType::Eoa,
            label: Some("Alpha".into()),
        };

        // Simulate pre-upgrade mixed-case vault file on case-sensitive Linux
        let mixed_path = vdir.join("Alpha.json");
        save_encrypted_vault(&mixed_path, "strong-passphrase-12", &secret, "0x5555...5555").unwrap();

        let mut cfg = AppConfig::default();
        cfg.active_vault_profile = "Alpha".into();
        cfg.sanitize();
        assert_eq!(cfg.active_vault_profile, "alpha");

        // Accessing canonical lowercase profile automatically migrates Alpha.json -> alpha.json and unlocks cleanly
        let resolved = vault_path_for_profile(&dir, &cfg.active_vault_profile).unwrap();
        assert_eq!(resolved, vdir.join("alpha.json"));
        assert!(resolved.exists());
        let unlocked = load_encrypted_vault(&resolved, "strong-passphrase-12").unwrap();
        assert_eq!(unlocked.private_key, secret.private_key);

        // If two distinct case variants exist on disk on a case-sensitive filesystem, fail closed with conflict error
        #[cfg(target_os = "linux")]
        {
            fs::write(vdir.join("Beta.json"), "{}").unwrap();
            fs::write(vdir.join("beta.json"), "{}").unwrap();
            let conflict = vault_path_for_profile(&dir, "beta");
            assert!(conflict.is_err());
            assert!(conflict.unwrap_err().to_string().contains("Conflicting"));
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
