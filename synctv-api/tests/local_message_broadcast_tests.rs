//! Tests for local message broadcasting without ClusterManager (TDD)
//!
//! These tests verify that:
//! 1. message_stream works without ClusterManager (no Redis)
//! 2. Local messages are correctly broadcast to subscribers
//! 3. Multiple subscribers all receive messages
//! 4. Local ClusterManager is lazily created when needed
//!
//! Issue: In non-cluster mode (without Redis), message_stream gRPC endpoint
//! previously returned an error. Now it creates a local ClusterManager fallback.

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use std::time::Duration;

use synctv_cluster::sync::{ClusterConfig, ClusterManager, ConnectionLimits, ConnectionManager};
use synctv_core::models::{RoomId, UserId};

// ============================================================================
// Test: ClusterManager works in single-node mode (no Redis)
// ============================================================================

#[tokio::test]
async fn test_cluster_manager_single_node_mode_works() {
    // Create ClusterManager without Redis - should work in single-node mode
    let config = ClusterConfig {
        redis_client: None,
        redis_conn: None,
        shared_redis_conn: None,
        cluster_enabled: false,
        node_id: "test_local_node".to_string(),
        dedup_window: Duration::from_mins(1),
        cleanup_interval: Duration::from_secs(10),
        critical_channel_capacity: 100,
        publish_channel_capacity: 1000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 1000,
        parent_cancel_token: None,
    };

    let manager = ClusterManager::new(config, None, None)
        .await
        .expect("ClusterManager::new should succeed without Redis");

    // Verify metrics show Redis is not enabled
    let metrics = manager.metrics();
    assert!(
        !metrics.redis_enabled,
        "Metrics should show Redis is not enabled in single-node mode"
    );
}

// ============================================================================
// Test: Local subscription and broadcast without Redis
// ============================================================================

#[tokio::test]
async fn test_local_subscribe_and_broadcast() {
    // Create ClusterManager without Redis
    let config = ClusterConfig {
        redis_client: None,
        redis_conn: None,
        shared_redis_conn: None,
        cluster_enabled: false,
        node_id: "test_local_broadcast".to_string(),
        dedup_window: Duration::from_mins(1),
        cleanup_interval: Duration::from_secs(10),
        critical_channel_capacity: 100,
        publish_channel_capacity: 1000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 1000,
        parent_cancel_token: None,
    };

    let manager = ClusterManager::new(config, None, None)
        .await
        .expect("ClusterManager::new should succeed");

    let room_id = RoomId::from_string("test_room".to_string());
    let user_id = UserId::from_string("user1".to_string());

    // Subscribe to room
    let (mut rx, conn_id) = manager.subscribe(room_id.clone(), user_id.clone()).await;

    // Broadcast a chat message
    use chrono::Utc;
    use synctv_cluster::sync::ClusterEvent;

    let event = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: user_id.clone(),
        username: "test_user".to_string(),
        message: "Hello local!".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
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

    // Verify message is received
    let received = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("Should receive message within timeout")
        .expect("Should have a message");

    match received {
        ClusterEvent::ChatMessage { message, .. } => {
            assert_eq!(message, "Hello local!");
        }
        _ => panic!("Expected ChatMessage event"),
    }

    // Cleanup
    manager.unsubscribe(&conn_id);
}

// ============================================================================
// Test: Multiple subscribers all receive broadcasts
// ============================================================================

#[tokio::test]
async fn test_multiple_subscribers_receive_broadcasts() {
    // Create ClusterManager without Redis
    let config = ClusterConfig {
        redis_client: None,
        redis_conn: None,
        shared_redis_conn: None,
        cluster_enabled: false,
        node_id: "test_multi_subscribers".to_string(),
        dedup_window: Duration::from_mins(1),
        cleanup_interval: Duration::from_secs(10),
        critical_channel_capacity: 100,
        publish_channel_capacity: 1000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 1000,
        parent_cancel_token: None,
    };

    let manager = Arc::new(
        ClusterManager::new(config, None, None)
            .await
            .expect("ClusterManager::new should succeed"),
    );

    let room_id = RoomId::from_string("shared_room".to_string());

    // Create 3 subscribers for the same room
    let mut subscribers = Vec::new();
    for i in 0..3 {
        let user_id = UserId::from_string(format!("user_{i}"));
        let (rx, conn_id) = manager.subscribe(room_id.clone(), user_id).await;
        subscribers.push((rx, conn_id));
    }

    // Broadcast a message
    use chrono::Utc;
    use synctv_cluster::sync::ClusterEvent;

    let event = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: UserId::from_string("broadcaster".to_string()),
        username: "broadcaster".to_string(),
        message: "Hello everyone!".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };

    let result = manager.broadcast(event);
    assert_eq!(
        result.local_sent, 3,
        "Local broadcast should reach all 3 subscribers"
    );

    // Verify all subscribers receive the message
    for (i, (rx, _)) in subscribers.iter_mut().enumerate() {
        let received = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("Subscriber {i} should receive message within timeout"))
            .expect("Should have a message");

        match received {
            ClusterEvent::ChatMessage { message, .. } => {
                assert_eq!(
                    message, "Hello everyone!",
                    "Subscriber {i} should receive correct message"
                );
            }
            _ => panic!("Subscriber {i} expected ChatMessage event"),
        }
    }

    // Cleanup
    for (_, conn_id) in subscribers {
        manager.unsubscribe(&conn_id);
    }
}

