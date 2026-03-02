//! `RoomMessageHub` integration tests
//!
//! Tests for message routing, targeted broadcast, room removal, and
//! safe unsubscribe of unknown connections.

#![allow(clippy::unwrap_used)]
use synctv_cluster::sync::events::ClusterEvent;
use synctv_cluster::RoomMessageHub;
use synctv_core::models::id::{RoomId, UserId};

fn uid(s: &str) -> UserId {
    UserId::from_string(s.to_string())
}

fn rid(s: &str) -> RoomId {
    RoomId::from_string(s.to_string())
}

fn chat_event(room: &RoomId, user: &UserId) -> ClusterEvent {
    ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room.clone(),
        user_id: user.clone(),
        username: "tester".to_string(),
        message: "hello".to_string(),
        timestamp: chrono::Utc::now(),
        position: None,
        color: None,
    }
}

// ============================================================================
// Test 1: broadcast_to_connection delivers only to target
// ============================================================================

#[tokio::test]
async fn test_broadcast_to_connection_targeted() {
    let hub = RoomMessageHub::new();
    let room = rid("r1");
    let u1 = uid("u1");
    let u2 = uid("u2");

    let mut rx1 = hub
        .subscribe(room.clone(), u1.clone(), "c1".to_string())
        .await;
    let mut rx2 = hub
        .subscribe(room.clone(), u2.clone(), "c2".to_string())
        .await;

    let event = chat_event(&room, &u1);
    let sent = hub.broadcast_to_connection(&room, "c2", event);
    assert_eq!(sent, 1, "broadcast_to_connection should return 1");

    // c2 should receive
    let msg = tokio::time::timeout(std::time::Duration::from_millis(100), rx2.recv())
        .await
        .expect("c2 should receive")
        .expect("channel not closed");
    assert_eq!(msg.event_type(), "chat_message");

    // c1 should NOT receive
    let r = tokio::time::timeout(std::time::Duration::from_millis(100), rx1.recv()).await;
    assert!(
        r.is_err(),
        "c1 should not have received the targeted message"
    );
}

// ============================================================================
// Test 2: broadcast_to_user delivers to all connections of that user
// ============================================================================

#[tokio::test]
async fn test_broadcast_to_user_multi_connection() {
    let hub = RoomMessageHub::new();
    let room = rid("r1");
    let user = uid("u1");
    let other = uid("u2");

    // Same user with two connections
    let mut rx1 = hub
        .subscribe(room.clone(), user.clone(), "c1".to_string())
        .await;
    let mut rx2 = hub
        .subscribe(room.clone(), user.clone(), "c2".to_string())
        .await;
    let mut rx3 = hub
        .subscribe(room.clone(), other.clone(), "c3".to_string())
        .await;

    let event = chat_event(&room, &user);
    let sent = hub.broadcast_to_user(&room, &user, event);
    assert_eq!(sent, 2, "Both user connections should receive");

    // Both user connections receive
    let _m1 = tokio::time::timeout(std::time::Duration::from_millis(100), rx1.recv())
        .await
        .expect("c1 should receive")
        .expect("channel not closed");
    let _m2 = tokio::time::timeout(std::time::Duration::from_millis(100), rx2.recv())
        .await
        .expect("c2 should receive")
        .expect("channel not closed");

    // Other user does NOT receive
    let r = tokio::time::timeout(std::time::Duration::from_millis(100), rx3.recv()).await;
    assert!(
        r.is_err(),
        "other user should not receive the targeted message"
    );
}

// ============================================================================
// Test 3: remove_room cleans up all state
// ============================================================================

#[tokio::test]
async fn test_remove_room_cleans_connections() {
    let hub = RoomMessageHub::new();
    let room = rid("r1");
    let u1 = uid("u1");
    let u2 = uid("u2");

    let _rx1 = hub
        .subscribe(room.clone(), u1.clone(), "c1".to_string())
        .await;
    let _rx2 = hub
        .subscribe(room.clone(), u2.clone(), "c2".to_string())
        .await;

    assert_eq!(hub.subscriber_count(&room), 2);
    assert_eq!(hub.connection_count(), 2);

    hub.remove_room(&room);

    assert_eq!(
        hub.subscriber_count(&room),
        0,
        "Room should have 0 subscribers after removal"
    );
    assert_eq!(
        hub.connection_count(),
        0,
        "All connections should be cleaned up"
    );
    assert_eq!(hub.room_count(), 0, "Room should be removed");
}

// ============================================================================
// Test 4: unsubscribe unknown connection is safe
// ============================================================================

#[tokio::test]
async fn test_unsubscribe_unknown_safe() {
    let hub = RoomMessageHub::new();

    // Should not panic
    hub.unsubscribe("nonexistent_connection");

    assert_eq!(hub.connection_count(), 0);
    assert_eq!(hub.room_count(), 0);
}

// ============================================================================
// D2: Lifecycle events are emitted on subscribe/unsubscribe
// ============================================================================

