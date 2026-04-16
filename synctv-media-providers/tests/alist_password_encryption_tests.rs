//! Alist Password Encryption Tests
//!
//! Tests that Alist passwords are encrypted at rest in credential storage.
//! These tests verify the encryption behavior of `InMemoryCredentialStorage`.
//!
//! Run with: cargo test --test `alist_password_encryption_tests`

#![allow(clippy::unwrap_used)]
use synctv_media_providers::{
    CredentialData, CredentialStorage, FieldEncryption, InMemoryCredentialStorage, ProviderType,
};

// Test key for encryption (32 bytes for AES-256)
fn test_encryption_key() -> Vec<u8> {
    vec![
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ]
}

/// Test that `FieldEncryption` works correctly
/// This is the foundational test - if encryption works, storage will work
#[test]
fn test_field_encryption_works() {
    let enc = FieldEncryption::new(&test_encryption_key()).unwrap();
    let plain_password = "my_secret_password_123";

    let encrypted = enc.encrypt(plain_password).unwrap();

    // Encrypted data should have the prefix
    assert!(
        encrypted.starts_with("enc:"),
        "Encrypted password should have 'enc:' prefix"
    );

    // Plaintext should not appear in encrypted data
    assert!(
        !encrypted.contains(plain_password),
        "Plaintext password should not appear in encrypted data"
    );

    // Decryption should work
    let decrypted = enc.decrypt(&encrypted).unwrap();
    assert_eq!(
        decrypted, plain_password,
        "Decrypted password should match original"
    );
}

/// Test that encrypting the same password twice produces different ciphertext
#[test]
fn test_field_encryption_different_ciphertext() {
    let enc = FieldEncryption::new(&test_encryption_key()).unwrap();
    let plain_password = "same_password";

    let enc1 = enc.encrypt(plain_password).unwrap();
    let enc2 = enc.encrypt(plain_password).unwrap();

    // Both should be encrypted
    assert!(enc1.starts_with("enc:"));
    assert!(enc2.starts_with("enc:"));

    // But different (due to random nonces)
    assert_ne!(
        enc1, enc2,
        "Same plaintext should produce different ciphertext"
    );

    // Both should decrypt correctly
    assert_eq!(enc.decrypt(&enc1).unwrap(), plain_password);
    assert_eq!(enc.decrypt(&enc2).unwrap(), plain_password);
}

/// Test that plaintext credentials are rejected
#[test]
fn test_field_encryption_rejects_plaintext() {
    let enc = FieldEncryption::new(&test_encryption_key()).unwrap();
    let plaintext = "plaintext_password";

    // Plaintext should be rejected
    let result = enc.decrypt(plaintext);
    assert!(result.is_err(), "Plaintext credentials should be rejected");
}

/// Test that Alist password round-trips correctly with encryption
#[tokio::test]
async fn test_alist_password_round_trip_with_encryption() {
    let encryption_key = test_encryption_key();
    let storage = InMemoryCredentialStorage::with_encryption(&encryption_key).unwrap();

    let plain_password = "my_secret_password_456";
    let host = "https://alist.example.com";
    let username = "admin";

    // Store Alist credential
    let stored = storage
        .set(
            "user1",
            Some("my_alist"),
            CredentialData::alist(
                host.to_string(),
                username.to_string(),
                plain_password.to_string(),
            ),
        )
        .await
        .expect("Failed to store credential");

    // Returned credential should have decrypted password (for caller convenience)
    let (_, _, password) = stored
        .data
        .as_alist()
        .expect("Expected Alist credential data");
    assert_eq!(
        password, plain_password,
        "Returned password should be decrypted"
    );

    // Retrieve the credential
    let server_id = CredentialData::alist(host.to_string(), String::new(), String::new())
        .server_id_for_instance(Some("my_alist"));
    let retrieved = storage
        .get("user1", ProviderType::Alist, &server_id)
        .await
        .expect("Failed to get credential")
        .expect("Credential should exist");

    // Verify: the retrieved password should match the original
    let (h, u, password) = retrieved
        .data
        .as_alist()
        .expect("Expected Alist credential data");
    assert_eq!(
        password, plain_password,
        "Password should be decrypted correctly"
    );
    assert_eq!(h, host);
    assert_eq!(u, username);
}

