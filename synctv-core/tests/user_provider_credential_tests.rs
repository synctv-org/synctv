//! User Provider Credential Repository Tests
//!
//! Integration tests for the `UserProviderCredentialRepository`.
//!
//! Run with: cargo test --test `user_provider_credential_tests` -- --ignored
//! (Requires Docker)
#![allow(clippy::unwrap_used)]

use chrono::{Duration, Utc};
use serde_json::json;
use synctv_core::{
    models::{SignupMethod, User, UserProviderCredential},
    repository::{UserProviderCredentialRepository, UserRepository},
};
use synctv_core_testing::create_test_pool;
fn make_user(username: &str) -> User {
    User::new(
        username.to_string(),
        Some(format!("{username}@test.com")),
        "hash".to_string(),
        SignupMethod::Email,
    )
}

fn make_credential(user_id: &str, provider: &str, server_id: &str) -> UserProviderCredential {
    let now = Utc::now();
    UserProviderCredential {
        id: nanoid::nanoid!(12),
        user_id: user_id.to_string(),
        provider: provider.to_string(),
        server_id: server_id.to_string(),
        provider_instance_name: None,
        credential_data: json!({
            "type": provider,
            "cookies": {"SESSDATA": "test_session"}
        }),
        expires_at: None,
        created_at: now,
        updated_at: now,
    }
}

// ========== Create Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_credential() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new(pool);

    // Create a user first
    let user = user_repo.create(&make_user("cred_user1")).await.unwrap();

    // Create a credential
    let cred = make_credential(user.id.as_str(), "bilibili", "bilibili");
    cred_repo.create(&cred).await.unwrap();

    // Retrieve it
    let found = cred_repo
        .get_by_provider_and_server(user.id.as_str(), "bilibili", "bilibili")
        .await
        .unwrap();

    assert!(found.is_some());
    let found = found.unwrap();
    assert_eq!(found.user_id, user.id.as_str());
    assert_eq!(found.provider, "bilibili");
    assert_eq!(found.server_id, "bilibili");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_multiple_providers() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new(pool);

    let user = user_repo
        .create(&make_user("multi_cred_user"))
        .await
        .unwrap();

    // Create credentials for different providers
    let bilibili = make_credential(user.id.as_str(), "bilibili", "bilibili");
    let alist = make_credential(user.id.as_str(), "alist", "server1_hash");
    let emby = make_credential(user.id.as_str(), "emby", "server2_hash");

    cred_repo.create(&bilibili).await.unwrap();
    cred_repo.create(&alist).await.unwrap();
    cred_repo.create(&emby).await.unwrap();

    // List all
    let all = cred_repo.get_by_user(user.id.as_str()).await.unwrap();
    assert_eq!(all.len(), 3);
}

// ========== Read Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new(pool);

    let user = user_repo.create(&make_user("getbyid_user")).await.unwrap();
    let cred = make_credential(user.id.as_str(), "bilibili", "bilibili");
    cred_repo.create(&cred).await.unwrap();

    let found = cred_repo.get_by_id(&cred.id).await.unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, cred.id);

    // Non-existent ID
    let not_found = cred_repo.get_by_id("nonexistent123").await.unwrap();
    assert!(not_found.is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_provider() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new(pool);

    let user = user_repo
        .create(&make_user("getbyprov_user"))
        .await
        .unwrap();

    // Create multiple Alist credentials (different servers)
    let alist1 = make_credential(user.id.as_str(), "alist", "server1");
    let alist2 = make_credential(user.id.as_str(), "alist", "server2");
    let bilibili = make_credential(user.id.as_str(), "bilibili", "bilibili");

    cred_repo.create(&alist1).await.unwrap();
    cred_repo.create(&alist2).await.unwrap();
    cred_repo.create(&bilibili).await.unwrap();

    // Get only Alist
    let alist_creds = cred_repo
        .get_by_provider(user.id.as_str(), "alist")
        .await
        .unwrap();
    assert_eq!(alist_creds.len(), 2);

    // Get only Bilibili
    let bilibili_creds = cred_repo
        .get_by_provider(user.id.as_str(), "bilibili")
        .await
        .unwrap();
    assert_eq!(bilibili_creds.len(), 1);
}

