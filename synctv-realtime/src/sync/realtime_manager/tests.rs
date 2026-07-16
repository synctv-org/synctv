use super::*;
use crate::sync::{CacheTarget, ConnectionLimits, ConnectionManager, SharedRealtimeEvent};
use async_trait::async_trait;
use std::sync::atomic::AtomicUsize;
use synctv_cluster::NodeRegistry;
use tokio::sync::{broadcast, mpsc};

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = std::result::Result<(), BoxError>;

fn missing(message: &'static str) -> BoxError {
    anyhow::anyhow!(message).into()
}

struct NoopRealtimeEventHandler;

#[async_trait]
impl RealtimeEventHandler for NoopRealtimeEventHandler {
    async fn handle_remote_event(&self, _room_id: Option<RoomId>, _event: &RealtimeEvent) {}
}

#[derive(Clone, Default)]
struct StubTransportFactory {
    start_count: Arc<AtomicUsize>,
    shutdown_count: Arc<AtomicUsize>,
}

struct StubTransport {
    start_count: Arc<AtomicUsize>,
    shutdown_count: Arc<AtomicUsize>,
    publish_rx: Arc<tokio::sync::Mutex<Option<mpsc::Receiver<PublishRequest>>>>,
}

impl RealtimeMessageTransportFactory for StubTransportFactory {
    fn build(
        &self,
        _config: RealtimeMessageTransportConfig,
    ) -> RealtimeResult<Arc<dyn RealtimeMessageTransport>> {
        Ok(Arc::new(StubTransport {
            start_count: self.start_count.clone(),
            shutdown_count: self.shutdown_count.clone(),
            publish_rx: Arc::new(tokio::sync::Mutex::new(None)),
        }))
    }
}

#[async_trait]
impl RealtimeMessageTransport for StubTransport {
    async fn start(
        self: Arc<Self>,
        _publish_channel_capacity: usize,
    ) -> RealtimeResult<crate::sync::RealtimeMessageTransportRuntime> {
        self.start_count.fetch_add(1, Ordering::Relaxed);
        let (publish_tx, publish_rx) = mpsc::channel(8);
        *self.publish_rx.lock().await = Some(publish_rx);
        Ok(crate::sync::RealtimeMessageTransportRuntime {
            publish_tx,
            publisher_handle: tokio::spawn(async {}),
        })
    }

    async fn shutdown(&self) {
        self.shutdown_count.fetch_add(1, Ordering::Relaxed);
    }
}

struct FixedMetricsRoomRuntime {
    room_count: usize,
    connection_count: usize,
}

impl FixedMetricsRoomRuntime {
    const fn new(room_count: usize, connection_count: usize) -> Self {
        Self {
            room_count,
            connection_count,
        }
    }
}

#[async_trait]
impl RoomMessageRuntime for FixedMetricsRoomRuntime {
    fn subscribe_lifecycle(&self) -> broadcast::Receiver<crate::sync::RoomLifecycleEvent> {
        let (_tx, rx) = broadcast::channel(1);
        rx
    }

    async fn subscribe(
        &self,
        _room_id: RoomId,
        _user_id: UserId,
        _connection_id: ConnectionId,
    ) -> RealtimeResult<mpsc::Receiver<SharedRealtimeEvent>> {
        let (_tx, rx) = mpsc::channel(1);
        Ok(rx)
    }

    fn unsubscribe(&self, _connection_id: &str) {}

    fn broadcast(&self, _room_id: &RoomId, _event: &RealtimeEvent) -> usize {
        0
    }

    async fn broadcast_reliably(&self, _room_id: &RoomId, _event: RealtimeEvent) -> usize {
        0
    }

    async fn broadcast_to_connection(
        &self,
        _room_id: &RoomId,
        _connection_id: &str,
        _event: RealtimeEvent,
    ) -> usize {
        0
    }

    fn room_count(&self) -> usize {
        self.room_count
    }

    fn active_room_ids(&self) -> Vec<RoomId> {
        Vec::new()
    }

    fn connection_count(&self) -> usize {
        self.connection_count
    }

    fn remove_room(&self, _room_id: &RoomId) {}

    fn get_room_subscribers(&self, _room_id: &RoomId) -> Vec<(UserId, ConnectionId)> {
        Vec::new()
    }

    async fn get_room_subscribers_replicas_wide(
        &self,
        _room_id: &RoomId,
    ) -> RealtimeResult<Vec<(UserId, ConnectionId)>> {
        Ok(Vec::new())
    }

    async fn audit_shared_subscriptions(&self) -> std::result::Result<usize, String> {
        Ok(0)
    }

    async fn shutdown(&self) {}

    fn background_shutdown_requested(&self) -> bool {
        false
    }
}

