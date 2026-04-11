//! Cluster messaging tests
//!
//! Tests for cross-node message broadcasting, deduplication, and Redis fallback.
//! Uses testcontainers for Redis integration tests.

#![allow(clippy::unwrap_used)]
use std::time::Duration;

use synctv_cluster::sync::events::{CacheTarget, ClusterEvent, NotificationLevel};
use synctv_cluster::{ClusterConfig, ClusterManager, DedupKey, MessageDeduplicator};
use synctv_core::models::id::{MediaId, RoomId, UserId};
use synctv_core_testing::{start_redis_with_client, RedisContainer};
mod integration_test_helpers;
use integration_test_helpers::{broadcast_until_admin_event, broadcast_until_room_event};

fn uid(s: &str) -> UserId {
    UserId::from_string(s.to_string())
}

fn rid(s: &str) -> RoomId {
    RoomId::from_string(s.to_string())
}

fn mid(s: &str) -> MediaId {
    MediaId::from_string(s.to_string())
}

/// Helper to create a Redis container and connection manager.
async fn setup_redis() -> (RedisContainer, redis::Client, redis::aio::ConnectionManager) {
    let (redis_container, redis_client) = start_redis_with_client().await;
    let conn = redis::aio::ConnectionManager::new(redis_client.clone())
        .await
        .expect("Failed to create Redis ConnectionManager");

    // Verify Redis
    let mut test_conn = conn.clone();
    let _: () = redis::cmd("PING")
        .query_async(&mut test_conn)
        .await
        .expect("Redis PING failed");

    (redis_container, redis_client, conn)
}

/// Create a test cluster config with Redis connection.
fn make_cluster_config(
    redis_client: redis::Client,
    redis_conn: redis::aio::ConnectionManager,
    node_id: &str,
) -> ClusterConfig {
    make_cluster_config_with_prefix(
        redis_client,
        redis_conn,
        node_id,
        format!("test_{}:", synctv_common::snanoid!(8)),
    )
}

fn make_cluster_config_with_prefix(
    redis_client: redis::Client,
    redis_conn: redis::aio::ConnectionManager,
    node_id: &str,
    key_prefix: String,
) -> ClusterConfig {
    ClusterConfig {
        redis_client: Some(redis_client),
        redis_conn: Some(redis_conn),
        shared_redis_conn: None,
        cluster_enabled: true,
        node_id: node_id.to_string(),
        dedup_window: Duration::from_mins(1),
        critical_channel_capacity: 100,
        publish_channel_capacity: 1000,
        key_prefix,
        catchup_window_secs: 300,
        stream_max_length: 1000,
        parent_cancel_token: None,
    }
}

// ============================================================================
// Test 1: Cross-node message broadcast via Redis Pub/Sub
// ============================================================================

/// Test that a message published from one node is received by another node.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_cross_node_broadcast() {
    let (container, redis_client1, conn1) = setup_redis().await;

    // Create a second Redis connection for node2
    let redis_url = container.connection_url();
    let redis_client2 =
        redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client 2");

    // Wait for connection 2 (generous timeout for parallel testcontainer startup)
    let conn2 = {
        let mut retries = 0;
        loop {
            match redis::aio::ConnectionManager::new(redis_client2.clone()).await {
                Ok(conn) => break conn,
                Err(_) if retries < 60 => {
                    retries += 1;
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(e) => panic!("Redis ConnectionManager 2 failed after {retries} retries: {e}"),
            }
        }
    };

    // Create two cluster managers (simulating two nodes) with separate Redis clients
    // but the same key prefix so they participate in the same logical cluster.
    let shared_prefix = format!("test_{}:", synctv_common::snanoid!(8));
    let config1 = make_cluster_config_with_prefix(
        redis_client1.clone(),
        conn1.clone(),
        "node1",
        shared_prefix.clone(),
    );
    let config2 = make_cluster_config_with_prefix(
        redis_client2.clone(),
        conn2.clone(),
        "node2",
        shared_prefix,
    );

    let manager1 = ClusterManager::new(config1, None, None)
        .await
        .expect("Failed to create ClusterManager 1");

    let manager2 = ClusterManager::new(config2, None, None)
        .await
        .expect("Failed to create ClusterManager 2");

    // Subscribe to room messages on node1
    let room = rid("room1");
    let user = uid("user1");
    let (mut rx, _conn_id) = manager1
        .subscribe_with_id(room.clone(), user.clone(), "conn1".to_string())
        .await
        .expect("subscribe should succeed");
    let received = broadcast_until_room_event(
        &manager2,
        &mut rx,
        || ClusterEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id: room.clone(),
            user_id: uid("user2"),
            username: "sender".to_string(),
            message: "hello from node2".to_string(),
            timestamp: chrono::Utc::now(),
            position: None,
            color: None,
        },
        |event| matches!(event, ClusterEvent::ChatMessage { message, .. } if message == "hello from node2"),
        "cross-node broadcast",
    )
    .await;
    assert_eq!(received.event_type(), "chat_message");

    manager1.shutdown().await;
    manager2.shutdown().await;
}

