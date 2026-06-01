//! `UserOAuthProviderRepository` integration tests
//!
//! Tests: upsert conflict handling, transaction executor path,
//!        `delete_all_for_user_with_executor`.
//!
//! Run with: cargo test -p synctv-core --test `user_oauth_provider_repository_tests`
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    models::{OAuth2Provider, OAuth2UserInfo, User, UserId, UserRole, UserStatus},
    repository::{UserOAuthProviderRepository, UserRepository},
};
use synctv_core_testing::create_test_pool;
fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

async fn create_user(pool: &PgPool, username: &str) -> User {
    let user_repo = UserRepository::new(pool.clone());
    user_repo.create(&make_user(username)).await.unwrap()
}

fn oauth_user_info(
    provider: OAuth2Provider,
    provider_instance_name: &str,
    provider_user_id: &str,
    username: &str,
    email: Option<&str>,
    avatar: Option<&str>,
) -> OAuth2UserInfo {
    OAuth2UserInfo {
        provider,
        provider_instance_name: provider_instance_name.to_string(),
        provider_issuer: None,
        provider_user_id: provider_user_id.to_string(),
        username: username.to_string(),
        email: email.map(str::to_string),
        avatar: avatar.map(str::to_string),
    }
}

// ─── upsert conflict handling ────────────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_upsert_different_user_id_rejects_rebinding_and_preserves_mapping() {
    let (_container, pool) = create_test_pool().await;
    let oauth_repo = UserOAuthProviderRepository::new(pool.clone());

    let user_a = create_user(&pool, "oauth_user_a").await;
    let user_b = create_user(&pool, "oauth_user_b").await;

    let provider = OAuth2Provider::GitHub;
    let provider_instance_name = "github-main";
    let provider_user_id = "gh_unique_001";
    let user_info = oauth_user_info(
        provider.clone(),
        provider_instance_name,
        provider_user_id,
        "ghuser",
        None,
        None,
    );

    // Initial upsert for user_a
    oauth_repo
        .upsert(
            &user_a.id,
            &provider,
            provider_instance_name,
            provider_user_id,
            &user_info,
        )
        .await
        .unwrap();

    let mapping = oauth_repo
        .find_by_provider_instance(provider_instance_name, provider_user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mapping.user_id, user_a.id);

    // Upsert again with user_b must be rejected: external identities are stable
    // and must never be silently reassigned to another local user.
    let err = oauth_repo
        .upsert(
            &user_b.id,
            &provider,
            provider_instance_name,
            provider_user_id,
            &user_info,
        )
        .await
        .expect_err("OAuth identity rebinding must be rejected");

    assert!(
        matches!(
            err,
            synctv_core::Error::AlreadyExists(ref msg)
            if msg.contains("already linked to another user")
        ),
        "Unexpected error: {err}"
    );

    let mapping = oauth_repo
        .find_by_provider_instance(provider_instance_name, provider_user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        mapping.user_id, user_a.id,
        "Original OAuth identity binding must be preserved"
    );

    // user_a must still own the mapping
    let user_a_mappings = oauth_repo.find_by_user(&user_a.id).await.unwrap();
    assert_eq!(user_a_mappings.len(), 1);

    // user_b must not gain the mapping
    let user_b_mappings = oauth_repo.find_by_user(&user_b.id).await.unwrap();
    assert!(
        user_b_mappings.is_empty(),
        "user_b should not receive another user's OAuth mapping"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_upsert_same_user_id_updates_profile_fields_without_rebinding() {
    let (_container, pool) = create_test_pool().await;
    let oauth_repo = UserOAuthProviderRepository::new(pool.clone());

    let user = create_user(&pool, "oauth_profile_user").await;

    let provider = OAuth2Provider::GitHub;
    let provider_instance_name = "github-main";
    let provider_user_id = "gh_profile_001";
    let initial_info = oauth_user_info(
        provider.clone(),
        provider_instance_name,
        provider_user_id,
        "oldname",
        None,
        None,
    );

    oauth_repo
        .upsert(
            &user.id,
            &provider,
            provider_instance_name,
            provider_user_id,
            &initial_info,
        )
        .await
        .unwrap();

    let mut updated_info = oauth_user_info(
        provider.clone(),
        provider_instance_name,
        provider_user_id,
        "newname",
        Some("new@example.com"),
        Some("https://avatar.example/new.png"),
    );
    updated_info.provider_issuer = Some("https://github.com".to_string());

    oauth_repo
        .upsert(
            &user.id,
            &provider,
            provider_instance_name,
            provider_user_id,
            &updated_info,
        )
        .await
        .unwrap();

    let mapping = oauth_repo
        .find_by_provider_instance(provider_instance_name, provider_user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mapping.user_id, user.id);
    assert_eq!(mapping.username, "newname");
    assert_eq!(mapping.email.as_deref(), Some("new@example.com"));
    assert_eq!(
        mapping.avatar_url.as_deref(),
        Some("https://avatar.example/new.png")
    );
    assert_eq!(
        mapping.provider_issuer.as_deref(),
        Some("https://github.com")
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_upsert_with_executor_rejects_rebinding_inside_transaction() {
    let (_container, pool) = create_test_pool().await;
    let oauth_repo = UserOAuthProviderRepository::new(pool.clone());

    let user_a = create_user(&pool, "oauth_tx_owner").await;
    let user_b = create_user(&pool, "oauth_tx_conflict").await;
    let provider = OAuth2Provider::Google;
    let provider_instance_name = "google-main";
    let provider_user_id = "google_tx_conflict_001";
    let user_info = oauth_user_info(
        provider.clone(),
        provider_instance_name,
        provider_user_id,
        "googleuser",
        Some("tx@google.com"),
        None,
    );

    oauth_repo
        .upsert(
            &user_a.id,
            &provider,
            provider_instance_name,
            provider_user_id,
            &user_info,
        )
        .await
        .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let err = oauth_repo
        .upsert_with_executor(
            &user_b.id,
            &provider,
            provider_instance_name,
            provider_user_id,
            &user_info,
            &mut *tx,
        )
        .await
        .expect_err("Rebinding in transaction must be rejected");
    tx.rollback().await.unwrap();

    assert!(
        matches!(
            err,
            synctv_core::Error::AlreadyExists(ref msg)
            if msg.contains("already linked to another user")
        ),
        "Unexpected error: {err}"
    );

    let mapping = oauth_repo
        .find_by_provider_instance(provider_instance_name, provider_user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mapping.user_id, user_a.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_upsert_with_executor_in_transaction() {
    let (_container, pool) = create_test_pool().await;
    let oauth_repo = UserOAuthProviderRepository::new(pool.clone());

    let user = create_user(&pool, "oauth_tx_user").await;
    let provider = OAuth2Provider::Google;
    let provider_instance_name = "google-main";
    let provider_user_id = "google_tx_001";
    let user_info = oauth_user_info(
        provider.clone(),
        provider_instance_name,
        provider_user_id,
        "googleuser",
        Some("tx@google.com"),
        None,
    );

    // Use within a transaction
    let mut tx = pool.begin().await.unwrap();
    oauth_repo
        .upsert_with_executor(
            &user.id,
            &provider,
            provider_instance_name,
            provider_user_id,
            &user_info,
            &mut *tx,
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Verify it was persisted
    let user_mappings = oauth_repo.find_by_user(&user.id).await.unwrap();
    let mapping = oauth_repo
        .find_by_provider_instance(provider_instance_name, provider_user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user_mappings.len(), 1);
    assert_eq!(mapping.user_id, user.id);
    assert_eq!(mapping.email.as_deref(), Some("tx@google.com"));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_find_by_provider_with_executor_in_transaction() {
    let (_container, pool) = create_test_pool().await;
    let oauth_repo = UserOAuthProviderRepository::new(pool.clone());

    let user = create_user(&pool, "oauth_find_tx_user").await;
    let provider = OAuth2Provider::Discord;
    let provider_instance_name = "discord-main";
    let provider_user_id = "discord_find_001";
    let user_info = oauth_user_info(
        provider.clone(),
        provider_instance_name,
        provider_user_id,
        "discorduser",
        None,
        None,
    );

    oauth_repo
        .upsert(
            &user.id,
            &provider,
            provider_instance_name,
            provider_user_id,
            &user_info,
        )
        .await
        .unwrap();

    // Find within transaction
    let mut tx = pool.begin().await.unwrap();
    let mapping = oauth_repo
        .find_by_provider_instance_with_executor(provider_instance_name, provider_user_id, &mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert!(mapping.is_some());
    assert_eq!(mapping.unwrap().user_id, user.id);
}

// ─── delete_all_for_user_with_executor ───────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_all_for_user_with_executor() {
    let (_container, pool) = create_test_pool().await;
    let oauth_repo = UserOAuthProviderRepository::new(pool.clone());

    let user = create_user(&pool, "oauth_del_tx_user").await;

    let info_gh = OAuth2UserInfo {
        provider: OAuth2Provider::GitHub,
        provider_instance_name: "github-main".to_string(),
        provider_issuer: None,
        provider_user_id: "gh_del_001".to_string(),
        username: "ghuser".to_string(),
        email: None,
        avatar: None,
    };
    oauth_repo
        .upsert(
            &user.id,
            &OAuth2Provider::GitHub,
            "github-main",
            "gh_del_001",
            &info_gh,
        )
        .await
        .unwrap();

    let info_google = OAuth2UserInfo {
        provider: OAuth2Provider::Google,
        provider_instance_name: "google-main".to_string(),
        provider_issuer: None,
        provider_user_id: "google_del_001".to_string(),
        username: "googleuser".to_string(),
        email: None,
        avatar: None,
    };
    oauth_repo
        .upsert(
            &user.id,
            &OAuth2Provider::Google,
            "google-main",
            "google_del_001",
            &info_google,
        )
        .await
        .unwrap();

    // Verify 2 mappings exist
    let mappings = oauth_repo.find_by_user(&user.id).await.unwrap();
    assert_eq!(mappings.len(), 2);

    // Delete all within a transaction
    let mut tx = pool.begin().await.unwrap();
    let deleted = oauth_repo
        .delete_all_for_user_with_executor(&user.id, &mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(deleted, 2);

    // Verify all gone
    let mappings = oauth_repo.find_by_user(&user.id).await.unwrap();
    assert!(mappings.is_empty());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_all_for_user_with_executor_no_mappings() {
    let (_container, pool) = create_test_pool().await;
    let oauth_repo = UserOAuthProviderRepository::new(pool.clone());

    let user = create_user(&pool, "oauth_del_empty_user").await;

    let mut tx = pool.begin().await.unwrap();
    let deleted = oauth_repo
        .delete_all_for_user_with_executor(&user.id, &mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(deleted, 0);
}
