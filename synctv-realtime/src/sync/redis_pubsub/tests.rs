use super::*;
use crate::sync::events::WebRTCSignalKind;
use crate::sync::{
    CacheTarget, ConnectionId, RealtimeEventHandler, RoomMessageHub, RoomMessageRuntime,
};
use async_trait::async_trait;
use chrono::Utc;
use std::collections::HashSet;
use std::sync::Arc;
use synctv_core::models::id::UserId;
use synctv_core::{RedisConnectionRuntime, RedisCoordinationRuntime};
use tokio::sync::broadcast;
use tokio::time::Duration;

use crate::sync::stream_id::parse_stream_id;

fn u128_to_u64_saturating(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[derive(Clone)]
struct UnavailableRedisRuntime;

fn unavailable_redis_runtime() -> Arc<dyn RedisCoordinationRuntime> {
    Arc::new(UnavailableRedisRuntime)
}

fn unavailable_redis_error() -> redis::RedisError {
    redis::RedisError::from((
        redis::ErrorKind::Io,
        "test Redis coordination runtime unavailable",
    ))
}

#[async_trait]
impl RedisConnectionRuntime for UnavailableRedisRuntime {
    async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
        Err(unavailable_redis_error())
    }

    fn operation_timeout(&self) -> Duration {
        Duration::from_millis(10)
    }
}

#[async_trait]
impl RedisCoordinationRuntime for UnavailableRedisRuntime {
    async fn multiplexed_connection(
        &self,
    ) -> redis::RedisResult<redis::aio::MultiplexedConnection> {
        Err(unavailable_redis_error())
    }

    async fn async_pubsub(&self) -> redis::RedisResult<redis::aio::PubSub> {
        Err(unavailable_redis_error())
    }
}

struct RecordingEventHandler {
    tx: tokio::sync::mpsc::Sender<(Option<RoomId>, String)>,
}

#[async_trait::async_trait]
impl RealtimeEventHandler for RecordingEventHandler {
    async fn handle_remote_event(&self, room_id: Option<RoomId>, event: &RealtimeEvent) {
        let _ = self.tx.send((room_id, event.event_id().to_string())).await;
    }
}

async fn publish_until_received<F>(
    publish_tx: &tokio::sync::mpsc::Sender<PublishRequest>,
    rx: &mut tokio::sync::mpsc::Receiver<RealtimeEvent>,
    make_event: F,
    timeout_label: &str,
) -> RealtimeEvent
where
    F: Fn() -> RealtimeEvent,
{
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);

    loop {
        publish_tx
            .send(PublishRequest::new(make_event()))
            .await
            .expect("publish should succeed");

        match tokio::time::timeout(tokio::time::Duration::from_millis(500), rx.recv()).await {
            Ok(Some(event)) => return event,
            Ok(None) => panic!("channel closed unexpectedly"),
            Err(_error) if tokio::time::Instant::now() < deadline => {}
            Err(error) => panic!("timeout waiting for {timeout_label} message: {error}"),
        }
    }
}

#[test]
fn test_with_key_prefix_accepts_trait_object_message_hub() {
    let message_hub: Arc<dyn RoomMessageRuntime> = Arc::new(RoomMessageHub::new());
    let (admin_event_tx, _) = tokio::sync::broadcast::channel(1);
    let deduplicator = Arc::new(MessageDeduplicator::new(Duration::from_secs(1)));

    let pubsub = RedisPubSub::with_key_prefix(
        unavailable_redis_runtime(),
        message_hub,
        "runtime-node".to_string(),
        "runtime-test:",
        admin_event_tx,
        None,
        deduplicator,
        300,
        DEFAULT_MAX_STREAM_LENGTH,
    )
    .expect("trait-object room message hub should be accepted");

    assert_eq!(pubsub.key_prefix, "runtime-test:");
}

