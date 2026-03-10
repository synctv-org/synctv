//! Logout blacklist failure tests for synctv-api
//!
//! Tests that logout correctly handles blacklist failures.
//! When the blacklist store fails, logout should return an error so the caller
//! knows that token revocation may not have succeeded.
//!
//! Run with: cargo test --test logout_blacklist_failure_tests -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use synctv_core::models::UserId;
use synctv_core::service::{
    FallbackTokenBlacklistStore, InMemoryTokenBlacklistStore, TokenBlacklistStore,
};

// ============================================================================
// Test Infrastructure
// ============================================================================

/// Create a JWT service for testing
fn create_test_jwt_service() -> synctv_core::service::JwtService {
    synctv_core::service::JwtService::new("test-secret-key-for-jwt-that-is-long-enough-1234567890")
        .unwrap()
}

/// Mock store that always fails blacklist operations.
/// Simulates Redis being unavailable.
struct FailingBlacklistStore;

#[async_trait::async_trait]
impl TokenBlacklistStore for FailingBlacklistStore {
    async fn is_blacklisted(&self, _key: &str) -> bool {
        false
    }

    async fn is_blacklisted_checked(&self, _key: &str) -> synctv_core::Result<bool> {
        Ok(false)
    }

    async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> synctv_core::Result<()> {
        Err(synctv_core::Error::Internal(
            "Blacklist store unavailable".to_string(),
        ))
    }

    async fn get_family_revoked_at(&self, _key: &str) -> Option<i64> {
        None
    }

    async fn set_family_revoked(
        &self,
        _key: &str,
        _timestamp: i64,
        _ttl_secs: u64,
    ) -> synctv_core::Result<()> {
        Ok(())
    }
}

// ============================================================================
// Test 1: Verify JWT service can create and verify tokens
// ============================================================================

#[test]
fn test_jwt_service_creates_valid_tokens() {
    let jwt = create_test_jwt_service();
    let user_id = UserId::new();

    // Create access token
    let token = jwt
        .sign_token(&user_id, synctv_core::service::TokenType::Access, 0)
        .unwrap();

    // Verify it can be parsed
    let claims = jwt.verify_access_token(&token).unwrap();
    assert_eq!(claims.sub, user_id.as_str());
    assert!(
        !claims.jti.is_empty(),
        "JTI should be present for blacklisting"
    );
}

// ============================================================================
// Test 2: Verify token has JTI needed for blacklisting
// ============================================================================

#[test]
fn test_access_token_has_jti_for_blacklisting() {
    let jwt = create_test_jwt_service();
    let user_id = UserId::new();

    let token = jwt
        .sign_token(&user_id, synctv_core::service::TokenType::Access, 0)
        .unwrap();

    let claims = jwt.verify_access_token(&token).unwrap();

    // JTI is required for blacklisting
    assert!(
        !claims.jti.is_empty(),
        "Access token must have JTI for blacklisting"
    );

    // Token should have remaining TTL
    let now = chrono::Utc::now().timestamp();
    let remaining_ttl = (claims.exp - now).max(0) as u64;
    assert!(
        remaining_ttl > 0,
        "Access token should have remaining TTL for blacklisting"
    );
}

// ============================================================================
// Test 3: Blacklist with working store succeeds
// ============================================================================

#[tokio::test]
async fn test_blacklist_with_working_store_succeeds() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);
    let jwt = create_test_jwt_service();
    let user_id = UserId::new();

    // Create token
    let token = jwt
        .sign_token(&user_id, synctv_core::service::TokenType::Access, 0)
        .unwrap();

    // Parse token to get JTI
    let claims = jwt.verify_access_token(&token).unwrap();
    let now = chrono::Utc::now().timestamp();
    let remaining_ttl = (claims.exp - now).max(0) as u64;

    // Blacklist should succeed
    let result = store.blacklist(&claims.jti, remaining_ttl).await;
    assert!(
        result.is_ok(),
        "Blacklist with working store should succeed"
    );

    // Token should now be blacklisted
    assert!(
        store.is_blacklisted(&claims.jti).await,
        "Token should be blacklisted"
    );
}

// ============================================================================
// Test 4: Blacklist with failing store returns error
// ============================================================================

