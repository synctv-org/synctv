//! Admin endpoint rate limiting tests
//!
//! Validates rate limiting for HTTP admin endpoints:
//! - DELETE /api/admin/rooms/{id}
//! - POST /api/admin/users/{id}/ban
//! - PUT /api/admin/settings
//!
//! These tests exercise the `RateLimiter` directly with the key patterns used
//! by the shared admin request execution path.

#![allow(clippy::unwrap_used)]

use synctv_core::service::rate_limit::{RateLimitError, RateLimiter};

/// Admin rate limit constants (must match the values in RequestRateLimitConfig::default())
const ADMIN_MAX_REQUESTS: u32 = 30;
const ADMIN_WINDOW_SECONDS: u64 = 60;

/// Helper to build the admin rate limit key used by request execution.
/// The shared request path uses: `format!("ratelimit:admin:user:{user_id}")`
/// or `format!("ratelimit:admin:anon:{ip}")` for unauthenticated requests.
fn admin_user_key(user_id: &str) -> String {
    format!("ratelimit:admin:user:{user_id}")
}

fn admin_anon_key(ip: &str) -> String {
    format!("ratelimit:admin:anon:{ip}")
}

// Basic admin rate limit tests

#[tokio::test]
async fn test_admin_rate_limit_allows_up_to_limit() {
    let limiter = RateLimiter::local_only("test_admin_basic:".to_string());
    let key = admin_user_key("admin_user_001");

    for i in 0..ADMIN_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .unwrap_or_else(|_| panic!("Admin request {} should succeed", i + 1));
    }
}

#[tokio::test]
async fn test_admin_rate_limit_blocks_after_limit() {
    let limiter = RateLimiter::local_only("test_admin_block:".to_string());
    let key = admin_user_key("admin_user_002");

    // Exhaust the limit
    for _ in 0..ADMIN_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .unwrap();
    }

    // 31st request should be rate limited
    let result = limiter
        .check_rate_limit(&key, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
        .await;
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "Request beyond admin limit should be rate limited"
    );
}

#[tokio::test]
async fn test_admin_rate_limit_different_users_independent() {
    let limiter = RateLimiter::local_only("test_admin_indep:".to_string());
    let key_admin1 = admin_user_key("admin_alice");
    let key_admin2 = admin_user_key("admin_bob");

    // Exhaust limit for admin_alice
    for _ in 0..ADMIN_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key_admin1, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .unwrap();
    }

    // admin_alice should be blocked
    assert!(
        limiter
            .check_rate_limit(&key_admin1, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .is_err(),
        "admin_alice should be rate limited"
    );

    // admin_bob should still be allowed
    assert!(
        limiter
            .check_rate_limit(&key_admin2, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .is_ok(),
        "admin_bob should not be affected by admin_alice's rate limit"
    );
}

// Per-IP anonymous admin rate limit tests

#[tokio::test]
async fn test_admin_anonymous_rate_limit_allows_up_to_limit() {
    let limiter = RateLimiter::local_only("test_admin_anon:".to_string());
    let key = admin_anon_key("192.168.1.100");

    for i in 0..ADMIN_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .unwrap_or_else(|_| panic!("Anonymous admin request {} should succeed", i + 1));
    }
}

#[tokio::test]
async fn test_admin_anonymous_rate_limit_blocks_after_limit() {
    let limiter = RateLimiter::local_only("test_admin_anon_block:".to_string());
    let key = admin_anon_key("192.168.1.101");

    // Exhaust the limit
    for _ in 0..ADMIN_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .unwrap();
    }

    // Next request should be rate limited
    let result = limiter
        .check_rate_limit(&key, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
        .await;
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "Anonymous request beyond admin limit should be rate limited"
    );
}