#[tokio::test]
async fn test_local_critical_broadcast_does_not_panic_on_current_thread_runtime() -> TestResult {
    let message_hub = Arc::new(RoomMessageHub::new());
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: message_hub.clone(),
        distributed_enabled: false,
        node_id: "test_local_critical_current_thread".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 4,
        publish_channel_capacity: 4,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };
    let manager = RealtimeManager::new(config).await?;
    let room_id = RoomId::expect_positive(10_000_212);
    let user_id = UserId::expect_positive(10_000_213);
    let mut rx = message_hub
        .subscribe(
            room_id,
            user_id,
            ConnectionId::new("local-critical-current-thread"),
        )
        .await?;

    let event = RealtimeEvent::UserLeft {
        event_id: synctv_common::snanoid!(16),
        room_id,
        user_id,
        username: "leaver".to_string(),
        remark_name: String::new(),
        display_tag: String::new(),
        role: i32::from(synctv_core::models::RoomRole::Member),
        timestamp: synctv_core::SystemClock.now(),
    };

    assert_eq!(manager.broadcast_local(event), 1);
    assert!(matches!(
        rx.recv().await,
        Some(event) if matches!(event.as_ref(), RealtimeEvent::UserLeft { .. })
    ));

    manager.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_realtime_manager_respects_injected_message_runtime() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(FixedMetricsRoomRuntime::new(7, 11)),
        distributed_enabled: false,
        node_id: "test_node_metrics".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config).await?;
    let metrics = manager.metrics();

    assert_eq!(metrics.total_rooms, 7);
    assert_eq!(metrics.total_connections, 11);
    Ok(())
}

#[tokio::test]
async fn test_admin_event_channel_subscription() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_node".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config).await?;

    // Subscribe to admin events
    let mut admin_rx = manager.subscribe_admin_events();

    // Send a KickPublisher event through the admin channel
    let event = RealtimeEvent::KickPublisher {
        event_id: synctv_common::snanoid!(16),
        room_id: RoomId::expect_positive(10_000_092),
        media_id: synctv_core::models::MediaId::expect_positive(10_000_093),
        reason: "user_banned".to_string(),
        timestamp: synctv_core::SystemClock.now(),
    };

    manager
        .admin_event_tx()
        .send(event.clone())
        .map_err(|_| missing("admin event channel should accept KickPublisher event"))?;

    // Verify event received
    let received = admin_rx
        .recv()
        .await
        .map_err(|_| missing("admin event channel closed"))?;
    assert_eq!(received.event_type(), "kick_publisher");

    let RealtimeEvent::KickPublisher {
        room_id,
        media_id,
        reason,
        ..
    } = &received
    else {
        return Err(missing("expected KickPublisher event"));
    };
    assert_eq!(*room_id, RoomId::expect_positive(10_000_092));
    assert_eq!(
        *media_id,
        synctv_core::models::MediaId::expect_positive(10_000_093)
    );
    assert_eq!(reason, "user_banned");
    Ok(())
}

#[tokio::test]
async fn test_admin_event_channel_multiple_subscribers() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_node".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config).await?;

    // Subscribe two receivers
    let mut rx1 = manager.subscribe_admin_events();
    let mut rx2 = manager.subscribe_admin_events();

    // Send event
    let event = RealtimeEvent::KickPublisher {
        event_id: synctv_common::snanoid!(16),
        room_id: RoomId::expect_positive(10_000_092),
        media_id: synctv_core::models::MediaId::expect_positive(10_000_093),
        reason: "room_deleted".to_string(),
        timestamp: synctv_core::SystemClock.now(),
    };
    manager
        .admin_event_tx()
        .send(event)
        .map_err(|_| missing("admin event channel should accept broadcast event"))?;

    // Both receivers should get the event
    let r1 = rx1
        .recv()
        .await
        .map_err(|_| missing("first admin event channel closed"))?;
    let r2 = rx2
        .recv()
        .await
        .map_err(|_| missing("second admin event channel closed"))?;
    assert_eq!(r1.event_type(), "kick_publisher");
    assert_eq!(r2.event_type(), "kick_publisher");
    Ok(())
}

#[tokio::test]
async fn test_outbox_side_effect_broadcast_does_not_poison_dedup_or_replay_subscribers(
) -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_node".to_string(),
        dedup_window: Duration::from_mins(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config).await?;
    let mut lifecycle_rx = manager.subscribe_lifecycle_events();
    let mut admin_rx = manager.subscribe_admin_events();
    let event = RealtimeEvent::KickPublisher {
        event_id: "outbox-side-effect-retryable".to_string(),
        room_id: RoomId::expect_positive(10_000_092),
        media_id: synctv_core::models::MediaId::expect_positive(10_000_093),
        reason: "outbox_retry".to_string(),
        timestamp: synctv_core::SystemClock.now(),
    };

    let side_effect_sent = manager.broadcast_local_outbox_side_effect(event.clone());
    assert_eq!(
        side_effect_sent, 1,
        "outbox side-effect broadcast should reach lifecycle listeners"
    );
    let first = lifecycle_rx
        .recv()
        .await
        .map_err(|_| missing("side-effect broadcast should reach lifecycle listeners"))?;
    assert_eq!(first.event_id(), "outbox-side-effect-retryable");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), admin_rx.recv())
            .await
            .is_err(),
        "outbox side-effect broadcast must not replay to admin subscribers"
    );

    let mut second_rx = manager.subscribe_admin_events();
    let sent = manager.broadcast_local(event.clone());
    assert_eq!(
        sent, 0,
        "admin-only broadcast returns zero room subscribers"
    );
    let second = second_rx
        .recv()
        .await
        .map_err(|_| missing("regular local broadcast should reach admin subscribers"))?;
    assert_eq!(second.event_id(), "outbox-side-effect-retryable");

    let mut third_rx = manager.subscribe_admin_events();
    manager.broadcast_local(event);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), third_rx.recv())
            .await
            .is_err(),
        "regular local broadcast should still populate dedup for later duplicates"
    );
    Ok(())
}

