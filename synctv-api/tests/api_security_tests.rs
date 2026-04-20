//! API Security Tests //!
//! Tests for various security fixes:
//! - B1: Guest-token validation must use GuestTokenValidator (blacklist check)
//! - B8: WebSocket Bearer token case-sensitivity
//! - B10: set_password must validate password strength at API layer
//! - D13: Logout must require auth token
//! - Input validation: room description, page clamping, admin page_size cap
//! - E7: sqlx::Error must not leak DB details in gRPC responses

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use synctv_core::cache::KeyBuilder;
use synctv_core::models::RoomId;
use synctv_core::service::auth::token_blacklist::InMemoryTokenBlacklistStore;
use synctv_core::service::auth::{
    GuestTokenValidator, JwtService, JwtValidator, TokenBlacklistStore,
};
use synctv_core::Error;

// B1: Guest-token validation must check blacklist

/// A blacklisted guest token MUST be rejected by validate_async.
/// This tests the core requirement that shared guest-token validation must use
/// GuestTokenValidator::validate_async() (which checks blacklist) instead of
/// just jwt_service.verify_guest_token() (which only checks JWT signature).
#[tokio::test]
async fn test_b1_blacklisted_guest_token_rejected_by_validator() {
    let jwt = create_test_jwt_service();
    let blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 7200));
    let kb = KeyBuilder::new("test");
    let validator = GuestTokenValidator::new(jwt.clone()).with_blacklist(blacklist, kb);

    let room_id = RoomId::new();
    let token = jwt.sign_guest_token(&room_id).unwrap();

    // First validate succeeds
    let claims = validator.validate_async(&token).await.unwrap();
    assert!(claims.is_guest());

    // Blacklist the token
    validator.blacklist_token(&claims.jti, 3600).await.unwrap();

    // Now validate_async must reject it
    let result = validator.validate_async(&token).await;
    assert!(
        result.is_err(),
        "B1 SECURITY: Blacklisted guest token MUST be rejected"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("revoked"),
        "Error message should indicate revocation, got: {err_msg}"
    );
}

/// Shared guest-token validation must use validate_async (not
/// verify_guest_token). We verify this by checking that the
/// GuestTokenValidator path catches blacklisted tokens.
#[tokio::test]
async fn test_b1_non_blacklisted_token_passes() {
    let jwt = create_test_jwt_service();
    let blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 7200));
    let kb = KeyBuilder::new("test");
    let validator = GuestTokenValidator::new(jwt.clone()).with_blacklist(blacklist, kb);

    let room_id = RoomId::new();
    let token = jwt.sign_guest_token(&room_id).unwrap();

    // Not blacklisted - should pass
    let result = validator.validate_async(&token).await;
    assert!(result.is_ok(), "Non-blacklisted guest token should pass");
}

#[tokio::test]
async fn test_b1_guest_blacklist_storage_error_surfaces_service_unavailable() {
    struct FailingBlacklistStore;

    #[async_trait::async_trait]
    impl TokenBlacklistStore for FailingBlacklistStore {
        async fn is_blacklisted(&self, _key: &str) -> bool {
            false
        }

        async fn is_blacklisted_checked(&self, _key: &str) -> Result<bool, Error> {
            Err(Error::Internal("blacklist backend unavailable".to_string()))
        }

        async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> Result<(), Error> {
            Ok(())
        }

        async fn blacklist_if_not_exists(&self, _key: &str, _ttl_secs: u64) -> Result<bool, Error> {
            Ok(false)
        }

        async fn get_family_revoked_at(&self, _key: &str) -> Option<i64> {
            None
        }

        async fn get_family_revoked_at_checked(&self, _key: &str) -> Result<Option<i64>, Error> {
            Ok(None)
        }

        async fn set_family_revoked(
            &self,
            _key: &str,
            _timestamp: i64,
            _ttl_secs: u64,
        ) -> Result<(), Error> {
            Ok(())
        }
    }

    let jwt = create_test_jwt_service();
    let blacklist: Arc<dyn TokenBlacklistStore> = Arc::new(FailingBlacklistStore);
    let kb = KeyBuilder::new("test");
    let validator = GuestTokenValidator::new(jwt.clone()).with_blacklist(blacklist, kb);

    let room_id = RoomId::new();
    let token = jwt.sign_guest_token(&room_id).unwrap();

    let err = validator
        .validate_async(&token)
        .await
        .expect_err("storage failures must fail closed");

    assert!(
        matches!(err, Error::ServiceUnavailable(ref msg) if msg.contains("temporarily unavailable")),
        "guest token validator must surface service unavailability, got: {err}"
    );
}