// ============================================================================
// Test 2: Message deduplication across nodes
// ============================================================================

/// Test that duplicate events are detected correctly.
#[tokio::test]
async fn test_message_deduplication() {
    let dedup = MessageDeduplicator::new(Duration::from_mins(1));

    let room = rid("room1");
    let user = uid("user1");

    let event = ClusterEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id: room.clone(),
        user_id: user.clone(),
        username: "test".to_string(),
        message: "hello".to_string(),
        timestamp: chrono::Utc::now(),
        position: None,
        color: None,
    };

    let key = DedupKey::from_event(&event);

    // First check should be processable (not duplicate)
    let should_process1 = dedup.should_process(&key);
    assert!(should_process1, "First occurrence should be processable");

    // Mark as processed
    dedup.mark_processed(key.clone());

    // Second check with same key should NOT be processable (duplicate)
    let should_process2 = dedup.should_process(&key);
    assert!(
        !should_process2,
        "Second occurrence should NOT be processable (duplicate)"
    );

    // Different event should be processable
    let event2 = ClusterEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16), // Different event_id
        room_id: room,
        user_id: user,
        username: "test".to_string(),
        message: "hello".to_string(),
        timestamp: chrono::Utc::now(),
        position: None,
        color: None,
    };
    let key2 = DedupKey::from_event(&event2);
    let should_process3 = dedup.should_process(&key2);
    assert!(should_process3, "Different event_id should be processable");
}

/// Test that deduplication respects the TTL window.
#[tokio::test]
async fn test_dedup_ttl_expiry() {
    let dedup = MessageDeduplicator::new(Duration::from_millis(100)); // Very short window

    let room = rid("room1");
    let event = ClusterEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id: room.clone(),
        user_id: uid("user1"),
        username: "test".to_string(),
        message: "hello".to_string(),
        timestamp: chrono::Utc::now(),
        position: None,
        color: None,
    };
    let key = DedupKey::from_event(&event);

    // First check
    let should_process1 = dedup.should_process(&key);
    assert!(should_process1, "First occurrence should be processable");
    dedup.mark_processed(key.clone());

    // Wait for TTL to expire
    tokio::time::sleep(Duration::from_millis(150)).await;

    // After expiry, should be processable again
    let should_process2 = dedup.should_process(&key);
    assert!(
        should_process2,
        "After TTL expiry, should be processable again"
    );
}

/// Test deduplication with different event types.
#[tokio::test]
async fn test_dedup_with_different_events() {
    let dedup = MessageDeduplicator::new(Duration::from_mins(1));

    let room = rid("room1");
    let user = uid("user1");
    let event_id = synctv_common::snanoid!(16);

    // Same event_id but different event types
    let event1 = ClusterEvent::UserJoined {
        event_id: event_id.clone(),
        room_id: room.clone(),
        user_id: user.clone(),
        username: "test".to_string(),
        permissions: synctv_core::models::permission::PermissionBits(0),
        role: 2,
        added_permissions: synctv_core::models::permission::PermissionBits(0),
        removed_permissions: synctv_core::models::permission::PermissionBits(0),
        admin_added_permissions: synctv_core::models::permission::PermissionBits(0),
        admin_removed_permissions: synctv_core::models::permission::PermissionBits(0),
        joined_at: chrono::Utc::now(),
        timestamp: chrono::Utc::now(),
    };

    let event2 = ClusterEvent::UserLeft {
        event_id,
        room_id: room,
        user_id: user,
        username: "test".to_string(),
        timestamp: chrono::Utc::now(),
    };

    let key1 = DedupKey::from_event(&event1);
    let key2 = DedupKey::from_event(&event2);

    // Different event types should have different keys
    assert_ne!(key1.event_type, key2.event_type);

    let should_process1 = dedup.should_process(&key1);
    assert!(should_process1, "First event should be processable");
    dedup.mark_processed(key1);

    let should_process2 = dedup.should_process(&key2);
    assert!(
        should_process2,
        "Different event type should be processable"
    );
}

