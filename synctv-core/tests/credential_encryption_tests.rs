//! Credential encryption integration tests
//!
//! Tests JSON edge cases and malformed ciphertext handling for credential encryption.
//!
#![allow(clippy::unwrap_used)]

use serde_json::json;
use synctv_core::credential_encryption::CredentialEncryption;

fn test_key() -> Vec<u8> {
    vec![
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ]
}

#[test]
fn test_empty_json_object() {
    let enc = CredentialEncryption::new(&test_key()).unwrap();
    let empty = json!({});

    let encrypted = enc.encrypt(&empty).unwrap();
    let decrypted = enc.decrypt(&encrypted).unwrap();
    assert_eq!(empty, decrypted);
}

#[test]
fn test_null_json_value() {
    let enc = CredentialEncryption::new(&test_key()).unwrap();
    let null_val = json!(null);

    let encrypted = enc.encrypt(&null_val).unwrap();
    let decrypted = enc.decrypt(&encrypted).unwrap();
    assert_eq!(null_val, decrypted);
}

#[test]
fn test_nested_json_structure() {
    let enc = CredentialEncryption::new(&test_key()).unwrap();
    let nested = json!({
        "provider": "emby",
        "credentials": {
            "api_key": "abc123",
            "server": {
                "host": "192.168.1.100",
                "port": 8096,
                "https": true
            }
        },
        "tags": ["media", "home"]
    });

    let encrypted = enc.encrypt(&nested).unwrap();
    let decrypted = enc.decrypt(&encrypted).unwrap();
    assert_eq!(nested, decrypted);
}

#[test]
fn test_decrypt_corrupted_data() {
    let enc = CredentialEncryption::new(&test_key()).unwrap();

    // Corrupted base64 payload
    let result = enc.decrypt("enc:not_valid_base64!!!");
    assert!(result.is_err());

    // Too short payload (valid base64 but not enough bytes for version + nonce)
    let result = enc.decrypt("enc:AAAA");
    assert!(result.is_err());
}
