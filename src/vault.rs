use std::fs;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
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

#[derive(Clone, Serialize, Deserialize)]
pub struct VaultSecret {
    pub private_key: String,
    pub funder: Option<String>,
    pub signature_type: WalletSignatureType,
    pub label: Option<String>,
}

impl Drop for VaultSecret {
    fn drop(&mut self) {
        self.private_key.zeroize();
    }
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
    rand::thread_rng().fill_bytes(&mut salt);
    rand::thread_rng().fill_bytes(&mut nonce_bytes);

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
            funder: Some("0x2222222222222222222222222222222222222222".into()),
            signature_type: WalletSignatureType::GnosisSafe,
            label: Some("test".into()),
        };

        save_encrypted_vault(&path, "super-secret-pass", &secret, "0x1234...5678").unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("1111111111111111"));

        let unlocked = load_encrypted_vault(&path, "super-secret-pass").unwrap();
        assert_eq!(unlocked.private_key, secret.private_key);
        assert_eq!(unlocked.signature_type, WalletSignatureType::GnosisSafe);

        assert!(load_encrypted_vault(&path, "wrong-password").is_err());

        let _ = fs::remove_dir_all(&dir);
    }
}
