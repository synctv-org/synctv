//! Credential encryption integration tests
//!
//! Tests AES-256-GCM encrypt/decrypt cycle, wrong key rejection, and edge cases.
//! These are pure unit tests (no database needed).
//!
//! Run with: cargo test --test credential_encryption_tests

use synctv_core::service::credential_encryption::CredentialEncryption;
use serde_json::json;

fn test_key() -> Vec<u8> {
    vec![
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
        0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
        0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
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
    assert!(encrypted.starts_with("enc:"), "Encrypted string should have enc: prefix");

    let decrypted = enc.decrypt(&encrypted).unwrap();
    assert_eq!(original, decrypted);
}

#[test]
fn test_wrong_key_cannot_decrypt() {
    let enc1 = CredentialEncryption::new(&test_key()).unwrap();
    let original = json!({"secret": "very_secret_data"});
    let encrypted = enc1.encrypt(&original).unwrap();

    // Create a different key
    let wrong_key = vec![0xffu8; 32];
    let enc2 = CredentialEncryption::new(&wrong_key).unwrap();

    let result = enc2.decrypt(&encrypted);
    assert!(result.is_err(), "Decryption with wrong key should fail");
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
fn test_each_encryption_unique_ciphertext() {
    let enc = CredentialEncryption::new(&test_key()).unwrap();
    let data = json!({"same": "data"});

    let enc1 = enc.encrypt(&data).unwrap();
    let enc2 = enc.encrypt(&data).unwrap();

    // Different nonces produce different ciphertext
    assert_ne!(enc1, enc2);

    // Both decrypt correctly
    assert_eq!(enc.decrypt(&enc1).unwrap(), data);
    assert_eq!(enc.decrypt(&enc2).unwrap(), data);
}

#[test]
fn test_invalid_key_lengths() {
    assert!(CredentialEncryption::new(&[0u8; 16]).is_err(), "16-byte key should fail");
    assert!(CredentialEncryption::new(&[0u8; 0]).is_err(), "Empty key should fail");
    assert!(CredentialEncryption::new(&[0u8; 64]).is_err(), "64-byte key should fail");
    assert!(CredentialEncryption::new(&[0u8; 32]).is_ok(), "32-byte key should succeed");
}

#[test]
fn test_decrypt_plaintext_backward_compatibility() {
    let enc = CredentialEncryption::new(&test_key()).unwrap();

    // Plaintext JSON string should be parsed directly (backward compat)
    let plaintext = r#"{"cookies":{"SESSDATA":"test_value"}}"#;
    let decrypted = enc.decrypt(plaintext).unwrap();
    assert_eq!(decrypted["cookies"]["SESSDATA"], "test_value");
}

#[test]
fn test_is_encrypted_detection() {
    assert!(CredentialEncryption::is_encrypted(&json!("enc:AAAA")));
    assert!(!CredentialEncryption::is_encrypted(&json!("not encrypted")));
    assert!(!CredentialEncryption::is_encrypted(&json!({"key": "value"})));
    assert!(!CredentialEncryption::is_encrypted(&json!(42)));
    assert!(!CredentialEncryption::is_encrypted(&json!(null)));
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

#[test]
fn test_from_hex_key() {
    let hex_key = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    let enc = CredentialEncryption::from_hex_key(hex_key).unwrap();
    let data = json!({"test": true});

    let encrypted = enc.encrypt(&data).unwrap();
    let decrypted = enc.decrypt(&encrypted).unwrap();
    assert_eq!(data, decrypted);
}
