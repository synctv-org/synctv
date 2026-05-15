//! Realtime messaging tests
//!
//! Tests for cross-node message broadcasting, deduplication, and Redis fallback.
//! Uses testcontainers for Redis integration tests.

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use std::time::Duration;

use synctv_core::models::id::{MediaId, RoomId, UserId};
use synctv_core::{DirectRedisConnectionRuntime, RedisConnectionRuntime, SharedStateProfile};
use synctv_core_testing::{redis_connection_manager, start_redis_client_manager, RedisContainer};
use synctv_realtime::sync::events::{CacheTarget, NotificationLevel, RealtimeEvent};
use synctv_realtime::{
    build_room_message_runtime, DedupKey, MessageDeduplicator, RealtimeConfig, RealtimeManager,
    RoomMessageHub,
};
mod integration_test_helpers;
use integration_test_helpers::{broadcast_until_admin_event, broadcast_until_room_event};

fn stable_test_id(s: &str) -> i64 {
    s.bytes().fold(0_i64, |acc, byte| {
        (acc * 131 + i64::from(byte)) % 900_000_000
    }) + 1
}

fn uid(s: &str) -> UserId {
    UserId::expect_positive(stable_test_id(s))
}

fn rid(s: &str) -> RoomId {
    RoomId::expect_positive(stable_test_id(s))
}

fn mid(s: &str) -> MediaId {
    MediaId::expect_positive(stable_test_id(s))
}

/// Helper to create a Redis container and connection manager.
async fn setup_redis() -> (RedisContainer, redis::Client, redis::aio::ConnectionManager) {
    start_redis_client_manager().await
}

/// Create a test realtime config with Redis connection.
fn make_realtime_config(
    redis_client: redis::Client,
    redis_conn: &redis::aio::ConnectionManager,
    node_id: &str,
) -> RealtimeConfig {
    make_realtime_config_with_prefix(
        redis_client,
        redis_conn,
        node_id,
        format!("test_{}:", synctv_common::snanoid!(8)),
    )
}

fn make_realtime_config_with_prefix(
    redis_client: redis::Client,
    redis_conn: &redis::aio::ConnectionManager,
    node_id: &str,
    key_prefix: String,
) -> RealtimeConfig {
    let shared_runtime: Arc<dyn RedisConnectionRuntime> =
        Arc::new(DirectRedisConnectionRuntime::new(redis_conn.clone()));
    let realtime_profile =
        SharedStateProfile::from_runtime(Some(shared_runtime), &key_prefix, true);
    RealtimeConfig {
        distributed_transport_factory: Some(Arc::new(
            synctv_realtime::RedisRealtimeMessageTransportFactory::new(
                synctv_core::coordination_runtime_from_client(redis_client),
            ),
        )),
        message_runtime: build_room_message_runtime(&realtime_profile)
            .expect("shared message runtime should initialize"),
        distributed_enabled: true,
        node_id: node_id.to_string(),
        dedup_window: Duration::from_mins(1),
        critical_channel_capacity: 100,
        publish_channel_capacity: 1000,
        key_prefix,
        catchup_window_secs: 300,
        stream_max_length: 1000,
        event_handler: None,
        parent_cancel_token: None,
    }
}

// Test 1: Cross-node message broadcast via Redis Pub/Sub

/// Test that a message published from one node is received by another node.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_cross_node_broadcast() {
    let (container, redis_client1, conn1) = setup_redis().await;

    let redis_url = container.connection_url();
    let redis_client2 =
        redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client 2");
    let conn2 = redis_connection_manager(&redis_client2).await;

    // but the same key prefix so they participate in the same logical cluster.
    let shared_prefix = format!("test_{}:", synctv_common::snanoid!(8));
    let config1 = make_realtime_config_with_prefix(
        redis_client1.clone(),
        &conn1,
        "node1",
        shared_prefix.clone(),
    );
    let config2 =
        make_realtime_config_with_prefix(redis_client2.clone(), &conn2, "node2", shared_prefix);

    let manager1 = RealtimeManager::new(config1)
        .await
        .expect("Failed to create RealtimeManager 1");

    let manager2 = RealtimeManager::new(config2)
        .await
        .expect("Failed to create RealtimeManager 2");

    // Subscribe to room messages on node1
    let room = rid("room1");
    let user = uid("user1");
    let (mut rx, _conn_id) = manager1
        .subscribe_with_id(room, user, "conn1".to_string())
        .await
        .expect("subscribe should succeed");
    let received = broadcast_until_room_event(
        &manager2,
        &mut rx,
        || RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id: room,
            user_id: uid("user2"),
            username: "sender".to_string(),
            message: "hello from node2".to_string(),
            timestamp: chrono::Utc::now(),
            position: None,
            color: None,
        },
        |event| matches!(event, RealtimeEvent::ChatMessage { message, .. } if message == "hello from node2"),
        "cross-node broadcast",
    )
    .await;
    assert_eq!(received.event_type(), "chat_message");

    manager1.shutdown().await;
    manager2.shutdown().await;
}

