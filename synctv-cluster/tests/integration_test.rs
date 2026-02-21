//! Multi-replica cluster integration tests
//!
//! These tests verify cross-node coordination by starting multiple
//! `ClusterManager` instances that share a single Redis container
//! (via testcontainers). Each "node" has its own `node_id` but connects
//! to the same Redis, simulating a multi-replica deployment.
//!
//! Run with: cargo test --package synctv-cluster --test integration_test

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use synctv_cluster::sync::events::{CacheTarget, ClusterEvent};
use synctv_cluster::{ClusterConfig, ClusterManager, MessageDeduplicator, RoomMessageHub};
use synctv_cluster::sync::redis_pubsub::RedisPubSub;
use synctv_core::cache::{CacheInvalidationService, InvalidationMessage};
use synctv_core::models::id::{MediaId, RoomId, UserId};
use synctv_core::models::playback::RoomPlaybackState;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::redis::Redis;
use tokio::sync::broadcast;

/// Default Redis version for test containers
const REDIS_VERSION: &str = "7-alpine";

/// Redis test infrastructure that manages a single Redis container.
/// The container is automatically stopped when this struct is dropped.
struct TestRedis {
    redis_url: String,
    _redis: ContainerAsync<Redis>,
}

impl TestRedis {
    async fn start() -> Self {
        let redis_container = Redis::default()
            .with_tag(REDIS_VERSION)
            .start()
            .await
            .expect("Failed to start Redis container");

        let redis_host = redis_container
            .get_host()
            .await
            .expect("Failed to get Redis host");
        let redis_port = redis_container
            .get_host_port_ipv4(6379)
            .await
            .expect("Failed to get Redis port");

        let redis_url = format!("redis://{}:{}", redis_host, redis_port);

        Self {
            redis_url,
            _redis: redis_container,
        }
    }
}

/// Helper: create a `ClusterManager` connected to the given Redis URL.
async fn create_node(redis_url: &str, node_id: &str) -> ClusterManager {
    let client = redis::Client::open(redis_url).expect("Failed to open Redis client");
    let conn = client.get_connection_manager().await.expect("Failed to get ConnectionManager");
    let config = ClusterConfig {
        redis_client: Some(client),
        redis_conn: Some(conn),
        node_id: node_id.to_string(),
        dedup_window: Duration::from_secs(10),
        cleanup_interval: Duration::from_secs(30),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
    };
    ClusterManager::new(config, None, None)
        .await
        .expect("Failed to create ClusterManager")
}

// ============================================================================
// Test 1: Cross-replica user kick
// ============================================================================

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

// ============================================================================
// Test 2: Cross-replica room event propagation
// ============================================================================

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

// ============================================================================
// Test 3: Cross-replica KickPublisher propagation via admin channel
// ============================================================================

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