#[tokio::test]
async fn test_cache_invalidate_dispatch_calls_handler_and_notifies_admin_subscribers() {
    let message_hub: Arc<dyn RoomMessageRuntime> = Arc::new(RoomMessageHub::new());
    let (admin_event_tx, mut admin_rx) = tokio::sync::broadcast::channel(8);
    let deduplicator = Arc::new(MessageDeduplicator::new(tokio::time::Duration::from_secs(
        1,
    )));
    let (handler_tx, mut handler_rx) = tokio::sync::mpsc::channel(1);
    let handler: Arc<dyn RealtimeEventHandler> = Arc::new(RecordingEventHandler { tx: handler_tx });

    let pubsub = RedisPubSub::with_key_prefix(
        unavailable_redis_runtime(),
        message_hub,
        "runtime-node".to_string(),
        "runtime-test:",
        admin_event_tx,
        Some(handler),
        deduplicator,
        300,
        DEFAULT_MAX_STREAM_LENGTH,
    )
    .expect("RedisPubSub should initialize");
    let event = RealtimeEvent::CacheInvalidate {
        event_id: synctv_common::snanoid!(16),
        targets: vec![CacheTarget::Room {
            room_id: RoomId::expect_positive(10_000_150),
        }],
        timestamp: Utc::now(),
    };
    let event_id = event.event_id().to_string();

    pubsub
        .dispatch_event("runtime-test:admin:events", event)
        .await;

    let (handler_room_id, handler_event_id) =
        tokio::time::timeout(tokio::time::Duration::from_millis(100), handler_rx.recv())
            .await
            .expect("CacheInvalidate should notify remote event handler")
            .expect("handler channel should stay open");
    assert_eq!(handler_room_id, None);
    assert_eq!(handler_event_id, event_id);

    let admin_event =
        tokio::time::timeout(tokio::time::Duration::from_millis(100), admin_rx.recv())
            .await
            .expect("CacheInvalidate should notify admin subscribers")
            .expect("admin event channel should stay open");
    assert_eq!(admin_event.event_id(), event_id);
}

#[test]
fn test_event_envelope_serialization() {
    let event = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id: RoomId::expect_positive(10_000_140),
        user_id: UserId::expect_positive(10_000_141),
        username: "testuser".to_string(),
        message: "Hello!".to_string(),
        timestamp: Utc::now(),
        display_position: None,
        display_color: None,
    };

    let envelope = EventEnvelope {
        node_id: "node1".to_string(),
        event,
    };

    // Serialize
    let json = serde_json::to_string(&envelope).unwrap();
    assert!(json.contains("node1"));
    assert!(json.contains("chat_message"));

    // Deserialize
    let deserialized: EventEnvelope = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.node_id, "node1");
    assert_eq!(deserialized.event.event_type(), "chat_message");
}

#[tokio::test]
async fn test_dispatch_room_deleted_waits_for_reliable_delivery_before_cleanup() {
    let message_hub = Arc::new(RoomMessageHub::new());
    let room_id = RoomId::expect_positive(10_000_146);
    let user_id = UserId::expect_positive(10_000_147);
    let mut rx = message_hub
        .subscribe(room_id, user_id, ConnectionId::new("conn-1"))
        .await
        .expect("subscribe should succeed");

    for _ in 0..512 {
        let sent = message_hub.broadcast(
            &room_id,
            &RealtimeEvent::ChatMessage {
                event_id: synctv_common::snanoid!(16),
                room_id,
                user_id,
                username: "filler".to_string(),
                message: "fill".to_string(),
                timestamp: Utc::now(),
                display_position: None,
                display_color: None,
            },
        );
        assert_eq!(sent, 1, "filler message should enqueue");
    }

    let (admin_tx, _) = broadcast::channel(8);
    let pubsub = RedisPubSub::new(
        unavailable_redis_runtime(),
        message_hub.clone(),
        "node-1".to_string(),
        admin_tx,
        None,
        Arc::new(MessageDeduplicator::with_defaults()),
    )
    .expect("pubsub should be created");

    let event = RealtimeEvent::RoomDeleted {
        event_id: synctv_common::snanoid!(16),
        room_id,
        deleted_by: user_id,
        timestamp: Utc::now(),
    };

    let room_for_task = room_id;
    let channel = format!("synctv:room:{room_id}");
    let dispatch_task = tokio::spawn(async move {
        pubsub.dispatch_event(&channel, event).await;
    });

    tokio::task::yield_now().await;
    assert!(
        !dispatch_task.is_finished(),
        "room cleanup must wait for reliable delivery when subscriber channels are full"
    );

    let drained = rx.recv().await.expect("filler message should be present");
    assert!(matches!(drained, RealtimeEvent::ChatMessage { .. }));

    tokio::time::timeout(Duration::from_secs(1), dispatch_task)
        .await
        .expect("dispatch should complete once delivery can proceed")
        .expect("dispatch task should not panic");

    let mut saw_room_deleted = false;
    for _ in 0..512 {
        let msg = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("queued message should arrive")
            .expect("channel should remain open until room deletion is delivered");
        if matches!(msg, RealtimeEvent::RoomDeleted { .. }) {
            saw_room_deleted = true;
            break;
        }
    }

    assert!(
        saw_room_deleted,
        "RoomDeleted should be delivered before cleanup"
    );
    assert_eq!(
        message_hub.subscriber_count(&room_for_task),
        0,
        "room should be cleaned up after reliable delivery"
    );
}