// ============================================================================
// Test 3: Redis unavailable graceful degradation
// ============================================================================

/// Test that `ClusterManager` works without Redis (single-node mode).
#[tokio::test]
async fn test_single_node_mode_without_redis() {
    let config = ClusterConfig {
        redis_client: None,
        redis_conn: None,
        shared_redis_conn: None,
        node_id: "standalone".to_string(),
        ..Default::default()
    };

    let manager = ClusterManager::new(config, None, None)
        .await
        .expect("Should work without Redis");

    let room = rid("room1");
    let user = uid("user1");
    let (mut rx, _conn_id) = manager
        .subscribe_with_id(room.clone(), user.clone(), "conn1".to_string())
        .await
        .expect("subscribe should succeed");

    // Local broadcast should still work
    let event = ClusterEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id: room.clone(),
        user_id: user.clone(),
        username: "test".to_string(),
        message: "local message".to_string(),
        timestamp: chrono::Utc::now(),
        position: None,
        color: None,
    };

    manager.broadcast(event);

    // Should receive locally
    let result = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await;
    assert!(result.is_ok(), "Should receive local broadcast");

    manager.shutdown().await;
}

// ============================================================================
// Test 4: PubSub subscription management
// ============================================================================

/// Test that room subscriptions are properly tracked in Redis.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_pubsub_subscription_tracking() {
    let (_container, redis_client, conn) = setup_redis().await;

    let config = make_cluster_config(redis_client.clone(), conn.clone(), "node1");
    let manager = ClusterManager::new(config, None, None)
        .await
        .expect("Failed to create ClusterManager");

    let room = rid("room1");
    let user = uid("user1");

    // Subscribe
    let (_rx, conn_id) = manager
        .subscribe_with_id(room.clone(), user.clone(), "conn1".to_string())
        .await
        .expect("subscribe should succeed");

    // Unsubscribe
    manager.unsubscribe(&conn_id);

    // Connection count should be 0
    let metrics = manager.metrics();
    assert_eq!(
        metrics.total_connections, 0,
        "Connection count should be 0 after unsubscribe"
    );

    manager.shutdown().await;
}

/// Test multiple subscriptions to the same room.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_multiple_subscriptions_same_room() {
    let (_container, redis_client, conn) = setup_redis().await;

    let config = make_cluster_config(redis_client.clone(), conn.clone(), "node1");
    let manager = ClusterManager::new(config, None, None)
        .await
        .expect("Failed to create ClusterManager");

    let room = rid("room1");

    // Subscribe with multiple connections
    let (mut rx1, _) = manager
        .subscribe_with_id(room.clone(), uid("user1"), "conn1".to_string())
        .await
        .expect("subscribe should succeed");
    let (mut rx2, _) = manager
        .subscribe_with_id(room.clone(), uid("user2"), "conn2".to_string())
        .await
        .expect("subscribe should succeed");

    // Broadcast a message
    let event = ClusterEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id: room.clone(),
        user_id: uid("user1"),
        username: "sender".to_string(),
        message: "broadcast test".to_string(),
        timestamp: chrono::Utc::now(),
        position: None,
        color: None,
    };

    manager.broadcast(event);

    // Both should receive
    let r1 = tokio::time::timeout(Duration::from_millis(100), rx1.recv()).await;
    let r2 = tokio::time::timeout(Duration::from_millis(100), rx2.recv()).await;

    assert!(r1.is_ok(), "Connection 1 should receive");
    assert!(r2.is_ok(), "Connection 2 should receive");

    manager.shutdown().await;
}

// ============================================================================
// Test 5: Critical event delivery
// ============================================================================

