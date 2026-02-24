//! ConnectionManager integration tests (no Redis required)
//!
//! Tests for connection lifecycle, room joins, limits, disconnect signals,
//! and RTC filtering.

use std::time::Duration;
use synctv_cluster::{ConnectionManager};
use synctv_core::models::id::{RoomId, UserId};

fn uid(s: &str) -> UserId {
    UserId::from_string(s.to_string())
}

fn rid(s: &str) -> RoomId {
    RoomId::from_string(s.to_string())
}

// ============================================================================
// Test: disconnect signal retry mechanism - signals queued when channel full
// ============================================================================

#[tokio::test]
async fn test_disconnect_signal_queued_when_no_receiver() {
    use synctv_cluster::sync::DisconnectSignal;

    let mgr = ConnectionManager::default();
    let user = uid("u1");

    mgr.register("c1".to_string(), user.clone()).await.unwrap();

    // Disconnect without subscribing first - signal should be handled gracefully
    // The signal won't be queued since there are no receivers (receiver_count == 0)
    mgr.disconnect_connection("c1");

    // Give the retry task a moment to process
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Check metrics - should have no pending signals (no receivers case)
    let metrics = mgr.disconnect_signal_metrics();
    assert_eq!(metrics.pending_count, 0, "No pending signals when no receivers");
}

#[tokio::test]
async fn test_disconnect_signal_metrics_initial_state() {
    let mgr = ConnectionManager::default();

    // Initial metrics should all be zero
    let metrics = mgr.disconnect_signal_metrics();
    assert_eq!(metrics.pending_count, 0);
    assert_eq!(metrics.dropped_count, 0);
    assert_eq!(metrics.retried_count, 0);
}

#[tokio::test]
async fn test_disconnect_user_from_room_signal() {
    use synctv_cluster::sync::DisconnectSignal;

    let mgr = ConnectionManager::default();
    let user = uid("u1");
    let room = rid("r1");

    mgr.register("c1".to_string(), user.clone()).await.unwrap();
    mgr.join_room("c1", room.clone()).await.unwrap();

    // Subscribe before sending signal
    let mut rx = mgr.subscribe_disconnect();

    // Disconnect user from room
    mgr.disconnect_user_from_room(&user, &room);

    let sig = rx.recv().await.unwrap();
    assert!(
        matches!(sig, DisconnectSignal::UserFromRoom { ref user_id, ref room_id }
            if user_id == &user && room_id == &room),
        "Expected UserFromRoom signal"
    );
}

#[tokio::test]
async fn test_disconnect_signal_reliability_under_load() {
    use synctv_cluster::sync::DisconnectSignal;

    let mgr = ConnectionManager::default();

    // Register multiple connections
    for i in 0..10 {
        let user = uid(&format!("u{}", i));
        mgr.register(format!("c{}", i), user.clone()).await.unwrap();
    }

    // Subscribe to receive signals
    let mut rx = mgr.subscribe_disconnect();

    // Send multiple disconnect signals rapidly
    for i in 0..10 {
        mgr.disconnect_connection(&format!("c{}", i));
    }

    // All signals should be received (broadcast channel should handle this)
    let mut received_count = 0;
    for _ in 0..10 {
        match rx.recv().await {
            Ok(_) => received_count += 1,
            Err(_) => break,
        }
    }

    assert_eq!(received_count, 10, "All disconnect signals should be received");
}

// ============================================================================
// Test 1: join_room is idempotent
// ============================================================================

#[tokio::test]
async fn test_join_room_idempotent() {
    let mgr = ConnectionManager::default();
    let user = uid("u1");
    let room = rid("r1");

    mgr.register("c1".to_string(), user.clone()).await.unwrap();
    mgr.join_room("c1", room.clone()).await.unwrap();
    mgr.join_room("c1", room.clone()).await.unwrap(); // second join to same room

    assert_eq!(
        mgr.room_connection_count(&room),
        1,
        "Joining the same room twice should not double-count"
    );
}

// ============================================================================
// Test 2: join_room moves between rooms
// ============================================================================

#[tokio::test]
async fn test_join_room_moves_between_rooms() {
    let mgr = ConnectionManager::default();
    let user = uid("u1");
    let r1 = rid("r1");
    let r2 = rid("r2");

    mgr.register("c1".to_string(), user.clone()).await.unwrap();

    mgr.join_room("c1", r1.clone()).await.unwrap();
    assert_eq!(mgr.room_connection_count(&r1), 1);

    mgr.join_room("c1", r2.clone()).await.unwrap();
    assert_eq!(
        mgr.room_connection_count(&r1),
        0,
        "Old room should have 0 connections after moving"
    );
    assert_eq!(
        mgr.room_connection_count(&r2),
        1,
        "New room should have 1 connection after moving"
    );

    let conn = mgr.get_connection("c1").unwrap();
    assert_eq!(conn.room_id.unwrap().as_str(), "r2");
}

