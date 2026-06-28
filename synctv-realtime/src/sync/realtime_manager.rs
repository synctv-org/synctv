//! Complete realtime synchronization service
//!
//! This module provides a unified interface for distributed realtime functionality:
//! - Message broadcasting (local)
//! - Redis pub/sub (cross-node)
//! - Message deduplication
//! - Connection management
//! - Metrics and monitoring

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, error, info, warn};

use super::dedup::{DedupKey, MessageDeduplicator};
use super::redis_pubsub::PublishRequest;
use super::room_hub::{ConnectionId, RoomMessageHub};
use super::runtime::{ConnectionRuntime, RoomMessageRuntime};
use super::transport::{
    RealtimeEventHandler, RealtimeMessageTransport, RealtimeMessageTransportConfig,
    RealtimeMessageTransportFactory,
};
use super::RealtimeEvent;
use crate::error::Result as RealtimeResult;
use synctv_cluster::discovery::{ClusterNodeDirectory, HeartbeatResult};
use synctv_core::config::ClusterChannelConfig;
use synctv_core::models::id::{RoomId, UserId};

#[cfg(not(test))]
const HEARTBEAT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const HEARTBEAT_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(200);

/// Realtime configuration
#[derive(Clone)]
pub struct RealtimeConfig {
    /// Optional distributed transport factory for cross-node fan-out.
    ///
    /// In standalone mode this stays `None`, even if Redis exists for caches or
    /// other shared-state concerns. The composition root chooses the concrete
    /// backend; `RealtimeManager` only depends on the abstraction.
    pub distributed_transport_factory: Option<Arc<dyn RealtimeMessageTransportFactory>>,
    /// Runtime used for local fan-out and room subscription tracking.
    ///
    /// The composition root decides whether this is local-only or shared across
    /// replicas; `RealtimeManager` only consumes the abstraction.
    pub message_runtime: Arc<dyn RoomMessageRuntime>,
    /// Whether distributed mode is explicitly enabled.
    /// When `true`, `RealtimeManager::new` will return an error if Redis is not configured.
    /// When `false`, missing Redis is allowed (single-node mode).
    /// Default: `false`
    pub distributed_enabled: bool,
    /// Unique identifier for this node
    pub node_id: String,
    /// Deduplication window duration
    pub dedup_window: Duration,
    /// Capacity for the high-priority critical event channel.
    /// Senders apply backpressure here so normal-channel pressure cannot drop
    /// critical events before they reach the Redis publisher.
    pub critical_channel_capacity: usize,
    /// Capacity for the normal-priority Redis publish channel.
    /// Normal events are dropped with warning when full.
    pub publish_channel_capacity: usize,
    /// Key prefix for Redis keys and pub/sub channels (e.g., "synctv:")
    pub key_prefix: String,
    /// How far back (in seconds) to replay Redis Stream events when a new node
    /// first joins distributed realtime. Mirrors the deployment channel catchup window.
    /// Default: 300 (5 minutes)
    pub catchup_window_secs: u64,
    /// Maximum number of entries per Redis Stream (approximate).
    /// Mirrors the deployment channel stream length setting.
    /// Default: 100000
    pub stream_max_length: usize,
    /// Optional application-owned handler for side effects caused by remote realtime events.
    ///
    /// Realtime itself only delivers events. Application/core owned code can use
    /// this hook to update permission caches, L1 caches, projections, or other
    /// business state without teaching the transport layer about those domains.
    pub event_handler: Option<Arc<dyn RealtimeEventHandler>>,
    /// Optional parent cancellation token (e.g., from `ShutdownCoordinator`).
    /// When provided, the `RealtimeManager`'s internal token is created as a
    /// child of this token, so cancelling the parent also cancels all realtime
    /// background tasks. When `None`, an independent token is created.
    pub parent_cancel_token: Option<CancellationToken>,
}

#[derive(Clone, Default)]
pub struct RealtimeManagerRuntime {
    /// Optional connection runtime for coordinated shutdown.
    pub connection_runtime: Option<Arc<dyn ConnectionRuntime>>,
    /// Optional leader runtime for resigning leadership on epoch mismatch.
    pub leader_runtime: Option<Arc<dyn synctv_cluster::leader::LeaderRuntime>>,
}

impl std::fmt::Debug for RealtimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealtimeConfig")
            .field(
                "distributed_transport_factory",
                &self
                    .distributed_transport_factory
                    .as_ref()
                    .map(|_| "configured"),
            )
            .field("message_runtime", &"Arc<dyn RoomMessageRuntime>")
            .field("distributed_enabled", &self.distributed_enabled)
            .field("node_id", &self.node_id)
            .field("dedup_window", &self.dedup_window)
            .field("critical_channel_capacity", &self.critical_channel_capacity)
            .field("publish_channel_capacity", &self.publish_channel_capacity)
            .field("key_prefix", &self.key_prefix)
            .field("catchup_window_secs", &self.catchup_window_secs)
            .field("stream_max_length", &self.stream_max_length)
            .field(
                "event_handler",
                &self.event_handler.as_ref().map(|_| "configured"),
            )
            .field(
                "parent_cancel_token",
                &self.parent_cancel_token.as_ref().map(|_| "Some(..)"),
            )
            .finish()
    }
}