/// Test that critical events (kick, permission change) are delivered reliably.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_critical_event_delivery() {
    let (container, redis_client1, conn1) = setup_redis().await;

    let redis_url = container.connection_url();
    let redis_client2 =
        redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client 2");
    let conn2 = redis::aio::ConnectionManager::new(redis_client2.clone())
        .await
        .expect("Failed to create Redis ConnectionManager 2");

    let shared_prefix = format!("test_{}:", synctv_common::snanoid!(8));
    let config1 = make_cluster_config_with_prefix(
        redis_client1.clone(),
        conn1.clone(),
        "node1",
        shared_prefix.clone(),
    );
    let config2 = make_cluster_config_with_prefix(
        redis_client2.clone(),
        conn2.clone(),
        "node2",
        shared_prefix,
    );

    let manager1 = ClusterManager::new(config1, None, None)
        .await
        .expect("Failed to create ClusterManager 1");
    let manager2 = ClusterManager::new(config2, None, None)
        .await
        .expect("Failed to create ClusterManager 2");

    let room = rid("room1");
    let user = uid("user1");

    // Subscribe to admin events on the receiving node, where Redis fan-out lands.
    let mut admin_rx = manager1.subscribe_admin_events();

    // Subscribe to room on node1
    let (mut room_rx, _) = manager1
        .subscribe_with_id(room.clone(), user.clone(), "conn1".to_string())
        .await
        .expect("subscribe should succeed");

    let room_received = broadcast_until_room_event(
        &manager2,
        &mut room_rx,
        || ClusterEvent::KickUserFromRoom {
            event_id: synctv_common::snanoid!(16),
            room_id: room.clone(),
            user_id: user.clone(),
            reason: "test_kick".to_string(),
            timestamp: chrono::Utc::now(),
        },
        |event| matches!(event, ClusterEvent::KickUserFromRoom { user_id, .. } if user_id.as_str() == "user1"),
        "critical event on remote room channel",
    )
    .await;
    assert_eq!(room_received.event_type(), "kick_user_from_room");

    let admin_received = broadcast_until_admin_event(
        &manager2,
        &mut admin_rx,
        || ClusterEvent::KickUserFromRoom {
            event_id: synctv_common::snanoid!(16),
            room_id: room.clone(),
            user_id: user.clone(),
            reason: "test_kick".to_string(),
            timestamp: chrono::Utc::now(),
        },
        |event| matches!(event, ClusterEvent::KickUserFromRoom { user_id, .. } if user_id.as_str() == "user1"),
        "critical event on remote admin channel",
    )
    .await;
    assert_eq!(admin_received.event_type(), "kick_user_from_room");

    manager1.shutdown().await;
    manager2.shutdown().await;
}

// ============================================================================
// Test 6: Event type routing
// ============================================================================

/// Test that events are routed to the correct channels based on type.
#[tokio::test]
async fn test_event_type_routing() {
    // Room events should have room_id
    let room_event = ClusterEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("room1"),
        user_id: uid("user1"),
        username: "test".to_string(),
        message: "hello".to_string(),
        timestamp: chrono::Utc::now(),
        position: None,
        color: None,
    };
    assert!(
        room_event.room_id().is_some(),
        "ChatMessage should have room_id"
    );
    assert_eq!(room_event.event_type(), "chat_message");

    // System events should not have room_id
    let system_event = ClusterEvent::SystemNotification {
        event_id: synctv_common::snanoid!(16),
        message: "maintenance".to_string(),
        level: NotificationLevel::Warning,
        timestamp: chrono::Utc::now(),
    };
    assert!(
        system_event.room_id().is_none(),
        "SystemNotification should not have room_id"
    );
    assert!(
        system_event.user_id().is_none(),
        "SystemNotification should not have user_id"
    );
    assert_eq!(system_event.event_type(), "system_notification");

    // KickUser should have user_id but no room_id
    let kick_event = ClusterEvent::KickUser {
        event_id: synctv_common::snanoid!(16),
        user_id: uid("user1"),
        reason: "banned".to_string(),
        timestamp: chrono::Utc::now(),
    };
    assert!(
        kick_event.room_id().is_none(),
        "KickUser should not have room_id"
    );
    assert!(
        kick_event.user_id().is_some(),
        "KickUser should have user_id"
    );
    assert!(kick_event.is_critical(), "KickUser should be critical");
}