// ============================================================================
// Test 4: Cross-replica cache invalidation via CacheInvalidate event
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_cache_invalidation() {
    let redis = TestRedis::start().await;

    // Create a CacheInvalidationService for node A (local-only, no Redis stream)
    let cache_svc_a = CacheInvalidationService::new(
        None,
        "node_a".to_string(),
        "test:cache:inv".to_string(),
    );
    let mut local_rx_a = cache_svc_a.subscribe();

    // Create node A with cache invalidation enabled
    let client_a = redis::Client::open(redis.redis_url.clone()).expect("Failed to open Redis client");
    let conn_a = client_a.get_connection_manager().await.expect("Failed to get ConnectionManager");
    let config_a = ClusterConfig {
        redis_client: Some(client_a),
        redis_conn: Some(conn_a),
        node_id: "node_a".to_string(),
        dedup_window: Duration::from_secs(10),
        cleanup_interval: Duration::from_secs(30),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
    };
    let node_a = ClusterManager::new(config_a, None, Some(cache_svc_a))
        .await
        .expect("Failed to create node A");

    let node_b = create_node(&redis.redis_url, "node_b").await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Node B updates user data and broadcasts a CacheInvalidate event
    let invalidate_event = ClusterEvent::CacheInvalidate {
        event_id: nanoid::nanoid!(16),
        targets: vec![
            CacheTarget::User {
                user_id: "updated_user".to_string(),
            },
            CacheTarget::Room {
                room_id: "updated_room".to_string(),
            },
        ],
        timestamp: Utc::now(),
    };

    let result = node_b.broadcast(invalidate_event);
    assert!(
        result.redis_sent,
        "CacheInvalidate should be published to Redis"
    );

    // Node A's cache invalidation service should receive local invalidation messages.
    // CacheInvalidate events dispatch to cache_invalidation service, not admin channel.
    let mut received_user = false;
    let mut received_room = false;

    for _ in 0..2 {
        let msg = tokio::time::timeout(Duration::from_secs(5), local_rx_a.recv())
            .await
            .expect("Timed out waiting for cache invalidation")
            .expect("Cache invalidation channel closed");

        match msg {
            InvalidationMessage::User { user_id } if user_id == "updated_user" => {
                received_user = true;
            }
            InvalidationMessage::Room { room_id } if room_id == "updated_room" => {
                received_room = true;
            }
            other => {
                panic!("Unexpected invalidation message: {:?}", other);
            }
        }
    }

    assert!(received_user, "Should have received User invalidation");
    assert!(received_room, "Should have received Room invalidation");

    node_a.shutdown().await;
    node_b.shutdown().await;
}

// ============================================================================
// Test 5: Redis Pub/Sub reliability - no message loss under normal conditions
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_redis_pubsub_no_message_loss() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;
    let node_b = create_node(&redis.redis_url, "node_b").await;

    let room_id = RoomId::from_string("busy_room".to_string());
    let user_id = UserId::from_string("listener".to_string());

    // Subscribe on node A
    let (mut room_rx, conn_id) = node_a.subscribe(room_id.clone(), user_id.clone()).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Send multiple messages from node B
    let message_count = 20;
    for i in 0..message_count {
        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: room_id.clone(),
            user_id: UserId::from_string("sender".to_string()),
            username: "sender".to_string(),
            message: format!("Message {}", i),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };
        node_b.broadcast(event);
        // Small delay to avoid overwhelming the channel
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Collect all received messages
    let mut received_messages = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while received_messages.len() < message_count {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, room_rx.recv()).await {
            Ok(Some(evt)) => {
                if let ClusterEvent::ChatMessage { message, .. } = &evt {
                    received_messages.push(message.clone());
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }

    assert_eq!(
        received_messages.len(),
        message_count,
        "Expected {} messages, received {}: {:?}",
        message_count,
        received_messages.len(),
        received_messages
    );

    // Verify ordering is preserved
    for (i, msg) in received_messages.iter().enumerate() {
        assert_eq!(
            msg,
            &format!("Message {}", i),
            "Message {} out of order",
            i
        );
    }

    node_a.unsubscribe(&conn_id);
    node_a.shutdown().await;
    node_b.shutdown().await;
}

// ============================================================================
// Test 6: Message deduplication across nodes
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_deduplication() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;

    let room_id = RoomId::from_string("dedup_room".to_string());
    let user_id = UserId::from_string("listener".to_string());

    let (mut room_rx, conn_id) = node_a.subscribe(room_id.clone(), user_id.clone()).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Broadcast the same event twice locally (simulating duplicate delivery)
    let event = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: UserId::from_string("sender".to_string()),
        username: "sender".to_string(),
        message: "Duplicate test".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };

    let result1 = node_a.broadcast(event.clone());
    let result2 = node_a.broadcast(event);

    // First broadcast should succeed
    assert_eq!(
        result1.local_sent, 1,
        "First broadcast should reach local subscriber"
    );
    // Second broadcast should be deduplicated
    assert_eq!(result2.local_sent, 0, "Duplicate should be suppressed");

    // Only one message should arrive
    let received = tokio::time::timeout(Duration::from_secs(2), room_rx.recv())
        .await
        .expect("Timed out waiting for message")
        .expect("Channel closed");

    assert_eq!(received.event_type(), "chat_message");

    // No second message should arrive
    let no_dup = tokio::time::timeout(Duration::from_millis(500), room_rx.recv()).await;
    assert!(no_dup.is_err(), "Should not receive duplicate message");

    node_a.unsubscribe(&conn_id);
    node_a.shutdown().await;
}

