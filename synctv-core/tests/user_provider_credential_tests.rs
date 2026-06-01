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
    credential_encryption::CredentialEncryption,
    models::{ProviderInstance, ProviderType, SignupMethod, User, UserId, UserProviderCredential},
    repository::{ProviderInstanceRepository, UserProviderCredentialRepository, UserRepository},
};
use synctv_core_testing::create_test_pool;

fn test_encryption() -> CredentialEncryption {
    CredentialEncryption::new(&[0x42; 32]).unwrap()
}
fn make_user(username: &str) -> User {
    User::new(username.to_string(), SignupMethod::Email)
}

fn bilibili_server_id() -> String {
    UserProviderCredential::bilibili_server_id()
}

fn provider_code(provider: ProviderType) -> i16 {
    provider.as_i16()
}

fn make_credential(user_id: UserId, provider: &str, server_id: &str) -> UserProviderCredential {
    let now = Utc::now();
    UserProviderCredential {
        id: 0,
        user_id,
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

fn make_credential_with_instance(
    user_id: UserId,
    provider: &str,
    server_id: &str,
    provider_instance_name: Option<&str>,
) -> UserProviderCredential {
    let mut credential = make_credential(user_id, provider, server_id);
    credential.provider_instance_name = provider_instance_name.map(ToString::to_string);
    credential
}

fn make_provider_instance(name: &str, providers: &[&str]) -> ProviderInstance {
    ProviderInstance {
        name: name.to_string(),
        endpoint: format!("http://{name}.example.com:50051"),
        comment: None,
        jwt_secret: None,
        custom_ca: None,
        timeout: "10s".to_string(),
        tls: false,
        insecure_tls: false,
        providers: providers
            .iter()
            .map(|provider| (*provider).to_string())
            .collect(),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_credential() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo.create(&make_user("cred_user1")).await.unwrap();

    let bilibili_server_id = bilibili_server_id();
    let cred = make_credential(user.id, "bilibili", &bilibili_server_id);
    cred_repo.create(&cred).await.unwrap();

    // Retrieve it
    let found = cred_repo
        .get_by_provider_and_server(user.id, "bilibili", &bilibili_server_id)
        .await
        .unwrap();

    assert!(found.is_some());
    let found = found.unwrap();
    assert_eq!(found.user_id, user.id);
    assert_eq!(found.provider, "bilibili");
    assert_eq!(found.server_id, bilibili_server_id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_multiple_providers() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("multi_cred_user"))
        .await
        .unwrap();

    let bilibili = make_credential(user.id, "bilibili", &bilibili_server_id());
    let alist = make_credential(user.id, "alist", "server1_hash");
    let emby = make_credential(user.id, "emby", "server2_hash");

    cred_repo.create(&bilibili).await.unwrap();
    cred_repo.create(&alist).await.unwrap();
    cred_repo.create(&emby).await.unwrap();

    // List all
    let all = cred_repo.get_by_user(user.id).await.unwrap();
    assert_eq!(all.len(), 3);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo.create(&make_user("getbyid_user")).await.unwrap();
    let bilibili_server_id = bilibili_server_id();
    let cred = make_credential(user.id, "bilibili", &bilibili_server_id);
    let cred = cred_repo.create(&cred).await.unwrap();

    let found = cred_repo.get_by_id(cred.id).await.unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, cred.id);

    // Non-existent ID
    let not_found = cred_repo.get_by_id(i64::MAX).await.unwrap();
    assert!(not_found.is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_provider() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("getbyprov_user"))
        .await
        .unwrap();

    let alist1 = make_credential(user.id, "alist", "server1");
    let alist2 = make_credential(user.id, "alist", "server2");
    let bilibili = make_credential(user.id, "bilibili", &bilibili_server_id());

    cred_repo.create(&alist1).await.unwrap();
    cred_repo.create(&alist2).await.unwrap();
    cred_repo.create(&bilibili).await.unwrap();

    // Get only Alist
    let alist_creds = cred_repo.get_by_provider(user.id, "alist").await.unwrap();
    assert_eq!(alist_creds.len(), 2);

    // Get only Bilibili
    let bilibili_creds = cred_repo
        .get_by_provider(user.id, "bilibili")
        .await
        .unwrap();
    assert_eq!(bilibili_creds.len(), 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_credential() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo.create(&make_user("update_user")).await.unwrap();
    let bilibili_server_id = bilibili_server_id();
    let mut cred = make_credential(user.id, "bilibili", &bilibili_server_id);
    cred = cred_repo.create(&cred).await.unwrap();

    // Update credential data
    cred.credential_data = json!({
        "type": "bilibili",
        "cookies": {"SESSDATA": "new_session_value"}
    });
    cred_repo.update(&cred).await.unwrap();

    // Verify update
    let found = cred_repo
        .get_by_provider_and_server(user.id, "bilibili", &bilibili_server_id)
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
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo.create(&make_user("expire_user")).await.unwrap();
    let bilibili_server_id = bilibili_server_id();
    let mut cred = make_credential(user.id, "bilibili", &bilibili_server_id);
    cred = cred_repo.create(&cred).await.unwrap();

    // Set expiration
    let expires = Utc::now() + Duration::hours(24);
    cred.expires_at = Some(expires);
    cred_repo.update(&cred).await.unwrap();

    // Verify expiration
    let found = cred_repo
        .get_by_provider_and_server(user.id, "bilibili", &bilibili_server_id)
        .await
        .unwrap()
        .unwrap();

    assert!(found.expires_at.is_some());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_credential() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo.create(&make_user("delete_user")).await.unwrap();
    let cred = make_credential(user.id, "bilibili", &bilibili_server_id());
    let cred = cred_repo.create(&cred).await.unwrap();

    // Delete
    cred_repo.delete(cred.id).await.unwrap();

    // Verify deleted
    let found = cred_repo.get_by_id(cred.id).await.unwrap();
    assert!(found.is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_by_user_and_provider() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo.create(&make_user("delprov_user")).await.unwrap();

    let alist1 = make_credential(user.id, "alist", "server1");
    let alist2 = make_credential(user.id, "alist", "server2");
    let bilibili = make_credential(user.id, "bilibili", &bilibili_server_id());

    cred_repo.create(&alist1).await.unwrap();
    cred_repo.create(&alist2).await.unwrap();
    cred_repo.create(&bilibili).await.unwrap();

    // Delete all Alist
    cred_repo
        .delete_by_user_and_provider(user.id, "alist")
        .await
        .unwrap();

    // Verify Alist deleted but Bilibili remains
    let alist = cred_repo.get_by_provider(user.id, "alist").await.unwrap();
    assert!(alist.is_empty());

    let bilibili = cred_repo
        .get_by_provider(user.id, "bilibili")
        .await
        .unwrap();
    assert_eq!(bilibili.len(), 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_unique_constraint_user_provider_server() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo.create(&make_user("unique_user")).await.unwrap();

    let bilibili_server_id = bilibili_server_id();
    let cred1 = make_credential(user.id, "bilibili", &bilibili_server_id);
    cred_repo.create(&cred1).await.unwrap();

    // Try to create duplicate (same user + provider + server_id)
    let cred2 = make_credential(user.id, "bilibili", &bilibili_server_id);
    let result = cred_repo.create(&cred2).await;

    assert!(result.is_err(), "Should fail due to unique constraint");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_same_provider_host_can_be_stored_for_different_instances() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let provider_repo =
        ProviderInstanceRepository::new_with_encryption(pool.clone(), test_encryption());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("instance_scoped_user"))
        .await
        .unwrap();

    provider_repo
        .create(&make_provider_instance("alist-main", &["alist"]))
        .await
        .unwrap();
    provider_repo
        .create(&make_provider_instance("alist-backup", &["alist"]))
        .await
        .unwrap();

    let server_main = UserProviderCredential::generate_server_id_for_instance(
        "https://alist.example.com",
        Some("alist-main"),
    );
    let server_backup = UserProviderCredential::generate_server_id_for_instance(
        "https://alist.example.com",
        Some("alist-backup"),
    );

    let main = make_credential_with_instance(user.id, "alist", &server_main, Some("alist-main"));
    let backup =
        make_credential_with_instance(user.id, "alist", &server_backup, Some("alist-backup"));

    cred_repo.create(&main).await.unwrap();
    cred_repo.create(&backup).await.unwrap();

    let all = cred_repo.get_by_provider(user.id, "alist").await.unwrap();
    assert_eq!(all.len(), 2);

    let main_found = cred_repo
        .get_by_provider_and_server(user.id, "alist", &server_main)
        .await
        .unwrap()
        .expect("main credential should exist");
    let backup_found = cred_repo
        .get_by_provider_and_server(user.id, "alist", &server_backup)
        .await
        .unwrap()
        .expect("backup credential should exist");

    assert_eq!(
        main_found.provider_instance_name.as_deref(),
        Some("alist-main")
    );
    assert_eq!(
        backup_found.provider_instance_name.as_deref(),
        Some("alist-backup")
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_provider_instance_cascades_instance_credentials() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let provider_repo =
        ProviderInstanceRepository::new_with_encryption(pool.clone(), test_encryption());
    let cred_repo =
        UserProviderCredentialRepository::new_with_encryption(pool.clone(), test_encryption());

    let user = user_repo
        .create(&make_user("instance_delete_user"))
        .await
        .unwrap();
    provider_repo
        .create(&make_provider_instance("alist-delete-me", &["alist"]))
        .await
        .unwrap();

    let server_id = UserProviderCredential::generate_server_id_for_instance(
        "https://alist-delete.example.com",
        Some("alist-delete-me"),
    );
    let credential =
        make_credential_with_instance(user.id, "alist", &server_id, Some("alist-delete-me"));
    let credential = cred_repo.create(&credential).await.unwrap();

    provider_repo.delete("alist-delete-me").await.unwrap();

    let found = cred_repo.get_by_id(credential.id).await.unwrap();
    assert!(
        found.is_none(),
        "deleting a provider instance must remove credentials bound to that instance"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_blank_provider_instance_name_is_normalized_to_null() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo =
        UserProviderCredentialRepository::new_with_encryption(pool.clone(), test_encryption());

    let user = user_repo
        .create(&make_user("blank_instance_user"))
        .await
        .unwrap();

    let server_id =
        UserProviderCredential::generate_server_id_for_instance("https://alist.example.com", None);
    let credential = make_credential_with_instance(user.id, "alist", &server_id, Some("   "));

    let credential = cred_repo.create(&credential).await.unwrap();

    let stored: Option<Option<String>> = sqlx::query_scalar(
        "SELECT provider_instance_name FROM user_media_provider_credentials WHERE id = $1",
    )
    .bind(credential.id)
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(stored, Some(None));

    let found = cred_repo
        .get_by_provider_and_server(user.id, "alist", &server_id)
        .await
        .unwrap()
        .expect("credential should exist");
    assert_eq!(found.provider_instance_name, None);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_upsert_by_user_provider_server_replaces_existing_credential_atomically() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo =
        UserProviderCredentialRepository::new_with_encryption(pool.clone(), test_encryption());

    let user = user_repo
        .create(&make_user("credential_upsert_user"))
        .await
        .unwrap();
    let server_id = bilibili_server_id();
    let first = make_credential(user.id, "bilibili", &server_id);
    let first = cred_repo
        .upsert_by_user_provider_server(&first)
        .await
        .unwrap();

    let mut replacement = make_credential(user.id, "bilibili", &server_id);
    replacement.credential_data = json!({
        "type": "bilibili",
        "cookies": {"SESSDATA": "replacement_session"}
    });
    cred_repo
        .upsert_by_user_provider_server(&replacement)
        .await
        .unwrap();

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_media_provider_credentials WHERE user_id = $1 AND provider = $2 AND server_id = $3",
    )
    .bind(user.id)
    .bind(provider_code(ProviderType::Bilibili))
    .bind(&server_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);

    let found = cred_repo
        .get_by_provider_and_server(user.id, "bilibili", &server_id)
        .await
        .unwrap()
        .expect("upserted credential should exist");
    assert_eq!(
        found.id, first.id,
        "upsert should keep the stable credential id"
    );
    assert_eq!(
        found.credential_data["cookies"]["SESSDATA"],
        "replacement_session"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_credentials_deleted_when_user_deleted() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo.create(&make_user("cascade_user")).await.unwrap();

    let cred = make_credential(user.id, "bilibili", &bilibili_server_id());
    let cred = cred_repo.create(&cred).await.unwrap();

    // Delete user (soft delete first, then hard delete would cascade)
    // Note: Soft delete does NOT cascade delete credentials
    user_repo.delete(&user.id).await.unwrap();

    // Credentials should still exist (soft delete)
    let found = cred_repo.get_by_id(cred.id).await.unwrap();
    // The credentials remain in DB even after user soft-delete
    // because the FK constraint uses ON DELETE CASCADE for hard delete only
    assert!(found.is_some());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_expired_credentials() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("expire_del_user"))
        .await
        .unwrap();

    let mut expired = make_credential(user.id, "alist", "expired_server");
    expired.expires_at = Some(Utc::now() - Duration::hours(1));
    cred_repo.create(&expired).await.unwrap();

    let mut valid = make_credential(user.id, "bilibili", &bilibili_server_id());
    valid.expires_at = Some(Utc::now() + Duration::hours(1));
    cred_repo.create(&valid).await.unwrap();

    // Delete expired
    let deleted = cred_repo.delete_expired().await.unwrap();
    assert_eq!(deleted, 1);

    // Verify only valid remains
    let all = cred_repo.get_by_user(user.id).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].provider, "bilibili");
}