#[tokio::test]
async fn test_admin_anonymous_different_ips_independent() {
    let limiter = RateLimiter::local_only("test_admin_anon_indep:".to_string());
    let key_ip1 = admin_anon_key("10.0.0.1");
    let key_ip2 = admin_anon_key("10.0.0.2");

    // Exhaust limit for IP 1
    for _ in 0..ADMIN_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key_ip1, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .unwrap();
    }

    // IP 1 should be blocked
    assert!(
        limiter
            .check_rate_limit(&key_ip1, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .is_err(),
        "IP 1 should be rate limited for admin endpoints"
    );

    // IP 2 should still be allowed
    assert!(
        limiter
            .check_rate_limit(&key_ip2, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .is_ok(),
        "IP 2 should not be affected by IP 1's rate limit"
    );
}

// Admin rate limit error details tests

#[tokio::test]
async fn test_admin_rate_limit_error_contains_retry_after() {
    let limiter = RateLimiter::local_only("test_admin_retry:".to_string());
    let key = admin_user_key("admin_test_001");

    // Exhaust the limit
    for _ in 0..ADMIN_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .unwrap();
    }

    // Check that the error contains a retry_after value
    let result = limiter
        .check_rate_limit(&key, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
        .await;
    match result {
        Err(RateLimitError::RateLimitExceeded {
            retry_after_seconds,
        }) => {
            assert!(
                retry_after_seconds > 0,
                "retry_after_seconds should be positive, got {retry_after_seconds}"
            );
            // retry_after should be at most the window size
            assert!(
                retry_after_seconds <= ADMIN_WINDOW_SECONDS,
                "retry_after_seconds should not exceed window size, got {retry_after_seconds}"
            );
        }
        other => panic!("Expected RateLimitExceeded, got: {other:?}"),
    }
}

// Category-wide admin rate limit tests (all admin endpoints share limit)

#[tokio::test]
async fn test_admin_rate_limit_category_wide() {
    // The shared admin request path uses `ratelimit:admin:{user/ip}` as the
    // key, which makes admin endpoints share one bucket.
    let limiter = RateLimiter::local_only("test_admin_category:".to_string());
    let key = admin_user_key("admin_shared_bucket");

    for i in 0..ADMIN_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .unwrap_or_else(|_| panic!("Admin request {} (any endpoint) should succeed", i + 1));
    }

    // The 31st request to ANY admin endpoint should be blocked
    let result = limiter
        .check_rate_limit(&key, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
        .await;
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "Request beyond admin category limit should be rate limited regardless of specific endpoint"
    );
}

// Admin vs other rate limit categories isolation tests

#[tokio::test]
async fn test_admin_rate_limit_isolated_from_auth() {
    // Admin rate limit should be independent from auth rate limit
    let limiter = RateLimiter::local_only("test_admin_vs_auth:".to_string());
    let admin_key = admin_user_key("admin_user_003");
    let auth_key = "ratelimit:auth:user:admin_user_003".to_string();

    // Exhaust admin limit
    for _ in 0..ADMIN_MAX_REQUESTS {
        limiter
            .check_rate_limit(&admin_key, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .unwrap();
    }

    // Admin should be blocked
    assert!(
        limiter
            .check_rate_limit(&admin_key, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .is_err(),
        "Admin endpoint should be rate limited"
    );

    // Auth endpoint should still work (different category)
    assert!(
        limiter.check_rate_limit(&auth_key, 5, 60).await.is_ok(),
        "Auth endpoint should not be affected by admin rate limit"
    );
}

#[tokio::test]
async fn test_admin_rate_limit_isolated_from_write() {
    // Admin rate limit should be independent from write rate limit
    let limiter = RateLimiter::local_only("test_admin_vs_write:".to_string());
    let admin_key = admin_user_key("admin_user_004");
    let write_key = "ratelimit:write:user:admin_user_004".to_string();

    // Exhaust admin limit
    for _ in 0..ADMIN_MAX_REQUESTS {
        limiter
            .check_rate_limit(&admin_key, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .unwrap();
    }

    // Admin should be blocked
    assert!(
        limiter
            .check_rate_limit(&admin_key, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
            .is_err(),
        "Admin endpoint should be rate limited"
    );

    // Write endpoint should still work (different category)
    assert!(
        limiter.check_rate_limit(&write_key, 30, 60).await.is_ok(),
        "Write endpoint should not be affected by admin rate limit"
    );
}

// Burst protection tests

#[tokio::test]
async fn test_admin_rate_limit_prevents_burst() {
    // Simulate a burst of 50 requests - only 30 should succeed
    let limiter = RateLimiter::local_only("test_admin_burst:".to_string());
    let key = admin_user_key("admin_burster");

    let mut successes = 0;
    let mut failures = 0;

    for _ in 0..50 {
        match limiter
            .check_rate_limit(&key, ADMIN_MAX_REQUESTS, ADMIN_WINDOW_SECONDS)
            .await
        {
            Ok(()) => successes += 1,
            Err(RateLimitError::RateLimitExceeded { .. }) => failures += 1,
            Err(e) => panic!("Unexpected error: {e}"),
        }
    }

    assert_eq!(
        successes, ADMIN_MAX_REQUESTS,
        "Exactly {ADMIN_MAX_REQUESTS} requests should succeed"
    );
    assert_eq!(failures, 20, "20 requests should be rate limited");
}