#[tokio::test]
async fn test_outbox_side_effect_ignores_non_lifecycle_events() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_node".to_string(),
        dedup_window: Duration::from_mins(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config).await?;
    let mut lifecycle_rx = manager.subscribe_lifecycle_events();
    let mut admin_rx = manager.subscribe_admin_events();
    let event = RealtimeEvent::RoomSettingsChanged {
        event_id: "outbox-non-lifecycle".to_string(),
        room_id: RoomId::expect_positive(10_000_092),
        user_id: UserId::expect_positive(10_000_093),
        username: "tester".to_string(),
        settings: synctv_core::models::RoomSettings::default(),
        version: 1,
        timestamp: synctv_core::SystemClock.now(),
    };

    assert_eq!(manager.broadcast_local_outbox_side_effect(event), 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), lifecycle_rx.recv())
            .await
            .is_err(),
        "non-lifecycle outbox events should not reach lifecycle listeners"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), admin_rx.recv())
            .await
            .is_err(),
        "non-lifecycle outbox events should not replay to admin subscribers"
    );
    Ok(())
}

/// Test that RealtimeManager handles the non-distributed mode degradation gracefully
/// when a CacheInvalidationService is provided but Redis is not available.
///
/// This verifies:
/// 1. RealtimeManager::new() succeeds even when an event handler is provided without Redis
/// 2. The service logs an appropriate warning about local-only invalidation
/// 3. The RealtimeManager operates normally in single-node mode
#[tokio::test]
async fn test_non_cluster_mode_with_event_handler() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_node_cache".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: Some(Arc::new(NoopRealtimeEventHandler)),
        parent_cancel_token: None,
    };

    // Create RealtimeManager with a remote-event handler but no Redis.
    let manager = RealtimeManager::new(config).await?;

    // Verify the manager operates normally in single-node mode
    let room_id = RoomId::expect_positive(10_000_092);
    let user_id = UserId::expect_positive(10_000_010);
    let (mut rx, conn_id) = manager.subscribe(room_id, user_id).await?;

    // Broadcast should work locally
    let event = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id,
        user_id,
        username: "user1".to_string(),
        message: "Hello local!".to_string(),
        timestamp: synctv_core::SystemClock.now(),
        display_position: None,
        display_color: None,
    };

    let result = manager.broadcast(event.clone());
    assert_eq!(
        result.local_sent, 1,
        "Local broadcast should work in non-distributed mode"
    );
    assert!(
        !result.redis_sent,
        "Redis should not be used in non-distributed mode"
    );

    // Verify message received locally
    let received = rx
        .recv()
        .await
        .ok_or_else(|| missing("local message channel closed"))?;
    assert_eq!(received.event_type(), "chat_message");

    // Cleanup
    manager.unsubscribe(&conn_id);

    // Verify metrics show single-node mode
    let metrics = manager.metrics();
    assert!(
        !metrics.distributed_enabled,
        "Metrics should show distributed transport is not enabled"
    );
    Ok(())
}

/// Test that RealtimeManager works correctly when both Redis and
/// CacheInvalidationService are not provided (pure single-node mode).
#[tokio::test]
async fn test_non_cluster_mode_without_event_handler() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_node_no_cache".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    // Create RealtimeManager without a remote-event handler and without Redis.
    let manager = RealtimeManager::new(config).await?;

    // Verify normal operation
    let room_id = RoomId::expect_positive(10_000_094);
    let user_id = UserId::expect_positive(10_000_095);
    let (mut rx, conn_id) = manager.subscribe(room_id, user_id).await?;

    let event = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id,
        user_id,
        username: "user2".to_string(),
        message: "Hello!".to_string(),
        timestamp: synctv_core::SystemClock.now(),
        display_position: None,
        display_color: None,
    };

    let result = manager.broadcast(event);
    assert_eq!(result.local_sent, 1);
    assert!(!result.redis_sent);

    let received = rx
        .recv()
        .await
        .ok_or_else(|| missing("local message channel closed"))?;
    assert_eq!(received.event_type(), "chat_message");

    manager.unsubscribe(&conn_id);
    Ok(())
}

#[tokio::test]
async fn test_shutdown_times_out_non_cooperative_heartbeat_handle() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "stuck-heartbeat-node".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config).await?;

    let stuck = tokio::spawn(async {
        futures::future::pending::<()>().await;
    });
    manager.test_set_heartbeat_handle(stuck).await;

    let start = std::time::Instant::now();
    manager.shutdown().await;
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(1),
        "Shutdown should time out stuck heartbeat handle quickly, took {elapsed:?}"
    );
    Ok(())
}