// ============================================================================
// Test: ConnectionManager works without ClusterManager
// ============================================================================

#[tokio::test]
async fn test_connection_manager_works_standalone() {
    // Create ConnectionManager without ClusterManager
    let limits = ConnectionLimits::default();
    let conn_manager = ConnectionManager::new(limits);

    let user_id = UserId::from_string("user1".to_string());
    let conn_id = format!("conn_{}", nanoid::nanoid!(8));

    // Register connection
    conn_manager
        .register(conn_id.clone(), user_id.clone())
        .await
        .expect("Should register connection");

    // Verify connection is tracked
    assert_eq!(conn_manager.connection_count(), 1);

    // Unregister
    conn_manager.unregister(&conn_id).await;
    assert_eq!(conn_manager.connection_count(), 0);
}

// ============================================================================
// Test: LocalMessageBroadcaster component for fallback
// ============================================================================

/// LocalMessageBroadcaster provides local-only message broadcasting
/// when ClusterManager is not available (no Redis).
///
/// This is a lightweight wrapper around RoomMessageHub that provides
/// the same interface as ClusterManager for local operations.
pub struct LocalMessageBroadcaster {
    message_hub: Arc<synctv_cluster::sync::RoomMessageHub>,
    #[allow(dead_code)]
    connection_manager: ConnectionManager,
}

impl LocalMessageBroadcaster {
    /// Create a new local message broadcaster
    #[must_use]
    pub fn new() -> Self {
        Self {
            message_hub: Arc::new(synctv_cluster::sync::RoomMessageHub::new()),
            connection_manager: ConnectionManager::new(ConnectionLimits::default()),
        }
    }

    /// Subscribe to room events
    pub async fn subscribe(
        &self,
        room_id: RoomId,
        user_id: UserId,
    ) -> (
        tokio::sync::mpsc::Receiver<synctv_cluster::sync::ClusterEvent>,
        String,
    ) {
        let connection_id = format!("{}_{}", user_id.as_str(), nanoid::nanoid!(8));
        let rx = self
            .message_hub
            .subscribe(room_id, user_id, connection_id.clone())
            .await;
        (rx, connection_id)
    }

    /// Broadcast an event to room subscribers
    pub fn broadcast(&self, room_id: &RoomId, event: synctv_cluster::sync::ClusterEvent) -> usize {
        self.message_hub.broadcast(room_id, event)
    }

    /// Unsubscribe from room events
    pub fn unsubscribe(&self, connection_id: &str) {
        self.message_hub.unsubscribe(connection_id);
    }
}

impl Default for LocalMessageBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

#[tokio::test]
async fn test_local_message_broadcaster_basic() {
    let broadcaster = LocalMessageBroadcaster::new();
    let room_id = RoomId::from_string("test_room".to_string());
    let user_id = UserId::from_string("user1".to_string());

    // Subscribe
    let (mut rx, conn_id) = broadcaster.subscribe(room_id.clone(), user_id).await;

    // Broadcast
    use chrono::Utc;
    use synctv_cluster::sync::ClusterEvent;

    let event = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: UserId::from_string("user1".to_string()),
        username: "user1".to_string(),
        message: "Hello from local!".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };

    let sent = broadcaster.broadcast(&room_id, event);
    assert_eq!(sent, 1, "Should send to 1 subscriber");

    // Receive
    let received = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("Should receive message")
        .expect("Should have message");

    match received {
        ClusterEvent::ChatMessage { message, .. } => {
            assert_eq!(message, "Hello from local!");
        }
        _ => panic!("Expected ChatMessage"),
    }

    // Cleanup
    broadcaster.unsubscribe(&conn_id);
}

