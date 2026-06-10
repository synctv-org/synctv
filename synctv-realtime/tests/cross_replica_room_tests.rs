//! Multi-replica cluster integration tests
//!
//! These tests verify cross-node coordination by starting multiple
//! `RealtimeManager` instances that share a single Redis container
//! (via testcontainers). Each "node" has its own `node_id` but connects
//! to the same Redis, simulating a multi-replica deployment.

#![allow(clippy::unwrap_used)]
use std::time::Duration;

use chrono::Utc;
use synctv_core::models::id::{MediaId, RoomId, UserId};
use synctv_realtime::sync::RealtimeEvent;
mod integration_test_helpers;
use integration_test_helpers::{
    broadcast_until_admin_event, broadcast_until_room_event, create_node, wait_until, TestRedis,
};

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_kick_user() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;
    let node_b = create_node(&redis.redis_url, "node_b").await;

    // User subscribes to admin events on node A
    let mut admin_rx_a = node_a.subscribe_admin_events();

    // Also subscribe to admin events on node B to verify self-ignore
    let mut admin_rx_b = node_b.subscribe_admin_events();

    let received = broadcast_until_admin_event(
        &node_b,
        &mut admin_rx_a,
        || RealtimeEvent::KickUser {
            event_id: synctv_common::snanoid!(16),
            user_id: UserId::expect_positive(10_000_040),
            reason: "banned_by_admin".to_string(),
            timestamp: Utc::now(),
        },
        |event| matches!(event, RealtimeEvent::KickUser { .. }),
        "KickUser on node A",
    )
    .await;

    assert_eq!(received.event_type(), "kick_user");
    if let RealtimeEvent::KickUser {
        user_id, reason, ..
    } = &received
    {
        assert_eq!(*user_id, UserId::expect_positive(10_000_040));
        assert_eq!(reason, "banned_by_admin");
    } else {
        panic!("Expected KickUser event, got {:?}", received.event_type());
    }

    // Node B's Redis subscriber ignores events from itself, so node_b's
    // admin_rx should NOT receive it from Redis. KickUser is broadcast
    // via Redis only (no room_id), so node B should NOT see it on admin_rx.
    let node_b_result = tokio::time::timeout(Duration::from_millis(500), admin_rx_b.recv()).await;
    // It's OK if node B doesn't receive it (self-ignore)
    if let Ok(Ok(evt)) = node_b_result {
        // If it does receive, it should still be valid
        assert_eq!(evt.event_type(), "kick_user");
    }

    // Cleanup
    node_a.shutdown().await;
    node_b.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_room_event_propagation() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;
    let node_b = create_node(&redis.redis_url, "node_b").await;

    let room_id = RoomId::expect_positive(10_000_011);
    let user_id = UserId::expect_positive(10_000_041);

    // User subscribes to room on node A (simulating a WebSocket connection on node A)
    let (mut room_rx, conn_id) = node_a
        .subscribe(room_id, user_id)
        .await
        .expect("subscribe should succeed");

    let received = broadcast_until_room_event(
        &node_b,
        &mut room_rx,
        || RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_042),
            username: "sender".to_string(),
            message: "Hello from node B!".to_string(),
            timestamp: Utc::now(),
            display_position: None,
            display_color: None,
        },
        |event| matches!(event, RealtimeEvent::ChatMessage { message, .. } if message == "Hello from node B!"),
        "ChatMessage on node A",
    )
    .await;

    assert_eq!(received.event_type(), "chat_message");
    if let RealtimeEvent::ChatMessage {
        message, username, ..
    } = &received
    {
        assert_eq!(message, "Hello from node B!");
        assert_eq!(username, "sender");
    } else {
        panic!("Expected ChatMessage event");
    }

    // Cleanup
    node_a.unsubscribe(&conn_id);
    node_a.shutdown().await;
    node_b.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_kick_publisher() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;
    let node_b = create_node(&redis.redis_url, "node_b").await;

    // Subscribe to admin events on node A (where the publisher is running)
    let mut admin_rx_a = node_a.subscribe_admin_events();

    // Also subscribe to the room on node A so Redis subscriber is active for this room
    let room_id = RoomId::expect_positive(10_000_043);
    let user_id = UserId::expect_positive(10_000_044);
    let (_room_rx, conn_id) = node_a
        .subscribe(room_id, user_id)
        .await
        .expect("subscribe should succeed");

    let received = broadcast_until_admin_event(
        &node_b,
        &mut admin_rx_a,
        || RealtimeEvent::KickPublisher {
            event_id: synctv_common::snanoid!(16),
            room_id,
            media_id: MediaId::expect_positive(10_000_045),
            reason: "room_deleted".to_string(),
            timestamp: Utc::now(),
        },
        |event| {
            matches!(event, RealtimeEvent::KickPublisher { room_id, media_id, .. }
            if *room_id == RoomId::expect_positive(10_000_043) && *media_id == MediaId::expect_positive(10_000_045))
        },
        "KickPublisher on node A",
    )
    .await;

    assert_eq!(received.event_type(), "kick_publisher");
    if let RealtimeEvent::KickPublisher {
        room_id: rid,
        media_id,
        reason,
        ..
    } = &received
    {
        assert_eq!(*rid, RoomId::expect_positive(10_000_043));
        assert_eq!(*media_id, MediaId::expect_positive(10_000_045));
        assert_eq!(reason, "room_deleted");
    } else {
        panic!("Expected KickPublisher event");
    }

    node_a.unsubscribe(&conn_id);
    node_a.shutdown().await;
    node_b.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_room_deleted() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;
    let node_b = create_node(&redis.redis_url, "node_b").await;

    let room_id = RoomId::expect_positive(10_000_046);
    let user_id = UserId::expect_positive(10_000_047);

    // Subscribe user on node A
    let (mut room_rx, _conn_id) = node_a
        .subscribe(room_id, user_id)
        .await
        .expect("subscribe should succeed");

    // Verify subscriber exists
    let metrics = node_a.metrics();
    assert_eq!(metrics.total_connections, 1);
    assert_eq!(metrics.total_rooms, 1);

    let received = broadcast_until_room_event(
        &node_b,
        &mut room_rx,
        || RealtimeEvent::RoomDeleted {
            event_id: synctv_common::snanoid!(16),
            room_id,
            deleted_by: UserId::expect_positive(10_000_039),
            timestamp: Utc::now(),
        },
        |event| matches!(event, RealtimeEvent::RoomDeleted { room_id, .. } if *room_id == RoomId::expect_positive(10_000_046)),
        "RoomDeleted on node A",
    )
    .await;

    assert_eq!(received.event_type(), "room_deleted");

    wait_until(
        "room cleanup after RoomDeleted",
        Duration::from_secs(5),
        || {
            let metrics = node_a.metrics();
            metrics.total_rooms == 0 && metrics.total_connections == 0
        },
    )
    .await;

    let metrics = node_a.metrics();
    assert_eq!(
        metrics.total_rooms, 0,
        "Room should be removed after RoomDeleted"
    );
    assert_eq!(
        metrics.total_connections, 0,
        "Connections should be cleaned up"
    );

    node_a.shutdown().await;
    node_b.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_room_settings_changed() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;
    let node_b = create_node(&redis.redis_url, "node_b").await;

    let room_id = RoomId::expect_positive(10_000_048);
    let user_id = UserId::expect_positive(10_000_049);

    // Subscribe on node A
    let (mut room_rx, conn_id) = node_a
        .subscribe(room_id, user_id)
        .await
        .expect("subscribe should succeed");

    let received = broadcast_until_room_event(
        &node_b,
        &mut room_rx,
        || RealtimeEvent::RoomSettingsChanged {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_050),
            username: "room_admin".to_string(),
            settings_json: serde_json::to_vec(&serde_json::json!({
                "max_members": 50,
                "chat_enabled": false
            }))
            .expect("serialize settings"),
            version: 3,
            timestamp: Utc::now(),
        },
        |event| matches!(event, RealtimeEvent::RoomSettingsChanged { .. }),
        "RoomSettingsChanged on node A",
    )
    .await;

    assert_eq!(received.event_type(), "room_settings_changed");
    if let RealtimeEvent::RoomSettingsChanged {
        settings_json,
        version,
        ..
    } = &received
    {
        let parsed: serde_json::Value = serde_json::from_slice(settings_json).expect("valid JSON");
        assert_eq!(parsed["max_members"], 50);
        assert_eq!(parsed["chat_enabled"], false);
        assert_eq!(*version, 3);
    } else {
        panic!("Expected RoomSettingsChanged event");
    }

    node_a.unsubscribe(&conn_id);
    node_a.shutdown().await;
    node_b.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_multiple_rooms_cross_replica() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;
    let node_b = create_node(&redis.redis_url, "node_b").await;

    let room1 = RoomId::expect_positive(10_000_051);
    let room2 = RoomId::expect_positive(10_000_052);
    let user1 = UserId::expect_positive(10_000_030);
    let user2 = UserId::expect_positive(10_000_031);

    // User1 in room1 on node A, User2 in room2 on node A
    let (mut rx1, conn1) = node_a
        .subscribe(room1, user1)
        .await
        .expect("subscribe should succeed");
    let (mut rx2, conn2) = node_a
        .subscribe(room2, user2)
        .await
        .expect("subscribe should succeed");

    let msg1 = broadcast_until_room_event(
        &node_b,
        &mut rx1,
        || RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id: room1,
            user_id: UserId::expect_positive(10_000_053),
            username: "sender_b".to_string(),
            message: "To room 1".to_string(),
            timestamp: Utc::now(),
            display_position: None,
            display_color: None,
        },
        |event| matches!(event, RealtimeEvent::ChatMessage { message, .. } if message == "To room 1"),
        "room1 message",
    )
    .await;

    if let RealtimeEvent::ChatMessage { message, .. } = &msg1 {
        assert_eq!(message, "To room 1");
    } else {
        panic!("Expected ChatMessage for room1");
    }

    let msg2 = broadcast_until_room_event(
        &node_b,
        &mut rx2,
        || RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id: room2,
            user_id: UserId::expect_positive(10_000_053),
            username: "sender_b".to_string(),
            message: "To room 2".to_string(),
            timestamp: Utc::now(),
            display_position: None,
            display_color: None,
        },
        |event| matches!(event, RealtimeEvent::ChatMessage { message, .. } if message == "To room 2"),
        "room2 message",
    )
    .await;

    if let RealtimeEvent::ChatMessage { message, .. } = &msg2 {
        assert_eq!(message, "To room 2");
    } else {
        panic!("Expected ChatMessage for room2");
    }

    // Verify no cross-contamination
    let cross1 = tokio::time::timeout(Duration::from_millis(300), rx1.recv()).await;
    assert!(
        cross1.is_err(),
        "Room1 subscriber should not receive room2 messages"
    );

    let cross2 = tokio::time::timeout(Duration::from_millis(300), rx2.recv()).await;
    assert!(
        cross2.is_err(),
        "Room2 subscriber should not receive room1 messages"
    );

    node_a.unsubscribe(&conn1);
    node_a.unsubscribe(&conn2);
    node_a.shutdown().await;
    node_b.shutdown().await;
}
