//! WebSocket connection tests
//!
//! Tests for connection lifecycle, heartbeat, reconnection, and cleanup.
//! These tests verify the `ConnectionManager` behavior for WebSocket-like connections.

#![allow(clippy::unwrap_used)]
use std::time::Duration;

use synctv_cluster::sync::{ConnectionLimits, DisconnectSignal};
use synctv_cluster::ConnectionManager;
use synctv_core::models::id::{RoomId, UserId};

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

// Test 1: Connection lifecycle - register and unregister

#[tokio::test]
async fn test_connection_register_unregister() {
    let mgr = ConnectionManager::default();

    // Register a connection
    let result = mgr.register("conn1".to_string(), uid("user1")).await;
    assert!(result.is_ok(), "Registration should succeed");
    assert_eq!(mgr.connection_count(), 1);

    // Get connection info
    let info = mgr.get_connection("conn1");
    assert!(info.is_some());
    let info = info.unwrap();
    assert_eq!(info.connection_id, "conn1");
    assert_eq!(info.user_id, uid("user1"));
    assert!(info.room_id.is_none());

    // Unregister
    mgr.unregister("conn1").await;
    assert_eq!(mgr.connection_count(), 0);
    assert!(mgr.get_connection("conn1").is_none());
}

// Test 2: Connection rejoin - reconnect after disconnect

#[tokio::test]
async fn test_connection_reconnect_after_disconnect() {
    let mgr = ConnectionManager::default();
    let user = uid("user1");
    let room = rid("room1");

    // First connection
    mgr.register("conn1".to_string(), user).await.unwrap();
    mgr.join_room("conn1", room).await.unwrap();

    // Disconnect
    mgr.unregister("conn1").await;

    // Reconnect with new connection ID for same user
    mgr.register("conn2".to_string(), user).await.unwrap();
    mgr.join_room("conn2", room).await.unwrap();

    assert_eq!(mgr.room_connection_count(&room), 1);
    let conn = mgr.get_connection("conn2").unwrap();
    assert_eq!(conn.room_id.unwrap(), room);
}

// Test 3: Heartbeat timeout detection

#[tokio::test]
async fn test_heartbeat_timeout() {
    let limits = ConnectionLimits {
        idle_timeout: Duration::from_millis(50),
        max_duration: Duration::from_hours(1),
        ..Default::default()
    };
    let mgr = ConnectionManager::new(limits);

    mgr.register("conn1".to_string(), uid("user1"))
        .await
        .unwrap();

    // Immediately should not timeout
    let timeouts = mgr.check_timeouts();
    assert!(timeouts.is_empty(), "Should not timeout immediately");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let timeouts = mgr.check_timeouts();
    assert_eq!(timeouts.len(), 1, "Should timeout after idle period");
    assert_eq!(timeouts[0], "conn1");
}

// Test 4: Multi-connection management per user

#[tokio::test]
async fn test_user_with_multiple_connections() {
    let limits = ConnectionLimits {
        max_per_user: 3,
        ..Default::default()
    };
    let mgr = ConnectionManager::new(limits);
    let user = uid("user1");

    // Should allow up to max_per_user connections
    mgr.register("conn1".to_string(), user).await.unwrap();
    mgr.register("conn2".to_string(), user).await.unwrap();
    mgr.register("conn3".to_string(), user).await.unwrap();

    // Fourth connection should fail
    let result = mgr.register("conn4".to_string(), user).await;
    assert!(result.is_err(), "Should fail when exceeding max_per_user");
    let err = result.unwrap_err();
    assert!(
        err.contains("Too many connections") || err.contains("user"),
        "Error should indicate too many connections, got: {err}"
    );

    // Total count should be 3
    assert_eq!(mgr.connection_count(), 3);
}

// Test 5: Connection cleanup on room leave

#[tokio::test]
async fn test_room_leave_cleanup() {
    let mgr = ConnectionManager::default();
    let room = rid("room1");

    // Multiple users in same room
    mgr.register("conn1".to_string(), uid("user1"))
        .await
        .unwrap();
    mgr.register("conn2".to_string(), uid("user2"))
        .await
        .unwrap();
    mgr.register("conn3".to_string(), uid("user3"))
        .await
        .unwrap();

    mgr.join_room("conn1", room).await.unwrap();
    mgr.join_room("conn2", room).await.unwrap();
    mgr.join_room("conn3", room).await.unwrap();

    assert_eq!(mgr.room_connection_count(&room), 3);

    // One user leaves
    mgr.unregister("conn1").await;

    assert_eq!(mgr.room_connection_count(&room), 2);
    assert_eq!(mgr.connection_count(), 2);
}

