//! Tests for single-node realtime message broadcasting.
//!
//! These tests verify that:
//! 1. `RealtimeManager` starts without a distributed transport
//! 2. Local messages are broadcast to subscribers
//! 3. Multiple subscribers receive the same room event
//! 4. Connection tracking works in standalone mode

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use synctv_core::models::{RoomId, RoomPermissionSet, UserId};
use synctv_realtime::sync::RealtimeEvent;
use synctv_realtime::sync::{
    ConnectionLimits, ConnectionManager, RealtimeConfig, RealtimeManager, RoomMessageHub,
};

fn local_realtime_config(node_id: &str) -> RealtimeConfig {
    RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: node_id.to_string(),
        dedup_window: Duration::from_mins(1),
        critical_channel_capacity: 100,
        publish_channel_capacity: 1000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 1000,
        event_handler: None,
        parent_cancel_token: None,
    }
}

#[tokio::test]
async fn test_realtime_manager_single_node_mode_works() {
    let manager = RealtimeManager::new(local_realtime_config("test_local_node"))
        .await
        .expect("RealtimeManager::new should succeed without Redis");

    let metrics = manager.metrics();
    assert!(
        !metrics.distributed_enabled,
        "Metrics should show Redis is not enabled in single-node mode"
    );
}

#[tokio::test]
async fn test_local_subscribe_and_broadcast() {
    let manager = RealtimeManager::new(local_realtime_config("test_local_broadcast"))
        .await
        .expect("RealtimeManager::new should succeed");

    let room_id = RoomId::expect_positive(10_000_009);
    let user_id = UserId::expect_positive(10_000_010);

    let (mut rx, conn_id) = manager
        .subscribe(room_id, user_id)
        .await
        .expect("subscribe should succeed");

    let event = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id,
        user_id,
        username: "test_user".to_string(),
        message: "Hello local!".to_string(),
        timestamp: Utc::now(),
        display_position: None,
        display_color: None,
    };

    let result = manager.broadcast(event.clone());
    assert_eq!(
        result.local_sent, 1,
        "Local broadcast should reach 1 subscriber"
    );
    assert!(
        !result.redis_sent,
        "Redis should not be used in single-node mode"
    );

    let received = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("Should receive message within timeout")
        .expect("Should have a message");

    match received {
        RealtimeEvent::ChatMessage { message, .. } => {
            assert_eq!(message, "Hello local!");
        }
        _ => panic!("Expected ChatMessage event"),
    }

    manager.unsubscribe(&conn_id);
}

#[tokio::test]
async fn test_multiple_subscribers_receive_broadcasts() {
    let manager = Arc::new(
        RealtimeManager::new(local_realtime_config("test_multi_subscribers"))
            .await
            .expect("RealtimeManager::new should succeed"),
    );

    let room_id = RoomId::expect_positive(10_000_011);

    let mut subscribers = Vec::new();
    for i in 0..3 {
        let user_id = UserId::expect_positive(10_000 + i);
        let (rx, conn_id) = manager
            .subscribe(room_id, user_id)
            .await
            .expect("subscribe should succeed");
        subscribers.push((rx, conn_id));
    }

    let event = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id,
        user_id: UserId::expect_positive(10_000_012),
        username: "broadcaster".to_string(),
        message: "Hello everyone!".to_string(),
        timestamp: Utc::now(),
        display_position: None,
        display_color: None,
    };

    let result = manager.broadcast(event);
    assert_eq!(
        result.local_sent, 3,
        "Local broadcast should reach all 3 subscribers"
    );

    for (i, (rx, _)) in subscribers.iter_mut().enumerate() {
        let received = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("Subscriber {i} should receive message within timeout"))
            .expect("Should have a message");

        match received {
            RealtimeEvent::ChatMessage { message, .. } => {
                assert_eq!(
                    message, "Hello everyone!",
                    "Subscriber {i} should receive correct message"
                );
            }
            _ => panic!("Subscriber {i} expected ChatMessage event"),
        }
    }

    for (_, conn_id) in subscribers {
        manager.unsubscribe(&conn_id);
    }
}

#[tokio::test]
async fn test_connection_manager_works_standalone() {
    let limits = ConnectionLimits::default();
    let conn_manager = ConnectionManager::new(limits);

    let user_id = UserId::expect_positive(10_000_010);
    let conn_id = format!("conn_{}", synctv_common::snanoid!(8));

    conn_manager
        .register(conn_id.clone(), user_id)
        .await
        .expect("Should register connection");

    assert_eq!(conn_manager.connection_count(), 1);

    conn_manager.unregister(&conn_id).await;
    assert_eq!(conn_manager.connection_count(), 0);
}

#[tokio::test]
async fn test_local_realtime_manager_supports_room_operations() {
    let manager = Arc::new(
        RealtimeManager::new(local_realtime_config("test_local_ops"))
            .await
            .expect("RealtimeManager::new should succeed"),
    );

    let room_id = RoomId::expect_positive(10_000_009);
    let user_id = UserId::expect_positive(10_000_010);

    let (mut rx, conn_id) = manager
        .subscribe(room_id, user_id)
        .await
        .expect("subscribe should succeed");

    let event = RealtimeEvent::UserJoined {
        event_id: synctv_common::snanoid!(16),
        room_id,
        user_id,
        username: "user1".to_string(),
        permissions: RoomPermissionSet(0),
        role: 2,
        added_permissions: RoomPermissionSet(0),
        removed_permissions: RoomPermissionSet(0),
        admin_added_permissions: RoomPermissionSet(0),
        admin_removed_permissions: RoomPermissionSet(0),
        joined_at: Utc::now(),
        timestamp: Utc::now(),
    };

    let result = manager.broadcast(event.clone());
    assert_eq!(result.local_sent, 1, "Should send to 1 subscriber");

    let received = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("Should receive event")
        .expect("Should have event");

    match received {
        RealtimeEvent::UserJoined { username, .. } => {
            assert_eq!(username, "user1");
        }
        _ => panic!("Expected UserJoined event"),
    }

    manager.unsubscribe(&conn_id);

    let result = manager.broadcast(event);
    assert_eq!(
        result.local_sent, 0,
        "Should send to 0 subscribers after unsubscribe"
    );
}
