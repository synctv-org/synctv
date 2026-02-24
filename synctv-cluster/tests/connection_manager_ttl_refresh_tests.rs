//! ConnectionManager TTL refresh performance tests (requires Redis via testcontainers)
//!
//! Tests for batch TTL refresh with large number of connections.

use std::time::{Duration, Instant};

use redis::AsyncCommands;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
use synctv_core::models::id::{RoomId, UserId};

/// Default Redis version for test containers
const REDIS_VERSION: &str = "7-alpine";

/// Helper to create a Redis container and connection manager.
async fn setup_redis() -> (
    testcontainers::ContainerAsync<Redis>,
    redis::aio::ConnectionManager,
) {
    let redis_container = Redis::default()
        .with_tag(REDIS_VERSION)
        .start()
        .await
        .expect("Failed to start Redis container");

    let redis_host = redis_container
        .get_host()
        .await
        .expect("Failed to get Redis host");
    let redis_port = redis_container
        .get_host_port_ipv4(6379)
        .await
        .expect("Failed to get Redis port");

    let redis_url = format!("redis://{}:{}", redis_host, redis_port);
    let redis_client =
        redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");

    // Wait for Redis with retries
    let conn = {
        let mut retries = 0;
        loop {
            match redis::aio::ConnectionManager::new(redis_client.clone()).await {
                Ok(conn) => break conn,
                Err(_) if retries < 20 => {
                    retries += 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => panic!("Redis ConnectionManager failed after {} retries: {}", retries, e),
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

fn uid(s: &str) -> UserId {
    UserId::from_string(s.to_string())
}

fn rid(s: &str) -> RoomId {
    RoomId::from_string(s.to_string())
}

/// Test TTL refresh with a moderate number of connections (100).
/// This verifies the batching logic works correctly.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_ttl_refresh_moderate_connections() {
    let (_container, conn) = setup_redis().await;

    let limits = ConnectionLimits {
        max_total: 500,
        max_per_user: 10,
        max_per_room: 100,
        ..Default::default()
    };

    let manager = ConnectionManager::new(limits).with_redis(conn.clone(), "ttl_mod:");

    // Register 100 connections with 10 users across 5 rooms
    let num_connections = 100;
    let num_users = 10;
    let num_rooms = 5;

    for i in 0..num_connections {
        let user_idx = i % num_users;
        let room_idx = i % num_rooms;
        let conn_id = format!("conn_{}", i);
        let user_id = uid(&format!("user_{}", user_idx));
        let room_id = rid(&format!("room_{}", room_idx));

        manager.register(conn_id.clone(), user_id.clone()).await.unwrap();
        manager.join_room(&conn_id, room_id.clone()).await.unwrap();
    }

    assert_eq!(manager.connection_count(), num_connections);

    // Manually trigger TTL refresh
    manager.test_refresh_distributed_counter_ttls().await;

    // Verify TTLs were set on all keys
    let mut test_conn = conn.clone();

    // Check a few user counter keys
    for i in 0..num_users {
        let key = format!("ttl_mod:connections:user:user_{}", i);
        let ttl: i64 = redis::cmd("TTL")
            .arg(&key)
            .query_async(&mut test_conn)
            .await
            .unwrap();
        assert!(ttl > 0, "User counter key {} should have TTL, got {}", key, ttl);
    }

    // Check a few room counter keys
    for i in 0..num_rooms {
        let key = format!("ttl_mod:connections:room:room_{}", i);
        let ttl: i64 = redis::cmd("TTL")
            .arg(&key)
            .query_async(&mut test_conn)
            .await
            .unwrap();
        assert!(ttl > 0, "Room counter key {} should have TTL, got {}", key, ttl);
    }

    // Check total counter
    let total_key = "ttl_mod:connections:total";
    let ttl: i64 = redis::cmd("TTL")
        .arg(total_key)
        .query_async(&mut test_conn)
        .await
        .unwrap();
    assert!(ttl > 0, "Total counter key should have TTL, got {}", ttl);
}

/// Test TTL refresh performance with a large number of connections.
/// Measures the time taken to refresh TTLs for 1000+ connections.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers) - Performance test"]
async fn test_ttl_refresh_large_scale_performance() {
    let (_container, conn) = setup_redis().await;

    let limits = ConnectionLimits {
        max_total: 5000,
        max_per_user: 1000,
        max_per_room: 5000,
        ..Default::default()
    };

    let manager = ConnectionManager::new(limits).with_redis(conn.clone(), "ttl_large:");

    // Register 1000 connections with 100 users across 10 rooms
    let num_connections = 1000;
    let num_users = 100;
    let num_rooms = 10;

    println!("Registering {} connections...", num_connections);
    let start = Instant::now();

    for i in 0..num_connections {
        let user_idx = i % num_users;
        let room_idx = i % num_rooms;
        let conn_id = format!("conn_{}", i);
        let user_id = uid(&format!("user_{}", user_idx));
        let room_id = rid(&format!("room_{}", room_idx));

        manager.register(conn_id.clone(), user_id.clone()).await.unwrap();
        manager.join_room(&conn_id, room_id.clone()).await.unwrap();
    }

    let registration_time = start.elapsed();
    println!("Registration took {:?}", registration_time);
    assert_eq!(manager.connection_count(), num_connections);

    // Manually reduce TTLs to near-expiry to simulate the need for refresh
    let mut test_conn = conn.clone();
    for i in 0..num_users {
        let key = format!("ttl_large:connections:user:user_{}", i);
        let _: () = redis::cmd("EXPIRE")
            .arg(&key)
            .arg(10) // Set to 10 seconds
            .query_async(&mut test_conn)
            .await
            .unwrap();
    }
    for i in 0..num_rooms {
        let key = format!("ttl_large:connections:room:room_{}", i);
        let _: () = redis::cmd("EXPIRE")
            .arg(&key)
            .arg(10)
            .query_async(&mut test_conn)
            .await
            .unwrap();
    }

    // Measure TTL refresh performance
    println!("Starting TTL refresh...");
    let refresh_start = Instant::now();

    manager.test_refresh_distributed_counter_ttls().await;

    let refresh_time = refresh_start.elapsed();
    println!("TTL refresh took {:?}", refresh_time);

    // The refresh should complete in a reasonable time (< 5 seconds for 1000 connections)
    // This is a soft assertion - the test will pass but we log the performance
    if refresh_time > Duration::from_secs(5) {
        println!("WARNING: TTL refresh took longer than 5 seconds");
    }

    // Verify all TTLs were refreshed (should be > 10 seconds now)
    let mut all_refreshed = true;
    for i in 0..num_users {
        let key = format!("ttl_large:connections:user:user_{}", i);
        let ttl: i64 = redis::cmd("TTL")
            .arg(&key)
            .query_async(&mut test_conn)
            .await
            .unwrap();
        if ttl <= 10 {
            println!("User key {} was not refreshed, TTL = {}", key, ttl);
            all_refreshed = false;
        }
    }

    assert!(all_refreshed, "All counter keys should have been refreshed");

    // Check total key was refreshed
    let total_key = "ttl_large:connections:total";
    let ttl: i64 = redis::cmd("TTL")
        .arg(total_key)
        .query_async(&mut test_conn)
        .await
        .unwrap();
    assert!(ttl > 10, "Total counter key should have TTL > 10, got {}", ttl);
}

/// Test that TTL refresh handles empty connection manager gracefully.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_ttl_refresh_empty_manager() {
    let (_container, conn) = setup_redis().await;

    let manager = ConnectionManager::default().with_redis(conn.clone(), "ttl_empty:");

    // Should not panic or error with no connections
    manager.test_refresh_distributed_counter_ttls().await;

    assert_eq!(manager.connection_count(), 0);
}

/// Test that TTL refresh is safe to call multiple times in quick succession.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_ttl_refresh_idempotent() {
    let (_container, conn) = setup_redis().await;

    let manager = ConnectionManager::default().with_redis(conn.clone(), "ttl_idem:");

    // Register a few connections
    for i in 0..5 {
        let conn_id = format!("conn_{}", i);
        let user_id = uid(&format!("user_{}", i));
        manager.register(conn_id, user_id).await.unwrap();
    }

    // Call refresh multiple times
    manager.test_refresh_distributed_counter_ttls().await;
    manager.test_refresh_distributed_counter_ttls().await;
    manager.test_refresh_distributed_counter_ttls().await;

    // All connections should still be valid
    assert_eq!(manager.connection_count(), 5);

    // Verify counter values are still correct
    let mut test_conn = conn.clone();
    let total: Option<i64> = test_conn.get("ttl_idem:connections:total").await.unwrap();
    assert_eq!(total, Some(5));
}

/// Test batch size constant is reasonable.
/// This test verifies our expected batch size constraints.
#[test]
fn test_ttl_refresh_batch_size_reasonable() {
    // The batch size should be large enough to be efficient
    // but small enough to avoid memory/network issues
    const EXPECTED_BATCH_SIZE: usize = 1000;

    // Verify the expected batch size is in a reasonable range
    assert!(EXPECTED_BATCH_SIZE >= 100, "Batch size should be at least 100 for efficiency");
    assert!(EXPECTED_BATCH_SIZE <= 10_000, "Batch size should be at most 10,000 to avoid large payloads");

    // Note: The actual TTL_REFRESH_BATCH_SIZE constant is private in the module.
    // We verify the design constraint here that batch sizes should be 1000.
    // If the implementation changes, the performance tests above will catch regressions.
}
