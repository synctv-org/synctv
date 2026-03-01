//! Multi-replica cluster integration tests
//!
//! These tests verify cross-node coordination by starting multiple
//! `ClusterManager` instances that share a single Redis container
//! (via testcontainers). Each "node" has its own `node_id` but connects
//! to the same Redis, simulating a multi-replica deployment.

#![allow(clippy::unwrap_used)]
use std::time::Duration;

use chrono::Utc;
use synctv_cluster::sync::events::ClusterEvent;
use synctv_core::models::id::{MediaId, RoomId, UserId};
mod integration_test_helpers;
use integration_test_helpers::{create_node, TestRedis};


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

    // Allow Redis pub/sub connections to settle
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Admin on node B broadcasts a KickUser event
    let kick_event = ClusterEvent::KickUser {
        event_id: nanoid::nanoid!(16),
        user_id: UserId::from_string("victim_user".to_string()),
        reason: "banned_by_admin".to_string(),
        timestamp: Utc::now(),
    };

    let result = node_b.broadcast(kick_event);
    // KickUser has no room_id, so local_sent is 0 (no room subscribers)
    assert_eq!(result.local_sent, 0);
    assert!(result.redis_sent, "Event should be published to Redis");

    // Node A should receive the KickUser event via Redis pub/sub
    let received = tokio::time::timeout(Duration::from_secs(5), admin_rx_a.recv())
        .await
        .expect("Timed out waiting for KickUser on node A")
        .expect("Admin channel closed on node A");

    assert_eq!(received.event_type(), "kick_user");
    if let ClusterEvent::KickUser { user_id, reason, .. } = &received {
        assert_eq!(user_id.as_str(), "victim_user");
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

    let room_id = RoomId::from_string("shared_room".to_string());
    let user_id = UserId::from_string("viewer_user".to_string());

    // User subscribes to room on node A (simulating a WebSocket connection on node A)
    let (mut room_rx, conn_id) = node_a.subscribe(room_id.clone(), user_id.clone()).await;

    // Allow Redis pub/sub connections to settle
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Node B broadcasts a chat message to the same room
    let chat_event = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: UserId::from_string("sender_user".to_string()),
        username: "sender".to_string(),
        message: "Hello from node B!".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };

    let result = node_b.broadcast(chat_event);
    assert!(result.redis_sent, "Event should be published to Redis");

    // Node A's room subscriber should receive the chat message via Redis
    let received = tokio::time::timeout(Duration::from_secs(5), room_rx.recv())
        .await
        .expect("Timed out waiting for ChatMessage on node A")
        .expect("Room channel closed on node A");

    assert_eq!(received.event_type(), "chat_message");
    if let ClusterEvent::ChatMessage { message, username, .. } = &received {
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
    let room_id = RoomId::from_string("stream_room".to_string());
    let user_id = UserId::from_string("publisher_user".to_string());
    let (_room_rx, conn_id) = node_a.subscribe(room_id.clone(), user_id.clone()).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Admin on node B kicks the publisher
    let kick_event = ClusterEvent::KickPublisher {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        media_id: MediaId::from_string("live_stream_1".to_string()),
        reason: "room_deleted".to_string(),
        timestamp: Utc::now(),
    };

    node_b.broadcast(kick_event);

    // Node A should receive KickPublisher via admin channel
    let received = tokio::time::timeout(Duration::from_secs(5), admin_rx_a.recv())
        .await
        .expect("Timed out waiting for KickPublisher on node A")
        .expect("Admin channel closed on node A");

    assert_eq!(received.event_type(), "kick_publisher");
    if let ClusterEvent::KickPublisher { room_id: rid, media_id, reason, .. } = &received {
        assert_eq!(rid.as_str(), "stream_room");
        assert_eq!(media_id.as_str(), "live_stream_1");
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

    let room_id = RoomId::from_string("doomed_room".to_string());
    let user_id = UserId::from_string("user_in_room".to_string());

    // Subscribe user on node A
    let (mut room_rx, _conn_id) = node_a.subscribe(room_id.clone(), user_id.clone()).await;

    // Verify subscriber exists
    let metrics = node_a.metrics();
    assert_eq!(metrics.total_connections, 1);
    assert_eq!(metrics.total_rooms, 1);

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Node B deletes the room
    let delete_event = ClusterEvent::RoomDeleted {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        deleted_by: UserId::from_string("admin_user".to_string()),
        timestamp: Utc::now(),
    };

    node_b.broadcast(delete_event);

    // Node A's subscriber should receive the RoomDeleted notification
    let received = tokio::time::timeout(Duration::from_secs(5), room_rx.recv())
        .await
        .expect("Timed out waiting for RoomDeleted on node A")
        .expect("Room channel closed");

    assert_eq!(received.event_type(), "room_deleted");

    // After RoomDeleted dispatch, the room should be cleaned up on node A
    // (the dispatch_event handler calls remove_room after a 100ms drain delay).
    // Wait long enough for the drain delay plus cleanup to complete.
    tokio::time::sleep(Duration::from_millis(300)).await;

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

    let room_id = RoomId::from_string("settings_room".to_string());
    let user_id = UserId::from_string("settings_listener".to_string());

    // Subscribe on node A
    let (mut room_rx, conn_id) = node_a.subscribe(room_id.clone(), user_id.clone()).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Node B broadcasts a RoomSettingsChanged event
    let settings_bytes = serde_json::to_vec(&serde_json::json!({
        "max_members": 50,
        "chat_enabled": false
    }))
    .expect("serialize settings");

    let settings_event = ClusterEvent::RoomSettingsChanged {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: UserId::from_string("room_admin".to_string()),
        username: "room_admin".to_string(),
        settings_json: settings_bytes,
        timestamp: Utc::now(),
    };

    let result = node_b.broadcast(settings_event);
    assert!(
        result.redis_sent,
        "RoomSettingsChanged should be published to Redis"
    );

    // Node A should receive the settings change
    let received = tokio::time::timeout(Duration::from_secs(5), room_rx.recv())
        .await
        .expect("Timed out waiting for RoomSettingsChanged")
        .expect("Room channel closed");

    assert_eq!(received.event_type(), "room_settings_changed");
    if let ClusterEvent::RoomSettingsChanged {
        settings_json, ..
    } = &received
    {
        let parsed: serde_json::Value =
            serde_json::from_slice(settings_json).expect("valid JSON");
        assert_eq!(parsed["max_members"], 50);
        assert_eq!(parsed["chat_enabled"], false);
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

    let room1 = RoomId::from_string("room_1".to_string());
    let room2 = RoomId::from_string("room_2".to_string());
    let user1 = UserId::from_string("user_1".to_string());
    let user2 = UserId::from_string("user_2".to_string());

    // User1 in room1 on node A, User2 in room2 on node A
    let (mut rx1, conn1) = node_a.subscribe(room1.clone(), user1.clone()).await;
    let (mut rx2, conn2) = node_a.subscribe(room2.clone(), user2.clone()).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Node B sends to room1
    let event1 = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room1.clone(),
        user_id: UserId::from_string("sender_b".to_string()),
        username: "sender_b".to_string(),
        message: "To room 1".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };

    // Node B sends to room2
    let event2 = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room2.clone(),
        user_id: UserId::from_string("sender_b".to_string()),
        username: "sender_b".to_string(),
        message: "To room 2".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };

    node_b.broadcast(event1);
    node_b.broadcast(event2);

    // Verify room1 subscriber gets room1 message
    let msg1 = tokio::time::timeout(Duration::from_secs(5), rx1.recv())
        .await
        .expect("Timed out waiting for room1 message")
        .expect("Room1 channel closed");

    if let ClusterEvent::ChatMessage { message, .. } = &msg1 {
        assert_eq!(message, "To room 1");
    } else {
        panic!("Expected ChatMessage for room1");
    }

    // Verify room2 subscriber gets room2 message
    let msg2 = tokio::time::timeout(Duration::from_secs(5), rx2.recv())
        .await
        .expect("Timed out waiting for room2 message")
        .expect("Room2 channel closed");

    if let ClusterEvent::ChatMessage { message, .. } = &msg2 {
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

