//! Complete cluster synchronization service
//!
//! This module provides a unified interface for all cross-cluster functionality:
//! - Message broadcasting (local)
//! - Redis pub/sub (cross-node)
//! - Message deduplication
//! - Connection management
//! - Metrics and monitoring

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::dedup::{DedupKey, MessageDeduplicator};
use super::events::ClusterEvent;
use super::redis_pubsub::{PublishRequest, RedisPubSub};
use super::room_hub::{ConnectionId, RoomMessageHub};
use crate::discovery::{HeartbeatResult, NodeRegistry};
use crate::error::Result as ClusterResult;
use synctv_core::models::id::{RoomId, UserId};
use synctv_core::service::PermissionService;

/// Cluster configuration
#[derive(Debug, Clone)]
pub struct ClusterConfig {
    /// Redis connection URL
    pub redis_url: String,
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
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            redis_url: "redis://127.0.0.1:6379".to_string(),
            node_id: format!("node_{}", nanoid::nanoid!(8)),
            dedup_window: Duration::from_secs(10),
            cleanup_interval: Duration::from_secs(30),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
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
    /// Cancellation token for background heartbeat task
    cancel_token: CancellationToken,
    /// Node registry + heartbeat handle (behind Mutex for async shutdown from &self)
    heartbeat_state: tokio::sync::Mutex<HeartbeatState>,
    /// Capacity for the critical event channel (for logging)
    critical_channel_capacity: usize,
    /// Capacity for the publish channel (for logging)
    publish_channel_capacity: usize,
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
        let message_hub = Arc::new(RoomMessageHub::new());
        let deduplicator = Arc::new(MessageDeduplicator::new(
            config.dedup_window,
            config.cleanup_interval,
        ));

        let (admin_event_tx, _) = broadcast::channel(256);

        // Start Redis pub/sub if Redis URL is provided
        let (redis_publish_tx, redis_critical_tx, redis_pubsub) = if config.redis_url.is_empty() {
            warn!("Redis URL not provided, running in single-node mode");
            (None, None, None)
        } else {
            let redis_pubsub = Arc::new(
                RedisPubSub::new(
                    &config.redis_url,
                    message_hub.clone(),
                    config.node_id.clone(),
                    admin_event_tx.clone(),
                    permission_service,
                    cache_invalidation,
                    deduplicator.clone(),
                )?
            );

            let tx = redis_pubsub.clone().start(config.publish_channel_capacity).await?;
            // Critical events share the same Redis publisher but use a separate
            // bounded channel so they are never dropped when the normal channel is full.
            let critical_capacity = config.critical_channel_capacity;
            let (critical_tx, mut critical_rx) = mpsc::channel::<PublishRequest>(critical_capacity);
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

            (Some(tx), Some(critical_tx), Some(redis_pubsub))
        };

