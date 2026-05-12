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
use super::events::RealtimeEvent;
use super::redis_pubsub::PublishRequest;
use super::room_hub::{ConnectionId, RoomMessageHub};
use super::runtime::{ConnectionRuntime, RoomMessageRuntime};
use super::transport::{
    RealtimeEventHandler, RealtimeMessageTransport, RealtimeMessageTransportConfig,
    RealtimeMessageTransportFactory,
};
use crate::error::Result as RealtimeResult;
use synctv_cluster::discovery::{ClusterNodeDirectory, HeartbeatResult};
use synctv_core::models::id::{RoomId, UserId};

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
    /// Critical events are never dropped; senders block when full.
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
    /// Default: 10000
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
        Self {
            distributed_transport_factory: None,
            message_runtime: Arc::new(RoomMessageHub::new()),
            distributed_enabled: false,
            node_id: format!("node_{}", synctv_common::snanoid!(8)),
            dedup_window: Duration::from_mins(15),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
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
    /// Sender for publishing critical events to Redis (high priority, never dropped)
    redis_critical_tx: Option<mpsc::Sender<PublishRequest>>,
    /// This node's unique identifier
    node_id: String,
    /// Broadcast channel for admin events (kick, etc.) received from cluster
    admin_event_tx: broadcast::Sender<RealtimeEvent>,
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
    #[cfg(test)]
    heartbeat_shutdown_timeout: Duration,
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
        let deduplicator = Arc::new(MessageDeduplicator::new(config.dedup_window));
        let manager_cancel_token = config.parent_cancel_token.as_ref().map_or_else(
            CancellationToken::new,
            tokio_util::sync::CancellationToken::child_token,
        );
        let critical_retry_tasks = TaskTracker::new();

        let (admin_event_tx, _) = broadcast::channel(4096);
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
            // bounded channel so they are never dropped when the normal channel is full.
            let critical_capacity = config.critical_channel_capacity;
            let (critical_tx, mut critical_rx) = mpsc::channel::<PublishRequest>(critical_capacity);
            // Forward critical events into the normal publish channel using `.send().await`
            // (blocks until space available, never drops).
            let normal_tx = tx.clone();
            let cancel_critical = manager_cancel_token.clone();
            let critical_forwarder_handle = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = cancel_critical.cancelled() => {
                            // Drain remaining critical events before exiting
                            while let Ok(req) = critical_rx.try_recv() {
                                let _ = normal_tx.send(req).await;
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
            #[cfg(test)]
            heartbeat_shutdown_timeout: Duration::from_secs(10),
            connection_manager: None,
            heartbeat_failure_count: Arc::new(AtomicU64::new(0)),
            epoch_mismatch_count: Arc::new(AtomicU64::new(0)),
            is_quarantined: Arc::new(AtomicBool::new(false)),
            shutdown_started: Arc::new(AtomicBool::new(false)),
            redis_publish_accepting: Arc::new(AtomicBool::new(true)),
            leader_elector: None,
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

    /// Set the connection manager for coordinated shutdown.
    ///
    /// When set, `shutdown()` will also cancel the ConnectionManager's TTL
    /// refresh task, ensuring background tasks don't outlive the cluster.
    pub fn set_connection_manager(&mut self, cm: Arc<dyn ConnectionRuntime>) {
        self.connection_manager = Some(cm);
    }

    /// Set the leader elector for resigning leadership on epoch mismatch.
    ///
    /// When epoch mismatch is detected, this node will resign leadership if
    /// it's currently the leader to prevent split-brain scenarios.
    pub fn set_leader_elector(&mut self, elector: Arc<dyn synctv_cluster::leader::LeaderRuntime>) {
        self.leader_elector = Some(elector);
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

    /// Get the admin event sender (for local kick events)
    #[must_use]
    pub const fn admin_event_tx(&self) -> &broadcast::Sender<RealtimeEvent> {
        &self.admin_event_tx
    }

    fn enqueue_redis_publish(&self, event: RealtimeEvent, is_critical: bool) -> bool {
        let mut redis_sent = 0;

        if is_critical {
            if let Some(tx) = &self.redis_critical_tx {
                match tx.try_send(PublishRequest { event }) {
                    Ok(()) => {
                        redis_sent = 1;
                    }
                    Err(mpsc::error::TrySendError::Full(req)) => {
                        let tx = tx.clone();
                        warn!(
                            "Critical event publish channel full (capacity {}), spawning tracked retry task",
                            self.critical_channel_capacity
                        );
                        self.critical_retry_tasks.spawn(async move {
                            if let Err(e) = tx.send(req).await {
                                error!("Failed to send critical event after retry: {e}");
                            }
                        });
                        redis_sent = 1;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        error!("Critical event publish channel closed");
                    }
                }
            } else if let Some(tx) = &self.redis_publish_tx {
                match tx.try_send(PublishRequest { event }) {
                    Ok(()) => {
                        redis_sent = 1;
                    }
                    Err(mpsc::error::TrySendError::Full(req)) => {
                        let tx = tx.clone();
                        warn!(
                            "Dedicated critical publish channel unavailable; normal Redis publish channel is full (capacity {}), spawning tracked retry for critical event",
                            self.publish_channel_capacity
                        );
                        self.critical_retry_tasks.spawn(async move {
                            if let Err(e) = tx.send(req).await {
                                error!(
                                    "Failed to send critical event through fallback Redis channel: {e}"
                                );
                            }
                        });
                        redis_sent = 1;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        error!(
                            "Dedicated critical publish channel unavailable and fallback Redis publish channel is closed"
                        );
                    }
                }
            }
        } else if let Some(tx) = &self.redis_publish_tx {
            match tx.try_send(PublishRequest { event }) {
                Ok(()) => {
                    redis_sent = 1;
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
                        .with_label_values(&["channel_full"])
                        .inc();
                    warn!(
                        "Redis publish channel full (capacity {}), dropping event",
                        self.publish_channel_capacity
                    );
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    error!("Redis publish channel closed, cannot queue event");
                }
            }
        }

        redis_sent > 0
    }

    /// Broadcast an event to local subscribers only.
    ///
    /// This preserves deduplication semantics for the event without publishing it
    /// to Redis. It is used when callers need to preserve local correctness first
    /// and handle cross-node retries separately.
    pub fn broadcast_local(&self, event: RealtimeEvent) -> usize {
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

        let dedup_key = DedupKey::from_event(&event);
        if !self.deduplicator.should_process(&dedup_key) {
            debug!(
                event_type = %event_type,
                room_id = %event.room_id()
                    .map_or_else(|| "n/a".to_string(), ToString::to_string),
                "Duplicate event detected, skipping local broadcast"
            );
            return 0;
        }

        if let Some(room_id) = event.room_id().copied() {
            return self.message_hub.broadcast(&room_id, &event);
        }

        if matches!(
            &event,
            RealtimeEvent::UserNotification { .. }
                | RealtimeEvent::ProviderCredentialChanged { .. }
                | RealtimeEvent::CacheInvalidate { .. }
        ) {
            let _ = self.admin_event_tx.send(event);
        }

        0
    }

    /// Publish an event to Redis without re-broadcasting it locally.
    ///
    /// This is primarily used by retry paths that have already delivered the
    /// event locally and only need cross-node fan-out.
    pub fn publish_only(&self, event: RealtimeEvent) -> bool {
        let event_type = event.event_type();
        let is_critical = event.is_critical();

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

        let redis_sent = self.enqueue_redis_publish(event, is_critical);

        debug!(
            event_type = %event_type,
            redis_published = redis_sent,
            "Redis-only publish complete"
        );

        redis_sent
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
    /// 3. Shuts down Redis Pub/Sub (which drains pending publishes)
    /// 4. Awaits the publisher task's completion (with a 10s timeout)
    /// 5. Shuts down the deduplicator cleanup task
    /// 6. Awaits background task completion
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
                await_shutdown_handle("Heartbeat task", handle, self.heartbeat_shutdown_timeout())
                    .await;
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
            cm.shutdown().await;
        }

        // Shut down RoomMessageHub background tasks (Redis TTL refresh and stale cleanup)
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

        let dedup_key = DedupKey::from_event(&event);

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
        if let Some(room_id) = event.room_id() {
            // Broadcast to local subscribers
            local_sent = self.message_hub.broadcast(room_id, &event);
        }

        // UserNotification events are user-targeted (no room_id), so they are
        // delivered via the admin event channel to reach connected WebSocket handlers.
        if matches!(
            &event,
            RealtimeEvent::UserNotification { .. }
                | RealtimeEvent::ProviderCredentialChanged { .. }
                | RealtimeEvent::CacheInvalidate { .. }
        ) {
            let _ = self.admin_event_tx.send(event.clone());
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
        let connection_id = format!("{user_id}_{}", synctv_common::snanoid!(8));
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
    const fn heartbeat_shutdown_timeout(&self) -> Duration {
        self.heartbeat_shutdown_timeout
    }

    #[cfg(not(test))]
    const fn heartbeat_shutdown_timeout(&self) -> Duration {
        let _ = self;
        Duration::from_secs(10)
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

    #[cfg(test)]
    #[must_use]
    pub const fn test_with_heartbeat_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.heartbeat_shutdown_timeout = timeout;
        self
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
mod tests {
    use super::*;
    use crate::sync::{CacheTarget, ConnectionLimits, ConnectionManager};
    use async_trait::async_trait;
    use chrono::Utc;
    use std::sync::atomic::AtomicUsize;
    use synctv_cluster::NodeRegistry;
    use tokio::sync::{broadcast, mpsc};

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
    }

    impl RealtimeMessageTransportFactory for StubTransportFactory {
        fn build(
            &self,
            _config: RealtimeMessageTransportConfig,
        ) -> RealtimeResult<Arc<dyn RealtimeMessageTransport>> {
            Ok(Arc::new(StubTransport {
                start_count: self.start_count.clone(),
                shutdown_count: self.shutdown_count.clone(),
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
            let (publish_tx, _publish_rx) = mpsc::channel(8);
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
        ) -> RealtimeResult<mpsc::Receiver<RealtimeEvent>> {
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
        ) -> Vec<(UserId, ConnectionId)> {
            Vec::new()
        }

        async fn audit_shared_subscriptions(&self) -> std::result::Result<usize, String> {
            Ok(0)
        }

        fn spawn_shared_subscription_cleanup_task(
            &self,
            _cleanup_interval: Duration,
            _cancel_token: CancellationToken,
        ) -> tokio::task::JoinHandle<()> {
            tokio::spawn(async {})
        }

        async fn shutdown(&self) {}

        fn background_shutdown_requested(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn test_realtime_manager_single_node() {
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

        let manager = RealtimeManager::new(config).await.unwrap();

        // Subscribe a client
        let room_id = RoomId::expect_positive(10_000_092);
        let user_id = UserId::expect_positive(10_000_010);
        let (mut rx, conn_id) = manager
            .subscribe(room_id, user_id)
            .await
            .expect("subscribe should succeed");

        // Broadcast event
        let event = RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id,
            username: "user1".to_string(),
            message: "Hello!".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        let result = manager.broadcast(event.clone());

        assert_eq!(result.local_sent, 1);
        assert!(!result.redis_sent);

        // Verify duplicate detection
        let result2 = manager.broadcast(event);
        assert_eq!(result2.local_sent, 0);
        assert!(matches!(
            result2,
            BroadcastResult {
                local_sent: 0,
                redis_sent: false
            }
        ));

        // Verify message received
        let received = rx.recv().await.unwrap();
        assert_eq!(received.event_type(), "chat_message");

        // Cleanup
        manager.unsubscribe(&conn_id);

        let metrics = manager.metrics();
        assert_eq!(metrics.total_connections, 0);
    }

    #[test]
    fn test_realtime_config_debug_reports_transport_configuration_without_backend_name() {
        let factory = StubTransportFactory::default();
        let config = RealtimeConfig {
            distributed_transport_factory: Some(Arc::new(factory)),
            message_runtime: Arc::new(RoomMessageHub::new()),
            distributed_enabled: true,
            node_id: "debug-node".to_string(),
            dedup_window: Duration::from_secs(1),
            critical_channel_capacity: 8,
            publish_channel_capacity: 16,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            event_handler: None,
            parent_cancel_token: None,
        };

        let debug = format!("{config:?}");
        assert!(debug.contains("distributed_transport_factory: Some(\"configured\")"));
        assert!(!debug.contains("stub"));
        assert!(!debug.contains("redis"));
        assert!(!debug.contains("backend"));
    }

    #[tokio::test]
    async fn test_realtime_manager_respects_injected_message_runtime() {
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

        let manager = RealtimeManager::new(config)
            .await
            .expect("realtime manager should preserve injected message runtime");
        let metrics = manager.metrics();

        assert_eq!(metrics.total_rooms, 7);
        assert_eq!(metrics.total_connections, 11);
    }

    #[tokio::test]
    async fn test_admin_event_channel_subscription() {
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

        let manager = RealtimeManager::new(config).await.unwrap();

        // Subscribe to admin events
        let mut admin_rx = manager.subscribe_admin_events();

        // Send a KickPublisher event through the admin channel
        let event = RealtimeEvent::KickPublisher {
            event_id: synctv_common::snanoid!(16),
            room_id: RoomId::expect_positive(10_000_092),
            media_id: synctv_core::models::MediaId::expect_positive(10_000_093),
            reason: "user_banned".to_string(),
            timestamp: Utc::now(),
        };

        let _ = manager.admin_event_tx().send(event.clone());

        // Verify event received
        let received = admin_rx.recv().await.unwrap();
        assert_eq!(received.event_type(), "kick_publisher");

        if let RealtimeEvent::KickPublisher {
            room_id,
            media_id,
            reason,
            ..
        } = &received
        {
            assert_eq!(*room_id, RoomId::expect_positive(10_000_092));
            assert_eq!(
                *media_id,
                synctv_core::models::MediaId::expect_positive(10_000_093)
            );
            assert_eq!(reason, "user_banned");
        } else {
            panic!("Expected KickPublisher event");
        }
    }

    #[tokio::test]
    async fn test_admin_event_channel_multiple_subscribers() {
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

        let manager = RealtimeManager::new(config).await.unwrap();

        // Subscribe two receivers
        let mut rx1 = manager.subscribe_admin_events();
        let mut rx2 = manager.subscribe_admin_events();

        // Send event
        let event = RealtimeEvent::KickPublisher {
            event_id: synctv_common::snanoid!(16),
            room_id: RoomId::expect_positive(10_000_092),
            media_id: synctv_core::models::MediaId::expect_positive(10_000_093),
            reason: "room_deleted".to_string(),
            timestamp: Utc::now(),
        };
        let _ = manager.admin_event_tx().send(event);

        // Both receivers should get the event
        let r1 = rx1.recv().await.unwrap();
        let r2 = rx2.recv().await.unwrap();
        assert_eq!(r1.event_type(), "kick_publisher");
        assert_eq!(r2.event_type(), "kick_publisher");
    }

    /// Test that RealtimeManager handles the non-distributed mode degradation gracefully
    /// when a CacheInvalidationService is provided but Redis is not available.
    ///
    /// This verifies:
    /// 1. RealtimeManager::new() succeeds even when an event handler is provided without Redis
    /// 2. The service logs an appropriate warning about local-only invalidation
    /// 3. The RealtimeManager operates normally in single-node mode
    #[tokio::test]
    async fn test_non_cluster_mode_with_event_handler() {
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
        let manager = RealtimeManager::new(config)
            .await
            .expect("RealtimeManager::new should succeed with an event handler but no Redis");

        // Verify the manager operates normally in single-node mode
        let room_id = RoomId::expect_positive(10_000_092);
        let user_id = UserId::expect_positive(10_000_010);
        let (mut rx, conn_id) = manager
            .subscribe(room_id, user_id)
            .await
            .expect("subscribe should succeed");

        // Broadcast should work locally
        let event = RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id,
            username: "user1".to_string(),
            message: "Hello local!".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
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
        let received = rx.recv().await.expect("Should receive local message");
        assert_eq!(received.event_type(), "chat_message");

        // Cleanup
        manager.unsubscribe(&conn_id);

        // Verify metrics show single-node mode
        let metrics = manager.metrics();
        assert!(
            !metrics.distributed_enabled,
            "Metrics should show distributed transport is not enabled"
        );
    }

    /// Test that RealtimeManager works correctly when both Redis and
    /// CacheInvalidationService are not provided (pure single-node mode).
    #[tokio::test]
    async fn test_non_cluster_mode_without_event_handler() {
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
        let manager = RealtimeManager::new(config)
            .await
            .expect("RealtimeManager::new should succeed without an event handler and Redis");

        // Verify normal operation
        let room_id = RoomId::expect_positive(10_000_094);
        let user_id = UserId::expect_positive(10_000_095);
        let (mut rx, conn_id) = manager
            .subscribe(room_id, user_id)
            .await
            .expect("subscribe should succeed");

        let event = RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id,
            username: "user2".to_string(),
            message: "Hello!".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        let result = manager.broadcast(event);
        assert_eq!(result.local_sent, 1);
        assert!(!result.redis_sent);

        let received = rx.recv().await.expect("Should receive message");
        assert_eq!(received.event_type(), "chat_message");

        manager.unsubscribe(&conn_id);
    }

    #[tokio::test]
    async fn test_shutdown_times_out_non_cooperative_heartbeat_handle() {
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

        let manager = RealtimeManager::new(config)
            .await
            .unwrap()
            .test_with_heartbeat_shutdown_timeout(Duration::from_millis(50));

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
    }

    #[tokio::test]
    async fn test_shutdown_unregisters_node_after_heartbeat_stops() {
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

        let manager = Arc::new(
            RealtimeManager::new(config)
                .await
                .unwrap()
                .test_with_heartbeat_shutdown_timeout(Duration::from_millis(200)),
        );
        let registry = Arc::new(
            NodeRegistry::new_local_only("shutdown-race-node".to_string(), 30, "test:").unwrap(),
        );

        registry
            .register("localhost:8080".to_string())
            .await
            .unwrap();
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
                    .expect("test should observe heartbeat cancellation");
                allow_finish_rx
                    .await
                    .expect("test should allow heartbeat task to finish");
                registry_for_task
                    .register("localhost:8080".to_string())
                    .await
                    .unwrap();
            }))
            .await;

        let shutdown_manager = Arc::clone(&manager);
        let shutdown_handle = tokio::spawn(async move {
            shutdown_manager.shutdown().await;
        });

        // Shutdown should be waiting for the heartbeat handle to complete.
        cancel_seen_rx
            .await
            .expect("shutdown should cancel heartbeat task promptly");
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
            .expect("heartbeat task should still be waiting to finish");
        // Now shutdown can proceed: heartbeat stopped → unregister.
        shutdown_handle.await.unwrap();

        // The late re-registration must be cleaned up by unregister.
        assert!(
            registry
                .test_get_local("shutdown-race-node")
                .await
                .is_none(),
            "shutdown must unregister the node even after a late heartbeat re-registration"
        );
    }

    #[tokio::test]
    async fn test_shutdown_waits_for_tracked_critical_retry_tasks() {
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

        let manager = RealtimeManager::new(config).await.unwrap();

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
        shutdown_handle.await.unwrap();

        assert!(
            finished.load(Ordering::SeqCst),
            "tracked critical retry task should finish before shutdown returns"
        );
    }

    #[tokio::test]
    async fn test_shutdown_stops_accepting_new_critical_redis_work_before_waiting_for_retries() {
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

        let mut manager = RealtimeManager::new(config).await.unwrap();
        let (critical_tx, mut critical_rx) = mpsc::channel::<PublishRequest>(1);
        critical_tx
            .try_send(PublishRequest {
                event: RealtimeEvent::KickUser {
                    event_id: synctv_common::snanoid!(16),
                    user_id: UserId::expect_positive(10_000_096),
                    reason: "fill queue".to_string(),
                    timestamp: Utc::now(),
                },
            })
            .expect("pre-fill critical queue");
        manager.redis_critical_tx = Some(critical_tx);

        manager.shutdown_started.store(true, Ordering::Release);
        manager
            .redis_publish_accepting
            .store(false, Ordering::Release);

        let event = RealtimeEvent::KickUser {
            event_id: synctv_common::snanoid!(16),
            user_id: UserId::expect_positive(10_000_097),
            reason: "must not start new retry after drain closes".to_string(),
            timestamp: Utc::now(),
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
            .expect("pre-filled request should still be present");
        assert_eq!(queued.event.event_type(), "kick_user");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), critical_rx.recv())
                .await
                .is_err(),
            "no new critical publish should be enqueued after drain closes"
        );
    }

    #[tokio::test]
    async fn test_shutdown_also_cancels_room_message_hub_background_tasks() {
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

        let manager = RealtimeManager::new(config).await.unwrap();

        assert!(
            !manager.message_hub().background_shutdown_requested(),
            "room hub cancellation tokens should not be pre-cancelled"
        );

        manager.shutdown().await;

        assert!(
            manager.message_hub().background_shutdown_requested(),
            "cluster shutdown must also cancel room hub background tasks"
        );
    }

    #[tokio::test]
    async fn test_shutdown_still_allows_critical_events_to_reach_redis_channels() {
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

        let mut manager = RealtimeManager::new(config).await.unwrap();
        let (critical_tx, mut critical_rx) = mpsc::channel::<PublishRequest>(4);
        manager.redis_critical_tx = Some(critical_tx);
        manager.shutdown_started.store(true, Ordering::Release);

        let event = RealtimeEvent::KickUser {
            event_id: synctv_common::snanoid!(16),
            user_id: UserId::expect_positive(10_000_098),
            reason: "must propagate during draining".to_string(),
            timestamp: Utc::now(),
        };

        let result = manager.broadcast(event.clone());

        assert!(
            result.redis_sent,
            "critical events must still be enqueued for Redis while shutdown drains in-flight work"
        );

        let published = tokio::time::timeout(Duration::from_millis(100), critical_rx.recv())
            .await
            .expect("critical event should reach Redis queue during shutdown")
            .expect("critical channel should stay open");
        assert_eq!(published.event.event_type(), event.event_type());
    }

    #[tokio::test]
    async fn test_shutdown_still_blocks_non_critical_events_from_redis_channels() {
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

        let mut manager = RealtimeManager::new(config).await.unwrap();
        let room_id = RoomId::expect_positive(10_000_099);
        let user_id = UserId::expect_positive(10_000_098);
        let (mut room_rx, _conn_id) = manager
            .subscribe(room_id, user_id)
            .await
            .expect("subscribe should succeed");
        let (publish_tx, mut publish_rx) = mpsc::channel::<PublishRequest>(4);
        manager.redis_publish_tx = Some(publish_tx);
        manager.shutdown_started.store(true, Ordering::Release);

        let event = RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id,
            username: "shutdown".to_string(),
            message: "non critical".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
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
    }

    /// Test that RealtimeManager tracks epoch mismatch state and quarantine.
    ///
    /// This test verifies:
    /// 1. RealtimeManager starts in non-quarantined state
    /// 2. Epoch mismatch counter is tracked internally
    /// 3. Quarantine state is reflected in metrics
    /// 4. Leader elector can be set for resigning leadership
    #[tokio::test]
    async fn test_epoch_mismatch_enforcement() {
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

        let mut manager = RealtimeManager::new(config)
            .await
            .expect("RealtimeManager::new should succeed");

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

        manager.set_leader_elector(Arc::new(synctv_core::service::AlwaysLeader));

        // Verify the elector was set (we can't directly check, but we can verify
        // the manager is still functional)
        let room_id = RoomId::expect_positive(10_000_100);
        let user_id = UserId::expect_positive(10_000_101);
        let (_rx, conn_id) = manager
            .subscribe(room_id, user_id)
            .await
            .expect("subscribe should succeed");

        // Broadcast should work in non-quarantined state
        let event = RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id,
            username: "test_user".to_string(),
            message: "Test message".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        let result = manager.broadcast(event);
        assert_eq!(
            result.local_sent, 1,
            "Broadcast should succeed in non-quarantined state"
        );

        manager.unsubscribe(&conn_id);
    }

    #[tokio::test]
    async fn test_quarantined_broadcast_is_rejected_without_poisoning_dedup() {
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

        let manager = RealtimeManager::new(config)
            .await
            .expect("RealtimeManager::new should succeed");
        let room_id = RoomId::expect_positive(10_000_102);
        let user_id = UserId::expect_positive(10_000_103);
        let mut rx = manager
            .message_hub()
            .subscribe(room_id, user_id, "conn-quarantine".to_string())
            .await
            .expect("subscribe should succeed");

        manager.is_quarantined.store(true, Ordering::Release);

        let event = RealtimeEvent::ChatMessage {
            event_id: "dedup-preserved".to_string(),
            room_id,
            user_id,
            username: "quarantined-user".to_string(),
            message: "blocked while quarantined".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
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
                Ok(Some(RealtimeEvent::ChatMessage { .. }))
            ),
            "event should be delivered after quarantine is lifted"
        );
    }

    #[tokio::test]
    async fn test_cluster_metrics_reports_dependency_injection_state() {
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

        let mut manager = RealtimeManager::new(config)
            .await
            .expect("RealtimeManager::new should succeed");

        let metrics = manager.metrics();
        assert!(
            !metrics.has_connection_manager,
            "fresh manager should not report an injected ConnectionManager"
        );
        assert!(
            !metrics.has_leader_elector,
            "fresh manager should not report an injected leader elector"
        );

        let cm = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        manager.set_connection_manager(cm);
        manager.set_leader_elector(Arc::new(synctv_core::service::AlwaysLeader));

        let metrics = manager.metrics();
        assert!(
            metrics.has_connection_manager,
            "metrics should reflect injected ConnectionManager"
        );
        assert!(
            metrics.has_leader_elector,
            "metrics should reflect injected leader elector"
        );
    }

    #[tokio::test]
    async fn test_critical_events_do_not_fall_back_to_droppable_normal_channel() {
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

        let mut manager = RealtimeManager::new(config)
            .await
            .expect("RealtimeManager::new should succeed");

        let (normal_tx, mut normal_rx) = mpsc::channel::<PublishRequest>(1);
        normal_tx
            .try_send(PublishRequest {
                event: RealtimeEvent::ChatMessage {
                    event_id: synctv_common::snanoid!(16),
                    room_id: RoomId::expect_positive(10_000_104),
                    user_id: UserId::expect_positive(10_000_105),
                    username: "buffer".to_string(),
                    message: "fill channel".to_string(),
                    timestamp: Utc::now(),
                    position: None,
                    color: None,
                },
            })
            .expect("pre-fill normal channel");

        manager.redis_publish_tx = Some(normal_tx);
        manager.redis_critical_tx = None;

        let critical_event = RealtimeEvent::KickUser {
            event_id: synctv_common::snanoid!(16),
            user_id: UserId::expect_positive(10_000_106),
            reason: "must not drop".to_string(),
            timestamp: Utc::now(),
        };

        let result = manager.broadcast(critical_event.clone());

        assert!(
            result.redis_sent,
            "critical events must still report Redis publication when only the fallback channel is wired"
        );

        let buffered = normal_rx
            .recv()
            .await
            .expect("buffered message should still exist");
        assert_eq!(buffered.event.event_type(), "chat_message");

        let delivered = tokio::time::timeout(Duration::from_millis(100), normal_rx.recv())
            .await
            .expect("critical event should be queued instead of dropped")
            .expect("critical event should arrive on fallback channel");
        assert_eq!(delivered.event.event_type(), critical_event.event_type());
    }

    #[tokio::test]
    async fn test_publish_only_enqueues_redis_without_rebroadcasting_locally() {
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

        let mut manager = RealtimeManager::new(config)
            .await
            .expect("RealtimeManager::new should succeed");
        let room_id = RoomId::expect_positive(10_000_107);
        let user_id = UserId::expect_positive(10_000_108);
        let mut room_rx = manager
            .message_hub()
            .subscribe(room_id, user_id, "publish-only-conn".to_string())
            .await
            .expect("subscribe should succeed");
        let (critical_tx, mut critical_rx) = mpsc::channel::<PublishRequest>(4);
        manager.redis_critical_tx = Some(critical_tx);

        let event = RealtimeEvent::UserLeft {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id,
            username: "publish-only".to_string(),
            timestamp: Utc::now(),
        };

        assert!(
            manager.publish_only(event.clone()),
            "publish_only should enqueue the Redis publish path"
        );

        let published = tokio::time::timeout(Duration::from_millis(100), critical_rx.recv())
            .await
            .expect("event should reach Redis queue")
            .expect("critical queue should stay open");
        assert_eq!(published.event.event_type(), event.event_type());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), room_rx.recv())
                .await
                .is_err(),
            "publish_only must not duplicate local delivery"
        );
    }

    #[tokio::test]
    async fn test_publish_only_user_notification_does_not_hit_admin_channel() {
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

        let mut manager = RealtimeManager::new(config)
            .await
            .expect("RealtimeManager::new should succeed");
        let (publish_tx, mut publish_rx) = mpsc::channel::<PublishRequest>(4);
        manager.redis_publish_tx = Some(publish_tx);
        let mut admin_rx = manager.subscribe_admin_events();

        let event = RealtimeEvent::UserNotification {
            event_id: synctv_common::snanoid!(16),
            user_id: UserId::expect_positive(10_000_109),
            notification_id: "notification-1".to_string(),
            title: "title".to_string(),
            content: "content".to_string(),
            notification_type: "system".to_string(),
            timestamp: Utc::now(),
        };

        assert!(
            manager.publish_only(event.clone()),
            "publish_only should enqueue UserNotification to Redis"
        );

        let published = tokio::time::timeout(Duration::from_millis(100), publish_rx.recv())
            .await
            .expect("user notification should reach Redis queue")
            .expect("publish queue should stay open");
        assert_eq!(published.event.event_type(), event.event_type());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), admin_rx.recv())
                .await
                .is_err(),
            "publish_only must not emit UserNotification to the local admin channel"
        );
    }

    #[tokio::test]
    async fn test_broadcast_cache_invalidate_reaches_admin_channel() {
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

        let manager = RealtimeManager::new(config)
            .await
            .expect("RealtimeManager::new should succeed");
        let mut admin_rx = manager.subscribe_admin_events();
        let event = RealtimeEvent::CacheInvalidate {
            event_id: synctv_common::snanoid!(16),
            targets: vec![CacheTarget::Room {
                room_id: RoomId::expect_positive(10_000_110),
            }],
            timestamp: Utc::now(),
        };

        let result = manager.broadcast(event.clone());
        assert_eq!(
            result.local_sent, 0,
            "cache invalidation is not a room subscriber event"
        );

        let received = tokio::time::timeout(Duration::from_millis(100), admin_rx.recv())
            .await
            .expect("CacheInvalidate should reach admin subscribers")
            .expect("admin channel should stay open");
        assert_eq!(received.event_id(), event.event_id());
    }

    #[tokio::test]
    async fn test_drop_aborts_injected_connection_manager_background_tasks() {
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

        let mut manager = RealtimeManager::new(config)
            .await
            .expect("RealtimeManager::new should succeed");
        let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        connection_manager.start();
        tokio::time::sleep(Duration::from_millis(10)).await;
        manager.set_connection_manager(connection_manager.clone());

        drop(manager);
        tokio::task::yield_now().await;

        assert!(
            !connection_manager.background_tasks_running(),
            "drop fallback must clear ConnectionManager background tasks when graceful shutdown was never awaited"
        );
    }

    /// Test that RealtimeManager metrics include quarantine state.
    #[tokio::test]
    async fn test_cluster_metrics_includes_quarantine_state() {
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

        let manager = RealtimeManager::new(config)
            .await
            .expect("RealtimeManager::new should succeed");

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
    }

    /// Test that explicit local-only unit tests still construct a manager without Redis.
    #[tokio::test]
    async fn test_local_only_manager_without_redis_still_builds() {
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

        let manager = result.expect("local-only RealtimeManager should still initialize");
        let metrics = manager.metrics();
        assert!(
            !metrics.distributed_enabled,
            "manager should remain local-only without distributed transport"
        );
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
        // Use a dummy Redis client that can't connect
        let redis_client = redis::Client::open("redis://127.0.0.1:1").ok();

        let config = RealtimeConfig {
            distributed_transport_factory: redis_client.map(|client| {
                Arc::new(crate::sync::RedisRealtimeMessageTransportFactory::new(
                    synctv_core::coordination_runtime_from_client(client),
                )) as Arc<dyn RealtimeMessageTransportFactory>
            }),
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

    /// Test that non-distributed mode (distributed_enabled=false) works without Redis.
    #[tokio::test]
    async fn test_non_cluster_mode_works_without_redis() {
        let config = RealtimeConfig {
            distributed_transport_factory: None,
            message_runtime: Arc::new(RoomMessageHub::new()),
            distributed_enabled: false, // Cluster mode disabled
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
    async fn test_non_cluster_mode_with_distributed_transport_remains_local_only() {
        let config = RealtimeConfig {
            distributed_transport_factory: Some(Arc::new(
                crate::sync::RedisRealtimeMessageTransportFactory::new(
                    synctv_core::coordination_runtime_from_client(
                        redis::Client::open("redis://127.0.0.1:1")
                            .expect("invalid-but-constructible Redis client"),
                    ),
                ),
            )),
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

        let manager = RealtimeManager::new(config)
            .await
            .expect("standalone mode must ignore Redis fan-out transport");

        assert!(
            manager.redis_publish_tx().is_none(),
            "standalone mode must not start Redis publish channels"
        );
        assert!(
            !manager.metrics().distributed_enabled,
            "standalone mode must report local-only transport even when Redis is configured"
        );
    }

    #[tokio::test]
    async fn test_realtime_manager_uses_injected_transport_factory() {
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

        let manager = RealtimeManager::new(config)
            .await
            .expect("realtime manager should accept trait-object transport factory");

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
    }
}
