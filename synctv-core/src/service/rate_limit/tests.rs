use super::*;
use crate::test_helpers::failing_redis_runtime;
use crate::RedisConnectionRuntime;
use async_trait::async_trait;
use std::time::Duration;
use synctv_core_testing::start_redis;

fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

async fn allow(limiter: &RateLimiter, key: &str, max_requests: u32, window_seconds: u64) {
    ok(
        limiter
            .check_rate_limit(key, max_requests, window_seconds)
            .await,
        "rate limit check should allow request",
    );
}

fn joined<T>(result: std::result::Result<T, tokio::task::JoinError>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

#[derive(Clone)]
struct FailingRedisRuntime;

#[async_trait]
impl RedisConnectionRuntime for FailingRedisRuntime {
    async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
        Err(redis::RedisError::from((
            redis::ErrorKind::Io,
            "test Redis runtime unavailable",
        )))
    }

    fn operation_timeout(&self) -> Duration {
        Duration::from_millis(10)
    }
}

#[test]
fn test_retry_after_uses_ceiled_remaining_window() {
    assert_eq!(retry_after_seconds_from_oldest(1_250, 1_000, 1), 1);
    assert_eq!(retry_after_seconds_from_oldest(2_001, 1_000, 2), 1);
    assert_eq!(retry_after_seconds_from_oldest(10_000, 0, 60), 1);
    assert_eq!(retry_after_seconds_from_oldest(1_000, 1_000, 60), 60);
}

#[test]
fn test_sliding_window_result_requires_count_and_oldest_score() {
    let (count, oldest) = ok(
        parse_sliding_window_result(&[3, 1_700_000]),
        "valid script result",
    );
    assert_eq!(count, 3);
    assert_eq!(oldest, 1_700_000);

    assert!(matches!(
        parse_sliding_window_result(&[3]),
        Err(RateLimitError::BackendUnavailable(_))
    ));
}

#[test]
fn test_quota_count_result_requires_count() {
    assert_eq!(ok(parse_quota_count_result(&[7]), "valid quota result"), 7);
    assert!(matches!(
        parse_quota_count_result(&[]),
        Err(RateLimitError::BackendUnavailable(_))
    ));
}

#[test]
fn test_extracts_rate_limit_tier_from_transport_scoped_keys() {
    assert_eq!(
        extract_rate_limit_tier("ratelimit:http:websocket:user:42"),
        "websocket"
    );
    assert_eq!(
        extract_rate_limit_tier("ratelimit:transport:streaming:user:42"),
        "streaming"
    );
    assert_eq!(
        extract_rate_limit_tier("ratelimit:http:read:user:42"),
        "read"
    );
}

#[test]
fn test_rate_limiter_without_redis() {
    let limiter = RateLimiter::local_only("test:".to_string());
    ok(
        limiter.check_rate_limit_sync("test-user", 5, 1),
        "limiter without shared runtime should still allow local sync checks",
    );
}

#[tokio::test]
async fn test_local_only_rate_limiter_supports_single_node_checks() {
    let limiter = RateLimiter::local_only("test-local-only:".to_string());
    ok(
        limiter.check_rate_limit("test-user", 2, 60).await,
        "local-only limiter should allow the first request",
    );
    ok(
        limiter.check_rate_limit("test-user", 2, 60).await,
        "local-only limiter should allow requests within the local quota",
    );
}

#[tokio::test]
async fn test_redis_rate_limit_backend_accepts_trait_object_runtime() {
    let runtime = failing_redis_runtime();
    let backend = RedisRateLimitBackend::from_runtime(runtime.clone(), "synctv:".to_string());

    assert!(
        Arc::ptr_eq(&backend.conn, &runtime),
        "rate-limit backend should retain the injected runtime object"
    );
}

#[tokio::test]
async fn test_redis_rate_limit_timeout_falls_back_to_in_memory() {
    #[derive(Clone)]
    struct HangingRedisRuntime;

    #[async_trait]
    impl RedisConnectionRuntime for HangingRedisRuntime {
        async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
            std::future::pending().await
        }

        fn operation_timeout(&self) -> Duration {
            Duration::from_millis(10)
        }
    }

    let backend = RedisRateLimitBackend::from_runtime(
        Arc::new(HangingRedisRuntime),
        "timeout-rate-limit:".to_string(),
    );

    let result = ok(
        tokio::time::timeout(Duration::from_millis(200), backend.check("key", 5, 60)).await,
        "Redis timeout should bound non-strict rate limiting",
    );

    assert!(
        result.is_ok(),
        "non-strict Redis timeout should fall back to in-memory"
    );
}

