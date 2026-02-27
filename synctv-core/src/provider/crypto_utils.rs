//! Shared credential encryption utilities for providers
//!
//! This module provides common functions for encrypting and decrypting
//! credential fields in provider source configs. All providers (bilibili, alist, emby)
//! use these utilities to handle sensitive credentials consistently.
//!
//! # Encryption Pattern
//!
//! 1. During `prepare_source_config`: encrypt plaintext credentials before storage
//! 2. During `generate_playback`: decrypt credentials for API calls
//!
//! # Supported Field Types
//!
//! - Object fields (e.g., bilibili cookies): encrypted as JSON object -> encrypted string
//! - String fields (e.g., alist/emby tokens): encrypted as JSON string -> encrypted string
//!
//! Encrypted values are prefixed with "enc:" to distinguish from plaintext.

use serde_json::Value;
use crate::service::CredentialEncryption;
use super::error::ProviderError;

/// Encrypt a field in a JSON object if encryption is available.
///
/// This function handles both object fields (like bilibili cookies) and string fields
/// (like alist/emby tokens). It only encrypts if the field is present and not already
/// encrypted (doesn't start with "enc:").
///
/// # Arguments
///
/// * `config` - The source config JSON value
/// * `encryption` - The credential encryption service
/// * `field_name` - Name of the field to encrypt (e.g., "token", "cookies")
/// * `provider_name` - Provider name for error messages (e.g., "Bilibili", "Alist")
///
/// # Returns
///
/// Returns the config with the specified field encrypted, or the original config
/// if the field is missing, empty, or already encrypted.
///
/// # Errors
///
/// Returns `ProviderError::ApiError` if encryption fails.
pub fn encrypt_field_in_value(
    config: &Value,
    encryption: &CredentialEncryption,
    field_name: &str,
    provider_name: &str,
) -> Result<Value, ProviderError> {
    let mut result = config.clone();
    if let Some(obj) = result.as_object_mut() {
        if let Some(field_value) = obj.get(field_name) {
            // Handle string fields (token, api_key, etc.)
            if let Some(field_str) = field_value.as_str() {
                // Only encrypt non-empty strings that aren't already encrypted
                if !field_str.is_empty() && !field_str.starts_with("enc:") {
                    let encrypted = encryption
                        .encrypt(&Value::String(field_str.to_string()))
                        .map_err(|e| {
                            ProviderError::ApiError(format!(
                                "Failed to encrypt {provider_name} {field_name}: {e}"
                            ))
                        })?;
                    obj.insert(field_name.to_string(), Value::String(encrypted));
                }
            }
            // Handle object fields (cookies, etc.)
            else if let Some(field_obj) = field_value.as_object() {
                // Only encrypt non-empty objects (not already encrypted string)
                if !field_obj.is_empty() {
                    let encrypted = encryption.encrypt(field_value).map_err(|e| {
                        ProviderError::ApiError(format!(
                            "Failed to encrypt {provider_name} {field_name}: {e}"
                        ))
                    })?;
                    obj.insert(field_name.to_string(), Value::String(encrypted));
                }
            }
        }
    }
    Ok(result)
}

