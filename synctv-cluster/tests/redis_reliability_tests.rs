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
use integration_test_helpers::{
    broadcast_until_all_clients_receive, broadcast_until_room_event, create_node_with_prefix,
    TestRedis,
};

async fn wait_for_stream_len(redis_url: &str, stream_key: &str, expected_min_len: usize) {
    let client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

    loop {
        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .expect("Failed to open Redis connection");
        let len: usize = redis::cmd("XLEN")
            .arg(stream_key)
            .query_async(&mut conn)
            .await
            .expect("Failed to query stream length");

        if len >= expected_min_len {
            return;
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for stream {stream_key} to reach length {expected_min_len}; current length {len}"
        );

        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_redis_pubsub_no_message_loss() {
    let redis = TestRedis::start().await;
    let key_prefix = redis.key_prefix.clone();

    let node_a = create_node_with_prefix(&redis.redis_url, "node_a", key_prefix.clone()).await;
    let node_b = create_node_with_prefix(&redis.redis_url, "node_b", key_prefix).await;

    let room_id = RoomId::expect_positive(10_000_056);
    let user_id = UserId::expect_positive(10_000_026);

    // Subscribe on node A and establish the cross-replica subscription path first.
    let (rx, conn_id) = node_a
        .subscribe(room_id, user_id)
        .await
        .expect("subscribe should succeed");
    let mut baseline_clients = vec![(rx, conn_id.clone())];
    broadcast_until_all_clients_receive(
        &node_b,
        &mut baseline_clients,
        "baseline no-loss message",
        || ClusterEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_057),
            username: "sender".to_string(),
            message: "baseline no-loss message".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        },
        "baseline no-loss message",
    )
    .await;
    let (mut room_rx, _baseline_conn_id) = baseline_clients.pop().expect("baseline client");

    let message_count = 20;
    for i in 0..message_count {
        let event = ClusterEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_057),
            username: "sender".to_string(),
            message: format!("Message {i}"),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };
        node_b.broadcast(event);
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
    let key_prefix = redis.key_prefix.clone();

    // Use raw RedisPubSub to test catchup mechanism directly
    let message_hub = Arc::new(RoomMessageHub::new());
    let (admin_tx, _admin_rx) = broadcast::channel::<ClusterEvent>(256);
    let dedup = Arc::new(MessageDeduplicator::with_defaults());

    let room_id = RoomId::expect_positive(10_000_058);
    let user_id = UserId::expect_positive(10_000_059);

    // Subscribe a user to the room in the hub
    let mut rx = message_hub
        .subscribe(room_id, user_id, "catchup_conn".to_string())
        .await
        .expect("subscribe should succeed");

    let publisher =
        create_node_with_prefix(&redis.redis_url, "publisher_node", key_prefix.clone()).await;

    // Publish events to Redis (they go into streams via dual-write)
    for i in 0..5 {
        let event = ClusterEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_060),
            username: "publisher".to_string(),
            message: format!("Catchup message {i}"),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };
        publisher.broadcast(event);
    }

    wait_for_stream_len(
        &redis.redis_url,
        &format!("{key_prefix}room:{room_id}:events"),
        5,
    )
    .await;

    // Now start a subscriber node that connects to the same Redis.
    // On first connect it should replay recent stream history within the
    // configured catch-up window so new replicas bootstrap with current state.
    let redis_client =
        redis::Client::open(redis.redis_url.clone()).expect("Failed to open Redis client");
    let subscriber_node = Arc::new(
        RedisPubSub::with_key_prefix(
            synctv_core::coordination_runtime_from_client(redis_client),
            message_hub.clone(),
            "subscriber_node".to_string(),
            &key_prefix,
            admin_tx,
            None,
            None,
            dedup,
            300,
            10_000,
        )
        .expect("Failed to create subscriber RedisPubSub"),
    );

    // Clone Arc before start() consumes it, so we can call shutdown() later
    let subscriber_for_shutdown = subscriber_node.clone();
    let _sub_tx = subscriber_node
        .start(10_000)
        .await
        .expect("Failed to start subscriber");

    let mut historical_messages = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while historical_messages.len() < 5 && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Some(ClusterEvent::ChatMessage { message, .. })) => {
                historical_messages.push(message);
            }
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => panic!("room channel closed unexpectedly during catch-up"),
        }
    }
    assert_eq!(
        historical_messages.len(),
        5,
        "first connect should replay recent stream entries from the catch-up window"
    );
    for i in 0..5 {
        assert!(
            historical_messages.contains(&format!("Catchup message {i}")),
            "missing catch-up message {i}: {historical_messages:?}"
        );
    }

    // Publish one more message (should be received live)
    let received = broadcast_until_room_event(
        &publisher,
        &mut rx,
        || ClusterEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_060),
            username: "publisher".to_string(),
            message: "Live message after subscriber connect".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        },
        |event| {
            matches!(event, ClusterEvent::ChatMessage { message, .. }
                if message == "Live message after subscriber connect")
        },
        "live message after subscriber connect",
    )
    .await;

    assert_eq!(received.event_type(), "chat_message");
    if let ClusterEvent::ChatMessage { message, .. } = &received {
        assert_eq!(message, "Live message after subscriber connect");
    }

    publisher.shutdown().await;
    subscriber_for_shutdown.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_room_stream_key_uses_ttl_for_inactive_room_cleanup() {
    let redis = TestRedis::start().await;
    let key_prefix = redis.key_prefix.clone();
    let node =
        create_node_with_prefix(&redis.redis_url, "publisher_node", key_prefix.clone()).await;

    let room_id = RoomId::expect_positive(10_000_061);
    let event = ClusterEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id,
        user_id: UserId::expect_positive(10_000_060),
        username: "publisher".to_string(),
        message: "ttl check".to_string(),
        timestamp: Utc::now(),
        position: None,
        color: None,
    };
    let _ = node.broadcast(event);

    let stream_key = format!("{key_prefix}room:{room_id}:events");
    wait_for_stream_len(&redis.redis_url, &stream_key, 1).await;

    let client =
        redis::Client::open(redis.redis_url.clone()).expect("Failed to create Redis client");
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .expect("Failed to open Redis connection");
    let ttl: i64 = redis::cmd("TTL")
        .arg(&stream_key)
        .query_async(&mut conn)
        .await
        .expect("Failed to read room stream TTL");

    assert!(
        (840..=900).contains(&ttl),
        "room stream key should use a bounded inactivity TTL, got {ttl}s"
    );

    node.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn test_redis_failure_and_recovery() {
    let redis = TestRedis::start().await;
    let key_prefix = redis.key_prefix.clone();

    let node_a =
        create_node_with_prefix(&redis.redis_url, "recovery_node_a", key_prefix.clone()).await;
    let node_b = create_node_with_prefix(&redis.redis_url, "recovery_node_b", key_prefix).await;

    let room_id = RoomId::expect_positive(10_000_062);

    // Subscribe on both nodes
    let (mut rx_a, conn_a) = node_a
        .subscribe(room_id, UserId::expect_positive(10_000_063))
        .await
        .expect("subscribe should succeed");
    let (mut rx_b, conn_b) = node_b
        .subscribe(room_id, UserId::expect_positive(10_000_064))
        .await
        .expect("subscribe should succeed");

    // Test 1: Verify normal operation and consume the local echo so later assertions
    // don't accidentally pass on stale buffered messages.
    let received = broadcast_until_room_event(
        &node_a,
        &mut rx_b,
        || ClusterEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_063),
            username: "user_a".to_string(),
            message: "Normal message".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        },
        |event| matches!(event, ClusterEvent::ChatMessage { message, .. } if message == "Normal message"),
        "normal cross-replica message",
    )
    .await;
    assert_eq!(received.event_type(), "chat_message");
    let local_received = tokio::time::timeout(Duration::from_secs(5), rx_a.recv())
        .await
        .expect("Local subscriber should receive normal message")
        .expect("Local channel not closed");
    if let ClusterEvent::ChatMessage { message, .. } = &local_received {
        assert_eq!(message, "Normal message");
    } else {
        panic!("Expected local normal ChatMessage");
    }

    // Test 2: Verify local broadcast still works even if Redis fails
    // (We can't actually stop the Redis container, but we can verify local delivery)

    // Subscribe a second client on node A
    let (mut rx_a2, conn_a2) = node_a
        .subscribe(room_id, UserId::expect_positive(10_000_065))
        .await
        .expect("subscribe should succeed");

    while let Ok(Some(ClusterEvent::ChatMessage { message, .. })) =
        tokio::time::timeout(Duration::from_millis(100), rx_a.recv()).await
    {
        assert_eq!(
            message, "Normal message",
            "unexpected buffered local message before recovery test"
        );
    }

    // Broadcast from user_b on node A should reach both subscribers on node A
    let local_event = ClusterEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id,
        user_id: UserId::expect_positive(10_000_064),
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
    let local_a = tokio::time::timeout(Duration::from_secs(2), rx_a.recv())
        .await
        .expect("First local subscriber should receive message")
        .expect("First local subscriber channel closed");
    let local_a2 = tokio::time::timeout(Duration::from_secs(2), rx_a2.recv())
        .await
        .expect("Second local subscriber should receive message")
        .expect("Second local subscriber channel closed");
    for received in [&local_a, &local_a2] {
        if let ClusterEvent::ChatMessage { message, .. } = received {
            assert_eq!(message, "Local broadcast test");
        } else {
            panic!("Expected local ChatMessage");
        }
    }

    // Test 3: Verify event ordering is maintained after recovery
    // First, drain any remaining messages from node B's queue (e.g., "Local broadcast test")
    // to ensure we start fresh for the ordering test
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(100), rx_b.recv()).await {}

    for i in 0..5 {
        let ordered_event = ClusterEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_063),
            username: "user_a".to_string(),
            message: format!("Ordered message {i}"),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };
        node_a.broadcast(ordered_event);
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
    let key_prefix = redis.key_prefix.clone();

    let node_a =
        create_node_with_prefix(&redis.redis_url, "reconnect_node_a", key_prefix.clone()).await;
    let node_b = create_node_with_prefix(&redis.redis_url, "reconnect_node_b", key_prefix).await;

    let room_id = RoomId::expect_positive(10_000_066);

    // Subscribe on node B
    let (rx_b, conn_b) = node_b
        .subscribe(room_id, UserId::expect_positive(10_000_067))
        .await
        .expect("subscribe should succeed");

    // Establish the cross-replica subscription path before asserting on burst delivery.
    // This avoids flakiness from Redis pub/sub room registration still converging.
    let mut baseline_clients = vec![(rx_b, conn_b.clone())];
    broadcast_until_all_clients_receive(
        &node_a,
        &mut baseline_clients,
        "Baseline message",
        || ClusterEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_068),
            username: "sender".to_string(),
            message: "Baseline message".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        },
        "baseline reconnect message",
    )
    .await;
    let (mut rx_b, _conn_b_again) = baseline_clients.pop().expect("baseline client");

    // Test rapid message sending (simulating high-throughput scenario)
    let mut event_ids = Vec::new();
    for i in 0..20 {
        let event_id = synctv_common::snanoid!(16);
        event_ids.push(event_id.clone());

        let rapid_event = ClusterEvent::ChatMessage {
            event_id,
            room_id,
            user_id: UserId::expect_positive(10_000_068),
            username: "sender".to_string(),
            message: format!("Rapid message {i}"),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        node_a.broadcast(rapid_event);
    }

    // Count received messages. Allow a longer deadline because Redis pub/sub
    // subscription propagation can lag under highly parallel workspace runs.
    let mut received_count = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
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

    // Once the baseline connection is established, buffered publish + stream catch-up
    // should preserve all events. Treat any missing events as a correctness bug.
    assert!(
        received_count == 20,
        "Should receive all 20 rapid messages after connection establishment, got {received_count}"
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
    let key_prefix = redis.key_prefix.clone();

    let node_a = create_node_with_prefix(&redis.redis_url, "node_a", key_prefix).await;

    let room_id = RoomId::expect_positive(10_000_069);
    let user_id = UserId::expect_positive(10_000_026);

    let (mut room_rx, conn_id) = node_a
        .subscribe(room_id, user_id)
        .await
        .expect("subscribe should succeed");

    // Broadcast the same event twice locally (simulating duplicate delivery)
    let event = ClusterEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id,
        user_id: UserId::expect_positive(10_000_057),
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
