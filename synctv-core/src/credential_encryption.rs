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
    /// Returns a string in the format "enc:<base64(nonce + ciphertext)>"
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

        let mut combined = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);

        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &combined);
        Ok(format!("{ENCRYPTED_PREFIX}{encoded}"))
    }

    pub fn decrypt(&self, stored: &str) -> Result<serde_json::Value> {
        let encoded = stored.strip_prefix(ENCRYPTED_PREFIX).ok_or_else(|| {
            Error::Internal(
                "Credential data must be an encrypted string with 'enc:' prefix.".to_string(),
            )
        })?;

        let combined = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
            .internal_with_err("Invalid base64 in encrypted credential")?;

        if combined.len() < NONCE_SIZE {
            return Err(Error::Internal(
                "Encrypted credential data too short".to_string(),
            ));
        }

        let (nonce_bytes, ciphertext) = combined.split_at(NONCE_SIZE);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = self.cipher.decrypt(nonce, ciphertext).map_err(|_| {
            Error::Internal(
                "Credential decryption failed (wrong key or corrupted data)".to_string(),
            )
        })?;

        serde_json::from_slice(&plaintext)
            .internal_with_err("Decrypted credential is not valid JSON")
    }

    pub fn decrypt_value(&self, value: &serde_json::Value) -> Result<serde_json::Value> {
        match value {
            serde_json::Value::String(s) => self.decrypt(s),
            other => Err(Error::Internal(format!(
                "Credential value must be an encrypted string with 'enc:' prefix, got {other}."
            ))),
        }
    }

    /// Encrypt a JSON Value and return as a string Value for DB storage
    pub fn encrypt_to_value(&self, plaintext: &serde_json::Value) -> Result<serde_json::Value> {
        let encrypted = self.encrypt(plaintext)?;
        Ok(serde_json::Value::String(encrypted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ok<T>(result: Result<T>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn test_key() -> Vec<u8> {
        vec![
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ]
    }

    #[test]
    fn test_encrypt_decrypt_round_trip() {
        let enc = ok(
            CredentialEncryption::new(&test_key()),
            "encryption should build",
        );
        let original = json!({
            "type": "alist",
            "host": "https://alist.example.com",
            "username": "admin",
            "password": "secret_password"
        });

        let encrypted = ok(enc.encrypt(&original), "credential should encrypt");
        assert!(encrypted.starts_with("enc:"));

        let decrypted = ok(enc.decrypt(&encrypted), "credential should decrypt");
        assert_eq!(original, decrypted);
    }

    #[test]
    fn test_decrypt_plaintext_returns_error() {
        let enc = ok(
            CredentialEncryption::new(&test_key()),
            "encryption should build",
        );
        let plaintext = r#"{"type":"bilibili","cookies":{"SESSDATA":"test"}}"#;

        let result = enc.decrypt(plaintext);
        assert!(result.is_err(), "Plaintext credentials should be rejected");
    }

    #[test]
    fn test_decrypt_value_encrypted() {
        let enc = ok(
            CredentialEncryption::new(&test_key()),
            "encryption should build",
        );
        let original = json!({"api_key": "secret123"});

        let encrypted_value = ok(
            enc.encrypt_to_value(&original),
            "credential value should encrypt",
        );
        assert!(encrypted_value
            .as_str()
            .is_some_and(|s| s.starts_with("enc:")));

        let decrypted = ok(
            enc.decrypt_value(&encrypted_value),
            "credential value should decrypt",
        );
        assert_eq!(original, decrypted);
    }

    #[test]
    fn test_decrypt_value_plaintext_returns_error() {
        let enc = ok(
            CredentialEncryption::new(&test_key()),
            "encryption should build",
        );
        let plaintext = json!({"cookies": {"SESSDATA": "test"}});

        let result = enc.decrypt_value(&plaintext);
        assert!(result.is_err(), "Plaintext JSON values should be rejected");
    }

    #[test]
    fn test_wrong_key_fails() {
        let enc1 = ok(
            CredentialEncryption::new(&test_key()),
            "encryption should build",
        );
        let original = json!({"secret": "data"});
        let encrypted = ok(enc1.encrypt(&original), "credential should encrypt");

        let wrong_key = vec![0xffu8; 32];
        let enc2 = ok(
            CredentialEncryption::new(&wrong_key),
            "wrong-key encryption should build",
        );

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
        let enc = ok(
            CredentialEncryption::from_hex_key(hex_key),
            "hex key should build encryption",
        );
        let original = json!({"test": true});

        let encrypted = ok(enc.encrypt(&original), "credential should encrypt");
        let decrypted = ok(enc.decrypt(&encrypted), "credential should decrypt");
        assert_eq!(original, decrypted);
    }

    #[test]
    fn test_each_encryption_produces_different_ciphertext() {
        let enc = ok(
            CredentialEncryption::new(&test_key()),
            "encryption should build",
        );
        let original = json!({"same": "data"});

        let encrypted1 = ok(enc.encrypt(&original), "first credential should encrypt");
        let encrypted2 = ok(enc.encrypt(&original), "second credential should encrypt");

        assert_ne!(encrypted1, encrypted2);

        assert_eq!(
            ok(enc.decrypt(&encrypted1), "first credential should decrypt"),
            original
        );
        assert_eq!(
            ok(enc.decrypt(&encrypted2), "second credential should decrypt"),
            original
        );
    }
}