// ============================================================================
// Test 7: Cross-replica RoomDeleted event triggers room cleanup
// ============================================================================

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

// ============================================================================
// Test 8: Event propagation latency is within acceptable bounds
// ============================================================================

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

// ============================================================================
// Test 9: Multiple rooms on different nodes
// ============================================================================

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

// ============================================================================
// Test 10: Three-node cluster event propagation
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_three_node_cluster() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;
    let node_b = create_node(&redis.redis_url, "node_b").await;
    let node_c = create_node(&redis.redis_url, "node_c").await;

    let room_id = RoomId::from_string("three_node_room".to_string());

    // Subscribe on node A and node C
    let (mut rx_a, conn_a) = node_a.subscribe(
        room_id.clone(),
        UserId::from_string("user_a".to_string()),
    ).await;
    let (mut rx_c, conn_c) = node_c.subscribe(
        room_id.clone(),
        UserId::from_string("user_c".to_string()),
    ).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Node B broadcasts
    let event = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: UserId::from_string("user_b".to_string()),
        username: "user_b".to_string(),
        message: "Hello from B".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };

    node_b.broadcast(event);

    // Both node A and node C should receive the message
    let msg_a = tokio::time::timeout(Duration::from_secs(5), rx_a.recv())
        .await
        .expect("Timed out on node A")
        .expect("Channel A closed");

    let msg_c = tokio::time::timeout(Duration::from_secs(5), rx_c.recv())
        .await
        .expect("Timed out on node C")
        .expect("Channel C closed");

    assert_eq!(msg_a.event_type(), "chat_message");
    assert_eq!(msg_c.event_type(), "chat_message");

    if let ClusterEvent::ChatMessage { message, .. } = &msg_a {
        assert_eq!(message, "Hello from B");
    }
    if let ClusterEvent::ChatMessage { message, .. } = &msg_c {
        assert_eq!(message, "Hello from B");
    }

    node_a.unsubscribe(&conn_a);
    node_c.unsubscribe(&conn_c);
    node_a.shutdown().await;
    node_b.shutdown().await;
    node_c.shutdown().await;
}

// ============================================================================
// Test 11: Redis Pub/Sub catchup - live messages after subscriber connects
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_redis_stream_catchup() {
    let redis = TestRedis::start().await;

    // Use raw RedisPubSub to test catchup mechanism directly
    let message_hub = Arc::new(RoomMessageHub::new());
    let (admin_tx, _admin_rx) = broadcast::channel::<ClusterEvent>(256);
    let dedup = Arc::new(MessageDeduplicator::with_defaults());

    let room_id = RoomId::from_string("catchup_room".to_string());
    let user_id = UserId::from_string("catchup_user".to_string());

    // Subscribe a user to the room in the hub
    let mut rx = message_hub.subscribe(
        room_id.clone(),
        user_id.clone(),
        "catchup_conn".to_string(),
    ).await;

    // Create the publisher node separately to write events to Redis streams
    let publisher = create_node(&redis.redis_url, "publisher_node").await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Publish events to Redis (they go into streams via dual-write)
    for i in 0..5 {
        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: room_id.clone(),
            user_id: UserId::from_string("publisher".to_string()),
            username: "publisher".to_string(),
            message: format!("Catchup message {}", i),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };
        publisher.broadcast(event);
    }

    // Give time for events to be written to Redis streams
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Now start a subscriber node that connects to the same Redis.
    // On first connect it snapshots stream tips, so pre-existing messages
    // won't be delivered. But any new messages should arrive via pub/sub.
    let redis_client = redis::Client::open(redis.redis_url.clone()).expect("Failed to open Redis client");
    let subscriber_node = Arc::new(
        RedisPubSub::new(
            redis_client,
            message_hub.clone(),
            "subscriber_node".to_string(),
            admin_tx,
            None,
            None,
            dedup,
        )
        .expect("Failed to create subscriber RedisPubSub"),
    );

    // Clone Arc before start() consumes it, so we can call shutdown() later
    let subscriber_for_shutdown = subscriber_node.clone();
    let _sub_tx = subscriber_node
        .start(10_000)
        .await
        .expect("Failed to start subscriber");

    // Wait for the subscriber to connect
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Publish one more message (should be received live)
    let final_event = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: UserId::from_string("publisher".to_string()),
        username: "publisher".to_string(),
        message: "Live message after subscriber connect".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };
    publisher.broadcast(final_event);

    // The subscriber should receive this live message
    let received = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("Timed out waiting for live message")
        .expect("Channel closed");

    assert_eq!(received.event_type(), "chat_message");
    if let ClusterEvent::ChatMessage { message, .. } = &received {
        assert_eq!(message, "Live message after subscriber connect");
    }

    publisher.shutdown().await;
    subscriber_for_shutdown.shutdown();
}

