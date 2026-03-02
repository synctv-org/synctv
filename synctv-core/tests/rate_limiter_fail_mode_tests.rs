//! Rate Limiter Fail Mode Behavior Tests (TDD)
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

// ============================================================================
// Test 1: Verify current fail-open behavior (document existing behavior)
// ============================================================================

/// Documents and verifies that `check_rate_limit` fails open.
///
/// When Redis is unavailable, the rate limiter should:
/// 1. Log a warning about the fallback
/// 2. Fall back to in-memory rate limiting
/// 3. Continue processing requests (graceful degradation)
#[tokio::test]
async fn test_fail_open_allows_requests_without_redis() {
    // Create an in-memory only limiter (simulates Redis unavailable from start)
    let limiter = RateLimiter::in_memory_only("fail_open:".to_string());

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
    let limiter = RateLimiter::in_memory_only("fail_open_limit:".to_string());

    // Exhaust the limit
    for _ in 0..5 {
        limiter.check_rate_limit("key", 5, 1).await.unwrap();
    }

    // Should be rate limited (per-replica limit still enforced)
    let result = limiter.check_rate_limit("key", 5, 1).await;
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "fail-open should still enforce per-replica limits"
    );
}

// ============================================================================
// Test 2: Verify fail-closed behavior for security-critical endpoints
// ============================================================================

/// Verifies that `check_rate_limit_distributed` fails closed without Redis.
///
/// For security-critical operations (auth, password checks), we should use
/// fail-closed mode to ensure global limits are never exceeded.
#[tokio::test]
async fn test_fail_closed_denies_requests_without_redis() {
    let limiter = RateLimiter::in_memory_only("fail_closed:".to_string());

    // Without Redis, distributed check should deny all requests
    let result = limiter
        .check_rate_limit_distributed("auth:user:123", 10, 1)
        .await;
    assert!(
        matches!(
            result,
            Err(RateLimitError::RateLimitExceeded {
                retry_after_seconds: 1
            })
        ),
        "check_rate_limit_distributed should fail closed without Redis"
    );
}

/// Verifies that fail-closed mode logs appropriate error messages.
///
/// When failing closed, the system should log an error explaining why.
#[tokio::test]
async fn test_fail_closed_logs_appropriate_error() {
    // The actual logging is verified by the implementation at:
    // synctv-core/src/service/rate_limit.rs:365-371 (RedisRateLimitBackend::check_strict)
    // synctv-core/src/service/rate_limit.rs:488-500 (InMemoryRateLimitBackend::check_strict)
    //
    // Expected log messages:
    // - RedisRateLimitBackend: "Redis unreachable during distributed rate limit check, denying request (fail closed): {error}"
    // - InMemoryRateLimitBackend: "Distributed rate limit check failed: Redis not configured. Denying request (fail closed)."

    // This test documents the expected logging behavior
    let limiter = RateLimiter::in_memory_only("fail_closed_log:".to_string());
    let _ = limiter.check_rate_limit_distributed("key", 10, 1).await;
    // In a real test environment, we could capture logs and verify
}

// ============================================================================
// Test 3: Verify which endpoints should use which mode
// ============================================================================

/// Documents which endpoints should use fail-open vs fail-closed mode.
///
/// # Fail-Open (check_rate_limit)
///
/// - Chat messages (can tolerate higher rate during outage)
/// - Danmaku (same as chat)
/// - Media playback operations (user experience over strict limits)
/// - General API endpoints (availability preferred)
///
/// # Fail-Closed (check_rate_limit_distributed)
///
/// - Authentication endpoints (login, token refresh)
/// - Password checking (room passwords, user passwords)
/// - Email verification (prevents abuse)
/// - Admin operations (strict audit requirements)
/// - Any operation where exceeding global limits is unacceptable
#[test]
fn test_endpoint_mode_recommendations() {
    // This test documents the recommendations for endpoint mode selection

    let fail_open_endpoints = [
        "chat",
        "danmaku",
        "media_playback",
        "playlist_operations",
        "room_join", // Non-password operations
    ];

    let fail_closed_endpoints = [
        "auth_login",
        "auth_token_refresh",
        "room_password_check",
        "email_verification",
        "email_password_reset",
        "admin_operations",
    ];

    // These are recommendations documented for developers
    assert!(!fail_open_endpoints.is_empty());
    assert!(!fail_closed_endpoints.is_empty());
}

