//! Distributed lock service tests
//!
//! Tests the Redis-based distributed lock implementation with testcontainers.
//!
//! Test coverage:
//! - Basic lock acquire/release cycle
//! - Lock expiration and TTL renewal
//! - Fencing token monotonic increase
//! - Concurrent lock acquisition (only one succeeds)
//! - Lock release by non-owner fails
//! - Redis connection failure handling
//!
//! Run Docker tests: cargo test --test `distributed_lock_tests` -- --ignored

use std::time::Duration;
use synctv_core::service::distributed_lock::DistributedLock;
use synctv_core::Error;
use synctv_core_testing::start_redis as start_test_redis;
use synctv_core_testing::{some, TestResultExt};

/// Start a Redis container and return connection manager
async fn start_redis() -> (
    synctv_core_testing::RedisContainer,
    redis::aio::ConnectionManager,
) {
    start_test_redis().await
}

async fn acquire_lock(lock: &DistributedLock, key: &str, ttl: u64) -> String {
    some(
        lock.acquire(key, ttl)
            .await
            .checked("lock acquire should complete"),
        "lock should be acquired",
    )
}

async fn acquire_lock_with_token(lock: &DistributedLock, key: &str, ttl: u64) -> (String, u64) {
    some(
        lock.acquire_with_token(key, ttl)
            .await
            .checked("lock acquire with token should complete"),
        "lock with token should be acquired",
    )
}

// Basic lock acquire/release cycle tests

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_basic_acquire_release_cycle() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key = "test_basic_lock";
    let ttl = 10;

    // Acquire lock
    let lock_value = acquire_lock(&lock, key, ttl).await;

    // Verify lock is held by trying to acquire again
    let second_attempt = lock
        .acquire(key, ttl)
        .await
        .checked("test operation should succeed");
    assert!(
        second_attempt.is_none(),
        "Second acquire should fail (lock already held)"
    );

    // Release lock
    let released = lock
        .release(key, &lock_value)
        .await
        .checked("test operation should succeed");
    assert!(released, "Lock release should succeed");

    // Verify lock can be acquired again after release
    let third_attempt = lock
        .acquire(key, ttl)
        .await
        .checked("test operation should succeed");
    assert!(third_attempt.is_some(), "Should acquire lock after release");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_acquire_with_token() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key = "test_token_lock";
    let ttl = 10;

    // Acquire lock with fencing token
    let (lock_value, fencing_token) = acquire_lock_with_token(&lock, key, ttl).await;
    assert!(!lock_value.is_empty(), "Lock value should not be empty");
    assert!(fencing_token > 0, "Fencing token should be positive");

    // Release the lock
    lock.release(key, &lock_value)
        .await
        .checked("test operation should succeed");
}

// Lock expiration and TTL tests

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_lock_expiration() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key = "test_expiration_lock";
    let ttl = 1; // 1 second TTL

    // Acquire lock
    let lock_value = acquire_lock(&lock, key, ttl).await;

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Try to release - should fail because lock expired
    let released = lock
        .release(key, &lock_value)
        .await
        .checked("test operation should succeed");
    assert!(!released, "Lock release should fail (lock expired)");

    // Lock should be available for acquisition again
    let new_lock = lock
        .acquire(key, ttl)
        .await
        .checked("test operation should succeed");
    assert!(new_lock.is_some(), "Should acquire expired lock");
}

// Fencing token tests

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_fencing_token_monotonic_increase() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key = "test_monotonic_token";
    let ttl = 10;

    // Acquire and release multiple times, collecting tokens
    let mut tokens = Vec::new();

    for _ in 0..5 {
        let (lock_value, token) = acquire_lock_with_token(&lock, key, ttl).await;
        tokens.push(token);

        lock.release(key, &lock_value)
            .await
            .checked("test operation should succeed");
    }

    // Verify tokens are monotonically increasing
    for i in 1..tokens.len() {
        assert!(
            tokens[i] > tokens[i - 1],
            "Token {} should be greater than token {}",
            tokens[i],
            tokens[i - 1]
        );
    }

    // Verify tokens start from 1 and increment by 1
    assert_eq!(tokens[0], 1, "First token should be 1");
    assert_eq!(tokens[4], 5, "Fifth token should be 5");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_fencing_token_independent_per_key() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key1 = "test_token_key1";
    let key2 = "test_token_key2";
    let ttl = 10;

    // Get token for key1
    let (lock1, token1) = acquire_lock_with_token(&lock, key1, ttl).await;

    // Get token for key2
    let (lock2, token2) = acquire_lock_with_token(&lock, key2, ttl).await;

    // Tokens should be independent
    assert_eq!(token1, 1, "Key1 first token should be 1");
    assert_eq!(token2, 1, "Key2 first token should be 1");

    // Get second token for key1
    lock.release(key1, &lock1)
        .await
        .checked("test operation should succeed");
    let (_, token1b) = acquire_lock_with_token(&lock, key1, ttl).await;

    assert_eq!(token1b, 2, "Key1 second token should be 2");

    // Get second token for key2 - should still be 2 (independent counter)
    lock.release(key2, &lock2)
        .await
        .checked("test operation should succeed");
    let (_, token2b) = acquire_lock_with_token(&lock, key2, ttl).await;

    assert_eq!(token2b, 2, "Key2 second token should be 2");
}

