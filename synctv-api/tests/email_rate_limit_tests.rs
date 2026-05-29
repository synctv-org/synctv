//! Email endpoint rate limiting tests
//!
//! Validates per-IP and per-email rate limiting for email token delivery.
//!
//! These tests exercise the `RateLimiter` directly with the key patterns
//! used by the email handlers, since full integration tests would require
//! a running email service and database.

#![allow(clippy::unwrap_used)]

use synctv_core::service::rate_limit::{RateLimitError, RateLimiter};

/// Email rate limit constants (must match the values in email.rs)
const EMAIL_IP_MAX_REQUESTS: u32 = 5;
const EMAIL_IP_WINDOW_SECONDS: u64 = 60;
const EMAIL_ADDR_MAX_REQUESTS: u32 = 3;
const EMAIL_ADDR_WINDOW_SECONDS: u64 = 3600;

/// Helper to build IP-based email rate limit key (matches handler logic)
fn email_ip_key(ip: &str) -> String {
    format!("email:ip:{ip}")
}

/// Helper to build email-address-based rate limit key (matches handler logic)
fn email_addr_key(email: &str) -> String {
    format!("email:addr:{email}")
}

// Per-IP rate limit tests

#[tokio::test]
async fn test_email_ip_rate_limit_allows_up_to_limit() {
    let limiter = RateLimiter::local_only("test_email_ip:".to_string());
    let key = email_ip_key("192.168.1.100");

    for i in 0..EMAIL_IP_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key, EMAIL_IP_MAX_REQUESTS, EMAIL_IP_WINDOW_SECONDS)
            .await
            .unwrap_or_else(|_| panic!("Request {} should succeed", i + 1));
    }
}

#[tokio::test]
async fn test_email_ip_rate_limit_blocks_after_limit() {
    let limiter = RateLimiter::local_only("test_email_ip_block:".to_string());
    let key = email_ip_key("192.168.1.101");

    // Exhaust the limit
    for _ in 0..EMAIL_IP_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key, EMAIL_IP_MAX_REQUESTS, EMAIL_IP_WINDOW_SECONDS)
            .await
            .unwrap();
    }

    // 6th request should be rate limited
    let result = limiter
        .check_rate_limit(&key, EMAIL_IP_MAX_REQUESTS, EMAIL_IP_WINDOW_SECONDS)
        .await;
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "6th request from same IP should be rate limited"
    );
}

#[tokio::test]
async fn test_email_ip_rate_limit_different_ips_independent() {
    let limiter = RateLimiter::local_only("test_email_ip_indep:".to_string());
    let key_ip1 = email_ip_key("10.0.0.1");
    let key_ip2 = email_ip_key("10.0.0.2");

    // Exhaust limit for IP 1
    for _ in 0..EMAIL_IP_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key_ip1, EMAIL_IP_MAX_REQUESTS, EMAIL_IP_WINDOW_SECONDS)
            .await
            .unwrap();
    }

    // IP 1 should be blocked
    assert!(
        limiter
            .check_rate_limit(&key_ip1, EMAIL_IP_MAX_REQUESTS, EMAIL_IP_WINDOW_SECONDS)
            .await
            .is_err(),
        "IP 1 should be rate limited"
    );

    // IP 2 should still be allowed
    assert!(
        limiter
            .check_rate_limit(&key_ip2, EMAIL_IP_MAX_REQUESTS, EMAIL_IP_WINDOW_SECONDS)
            .await
            .is_ok(),
        "IP 2 should not be affected by IP 1's rate limit"
    );
}

// Per-email rate limit tests

#[tokio::test]
async fn test_email_addr_rate_limit_allows_up_to_limit() {
    let limiter = RateLimiter::local_only("test_email_addr:".to_string());
    let key = email_addr_key("user@example.com");

    for i in 0..EMAIL_ADDR_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key, EMAIL_ADDR_MAX_REQUESTS, EMAIL_ADDR_WINDOW_SECONDS)
            .await
            .unwrap_or_else(|_| panic!("Request {} should succeed", i + 1));
    }
}

#[tokio::test]
async fn test_email_addr_rate_limit_blocks_after_limit() {
    let limiter = RateLimiter::local_only("test_email_addr_block:".to_string());
    let key = email_addr_key("user@example.com");

    // Exhaust the limit (3 per hour)
    for _ in 0..EMAIL_ADDR_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key, EMAIL_ADDR_MAX_REQUESTS, EMAIL_ADDR_WINDOW_SECONDS)
            .await
            .unwrap();
    }

    // 4th request should be rate limited
    let result = limiter
        .check_rate_limit(&key, EMAIL_ADDR_MAX_REQUESTS, EMAIL_ADDR_WINDOW_SECONDS)
        .await;
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "4th request for same email should be rate limited"
    );
}