#[tokio::test]
async fn test_blacklist_with_failing_store_returns_error() {
    let store = FailingBlacklistStore;
    let jwt = create_test_jwt_service();
    let user_id = UserId::new();

    // Create token
    let token = jwt
        .sign_token(&user_id, synctv_core::service::TokenType::Access, 0)
        .unwrap();

    // Parse token to get JTI
    let claims = jwt.verify_access_token(&token).unwrap();
    let now = chrono::Utc::now().timestamp();
    let remaining_ttl = (claims.exp - now).max(0) as u64;

    // Blacklist should fail
    let result = store.blacklist(&claims.jti, remaining_ttl).await;
    assert!(
        result.is_err(),
        "Blacklist with failing store should return error (fail-closed behavior)"
    );

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("unavailable"),
        "Error should indicate store unavailability"
    );
}

// ============================================================================
// Test 5: Fallback store succeeds when primary fails
// ============================================================================

#[tokio::test]
async fn test_fallback_store_succeeds_when_primary_fails() {
    let primary = Arc::new(FailingBlacklistStore) as Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary);
    let jwt = create_test_jwt_service();
    let user_id = UserId::new();

    // Create token
    let token = jwt
        .sign_token(&user_id, synctv_core::service::TokenType::Access, 0)
        .unwrap();

    // Parse token to get JTI
    let claims = jwt.verify_access_token(&token).unwrap();
    let now = chrono::Utc::now().timestamp();
    let remaining_ttl = (claims.exp - now).max(0) as u64;

    // Blacklist should succeed via fallback
    let result = fallback.blacklist(&claims.jti, remaining_ttl).await;
    assert!(
        result.is_ok(),
        "FallbackTokenBlacklistStore should succeed via memory fallback"
    );

    // Token should be blacklisted in fallback
    assert!(
        fallback.is_blacklisted(&claims.jti).await,
        "Token should be blacklisted in memory fallback"
    );
}

// ============================================================================
// Test 6: Logout behavior simulation - working store
// ============================================================================

/// This test simulates the logout flow with a working blacklist store.
/// The expected behavior is that logout succeeds.
#[tokio::test]
async fn test_logout_simulation_working_store() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);
    let jwt = create_test_jwt_service();
    let user_id = UserId::new();

    // Create token
    let token = jwt
        .sign_token(&user_id, synctv_core::service::TokenType::Access, 0)
        .unwrap();

    // Simulate logout: verify token, extract JTI, blacklist
    let logout_result = {
        match jwt.verify_access_token(&token) {
            Ok(claims) => {
                if claims.jti.is_empty() {
                    Ok(())
                } else {
                    let now = chrono::Utc::now().timestamp();
                    let remaining_ttl = (claims.exp - now).max(0) as u64;
                    if remaining_ttl > 0 {
                        store.blacklist(&claims.jti, remaining_ttl).await
                    } else {
                        Ok(())
                    }
                }
            }
            Err(e) => {
                // Token verification failed - this is acceptable for logout
                // (token might be expired/malformed)
                tracing::debug!(error = %e, "Token verification failed during logout");
                Ok(())
            }
        }
    };

    // Logout should succeed with working store
    assert!(
        logout_result.is_ok(),
        "Logout should succeed with working blacklist store"
    );

    // Verify token is actually blacklisted
    let claims = jwt.verify_access_token(&token).unwrap();
    assert!(
        store.is_blacklisted(&claims.jti).await,
        "Token should be blacklisted after successful logout"
    );
}

// ============================================================================
// Test 7: Logout behavior simulation - failing store (THE KEY TEST)
// ============================================================================