/// Decrypt a field in a JSON object if it was encrypted.
///
/// This function handles both object fields (like bilibili cookies) and string fields
/// (like alist/emby tokens). It only decrypts if the field is an encrypted string
/// (starts with "enc:").
///
/// # Arguments
///
/// * `config` - The source config JSON value
/// * `encryption` - The credential encryption service
/// * `field_name` - Name of the field to decrypt (e.g., "token", "cookies")
/// * `provider_name` - Provider name for error messages (e.g., "Bilibili", "Alist")
///
/// # Returns
///
/// Returns the config with the specified field decrypted. If the field is not
/// encrypted (doesn't start with "enc:"), returns the original config unchanged
/// for backward compatibility with plaintext credentials.
///
/// # Errors
///
/// Returns `ProviderError::ApiError` if decryption fails.
pub fn decrypt_field_in_value(
    config: &Value,
    encryption: &CredentialEncryption,
    field_name: &str,
    provider_name: &str,
) -> Result<Value, ProviderError> {
    let mut result = config.clone();
    if let Some(obj) = result.as_object_mut() {
        if let Some(field_value) = obj.get(field_name) {
            if let Some(encrypted_str) = field_value.as_str() {
                if encrypted_str.starts_with("enc:") {
                    let decrypted = encryption.decrypt(encrypted_str).map_err(|e| {
                        ProviderError::ApiError(format!(
                            "Failed to decrypt {provider_name} {field_name}: {e}"
                        ))
                    })?;
                    // For string fields, extract the inner string value
                    // For object fields, use the decrypted value directly
                    if let Some(s) = decrypted.as_str() {
                        obj.insert(field_name.to_string(), Value::String(s.to_string()));
                    } else {
                        obj.insert(field_name.to_string(), decrypted);
                    }
                }
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_encryption() -> CredentialEncryption {
        let key = vec![
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ];
        CredentialEncryption::new(&key).expect("Test encryption key should be valid")
    }

    // ============== encrypt_field_in_value tests ==============

    #[test]
    fn test_encrypt_string_field_encrypts_non_empty_plaintext() {
        let enc = test_encryption();
        let config = json!({
            "host": "https://example.com",
            "token": "secret_token_123"
        });

        let result = encrypt_field_in_value(&config, &enc, "token", "Test").unwrap();

        // Token should be encrypted
        let token = result.get("token").unwrap().as_str().unwrap();
        assert!(token.starts_with("enc:"));
        assert_ne!(token, "secret_token_123");
        // Other fields should be unchanged
        assert_eq!(result.get("host").unwrap(), "https://example.com");
    }

    #[test]
    fn test_encrypt_string_field_skips_empty_string() {
        let enc = test_encryption();
        let config = json!({
            "host": "https://example.com",
            "token": ""
        });

        let result = encrypt_field_in_value(&config, &enc, "token", "Test").unwrap();

        // Empty token should not be encrypted
        assert_eq!(result.get("token").unwrap(), "");
    }

    #[test]
    fn test_encrypt_string_field_skips_already_encrypted() {
        let enc = test_encryption();
        let config = json!({
            "token": "enc:already_encrypted_value"
        });

        let result = encrypt_field_in_value(&config, &enc, "token", "Test").unwrap();

        // Already encrypted value should not be double-encrypted
        assert_eq!(result.get("token").unwrap(), "enc:already_encrypted_value");
    }

    #[test]
    fn test_encrypt_object_field_encrypts_non_empty_object() {
        let enc = test_encryption();
        let config = json!({
            "host": "https://example.com",
            "cookies": {
                "SESSDATA": "session_value",
                "BILI_JCT": "csrf_token"
            }
        });

        let result = encrypt_field_in_value(&config, &enc, "cookies", "Test").unwrap();

        // Cookies should be encrypted
        let cookies = result.get("cookies").unwrap().as_str().unwrap();
        assert!(cookies.starts_with("enc:"));
        // Other fields should be unchanged
        assert_eq!(result.get("host").unwrap(), "https://example.com");
    }

    #[test]
    fn test_encrypt_object_field_skips_empty_object() {
        let enc = test_encryption();
        let config = json!({
            "host": "https://example.com",
            "cookies": {}
        });

        let result = encrypt_field_in_value(&config, &enc, "cookies", "Test").unwrap();

        // Empty cookies object should not be encrypted
        assert!(result.get("cookies").unwrap().is_object());
        assert!(result.get("cookies").unwrap().as_object().unwrap().is_empty());
    }

    #[test]
    fn test_encrypt_missing_field_returns_unchanged() {
        let enc = test_encryption();
        let config = json!({
            "host": "https://example.com"
        });

        let result = encrypt_field_in_value(&config, &enc, "token", "Test").unwrap();

        // Missing field should return config unchanged
        assert_eq!(result, config);
    }

    #[test]
    fn test_encrypt_non_object_config_returns_unchanged() {
        let enc = test_encryption();
        let config = json!("not an object");

        let result = encrypt_field_in_value(&config, &enc, "token", "Test").unwrap();

        // Non-object should return unchanged
        assert_eq!(result, config);
    }

    // ============== decrypt_field_in_value tests ==============

    #[test]
    fn test_decrypt_string_field_decrypts_encrypted_value() {
        let enc = test_encryption();
        let original_token = "secret_token_123";
        let encrypted = enc.encrypt(&json!(original_token)).unwrap();

        let config = json!({
            "host": "https://example.com",
            "token": encrypted
        });

        let result = decrypt_field_in_value(&config, &enc, "token", "Test").unwrap();

        // Token should be decrypted back to original
        assert_eq!(result.get("token").unwrap(), original_token);
        // Other fields should be unchanged
        assert_eq!(result.get("host").unwrap(), "https://example.com");
    }

    #[test]
    fn test_decrypt_object_field_decrypts_encrypted_value() {
        let enc = test_encryption();
        let original_cookies = json!({
            "SESSDATA": "session_value",
            "BILI_JCT": "csrf_token"
        });
        let encrypted = enc.encrypt(&original_cookies).unwrap();

        let config = json!({
            "host": "https://example.com",
            "cookies": encrypted
        });

        let result = decrypt_field_in_value(&config, &enc, "cookies", "Test").unwrap();

        // Cookies should be decrypted back to original object
        assert_eq!(result.get("cookies").unwrap(), &original_cookies);
    }

    #[test]
    fn test_decrypt_field_skips_plaintext_string_for_backward_compat() {
        let enc = test_encryption();
        let config = json!({
            "token": "plaintext_token"
        });

        let result = decrypt_field_in_value(&config, &enc, "token", "Test").unwrap();

        // Plaintext token should pass through unchanged
        assert_eq!(result.get("token").unwrap(), "plaintext_token");
    }

    #[test]
    fn test_decrypt_field_skips_object_for_backward_compat() {
        let enc = test_encryption();
        let config = json!({
            "cookies": {
                "SESSDATA": "session_value"
            }
        });

        let result = decrypt_field_in_value(&config, &enc, "cookies", "Test").unwrap();

        // Plaintext cookies object should pass through unchanged
        assert_eq!(result.get("cookies").unwrap().get("SESSDATA").unwrap(), "session_value");
    }

    #[test]
    fn test_decrypt_missing_field_returns_unchanged() {
        let enc = test_encryption();
        let config = json!({
            "host": "https://example.com"
        });

        let result = decrypt_field_in_value(&config, &enc, "token", "Test").unwrap();

        // Missing field should return config unchanged
        assert_eq!(result, config);
    }

    #[test]
    fn test_decrypt_non_object_config_returns_unchanged() {
        let enc = test_encryption();
        let config = json!("not an object");

        let result = decrypt_field_in_value(&config, &enc, "token", "Test").unwrap();

        // Non-object should return unchanged
        assert_eq!(result, config);
    }

    // ============== Round-trip tests ==============

    #[test]
    fn test_roundtrip_string_field() {
        let enc = test_encryption();
        let original = json!({
            "host": "https://example.com",
            "token": "secret_token_123"
        });

        let encrypted = encrypt_field_in_value(&original, &enc, "token", "Test").unwrap();
        let decrypted = decrypt_field_in_value(&encrypted, &enc, "token", "Test").unwrap();

        assert_eq!(decrypted, original);
    }

    #[test]
    fn test_roundtrip_object_field() {
        let enc = test_encryption();
        let original = json!({
            "host": "https://example.com",
            "cookies": {
                "SESSDATA": "session_value",
                "BILI_JCT": "csrf_token"
            }
        });

        let encrypted = encrypt_field_in_value(&original, &enc, "cookies", "Test").unwrap();
        let decrypted = decrypt_field_in_value(&encrypted, &enc, "cookies", "Test").unwrap();

        assert_eq!(decrypted, original);
    }

    // ============== Error handling tests ==============

    #[test]
    fn test_decrypt_with_wrong_key_fails() {
        let enc1 = test_encryption();
        let enc2_key = vec![0xffu8; 32];
        let enc2 = CredentialEncryption::new(&enc2_key).unwrap();

        let original = json!({"token": "secret"});
        let encrypted = encrypt_field_in_value(&original, &enc1, "token", "Test").unwrap();

        // Decrypting with wrong key should fail
        let result = decrypt_field_in_value(&encrypted, &enc2, "token", "Test");
        assert!(result.is_err());
    }
}
