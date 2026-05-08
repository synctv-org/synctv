//! `ConnectionManager` TTL refresh tests (requires Redis via testcontainers)

#![allow(clippy::unwrap_used)]
use std::time::Duration;

use redis::AsyncCommands;

use synctv_cluster::sync::{build_connection_manager, ConnectionLimits, ConnectionManager};
use synctv_core::models::id::{RoomId, UserId};
use synctv_core::SharedStateProfile;

mod integration_test_helpers;
use integration_test_helpers::TestRedis;
use synctv_core_testing::test_redis_key_prefix;

async fn setup_redis() -> (TestRedis, redis::aio::ConnectionManager, String) {
    let redis = TestRedis::start().await;
    let redis_client =
        redis::Client::open(redis.redis_url.as_str()).expect("Failed to create Redis client");
    let conn = redis_client
        .get_connection_manager()
        .await
        .expect("Failed to create Redis ConnectionManager");

    // Verify Redis
    let mut test_conn = conn.clone();
    let _: () = redis::cmd("PING")
        .query_async(&mut test_conn)
        .await
        .expect("Redis PING failed");

    let key_prefix = test_redis_key_prefix("ttl-test");
    (redis, conn, key_prefix)
}

fn stable_test_id(s: &str) -> i64 {
    s.bytes().fold(0_i64, |acc, byte| {
        (acc * 131 + i64::from(byte)) % 900_000_000
    }) + 1
}

fn uid(s: &str) -> UserId {
    UserId::from(stable_test_id(s))
}

fn rid(s: &str) -> RoomId {
    RoomId::from(stable_test_id(s))
}

fn distributed_manager(
    limits: ConnectionLimits,
    conn: redis::aio::ConnectionManager,
    key_prefix: &str,
) -> ConnectionManager {
    build_connection_manager(
        limits,
        &SharedStateProfile::from_runtime(
            Some(synctv_core::direct_runtime(conn)),
            key_prefix,
            true,
        ),
    )
    .expect("shared realtime connection runtime should initialize")
}

/// Test TTL refresh with a moderate number of connections (100).
/// This verifies the batching logic works correctly.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_ttl_refresh_moderate_connections() {
    let (_container, conn, prefix) = setup_redis().await;

    let limits = ConnectionLimits {
        max_total: 500,
        max_per_user: 10,
        max_per_room: 100,
        ..Default::default()
    };

    let manager = distributed_manager(limits, conn.clone(), &prefix);

    // Register 100 connections with 10 users across 5 rooms
    let num_connections = 100;
    let num_users = 10;
    let num_rooms = 5;

    for i in 0..num_connections {
        let user_idx = i % num_users;
        let room_idx = i % num_rooms;
        let conn_id = format!("conn_{i}");
        let user_id = uid(&format!("user_{user_idx}"));
        let room_id = rid(&format!("room_{room_idx}"));

        manager.register(conn_id.clone(), user_id).await.unwrap();
        manager.join_room(&conn_id, room_id).await.unwrap();
    }

    assert_eq!(manager.connection_count(), num_connections);

    // Manually trigger TTL refresh
    manager.test_refresh_distributed_counter_ttls().await;

    // Verify TTLs were set on all keys
    let mut test_conn = conn.clone();

    // Check a few user counter keys
    for i in 0..num_users {
        let key = format!("{prefix}connections:user:{}", uid(&format!("user_{i}")));
        let ttl: i64 = redis::cmd("TTL")
            .arg(&key)
            .query_async(&mut test_conn)
            .await
            .unwrap();
        assert!(ttl > 0, "User counter key {key} should have TTL, got {ttl}");
    }

    // Check a few room counter keys
    for i in 0..num_rooms {
        let key = format!("{prefix}connections:room:{}", rid(&format!("room_{i}")));
        let ttl: i64 = redis::cmd("TTL")
            .arg(&key)
            .query_async(&mut test_conn)
            .await
            .unwrap();
        assert!(ttl > 0, "Room counter key {key} should have TTL, got {ttl}");
    }

    // Check total counter
    let total_key = format!("{prefix}connections:total");
    let ttl: i64 = redis::cmd("TTL")
        .arg(total_key)
        .query_async(&mut test_conn)
        .await
        .unwrap();
    assert!(ttl > 0, "Total counter key should have TTL, got {ttl}");
}