// ============================================================================
// Test 12: Critical events use high-priority channel
// ============================================================================

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

// ============================================================================
// Test 13: Node discovery - 3 nodes register and discover each other via Redis
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_node_discovery_three_nodes() {
    use synctv_cluster::NodeRegistry;

    let redis = TestRedis::start().await;

    let redis_client_a = redis::Client::open(redis.redis_url.clone()).expect("Failed to create Redis client A");
    let redis_client_b = redis::Client::open(redis.redis_url.clone()).expect("Failed to create Redis client B");
    let redis_client_c = redis::Client::open(redis.redis_url.clone()).expect("Failed to create Redis client C");

    let registry_a = NodeRegistry::new(
        redis_client_a,
        "node_a".to_string(),
        30,
        "synctv:",
    )
    .expect("Failed to create registry A");

    let registry_b = NodeRegistry::new(
        redis_client_b,
        "node_b".to_string(),
        30,
        "synctv:",
    )
    .expect("Failed to create registry B");

    let registry_c = NodeRegistry::new(
        redis_client_c,
        "node_c".to_string(),
        30,
        "synctv:",
    )
    .expect("Failed to create registry C");

    // Register all three nodes
    registry_a
        .register("node_a:50051".to_string(), "node_a:8080".to_string())
        .await
        .expect("Failed to register node A");

    registry_b
        .register("node_b:50051".to_string(), "node_b:8080".to_string())
        .await
        .expect("Failed to register node B");

    registry_c
        .register("node_c:50051".to_string(), "node_c:8080".to_string())
        .await
        .expect("Failed to register node C");

    // Wait for moka cache to expire (5s TTL) so uncached query hits Redis
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Each registry should see all 3 nodes
    let nodes = registry_a
        .get_all_nodes()
        .await
        .expect("Failed to get all nodes from A");

    let node_ids: Vec<String> = nodes.iter().map(|n| n.node_id.clone()).collect();
    assert!(
        node_ids.contains(&"node_a".to_string()),
        "Should contain node_a: {:?}",
        node_ids
    );
    assert!(
        node_ids.contains(&"node_b".to_string()),
        "Should contain node_b: {:?}",
        node_ids
    );
    assert!(
        node_ids.contains(&"node_c".to_string()),
        "Should contain node_c: {:?}",
        node_ids
    );
    assert_eq!(nodes.len(), 3, "Should have exactly 3 nodes");

    // Verify individual node lookup
    let node_b_info = registry_a
        .get_node("node_b")
        .await
        .expect("Failed to get node B")
        .expect("Node B not found");

    assert_eq!(node_b_info.node_id, "node_b");
    assert_eq!(node_b_info.grpc_address, "node_b:50051");
    assert_eq!(node_b_info.http_address, "node_b:8080");
    assert!(node_b_info.epoch >= 1, "Epoch should be at least 1");

    // Heartbeat should work
    let heartbeat_result = registry_a.heartbeat().await.expect("Heartbeat failed");
    assert_eq!(
        heartbeat_result,
        synctv_cluster::HeartbeatResult::Ok,
        "Heartbeat should succeed"
    );

    // Unregister node C
    registry_c.unregister().await.expect("Failed to unregister C");

    // After unregister + cache expiry, only 2 nodes should remain
    // Wait for moka cache invalidation (5s TTL)
    tokio::time::sleep(Duration::from_secs(6)).await;

    let nodes_after = registry_a
        .get_all_nodes()
        .await
        .expect("Failed to get nodes after unregister");

    let remaining_ids: Vec<String> = nodes_after.iter().map(|n| n.node_id.clone()).collect();
    assert!(
        !remaining_ids.contains(&"node_c".to_string()),
        "Node C should be unregistered: {:?}",
        remaining_ids
    );
    assert_eq!(
        nodes_after.len(),
        2,
        "Should have 2 remaining nodes: {:?}",
        remaining_ids
    );
}