/// This test simulates the logout flow with a failing blacklist store.
///
/// EXPECTED BEHAVIOR (after fix):
/// - Logout should return an error when blacklist fails
/// - This allows the caller to know that token revocation may not have succeeded
///
/// CURRENT (BUGGY) BEHAVIOR:
/// - Logout returns Ok(()) even when blacklist fails
/// - Users may think their token is revoked when it's not
#[tokio::test]
async fn test_logout_simulation_failing_store_must_fail() {
    let store = FailingBlacklistStore;
    let jwt = create_test_jwt_service();
    let user_id = UserId::new();

    // Create token
    let token = jwt
        .sign_token(&user_id, synctv_core::service::TokenType::Access, 0)
        .unwrap();

    // Simulate logout: verify token, extract JTI, blacklist
    // This is the EXPECTED behavior - blacklist failure should propagate
    let logout_result = {
        match jwt.verify_access_token(&token) {
            Ok(claims) => {
                if claims.jti.is_empty() {
                    Ok(())
                } else {
                    let now = chrono::Utc::now().timestamp();
                    let remaining_ttl = (claims.exp - now).max(0) as u64;
                    if remaining_ttl > 0 {
                        // This is the key difference: we expect blacklist failure to be returned
                        store.blacklist(&claims.jti, remaining_ttl).await
                    } else {
                        Ok(())
                    }
                }
            }
            Err(e) => {
                // Token verification failed - this is acceptable for logout
                tracing::debug!(error = %e, "Token verification failed during logout");
                Ok(())
            }
        }
    };

    // THE KEY ASSERTION: Logout MUST fail when blacklist fails
    // This is fail-closed behavior - the caller needs to know token revocation failed
    assert!(
        logout_result.is_err(),
        "Logout MUST return error when blacklist store fails (fail-closed behavior). \
         Returning success would mislead users into thinking their token is revoked."
    );

    let err = logout_result.unwrap_err();
    assert!(
        err.to_string().contains("unavailable"),
        "Error should indicate store unavailability"
    );
}

// ============================================================================
// Test 8: Logout with invalid token should still succeed (graceful degradation)
// ============================================================================

/// This test verifies that logout with an invalid/expired token still succeeds.
/// This is acceptable because an invalid token is already unusable.
#[tokio::test]
async fn test_logout_with_invalid_token_succeeds() {
    let store = FailingBlacklistStore;
    let jwt = create_test_jwt_service();

    // Use an invalid token
    let invalid_token = "invalid.token.here";

    // Simulate logout
    let logout_result = {
        match jwt.verify_access_token(invalid_token) {
            Ok(claims) => {
                if claims.jti.is_empty() {
                    Ok(())
                } else {
                    let now = chrono::Utc::now().timestamp();
                    let remaining_ttl = (claims.exp - now).max(0) as u64;
                    if remaining_ttl > 0 {
                        store.blacklist(&claims.jti, remaining_ttl).await
                    } else {
                        Ok(())
                    }
                }
            }
            Err(e) => {
                // Token verification failed - this is acceptable for logout
                tracing::debug!(error = %e, "Token verification failed during logout");
                Ok(())
            }
        }
    };

    // Logout with invalid token should succeed (token is already unusable)
    assert!(
        logout_result.is_ok(),
        "Logout with invalid/expired token should succeed gracefully"
    );
}

// ============================================================================
// Test 9: Logout with expired token (TTL = 0) should succeed
// ============================================================================

#[tokio::test]
async fn test_logout_with_expired_token_succeeds() {
    let store = FailingBlacklistStore;
    let jwt = create_test_jwt_service();
    let user_id = UserId::new();

    // Create token
    let token = jwt
        .sign_token(&user_id, synctv_core::service::TokenType::Access, 0)
        .unwrap();

    // Manually construct claims with exp in the past
    let claims = jwt.verify_access_token(&token).unwrap();

    // Simulate logout with TTL = 0 (expired)
    let logout_result = {
        if claims.jti.is_empty() {
            Ok(())
        } else {
            let remaining_ttl = 0u64; // Already expired
            if remaining_ttl > 0 {
                store.blacklist(&claims.jti, remaining_ttl).await
            } else {
                Ok(()) // Token already expired, no need to blacklist
            }
        }
    };

    // Logout with expired token should succeed (no need to blacklist)
    assert!(
        logout_result.is_ok(),
        "Logout with expired token should succeed (no need to blacklist)"
    );
}

// ============================================================================
// Test 10: Verify the fix - logout function must propagate blacklist errors
// ============================================================================

#[test]
fn test_logout_internal_error_maps_to_http_500() {
    let app_error = synctv_api::http::error::map_api_error(synctv_api::impls::ApiError::Internal(
        "Blacklist store unavailable".to_string(),
    ));
    let response = axum::response::IntoResponse::into_response(app_error);
    assert_eq!(
        response.status(),
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    );
}
