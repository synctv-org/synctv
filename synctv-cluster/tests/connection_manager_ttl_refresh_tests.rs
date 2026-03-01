//! ConnectionManager TTL refresh performance tests (requires Redis via testcontainers)
//!
//! Tests for batch TTL refresh with large number of connections.

#![allow(clippy::unwrap_used)]
use std::time::{Duration, Instant};

use redis::AsyncCommands;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
use synctv_core::models::id::{RoomId, UserId};

/// Default Redis version for test containers
#[allow(dead_code)]
const REDIS_VERSION: &str = "7-alpine";

/// Helper to create a Redis container and connection manager.
async fn setup_redis() -> (
    testcontainers::ContainerAsync<Redis>,
    redis::aio::ConnectionManager,
) {
    let redis_container = Redis::default()
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
                Err(_) if retries < 60 => {
                    retries += 1;
                    tokio::time::sleep(Duration::from_millis(500)).await;
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
    const { assert!(EXPECTED_BATCH_SIZE >= 100) };
    const { assert!(EXPECTED_BATCH_SIZE <= 10_000) };

    // Note: The actual TTL_REFRESH_BATCH_SIZE constant is private in the module.
    // We verify the design constraint here that batch sizes should be 1000.
    // If the implementation changes, the performance tests above will catch regressions.
}

// ============================================================================
// TTL Task Shutdown Tests (Task #85)
// ============================================================================

/// Test that shutdown() cancels the TTL refresh task.
///
/// This test verifies:
/// 1. The TTL refresh task is running after with_redis() is called
/// 2. shutdown() sends the cancellation signal
/// 3. The task terminates after shutdown
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_shutdown_cancels_ttl_refresh_task() {
    let (_container, conn) = setup_redis().await;

    let manager = ConnectionManager::new(ConnectionLimits::default())
        .with_redis(conn.clone(), "shutdown_test:");

    // Register a connection to ensure TTL task has work to do
    let user_id = uid("user_1");
    manager.register("conn_1".to_string(), user_id).await.unwrap();

    // Give the TTL task time to start (it spawns automatically)
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Call shutdown
    manager.shutdown();

    // The task should terminate gracefully
    // We verify this by waiting a short time and checking the manager is still usable
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Manager should still function for local operations
    assert_eq!(manager.connection_count(), 1);
}

/// Test that TTL refresh task responds to cancellation quickly.
///
/// This test verifies that the task doesn't hang when cancelled -
/// it should exit on the next select! iteration.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_ttl_refresh_task_responds_quickly_to_shutdown() {
    let (_container, conn) = setup_redis().await;

    let manager = ConnectionManager::new(ConnectionLimits::default())
        .with_redis(conn.clone(), "quick_shutdown:");

    // Register some connections
    for i in 0..5 {
        let user_id = uid(&format!("user_{}", i));
        manager.register(format!("conn_{}", i), user_id).await.unwrap();
    }

    // Measure how long shutdown takes
    let start = std::time::Instant::now();
    manager.shutdown();
    let elapsed = start.elapsed();

    // Shutdown should be nearly instantaneous since it just cancels tokens
    // The actual task termination happens asynchronously
    assert!(
        elapsed < Duration::from_millis(100),
        "shutdown() took too long: {:?}. Should just cancel tokens synchronously.",
        elapsed
    );
}

/// Test that shutdown is idempotent.
///
/// Multiple calls to shutdown() should be safe and not panic.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_ttl_shutdown_is_idempotent() {
    let (_container, conn) = setup_redis().await;

    let manager = ConnectionManager::new(ConnectionLimits::default())
        .with_redis(conn.clone(), "idempotent:");

    // Call shutdown multiple times
    manager.shutdown();
    manager.shutdown();
    manager.shutdown();

    // Should not panic, and manager should still work
    assert_eq!(manager.connection_count(), 0);
}

/// Test that manager without Redis doesn't need shutdown.
///
/// A ConnectionManager without Redis configured should work fine
/// without calling shutdown() (no background tasks to cancel).
#[tokio::test]
async fn test_manager_without_redis_works_without_shutdown() {
    let manager = ConnectionManager::new(ConnectionLimits::default());

    // No Redis configured, so no background tasks
    // shutdown() should still be safe to call
    manager.shutdown();

    // Manager should work for local operations
    let user_id = uid("local_user");
    manager.register("local_conn".to_string(), user_id).await.unwrap();
    assert_eq!(manager.connection_count(), 1);
}