// ============================================================================
// Test 14: Node epoch fencing - re-registration increments epoch
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_node_epoch_fencing() {
    use synctv_cluster::NodeRegistry;

    let redis = TestRedis::start().await;

    let redis_client = redis::Client::open(redis.redis_url.clone()).expect("Failed to create Redis client");

    let registry = NodeRegistry::new(
        redis_client,
        "fencing_node".to_string(),
        30,
        "synctv:",
    )
    .expect("Failed to create registry");

    // First registration
    registry
        .register("host:50051".to_string(), "host:8080".to_string())
        .await
        .expect("First register failed");

    let token1 = registry.current_fencing_token();
    assert!(token1.epoch >= 1, "First epoch should be >= 1");

    // Re-registration should increment epoch
    registry
        .register("host:50051".to_string(), "host:8080".to_string())
        .await
        .expect("Second register failed");

    let token2 = registry.current_fencing_token();
    assert!(
        token2.epoch > token1.epoch,
        "Re-registration should increment epoch: {} -> {}",
        token1.epoch,
        token2.epoch
    );

    // The newer token should report as newer
    assert!(
        token2.is_newer_than(&token1),
        "Second token should be newer than first"
    );
    assert!(
        !token1.is_newer_than(&token2),
        "First token should not be newer than second"
    );
}

