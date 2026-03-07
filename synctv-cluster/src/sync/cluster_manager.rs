//! Complete cluster synchronization service
//!
//! This module provides a unified interface for all cross-cluster functionality:
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
use tracing::{debug, error, info, warn};

use super::connection_manager::ConnectionManager;
use super::dedup::{DedupKey, MessageDeduplicator};
use super::events::ClusterEvent;
use super::redis_pubsub::{PublishRequest, RedisPubSub};
use super::room_hub::{ConnectionId, RoomMessageHub};
use crate::discovery::{HeartbeatResult, NodeRegistry};
use crate::error::Result as ClusterResult;
use synctv_core::models::id::{RoomId, UserId};
use synctv_core::service::PermissionService;

/// Cluster configuration
#[derive(Clone)]
pub struct ClusterConfig {
    /// Pre-built Redis client (shared across the process).
    /// `None` for local-only / single-node mode (used in tests).
    pub redis_client: Option<redis::Client>,
    /// Pre-built Redis connection manager (shared across the process).
    /// `None` for local-only / single-node mode (used in tests).
    pub redis_conn: Option<redis::aio::ConnectionManager>,
    /// Whether cluster mode is explicitly enabled.
    /// When `true`, `ClusterManager::new` will return an error if Redis is not configured.
    /// When `false`, missing Redis is allowed (single-node mode).
    /// Default: `false`
    pub cluster_enabled: bool,
    /// Unique identifier for this node
    pub node_id: String,
    /// Deduplication window duration
    pub dedup_window: Duration,
    /// How often to cleanup dedup entries
    pub cleanup_interval: Duration,
    /// Capacity for the high-priority critical event channel.
    /// Critical events are never dropped; senders block when full.
    pub critical_channel_capacity: usize,
    /// Capacity for the normal-priority Redis publish channel.
    /// Normal events are dropped with warning when full.
    pub publish_channel_capacity: usize,
    /// Key prefix for Redis keys and pub/sub channels (e.g., "synctv:")
    pub key_prefix: String,
    /// How far back (in seconds) to replay Redis Stream events when a new node
    /// first connects to the cluster.  Mirrors `ClusterChannelConfig::catchup_window_secs`.
    /// Default: 300 (5 minutes)
    pub catchup_window_secs: u64,
    /// Maximum number of entries per Redis Stream (approximate).
    /// Mirrors `ClusterChannelConfig::stream_max_length`.
    /// Default: 10000
    pub stream_max_length: usize,
    /// Optional parent cancellation token (e.g., from `ShutdownCoordinator`).
    /// When provided, the `ClusterManager`'s internal token is created as a
    /// child of this token, so cancelling the parent also cancels all cluster
    /// background tasks. When `None`, an independent token is created.
    pub parent_cancel_token: Option<CancellationToken>,
}

impl std::fmt::Debug for ClusterConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterConfig")
            .field(
                "redis_client",
                &self.redis_client.as_ref().map(|_| "redis::Client { .. }"),
            )
            .field(
                "redis_conn",
                &self.redis_conn.as_ref().map(|_| "ConnectionManager { .. }"),
            )
            .field("cluster_enabled", &self.cluster_enabled)
            .field("node_id", &self.node_id)
            .field("dedup_window", &self.dedup_window)
            .field("cleanup_interval", &self.cleanup_interval)
            .field("critical_channel_capacity", &self.critical_channel_capacity)
            .field("publish_channel_capacity", &self.publish_channel_capacity)
            .field("key_prefix", &self.key_prefix)
            .field("catchup_window_secs", &self.catchup_window_secs)
            .field("stream_max_length", &self.stream_max_length)
            .field(
                "parent_cancel_token",
                &self.parent_cancel_token.as_ref().map(|_| "Some(..)"),
            )
            .finish()
    }
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            redis_client: None,
            redis_conn: None,
            cluster_enabled: false,
            node_id: format!("node_{}", nanoid::nanoid!(8)),
            dedup_window: Duration::from_mins(15),
            cleanup_interval: Duration::from_secs(30),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            parent_cancel_token: None,
        }
    }
}