/// Test critical event classification.
#[tokio::test]
async fn test_critical_event_classification() {
    // Critical events
    assert!(ClusterEvent::KickPublisher {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("r1"),
        media_id: mid("m1"),
        reason: "test".to_string(),
        timestamp: chrono::Utc::now(),
    }
    .is_critical());

    assert!(ClusterEvent::KickUser {
        event_id: synctv_common::snanoid!(16),
        user_id: uid("u1"),
        reason: "test".to_string(),
        timestamp: chrono::Utc::now(),
    }
    .is_critical());

    assert!(ClusterEvent::KickUserFromRoom {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("r1"),
        user_id: uid("u1"),
        reason: "test".to_string(),
        timestamp: chrono::Utc::now(),
    }
    .is_critical());

    assert!(ClusterEvent::PermissionChanged {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("r1"),
        target_user_id: uid("u1"),
        target_username: "test".to_string(),
        changed_by: uid("u2"),
        changed_by_username: "admin".to_string(),
        new_permissions: synctv_core::models::permission::PermissionBits(0),
        role: 2,
        added_permissions: synctv_core::models::permission::PermissionBits(0),
        removed_permissions: synctv_core::models::permission::PermissionBits(0),
        admin_added_permissions: synctv_core::models::permission::PermissionBits(0),
        admin_removed_permissions: synctv_core::models::permission::PermissionBits(0),
        timestamp: chrono::Utc::now(),
    }
    .is_critical());

    assert!(ClusterEvent::RoomDeleted {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("r1"),
        deleted_by: uid("u1"),
        timestamp: chrono::Utc::now(),
    }
    .is_critical());

    assert!(ClusterEvent::UserLeft {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("r1"),
        user_id: uid("u1"),
        username: "test".to_string(),
        timestamp: chrono::Utc::now(),
    }
    .is_critical());

    // Non-critical events
    assert!(!ClusterEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("r1"),
        user_id: uid("u1"),
        username: "test".to_string(),
        message: "hello".to_string(),
        timestamp: chrono::Utc::now(),
        position: None,
        color: None,
    }
    .is_critical());

    assert!(!ClusterEvent::WebRTCJoin {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("r1"),
        user_id: uid("u1"),
        conn_id: "c1".to_string(),
        username: "test".to_string(),
        timestamp: chrono::Utc::now(),
    }
    .is_critical());
}

// ============================================================================
// Test 7: Broadcast result tracking
// ============================================================================

/// Test that broadcast returns correct recipient count.
#[tokio::test]
async fn test_broadcast_recipient_count() {
    let config = ClusterConfig {
        redis_client: None,
        redis_conn: None,
        shared_redis_conn: None,
        node_id: "standalone".to_string(),
        ..Default::default()
    };

    let manager = ClusterManager::new(config, None, None)
        .await
        .expect("Failed to create ClusterManager");

    let room = rid("room1");

    // Subscribe 3 connections
    let (_rx1, _) = manager
        .subscribe_with_id(room.clone(), uid("user1"), "conn1".to_string())
        .await
        .expect("subscribe should succeed");
    let (_rx2, _) = manager
        .subscribe_with_id(room.clone(), uid("user2"), "conn2".to_string())
        .await
        .expect("subscribe should succeed");
    let (_rx3, _) = manager
        .subscribe_with_id(room.clone(), uid("user3"), "conn3".to_string())
        .await
        .expect("subscribe should succeed");

    // Broadcast a message
    let event = ClusterEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id: room.clone(),
        user_id: uid("user1"),
        username: "test".to_string(),
        message: "hello".to_string(),
        timestamp: chrono::Utc::now(),
        position: None,
        color: None,
    };

    let result = manager.broadcast(event);
    assert_eq!(result.local_sent, 3, "Should have 3 local recipients");

    manager.shutdown().await;
}

// ============================================================================
// Test 8: Cache invalidation event
// ============================================================================

/// Test cache invalidation event serialization.
#[tokio::test]
async fn test_cache_invalidation_event() {
    let event = ClusterEvent::CacheInvalidate {
        event_id: synctv_common::snanoid!(16),
        targets: vec![
            CacheTarget::User {
                user_id: "u1".to_string(),
            },
            CacheTarget::Room {
                room_id: "r1".to_string(),
            },
            CacheTarget::All,
        ],
        timestamp: chrono::Utc::now(),
    };

    // Serialize
    let json = serde_json::to_string(&event).expect("Should serialize");
    assert!(json.contains("cache_invalidate"));

    // Deserialize
    let decoded: ClusterEvent = serde_json::from_str(&json).expect("Should deserialize");
    assert_eq!(decoded.event_type(), "cache_invalidate");

    // No room_id for cache invalidation
    assert!(decoded.room_id().is_none());
    assert!(decoded.user_id().is_none());
}

// ============================================================================
// Test 9: Media removed batch event
// ============================================================================

