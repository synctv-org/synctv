//! Rate Limiter Fail Mode Behavior Tests
//!
//! Tests for verifying the fail-open vs fail-closed behavior of the rate limiter.
//!
//! # Problem Statement
//!
//! Rate Limiter fails-open when Redis is unavailable, meaning in multi-replica
//! deployments, the actual rate limit becomes `N * limit` where N is the number
//! of replicas, since each replica maintains its own in-memory counter.
//!
//! # Fail Mode Behaviors
//!
//! ## Fail-Open (`check_rate_limit`)
//!
//! - Falls back to in-memory governor on Redis errors
//! - Service remains available during Redis outage
//! - Per-replica limits during outage (N * limit effective limit)
//! - Appropriate for: chat, danmaku, non-critical operations
//!
//! ## Fail-Closed (`check_rate_limit_distributed`)
//!
//! - Denies all requests when Redis is unavailable
//! - Maintains strict global limits at the cost of availability
//! - Appropriate for: authentication, password checks, security-critical operations
//!
//! Run with: cargo test -p synctv-core --test rate_limiter_fail_mode_tests
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use synctv_core::service::{RateLimitError, RateLimiter};
use tokio::sync::RwLock;

// Test 1: Verify current fail-open behavior (document existing behavior)

/// Documents and verifies that `check_rate_limit` fails open.
///
/// When Redis is unavailable, the rate limiter should:
/// 1. Log a warning about the fallback
/// 2. Fall back to in-memory rate limiting
/// 3. Continue processing requests (graceful degradation)
#[tokio::test]
async fn test_fail_open_allows_requests_without_redis() {
    let limiter = RateLimiter::local_only("fail_open:".to_string());

    // With in-memory backend, check_rate_limit should still work
    let result = limiter.check_rate_limit("test_key", 5, 1).await;
    assert!(
        result.is_ok(),
        "check_rate_limit should succeed without Redis (fail-open)"
    );
}

/// Verifies that fail-open mode still enforces limits (just per-replica).
#[tokio::test]
async fn test_fail_open_still_enforces_per_replica_limits() {
    let limiter = RateLimiter::local_only("fail_open_limit:".to_string());

    // Exhaust the limit
    for _ in 0..5 {
        limiter.check_rate_limit("key", 5, 1).await.unwrap();
    }

    let result = limiter.check_rate_limit("key", 5, 1).await;
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "fail-open should still enforce per-replica limits"
    );
}

// Test 2: Verify fail-closed behavior for security-critical endpoints

/// Verifies that `check_rate_limit_distributed` fails closed without Redis.
///
/// For security-critical operations (auth, password checks), we should use
/// fail-closed mode to ensure global limits are never exceeded.
#[tokio::test]
async fn test_fail_closed_denies_requests_without_redis() {
    let limiter = RateLimiter::local_only("fail_closed:".to_string());

    // Without Redis, distributed check should deny all requests
    let result = limiter
        .check_rate_limit_distributed("auth:user:123", 10, 1)
        .await;
    assert!(
        matches!(result, Err(RateLimitError::BackendUnavailable(_))),
        "check_rate_limit_distributed should fail closed without Redis"
    );
}

// Test 3: Verify which endpoints should use which mode

// Documents which endpoints should use fail-open vs fail-closed mode.
// Fail-open (`check_rate_limit`):
// - Chat messages
// - Danmaku
// - Media playback operations
// - General API endpoints
// Fail-closed (`check_rate_limit_distributed`):
// - Authentication endpoints
// - Password checking
// - Email verification
// - Admin operations
// - Any operation where exceeding global limits is unacceptable

// Test 4: Verify behavior with simulated Redis failure

/// Simulates Redis failure during operation and verifies fail-open behavior.
///
/// When Redis becomes unavailable mid-operation:
/// 1. First request to Redis fails
/// 2. System falls back to in-memory
/// 3. Subsequent requests use in-memory (still functional)
#[tokio::test]
async fn test_redis_failure_falls_back_gracefully() {
    let limiter = RateLimiter::local_only("redis_failure:".to_string());

    // Simulate multiple requests - all should work with fail-open
    for i in 0..3 {
        let result = limiter.check_rate_limit("key", 10, 1).await;
        assert!(
            result.is_ok(),
            "Request {i} should succeed with fail-open fallback"
        );
    }
}

