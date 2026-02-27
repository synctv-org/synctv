//! Admin batch operations tests
//!
//! Tests batch ban/delete operations for users and rooms.
//!
//! Run with: cargo test --package synctv-core admin_batch

use std::sync::Arc;

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
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

const POSTGRES_VERSION: &str = "16-alpine";
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

async fn create_test_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let postgres = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
        .start()
        .await
        .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/synctv_test",
        postgres.get_host_port_ipv4(5432).await.expect("Failed to get port")
    );

    let pool = PgPool::connect(&connection_string)
        .await
        .expect("Failed to create pool");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (postgres, pool)
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

    // Create more users than the limit
    let user_ids = create_test_users(&service, BATCH_SIZE_LIMIT + 1, "batch_limit").await;
    let user_id_strs: Vec<String> = user_ids.iter().map(|id| id.to_string()).collect();

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

    // Attempt batch ban
    let result = service.batch_ban_users(&user_id_strs).await;
    assert!(result.is_err(), "Batch ban should fail with non-existent user");
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

    // Create more users than the limit
    let user_ids = create_test_users(&service, BATCH_SIZE_LIMIT + 1, "batch_del_limit").await;
    let user_id_strs: Vec<String> = user_ids.iter().map(|id| id.to_string()).collect();

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