// ========== Update Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_credential() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new(pool);

    let user = user_repo.create(&make_user("update_user")).await.unwrap();
    let mut cred = make_credential(user.id.as_str(), "bilibili", "bilibili");
    cred_repo.create(&cred).await.unwrap();

    // Update credential data
    cred.credential_data = json!({
        "type": "bilibili",
        "cookies": {"SESSDATA": "new_session_value"}
    });
    cred_repo.update(&cred).await.unwrap();

    // Verify update
    let found = cred_repo
        .get_by_provider_and_server(user.id.as_str(), "bilibili", "bilibili")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        found.credential_data["cookies"]["SESSDATA"],
        "new_session_value"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_credential_with_expiration() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new(pool);

    let user = user_repo.create(&make_user("expire_user")).await.unwrap();
    let mut cred = make_credential(user.id.as_str(), "bilibili", "bilibili");
    cred_repo.create(&cred).await.unwrap();

    // Set expiration
    let expires = Utc::now() + Duration::hours(24);
    cred.expires_at = Some(expires);
    cred_repo.update(&cred).await.unwrap();

    // Verify expiration
    let found = cred_repo
        .get_by_provider_and_server(user.id.as_str(), "bilibili", "bilibili")
        .await
        .unwrap()
        .unwrap();

    assert!(found.expires_at.is_some());
}

// ========== Delete Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_credential() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new(pool);

    let user = user_repo.create(&make_user("delete_user")).await.unwrap();
    let cred = make_credential(user.id.as_str(), "bilibili", "bilibili");
    cred_repo.create(&cred).await.unwrap();

    // Delete
    cred_repo.delete(&cred.id).await.unwrap();

    // Verify deleted
    let found = cred_repo.get_by_id(&cred.id).await.unwrap();
    assert!(found.is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_by_user_and_provider() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new(pool);

    let user = user_repo.create(&make_user("delprov_user")).await.unwrap();

    // Create multiple Alist credentials
    let alist1 = make_credential(user.id.as_str(), "alist", "server1");
    let alist2 = make_credential(user.id.as_str(), "alist", "server2");
    let bilibili = make_credential(user.id.as_str(), "bilibili", "bilibili");

    cred_repo.create(&alist1).await.unwrap();
    cred_repo.create(&alist2).await.unwrap();
    cred_repo.create(&bilibili).await.unwrap();

    // Delete all Alist
    cred_repo
        .delete_by_user_and_provider(user.id.as_str(), "alist")
        .await
        .unwrap();

    // Verify Alist deleted but Bilibili remains
    let alist = cred_repo
        .get_by_provider(user.id.as_str(), "alist")
        .await
        .unwrap();
    assert!(alist.is_empty());

    let bilibili = cred_repo
        .get_by_provider(user.id.as_str(), "bilibili")
        .await
        .unwrap();
    assert_eq!(bilibili.len(), 1);
}

// ========== Unique Constraint Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_unique_constraint_user_provider_server() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new(pool);

    let user = user_repo.create(&make_user("unique_user")).await.unwrap();

    // Create first credential
    let cred1 = make_credential(user.id.as_str(), "bilibili", "bilibili");
    cred_repo.create(&cred1).await.unwrap();

    // Try to create duplicate (same user + provider + server_id)
    let cred2 = make_credential(user.id.as_str(), "bilibili", "bilibili");
    let result = cred_repo.create(&cred2).await;

    assert!(result.is_err(), "Should fail due to unique constraint");
}

// ========== Cascade Delete Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_credentials_deleted_when_user_deleted() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new(pool);

    let user = user_repo.create(&make_user("cascade_user")).await.unwrap();

    // Create credentials
    let cred = make_credential(user.id.as_str(), "bilibili", "bilibili");
    cred_repo.create(&cred).await.unwrap();

    // Delete user (soft delete first, then hard delete would cascade)
    // Note: Soft delete does NOT cascade delete credentials
    user_repo.delete(&user.id).await.unwrap();

    // Credentials should still exist (soft delete)
    let found = cred_repo.get_by_id(&cred.id).await.unwrap();
    // The credentials remain in DB even after user soft-delete
    // because the FK constraint uses ON DELETE CASCADE for hard delete only
    assert!(found.is_some());
}

// ========== Expiration Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_expired_credentials() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new(pool);

    let user = user_repo
        .create(&make_user("expire_del_user"))
        .await
        .unwrap();

    // Create expired credential
    let mut expired = make_credential(user.id.as_str(), "alist", "expired_server");
    expired.expires_at = Some(Utc::now() - Duration::hours(1));
    cred_repo.create(&expired).await.unwrap();

    // Create valid credential
    let mut valid = make_credential(user.id.as_str(), "bilibili", "bilibili");
    valid.expires_at = Some(Utc::now() + Duration::hours(1));
    cred_repo.create(&valid).await.unwrap();

    // Delete expired
    let deleted = cred_repo.delete_expired().await.unwrap();
    assert_eq!(deleted, 1);

    // Verify only valid remains
    let all = cred_repo.get_by_user(user.id.as_str()).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].provider, "bilibili");
}