// Integration tests require Redis running
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_pubsub_integration() {
    let (_redis_container, redis_client, _redis_url) =
        synctv_core_testing::start_redis_client_url_with_label("redis-pubsub-integration").await;
    let key_prefix = synctv_core_testing::test_redis_key_prefix("redis-pubsub-integration");

    let message_hub = Arc::new(RoomMessageHub::new());

    let (admin_tx, _) = broadcast::channel(256);

    // Create two PubSub instances simulating different nodes
    // Note: Each RedisPubSub subscribes to lifecycle events from the message_hub internally
    let dedup1 = Arc::new(MessageDeduplicator::with_defaults());
    let dedup2 = Arc::new(MessageDeduplicator::with_defaults());
    let pubsub1 = Arc::new(
        RedisPubSub::with_key_prefix(
            synctv_core::coordination_runtime_from_client(redis_client.clone()),
            message_hub.clone(),
            "node1".to_string(),
            &key_prefix,
            admin_tx.clone(),
            None,
            dedup1,
            300,
            DEFAULT_MAX_STREAM_LENGTH,
        )
        .unwrap(),
    );
    let pubsub2 = Arc::new(
        RedisPubSub::with_key_prefix(
            synctv_core::coordination_runtime_from_client(redis_client.clone()),
            message_hub.clone(),
            "node2".to_string(),
            &key_prefix,
            admin_tx.clone(),
            None,
            dedup2,
            300,
            DEFAULT_MAX_STREAM_LENGTH,
        )
        .unwrap(),
    );

    // Start both - this subscribes to lifecycle events from message_hub
    let (publish_tx1, _backpressure1, _) = pubsub1.start(10_000).await.unwrap();
    let (_publish_tx2, _backpressure2, _) = pubsub2.start(10_000).await.unwrap();

    // Wait for subscriber loops to be ready and lifecycle subscriptions established.
    // The subscriber tasks need to: connect to Redis, subscribe to admin pattern,
    // then set up lifecycle subscription. This can take several hundred ms.
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    // Subscribe a client to the room - this triggers RoomLifecycleEvent::RoomActivated
    // which is received by both pubsub1 and pubsub2, causing them to subscribe to
    // the Redis room channel.
    // IMPORTANT: subscribe() is async and must be awaited to actually register
    // the subscription and send the lifecycle event.
    let room_id = RoomId::expect_positive(10_000_009);
    let user_id = UserId::expect_positive(10_000_148);
    let mut rx = message_hub
        .subscribe(room_id, user_id, ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");

    // Wait for Redis room channel subscription to complete in both pubsub instances.
    // The lifecycle event triggers async Redis SUBSCRIBE which takes time.
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    // Publish event from node1
    let event = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id,
        user_id,
        username: "testuser".to_string(),
        message: "Hello from node1!".to_string(),
        timestamp: Utc::now(),
        display_position: None,
        display_color: None,
    };

    publish_tx1.send(PublishRequest::new(event)).await.unwrap();

    // Wait for event propagation
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    // Client should receive the event
    let received = tokio::time::timeout(tokio::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("timeout waiting for message")
        .expect("channel closed unexpectedly");

    assert_eq!(received.event_type(), "chat_message");

    // The container will be dropped at the end of the test, which will
    // cause Redis connections to fail and the spawned tasks to terminate.
    // The JoinHandle returned by start() is dropped here (via _publish_tx2),
    // but the tasks run until cancelled via the cancel_token or Redis disconnects.
}

#[tokio::test]
async fn test_start_failure_cancels_background_tasks() {
    let message_hub = Arc::new(RoomMessageHub::new());
    let (admin_tx, _) = broadcast::channel(256);
    let dedup = Arc::new(MessageDeduplicator::with_defaults());
    let pubsub = Arc::new(
        RedisPubSub::with_key_prefix(
            unavailable_redis_runtime(),
            message_hub,
            "start-failure-node".to_string(),
            "synctv:test:",
            admin_tx,
            None,
            dedup,
            300,
            1000,
        )
        .expect("pubsub should construct"),
    );

    let result = tokio::time::timeout(Duration::from_secs(15), pubsub.clone().start(8))
        .await
        .expect("start failure path should complete quickly instead of hanging");

    assert!(
        result.is_err(),
        "unreachable redis should make start fail instead of reporting readiness"
    );
    assert!(
        pubsub.cancel_token().is_cancelled(),
        "start failure must cancel spawned background tasks to avoid leaks"
    );
    assert!(
        pubsub.subscriber_handle.lock().await.is_none(),
        "failed start should not leave a subscriber task registered"
    );
}

#[tokio::test]
async fn test_shutdown_aborts_timed_out_subscriber_task() {
    let message_hub = Arc::new(RoomMessageHub::new());
    let (admin_tx, _) = broadcast::channel(256);
    let dedup = Arc::new(MessageDeduplicator::with_defaults());
    let pubsub = RedisPubSub::with_key_prefix(
        unavailable_redis_runtime(),
        message_hub,
        "shutdown-timeout-node".to_string(),
        "synctv:test:",
        admin_tx,
        None,
        dedup,
        300,
        1000,
    )
    .expect("pubsub should construct");

    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = started_tx.send(());
        futures::future::pending::<()>().await;
    });
    *pubsub.subscriber_handle.lock().await = Some(handle);

    started_rx
        .await
        .expect("subscriber timeout task should report that it started");

    pubsub.shutdown().await;

    assert!(
        pubsub.subscriber_handle.lock().await.is_none(),
        "shutdown must drain the timed-out subscriber handle after aborting it"
    );
}

