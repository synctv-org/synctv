//! Admin batch operations tests
//!
//! Tests batch delete operations for users.
//!

use std::sync::Arc;

use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::UserId,
    service::{BruteForceProtection, InMemoryTokenBlacklistStore, JwtService, UserService},
};
use synctv_core_testing::{create_test_pool, ok, opaque_register_user};

fn create_jwt_service() -> JwtService {
    ok(
        JwtService::new("test-secret-key-for-batch-tests-long-enough-1234567890"),
        "test JWT service should initialize",
    )
}

fn create_user_service(pool: &sqlx::PgPool) -> UserService {
    let jwt = create_jwt_service();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new_with_runtime(
        pool,
        jwt,
        username_cache,
        token_blacklist,
        key_builder,
        brute_force,
        synctv_core::service::UserServiceRuntimeOptions {
            password_registration_policy_override: Some(synctv_core::service::RegistrationPolicy {
                enabled: true,
                need_review: false,
            }),
            ..synctv_core::service::UserServiceRuntimeOptions::test_defaults()
        },
    )
}

/// Helper to create multiple test users
async fn create_test_users(service: &UserService, count: usize, prefix: &str) -> Vec<UserId> {
    let mut user_ids = Vec::with_capacity(count);
    for i in 0..count {
        let (user, _, _) = ok(
            opaque_register_user(
                service,
                format!("{prefix}_{i}"),
                Some(format!("{i}@test.com")),
                "Password123",
            )
            .await,
            "test user should be created",
        );
        user_ids.push(user.id);
    }
    user_ids
}

// Batch Delete Users Tests

#[tokio::test]
#[ignore = "Requires Docker"]
async fn batch_delete_users_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);

    let user_ids = create_test_users(&service, 5, "batch_del").await;
    let result = service.batch_delete_users(&user_ids).await;
    assert!(result.is_ok(), "Batch delete should succeed: {result:?}");

    for user_id in &user_ids {
        let result = service.get_user(user_id).await;
        assert!(result.is_err(), "User {user_id} should be deleted");
    }
}
