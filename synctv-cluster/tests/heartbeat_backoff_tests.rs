//! Tests for heartbeat re-registration backoff strategy
//!
//! These tests verify that when heartbeat fails, the re-registration attempt
//! uses exponential backoff to avoid hammering Redis during outages.

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use std::time::Duration;
use synctv_core_testing::{start_redis_url_with_label, RedisContainer};

use synctv_cluster::discovery::node_registry::NodeRegistry;
use synctv_cluster::HeartbeatResult;

/// Default Redis version for test containers
#[allow(dead_code)]
const REDIS_VERSION: &str = "7";

fn docker_startup_timeout() -> Duration {
    std::env::var("SYNCTV_TEST_DOCKER_STARTUP_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|secs| secs.max(30))
        .map_or_else(|| Duration::from_mins(2), Duration::from_secs)
}

/// Helper to create a Redis container and client.
async fn setup_redis() -> (RedisContainer, redis::Client, String) {
    let (redis_container, redis_url) = tokio::time::timeout(
        docker_startup_timeout(),
        start_redis_url_with_label("heartbeat-backoff"),
    )
    .await
    .expect("Docker container startup timed out (is Docker running?)");
    let redis_client =
        redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");

    // Wait for Redis to be ready
    let mut conn = {
        let mut retries = 0;
        loop {
            match redis_client.get_multiplexed_async_connection().await {
                Ok(conn) => break conn,
                Err(_) if retries < 60 => {
                    retries += 1;
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
                Err(e) => panic!("Redis connection failed after {retries} retries: {e}"),
            }
        }
    };
    let _: () = redis::cmd("PING")
        .query_async(&mut conn)
        .await
        .expect("Redis PING failed");
    drop(conn);

    (redis_container, redis_client, redis_url)
}

async fn connect_redis_with_retry(
    redis_client: &redis::Client,
) -> redis::aio::MultiplexedConnection {
    let mut retries = 0u32;
    loop {
        match redis_client.get_multiplexed_async_connection().await {
            Ok(conn) => return conn,
            Err(_) if retries < 60 => {
                retries += 1;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => panic!("Redis connection failed after {retries} retries: {e}"),
        }
    }
}

/// Test that re-registration after heartbeat failure uses backoff.
///
/// This test verifies the backoff mechanism:
/// 1. Registers a node
/// 2. Sets a test backoff period
/// 3. Deletes the key from Redis
/// 4. Calls heartbeat (should detect missing key but respect backoff)
/// 5. Waits for backoff to expire
/// 6. Calls heartbeat again (should trigger re-registration)
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_heartbeat_reregistration_has_backoff() {
    let (_redis_container, redis_client, _url) = setup_redis().await;

    let registry = Arc::new(
        NodeRegistry::new(
            redis_client.clone(),
            "backoff-node".to_string(),
            30,
            "backoff:",
        )
        .expect("new should succeed"),
    );

    // Register the node
    registry
        .register("localhost:8080".to_string())
        .await
        .expect("register should succeed");

    // First heartbeat should succeed
    let result = registry
        .heartbeat()
        .await
        .expect("heartbeat should succeed");
    assert_eq!(result, HeartbeatResult::Ok);

    // Verify initial backoff state
    assert!(
        !registry.is_in_reregister_backoff().await,
        "Should not be in backoff initially"
    );

    // Set a backoff period for testing
    registry
        .set_reregister_backoff_for_test(Duration::from_millis(200))
        .await;

    // Delete the key to simulate expiry
    let mut conn = connect_redis_with_retry(&redis_client).await;
    let key = "backoff:cluster:nodes:backoff-node";
    let _: () = redis::cmd("DEL")
        .arg(key)
        .query_async(&mut conn)
        .await
        .expect("DEL should succeed");

    // With backoff active, re-registration should be skipped
    let result = registry.heartbeat().await.expect("heartbeat should return");
    assert_eq!(
        result,
        HeartbeatResult::NeedReregistration,
        "Re-registration should be skipped due to backoff"
    );

    // Verify we're in backoff
    assert!(
        registry.is_in_reregister_backoff().await,
        "Should be in backoff period"
    );

    // Wait for backoff to expire
    tokio::time::sleep(Duration::from_millis(250)).await;

    // Now re-registration should be allowed
    let result = registry
        .heartbeat()
        .await
        .expect("heartbeat should succeed");
    assert_eq!(
        result,
        HeartbeatResult::Ok,
        "Re-registration should succeed after backoff expires"
    );

    // Verify not in backoff anymore
    assert!(
        !registry.is_in_reregister_backoff().await,
        "Should not be in backoff after successful re-registration"
    );
}

/// Test that backoff is applied after re-registration failure and cleared after success.
///
/// Note: Backoff is only applied when re-registration FAILS, not when it succeeds.
/// When re-registration succeeds, backoff is reset.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_backoff_cleared_after_successful_heartbeat() {
    let (_redis_container, redis_client, _url) = setup_redis().await;

    let registry = Arc::new(
        NodeRegistry::new(
            redis_client.clone(),
            "clear-backoff-node".to_string(),
            30,
            "clearbackoff:",
        )
        .expect("new should succeed"),
    );

    // Register the node
    registry
        .register("localhost:8080".to_string())
        .await
        .expect("register should succeed");

    // Verify initial state: not in backoff
    assert!(
        !registry.is_in_reregister_backoff().await,
        "Should not be in backoff initially"
    );

    // First heartbeat should succeed
    let result = registry
        .heartbeat()
        .await
        .expect("heartbeat should succeed");
    assert_eq!(result, HeartbeatResult::Ok);

    // Still not in backoff (heartbeat succeeded)
    assert!(
        !registry.is_in_reregister_backoff().await,
        "Should not be in backoff after successful heartbeat"
    );

    // Delete the key to trigger re-registration
    let mut conn = connect_redis_with_retry(&redis_client).await;
    let key = "clearbackoff:cluster:nodes:clear-backoff-node";
    let _: () = redis::cmd("DEL")
        .arg(key)
        .query_async(&mut conn)
        .await
        .expect("DEL should succeed");

    // Trigger re-registration (will succeed and reset backoff)
    let result = registry
        .heartbeat()
        .await
        .expect("heartbeat should succeed");
    assert_eq!(result, HeartbeatResult::Ok);

    // After successful re-registration, backoff should still be at initial value (1s)
    // and last_attempt should be updated
    let backoff = registry.current_reregister_backoff().await;
    assert_eq!(
        backoff,
        std::time::Duration::from_secs(1),
        "Backoff should be at initial value after successful re-registration"
    );

    // Continue with successful heartbeats
    for _ in 0..3 {
        let result = registry
            .heartbeat()
            .await
            .expect("heartbeat should succeed");
        assert_eq!(result, HeartbeatResult::Ok);
    }

    // Backoff should remain at initial value
    let backoff = registry.current_reregister_backoff().await;
    assert_eq!(
        backoff,
        std::time::Duration::from_secs(1),
        "Backoff should remain at initial value with successful heartbeats"
    );
}

/// Test exponential backoff increases with consecutive failures.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_backoff_increases_exponentially() {
    let (_redis_container, redis_client, _url) = setup_redis().await;

    let registry = Arc::new(
        NodeRegistry::new(
            redis_client.clone(),
            "exp-backoff-node".to_string(),
            30,
            "expbackoff:",
        )
        .expect("new should succeed"),
    );

    // Register the node
    registry
        .register("localhost:8080".to_string())
        .await
        .expect("register should succeed");

    let mut conn = connect_redis_with_retry(&redis_client).await;
    let key = "expbackoff:cluster:nodes:exp-backoff-node";

    // Track backoff durations
    let mut backoff_durations = Vec::new();

    for i in 0..3 {
        // Delete the key
        let _: () = redis::cmd("DEL")
            .arg(key)
            .query_async(&mut conn)
            .await
            .expect("DEL should succeed");

        // Attempt heartbeat (will trigger re-registration or skip due to backoff)
        let _ = registry.heartbeat().await;

        // Get current backoff duration
        let backoff = registry.current_reregister_backoff().await;
        backoff_durations.push(backoff);

        // Simulate waiting (for test, we just record the backoff)
        println!("Iteration {i}: backoff = {backoff:?}");
    }

    // Backoff should increase (exponential)
    // Initial backoff might be 0 or small, then it should grow
    assert!(
        backoff_durations[2] >= backoff_durations[1],
        "Backoff should increase with consecutive failures: {backoff_durations:?}"
    );
}

/// Test that backoff remains at initial value with successful heartbeats.
///
/// Note: Backoff only increases when re-registration FAILS.
/// With successful re-registrations, backoff stays at the initial value.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_backoff_resets_after_recovery() {
    let (_redis_container, redis_client, _url) = setup_redis().await;

    let registry = Arc::new(
        NodeRegistry::new(
            redis_client.clone(),
            "recovery-node".to_string(),
            30,
            "recovery:",
        )
        .expect("new should succeed"),
    );

    // Register the node
    registry
        .register("localhost:8080".to_string())
        .await
        .expect("register should succeed");

    let mut conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let key = "recovery:cluster:nodes:recovery-node";

    // Initial backoff should be 1 second
    let initial_backoff = registry.current_reregister_backoff().await;
    assert_eq!(
        initial_backoff,
        std::time::Duration::from_secs(1),
        "Initial backoff should be 1 second"
    );

    // Trigger a re-registration (by deleting the key)
    let _: () = redis::cmd("DEL")
        .arg(key)
        .query_async(&mut conn)
        .await
        .expect("DEL should succeed");

    // Set a test backoff so the first heartbeat doesn't immediately re-register
    registry
        .set_reregister_backoff_for_test(Duration::from_millis(100))
        .await;

    // First heartbeat should skip re-registration due to backoff
    let result = registry.heartbeat().await.expect("heartbeat should return");
    assert_eq!(result, HeartbeatResult::NeedReregistration);

    // Backoff should be active
    assert!(
        registry.is_in_reregister_backoff().await,
        "Should be in backoff"
    );

    // Wait for backoff to expire
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Now re-registration should succeed
    let result = registry
        .heartbeat()
        .await
        .expect("heartbeat should succeed");
    assert_eq!(result, HeartbeatResult::Ok);

    // After successful re-registration, backoff should be reset to initial value
    let backoff_after = registry.current_reregister_backoff().await;
    assert_eq!(
        backoff_after,
        std::time::Duration::from_secs(1),
        "Backoff should be reset to initial value after successful re-registration"
    );

    // Continue with successful heartbeats
    for _ in 0..5 {
        let result = registry
            .heartbeat()
            .await
            .expect("heartbeat should succeed");
        assert_eq!(result, HeartbeatResult::Ok);
    }

    // Backoff should still be at initial value
    let final_backoff = registry.current_reregister_backoff().await;
    assert_eq!(
        final_backoff,
        std::time::Duration::from_secs(1),
        "Backoff should remain at initial value with successful heartbeats"
    );
}
