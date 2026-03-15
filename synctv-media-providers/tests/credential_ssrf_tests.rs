//! Credential storage tests
//!
//! URL-shape validation happens before credentials are persisted; SSRF blocking
//! is enforced by the outbound HTTP transport.

#![allow(clippy::unwrap_used)]
use std::collections::HashMap;
use synctv_media_providers::{
    CredentialData, CredentialStorage, InMemoryCredentialStorage, ProviderType,
};

#[tokio::test]
async fn test_credential_storage_accepts_public_host() {
    let storage = InMemoryCredentialStorage::new();

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

    assert!(
        result.is_ok(),
        "Storing public hostname should succeed: {result:?}"
    );
    let cred = result.unwrap();
    assert_eq!(cred.user_id, "user1");
    assert_eq!(cred.provider, ProviderType::Alist);
}

#[tokio::test]
async fn test_credential_storage_accepts_emby_public_host() {
    let storage = InMemoryCredentialStorage::new();

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

    assert!(
        result.is_ok(),
        "Storing public Emby hostname should succeed: {result:?}"
    );
}

#[tokio::test]
async fn test_credential_storage_accepts_bilibili() {
    let storage = InMemoryCredentialStorage::new();

    let mut cookies = HashMap::new();
    cookies.insert("SESSDATA".to_string(), "test_session".to_string());

    let result = storage
        .set(
            "user1",
            Some("bilibili-instance"),
            CredentialData::bilibili(cookies),
        )
        .await;

    assert!(
        result.is_ok(),
        "Storing Bilibili credentials should succeed: {result:?}"
    );
}

#[tokio::test]
async fn test_credential_storage_accepts_public_ip() {
    let storage = InMemoryCredentialStorage::new();

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

    assert!(
        result.is_ok(),
        "Storing public IP should succeed: {result:?}"
    );
}

#[tokio::test]
async fn test_credential_storage_accepts_private_and_local_hosts() {
    let storage = InMemoryCredentialStorage::new();

    let hosts = vec![
        "http://192.168.1.100:5244",
        "http://localhost:8096",
        "http://127.0.0.1:5244",
        "http://10.0.0.1:5244",
    ];

    for host in hosts {
        let result = storage
            .set(
                "user1",
                Some("test-instance"),
                CredentialData::alist(host.into(), "admin".into(), "password".into()),
            )
            .await;

        assert!(
            result.is_ok(),
            "Credential storage should accept routable/private hosts and rely on transport SSRF protection for {host}: {result:?}"
        );
    }
}