#[tokio::test]
async fn test_redis_rate_limit_timeout_fails_closed_in_strict_mode() {
    #[derive(Clone)]
    struct HangingRedisRuntime;

    #[async_trait]
    impl RedisConnectionRuntime for HangingRedisRuntime {
        async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
            std::future::pending().await
        }

        fn operation_timeout(&self) -> Duration {
            Duration::from_millis(10)
        }
    }

    let backend = RedisRateLimitBackend::from_runtime(
        Arc::new(HangingRedisRuntime),
        "timeout-strict-rate-limit:".to_string(),
    );

    let result = tokio::time::timeout(
        Duration::from_millis(200),
        backend.check_strict("key", 5, 60),
    )
    .await;
    let result = ok(result, "Redis timeout should bound strict rate limiting");

    assert!(
        matches!(result, Err(RateLimitError::BackendUnavailable(_))),
        "strict Redis timeout should fail closed"
    );
}

#[tokio::test]
async fn test_rate_limiter_supports_service_trait_object() {
    let limiter: Arc<dyn RequestRateLimiterService> =
        Arc::new(RateLimiter::local_only("trait-test:".to_string()));

    ok(
        limiter.check_rate_limit("user-1", 2, 60).await,
        "trait-object limiter should allow the first request",
    );
    ok(
        limiter.check_rate_limit("user-1", 2, 60).await,
        "trait-object limiter should allow requests within quota",
    );
}

#[test]
fn test_rate_limiter_from_shared_state_profile_uses_memory_without_shared_runtime() {
    let profile = SharedStateProfile::for_cluster_runtime(None, "test:", false);
    let limiter = ok(
        RateLimiter::from_shared_state_profile(&profile),
        "standalone mode should allow local rate limiting",
    );

    assert!(
        limiter.check_rate_limit_sync("test-user", 1, 60).is_ok(),
        "shared-state builder must return a live rate limiter"
    );
}