/// Test that multiple Alist credentials for different servers work correctly
#[tokio::test]
async fn test_multiple_alist_credentials_encrypted() {
    let encryption_key = test_encryption_key();
    let storage = InMemoryCredentialStorage::with_encryption(&encryption_key).unwrap();

    let password1 = "password_for_server1";
    let password2 = "password_for_server2";

    // Store first credential
    storage
        .set(
            "user1",
            Some("alist1"),
            CredentialData::alist(
                "https://alist1.example.com".to_string(),
                "admin".to_string(),
                password1.to_string(),
            ),
        )
        .await
        .expect("Failed to store first credential");

    // Store second credential
    storage
        .set(
            "user1",
            Some("alist2"),
            CredentialData::alist(
                "https://alist2.example.com".to_string(),
                "user".to_string(),
                password2.to_string(),
            ),
        )
        .await
        .expect("Failed to store second credential");

    // Retrieve and verify first
    let server_id1 = CredentialData::alist(
        "https://alist1.example.com".to_string(),
        String::new(),
        String::new(),
    )
    .server_id_for_instance(Some("alist1"));
    let retrieved1 = storage
        .get("user1", ProviderType::Alist, &server_id1)
        .await
        .expect("Failed to get first credential")
        .expect("First credential should exist");

    let (_, _, password) = retrieved1
        .data
        .as_alist()
        .expect("Expected Alist credential data");
    assert_eq!(password, password1);

    // Retrieve and verify second
    let server_id2 = CredentialData::alist(
        "https://alist2.example.com".to_string(),
        String::new(),
        String::new(),
    )
    .server_id_for_instance(Some("alist2"));
    let retrieved2 = storage
        .get("user1", ProviderType::Alist, &server_id2)
        .await
        .expect("Failed to get second credential")
        .expect("Second credential should exist");

    let (_, _, password) = retrieved2
        .data
        .as_alist()
        .expect("Expected Alist credential data");
    assert_eq!(password, password2);
}

/// Test that Emby `api_key` can be correctly stored and retrieved with encryption
#[tokio::test]
async fn test_emby_api_key_round_trip_with_encryption() {
    let encryption_key = test_encryption_key();
    let storage = InMemoryCredentialStorage::with_encryption(&encryption_key).unwrap();

    let api_key = "secret_api_key_67890";
    let host = "https://emby.example.com";
    let emby_user_id = "user_123";

    // Store Emby credential
    let stored = storage
        .set(
            "user1",
            Some("my_emby"),
            CredentialData::emby(
                host.to_string(),
                api_key.to_string(),
                emby_user_id.to_string(),
            ),
        )
        .await
        .expect("Failed to store credential");

    // Returned credential should have decrypted api_key
    let (_, key, _) = stored
        .data
        .as_emby()
        .expect("Expected Emby credential data");
    assert_eq!(key, api_key, "Returned api_key should be decrypted");

    // Retrieve the credential
    let server_id = CredentialData::emby(host.to_string(), String::new(), String::new())
        .server_id_for_instance(Some("my_emby"));
    let retrieved = storage
        .get("user1", ProviderType::Emby, &server_id)
        .await
        .expect("Failed to get credential")
        .expect("Credential should exist");

    // Verify: the retrieved api_key should match the original
    let (h, key, uid) = retrieved
        .data
        .as_emby()
        .expect("Expected Emby credential data");
    assert_eq!(key, api_key, "API key should be decrypted correctly");
    assert_eq!(h, host);
    assert_eq!(uid, emby_user_id);
}

/// Test that `list_by_user` and `list_by_provider` return decrypted credentials
#[tokio::test]
async fn test_list_credentials_are_decrypted() {
    let encryption_key = test_encryption_key();
    let storage = InMemoryCredentialStorage::with_encryption(&encryption_key).unwrap();

    let password1 = "alist_password";
    let api_key = "emby_api_key";

    // Store Alist credential
    storage
        .set(
            "user1",
            Some("alist1"),
            CredentialData::alist(
                "https://alist.example.com".to_string(),
                "admin".to_string(),
                password1.to_string(),
            ),
        )
        .await
        .unwrap();

    // Store Emby credential
    storage
        .set(
            "user1",
            Some("emby1"),
            CredentialData::emby(
                "https://emby.example.com".to_string(),
                api_key.to_string(),
                "user_id".to_string(),
            ),
        )
        .await
        .unwrap();

    // List by user
    let all_creds = storage.list_by_user("user1").await.unwrap();
    assert_eq!(all_creds.len(), 2);

    for cred in &all_creds {
        match &cred.data {
            CredentialData::Alist { password, .. } => {
                assert_eq!(
                    password, password1,
                    "Alist password should be decrypted in list"
                );
            }
            CredentialData::Emby { api_key: key, .. } => {
                assert_eq!(key, api_key, "Emby api_key should be decrypted in list");
            }
            CredentialData::Bilibili { .. } => {}
        }
    }

    // List by provider (Alist)
    let alist_creds = storage
        .list_by_provider("user1", ProviderType::Alist)
        .await
        .unwrap();
    assert_eq!(alist_creds.len(), 1);
    if let CredentialData::Alist { password, .. } = &alist_creds[0].data {
        assert_eq!(password, password1);
    }

    // List by provider (Emby)
    let emby_creds = storage
        .list_by_provider("user1", ProviderType::Emby)
        .await
        .unwrap();
    assert_eq!(emby_creds.len(), 1);
    if let CredentialData::Emby { api_key: key, .. } = &emby_creds[0].data {
        assert_eq!(key, api_key);
    }
}

/// Test that `FieldEncryption::is_encrypted` correctly detects encrypted values
#[test]
fn test_is_encrypted_detection() {
    assert!(FieldEncryption::is_encrypted("enc:some_base64_data"));
    assert!(!FieldEncryption::is_encrypted("plaintext"));
    assert!(!FieldEncryption::is_encrypted(""));
    assert!(!FieldEncryption::is_encrypted("ENC:uppercase")); // Case sensitive
}