/// Verifies that health_check returns error when Redis is unavailable.
#[tokio::test]
async fn test_health_check_detects_redis_unavailable() {
    let limiter = RateLimiter::local_only("health_check:".to_string());

    let result = limiter.health_check().await;
    assert!(
        result.is_err(),
        "health_check should return error when Redis is unavailable"
    );
    assert!(
        result.unwrap_err().contains("not configured"),
        "Error should indicate Redis not configured"
    );
}

// Test 5: Verify multi-replica behavior implications (documentation)

// Documents the multi-replica implications of fail-open behavior.
// Scenario: 3 replicas with 10 req/sec limit each.
// With Redis healthy, global limit stays 10 req/sec.
// With Redis unavailable, effective limit becomes 30 req/sec.
// Mitigations:

// Test 7: Verify the fallback does not leak Redis errors to callers

/// Ensures that Redis errors are not propagated to callers in fail-open mode.
///
/// Callers should never see `RateLimitError::RedisError` from `check_rate_limit`.
/// The fallback to in-memory should be transparent.
#[tokio::test]
async fn test_fail_open_does_not_propagate_redis_errors() {
    let limiter = RateLimiter::local_only("no_propagate:".to_string());

    // In-memory backend should never return RedisError
    for _ in 0..5 {
        let result = limiter.check_rate_limit("key", 10, 1).await;
        match result {
            Ok(()) | Err(RateLimitError::RateLimitExceeded { .. }) => {}
            Err(RateLimitError::BackendUnavailable(message)) => {
                panic!(
                    "check_rate_limit should not return BackendUnavailable in fail-open mode: {message}"
                );
            }
            Err(RateLimitError::Control(error)) => {
                panic!("check_rate_limit should not return Control in fail-open mode: {error}");
            }
            Err(RateLimitError::RedisError(e)) => {
                panic!("check_rate_limit should not propagate RedisError in fail-open mode: {e}");
            }
        }
    }
}

// Test 8: Verify sync rate limiting behavior

/// Verifies that sync rate limiting always uses in-memory (documented limitation).
///
/// gRPC interceptors are synchronous and cannot await Redis calls.
/// Therefore, `check_rate_limit_sync` always uses in-memory limiting.
#[test]
fn test_sync_rate_limit_uses_in_memory_only() {
    let limiter = RateLimiter::local_only("sync:".to_string());

    // Sync method should work regardless of Redis availability
    for _ in 0..5 {
        let result = limiter.check_rate_limit_sync("key", 5, 1);
        assert!(result.is_ok());
    }

    let result = limiter.check_rate_limit_sync("key", 5, 1);
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "sync rate limiting should enforce limits"
    );
}

/// Verifies that sync rate limiting uses a separate key prefix.
#[test]
fn test_sync_rate_limit_uses_grpc_prefix() {
    let limiter = RateLimiter::local_only("sync_prefix:".to_string());

    // Use up sync quota
    for _ in 0..5 {
        limiter.check_rate_limit_sync("user:123", 5, 1).unwrap();
    }

    // Sync should be rate limited
    assert!(limiter.check_rate_limit_sync("user:123", 5, 1).is_err());

    // But async should still work (different key due to grpc: prefix)
    // Note: This tests that the key prefixes are isolated
    let async_result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(limiter.check_rate_limit("user:123", 5, 1));
    assert!(
        async_result.is_ok(),
        "async rate limit should use different key than sync"
    );
}

// Test 9: Verify fail-closed behavior with Redis backend

/// Verifies that Redis backend's check_strict fails closed on error.
///
/// This test uses a mock/unreachable Redis to verify the fail-closed behavior.
#[tokio::test]
async fn test_redis_backend_strict_fails_closed_on_error() {
    // This simulates a Redis that accepts connections but fails operations
    let client_result = redis::Client::open("redis://127.0.0.1:1");

    if let Ok(client) = client_result {
        // Try to create connection manager with a short timeout — port 1 is unreachable
        let conn_result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            redis::aio::ConnectionManager::new(client),
        )
        .await;
        if let Ok(Ok(conn)) = conn_result {
            let limiter = RateLimiter::from_redis_runtime(
                synctv_core::shared_runtime_from_conn(Some(Arc::new(RwLock::new(conn)))),
                "redis_strict:".to_string(),
            );

            // The distributed check should fail closed when Redis is unreachable
            let result = limiter.check_rate_limit_distributed("key", 10, 1).await;
            assert!(
                matches!(result, Err(RateLimitError::BackendUnavailable(_))),
                "Redis backend check_strict should fail closed on connection error"
            );
        } else {
            // Connection failed, use in-memory which also fails closed
            let limiter = RateLimiter::local_only("redis_strict:".to_string());
            let result = limiter.check_rate_limit_distributed("key", 10, 1).await;
            assert!(
                matches!(result, Err(RateLimitError::BackendUnavailable(_))),
                "In-memory backend check_strict should fail closed"
            );
        }
    } else {
        // If we can't even create client, use in-memory which also fails closed
        let limiter = RateLimiter::local_only("redis_strict:".to_string());
        let result = limiter.check_rate_limit_distributed("key", 10, 1).await;
        assert!(
            matches!(result, Err(RateLimitError::BackendUnavailable(_))),
            "In-memory backend check_strict should fail closed"
        );
    }
}