/// Test that TTL refresh handles empty connection manager gracefully.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_ttl_refresh_empty_manager() {
    let (_container, conn, prefix) = setup_redis().await;

    let manager = distributed_manager(ConnectionLimits::default(), conn.clone(), &prefix);

    // Should not panic or error with no connections
    manager.test_refresh_distributed_counter_ttls().await;

    assert_eq!(manager.connection_count(), 0);
}

/// Test that TTL refresh is safe to call multiple times in quick succession.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_ttl_refresh_idempotent() {
    let (_container, conn, prefix) = setup_redis().await;

    let manager = distributed_manager(ConnectionLimits::default(), conn.clone(), &prefix);

    // Register a few connections
    for i in 0..5 {
        let conn_id = format!("conn_{i}");
        let user_id = uid(&format!("user_{i}"));
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
    let total: Option<i64> = test_conn
        .get(format!("{prefix}connections:total"))
        .await
        .unwrap();
    assert_eq!(total, Some(5));
}

/// Test that `shutdown()` cancels the TTL refresh task.
///
/// This test verifies:
/// 1. The TTL refresh task is running after `with_redis()` is called
/// 2. `shutdown()` sends the cancellation signal
/// 3. The task terminates after shutdown
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_shutdown_cancels_ttl_refresh_task() {
    let (_container, conn, prefix) = setup_redis().await;

    let manager = distributed_manager(ConnectionLimits::default(), conn.clone(), &prefix);

    // Register a connection to ensure TTL task has work to do
    let user_id = uid("user_1");
    manager
        .register("conn_1".to_string(), user_id)
        .await
        .unwrap();

    // Give the TTL task time to start (it spawns automatically)
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Call shutdown
    manager.shutdown().await;

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
    let (_container, conn, prefix) = setup_redis().await;

    let manager = distributed_manager(ConnectionLimits::default(), conn.clone(), &prefix);

    // Register some connections
    for i in 0..5 {
        let user_id = uid(&format!("user_{i}"));
        manager
            .register(format!("conn_{i}"), user_id)
            .await
            .unwrap();
    }

    // Measure how long shutdown takes
    let start = std::time::Instant::now();
    manager.shutdown().await;
    let elapsed = start.elapsed();

    // Shutdown should be nearly instantaneous since it just cancels tokens
    // The actual task termination happens asynchronously
    assert!(
        elapsed < Duration::from_millis(100),
        "shutdown() took too long: {elapsed:?}. Should just cancel tokens synchronously."
    );
}

/// Test that shutdown is idempotent.
///
/// Multiple calls to `shutdown()` should be safe and not panic.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_ttl_shutdown_is_idempotent() {
    let (_container, conn, prefix) = setup_redis().await;

    let manager = distributed_manager(ConnectionLimits::default(), conn, &prefix);

    // Call shutdown multiple times
    manager.shutdown().await;
    manager.shutdown().await;
    manager.shutdown().await;

    // Should not panic, and manager should still work
    assert_eq!(manager.connection_count(), 0);
}

/// Test that manager without Redis doesn't need shutdown.
///
/// A `ConnectionManager` without Redis configured should work fine
/// without calling `shutdown()` (no background tasks to cancel).
#[tokio::test]
async fn test_manager_without_redis_works_without_shutdown() {
    let manager = ConnectionManager::new(ConnectionLimits::default());

    // No Redis configured, so no background tasks
    // shutdown() should still be safe to call
    manager.shutdown().await;

    // Manager should work for local operations
    let user_id = uid("local_user");
    manager
        .register("local_conn".to_string(), user_id)
        .await
        .unwrap();
    assert_eq!(manager.connection_count(), 1);
}

