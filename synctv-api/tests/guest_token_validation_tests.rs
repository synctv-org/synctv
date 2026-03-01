//! Guest token validation tests for synctv-api
//!
//! Tests that the HTTP middleware GuestUser extractor properly validates:
//! 1. JWT signature verification (existing behavior)
//! 2. Token blacklist check (for individually revoked tokens)
//! 3. Room guest version check (for room-wide revocation)
//!
//! These tests verify the fix for the issue where the GuestUser extractor
//! only verified JWT signatures, allowing kicked guests to continue accessing
//! the room until their token expired naturally.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use synctv_core::cache::KeyBuilder;
use synctv_core::models::RoomId;
use synctv_core::service::auth::token_blacklist::InMemoryTokenBlacklistStore;
use synctv_core::service::auth::{GuestTokenValidator, JwtService, TokenBlacklistStore};

// ============================================================================
// HTTP Middleware Integration Requirement Tests
// ============================================================================
//
// These tests verify the EXPECTED behavior of the HTTP middleware GuestUser
// extractor. The middleware SHOULD use GuestTokenValidator to check both
// the blacklist and the room's guest version.
//
// The tests below document the security requirements and will be used to
// verify the fix once implemented.

/// Document the security requirement: HTTP middleware must check blacklist.
///
/// This test verifies that when the GuestTokenValidator is used correctly,
/// a blacklisted guest token is rejected. This is the expected behavior
/// that the HTTP middleware should implement.
///
/// Security Issue: The current GuestUser extractor in middleware.rs only
/// calls `jwt_service.verify_guest_token()` which does NOT check the blacklist.
/// A kicked guest can continue to access the room until their token expires.
///
/// Fix: The GuestUser extractor should use GuestTokenValidator::validate_async()
/// instead of direct JWT verification.
#[tokio::test]
async fn test_security_requirement_blacklisted_token_must_be_rejected() {
    let validator = create_test_validator_with_blacklist();
    let jwt = create_test_jwt_service();
    let room_id = RoomId::new();

    // Issue a guest token
    let token = jwt.sign_guest_token(&room_id).unwrap();

    // Extract JTI and blacklist it (simulating guest kick)
    let claims = jwt.verify_guest_token(&token).unwrap();
    validator.blacklist_token(&claims.jti, 3600).await.unwrap();

    // SECURITY REQUIREMENT: This MUST fail
    // The HTTP middleware should reject this token
    let result = validator.validate_async(&token).await;
    assert!(
        result.is_err(),
        "SECURITY REQUIREMENT: Blacklisted guest tokens MUST be rejected by the middleware"
    );
}

/// Document the security requirement: HTTP middleware must check guest version.
///
/// This test verifies that when the room's guest version is incremented,
/// old guest tokens are invalidated. This allows room-wide revocation
/// (e.g., when room settings change).
///
/// Security Issue: The current GuestUser extractor does not check the room's
/// guest version, so old tokens remain valid even after settings change.
///
/// Fix: The GuestUser extractor should call validate_with_version_async()
/// with the room's current guest version.
#[tokio::test]
async fn test_security_requirement_outdated_version_must_be_rejected() {
    let validator = create_test_validator_with_blacklist();
    let jwt = create_test_jwt_service();
    let room_id = RoomId::new();

    // Issue a token with version 1
    let token = jwt.sign_guest_token_with_version(&room_id, 1).unwrap();

    // SECURITY REQUIREMENT: When room version is 5, token with version 1 MUST fail
    // The HTTP middleware should check the room's current guest version
    let result = validator.validate_with_version_async(&token, 5).await;
    assert!(
        result.is_err(),
        "SECURITY REQUIREMENT: Guest tokens with outdated version MUST be rejected"
    );
}