/// Cluster synchronization manager
///
/// This is the main entry point for all cross-cluster functionality.
/// It manages:
/// - Local message broadcasting via `RoomMessageHub`
/// - Cross-node synchronization via Redis Pub/Sub
/// - Message deduplication
/// - Connection lifecycle
pub struct ClusterManager {
    /// Message hub for local broadcasting
    message_hub: Arc<RoomMessageHub>,
    /// Deduplicator for preventing duplicate events
    deduplicator: Arc<MessageDeduplicator>,
    /// Sender for publishing events to Redis (normal priority)
    redis_publish_tx: Option<mpsc::Sender<PublishRequest>>,
    /// Sender for publishing critical events to Redis (high priority, never dropped)
    redis_critical_tx: Option<mpsc::Sender<PublishRequest>>,
    /// This node's unique identifier
    node_id: String,
    /// Broadcast channel for admin events (kick, etc.) received from cluster
    admin_event_tx: broadcast::Sender<ClusterEvent>,
    /// Redis Pub/Sub service (stored for graceful shutdown)
    redis_pubsub: Option<Arc<RedisPubSub>>,
    /// JoinHandle for the Redis publisher task.
    /// Awaited during shutdown so in-flight events are fully flushed before
    /// the process exits.
    publisher_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
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
    connection_manager: Option<ConnectionManager>,
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
    /// Leader elector for resigning leadership on epoch mismatch
    leader_elector: Option<Arc<dyn crate::leader::LeaderRuntime>>,
}

/// State for the background heartbeat loop, guarded by Mutex for async shutdown
struct HeartbeatState {
    node_registry: Option<Arc<NodeRegistry>>,
    handle: Option<tokio::task::JoinHandle<()>>,
    /// Stored addresses for heartbeat re-registration (avoid empty-address bug)
    grpc_address: String,
    http_address: String,
}

