//! Credential Storage Tests
//!
//! Tests for the in-memory credential storage implementation.

#![allow(clippy::unwrap_used)]
use std::collections::HashMap;

use synctv_media_providers::{
    CredentialData, CredentialStorage, InMemoryCredentialStorage, ProviderType,
};

#[tokio::test]
async fn test_in_memory_storage_basic_crud() {
    let storage = InMemoryCredentialStorage::new();

    let cred = storage
        .set("user1", None, CredentialData::bilibili(HashMap::new()))
        .await
        .unwrap();
    assert_eq!(cred.user_id, "user1");

    // Read
    let found = storage
        .get("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap();
    assert!(found.is_some());

    // Delete
    let deleted = storage
        .delete("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap();
    assert!(deleted);

    // Verify deleted
    let not_found = storage
        .get("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap();
    assert!(not_found.is_none());
}

#[tokio::test]
async fn test_in_memory_storage_multiple_providers() {
    let storage = InMemoryCredentialStorage::new();

    storage
        .set("user1", None, CredentialData::bilibili(HashMap::new()))
        .await
        .unwrap();

    storage
        .set(
            "user1",
            None,
            CredentialData::alist(
                "https://alist.example.com".into(),
                "admin".into(),
                "hashed_password".into(),
            ),
        )
        .await
        .unwrap();

    storage
        .set(
            "user1",
            None,
            CredentialData::emby(
                "https://emby.example.com".into(),
                "api_key".into(),
                "user_id".into(),
            ),
        )
        .await
        .unwrap();

    // Should have 3 credentials
    let all = storage.list_by_user("user1").await.unwrap();
    assert_eq!(all.len(), 3);

    // Each provider should be queryable
    let bilibili = storage
        .get("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap();
    assert!(bilibili.is_some());

    let alist_server_id = CredentialData::alist(
        "https://alist.example.com".into(),
        String::new(),
        String::new(),
    )
    .server_id();
    let alist = storage
        .get("user1", ProviderType::Alist, &alist_server_id)
        .await
        .unwrap();
    assert!(alist.is_some());

    let emby_server_id = CredentialData::emby(
        "https://emby.example.com".into(),
        String::new(),
        String::new(),
    )
    .server_id();
    let emby = storage
        .get("user1", ProviderType::Emby, &emby_server_id)
        .await
        .unwrap();
    assert!(emby.is_some());
}

#[tokio::test]
async fn test_in_memory_storage_concurrent_access() {
    let storage = std::sync::Arc::new(InMemoryCredentialStorage::new());
    let mut handles = vec![];

    // Spawn multiple concurrent operations
    for i in 0..10 {
        let s = storage.clone();
        handles.push(tokio::spawn(async move {
            let user_id = format!("user{i}");
            s.set(&user_id, None, CredentialData::bilibili(HashMap::new()))
                .await
                .unwrap();
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // Verify all were created
    let all = storage.list_by_user("user0").await.unwrap();
    assert_eq!(all.len(), 1);
}

#[tokio::test]
async fn test_credential_storage_as_trait_object() {
    // Verify that CredentialStorage can be used as a trait object
    let storage: Box<dyn CredentialStorage> = Box::new(InMemoryCredentialStorage::new());

    storage
        .set("user1", None, CredentialData::bilibili(HashMap::new()))
        .await
        .unwrap();

    let found = storage
        .get("user1", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap();
    assert!(found.is_some());
}

// These tests demonstrate the recommended pattern for integrating
// CredentialStorage with provider operations:

/// Test the Bilibili credential flow pattern.
///
/// This demonstrates how an application would:
/// 1. Receive cookies from Bilibili login
/// 2. Store them using `CredentialStorage`
/// 3. Retrieve them later for API calls
#[tokio::test]
async fn test_bilibili_credential_flow_pattern() {
    let storage = InMemoryCredentialStorage::new();

    // (In real code, this comes from BilibiliService::login_with_qr_code or login_with_sms)
    let mut login_cookies = HashMap::new();
    login_cookies.insert("SESSDATA".to_string(), "sess_value_123".to_string());
    login_cookies.insert("bili_jct".to_string(), "csrf_token_456".to_string());
    login_cookies.insert("DedeUserID".to_string(), "user_id_789".to_string());

    let stored = storage
        .set(
            "user123",
            Some("my_bilibili"),
            CredentialData::bilibili(login_cookies.clone()),
        )
        .await
        .unwrap();

    assert_eq!(stored.user_id, "user123");
    assert_eq!(stored.provider, ProviderType::Bilibili);
    assert_eq!(
        stored.server_id,
        CredentialData::bilibili(HashMap::new()).server_id_for_instance(Some("my_bilibili"))
    );
    assert_eq!(
        stored.provider_instance_name,
        Some("my_bilibili".to_string())
    );

    let retrieved = storage
        .get("user123", ProviderType::Bilibili, &stored.server_id)
        .await
        .unwrap()
        .expect("credential should exist");

    assert_eq!(retrieved.id, stored.id);

    let cookies = retrieved
        .data
        .as_bilibili()
        .expect("Expected Bilibili credential data");
    assert_eq!(cookies.get("SESSDATA"), Some(&"sess_value_123".to_string()));
    assert_eq!(cookies.get("bili_jct"), Some(&"csrf_token_456".to_string()));
    // These cookies would be passed to BilibiliService methods
}

/// Test the Alist credential flow pattern.
///
/// Alist credentials include host, username, and password,
/// and multiple Alist servers can be stored per user.
#[tokio::test]
async fn test_alist_credential_flow_pattern() {
    let storage = InMemoryCredentialStorage::new();

    let host1 = "https://alist1.example.com";
    let cred1 = storage
        .set(
            "user123",
            Some("personal_alist"),
            CredentialData::alist(
                host1.to_string(),
                "admin".to_string(),
                "hashed_password_1".to_string(),
            ),
        )
        .await
        .unwrap();

    let host2 = "https://alist2.example.com";
    let cred2 = storage
        .set(
            "user123",
            Some("shared_alist"),
            CredentialData::alist(
                host2.to_string(),
                "user".to_string(),
                "hashed_password_2".to_string(),
            ),
        )
        .await
        .unwrap();

    let all_alist = storage
        .list_by_provider("user123", ProviderType::Alist)
        .await
        .unwrap();
    assert_eq!(all_alist.len(), 2);

    let server1_id = cred1.server_id.clone();
    let retrieved1 = storage
        .get("user123", ProviderType::Alist, &server1_id)
        .await
        .unwrap()
        .expect("first alist credential should exist");

    let (host, username, password) = retrieved1
        .data
        .as_alist()
        .expect("Expected Alist credential data");
    assert_eq!(host, host1);
    assert_eq!(username, "admin");
    assert_eq!(password, "hashed_password_1");

    let server2_id = cred2.server_id.clone();
    let retrieved2 = storage
        .get("user123", ProviderType::Alist, &server2_id)
        .await
        .unwrap()
        .expect("second alist credential should exist");

    let (host, username, password) = retrieved2
        .data
        .as_alist()
        .expect("Expected Alist credential data");
    assert_eq!(host, host2);
    assert_eq!(username, "user");
    assert_eq!(password, "hashed_password_2");
}

#[tokio::test]
async fn test_same_host_different_instance_names_do_not_overwrite_each_other() {
    let storage = InMemoryCredentialStorage::new();
    let host = "https://alist.example.com";

    let main = storage
        .set(
            "user123",
            Some("alist-main"),
            CredentialData::alist(
                host.to_string(),
                "main-user".to_string(),
                "main-pass".to_string(),
            ),
        )
        .await
        .unwrap();

    let backup = storage
        .set(
            "user123",
            Some("alist-backup"),
            CredentialData::alist(
                host.to_string(),
                "backup-user".to_string(),
                "backup-pass".to_string(),
            ),
        )
        .await
        .unwrap();

    assert_ne!(
        main.server_id, backup.server_id,
        "instance-scoped credentials must not collapse to the same server_id"
    );

    let main_retrieved = storage
        .get("user123", ProviderType::Alist, &main.server_id)
        .await
        .unwrap()
        .expect("main credential should exist");
    let backup_retrieved = storage
        .get("user123", ProviderType::Alist, &backup.server_id)
        .await
        .unwrap()
        .expect("backup credential should exist");

    assert_eq!(
        main_retrieved.provider_instance_name.as_deref(),
        Some("alist-main")
    );
    assert_eq!(
        backup_retrieved.provider_instance_name.as_deref(),
        Some("alist-backup")
    );
    assert_eq!(
        main_retrieved.data.as_alist().expect("alist main").1,
        "main-user"
    );
    assert_eq!(
        backup_retrieved.data.as_alist().expect("alist backup").1,
        "backup-user"
    );

    let all_alist = storage
        .list_by_provider("user123", ProviderType::Alist)
        .await
        .unwrap();
    assert_eq!(all_alist.len(), 2);
}

/// Test the Emby credential flow pattern.
///
/// Emby credentials include host, `api_key`, and `emby_user_id`.
#[tokio::test]
async fn test_emby_credential_flow_pattern() {
    let storage = InMemoryCredentialStorage::new();

    let host = "https://emby.example.com";
    let cred = storage
        .set(
            "user123",
            Some("home_emby"),
            CredentialData::emby(
                host.to_string(),
                "api_token_abc123".to_string(),
                "emby_user_id_456".to_string(),
            ),
        )
        .await
        .unwrap();

    let server_id = cred.server_id.clone();
    let retrieved = storage
        .get("user123", ProviderType::Emby, &server_id)
        .await
        .unwrap()
        .expect("emby credential should exist");

    let (h, api_key, emby_user_id) = retrieved
        .data
        .as_emby()
        .expect("Expected Emby credential data");
    assert_eq!(h, host);
    assert_eq!(api_key, "api_token_abc123");
    assert_eq!(emby_user_id, "emby_user_id_456");
    // These would be passed to EmbyService methods
}

/// Test credential update pattern (e.g., after token refresh).
#[tokio::test]
async fn test_credential_update_pattern() {
    let storage = InMemoryCredentialStorage::new();

    // Initial credential
    let mut cookies = HashMap::new();
    cookies.insert("SESSDATA".to_string(), "old_session".to_string());

    storage
        .set("user123", None, CredentialData::bilibili(cookies))
        .await
        .unwrap();

    // Simulate token refresh - update with new cookies
    let mut new_cookies = HashMap::new();
    new_cookies.insert("SESSDATA".to_string(), "new_session".to_string());
    new_cookies.insert("refreshed".to_string(), "true".to_string());

    storage
        .set(
            "user123",
            None,
            CredentialData::bilibili(new_cookies.clone()),
        )
        .await
        .unwrap();

    // Should have updated, not created new
    let all = storage.list_by_user("user123").await.unwrap();
    assert_eq!(all.len(), 1);

    // Verify updated data
    let cookies = all[0]
        .data
        .as_bilibili()
        .expect("Expected Bilibili credential data");
    assert_eq!(cookies.get("SESSDATA"), Some(&"new_session".to_string()));
    assert_eq!(cookies.get("refreshed"), Some(&"true".to_string()));
}

/// Test credential deletion pattern (e.g., on logout).
#[tokio::test]
async fn test_credential_logout_pattern() {
    let storage = InMemoryCredentialStorage::new();

    // User logs in and stores credentials
    let mut cookies = HashMap::new();
    cookies.insert("SESSDATA".to_string(), "session_value".to_string());

    let cred = storage
        .set("user123", None, CredentialData::bilibili(cookies))
        .await
        .unwrap();

    assert!(storage
        .exists("user123", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap());

    // User logs out - delete credentials
    let deleted = storage
        .delete("user123", ProviderType::Bilibili, &cred.server_id)
        .await
        .unwrap();
    assert!(deleted);

    // Verify credential is gone
    assert!(!storage
        .exists("user123", ProviderType::Bilibili, "bilibili")
        .await
        .unwrap());
}

/// Test listing credentials for UI display (e.g., "My Connected Accounts").
#[tokio::test]
async fn test_list_credentials_for_ui() {
    let storage = InMemoryCredentialStorage::new();

    // User connects multiple providers
    storage
        .set(
            "user123",
            Some("My Bilibili"),
            CredentialData::bilibili(HashMap::new()),
        )
        .await
        .unwrap();

    storage
        .set(
            "user123",
            Some("Personal Alist"),
            CredentialData::alist(
                "https://alist.example.com".into(),
                "admin".into(),
                "pass".into(),
            ),
        )
        .await
        .unwrap();

    storage
        .set(
            "user123",
            Some("Family Emby"),
            CredentialData::emby(
                "https://emby.example.com".into(),
                "key".into(),
                "uid".into(),
            ),
        )
        .await
        .unwrap();

    // Another user
    storage
        .set("user456", None, CredentialData::bilibili(HashMap::new()))
        .await
        .unwrap();

    // List user123's credentials
    let user123_creds = storage.list_by_user("user123").await.unwrap();
    assert_eq!(user123_creds.len(), 3);

    // Verify instance names are preserved
    let instance_names: std::collections::HashSet<_> = user123_creds
        .iter()
        .map(|c| c.provider_instance_name.clone().unwrap_or_default())
        .collect();
    assert!(instance_names.contains("My Bilibili"));
    assert!(instance_names.contains("Personal Alist"));
    assert!(instance_names.contains("Family Emby"));
}

/// Test that `StoredCredential` is Clone and can be shared across threads.
#[tokio::test]
async fn test_stored_credential_thread_safety() {
    use std::sync::Arc;

    let storage = Arc::new(InMemoryCredentialStorage::new());

    // Store a credential
    storage
        .set("user123", None, CredentialData::bilibili(HashMap::new()))
        .await
        .unwrap();

    // Retrieve from multiple concurrent tasks
    let mut handles = vec![];

    for _ in 0..5 {
        let s = storage.clone();
        handles.push(tokio::spawn(async move {
            let cred = s
                .get("user123", ProviderType::Bilibili, "bilibili")
                .await
                .unwrap();
            cred.expect("credential should exist")
        }));
    }

    // All tasks should get the same credential
    let results: Vec<_> = futures::future::join_all(handles).await;
    let first_id = results[0].as_ref().unwrap().id.clone();
    for result in results {
        assert_eq!(result.unwrap().id, first_id);
    }
}