#[tokio::test]
async fn test_shutdown_unregisters_node_after_heartbeat_stops() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "shutdown-race-node".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = Arc::new(RealtimeManager::new(config).await?);
    let registry = Arc::new(NodeRegistry::new_local_only(
        "shutdown-race-node".to_string(),
        30,
        "test:",
    )?);

    registry.register("localhost:8080".to_string()).await?;
    manager.test_set_heartbeat_registry(registry.clone()).await;

    let cancel = manager.cancel_token();
    let registry_for_task = registry.clone();
    let (cancel_seen_tx, cancel_seen_rx) = tokio::sync::oneshot::channel();
    let (allow_finish_tx, allow_finish_rx) = tokio::sync::oneshot::channel();
    manager
        .test_set_heartbeat_handle(tokio::spawn(async move {
            cancel.cancelled().await;
            cancel_seen_tx
                .send(())
                .expect("shutdown race test should receive cancellation signal");
            assert!(
                allow_finish_rx.await.is_ok(),
                "heartbeat finish signal should stay open"
            );
            let register_result = registry_for_task
                .register("localhost:8080".to_string())
                .await;
            assert!(
                register_result.is_ok(),
                "late heartbeat registration should succeed: {register_result:?}"
            );
        }))
        .await;

    let shutdown_manager = Arc::clone(&manager);
    let shutdown_handle = tokio::spawn(async move {
        shutdown_manager.shutdown().await;
    });

    // Shutdown should be waiting for the heartbeat handle to complete.
    cancel_seen_rx
        .await
        .map_err(|_| missing("heartbeat cancellation signal closed"))?;
    // Node is still registered because shutdown awaits heartbeat before unregistering.
    assert!(
        registry
            .test_get_local("shutdown-race-node")
            .await
            .is_some(),
        "shutdown should still be waiting for heartbeat, node not yet unregistered"
    );

    // Allow the heartbeat task to finish (and re-register the node).
    allow_finish_tx
        .send(())
        .map_err(|()| missing("heartbeat task finished before release"))?;
    // Now shutdown can proceed: heartbeat stopped → unregister.
    shutdown_handle.await?;

    // The late re-registration must be cleaned up by unregister.
    assert!(
        registry
            .test_get_local("shutdown-race-node")
            .await
            .is_none(),
        "shutdown must unregister the node even after a late heartbeat re-registration"
    );
    Ok(())
}

#[tokio::test]
async fn test_shutdown_waits_for_tracked_critical_retry_tasks() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "tracked-critical-retry-node".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1,
        publish_channel_capacity: 1,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config).await?;

    let retry_gate = Arc::new(tokio::sync::Notify::new());
    let retry_gate_clone = Arc::clone(&retry_gate);
    let finished = Arc::new(AtomicBool::new(false));
    let finished_clone = Arc::clone(&finished);

    manager.critical_retry_tasks.spawn(async move {
        retry_gate_clone.notified().await;
        finished_clone.store(true, Ordering::SeqCst);
    });

    let manager = Arc::new(manager);
    let shutdown_manager = Arc::clone(&manager);
    let shutdown_handle = tokio::spawn(async move {
        shutdown_manager.shutdown().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !shutdown_handle.is_finished(),
        "shutdown must wait for tracked critical retry tasks to finish"
    );
    assert!(
        !finished.load(Ordering::SeqCst),
        "retry task should still be blocked before gate release"
    );

    retry_gate.notify_waiters();
    shutdown_handle.await?;

    assert!(
        finished.load(Ordering::SeqCst),
        "tracked critical retry task should finish before shutdown returns"
    );
    Ok(())
}

#[tokio::test]
async fn test_shutdown_stops_accepting_new_critical_redis_work_before_waiting_for_retries(
) -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "shutdown-drain-critical-window".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1,
        publish_channel_capacity: 1,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let mut manager = RealtimeManager::new(config).await?;
    let (critical_tx, mut critical_rx) = mpsc::channel::<PublishRequest>(1);
    critical_tx.try_send(PublishRequest::new(RealtimeEvent::KickUser {
        event_id: synctv_common::snanoid!(16),
        user_id: UserId::expect_positive(10_000_096),
        reason: "fill queue".to_string(),
        timestamp: synctv_core::SystemClock.now(),
    }))?;
    manager.redis_critical_tx = Some(critical_tx);

    manager.shutdown_started.store(true, Ordering::Release);
    manager
        .redis_publish_accepting
        .store(false, Ordering::Release);

    let event = RealtimeEvent::KickUser {
        event_id: synctv_common::snanoid!(16),
        user_id: UserId::expect_positive(10_000_097),
        reason: "must not start new retry after drain closes".to_string(),
        timestamp: synctv_core::SystemClock.now(),
    };

    let result = manager.broadcast(event);

    assert!(
        !result.redis_sent,
        "shutdown drain must reject new critical Redis work once retry waiting begins"
    );
    assert_eq!(
        manager.critical_retry_tasks.len(),
        0,
        "rejecting post-drain fan-out must avoid spawning new tracked retry tasks"
    );

    let queued = critical_rx
        .recv()
        .await
        .ok_or_else(|| missing("pre-filled request should still be present"))?;
    assert_eq!(queued.event.event_type(), "kick_user");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), critical_rx.recv())
            .await
            .is_err(),
        "no new critical publish should be enqueued after drain closes"
    );
    Ok(())
}

#[tokio::test]
async fn test_shutdown_also_cancels_room_message_hub_background_tasks() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "shutdown-room-hub-node".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1,
        publish_channel_capacity: 1,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config).await?;

    assert!(
        !manager.message_hub().background_shutdown_requested(),
        "room hub cancellation tokens should not be pre-cancelled"
    );

    manager.shutdown().await;

    assert!(
        manager.message_hub().background_shutdown_requested(),
        "cluster shutdown must also cancel room hub background tasks"
    );
    Ok(())
}