impl Default for RealtimeConfig {
    fn default() -> Self {
        let cluster_config = ClusterChannelConfig::default();
        Self {
            distributed_transport_factory: None,
            message_runtime: Arc::new(RoomMessageHub::new()),
            distributed_enabled: false,
            node_id: format!("node_{}", synctv_common::snanoid!(8)),
            dedup_window: Duration::from_mins(15),
            critical_channel_capacity: cluster_config.critical_channel_capacity,
            publish_channel_capacity: cluster_config.publish_channel_capacity,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: cluster_config.catchup_window_secs,
            stream_max_length: cluster_config.stream_max_length,
            event_handler: None,
            parent_cancel_token: None,
        }
    }
}

/// Realtime synchronization manager
///
/// This is the main entry point for distributed realtime functionality.
/// It manages:
/// - Local message broadcasting via `RoomMessageHub`
/// - Cross-node synchronization via Redis Pub/Sub
/// - Message deduplication
/// - Connection lifecycle
pub struct RealtimeManager {
    /// Message hub for local broadcasting
    message_hub: Arc<dyn RoomMessageRuntime>,
    /// Deduplicator for preventing duplicate events
    deduplicator: Arc<MessageDeduplicator>,
    /// Sender for publishing events to Redis (normal priority)
    redis_publish_tx: Option<mpsc::Sender<PublishRequest>>,
    /// Sender for publishing critical events to Redis (high priority)
    redis_critical_tx: Option<mpsc::Sender<PublishRequest>>,
    /// This node's unique identifier
    node_id: String,
    /// Broadcast channel for admin events (kick, etc.) received from cluster
    admin_event_tx: broadcast::Sender<RealtimeEvent>,
    /// Internal channel for local lifecycle side effects that must not be
    /// replayed to room/admin subscribers.
    lifecycle_event_tx: broadcast::Sender<RealtimeEvent>,
    /// Distributed transport service (stored for graceful shutdown)
    distributed_transport: Option<Arc<dyn RealtimeMessageTransport>>,
    /// JoinHandle for the Redis publisher task.
    /// Awaited during shutdown so in-flight events are fully flushed before
    /// the process exits.
    publisher_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// JoinHandle for the critical-event forwarder task.
    /// Awaited during shutdown before the Redis publisher is stopped so the
    /// dedicated critical queue is fully drained.
    critical_forwarder_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Tracks retry tasks spawned when the critical queue is full.
    /// These tasks must finish enqueueing before shutdown drains the queue.
    critical_retry_tasks: TaskTracker,
    /// Cancellation token for background heartbeat task
    cancel_token: CancellationToken,
    /// Node registry + heartbeat handle (behind Mutex for async shutdown from &self)
    heartbeat_state: tokio::sync::Mutex<HeartbeatState>,
    /// Capacity for the critical event channel (for logging)
    critical_channel_capacity: usize,
    /// Capacity for the publish channel (for logging)
    publish_channel_capacity: usize,
    /// Optional connection manager for coordinated shutdown
    connection_manager: Option<Arc<dyn ConnectionRuntime>>,
    /// Independent heartbeat failure counter for business logic (network partition detection).
    /// The Prometheus `CLUSTER_HEARTBEAT_FAILURES` gauge is written but never read for decisions.
    heartbeat_failure_count: Arc<AtomicU64>,
    /// Consecutive epoch mismatch counter for split-brain detection.
    /// When epoch mismatch exceeds threshold, node enters quarantine mode.
    epoch_mismatch_count: Arc<AtomicU64>,
    /// Flag indicating node is quarantined due to epoch mismatch (split-brain).
    /// When true, fan-out requests are rejected and leadership is resigned.
    is_quarantined: Arc<AtomicBool>,
    /// Ensures shutdown work only runs once even if called multiple times.
    shutdown_started: Arc<AtomicBool>,
    /// Controls whether Redis fan-out is still accepting new work.
    /// Shutdown flips this after in-flight critical event producers have drained.
    redis_publish_accepting: Arc<AtomicBool>,
    /// Leader elector for resigning leadership on epoch mismatch
    leader_elector: Option<Arc<dyn synctv_cluster::leader::LeaderRuntime>>,
}

/// State for the background heartbeat loop, guarded by Mutex for async shutdown
struct HeartbeatState {
    node_registry: Option<Arc<dyn ClusterNodeDirectory>>,
    handle: Option<tokio::task::JoinHandle<()>>,
    /// Stored API address for heartbeat re-registration (avoid empty-address bug)
    api_address: String,
}

async fn await_shutdown_handle(
    name: &'static str,
    mut handle: tokio::task::JoinHandle<()>,
    timeout: Duration,
) {
    match tokio::time::timeout(timeout, &mut handle).await {
        Ok(Ok(())) => info!("{name} completed cleanly during shutdown"),
        Ok(Err(e)) => warn!(error = %e, "{name} panicked during shutdown"),
        Err(_) => {
            warn!(
                "{name} did not finish within {}s timeout during shutdown; aborting",
                timeout.as_secs()
            );
            handle.abort();
            match handle.await {
                Ok(()) => info!("{name} aborted cleanly during shutdown"),
                Err(e) if e.is_cancelled() => info!("{name} aborted during shutdown"),
                Err(e) => warn!(error = %e, "{name} failed after abort during shutdown"),
            }
        }
    }
}

impl RealtimeManager {
    /// Create a new realtime manager
    ///
    /// # Arguments
    /// * `config` - Realtime configuration
    pub async fn new(config: RealtimeConfig) -> RealtimeResult<Self> {
        Self::new_with_runtime(config, RealtimeManagerRuntime::default()).await
    }