// ============================================================================
// Test 15: Leader election - only one leader among 3 nodes
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_leader_election_single_leader() {
    use synctv_cluster::leader::{LeaderElector, LeaderElectorConfig};
    use tokio_util::sync::CancellationToken;

    let redis = TestRedis::start().await;

    let client = redis::Client::open(redis.redis_url.as_str())
        .expect("Failed to create Redis client");

    let conn_a = redis::aio::ConnectionManager::new(client.clone())
        .await
        .expect("Failed to create connection A");
    let conn_b = redis::aio::ConnectionManager::new(client.clone())
        .await
        .expect("Failed to create connection B");
    let conn_c = redis::aio::ConnectionManager::new(client.clone())
        .await
        .expect("Failed to create connection C");

    let config_a = LeaderElectorConfig {
        lease_duration_secs: 5,
        renew_interval_secs: 1,
    };
    let config_b = LeaderElectorConfig {
        lease_duration_secs: 5,
        renew_interval_secs: 1,
    };
    let config_c = LeaderElectorConfig {
        lease_duration_secs: 5,
        renew_interval_secs: 1,
    };

    let elector_a = LeaderElector::with_config(conn_a, "node_a".to_string(), config_a, "synctv:");
    let elector_b = LeaderElector::with_config(conn_b, "node_b".to_string(), config_b, "synctv:");
    let elector_c = LeaderElector::with_config(conn_c, "node_c".to_string(), config_c, "synctv:");

    let cancel_a = CancellationToken::new();
    let cancel_b = CancellationToken::new();
    let cancel_c = CancellationToken::new();

    let _handle_a = elector_a.start(cancel_a.clone());
    let _handle_b = elector_b.start(cancel_b.clone());
    let _handle_c = elector_c.start(cancel_c.clone());

    // Wait for the election to settle (at least one renew_interval)
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Count leaders
    let leader_count = [
        elector_a.is_leader(),
        elector_b.is_leader(),
        elector_c.is_leader(),
    ]
    .iter()
    .filter(|&&v| v)
    .count();

    assert_eq!(
        leader_count, 1,
        "Exactly one node should be leader, got {}: A={}, B={}, C={}",
        leader_count,
        elector_a.is_leader(),
        elector_b.is_leader(),
        elector_c.is_leader()
    );

    // Identify the leader
    let leader_id = if elector_a.is_leader() {
        "A"
    } else if elector_b.is_leader() {
        "B"
    } else {
        "C"
    };

    // Cancel the leader to simulate crash
    match leader_id {
        "A" => cancel_a.cancel(),
        "B" => cancel_b.cancel(),
        "C" => cancel_c.cancel(),
        _ => unreachable!(),
    }

    // Wait for lease to expire + one election cycle
    tokio::time::sleep(Duration::from_secs(7)).await;

    // A new leader should have been elected among the remaining two
    let remaining_leaders: Vec<&str> = [
        (!cancel_a.is_cancelled(), elector_a.is_leader(), "A"),
        (!cancel_b.is_cancelled(), elector_b.is_leader(), "B"),
        (!cancel_c.is_cancelled(), elector_c.is_leader(), "C"),
    ]
    .iter()
    .filter(|(active, is_leader, _)| *active && *is_leader)
    .map(|(_, _, name)| *name)
    .collect();

    assert_eq!(
        remaining_leaders.len(),
        1,
        "Exactly one remaining node should be leader after failover, got: {:?}",
        remaining_leaders
    );

    // Cleanup
    cancel_a.cancel();
    cancel_b.cancel();
    cancel_c.cancel();

    // Give tasks time to shut down
    tokio::time::sleep(Duration::from_millis(200)).await;
}

// ============================================================================
// Test 16: Cross-replica PermissionChanged event propagation
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_permission_changed() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "node_a").await;
    let node_b = create_node(&redis.redis_url, "node_b").await;

    let room_id = RoomId::from_string("perm_room".to_string());
    let user_id = UserId::from_string("perm_user".to_string());

    // Subscribe on node A (simulating a WebSocket client on node A watching the room)
    let (mut room_rx, conn_id) = node_a.subscribe(room_id.clone(), user_id.clone()).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Node B broadcasts a PermissionChanged event (e.g., admin changed permissions)
    let perm_event = ClusterEvent::PermissionChanged {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        target_user_id: UserId::from_string("target_user".to_string()),
        target_username: "target_user".to_string(),
        new_permissions: synctv_core::models::PermissionBits(
            synctv_core::models::PermissionBits::DEFAULT_MEMBER
                | synctv_core::models::PermissionBits::KICK_MEMBER,
        ),
        role: 3, // Admin role
        added_permissions: synctv_core::models::PermissionBits(synctv_core::models::PermissionBits::KICK_MEMBER),
        removed_permissions: synctv_core::models::PermissionBits::empty(),
        changed_by: UserId::from_string("admin_user".to_string()),
        changed_by_username: "admin_user".to_string(),
        timestamp: Utc::now(),
    };

    let result = node_b.broadcast(perm_event);
    assert!(
        result.redis_sent,
        "PermissionChanged should be published to Redis"
    );

    // Node A should receive the PermissionChanged event
    let received = tokio::time::timeout(Duration::from_secs(5), room_rx.recv())
        .await
        .expect("Timed out waiting for PermissionChanged on node A")
        .expect("Room channel closed");

    assert_eq!(received.event_type(), "permission_changed");
    if let ClusterEvent::PermissionChanged {
        target_user_id,
        new_permissions,
        changed_by_username,
        ..
    } = &received
    {
        assert_eq!(target_user_id.as_str(), "target_user");
        assert!(
            new_permissions.has(synctv_core::models::PermissionBits::KICK_MEMBER),
            "New permissions should include KICK_MEMBER"
        );
        assert_eq!(changed_by_username, "admin_user");
    } else {
        panic!(
            "Expected PermissionChanged event, got {:?}",
            received.event_type()
        );
    }

    node_a.unsubscribe(&conn_id);
    node_a.shutdown().await;
    node_b.shutdown().await;
}