#[tokio::test]
async fn test_shutdown_still_allows_critical_events_to_reach_redis_channels() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "shutdown-critical-event-node".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 4,
        publish_channel_capacity: 4,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let mut manager = RealtimeManager::new(config).await?;
    let (critical_tx, mut critical_rx) = mpsc::channel::<PublishRequest>(4);
    manager.redis_critical_tx = Some(critical_tx);
    manager.shutdown_started.store(true, Ordering::Release);

    let event = RealtimeEvent::KickUser {
        event_id: synctv_common::snanoid!(16),
        user_id: UserId::expect_positive(10_000_098),
        reason: "must propagate during draining".to_string(),
        timestamp: synctv_core::SystemClock.now(),
    };

    let result = manager.broadcast(event.clone());

    assert!(
        result.redis_sent,
        "critical events must still be enqueued for Redis while shutdown drains in-flight work"
    );

    let published = tokio::time::timeout(Duration::from_millis(100), critical_rx.recv())
        .await
        .map_err(|_| missing("critical event should reach Redis queue during shutdown"))?
        .ok_or_else(|| missing("critical channel should stay open"))?;
    assert_eq!(published.event.event_type(), event.event_type());
    Ok(())
}

#[tokio::test]
async fn test_shutdown_still_blocks_non_critical_events_from_redis_channels() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "shutdown-noncritical-event-node".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 4,
        publish_channel_capacity: 4,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let mut manager = RealtimeManager::new(config).await?;
    let room_id = RoomId::expect_positive(10_000_099);
    let user_id = UserId::expect_positive(10_000_098);
    let (mut room_rx, _conn_id) = manager.subscribe(room_id, user_id).await?;
    let (publish_tx, mut publish_rx) = mpsc::channel::<PublishRequest>(4);
    manager.redis_publish_tx = Some(publish_tx);
    manager.shutdown_started.store(true, Ordering::Release);

    let event = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id,
        user_id,
        username: "shutdown".to_string(),
        message: "non critical".to_string(),
        timestamp: synctv_core::SystemClock.now(),
        display_position: None,
        display_color: None,
    };

    let result = manager.broadcast(event);

    assert!(
        !result.redis_sent,
        "non-critical events should not enter Redis publish queues after shutdown starts"
    );
    assert_eq!(
        result.local_sent, 0,
        "non-critical events should not be delivered locally once shutdown begins"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), publish_rx.recv())
            .await
            .is_err(),
        "non-critical event must not be queued during shutdown"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), room_rx.recv())
            .await
            .is_err(),
        "non-critical event must not reach local subscribers during shutdown"
    );
    Ok(())
}

/// Test that RealtimeManager tracks epoch mismatch state and quarantine.
///
/// This test verifies:
/// 1. RealtimeManager starts in non-quarantined state
/// 2. Epoch mismatch counter is tracked internally
/// 3. Quarantine state is reflected in metrics
/// 4. Leader elector can be set for resigning leadership
#[tokio::test]
async fn test_epoch_mismatch_enforcement() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_node_epoch".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new_with_runtime(
        config,
        RealtimeManagerRuntime {
            connection_runtime: None,
            leader_runtime: Some(Arc::new(synctv_core::service::AlwaysLeader)),
        },
    )
    .await?;

    // Verify initial state: not quarantined
    assert!(
        !manager.is_quarantined(),
        "Should start in non-quarantined state"
    );

    let metrics = manager.metrics();
    assert!(
        !metrics.is_quarantined,
        "Metrics should show non-quarantined state"
    );

    let room_id = RoomId::expect_positive(10_000_100);
    let user_id = UserId::expect_positive(10_000_101);
    let (_rx, conn_id) = manager.subscribe(room_id, user_id).await?;

    // Broadcast should work in non-quarantined state
    let event = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id,
        user_id,
        username: "test_user".to_string(),
        message: "Test message".to_string(),
        timestamp: synctv_core::SystemClock.now(),
        display_position: None,
        display_color: None,
    };

    let result = manager.broadcast(event);
    assert_eq!(
        result.local_sent, 1,
        "Broadcast should succeed in non-quarantined state"
    );

    manager.unsubscribe(&conn_id);
    Ok(())
}

#[tokio::test]
async fn test_quarantined_broadcast_is_rejected_without_poisoning_dedup() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_node_quarantine".to_string(),
        dedup_window: Duration::from_mins(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config).await?;
    let room_id = RoomId::expect_positive(10_000_102);
    let user_id = UserId::expect_positive(10_000_103);
    let mut rx = manager
        .message_hub()
        .subscribe(room_id, user_id, ConnectionId::new("conn-quarantine"))
        .await?;

    manager.is_quarantined.store(true, Ordering::Release);

    let event = RealtimeEvent::ChatMessage {
        event_id: "dedup-preserved".to_string(),
        room_id,
        user_id,
        username: "quarantined-user".to_string(),
        message: "blocked while quarantined".to_string(),
        timestamp: synctv_core::SystemClock.now(),
        display_position: None,
        display_color: None,
    };

    let blocked = manager.broadcast(event.clone());
    assert_eq!(blocked.local_sent, 0);
    assert!(!blocked.redis_sent);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .is_err(),
        "quarantined manager must not deliver locally"
    );

    manager.is_quarantined.store(false, Ordering::Release);

    let delivered = manager.broadcast(event);
    assert_eq!(
        delivered.local_sent, 1,
        "retry after quarantine should still be deliverable with the same event id"
    );
    assert!(!delivered.redis_sent);
    assert!(
        matches!(
            tokio::time::timeout(Duration::from_secs(1), rx.recv()).await,
            Ok(Some(event)) if matches!(event.as_ref(), RealtimeEvent::ChatMessage { .. })
        ),
        "event should be delivered after quarantine is lifted"
    );
    Ok(())
}

