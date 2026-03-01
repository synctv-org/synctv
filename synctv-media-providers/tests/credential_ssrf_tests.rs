//! SSRF validation tests for `PostgreSQL` credential storage
//!
//! Tests that credential storage validates host URLs against SSRF attacks
//! before persisting them to the database.

#![allow(clippy::unwrap_used)]
use std::collections::HashMap;
use synctv_media_providers::{CredentialData, InMemoryCredentialStorage, CredentialStorage, ProviderType};

/// Test that storing a credential with a private IP address is rejected
#[tokio::test]
async fn test_credential_storage_rejects_private_ip() {
    let storage = InMemoryCredentialStorage::new();

    // Attempt to store an Alist credential with a private IP
    let result = storage
        .set(
            "user1",
            Some("test-instance"),
            CredentialData::alist(
                "http://192.168.1.100:5244".into(),
                "admin".into(),
                "password".into(),
            ),
        )
        .await;

    // Should fail with an SSRF-related error
    assert!(result.is_err(), "Storing private IP should be rejected");
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("private")
            || err.to_string().contains("reserved")
            || err.to_string().contains("SSRF")
            || err.to_string().contains("blocked"),
        "Error should mention SSRF/private/blocked: {err}"
    );
}

/// Test that storing a credential with localhost is rejected
#[tokio::test]
async fn test_credential_storage_rejects_localhost() {
    let storage = InMemoryCredentialStorage::new();

    // Attempt to store an Emby credential with localhost
    let result = storage
        .set(
            "user1",
            Some("test-instance"),
            CredentialData::emby(
                "http://localhost:8096".into(),
                "api_key".into(),
                "user_id".into(),
            ),
        )
        .await;

    // Should fail with an SSRF-related error
    assert!(result.is_err(), "Storing localhost should be rejected");
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("localhost")
            || err.to_string().contains("internal")
            || err.to_string().contains("SSRF")
            || err.to_string().contains("blocked"),
        "Error should mention localhost/internal/blocked: {err}"
    );
}

/// Test that storing a credential with loopback IP is rejected
#[tokio::test]
async fn test_credential_storage_rejects_loopback_ip() {
    let storage = InMemoryCredentialStorage::new();

    // Attempt to store an Alist credential with 127.0.0.1
    let result = storage
        .set(
            "user1",
            Some("test-instance"),
            CredentialData::alist(
                "http://127.0.0.1:5244".into(),
                "admin".into(),
                "password".into(),
            ),
        )
        .await;

    // Should fail with an SSRF-related error
    assert!(result.is_err(), "Storing loopback IP should be rejected");
}

/// Test that storing a credential with link-local IP is rejected
#[tokio::test]
async fn test_credential_storage_rejects_link_local() {
    let storage = InMemoryCredentialStorage::new();

    // Attempt to store an Emby credential with link-local IP (169.254.x.x)
    let result = storage
        .set(
            "user1",
            Some("test-instance"),
            CredentialData::emby(
                "http://169.254.169.254:8096".into(), // Cloud metadata IP
                "api_key".into(),
                "user_id".into(),
            ),
        )
        .await;

    // Should fail with an SSRF-related error
    assert!(result.is_err(), "Storing link-local IP should be rejected");
}

/// Test that storing a credential with internal hostname is rejected
#[tokio::test]
async fn test_credential_storage_rejects_internal_hostname() {
    let storage = InMemoryCredentialStorage::new();

    // Attempt to store an Alist credential with internal hostname
    let result = storage
        .set(
            "user1",
            Some("test-instance"),
            CredentialData::alist(
                "http://metadata.google.internal:5244".into(),
                "admin".into(),
                "password".into(),
            ),
        )
        .await;

    // Should fail with an SSRF-related error
    assert!(result.is_err(), "Storing internal hostname should be rejected");
}

/// Test that storing a credential with .local hostname is rejected
#[tokio::test]
async fn test_credential_storage_rejects_local_suffix() {
    let storage = InMemoryCredentialStorage::new();

    let result = storage
        .set(
            "user1",
            Some("test-instance"),
            CredentialData::emby(
                "http://myserver.local:8096".into(),
                "api_key".into(),
                "user_id".into(),
            ),
        )
        .await;

    assert!(result.is_err(), "Storing .local hostname should be rejected");
}

/// Test that storing a credential with valid public host succeeds
#[tokio::test]
async fn test_credential_storage_accepts_public_host() {
    let storage = InMemoryCredentialStorage::new();

    // Store an Alist credential with a public hostname
    let result = storage
        .set(
            "user1",
            Some("test-instance"),
            CredentialData::alist(
                "https://alist.example.com".into(),
                "admin".into(),
                "password".into(),
            ),
        )
        .await;

    assert!(result.is_ok(), "Storing public hostname should succeed: {result:?}");
    let cred = result.unwrap();
    assert_eq!(cred.user_id, "user1");
    assert_eq!(cred.provider, ProviderType::Alist);
}

/// Test that storing an Emby credential with valid public host succeeds
#[tokio::test]
async fn test_credential_storage_accepts_emby_public_host() {
    let storage = InMemoryCredentialStorage::new();

    // Store an Emby credential with a public hostname
    let result = storage
        .set(
            "user1",
            Some("test-instance"),
            CredentialData::emby(
                "https://emby.public-server.com:8096".into(),
                "api_key_123".into(),
                "user_id_456".into(),
            ),
        )
        .await;

    assert!(result.is_ok(), "Storing public Emby hostname should succeed: {result:?}");
}

/// Test that storing Bilibili credentials succeeds (no host to validate)
#[tokio::test]
async fn test_credential_storage_accepts_bilibili() {
    let storage = InMemoryCredentialStorage::new();

    // Bilibili has no host URL, so it should always succeed
    let mut cookies = HashMap::new();
    cookies.insert("SESSDATA".to_string(), "test_session".to_string());

    let result = storage
        .set("user1", Some("bilibili-instance"), CredentialData::bilibili(cookies))
        .await;

    assert!(result.is_ok(), "Storing Bilibili credentials should succeed: {result:?}");
}

/// Test that storing credential with public IP succeeds
#[tokio::test]
async fn test_credential_storage_accepts_public_ip() {
    let storage = InMemoryCredentialStorage::new();

    // 8.8.8.8 is a public IP (Google DNS)
    let result = storage
        .set(
            "user1",
            Some("test-instance"),
            CredentialData::alist(
                "http://8.8.8.8:5244".into(),
                "admin".into(),
                "password".into(),
            ),
        )
        .await;

    assert!(result.is_ok(), "Storing public IP should succeed: {result:?}");
}

/// Test that storing credential with 10.x.x.x is rejected
#[tokio::test]
async fn test_credential_storage_rejects_10_range() {
    let storage = InMemoryCredentialStorage::new();

    let result = storage
        .set(
            "user1",
            Some("test-instance"),
            CredentialData::alist(
                "http://10.0.0.1:5244".into(),
                "admin".into(),
                "password".into(),
            ),
        )
        .await;

    assert!(result.is_err(), "Storing 10.x.x.x should be rejected");
}

/// Test that storing credential with 172.16-31.x.x is rejected
#[tokio::test]
async fn test_credential_storage_rejects_172_range() {
    let storage = InMemoryCredentialStorage::new();

    let result = storage
        .set(
            "user1",
            Some("test-instance"),
            CredentialData::alist(
                "http://172.16.0.1:5244".into(),
                "admin".into(),
                "password".into(),
            ),
        )
        .await;

    assert!(result.is_err(), "Storing 172.16-31.x.x should be rejected");
}