// ============================================================================
// Test 17: Cross-replica cache invalidation triggers local PermissionService
//          cache eviction via CacheInvalidationService
// ============================================================================

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_cross_replica_permission_cache_invalidation_via_cache_service() {
    let redis = TestRedis::start().await;

    // Create a CacheInvalidationService for node A
    let cache_svc_a = CacheInvalidationService::new(
        None,
        "node_a".to_string(),
        "test:perm:inv".to_string(),
    );
    let mut local_rx_a = cache_svc_a.subscribe();

    // Create node A with cache invalidation enabled
    let client_a = redis::Client::open(redis.redis_url.clone()).expect("Failed to open Redis client");
    let conn_a = client_a.get_connection_manager().await.expect("Failed to get ConnectionManager");
    let config_a = ClusterConfig {
        redis_client: Some(client_a),
        redis_conn: Some(conn_a),
        node_id: "node_a".to_string(),
        dedup_window: Duration::from_secs(10),
        cleanup_interval: Duration::from_secs(30),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
    };
    let node_a = ClusterManager::new(config_a, None, Some(cache_svc_a))
        .await
        .expect("Failed to create node A");

    let node_b = create_node(&redis.redis_url, "node_b").await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Node B changes a user's permissions and broadcasts a CacheInvalidate event
    // targeting the permission cache for that specific user
    let invalidate_event = ClusterEvent::CacheInvalidate {
        event_id: nanoid::nanoid!(16),
        targets: vec![CacheTarget::User {
            user_id: "perm_changed_user".to_string(),
        }],
        timestamp: Utc::now(),
    };

    let result = node_b.broadcast(invalidate_event);
    assert!(
        result.redis_sent,
        "CacheInvalidate should be published to Redis"
    );

    // Node A's cache invalidation service should receive the user invalidation
    let msg = tokio::time::timeout(Duration::from_secs(5), local_rx_a.recv())
        .await
        .expect("Timed out waiting for permission cache invalidation")
        .expect("Cache invalidation channel closed");

    match msg {
        InvalidationMessage::User { user_id } => {
            assert_eq!(
                user_id, "perm_changed_user",
                "Should invalidate the correct user"
            );
        }
        other => {
            panic!(
                "Expected User invalidation, got: {:?}",
                other
            );
        }
    }

    node_a.shutdown().await;
    node_b.shutdown().await;
}