#[tokio::test]
async fn test_cluster_metrics_reports_dependency_injection_state() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_metrics_injection".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config.clone()).await?;

    let metrics = manager.metrics();
    assert!(
        !metrics.has_connection_manager,
        "fresh manager should not report an injected ConnectionManager"
    );
    assert!(
        !metrics.has_leader_elector,
        "fresh manager should not report an injected leader elector"
    );

    let manager = RealtimeManager::new_with_runtime(
        config,
        RealtimeManagerRuntime {
            connection_runtime: Some(Arc::new(
                ConnectionManager::new(ConnectionLimits::default()),
            )),
            leader_runtime: Some(Arc::new(synctv_core::service::AlwaysLeader)),
        },
    )
    .await?;
    let metrics = manager.metrics();
    assert!(
        metrics.has_connection_manager,
        "metrics should reflect injected ConnectionManager"
    );
    assert!(
        metrics.has_leader_elector,
        "metrics should reflect injected leader elector"
    );
    Ok(())
}

#[tokio::test]
async fn test_critical_events_do_not_fall_back_to_droppable_normal_channel() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_critical_fallback".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1,
        publish_channel_capacity: 1,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let mut manager = RealtimeManager::new(config).await?;

    let (normal_tx, mut normal_rx) = mpsc::channel::<PublishRequest>(1);
    normal_tx.try_send(PublishRequest::new(RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id: RoomId::expect_positive(10_000_104),
        user_id: UserId::expect_positive(10_000_105),
        username: "buffer".to_string(),
        message: "fill channel".to_string(),
        timestamp: synctv_core::SystemClock.now(),
        display_position: None,
        display_color: None,
    }))?;

    manager.redis_publish_tx = Some(normal_tx);
    manager.redis_critical_tx = None;

    let critical_event = RealtimeEvent::KickUser {
        event_id: synctv_common::snanoid!(16),
        user_id: UserId::expect_positive(10_000_106),
        reason: "must not drop".to_string(),
        timestamp: synctv_core::SystemClock.now(),
    };

    let result = manager.broadcast(critical_event.clone());

    assert!(
        result.redis_sent,
        "critical events must still report Redis publication when only the fallback channel is wired"
    );

    let buffered = normal_rx
        .recv()
        .await
        .ok_or_else(|| missing("buffered message should still exist"))?;
    assert_eq!(buffered.event.event_type(), "chat_message");

    let delivered = tokio::time::timeout(Duration::from_millis(100), normal_rx.recv())
        .await
        .map_err(|_| missing("critical event should be queued"))?
        .ok_or_else(|| missing("critical event should arrive on fallback channel"))?;
    assert_eq!(delivered.event.event_type(), critical_event.event_type());
    Ok(())
}

#[tokio::test]
async fn test_publish_only_enqueues_redis_without_rebroadcasting_locally() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_publish_only".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 4,
        publish_channel_capacity: 4,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let mut manager = RealtimeManager::new(config).await?;
    let room_id = RoomId::expect_positive(10_000_107);
    let user_id = UserId::expect_positive(10_000_108);
    let mut room_rx = manager
        .message_hub()
        .subscribe(room_id, user_id, ConnectionId::new("publish-only-conn"))
        .await?;
    let (critical_tx, mut critical_rx) = mpsc::channel::<PublishRequest>(4);
    manager.redis_critical_tx = Some(critical_tx);

    let event = RealtimeEvent::UserLeft {
        event_id: synctv_common::snanoid!(16),
        room_id,
        user_id,
        username: "publish-only".to_string(),
        remark_name: String::new(),
        display_tag: String::new(),
        role: i32::from(synctv_core::models::RoomRole::Member),
        timestamp: synctv_core::SystemClock.now(),
    };

    assert!(
        manager.publish_only(event.clone()),
        "publish_only should enqueue the Redis publish path"
    );

    let published = tokio::time::timeout(Duration::from_millis(100), critical_rx.recv())
        .await
        .map_err(|_| missing("event should reach Redis queue"))?
        .ok_or_else(|| missing("critical queue should stay open"))?;
    assert_eq!(published.event.event_type(), event.event_type());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), room_rx.recv())
            .await
            .is_err(),
        "publish_only must not duplicate local delivery"
    );
    Ok(())
}