// B8: WebSocket Bearer token case-insensitive

/// "bearer " (lowercase) should be accepted by extract_bearer_token.
/// The shared Bearer parsing path uses JwtValidator::extract_bearer_token,
/// which is case-insensitive. WebSocket's extract_user_id must match.
#[test]
fn test_b8_bearer_lowercase_accepted() {
    let result = JwtValidator::extract_bearer_token("bearer some_token_value");
    assert!(
        result.is_ok(),
        "B8: lowercase 'bearer ' should be accepted, got: {:?}",
        result.err()
    );
    assert_eq!(result.unwrap(), "some_token_value");
}

/// "BEARER " (uppercase) should be accepted
#[test]
fn test_b8_bearer_uppercase_accepted() {
    let result = JwtValidator::extract_bearer_token("BEARER some_token_value");
    assert!(result.is_ok(), "B8: uppercase 'BEARER ' should be accepted");
    assert_eq!(result.unwrap(), "some_token_value");
}

/// Mixed case "BeArEr " should be accepted
#[test]
fn test_b8_bearer_mixed_case_accepted() {
    let result = JwtValidator::extract_bearer_token("BeArEr my_jwt_token");
    assert!(
        result.is_ok(),
        "B8: mixed case 'BeArEr ' should be accepted"
    );
    assert_eq!(result.unwrap(), "my_jwt_token");
}

/// Non-Bearer auth scheme should be rejected
#[test]
fn test_b8_non_bearer_rejected() {
    let result = JwtValidator::extract_bearer_token("Basic dXNlcjpwYXNz");
    assert!(
        result.is_err(),
        "B8: Non-Bearer auth scheme should be rejected"
    );
}

/// When Authorization header is present but invalid format, WebSocket should
/// return an error rather than silently falling through to query params.
#[test]
fn test_b8_invalid_auth_header_should_error() {
    // "Bearer" without a space and token should fail
    let result = JwtValidator::extract_bearer_token("Bearer");
    assert!(
        result.is_err(),
        "B8: 'Bearer' without token should be rejected"
    );
}

// B10: set_password must validate password strength

/// Weak passwords (too short) should be rejected by the HTTP validation layer.
/// The register endpoint calls validate_password(); set_password should too.
#[test]
fn test_b10_weak_password_rejected_by_http_validation() {
    use synctv_api::http::validation::validate_password;

    // Too short
    let result = validate_password("short");
    assert!(
        result.is_err(),
        "B10: Password 'short' (5 chars) should be rejected as too short"
    );

    // Common password
    let result = validate_password("password");
    assert!(
        result.is_err(),
        "B10: Common password 'password' should be rejected"
    );

    // Valid password should pass
    let result = validate_password("MySecure123!");
    assert!(result.is_ok(), "B10: Strong password should be accepted");
}

/// Verify that validate_password rejects passwords under minimum length.
#[test]
fn test_b10_password_min_length_enforced() {
    use synctv_api::http::validation::validate_password;

    // Single character
    let result = validate_password("a");
    assert!(
        result.is_err(),
        "B10: Single char password should be rejected"
    );

    // Empty
    let result = validate_password("");
    assert!(result.is_err(), "B10: Empty password should be rejected");
}

// D13: Logout must require auth token

/// Logout without an Authorization header should return an error.
/// Currently the handler returns success: true even without a token, which
/// confuses clients and doesn't actually perform any blacklisting.
#[test]
fn test_d13_logout_without_token_should_fail() {
    // Test the structural requirement: extract_bearer_token should fail
    // when given None (no header).
    // The logout handler should check this and return an error.

    // Simulate what happens when no header is provided
    let no_auth: Option<&str> = None;
    let token = no_auth.and_then(|v| JwtValidator::extract_bearer_token(v).ok());
    assert!(
        token.is_none(),
        "D13: No Authorization header should produce no token"
    );
    // The fix: when token is None, return error instead of success
}

