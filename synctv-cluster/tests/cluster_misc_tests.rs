//! Multi-replica cluster integration tests
//!
//! These tests verify cross-node coordination by starting multiple
//! `ClusterManager` instances that share a single Redis container
//! (via testcontainers). Each "node" has its own `node_id` but connects
//! to the same Redis, simulating a multi-replica deployment.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use synctv_cluster::sync::events::{CacheTarget, ClusterEvent};
use synctv_cluster::{ClusterConfig, ClusterManager, MessageDeduplicator, RoomMessageHub};
use synctv_cluster::sync::redis_pubsub::RedisPubSub;
use synctv_core::cache::{CacheInvalidationService, InvalidationMessage};
use synctv_core::models::id::{MediaId, RoomId, UserId};
use testcontainers::ContainerAsync;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;
use tokio::sync::broadcast;

mod integration_test_helpers;
use integration_test_helpers::{create_node, TestRedis};


#[tokio::test]
#[ignore = "requires Docker"]
async fn test_event_propagation_latency() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;
    let node_b = create_node(&redis.redis_url, "node_b").await;

    let room_id = RoomId::from_string("latency_room".to_string());
    let user_id = UserId::from_string("listener".to_string());

    let (mut room_rx, conn_id) = node_a.subscribe(room_id.clone(), user_id.clone()).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let send_time = std::time::Instant::now();

    let event = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: UserId::from_string("sender".to_string()),
        username: "sender".to_string(),
        message: "Latency test".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };

    node_b.broadcast(event);

    let received = tokio::time::timeout(Duration::from_secs(5), room_rx.recv())
        .await
        .expect("Timed out waiting for latency message")
        .expect("Channel closed");

    let latency = send_time.elapsed();

    assert_eq!(received.event_type(), "chat_message");
    assert!(
        latency < Duration::from_millis(100),
        "Event propagation latency ({:?}) exceeds 100ms threshold",
        latency
    );

    node_a.unsubscribe(&conn_id);
    node_a.shutdown().await;
    node_b.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_critical_events_high_priority() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;
    let node_b = create_node(&redis.redis_url, "node_b").await;

    let room_id = RoomId::from_string("critical_room".to_string());
    let user_id = UserId::from_string("listener".to_string());

    let (mut room_rx, conn_id) = node_a.subscribe(room_id.clone(), user_id.clone()).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Send a critical event (PermissionChanged is marked as critical)
    let critical_event = ClusterEvent::PermissionChanged {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        target_user_id: user_id.clone(),
        target_username: "listener".to_string(),
        changed_by: UserId::from_string("admin".to_string()),
        changed_by_username: "admin".to_string(),
        new_permissions: synctv_core::models::PermissionBits(
            synctv_core::models::PermissionBits::DEFAULT_MEMBER,
        ),
        role: 2, // Member role
        added_permissions: synctv_core::models::PermissionBits::empty(),
        removed_permissions: synctv_core::models::PermissionBits::empty(),
        timestamp: Utc::now(),
    };

    assert!(
        critical_event.is_critical(),
        "PermissionChanged should be critical"
    );

    let result = node_b.broadcast(critical_event);
    assert!(
        result.redis_sent,
        "Critical event should be published to Redis"
    );

    // Should be received on node A
    let received = tokio::time::timeout(Duration::from_secs(5), room_rx.recv())
        .await
        .expect("Timed out waiting for critical event")
        .expect("Channel closed");

    assert_eq!(received.event_type(), "permission_changed");

    node_a.unsubscribe(&conn_id);
    node_a.shutdown().await;
    node_b.shutdown().await;
}

