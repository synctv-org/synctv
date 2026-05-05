//! Admin batch operations tests
//!
//! Tests batch delete operations for users.
//!
//! Run with: cargo test --package synctv-core `admin_batch`
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::UserId,
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        InMemoryTokenBlacklistStore, UserService,
    },
};
use synctv_core_testing::create_test_pool;
const BATCH_SIZE_LIMIT: usize = 100;

fn create_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-batch-tests-long-enough-1234567890").unwrap()
}

fn create_lazy_pool() -> PgPool {
    PgPool::connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused")
        .expect("lazy pool construction should not connect")
}

fn create_user_service(pool: PgPool) -> UserService {
    let jwt = create_jwt_service();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let password_config = PasswordComplexityConfig::default();
    let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    let mut svc = UserService::new(
        pool,
        jwt,
        username_cache,
        password_config,
        token_blacklist,
        key_builder,
        brute_force,
    );
    svc.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    svc.enable_password_registration_for_tests();
    svc
}

/// Helper to create multiple test users
async fn create_test_users(service: &UserService, count: usize, prefix: &str) -> Vec<UserId> {
    let mut user_ids = Vec::with_capacity(count);
    for i in 0..count {
        let (user, _, _) = service
            .register(
                format!("{prefix}_{i}"),
                Some(format!("{i}@test.com")),
                "Password123".to_string(),
                None,
            )
            .await
            .expect("Failed to create user");
        user_ids.push(user.id);
    }
    user_ids
}

// Batch Delete Users Tests

#[tokio::test]
#[ignore = "Requires Docker"]
async fn batch_delete_users_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool);

    let user_ids = create_test_users(&service, 5, "batch_del").await;
    // Delete all 5 users
    let result = service.batch_delete_users(&user_ids).await;
    assert!(result.is_ok(), "Batch delete should succeed: {result:?}");

    // Verify all users are soft-deleted
    for user_id in &user_ids {
        let result = service.get_user(user_id).await;
        assert!(result.is_err(), "User {user_id} should be deleted");
    }
}

#[tokio::test]
async fn batch_delete_users_exceeds_limit_fails() {
    let service = create_user_service(create_lazy_pool());

    // The size limit check happens before any DB operations, so we don't need
    // to create real users -- fake IDs are enough to trigger the validation.
    let user_ids: Vec<UserId> = (0..=BATCH_SIZE_LIMIT)
        .map(|i| UserId::from(i64::try_from(i + 1).expect("test id fits i64")))
        .collect();

    let result = service.batch_delete_users(&user_ids).await;
    assert!(
        result.is_err(),
        "Batch delete should fail when exceeding limit"
    );
}

#[tokio::test]
async fn batch_delete_users_empty_list_fails() {
    let service = create_user_service(create_lazy_pool());

    let result = service.batch_delete_users(&[]).await;
    assert!(result.is_err(), "Batch delete should fail with empty list");
}