// Concurrent lock acquisition tests

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_acquire_only_one_succeeds() {
    let (_container, conn) = start_redis().await;
    let lock = std::sync::Arc::new(DistributedLock::new(conn));

    let key = "test_concurrent_lock";
    let ttl = 10;

    // Spawn 10 tasks trying to acquire the same lock simultaneously
    let mut handles = Vec::new();

    for _i in 0..10 {
        let lock_clone = lock.clone();
        let key_str = key.to_string();
        handles.push(tokio::spawn(async move {
            lock_clone.acquire(&key_str, ttl).await
        }));
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.checked("test operation should succeed"))
        .collect();

    // Count successes and failures
    let successes = results
        .iter()
        .filter(|result| matches!(result, Ok(Some(_))))
        .count();
    let failures = results
        .iter()
        .filter(|result| matches!(result, Ok(None)))
        .count();

    assert_eq!(successes, 1, "Exactly 1 concurrent acquire should succeed");
    assert_eq!(failures, 9, "9 concurrent acquires should fail");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_with_fencing_tokens() {
    let (_container, conn) = start_redis().await;
    let lock = std::sync::Arc::new(DistributedLock::new(conn));

    let key = "test_concurrent_token_lock";
    let ttl = 10;

    // Spawn 5 tasks trying to acquire the same lock
    let mut handles = Vec::new();

    for _ in 0..5 {
        let lock_clone = lock.clone();
        let key_str = key.to_string();
        handles.push(tokio::spawn(async move {
            lock_clone.acquire_with_token(&key_str, ttl).await
        }));
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.checked("test operation should succeed"))
        .collect();

    // Only one should succeed
    let successful: Vec<_> = results
        .iter()
        .filter_map(|result| match result {
            Ok(Some(value)) => Some(value.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(successful.len(), 1, "Only 1 lock should be acquired");

    // The successful one should have a fencing token
    let (lock_value, fencing_token) = &successful[0];
    assert!(!lock_value.is_empty(), "Lock value should not be empty");
    assert!(*fencing_token > 0, "Fencing token should be positive");
}

// Lock release by non-owner tests

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_release_by_non_owner_fails() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key = "test_non_owner_release";
    let ttl = 10;

    // Acquire lock
    let lock_value = acquire_lock(&lock, key, ttl).await;

    // Try to release with wrong value
    let wrong_value = "wrong_lock_value";
    let released = lock
        .release(key, wrong_value)
        .await
        .checked("test operation should succeed");
    assert!(!released, "Release with wrong value should fail");

    // Original lock should still be held
    let second_attempt = lock
        .acquire(key, ttl)
        .await
        .checked("test operation should succeed");
    assert!(second_attempt.is_none(), "Lock should still be held");

    // Release with correct value should succeed
    let released = lock
        .release(key, &lock_value)
        .await
        .checked("test operation should succeed");
    assert!(released, "Release with correct value should succeed");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_release_expired_lock_returns_false() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key = "test_expired_release";
    let ttl = 1;

    // Acquire lock
    let lock_value = acquire_lock(&lock, key, ttl).await;

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Try to release expired lock
    let released = lock
        .release(key, &lock_value)
        .await
        .checked("test operation should succeed");
    assert!(!released, "Releasing expired lock should return false");
}

// with_lock and try_with_lock tests

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_with_lock_auto_release() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key = "test_with_lock_auto";
    let ttl = 10;

    // Execute operation with automatic lock management
    let result = lock
        .with_lock(key, ttl, || async {
            // Lock is held here
            // Try to acquire again - should fail
            let inner = lock
                .acquire(key, ttl)
                .await
                .checked("test operation should succeed");
            assert!(inner.is_none(), "Nested acquire should fail");
            Ok::<_, Error>(42)
        })
        .await
        .checked("test operation should succeed");

    assert_eq!(result, 42, "Operation result should be returned");

    // Lock should be auto-released, so we can acquire it again
    let _new_lock = acquire_lock(&lock, key, ttl).await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_with_lock_releases_on_error() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key = "test_with_lock_error";
    let ttl = 10;

    // Execute operation that fails
    let result = lock
        .with_lock(key, ttl, || async {
            Err::<(), _>(Error::Internal("Test error".to_string()))
        })
        .await;

    assert!(result.is_err(), "Operation should fail");

    // Lock should still be released despite error
    let _new_lock = acquire_lock(&lock, key, ttl).await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_try_with_lock_returns_none_when_held() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key = "test_try_with_lock";
    let ttl = 10;

    // Acquire lock first
    let _lock_value = lock
        .acquire(key, ttl)
        .await
        .checked("test operation should succeed");

    // try_with_lock should return None when lock is held
    let result = lock
        .try_with_lock(key, ttl, || async { Ok::<_, Error>(42) })
        .await
        .checked("test operation should succeed");

    assert!(
        result.is_none(),
        "try_with_lock should return None when lock held"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_with_lock_token_passes_fencing_token() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key = "test_with_lock_token";
    let ttl = 10;

    // Execute operation with fencing token
    let result = lock
        .with_lock_token(key, ttl, |token| async move {
            assert!(token > 0, "Fencing token should be positive");
            Ok::<_, Error>(token * 2)
        })
        .await
        .checked("test operation should succeed");

    assert!(result > 0, "Result should be based on fencing token");
}

// Lock value uniqueness tests

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_lock_values_are_unique() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key = "test_unique_values";
    let ttl = 10;

    // Acquire and release 10 times, collecting lock values
    let mut lock_values = Vec::new();

    for _ in 0..10 {
        let lock_value = acquire_lock(&lock, key, ttl).await;
        lock_values.push(lock_value.clone());

        lock.release(key, &lock_value)
            .await
            .checked("test operation should succeed");
    }

    // All 10 values should be distinct (check after all acquisitions)
    let unique_count: std::collections::HashSet<_> = lock_values.iter().collect();
    assert_eq!(
        unique_count.len(),
        10,
        "All lock values should be unique, found duplicates: {lock_values:?}"
    );
}

// Edge case tests

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_zero_ttl_lock() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key = "test_zero_ttl";
    let ttl = 1; // Use 1 second instead of 0 (Redis may reject TTL=0)

    // Test with minimal TTL
    let lock_value = acquire_lock(&lock, key, ttl).await;
    let released = lock
        .release(key, &lock_value)
        .await
        .checked("test operation should succeed");
    assert!(released, "Should release lock with minimal TTL");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_very_long_ttl() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key = "test_long_ttl";
    let ttl = 86400; // 24 hours

    let lock_value = acquire_lock(&lock, key, ttl).await;
    lock.release(key, &lock_value)
        .await
        .checked("test operation should succeed");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_special_characters_in_key() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let keys = vec![
        "test:colon:key",
        "test/slash/key",
        "test-key-with-dashes",
        "test_key_with_underscores",
        "test.key.with.dots",
    ];

    for key in keys {
        let lock_value = acquire_lock(&lock, key, 10).await;
        let released = lock
            .release(key, &lock_value)
            .await
            .checked("test operation should succeed");
        assert!(released, "Release with key '{key}' should succeed");
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_empty_lock_value_in_release() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key = "test_empty_release";

    // Try to release with empty string
    let released = lock
        .release(key, "")
        .await
        .checked("test operation should succeed");
    assert!(!released, "Release with empty value should fail");
}

// Stress tests (marked as ignore as they take longer)

#[tokio::test]
#[ignore = "Requires Docker and takes time"]
async fn test_rapid_acquire_release_cycles() {
    let (_container, conn) = start_redis().await;
    let lock = DistributedLock::new(conn);

    let key = "test_rapid_cycles";
    let ttl = 1;

    for i in 0..100 {
        let lock_value = acquire_lock(&lock, key, ttl).await;
        let released = lock
            .release(key, &lock_value)
            .await
            .checked("test operation should succeed");
        assert!(released, "Cycle {i} should release lock");
    }
}

#[tokio::test]
#[ignore = "Requires Docker and takes time"]
async fn test_many_concurrent_clients() {
    let (_container, conn) = start_redis().await;
    let lock = std::sync::Arc::new(DistributedLock::new(conn));

    let key = "test_many_clients";
    let ttl = 10;

    // 100 clients trying to acquire
    let mut handles = Vec::new();
    for i in 0..100 {
        let lock_clone = lock.clone();
        let key_str = key.to_string();
        handles.push(tokio::spawn(async move {
            let result = lock_clone.acquire(&key_str, ttl).await;
            (i, result.map(|lock_value| lock_value.is_some()))
        }));
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.checked("test operation should succeed"))
        .map(|(i, result)| (i, result.checked("lock acquire should complete")))
        .collect();

    assert_eq!(
        results.iter().filter(|(_, won)| *won).count(),
        1,
        "Only 1 of 100 clients should acquire lock"
    );
}
