//! `UserOAuthProviderRepository` integration tests
//!
//! Tests: upsert conflict handling, transaction executor path,
//!        `delete_all_for_user_with_executor`.

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    models::{
        OAuth2Provider, OAuth2UserInfo, User, UserId, UserOAuthProviderMapping, UserRole,
        UserStatus,
    },
    repository::{UserOAuthProviderRepository, UserRepository},
};
use synctv_core_testing::{create_test_pool, err, ok, some};
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
    ok(
        user_repo.create(&make_user(username)).await,
        "OAuth test user should be created",
    )
}

fn oauth_user_info(
    provider: OAuth2Provider,
    provider_instance_name: &str,
    provider_user_id: &str,
    username: &str,
    avatar: Option<&str>,
) -> OAuth2UserInfo {
    OAuth2UserInfo {
        provider,
        provider_instance_name: provider_instance_name.to_string(),
        provider_issuer: None,
        provider_user_id: provider_user_id.to_string(),
        username: username.to_string(),
        avatar: avatar.map(str::to_string),
    }
}

async fn find_mapping(
    repo: &UserOAuthProviderRepository,
    provider_instance_name: &str,
    provider_user_id: &str,
) -> UserOAuthProviderMapping {
    some(
        ok(
            repo.find_by_provider_instance(provider_instance_name, provider_user_id)
                .await,
            "OAuth provider mapping should be fetched",
        ),
        "OAuth provider mapping should exist",
    )
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
    );

    // Initial upsert for user_a
    ok(
        oauth_repo
            .upsert(
                &user_a.id,
                &provider,
                provider_instance_name,
                provider_user_id,
                &user_info,
            )
            .await,
        "OAuth identity should be linked",
    );

    let mapping = find_mapping(&oauth_repo, provider_instance_name, provider_user_id).await;
    assert_eq!(mapping.user_id, user_a.id);

    // Upsert again with user_b must be rejected: external identities are stable
    // and must never be silently reassigned to another local user.
    let err = err(
        oauth_repo
            .upsert(
                &user_b.id,
                &provider,
                provider_instance_name,
                provider_user_id,
                &user_info,
            )
            .await,
        "OAuth identity rebinding should be rejected",
    );

    assert!(
        matches!(
            err,
            synctv_core::Error::AlreadyExists(ref msg)
            if msg.contains("already linked to another user")
        ),
        "Unexpected error: {err}"
    );

    let mapping = find_mapping(&oauth_repo, provider_instance_name, provider_user_id).await;
    assert_eq!(
        mapping.user_id, user_a.id,
        "Original OAuth identity binding must be preserved"
    );

    // user_a must still own the mapping
    let user_a_mappings = ok(
        oauth_repo.find_by_user(&user_a.id).await,
        "user A OAuth mappings should be fetched",
    );
    assert_eq!(user_a_mappings.len(), 1);

    // user_b must not gain the mapping
    let user_b_mappings = ok(
        oauth_repo.find_by_user(&user_b.id).await,
        "user B OAuth mappings should be fetched",
    );
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
    );

    ok(
        oauth_repo
            .upsert(
                &user.id,
                &provider,
                provider_instance_name,
                provider_user_id,
                &initial_info,
            )
            .await,
        "initial OAuth identity should be linked",
    );

    let mut updated_info = oauth_user_info(
        provider.clone(),
        provider_instance_name,
        provider_user_id,
        "newname",
        Some("https://avatar.example/new.png"),
    );
    updated_info.provider_issuer = Some("https://github.com".to_string());

    ok(
        oauth_repo
            .upsert(
                &user.id,
                &provider,
                provider_instance_name,
                provider_user_id,
                &updated_info,
            )
            .await,
        "OAuth identity profile fields should update",
    );

    let mapping = find_mapping(&oauth_repo, provider_instance_name, provider_user_id).await;
    assert_eq!(mapping.user_id, user.id);
    assert_eq!(mapping.username, "newname");
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
        None,
    );

    ok(
        oauth_repo
            .upsert(
                &user_a.id,
                &provider,
                provider_instance_name,
                provider_user_id,
                &user_info,
            )
            .await,
        "OAuth identity should be linked before transaction conflict",
    );

    let mut tx = ok(pool.begin().await, "transaction should begin");
    let err = err(
        oauth_repo
            .upsert_with_executor(
                &user_b.id,
                &provider,
                provider_instance_name,
                provider_user_id,
                &user_info,
                &mut *tx,
            )
            .await,
        "rebinding in transaction should be rejected",
    );
    ok(tx.rollback().await, "transaction should roll back");

    assert!(
        matches!(
            err,
            synctv_core::Error::AlreadyExists(ref msg)
            if msg.contains("already linked to another user")
        ),
        "Unexpected error: {err}"
    );

    let mapping = find_mapping(&oauth_repo, provider_instance_name, provider_user_id).await;
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
        None,
    );

    // Use within a transaction
    let mut tx = ok(pool.begin().await, "transaction should begin");
    ok(
        oauth_repo
            .upsert_with_executor(
                &user.id,
                &provider,
                provider_instance_name,
                provider_user_id,
                &user_info,
                &mut *tx,
            )
            .await,
        "OAuth identity should be linked in transaction",
    );
    ok(tx.commit().await, "transaction should commit");

    // Verify it was persisted
    let user_mappings = ok(
        oauth_repo.find_by_user(&user.id).await,
        "user OAuth mappings should be fetched",
    );
    let mapping = find_mapping(&oauth_repo, provider_instance_name, provider_user_id).await;
    assert_eq!(user_mappings.len(), 1);
    assert_eq!(mapping.user_id, user.id);
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
    );

    ok(
        oauth_repo
            .upsert(
                &user.id,
                &provider,
                provider_instance_name,
                provider_user_id,
                &user_info,
            )
            .await,
        "OAuth identity should be linked",
    );

    // Find within transaction
    let mut tx = ok(pool.begin().await, "transaction should begin");
    let mapping = ok(
        oauth_repo
            .find_by_provider_instance_with_executor(
                provider_instance_name,
                provider_user_id,
                &mut *tx,
            )
            .await,
        "OAuth provider mapping should be fetched in transaction",
    );
    ok(tx.commit().await, "transaction should commit");

    assert_eq!(
        some(mapping, "OAuth provider mapping should exist").user_id,
        user.id
    );
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
        avatar: None,
    };
    ok(
        oauth_repo
            .upsert(
                &user.id,
                &OAuth2Provider::GitHub,
                "github-main",
                "gh_del_001",
                &info_gh,
            )
            .await,
        "GitHub OAuth identity should be linked",
    );

    let info_google = OAuth2UserInfo {
        provider: OAuth2Provider::Google,
        provider_instance_name: "google-main".to_string(),
        provider_issuer: None,
        provider_user_id: "google_del_001".to_string(),
        username: "googleuser".to_string(),
        avatar: None,
    };
    ok(
        oauth_repo
            .upsert(
                &user.id,
                &OAuth2Provider::Google,
                "google-main",
                "google_del_001",
                &info_google,
            )
            .await,
        "Google OAuth identity should be linked",
    );

    // Verify 2 mappings exist
    let mappings = ok(
        oauth_repo.find_by_user(&user.id).await,
        "OAuth mappings should be fetched before delete",
    );
    assert_eq!(mappings.len(), 2);

    // Delete all within a transaction
    let mut tx = ok(pool.begin().await, "transaction should begin");
    let deleted = ok(
        oauth_repo
            .delete_all_for_user_with_executor(&user.id, &mut *tx)
            .await,
        "OAuth mappings should be deleted in transaction",
    );
    ok(tx.commit().await, "transaction should commit");

    assert_eq!(deleted, 2);

    // Verify all gone
    let mappings = ok(
        oauth_repo.find_by_user(&user.id).await,
        "OAuth mappings should be fetched after delete",
    );
    assert!(mappings.is_empty());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_all_for_user_with_executor_no_mappings() {
    let (_container, pool) = create_test_pool().await;
    let oauth_repo = UserOAuthProviderRepository::new(pool.clone());

    let user = create_user(&pool, "oauth_del_empty_user").await;

    let mut tx = ok(pool.begin().await, "transaction should begin");
    let deleted = ok(
        oauth_repo
            .delete_all_for_user_with_executor(&user.id, &mut *tx)
            .await,
        "empty OAuth mapping delete should execute in transaction",
    );
    ok(tx.commit().await, "transaction should commit");

    assert_eq!(deleted, 0);
}