// ============================================================================
// Test 3: max_duration timeout
// ============================================================================

#[tokio::test]
async fn test_max_duration_timeout() {
    use synctv_cluster::ConnectionManager;

    let limits = synctv_cluster::sync::ConnectionLimits {
        max_duration: Duration::from_millis(50),
        idle_timeout: Duration::from_secs(3600), // effectively disabled
        ..Default::default()
    };
    let mgr = ConnectionManager::new(limits);
    let user = uid("u1");

    mgr.register("c1".to_string(), user.clone()).await.unwrap();

    // Not yet expired
    let timeouts = mgr.check_timeouts();
    assert!(timeouts.is_empty(), "Connection should not time out immediately");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let timeouts = mgr.check_timeouts();
    assert_eq!(timeouts.len(), 1, "Connection should have timed out");
    assert_eq!(timeouts[0], "c1");
}

// ============================================================================
// Test 4: total connection limit
// ============================================================================

#[tokio::test]
async fn test_total_connection_limit() {
    let limits = synctv_cluster::sync::ConnectionLimits {
        max_total: 2,
        ..Default::default()
    };
    let mgr = ConnectionManager::new(limits);

    assert!(mgr.register("c1".to_string(), uid("u1")).await.is_ok());
    assert!(mgr.register("c2".to_string(), uid("u2")).await.is_ok());

    let result = mgr.register("c3".to_string(), uid("u3")).await;
    assert!(
        result.is_err(),
        "Third registration should fail when max_total=2"
    );
    assert!(result.unwrap_err().contains("capacity"));

    assert_eq!(mgr.connection_count(), 2);
}

// ============================================================================
// Test 5: disconnect signals
// ============================================================================

#[tokio::test]
async fn test_disconnect_signals() {
    use synctv_cluster::sync::DisconnectSignal;

    let mgr = ConnectionManager::default();
    let user = uid("u1");
    let room = rid("r1");

    mgr.register("c1".to_string(), user.clone()).await.unwrap();
    mgr.join_room("c1", room.clone()).await.unwrap();

    // Subscribe before sending signals
    let mut rx = mgr.subscribe_disconnect();

    // Connection disconnect
    mgr.disconnect_connection("c1");
    let sig = rx.recv().await.unwrap();
    assert!(
        matches!(sig, DisconnectSignal::Connection(ref id) if id == "c1"),
        "Expected Connection signal"
    );

    // User disconnect
    mgr.disconnect_user(&user);
    let sig = rx.recv().await.unwrap();
    assert!(
        matches!(sig, DisconnectSignal::User(ref id) if id == &user),
        "Expected User signal"
    );

    // Room disconnect
    mgr.disconnect_room(&room);
    let sig = rx.recv().await.unwrap();
    assert!(
        matches!(sig, DisconnectSignal::Room(ref id) if id == &room),
        "Expected Room signal"
    );
}

// ============================================================================
// Test 6: RTC connections filter
// ============================================================================

#[tokio::test]
async fn test_rtc_connections_filter() {
    let mgr = ConnectionManager::default();
    let u1 = uid("u1");
    let u2 = uid("u2");
    let room = rid("r1");

    mgr.register("c1".to_string(), u1.clone()).await.unwrap();
    mgr.register("c2".to_string(), u2.clone()).await.unwrap();
    mgr.join_room("c1", room.clone()).await.unwrap();
    mgr.join_room("c2", room.clone()).await.unwrap();

    // Mark only c1 as RTC-joined
    mgr.mark_rtc_joined(&room, &u1, "c1", true);

    let rtc = mgr.get_rtc_connections(&room);
    assert_eq!(rtc.len(), 1, "Only 1 connection should be RTC-joined");
    assert_eq!(rtc[0].connection_id, "c1");

    // Mark c1 as not RTC-joined
    mgr.mark_rtc_joined(&room, &u1, "c1", false);
    let rtc = mgr.get_rtc_connections(&room);
    assert_eq!(rtc.len(), 0, "No connections should be RTC-joined after unmark");
}

// ============================================================================
// Test 7: unregister nonexistent is a no-op
// ============================================================================

#[tokio::test]
async fn test_unregister_nonexistent_noop() {
    let mgr = ConnectionManager::default();

    // Should not panic or error
    mgr.unregister("does_not_exist").await;

    assert_eq!(mgr.connection_count(), 0);
}