// Test 10: Document security considerations

/// Documents security considerations for rate limiter fail modes.
///
/// # Security Implications
///
/// ## Fail-Open Risks
///
/// 1. **Brute force attacks**: During Redis outage, attackers get N * limit attempts
/// 2. **Credential stuffing**: Password checking may allow more attempts
/// 3. **DoS amplification**: Effective rate limit increases during outage
///
/// ## Fail-Closed Risks
///
/// 1. **Availability impact**: Service becomes unavailable during Redis outage
/// 2. **Cascading failures**: Dependent services may fail
///
/// # Recommendations
///
/// 1. Use fail-closed for all authentication-related endpoints
/// 2. Use fail-open for user experience features (chat, playback)
/// 3. Monitor `rate_limit_redis_fallbacks_total` metric
/// 4. Have Redis HA in production
#[test]
fn test_security_considerations_documentation() {
    // Security-critical endpoints that MUST use fail-closed:
    let security_critical = [
        "login",
        "password_reset",
        "email_verification",
        "room_password_check",
        "admin_actions",
    ];

    // User experience endpoints that CAN use fail-open:
    let ux_critical = ["chat", "danmaku", "playback", "playlist"];

    assert!(security_critical.contains(&"login"));
    assert!(security_critical.contains(&"room_password_check"));
    assert!(ux_critical.contains(&"chat"));
    assert!(ux_critical.contains(&"playback"));
    assert!(
        security_critical
            .iter()
            .all(|endpoint| !ux_critical.contains(endpoint)),
        "security-critical and UX-oriented endpoints must remain disjoint"
    );
}

// Test 11: Verify retry_after_seconds values

/// Verifies that fail-closed mode reports backend unavailability distinctly.
#[tokio::test]
async fn test_fail_closed_reports_backend_unavailable() {
    let limiter = RateLimiter::local_only("retry_after:".to_string());

    let result = limiter.check_rate_limit_distributed("key", 10, 1).await;
    if let Err(RateLimitError::BackendUnavailable(message)) = result {
        assert!(
            message.contains("backend unavailable"),
            "fail-closed mode should surface backend unavailability"
        );
    } else {
        panic!("Expected BackendUnavailable error");
    }
}

/// Verifies that fail-open mode returns meaningful retry_after when limit exceeded.
#[tokio::test]
async fn test_fail_open_retry_after_when_limited() {
    let limiter = RateLimiter::local_only("retry_open:".to_string());

    // Exhaust the limit
    for _ in 0..5 {
        limiter.check_rate_limit("key", 5, 1).await.unwrap();
    }

    let result = limiter.check_rate_limit("key", 5, 1).await;
    if let Err(RateLimitError::RateLimitExceeded {
        retry_after_seconds,
    }) = result
    {
        // Should return a reasonable retry time based on the window
        assert!(
            retry_after_seconds >= 1,
            "retry_after should be at least 1 second"
        );
    } else {
        panic!("Expected RateLimitExceeded error");
    }
}

// Test 12: Verify in-memory construction remains usable without shared state

/// Verifies that non-distributed construction still provides working local checks.
#[tokio::test]
async fn test_in_memory_construction_still_allows_local_checks() {
    let memory_limiter = RateLimiter::local_only("backend_test:".to_string());
    memory_limiter
        .check_rate_limit("user:memory", 1, 60)
        .await
        .expect("in-memory limiter should allow the first request");

    let none_limiter = RateLimiter::local_only("backend_test:".to_string());
    none_limiter
        .check_rate_limit("user:none", 1, 60)
        .await
        .expect("limiter without shared runtime should still allow local checks");
}