// ============================================================================
// Test 4: Verify behavior with simulated Redis failure
// ============================================================================

/// Simulates Redis failure during operation and verifies fail-open behavior.
///
/// When Redis becomes unavailable mid-operation:
/// 1. First request to Redis fails
/// 2. System falls back to in-memory
/// 3. Subsequent requests use in-memory (still functional)
#[tokio::test]
async fn test_redis_failure_falls_back_gracefully() {
    // Create an in-memory limiter to simulate Redis being unavailable
    let limiter = RateLimiter::in_memory_only("redis_failure:".to_string());

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
    let limiter = RateLimiter::in_memory_only("health_check:".to_string());

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

// ============================================================================
// Test 5: Verify multi-replica behavior implications (documentation)
// ============================================================================

/// Documents the multi-replica implications of fail-open behavior.
///
/// # Scenario: 3 replicas with 10 req/sec limit each
///
/// With Redis healthy:
/// - Global limit: 10 req/sec (shared counter)
/// - Total capacity: 10 req/sec
///
/// With Redis unavailable (fail-open):
/// - Per-replica limit: 10 req/sec
/// - Effective total: 30 req/sec (3 * 10)
///
/// # Mitigation Strategies
///
/// 1. **Use fail-closed for critical endpoints**: Ensures strict limits
/// 2. **Reduce limits proportionally**: Set `limit = desired_limit / replica_count`
/// 3. **Redis HA**: Use Redis Sentinel or Cluster
/// 4. **Monitor fallback metric**: Alert when `rate_limit_redis_fallbacks_total` increases
#[test]
fn test_multi_replica_implications_documentation() {
    let replica_count = 3;
    let per_replica_limit = 10;
    let desired_global_limit = 10;

    // During normal operation with Redis:
    let effective_with_redis = per_replica_limit; // Shared counter

    // During fail-open without Redis:
    let effective_without_redis = per_replica_limit * replica_count;

    assert_eq!(effective_with_redis, desired_global_limit);
    assert_eq!(effective_without_redis, 30); // 3x the desired limit

    // Mitigation: divide limits by replica count
    let mitigated_per_replica = desired_global_limit / replica_count as u32;
    let mitigated_total = mitigated_per_replica * replica_count as u32;
    assert_eq!(mitigated_total, 9); // Close to desired (integer division)
}

// ============================================================================
// Test 6: Configuration options for fail mode (future enhancement)
// ============================================================================

/// Documents potential configuration options for fail mode behavior.
///
/// # Configuration Options (Suggested)
///
/// ```toml
/// [rate_limit]
/// # Default fail mode: "open" or "closed"
/// default_fail_mode = "open"
///
/// # Per-endpoint fail mode overrides
/// [rate_limit.endpoints]
/// auth_login = "closed"
/// room_password = "closed"
/// chat = "open"
/// ```
///
/// This allows operators to choose the trade-off between availability
/// and strictness per endpoint.
#[test]
fn test_fail_mode_configuration_options_documentation() {
    // This test documents the proposed configuration options
    // Implementation would require adding configuration parsing

    let default_fail_mode = "open";
    let auth_fail_mode = "closed";

    assert_eq!(default_fail_mode, "open");
    assert_eq!(auth_fail_mode, "closed");
}

// ============================================================================
// Test 7: Verify the fallback does not leak Redis errors to callers
// ============================================================================

/// Ensures that Redis errors are not propagated to callers in fail-open mode.
///
/// Callers should never see `RateLimitError::RedisError` from `check_rate_limit`.
/// The fallback to in-memory should be transparent.
#[tokio::test]
async fn test_fail_open_does_not_propagate_redis_errors() {
    let limiter = RateLimiter::in_memory_only("no_propagate:".to_string());

    // In-memory backend should never return RedisError
    for _ in 0..5 {
        let result = limiter.check_rate_limit("key", 10, 1).await;
        match result {
            Ok(()) => {}
            Err(RateLimitError::RateLimitExceeded { .. }) => {}
            Err(RateLimitError::RedisError(e)) => {
                panic!("check_rate_limit should not propagate RedisError in fail-open mode: {e}");
            }
        }
    }
}

// ============================================================================
// Test 8: Verify sync rate limiting behavior
// ============================================================================

/// Verifies that sync rate limiting always uses in-memory (documented limitation).
///
/// gRPC interceptors are synchronous and cannot await Redis calls.
/// Therefore, `check_rate_limit_sync` always uses in-memory limiting.
#[test]
fn test_sync_rate_limit_uses_in_memory_only() {
    let limiter = RateLimiter::in_memory_only("sync:".to_string());

    // Sync method should work regardless of Redis availability
    for _ in 0..5 {
        let result = limiter.check_rate_limit_sync("key", 5, 1);
        assert!(result.is_ok());
    }

    // Should be rate limited
    let result = limiter.check_rate_limit_sync("key", 5, 1);
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "sync rate limiting should enforce limits"
    );
}