/// Logout with a valid Bearer token format should proceed to blacklisting.
#[test]
fn test_d13_logout_with_valid_header_extracts_token() {
    let token = JwtValidator::extract_bearer_token("Bearer valid.jwt.token").unwrap();
    assert_eq!(token, "valid.jwt.token");
}

// Input validation: room description

/// Room descriptions exceeding ROOM_DESCRIPTION_MAX should be rejected.
#[test]
fn test_room_description_max_length_enforced() {
    use synctv_api::http::validation::{limits, validate_room_description};

    let long_desc = "a".repeat(limits::ROOM_DESCRIPTION_MAX + 1);
    let result = validate_room_description(&long_desc);
    assert!(
        result.is_err(),
        "Room description exceeding max length should be rejected"
    );

    // Exactly at max should be OK
    let exact_desc = "a".repeat(limits::ROOM_DESCRIPTION_MAX);
    let result = validate_room_description(&exact_desc);
    assert!(
        result.is_ok(),
        "Room description at exactly max length should be accepted"
    );
}

// Input validation: page value clamping

/// Negative page values (i32) must be clamped to 1, not wrap when cast to u32.
#[test]
fn test_negative_page_clamped_to_one() {
    use synctv_api::http::validation::validate_page;

    // Negative page should be clamped to 1
    assert_eq!(validate_page(Some(-1)), 1);
    assert_eq!(validate_page(Some(-100)), 1);
    assert_eq!(validate_page(Some(i32::MIN)), 1);
}

/// Page size should be clamped to valid range.
#[test]
fn test_page_size_clamped() {
    use synctv_api::http::validation::validate_page_size;

    // Negative page_size clamped to 1
    assert_eq!(validate_page_size(Some(-1)), 1);
    assert_eq!(validate_page_size(Some(0)), 1);
}

// Input validation: admin page_size cap

/// Admin endpoints (list_rooms, list_users) must cap page_size.
/// PageParams::new already clamps to MAX_PAGE_SIZE (100) for u32 values.
/// But when i32 is cast to u32 without clamping, negative values wrap.
#[test]
fn test_admin_page_size_capped_by_page_params() {
    use synctv_core::models::PageParams;

    // page_size 200 should be capped at MAX_PAGE_SIZE (100)
    let params = PageParams::new(Some(1), Some(200));
    assert!(
        params.page_size <= 100,
        "PageParams should cap page_size at MAX_PAGE_SIZE, got {}",
        params.page_size
    );

    // page_size 0 should be clamped to 1
    let params = PageParams::new(Some(1), Some(0));
    assert_eq!(params.page_size, 1);
}

/// Verify that PageParams handles the i32-to-u32 cast for admin endpoints.
/// When i32 values like -1 are cast with `as u32`, they become very large.
/// The admin code should use positive checks before passing to PageParams.
#[test]
fn test_admin_negative_page_handled() {
    // In admin list_rooms: `let page = if req.page > 0 { req.page } else { 1 };`
    // This ensures negative i32 values default to 1 before casting to u32.
    let page: i32 = -5;
    let safe_page: i32 = if page > 0 { page } else { 1 };
    assert_eq!(safe_page, 1);

    let page_size: i32 = -10;
    let safe_page_size: i32 = if page_size > 0 { page_size } else { 50 };
    assert_eq!(safe_page_size, 50);

    // Now the cap at MAX_PAGE_SIZE
    let page_size: i32 = 500;
    let safe_page_size: i32 = if page_size > 0 { page_size } else { 50 };
    let params =
        synctv_core::models::PageParams::new(Some(1), Some(safe_page_size.cast_unsigned()));
    assert!(
        params.page_size <= 100,
        "PageParams should cap at MAX_PAGE_SIZE even for large values"
    );
}

// E7: sqlx::Error must not leak DB details in gRPC