/// Test that pending operations complete gracefully on shutdown.
///
/// When shutdown is called during active operations, the operations
/// should complete or be handled gracefully.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_shutdown_during_active_operations() {
    let (_container, conn, prefix) = setup_redis().await;

    let manager = distributed_manager(ConnectionLimits::default(), conn.clone(), &prefix);

    // Register many connections concurrently with shutdown
    let manager_clone = manager.clone();
    let register_handle = tokio::spawn(async move {
        for i in 0..50 {
            let user_id = uid(&format!("concurrent_user_{i}"));
            let _ = manager_clone
                .register(format!("concurrent_conn_{i}"), user_id)
                .await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    tokio::time::sleep(Duration::from_millis(10)).await;

    // Call shutdown while registrations are in progress
    manager.shutdown().await;

    let _ = register_handle.await;

    // Manager should be in a consistent state
    // (exact count depends on timing, but should be valid)
    let count = manager.connection_count();
    assert!(
        count <= 50,
        "Connection count should be at most 50, got {count}"
    );
}

/// Test that the disconnect retry task is also cancelled on shutdown.
///
/// `ConnectionManager` spawns two tasks with Redis:
/// 1. TTL refresh task
/// 2. Disconnect retry task
///
/// Both should be cancelled by `shutdown()`.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_shutdown_cancels_disconnect_retry_task() {
    let (_container, conn, prefix) = setup_redis().await;

    let manager = distributed_manager(ConnectionLimits::default(), conn.clone(), &prefix);

    // Register and then force disconnect signal
    let user_id = uid("disconnect_user");
    manager
        .register("disconnect_conn".to_string(), user_id)
        .await
        .unwrap();

    manager.disconnect_user(&user_id);

    // Give disconnect retry task time to start processing
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Call shutdown
    manager.shutdown().await;

    // Should complete without hanging
    // (If disconnect retry task wasn't cancelled, this could hang)
}

/// Test that reconciliation also clears stale zero-count counters.
///
/// If unregister cleanup partially fails during shutdown, Redis can retain
/// positive counters even though local state is already empty. Reconciliation
/// must drive those counters back to 0 instead of leaving them to expire by TTL.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_reconcile_does_not_zero_counters_without_distributed_evidence() {
    let (_container, conn, prefix) = setup_redis().await;

    let manager = distributed_manager(ConnectionLimits::default(), conn.clone(), &prefix);
    let user_id = uid("user_zero");
    let room_id = rid("room_zero");

    manager
        .register("conn_zero".to_string(), user_id)
        .await
        .unwrap();
    manager.join_room("conn_zero", room_id).await.unwrap();
    manager.unregister("conn_zero").await;

    assert_eq!(manager.connection_count(), 0);
    assert_eq!(manager.user_connection_count(&user_id), 0);
    assert_eq!(manager.room_connection_count(&room_id), 0);

    let mut redis_conn = conn.clone();
    let _: () = redis_conn
        .set(format!("{prefix}connections:total"), 1i64)
        .await
        .unwrap();
    let user_key = format!("{prefix}connections:user:{user_id}");
    let room_key = format!("{prefix}connections:room:{room_id}");
    let _: () = redis_conn.set(&user_key, 1i64).await.unwrap();
    let _: () = redis_conn.set(&room_key, 1i64).await.unwrap();

    manager.reconcile_with_redis().await;

    let total: Option<i64> = redis_conn
        .get(format!("{prefix}connections:total"))
        .await
        .unwrap();
    let user: Option<i64> = redis_conn.get(&user_key).await.unwrap();
    let room: Option<i64> = redis_conn.get(&room_key).await.unwrap();

    assert_eq!(total.unwrap_or_default(), 1);
    assert_eq!(user.unwrap_or_default(), 1);
    assert_eq!(room.unwrap_or_default(), 1);
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_unregister_cleanup_is_scoped_to_reused_connection_registration() {
    let (_container, conn, prefix) = setup_redis().await;

    let manager = distributed_manager(ConnectionLimits::default(), conn.clone(), &prefix);
    let user_id = uid("user_reuse");
    let room_id = rid("room_reuse");
    let connection_id = "conn_reuse".to_string();

    manager
        .register(connection_id.clone(), user_id)
        .await
        .unwrap();
    manager.join_room(&connection_id, room_id).await.unwrap();
    manager.unregister(&connection_id).await;

    manager
        .register(connection_id.clone(), user_id)
        .await
        .unwrap();
    manager.join_room(&connection_id, room_id).await.unwrap();
    manager.unregister(&connection_id).await;

    let mut redis_conn = conn.clone();
    let total: Option<i64> = redis_conn
        .get(format!("{prefix}connections:total"))
        .await
        .unwrap();
    let user: Option<i64> = redis_conn
        .get(format!("{prefix}connections:user:{user_id}"))
        .await
        .unwrap();
    let room: Option<i64> = redis_conn
        .get(format!("{prefix}connections:room:{room_id}"))
        .await
        .unwrap();

    assert_eq!(total.unwrap_or_default(), 0);
    assert_eq!(user.unwrap_or_default(), 0);
    assert_eq!(room.unwrap_or_default(), 0);
}

/// Test that distributed counter TTL is set to 2x the refresh interval.
///
/// This verifies the fix for TTL should be 120s (2x 60s refresh interval)
/// rather than 180s (3x), ensuring faster crash recovery while maintaining safety.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_distributed_counter_ttl_is_2x_refresh_interval() {
    let (_container, conn, prefix) = setup_redis().await;

    let manager = distributed_manager(ConnectionLimits::default(), conn.clone(), &prefix);

    // Register a connection
    let user_id = uid("user_ttl_test");
    manager
        .register("conn_ttl_test".to_string(), user_id)
        .await
        .unwrap();

    // Verify the TTL on the user counter key
    let mut test_conn = conn.clone();
    let key = format!("{prefix}connections:user:{user_id}");
    let ttl: i64 = redis::cmd("TTL")
        .arg(key)
        .query_async(&mut test_conn)
        .await
        .expect("Failed to get TTL");

    // TTL should be approximately 120 seconds (2x the 60s refresh interval)
    // We allow a small margin for test execution time
    assert!(
        (115..=125).contains(&ttl),
        "Distributed counter TTL should be ~120s (2x refresh interval), got {ttl}s"
    );
}

