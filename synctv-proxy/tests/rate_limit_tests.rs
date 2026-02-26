//! Rate limiting tests for the synctv-proxy crate.
//!
//! These tests verify that rate limiting based on source IP works correctly.

use std::time::Duration;

// The deprecated function is intentionally tested for backward compatibility
#[allow(deprecated)]
use synctv_proxy::{proxy_options_preflight_rate_limited, RateLimiter};

// ==================================================================
// Rate limiter unit tests
// ==================================================================

#[test]
fn test_rate_limiter_allows_requests_within_limit() {
    // A rate limiter with limit 5 should allow 5 requests per window
    let limiter = RateLimiter::new(5, Duration::from_secs(60));

    // First 5 requests from the same IP should be allowed
    for _ in 0..5 {
        assert!(
            limiter.check("192.168.1.100"),
            "Requests within limit should be allowed"
        );
    }
}

#[test]
fn test_rate_limiter_blocks_requests_over_limit() {
    // A rate limiter with limit 3 should block the 4th request
    let limiter = RateLimiter::new(3, Duration::from_secs(60));

    // First 3 requests should be allowed
    for i in 0..3 {
        assert!(
            limiter.check("10.0.0.1"),
            "Request {} should be allowed",
            i + 1
        );
    }

    // 4th request should be blocked
    assert!(
        !limiter.check("10.0.0.1"),
        "Request over limit should be blocked"
    );
}

#[test]
fn test_rate_limiter_tracks_ips_separately() {
    // Each IP should have its own rate limit counter
    let limiter = RateLimiter::new(2, Duration::from_secs(60));

    // Two requests from IP1
    assert!(limiter.check("192.168.1.1"));
    assert!(limiter.check("192.168.1.1"));

    // Two requests from IP2 (should still be allowed - different counter)
    assert!(limiter.check("192.168.1.2"));
    assert!(limiter.check("192.168.1.2"));

    // Third request from IP1 should be blocked
    assert!(
        !limiter.check("192.168.1.1"),
        "IP1 should be at its limit"
    );

    // Third request from IP2 should also be blocked
    assert!(
        !limiter.check("192.168.1.2"),
        "IP2 should be at its limit"
    );
}

#[test]
fn test_rate_limiter_resets_after_window() {
    // Use a very short window for testing
    let limiter = RateLimiter::new(2, Duration::from_millis(50));

    // Use up the limit
    assert!(limiter.check("127.0.0.1"));
    assert!(limiter.check("127.0.0.1"));
    assert!(!limiter.check("127.0.0.1"));

    // Wait for window to reset
    std::thread::sleep(Duration::from_millis(100));

    // Should be allowed again
    assert!(
        limiter.check("127.0.0.1"),
        "Rate limit should reset after window expires"
    );
}

// ==================================================================
// Rate-limited preflight tests
// ==================================================================

#[tokio::test]
async fn test_rate_limited_preflight_returns_429_when_over_limit() {
    // Create a rate limiter with limit 1
    let limiter = std::sync::Arc::new(RateLimiter::new(1, Duration::from_secs(60)));

    // First request should succeed
    let response = proxy_options_preflight_rate_limited(
        Some("192.168.1.50"),
        limiter.clone(),
    ).await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::NO_CONTENT,
        "First request should succeed"
    );

    // Second request from same IP should return 429
    let response = proxy_options_preflight_rate_limited(
        Some("192.168.1.50"),
        limiter.clone(),
    ).await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        "Second request should be rate limited"
    );
}

#[tokio::test]
async fn test_rate_limited_preflight_allows_different_ips() {
    // Create a rate limiter with limit 1
    let limiter = std::sync::Arc::new(RateLimiter::new(1, Duration::from_secs(60)));

    // Request from IP1
    let response = proxy_options_preflight_rate_limited(
        Some("10.0.0.1"),
        limiter.clone(),
    ).await;
    assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);

    // Request from IP2 (should be allowed - different IP)
    let response = proxy_options_preflight_rate_limited(
        Some("10.0.0.2"),
        limiter.clone(),
    ).await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::NO_CONTENT,
        "Different IP should have separate rate limit"
    );
}

#[tokio::test]
async fn test_rate_limited_preflight_missing_ip_uses_unknown() {
    // When no IP is provided, it should use "unknown" as key
    let limiter = std::sync::Arc::new(RateLimiter::new(1, Duration::from_secs(60)));

    // Request without IP
    let response = proxy_options_preflight_rate_limited(
        None,
        limiter.clone(),
    ).await;
    assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);

    // Second request without IP should be rate limited
    let response = proxy_options_preflight_rate_limited(
        None,
        limiter.clone(),
    ).await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        "Unknown IP should also be rate limited"
    );
}

// ==================================================================
// Rate limiting does not affect normal requests within limits
// ==================================================================

#[tokio::test]
async fn test_rate_limit_normal_request_succeeds() {
    // This test verifies that rate limiting doesn't block legitimate traffic
    let limiter = std::sync::Arc::new(RateLimiter::new(100, Duration::from_secs(60)));

    // Multiple requests from same IP within limit should all succeed
    for _ in 0..10 {
        let response = proxy_options_preflight_rate_limited(
            Some("172.16.0.1"),
            limiter.clone(),
        ).await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::NO_CONTENT,
            "Requests within limit should succeed"
        );
    }
}