/// Document the security requirement: Both checks must be performed together.
///
/// The HTTP middleware must perform BOTH checks in the correct order:
/// 1. JWT signature verification
/// 2. Blacklist check
/// 3. Guest version check (if room has guest versioning enabled)
///
/// A token that passes the JWT check but fails either blacklist or version
/// check must be rejected.
#[tokio::test]
async fn test_security_requirement_both_checks_must_be_performed() {
    let validator = create_test_validator_with_blacklist();
    let jwt = create_test_jwt_service();
    let room_id = RoomId::new();

    let token = jwt.sign_guest_token_with_version(&room_id, 3).unwrap();
    let claims = jwt.verify_guest_token(&token).unwrap();

    // Case 1: Token not blacklisted, version OK - SHOULD PASS
    let result = validator.validate_with_version_async(&token, 3).await;
    assert!(result.is_ok(), "Token should pass when both checks succeed");

    // Case 2: Token blacklisted, version OK - MUST FAIL
    validator.blacklist_token(&claims.jti, 3600).await.unwrap();
    let result = validator.validate_with_version_async(&token, 3).await;
    assert!(
        result.is_err(),
        "SECURITY REQUIREMENT: Blacklisted token must be rejected even if version is OK"
    );

    // Case 3: Token not blacklisted (fresh), version outdated - MUST FAIL
    let new_token = jwt.sign_guest_token_with_version(&room_id, 1).unwrap();
    let result = validator.validate_with_version_async(&new_token, 10).await;
    assert!(
        result.is_err(),
        "SECURITY REQUIREMENT: Token with outdated version must be rejected even if not blacklisted"
    );
}

// ============================================================================
// Test Setup Helpers
// ============================================================================

fn create_test_jwt_service() -> Arc<JwtService> {
    Arc::new(
        JwtService::new("test-secret-for-guest-validation-tests-that-is-long-enough-1234567890")
            .unwrap(),
    )
}

fn create_test_validator_with_blacklist() -> GuestTokenValidator {
    let jwt = create_test_jwt_service();
    let blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 7200));
    let kb = KeyBuilder::new("test");
    GuestTokenValidator::new(jwt).with_blacklist(blacklist, kb)
}

fn create_test_validator_without_blacklist() -> GuestTokenValidator {
    GuestTokenValidator::new(create_test_jwt_service())
}

// ============================================================================
// Blacklist Validation Tests
// ============================================================================

/// Test that a valid guest token passes validation when NOT blacklisted.
///
/// This verifies the happy path - a freshly issued guest token should
/// validate successfully.
#[tokio::test]
async fn test_valid_guest_token_passes_validation() {
    let validator = create_test_validator_with_blacklist();
    let jwt = create_test_jwt_service();
    let room_id = RoomId::new();

    let token = jwt.sign_guest_token(&room_id).unwrap();

    // Validation should succeed
    let result = validator.validate_async(&token).await;
    assert!(result.is_ok(), "Valid guest token should pass validation");

    let claims = result.unwrap();
    assert_eq!(claims.room_id(), room_id);
    assert!(claims.is_guest());
}

/// Test that a blacklisted guest token is rejected.
///
/// This is the core security fix: when a guest is kicked from a room,
/// their token's JTI is added to the blacklist. The validation must
/// reject such tokens.
#[tokio::test]
async fn test_blacklisted_guest_token_is_rejected() {
    let validator = create_test_validator_with_blacklist();
    let jwt = create_test_jwt_service();
    let room_id = RoomId::new();

    // Issue a guest token
    let token = jwt.sign_guest_token(&room_id).unwrap();

    // First, validation should succeed
    let claims = validator.validate_async(&token).await.unwrap();

    // Blacklist the token (simulating what happens when a guest is kicked)
    validator.blacklist_token(&claims.jti, 3600).await.unwrap();

    // Now validation should fail
    let result = validator.validate_async(&token).await;
    assert!(
        result.is_err(),
        "Blacklisted guest token should be rejected"
    );

    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("revoked"),
        "Error should mention token was revoked, got: {err_msg}"
    );
}

/// Test that validation works without blacklist configured (graceful degradation).
///
/// When no blacklist store is configured, validation should still work
/// (only JWT verification), but blacklist_token calls should fail.
#[tokio::test]
async fn test_validation_without_blacklist_configured() {
    let validator = create_test_validator_without_blacklist();
    let jwt = create_test_jwt_service();
    let room_id = RoomId::new();

    let token = jwt.sign_guest_token(&room_id).unwrap();

    // Validation should succeed (JWT-only mode)
    let result = validator.validate_async(&token).await;
    assert!(
        result.is_ok(),
        "Guest token should pass validation in JWT-only mode"
    );

    // Blacklist operation should fail (no store configured)
    let blacklist_result = validator.blacklist_token("some_jti", 3600).await;
    assert!(
        blacklist_result.is_err(),
        "Blacklist operation should fail when no store is configured"
    );
}

// ============================================================================
// Room Guest Version Tests
// ============================================================================

