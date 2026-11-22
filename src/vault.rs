use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use alloy::signers::local::coins_bip39::{English, Mnemonic};
use alloy::signers::local::{MnemonicBuilder, PrivateKeySigner};
use anyhow::{Context, Result, anyhow};
use argon2::Argon2;
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedWallet {
    pub address: String,
    pub private_key: String,
    pub mnemonic: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncryptedVaultFile {
    version: u8,
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

pub fn default_vault_path() -> PathBuf {
    AppConfig::config_dir().join("vault.json")
}

fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow!("argon2 key derivation failed: {e}"))?;
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
    if password.len() < 6 {
        return Err(anyhow!("Vault passphrase must be at least 6 characters"));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut salt = [0u8; 16];
    let mut nonce_bytes = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);

    let mut key = derive_key(password, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| anyhow!("cipher init failed: {e}"))?;
    key.zeroize();

    let mut plaintext = serde_json::to_vec(secret)?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_ref())
        .map_err(|e| anyhow!("vault encryption failed: {e}"))?;
    plaintext.zeroize();

    let payload = EncryptedVaultFile {
        version: 1,
        salt_b64: B64.encode(salt),
        nonce_b64: B64.encode(nonce_bytes),
        ciphertext_b64: B64.encode(ciphertext),
        address_hint: address_hint.to_string(),
        signature_type: secret.signature_type,
        funder: secret.funder.clone(),
    };

    let serialized = serde_json::to_string_pretty(&payload)?;
    fs::write(path, serialized)
        .with_context(|| format!("failed writing vault at {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

pub fn load_encrypted_vault(path: &Path, password: &str) -> Result<VaultSecret> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("vault not found at {}", path.display()))?;
    let file: EncryptedVaultFile = serde_json::from_str(&raw)?;

    let salt = B64.decode(&file.salt_b64)?;
    let nonce_bytes = B64.decode(&file.nonce_b64)?;
    let ciphertext = B64.decode(&file.ciphertext_b64)?;

    let mut key = derive_key(password, &salt)?;
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

        // Verify true CSPRNG uniqueness across multiple generated wallets
        let mut addrs = std::collections::HashSet::new();
        let mut phrases = std::collections::HashSet::new();
        for _ in 0..5 {
            let g = generate_local_wallet().unwrap();
            addrs.insert(g.address);
            phrases.insert(g.mnemonic.unwrap());
        }
        assert_eq!(addrs.len(), 5);
        assert_eq!(phrases.len(), 5);
    }
}
