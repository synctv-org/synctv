//! Credential encryption service using AES-256-GCM
//!
//! Provides encryption and decryption for user provider credentials stored in the database.
//! Uses AES-256-GCM authenticated encryption to protect sensitive credential data at rest.

use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use std::sync::Arc;

use crate::{Error, InternalExt, Result};

/// AES-256-GCM nonce size (96 bits / 12 bytes)
const NONCE_SIZE: usize = 12;

/// Prefix for encrypted data to distinguish from plaintext
const ENCRYPTED_PREFIX: &str = "enc:";

/// Key version byte prepended to encrypted payloads for future key rotation support.
/// When rotating keys, increment this version and use it to select the correct
/// decryption key.
const KEY_VERSION: u8 = 0x01;

/// Credential encryption service
///
/// Encrypts and decrypts credential data using AES-256-GCM.
/// The encryption key should be loaded from a secure source (file, env var, KMS).
///
/// Uses `Arc` internally so cloning shares a single copy of the cipher
/// (and its key material) rather than duplicating the key in memory.
#[derive(Clone)]
pub struct CredentialEncryption {
    cipher: Arc<Aes256Gcm>,
}

impl std::fmt::Debug for CredentialEncryption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialEncryption")
            .field("cipher", &"[REDACTED]")
            .finish()
    }
}

impl CredentialEncryption {
    /// Create a new encryption service from a 32-byte key
    ///
    /// # Arguments
    /// * `key_bytes` - 32-byte encryption key (AES-256)
    ///
    /// # Errors
    /// Returns error if the key length is not exactly 32 bytes.
    pub fn new(key_bytes: &[u8]) -> Result<Self> {
        if key_bytes.len() != 32 {
            return Err(Error::Internal(format!(
                "Credential encryption key must be exactly 32 bytes, got {}",
                key_bytes.len()
            )));
        }
        let key = Key::<Aes256Gcm>::from_slice(key_bytes);
        let cipher = Aes256Gcm::new(key);
        Ok(Self {
            cipher: Arc::new(cipher),
        })
    }

    /// Create from a hex-encoded key string
    ///
    /// # Arguments
    /// * `hex_key` - 64-character hex string representing a 32-byte key
    pub fn from_hex_key(hex_key: &str) -> Result<Self> {
        let key_bytes = hex::decode(hex_key).internal_with_err("Invalid hex key")?;
        Self::new(&key_bytes)
    }

    /// Encrypt JSON credential data
    ///
    /// Returns a string in the format "enc:<base64(version + nonce + ciphertext)>"
    /// A version byte is prepended for future key rotation support.
    pub fn encrypt(&self, plaintext: &serde_json::Value) -> Result<String> {
        let plaintext_bytes = serde_json::to_vec(plaintext)
            .internal_with_err("Failed to serialize credential data")?;

        // Generate random nonce
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        // Encrypt
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext_bytes.as_ref())
            .internal_with_err("Credential encryption failed")?;