// Test 2: Message deduplication across nodes

/// Test that duplicate events are detected correctly.
#[tokio::test]
async fn test_message_deduplication() {
    let dedup = MessageDeduplicator::new(Duration::from_mins(1));

    let room = rid("room1");
    let user = uid("user1");

    let event = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        user_id: user,
        username: "test".to_string(),
        message: "hello".to_string(),
        timestamp: chrono::Utc::now(),
        position: None,
        color: None,
    };

    let key = DedupKey::try_from_event(&event).unwrap();

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
    let event2 = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16), // Different event_id
        room_id: room,
        user_id: user,
        username: "test".to_string(),
        message: "hello".to_string(),
        timestamp: chrono::Utc::now(),
        position: None,
        color: None,
    };
    let key2 = DedupKey::try_from_event(&event2).unwrap();
    let should_process3 = dedup.should_process(&key2);
    assert!(should_process3, "Different event_id should be processable");
}

/// Test that deduplication respects the TTL window.
#[tokio::test]
async fn test_dedup_ttl_expiry() {
    let dedup = MessageDeduplicator::new(Duration::from_millis(100)); // Very short window

    let room = rid("room1");
    let event = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        user_id: uid("user1"),
        username: "test".to_string(),
        message: "hello".to_string(),
        timestamp: chrono::Utc::now(),
        position: None,
        color: None,
    };
    let key = DedupKey::try_from_event(&event).unwrap();

    // First check
    let should_process1 = dedup.should_process(&key);
    assert!(should_process1, "First occurrence should be processable");
    dedup.mark_processed(key.clone());

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
    let event1 = RealtimeEvent::UserJoined {
        event_id: event_id.clone(),
        room_id: room,
        user_id: user,
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

    let event2 = RealtimeEvent::UserLeft {
        event_id,
        room_id: room,
        user_id: user,
        username: "test".to_string(),
        timestamp: chrono::Utc::now(),
    };

    let key1 = DedupKey::try_from_event(&event1).unwrap();
    let key2 = DedupKey::try_from_event(&event2).unwrap();

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

// Test 3: Redis unavailable graceful degradation

/// Test that `RealtimeManager` works without Redis (single-node mode).
#[tokio::test]
async fn test_single_node_mode_without_redis() {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        node_id: "standalone".to_string(),
        ..Default::default()
    };

    let manager = RealtimeManager::new(config)
        .await
        .expect("Should work without Redis");

    let room = rid("room1");
    let user = uid("user1");
    let (mut rx, _conn_id) = manager
        .subscribe_with_id(room, user, "conn1".to_string())
        .await
        .expect("subscribe should succeed");

    // Local broadcast should still work
    let event = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        user_id: user,
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

// Test 4: PubSub subscription management

/// Test that room subscriptions are properly tracked in Redis.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_pubsub_subscription_tracking() {
    let (_container, redis_client, conn) = setup_redis().await;

    let config = make_realtime_config(redis_client.clone(), &conn, "node1");
    let manager = RealtimeManager::new(config)
        .await
        .expect("Failed to create RealtimeManager");

    let room = rid("room1");
    let user = uid("user1");

    // Subscribe
    let (_rx, conn_id) = manager
        .subscribe_with_id(room, user, "conn1".to_string())
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

    let config = make_realtime_config(redis_client.clone(), &conn, "node1");
    let manager = RealtimeManager::new(config)
        .await
        .expect("Failed to create RealtimeManager");

    let room = rid("room1");

    // Subscribe with multiple connections
    let (mut rx1, _) = manager
        .subscribe_with_id(room, uid("user1"), "conn1".to_string())
        .await
        .expect("subscribe should succeed");
    let (mut rx2, _) = manager
        .subscribe_with_id(room, uid("user2"), "conn2".to_string())
        .await
        .expect("subscribe should succeed");

    // Broadcast a message
    let event = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
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

// Test 5: Critical event delivery

/// Test that critical events (kick, permission change) are delivered reliably.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_critical_event_delivery() {
    let (container, redis_client1, conn1) = setup_redis().await;

    let redis_url = container.connection_url();
    let redis_client2 =
        redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client 2");
    let conn2 = redis_connection_manager(&redis_client2).await;

    let shared_prefix = format!("test_{}:", synctv_common::snanoid!(8));
    let config1 = make_realtime_config_with_prefix(
        redis_client1.clone(),
        &conn1,
        "node1",
        shared_prefix.clone(),
    );
    let config2 =
        make_realtime_config_with_prefix(redis_client2.clone(), &conn2, "node2", shared_prefix);

    let manager1 = RealtimeManager::new(config1)
        .await
        .expect("Failed to create RealtimeManager 1");
    let manager2 = RealtimeManager::new(config2)
        .await
        .expect("Failed to create RealtimeManager 2");

    let room = rid("room1");
    let user = uid("user1");

    // Subscribe to admin events on the receiving node, where Redis fan-out lands.
    let mut admin_rx = manager1.subscribe_admin_events();

    // Subscribe to room on node1
    let (mut room_rx, _) = manager1
        .subscribe_with_id(room, user, "conn1".to_string())
        .await
        .expect("subscribe should succeed");

    let room_received = broadcast_until_room_event(
        &manager2,
        &mut room_rx,
        || RealtimeEvent::KickUserFromRoom {
            event_id: synctv_common::snanoid!(16),
            room_id: room,
            user_id: user,
            reason: "test_kick".to_string(),
            timestamp: chrono::Utc::now(),
        },
        |event| matches!(event, RealtimeEvent::KickUserFromRoom { user_id, .. } if *user_id == user),
        "critical event on remote room channel",
    )
    .await;
    assert_eq!(room_received.event_type(), "kick_user_from_room");

    let admin_received = broadcast_until_admin_event(
        &manager2,
        &mut admin_rx,
        || RealtimeEvent::KickUserFromRoom {
            event_id: synctv_common::snanoid!(16),
            room_id: room,
            user_id: user,
            reason: "test_kick".to_string(),
            timestamp: chrono::Utc::now(),
        },
        |event| matches!(event, RealtimeEvent::KickUserFromRoom { user_id, .. } if *user_id == user),
        "critical event on remote admin channel",
    )
    .await;
    assert_eq!(admin_received.event_type(), "kick_user_from_room");

    manager1.shutdown().await;
    manager2.shutdown().await;
}

