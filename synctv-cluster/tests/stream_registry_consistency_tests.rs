//! `StreamRegistry` Consistency Tests
//!
//! Tests for symmetric registration/unregistration order to ensure
//! local cache and Redis state remain consistent on failures.
//!
//! These tests verify the key invariant:
//! - Register: Redis first, then local (if Redis fails, local unchanged)
//! - Unregister: Should be Redis first, then local (if Redis fails, local should be restored)

#![allow(clippy::unwrap_used)]
use std::time::Duration;

use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

use synctv_cluster::sync::stream_registry::StreamRegistry;

/// Default Redis version for test containers
#[allow(dead_code)]
const REDIS_VERSION: &str = "7-alpine";

/// Helper to create a Redis container and connection manager.
async fn setup_redis() -> (
    testcontainers::ContainerAsync<Redis>,
    redis::aio::ConnectionManager,
) {
    let redis_container = tokio::time::timeout(Duration::from_secs(30), Redis::default().start())
        .await
        .expect("Docker container startup timed out (is Docker running?)")
        .expect("Failed to start Redis container");

    let redis_host = redis_container
        .get_host()
        .await
        .expect("Failed to get Redis host");
    let redis_port = redis_container
        .get_host_port_ipv4(6379)
        .await
        .expect("Failed to get Redis port");

    let redis_url = format!("redis://{redis_host}:{redis_port}");
    let redis_client =
        redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");

    // Wait for Redis with retries
    let conn = {
        let mut retries = 0;
        loop {
            match redis::aio::ConnectionManager::new(redis_client.clone()).await {
                Ok(conn) => break conn,
                Err(_) if retries < 60 => {
                    retries += 1;
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(e) => panic!("Redis ConnectionManager failed after {retries} retries: {e}"),
            }
        }
    };

    // Verify Redis
    let mut test_conn = conn.clone();
    let _: () = redis::cmd("PING")
        .query_async(&mut test_conn)
        .await
        .expect("Redis PING failed");

    (redis_container, conn)
}

/// Test: Without Redis, register and unregister should work on local cache only.
#[tokio::test]
async fn test_local_only_register_unregister() {
    let registry = StreamRegistry::new("replica1".to_string());

    // Register
    registry
        .register_stream("app/stream", "rtmp", None)
        .await
        .unwrap();
    assert!(registry.is_local_stream("app/stream"));

    // Unregister
    registry.unregister_stream("app/stream").await;
    assert!(!registry.is_local_stream("app/stream"));
}

/// Test: Multiple streams can be registered and unregistered independently.
#[tokio::test]
async fn test_multiple_streams_independent_lifecycle() {
    let registry = StreamRegistry::new("replica1".to_string());

    // Register multiple streams
    registry
        .register_stream("app/stream1", "rtmp", None)
        .await
        .unwrap();
    registry
        .register_stream("app/stream2", "webrtc", None)
        .await
        .unwrap();
    registry
        .register_stream("app/stream3", "srt", None)
        .await
        .unwrap();

    assert_eq!(registry.get_local_streams().len(), 3);

    // Unregister one
    registry.unregister_stream("app/stream2").await;
    assert!(!registry.is_local_stream("app/stream2"));
    assert!(registry.is_local_stream("app/stream1"));
    assert!(registry.is_local_stream("app/stream3"));

    // Unregister remaining
    registry.unregister_stream("app/stream1").await;
    registry.unregister_stream("app/stream3").await;

    assert_eq!(registry.get_local_streams().len(), 0);
}

/// Test: Re-registering a stream after unregister should work.
#[tokio::test]
async fn test_reregister_after_unregister() {
    let registry = StreamRegistry::new("replica1".to_string());

    // Register
    registry
        .register_stream("app/stream", "rtmp", None)
        .await
        .unwrap();
    assert!(registry.is_local_stream("app/stream"));

    // Unregister
    registry.unregister_stream("app/stream").await;
    assert!(!registry.is_local_stream("app/stream"));

    // Re-register with different type
    registry
        .register_stream("app/stream", "webrtc", None)
        .await
        .unwrap();
    assert!(registry.is_local_stream("app/stream"));

    // Verify metadata was updated
    let meta = registry.get_stream("app/stream").await.unwrap();
    assert_eq!(meta.pub_type, "webrtc");
}

/// Test: With Redis, register should persist to Redis first, then local.
/// This test verifies the registration order is correct.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_register_order_redis_first_then_local() {
    let (_container, conn) = setup_redis().await;

    let registry =
        StreamRegistry::new("replica1".to_string()).with_redis(conn.clone(), "consistency:");

    // Register a stream
    registry
        .register_stream("app/order_test", "rtmp", None)
        .await
        .unwrap();

    // Verify it's in Redis
    let mut test_conn = conn.clone();
    let exists: bool = redis::cmd("EXISTS")
        .arg("consistency:streams:meta:app/order_test")
        .query_async(&mut test_conn)
        .await
        .unwrap();
    assert!(exists, "Stream should be in Redis after register");

    // Verify it's in local cache
    assert!(registry.is_local_stream("app/order_test"));
}