    pub async fn new_with_runtime(
        config: RealtimeConfig,
        runtime: RealtimeManagerRuntime,
    ) -> RealtimeResult<Self> {
        let deduplicator = Arc::new(MessageDeduplicator::new(config.dedup_window));
        let manager_cancel_token = config.parent_cancel_token.as_ref().map_or_else(
            CancellationToken::new,
            tokio_util::sync::CancellationToken::child_token,
        );
        let critical_retry_tasks = TaskTracker::new();

        let (admin_event_tx, _) = broadcast::channel(4096);
        let (lifecycle_event_tx, _) = broadcast::channel(4096);
        let distributed_transport_ready =
            config.distributed_enabled && config.distributed_transport_factory.is_some();
        let message_hub = config.message_runtime.clone();

        // Start distributed transport only when distributed mode is explicitly enabled.
        // In standalone mode, Redis may still exist for caches/shared state, but
        // realtime fan-out stays local-only.
        let (
            redis_publish_tx,
            redis_critical_tx,
            distributed_transport,
            publisher_handle,
            critical_forwarder_handle,
        ) = if distributed_transport_ready {
            let distributed_transport = config
                .distributed_transport_factory
                .as_ref()
                .ok_or_else(|| {
                    crate::error::Error::Configuration(
                        "cluster.enabled=true requires shared realtime transport".to_string(),
                    )
                })?
                .build(RealtimeMessageTransportConfig {
                    message_runtime: message_hub.clone(),
                    node_id: config.node_id.clone(),
                    key_prefix: config.key_prefix.clone(),
                    admin_event_tx: admin_event_tx.clone(),
                    event_handler: config.event_handler.clone(),
                    deduplicator: deduplicator.clone(),
                    catchup_window_secs: config.catchup_window_secs,
                    stream_max_length: config.stream_max_length,
                })?;

            let transport_runtime = distributed_transport
                .clone()
                .start(config.publish_channel_capacity)
                .await?;

            let tx = transport_runtime.publish_tx.clone();
            let publisher_handle = transport_runtime.publisher_handle;
            // Critical events share the same distributed publisher but use a separate
            // bounded channel so normal-channel pressure cannot evict them.
            let critical_capacity = config.critical_channel_capacity;
            let (critical_tx, mut critical_rx) = mpsc::channel::<PublishRequest>(critical_capacity);
            // Forward critical events into the normal publish channel using `.send().await`
            // so channel pressure becomes backpressure on critical senders.
            let normal_tx = tx.clone();
            let cancel_critical = manager_cancel_token.clone();
            let critical_forwarder_handle = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = cancel_critical.cancelled() => {
                            // Drain remaining critical events before exiting
                            while let Ok(req) = critical_rx.try_recv() {
                                let event_type = req.event.event_type();
                                if let Err(error) = normal_tx.send(req).await {
                                    error!(
                                        event_type = %event_type,
                                        error = %error,
                                        "Critical event publish channel closed while draining"
                                    );
                                    return;
                                }
                            }
                            return;
                        }
                        req = critical_rx.recv() => {
                            if let Some(req) = req {
                                if let Err(e) = normal_tx.send(req).await {
                                    error!("Critical event publish channel closed: {e}");
                                    return;
                                }
                            } else {
                                return;
                            }
                        }
                    }
                }
            });

            (
                Some(tx),
                Some(critical_tx),
                Some(distributed_transport),
                Some(publisher_handle),
                Some(critical_forwarder_handle),
            )
        } else {
            if config.distributed_enabled {
                return Err(crate::error::Error::Configuration(
                    "cluster.enabled=true requires shared realtime transport".to_string(),
                ));
            }
            if config.distributed_transport_factory.is_some() {
                warn!(
                    "Distributed transport provided while distributed mode is disabled; RealtimeManager remains local-only"
                );
            } else {
                warn!("Distributed transport not provided, running in single-node mode");
            }
            (None, None, None, None, None)
        };

        Ok(Self {
            message_hub,
            deduplicator,
            redis_publish_tx,
            redis_critical_tx,
            node_id: config.node_id,
            admin_event_tx,
            lifecycle_event_tx,
            distributed_transport,
            publisher_task: tokio::sync::Mutex::new(publisher_handle),
            critical_forwarder_task: tokio::sync::Mutex::new(critical_forwarder_handle),
            critical_retry_tasks,
            cancel_token: manager_cancel_token,
            critical_channel_capacity: config.critical_channel_capacity,
            publish_channel_capacity: config.publish_channel_capacity,
            heartbeat_state: tokio::sync::Mutex::new(HeartbeatState {
                node_registry: None,
                handle: None,
                api_address: String::new(),
            }),
            connection_manager: runtime.connection_runtime,
            heartbeat_failure_count: Arc::new(AtomicU64::new(0)),
            epoch_mismatch_count: Arc::new(AtomicU64::new(0)),
            is_quarantined: Arc::new(AtomicBool::new(false)),
            shutdown_started: Arc::new(AtomicBool::new(false)),
            redis_publish_accepting: Arc::new(AtomicBool::new(true)),
            leader_elector: runtime.leader_runtime,
        })
    }

    /// Get the message hub (for subscriptions)
    #[must_use]
    pub const fn message_hub(&self) -> &Arc<dyn RoomMessageRuntime> {
        &self.message_hub
    }

    /// Get the deduplicator
    #[must_use]
    pub const fn deduplicator(&self) -> &Arc<MessageDeduplicator> {
        &self.deduplicator
    }

    /// Get this node's unique identifier
    #[must_use]
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Get the cancellation token (for coordinating background tasks)
    #[must_use]
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Check if this node is quarantined due to epoch mismatch.
    ///
    /// When true, the node has detected split-brain (epoch mismatch) and
    /// should reject fan-out requests and leadership operations until
    /// successfully re-registered with a new epoch.
    #[must_use]
    pub fn is_quarantined(&self) -> bool {
        self.is_quarantined.load(Ordering::Acquire)
    }

    /// Get the Redis publish sender
    #[must_use]
    pub const fn redis_publish_tx(&self) -> Option<&mpsc::Sender<PublishRequest>> {
        self.redis_publish_tx.as_ref()
    }

    /// Subscribe to admin events (kick, etc.) received from cluster
    #[must_use]
    pub fn subscribe_admin_events(&self) -> broadcast::Receiver<RealtimeEvent> {
        self.admin_event_tx.subscribe()
    }

    /// Subscribe to internal lifecycle side-effect events.
    ///
    /// These events are consumed by server-owned lifecycle workers and are not
    /// delivered to room or admin subscribers.
    #[must_use]
    pub fn subscribe_lifecycle_events(&self) -> broadcast::Receiver<RealtimeEvent> {
        self.lifecycle_event_tx.subscribe()
    }

    /// Get the admin event sender (for local kick events)
    #[must_use]
    pub const fn admin_event_tx(&self) -> &broadcast::Sender<RealtimeEvent> {
        &self.admin_event_tx
    }

    fn validate_redis_publish(&self, event: &RealtimeEvent, is_critical: bool) -> bool {
        let event_type = event.event_type();

        if self.is_quarantined() {
            warn!(
                event_type = %event_type,
                room_id = %event.room_id()
                    .map_or_else(|| "n/a".to_string(), ToString::to_string),
                "Rejecting Redis publish because node is quarantined"
            );
            return false;
        }

        if self.shutdown_started.load(Ordering::Acquire) && !is_critical {
            debug!(
                event_type = %event_type,
                "Skipping Redis publish because RealtimeManager shutdown is in progress"
            );
            return false;
        }
        if !self.redis_publish_accepting.load(Ordering::Acquire) {
            debug!(
                event_type = %event_type,
                "Skipping Redis publish because RealtimeManager is draining publisher shutdown"
            );
            return false;
        }

        true
    }

    fn enqueue_redis_publish_request(
        &self,
        req: PublishRequest,
        is_critical: bool,
        allow_waiting_retry: bool,
    ) -> bool {
        if is_critical {
            if let Some(tx) = &self.redis_critical_tx {
                self.enqueue_critical_publish_request(
                    tx,
                    req,
                    self.critical_channel_capacity,
                    "critical",
                    allow_waiting_retry,
                )
            } else if let Some(tx) = &self.redis_publish_tx {
                self.enqueue_critical_publish_request(
                    tx,
                    req,
                    self.publish_channel_capacity,
                    "fallback",
                    allow_waiting_retry,
                )
            } else {
                false
            }
        } else if let Some(tx) = &self.redis_publish_tx {
            match tx.try_send(req) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
                        .with_label_values(&["channel_full"])
                        .inc();
                    warn!(
                        "Redis publish channel full (capacity {}), dropping event",
                        self.publish_channel_capacity
                    );
                    false
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    error!("Redis publish channel closed, cannot queue event");
                    false
                }
            }
        } else {
            false
        }
    }

    fn enqueue_critical_publish_request(
        &self,
        tx: &mpsc::Sender<PublishRequest>,
        req: PublishRequest,
        capacity: usize,
        channel: &'static str,
        allow_waiting_retry: bool,
    ) -> bool {
        match tx.try_send(req) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(req)) if allow_waiting_retry => {
                let tx = tx.clone();
                warn!(
                    channel = %channel,
                    capacity = capacity,
                    "Critical realtime publish channel full, spawning tracked retry task"
                );
                self.critical_retry_tasks.spawn(async move {
                    if let Err(error) = tx.send(req).await {
                        error!(
                            channel = %channel,
                            error = %error,
                            "Failed to send critical event after retry"
                        );
                    }
                });
                true
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    channel = %channel,
                    capacity = capacity,
                    "Critical realtime publish channel full, rejecting confirmed publish"
                );
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                error!(channel = %channel, "Critical realtime publish channel closed");
                false
            }
        }
    }

    fn enqueue_redis_publish(&self, event: RealtimeEvent, is_critical: bool) -> bool {
        self.enqueue_redis_publish_request(PublishRequest::new(event), is_critical, true)
    }

    /// Broadcast an event to local subscribers only.
    ///
    /// This preserves deduplication semantics for the event without publishing it
    /// to Redis. It is used when callers need to preserve local correctness first
    /// and handle cross-node retries separately.
    pub fn broadcast_local(&self, event: RealtimeEvent) -> usize {
        self.broadcast_local_inner(event, true)
    }

    /// Deliver an outbox-claimed event to local lifecycle side-effect consumers
    /// without recording it in the shared realtime deduplicator.
    ///
    /// This deliberately does not use room/admin subscriber delivery. API paths
    /// already deliver the event locally after commit; the outbox dispatcher only
    /// needs to keep retryable server-side lifecycle effects such as local stream
    /// kicks.
    pub fn broadcast_local_outbox_side_effect(&self, event: RealtimeEvent) -> usize {
        match event {
            event @ (RealtimeEvent::KickPublisher { .. }
            | RealtimeEvent::KickUser { .. }
            | RealtimeEvent::KickUserFromRoom { .. }
            | RealtimeEvent::RoomDeleted { .. }
            | RealtimeEvent::RoomBanned { .. }
            | RealtimeEvent::RoomOwnerInactive { .. }) => match self.lifecycle_event_tx.send(event)
            {
                Ok(sent) => sent,
                Err(error) => {
                    warn!(
                        error = %error,
                        "Failed to deliver outbox local lifecycle side-effect event"
                    );
                    0
                }
            },
            event => {
                debug!(
                    event_type = %event.event_type(),
                    "Skipping outbox local side-effect delivery for event without local lifecycle effect"
                );
                0
            }
        }
    }

    fn broadcast_local_inner(&self, event: RealtimeEvent, use_dedup: bool) -> usize {
        let event_type = event.event_type();

        if self.is_quarantined() {
            warn!(
                event_type = %event_type,
                room_id = %event.room_id()
                    .map_or_else(|| "n/a".to_string(), ToString::to_string),
                "Rejecting local broadcast because node is quarantined"
            );
            return 0;
        }

        if self.shutdown_started.load(Ordering::Acquire) && !event.is_critical() {
            debug!(
                event_type = %event_type,
                "Skipping local event because RealtimeManager shutdown is in progress"
            );
            return 0;
        }

        if use_dedup {
            let dedup_key = match DedupKey::try_from_event(&event) {
                Ok(key) => key,
                Err(error) => {
                    warn!(
                        event_type = %event_type,
                        error = %error,
                        "Dropping local realtime event with invalid dedup identity"
                    );
                    return 0;
                }
            };
            if !self.deduplicator.should_process(&dedup_key) {
                debug!(
                    event_type = %event_type,
                    room_id = %event.room_id()
                        .map_or_else(|| "n/a".to_string(), ToString::to_string),
                    "Duplicate event detected, skipping local broadcast"
                );
                return 0;
            }
        }

        let mut local_sent = 0;
        if event.delivers_to_room_channel() {
            if let Some(room_id) = event.room_id().copied() {
                local_sent = self.message_hub.broadcast(&room_id, &event);
            }
        }
        if event.delivers_to_admin_channel() {
            super::events::publish_admin_event(&self.admin_event_tx, event, "local");
        }

        local_sent
    }

    /// Publish an event to Redis without re-broadcasting it locally.
    ///
    /// This is primarily used by retry paths that have already delivered the
    /// event locally and only need cross-node fan-out.
    pub fn publish_only(&self, event: RealtimeEvent) -> bool {
        let event_type = event.event_type();
        let is_critical = event.is_critical();

        if !self.validate_redis_publish(&event, is_critical) {
            return false;
        }

        let redis_sent = self.enqueue_redis_publish(event, is_critical);

        debug!(
            event_type = %event_type,
            redis_published = redis_sent,
            "Redis-only publish complete"
        );

        redis_sent
    }

    /// Publish an event to Redis and wait for the publisher task to confirm the
    /// Redis XADD+PUBLISH write. This is for durable retry paths that must not
    /// mark their own outbox row sent merely because the in-process queue
    /// accepted the event.
    pub async fn publish_only_confirmed(
        &self,
        event: RealtimeEvent,
        timeout_duration: Duration,
    ) -> std::result::Result<(), String> {
        let event_type = event.event_type();
        let is_critical = event.is_critical();

        if !self.validate_redis_publish(&event, is_critical) {
            return Err("Realtime publish queue rejected event".to_string());
        }

        let (request, ack) = PublishRequest::with_ack(event);
        if !self.enqueue_redis_publish_request(request, is_critical, false) {
            return Err("Realtime publish queue rejected event".to_string());
        }

        match tokio::time::timeout(timeout_duration, ack).await {
            Ok(Ok(Ok(()))) => {
                debug!(
                    event_type = %event_type,
                    "Confirmed Redis-only publish complete"
                );
                Ok(())
            }
            Ok(Ok(Err(error))) => Err(error),
            Ok(Err(_closed)) => Err("Realtime publisher dropped confirmation channel".to_string()),
            Err(_elapsed) => Err(format!(
                "Timed out waiting for confirmed Redis publish after {}ms",
                timeout_duration.as_millis()
            )),
        }
    }

    /// Start a background heartbeat loop that keeps this node alive in Redis.
    ///
    /// Calls `NodeRegistry::heartbeat()` every `heartbeat_timeout / 3` seconds.
    /// If the heartbeat indicates re-registration is needed (key expired or
    /// epoch mismatch), the node automatically re-registers.
    ///
    /// The optional `connection_count_fn` callback is invoked before each heartbeat
    /// to publish the current connection count to Redis metadata, enabling the
    /// `LeastConnections` load balancing strategy.
    ///
    /// Must be called after `register()` on the `NodeRegistry`.
    pub async fn start_heartbeat_loop<N, F>(
        &self,
        node_registry: Arc<N>,
        api_address: String,
        connection_count_fn: Option<F>,
    ) where
        N: ClusterNodeDirectory + 'static,
        F: Fn() -> usize + Send + Sync + 'static,
    {
        self.start_heartbeat_loop_with_directory(node_registry, api_address, connection_count_fn)
            .await;
    }

    pub async fn start_heartbeat_loop_with_directory<F>(
        &self,
        node_registry: Arc<dyn ClusterNodeDirectory>,
        api_address: String,
        connection_count_fn: Option<F>,
    ) where
        F: Fn() -> usize + Send + Sync + 'static,
    {
        let cancel_token = self.cancel_token.clone();
        let interval_secs =
            u64::try_from((node_registry.heartbeat_timeout_secs() / 3).max(1)).unwrap_or(1);
        let failure_count = self.heartbeat_failure_count.clone();
        let epoch_mismatch_count = self.epoch_mismatch_count.clone();
        let is_quarantined = self.is_quarantined.clone();
        let leader_elector = self.leader_elector.clone();

        // Store the API address into the spawned task so it can be used for
        // re-registration when the local cache has been lost or corrupted.
        let stored_api_address = api_address.clone();

        let registry_for_task = node_registry.clone();
        let handle = tokio::spawn(async move {
            let node_registry = registry_for_task;
            let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
            // Skip the first immediate tick (node was just registered)
            ticker.tick().await;

            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        info!("Heartbeat loop cancelled");
                        return;
                    }
                    _ = ticker.tick() => {
                        // Publish connection count to Redis metadata before heartbeat
                        if let Some(ref count_fn) = connection_count_fn {
                            let count = count_fn();
                            node_registry.update_local_metadata("connections", count.to_string()).await;
                        }

                        match node_registry.heartbeat().await {
                            Ok(HeartbeatResult::Ok) => {
                                debug!("Heartbeat sent successfully");
                                // Reset consecutive failure counter on success (for partition detection)
                                failure_count.store(0, Ordering::Release);
                                synctv_core::metrics::cluster::CLUSTER_HEARTBEAT_FAILURES.set(0);
                                // Exit quarantine on successful heartbeat
                                epoch_mismatch_count.store(0, Ordering::Release);
                                is_quarantined.store(false, Ordering::Release);
                                synctv_core::metrics::cluster::CLUSTER_EPOCH_MISMATCH_QUARANTINE.set(0);
                            }
                            Ok(HeartbeatResult::NeedReregistration) => {
                                // NodeRegistry::heartbeat() already attempted auto-registration
                                // internally. If we still get NeedReregistration, it means the
                                // internal retry failed.
                                warn!("Node key expired in Redis, internal auto-registration failed; \
                                       attempting re-registration with stored api_address");
                                if let Err(e) = node_registry
                                    .register(stored_api_address.clone())
                                    .await
                                {
                                    error!(
                                        error = %e,
                                        "Re-registration with stored api_address also failed; will retry on next heartbeat"
                                    );
                                } else {
                                    info!("Re-registration with stored api_address succeeded");
                                }
                            }
                            Ok(HeartbeatResult::EpochMismatch(remote_epoch)) => {
                                // Increment epoch mismatch counter
                                let mismatches = epoch_mismatch_count.fetch_add(1, Ordering::AcqRel) + 1;
                                warn!(
                                    remote_epoch = remote_epoch,
                                    consecutive_mismatches = mismatches,
                                    "Epoch mismatch during heartbeat, internal auto-registration failed; \
                                     attempting re-registration with stored api_address"
                                );

                                if let Err(e) = node_registry
                                    .register(stored_api_address.clone())
                                    .await
                                {
                                    error!(
                                        error = %e,
                                        "Re-registration with stored api_address also failed after epoch mismatch"
                                    );
                                } else {
                                    info!("Re-registration with stored api_address succeeded after epoch mismatch");
                                    // Reset epoch mismatch counter on successful re-registration
                                    epoch_mismatch_count.store(0, Ordering::Release);
                                    is_quarantined.store(false, Ordering::Release);
                                    synctv_core::metrics::cluster::CLUSTER_EPOCH_MISMATCH_QUARANTINE.set(0);
                                    continue;
                                }

                                // After 2 consecutive epoch mismatches, enter quarantine
                                if mismatches >= 2 {
                                    error!(
                                        remote_epoch = remote_epoch,
                                        consecutive_mismatches = mismatches,
                                        "Split-brain detected: multiple epoch mismatches, entering quarantine"
                                    );
                                    is_quarantined.store(true, Ordering::Release);
                                    synctv_core::metrics::cluster::CLUSTER_EPOCH_MISMATCH_QUARANTINE.set(1);

                                    // Resign leadership if we are the leader
                                    if let Some(ref elector) = leader_elector {
                                        if elector.is_leader() {
                                            warn!("Resigning leadership due to epoch mismatch (split-brain prevention)");
                                            // Call resign to immediately release the distributed lock
                                            elector.resign().await;
                                        }
                                    }
                                }
                            }
                            Ok(HeartbeatResult::EmptyAddress) => {
                                warn!(
                                    "Heartbeat: local cache has empty api_address; \
                                     attempting re-registration with stored api_address ({})",
                                    stored_api_address
                                );
                                if let Err(e) = node_registry
                                    .register(stored_api_address.clone())
                                    .await
                                {
                                    error!(
                                        error = %e,
                                        "Re-registration with stored api_address failed; \
                                         node remains unreachable by peers"
                                    );
                                } else {
                                    info!("Re-registration with stored api_address succeeded; \
                                           node should be reachable again");
                                }
                            }
                            Err(e) => {
                                // Increment independent failure counter for business logic
                                let failures = failure_count.fetch_add(1, Ordering::AcqRel) + 1;
                                // Update Prometheus gauge for monitoring only (never read for decisions)
                                synctv_core::metrics::cluster::CLUSTER_HEARTBEAT_FAILURES
                                    .set(i64::try_from(failures).unwrap_or(i64::MAX));
                                error!(
                                    error = %e,
                                    consecutive_failures = failures,
                                    "Heartbeat failed (Redis error), will retry"
                                );

                                // After 3 consecutive failures, suspect network partition
                                // Log a warning but continue attempting heartbeats for recovery
                                if failures >= 3 {
                                    warn!(
                                        consecutive_failures = failures,
                                        "Possible network partition: {} consecutive Redis heartbeat failures",
                                        failures
                                    );
                                }
                            }
                        }
                    }
                }
            }
        });

        // Store the node_registry, handle, and api_address for re-registration
        let mut state = self.heartbeat_state.lock().await;
        state.node_registry = Some(node_registry);
        state.handle = Some(handle);
        state.api_address = api_address;
        info!(interval_secs = interval_secs, "Heartbeat loop started");
    }

    /// Gracefully shut down the realtime manager and all background tasks.
    ///
    /// This method:
    /// 1. Cancels the heartbeat loop
    /// 2. Unregisters this node from Redis (so peers stop routing traffic immediately)
    /// 3. Drains critical retry work and stops the critical forwarder
    /// 4. Shuts down Redis Pub/Sub and local realtime runtimes
    /// 5. Awaits the publisher task's completion
    /// 6. Clears the deduplication cache
    pub async fn shutdown(&self) {
        if self.shutdown_started.swap(true, Ordering::AcqRel) {
            debug!("RealtimeManager shutdown already completed or in progress");
            return;
        }

        info!("Shutting down RealtimeManager");

        // Cancel heartbeat loop
        self.cancel_token.cancel();

        {
            let mut state = self.heartbeat_state.lock().await;
            // Stop the heartbeat loop first to prevent it from re-registering
            // the node between unregister and shutdown completion (TOCTOU race).
            if let Some(handle) = state.handle.take() {
                await_shutdown_handle("Heartbeat task", handle, HEARTBEAT_SHUTDOWN_TIMEOUT).await;
            }
            // Unregister only after heartbeat has stopped, ensuring the
            // registration won't be re-created by a concurrent heartbeat tick.
            if let Some(ref registry) = state.node_registry {
                if let Err(e) = registry.unregister().await {
                    warn!(error = %e, "Failed to unregister node during shutdown");
                } else {
                    info!("Node unregistered from Redis during shutdown");
                }
            }
        }

        self.redis_publish_accepting.store(false, Ordering::Release);
        self.critical_retry_tasks.close();

        match tokio::time::timeout(Duration::from_secs(5), self.critical_retry_tasks.wait()).await {
            Ok(()) => {
                debug!("Critical-event retry tasks completed during shutdown");
            }
            Err(_) => {
                warn!("Critical-event retry tasks did not finish within 5s timeout during shutdown; proceeding");
            }
        }

        {
            let mut forwarder_guard = self.critical_forwarder_task.lock().await;
            if let Some(handle) = forwarder_guard.take() {
                await_shutdown_handle("Critical-event forwarder", handle, Duration::from_secs(5))
                    .await;
            }
        }

        // Cancel Redis Pub/Sub tasks and await subscriber completion
        if let Some(ref transport) = self.distributed_transport {
            transport.shutdown().await;
        }

        // Shut down ConnectionManager's TTL refresh task
        if let Some(ref cm) = self.connection_manager {
            let report = cm.shutdown().await;
            if !report.all_clean() {
                warn!(
                    ?report,
                    "ConnectionManager reported background task issues during realtime shutdown"
                );
            }
        }

        // Shut down RoomMessageHub background tasks.
        self.message_hub.shutdown().await;

        // Await the publisher task so any in-flight events are fully flushed before
        // we return. A 10-second timeout prevents hanging indefinitely when Redis is
        // unreachable during shutdown.
        {
            let mut publisher_guard = self.publisher_task.lock().await;
            if let Some(handle) = publisher_guard.take() {
                await_shutdown_handle("Redis publisher task", handle, Duration::from_secs(10))
                    .await;
            }
        }

        self.deduplicator.clear();
    }

    /// Broadcast an event to all subscribers
    ///
    /// This will:
    /// 1. Check for duplicates
    /// 2. Broadcast to local subscribers
    /// 3. Publish to Redis for cross-node sync
    pub fn broadcast(&self, event: RealtimeEvent) -> BroadcastResult {
        let event_type = event.event_type();
        let is_critical = event.is_critical();

        if self.is_quarantined() {
            warn!(
                event_type = %event_type,
                room_id = %event.room_id()
                    .map_or_else(|| "n/a".to_string(), ToString::to_string),
                "Rejecting broadcast because node is quarantined"
            );
            return BroadcastResult {
                local_sent: 0,
                redis_sent: false,
            };
        }

        if self.shutdown_started.load(Ordering::Acquire) && !is_critical {
            debug!(
                event_type = %event_type,
                "Skipping event because RealtimeManager shutdown is in progress"
            );
            return BroadcastResult {
                local_sent: 0,
                redis_sent: false,
            };
        }
        if !self.redis_publish_accepting.load(Ordering::Acquire) {
            debug!(
                event_type = %event_type,
                "Skipping Redis fan-out because RealtimeManager is draining publisher shutdown"
            );
            return BroadcastResult {
                local_sent: 0,
                redis_sent: false,
            };
        }

        let dedup_key = match DedupKey::try_from_event(&event) {
            Ok(key) => key,
            Err(error) => {
                warn!(
                    event_type = %event_type,
                    error = %error,
                    "Dropping realtime event with invalid dedup identity"
                );
                return BroadcastResult {
                    local_sent: 0,
                    redis_sent: false,
                };
            }
        };

        // Check if this is a duplicate
        if !self.deduplicator.should_process(&dedup_key) {
            debug!(
                event_type = %event_type,
                room_id = %event.room_id()
                    .map_or_else(|| "n/a".to_string(), std::string::ToString::to_string),
                "Duplicate event detected, skipping"
            );
            return BroadcastResult {
                local_sent: 0,
                redis_sent: false,
            };
        }

        let mut local_sent = 0;

        // Get room_id for broadcasting
        if event.delivers_to_room_channel() {
            if let Some(room_id) = event.room_id() {
                // Broadcast to local subscribers
                local_sent = self.message_hub.broadcast(room_id, &event);
            }
        }

        // Admin-routed events reach app-level handlers and user-targeted WebSocket handlers.
        if event.delivers_to_admin_channel() {
            super::events::publish_admin_event(&self.admin_event_tx, event.clone(), "outbound");
        }

        let redis_sent = self.enqueue_redis_publish(event, is_critical);

        // Record realtime metrics
        synctv_core::metrics::cluster::REALTIME_EVENTS_PUBLISHED
            .with_label_values(&[event_type])
            .inc();

        debug!(
            event_type = %event_type,
            local_subscribers = local_sent,
            redis_published = redis_sent,
            "Event broadcast complete"
        );

        BroadcastResult {
            local_sent,
            redis_sent,
        }
    }

    /// Subscribe a client to room events
    ///
    /// Returns a receiver for messages and a connection ID for cleanup.
    /// Generates a new connection ID internally. Prefer `subscribe_with_id`
    /// when the caller already has a connection ID (e.g., from `ConnectionManager`).
    pub async fn subscribe(
        &self,
        room_id: RoomId,
        user_id: UserId,
    ) -> crate::Result<(tokio::sync::mpsc::Receiver<RealtimeEvent>, ConnectionId)> {
        let connection_id = ConnectionId::new(format!("{user_id}_{}", synctv_common::snanoid!(8)));
        self.subscribe_with_id(room_id, user_id, connection_id)
            .await
    }

    /// Subscribe a client to room events using an existing connection ID.
    ///
    /// This ensures the same connection ID is used across the `ConnectionManager`
    /// and the message hub, avoiding mismatches that can cause leaked subscriptions
    /// or missed disconnect signals.
    pub async fn subscribe_with_id(
        &self,
        room_id: RoomId,
        user_id: UserId,
        connection_id: ConnectionId,
    ) -> crate::Result<(tokio::sync::mpsc::Receiver<RealtimeEvent>, ConnectionId)> {
        let room_id_str = room_id.to_string();
        let user_id_str = user_id.to_string();
        let rx = self
            .message_hub
            .subscribe(room_id, user_id, connection_id.clone())
            .await?;

        info!(
            room_id = %room_id_str,
            user_id = %user_id_str,
            connection_id = %connection_id,
            "Client subscribed to room"
        );

        Ok((rx, connection_id))
    }

    /// Unsubscribe a client from room events
    pub fn unsubscribe(&self, connection_id: &str) {
        self.message_hub.unsubscribe(connection_id);
    }

    /// Get realtime metrics
    #[must_use]
    pub fn metrics(&self) -> RealtimeMetrics {
        RealtimeMetrics {
            node_id: self.node_id.clone(),
            total_rooms: self.message_hub.room_count(),
            total_connections: self.message_hub.connection_count(),
            tracked_events: self.deduplicator.len(),
            distributed_enabled: self.redis_publish_tx.is_some(),
            is_quarantined: self.is_quarantined(),
            has_connection_manager: self.connection_manager.is_some(),
            has_leader_elector: self.leader_elector.is_some(),
        }
    }

    /// Get subscribers in a room
    #[must_use]
    pub fn get_room_subscribers(&self, room_id: &RoomId) -> Vec<(UserId, ConnectionId)> {
        self.message_hub.get_room_subscribers(room_id)
    }

    #[cfg(test)]
    pub async fn test_set_heartbeat_handle(&self, handle: tokio::task::JoinHandle<()>) {
        let mut state = self.heartbeat_state.lock().await;
        state.handle = Some(handle);
    }

    #[cfg(test)]
    pub async fn test_set_heartbeat_registry<N>(&self, node_registry: Arc<N>)
    where
        N: ClusterNodeDirectory + 'static,
    {
        let node_registry: Arc<dyn ClusterNodeDirectory> = node_registry;
        let mut state = self.heartbeat_state.lock().await;
        state.node_registry = Some(node_registry);
    }
}