#[test]
fn test_rate_limiter_from_shared_state_profile_requires_shared_runtime_in_cluster_mode() {
    let profile = SharedStateProfile::for_cluster_runtime(None, "test:", true);
    let Err(error) = RateLimiter::from_shared_state_profile(&profile) else {
        std::panic::panic_any("cluster runtime must reject local-only rate limiting");
    };

    assert!(
        error
            .to_string()
            .contains("distributed runtime requires shared rate-limit state"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_rate_limit_basic() {
    let (_redis, conn) = start_redis().await;
    let conn = Arc::new(tokio::sync::RwLock::new(conn));
    let limiter = RateLimiter::from_redis_runtime(
        crate::shared_runtime_from_conn(Some(conn)),
        "test:".to_string(),
    );

    let key = "user:test1:chat";
    ok(limiter.reset(key).await, "rate limit key should reset");

    for i in 0..10 {
        ok(
            limiter.check_rate_limit(key, 10, 1).await,
            &format!("Request {i} should succeed"),
        );
    }

    let result = limiter.check_rate_limit(key, 10, 1).await;
    assert!(matches!(
        result,
        Err(RateLimitError::RateLimitExceeded { .. })
    ));

    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    allow(&limiter, key, 10, 1).await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_rate_limit_sliding_window() {
    let (_redis, conn) = start_redis().await;
    let conn = Arc::new(tokio::sync::RwLock::new(conn));
    let limiter = RateLimiter::from_redis_runtime(
        crate::shared_runtime_from_conn(Some(conn)),
        "test:".to_string(),
    );

    let key = "user:test2:chat";
    ok(limiter.reset(key).await, "rate limit key should reset");

    for _ in 0..5 {
        allow(&limiter, key, 5, 1).await;
    }
    assert!(limiter.check_rate_limit(key, 5, 1).await.is_err());

    tokio::time::sleep(tokio::time::Duration::from_millis(600)).await;
    assert!(limiter.check_rate_limit(key, 5, 1).await.is_err());

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    allow(&limiter, key, 5, 1).await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_rejected_redis_requests_do_not_extend_window() {
    let (_redis, conn) = start_redis().await;
    let conn = Arc::new(tokio::sync::RwLock::new(conn));
    let limiter = RateLimiter::from_redis_runtime(
        crate::shared_runtime_from_conn(Some(conn)),
        "test:".to_string(),
    );

    let key = "user:rejected_requests_do_not_extend_window:auth";
    ok(limiter.reset(key).await, "rate limit key should reset");

    allow(&limiter, key, 2, 2).await;
    allow(&limiter, key, 2, 2).await;
    assert!(matches!(
        limiter.check_rate_limit(key, 2, 2).await,
        Err(RateLimitError::RateLimitExceeded { .. })
    ));

    tokio::time::sleep(tokio::time::Duration::from_millis(1_100)).await;
    assert!(matches!(
        limiter.check_rate_limit(key, 2, 2).await,
        Err(RateLimitError::RateLimitExceeded { .. })
    ));

    tokio::time::sleep(tokio::time::Duration::from_millis(1_050)).await;
    allow(&limiter, key, 2, 2).await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_quota() {
    let (_redis, conn) = start_redis().await;
    let conn = Arc::new(tokio::sync::RwLock::new(conn));
    let limiter = RateLimiter::from_redis_runtime(
        crate::shared_runtime_from_conn(Some(conn)),
        "test:".to_string(),
    );

    let key = "user:test3:chat";
    ok(limiter.reset(key).await, "rate limit key should reset");

    let (remaining, _) = ok(limiter.get_quota(key, 10, 1).await, "quota should load");
    assert_eq!(remaining, 10);

    for _ in 0..3 {
        allow(&limiter, key, 10, 1).await;
    }

    let (remaining, reset_time) = ok(limiter.get_quota(key, 10, 1).await, "quota should reload");
    assert_eq!(remaining, 7);
    assert!(reset_time <= 1);
}

#[tokio::test]
async fn test_without_redis_uses_governor_fallback() {
    let limiter = RateLimiter::local_only("test:".to_string());

    let key = "user:test_gov:chat";
    for i in 0..10 {
        ok(
            limiter.check_rate_limit(key, 10, 1).await,
            &format!("Governor request {i} should succeed"),
        );
    }

    let result = limiter.check_rate_limit(key, 10, 1).await;
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "Governor rate limiter should enforce limits"
    );
}

#[tokio::test]
async fn test_governor_independent_keys() {
    let limiter = RateLimiter::local_only("test:".to_string());

    for _ in 0..5 {
        allow(&limiter, "key1", 5, 1).await;
    }
    assert!(limiter.check_rate_limit("key1", 5, 1).await.is_err());
    assert!(limiter.check_rate_limit("key2", 5, 1).await.is_ok());
}

#[tokio::test]
async fn test_room_password_rate_limit_pattern() {
    let limiter = RateLimiter::local_only("test:".to_string());

    let ip = "192.168.1.1";
    let room_id = "room_abc";
    let key = format!("room_password_check:{ip}:{room_id}");

    for i in 0..5 {
        ok(
            limiter.check_rate_limit(&key, 5, 300).await,
            &format!("Attempt {} should succeed", i + 1),
        );
    }

    let result = limiter.check_rate_limit(&key, 5, 300).await;
    assert!(
        matches!(result, Err(RateLimitError::RateLimitExceeded { .. })),
        "6th attempt should be rate limited"
    );
}

#[tokio::test]
async fn test_room_password_rate_limit_per_ip_isolation() {
    let limiter = RateLimiter::local_only("test:".to_string());

    let room_id = "room_xyz";
    let key_ip1 = format!("room_password_check:10.0.0.1:{room_id}");
    let key_ip2 = format!("room_password_check:10.0.0.2:{room_id}");

    for _ in 0..5 {
        allow(&limiter, &key_ip1, 5, 300).await;
    }
    assert!(limiter.check_rate_limit(&key_ip1, 5, 300).await.is_err());
    assert!(limiter.check_rate_limit(&key_ip2, 5, 300).await.is_ok());
}

#[tokio::test]
async fn test_room_password_rate_limit_per_room_isolation() {
    let limiter = RateLimiter::local_only("test:".to_string());

    let ip = "10.0.0.1";
    let key_room1 = format!("room_password_check:{ip}:room_1");
    let key_room2 = format!("room_password_check:{ip}:room_2");

    for _ in 0..5 {
        allow(&limiter, &key_room1, 5, 300).await;
    }
    assert!(limiter.check_rate_limit(&key_room1, 5, 300).await.is_err());
    assert!(limiter.check_rate_limit(&key_room2, 5, 300).await.is_ok());
}

#[tokio::test]
async fn test_concurrent_burst_all_within_limit() {
    let limiter = RateLimiter::local_only("burst_test:".to_string());

    let mut handles = Vec::new();
    for _ in 0..10 {
        let limiter = limiter.clone();
        handles.push(tokio::spawn(async move {
            limiter.check_rate_limit("burst_key", 10, 1).await
        }));
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|result| joined(result, "rate-limit task should join"))
        .collect();

    let successes = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        successes, 10,
        "All 10 concurrent requests within limit should succeed"
    );
}

#[tokio::test]
async fn test_concurrent_burst_exceeding_limit() {
    let limiter = RateLimiter::local_only("burst_over:".to_string());

    let mut handles = Vec::new();
    for _ in 0..20 {
        let limiter = limiter.clone();
        handles.push(tokio::spawn(async move {
            limiter.check_rate_limit("burst_over_key", 5, 1).await
        }));
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|result| joined(result, "rate-limit task should join"))
        .collect();

    let successes = results.iter().filter(|r| r.is_ok()).count();
    let failures = results.iter().filter(|r| r.is_err()).count();

    assert_eq!(successes, 5, "Only 5 concurrent requests should succeed");
    assert_eq!(failures, 15, "15 requests should be rate limited");
}

#[test]
fn test_check_rate_limit_sync_allows_within_limit() {
    let limiter = RateLimiter::local_only("sync_test:".to_string());
    assert!(limiter.check_rate_limit_sync("sync_key", 5, 1).is_ok());
}

#[test]
fn test_check_rate_limit_sync_blocks_over_limit() {
    let limiter = RateLimiter::local_only("sync_block:".to_string());

    for _ in 0..5 {
        ok(
            limiter.check_rate_limit_sync("sync_key", 5, 1),
            "sync rate limit should allow request",
        );
    }

    let result = limiter.check_rate_limit_sync("sync_key", 5, 1);
    assert!(matches!(
        result,
        Err(RateLimitError::RateLimitExceeded { .. })
    ));
}

#[test]
fn test_check_rate_limit_sync_uses_grpc_key_prefix() {
    let limiter = RateLimiter::local_only("myprefix:".to_string());

    for _ in 0..3 {
        ok(
            limiter.check_rate_limit_sync("key1", 3, 1),
            "sync rate limit should allow request",
        );
    }
    assert!(limiter.check_rate_limit_sync("key1", 3, 1).is_err());
}

/// Test that InMemoryRateLimitBackend::check_strict fails closed when Redis is not configured.
/// The caller gets an explicit backend-unavailable error so transports can map
/// it to 503/Unavailable instead of pretending the quota itself was exceeded.

#[tokio::test]
async fn test_strict_distributed_flag_makes_check_rate_limit_fail_closed() {
    let limiter = RateLimiter::local_only("strict_switch:".to_string()).with_strict_distributed();

    let result = limiter.check_rate_limit("key", 5, 60).await;
    assert!(
        matches!(result, Err(RateLimitError::BackendUnavailable(_))),
        "strict distributed mode should fail closed through check_rate_limit"
    );
}

#[tokio::test]
async fn test_non_strict_in_memory_check_rate_limit_still_allows_requests() {
    let limiter = RateLimiter::local_only("non_strict_switch:".to_string());

    let result = limiter.check_rate_limit("key", 5, 60).await;
    assert!(
        result.is_ok(),
        "non-strict mode should preserve in-memory behavior"
    );
}

#[tokio::test]
async fn test_in_memory_check_strict_fails_closed() {
    let limiter = RateLimiter::local_only("strict_test:".to_string());

    // Should reject because distributed coordination is unavailable.
    let result = limiter
        .check_rate_limit_distributed("strict_key", 5, 1)
        .await;
    assert!(
        matches!(result, Err(RateLimitError::BackendUnavailable(_))),
        "check_strict should fail closed when Redis is not configured"
    );
}

/// Test that check_strict with in-memory backend fails closed for every key.
#[tokio::test]
async fn test_in_memory_check_strict_rejects_all_keys() {
    let limiter = RateLimiter::local_only("strict_keys:".to_string());

    // key1 should fail closed
    let result1 = limiter.check_rate_limit_distributed("key1", 5, 1).await;
    assert!(
        matches!(result1, Err(RateLimitError::BackendUnavailable(_))),
        "check_strict should fail closed for key1 when Redis is not configured"
    );

    // key2 should also fail closed
    let result2 = limiter.check_rate_limit_distributed("key2", 5, 1).await;
    assert!(
        matches!(result2, Err(RateLimitError::BackendUnavailable(_))),
        "check_strict should fail closed for key2 when Redis is not configured"
    );
}

#[test]
fn test_rate_limit_error_to_core_error_exceeded() {
    let err = RateLimitError::RateLimitExceeded {
        retry_after_seconds: 30,
    };
    let core_err: crate::Error = err.into();
    match core_err {
        crate::Error::RateLimited(msg) => {
            assert!(msg.contains("30"));
        }
        other => std::panic::panic_any(format!("Expected RateLimited, got: {other:?}")),
    }
}

#[test]
fn test_rate_limit_error_to_core_error_redis() {
    let redis_err = redis::RedisError::from((redis::ErrorKind::Io, "connection refused"));
    let err = RateLimitError::RedisError(redis_err);
    let core_err: crate::Error = err.into();
    match core_err {
        crate::Error::Internal(msg) => {
            assert!(msg.contains("Rate limiter Redis error"));
        }
        other => std::panic::panic_any(format!("Expected Internal, got: {other:?}")),
    }
}

#[test]
fn test_rate_limit_error_to_core_error_backend_unavailable() {
    let err = RateLimitError::BackendUnavailable("redis unavailable".to_string());
    let core_err: crate::Error = err.into();
    match core_err {
        crate::Error::ServiceUnavailable(msg) => {
            assert!(msg.contains("redis unavailable"));
        }
        other => std::panic::panic_any(format!("Expected ServiceUnavailable, got: {other:?}")),
    }
}

#[tokio::test]
async fn test_get_quota_without_redis_does_not_consume_token() {
    let limiter = RateLimiter::local_only("quota_no_consume:".to_string());

    for _ in 0..20 {
        let (remaining, _) = ok(limiter.get_quota("key", 10, 1).await, "quota should load");
        assert_eq!(remaining, 10);
    }

    for i in 0..10 {
        ok(
            limiter.check_rate_limit("key", 10, 1).await,
            &format!("Request {i} should succeed after get_quota calls"),
        );
    }
}

#[tokio::test]
async fn test_health_check_without_redis() {
    let limiter = RateLimiter::local_only("health:".to_string());
    let result = limiter.health_check().await;
    assert!(result.is_err());
    assert!(match result {
        Ok(()) => {
            std::panic::panic_any("health check should fail without Redis");
        }
        Err(error) => error.contains("not configured"),
    });
}

#[tokio::test]
async fn test_in_memory_different_quotas_are_independent() {
    let limiter = RateLimiter::local_only("quotas:".to_string());

    for _ in 0..5 {
        allow(&limiter, "same_key", 5, 1).await;
    }
    assert!(limiter.check_rate_limit("same_key", 5, 1).await.is_err());
    assert!(limiter.check_rate_limit("same_key", 10, 1).await.is_ok());
}

#[tokio::test]
async fn test_redis_failure_falls_back_to_in_memory() {
    let limiter = RateLimiter::from_redis_runtime(
        Some(Arc::new(FailingRedisRuntime)),
        "fallback_test:".to_string(),
    );

    for i in 0..5 {
        ok(
            limiter.check_rate_limit("fb_key", 5, 1).await,
            &format!("Request {i} should use in-memory fallback"),
        );
    }
    assert!(
        matches!(
            limiter.check_rate_limit("fb_key", 5, 1).await,
            Err(RateLimitError::RateLimitExceeded { .. })
        ),
        "in-memory fallback should enforce the quota after Redis is unavailable"
    );
}
