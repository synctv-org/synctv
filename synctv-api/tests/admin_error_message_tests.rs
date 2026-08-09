//! Admin error message consistency tests
//!
//! These tests verify that admin authentication errors do not leak information
//! about user existence or moderation state. All authentication failures should return
//! the same error message to prevent user enumeration attacks.
//!
//! validate_admin_auth must not expose distinct failure causes:
//! - "Failed to verify user" when user lookup fails (line 49)
//! - "Authentication failed" when user is banned/deleted (line 54)
//!
//! All user existence/moderation authentication failures should return
//! "Authentication failed".
//!
//! Note: Password change errors ("Token invalidated due to password change...")
//! are intentionally different because they don't leak user existence -
//! the caller already has a valid JWT token.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_api::{AdminAuthValidator, ApiError, ValidatedAdmin};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::UserId,
    service::{
        BruteForceProtection, InMemoryTokenBlacklistStore, JwtService, UserService,
        UserServiceRuntimeOptions,
    },
};
use synctv_core_testing::{create_test_pool, opaque_register_user};

// Test constants - the expected unified error message

/// The unified error message that should be returned for all auth failures
/// that could leak user existence information.
const UNIFIED_AUTH_ERROR_MESSAGE: &str = "Authentication failed";

// Helper functions for testing

fn create_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-admin-error-tests-long-enough-1234567890").unwrap()
}

fn create_user_service(pool: &PgPool) -> UserService {
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
        UserServiceRuntimeOptions {
            password_registration_policy_override: Some(synctv_core::service::RegistrationPolicy {
                enabled: true,
                need_review: false,
            }),
            ..synctv_core::service::UserServiceRuntimeOptions::test_defaults()
        },
    )
}

async fn register_test_user(
    service: &UserService,
    username: &str,
    email: &str,
) -> synctv_core::models::User {
    opaque_register_user(service, username, Some(email.to_string()), "StrongPass1")
        .await
        .expect("Failed to create user")
        .0
}

/// Extract the error message from an ApiError::Authentication variant
fn get_authentication_error_message(result: Result<ValidatedAdmin, ApiError>) -> String {
    match result {
        Err(ApiError::Authentication(msg)) => msg,
        Ok(_) => panic!("Expected Authentication error, but got success"),
        Err(other) => panic!("Expected Authentication error, got: {other:?}"),
    }
}

// Unit tests (no Docker required)

// Integration tests (require Docker)

/// Integration test: Verify user-not-found returns unified error message
///
/// STEPS:
/// 1. Create a test database with a clean state
/// 2. Call validate_admin_auth with a non-existent user ID
/// 3. Verify the error message is UNIFIED_AUTH_ERROR_MESSAGE
///
/// BEFORE FIX: This test will FAIL because the error message is "Failed to verify user"
/// AFTER FIX: This test will PASS because the error message is "Authentication failed"
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_user_not_found_returns_unified_error_message() {
    let (_container, pool) = create_test_pool().await;
    let user_service = create_user_service(&pool);

    // Use a non-existent user ID
    let non_existent_user_id = UserId::expect_positive(10_000_007);
    let token_iat = Utc::now().timestamp();

    let result = AdminAuthValidator::new(&user_service)
        .validate(non_existent_user_id, 0, token_iat)
        .await;

    assert!(result.is_err(), "Non-existent user should fail auth");

    let error_message = get_authentication_error_message(result);

    assert_eq!(
        error_message, UNIFIED_AUTH_ERROR_MESSAGE,
        "User not found should return unified error message 'Authentication failed', \
         got '{error_message}' instead. This prevents user enumeration attacks."
    );
}

/// Integration test: Verify banned user returns unified error message
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_banned_user_returns_unified_error_message() {
    let (_container, pool) = create_test_pool().await;
    let user_service = create_user_service(&pool);

    let user = register_test_user(&user_service, "banned_test_user", "banned@example.com").await;

    user_service
        .ban_user(&user.id, None, None)
        .await
        .expect("Failed to ban user");

    let token_iat = Utc::now().timestamp();

    let result = AdminAuthValidator::new(&user_service)
        .validate(user.id, 0, token_iat)
        .await;

    assert!(result.is_err(), "Banned user should fail auth");

    let error_message = get_authentication_error_message(result);

    assert_eq!(
        error_message, UNIFIED_AUTH_ERROR_MESSAGE,
        "Banned user should return unified error message 'Authentication failed', \
         got '{error_message}' instead"
    );
}