        // Prepend version byte + nonce to ciphertext and encode as base64
        let mut combined = Vec::with_capacity(1 + NONCE_SIZE + ciphertext.len());
        combined.push(KEY_VERSION);
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);

        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &combined);
        Ok(format!("{ENCRYPTED_PREFIX}{encoded}"))
    }

    /// Decrypt credential data
    ///
    /// Only accepts encrypted format: "enc:<base64(version + nonce + ciphertext)>"
    /// Plaintext credentials are no longer supported.
    pub fn decrypt(&self, stored: &str) -> Result<serde_json::Value> {
        let encoded = stored.strip_prefix(ENCRYPTED_PREFIX).ok_or_else(|| {
            Error::Internal(
                "Credential data is not encrypted (missing 'enc:' prefix). \
                 Plaintext credentials are no longer supported."
                    .to_string(),
            )
        })?;

        let combined = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
            .internal_with_err("Invalid base64 in encrypted credential")?;

        if combined.len() < 1 + NONCE_SIZE {
            return Err(Error::Internal(
                "Encrypted credential data too short".to_string(),
            ));
        }

        let version = combined[0];
        if version != KEY_VERSION {
            // Future: select decryption key based on version byte
            return Err(Error::Internal(format!(
                "Unsupported credential encryption version: {version} (expected {KEY_VERSION})"
            )));
        }

        let (nonce_bytes, ciphertext) = combined[1..].split_at(NONCE_SIZE);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = self.cipher.decrypt(nonce, ciphertext).map_err(|_| {
            Error::Internal(
                "Credential decryption failed (wrong key or corrupted data)".to_string(),
            )
        })?;

        serde_json::from_slice(&plaintext)
            .internal_with_err("Decrypted credential is not valid JSON")
    }

    /// Decrypt a JSON Value that must be an encrypted string
    ///
    /// The value must be a string starting with "enc:". Plaintext JSON
    /// objects/arrays are no longer supported.
    pub fn decrypt_value(&self, value: &serde_json::Value) -> Result<serde_json::Value> {
        match value {
            serde_json::Value::String(s) => self.decrypt(s),
            other => Err(Error::Internal(format!(
                "Expected encrypted string value (enc:...), got {}. \
                 Plaintext credentials are no longer supported.",
                other
            ))),
        }
    }

    /// Encrypt a JSON Value and return as a string Value for DB storage
    pub fn encrypt_to_value(&self, plaintext: &serde_json::Value) -> Result<serde_json::Value> {
        let encrypted = self.encrypt(plaintext)?;
        Ok(serde_json::Value::String(encrypted))
    }

    /// Check if a stored value is already encrypted
    #[must_use]
    pub fn is_encrypted(value: &serde_json::Value) -> bool {
        matches!(value, serde_json::Value::String(s) if s.starts_with(ENCRYPTED_PREFIX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_key() -> Vec<u8> {
        // 32 bytes for AES-256
        vec![
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ]
    }

    #[test]
    fn test_encrypt_decrypt_round_trip() {
        let enc = CredentialEncryption::new(&test_key()).unwrap();
        let original = json!({
            "type": "alist",
            "host": "https://alist.example.com",
            "username": "admin",
            "password": "secret_password"
        });

        let encrypted = enc.encrypt(&original).unwrap();
        assert!(encrypted.starts_with("enc:"));

        let decrypted = enc.decrypt(&encrypted).unwrap();
        assert_eq!(original, decrypted);
    }

    #[test]
    fn test_decrypt_plaintext_returns_error() {
        let enc = CredentialEncryption::new(&test_key()).unwrap();
        let plaintext = r#"{"type":"bilibili","cookies":{"SESSDATA":"test"}}"#;

        let result = enc.decrypt(plaintext);
        assert!(result.is_err(), "Plaintext credentials should be rejected");
    }

    #[test]
    fn test_decrypt_value_encrypted() {
        let enc = CredentialEncryption::new(&test_key()).unwrap();
        let original = json!({"api_key": "secret123"});

        let encrypted_value = enc.encrypt_to_value(&original).unwrap();
        assert!(CredentialEncryption::is_encrypted(&encrypted_value));

        let decrypted = enc.decrypt_value(&encrypted_value).unwrap();
        assert_eq!(original, decrypted);
    }

    #[test]
    fn test_decrypt_value_plaintext_returns_error() {
        let enc = CredentialEncryption::new(&test_key()).unwrap();
        let plaintext = json!({"cookies": {"SESSDATA": "test"}});

        // Plaintext JSON objects should be rejected
        let result = enc.decrypt_value(&plaintext);
        assert!(result.is_err(), "Plaintext JSON values should be rejected");
    }

    #[test]
    fn test_is_encrypted() {
        assert!(CredentialEncryption::is_encrypted(&json!("enc:AAAA")));
        assert!(!CredentialEncryption::is_encrypted(&json!("not encrypted")));
        assert!(!CredentialEncryption::is_encrypted(
            &json!({"key": "value"})
        ));
    }

    #[test]
    fn test_wrong_key_fails() {
        let enc1 = CredentialEncryption::new(&test_key()).unwrap();
        let original = json!({"secret": "data"});
        let encrypted = enc1.encrypt(&original).unwrap();

        let wrong_key = vec![0xffu8; 32];
        let enc2 = CredentialEncryption::new(&wrong_key).unwrap();

        let result = enc2.decrypt(&encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_key_length() {
        let result = CredentialEncryption::new(&[0u8; 16]);
        assert!(result.is_err());
    }

    #[test]
    fn test_from_hex_key() {
        let hex_key = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        let enc = CredentialEncryption::from_hex_key(hex_key).unwrap();
        let original = json!({"test": true});

        let encrypted = enc.encrypt(&original).unwrap();
        let decrypted = enc.decrypt(&encrypted).unwrap();
        assert_eq!(original, decrypted);
    }

    #[test]
    fn test_each_encryption_produces_different_ciphertext() {
        let enc = CredentialEncryption::new(&test_key()).unwrap();
        let original = json!({"same": "data"});

        let encrypted1 = enc.encrypt(&original).unwrap();
        let encrypted2 = enc.encrypt(&original).unwrap();

        // Different nonces produce different ciphertext
        assert_ne!(encrypted1, encrypted2);

        // Both decrypt to the same value
        assert_eq!(enc.decrypt(&encrypted1).unwrap(), original);
        assert_eq!(enc.decrypt(&encrypted2).unwrap(), original);
    }
}