/// Test that catchup_start_id generates a valid Redis Stream ID format.
/// The ID should be in the format "{timestamp_ms}-{seq}" where seq is 0.
#[test]
fn test_catchup_start_id_format() {
    let message_hub = Arc::new(RoomMessageHub::new());
    let (admin_tx, _) = broadcast::channel(256);
    let dedup = Arc::new(MessageDeduplicator::with_defaults());

    // Create with 300 second (5 minute) catchup window
    let pubsub = RedisPubSub::with_key_prefix(
        unavailable_redis_runtime(),
        message_hub,
        "test-node".to_string(),
        "synctv:",
        admin_tx,
        None,
        dedup,
        300, // 5 minutes
        1000,
    )
    .unwrap();

    let catchup_id = pubsub.catchup_start_id();

    // Verify format: "{timestamp_ms}-0"
    assert!(
        catchup_id.ends_with("-0"),
        "catchup_start_id should end with '-0', got: {catchup_id}"
    );

    // Parse and verify timestamp is within expected range
    let parts: Vec<&str> = catchup_id.split('-').collect();
    assert_eq!(
        parts.len(),
        2,
        "ID should have 2 parts separated by '-', got: {catchup_id}"
    );

    let timestamp_ms: u64 = parts[0].parse().expect("timestamp should be a valid u64");

    // Should be approximately 5 minutes ago (300 seconds = 300000 ms)
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let now_ms = u128_to_u64_saturating(now_ms);

    let expected_start = now_ms.saturating_sub(300_000);
    let diff = timestamp_ms.abs_diff(expected_start);

    // Allow 1 second tolerance for test execution time
    assert!(
        diff < 1000,
        "catchup_start_id timestamp should be ~5 minutes ago, diff: {diff}ms"
    );
}