impl Drop for RealtimeManager {
    fn drop(&mut self) {
        // Cancel all background tasks (heartbeat, Redis pub/sub, connection manager).
        // Drop cannot run async code, but CancellationToken::cancel() is synchronous
        // and will notify all tasks holding a clone of this token to stop.
        // For graceful shutdown with awaiting, use the async shutdown() method instead.
        self.cancel_token.cancel();
        self.redis_publish_accepting.store(false, Ordering::Release);
        self.critical_retry_tasks.close();

        if let Some(handle) = self.publisher_task.get_mut().take() {
            handle.abort();
        }

        if let Some(handle) = self.critical_forwarder_task.get_mut().take() {
            handle.abort();
        }

        if let Some(connection_manager) = &self.connection_manager {
            connection_manager.abort_background_tasks();
        }
    }
}

/// Result of broadcasting an event
#[derive(Debug, Clone)]
pub struct BroadcastResult {
    /// Number of local subscribers the event was sent to
    pub local_sent: usize,
    /// Whether the event was published to Redis
    pub redis_sent: bool,
}

/// Realtime metrics
#[derive(Debug, Clone)]
pub struct RealtimeMetrics {
    pub node_id: String,
    pub total_rooms: usize,
    pub total_connections: usize,
    pub tracked_events: usize,
    pub distributed_enabled: bool,
    /// Whether this node is quarantined due to epoch mismatch (split-brain)
    pub is_quarantined: bool,
    /// Whether a coordinated `ConnectionManager` was injected.
    pub has_connection_manager: bool,
    /// Whether a leader elector was injected for quarantine-triggered resign.
    pub has_leader_elector: bool,
}

#[cfg(test)]
mod tests;
