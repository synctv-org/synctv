//! User Provider Credential Repository Tests
//!
//! Integration tests for the `UserProviderCredentialRepository`.
//!
//! (Requires Docker)

use chrono::{Duration, Utc};
use synctv_core::{
    credential_encryption::CredentialEncryption,
    models::{
        ProviderCredential, ProviderInstance, ProviderType, SignupMethod, SourceProvider, User,
        UserId, UserProviderCredential,
    },
    provider::{AlistProvider, BilibiliProvider},
    repository::{ProviderInstanceRepository, UserProviderCredentialRepository, UserRepository},
};
use synctv_core_testing::create_test_pool;
use synctv_core_testing::{TestOptionExt, TestResultExt};

fn test_encryption() -> CredentialEncryption {
    CredentialEncryption::new(&[0x42; 32]).checked("test encryption should be created")
}
fn make_user(username: &str) -> User {
    User::new(username.to_string(), SignupMethod::Email)
}

fn bilibili_server_id() -> String {
    BilibiliProvider::credential_server_id()
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
        credential_data: match provider {
            "bilibili" => ProviderCredential::Bilibili {
                cookies: std::collections::HashMap::from([(
                    "SESSDATA".to_string(),
                    "test_session".to_string(),
                )]),
            },
            "alist" => ProviderCredential::Alist {
                host: "https://alist.example.com".to_string(),
                username: "alice".to_string(),
                password: "hashed_password".to_string(),
                otp_secret: None,
            },
            "emby" => ProviderCredential::Emby {
                host: "https://emby.example.com".to_string(),
                api_key: "api_key".to_string(),
                emby_user_id: "emby_user".to_string(),
            },
            _ => ProviderCredential::default(),
        },
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
            .map(|provider| {
                provider
                    .parse::<SourceProvider>()
                    .checked("test provider should be known")
            })
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

    let user = user_repo
        .create(&make_user("cred_user1"))
        .await
        .checked("test operation should succeed");

    let bilibili_server_id = bilibili_server_id();
    let cred = make_credential(user.id, "bilibili", &bilibili_server_id);
    cred_repo
        .create(&cred)
        .await
        .checked("test operation should succeed");

    // Retrieve it
    let found = cred_repo
        .get_by_provider_and_server(user.id, "bilibili", &bilibili_server_id)
        .await
        .checked("test operation should succeed");

    assert!(found.is_some());
    let found = found.checked("test operation should succeed");
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
        .checked("test operation should succeed");

    let bilibili = make_credential(user.id, "bilibili", &bilibili_server_id());
    let alist = make_credential(user.id, "alist", "server1_hash");
    let emby = make_credential(user.id, "emby", "server2_hash");

    cred_repo
        .create(&bilibili)
        .await
        .checked("test operation should succeed");
    cred_repo
        .create(&alist)
        .await
        .checked("test operation should succeed");
    cred_repo
        .create(&emby)
        .await
        .checked("test operation should succeed");

    // List all
    let all = cred_repo
        .get_by_user(user.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(all.len(), 3);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("getbyid_user"))
        .await
        .checked("test operation should succeed");
    let bilibili_server_id = bilibili_server_id();
    let cred = make_credential(user.id, "bilibili", &bilibili_server_id);
    let cred = cred_repo
        .create(&cred)
        .await
        .checked("test operation should succeed");

    let found = cred_repo
        .get_by_id(cred.id)
        .await
        .checked("test operation should succeed");
    assert!(found.is_some());
    assert_eq!(found.checked("test operation should succeed").id, cred.id);

    // Non-existent ID
    let not_found = cred_repo
        .get_by_id(i64::MAX)
        .await
        .checked("test operation should succeed");
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
        .checked("test operation should succeed");

    let alist1 = make_credential(user.id, "alist", "server1");
    let alist2 = make_credential(user.id, "alist", "server2");
    let bilibili = make_credential(user.id, "bilibili", &bilibili_server_id());

    cred_repo
        .create(&alist1)
        .await
        .checked("test operation should succeed");
    cred_repo
        .create(&alist2)
        .await
        .checked("test operation should succeed");
    cred_repo
        .create(&bilibili)
        .await
        .checked("test operation should succeed");

    // Get only Alist
    let alist_creds = cred_repo
        .get_by_provider(user.id, "alist")
        .await
        .checked("test operation should succeed");
    assert_eq!(alist_creds.len(), 2);

    // Get only Bilibili
    let bilibili_creds = cred_repo
        .get_by_provider(user.id, "bilibili")
        .await
        .checked("test operation should succeed");
    assert_eq!(bilibili_creds.len(), 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_credential() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("update_user"))
        .await
        .checked("test operation should succeed");
    let bilibili_server_id = bilibili_server_id();
    let mut cred = make_credential(user.id, "bilibili", &bilibili_server_id);
    cred = cred_repo
        .create(&cred)
        .await
        .checked("test operation should succeed");

    // Update credential data
    cred.credential_data = ProviderCredential::Bilibili {
        cookies: std::collections::HashMap::from([(
            "SESSDATA".to_string(),
            "new_session_value".to_string(),
        )]),
    };
    cred_repo
        .update(&cred)
        .await
        .checked("test operation should succeed");

    // Verify update
    let found = cred_repo
        .get_by_provider_and_server(user.id, "bilibili", &bilibili_server_id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");

    let ProviderCredential::Bilibili { cookies } = found.credential_data else {
        panic!("expected bilibili credential");
    };
    assert_eq!(
        cookies.get("SESSDATA").map(String::as_str),
        Some("new_session_value")
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_credential_with_expiration() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("expire_user"))
        .await
        .checked("test operation should succeed");
    let bilibili_server_id = bilibili_server_id();
    let mut cred = make_credential(user.id, "bilibili", &bilibili_server_id);
    cred = cred_repo
        .create(&cred)
        .await
        .checked("test operation should succeed");

    // Set expiration
    let expires = Utc::now() + Duration::hours(24);
    cred.expires_at = Some(expires);
    cred_repo
        .update(&cred)
        .await
        .checked("test operation should succeed");

    // Verify expiration
    let found = cred_repo
        .get_by_provider_and_server(user.id, "bilibili", &bilibili_server_id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");

    assert!(found.expires_at.is_some());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_credential() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("delete_user"))
        .await
        .checked("test operation should succeed");
    let cred = make_credential(user.id, "bilibili", &bilibili_server_id());
    let cred = cred_repo
        .create(&cred)
        .await
        .checked("test operation should succeed");

    // Delete
    cred_repo
        .delete(cred.id)
        .await
        .checked("test operation should succeed");

    // Verify deleted
    let found = cred_repo
        .get_by_id(cred.id)
        .await
        .checked("test operation should succeed");
    assert!(found.is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_by_user_and_provider() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("delprov_user"))
        .await
        .checked("test operation should succeed");

    let alist1 = make_credential(user.id, "alist", "server1");
    let alist2 = make_credential(user.id, "alist", "server2");
    let bilibili = make_credential(user.id, "bilibili", &bilibili_server_id());

    cred_repo
        .create(&alist1)
        .await
        .checked("test operation should succeed");
    cred_repo
        .create(&alist2)
        .await
        .checked("test operation should succeed");
    cred_repo
        .create(&bilibili)
        .await
        .checked("test operation should succeed");

    // Delete all Alist
    cred_repo
        .delete_by_user_and_provider(user.id, "alist")
        .await
        .checked("test operation should succeed");

    // Verify Alist deleted but Bilibili remains
    let alist = cred_repo
        .get_by_provider(user.id, "alist")
        .await
        .checked("test operation should succeed");
    assert!(alist.is_empty());

    let bilibili = cred_repo
        .get_by_provider(user.id, "bilibili")
        .await
        .checked("test operation should succeed");
    assert_eq!(bilibili.len(), 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_unique_constraint_user_provider_server() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("unique_user"))
        .await
        .checked("test operation should succeed");

    let bilibili_server_id = bilibili_server_id();
    let cred1 = make_credential(user.id, "bilibili", &bilibili_server_id);
    cred_repo
        .create(&cred1)
        .await
        .checked("test operation should succeed");

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
        .checked("test operation should succeed");

    provider_repo
        .create(&make_provider_instance("alist-main", &["alist"]))
        .await
        .checked("test operation should succeed");
    provider_repo
        .create(&make_provider_instance("alist-backup", &["alist"]))
        .await
        .checked("test operation should succeed");

    let server_main = AlistProvider::credential_server_id_for_instance(
        "https://alist.example.com",
        Some("alist-main"),
    );
    let server_backup = AlistProvider::credential_server_id_for_instance(
        "https://alist.example.com",
        Some("alist-backup"),
    );

    let main = make_credential_with_instance(user.id, "alist", &server_main, Some("alist-main"));
    let backup =
        make_credential_with_instance(user.id, "alist", &server_backup, Some("alist-backup"));

    cred_repo
        .create(&main)
        .await
        .checked("test operation should succeed");
    cred_repo
        .create(&backup)
        .await
        .checked("test operation should succeed");

    let all = cred_repo
        .get_by_provider(user.id, "alist")
        .await
        .checked("test operation should succeed");
    assert_eq!(all.len(), 2);

    let main_found = cred_repo
        .get_by_provider_and_server(user.id, "alist", &server_main)
        .await
        .checked("test operation should succeed")
        .checked("main credential should exist");
    let backup_found = cred_repo
        .get_by_provider_and_server(user.id, "alist", &server_backup)
        .await
        .checked("test operation should succeed")
        .checked("backup credential should exist");

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
        .checked("test operation should succeed");
    provider_repo
        .create(&make_provider_instance("alist-delete-me", &["alist"]))
        .await
        .checked("test operation should succeed");

    let server_id = AlistProvider::credential_server_id_for_instance(
        "https://alist-delete.example.com",
        Some("alist-delete-me"),
    );
    let credential =
        make_credential_with_instance(user.id, "alist", &server_id, Some("alist-delete-me"));
    let credential = cred_repo
        .create(&credential)
        .await
        .checked("test operation should succeed");

    provider_repo
        .delete("alist-delete-me")
        .await
        .checked("test operation should succeed");

    let found = cred_repo
        .get_by_id(credential.id)
        .await
        .checked("test operation should succeed");
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
        .checked("test operation should succeed");

    let server_id =
        AlistProvider::credential_server_id_for_instance("https://alist.example.com", None);
    let credential = make_credential_with_instance(user.id, "alist", &server_id, Some("   "));

    let credential = cred_repo
        .create(&credential)
        .await
        .checked("test operation should succeed");

    let stored: Option<Option<String>> = sqlx::query_scalar!(
        "SELECT provider_instance_name FROM user_media_provider_credentials WHERE id = $1",
        credential.id
    )
    .fetch_optional(&pool)
    .await
    .checked("test operation should succeed");
    assert_eq!(stored, Some(None));

    let found = cred_repo
        .get_by_provider_and_server(user.id, "alist", &server_id)
        .await
        .checked("test operation should succeed")
        .checked("credential should exist");
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
        .checked("test operation should succeed");
    let server_id = bilibili_server_id();
    let first = make_credential(user.id, "bilibili", &server_id);
    let first = cred_repo
        .upsert_by_user_provider_server(&first)
        .await
        .checked("test operation should succeed");

    let mut replacement = make_credential(user.id, "bilibili", &server_id);
    replacement.credential_data = ProviderCredential::Bilibili {
        cookies: std::collections::HashMap::from([(
            "SESSDATA".to_string(),
            "replacement_session".to_string(),
        )]),
    };
    cred_repo
        .upsert_by_user_provider_server(&replacement)
        .await
        .checked("test operation should succeed");

    let count: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM user_media_provider_credentials WHERE user_id = $1 AND provider = $2 AND server_id = $3"#,
        user.id.as_i64(),
        provider_code(ProviderType::Bilibili),
        &server_id
    )
    .fetch_one(&pool)
    .await
    .checked("test operation should succeed");
    assert_eq!(count, 1);

    let found = cred_repo
        .get_by_provider_and_server(user.id, "bilibili", &server_id)
        .await
        .checked("test operation should succeed")
        .checked("upserted credential should exist");
    assert_eq!(
        found.id, first.id,
        "upsert should keep the stable credential id"
    );
    let ProviderCredential::Bilibili { cookies } = found.credential_data else {
        panic!("expected bilibili credential");
    };
    assert_eq!(
        cookies.get("SESSDATA").map(String::as_str),
        Some("replacement_session")
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_credentials_deleted_when_user_deleted() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let cred_repo = UserProviderCredentialRepository::new_with_encryption(pool, test_encryption());

    let user = user_repo
        .create(&make_user("cascade_user"))
        .await
        .checked("test operation should succeed");

    let cred = make_credential(user.id, "bilibili", &bilibili_server_id());
    let cred = cred_repo
        .create(&cred)
        .await
        .checked("test operation should succeed");

    // Delete user (soft delete first, then hard delete would cascade)
    // Note: Soft delete does NOT cascade delete credentials
    user_repo
        .delete(&user.id)
        .await
        .checked("test operation should succeed");

    // Credentials should still exist (soft delete)
    let found = cred_repo
        .get_by_id(cred.id)
        .await
        .checked("test operation should succeed");
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
        .checked("test operation should succeed");

    let mut expired = make_credential(user.id, "alist", "expired_server");
    expired.expires_at = Some(Utc::now() - Duration::hours(1));
    cred_repo
        .create(&expired)
        .await
        .checked("test operation should succeed");

    let mut valid = make_credential(user.id, "bilibili", &bilibili_server_id());
    valid.expires_at = Some(Utc::now() + Duration::hours(1));
    cred_repo
        .create(&valid)
        .await
        .checked("test operation should succeed");

    // Delete expired
    let deleted = cred_repo
        .delete_expired()
        .await
        .checked("test operation should succeed");
    assert_eq!(deleted, 1);

    // Verify only valid remains
    let all = cred_repo
        .get_by_user(user.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].provider, "bilibili");
}