#[tokio::test]
async fn test_publish_only_user_notification_does_not_hit_admin_channel() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_publish_only_user_notification".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 4,
        publish_channel_capacity: 4,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let mut manager = RealtimeManager::new(config).await?;
    let (publish_tx, mut publish_rx) = mpsc::channel::<PublishRequest>(4);
    manager.redis_publish_tx = Some(publish_tx);
    let mut admin_rx = manager.subscribe_admin_events();

    let event = RealtimeEvent::UserNotification {
        event_id: synctv_common::snanoid!(16),
        user_id: UserId::expect_positive(10_000_109),
        notification_id: "notification-1".to_string(),
        title: "title".to_string(),
        content: "content".to_string(),
        notification_type: synctv_core::models::NotificationType::SystemAnnouncement,
        data: synctv_core::models::NotificationData::default(),
        timestamp: synctv_core::SystemClock.now(),
    };

    assert!(
        manager.publish_only(event.clone()),
        "publish_only should enqueue UserNotification to Redis"
    );

    let published = tokio::time::timeout(Duration::from_millis(100), publish_rx.recv())
        .await
        .map_err(|_| missing("user notification should reach Redis queue"))?
        .ok_or_else(|| missing("publish queue should stay open"))?;
    assert_eq!(published.event.event_type(), event.event_type());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), admin_rx.recv())
            .await
            .is_err(),
        "publish_only must not emit UserNotification to the local admin channel"
    );
    Ok(())
}

#[tokio::test]
async fn test_publish_only_confirmed_waits_for_publisher_ack() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_publish_only_confirmed_waits".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 4,
        publish_channel_capacity: 4,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let mut manager = RealtimeManager::new(config).await?;
    let (publish_tx, mut publish_rx) = mpsc::channel::<PublishRequest>(4);
    manager.redis_publish_tx = Some(publish_tx);

    let event = RealtimeEvent::CacheInvalidate {
        event_id: synctv_common::snanoid!(16),
        targets: vec![CacheTarget::All],
        timestamp: synctv_core::SystemClock.now(),
    };
    let publish = manager.publish_only_confirmed(event, Duration::from_millis(50));
    tokio::pin!(publish);

    let queued = tokio::select! {
        queued = publish_rx.recv() => queued.ok_or_else(|| missing("event should be queued"))?,
        result = &mut publish => return Err(anyhow::anyhow!(
            "confirmed publish completed before publisher ack: {result:?}"
        ).into()),
    };
    assert_eq!(queued.event.event_type(), "cache_invalidate");
    assert!(
        (&mut publish).await.is_err(),
        "confirmed publish must not complete just because the queue accepted the event"
    );
    drop(queued);
    Ok(())
}

#[tokio::test]
async fn test_publish_only_confirmed_observes_publisher_success_ack() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_publish_only_confirmed_ack".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 4,
        publish_channel_capacity: 4,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let mut manager = RealtimeManager::new(config).await?;
    let (publish_tx, mut publish_rx) = mpsc::channel::<PublishRequest>(4);
    manager.redis_publish_tx = Some(publish_tx);

    let event = RealtimeEvent::CacheInvalidate {
        event_id: synctv_common::snanoid!(16),
        targets: vec![CacheTarget::All],
        timestamp: synctv_core::SystemClock.now(),
    };
    let publish = manager.publish_only_confirmed(event, Duration::from_secs(1));
    tokio::pin!(publish);

    let mut queued = tokio::select! {
        queued = publish_rx.recv() => queued.ok_or_else(|| missing("event should be queued"))?,
        result = &mut publish => return Err(anyhow::anyhow!(
            "confirmed publish completed before publisher ack: {result:?}"
        ).into()),
    };
    queued.acknowledge_success();
    (&mut publish).await?;
    Ok(())
}

#[tokio::test]
async fn test_broadcast_cache_invalidate_reaches_admin_channel() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_cache_invalidate_admin_channel".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 4,
        publish_channel_capacity: 4,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config).await?;
    let mut admin_rx = manager.subscribe_admin_events();
    let event = RealtimeEvent::CacheInvalidate {
        event_id: synctv_common::snanoid!(16),
        targets: vec![CacheTarget::Room {
            room_id: RoomId::expect_positive(10_000_110),
        }],
        timestamp: synctv_core::SystemClock.now(),
    };

    let result = manager.broadcast(event.clone());
    assert_eq!(
        result.local_sent, 0,
        "cache invalidation is not a room subscriber event"
    );

    let received = tokio::time::timeout(Duration::from_millis(100), admin_rx.recv())
        .await
        .map_err(|_| missing("CacheInvalidate should reach admin subscribers"))?
        .map_err(|_| missing("admin channel should stay open"))?;
    assert_eq!(received.event_id(), event.event_id());
    Ok(())
}

#[tokio::test]
async fn test_drop_aborts_injected_connection_manager_background_tasks() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_drop_connection_manager_cleanup".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 4,
        publish_channel_capacity: 4,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    let manager = RealtimeManager::new_with_runtime(
        config,
        RealtimeManagerRuntime {
            connection_runtime: Some(connection_manager.clone()),
            leader_runtime: None,
        },
    )
    .await?;

    drop(manager);
    tokio::task::yield_now().await;

    assert!(
        !connection_manager.background_tasks_running(),
        "drop fallback must clear ConnectionManager background tasks when graceful shutdown was never awaited"
    );
    Ok(())
}