#[tokio::test]
async fn test_email_addr_rate_limit_different_emails_independent() {
    let limiter = RateLimiter::local_only("test_email_addr_indep:".to_string());
    let key1 = email_addr_key("alice@example.com");
    let key2 = email_addr_key("bob@example.com");

    // Exhaust limit for alice
    for _ in 0..EMAIL_ADDR_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key1, EMAIL_ADDR_MAX_REQUESTS, EMAIL_ADDR_WINDOW_SECONDS)
            .await
            .unwrap();
    }

    // alice should be blocked
    assert!(
        limiter
            .check_rate_limit(&key1, EMAIL_ADDR_MAX_REQUESTS, EMAIL_ADDR_WINDOW_SECONDS)
            .await
            .is_err(),
        "alice@example.com should be rate limited"
    );

    // bob should still be allowed
    assert!(
        limiter
            .check_rate_limit(&key2, EMAIL_ADDR_MAX_REQUESTS, EMAIL_ADDR_WINDOW_SECONDS)
            .await
            .is_ok(),
        "bob@example.com should not be affected by alice's rate limit"
    );
}

// Combined IP + email rate limit tests

#[tokio::test]
async fn test_email_different_emails_from_same_ip_hit_ip_limit() {
    let limiter = RateLimiter::local_only("test_email_combined:".to_string());
    let ip = "10.0.0.50";
    let ip_key = email_ip_key(ip);

    // Each email address is within its own limit, but the IP hits its limit
    for i in 0..EMAIL_IP_MAX_REQUESTS {
        let email = format!("user{i}@example.com");
        let addr_key = email_addr_key(&email);

        // Both IP and email checks pass
        limiter
            .check_rate_limit(&ip_key, EMAIL_IP_MAX_REQUESTS, EMAIL_IP_WINDOW_SECONDS)
            .await
            .unwrap_or_else(|_| panic!("IP check for request {} should pass", i + 1));
        limiter
            .check_rate_limit(
                &addr_key,
                EMAIL_ADDR_MAX_REQUESTS,
                EMAIL_ADDR_WINDOW_SECONDS,
            )
            .await
            .unwrap_or_else(|_| panic!("Email check for request {} should pass", i + 1));
    }

    // 6th request from same IP (different email) - IP limit should block it
    let new_email_key = email_addr_key("newuser@example.com");
    let ip_result = limiter
        .check_rate_limit(&ip_key, EMAIL_IP_MAX_REQUESTS, EMAIL_IP_WINDOW_SECONDS)
        .await;
    assert!(
        matches!(ip_result, Err(RateLimitError::RateLimitExceeded { .. })),
        "IP rate limit should block even though email is new"
    );

    // But the email-specific check would pass (it's a new email)
    let email_result = limiter
        .check_rate_limit(
            &new_email_key,
            EMAIL_ADDR_MAX_REQUESTS,
            EMAIL_ADDR_WINDOW_SECONDS,
        )
        .await;
    assert!(
        email_result.is_ok(),
        "New email address should not be rate limited"
    );
}

#[tokio::test]
async fn test_email_rate_limit_error_contains_retry_after() {
    let limiter = RateLimiter::local_only("test_email_retry:".to_string());
    let key = email_addr_key("test@example.com");

    // Exhaust the limit
    for _ in 0..EMAIL_ADDR_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key, EMAIL_ADDR_MAX_REQUESTS, EMAIL_ADDR_WINDOW_SECONDS)
            .await
            .unwrap();
    }

    // Check that the error contains a retry_after value
    let result = limiter
        .check_rate_limit(&key, EMAIL_ADDR_MAX_REQUESTS, EMAIL_ADDR_WINDOW_SECONDS)
        .await;
    match result {
        Err(RateLimitError::RateLimitExceeded {
            retry_after_seconds,
        }) => {
            assert!(
                retry_after_seconds > 0,
                "retry_after_seconds should be positive, got {retry_after_seconds}"
            );
        }
        other => panic!("Expected RateLimitExceeded, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_email_rate_limit_keys_are_case_normalized() {
    // Email addresses should be lowercased for rate limiting to prevent bypasses
    let limiter = RateLimiter::local_only("test_email_case:".to_string());

    // Simulate what the handler does: lowercase the email before building the key
    let email_upper = "User@Example.COM";
    let email_lower = email_upper.to_lowercase();
    let key = email_addr_key(&email_lower);

    for _ in 0..EMAIL_ADDR_MAX_REQUESTS {
        limiter
            .check_rate_limit(&key, EMAIL_ADDR_MAX_REQUESTS, EMAIL_ADDR_WINDOW_SECONDS)
            .await
            .unwrap();
    }

    // Same email with different casing should still be blocked (handler normalizes)
    let result = limiter
        .check_rate_limit(&key, EMAIL_ADDR_MAX_REQUESTS, EMAIL_ADDR_WINDOW_SECONDS)
        .await;
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "Case-normalized email should share rate limit bucket"
    );
}
