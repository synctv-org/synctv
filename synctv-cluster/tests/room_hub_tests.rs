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