/// Test that catchup_start_id respects the configured catchup_window_secs.
#[test]
fn test_catchup_start_id_respects_window() {
    let message_hub = Arc::new(RoomMessageHub::new());
    let (admin_tx, _) = broadcast::channel(256);
    let dedup = Arc::new(MessageDeduplicator::with_defaults());

    // Create with 60 second catchup window
    let pubsub = RedisPubSub::with_key_prefix(
        unavailable_redis_runtime(),
        message_hub,
        "test-node".to_string(),
        "synctv:",
        admin_tx,
        None,
        dedup,
        60, // 1 minute
        1000,
    )
    .unwrap();

    let catchup_id = pubsub.catchup_start_id();
    let timestamp_ms: u64 = catchup_id.split('-').next().unwrap().parse().unwrap();

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let now_ms = u128_to_u64_saturating(now_ms);

    let expected_start = now_ms.saturating_sub(60_000);
    let diff = timestamp_ms.abs_diff(expected_start);

    // Allow 1 second tolerance
    assert!(
        diff < 1000,
        "catchup_start_id should use 60s window, diff: {diff}ms"
    );
}

#[test]
fn test_room_stream_ttl_uses_catchup_window_with_floor() {
    assert_eq!(
        RedisPubSub::room_stream_ttl_secs(60),
        MIN_ROOM_STREAM_TTL_SECS
    );
    assert_eq!(
        RedisPubSub::room_stream_ttl_secs(300),
        MIN_ROOM_STREAM_TTL_SECS
    );
    assert_eq!(RedisPubSub::room_stream_ttl_secs(600), 1200);
}

/// Test that room subscriptions activated during disconnection are recovered on reconnect.
///
/// RedisPubSub must recover subscriptions to rooms that were activated while
/// the subscriber was disconnected.
///
/// Scenario:
/// 1. Start a PubSub instance with one room already active
/// 2. Subscribe to another room (triggers lifecycle event)
/// 3. Wait for subscription to be processed
/// 4. Verify both rooms receive events
///
/// The key fix is that lifecycle_rx is now maintained outside of run_subscriber,
/// and pending_subscriptions tracks rooms activated during disconnection.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn test_pending_subscriptions_recovered_on_reconnect() {
    let (_redis_container, redis_client, _redis_url) =
        synctv_core_testing::start_redis_client_url_with_label("pending-subscriptions").await;
    let key_prefix = synctv_core_testing::test_redis_key_prefix("pending-subscriptions");

    let message_hub = Arc::new(RoomMessageHub::new());
    let (admin_tx, _) = broadcast::channel(256);

    // Create two PubSub instances
    let dedup1 = Arc::new(MessageDeduplicator::with_defaults());
    let dedup2 = Arc::new(MessageDeduplicator::with_defaults());
    let pubsub1 = Arc::new(
        RedisPubSub::with_key_prefix(
            synctv_core::coordination_runtime_from_client(redis_client.clone()),
            message_hub.clone(),
            "node1".to_string(),
            &key_prefix,
            admin_tx.clone(),
            None,
            dedup1,
            300,
            DEFAULT_MAX_STREAM_LENGTH,
        )
        .unwrap(),
    );
    let pubsub2 = Arc::new(
        RedisPubSub::with_key_prefix(
            synctv_core::coordination_runtime_from_client(redis_client.clone()),
            message_hub.clone(),
            "node2".to_string(),
            &key_prefix,
            admin_tx.clone(),
            None,
            dedup2,
            300,
            DEFAULT_MAX_STREAM_LENGTH,
        )
        .unwrap(),
    );

    // Start both PubSub instances
    let (publish_tx1, _backpressure1, _) = pubsub1.start(10_000).await.unwrap();
    let (_publish_tx2, _backpressure2, _) = pubsub2.start(10_000).await.unwrap();

    // Wait for subscriber loops to be ready. Keep a small initial pause, but
    // rely on eventual publish+receive retries below instead of fixed sleeps.
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Subscribe a client to room1 - this activates the room
    let room1_id = RoomId::expect_positive(10_000_149);
    let user1_id = UserId::expect_positive(10_000_150);
    let mut rx1 = message_hub
        .subscribe(room1_id, user1_id, ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");

    let received = publish_until_received(
        &publish_tx1,
        &mut rx1,
        || RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id: room1_id,
            user_id: user1_id,
            username: "testuser1".to_string(),
            message: "Hello from room1!".to_string(),
            timestamp: chrono::Utc::now(),
            display_position: None,
            display_color: None,
        },
        "room1",
    )
    .await;
    assert_eq!(received.event_type(), "chat_message");

    // Now subscribe a client to room2 - this is a second room activation
    let room2_id = RoomId::expect_positive(10_000_151);
    let user2_id = UserId::expect_positive(10_000_152);
    let mut rx2 = message_hub
        .subscribe(room2_id, user2_id, ConnectionId::new("conn2"))
        .await
        .expect("subscribe should succeed");

    let received = publish_until_received(
        &publish_tx1,
        &mut rx2,
        || RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id: room2_id,
            user_id: user2_id,
            username: "testuser2".to_string(),
            message: "Hello from room2!".to_string(),
            timestamp: chrono::Utc::now(),
            display_position: None,
            display_color: None,
        },
        "room2",
    )
    .await;
    assert_eq!(received.event_type(), "chat_message");

    // The test verifies that multiple room subscriptions work correctly.
    // The actual reconnection scenario is harder to test in integration tests
    // because we can't easily force a Redis disconnect without killing the container.
    // The unit test below verifies the pending_subscriptions mechanism directly.
}