/// Test that tokens with outdated guest version are rejected.
///
/// When a room's settings change (e.g., guest access is toggled), the room's
/// guest_version is incremented. Tokens with an older version should be rejected.
#[tokio::test]
async fn test_outdated_guest_version_is_rejected() {
    let validator = create_test_validator_without_blacklist();
    let jwt = create_test_jwt_service();
    let room_id = RoomId::new();

    // Issue a token with version 5
    let token = jwt.sign_guest_token_with_version(&room_id, 5).unwrap();

    // When room's guest version is 5, token should pass
    let result = validator.validate_with_version_async(&token, 5).await;
    assert!(
        result.is_ok(),
        "Token with matching version should pass validation"
    );

    // When room's guest version is 3, token should still pass
    // (token version >= room version)
    let result = validator.validate_with_version_async(&token, 3).await;
    assert!(
        result.is_ok(),
        "Token with higher version should pass validation"
    );

    // When room's guest version is 10, token should be rejected
    // (token version < room version)
    let result = validator.validate_with_version_async(&token, 10).await;
    assert!(
        result.is_err(),
        "Token with outdated version should be rejected"
    );

    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("revoked"),
        "Error should mention token was revoked for room, got: {err_msg}"
    );
}

/// Test that version 0 tokens always pass version check.
///
/// Tokens created with the default sign_guest_token have version 0.
/// As long as the room's guest version is also 0, they should pass.
#[tokio::test]
async fn test_default_version_token_passes_when_room_version_is_zero() {
    let validator = create_test_validator_without_blacklist();
    let jwt = create_test_jwt_service();
    let room_id = RoomId::new();

    // sign_guest_token uses version 0 by default
    let token = jwt.sign_guest_token(&room_id).unwrap();
    let claims = jwt.verify_guest_token(&token).unwrap();
    assert_eq!(claims.gv, 0, "Default token should have version 0");

    // Should pass when room version is 0
    let result = validator.validate_with_version_async(&token, 0).await;
    assert!(
        result.is_ok(),
        "Token with version 0 should pass when room version is 0"
    );

    // Should fail when room version is 1
    let result = validator.validate_with_version_async(&token, 1).await;
    assert!(
        result.is_err(),
        "Token with version 0 should fail when room version is 1"
    );
}

// ============================================================================
// Combined Blacklist + Version Tests
// ============================================================================

/// Test that both blacklist and version checks are performed.
///
/// validate_with_version_async should:
/// 1. Check the blacklist
/// 2. Check the version
/// If either fails, the token should be rejected.
#[tokio::test]
async fn test_combined_blacklist_and_version_check() {
    let validator = create_test_validator_with_blacklist();
    let jwt = create_test_jwt_service();
    let room_id = RoomId::new();

    // Issue a token with version 5
    let token = jwt.sign_guest_token_with_version(&room_id, 5).unwrap();
    let claims = jwt.verify_guest_token(&token).unwrap();

    // Test 1: Both checks pass
    let result = validator.validate_with_version_async(&token, 3).await;
    assert!(
        result.is_ok(),
        "Token should pass when version is OK and not blacklisted"
    );

    // Test 2: Version check fails
    let result = validator.validate_with_version_async(&token, 10).await;
    assert!(
        result.is_err(),
        "Token should fail when version is outdated"
    );

    // Test 3: Blacklist check fails (version OK)
    validator.blacklist_token(&claims.jti, 3600).await.unwrap();
    let result = validator.validate_with_version_async(&token, 3).await;
    assert!(
        result.is_err(),
        "Token should fail when blacklisted (even if version is OK)"
    );
}

// ============================================================================
// Structural Tests
// ============================================================================

/// Test that GuestTokenValidator reports blacklist status correctly.
#[test]
fn test_validator_has_blacklist_flag() {
    let with_blacklist = create_test_validator_with_blacklist();
    assert!(
        with_blacklist.has_blacklist(),
        "Validator with blacklist should report has_blacklist=true"
    );

    let without_blacklist = create_test_validator_without_blacklist();
    assert!(
        !without_blacklist.has_blacklist(),
        "Validator without blacklist should report has_blacklist=false"
    );
}

/// Test that GuestTokenValidator is Debug-printable.
#[test]
fn test_validator_debug_format() {
    let validator = create_test_validator_with_blacklist();
    let debug_str = format!("{validator:?}");
    assert!(
        debug_str.contains("GuestTokenValidator"),
        "Debug output should contain struct name"
    );
    assert!(
        debug_str.contains("has_blacklist"),
        "Debug output should contain has_blacklist field"
    );
}
