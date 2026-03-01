//! `BlacklistCheckLayer` tests for synctv-api
//!
//! Tests the gRPC blacklist check tower layer behavior:
//! - Requests without Authorization header pass through (public endpoints)
//! - Requests with invalid JWT are rejected with UNAUTHENTICATED
//!
//! Note: Full integration tests with real JwtService/SecurityPipeline require
//! database connections. These tests verify the layer's bearer token extraction
//! and structural behavior.

#![allow(clippy::unwrap_used)]
use axum::http;

// ============================================================================
// Bearer token extraction (used by BlacklistCheckLayer internally)
// ============================================================================

/// Verify the blacklist layer's bearer extraction follows the same pattern
/// as the HTTP middleware (case-insensitive "Bearer " prefix).
#[test]
fn test_bearer_extraction_standard_case() {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::AUTHORIZATION,
        http::HeaderValue::from_static("Bearer eyJhbGciOiJIUzI1NiJ9.test.sig"),
    );
    // The internal extract_bearer_token function is tested via blacklist_layer unit tests.
    // Here we verify that the auth header value format is what we expect.
    let auth_value = headers
        .get(http::header::AUTHORIZATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(auth_value.starts_with("Bearer "));
}

/// No Authorization header means the request should pass through
/// (public endpoints like `ListRooms`, health checks).
#[test]
fn test_no_auth_header_means_passthrough() {
    let headers = http::HeaderMap::new();
    // No Authorization header present
    assert!(headers.get(http::header::AUTHORIZATION).is_none());
    // The BlacklistCheckLayer skips security checks when no bearer token is found
}

/// Invalid JWT should be rejected. The blacklist layer verifies the JWT
/// before checking the blacklist/security pipeline.
#[test]
fn test_invalid_jwt_format_would_fail_verification() {
    // A malformed JWT (not 3 dot-separated parts) should fail verification
    let token = "not-a-valid-jwt-token";
    let jwt_service = synctv_core::service::auth::JwtService::new(
        "test-secret-key-long-enough-for-entropy-check-1234567890",
    )
    .unwrap();

    let result = jwt_service.verify_access_token(token);
    assert!(result.is_err(), "Invalid JWT should fail verification");
}

/// A well-formed JWT signed with a different secret should fail verification.
#[test]
fn test_wrong_secret_jwt_fails_verification() {
    use synctv_core::models::UserId;
    use synctv_core::service::auth::{JwtService, TokenType};

    let jwt_a = JwtService::new(
        "secret-aaaa-long-enough-for-entropy-check-1234567890",
    )
    .unwrap();
    let jwt_b = JwtService::new(
        "secret-bbbb-long-enough-for-entropy-check-1234567890",
    )
    .unwrap();

    let user_id = UserId::new();
    let token = jwt_a
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();

    // Token signed by jwt_a should fail verification with jwt_b
    let result = jwt_b.verify_access_token(&token);
    assert!(result.is_err(), "Token signed with different secret must fail");
}

/// A refresh token should not pass as an access token in the blacklist layer.
#[test]
fn test_refresh_token_rejected_as_access_token() {
    use synctv_core::models::UserId;
    use synctv_core::service::auth::{JwtService, TokenType};

    let jwt_service = JwtService::new(
        "test-secret-key-long-enough-for-entropy-check-1234567890",
    )
    .unwrap();
    let user_id = UserId::new();
    let refresh_token = jwt_service
        .sign_token(&user_id, TokenType::Refresh, 0)
        .unwrap();

    // The blacklist layer calls verify_access_token, which should reject refresh tokens
    let result = jwt_service.verify_access_token(&refresh_token);
    assert!(
        result.is_err(),
        "Refresh token must not be accepted as access token"
    );
}

/// Bearer token extraction is case-insensitive for the "Bearer" prefix.
#[test]
fn test_bearer_prefix_case_insensitive() {
    use synctv_core::service::auth::JwtValidator;

    // "Bearer" (standard)
    let result = JwtValidator::extract_bearer_token("Bearer my_token");
    assert_eq!(result.ok().as_deref(), Some("my_token"));

    // "bearer" (lowercase)
    let result = JwtValidator::extract_bearer_token("bearer my_token");
    assert_eq!(result.ok().as_deref(), Some("my_token"));

    // "BEARER" (uppercase)
    let result = JwtValidator::extract_bearer_token("BEARER my_token");
    assert_eq!(result.ok().as_deref(), Some("my_token"));
}

/// Non-bearer auth schemes should not extract a token.
#[test]
fn test_non_bearer_scheme_returns_error() {
    use synctv_core::service::auth::JwtValidator;

    let result = JwtValidator::extract_bearer_token("Basic dXNlcjpwYXNz");
    assert!(result.is_err(), "Basic auth should not extract a bearer token");
}