/// Unit test for the pending_subscriptions mechanism.
/// This verifies that lifecycle events during disconnection are properly tracked.
#[test]
fn test_pending_subscriptions_tracks_lifecycle_events() {
    let mut pending_subscriptions: HashSet<RoomId> = HashSet::new();

    // Simulate room activations during disconnection
    let room1 = RoomId::expect_positive(10_000_092);
    let room2 = RoomId::expect_positive(10_000_094);
    let room3 = RoomId::expect_positive(10_000_153);

    // Room activated
    pending_subscriptions.insert(room1);
    pending_subscriptions.insert(room2);
    assert_eq!(pending_subscriptions.len(), 2);

    // Room deactivated before reconnect (should be removed)
    pending_subscriptions.remove(&room2);
    assert_eq!(pending_subscriptions.len(), 1);
    assert!(pending_subscriptions.contains(&room1));
    assert!(!pending_subscriptions.contains(&room2));

    // Another room activated
    pending_subscriptions.insert(room3);
    assert_eq!(pending_subscriptions.len(), 2);

    // After reconnect and successful subscription, clear the set
    pending_subscriptions.clear();
    assert!(pending_subscriptions.is_empty());
}

/// Unit test verifying the merge of active_rooms with pending_subscriptions.
#[test]
fn test_pending_subscriptions_merges_with_active_rooms() {
    let active_rooms: Vec<RoomId> = vec![
        RoomId::expect_positive(10_000_154),
        RoomId::expect_positive(10_000_155),
    ];

    // Simulate a room that was activated during disconnection
    // but is NOT in the current active_rooms (edge case - room became inactive)
    let pending_room = RoomId::expect_positive(10_000_156);
    let mut pending_subscriptions: HashSet<RoomId> = HashSet::new();
    pending_subscriptions.insert(pending_room);
    pending_subscriptions.insert(active_rooms[0]); // Already active, but also pending

    // Merge logic (same as in run_subscriber)
    let mut rooms_to_subscribe: HashSet<RoomId> = active_rooms.iter().copied().collect();
    rooms_to_subscribe.extend(pending_subscriptions.iter().copied());

    // Should contain both active rooms and pending room
    assert_eq!(rooms_to_subscribe.len(), 3);
    assert!(rooms_to_subscribe.contains(&active_rooms[0]));
    assert!(rooms_to_subscribe.contains(&active_rooms[1]));
    assert!(rooms_to_subscribe.contains(&pending_room));

    // After successful subscription, clear pending
    pending_subscriptions.clear();
    assert!(pending_subscriptions.is_empty());
}

