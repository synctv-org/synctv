//! CORS (Cross-Origin Resource Sharing) tests for the synctv-proxy crate.
//!
//! These tests verify that CORS headers are properly restricted based on
//! an allowed origins list, rather than using a wildcard (*).

#![allow(clippy::unwrap_used)]
use std::sync::Arc;

use axum::http::StatusCode;

// ==================================================================
// CORS preflight with allowed origins tests
// ==================================================================

/// Test that Origin in allowed list returns correct CORS headers
#[tokio::test]
async fn test_cors_origin_in_allowed_list_returns_headers() {
    let allowed_origins = vec![
        "https://example.com".to_string(),
        "https://app.example.com".to_string(),
    ];
    let cors_config = Arc::new(synctv_proxy::CorsConfig::new(allowed_origins));

    let response = synctv_proxy::proxy_options_preflight_with_cors(
        Some("https://example.com"),
        cors_config.clone(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let headers = response.headers();
    assert_eq!(
        headers
            .get("Access-Control-Allow-Origin")
            .map(|v| v.to_str().unwrap()),
        Some("https://example.com"),
        "Origin in allowed list should be echoed back"
    );
    assert!(
        headers.get("Access-Control-Allow-Credentials").is_some(),
        "Should include Allow-Credentials when origin is allowed"
    );
    assert!(
        headers.get("Access-Control-Allow-Methods").is_some(),
        "Should include Allow-Methods"
    );
}

/// Test that Origin NOT in allowed list is rejected
#[tokio::test]
async fn test_cors_origin_not_in_allowed_list_rejected() {
    let allowed_origins = vec![
        "https://example.com".to_string(),
        "https://app.example.com".to_string(),
    ];
    let cors_config = Arc::new(synctv_proxy::CorsConfig::new(allowed_origins));

    let response = synctv_proxy::proxy_options_preflight_with_cors(
        Some("https://evil.com"),
        cors_config.clone(),
    )
    .await;

    // Should return 403 Forbidden for disallowed origins
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "Origin not in allowed list should be rejected"
    );

    // Should NOT include CORS headers that would allow the request
    let headers = response.headers();
    assert!(
        headers.get("Access-Control-Allow-Origin").is_none()
            || headers
                .get("Access-Control-Allow-Origin")
                .unwrap()
                .to_str()
                .unwrap()
                != "https://evil.com",
        "Should not return the evil origin in Allow-Origin"
    );
}

/// Test that empty allowed origins list has safe default behavior
#[tokio::test]
async fn test_cors_empty_allowed_origins_default_behavior() {
    // Empty allowed origins should reject all origins (secure by default)
    let allowed_origins: Vec<String> = vec![];
    let cors_config = Arc::new(synctv_proxy::CorsConfig::new(allowed_origins));

    let response = synctv_proxy::proxy_options_preflight_with_cors(
        Some("https://any-site.com"),
        cors_config.clone(),
    )
    .await;

    // Should reject when no origins are allowed
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "Empty allowed origins should reject all requests"
    );
}

/// Test missing Origin header behavior
#[tokio::test]
async fn test_cors_missing_origin_header() {
    let allowed_origins = vec!["https://example.com".to_string()];
    let cors_config = Arc::new(synctv_proxy::CorsConfig::new(allowed_origins));

    // When Origin is missing (non-browser request), behavior depends on policy
    // A secure default is to reject or return minimal headers
    let response = synctv_proxy::proxy_options_preflight_with_cors(None, cors_config.clone()).await;

    // Missing origin should still return a valid response
    // but without Access-Control-Allow-Origin header
    assert!(
        response.status() == StatusCode::NO_CONTENT || response.status() == StatusCode::FORBIDDEN,
        "Missing origin should return a valid status"
    );
}

/// Test wildcard (*) is not allowed when using explicit origins
#[tokio::test]
async fn test_cors_wildcard_not_echoed() {
    let allowed_origins = vec!["https://example.com".to_string()];
    let cors_config = Arc::new(synctv_proxy::CorsConfig::new(allowed_origins));

    // Request with "*" as Origin should NOT be treated specially
    let response =
        synctv_proxy::proxy_options_preflight_with_cors(Some("*"), cors_config.clone()).await;

    // "*" is not in the allowed list, so should be rejected
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "Wildcard should not bypass origin check"
    );
}

/// Test Vary header is set correctly for caching
#[tokio::test]
async fn test_cors_vary_header_set() {
    let allowed_origins = vec!["https://example.com".to_string()];
    let cors_config = Arc::new(synctv_proxy::CorsConfig::new(allowed_origins));

    let response = synctv_proxy::proxy_options_preflight_with_cors(
        Some("https://example.com"),
        cors_config.clone(),
    )
    .await;

    let headers = response.headers();

    // Vary: Origin is important for caching - prevents serving cached
    // CORS responses for different origins
    let vary = headers.get("Vary").map(|v| v.to_str().unwrap());
    assert!(
        vary.is_some() && vary.unwrap().contains("Origin"),
        "Should include Vary: Origin header for proper caching"
    );
}

/// Test multiple allowed origins
#[tokio::test]
async fn test_cors_multiple_allowed_origins() {
    let allowed_origins = vec![
        "https://example.com".to_string(),
        "https://app.example.com".to_string(),
        "https://cdn.example.com".to_string(),
    ];
    let cors_config = Arc::new(synctv_proxy::CorsConfig::new(allowed_origins));

    // Test each allowed origin
    for origin in &[
        "https://example.com",
        "https://app.example.com",
        "https://cdn.example.com",
    ] {
        let response =
            synctv_proxy::proxy_options_preflight_with_cors(Some(*origin), cors_config.clone())
                .await;

        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "Origin {origin} should be allowed"
        );

        let headers = response.headers();
        assert_eq!(
            headers
                .get("Access-Control-Allow-Origin")
                .map(|v| v.to_str().unwrap()),
            Some(*origin),
            "Origin {origin} should be echoed back"
        );
    }
}

/// Test CORS config with wildcard enabled (special mode)
#[tokio::test]
async fn test_cors_wildcard_mode_allows_all() {
    // When configured with wildcard mode, all origins are allowed
    let cors_config = Arc::new(synctv_proxy::CorsConfig::new_wildcard());

    let response = synctv_proxy::proxy_options_preflight_with_cors(
        Some("https://any-random-site.com"),
        cors_config.clone(),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "Wildcard mode should allow all origins"
    );

    // In wildcard mode, we return * but this is explicitly configured,
    // not the default behavior
    let headers = response.headers();
    assert!(
        headers.get("Access-Control-Allow-Origin").is_some(),
        "Wildcard mode should return Allow-Origin header"
    );
}

// ==================================================================
// Security: Wildcard mode with credentials is forbidden
// ==================================================================

/// Test that wildcard mode does NOT include credentials header
/// Per CORS spec, Access-Control-Allow-Credentials cannot be used with wildcard origin
#[tokio::test]
async fn test_wildcard_mode_no_credentials() {
    let cors_config = Arc::new(synctv_proxy::CorsConfig::new_wildcard());

    let response = synctv_proxy::proxy_options_preflight_with_cors(
        Some("https://any-site.com"),
        cors_config.clone(),
    )
    .await;

    let headers = response.headers();

    // Wildcard mode must NOT include Allow-Credentials
    assert!(
        headers.get("Access-Control-Allow-Credentials").is_none(),
        "Wildcard mode must NOT include Access-Control-Allow-Credentials (per CORS spec)"
    );
}