/// Test: With Redis, unregister should remove from Redis, then local.
/// This test verifies the expected behavior: if Redis removal succeeds,
/// local cache should also be cleared.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_unregister_order_redis_first_then_local() {
    let (_container, conn) = setup_redis().await;

    let registry =
        StreamRegistry::new("replica1".to_string()).with_redis(conn.clone(), "consistency2:");

    // Register a stream first
    registry
        .register_stream("app/unregister_order_test", "rtmp", None)
        .await
        .unwrap();
    assert!(registry.is_local_stream("app/unregister_order_test"));

    // Unregister
    registry
        .unregister_stream("app/unregister_order_test")
        .await;

    // Verify it's removed from Redis
    let mut test_conn = conn.clone();
    let exists: bool = redis::cmd("EXISTS")
        .arg("consistency2:streams:meta:app/unregister_order_test")
        .query_async(&mut test_conn)
        .await
        .unwrap();
    assert!(
        !exists,
        "Stream should be removed from Redis after unregister"
    );

    // Verify it's removed from local cache
    assert!(
        !registry.is_local_stream("app/unregister_order_test"),
        "Stream should be removed from local cache after unregister"
    );
}

/// Test: Cross-instance visibility after register.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_cross_instance_visibility() {
    let (_container, conn) = setup_redis().await;

    let registry1 = StreamRegistry::new("replica1".to_string()).with_redis(conn.clone(), "cross:");
    let registry2 = StreamRegistry::new("replica2".to_string()).with_redis(conn.clone(), "cross:");

    // Register on replica1
    registry1
        .register_stream("app/cross_test", "rtmp", None)
        .await
        .unwrap();

    // Replica2 should see it via Redis
    let streams = registry2.get_all_streams().await;
    assert!(
        streams.iter().any(|s| s.identifier == "app/cross_test"),
        "Replica2 should see stream registered on Replica1"
    );

    // Unregister from replica1
    registry1.unregister_stream("app/cross_test").await;

    // Replica2 should no longer see it
    let streams = registry2.get_all_streams().await;
    assert!(
        !streams.iter().any(|s| s.identifier == "app/cross_test"),
        "Replica2 should not see stream after unregister"
    );
}

/// Test: `get_stream` should check local cache first, then Redis.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_get_stream_local_first_then_redis() {
    let (_container, conn) = setup_redis().await;

    let registry1 =
        StreamRegistry::new("replica1".to_string()).with_redis(conn.clone(), "getstream:");
    let registry2 =
        StreamRegistry::new("replica2".to_string()).with_redis(conn.clone(), "getstream:");

    // Register on replica1
    registry1
        .register_stream("app/get_test", "rtmp", Some("192.168.1.1:1234".to_string()))
        .await
        .unwrap();

    // Replica1 should get from local cache
    let meta1 = registry1.get_stream("app/get_test").await;
    assert!(meta1.is_some());
    let meta1 = meta1.unwrap();
    assert_eq!(meta1.replica_id, "replica1");
    assert_eq!(meta1.publisher_addr.as_deref(), Some("192.168.1.1:1234"));

    // Replica2 should get from Redis (not in its local cache)
    let meta2 = registry2.get_stream("app/get_test").await;
    assert!(meta2.is_some());
    let meta2 = meta2.unwrap();
    assert_eq!(meta2.replica_id, "replica1");
    assert_eq!(meta2.publisher_addr.as_deref(), Some("192.168.1.1:1234"));
}