/// Test that RealtimeManager metrics include quarantine state.
#[tokio::test]
async fn test_cluster_metrics_includes_quarantine_state() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_metrics_quarantine".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config).await?;

    let metrics = manager.metrics();

    // Verify all expected fields are present
    assert_eq!(metrics.node_id, "test_metrics_quarantine");
    assert_eq!(metrics.total_rooms, 0);
    assert_eq!(metrics.total_connections, 0);
    assert!(!metrics.distributed_enabled);
    assert!(
        !metrics.is_quarantined,
        "Should not be quarantined initially"
    );
    Ok(())
}

/// Test that explicit local-only unit tests still construct a manager without Redis.
#[tokio::test]
async fn test_local_only_manager_without_redis_still_builds() -> TestResult {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_cluster_requires_redis".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let result = RealtimeManager::new(config).await;

    assert!(
        result.is_ok(),
        "RealtimeManager::new should support explicit local-only tests without Redis"
    );

    let manager = result?;
    let metrics = manager.metrics();
    assert!(
        !metrics.distributed_enabled,
        "manager should remain local-only without distributed transport"
    );
    Ok(())
}

/// Test that distributed mode fails closed when Redis wiring is missing.
#[tokio::test]
async fn test_distributed_enabled_without_redis_returns_configuration_error() {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: true,
        node_id: "test_cluster_requires_redis".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let result = RealtimeManager::new(config).await;

    assert!(
        result.is_err(),
        "cluster.enabled=true must fail closed when Redis is absent"
    );
}

/// Test that partial Redis wiring in distributed mode does not silently degrade to local-only.
#[tokio::test]
async fn test_distributed_enabled_with_partial_redis_wiring_returns_configuration_error() {
    #[derive(Clone, Default)]
    struct FailingTransportFactory;

    impl RealtimeMessageTransportFactory for FailingTransportFactory {
        fn build(
            &self,
            _config: RealtimeMessageTransportConfig,
        ) -> RealtimeResult<Arc<dyn RealtimeMessageTransport>> {
            Err(crate::error::Error::Configuration(
                "test distributed transport unavailable".to_string(),
            ))
        }
    }

    let config = RealtimeConfig {
        distributed_transport_factory: Some(Arc::new(FailingTransportFactory)),
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: true,
        node_id: "test_cluster_missing_conn".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let result = RealtimeManager::new(config).await;

    assert!(
        result.is_err(),
        "cluster.enabled=true must fail closed on partial Redis wiring"
    );
}

/// Test that non-distributed mode works without Redis.
#[tokio::test]
async fn test_non_cluster_mode_works_without_redis() {
    let config = RealtimeConfig {
        distributed_transport_factory: None,
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_non_cluster_no_redis".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let result = RealtimeManager::new(config).await;

    assert!(
        result.is_ok(),
        "RealtimeManager::new should succeed in non-distributed mode without Redis, got error: {:?}",
        result.err()
    );
}

/// Test that standalone mode stays local-only even when a Redis client is provided.
///
/// This protects the single-node deployment contract: Redis may exist for
/// cache/shared-state features, but local fan-out must not start Redis
/// Pub/Sub consumers unless distributed mode is explicitly enabled.
#[tokio::test]
async fn test_non_cluster_mode_with_distributed_transport_remains_local_only() -> TestResult {
    let transport_factory = StubTransportFactory::default();
    let config = RealtimeConfig {
        distributed_transport_factory: Some(Arc::new(transport_factory.clone())),
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: false,
        node_id: "test_non_cluster_with_redis".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 1000,
        publish_channel_capacity: 10_000,
        key_prefix: "synctv:".to_string(),
        catchup_window_secs: 300,
        stream_max_length: 10_000,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config).await?;

    assert!(
        manager.redis_publish_tx().is_none(),
        "standalone mode must not start Redis publish channels"
    );
    assert!(
        !manager.metrics().distributed_enabled,
        "standalone mode must report local-only transport even when Redis is configured"
    );
    assert_eq!(
        transport_factory.start_count.load(Ordering::Relaxed),
        0,
        "standalone mode should ignore injected distributed transport"
    );
    Ok(())
}

#[tokio::test]
async fn test_realtime_manager_uses_injected_transport_factory() -> TestResult {
    let factory = Arc::new(StubTransportFactory::default());
    let config = RealtimeConfig {
        distributed_transport_factory: Some(factory.clone()),
        message_runtime: Arc::new(RoomMessageHub::new()),
        distributed_enabled: true,
        node_id: "test_trait_transport".to_string(),
        dedup_window: Duration::from_secs(1),
        critical_channel_capacity: 8,
        publish_channel_capacity: 8,
        key_prefix: "test:".to_string(),
        catchup_window_secs: 60,
        stream_max_length: 100,
        event_handler: None,
        parent_cancel_token: None,
    };

    let manager = RealtimeManager::new(config).await?;

    assert_eq!(
        factory.start_count.load(Ordering::Relaxed),
        1,
        "transport factory must be used to start distributed transport"
    );
    assert!(
        manager.metrics().distributed_enabled,
        "distributed transport should mark cross-node fanout as enabled"
    );

    manager.shutdown().await;

    assert_eq!(
        factory.shutdown_count.load(Ordering::Relaxed),
        1,
        "cluster shutdown must delegate to the injected transport"
    );
    Ok(())
}