/// Test batch media removal event for efficiency.
#[tokio::test]
async fn test_media_removed_batch_event() {
    let room = rid("room1");
    let user = uid("user1");
    let media_ids: Vec<MediaId> = (0..100)
        .map(|i| MediaId::from_string(format!("media_{i}")))
        .collect();

    let event = ClusterEvent::MediaRemovedBatch {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        user_id: user,
        username: "admin".to_string(),
        media_ids,
        timestamp: chrono::Utc::now(),
    };

    // Serialize and deserialize
    let json = serde_json::to_string(&event).expect("Should serialize");
    let decoded: ClusterEvent = serde_json::from_str(&json).expect("Should deserialize");

    if let ClusterEvent::MediaRemovedBatch {
        media_ids: decoded_ids,
        ..
    } = decoded
    {
        assert_eq!(decoded_ids.len(), 100, "Should have 100 media IDs");
    } else {
        panic!("Expected MediaRemovedBatch variant");
    }
}

// ============================================================================
// Test 10: User notification event
// ============================================================================

/// Test user notification event for real-time delivery.
#[tokio::test]
async fn test_user_notification_event() {
    let user = uid("user1");

    let event = ClusterEvent::UserNotification {
        event_id: synctv_common::snanoid!(16),
        user_id: user,
        notification_id: "notif-123".to_string(),
        title: "Room Invitation".to_string(),
        content: "You have been invited to room X".to_string(),
        notification_type: "room_invitation".to_string(),
        timestamp: chrono::Utc::now(),
    };

    // Should have user_id but no room_id
    assert!(event.room_id().is_none());
    assert!(event.user_id().is_some());

    // Check dedup_extra includes notification_id
    let extra = event.dedup_extra();
    assert!(extra.contains("user1"));
    assert!(extra.contains("notif-123"));
}

// ============================================================================
// Test 11: DedupKey from event
// ============================================================================

/// Test that `DedupKey` correctly extracts fields from events.
#[tokio::test]
async fn test_dedup_key_from_event() {
    let room = rid("room1");
    let user = uid("user1");
    let event_id = synctv_common::snanoid!(16);

    let event = ClusterEvent::ChatMessage {
        event_id: event_id.clone(),
        room_id: room.clone(),
        user_id: user,
        username: "test".to_string(),
        message: "hello".to_string(),
        timestamp: chrono::Utc::now(),
        position: None,
        color: None,
    };

    let key = DedupKey::from_event(&event);

    assert_eq!(key.event_type, "chat_message");
    assert_eq!(key.room_id, room.as_str());
    // When event_id is present, it's embedded in the extra field
    assert_eq!(key.extra, event_id);
}

// ============================================================================
// Test 12: Room subscribers listing
// ============================================================================

/// Test getting room subscriber list.
#[tokio::test]
async fn test_get_room_subscribers() {
    let config = ClusterConfig {
        redis_client: None,
        redis_conn: None,
        shared_redis_conn: None,
        node_id: "standalone".to_string(),
        ..Default::default()
    };

    let manager = ClusterManager::new(config, None, None)
        .await
        .expect("Failed to create ClusterManager");

    let room = rid("room1");

    let (_rx1, _) = manager
        .subscribe_with_id(room.clone(), uid("user1"), "conn1".to_string())
        .await
        .expect("subscribe should succeed");
    let (_rx2, _) = manager
        .subscribe_with_id(room.clone(), uid("user2"), "conn2".to_string())
        .await
        .expect("subscribe should succeed");

    let subscribers = manager.get_room_subscribers(&room);
    assert_eq!(subscribers.len(), 2, "Should have 2 subscribers");

    manager.shutdown().await;
}

// ============================================================================
// Test 13: Cluster metrics
// ============================================================================

/// Test cluster metrics reporting.
#[tokio::test]
async fn test_cluster_metrics() {
    let config = ClusterConfig {
        redis_client: None,
        redis_conn: None,
        shared_redis_conn: None,
        node_id: "test_node".to_string(),
        ..Default::default()
    };

    let manager = ClusterManager::new(config, None, None)
        .await
        .expect("Failed to create ClusterManager");

    let room = rid("room1");
    let (_rx, _) = manager
        .subscribe_with_id(room.clone(), uid("user1"), "conn1".to_string())
        .await
        .expect("subscribe should succeed");

    let metrics = manager.metrics();
    assert_eq!(metrics.node_id, "test_node");
    assert_eq!(metrics.total_rooms, 1);
    assert_eq!(metrics.total_connections, 1);
    assert!(!metrics.redis_enabled);

    manager.shutdown().await;
}
