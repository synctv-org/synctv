//! User service tests
//!
//! Tests user registration and login validation using testcontainers.
//!
//! Run with: cargo test --test user_service_tests
//! Run Docker tests: cargo test --test user_service_tests -- --ignored

use std::sync::Arc;

use synctv_core::{
    cache::{KeyBuilder, UsernameCache, NoopCacheL2},
    config::PasswordComplexityConfig,
    service::{
        UserService, InMemoryTokenBlacklistStore,
        auth::{JwtService, BruteForceProtection},
    },
};
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

const POSTGRES_VERSION: &str = "16-alpine";

fn create_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-user-service-tests-long-enough-1234567890").unwrap()
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

// ============================================================================
// Integration tests (require Docker)
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_register_duplicate_username_error() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool);

    // Register first user
    let result = service
        .register(
            "unique_user_dup".to_string(),
            Some("dup1@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await;
    assert!(result.is_ok(), "First registration should succeed: {result:?}");

    // Register with same username, different email
    let result = service
        .register(
            "unique_user_dup".to_string(),
            Some("dup2@example.com".to_string()),
            "StrongPass2".to_string(),
            None,
        )
        .await;
    assert!(result.is_err(), "Duplicate username should be rejected");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_register_duplicate_email_error() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool);

    // Register first user
    let result = service
        .register(
            "email_dup_1".to_string(),
            Some("same_email@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await;
    assert!(result.is_ok(), "First registration should succeed");

    // Register with different username, same email
    let result = service
        .register(
            "email_dup_2".to_string(),
            Some("same_email@example.com".to_string()),
            "StrongPass2".to_string(),
            None,
        )
        .await;
    assert!(result.is_err(), "Duplicate email should be rejected");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_wrong_password() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool);

    // Register a user
    service
        .register(
            "login_test_user".to_string(),
            Some("login@example.com".to_string()),
            "CorrectPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    // Try to login with wrong password
    let result = service
        .login("login_test_user".to_string(), "WrongPass1".to_string(), None)
        .await;

    assert!(result.is_err(), "Login with wrong password should fail");
}

// ============================================================================
// Validation tests (no Docker needed)
// ============================================================================

#[test]
fn test_username_validation() {
    let validator = synctv_core::validation::UsernameValidator::new();

    assert!(validator.validate("good_user").is_ok());
    assert!(validator.validate("ab").is_err()); // too short
    assert!(validator.validate("user@name").is_err()); // invalid chars
}

#[test]
fn test_password_validation() {
    let validator = synctv_core::validation::PasswordValidator::from_config(
        &PasswordComplexityConfig::default(),
    );

    assert!(validator.validate("StrongPass1").is_ok());
    assert!(validator.validate("weak").is_err());
    assert!(validator.validate("nouppercase1").is_err());
}