#[tokio::test]
async fn test_local_message_broadcaster_multiple_subscribers() {
    let broadcaster = LocalMessageBroadcaster::new();
    let room_id = RoomId::from_string("shared_room".to_string());

    // Create 3 subscribers
    let mut receivers = Vec::new();
    let mut conn_ids = Vec::new();
    for i in 0..3 {
        let user_id = UserId::from_string(format!("user_{i}"));
        let (rx, conn_id) = broadcaster.subscribe(room_id.clone(), user_id).await;
        receivers.push(rx);
        conn_ids.push(conn_id);
    }

    // Broadcast
    use chrono::Utc;
    use synctv_cluster::sync::ClusterEvent;

    let event = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: UserId::from_string("broadcaster".to_string()),
        username: "broadcaster".to_string(),
        message: "Broadcast to all".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };

    let sent = broadcaster.broadcast(&room_id, event);
    assert_eq!(sent, 3, "Should send to all 3 subscribers");

    // All should receive
    for (i, rx) in receivers.iter_mut().enumerate() {
        let received = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("Subscriber {i} should receive"))
            .expect("Should have message");

        match received {
            ClusterEvent::ChatMessage { message, .. } => {
                assert_eq!(message, "Broadcast to all");
            }
            _ => panic!("Subscriber {i} expected ChatMessage"),
        }
    }

    // Cleanup
    for conn_id in conn_ids {
        broadcaster.unsubscribe(&conn_id);
    }
}

// ============================================================================
// Test: Verify local ClusterManager is lazily created
// ============================================================================

/// Test that a local ClusterManager can be created on-demand.
/// This is what ClientServiceImpl.get_cluster_manager() does internally.
#[tokio::test]
async fn test_lazy_cluster_manager_creation() {
    use tokio::sync::OnceCell;

    // Simulate the lazy creation pattern used in ClientServiceImpl
    let local_cluster_manager: Arc<OnceCell<Arc<ClusterManager>>> = Arc::new(OnceCell::new());

    // First call should create the ClusterManager
    let cm1 = local_cluster_manager
        .get_or_init(|| async {
            let config = ClusterConfig {
                redis_client: None,
                redis_conn: None,
                shared_redis_conn: None,
                cluster_enabled: false,
                node_id: format!("local_{}", nanoid::nanoid!(8)),
                dedup_window: Duration::from_mins(1),
                cleanup_interval: Duration::from_secs(10),
                critical_channel_capacity: 100,
                publish_channel_capacity: 1000,
                key_prefix: "synctv:".to_string(),
                catchup_window_secs: 300,
                stream_max_length: 1000,
                parent_cancel_token: None,
            };
            Arc::new(
                ClusterManager::new(config, None, None)
                    .await
                    .expect("ClusterManager::new should succeed without Redis"),
            )
        })
        .await;

    // Second call should return the same instance
    let cm2 = local_cluster_manager
        .get_or_init(|| async {
            // This should not be called
            panic!("Should not create a second ClusterManager");
        })
        .await;

    // Both should be the same instance
    assert!(
        Arc::ptr_eq(cm1, cm2),
        "Should return the same ClusterManager instance"
    );

    // Verify it works
    let metrics = cm1.metrics();
    assert!(
        !metrics.redis_enabled,
        "Local ClusterManager should not have Redis enabled"
    );
}

// ============================================================================
// Test: Local ClusterManager supports subscription and broadcast
// ============================================================================

/// Test that a locally-created ClusterManager supports all necessary operations.
#[tokio::test]
async fn test_local_cluster_manager_supports_room_operations() {
    // Create local ClusterManager
    let config = ClusterConfig {
        redis_client: None,
        redis_conn: None,
        shared_redis_conn: None,
        cluster_enabled: false,
        node_id: "test_local_ops".to_string(),
        dedup_window: Duration::from_mins(1),
        cleanup_interval: Duration::from_secs(10),
        critical_channel_capacity: 100,
        publish_channel_capacity: 1000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 1000,
        parent_cancel_token: None,
    };

    let manager = Arc::new(
        ClusterManager::new(config, None, None)
            .await
            .expect("ClusterManager::new should succeed"),
    );

    let room_id = RoomId::from_string("test_room".to_string());
    let user_id = UserId::from_string("user1".to_string());

    // Test subscribe
    let (mut rx, conn_id) = manager.subscribe(room_id.clone(), user_id.clone()).await;

    // Test broadcast
    use chrono::Utc;
    use synctv_cluster::sync::ClusterEvent;
    use synctv_core::models::PermissionBits;

    let event = ClusterEvent::UserJoined {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: user_id.clone(),
        username: "user1".to_string(),
        permissions: PermissionBits(0),
        role: 2, // Member role
        added_permissions: PermissionBits(0),
        removed_permissions: PermissionBits(0),
        admin_added_permissions: PermissionBits(0),
        admin_removed_permissions: PermissionBits(0),
        joined_at: Utc::now(),
        timestamp: Utc::now(),
    };

    let result = manager.broadcast(event.clone());
    assert_eq!(result.local_sent, 1, "Should send to 1 subscriber");

    // Test receive
    let received = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("Should receive event")
        .expect("Should have event");

    match received {
        ClusterEvent::UserJoined { username, .. } => {
            assert_eq!(username, "user1");
        }
        _ => panic!("Expected UserJoined event"),
    }

    // Test unsubscribe
    manager.unsubscribe(&conn_id);

    // Verify no more subscribers
    let result = manager.broadcast(event);
    assert_eq!(
        result.local_sent, 0,
        "Should send to 0 subscribers after unsubscribe"
    );
}