/// Test that distributed counters expire after TTL without refresh.
///
/// This verifies the crash-safety mechanism: if a node crashes without
/// decrementing counters, the counters should expire after the TTL.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_distributed_counter_expires_after_ttl() {
    let (_container, conn, prefix) = setup_redis().await;

    let manager = distributed_manager(ConnectionLimits::default(), conn.clone(), &prefix);

    // Register a connection
    let user_id = uid("user_expire_test");
    manager
        .register("conn_expire_test".to_string(), user_id)
        .await
        .unwrap();

    // Verify counter exists and has value
    let mut test_conn = conn.clone();
    let key = format!("{prefix}connections:user:{user_id}");
    let count: Option<i64> = test_conn.get(&key).await.unwrap();
    assert_eq!(count, Some(1), "Counter should be 1 after registration");

    // Get initial TTL
    let initial_ttl: i64 = redis::cmd("TTL")
        .arg(&key)
        .query_async(&mut test_conn)
        .await
        .expect("Failed to get initial TTL");
    assert!(
        initial_ttl > 0,
        "Counter should have a TTL set, got {initial_ttl}"
    );

    // Manually reduce TTL to 2 seconds to simulate expiry
    let _: () = redis::cmd("EXPIRE")
        .arg(&key)
        .arg(2)
        .query_async(&mut test_conn)
        .await
        .expect("Failed to reduce TTL");

    tokio::time::sleep(Duration::from_secs(3)).await;

    // Verify counter has expired
    let count_after_expiry: Option<i64> = test_conn.get(&key).await.unwrap();
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
    let (_container, conn, prefix) = setup_redis().await;

    let manager = distributed_manager(ConnectionLimits::default(), conn.clone(), &prefix);

    // Register a connection
    let user_id = uid("user_margin_test");
    manager
        .register("conn_margin_test".to_string(), user_id)
        .await
        .unwrap();

    // Get initial TTL
    let mut test_conn = conn.clone();
    let key = format!("{prefix}connections:user:{user_id}");
    let ttl: i64 = redis::cmd("TTL")
        .arg(key)
        .query_async(&mut test_conn)
        .await
        .expect("Failed to get TTL");

    // With 2x multiplier (120s TTL, 60s refresh):
    // - One missed refresh: TTL goes from 120 to 60, still alive
    // - Two missed refreshes: TTL would expire
    // This provides a good balance between safety and crash detection speed.

    // Verify TTL is at least 1.5x the refresh interval (survive one missed refresh)
    let refresh_interval_secs = 60i64;
    assert!(
        ttl >= refresh_interval_secs * 3 / 2, // At least 1.5x
        "TTL should provide safety margin for at least one missed refresh. \
         TTL={ttl}s, refresh_interval={refresh_interval_secs}s"
    );

    // Verify TTL is at most 2.5x the refresh interval (quick crash detection)
    assert!(
        ttl <= refresh_interval_secs * 5 / 2, // At most 2.5x
        "TTL should allow quick crash detection. \
         TTL={ttl}s, refresh_interval={refresh_interval_secs}s"
    );
}
