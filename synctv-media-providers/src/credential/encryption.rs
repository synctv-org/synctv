//! Credential Field Encryption using AES-256-GCM
//!
//! Provides encryption and decryption for sensitive credential fields
//! (Alist passwords, Emby API keys) at rest.
//!
//! This is a simplified version of the encryption in synctv-core,
//! designed for the credential storage layer.

use aes_gcm::{
    aead::{rand_core::RngCore, Aead, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use std::sync::Arc;

/// AES-256-GCM nonce size (96 bits / 12 bytes)
const NONCE_SIZE: usize = 12;

/// Prefix for encrypted data to distinguish from plaintext
const ENCRYPTED_PREFIX: &str = "enc:";

/// Error type for encryption operations
#[derive(Debug, thiserror::Error)]
pub enum EncryptionError {
    /// Invalid key length
    #[error("Invalid encryption key length: expected 32 bytes, got {0}")]
    InvalidKeyLength(usize),

    /// Encryption failed
    #[error("Encryption failed: {0}")]
    EncryptionFailed(String),

    /// Decryption failed
    #[error("Decryption failed: {0}")]
    DecryptionFailed(String),

    /// Invalid encrypted data format
    #[error("Invalid encrypted data format: {0}")]
    InvalidFormat(String),
}

/// Result type for encryption operations
pub type EncryptionResult<T> = std::result::Result<T, EncryptionError>;

/// Credential field encryption service
///
/// Encrypts and decrypts individual string fields (like passwords, API keys).
/// Uses AES-256-GCM for authenticated encryption.
///
/// Uses `Arc` internally so cloning shares a single copy of the cipher.
#[derive(Clone)]
pub struct FieldEncryption {
    cipher: Arc<Aes256Gcm>,
}

impl std::fmt::Debug for FieldEncryption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldEncryption")
            .field("cipher", &"[REDACTED]")
            .finish()
    }
}

impl FieldEncryption {
    /// Create a new encryption service from a 32-byte key
    ///
    /// # Arguments
    /// * `key_bytes` - 32-byte encryption key (AES-256)
    ///
    /// # Errors
    /// Returns error if the key length is not exactly 32 bytes.
    pub fn new(key_bytes: &[u8]) -> EncryptionResult<Self> {
        if key_bytes.len() != 32 {
            return Err(EncryptionError::InvalidKeyLength(key_bytes.len()));
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
    pub fn from_hex_key(hex_key: &str) -> EncryptionResult<Self> {
        let key_bytes = hex::decode(hex_key)
            .map_err(|e| EncryptionError::InvalidFormat(format!("Invalid hex key: {e}")))?;
        Self::new(&key_bytes)
    }

    /// Encrypt a plaintext string
    ///
    /// Returns a string in the format "enc:<base64(nonce + ciphertext)>"
    pub fn encrypt(&self, plaintext: &str) -> EncryptionResult<String> {
        // Generate random nonce
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        // Encrypt
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| EncryptionError::EncryptionFailed(e.to_string()))?;

        let mut combined = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);

        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &combined);
        Ok(format!("{ENCRYPTED_PREFIX}{encoded}"))
    }

    /// Decrypt an encrypted string
    ///
    /// Only accepts encrypted format: "enc:<base64(nonce + ciphertext)>"
    /// Plaintext credentials are no longer supported.
    pub fn decrypt(&self, stored: &str) -> EncryptionResult<String> {
        let Some(encoded) = stored.strip_prefix(ENCRYPTED_PREFIX) else {
            return Err(EncryptionError::InvalidFormat(
                "Credential is not encrypted (missing 'enc:' prefix). \
                 Plaintext credentials are no longer supported."
                    .to_string(),
            ));
        };

        // Encrypted format
        let combined = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
            .map_err(|e| EncryptionError::InvalidFormat(format!("Invalid base64: {e}")))?;

        if combined.len() < NONCE_SIZE {
            return Err(EncryptionError::InvalidFormat(
                "Encrypted data too short".to_string(),
            ));
        }

        let (nonce_bytes, ciphertext) = combined.split_at(NONCE_SIZE);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| EncryptionError::DecryptionFailed(e.to_string()))?;

        String::from_utf8(plaintext)
            .map_err(|e| EncryptionError::DecryptionFailed(format!("Invalid UTF-8: {e}")))
    }

    /// Check if a stored value is encrypted
    #[must_use]
    pub fn is_encrypted(value: &str) -> bool {
        value.starts_with(ENCRYPTED_PREFIX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn test_key() -> Vec<u8> {
        vec![
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ]
    }

    #[test]
    fn test_encrypt_decrypt_round_trip() -> TestResult {
        let enc = FieldEncryption::new(&test_key())?;
        let original = "my_secret_password_123";

        let encrypted = enc.encrypt(original)?;
        assert!(encrypted.starts_with("enc:"));
        assert_ne!(encrypted, original);

        let decrypted = enc.decrypt(&encrypted)?;
        assert_eq!(original, decrypted);
        Ok(())
    }

    #[test]
    fn test_decrypt_plaintext_returns_error() -> TestResult {
        let enc = FieldEncryption::new(&test_key())?;
        let plaintext = "plaintext_password";

        let result = enc.decrypt(plaintext);
        assert!(result.is_err(), "Plaintext credentials should be rejected");
        Ok(())
    }

    #[test]
    fn test_each_encryption_unique() -> TestResult {
        let enc = FieldEncryption::new(&test_key())?;
        let plaintext = "same_password";

        let enc1 = enc.encrypt(plaintext)?;
        let enc2 = enc.encrypt(plaintext)?;

        assert_ne!(enc1, enc2);

        assert_eq!(enc.decrypt(&enc1)?, plaintext);
        assert_eq!(enc.decrypt(&enc2)?, plaintext);
        Ok(())
    }

    #[test]
    fn test_wrong_key_fails() -> TestResult {
        let enc1 = FieldEncryption::new(&test_key())?;
        let encrypted = enc1.encrypt("secret")?;

        let wrong_key = vec![0xffu8; 32];
        let enc2 = FieldEncryption::new(&wrong_key)?;

        let result = enc2.decrypt(&encrypted);
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_invalid_key_length() {
        assert!(FieldEncryption::new(&[0u8; 16]).is_err());
        assert!(FieldEncryption::new(&[0u8; 0]).is_err());
        assert!(FieldEncryption::new(&[0u8; 64]).is_err());
        assert!(FieldEncryption::new(&[0u8; 32]).is_ok());
    }

    #[test]
    fn test_is_encrypted() {
        assert!(FieldEncryption::is_encrypted("enc:AAAA"));
        assert!(!FieldEncryption::is_encrypted("not encrypted"));
        assert!(!FieldEncryption::is_encrypted(""));
    }

    #[test]
    fn test_from_hex_key() -> TestResult {
        let hex_key = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        let enc = FieldEncryption::from_hex_key(hex_key)?;
        let plaintext = "test_password";

        let encrypted = enc.encrypt(plaintext)?;
        let decrypted = enc.decrypt(&encrypted)?;
        assert_eq!(plaintext, decrypted);
        Ok(())
    }
}