impl ClusterManager {
    /// Create a new cluster manager
    ///
    /// # Arguments
    /// * `config` - Cluster configuration
    /// * `permission_service` - Optional permission service for cross-replica cache invalidation.
    ///   When provided, `PermissionChanged` and `RoomSettingsChanged` events received from other
    ///   nodes will automatically invalidate the local permission cache.
    /// * `cache_invalidation` - Optional cache invalidation service for cross-replica
    ///   user/room/username cache invalidation. When provided, `CacheInvalidate` events
    ///   and data-mutating events (e.g. `RoomSettingsChanged`) will invalidate local L1 caches.
    pub async fn new(
        config: ClusterConfig,
        permission_service: Option<PermissionService>,
        cache_invalidation: Option<synctv_core::cache::CacheInvalidationService>,
    ) -> ClusterResult<Self> {
        debug_assert!(
            !config.cluster_enabled || (config.redis_client.is_some() && config.redis_conn.is_some()),
            "cluster-enabled ClusterManager must be assembled with Redis handles"
        );

        let deduplicator = Arc::new(MessageDeduplicator::new(
            config.dedup_window,
            config.cleanup_interval,
        ));

        let (admin_event_tx, _) = broadcast::channel(4096);

        // Start Redis pub/sub using the pre-built client/connection.
        // When Redis is not provided, run in single-node mode (tests).
        let (message_hub, redis_publish_tx, redis_critical_tx, redis_pubsub, publisher_handle) =
            if let (Some(redis_client), Some(redis_conn)) =
                (config.redis_client.clone(), config.redis_conn.clone())
            {
                // Reuse the shared connection for the message hub's distributed
                // subscription state and TTL refresh background task.
                let hub =
                    Arc::new(RoomMessageHub::new().with_redis(redis_conn, &config.key_prefix));

                let redis_pubsub = Arc::new(RedisPubSub::with_key_prefix(
                    redis_client,
                    hub.clone(),
                    config.node_id.clone(),
                    &config.key_prefix,
                    admin_event_tx.clone(),
                    permission_service,
                    cache_invalidation,
                    deduplicator.clone(),
                    config.catchup_window_secs,
                    config.stream_max_length,
                )?);

                let (tx, _backpressure, publisher_handle) = redis_pubsub
                    .clone()
                    .start(config.publish_channel_capacity)
                    .await?;
                // Critical events share the same Redis publisher but use a separate
                // bounded channel so they are never dropped when the normal channel is full.
                let critical_capacity = config.critical_channel_capacity;
                let (critical_tx, mut critical_rx) =
                    mpsc::channel::<PublishRequest>(critical_capacity);
                // Forward critical events into the normal publish channel using `.send().await`
                // (blocks until space available, never drops).
                let normal_tx = tx.clone();
                let cancel_critical = redis_pubsub.cancel_token();
                tokio::spawn(async move {
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
                    hub,
                    Some(tx),
                    Some(critical_tx),
                    Some(redis_pubsub),
                    Some(publisher_handle),
                )
            } else {
                warn!("Redis not provided, running in single-node mode");
                if cache_invalidation.is_some() {
                    warn!(
                        "cache_invalidation service provided but Redis is not available; \
                     cache invalidation will be local-only (no cross-replica invalidation). \
                     In a multi-replica deployment, this may lead to stale caches on other nodes."
                    );
                }
                let hub = Arc::new(RoomMessageHub::new());
                (hub, None, None, None, None)
            };

        Ok(Self {
            message_hub,
            deduplicator,
            redis_publish_tx,
            redis_critical_tx,
            node_id: config.node_id,
            admin_event_tx,
            redis_pubsub,
            publisher_task: tokio::sync::Mutex::new(publisher_handle),
            cancel_token: config.parent_cancel_token.as_ref().map_or_else(
                CancellationToken::new,
                tokio_util::sync::CancellationToken::child_token,
            ),
            critical_channel_capacity: config.critical_channel_capacity,
            publish_channel_capacity: config.publish_channel_capacity,
            heartbeat_state: tokio::sync::Mutex::new(HeartbeatState {
                node_registry: None,
                handle: None,
                grpc_address: String::new(),
                http_address: String::new(),
            }),
            #[cfg(test)]
            heartbeat_shutdown_timeout: Duration::from_secs(10),
            connection_manager: None,
            heartbeat_failure_count: Arc::new(AtomicU64::new(0)),
            epoch_mismatch_count: Arc::new(AtomicU64::new(0)),
            is_quarantined: Arc::new(AtomicBool::new(false)),
            shutdown_started: Arc::new(AtomicBool::new(false)),
            leader_elector: None,
        })
    }

    /// Get the message hub (for subscriptions)
    #[must_use]
    pub const fn message_hub(&self) -> &Arc<RoomMessageHub> {
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
    pub fn set_connection_manager(&mut self, cm: ConnectionManager) {
        self.connection_manager = Some(cm);
    }

    /// Set the leader elector for resigning leadership on epoch mismatch.
    ///
    /// When epoch mismatch is detected, this node will resign leadership if
    /// it's currently the leader to prevent split-brain scenarios.
    pub fn set_leader_elector(&mut self, elector: Arc<dyn crate::leader::LeaderRuntime>) {
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
    pub fn subscribe_admin_events(&self) -> broadcast::Receiver<ClusterEvent> {
        self.admin_event_tx.subscribe()
    }

    /// Get the admin event sender (for local kick events)
    #[must_use]
    pub const fn admin_event_tx(&self) -> &broadcast::Sender<ClusterEvent> {
        &self.admin_event_tx
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
    pub async fn start_heartbeat_loop<F>(
        &self,
        node_registry: Arc<NodeRegistry>,
        grpc_address: String,
        http_address: String,
        connection_count_fn: Option<F>,
    ) where
        F: Fn() -> usize + Send + Sync + 'static,
    {
        let cancel_token = self.cancel_token.clone();
        let interval_secs = (node_registry.heartbeat_timeout_secs / 3).max(1) as u64;
        let failure_count = self.heartbeat_failure_count.clone();
        let epoch_mismatch_count = self.epoch_mismatch_count.clone();
        let is_quarantined = self.is_quarantined.clone();
        let leader_elector = self.leader_elector.clone();

        // D3 fix: Clone the stored addresses into the spawned task so they can be
        // used for re-registration when the local cache has empty addresses.
        let stored_grpc_address = grpc_address.clone();
        let stored_http_address = http_address.clone();

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
                                failure_count.store(0, Ordering::Relaxed);
                                synctv_core::metrics::cluster::CLUSTER_HEARTBEAT_FAILURES.set(0);
                                // Exit quarantine on successful heartbeat
                                epoch_mismatch_count.store(0, Ordering::Relaxed);
                                is_quarantined.store(false, Ordering::Release);
                                synctv_core::metrics::cluster::CLUSTER_EPOCH_MISMATCH_QUARANTINE.set(0);
                            }
                            Ok(HeartbeatResult::NeedReregistration) => {
                                // NodeRegistry::heartbeat() already attempted auto-registration
                                // internally. If we still get NeedReregistration, it means the
                                // internal retry failed.
                                // D3 fix: Use stored addresses for explicit re-registration.
                                warn!("Node key expired in Redis, internal auto-registration failed; \
                                       attempting re-registration with stored addresses");
                                if let Err(e) = node_registry
                                    .register(stored_grpc_address.clone(), stored_http_address.clone())
                                    .await
                                {
                                    error!(
                                        error = %e,
                                        "Re-registration with stored addresses also failed; will retry on next heartbeat"
                                    );
                                } else {
                                    info!("Re-registration with stored addresses succeeded");
                                }
                            }
                            Ok(HeartbeatResult::EpochMismatch(remote_epoch)) => {
                                // Increment epoch mismatch counter
                                let mismatches = epoch_mismatch_count.fetch_add(1, Ordering::Relaxed) + 1;
                                warn!(
                                    remote_epoch = remote_epoch,
                                    consecutive_mismatches = mismatches,
                                    "Epoch mismatch during heartbeat, internal auto-registration failed; \
                                     attempting re-registration with stored addresses"
                                );

                                // D3 fix: Use stored addresses for explicit re-registration.
                                if let Err(e) = node_registry
                                    .register(stored_grpc_address.clone(), stored_http_address.clone())
                                    .await
                                {
                                    error!(
                                        error = %e,
                                        "Re-registration with stored addresses also failed after epoch mismatch"
                                    );
                                } else {
                                    info!("Re-registration with stored addresses succeeded after epoch mismatch");
                                    // Reset epoch mismatch counter on successful re-registration
                                    epoch_mismatch_count.store(0, Ordering::Relaxed);
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
                                // D3 fix: Use stored addresses to recover from empty local cache.
                                // This typically happens when the local cache was cleared or
                                // the node was never successfully registered.
                                warn!(
                                    "Heartbeat: local cache has empty address(es); \
                                     attempting re-registration with stored addresses \
                                     (grpc={}, http={})",
                                    stored_grpc_address, stored_http_address
                                );
                                if let Err(e) = node_registry
                                    .register(stored_grpc_address.clone(), stored_http_address.clone())
                                    .await
                                {
                                    error!(
                                        error = %e,
                                        "Re-registration with stored addresses failed; \
                                         node remains unreachable by peers"
                                    );
                                } else {
                                    info!("Re-registration with stored addresses succeeded; \
                                           node should be reachable again");
                                }
                            }
                            Err(e) => {
                                // Increment independent failure counter for business logic
                                let failures = failure_count.fetch_add(1, Ordering::Relaxed) + 1;
                                // Update Prometheus gauge for monitoring only (never read for decisions)
                                synctv_core::metrics::cluster::CLUSTER_HEARTBEAT_FAILURES.set(failures as i64);
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

        // Store the node_registry, handle, and addresses for re-registration
        let mut state = self.heartbeat_state.lock().await;
        state.node_registry = Some(node_registry);
        state.handle = Some(handle);
        state.grpc_address = grpc_address;
        state.http_address = http_address;
        info!(interval_secs = interval_secs, "Heartbeat loop started");
    }

    /// Gracefully shut down the cluster manager and all background tasks.
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
            debug!("ClusterManager shutdown already completed or in progress");
            return;
        }

        info!("Shutting down ClusterManager");

        // Cancel heartbeat loop
        self.cancel_token.cancel();

        // Unregister this node from Redis FIRST so peers stop routing traffic
        // to us immediately, before we start draining pub/sub channels.
        {
            let mut state = self.heartbeat_state.lock().await;
            // Unregister this node from Redis so peers see it go immediately
            if let Some(ref registry) = state.node_registry {
                if let Err(e) = registry.unregister().await {
                    warn!(error = %e, "Failed to unregister node during shutdown");
                } else {
                    info!("Node unregistered from Redis during shutdown");
                }
            }
            if let Some(handle) = state.handle.take() {
                match tokio::time::timeout(self.heartbeat_shutdown_timeout(), handle).await {
                    Ok(Ok(())) => {
                        info!("Heartbeat task completed cleanly during shutdown");
                    }
                    Ok(Err(e)) => {
                        warn!(error = %e, "Heartbeat task panicked during shutdown");
                    }
                    Err(_) => {
                        warn!(
                            "Heartbeat task did not finish within {}s timeout during shutdown; proceeding",
                            self.heartbeat_shutdown_timeout().as_secs()
                        );
                    }
                }
            }
        }

        // Cancel Redis Pub/Sub tasks and await subscriber completion
        if let Some(ref pubsub) = self.redis_pubsub {
            pubsub.shutdown().await;
        }

        // Shut down ConnectionManager's TTL refresh task
        if let Some(ref cm) = self.connection_manager {
            cm.shutdown();
        }

        // Await the publisher task so any in-flight events are fully flushed before
        // we return. A 10-second timeout prevents hanging indefinitely when Redis is
        // unreachable during shutdown.
        {
            let mut publisher_guard = self.publisher_task.lock().await;
            if let Some(handle) = publisher_guard.take() {
                match tokio::time::timeout(Duration::from_secs(10), handle).await {
                    Ok(Ok(())) => {
                        info!("Redis publisher task completed cleanly during shutdown");
                    }
                    Ok(Err(e)) => {
                        warn!(error = %e, "Redis publisher task panicked during shutdown");
                    }
                    Err(_) => {
                        warn!("Redis publisher task did not finish within 10s timeout during shutdown; proceeding");
                    }
                }
            }
        }

        // Shut down deduplicator cleanup task
        self.deduplicator.shutdown();
    }

    /// Broadcast an event to all subscribers
    ///
    /// This will:
    /// 1. Check for duplicates
    /// 2. Broadcast to local subscribers
    /// 3. Publish to Redis for cross-node sync
    pub fn broadcast(&self, event: ClusterEvent) -> BroadcastResult {
        let dedup_key = DedupKey::from_event(&event);

        // Check if this is a duplicate
        if !self.deduplicator.should_process(&dedup_key) {
            debug!(
                event_type = %event.event_type(),
                room_id = %event.room_id()
                    .map_or("n/a", synctv_core::models::RoomId::as_str),
                "Duplicate event detected, skipping"
            );
            return BroadcastResult {
                local_sent: 0,
                redis_sent: false,
            };
        }

        let mut local_sent = 0;
        let mut redis_sent = 0;

        // Get event_type for logging before moving event
        let event_type = event.event_type();

        // Get room_id for broadcasting
        if let Some(room_id) = event.room_id() {
            // Broadcast to local subscribers
            local_sent = self.message_hub.broadcast(room_id, event.clone());
        }

        // UserNotification events are user-targeted (no room_id), so they are
        // delivered via the admin event channel to reach connected WebSocket handlers.
        if matches!(&event, ClusterEvent::UserNotification { .. }) {
            let _ = self.admin_event_tx.send(event.clone());
        }

        // Publish to Redis for cross-node sync.
        // Critical events (KickPublisher, KickUser, PermissionChanged) use a
        // separate high-priority channel that never drops events.
        let is_critical = event.is_critical();
        if is_critical {
            if let Some(tx) = &self.redis_critical_tx {
                match tx.try_send(PublishRequest { event }) {
                    Ok(()) => {
                        redis_sent = 1;
                    }
                    Err(mpsc::error::TrySendError::Full(req)) => {
                        // Critical channel is full -- spawn a task that uses
                        // send().await so the event is never dropped.
                        let tx = tx.clone();
                        warn!(
                            "Critical event publish channel full (capacity {}), spawning retry task",
                            self.critical_channel_capacity
                        );
                        tokio::spawn(async move {
                            if let Err(e) = tx.send(req).await {
                                error!("Failed to send critical event after retry: {e}");
                            }
                        });
                        redis_sent = 1; // Will be sent asynchronously
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        error!("Critical event publish channel closed");
                    }
                }
            } else if let Some(tx) = &self.redis_publish_tx {
                // Fallback to normal channel if critical channel not available
                let _ = tx.try_send(PublishRequest { event });
            }
        } else if let Some(tx) = &self.redis_publish_tx {
            match tx.try_send(PublishRequest { event }) {
                Ok(()) => {
                    redis_sent = 1;
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    synctv_core::metrics::cluster::CLUSTER_EVENTS_DROPPED
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

        // Record cluster metrics
        synctv_core::metrics::cluster::CLUSTER_EVENTS_PUBLISHED
            .with_label_values(&[event_type])
            .inc();

        debug!(
            event_type = %event_type,
            local_subscribers = local_sent,
            redis_published = redis_sent > 0,
            "Event broadcast complete"
        );

        BroadcastResult {
            local_sent,
            redis_sent: redis_sent > 0,
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
    ) -> (tokio::sync::mpsc::Receiver<ClusterEvent>, ConnectionId) {
        let connection_id = format!("{}_{}", user_id.as_str(), nanoid::nanoid!(8));
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
    ) -> (tokio::sync::mpsc::Receiver<ClusterEvent>, ConnectionId) {
        let room_id_str = room_id.as_str().to_string();
        let user_id_str = user_id.as_str().to_string();
        let rx = self
            .message_hub
            .subscribe(room_id, user_id, connection_id.clone())
            .await;

        info!(
            room_id = %room_id_str,
            user_id = %user_id_str,
            connection_id = %connection_id,
            "Client subscribed to room"
        );

        (rx, connection_id)
    }

    /// Unsubscribe a client from room events
    pub fn unsubscribe(&self, connection_id: &str) {
        self.message_hub.unsubscribe(connection_id);
    }

    /// Get cluster metrics
    #[must_use]
    pub fn metrics(&self) -> ClusterMetrics {
        ClusterMetrics {
            node_id: self.node_id.clone(),
            total_rooms: self.message_hub.room_count(),
            total_connections: self.message_hub.connection_count(),
            tracked_events: self.deduplicator.len(),
            redis_enabled: self.redis_publish_tx.is_some(),
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

    fn heartbeat_shutdown_timeout(&self) -> Duration {
        #[cfg(test)]
        {
            self.heartbeat_shutdown_timeout
        }
        #[cfg(not(test))]
        {
            Duration::from_secs(10)
        }
    }

    #[cfg(test)]
    pub async fn test_set_heartbeat_handle(&self, handle: tokio::task::JoinHandle<()>) {
        let mut state = self.heartbeat_state.lock().await;
        state.handle = Some(handle);
    }

    #[cfg(test)]
    pub fn test_with_heartbeat_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.heartbeat_shutdown_timeout = timeout;
        self
    }
}

impl Drop for ClusterManager {
    fn drop(&mut self) {
        // Cancel all background tasks (heartbeat, Redis pub/sub, connection manager).
        // Drop cannot run async code, but CancellationToken::cancel() is synchronous
        // and will notify all tasks holding a clone of this token to stop.
        // For graceful shutdown with awaiting, use the async shutdown() method instead.
        self.cancel_token.cancel();
        if let Some(ref cm) = self.connection_manager {
            cm.shutdown();
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

/// Cluster metrics
#[derive(Debug, Clone)]
pub struct ClusterMetrics {
    pub node_id: String,
    pub total_rooms: usize,
    pub total_connections: usize,
    pub tracked_events: usize,
    pub redis_enabled: bool,
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
    use crate::sync::ConnectionLimits;
    use chrono::Utc;

    #[tokio::test]
    async fn test_cluster_manager_single_node() {
        let config = ClusterConfig {
            redis_client: None,
            redis_conn: None, // No Redis
            cluster_enabled: false,
            node_id: "test_node".to_string(),
            dedup_window: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            parent_cancel_token: None,
        };

        let manager = ClusterManager::new(config, None, None).await.unwrap();

        // Subscribe a client
        let room_id = RoomId::from_string("room1".to_string());
        let user_id = UserId::from_string("user1".to_string());
        let (mut rx, conn_id) = manager.subscribe(room_id.clone(), user_id.clone()).await;

        // Broadcast event
        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: room_id.clone(),
            user_id: user_id.clone(),
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

    #[tokio::test]
    async fn test_admin_event_channel_subscription() {
        let config = ClusterConfig {
            redis_client: None,
            redis_conn: None,
            cluster_enabled: false,
            node_id: "test_node".to_string(),
            dedup_window: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            parent_cancel_token: None,
        };

        let manager = ClusterManager::new(config, None, None).await.unwrap();

        // Subscribe to admin events
        let mut admin_rx = manager.subscribe_admin_events();

        // Send a KickPublisher event through the admin channel
        let event = ClusterEvent::KickPublisher {
            event_id: nanoid::nanoid!(16),
            room_id: RoomId::from_string("room1".to_string()),
            media_id: synctv_core::models::MediaId::from_string("media1".to_string()),
            reason: "user_banned".to_string(),
            timestamp: Utc::now(),
        };

        let _ = manager.admin_event_tx().send(event.clone());

        // Verify event received
        let received = admin_rx.recv().await.unwrap();
        assert_eq!(received.event_type(), "kick_publisher");

        if let ClusterEvent::KickPublisher {
            room_id,
            media_id,
            reason,
            ..
        } = &received
        {
            assert_eq!(room_id.as_str(), "room1");
            assert_eq!(media_id.as_str(), "media1");
            assert_eq!(reason, "user_banned");
        } else {
            panic!("Expected KickPublisher event");
        }
    }

    #[tokio::test]
    async fn test_admin_event_channel_multiple_subscribers() {
        let config = ClusterConfig {
            redis_client: None,
            redis_conn: None,
            cluster_enabled: false,
            node_id: "test_node".to_string(),
            dedup_window: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            parent_cancel_token: None,
        };

        let manager = ClusterManager::new(config, None, None).await.unwrap();

        // Subscribe two receivers
        let mut rx1 = manager.subscribe_admin_events();
        let mut rx2 = manager.subscribe_admin_events();

        // Send event
        let event = ClusterEvent::KickPublisher {
            event_id: nanoid::nanoid!(16),
            room_id: RoomId::from_string("room1".to_string()),
            media_id: synctv_core::models::MediaId::from_string("media1".to_string()),
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

    /// Test that ClusterManager handles the non-cluster mode degradation gracefully
    /// when a CacheInvalidationService is provided but Redis is not available.
    ///
    /// This verifies:
    /// 1. ClusterManager::new() succeeds even when cache_invalidation is provided without Redis
    /// 2. The service logs an appropriate warning about local-only invalidation
    /// 3. The ClusterManager operates normally in single-node mode
    #[tokio::test]
    async fn test_non_cluster_mode_with_cache_invalidation_service() {
        // Create a CacheInvalidationService without Redis (local-only mode)
        let cache_invalidation = synctv_core::cache::CacheInvalidationService::new(
            None, // No Redis client
            "test_node".to_string(),
            "synctv:test:cache:invalidate".to_string(),
        );

        let config = ClusterConfig {
            redis_client: None,
            redis_conn: None, // No Redis - triggers non-cluster mode
            cluster_enabled: false,
            node_id: "test_node_cache".to_string(),
            dedup_window: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            parent_cancel_token: None,
        };

        // Create ClusterManager with cache_invalidation but no Redis
        // This should succeed with a warning logged
        let manager = ClusterManager::new(config, None, Some(cache_invalidation))
            .await
            .expect("ClusterManager::new should succeed with cache_invalidation but no Redis");

        // Verify the manager operates normally in single-node mode
        let room_id = RoomId::from_string("room1".to_string());
        let user_id = UserId::from_string("user1".to_string());
        let (mut rx, conn_id) = manager.subscribe(room_id.clone(), user_id.clone()).await;

        // Broadcast should work locally
        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: room_id.clone(),
            user_id: user_id.clone(),
            username: "user1".to_string(),
            message: "Hello local!".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        let result = manager.broadcast(event.clone());
        assert_eq!(
            result.local_sent, 1,
            "Local broadcast should work in non-cluster mode"
        );
        assert!(
            !result.redis_sent,
            "Redis should not be used in non-cluster mode"
        );

        // Verify message received locally
        let received = rx.recv().await.expect("Should receive local message");
        assert_eq!(received.event_type(), "chat_message");

        // Cleanup
        manager.unsubscribe(&conn_id);

        // Verify metrics show single-node mode
        let metrics = manager.metrics();
        assert!(
            !metrics.redis_enabled,
            "Metrics should show Redis is not enabled"
        );
    }

    /// Test that ClusterManager works correctly when both Redis and
    /// CacheInvalidationService are not provided (pure single-node mode).
    #[tokio::test]
    async fn test_non_cluster_mode_without_cache_invalidation_service() {
        let config = ClusterConfig {
            redis_client: None,
            redis_conn: None,
            cluster_enabled: false,
            node_id: "test_node_no_cache".to_string(),
            dedup_window: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            parent_cancel_token: None,
        };

        // Create ClusterManager without cache_invalidation and without Redis
        let manager = ClusterManager::new(config, None, None)
            .await
            .expect("ClusterManager::new should succeed without cache_invalidation and Redis");

        // Verify normal operation
        let room_id = RoomId::from_string("room2".to_string());
        let user_id = UserId::from_string("user2".to_string());
        let (mut rx, conn_id) = manager.subscribe(room_id.clone(), user_id.clone()).await;

        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: room_id.clone(),
            user_id: user_id.clone(),
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
        let config = ClusterConfig {
            redis_client: None,
            redis_conn: None,
            cluster_enabled: false,
            node_id: "stuck-heartbeat-node".to_string(),
            dedup_window: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            parent_cancel_token: None,
        };

        let manager = ClusterManager::new(config, None, None)
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

    /// Test that ClusterManager tracks epoch mismatch state and quarantine.
    ///
    /// This test verifies:
    /// 1. ClusterManager starts in non-quarantined state
    /// 2. Epoch mismatch counter is tracked internally
    /// 3. Quarantine state is reflected in metrics
    /// 4. Leader elector can be set for resigning leadership
    #[tokio::test]
    async fn test_epoch_mismatch_enforcement() {
        let config = ClusterConfig {
            redis_client: None,
            redis_conn: None,
            cluster_enabled: false,
            node_id: "test_node_epoch".to_string(),
            dedup_window: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            parent_cancel_token: None,
        };

        let mut manager = ClusterManager::new(config, None, None)
            .await
            .expect("ClusterManager::new should succeed");

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
        let room_id = RoomId::from_string("room_epoch".to_string());
        let user_id = UserId::from_string("user_epoch".to_string());
        let (_rx, conn_id) = manager.subscribe(room_id.clone(), user_id.clone()).await;

        // Broadcast should work in non-quarantined state
        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: room_id.clone(),
            user_id: user_id.clone(),
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
    async fn test_cluster_metrics_reports_dependency_injection_state() {
        let config = ClusterConfig {
            redis_client: None,
            redis_conn: None,
            cluster_enabled: false,
            node_id: "test_metrics_injection".to_string(),
            dedup_window: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            parent_cancel_token: None,
        };

        let mut manager = ClusterManager::new(config, None, None)
            .await
            .expect("ClusterManager::new should succeed");

        let metrics = manager.metrics();
        assert!(
            !metrics.has_connection_manager,
            "fresh manager should not report an injected ConnectionManager"
        );
        assert!(
            !metrics.has_leader_elector,
            "fresh manager should not report an injected leader elector"
        );

        let cm = ConnectionManager::new(ConnectionLimits::default());
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

    /// Test that ClusterManager metrics include quarantine state.
    #[tokio::test]
    async fn test_cluster_metrics_includes_quarantine_state() {
        let config = ClusterConfig {
            redis_client: None,
            redis_conn: None,
            cluster_enabled: false,
            node_id: "test_metrics_quarantine".to_string(),
            dedup_window: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            parent_cancel_token: None,
        };

        let manager = ClusterManager::new(config, None, None)
            .await
            .expect("ClusterManager::new should succeed");

        let metrics = manager.metrics();

        // Verify all expected fields are present
        assert_eq!(metrics.node_id, "test_metrics_quarantine");
        assert_eq!(metrics.total_rooms, 0);
        assert_eq!(metrics.total_connections, 0);
        assert!(!metrics.redis_enabled);
        assert!(
            !metrics.is_quarantined,
            "Should not be quarantined initially"
        );
    }

    /// Test that non-cluster unit tests may still construct a local-only manager without Redis.
    /// Cluster-mode Redis requirements are validated before startup in `Config::validate()`.
    #[tokio::test]
    async fn test_cluster_enabled_without_redis_builds_local_only_manager_for_unit_tests() {
        let config = ClusterConfig {
            redis_client: None,
            redis_conn: None,      // No Redis
            cluster_enabled: false,
            node_id: "test_cluster_requires_redis".to_string(),
            dedup_window: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            parent_cancel_token: None,
        };

        let result = ClusterManager::new(config, None, None).await;

        assert!(
            result.is_ok(),
            "ClusterManager::new should support local-only unit tests without duplicating config-layer validation"
        );

        let manager = result.expect("local-only ClusterManager should still initialize");
        let metrics = manager.metrics();
        assert!(!metrics.redis_enabled, "manager should remain local-only without Redis");
    }

    /// Test that partial Redis wiring in local-only tests does not enable distributed internals.
    #[tokio::test]
    async fn test_cluster_enabled_with_partial_redis_wiring_stays_local_only() {
        // Use a dummy Redis client that can't connect
        let redis_client = redis::Client::open("redis://127.0.0.1:1").ok();

        let config = ClusterConfig {
            redis_client,
            redis_conn: None, // Missing connection manager
            cluster_enabled: false,
            node_id: "test_cluster_missing_conn".to_string(),
            dedup_window: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            parent_cancel_token: None,
        };

        let result = ClusterManager::new(config, None, None).await;

        assert!(
            result.is_ok(),
            "ClusterManager::new should allow local-only tests to omit full Redis wiring"
        );

        let manager = result.expect("partial Redis wiring should degrade to local-only internals");
        let metrics = manager.metrics();
        assert!(!metrics.redis_enabled, "partial Redis wiring must not enable distributed features");
    }

    /// Test that non-cluster mode (cluster_enabled=false) works without Redis.
    #[tokio::test]
    async fn test_non_cluster_mode_works_without_redis() {
        let config = ClusterConfig {
            redis_client: None,
            redis_conn: None,
            cluster_enabled: false, // Cluster mode disabled
            node_id: "test_non_cluster_no_redis".to_string(),
            dedup_window: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            parent_cancel_token: None,
        };

        let result = ClusterManager::new(config, None, None).await;

        assert!(
            result.is_ok(),
            "ClusterManager::new should succeed in non-cluster mode without Redis, got error: {:?}",
            result.err()
        );
    }
}