// Test 6: Disconnect signal propagation

#[tokio::test]
async fn test_disconnect_signal_to_room() {
    let mgr = ConnectionManager::default();
    let room = rid("room1");
    let user1 = uid("user1");
    let user2 = uid("user2");

    mgr.register("conn1".to_string(), user1).await.unwrap();
    mgr.register("conn2".to_string(), user2).await.unwrap();
    mgr.join_room("conn1", room).await.unwrap();
    mgr.join_room("conn2", room).await.unwrap();

    // Subscribe to disconnect signals
    let mut rx = mgr.subscribe_disconnect();

    // Disconnect entire room
    mgr.disconnect_room(&room);

    // Should receive signal
    let signal = rx.recv().await.expect("Should receive disconnect signal");
    assert!(matches!(signal, DisconnectSignal::Room(ref r) if r == &room));
}

#[tokio::test]
async fn test_disconnect_signal_to_user() {
    let mgr = ConnectionManager::default();
    let user = uid("user1");

    mgr.register("conn1".to_string(), user).await.unwrap();

    let mut rx = mgr.subscribe_disconnect();

    mgr.disconnect_user(&user);

    let signal = rx.recv().await.expect("Should receive disconnect signal");
    assert!(matches!(signal, DisconnectSignal::User(ref u) if u == &user));
}

#[tokio::test]
async fn test_disconnect_signal_to_connection() {
    let mgr = ConnectionManager::default();

    mgr.register("conn1".to_string(), uid("user1"))
        .await
        .unwrap();

    let mut rx = mgr.subscribe_disconnect();

    mgr.disconnect_connection("conn1");

    let signal = rx.recv().await.expect("Should receive disconnect signal");
    assert!(matches!(signal, DisconnectSignal::Connection(ref id) if id == "conn1"));
}

// Test 7: Connection duration tracking

#[tokio::test]
async fn test_connection_duration_tracking() {
    let mgr = ConnectionManager::default();

    mgr.register("conn1".to_string(), uid("user1"))
        .await
        .unwrap();

    // Small delay
    tokio::time::sleep(Duration::from_millis(50)).await;

    let info = mgr.get_connection("conn1").expect("Should have connection");
    assert!(info.duration() >= Duration::from_millis(50));
    assert!(info.idle_duration() >= Duration::from_millis(50));
}

// Test 8: Max connection duration enforcement

#[tokio::test]
async fn test_max_connection_duration() {
    let limits = ConnectionLimits {
        max_duration: Duration::from_millis(100),
        idle_timeout: Duration::from_hours(1),
        ..Default::default()
    };
    let mgr = ConnectionManager::new(limits);

    mgr.register("conn1".to_string(), uid("user1"))
        .await
        .unwrap();

    // Before timeout
    let timeouts = mgr.check_timeouts();
    assert!(timeouts.is_empty());

    // After max_duration
    tokio::time::sleep(Duration::from_millis(150)).await;

    let timeouts = mgr.check_timeouts();
    assert_eq!(timeouts.len(), 1);
    assert_eq!(timeouts[0], "conn1");
}

// Test 9: Total connection limit

#[tokio::test]
async fn test_total_connection_limit() {
    let limits = ConnectionLimits {
        max_total: 3,
        ..Default::default()
    };
    let mgr = ConnectionManager::new(limits);

    // Fill up to limit
    mgr.register("conn1".to_string(), uid("user1"))
        .await
        .unwrap();
    mgr.register("conn2".to_string(), uid("user2"))
        .await
        .unwrap();
    mgr.register("conn3".to_string(), uid("user3"))
        .await
        .unwrap();

    // Fourth should fail
    let result = mgr.register("conn4".to_string(), uid("user4")).await;
    assert!(result.is_err(), "Should fail when exceeding max_total");

    // Unregister one
    mgr.unregister("conn1").await;

    // Now should succeed
    let result = mgr.register("conn5".to_string(), uid("user5")).await;
    assert!(result.is_ok(), "Should succeed after freeing slot");
}

// Test 10: Room connection limit

#[tokio::test]
async fn test_room_connection_limit() {
    let limits = ConnectionLimits {
        max_per_room: 3,
        ..Default::default()
    };
    let mgr = ConnectionManager::new(limits);
    let room = rid("room1");

    // Fill up room
    mgr.register("conn1".to_string(), uid("user1"))
        .await
        .unwrap();
    mgr.register("conn2".to_string(), uid("user2"))
        .await
        .unwrap();
    mgr.register("conn3".to_string(), uid("user3"))
        .await
        .unwrap();

    mgr.join_room("conn1", room).await.unwrap();
    mgr.join_room("conn2", room).await.unwrap();
    mgr.join_room("conn3", room).await.unwrap();

    // Fourth connection
    mgr.register("conn4".to_string(), uid("user4"))
        .await
        .unwrap();

    // Should fail to join room (but connection exists)
    let result = mgr.join_room("conn4", room).await;
    assert!(result.is_err(), "Should fail when room is full");
}