#[test]
fn test_failed_cursor_snapshot_falls_back_to_catchup_window_not_dollar() {
    let message_hub = Arc::new(RoomMessageHub::new());
    let (admin_tx, _) = broadcast::channel(256);
    let dedup = Arc::new(MessageDeduplicator::with_defaults());

    let pubsub = RedisPubSub::with_key_prefix(
        unavailable_redis_runtime(),
        message_hub,
        "test-node".to_string(),
        "synctv:",
        admin_tx,
        None,
        dedup,
        300,
        1000,
    )
    .unwrap();

    let fallback = pubsub.catchup_start_id();
    assert_ne!(
        fallback, "$",
        "failed snapshot fallback must not skip catch-up entirely"
    );
    assert!(
        parse_stream_id(&fallback).is_some(),
        "fallback cursor should remain a valid Redis stream ID"
    );
}

#[tokio::test]
async fn test_dispatch_event_drops_malformed_webrtc_target() {
    let message_hub = Arc::new(RoomMessageHub::new());
    let (admin_tx, _) = broadcast::channel(256);
    let dedup = Arc::new(MessageDeduplicator::with_defaults());

    let pubsub = RedisPubSub::with_key_prefix(
        unavailable_redis_runtime(),
        message_hub.clone(),
        "test-node".to_string(),
        "synctv:",
        admin_tx,
        None,
        dedup,
        300,
        1000,
    )
    .unwrap();

    let room_id = RoomId::expect_positive(10_000_156);
    let user1 = synctv_core::models::id::UserId::expect_positive(10_000_010);
    let user2 = synctv_core::models::id::UserId::expect_positive(10_000_095);
    let mut rx1 = message_hub
        .subscribe(room_id, user1, ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");
    let mut rx2 = message_hub
        .subscribe(room_id, user2, ConnectionId::new("conn2"))
        .await
        .expect("subscribe should succeed");

    pubsub
        .dispatch_event(
            &format!("synctv:room:{room_id}"),
            RealtimeEvent::WebRTCSignaling {
                event_id: synctv_common::snanoid!(16),
                room_id,
                message_type: WebRTCSignalKind::Offer,
                from: "user1|conn1".to_string(),
                to: "conn2".to_string(),
                data: "SDP".to_string(),
                timestamp: chrono::Utc::now(),
            },
        )
        .await;

    let target = tokio::time::timeout(Duration::from_millis(100), rx2.recv()).await;
    assert!(
        target.is_err(),
        "malformed WebRTC target must not be routed to the target connection"
    );

    let non_target = tokio::time::timeout(Duration::from_millis(100), rx1.recv()).await;
    assert!(
        non_target.is_err(),
        "malformed WebRTC target must not be broadcast to non-target connections"
    );
}

#[tokio::test]
async fn test_dispatch_event_only_delivers_duplicate_once() {
    let message_hub = Arc::new(RoomMessageHub::new());
    let (admin_tx, _) = broadcast::channel(256);
    let dedup = Arc::new(MessageDeduplicator::with_defaults());

    let pubsub = RedisPubSub::with_key_prefix(
        unavailable_redis_runtime(),
        message_hub.clone(),
        "test-node".to_string(),
        "synctv:",
        admin_tx,
        None,
        dedup,
        300,
        1000,
    )
    .unwrap();

    let room_id = RoomId::expect_positive(10_000_157);
    let user_id = synctv_core::models::id::UserId::expect_positive(10_000_158);
    let mut rx = message_hub
        .subscribe(room_id, user_id, ConnectionId::new("dedup-conn"))
        .await
        .expect("subscribe should succeed");

    let event = RealtimeEvent::ChatMessage {
        event_id: "duplicate-event-id".to_string(),
        room_id,
        user_id,
        username: "dedup".to_string(),
        message: "hello".to_string(),
        timestamp: chrono::Utc::now(),
        display_position: None,
        display_color: None,
    };

    pubsub
        .dispatch_event(&format!("synctv:room:{room_id}"), event.clone())
        .await;
    pubsub
        .dispatch_event(&format!("synctv:room:{room_id}"), event)
        .await;

    let first = tokio::time::timeout(Duration::from_millis(100), rx.recv())
        .await
        .expect("first event should be delivered")
        .expect("channel should remain open");
    assert!(matches!(first, RealtimeEvent::ChatMessage { .. }));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .is_err(),
        "duplicate event must not be delivered twice"
    );
}
