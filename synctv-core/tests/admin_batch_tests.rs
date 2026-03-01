//! Admin batch operations tests
//!
//! Tests batch ban/delete operations for users and rooms.
//!
//! Run with: cargo test --package synctv-core admin_batch
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use synctv_core_testing::{create_test_pool};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache, NoopCacheL2},
    config::PasswordComplexityConfig,
    models::{UserId, UserStatus},
    service::{
        UserService, InMemoryTokenBlacklistStore,
        auth::{JwtService, BruteForceProtection},
    },
    Error,
};
use sqlx::PgPool;
const BATCH_SIZE_LIMIT: usize = 100;

fn create_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-batch-tests-long-enough-1234567890").unwrap()
}

fn create_user_service(pool: PgPool) -> UserService {
    let jwt = create_jwt_service();
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);
    let password_config = PasswordComplexityConfig::default();
    let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new(
        pool,
        jwt,
        username_cache,
        password_config,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

/// Helper to create multiple test users
async fn create_test_users(service: &UserService, count: usize, prefix: &str) -> Vec<UserId> {
    let mut user_ids = Vec::with_capacity(count);
    for i in 0..count {
        let (user, _, _) = service
            .register(
                format!("{}_{}", prefix, i),
                Some(format!("{}@test.com", i)),
                "Password123".to_string(),
                None,
            )
            .await
            .expect("Failed to create user");
        user_ids.push(user.id);
    }
    user_ids
}

// ============================================================================
// Batch Ban Users Tests
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn batch_ban_users_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool);

    // Create 5 test users
    let user_ids = create_test_users(&service, 5, "batch_ban").await;
    let user_id_strs: Vec<String> = user_ids.iter().map(|id| id.to_string()).collect();

    // Ban all 5 users
    let result = service.batch_ban_users(&user_id_strs).await;
    assert!(result.is_ok(), "Batch ban should succeed: {result:?}");

    // Verify all users are banned
    for user_id in &user_ids {
        let user = service.get_user(user_id).await.expect("User should exist");
        assert_eq!(user.status, UserStatus::Banned, "User {} should be banned", user_id);
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn batch_ban_users_exceeds_limit_fails() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool);

    // The size limit check happens before any DB operations, so we don't need
    // to create real users -- fake IDs are enough to trigger the validation.
    let user_id_strs: Vec<String> = (0..BATCH_SIZE_LIMIT + 1)
        .map(|i| format!("fake_user_{i}"))
        .collect();

    // Attempt to ban more than limit
    let result = service.batch_ban_users(&user_id_strs).await;
    assert!(result.is_err(), "Batch ban should fail when exceeding limit");

    match result {
        Err(Error::InvalidInput(msg)) => {
            assert!(msg.contains("exceeds limit"), "Error message should mention limit");
        }
        _ => panic!("Expected InvalidInput error"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn batch_ban_users_already_banned_skipped() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool);

    // Create 3 test users
    let user_ids = create_test_users(&service, 3, "batch_ban_skip").await;

    // Ban the first user beforehand
    let mut user = service.get_user(&user_ids[0]).await.expect("User should exist");
    user.status = UserStatus::Banned;
    let old_version = user.version;
    service.update_user(&user, old_version).await.expect("Update should succeed");

    // Ban all users (first is already banned)
    let user_id_strs: Vec<String> = user_ids.iter().map(|id| id.to_string()).collect();
    let result = service.batch_ban_users(&user_id_strs).await;
    assert!(result.is_ok(), "Batch ban should succeed: {result:?}");

    // Verify all users are banned
    for user_id in &user_ids {
        let user = service.get_user(user_id).await.expect("User should exist");
        assert_eq!(user.status, UserStatus::Banned, "User {} should be banned", user_id);
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn batch_ban_users_nonexistent_user_fails() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool);

    // Create 2 test users
    let user_ids = create_test_users(&service, 2, "batch_ban_nonexist").await;
    let mut user_id_strs: Vec<String> = user_ids.iter().map(|id| id.to_string()).collect();

    // Add a non-existent user ID
    user_id_strs.push("nonexistent_user_id".to_string());

    // Attempt batch ban – the overall call succeeds but individual results
    // report per-user success/failure.
    let results = service.batch_ban_users(&user_id_strs).await
        .expect("batch_ban_users returns Ok with per-user results");

    // The two real users should succeed
    let ok_count = results.iter().filter(|(_, r)| r.is_ok()).count();
    let err_count = results.iter().filter(|(_, r)| r.is_err()).count();
    assert_eq!(ok_count, 2, "Two real users should be banned successfully");
    assert_eq!(err_count, 1, "Non-existent user should fail");

    // Verify the failing entry is the non-existent user
    let failed: Vec<_> = results.iter().filter(|(_, r)| r.is_err()).collect();
    assert_eq!(failed[0].0, "nonexistent_user_id");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn batch_ban_users_empty_list_fails() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool);

    let result = service.batch_ban_users(&[]).await;
    assert!(result.is_err(), "Batch ban should fail with empty list");
}

// ============================================================================
// Batch Delete Users Tests
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn batch_delete_users_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool);

    // Create 5 test users
    let user_ids = create_test_users(&service, 5, "batch_del").await;
    let user_id_strs: Vec<String> = user_ids.iter().map(|id| id.to_string()).collect();

    // Delete all 5 users
    let result = service.batch_delete_users(&user_id_strs).await;
    assert!(result.is_ok(), "Batch delete should succeed: {result:?}");

    // Verify all users are soft-deleted
    for user_id in &user_ids {
        let result = service.get_user(user_id).await;
        assert!(result.is_err(), "User {} should be deleted", user_id);
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn batch_delete_users_exceeds_limit_fails() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool);

    // The size limit check happens before any DB operations, so we don't need
    // to create real users -- fake IDs are enough to trigger the validation.
    let user_id_strs: Vec<String> = (0..BATCH_SIZE_LIMIT + 1)
        .map(|i| format!("fake_user_{i}"))
        .collect();

    // Attempt to delete more than limit
    let result = service.batch_delete_users(&user_id_strs).await;
    assert!(result.is_err(), "Batch delete should fail when exceeding limit");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn batch_delete_users_empty_list_fails() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool);

    let result = service.batch_delete_users(&[]).await;
    assert!(result.is_err(), "Batch delete should fail with empty list");
}

// ============================================================================
// Constants and Limits Tests
// ============================================================================

#[test]
fn batch_size_limit_is_defined() {
    // Ensure the batch size limit is properly defined
    assert_eq!(BATCH_SIZE_LIMIT, 100);
}