        Ok(Self {
            message_hub,
            deduplicator,
            redis_publish_tx,
            redis_critical_tx,
            node_id: config.node_id,
            admin_event_tx,
            redis_pubsub,
            cancel_token: CancellationToken::new(),
            critical_channel_capacity: config.critical_channel_capacity,
            publish_channel_capacity: config.publish_channel_capacity,
            heartbeat_state: tokio::sync::Mutex::new(HeartbeatState {
                node_registry: None,
                handle: None,
                grpc_address: String::new(),
                http_address: String::new(),
            }),
        })
    }

    /// Create with default configuration
    pub async fn with_defaults() -> ClusterResult<Self> {
        Self::new(ClusterConfig::default(), None, None).await
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
    /// Calls `NodeRegistry::heartbeat()` every `heartbeat_timeout / 2` seconds.
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
    )
    where
        F: Fn() -> usize + Send + Sync + 'static,
    {
        let cancel_token = self.cancel_token.clone();
        let interval_secs = (node_registry.heartbeat_timeout_secs / 3).max(1) as u64;

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
                                synctv_core::metrics::cluster::CLUSTER_HEARTBEAT_FAILURES.set(0);
                            }
                            Ok(HeartbeatResult::NeedReregistration) => {
                                // NodeRegistry::heartbeat() already attempted auto-registration
                                // internally. If we still get NeedReregistration, it means the
                                // internal retry failed. Just log and let the next tick try again.
                                warn!("Node key expired in Redis, internal auto-registration failed; will retry on next heartbeat");
                            }
                            Ok(HeartbeatResult::EpochMismatch(remote_epoch)) => {
                                // NodeRegistry::heartbeat() already attempted auto-registration
                                // internally. If we still get EpochMismatch, the retry failed.
                                warn!(
                                    remote_epoch = remote_epoch,
                                    "Epoch mismatch during heartbeat, internal auto-registration failed; will retry on next heartbeat"
                                );
                            }
                            Err(e) => {
                                // Increment failure counter for network partition detection
                                synctv_core::metrics::cluster::CLUSTER_HEARTBEAT_FAILURES.inc();
                                let failures = synctv_core::metrics::cluster::CLUSTER_HEARTBEAT_FAILURES.get();
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
        info!(
            interval_secs = interval_secs,
            "Heartbeat loop started"
        );
    }


    /// Gracefully shut down the cluster manager and all background tasks.
    ///
    /// This method:
    /// 1. Cancels the heartbeat loop
    /// 2. Shuts down Redis Pub/Sub (which drains pending publishes)
    /// 3. Unregisters this node from Redis
    /// 4. Shuts down the deduplicator cleanup task
    /// 5. Awaits background task completion
    pub async fn shutdown(&self) {
        info!("Shutting down ClusterManager");

        // Cancel heartbeat loop
        self.cancel_token.cancel();

        // Cancel Redis Pub/Sub tasks
        if let Some(ref pubsub) = self.redis_pubsub {
            pubsub.shutdown();
        }

        // Wait for the heartbeat task to finish and unregister
        {
            let mut state = self.heartbeat_state.lock().await;
            if let Some(handle) = state.handle.take() {
                let _ = handle.await;
            }
            // Unregister this node from Redis so peers see it go immediately
            if let Some(ref registry) = state.node_registry {
                if let Err(e) = registry.unregister().await {
                    warn!(error = %e, "Failed to unregister node during shutdown");
                } else {
                    info!("Node unregistered from Redis during shutdown");
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

        // Publish to Redis for cross-node sync.
        // Critical events (KickPublisher, KickUser, PermissionChanged) use a
        // separate high-priority channel that never drops events.
        let is_critical = event.is_critical();
        if is_critical {
            if let Some(tx) = &self.redis_critical_tx {
                match tx.try_send(PublishRequest {
                    event,
                }) {
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
                let _ = tx.try_send(PublishRequest {
                    event,
                });
            }
        } else if let Some(tx) = &self.redis_publish_tx {
            match tx.try_send(PublishRequest {
                event,
            }) {
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
        self.subscribe_with_id(room_id, user_id, connection_id).await
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
        let rx = self.message_hub.subscribe(room_id, user_id, connection_id.clone()).await;

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
        }
    }

    /// Get subscribers in a room
    #[must_use] 
    pub fn get_room_subscribers(&self, room_id: &RoomId) -> Vec<(UserId, ConnectionId)> {
        self.message_hub.get_room_subscribers(room_id)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn test_cluster_manager_single_node() {
        let config = ClusterConfig {
            redis_url: "".to_string(), // No Redis
            node_id: "test_node".to_string(),
            dedup_window: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
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
        assert!(matches!(result2, BroadcastResult { local_sent: 0, redis_sent: false }));

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
            redis_url: "".to_string(),
            node_id: "test_node".to_string(),
            dedup_window: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
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

        if let ClusterEvent::KickPublisher { room_id, media_id, reason, .. } = &received {
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
            redis_url: "".to_string(),
            node_id: "test_node".to_string(),
            dedup_window: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
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
}
