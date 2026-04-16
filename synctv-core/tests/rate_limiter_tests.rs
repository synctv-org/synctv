//! Rate limiter tests
//!
//! Tests in-memory rate limiter behavior and Redis rate limiter with testcontainers.
//!
//! Run with: cargo test --test `rate_limiter_tests`
//! Run Docker tests: cargo test --test `rate_limiter_tests` -- --ignored
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use synctv_core::service::{RateLimitError, RateLimiter};
use synctv_core_testing::{start_redis_with_client, RedisContainer};
use tokio::sync::RwLock;

// In-memory rate limiter tests

#[tokio::test]
async fn test_in_memory_rate_limiter_allows_under_limit() {
    let limiter = RateLimiter::local_only("test_allow:".to_string());

    for i in 0..5 {
        limiter
            .check_rate_limit("user:1:chat", 10, 1)
            .await
            .unwrap_or_else(|_| panic!("Request {i} should succeed under limit"));
    }
}

#[tokio::test]
async fn test_in_memory_rate_limiter_blocks_over_limit() {
    let limiter = RateLimiter::local_only("test_block:".to_string());
    let key = "user:block:chat";

    // Exhaust the limit
    for i in 0..5 {
        limiter
            .check_rate_limit(key, 5, 1)
            .await
            .unwrap_or_else(|_| panic!("Request {i} should succeed within limit"));
    }

    // Next request should be blocked
    let result = limiter.check_rate_limit(key, 5, 1).await;
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "6th request should be rate limited"
    );
}

#[tokio::test]
async fn test_in_memory_rate_limiter_window_expiry() {
    let limiter = RateLimiter::local_only("test_expiry:".to_string());
    let key = "user:expiry:chat";

    // Exhaust the limit
    for _ in 0..3 {
        limiter.check_rate_limit(key, 3, 1).await.unwrap();
    }
    assert!(limiter.check_rate_limit(key, 3, 1).await.is_err());

    tokio::time::sleep(tokio::time::Duration::from_millis(1200)).await;

    let result = limiter.check_rate_limit(key, 3, 1).await;
    assert!(
        result.is_ok(),
        "Requests should succeed after window expiry"
    );
}

#[tokio::test]
async fn test_in_memory_independent_keys() {
    let limiter = RateLimiter::local_only("test_indep:".to_string());

    // Exhaust key1
    for _ in 0..5 {
        limiter.check_rate_limit("key1", 5, 1).await.unwrap();
    }
    assert!(limiter.check_rate_limit("key1", 5, 1).await.is_err());

    // key2 should still work
    assert!(limiter.check_rate_limit("key2", 5, 1).await.is_ok());
}

#[test]
fn test_in_memory_sync_check() {
    let limiter = RateLimiter::local_only("sync_test:".to_string());

    for _ in 0..5 {
        limiter.check_rate_limit_sync("key", 5, 1).unwrap();
    }
    assert!(matches!(
        limiter.check_rate_limit_sync("key", 5, 1),
        Err(RateLimitError::RateLimitExceeded { .. })
    ));
}

#[tokio::test]
async fn test_in_memory_distributed_uses_governor() {
    let limiter = RateLimiter::local_only("dist:".to_string());

    // Without Redis, the async check should work using in-memory governor.
    // Note: check_rate_limit_distributed (check_strict) intentionally fails
    // closed without Redis. Use check_rate_limit (check) for in-memory fallback.
    for i in 0..5 {
        limiter
            .check_rate_limit("key", 5, 1)
            .await
            .unwrap_or_else(|_| panic!("Request {i} should succeed via in-memory governor"));
    }

    let result = limiter.check_rate_limit("key", 5, 1).await;
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "Request exceeding limit should be blocked via in-memory governor"
    );
}

// Redis rate limiter tests (require Docker)

async fn create_redis_connection_manager() -> (redis::aio::ConnectionManager, RedisContainer) {
    let (container, client) = start_redis_with_client().await;
    let conn = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to create Redis ConnectionManager");

    (conn, container)
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_rate_limiter_allows_under_limit() {
    let (conn, _container) = create_redis_connection_manager().await;
    let limiter = RateLimiter::from_redis_runtime(
        synctv_core::shared_runtime_from_conn(Some(Arc::new(RwLock::new(conn)))),
        "redis_allow:".to_string(),
    );

    let key = "user:redis_allow:chat";
    limiter.reset(key).await.unwrap();

    for i in 0..10 {
        limiter
            .check_rate_limit(key, 10, 1)
            .await
            .unwrap_or_else(|_| panic!("Redis request {i} should succeed under limit"));
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_rate_limiter_blocks_over_limit() {
    let (conn, _container) = create_redis_connection_manager().await;
    let limiter = RateLimiter::from_redis_runtime(
        synctv_core::shared_runtime_from_conn(Some(Arc::new(RwLock::new(conn)))),
        "redis_block:".to_string(),
    );

    let key = "user:redis_block:chat";
    limiter.reset(key).await.unwrap();

    for i in 0..5 {
        limiter
            .check_rate_limit(key, 5, 1)
            .await
            .unwrap_or_else(|_| panic!("Redis request {i} should succeed within limit"));
    }

    let result = limiter.check_rate_limit(key, 5, 1).await;
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "6th request should be rate limited via Redis"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_rate_limiter_concurrent_requests() {
    let (conn, _container) = create_redis_connection_manager().await;
    let limiter = RateLimiter::from_redis_runtime(
        synctv_core::shared_runtime_from_conn(Some(Arc::new(RwLock::new(conn)))),
        "redis_conc:".to_string(),
    );

    let key = "user:redis_conc:chat";
    limiter.reset(key).await.unwrap();

    // Launch 20 concurrent requests with a limit of 10
    let mut handles = Vec::new();
    for _ in 0..20 {
        let l = limiter.clone();
        handles.push(tokio::spawn(
            async move { l.check_rate_limit(key, 10, 1).await },
        ));
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    let successes = results.iter().filter(|r| r.is_ok()).count();
    let failures = results.iter().filter(|r| r.is_err()).count();

    assert_eq!(
        successes, 10,
        "Only 10 of 20 concurrent requests should succeed"
    );
    assert_eq!(failures, 10, "10 requests should be rate limited");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_rate_limiter_strict_enforcement() {
    let (conn, _container) = create_redis_connection_manager().await;
    let limiter = RateLimiter::from_redis_runtime(
        synctv_core::shared_runtime_from_conn(Some(Arc::new(RwLock::new(conn)))),
        "redis_strict:".to_string(),
    );

    let key = "user:redis_strict:auth";
    limiter.reset(key).await.unwrap();

    // Strict check should work within limits
    for i in 0..5 {
        limiter
            .check_rate_limit_distributed(key, 5, 1)
            .await
            .unwrap_or_else(|_| panic!("Strict request {i} should succeed within limit"));
    }

    let result = limiter.check_rate_limit_distributed(key, 5, 1).await;
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "6th strict request should be rate limited via Redis"
    );
}

#[tokio::test]
async fn test_redis_rate_limiter_distributed_fails_closed_without_redis() {
    // In-memory-only limiter should fail closed for distributed checks
    // when Redis is not configured (security-critical operations)
    let limiter = RateLimiter::local_only("redis_fc:".to_string());

    // Should deny ALL requests when Redis is not configured (fail-closed)
    let result = limiter.check_rate_limit_distributed("key", 10, 1).await;
    assert!(
        matches!(result, Err(RateLimitError::BackendUnavailable(_))),
        "check_rate_limit_distributed should fail closed when Redis is not configured"
    );
}
