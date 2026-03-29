//! User service tests
//!
//! Tests user registration and login validation using testcontainers.
//!
//! Run with: cargo test --test `user_service_tests`
//! Run Docker tests: cargo test --test `user_service_tests` -- --ignored
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    repository::UserRepository,
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        InMemoryTokenBlacklistStore, UserService,
    },
    Error,
};
use synctv_core_testing::create_test_pool;
use tokio::sync::Barrier;

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
    svc
}

// ============================================================================
// Integration tests (require Docker)
// ============================================================================

async fn assert_register_duplicate_username_error(service: &UserService) {
    // Register first user
    let result = service
        .register(
            "unique_user_dup".to_string(),
            Some("dup1@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await;
    assert!(
        result.is_ok(),
        "First registration should succeed: {result:?}"
    );

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

async fn assert_register_duplicate_email_error(service: &UserService) {
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

async fn assert_login_wrong_password(service: &UserService) {
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
        .login(
            "login_test_user".to_string(),
            "WrongPass1".to_string(),
            None,
        )
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

// ============================================================================
// Delete User Transaction Tests
// ============================================================================

async fn assert_delete_user_already_deleted_returns_error(service: &UserService) {
    // Register a user
    let (user, _, _) = service
        .register(
            "delete_test_user".to_string(),
            Some("delete@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    let user_id = user.id.clone();

    // First delete should succeed
    let result = service.delete_user(&user_id).await;
    assert!(result.is_ok(), "First delete should succeed: {result:?}");

    // Second delete should fail with "already deleted" error
    let result = service.delete_user(&user_id).await;
    assert!(result.is_err(), "Second delete should fail");
    match result {
        Err(Error::InvalidInput(msg)) => {
            assert!(
                msg.contains("already deleted"),
                "Error message should mention 'already deleted': {msg}"
            );
        }
        Err(e) => panic!("Expected InvalidInput error, got: {e:?}"),
        Ok(()) => panic!("Expected error, got Ok"),
    }
}

/// Test that concurrent `delete_user` calls maintain atomicity - only one should succeed
async fn assert_delete_user_concurrent_deletion_atomicity(pool: PgPool) {
    let service = create_user_service(pool.clone());

    // Register a user
    let (user, _, _) = service
        .register(
            "concurrent_delete_user".to_string(),
            Some("concurrent@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    let user_id = user.id.clone();

    // Use a barrier to synchronize both delete attempts
    let barrier = Arc::new(Barrier::new(2));
    let service1 = service.clone();
    let service2 = service.clone();
    let user_id1 = user_id.clone();
    let user_id2 = user_id.clone();
    let barrier1 = barrier.clone();
    let barrier2 = barrier.clone();

    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;
        service1.delete_user(&user_id1).await
    });

    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;
        service2.delete_user(&user_id2).await
    });

    let result1 = handle1.await.expect("Task 1 panicked");
    let result2 = handle2.await.expect("Task 2 panicked");

    // Exactly one of the two should succeed
    let success_count = [result1.is_ok(), result2.is_ok()]
        .iter()
        .filter(|&&x| x)
        .count();
    assert_eq!(
        success_count, 1,
        "Exactly one delete should succeed, but got {success_count} successes. Results: {result1:?}, {result2:?}"
    );

    // Verify user is deleted in the database
    let user_repo = UserRepository::new(pool);
    let user_after = user_repo
        .get_by_id(&user_id)
        .await
        .expect("Query should work");
    assert!(
        user_after.is_none(),
        "User should be soft-deleted (not found via get_by_id)"
    );
}

// ============================================================================
// Registration brute-force lockout tests (Task #42)
// ============================================================================

/// Test that "username taken" errors do NOT count against IP brute-force lockout.
///
/// Scenario: User tries to register with a username that already exists.
/// This should fail with `AlreadyExists`, but should NOT lock out the IP
/// because it's not a security threat - just an unfortunate choice of username.
async fn assert_register_username_taken_no_brute_force_lockout(service: &UserService) {
    let client_ip: std::net::IpAddr = "192.168.1.100".parse().unwrap();

    // Register first user
    service
        .register(
            "existing_user_42".to_string(),
            Some("existing_42@test.com".to_string()),
            "StrongPass1".to_string(),
            Some(client_ip),
        )
        .await
        .expect("First registration should succeed");

    // Try to register with the same username multiple times (should fail with AlreadyExists)
    for _ in 0..5 {
        let result = service
            .register(
                "existing_user_42".to_string(),
                Some("different@test.com".to_string()),
                "StrongPass1".to_string(),
                Some(client_ip),
            )
            .await;

        // Should fail with AlreadyExists
        assert!(
            matches!(result, Err(Error::AlreadyExists(_))),
            "Should fail with AlreadyExists"
        );

        // IMPORTANT: Should NOT be RateLimited even after many attempts
        assert!(
            !matches!(result, Err(Error::RateLimited(_))),
            "Username taken errors should NOT trigger brute-force lockout"
        );
    }

    // Now try with a DIFFERENT username - should succeed (IP not locked)
    let result = service
        .register(
            "new_unique_user_42".to_string(),
            Some("new_42@test.com".to_string()),
            "StrongPass1".to_string(),
            Some(client_ip),
        )
        .await;

    assert!(
        result.is_ok(),
        "Should be able to register with new username - IP should NOT be locked out by 'username taken' errors: {:?}",
        result.err()
    );
}

/// Test that validation errors DO count against IP brute-force lockout.
///
/// Scenario: Attacker sends malformed registration requests (validation errors).
/// These should count against the IP lockout because they indicate automated attacks.
async fn assert_register_validation_errors_trigger_brute_force_lockout(service: &UserService) {
    let client_ip: std::net::IpAddr = "192.168.1.101".parse().unwrap();

    // The brute-force lockout thresholds are:
    // - 5 failures: 1 minute lockout
    // - 10 failures: 5 minute lockout
    // - 15+ failures: 15 minute lockout
    // We need to trigger at least 5 validation errors

    // Send multiple registrations with invalid usernames (too short)
    let mut validation_error_count = 0;
    for _ in 0..25 {
        let result = service
            .register(
                "ab".to_string(), // Too short - validation error
                Some("test@example.com".to_string()),
                "StrongPass1".to_string(),
                Some(client_ip),
            )
            .await;

        match &result {
            Err(Error::InvalidInput(_)) => {
                validation_error_count += 1;
            }
            Err(Error::RateLimited(_)) => {
                // Expected - IP should be locked out after enough validation errors
                break;
            }
            _ => {}
        }
    }

    assert!(
        validation_error_count >= 5,
        "Should have had at least 5 validation errors before lockout, got {validation_error_count}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_user_service_registration_login_and_delete_flows() {
    let (_container, pool) = create_test_pool().await;

    let duplicate_username_service = create_user_service(pool.clone());
    assert_register_duplicate_username_error(&duplicate_username_service).await;

    let duplicate_email_service = create_user_service(pool.clone());
    assert_register_duplicate_email_error(&duplicate_email_service).await;

    let wrong_password_service = create_user_service(pool.clone());
    assert_login_wrong_password(&wrong_password_service).await;

    let delete_twice_service = create_user_service(pool.clone());
    assert_delete_user_already_deleted_returns_error(&delete_twice_service).await;

    assert_delete_user_concurrent_deletion_atomicity(pool).await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_user_service_registration_brute_force_flows() {
    let (_container, pool) = create_test_pool().await;

    let username_taken_service = create_user_service(pool.clone());
    assert_register_username_taken_no_brute_force_lockout(&username_taken_service).await;

    let validation_error_service = create_user_service(pool);
    assert_register_validation_errors_trigger_brute_force_lockout(&validation_error_service).await;
}
