//! CL4: `StreamRegistry` Redis (testcontainer Redis)
//!
//! - `register_stream` Redis path, `get_all_streams` from second instance
//! - `cleanup_stale_active_entries` after key expiry
//! - `refresh_ttls` pipeline

#![allow(clippy::unwrap_used)]
use std::time::Duration;

use redis::AsyncCommands;
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

/// Test `register_stream` from one instance, `get_all_streams` from a second instance.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_register_stream_cross_instance_visibility() {
    let (_container, conn) = setup_redis().await;

    // Create two StreamRegistry instances (simulating two replicas)
    let registry1 =
        StreamRegistry::new("replica1".to_string()).with_redis(conn.clone(), "cl4test:");
    let registry2 =
        StreamRegistry::new("replica2".to_string()).with_redis(conn.clone(), "cl4test:");

    // Register a stream on replica1
    registry1
        .register_stream("app/stream1", "rtmp", Some("192.168.1.1:1234".to_string()))
        .await
        .unwrap();

    // Register a different stream on replica2
    registry2
        .register_stream("app/stream2", "webrtc", None)
        .await
        .unwrap();

    // get_all_streams from replica2 should see both streams (via Redis)
    let all_streams = registry2.get_all_streams().await;
    assert_eq!(
        all_streams.len(),
        2,
        "Should see both streams from get_all_streams, got {:?}",
        all_streams
            .iter()
            .map(|s| &s.identifier)
            .collect::<Vec<_>>()
    );

    // Verify stream metadata
    let stream1 = all_streams.iter().find(|s| s.identifier == "app/stream1");
    assert!(stream1.is_some(), "Should find stream1");
    let stream1 = stream1.unwrap();
    assert_eq!(stream1.replica_id, "replica1");
    assert_eq!(stream1.pub_type, "rtmp");
    assert_eq!(stream1.publisher_addr.as_deref(), Some("192.168.1.1:1234"));

    let stream2 = all_streams.iter().find(|s| s.identifier == "app/stream2");
    assert!(stream2.is_some(), "Should find stream2");
    let stream2 = stream2.unwrap();
    assert_eq!(stream2.replica_id, "replica2");
    assert_eq!(stream2.pub_type, "webrtc");
}

/// Test `cleanup_stale_active_entries` after key expiry.
///
/// The active set tracks stream identifiers, while metadata keys have TTLs.
/// When a metadata key expires (simulating a crashed node), the cleanup
/// should remove the stale entry from the active set.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_cleanup_stale_active_entries_after_expiry() {
    let (_container, conn) = setup_redis().await;

    let registry =
        StreamRegistry::new("replica1".to_string()).with_redis(conn.clone(), "cl4stale:");

    // Register a stream
    registry
        .register_stream("app/stale_stream", "rtmp", None)
        .await
        .unwrap();

    // Verify it's in the active set
    let mut test_conn = conn.clone();
    let active_members: Vec<String> = test_conn.smembers("cl4stale:streams:active").await.unwrap();
    assert!(
        active_members.contains(&"app/stale_stream".to_string()),
        "Stream should be in active set"
    );

    // Manually delete the metadata key to simulate expiry
    let _: () = redis::cmd("DEL")
        .arg("cl4stale:streams:meta:app/stale_stream")
        .query_async(&mut test_conn)
        .await
        .unwrap();

    // Verify metadata key is gone
    let exists: bool = redis::cmd("EXISTS")
        .arg("cl4stale:streams:meta:app/stale_stream")
        .query_async(&mut test_conn)
        .await
        .unwrap();
    assert!(!exists, "Metadata key should be gone");

    // The active set still has the entry (stale)
    let active_members: Vec<String> = test_conn.smembers("cl4stale:streams:active").await.unwrap();
    assert!(
        active_members.contains(&"app/stale_stream".to_string()),
        "Active set should still have the stale entry"
    );

    // Use a second registry (simulating a different replica) that has no local cache.
    // get_all_streams on this instance should skip the entry whose metadata key is gone.
    let registry2 =
        StreamRegistry::new("replica2".to_string()).with_redis(conn.clone(), "cl4stale:");
    let streams = registry2.get_all_streams().await;
    assert!(
        streams.is_empty() || !streams.iter().any(|s| s.identifier == "app/stale_stream"),
        "get_all_streams from a different replica should not return stream with expired metadata"
    );

    // To properly test cleanup, we register and immediately expire:
    // 1. Register another stream with very short TTL by manipulating Redis directly
    let _: () = test_conn
        .sadd("cl4stale:streams:active", "app/will_expire")
        .await
        .unwrap();
    // Don't create metadata key at all (simulating expired)

    // The cleanup_stale_active_entries is private, but spawn_active_set_cleanup_task
    // calls it. We can test by running the task briefly.
    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = registry.spawn_active_set_cleanup_task(Duration::from_millis(50), cancel.clone());

    // Wait for at least one cleanup cycle
    tokio::time::sleep(Duration::from_millis(200)).await;
    cancel.cancel();
    let _ = handle.await;

    // Verify the stale entry was cleaned up
    let active_members: Vec<String> = test_conn.smembers("cl4stale:streams:active").await.unwrap();
    assert!(
        !active_members.contains(&"app/will_expire".to_string()),
        "Stale entry 'app/will_expire' should be cleaned up from active set"
    );
}