// Test 11: RTC state management

#[tokio::test]
async fn test_rtc_state_management() {
    let mgr = ConnectionManager::default();
    let room = rid("room1");
    let user = uid("user1");

    mgr.register("conn1".to_string(), user).await.unwrap();
    mgr.join_room("conn1", room).await.unwrap();

    // Initially not RTC joined
    let conn = mgr.get_connection("conn1").unwrap();
    assert!(!conn.rtc_joined);

    // Mark as RTC joined
    mgr.mark_rtc_joined(&room, &user, "conn1", true);

    let conn = mgr.get_connection("conn1").unwrap();
    assert!(conn.rtc_joined);

    // Get RTC connections
    let rtc_conns = mgr.get_rtc_connections(&room);
    assert_eq!(rtc_conns.len(), 1);
    assert_eq!(rtc_conns[0].connection_id, "conn1");

    // Unmark
    mgr.mark_rtc_joined(&room, &user, "conn1", false);

    let rtc_conns = mgr.get_rtc_connections(&room);
    assert!(rtc_conns.is_empty());
}

// Test 12: Activity tracking

#[tokio::test]
async fn test_activity_tracking() {
    let mgr = ConnectionManager::default();

    mgr.register("conn1".to_string(), uid("user1"))
        .await
        .unwrap();

    let info1 = mgr.get_connection("conn1").unwrap();
    let initial_msg_count = info1.message_count;

    // Record activity
    mgr.record_message("conn1");

    let info2 = mgr.get_connection("conn1").unwrap();
    assert!(info2.message_count > initial_msg_count);
    // idle_duration should be reset (less than before)
}

// Test 13: Duplicate register overwrites

/// Test that registering with the same connection ID is rejected.
///
/// Reusing a live `connection_id` would corrupt the in-memory connection indexes
/// and make targeted disconnects ambiguous, so production code must reject it.
#[tokio::test]
async fn test_duplicate_register_is_rejected_and_preserves_original_connection() {
    let mgr = ConnectionManager::default();

    mgr.register("conn1".to_string(), uid("user1"))
        .await
        .unwrap();
    assert_eq!(mgr.connection_count(), 1);

    // Same connection ID with different user should be rejected
    let result = mgr.register("conn1".to_string(), uid("user2")).await;
    assert!(result.is_err(), "Duplicate connection ID must be rejected");

    // Still only 1 connection
    assert_eq!(mgr.connection_count(), 1);

    // Original connection must remain intact
    let conn = mgr.get_connection("conn1").unwrap();
    assert_eq!(conn.user_id, uid("user1"));
}

// Test 14: Unregister non-existent is safe

#[tokio::test]
async fn test_unregister_non_existent_safe() {
    let mgr = ConnectionManager::default();

    // Should not panic
    mgr.unregister("non_existent").await;

    assert_eq!(mgr.connection_count(), 0);
}

// Test 15: User from room disconnect signal

/// Test that `disconnect_user_from_room` sends the correct signal.
/// Note: This method only sends a signal; it does not directly remove connections.
/// The actual disconnection is handled by the signal recipient.
#[tokio::test]
async fn test_disconnect_user_from_room_signal() {
    let mgr = ConnectionManager::default();
    let user = uid("user1");
    let room1 = rid("room1");
    let room2 = rid("room2");

    // User in two rooms with two connections
    mgr.register("conn1".to_string(), user).await.unwrap();
    mgr.register("conn2".to_string(), user).await.unwrap();
    mgr.join_room("conn1", room1).await.unwrap();
    mgr.join_room("conn2", room2).await.unwrap();

    let mut rx = mgr.subscribe_disconnect();

    // Disconnect user from room1 only - sends signal
    mgr.disconnect_user_from_room(&user, &room1);

    let signal = rx.recv().await.expect("Should receive signal");
    match signal {
        DisconnectSignal::UserFromRoom { user_id, room_id } => {
            assert_eq!(user_id, user);
            assert_eq!(room_id, room1);
        }
        _ => panic!("Expected UserFromRoom signal"),
    }

    // Note: The signal recipient would need to call unregister()
    // to actually remove the connections. The signal itself doesn't remove them.
    // So room counts remain unchanged until explicit unregister.
    assert_eq!(mgr.room_connection_count(&room1), 1);
    assert_eq!(mgr.room_connection_count(&room2), 1);
}