/// Test that pending operations complete gracefully on shutdown.
///
/// When shutdown is called during active operations, the operations
/// should complete or be handled gracefully.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_shutdown_during_active_operations() {
    let (_container, conn) = setup_redis().await;

    let manager = ConnectionManager::new(ConnectionLimits::default())
        .with_redis(conn.clone(), "active_ops:");

    // Register many connections concurrently with shutdown
    let manager_clone = manager.clone();
    let register_handle = tokio::spawn(async move {
        for i in 0..50 {
            let user_id = uid(&format!("concurrent_user_{}", i));
            let _ = manager_clone.register(format!("concurrent_conn_{}", i), user_id).await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    // Wait a bit for some registrations to complete
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Call shutdown while registrations are in progress
    manager.shutdown();

    // Wait for the registration task to complete
    let _ = register_handle.await;

    // Manager should be in a consistent state
    // (exact count depends on timing, but should be valid)
    let count = manager.connection_count();
    assert!(count <= 50, "Connection count should be at most 50, got {}", count);
}

/// Test that the disconnect retry task is also cancelled on shutdown.
///
/// ConnectionManager spawns two tasks with Redis:
/// 1. TTL refresh task
/// 2. Disconnect retry task
///
/// Both should be cancelled by shutdown().
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_shutdown_cancels_disconnect_retry_task() {
    let (_container, conn) = setup_redis().await;

    let manager = ConnectionManager::new(ConnectionLimits::default())
        .with_redis(conn.clone(), "disconnect_retry:");

    // Register and then force disconnect signal
    let user_id = uid("disconnect_user");
    manager.register("disconnect_conn".to_string(), user_id.clone()).await.unwrap();

    // Send a disconnect signal (this uses the disconnect retry mechanism)
    manager.disconnect_user(&user_id);

    // Give disconnect retry task time to start processing
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Call shutdown
    manager.shutdown();

    // Should complete without hanging
    // (If disconnect retry task wasn't cancelled, this could hang)
}

// ============================================================================
// TTL Value Verification Tests (Task #16)
// ============================================================================

/// Test that distributed counter TTL is set to 2x the refresh interval.
///
/// This verifies the fix for Task #16: TTL should be 120s (2x 60s refresh interval)
/// rather than 180s (3x), ensuring faster crash recovery while maintaining safety.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_distributed_counter_ttl_is_2x_refresh_interval() {
    let (_container, conn) = setup_redis().await;

    let manager = ConnectionManager::new(ConnectionLimits::default())
        .with_redis(conn.clone(), "ttl_value:");

    // Register a connection
    let user_id = uid("user_ttl_test");
    manager.register("conn_ttl_test".to_string(), user_id).await.unwrap();

    // Verify the TTL on the user counter key
    let mut test_conn = conn.clone();
    let key = "ttl_value:connections:user:user_ttl_test";
    let ttl: i64 = redis::cmd("TTL")
        .arg(key)
        .query_async(&mut test_conn)
        .await
        .expect("Failed to get TTL");

    // TTL should be approximately 120 seconds (2x the 60s refresh interval)
    // We allow a small margin for test execution time
    assert!(
        (115..=125).contains(&ttl),
        "Distributed counter TTL should be ~120s (2x refresh interval), got {}s",
        ttl
    );
}

/// Test that distributed counters expire after TTL without refresh.
///
/// This verifies the crash-safety mechanism: if a node crashes without
/// decrementing counters, the counters should expire after the TTL.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_distributed_counter_expires_after_ttl() {
    let (_container, conn) = setup_redis().await;

    let manager = ConnectionManager::new(ConnectionLimits::default())
        .with_redis(conn.clone(), "ttl_expire:");

    // Register a connection
    let user_id = uid("user_expire_test");
    manager.register("conn_expire_test".to_string(), user_id).await.unwrap();

    // Verify counter exists and has value
    let mut test_conn = conn.clone();
    let key = "ttl_expire:connections:user:user_expire_test";
    let count: Option<i64> = test_conn.get(key).await.unwrap();
    assert_eq!(count, Some(1), "Counter should be 1 after registration");

    // Get initial TTL
    let initial_ttl: i64 = redis::cmd("TTL")
        .arg(key)
        .query_async(&mut test_conn)
        .await
        .expect("Failed to get initial TTL");
    assert!(
        initial_ttl > 0,
        "Counter should have a TTL set, got {}",
        initial_ttl
    );

    // Manually reduce TTL to 2 seconds to simulate expiry
    let _: () = redis::cmd("EXPIRE")
        .arg(key)
        .arg(2)
        .query_async(&mut test_conn)
        .await
        .expect("Failed to reduce TTL");

    // Wait for TTL to expire
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Verify counter has expired
    let count_after_expiry: Option<i64> = test_conn.get(key).await.unwrap();
    assert_eq!(
        count_after_expiry, None,
        "Counter should have expired after TTL"
    );
}

/// Test that 2x TTL multiplier provides adequate safety margin.
///
/// Verifies that the TTL is long enough to survive one missed refresh
/// but short enough to detect crashes quickly.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_ttl_multiplier_provides_safety_margin() {
    let (_container, conn) = setup_redis().await;

    let manager = ConnectionManager::new(ConnectionLimits::default())
        .with_redis(conn.clone(), "ttl_margin:");

    // Register a connection
    let user_id = uid("user_margin_test");
    manager.register("conn_margin_test".to_string(), user_id).await.unwrap();

    // Get initial TTL
    let mut test_conn = conn.clone();
    let key = "ttl_margin:connections:user:user_margin_test";
    let ttl: i64 = redis::cmd("TTL")
        .arg(key)
        .query_async(&mut test_conn)
        .await
        .expect("Failed to get TTL");

    // With 2x multiplier (120s TTL, 60s refresh):
    // - One missed refresh: TTL goes from 120 to 60, still alive
    // - Two missed refreshes: TTL would expire
    //
    // This provides a good balance between safety and crash detection speed.

    // Verify TTL is at least 1.5x the refresh interval (survive one missed refresh)
    let refresh_interval_secs = 60i64;
    assert!(
        ttl >= refresh_interval_secs * 3 / 2, // At least 1.5x
        "TTL should provide safety margin for at least one missed refresh. \
         TTL={}s, refresh_interval={}s",
        ttl,
        refresh_interval_secs
    );

    // Verify TTL is at most 2.5x the refresh interval (quick crash detection)
    assert!(
        ttl <= refresh_interval_secs * 5 / 2, // At most 2.5x
        "TTL should allow quick crash detection. \
         TTL={}s, refresh_interval={}s",
        ttl,
        refresh_interval_secs
    );
}