/// Test `refresh_ttls` pipeline refreshes metadata TTLs.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_refresh_ttls_pipeline() {
    let (_container, conn) = setup_redis().await;

    let registry = StreamRegistry::new("replica1".to_string()).with_redis(conn.clone(), "cl4ttl:");

    // Register two streams
    registry
        .register_stream("app/s1", "rtmp", None)
        .await
        .unwrap();
    registry
        .register_stream("app/s2", "webrtc", None)
        .await
        .unwrap();

    // Verify metadata keys exist with TTL
    let mut test_conn = conn.clone();
    let ttl1: i64 = redis::cmd("TTL")
        .arg("cl4ttl:streams:meta:app/s1")
        .query_async(&mut test_conn)
        .await
        .unwrap();
    assert!(ttl1 > 0, "Metadata key should have TTL, got {ttl1}");

    // Set a very low TTL to simulate near-expiry
    let _: () = redis::cmd("EXPIRE")
        .arg("cl4ttl:streams:meta:app/s1")
        .arg(5)
        .query_async(&mut test_conn)
        .await
        .unwrap();

    let ttl_before: i64 = redis::cmd("TTL")
        .arg("cl4ttl:streams:meta:app/s1")
        .query_async(&mut test_conn)
        .await
        .unwrap();
    assert!(
        ttl_before <= 5,
        "TTL should be <= 5 before refresh, got {ttl_before}"
    );

    // Spawn the TTL refresh task and let it run one cycle
    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = registry.spawn_ttl_refresh_task(Duration::from_millis(50), cancel.clone());

    // Wait for at least one refresh cycle
    tokio::time::sleep(Duration::from_millis(200)).await;
    cancel.cancel();
    let _ = handle.await;

    // After refresh, TTL should be restored to the full 300 seconds
    let ttl_after: i64 = redis::cmd("TTL")
        .arg("cl4ttl:streams:meta:app/s1")
        .query_async(&mut test_conn)
        .await
        .unwrap();
    assert!(
        ttl_after > 5,
        "TTL should be refreshed to > 5 seconds, got {ttl_after}"
    );
}

/// Test `unregister_stream` removes from both local and Redis.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_unregister_stream_removes_from_redis() {
    let (_container, conn) = setup_redis().await;

    let registry =
        StreamRegistry::new("replica1".to_string()).with_redis(conn.clone(), "cl4unreg:");

    // Register a stream
    registry
        .register_stream("app/to_remove", "rtmp", None)
        .await
        .unwrap();

    // Verify in Redis
    let mut test_conn = conn.clone();
    let exists: bool = redis::cmd("EXISTS")
        .arg("cl4unreg:streams:meta:app/to_remove")
        .query_async(&mut test_conn)
        .await
        .unwrap();
    assert!(exists, "Metadata key should exist");

    // Unregister
    registry.unregister_stream("app/to_remove").await;

    // Verify removed from Redis
    let exists: bool = redis::cmd("EXISTS")
        .arg("cl4unreg:streams:meta:app/to_remove")
        .query_async(&mut test_conn)
        .await
        .unwrap();
    assert!(!exists, "Metadata key should be removed");

    let active_members: Vec<String> = test_conn.smembers("cl4unreg:streams:active").await.unwrap();
    assert!(
        !active_members.contains(&"app/to_remove".to_string()),
        "Should be removed from active set"
    );
}