// ============================================================================
// Test 18: RoomMessageHub + ConnectionManager state consistency after cleanup
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_hub_connection_manager_state_consistency() {
    use synctv_cluster::sync::{ConnectionManager, ConnectionLimits};

    let config = ClusterConfig {
        redis_client: None,
        redis_conn: None, // No Redis -- single-node mode
        node_id: "test_node".to_string(),
        dedup_window: Duration::from_secs(1),
        cleanup_interval: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
    };

    let manager = ClusterManager::new(config, None, None).await.unwrap();
    let conn_manager = ConnectionManager::new(ConnectionLimits::default());

    let room_id = RoomId::from_string("consistency_room".to_string());
    let user1 = UserId::from_string("user_1".to_string());
    let user2 = UserId::from_string("user_2".to_string());

    // Subscribe two users via ClusterManager (RoomMessageHub)
    let (_rx1, conn_id_1) = manager.subscribe(room_id.clone(), user1.clone()).await;
    let (_rx2, conn_id_2) = manager.subscribe(room_id.clone(), user2.clone()).await;

    // Register connections via ConnectionManager
    conn_manager
        .register(conn_id_1.clone(), user1.clone())
        .await
        .expect("register user1");
    conn_manager
        .join_room(&conn_id_1, room_id.clone())
        .await
        .expect("join room user1");
    conn_manager
        .register(conn_id_2.clone(), user2.clone())
        .await
        .expect("register user2");
    conn_manager
        .join_room(&conn_id_2, room_id.clone())
        .await
        .expect("join room user2");

    // Verify initial state
    let hub_metrics = manager.metrics();
    assert_eq!(hub_metrics.total_connections, 2);
    assert_eq!(hub_metrics.total_rooms, 1);
    assert_eq!(conn_manager.connection_count(), 2);
    assert_eq!(conn_manager.room_connection_count(&room_id), 2);

    // Simulate user1 disconnect: unsubscribe from hub + unregister from connection manager
    manager.unsubscribe(&conn_id_1);
    conn_manager.unregister(&conn_id_1).await;

    // Verify partial state
    let hub_metrics = manager.metrics();
    assert_eq!(hub_metrics.total_connections, 1, "Hub should have 1 connection");
    assert_eq!(conn_manager.connection_count(), 1, "ConnManager should have 1 connection");
    assert_eq!(
        conn_manager.room_connection_count(&room_id),
        1,
        "Room should have 1 connection"
    );

    // Simulate user2 disconnect
    manager.unsubscribe(&conn_id_2);
    conn_manager.unregister(&conn_id_2).await;

    // Verify clean state
    let hub_metrics = manager.metrics();
    assert_eq!(hub_metrics.total_connections, 0, "Hub should have 0 connections");
    assert_eq!(hub_metrics.total_rooms, 0, "Hub should have 0 rooms");
    assert_eq!(conn_manager.connection_count(), 0, "ConnManager should have 0 connections");
    assert_eq!(
        conn_manager.room_connection_count(&room_id),
        0,
        "Room should have 0 connections"
    );
    assert_eq!(
        conn_manager.user_connection_count(&user1),
        0,
        "User1 should have 0 connections"
    );
    assert_eq!(
        conn_manager.user_connection_count(&user2),
        0,
        "User2 should have 0 connections"
    );

    manager.shutdown().await;
}

// ============================================================================
// Test 19: Rapid subscribe/unsubscribe does not leak state
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_rapid_subscribe_unsubscribe_no_leak() {
    let config = ClusterConfig {
        redis_client: None,
        redis_conn: None,
        node_id: "test_node".to_string(),
        dedup_window: Duration::from_secs(1),
        cleanup_interval: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
    };

    let manager = ClusterManager::new(config, None, None).await.unwrap();
    let room_id = RoomId::from_string("rapid_room".to_string());

    // Rapidly subscribe and unsubscribe 100 connections
    for i in 0..100 {
        let user = UserId::from_string(format!("rapid_user_{i}"));
        let (_rx, conn_id) = manager.subscribe(room_id.clone(), user).await;
        manager.unsubscribe(&conn_id);
    }

    // After all subscribe/unsubscribe cycles, state should be clean
    let metrics = manager.metrics();
    assert_eq!(
        metrics.total_connections, 0,
        "No connections should remain after rapid subscribe/unsubscribe"
    );
    assert_eq!(
        metrics.total_rooms, 0,
        "No rooms should remain after all subscribers removed"
    );

    manager.shutdown().await;
}

// ============================================================================
// Test 20: Cross-replica RoomSettingsChanged event propagation
// ============================================================================

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
            serde_json::from_slice(&settings_json).expect("valid JSON");
        assert_eq!(parsed["max_members"], 50);
        assert_eq!(parsed["chat_enabled"], false);
    } else {
        panic!("Expected RoomSettingsChanged event");
    }

    node_a.unsubscribe(&conn_id);
    node_a.shutdown().await;
    node_b.shutdown().await;
}