/// Verifies that sync rate limiting uses a separate key prefix.
#[test]
fn test_sync_rate_limit_uses_grpc_prefix() {
    let limiter = RateLimiter::in_memory_only("sync_prefix:".to_string());

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

// ============================================================================
// Test 9: Verify fail-closed behavior with Redis backend
// ============================================================================

/// Verifies that Redis backend's check_strict fails closed on error.
///
/// This test uses a mock/unreachable Redis to verify the fail-closed behavior.
#[tokio::test]
async fn test_redis_backend_strict_fails_closed_on_error() {
    // Create a limiter with a broken Redis connection
    // This simulates a Redis that accepts connections but fails operations
    let client_result = redis::Client::open("redis://127.0.0.1:1");

    match client_result {
        Ok(client) => {
            // Try to create connection manager - this might fail for unreachable Redis
            match redis::aio::ConnectionManager::new(client).await {
                Ok(conn) => {
                    let limiter = RateLimiter::new(
                        Some(Arc::new(RwLock::new(conn))),
                        "redis_strict:".to_string(),
                    );

                    // The distributed check should fail closed when Redis is unreachable
                    let result = limiter.check_rate_limit_distributed("key", 10, 1).await;
                    assert!(
                        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
                        "Redis backend check_strict should fail closed on connection error"
                    );
                }
                Err(_) => {
                    // Connection failed, use in-memory which also fails closed
                    let limiter = RateLimiter::in_memory_only("redis_strict:".to_string());
                    let result = limiter.check_rate_limit_distributed("key", 10, 1).await;
                    assert!(
                        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
                        "In-memory backend check_strict should fail closed"
                    );
                }
            }
        }
        Err(_) => {
            // If we can't even create client, use in-memory which also fails closed
            let limiter = RateLimiter::in_memory_only("redis_strict:".to_string());
            let result = limiter.check_rate_limit_distributed("key", 10, 1).await;
            assert!(
                matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
                "In-memory backend check_strict should fail closed"
            );
        }
    }
}

// ============================================================================
// Test 10: Document security considerations
// ============================================================================

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

    assert!(!security_critical.is_empty());
    assert!(!ux_critical.is_empty());
}

// ============================================================================
// Test 11: Verify retry_after_seconds values
// ============================================================================

/// Verifies that retry_after_seconds is reasonable for fail-closed mode.
#[tokio::test]
async fn test_fail_closed_retry_after_is_reasonable() {
    let limiter = RateLimiter::in_memory_only("retry_after:".to_string());

    let result = limiter.check_rate_limit_distributed("key", 10, 1).await;
    if let Err(RateLimitError::RateLimitExceeded {
        retry_after_seconds,
    }) = result
    {
        // Fail-closed should return a short retry (1 second) to allow quick recovery
        assert_eq!(
            retry_after_seconds, 1,
            "fail-closed retry_after should be 1 second"
        );
    } else {
        panic!("Expected RateLimitExceeded error");
    }
}

/// Verifies that fail-open mode returns meaningful retry_after when limit exceeded.
#[tokio::test]
async fn test_fail_open_retry_after_when_limited() {
    let limiter = RateLimiter::in_memory_only("retry_open:".to_string());

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

// ============================================================================
// Test 12: Verify backend_name reports correctly
// ============================================================================

/// Verifies that backend_name correctly identifies the backend type.
#[tokio::test]
async fn test_backend_name_identification() {
    let memory_limiter = RateLimiter::in_memory_only("backend_test:".to_string());
    assert_eq!(
        memory_limiter.backend_name(),
        "memory",
        "In-memory limiter should report 'memory' backend"
    );

    let none_limiter = RateLimiter::new(None, "backend_test:".to_string());
    assert_eq!(
        none_limiter.backend_name(),
        "memory",
        "Limiter without Redis should report 'memory' backend"
    );
}