/// Integration test: Verify deleted user returns unified error message
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deleted_user_returns_unified_error_message() {
    let (_container, pool) = create_test_pool().await;
    let user_service = create_user_service(&pool);

    let user = register_test_user(&user_service, "deleted_test_user", "deleted@example.com").await;

    // Delete the user (soft delete)
    user_service
        .delete_user(&user.id)
        .await
        .expect("Failed to delete user");

    let token_iat = Utc::now().timestamp();

    let result = AdminAuthValidator::new(&user_service)
        .validate(user.id, 0, token_iat)
        .await;

    assert!(result.is_err(), "Deleted user should fail auth");

    let error_message = get_authentication_error_message(result);

    assert_eq!(
        error_message, UNIFIED_AUTH_ERROR_MESSAGE,
        "Deleted user should return unified error message 'Authentication failed', \
         got '{error_message}' instead"
    );
}

/// Integration test: Verify active user PASSES auth (sanity check)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_active_user_passes_auth() {
    let (_container, pool) = create_test_pool().await;
    let user_service = create_user_service(&pool);

    let user = register_test_user(&user_service, "active_test_user", "active@example.com").await;

    let token_iat = Utc::now().timestamp();

    let result = AdminAuthValidator::new(&user_service)
        .validate(user.id, 0, token_iat)
        .await;

    assert!(
        result.is_ok(),
        "Active user should pass auth, got error: {:?}",
        result.err()
    );
}

/// Integration test: Verify all failure scenarios return IDENTICAL error messages
///
/// This is the key security test: all user-not-found/banned/deleted/pending scenarios
/// must return the EXACT SAME error message to prevent user enumeration.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_all_failure_scenarios_return_identical_error_messages() {
    let (_container, pool) = create_test_pool().await;
    let user_service = create_user_service(&pool);

    let mut error_messages: Vec<String> = Vec::new();

    // Scenario 1: User not found
    let non_existent_id = UserId::expect_positive(10_000_008);
    let token_iat = Utc::now().timestamp();
    let result = AdminAuthValidator::new(&user_service)
        .validate(non_existent_id, 0, token_iat)
        .await;
    error_messages.push(get_authentication_error_message(result));

    // Scenario 2: Banned user
    let banned_user = register_test_user(
        &user_service,
        "banned_for_comparison",
        "banned_comp@example.com",
    )
    .await;
    user_service
        .ban_user(&banned_user.id, None, None)
        .await
        .unwrap();
    let result = AdminAuthValidator::new(&user_service)
        .validate(banned_user.id, 0, token_iat)
        .await;
    error_messages.push(get_authentication_error_message(result));

    // Scenario 3: Deleted user
    let deleted_user = register_test_user(
        &user_service,
        "deleted_for_comparison",
        "deleted_comp@example.com",
    )
    .await;
    user_service.delete_user(&deleted_user.id).await.unwrap();
    let result = AdminAuthValidator::new(&user_service)
        .validate(deleted_user.id, 0, token_iat)
        .await;
    error_messages.push(get_authentication_error_message(result));

    // Verify all error messages are identical
    assert!(
        !error_messages.is_empty(),
        "Should have collected error messages"
    );

    let first_message = &error_messages[0];
    for (i, msg) in error_messages.iter().enumerate() {
        assert_eq!(
            msg, first_message,
            "Error message {i} differs from first message. All failure scenarios must return \
             identical error messages to prevent user enumeration. Messages: {error_messages:?}"
        );
    }

    // Verify they all match the unified message
    assert_eq!(
        first_message, UNIFIED_AUTH_ERROR_MESSAGE,
        "All error messages should be 'Authentication failed'"
    );
}
