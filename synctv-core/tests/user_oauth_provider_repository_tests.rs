//! UserOAuthProviderRepository integration tests
//!
//! Tests: upsert with different user_id, transaction executor path,
//!        delete_all_for_user_with_executor.
//!
//! Run with: cargo test -p synctv-core --test user_oauth_provider_repository_tests

use synctv_core_testing::{create_test_pool, create_test_jwt_service};
use synctv_core::{
    models::{
        UserId, User, UserRole, UserStatus,
        OAuth2Provider, OAuth2UserInfo,
    },
    repository::{UserOAuthProviderRepository, UserRepository},
};
use chrono::Utc;
use sqlx::PgPool;
fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{}@test.com", username)),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: None,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

async fn create_user(pool: &PgPool, username: &str) -> User {
    let user_repo = UserRepository::new(pool.clone());
    user_repo.create(&make_user(username)).await.unwrap()
}

// ─── upsert with different user_id (OAuth identity update) ───────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_upsert_different_user_id_updates_mapping() {
    let (_container, pool) = create_test_pool().await;
    let oauth_repo = UserOAuthProviderRepository::new(pool.clone());

    let user_a = create_user(&pool, "oauth_user_a").await;
    let user_b = create_user(&pool, "oauth_user_b").await;

    let provider = OAuth2Provider::GitHub;
    let provider_user_id = "gh_unique_001";
    let user_info = OAuth2UserInfo {
        provider: provider.clone(),
        provider_user_id: provider_user_id.to_string(),
        username: "ghuser".to_string(),
        email: None,
        avatar: None,
    };

    // Initial upsert for user_a
    oauth_repo
        .upsert(&user_a.id, &provider, provider_user_id, &user_info)
        .await
        .unwrap();

    let mapping = oauth_repo
        .find_by_provider(&provider, provider_user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mapping.user_id, user_a.id);

    // Upsert again with user_b (re-linking the OAuth identity)
    oauth_repo
        .upsert(&user_b.id, &provider, provider_user_id, &user_info)
        .await
        .unwrap();

    let mapping = oauth_repo
        .find_by_provider(&provider, provider_user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mapping.user_id, user_b.id, "OAuth identity should now be linked to user_b");

    // user_a should no longer have this mapping
    let user_a_mappings = oauth_repo.find_by_user(&user_a.id).await.unwrap();
    assert!(
        user_a_mappings.is_empty(),
        "user_a should have no OAuth mappings after re-link"
    );
}

// ─── transaction executor path ───────────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_upsert_with_executor_in_transaction() {
    let (_container, pool) = create_test_pool().await;
    let oauth_repo = UserOAuthProviderRepository::new(pool.clone());

    let user = create_user(&pool, "oauth_tx_user").await;
    let provider = OAuth2Provider::Google;
    let provider_user_id = "google_tx_001";
    let user_info = OAuth2UserInfo {
        provider: provider.clone(),
        provider_user_id: provider_user_id.to_string(),
        username: "googleuser".to_string(),
        email: Some("tx@google.com".to_string()),
        avatar: None,
    };

    // Use within a transaction
    let mut tx = pool.begin().await.unwrap();
    oauth_repo
        .upsert_with_executor(&user.id, &provider, provider_user_id, &user_info, &mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Verify it was persisted
    let mapping = oauth_repo
        .find_by_provider(&provider, provider_user_id)
        .await
        .unwrap()
        .unwrap();
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
    let provider_user_id = "discord_find_001";
    let user_info = OAuth2UserInfo {
        provider: provider.clone(),
        provider_user_id: provider_user_id.to_string(),
        username: "discorduser".to_string(),
        email: None,
        avatar: None,
    };

    oauth_repo
        .upsert(&user.id, &provider, provider_user_id, &user_info)
        .await
        .unwrap();

    // Find within transaction
    let mut tx = pool.begin().await.unwrap();
    let mapping = oauth_repo
        .find_by_provider_with_executor(&provider, provider_user_id, &mut *tx)
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

    // Create multiple mappings for this user
    let info_gh = OAuth2UserInfo {
        provider: OAuth2Provider::GitHub,
        provider_user_id: "gh_del_001".to_string(),
        username: "ghuser".to_string(),
        email: None,
        avatar: None,
    };
    oauth_repo
        .upsert(&user.id, &OAuth2Provider::GitHub, "gh_del_001", &info_gh)
        .await
        .unwrap();

    let info_google = OAuth2UserInfo {
        provider: OAuth2Provider::Google,
        provider_user_id: "google_del_001".to_string(),
        username: "googleuser".to_string(),
        email: None,
        avatar: None,
    };
    oauth_repo
        .upsert(&user.id, &OAuth2Provider::Google, "google_del_001", &info_google)
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