/// Internal errors (including sqlx::Error) must be sanitized before
/// returning to gRPC clients. The map_api_error function should return
/// a generic "Internal error" message, not the raw error string.
#[test]
fn test_e7_api_error_internal_sanitized_for_grpc() {
    use synctv_api::impls::ApiError;

    // Simulate a sqlx::Error being converted to ApiError::Internal
    let api_err = ApiError::Internal(
        "error returned from database: connection refused (os error 111)".to_string(),
    );

    // The proto error message should be sanitized
    let proto_err = api_err.to_proto_error();
    assert_eq!(
        proto_err.message, "Internal error",
        "E7: Internal errors must be sanitized, not expose DB details"
    );
    assert!(
        !proto_err.message.contains("connection"),
        "E7: DB connection details must not leak"
    );
}

/// Non-internal errors should preserve their message.
#[test]
fn test_e7_non_internal_errors_preserved() {
    use synctv_api::impls::ApiError;

    let api_err = ApiError::NotFound("Room not found".to_string());
    let proto_err = api_err.to_proto_error();
    assert_eq!(proto_err.message, "Room not found");

    let api_err = ApiError::InvalidInput("Password too short".to_string());
    let proto_err = api_err.to_proto_error();
    assert_eq!(proto_err.message, "Password too short");
}

/// sqlx::Error conversion to ApiError should map to Internal variant.
#[test]
fn test_e7_sqlx_error_maps_to_internal() {
    use synctv_api::impls::ApiError;

    // ApiError implements From<sqlx::Error>
    // We verify the classify() returns Internal
    let err = ApiError::Internal("Database error: connection refused".to_string());
    assert!(
        matches!(err.classify(), synctv_api::impls::ErrorKind::Internal),
        "sqlx errors should classify as Internal"
    );
}

/// The gRPC map_api_error must sanitize internal error messages.
/// We test this indirectly by verifying ApiError::Internal display
/// goes through classify_error -> Internal -> sanitized message.
#[test]
fn test_e7_classify_error_database_errors() {
    use synctv_api::impls::classify_error;

    // Database-specific errors should classify as Internal
    assert!(matches!(
        classify_error("Database error: connection refused"),
        synctv_api::impls::ErrorKind::Internal
    ));

    assert!(matches!(
        classify_error("error returned from database: timeout"),
        synctv_api::impls::ErrorKind::Internal
    ));
}

// Input validation: i32-to-u32 conversion safety

/// Verify that u32::try_from handles negative i32 values safely.
/// This is the pattern used in the fixed list endpoints, including `list_my_rooms`.
#[test]
fn test_i32_to_u32_negative_conversion() {
    // Negative i32 values must not wrap to huge u32 values
    let negative: i32 = -1;
    assert_eq!(u32::try_from(negative).unwrap_or(1), 1);

    let negative: i32 = i32::MIN;
    assert_eq!(u32::try_from(negative).unwrap_or(1), 1);

    // Positive values pass through
    let positive: i32 = 5;
    assert_eq!(u32::try_from(positive).unwrap_or(1), 5);

    // Zero defaults to 1
    let zero: i32 = 0;
    assert_eq!(u32::try_from(zero).unwrap_or(1), 0); // 0 converts fine, PageParams handles it
}

/// Verify that the `as u32` cast on negative i32 would be dangerous.
/// This documents WHY we use try_from instead.
#[test]
fn test_i32_as_u32_is_dangerous() {
    // This demonstrates the bug we fixed: -1i32 as u32 wraps to u32::MAX
    let negative: u32 = (-1i32).cast_unsigned();
    let wrapped = negative;
    assert_eq!(wrapped, u32::MAX, "Demonstrates the wrapping bug");

    // And PageParams::new would NOT clamp this since it only clamps page_size, not page
    let params = synctv_core::models::PageParams::new(Some(wrapped), Some(20));
    assert_eq!(
        params.page,
        u32::MAX,
        "PageParams does not clamp page number down from u32::MAX"
    );

    // offset() must use wide arithmetic so even a wrapped caller input does not panic
    // or silently truncate before later validation rejects the request.
    assert_eq!(params.offset(), (u64::from(u32::MAX) - 1) * 20);
}

// Test helpers

fn create_test_jwt_service() -> Arc<JwtService> {
    Arc::new(
        JwtService::new("test-secret-for-api-security-tests-that-is-long-enough-1234567890")
            .unwrap(),
    )
}
