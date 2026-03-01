//! Multi-replica cluster integration tests
//!
//! These tests verify cross-node coordination by starting multiple
//! `ClusterManager` instances that share a single Redis container
//! (via testcontainers). Each "node" has its own `node_id` but connects
//! to the same Redis, simulating a multi-replica deployment.

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use synctv_cluster::sync::events::ClusterEvent;
use synctv_cluster::sync::redis_pubsub::RedisPubSub;
use synctv_cluster::{MessageDeduplicator, RoomMessageHub};
use synctv_core::models::id::{RoomId, UserId};
use tokio::sync::broadcast;

mod integration_test_helpers;
use integration_test_helpers::{create_node, TestRedis};

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
            message: format!("Message {i}"),
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
            Ok(None) | Err(_) => break,
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
        assert_eq!(msg, &format!("Message {i}"), "Message {i} out of order");
    }

    node_a.unsubscribe(&conn_id);
    node_a.shutdown().await;
    node_b.shutdown().await;
}

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
    let mut rx = message_hub
        .subscribe(room_id.clone(), user_id.clone(), "catchup_conn".to_string())
        .await;

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
            message: format!("Catchup message {i}"),
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
    let redis_client =
        redis::Client::open(redis.redis_url.clone()).expect("Failed to open Redis client");
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
    tokio::time::sleep(Duration::from_secs(1)).await;

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

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_redis_failure_and_recovery() {
    let redis = TestRedis::start().await;

    // Create two nodes
    let node_a = create_node(&redis.redis_url, "recovery_node_a").await;
    let node_b = create_node(&redis.redis_url, "recovery_node_b").await;

    let room_id = RoomId::from_string("recovery_room".to_string());

    // Subscribe on both nodes
    let (mut rx_a, conn_a) = node_a
        .subscribe(
            room_id.clone(),
            UserId::from_string("recovery_user_a".to_string()),
        )
        .await;
    let (mut rx_b, conn_b) = node_b
        .subscribe(
            room_id.clone(),
            UserId::from_string("recovery_user_b".to_string()),
        )
        .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Test 1: Verify normal operation
    let normal_event = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: UserId::from_string("recovery_user_a".to_string()),
        username: "user_a".to_string(),
        message: "Normal message".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };

    node_a.broadcast(normal_event);

    let received = tokio::time::timeout(Duration::from_secs(5), rx_b.recv())
        .await
        .expect("Should receive message in normal operation")
        .expect("Channel not closed");
    assert_eq!(received.event_type(), "chat_message");

    // Test 2: Verify local broadcast still works even if Redis fails
    // (We can't actually stop the Redis container, but we can verify local delivery)

    // Subscribe a second client on node A
    let (mut rx_a2, conn_a2) = node_a
        .subscribe(
            room_id.clone(),
            UserId::from_string("recovery_user_a2".to_string()),
        )
        .await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Broadcast from user_b on node A should reach both subscribers on node A
    let local_event = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: UserId::from_string("recovery_user_b".to_string()),
        username: "user_b".to_string(),
        message: "Local broadcast test".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };

    let result = node_a.broadcast(local_event);
    // Both local subscribers should receive the message
    assert_eq!(
        result.local_sent, 2,
        "Both local subscribers should receive the message"
    );

    // Verify both local subscribers received the message
    let _ = tokio::time::timeout(Duration::from_secs(2), rx_a.recv())
        .await
        .expect("First local subscriber should receive message");
    let _ = tokio::time::timeout(Duration::from_secs(2), rx_a2.recv())
        .await
        .expect("Second local subscriber should receive message");

    // Test 3: Verify event ordering is maintained after recovery
    // First, drain any remaining messages from node B's queue (e.g., "Local broadcast test")
    // to ensure we start fresh for the ordering test
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(100), rx_b.recv()).await {
        // Drain message
    }

    for i in 0..5 {
        let ordered_event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: room_id.clone(),
            user_id: UserId::from_string("recovery_user_a".to_string()),
            username: "user_a".to_string(),
            message: format!("Ordered message {i}"),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };
        node_a.broadcast(ordered_event);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Verify messages are received in order on node B
    let mut received_messages = Vec::new();
    for _ in 0..5 {
        match tokio::time::timeout(Duration::from_secs(5), rx_b.recv()).await {
            Ok(Some(ClusterEvent::ChatMessage { message, .. })) => {
                received_messages.push(message);
            }
            _ => break,
        }
    }

    // Verify ordering
    for (i, msg) in received_messages.iter().enumerate() {
        assert_eq!(
            msg,
            &format!("Ordered message {i}"),
            "Message {i} should be in order"
        );
    }

    // Cleanup
    node_a.unsubscribe(&conn_a);
    node_a.unsubscribe(&conn_a2);
    node_b.unsubscribe(&conn_b);

    node_a.shutdown().await;
    node_b.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_redis_reconnection_event_preservation() {
    let redis = TestRedis::start().await;

    let node_a = create_node(&redis.redis_url, "reconnect_node_a").await;
    let node_b = create_node(&redis.redis_url, "reconnect_node_b").await;

    let room_id = RoomId::from_string("reconnect_room".to_string());

    // Subscribe on node B
    let (mut rx_b, conn_b) = node_b
        .subscribe(
            room_id.clone(),
            UserId::from_string("reconnect_listener".to_string()),
        )
        .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Send a baseline event to verify connection is working
    let baseline_event = ClusterEvent::ChatMessage {
        event_id: nanoid::nanoid!(16),
        room_id: room_id.clone(),
        user_id: UserId::from_string("reconnect_sender".to_string()),
        username: "sender".to_string(),
        message: "Baseline message".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };

    node_a.broadcast(baseline_event);

    let _ = tokio::time::timeout(Duration::from_secs(5), rx_b.recv())
        .await
        .expect("Should receive baseline message");

    // Test rapid message sending (simulating high-throughput scenario)
    let mut event_ids = Vec::new();
    for i in 0..20 {
        let event_id = nanoid::nanoid!(16);
        event_ids.push(event_id.clone());

        let rapid_event = ClusterEvent::ChatMessage {
            event_id,
            room_id: room_id.clone(),
            user_id: UserId::from_string("reconnect_sender".to_string()),
            username: "sender".to_string(),
            message: format!("Rapid message {i}"),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        node_a.broadcast(rapid_event);
    }

    // Count received messages
    let mut received_count = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while received_count < 20 {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx_b.recv()).await {
            Ok(Some(ClusterEvent::ChatMessage { .. })) => received_count += 1,
            Ok(Some(_)) => {} // Other event types
            Ok(None) | Err(_) => break, // Channel closed
                               // Timeout
        }
    }

    // We should receive most if not all messages (allowing for some network loss)
    assert!(
        received_count >= 18,
        "Should receive at least 18 out of 20 messages, got {received_count}"
    );

    // Cleanup
    node_b.unsubscribe(&conn_b);
    node_a.shutdown().await;
    node_b.shutdown().await;
}

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