/// D2 fix verification: lifecycle_tx.send() results are checked (not silently dropped).
/// This test verifies that lifecycle events are still delivered correctly when
/// receivers are active.
#[tokio::test]
async fn test_lifecycle_events_emitted_on_subscribe_unsubscribe() {
    use synctv_cluster::sync::room_hub::RoomLifecycleEvent;

    let hub = RoomMessageHub::new();
    let mut lifecycle_rx = hub.subscribe_lifecycle();

    let room = rid("lc_room");
    let user = uid("lc_user");

    // Subscribe should emit RoomActivated
    let _rx = hub
        .subscribe(room.clone(), user.clone(), "lc_conn".to_string())
        .await;

    let event = lifecycle_rx.try_recv().unwrap();
    match event {
        RoomLifecycleEvent::RoomActivated(r) => assert_eq!(r, room),
        other => panic!("Expected RoomActivated, got: {other:?}"),
    }

    // Unsubscribe the only subscriber should emit RoomDeactivated
    hub.unsubscribe("lc_conn");

    let event = lifecycle_rx.try_recv().unwrap();
    match event {
        RoomLifecycleEvent::RoomDeactivated(r) => assert_eq!(r, room),
        other => panic!("Expected RoomDeactivated, got: {other:?}"),
    }
}

/// D2: lifecycle events are not lost when multiple rooms are created quickly.
#[tokio::test]
async fn test_lifecycle_events_not_lost_under_room_churn() {
    use synctv_cluster::sync::room_hub::RoomLifecycleEvent;

    let hub = RoomMessageHub::new();
    let mut lifecycle_rx = hub.subscribe_lifecycle();

    // Rapidly create and destroy 10 rooms
    for i in 0..10 {
        let room = rid(&format!("churn_room_{i}"));
        let user = uid(&format!("churn_user_{i}"));
        let conn_id = format!("churn_conn_{i}");

        let _rx = hub
            .subscribe(room.clone(), user.clone(), conn_id.clone())
            .await;
        hub.unsubscribe(&conn_id);
    }

    // We should receive all 20 events (10 activated + 10 deactivated)
    let mut activated = 0;
    let mut deactivated = 0;
    while let Ok(event) = lifecycle_rx.try_recv() {
        match event {
            RoomLifecycleEvent::RoomActivated(_) => activated += 1,
            RoomLifecycleEvent::RoomDeactivated(_) => deactivated += 1,
        }
    }

    assert_eq!(
        activated, 10,
        "All 10 RoomActivated events should be received"
    );
    assert_eq!(
        deactivated, 10,
        "All 10 RoomDeactivated events should be received"
    );
}

// ============================================================================
// L9: Atomic unsubscribe prevents missed RoomActivated events
// ============================================================================

/// When the last subscriber unsubscribes and a new subscriber joins the same
/// room concurrently, the new subscriber must see a RoomActivated event.
/// Before the fix, a TOCTOU race between remove-subscriber and remove_if
/// could cause the new subscribe to see the room entry as Occupied (with 0
/// subscribers) and skip the RoomActivated event.
#[tokio::test]
async fn test_unsubscribe_last_then_subscribe_emits_activated() {
    use synctv_cluster::sync::room_hub::RoomLifecycleEvent;

    let hub = RoomMessageHub::new();
    let mut lifecycle_rx = hub.subscribe_lifecycle();

    let room = rid("race_room");
    let user1 = uid("user1");
    let user2 = uid("user2");

    // Subscribe first user -> RoomActivated
    let _rx1 = hub
        .subscribe(room.clone(), user1.clone(), "conn1".to_string())
        .await;

    let event = lifecycle_rx.try_recv().unwrap();
    assert!(matches!(event, RoomLifecycleEvent::RoomActivated(_)));

    // Unsubscribe first user -> RoomDeactivated
    hub.unsubscribe("conn1");

    let event = lifecycle_rx.try_recv().unwrap();
    assert!(matches!(event, RoomLifecycleEvent::RoomDeactivated(_)));

    // After the room is fully removed, subscribing a second user must
    // emit another RoomActivated (the room is re-created from scratch).
    let _rx2 = hub
        .subscribe(room.clone(), user2.clone(), "conn2".to_string())
        .await;

    let event = lifecycle_rx.try_recv().unwrap();
    match event {
        RoomLifecycleEvent::RoomActivated(r) => assert_eq!(r, room),
        other => panic!("Expected RoomActivated after re-subscribe, got: {other:?}"),
    }

    // Verify room has exactly 1 subscriber
    assert_eq!(hub.subscriber_count(&room), 1);
    assert_eq!(hub.room_count(), 1);
}

/// D2: remove_room emits a RoomDeactivated lifecycle event.
#[tokio::test]
async fn test_remove_room_emits_deactivated_event() {
    use synctv_cluster::sync::room_hub::RoomLifecycleEvent;

    let hub = RoomMessageHub::new();
    let mut lifecycle_rx = hub.subscribe_lifecycle();

    let room = rid("rm_room");
    let user = uid("rm_user");

    let _rx = hub
        .subscribe(room.clone(), user.clone(), "rm_conn".to_string())
        .await;

    // Consume the RoomActivated event
    let _ = lifecycle_rx.try_recv().unwrap();

    // Remove the room (simulates cross-replica deletion)
    hub.remove_room(&room);

    let event = lifecycle_rx.try_recv().unwrap();
    match event {
        RoomLifecycleEvent::RoomDeactivated(r) => assert_eq!(r, room),
        other => panic!("Expected RoomDeactivated on remove_room, got: {other:?}"),
    }
}