// Test 6: Event type routing

/// Test that events are routed to the correct channels based on type.
#[tokio::test]
async fn test_event_type_routing() {
    // Room events should have room_id
    let room_event = RealtimeEvent::ChatMessage {
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
    let system_event = RealtimeEvent::SystemNotification {
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
    let kick_event = RealtimeEvent::KickUser {
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
    assert!(RealtimeEvent::KickPublisher {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("r1"),
        media_id: mid("m1"),
        reason: "test".to_string(),
        timestamp: chrono::Utc::now(),
    }
    .is_critical());

    assert!(RealtimeEvent::KickUser {
        event_id: synctv_common::snanoid!(16),
        user_id: uid("u1"),
        reason: "test".to_string(),
        timestamp: chrono::Utc::now(),
    }
    .is_critical());

    assert!(RealtimeEvent::KickUserFromRoom {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("r1"),
        user_id: uid("u1"),
        reason: "test".to_string(),
        timestamp: chrono::Utc::now(),
    }
    .is_critical());

    assert!(RealtimeEvent::PermissionChanged {
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

    assert!(RealtimeEvent::RoomDeleted {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("r1"),
        deleted_by: uid("u1"),
        timestamp: chrono::Utc::now(),
    }
    .is_critical());

    assert!(RealtimeEvent::UserLeft {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("r1"),
        user_id: uid("u1"),
        username: "test".to_string(),
        timestamp: chrono::Utc::now(),
    }
    .is_critical());

    // Non-critical events
    assert!(!RealtimeEvent::ChatMessage {
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

    assert!(!RealtimeEvent::WebRTCJoin {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("r1"),
        actor_id: "usr_u1".to_string(),
        conn_id: "c1".to_string(),
        username: "test".to_string(),
        timestamp: chrono::Utc::now(),
    }
    .is_critical());
}

// Test 7: Broadcast result tracking

/// Test that broadcast returns correct recipient count.
#[tokio::test]
async fn test_broadcast_recipient_count() {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        node_id: "standalone".to_string(),
        ..Default::default()
    };

    let manager = RealtimeManager::new(config)
        .await
        .expect("Failed to create RealtimeManager");

    let room = rid("room1");

    // Subscribe 3 connections
    let (_rx1, _) = manager
        .subscribe_with_id(room, uid("user1"), "conn1".to_string())
        .await
        .expect("subscribe should succeed");
    let (_rx2, _) = manager
        .subscribe_with_id(room, uid("user2"), "conn2".to_string())
        .await
        .expect("subscribe should succeed");
    let (_rx3, _) = manager
        .subscribe_with_id(room, uid("user3"), "conn3".to_string())
        .await
        .expect("subscribe should succeed");

    // Broadcast a message
    let event = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
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

// Test 8: Cache invalidation event

/// Test cache invalidation event serialization.
#[tokio::test]
async fn test_cache_invalidation_event() {
    let event = RealtimeEvent::CacheInvalidate {
        event_id: synctv_common::snanoid!(16),
        targets: vec![
            CacheTarget::User {
                user_id: UserId::expect_positive(10_060_001),
            },
            CacheTarget::Room {
                room_id: RoomId::expect_positive(10_060_002),
            },
            CacheTarget::All,
        ],
        timestamp: chrono::Utc::now(),
    };

    // Serialize
    let json = serde_json::to_string(&event).expect("Should serialize");
    assert!(json.contains("cache_invalidate"));

    // Deserialize
    let decoded: RealtimeEvent = serde_json::from_str(&json).expect("Should deserialize");
    assert_eq!(decoded.event_type(), "cache_invalidate");

    // No room_id for cache invalidation
    assert!(decoded.room_id().is_none());
    assert!(decoded.user_id().is_none());
}

// Test 9: Media removed batch event

/// Test batch media removal event for efficiency.
#[tokio::test]
async fn test_media_removed_batch_event() {
    let room = rid("room1");
    let user = uid("user1");
    let media_ids: Vec<MediaId> = (0..100)
        .map(|i| MediaId::expect_positive(100_000 + i))
        .collect();

    let event = RealtimeEvent::MediaRemovedBatch {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        user_id: user,
        username: "admin".to_string(),
        media_ids,
        timestamp: chrono::Utc::now(),
    };

    // Serialize and deserialize
    let json = serde_json::to_string(&event).expect("Should serialize");
    let decoded: RealtimeEvent = serde_json::from_str(&json).expect("Should deserialize");

    if let RealtimeEvent::MediaRemovedBatch {
        media_ids: decoded_ids,
        ..
    } = decoded
    {
        assert_eq!(decoded_ids.len(), 100, "Should have 100 media IDs");
    } else {
        panic!("Expected MediaRemovedBatch variant");
    }
}

// Test 10: User notification event

/// Test user notification event for real-time delivery.
#[tokio::test]
async fn test_user_notification_event() {
    let user = uid("user1");

    let event = RealtimeEvent::UserNotification {
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
    assert!(extra.contains(&user.to_string()));
    assert!(extra.contains("notif-123"));
}

// Test 11: DedupKey from event

/// Test that `DedupKey` correctly extracts fields from events.
#[tokio::test]
async fn test_dedup_key_from_event() {
    let room = rid("room1");
    let user = uid("user1");
    let event_id = synctv_common::snanoid!(16);

    let event = RealtimeEvent::ChatMessage {
        event_id: event_id.clone(),
        room_id: room,
        user_id: user,
        username: "test".to_string(),
        message: "hello".to_string(),
        timestamp: chrono::Utc::now(),
        position: None,
        color: None,
    };

    let key = DedupKey::try_from_event(&event).unwrap();

    assert_eq!(key.event_type, "chat_message");
    assert_eq!(key.room_id, room.to_string());
    // When event_id is present, it's embedded in the extra field
    assert_eq!(key.extra, event_id);
}

// Test 12: Room subscribers listing

/// Test getting room subscriber list.
#[tokio::test]
async fn test_get_room_subscribers() {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        node_id: "standalone".to_string(),
        ..Default::default()
    };

    let manager = RealtimeManager::new(config)
        .await
        .expect("Failed to create RealtimeManager");

    let room = rid("room1");

    let (_rx1, _) = manager
        .subscribe_with_id(room, uid("user1"), "conn1".to_string())
        .await
        .expect("subscribe should succeed");
    let (_rx2, _) = manager
        .subscribe_with_id(room, uid("user2"), "conn2".to_string())
        .await
        .expect("subscribe should succeed");

    let subscribers = manager.get_room_subscribers(&room);
    assert_eq!(subscribers.len(), 2, "Should have 2 subscribers");

    manager.shutdown().await;
}

// Test 13: Realtime metrics

/// Test realtime metrics reporting.
#[tokio::test]
async fn test_cluster_metrics() {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        node_id: "test_node".to_string(),
        ..Default::default()
    };

    let manager = RealtimeManager::new(config)
        .await
        .expect("Failed to create RealtimeManager");

    let room = rid("room1");
    let (_rx, _) = manager
        .subscribe_with_id(room, uid("user1"), "conn1".to_string())
        .await
        .expect("subscribe should succeed");

    let metrics = manager.metrics();
    assert_eq!(metrics.node_id, "test_node");
    assert_eq!(metrics.total_rooms, 1);
    assert_eq!(metrics.total_connections, 1);
    assert!(!metrics.distributed_enabled);

    manager.shutdown().await;
}
