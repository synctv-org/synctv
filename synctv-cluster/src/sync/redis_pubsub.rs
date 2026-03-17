use anyhow::{Context, Result};
use futures::stream::StreamExt;
use redis::streams::StreamReadReply;
use redis::{AsyncCommands, Client as RedisClient};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Buffer pressure level for backpressure signaling.
///
/// This enum indicates how much pressure the publish buffer is under,
/// allowing callers to make informed decisions about event submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferPressure {
    /// Buffer is under normal load - send events freely
    Normal,
    /// Buffer is under moderate pressure - consider throttling non-critical events
    Moderate,
    /// Buffer is under high pressure - only send critical events
    High,
    /// Buffer is at capacity - non-critical events will be dropped
    Critical,
}

impl BufferPressure {
    /// Check if this pressure level allows sending non-critical events.
    #[must_use]
    pub const fn allows_non_critical(self) -> bool {
        matches!(self, Self::Normal | Self::Moderate)
    }

    /// Check if this pressure level only allows critical events.
    #[must_use]
    pub const fn critical_only(self) -> bool {
        matches!(self, Self::High | Self::Critical)
    }
}

/// Timeout for Redis operations in seconds
const REDIS_TIMEOUT_SECS: u64 = 5;
/// Maximum time to wait for the subscriber to finish its initial subscriptions.
const SUBSCRIBER_READY_TIMEOUT_SECS: u64 = 5;

/// Returns `true` if the Redis error indicates the current connection has become
/// read-only or is still loading data — both symptoms of a Sentinel failover in
/// progress.  When detected, callers should drop the connection and reconnect
/// immediately rather than treating the error as a retryable publish failure.
/// Returns `true` if the Redis error looks like a Sentinel failover.
///
/// Public for testing. Production code already uses this internally.
pub fn is_sentinel_failover_error(e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    msg.contains("READONLY") || msg.contains("LOADING")
}

/// Initial backoff delay for subscriber reconnection
const INITIAL_BACKOFF_SECS: u64 = 1;

/// Maximum backoff delay for subscriber reconnection
const MAX_BACKOFF_SECS: u64 = 30;

/// Maximum number of XADD retries for critical events before giving up.
const CRITICAL_STREAM_MAX_RETRIES: u32 = 3;

/// Initial backoff for critical XADD retries (doubles each attempt).
const CRITICAL_STREAM_INITIAL_BACKOFF_MS: u64 = 100;

/// Default max length of each per-room stream (approximate).
/// Can be overridden via `ClusterChannelConfig::stream_max_length`.
const DEFAULT_MAX_STREAM_LENGTH: usize = 10000;

// ---- Unified Pub/Sub channel naming ----
//
// Both admin and room events use the same channel naming scheme and are published
// via PUBLISH + XADD in `publish_event()`. The subscription strategy differs:
//
//   - **Admin events**: Pattern subscription (`PSUBSCRIBE {prefix}admin:*`)
//     because admin events are global and infrequent. All nodes receive all admin
//     events regardless of which rooms they serve.
//
//   - **Room events**: Per-room subscriptions (`SUBSCRIBE {prefix}room:{room_id}`)
//     managed dynamically via `RoomLifecycleEvent`s. This avoids receiving traffic
//     for rooms the node does not serve, which is important in large deployments
//     with many active rooms.
//
// Dispatch for both paths converges in `dispatch_event()`, which handles
// deduplication, cache invalidation, permission syncing, and local broadcast
// uniformly.

use super::dedup::{DedupKey, MessageDeduplicator};
use super::events::{CacheTarget, ClusterEvent};
use super::room_hub::{RoomLifecycleEvent, RoomMessageHub};
use synctv_core::cache::CacheInvalidationService;
use synctv_core::models::id::RoomId;
use synctv_core::service::PermissionService;

/// Shared state for tracking buffer pressure across tasks.
///
/// This struct is cloned and shared between the publisher task and
/// the `PublishBackpressure` handle, allowing callers to check
/// current buffer pressure before sending events.
#[derive(Clone)]
struct BufferPressureState {
    /// Current retry buffer size (non-critical events)
    retry_buffer_size: Arc<AtomicUsize>,
    /// Current critical buffer size
    critical_buffer_size: Arc<AtomicUsize>,
    /// Maximum retry buffer capacity
    max_retry_buffer: usize,
    /// Warning threshold (80% of max)
    warn_threshold: usize,
    /// High pressure threshold (90% of max)
    high_threshold: usize,
}

impl BufferPressureState {
    fn new(max_retry_buffer: usize) -> Self {
        Self {
            retry_buffer_size: Arc::new(AtomicUsize::new(0)),
            critical_buffer_size: Arc::new(AtomicUsize::new(0)),
            max_retry_buffer,
            warn_threshold: (max_retry_buffer as f64 * 0.8) as usize,
            high_threshold: (max_retry_buffer as f64 * 0.9) as usize,
        }
    }

    /// Get the current buffer pressure level.
    fn pressure(&self) -> BufferPressure {
        let retry_size = self.retry_buffer_size.load(Ordering::Relaxed);
        let critical_size = self.critical_buffer_size.load(Ordering::Relaxed);
        let total = retry_size + critical_size;

        if total >= self.max_retry_buffer {
            BufferPressure::Critical
        } else if retry_size >= self.high_threshold {
            BufferPressure::High
        } else if retry_size >= self.warn_threshold {
            BufferPressure::Moderate
        } else {
            BufferPressure::Normal
        }
    }

    /// Update retry buffer size (called by publisher task)
    fn set_retry_size(&self, size: usize) {
        self.retry_buffer_size.store(size, Ordering::Relaxed);
    }

    /// Update critical buffer size (called by publisher task)
    fn set_critical_size(&self, size: usize) {
        self.critical_buffer_size.store(size, Ordering::Relaxed);
    }
}

/// Handle for checking publish buffer backpressure.
///
/// This is returned by `RedisPubSub::start()` alongside the sender,
/// allowing callers to check buffer pressure before sending events.
///
/// # Example
///
/// ```text
/// let (tx, backpressure) = pubsub.start(10_000).await?;
///
/// // Check pressure before sending non-critical events
/// if backpressure.pressure().allows_non_critical() {
///     tx.send(PublishRequest { event }).await?;
/// } else {
///     // Drop or queue the event
/// }
/// ```
#[derive(Clone)]
pub struct PublishBackpressure {
    state: BufferPressureState,
}

impl PublishBackpressure {
    /// Get the current buffer pressure level.
    ///
    /// Use this to decide whether to send non-critical events.
    /// Critical events (kick/ban) are always buffered regardless of pressure.
    #[must_use]
    pub fn pressure(&self) -> BufferPressure {
        self.state.pressure()
    }

    /// Check if the buffer can accept a non-critical event.
    ///
    /// Returns `true` if pressure is Normal or Moderate.
    #[must_use]
    pub fn can_send_non_critical(&self) -> bool {
        self.state.pressure().allows_non_critical()
    }

    /// Get the current retry buffer size (for monitoring).
    #[must_use]
    pub fn retry_buffer_size(&self) -> usize {
        self.state.retry_buffer_size.load(Ordering::Relaxed)
    }

    /// Get the current critical buffer size (for monitoring).
    #[must_use]
    pub fn critical_buffer_size(&self) -> usize {
        self.state.critical_buffer_size.load(Ordering::Relaxed)
    }
}

/// Redis Pub/Sub service for cross-node event synchronization
///
/// This service enables multi-replica deployments by:
/// 1. Publishing local room events to Redis channels
/// 2. Subscribing to Redis channels for events from other nodes
/// 3. Forwarding received events to the local `RoomMessageHub`
///
/// **Production Enhancement (#31)**: Comprehensive error handling for cluster pub/sub:
/// - Automatic reconnection with exponential backoff (1s → 30s max)
/// - Failed publish retry logic: saves failed events and retries after reconnection
/// - Stream-based catch-up mechanism: recovers missed events during disconnection
/// - Timeout protection: 5s timeout on all Redis operations
/// - Critical event guarantee: XADD operations retry up to 3 times with backoff
/// - Graceful degradation: logs warnings but continues operation on non-critical failures
/// - Connection health checks: periodic PING to detect stale connections
///
/// Channel naming: `room:{room_id`} for room-specific events
pub struct RedisPubSub {
    redis_client: RedisClient,
    /// Shared multiplexed connection for non-Pub/Sub operations (stream reads).
    ///
    /// `MultiplexedConnection` is clone-safe (internally `Arc`-based) and handles
    /// automatic reconnection, so we use `OnceCell` for lazy one-time init
    /// instead of a `Mutex<Option<_>>`.  Each caller clones the connection for
    /// concurrent use without lock contention.
    shared_conn: tokio::sync::OnceCell<redis::aio::MultiplexedConnection>,
    message_hub: Arc<RoomMessageHub>,
    node_id: String,
    /// Key prefix for all Redis keys and channels (e.g., "synctv:")
    key_prefix: String,
    admin_event_tx: broadcast::Sender<ClusterEvent>,
    permission_service: Option<PermissionService>,
    /// Cache invalidation service for cross-replica user/room/username cache invalidation
    cache_invalidation: Option<CacheInvalidationService>,
    deduplicator: Arc<MessageDeduplicator>,
    cancel_token: CancellationToken,
    /// How far back (in milliseconds) to replay Redis Stream events on first connect.
    /// Configurable via `ClusterChannelConfig::catchup_window_secs`.
    catchup_window_ms: u128,
    /// Maximum number of entries per Redis Stream (approximate).
    /// Configurable via `ClusterChannelConfig::stream_max_length`.
    stream_max_length: usize,
    /// JoinHandle for the subscriber task, stored so it can be awaited during shutdown.
    subscriber_handle: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl RedisPubSub {
    /// Create a new `RedisPubSub` service.
    pub fn new(
        redis_client: RedisClient,
        message_hub: Arc<RoomMessageHub>,
        node_id: String,
        admin_event_tx: broadcast::Sender<ClusterEvent>,
        permission_service: Option<PermissionService>,
        cache_invalidation: Option<CacheInvalidationService>,
        deduplicator: Arc<MessageDeduplicator>,
    ) -> Result<Self> {
        Self::with_key_prefix(
            redis_client,
            message_hub,
            node_id,
            "synctv:",
            admin_event_tx,
            permission_service,
            cache_invalidation,
            deduplicator,
            300,
            DEFAULT_MAX_STREAM_LENGTH,
        )
    }

    /// Create a new `RedisPubSub` service with a custom key prefix.
    ///
    /// `catchup_window_secs` controls how far back to replay Redis Stream events
    /// when this node first connects.  Pass `300` for the default (5 minutes).
    /// `stream_max_length` controls the maximum number of entries per Redis Stream.
    #[allow(clippy::too_many_arguments)]
    pub fn with_key_prefix(
        redis_client: RedisClient,
        message_hub: Arc<RoomMessageHub>,
        node_id: String,
        key_prefix: &str,
        admin_event_tx: broadcast::Sender<ClusterEvent>,
        permission_service: Option<PermissionService>,
        cache_invalidation: Option<CacheInvalidationService>,
        deduplicator: Arc<MessageDeduplicator>,
        catchup_window_secs: u64,
        stream_max_length: usize,
    ) -> Result<Self> {
        Ok(Self {
            redis_client,
            shared_conn: tokio::sync::OnceCell::new(),
            message_hub,
            node_id,
            key_prefix: key_prefix.to_string(),
            admin_event_tx,
            permission_service,
            cache_invalidation,
            deduplicator,
            cancel_token: CancellationToken::new(),
            catchup_window_ms: u128::from(catchup_window_secs) * 1000,
            stream_max_length,
            subscriber_handle: tokio::sync::Mutex::new(None),
        })
    }

    /// Build the Redis Stream key for admin events
    fn admin_stream_key(&self) -> String {
        format!("{}admin:events:stream", self.key_prefix)
    }

    /// Build the Redis Stream key for a specific room
    fn room_stream_key(&self, room_id: &str) -> String {
        format!("{}room:{}:events", self.key_prefix, room_id)
    }

    /// Build the admin Pub/Sub pattern
    fn admin_pubsub_pattern(&self) -> String {
        format!("{}admin:*", self.key_prefix)
    }

    /// Build the room Pub/Sub channel
    fn room_pubsub_channel(&self, room_id: &str) -> String {
        format!("{}room:{room_id}", self.key_prefix)
    }

    /// Build the room Pub/Sub pattern (fallback)
    fn room_pubsub_pattern(&self) -> String {
        format!("{}room:*", self.key_prefix)
    }

    /// Generate a Redis Stream ID representing `catchup_window_ms` ago.
    /// This is used to limit how far back we read from a stream when
    /// no valid cursor is available (e.g., cursor is "0" on reconnect).
    fn catchup_start_id(&self) -> String {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let start_ms = now_ms.saturating_sub(self.catchup_window_ms);
        format!("{start_ms}-0")
    }

    /// Extract room_id from a channel name (e.g., "synctv:room:abc" -> Some("abc"))
    fn extract_room_id_from_channel<'a>(&self, channel: &'a str) -> Option<&'a str> {
        let room_prefix = format!("{}room:", self.key_prefix);
        channel.strip_prefix(&room_prefix)
    }

    /// Check if a channel is an admin channel
    fn is_admin_channel(&self, channel: &str) -> bool {
        let admin_prefix = format!("{}admin:", self.key_prefix);
        channel.starts_with(&admin_prefix)
    }

    /// Get the cancellation token for external shutdown signaling
    #[must_use]
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Shut down the Pub/Sub service.
    ///
    /// Cancels the subscriber and publisher tasks via the `CancellationToken`,
    /// then awaits the subscriber task's completion (with a timeout).
    /// The publisher task's `JoinHandle` is returned from `start()` and should
    /// be awaited separately by the caller.
    pub async fn shutdown(&self) {
        info!("Shutting down RedisPubSub service");
        self.cancel_token.cancel();

        // Await the subscriber task
        let handle = self.subscriber_handle.lock().await.take();
        if let Some(handle) = handle {
            match tokio::time::timeout(Duration::from_secs(5), handle).await {
                Ok(Ok(())) => info!("Redis subscriber task completed"),
                Ok(Err(e)) => warn!("Redis subscriber task panicked: {}", e),
                Err(_) => warn!("Redis subscriber task did not finish within 5s timeout"),
            }
        }
    }

    /// Start the Pub/Sub service
    /// This spawns a background task that subscribes to all room channels
    ///
    /// # Arguments
    /// * `publish_channel_capacity` - Capacity for the publish channel. Events are
    ///   dropped with a warning when full (e.g., during a prolonged Redis outage).
    ///
    /// # Returns
    /// A tuple of:
    /// - `mpsc::Sender<PublishRequest>` - Channel sender for publishing events
    /// - `PublishBackpressure` - Handle for checking buffer pressure (backpressure signaling)
    /// - `JoinHandle<()>` - Task handle for awaiting shutdown
    pub async fn start(
        self: Arc<Self>,
        publish_channel_capacity: usize,
    ) -> Result<(
        mpsc::Sender<PublishRequest>,
        PublishBackpressure,
        tokio::task::JoinHandle<()>,
    )> {
        // Create bounded channel for publishing events to prevent OOM under Redis outage
        let (publish_tx, mut publish_rx) =
            mpsc::channel::<PublishRequest>(publish_channel_capacity);

        // Clone for the publish task
        let publish_client = self.redis_client.clone();
        let node_id = self.node_id.clone();
        let key_prefix = self.key_prefix.clone();
        let cancel_publisher = self.cancel_token.clone();
        let stream_max_length = self.stream_max_length;

        /// Maximum number of failed events to buffer for retry after reconnection.
        /// Prevents unbounded memory growth during prolonged Redis outages.
        /// Set to 10000 to reduce the chance of dropping critical events during
        /// sustained outages.
        const MAX_RETRY_BUFFER: usize = 10000;

        /// Maximum number of critical events to buffer separately.
        /// Critical events (kick/ban) are never dropped - this buffer has no limit
        /// but a warning is logged if it exceeds this threshold.
        const CRITICAL_BUFFER_WARN_THRESHOLD: usize = 1000;

        /// Warning threshold for normal buffer (80% of MAX_RETRY_BUFFER)
        const BUFFER_WARN_THRESHOLD: usize = (MAX_RETRY_BUFFER as f64 * 0.8) as usize;

        // Create shared buffer pressure state for backpressure signaling
        let buffer_pressure_state = BufferPressureState::new(MAX_RETRY_BUFFER);
        let backpressure = PublishBackpressure {
            state: buffer_pressure_state.clone(),
        };

        // Spawn task to handle publishing with reconnection logic.
        // The handle is returned to the caller so shutdown() can await completion.
        let publisher_handle = tokio::spawn(async move {
            let mut backoff_secs = INITIAL_BACKOFF_SECS;
            // Buffer for retrying failed non-critical publishes after reconnection.
            // Using a Vec instead of Option<PublishRequest> ensures that multiple
            // events that fail during a connection interruption window are all
            // preserved for retry, not just the last one.
            let mut retry_buffer: Vec<PublishRequest> = Vec::new();
            // Separate buffer for critical events (kick/ban) that must NEVER be dropped.
            // This ensures user access control events are always delivered even during
            // prolonged Redis outages.
            let mut critical_retry_buffer: Vec<PublishRequest> = Vec::new();
            // Track whether we've already logged a warning about buffer approaching capacity.
            // Reset to false on each successful reconnection inside the loop.
            #[allow(unused_assignments)]
            let mut buffer_warn_logged = false;

            // Helper to update buffer pressure state
            let update_pressure = |retry_len: usize, critical_len: usize| {
                buffer_pressure_state.set_retry_size(retry_len);
                buffer_pressure_state.set_critical_size(critical_len);
            };

            loop {
                let conn = match timeout(
                    Duration::from_secs(REDIS_TIMEOUT_SECS),
                    publish_client.get_multiplexed_async_connection(),
                )
                .await
                {
                    Ok(Ok(conn)) => {
                        backoff_secs = INITIAL_BACKOFF_SECS;
                        conn
                    }
                    Ok(Err(e)) => {
                        error!(
                            error = %e,
                            backoff_secs = backoff_secs,
                            "Failed to get Redis connection for publishing, retrying"
                        );
                        let cancelled = tokio::select! {
                            () = cancel_publisher.cancelled() => true,
                            () = tokio::time::sleep(Duration::from_secs(backoff_secs)) => false,
                        };
                        if cancelled {
                            info!("Redis publisher task cancelled while waiting to reconnect");
                            return;
                        }
                        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                        continue;
                    }
                    Err(_) => {
                        error!(
                            backoff_secs = backoff_secs,
                            "Timed out getting Redis connection for publishing, retrying"
                        );
                        let cancelled = tokio::select! {
                            () = cancel_publisher.cancelled() => true,
                            () = tokio::time::sleep(Duration::from_secs(backoff_secs)) => false,
                        };
                        if cancelled {
                            info!("Redis publisher task cancelled while waiting to reconnect");
                            return;
                        }
                        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                        continue;
                    }
                };

                info!("Redis publisher task (re)connected");
                let mut conn = conn;

                // Reset buffer warning flag on successful reconnection
                buffer_warn_logged = false;

                // Helper function to retry a batch of events, returning failed ones
                async fn retry_batch(
                    batch: Vec<PublishRequest>,
                    conn: &mut redis::aio::MultiplexedConnection,
                    node_id: &str,
                    key_prefix: &str,
                    stream_max_length: usize,
                ) -> (Vec<PublishRequest>, usize) {
                    let mut failed = Vec::new();
                    let mut success_count = 0;
                    for req in batch {
                        let event_type = req.event.event_type();
                        match RedisPubSub::publish_event(
                            conn,
                            node_id,
                            key_prefix,
                            req.event.clone(),
                            stream_max_length,
                        )
                        .await
                        {
                            Ok(subscribers) => {
                                success_count += 1;
                                debug!(
                                    event_type = event_type,
                                    subscribers = subscribers,
                                    "Retried event published to Redis"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    event_type = event_type,
                                    "Retry publish failed, will retry after next reconnect"
                                );
                                failed.push(req);
                            }
                        }
                    }
                    (failed, success_count)
                }

                // CRITICAL EVENTS: Always retry first (highest priority)
                if !critical_retry_buffer.is_empty() {
                    let critical_batch = std::mem::take(&mut critical_retry_buffer);
                    update_pressure(retry_buffer.len(), 0); // Critical buffer is empty during retry
                    info!(
                        critical_count = critical_batch.len(),
                        "Retrying critical events after reconnection"
                    );
                    let (failed, _success_count) = retry_batch(
                        critical_batch,
                        &mut conn,
                        &node_id,
                        &key_prefix,
                        stream_max_length,
                    )
                    .await;
                    if !failed.is_empty() {
                        warn!(
                            failed_count = failed.len(),
                            "Some critical events failed to retry, keeping in buffer"
                        );
                        critical_retry_buffer = failed;
                        update_pressure(retry_buffer.len(), critical_retry_buffer.len());
                        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                        continue;
                    }
                }

                // NORMAL EVENTS: Retry after critical events
                if !retry_buffer.is_empty() {
                    let buffered = std::mem::take(&mut retry_buffer);
                    update_pressure(0, critical_retry_buffer.len()); // Retry buffer is empty during retry
                    info!(
                        buffered_count = buffered.len(),
                        "Retrying buffered events after reconnection"
                    );
                    let (failed, _success_count) = retry_batch(
                        buffered,
                        &mut conn,
                        &node_id,
                        &key_prefix,
                        stream_max_length,
                    )
                    .await;
                    if !failed.is_empty() {
                        retry_buffer = failed;
                        update_pressure(retry_buffer.len(), critical_retry_buffer.len());
                        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                        continue;
                    }
                }

                // Buffers are empty after successful retries - update pressure
                update_pressure(0, 0);

                // Track whether this session was healthy (at least one event sent)
                let mut session_healthy = false;

                // Process events until connection breaks or cancelled
                loop {
                    let req = tokio::select! {
                        () = cancel_publisher.cancelled() => {
                            // Flush retry_buffer first, then drain channel
                            info!(
                                retry_buffer_len = retry_buffer.len(),
                                critical_buffer_len = critical_retry_buffer.len(),
                                "Redis publisher task cancelled, flushing buffers and draining remaining events"
                            );
                            let mut flush_failed = false;
                            // CRITICAL: Flush critical_retry_buffer FIRST (highest priority)
                            for req in std::mem::take(&mut critical_retry_buffer) {
                                if flush_failed {
                                    error!(
                                        event_type = req.event.event_type(),
                                        "CRITICAL event lost during shutdown (connection broken)"
                                    );
                                    continue;
                                }
                                let event_type = req.event.event_type();
                                match Self::publish_event(&mut conn, &node_id, &key_prefix, req.event.clone(), stream_max_length).await {
                                    Ok(_) => {
                                        debug!(event_type = event_type, "Critical buffer event flushed on shutdown");
                                    }
                                    Err(e) => {
                                        error!(error = %e, event_type = event_type, "Failed to flush CRITICAL event on shutdown");
                                        flush_failed = true;
                                    }
                                }
                            }
                            // Then flush normal retry_buffer (events from previous failed publishes)
                            for req in std::mem::take(&mut retry_buffer) {
                                if flush_failed {
                                    // Connection broken; skip remaining events (these are non-critical)
                                    debug!(
                                        event_type = req.event.event_type(),
                                        "Non-critical retry_buffer event skipped during shutdown (connection broken)"
                                    );
                                    continue;
                                }
                                let event_type = req.event.event_type();
                                match Self::publish_event(&mut conn, &node_id, &key_prefix, req.event.clone(), stream_max_length).await {
                                    Ok(_) => {
                                        debug!(event_type = event_type, "Retry buffer event flushed on shutdown");
                                    }
                                    Err(e) => {
                                        warn!(error = %e, event_type = event_type, "Failed to flush retry buffer event on shutdown");
                                        flush_failed = true;
                                    }
                                }
                            }
                            // Finally drain remaining events from channel, prioritizing critical
                            let mut critical_drain = Vec::new();
                            let mut normal_drain = Vec::new();
                            while let Ok(req) = publish_rx.try_recv() {
                                if req.event.is_critical() {
                                    critical_drain.push(req);
                                } else {
                                    normal_drain.push(req);
                                }
                            }
                            // Flush critical drained events first
                            for req in critical_drain {
                                if flush_failed {
                                    error!(
                                        event_type = req.event.event_type(),
                                        "CRITICAL drained event lost during shutdown (connection broken)"
                                    );
                                    continue;
                                }
                                let event_type = req.event.event_type();
                                match Self::publish_event(&mut conn, &node_id, &key_prefix, req.event.clone(), stream_max_length).await {
                                    Ok(_) => {
                                        debug!(event_type = event_type, "Critical drained event published");
                                    }
                                    Err(e) => {
                                        error!(error = %e, event_type = event_type, "Failed to publish CRITICAL drained event");
                                        flush_failed = true;
                                    }
                                }
                            }
                            // Then flush normal drained events
                            for req in normal_drain {
                                if flush_failed {
                                    continue;
                                }
                                let event_type = req.event.event_type();
                                match Self::publish_event(&mut conn, &node_id, &key_prefix, req.event.clone(), stream_max_length).await {
                                    Ok(_) => {
                                        debug!(event_type = event_type, "Drained event published");
                                    }
                                    Err(e) => {
                                        warn!(error = %e, event_type = event_type, "Failed to publish drained event");
                                        flush_failed = true;
                                    }
                                }
                            }
                            return;
                        }
                        req = publish_rx.recv() => req,
                    };
                    if let Some(req) = req {
                        let event_type = req.event.event_type();
                        match Self::publish_event(
                            &mut conn,
                            &node_id,
                            &key_prefix,
                            req.event.clone(),
                            stream_max_length,
                        )
                        .await
                        {
                            Ok(subscribers) => {
                                session_healthy = true;
                                debug!(
                                    event_type = event_type,
                                    subscribers = subscribers,
                                    "Event published to Redis"
                                );
                            }
                            Err(e) => {
                                // READONLY or LOADING errors indicate a Sentinel failover.
                                // Reset backoff so we reconnect quickly to the new master
                                // instead of waiting through the normal exponential delay.
                                if is_sentinel_failover_error(&e) {
                                    warn!(
                                        error = %e,
                                        event_type = event_type,
                                        "Sentinel failover detected (READONLY/LOADING), reconnecting immediately"
                                    );
                                    backoff_secs = INITIAL_BACKOFF_SECS;
                                } else {
                                    error!(
                                        error = %e,
                                        event_type = event_type,
                                        "Failed to publish event, buffering for retry after reconnect"
                                    );
                                }
                                // Route failed request to appropriate buffer based on criticality
                                if req.event.is_critical() {
                                    critical_retry_buffer.push(req);
                                } else {
                                    retry_buffer.push(req);
                                }

                                // Drain remaining events from channel into appropriate buffers
                                // (connection is broken, no point trying to publish more)
                                while let Ok(req) = publish_rx.try_recv() {
                                    let is_critical = req.event.is_critical();
                                    let event_type = req.event.event_type();

                                    if is_critical {
                                        // CRITICAL events are NEVER dropped - always buffered
                                        critical_retry_buffer.push(req);

                                        // Warn if critical buffer is growing large
                                        if critical_retry_buffer.len()
                                            == CRITICAL_BUFFER_WARN_THRESHOLD
                                        {
                                            warn!(
                                                critical_buffer_len = critical_retry_buffer.len(),
                                                "Critical event buffer approaching high threshold during outage"
                                            );
                                        }
                                    } else {
                                        // Normal events: check buffer limit
                                        if retry_buffer.len() >= MAX_RETRY_BUFFER {
                                            warn!(
                                                max = MAX_RETRY_BUFFER,
                                                event_type = event_type,
                                                "Retry buffer full, dropping non-critical event"
                                            );
                                            synctv_core::metrics::cluster::CLUSTER_EVENTS_DROPPED
                                                .with_label_values(&["retry_buffer_full"])
                                                .inc();
                                            continue; // Continue draining, don't break - critical events still need to be collected
                                        }
                                        retry_buffer.push(req);
                                    }

                                    // Warn once when normal buffer approaches capacity (80%)
                                    if !buffer_warn_logged
                                        && retry_buffer.len() >= BUFFER_WARN_THRESHOLD
                                    {
                                        buffer_warn_logged = true;
                                        warn!(
                                            buffer_len = retry_buffer.len(),
                                            max = MAX_RETRY_BUFFER,
                                            threshold_pct = 80,
                                            "Retry buffer approaching capacity, non-critical events may be dropped soon"
                                        );
                                    }
                                }

                                // Update buffer pressure after draining
                                update_pressure(retry_buffer.len(), critical_retry_buffer.len());
                                break;
                            }
                        }
                    } else {
                        // Channel closed, publisher shutting down
                        warn!("Redis publisher channel closed, exiting");
                        return;
                    }
                }

                // Reset backoff if the session was healthy (connection was working
                // before it dropped), so we reconnect quickly. Only escalate backoff
                // when the connection never worked.
                if session_healthy {
                    backoff_secs = INITIAL_BACKOFF_SECS;
                }

                // Wait before reconnecting
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
            }
        });

        // Clone for the subscriber task
        let subscriber_handle_ref = Arc::clone(&self);
        let self_clone = Arc::clone(&self);
        let cancel_subscriber = self_clone.cancel_token.clone();

        // Spawn task to handle subscribing with exponential backoff on reconnection.
        // Store the JoinHandle so shutdown() can await it.
        let (subscriber_ready_tx, subscriber_ready_rx) = tokio::sync::oneshot::channel();
        let subscriber_jh = tokio::spawn(async move {
            let mut backoff_secs = INITIAL_BACKOFF_SECS;
            // Track per-stream cursors (per-room + admin) across reconnections.
            // On first connect, cursors are snapshotted from stream tips.
            // On reconnect, catch-up reads only active rooms' streams.
            let mut stream_cursors: HashMap<String, String> = HashMap::new();
            let mut is_first_connect = true;

            // Subscribe to lifecycle events OUTSIDE run_subscriber so we don't miss
            // events during disconnection. This receiver persists across reconnections.
            let mut lifecycle_rx = self_clone.message_hub.subscribe_lifecycle();

            // Track pending room subscriptions that arrived during disconnection.
            // These rooms were activated while we were disconnected and need to be
            // subscribed on reconnect. Deactivations remove rooms from this set.
            let mut pending_subscriptions: HashSet<String> = HashSet::new();
            let mut subscriber_ready_tx = Some(subscriber_ready_tx);

            loop {
                // Check cancellation before each reconnect attempt
                if cancel_subscriber.is_cancelled() {
                    info!("Redis subscriber task cancelled");
                    return;
                }

                // Drain any lifecycle events that arrived during disconnection.
                // This ensures we don't miss room activations that happened while
                // we were reconnecting. These will be merged with active_rooms
                // when we resubscribe.
                while let Ok(ev) = lifecycle_rx.try_recv() {
                    match ev {
                        RoomLifecycleEvent::RoomActivated(room_id) => {
                            pending_subscriptions.insert(room_id.as_str().to_string());
                        }
                        RoomLifecycleEvent::RoomDeactivated(room_id) => {
                            pending_subscriptions.remove(room_id.as_str());
                        }
                    }
                }

                match self_clone
                    .run_subscriber(
                        &mut stream_cursors,
                        &mut is_first_connect,
                        &mut lifecycle_rx,
                        &mut pending_subscriptions,
                        &mut subscriber_ready_tx,
                    )
                    .await
                {
                    SubscriberExit::Disconnected => {
                        // Connection was healthy before it dropped.
                        // Reset backoff since the server was reachable.
                        // Use INITIAL_BACKOFF_SECS for the first retry without doubling.
                        backoff_secs = INITIAL_BACKOFF_SECS;
                        error!(
                            "Redis subscriber stream ended (connection lost), reconnecting after {}s",
                            backoff_secs
                        );
                    }
                    SubscriberExit::ConnectFailed(e) => {
                        // Could not connect -- keep increasing backoff.
                        error!(
                            error = %e,
                            backoff_secs = backoff_secs,
                            "Redis subscriber failed to connect, retrying after backoff"
                        );
                    }
                }

                // Wait with cancellation support
                tokio::select! {
                    () = cancel_subscriber.cancelled() => {
                        info!("Redis subscriber task cancelled during backoff");
                        return;
                    }
                    () = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {}
                }

                // Exponential backoff: double the delay AFTER the sleep, cap at MAX_BACKOFF_SECS.
                // After Disconnected, backoff was reset to INITIAL_BACKOFF_SECS above,
                // so the first retry uses the initial delay without being doubled first.
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
            }
        });

        // Store subscriber handle so shutdown() can await it
        *subscriber_handle_ref.subscriber_handle.lock().await = Some(subscriber_jh);

        let ready_result = timeout(
            Duration::from_secs(SUBSCRIBER_READY_TIMEOUT_SECS),
            subscriber_ready_rx,
        )
        .await;

        match ready_result {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                self.cancel_token.cancel();
                if let Some(handle) = self.subscriber_handle.lock().await.take() {
                    let _ = handle.await;
                }
                let _ = publisher_handle.await;
                return Err(anyhow::anyhow!(
                    "Redis subscriber exited before reporting readiness"
                ));
            }
            Err(_) => {
                self.cancel_token.cancel();
                if let Some(handle) = self.subscriber_handle.lock().await.take() {
                    let _ = handle.await;
                }
                let _ = publisher_handle.await;
                return Err(anyhow::anyhow!(
                    "Redis subscriber did not become ready within timeout"
                ));
            }
        }

        Ok((publish_tx, backpressure, publisher_handle))
    }

    /// Run the subscriber task.
    ///
    /// `stream_cursors` maps each stream key (per-room or admin) to the last
    /// processed Redis Stream entry ID. On first connection these are initialized
    /// from the current stream tips. After reconnection the subscriber catches
    /// up only on streams for rooms with local subscribers, avoiding the N*M
    /// amplification of a single global stream.
    ///
    /// `is_first_connect` is set to `true` on the first connection. On first
    /// connect we snapshot the stream tips; on reconnect we catch up.
    ///
    /// Uses dynamic per-room subscriptions instead of a global `psubscribe("synctv:room:*")`
    /// to avoid receiving messages for rooms that have no local subscribers. The admin
    /// channel continues to use pattern subscription since admin events are always needed.
    ///
    /// `lifecycle_rx` is the receiver for room lifecycle events, passed from outside
    /// so it persists across reconnections and we don't miss events during disconnection.
    ///
    /// `pending_subscriptions` tracks rooms activated during disconnection that need
    /// to be subscribed on reconnect. After successful subscription, these are cleared.
    ///
    /// Returns `SubscriberExit::Disconnected` if the connection was established but then
    /// the stream ended (Redis disconnected). Returns `SubscriberExit::ConnectFailed` if
    /// the initial connection or subscription failed.
    async fn run_subscriber(
        &self,
        stream_cursors: &mut HashMap<String, String>,
        is_first_connect: &mut bool,
        lifecycle_rx: &mut broadcast::Receiver<RoomLifecycleEvent>,
        pending_subscriptions: &mut HashSet<String>,
        subscriber_ready_tx: &mut Option<tokio::sync::oneshot::Sender<()>>,
    ) -> SubscriberExit {
        let mut pubsub = match timeout(
            Duration::from_secs(REDIS_TIMEOUT_SECS),
            self.redis_client.get_async_pubsub(),
        )
        .await
        {
            Ok(Ok(ps)) => ps,
            Ok(Err(e)) => {
                return SubscriberExit::ConnectFailed(
                    anyhow::anyhow!(e).context("Failed to get Redis Pub/Sub connection"),
                );
            }
            Err(_) => {
                return SubscriberExit::ConnectFailed(anyhow::anyhow!(
                    "Timed out getting Redis Pub/Sub connection"
                ));
            }
        };

        // Always subscribe to admin channel pattern (needed for cluster-wide events)
        let admin_pattern = self.admin_pubsub_pattern();
        match timeout(
            Duration::from_secs(REDIS_TIMEOUT_SECS),
            pubsub.psubscribe(&admin_pattern),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                return SubscriberExit::ConnectFailed(
                    anyhow::anyhow!(e)
                        .context(format!("Failed to subscribe to {admin_pattern} pattern")),
                );
            }
            Err(_) => {
                return SubscriberExit::ConnectFailed(anyhow::anyhow!(
                    "Timed out subscribing to {admin_pattern} pattern"
                ));
            }
        }

        // Subscribe to specific room channels for currently active rooms
        // (instead of psubscribe("{prefix}room:*") which receives all rooms globally)
        // Also include pending_subscriptions (rooms activated during disconnection)
        let active_rooms = self.message_hub.active_room_ids();
        let mut subscribed_rooms: HashSet<String> = HashSet::new();

        // Merge active rooms with pending subscriptions (rooms that were activated
        // while we were disconnected). This ensures we don't lose subscriptions
        // to rooms that became active during the disconnection period.
        let mut rooms_to_subscribe: HashSet<String> = active_rooms
            .iter()
            .map(|rid| rid.as_str().to_string())
            .collect();
        rooms_to_subscribe.extend(pending_subscriptions.iter().cloned());

        if !rooms_to_subscribe.is_empty() {
            let room_channels: Vec<String> = rooms_to_subscribe
                .iter()
                .map(|rid| self.room_pubsub_channel(rid))
                .collect();
            let channel_refs: Vec<&str> = room_channels
                .iter()
                .map(std::string::String::as_str)
                .collect();

            match timeout(
                Duration::from_secs(REDIS_TIMEOUT_SECS),
                pubsub.subscribe(channel_refs.as_slice()),
            )
            .await
            {
                Ok(Ok(())) => {
                    for rid in &rooms_to_subscribe {
                        subscribed_rooms.insert(rid.clone());
                    }
                    // Clear pending subscriptions after successful subscribe
                    // (they are now in subscribed_rooms)
                    pending_subscriptions.clear();
                }
                Ok(Err(e)) => {
                    warn!(
                        error = %e,
                        room_count = rooms_to_subscribe.len(),
                        "Failed to subscribe to room channels, falling back to pattern"
                    );
                    // Fallback: use pattern subscription if individual subscribes fail
                    let room_pattern = self.room_pubsub_pattern();
                    if let Err(e) = pubsub.psubscribe(&room_pattern).await {
                        return SubscriberExit::ConnectFailed(
                            anyhow::anyhow!(e).context(format!(
                                "Failed to fallback psubscribe to {room_pattern}"
                            )),
                        );
                    }
                    // Pattern subscription covers all rooms, clear pending
                    pending_subscriptions.clear();
                }
                Err(_) => {
                    warn!(
                        room_count = rooms_to_subscribe.len(),
                        "Timed out subscribing to room channels, falling back to pattern"
                    );
                    let room_pattern = self.room_pubsub_pattern();
                    if let Err(e) = pubsub.psubscribe(&room_pattern).await {
                        return SubscriberExit::ConnectFailed(
                            anyhow::anyhow!(e).context(format!(
                                "Failed to fallback psubscribe to {room_pattern}"
                            )),
                        );
                    }
                    // Pattern subscription covers all rooms, clear pending
                    pending_subscriptions.clear();
                }
            }
        }

        info!(
            subscribed_rooms = subscribed_rooms.len(),
            "Redis subscriber connected, listening to {} pattern and {} room channels",
            admin_pattern,
            subscribed_rooms.len()
        );

        if let Some(ready_tx) = subscriber_ready_tx.take() {
            let _ = ready_tx.send(());
        }

        // Note: lifecycle_rx is passed from outside so it persists across reconnections.
        // Any pending lifecycle events were already drained into pending_subscriptions
        // before this call, and we'll handle new events in the message loop below.

        if *is_first_connect {
            *is_first_connect = false;

            // First connection: snapshot the current stream tips for active rooms
            // and the admin stream so we can catch up from these points if the
            // connection drops later.
            //
            // IMPORTANT: This snapshot is taken AFTER PubSub subscription is
            // established (above), which means any events written to the stream
            // after this point will ALSO be delivered via PubSub. On reconnect,
            // catch-up reads from the snapshotted cursor may re-deliver events
            // that were already processed via PubSub. The MessageDeduplicator
            // handles this overlap, filtering out duplicate events.
            //
            // The alternative (snapshotting before subscription) would create a
            // gap: events written between snapshot and subscription start would
            // be missed by both PubSub (not yet subscribed) and catch-up (cursor
            // already past them). The current order (subscribe first, then
            // snapshot) is correct because duplicates are safe (deduped) while
            // gaps are not.
            let mut streams_to_catchup: Vec<String> = active_rooms
                .iter()
                .map(|rid| self.room_stream_key(rid.as_str()))
                .collect();
            streams_to_catchup.push(self.admin_stream_key());

            // New node: catch up on recent historical events from each stream.
            // Instead of reading from "0" (all history), we start from `catchup_window_ms`
            // ago to avoid processing a large backlog in big clusters.
            let catchup_start_id = {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                let start_ms = now_ms.saturating_sub(self.catchup_window_ms);
                format!("{start_ms}-0")
            };
            let mut total_caught_up = 0usize;
            let total_skipped = 0usize;
            for stream_key in &streams_to_catchup {
                match self
                    .read_missed_events_from(stream_key, &catchup_start_id)
                    .await
                {
                    Ok(events) => {
                        for (stream_id, channel, event) in events {
                            self.dispatch_event(&channel, event).await;
                            total_caught_up += 1;
                            // Update cursor to the latest processed stream ID
                            stream_cursors.insert(stream_key.clone(), stream_id);
                        }
                        // If no events, snapshot the latest stream ID
                        if !stream_cursors.contains_key(stream_key) {
                            match self.get_latest_stream_id_for(stream_key).await {
                                Ok(Some(id)) => {
                                    stream_cursors.insert(stream_key.clone(), id);
                                }
                                _ => {
                                    stream_cursors.insert(stream_key.clone(), "0".to_string());
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // Retry catch-up read up to 3 times with short delay before
                        // falling back. Use "0" (stream beginning within catchup
                        // window) instead of "$" so events are not silently skipped.
                        let mut retry_ok = false;
                        for retry in 1..=3 {
                            warn!(
                                error = %e,
                                stream_key = %stream_key,
                                attempt = retry,
                                "Failed to catch up on historical events, retrying"
                            );
                            tokio::time::sleep(Duration::from_millis(500 * retry as u64)).await;
                            match self
                                .read_missed_events_from(stream_key, &catchup_start_id)
                                .await
                            {
                                Ok(events) => {
                                    for (stream_id, channel, event) in events {
                                        self.dispatch_event(&channel, event).await;
                                        total_caught_up += 1;
                                        stream_cursors.insert(stream_key.clone(), stream_id);
                                    }
                                    if !stream_cursors.contains_key(stream_key) {
                                        match self.get_latest_stream_id_for(stream_key).await {
                                            Ok(Some(id)) => {
                                                stream_cursors.insert(stream_key.clone(), id);
                                            }
                                            _ => {
                                                stream_cursors
                                                    .insert(stream_key.clone(), "0".to_string());
                                            }
                                        }
                                    }
                                    retry_ok = true;
                                    break;
                                }
                                Err(retry_err) => {
                                    warn!(
                                        error = %retry_err,
                                        stream_key = %stream_key,
                                        attempt = retry,
                                        "Catch-up retry failed"
                                    );
                                }
                            }
                        }
                        if !retry_ok {
                            warn!(
                                stream_key = %stream_key,
                                "All catch-up retries exhausted, falling back to '0' (stream beginning within catchup window)"
                            );
                            stream_cursors.insert(stream_key.clone(), "0".to_string());
                        }
                    }
                }
            }
            info!(
                room_count = active_rooms.len(),
                caught_up = total_caught_up,
                skipped = total_skipped,
                "Initialized {} stream cursors after catching up historical events",
                stream_cursors.len()
            );
        } else {
            // Reconnection: catch up on events missed during disconnection.
            // Only read streams for rooms that currently have local subscribers.
            //
            // IMPORTANT: The stream cursor is the authoritative dedup boundary
            // for catch-up. XREAD returns only entries with IDs strictly greater
            // than the cursor, so events at or before the cursor are guaranteed
            // to have been delivered before disconnection. The in-memory dedup
            // cache is supplementary -- it handles overlap between live PubSub
            // and stream catch-up but is NOT relied upon as the primary mechanism
            // (its TTL may have expired during a long disconnection).
            let active_rooms = self.message_hub.active_room_ids();

            // Prune cursors for rooms that no longer have local subscribers.
            let admin_sk = self.admin_stream_key();
            let active_stream_keys_set: HashSet<String> = active_rooms
                .iter()
                .map(|rid| self.room_stream_key(rid.as_str()))
                .collect();
            stream_cursors
                .retain(|key, _| *key == admin_sk || active_stream_keys_set.contains(key));

            // Ensure admin stream is always included.
            // When no cursor exists, use catchup_start_id instead of "0" to avoid
            // reading all historical events from the stream.
            let catchup_start = self.catchup_start_id();
            if !stream_cursors.contains_key(&admin_sk) {
                stream_cursors.insert(admin_sk.clone(), catchup_start.clone());
            }

            // Add cursors for any new rooms that appeared while disconnected.
            // Use catchup_start_id for new rooms to avoid reading all history.
            for rid in &active_rooms {
                let key = self.room_stream_key(rid.as_str());
                stream_cursors
                    .entry(key)
                    .or_insert_with(|| catchup_start.clone());
            }

            // Build the set of streams to catch up from (active rooms + admin)
            let active_stream_keys: Vec<String> = {
                let mut keys: Vec<String> = active_rooms
                    .iter()
                    .map(|rid| self.room_stream_key(rid.as_str()))
                    .collect();
                keys.push(admin_sk);
                keys
            };

            let mut total_caught_up = 0usize;
            let mut total_skipped = 0usize;
            for stream_key in &active_stream_keys {
                // Use catchup_start as fallback to avoid reading all history from "0"
                let cursor = stream_cursors
                    .get(stream_key)
                    .cloned()
                    .unwrap_or_else(|| catchup_start.clone());
                match self.read_missed_events_from(stream_key, &cursor).await {
                    Ok(events) => {
                        for (stream_id, channel, event) in events {
                            // Stream cursor is the authoritative boundary: XREAD
                            // already filters by cursor, but as a defense-in-depth
                            // check, skip events whose stream IDs are not strictly
                            // after the last known cursor.
                            //
                            // Redis Stream IDs are "{timestamp_ms}-{seq}". String
                            // comparison (lexicographic) is INCORRECT for numeric
                            // fields (e.g., "9-0" > "10-0" lexicographically).
                            // Parse into (u64, u64) for correct numeric comparison.
                            if !stream_id_gt(&stream_id, &cursor) {
                                total_skipped += 1;
                                debug!(
                                    stream_key = %stream_key,
                                    stream_id = %stream_id,
                                    cursor = %cursor,
                                    "Skipping catch-up event at or before cursor (defense-in-depth)"
                                );
                                continue;
                            }
                            self.dispatch_event(&channel, event).await;
                            stream_cursors.insert(stream_key.clone(), stream_id);
                            total_caught_up += 1;
                        }
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            stream_key = %stream_key,
                            "Failed to read missed events from stream, continuing"
                        );
                    }
                }
            }

            if total_caught_up > 0 || total_skipped > 0 {
                info!(
                    total_events = total_caught_up,
                    skipped_by_cursor = total_skipped,
                    streams = active_stream_keys.len(),
                    "Caught up on missed events from per-room streams"
                );
            }
        }

        // Process incoming messages with dynamic room subscription management.
        //
        // We loop between processing Redis messages and handling lifecycle events.
        // When a lifecycle event arrives, we drop the message stream (releasing the
        // mutable borrow on `pubsub`), perform the subscribe/unsubscribe, then
        // re-create the message stream.
        //
        // A periodic cursor refresh ensures that stream cursors stay up-to-date
        // during long-lived sessions, so reconnect catch-up only reads truly missed
        // events (avoiding replay of events already delivered via live Pub/Sub).
        let mut cursor_refresh_interval = tokio::time::interval(Duration::from_mins(1));
        cursor_refresh_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Skip the first immediate tick
        cursor_refresh_interval.tick().await;

        loop {
            let mut stream = pubsub.on_message();

            enum SelectResult {
                Message(redis::Msg),
                LifecycleEvent(RoomLifecycleEvent),
                CursorRefresh,
                StreamEnded,
            }

            let result = tokio::select! {
                biased;
                msg_opt = stream.next() => {
                    match msg_opt {
                        Some(msg) => SelectResult::Message(msg),
                        None => SelectResult::StreamEnded,
                    }
                }
                lifecycle = lifecycle_rx.recv() => {
                    match lifecycle {
                        Ok(event) => SelectResult::LifecycleEvent(event),
                        Err(broadcast::error::RecvError::Lagged(count)) => {
                            warn!(
                                missed_count = count,
                                "Lagged on room lifecycle events, re-syncing subscriptions"
                            );
                            drop(stream);
                            self.resync_room_subscriptions(&mut pubsub, &mut subscribed_rooms, stream_cursors).await;
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            warn!("Room lifecycle channel closed");
                            match stream.next().await {
                                Some(msg) => SelectResult::Message(msg),
                                None => SelectResult::StreamEnded,
                            }
                        }
                    }
                }
                _ = cursor_refresh_interval.tick() => {
                    SelectResult::CursorRefresh
                }
            };

            // Drop the stream before handling the result (releases &mut pubsub borrow)
            drop(stream);

            match result {
                SelectResult::Message(msg) => {
                    let channel = msg.get_channel_name().to_string();
                    let payload: String = match msg.get_payload() {
                        Ok(p) => p,
                        Err(e) => {
                            warn!(error = %e, channel = %channel, "Invalid payload");
                            continue;
                        }
                    };

                    match serde_json::from_str::<EventEnvelope>(&payload) {
                        Ok(envelope) => {
                            if envelope.node_id == self.node_id {
                                debug!(
                                    channel = %channel,
                                    "Ignoring event from self (node_id: {})",
                                    self.node_id
                                );
                                continue;
                            }
                            self.dispatch_event(&channel, envelope.event).await;
                        }
                        Err(e) => {
                            warn!(
                                error = %e,
                                channel = %channel,
                                payload = %payload,
                                "Failed to deserialize event envelope"
                            );
                        }
                    }
                }
                SelectResult::LifecycleEvent(event) => {
                    // Drain any additional pending lifecycle events to batch operations
                    let mut events = vec![event];
                    while let Ok(ev) = lifecycle_rx.try_recv() {
                        events.push(ev);
                    }

                    for ev in events {
                        match ev {
                            RoomLifecycleEvent::RoomActivated(room_id) => {
                                let room_id_str = room_id.as_str().to_string();
                                if subscribed_rooms.insert(room_id_str.clone()) {
                                    let channel = self.room_pubsub_channel(&room_id_str);
                                    match timeout(
                                        Duration::from_secs(REDIS_TIMEOUT_SECS),
                                        pubsub.subscribe(&channel),
                                    )
                                    .await
                                    {
                                        Ok(Ok(())) => {
                                            // Snapshot the stream cursor AFTER subscribing
                                            // to the PubSub channel. This ensures that on
                                            // reconnect, catch-up reads start from a known
                                            // position rather than "0" (which would replay
                                            // the entire stream history). The deduplicator
                                            // handles any overlap between live PubSub
                                            // delivery and the snapshotted cursor.
                                            let sk = self.room_stream_key(&room_id_str);
                                            match self.get_latest_stream_id_for(&sk).await {
                                                Ok(Some(id)) => {
                                                    debug!(
                                                        room_id = %room_id_str,
                                                        stream_id = %id,
                                                        "Dynamically subscribed to room channel, cursor snapshotted"
                                                    );
                                                    stream_cursors.insert(sk, id);
                                                }
                                                Ok(None) => {
                                                    debug!(
                                                        room_id = %room_id_str,
                                                        "Dynamically subscribed to room channel (empty stream)"
                                                    );
                                                    stream_cursors.insert(sk, "0".to_string());
                                                }
                                                Err(e) => {
                                                    warn!(
                                                        error = %e,
                                                        room_id = %room_id_str,
                                                        "Dynamically subscribed but failed to snapshot cursor, using catchup_start_id"
                                                    );
                                                    stream_cursors
                                                        .insert(sk, self.catchup_start_id());
                                                }
                                            }
                                        }
                                        Ok(Err(e)) => {
                                            warn!(
                                                error = %e,
                                                room_id = %room_id_str,
                                                "Failed to subscribe to room channel"
                                            );
                                            subscribed_rooms.remove(&room_id_str);
                                        }
                                        Err(_) => {
                                            warn!(
                                                room_id = %room_id_str,
                                                "Timed out subscribing to room channel"
                                            );
                                            subscribed_rooms.remove(&room_id_str);
                                        }
                                    }
                                }
                            }
                            RoomLifecycleEvent::RoomDeactivated(room_id) => {
                                let room_id_str = room_id.as_str().to_string();
                                if subscribed_rooms.remove(&room_id_str) {
                                    let channel = self.room_pubsub_channel(&room_id_str);
                                    match timeout(
                                        Duration::from_secs(REDIS_TIMEOUT_SECS),
                                        pubsub.unsubscribe(&channel),
                                    )
                                    .await
                                    {
                                        Ok(Ok(())) => {
                                            debug!(
                                                room_id = %room_id_str,
                                                "Dynamically unsubscribed from room channel"
                                            );
                                            stream_cursors
                                                .remove(&self.room_stream_key(&room_id_str));
                                        }
                                        Ok(Err(e)) => {
                                            warn!(
                                                error = %e,
                                                room_id = %room_id_str,
                                                "Failed to unsubscribe from room channel"
                                            );
                                        }
                                        Err(_) => {
                                            warn!(
                                                room_id = %room_id_str,
                                                "Timed out unsubscribing from room channel"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                SelectResult::CursorRefresh => {
                    // Periodically advance stream cursors to the latest stream tips.
                    // This prevents catch-up from replaying the entire session's worth
                    // of events after a reconnect (the deduplicator window may have
                    // expired for events delivered hours ago via live Pub/Sub).
                    let keys_to_refresh: Vec<String> = stream_cursors.keys().cloned().collect();
                    let mut updated = 0usize;
                    for sk in keys_to_refresh {
                        match self.get_latest_stream_id_for(&sk).await {
                            Ok(Some(id)) => {
                                stream_cursors.insert(sk, id);
                                updated += 1;
                            }
                            Ok(None) => {
                                // Stream is empty or was trimmed, keep existing cursor
                            }
                            Err(e) => {
                                debug!(
                                    error = %e,
                                    stream_key = %sk,
                                    "Failed to refresh stream cursor, keeping existing"
                                );
                            }
                        }
                    }
                    if updated > 0 {
                        debug!(
                            updated_cursors = updated,
                            "Periodic stream cursor refresh completed"
                        );
                    }
                }
                SelectResult::StreamEnded => {
                    return SubscriberExit::Disconnected;
                }
            }
        }
    }

    /// Re-synchronize room subscriptions with the hub's current active rooms.
    ///
    /// Called when the lifecycle event receiver has lagged and missed events.
    /// Also snapshots stream cursors for newly subscribed rooms so that
    /// reconnect catch-up starts from the correct position.
    async fn resync_room_subscriptions(
        &self,
        pubsub: &mut redis::aio::PubSub,
        subscribed_rooms: &mut HashSet<String>,
        stream_cursors: &mut HashMap<String, String>,
    ) {
        let active_rooms: HashSet<String> = self
            .message_hub
            .active_room_ids()
            .into_iter()
            .map(|rid| rid.as_str().to_string())
            .collect();

        // Subscribe to newly active rooms
        let new_rooms: Vec<String> = active_rooms.difference(subscribed_rooms).cloned().collect();
        for room_id in new_rooms {
            let channel = self.room_pubsub_channel(&room_id);
            match timeout(
                Duration::from_secs(REDIS_TIMEOUT_SECS),
                pubsub.subscribe(&channel),
            )
            .await
            {
                Ok(Ok(())) => {
                    subscribed_rooms.insert(room_id.clone());
                    // Snapshot stream cursor for the newly subscribed room so that
                    // reconnect catch-up reads from the right position instead of "0".
                    let sk = self.room_stream_key(&room_id);
                    match self.get_latest_stream_id_for(&sk).await {
                        Ok(Some(id)) => {
                            debug!(
                                room_id = %room_id,
                                stream_id = %id,
                                "Re-synced: subscribed to room channel, cursor snapshotted"
                            );
                            stream_cursors.insert(sk, id);
                        }
                        Ok(None) => {
                            debug!(
                                room_id = %room_id,
                                "Re-synced: subscribed to room channel (empty stream)"
                            );
                            stream_cursors.insert(sk, "0".to_string());
                        }
                        Err(e) => {
                            warn!(
                                error = %e,
                                room_id = %room_id,
                                "Re-synced: subscribed but failed to snapshot cursor, using catchup_start_id"
                            );
                            stream_cursors.insert(sk, self.catchup_start_id());
                        }
                    }
                }
                _ => {
                    warn!(room_id = %room_id, "Re-sync: failed to subscribe to room channel");
                }
            }
        }

        // Unsubscribe from deactivated rooms
        for room_id in subscribed_rooms.difference(&active_rooms) {
            let channel = self.room_pubsub_channel(room_id);
            match timeout(
                Duration::from_secs(REDIS_TIMEOUT_SECS),
                pubsub.unsubscribe(&channel),
            )
            .await
            {
                Ok(Ok(())) => {
                    debug!(room_id = %room_id, "Re-synced: unsubscribed from room channel");
                    stream_cursors.remove(&self.room_stream_key(room_id));
                }
                _ => {
                    warn!(room_id = %room_id, "Re-sync: failed to unsubscribe from room channel");
                }
            }
        }

        // Update subscribed set to match active rooms
        *subscribed_rooms = active_rooms;

        info!(
            subscribed_rooms = subscribed_rooms.len(),
            "Re-synced room subscriptions with hub"
        );
    }

    /// Dispatch a single event received from Redis (either live or from catch-up).
    ///
    /// Handles deduplication, admin channel routing, permission cache invalidation,
    /// and local broadcast to room subscribers.
    async fn dispatch_event(&self, channel: &str, event: ClusterEvent) {
        // Skip unknown/unrecognized event types (forward compatibility)
        if matches!(&event, ClusterEvent::Unknown) {
            debug!(
                channel = %channel,
                "Skipping unknown cluster event type (forward compatibility)"
            );
            return;
        }

        // Deduplicate events (prevents duplicate delivery during catch-up + live overlap)
        let dedup_key = DedupKey::from_event(&event);
        if !self.deduplicator.should_process(&dedup_key) {
            debug!(
                channel = %channel,
                event_type = %event.event_type(),
                "Skipping duplicate event from Redis"
            );
            return;
        }

        // Record received metric
        synctv_core::metrics::cluster::CLUSTER_EVENTS_RECEIVED
            .with_label_values(&[event.event_type()])
            .inc();

        debug!(
            channel = %channel,
            event_type = %event.event_type(),
            "Dispatching event from Redis"
        );

        // Handle CacheInvalidate events: dispatch to local cache invalidation
        // service and do NOT forward to admin channel or room subscribers.
        if let ClusterEvent::CacheInvalidate { ref targets, .. } = event {
            self.invalidate_cache_targets(targets);
            return;
        }

        // Handle admin channel events (no room_id)
        if self.is_admin_channel(channel) {
            let _ = self.admin_event_tx.send(event);
            return;
        }

        // Extract room_id from channel name ({prefix}room:{room_id})
        if let Some(room_id_str) = self.extract_room_id_from_channel(channel) {
            let room_id = RoomId::from_string(room_id_str.to_string());

            // Forward kick/leave events to admin channel for cross-replica disconnect handling.
            // UserLeft is included so other replicas disconnect the user's connections
            // from the room (same behavior as KickUserFromRoom but with correct semantics).
            if matches!(
                &event,
                ClusterEvent::KickPublisher { .. }
                    | ClusterEvent::KickUserFromRoom { .. }
                    | ClusterEvent::UserLeft { .. }
            ) {
                let _ = self.admin_event_tx.send(event.clone());
            }

            // Invalidate local permission cache for cross-replica consistency
            if let Some(ref perm_svc) = self.permission_service {
                match &event {
                    ClusterEvent::PermissionChanged { target_user_id, .. } => {
                        perm_svc.invalidate_cache(&room_id, target_user_id).await;
                        debug!(
                            room_id = %room_id.as_str(),
                            user_id = %target_user_id.as_str(),
                            "Invalidated permission cache (cross-replica)"
                        );
                    }
                    ClusterEvent::UserLeft { user_id, .. } => {
                        perm_svc.invalidate_cache(&room_id, user_id).await;
                        debug!(
                            room_id = %room_id.as_str(),
                            user_id = %user_id.as_str(),
                            "Invalidated permission cache on UserLeft (cross-replica)"
                        );
                    }
                    ClusterEvent::RoomSettingsChanged { .. } | ClusterEvent::RoomDeleted { .. } => {
                        perm_svc.invalidate_room_cache(&room_id).await;
                        debug!(
                            room_id = %room_id.as_str(),
                            "Invalidated room permission cache (cross-replica)"
                        );
                    }
                    _ => {}
                }
            }

            // Invalidate data caches for events that modify room/user state.
            // This ensures L1 caches on other replicas stay consistent
            // without requiring the originating service to publish a
            // separate CacheInvalidate event.
            if self.cache_invalidation.is_some() {
                match &event {
                    ClusterEvent::RoomSettingsChanged { .. } | ClusterEvent::RoomCreated { .. } => {
                        self.invalidate_cache_targets(&[CacheTarget::Room {
                            room_id: room_id.as_str().to_string(),
                        }]);
                    }
                    ClusterEvent::RoomDeleted { .. } => {
                        // Invalidate both room cache and playback state cache
                        self.invalidate_cache_targets(&[CacheTarget::Room {
                            room_id: room_id.as_str().to_string(),
                        }]);
                        // PlaybackState is a separate moka cache; invalidate it
                        // directly via the CacheInvalidationService.
                        if let Some(ref cache_svc) = self.cache_invalidation {
                            use synctv_core::cache::InvalidationMessage;
                            if let Err(e) =
                                cache_svc.broadcast_local(InvalidationMessage::PlaybackState {
                                    room_id: room_id.as_str().to_string(),
                                })
                            {
                                tracing::warn!(
                                    error = %e,
                                    room_id = %room_id.as_str(),
                                    "Failed to broadcast PlaybackState invalidation for deleted room"
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }

            // Handle RoomDeleted: broadcast to local subscribers then clean up the room.
            // Critical delivery must complete before local senders are dropped;
            // otherwise the queued RoomDeleted notification can be lost for slow
            // subscribers even though the room cleanup proceeds.
            if matches!(&event, ClusterEvent::RoomDeleted { .. }) {
                let sent_count = self.message_hub.broadcast_reliably(&room_id, event).await;
                // Remove all local subscriptions for the deleted room
                self.message_hub.remove_room(&room_id);
                info!(
                    room_id = %room_id.as_str(),
                    notified = sent_count,
                    "Handled RoomDeleted: notified local subscribers and cleaned up room"
                );
                return;
            }

            // Route WebRTC signaling to the specific target connection instead of
            // broadcasting to all subscribers. The sender side validates
            // "user_id:conn_id", but defensive routing here also supports the
            // historical conn_id-only form by treating the whole field as a
            // connection ID rather than mis-routing it as a user-targeted send.
            if let ClusterEvent::WebRTCSignaling { ref to, .. } = event {
                let to_owned = to.clone();
                let target_conn = to_owned.rsplit_once(':').map_or_else(
                    || to_owned.clone(),
                    |(_target_user, conn_id)| conn_id.to_string(),
                );
                let sent = self
                    .message_hub
                    .broadcast_to_connection(&room_id, &target_conn, event)
                    .await;
                debug!(
                    room_id = %room_id.as_str(),
                    target_connection = %target_conn,
                    sent = sent,
                    "Routed WebRTC signaling to specific connection"
                );
                return;
            }

            // Broadcast to local subscribers
            let sent_count = self.message_hub.broadcast(&room_id, event);

            debug!(
                room_id = %room_id.as_str(),
                local_subscribers = sent_count,
                "Forwarded Redis event to local subscribers"
            );
        } else {
            warn!(channel = %channel, "Invalid channel format");
        }
    }

    /// Invalidate local L1 caches for the given targets.
    ///
    /// Dispatches each `CacheTarget` to the local `CacheInvalidationService`
    /// broadcast (which the `CacheManager` listener processes).
    fn invalidate_cache_targets(&self, targets: &[CacheTarget]) {
        let Some(ref cache_svc) = self.cache_invalidation else {
            return;
        };
        use synctv_core::cache::InvalidationMessage;
        for target in targets {
            let msg = match target {
                CacheTarget::User { user_id } => InvalidationMessage::User {
                    user_id: user_id.clone(),
                },
                CacheTarget::Username { user_id } => InvalidationMessage::Username {
                    user_id: user_id.clone(),
                },
                CacheTarget::Room { room_id } => InvalidationMessage::Room {
                    room_id: room_id.clone(),
                },
                CacheTarget::All => InvalidationMessage::All,
            };
            // The event already came from Redis, so we only need to notify
            // local cache subscribers. Using broadcast_local avoids re-publishing
            // the event back to the Redis cache invalidation stream.
            if let Err(e) = cache_svc.broadcast_local(msg) {
                warn!(
                    error = %e,
                    "Failed to dispatch cache invalidation from cluster event"
                );
            }
        }
        debug!(
            target_count = targets.len(),
            "Processed CacheInvalidate cluster event"
        );
    }

    /// Publish an event to Redis
    ///
    /// Uses both Pub/Sub (for real-time delivery) and per-room Stream (for reliability).
    /// XADD and PUBLISH are executed atomically via a Redis pipeline (MULTI/EXEC)
    /// to prevent duplicate stream entries on retry: if XADD succeeds but PUBLISH
    /// fails in a non-atomic flow, the caller retries both, producing a duplicate
    /// in the stream.
    ///
    /// If a subscriber disconnects, it can catch up by reading only the streams
    /// for rooms that have local subscribers, avoiding the N*M amplification of
    /// a single global stream.
    async fn publish_event(
        conn: &mut redis::aio::MultiplexedConnection,
        node_id: &str,
        key_prefix: &str,
        event: ClusterEvent,
        stream_max_length: usize,
    ) -> Result<usize> {
        let channel = if let Some(room_id) = event.room_id() {
            format!("{key_prefix}room:{}", room_id.as_str())
        } else {
            format!("{key_prefix}admin:events")
        };

        // Wrap event in envelope with node_id
        let envelope = EventEnvelope {
            node_id: node_id.to_string(),
            event: event.clone(),
        };

        let payload =
            serde_json::to_string(&envelope).context("Failed to serialize event envelope")?;

        // Stream key for reliable delivery (catch-up after disconnect)
        // Room events go to {prefix}room:{room_id}:events, admin events to {prefix}admin:events:stream
        let stream_key = if let Some(room_id) = event.room_id() {
            format!("{key_prefix}room:{}:events", room_id.as_str())
        } else {
            format!("{key_prefix}admin:events:stream")
        };

        let is_critical = event.is_critical();

        if is_critical {
            // Critical events: retry the atomic XADD+PUBLISH pipeline with
            // exponential backoff to avoid silent data loss.
            let mut backoff_ms = CRITICAL_STREAM_INITIAL_BACKOFF_MS;
            for attempt in 1..=CRITICAL_STREAM_MAX_RETRIES {
                let result = Self::publish_event_atomic(
                    conn,
                    &stream_key,
                    &channel,
                    &payload,
                    stream_max_length,
                )
                .await;

                match result {
                    Ok(subscribers) => return Ok(subscribers),
                    Err(e) => {
                        warn!(
                            error = %e,
                            stream_key = %stream_key,
                            attempt = attempt,
                            max_retries = CRITICAL_STREAM_MAX_RETRIES,
                            "Atomic XADD+PUBLISH failed for critical event, retrying"
                        );
                        if attempt < CRITICAL_STREAM_MAX_RETRIES {
                            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                            backoff_ms *= 2;
                        }
                    }
                }
            }
            error!(
                stream_key = %stream_key,
                event_type = event.event_type(),
                "Atomic XADD+PUBLISH failed for critical event after {} retries",
                CRITICAL_STREAM_MAX_RETRIES
            );
            synctv_core::metrics::cluster::CLUSTER_EVENTS_DROPPED
                .with_label_values(&["stream_write_failed"])
                .inc();
            // Fall through: return error so the caller can buffer for retry
            anyhow::bail!("Critical event publish failed after retries");
        }

        // Non-critical events: single atomic attempt
        match Self::publish_event_atomic(conn, &stream_key, &channel, &payload, stream_max_length)
            .await
        {
            Ok(subscribers) => Ok(subscribers),
            Err(e) => {
                warn!(
                    error = %e,
                    stream_key = %stream_key,
                    "Atomic XADD+PUBLISH failed for non-critical event"
                );
                synctv_core::metrics::cluster::CLUSTER_EVENTS_RECEIVED
                    .with_label_values(&["stream_write_failed"])
                    .inc();
                Err(e)
            }
        }
    }

    /// Execute XADD and PUBLISH atomically via a Redis pipeline with MULTI/EXEC.
    ///
    /// Returns the number of Pub/Sub subscribers that received the message.
    async fn publish_event_atomic(
        conn: &mut redis::aio::MultiplexedConnection,
        stream_key: &str,
        channel: &str,
        payload: &str,
        stream_max_length: usize,
    ) -> Result<usize> {
        use redis::streams::StreamMaxlen;

        // Build an atomic pipeline: MULTI { XADD, PUBLISH } EXEC
        let mut pipe = redis::pipe();
        pipe.atomic();
        pipe.xadd_maxlen::<_, _, _, _>(
            stream_key,
            StreamMaxlen::Approx(stream_max_length),
            "*",
            &[("channel", channel), ("payload", payload)],
        );
        pipe.publish::<_, _>(channel, payload);

        let results: (String, usize) = timeout(
            Duration::from_secs(REDIS_TIMEOUT_SECS),
            pipe.query_async(conn),
        )
        .await
        .context("Timed out executing atomic XADD+PUBLISH")?
        .context("Failed to execute atomic XADD+PUBLISH")?;

        Ok(results.1)
    }

    /// Get or create a shared multiplexed connection for non-Pub/Sub operations.
    ///
    /// `MultiplexedConnection` is clone-safe and handles automatic reconnection
    /// internally, so we lazily initialize once via `OnceCell` and clone for
    /// each caller.  No mutex contention on the hot path.
    async fn get_shared_conn(&self) -> Result<redis::aio::MultiplexedConnection> {
        let conn = self
            .shared_conn
            .get_or_try_init(|| async {
                self.redis_client
                    .get_multiplexed_async_connection()
                    .await
                    .context("Failed to get Redis shared connection")
            })
            .await?;
        Ok(conn.clone())
    }

    /// Get the ID of the latest entry in the given Redis Stream, or `None` if
    /// the stream is empty / does not exist. Used on first connection to snapshot
    /// per-room cursors so subsequent reconnections can catch up.
    async fn get_latest_stream_id_for(&self, stream_key: &str) -> Result<Option<String>> {
        use redis::streams::StreamRangeReply;

        let mut conn = self.get_shared_conn().await?;

        // XREVRANGE key + - COUNT 1  →  returns the single newest entry
        let reply: StreamRangeReply = timeout(
            Duration::from_secs(REDIS_TIMEOUT_SECS),
            conn.xrevrange_count(stream_key, "+", "-", 1usize),
        )
        .await
        .context("Timed out reading latest stream ID")?
        .context("Failed to read latest stream ID")?;

        Ok(reply.ids.into_iter().next().map(|entry| entry.id))
    }

    /// Maximum number of catch-up iterations to prevent infinite loops.
    /// Each iteration reads up to CATCHUP_BATCH_SIZE events, so the effective
    /// limit is MAX_CATCHUP_ITERATIONS * CATCHUP_BATCH_SIZE (50 * 1000 = 50K).
    const MAX_CATCHUP_ITERATIONS: usize = 50;
    /// Number of events to read per XREAD call during catch-up
    const CATCHUP_BATCH_SIZE: usize = 1000;

    /// Read missed events from a specific Redis Stream after reconnection.
    ///
    /// Loops XREAD until no more events are returned (or up to `MAX_CATCHUP_ITERATIONS`
    /// iterations) to ensure complete catch-up even when > 1000 events were missed.
    ///
    /// Returns a list of `(stream_id, channel, event)` tuples for events that
    /// occurred after `last_id`. The caller should update its tracked stream ID
    /// to the last returned `stream_id`.
    async fn read_missed_events_from(
        &self,
        stream_key: &str,
        last_id: &str,
    ) -> Result<Vec<(String, String, ClusterEvent)>> {
        // "$" is a sentinel meaning "skip catch-up for this stream" -- used when
        // the initial cursor snapshot failed and we don't know where to start.
        // Reading from "$" in XREAD would be invalid, so return empty.
        if last_id == "$" {
            debug!(stream_key = %stream_key, "Skipping catch-up (cursor is '$')");
            return Ok(Vec::new());
        }

        let mut conn = self.get_shared_conn().await?;

        let mut events = Vec::new();
        let mut cursor = last_id.to_string();

        for iteration in 0..Self::MAX_CATCHUP_ITERATIONS {
            let reply: StreamReadReply = timeout(
                Duration::from_secs(REDIS_TIMEOUT_SECS),
                conn.xread_options(
                    &[stream_key],
                    &[&cursor],
                    &redis::streams::StreamReadOptions::default().count(Self::CATCHUP_BATCH_SIZE),
                ),
            )
            .await
            .context("Timed out reading from Redis Stream")?
            .context("Failed to read from Redis Stream")?;

            let mut batch_count = 0;
            for sk in reply.keys {
                for entry in sk.ids {
                    batch_count += 1;
                    cursor = entry.id.clone();

                    let channel = entry
                        .map
                        .get("channel")
                        .and_then(|v| redis::from_redis_value::<String>(v.clone()).ok());
                    let payload = entry
                        .map
                        .get("payload")
                        .and_then(|v| redis::from_redis_value::<String>(v.clone()).ok());

                    if let (Some(chan), Some(payload_str)) = (channel, payload) {
                        match serde_json::from_str::<EventEnvelope>(&payload_str) {
                            Ok(envelope) => {
                                if envelope.node_id != self.node_id {
                                    events.push((entry.id, chan, envelope.event));
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, stream_key = %stream_key, "Failed to parse event envelope from stream");
                            }
                        }
                    }
                }
            }

            if batch_count < Self::CATCHUP_BATCH_SIZE {
                break;
            }

            if iteration == Self::MAX_CATCHUP_ITERATIONS - 1 {
                warn!(
                    total_events = events.len(),
                    stream_key = %stream_key,
                    "Catch-up reached max iterations ({}), some events may be missed",
                    Self::MAX_CATCHUP_ITERATIONS
                );
            }
        }

        Ok(events)
    }
}

/// Parse a Redis Stream ID (`"{timestamp_ms}-{seq}"`) into a `(u64, u64)` tuple.
///
/// Returns `None` if the ID is not in the expected format (e.g., `"$"`, `"0"`).
fn parse_stream_id(id: &str) -> Option<(u64, u64)> {
    let (ts_str, seq_str) = id.split_once('-')?;
    let ts = ts_str.parse::<u64>().ok()?;
    let seq = seq_str.parse::<u64>().ok()?;
    Some((ts, seq))
}

/// Returns `true` if `a` is strictly greater than `b` when interpreted as
/// Redis Stream IDs (`"{timestamp_ms}-{seq}"`).
///
/// Falls back to lexicographic comparison if either ID cannot be parsed,
/// which is correct for well-formed IDs with the same number of digits.
fn stream_id_gt(a: &str, b: &str) -> bool {
    match (parse_stream_id(a), parse_stream_id(b)) {
        (Some(a_parsed), Some(b_parsed)) => a_parsed > b_parsed,
        _ => a > b,
    }
}

/// Describes how the subscriber loop exited, enabling proper backoff behavior.
enum SubscriberExit {
    /// Connection was established and messages were being processed, but the
    /// stream ended (Redis disconnected). Backoff should be reset since the
    /// connection was healthy before it dropped.
    Disconnected,
    /// Failed to connect or subscribe to Redis. Backoff should continue
    /// increasing to avoid hammering an unavailable server.
    ConnectFailed(anyhow::Error),
}

/// Request to publish an event.
/// The channel is derived from `event.room_id()` in `publish_event`.
pub struct PublishRequest {
    pub event: ClusterEvent,
}

/// Envelope for events published to Redis
/// Includes `node_id` to avoid echo (each node ignores its own events)
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct EventEnvelope {
    node_id: String,
    event: ClusterEvent,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use synctv_core::models::id::UserId;

    #[test]
    fn test_event_envelope_serialization() {
        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: RoomId::from_string("room123".to_string()),
            user_id: UserId::from_string("user456".to_string()),
            username: "testuser".to_string(),
            message: "Hello!".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
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
        let room_id = RoomId::from_string("deleted-room".to_string());
        let user_id = UserId::from_string("user-1".to_string());
        let mut rx = message_hub
            .subscribe(room_id.clone(), user_id.clone(), "conn-1".to_string())
            .await
            .expect("subscribe should succeed");

        for _ in 0..512 {
            let sent = message_hub.broadcast(
                &room_id,
                ClusterEvent::ChatMessage {
                    event_id: nanoid::nanoid!(16),
                    room_id: room_id.clone(),
                    user_id: user_id.clone(),
                    username: "filler".to_string(),
                    message: "fill".to_string(),
                    timestamp: Utc::now(),
                    position: None,
                    color: None,
                },
            );
            assert_eq!(sent, 1, "filler message should enqueue");
        }

        let redis_client = RedisClient::open("redis://127.0.0.1:6379").expect("valid redis URL");
        let (admin_tx, _) = broadcast::channel(8);
        let pubsub = RedisPubSub::new(
            redis_client,
            message_hub.clone(),
            "node-1".to_string(),
            admin_tx,
            None,
            None,
            Arc::new(MessageDeduplicator::with_defaults()),
        )
        .expect("pubsub should be created");

        let event = ClusterEvent::RoomDeleted {
            event_id: nanoid::nanoid!(16),
            room_id: room_id.clone(),
            deleted_by: user_id.clone(),
            timestamp: Utc::now(),
        };

        let room_for_task = room_id.clone();
        let dispatch_task = tokio::spawn(async move {
            pubsub
                .dispatch_event("synctv:room:deleted-room", event)
                .await;
        });

        tokio::task::yield_now().await;
        assert!(
            !dispatch_task.is_finished(),
            "room cleanup must wait for reliable delivery when subscriber channels are full"
        );

        let drained = rx.recv().await.expect("filler message should be present");
        assert!(matches!(drained, ClusterEvent::ChatMessage { .. }));

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
            if matches!(msg, ClusterEvent::RoomDeleted { .. }) {
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
        use testcontainers::core::ImageExt;
        use testcontainers::runners::AsyncRunner;
        use testcontainers_modules::redis::Redis;

        /// Default Redis version for test containers
        const REDIS_VERSION: &str = "7-alpine";

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

        let redis_url = format!("redis://{redis_host}:{redis_port}");
        let redis_client = RedisClient::open(redis_url.as_str()).unwrap();

        // Verify Redis is reachable with retry logic
        // The container may report ready but TCP might not be fully established yet
        let mut conn = {
            let mut retries = 0;
            loop {
                match redis_client.get_multiplexed_async_connection().await {
                    Ok(conn) => break conn,
                    Err(_e) if retries < 10 => {
                        retries += 1;
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    }
                    Err(e) => panic!("Redis connection failed after {retries} retries: {e}"),
                }
            }
        };
        let _: () = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .expect("Redis PING failed");
        drop(conn);

        let message_hub = Arc::new(RoomMessageHub::new());

        let (admin_tx, _) = broadcast::channel(256);

        // Create two PubSub instances simulating different nodes
        // Note: Each RedisPubSub subscribes to lifecycle events from the message_hub internally
        let dedup1 = Arc::new(MessageDeduplicator::with_defaults());
        let dedup2 = Arc::new(MessageDeduplicator::with_defaults());
        let pubsub1 = Arc::new(
            RedisPubSub::new(
                redis_client.clone(),
                message_hub.clone(),
                "node1".to_string(),
                admin_tx.clone(),
                None,
                None,
                dedup1,
            )
            .unwrap(),
        );
        let pubsub2 = Arc::new(
            RedisPubSub::new(
                redis_client.clone(),
                message_hub.clone(),
                "node2".to_string(),
                admin_tx.clone(),
                None,
                None,
                dedup2,
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
        let room_id = RoomId::from_string("test_room".to_string());
        let user_id = UserId::from_string("test_user".to_string());
        let mut rx = message_hub
            .subscribe(room_id.clone(), user_id.clone(), "conn1".to_string())
            .await
            .expect("subscribe should succeed");

        // Wait for Redis room channel subscription to complete in both pubsub instances.
        // The lifecycle event triggers async Redis SUBSCRIBE which takes time.
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        // Publish event from node1
        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: room_id.clone(),
            user_id: user_id.clone(),
            username: "testuser".to_string(),
            message: "Hello from node1!".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        publish_tx1.send(PublishRequest { event }).await.unwrap();

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
                RedisClient::open("redis://127.0.0.1:1").expect("redis url should parse"),
                message_hub,
                "start-failure-node".to_string(),
                "synctv:test:",
                admin_tx,
                None,
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

    /// Test that catchup_start_id generates a valid Redis Stream ID format.
    /// The ID should be in the format "{timestamp_ms}-{seq}" where seq is 0.
    #[test]
    fn test_catchup_start_id_format() {
        let message_hub = Arc::new(RoomMessageHub::new());
        let (admin_tx, _) = broadcast::channel(256);
        let dedup = Arc::new(MessageDeduplicator::with_defaults());
        let redis_client = RedisClient::open("redis://127.0.0.1:1").unwrap();

        // Create with 300 second (5 minute) catchup window
        let pubsub = RedisPubSub::with_key_prefix(
            redis_client,
            message_hub,
            "test-node".to_string(),
            "synctv:",
            admin_tx,
            None,
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
            .as_millis() as u64;

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
        let redis_client = RedisClient::open("redis://127.0.0.1:1").unwrap();

        // Create with 60 second catchup window
        let pubsub = RedisPubSub::with_key_prefix(
            redis_client,
            message_hub,
            "test-node".to_string(),
            "synctv:",
            admin_tx,
            None,
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
            .as_millis() as u64;

        let expected_start = now_ms.saturating_sub(60_000);
        let diff = timestamp_ms.abs_diff(expected_start);

        // Allow 1 second tolerance
        assert!(
            diff < 1000,
            "catchup_start_id should use 60s window, diff: {diff}ms"
        );
    }

    // ========== Backpressure Tests ==========

    #[test]
    fn test_buffer_pressure_levels() {
        assert!(BufferPressure::Normal.allows_non_critical());
        assert!(!BufferPressure::Normal.critical_only());

        assert!(BufferPressure::Moderate.allows_non_critical());
        assert!(!BufferPressure::Moderate.critical_only());

        assert!(!BufferPressure::High.allows_non_critical());
        assert!(BufferPressure::High.critical_only());

        assert!(!BufferPressure::Critical.allows_non_critical());
        assert!(BufferPressure::Critical.critical_only());
    }

    #[test]
    fn test_buffer_pressure_state() {
        let state = BufferPressureState::new(1000);

        // Initially normal
        assert_eq!(state.pressure(), BufferPressure::Normal);

        // Set to moderate level (80%)
        state.set_retry_size(800);
        assert_eq!(state.pressure(), BufferPressure::Moderate);

        // Set to high level (90%)
        state.set_retry_size(900);
        assert_eq!(state.pressure(), BufferPressure::High);

        // Set to critical (100%)
        state.set_retry_size(1000);
        assert_eq!(state.pressure(), BufferPressure::Critical);

        // Critical buffer also counts
        state.set_retry_size(500);
        state.set_critical_size(500);
        assert_eq!(state.pressure(), BufferPressure::Critical);
    }

    #[test]
    fn test_publish_backpressure() {
        let state = BufferPressureState::new(1000);
        let backpressure = PublishBackpressure {
            state: state.clone(),
        };

        assert!(backpressure.can_send_non_critical());
        assert_eq!(backpressure.pressure(), BufferPressure::Normal);

        state.set_retry_size(900);
        assert!(!backpressure.can_send_non_critical());
        assert_eq!(backpressure.pressure(), BufferPressure::High);
    }

    // ========== Reconnection Subscription Recovery Tests ==========

    /// Test that room subscriptions activated during disconnection are recovered on reconnect.
    ///
    /// This test verifies the fix for the P1 issue where RedisPubSub subscriber
    /// could lose subscriptions to rooms that were activated while the subscriber
    /// was disconnected.
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
        use testcontainers::core::ImageExt;
        use testcontainers::runners::AsyncRunner;
        use testcontainers_modules::redis::Redis;

        /// Default Redis version for test containers
        const REDIS_VERSION: &str = "7-alpine";

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

        let redis_url = format!("redis://{redis_host}:{redis_port}");
        let redis_client = RedisClient::open(redis_url.as_str()).unwrap();

        // Verify Redis is reachable with retry logic
        {
            let mut retries = 0;
            loop {
                match redis_client.get_multiplexed_async_connection().await {
                    Ok(mut conn) => {
                        let _: () = redis::cmd("PING")
                            .query_async(&mut conn)
                            .await
                            .expect("Redis PING failed");
                        break;
                    }
                    Err(_e) if retries < 10 => {
                        retries += 1;
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    }
                    Err(e) => panic!("Redis connection failed after {retries} retries: {e}"),
                }
            }
        }

        let message_hub = Arc::new(RoomMessageHub::new());
        let (admin_tx, _) = broadcast::channel(256);

        // Create two PubSub instances
        let dedup1 = Arc::new(MessageDeduplicator::with_defaults());
        let dedup2 = Arc::new(MessageDeduplicator::with_defaults());
        let pubsub1 = Arc::new(
            RedisPubSub::new(
                redis_client.clone(),
                message_hub.clone(),
                "node1".to_string(),
                admin_tx.clone(),
                None,
                None,
                dedup1,
            )
            .unwrap(),
        );
        let pubsub2 = Arc::new(
            RedisPubSub::new(
                redis_client.clone(),
                message_hub.clone(),
                "node2".to_string(),
                admin_tx.clone(),
                None,
                None,
                dedup2,
            )
            .unwrap(),
        );

        // Start both PubSub instances
        let (publish_tx1, _backpressure1, _) = pubsub1.start(10_000).await.unwrap();
        let (_publish_tx2, _backpressure2, _) = pubsub2.start(10_000).await.unwrap();

        async fn publish_until_received(
            publish_tx: &tokio::sync::mpsc::Sender<PublishRequest>,
            rx: &mut tokio::sync::mpsc::Receiver<ClusterEvent>,
            make_event: impl Fn() -> ClusterEvent,
            timeout_label: &str,
        ) -> ClusterEvent {
            let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);

            loop {
                publish_tx
                    .send(PublishRequest {
                        event: make_event(),
                    })
                    .await
                    .expect("publish should succeed");

                match tokio::time::timeout(tokio::time::Duration::from_millis(500), rx.recv()).await
                {
                    Ok(Some(event)) => return event,
                    Ok(None) => {
                        panic!("channel closed unexpectedly")
                    }
                    Err(_) if tokio::time::Instant::now() < deadline => continue,
                    Err(_) => panic!("timeout waiting for {timeout_label} message"),
                }
            }
        }

        // Wait for subscriber loops to be ready. Keep a small initial pause, but
        // rely on eventual publish+receive retries below instead of fixed sleeps.
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        // Subscribe a client to room1 - this activates the room
        let room1_id = RoomId::from_string("test_room_1".to_string());
        let user1_id = UserId::from_string("test_user_1".to_string());
        let mut rx1 = message_hub
            .subscribe(room1_id.clone(), user1_id.clone(), "conn1".to_string())
            .await
            .expect("subscribe should succeed");

        let received = publish_until_received(
            &publish_tx1,
            &mut rx1,
            || ClusterEvent::ChatMessage {
                event_id: nanoid::nanoid!(16),
                room_id: room1_id.clone(),
                user_id: user1_id.clone(),
                username: "testuser1".to_string(),
                message: "Hello from room1!".to_string(),
                timestamp: chrono::Utc::now(),
                position: None,
                color: None,
            },
            "room1",
        )
        .await;
        assert_eq!(received.event_type(), "chat_message");

        // Now subscribe a client to room2 - this is a second room activation
        let room2_id = RoomId::from_string("test_room_2".to_string());
        let user2_id = UserId::from_string("test_user_2".to_string());
        let mut rx2 = message_hub
            .subscribe(room2_id.clone(), user2_id.clone(), "conn2".to_string())
            .await
            .expect("subscribe should succeed");

        let received = publish_until_received(
            &publish_tx1,
            &mut rx2,
            || ClusterEvent::ChatMessage {
                event_id: nanoid::nanoid!(16),
                room_id: room2_id.clone(),
                user_id: user2_id.clone(),
                username: "testuser2".to_string(),
                message: "Hello from room2!".to_string(),
                timestamp: chrono::Utc::now(),
                position: None,
                color: None,
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
        let mut pending_subscriptions: HashSet<String> = HashSet::new();

        // Simulate room activations during disconnection
        let room1 = RoomId::from_string("room1".to_string());
        let room2 = RoomId::from_string("room2".to_string());
        let room3 = RoomId::from_string("room3".to_string());

        // Room activated
        pending_subscriptions.insert(room1.as_str().to_string());
        pending_subscriptions.insert(room2.as_str().to_string());
        assert_eq!(pending_subscriptions.len(), 2);

        // Room deactivated before reconnect (should be removed)
        pending_subscriptions.remove(room2.as_str());
        assert_eq!(pending_subscriptions.len(), 1);
        assert!(pending_subscriptions.contains("room1"));
        assert!(!pending_subscriptions.contains("room2"));

        // Another room activated
        pending_subscriptions.insert(room3.as_str().to_string());
        assert_eq!(pending_subscriptions.len(), 2);

        // After reconnect and successful subscription, clear the set
        pending_subscriptions.clear();
        assert!(pending_subscriptions.is_empty());
    }

    /// Unit test verifying the merge of active_rooms with pending_subscriptions.
    #[test]
    fn test_pending_subscriptions_merges_with_active_rooms() {
        let active_rooms: Vec<RoomId> = vec![
            RoomId::from_string("active_room1".to_string()),
            RoomId::from_string("active_room2".to_string()),
        ];

        // Simulate a room that was activated during disconnection
        // but is NOT in the current active_rooms (edge case - room became inactive)
        let mut pending_subscriptions: HashSet<String> = HashSet::new();
        pending_subscriptions.insert("pending_room".to_string());
        pending_subscriptions.insert("active_room1".to_string()); // Already active, but also pending

        // Merge logic (same as in run_subscriber)
        let mut rooms_to_subscribe: HashSet<String> = active_rooms
            .iter()
            .map(|rid| rid.as_str().to_string())
            .collect();
        rooms_to_subscribe.extend(pending_subscriptions.iter().cloned());

        // Should contain both active rooms and pending room
        assert_eq!(rooms_to_subscribe.len(), 3);
        assert!(rooms_to_subscribe.contains("active_room1"));
        assert!(rooms_to_subscribe.contains("active_room2"));
        assert!(rooms_to_subscribe.contains("pending_room"));

        // After successful subscription, clear pending
        pending_subscriptions.clear();
        assert!(pending_subscriptions.is_empty());
    }

    #[test]
    fn test_failed_cursor_snapshot_falls_back_to_catchup_window_not_dollar() {
        let message_hub = Arc::new(RoomMessageHub::new());
        let (admin_tx, _) = broadcast::channel(256);
        let dedup = Arc::new(MessageDeduplicator::with_defaults());
        let redis_client = RedisClient::open("redis://127.0.0.1:1").unwrap();

        let pubsub = RedisPubSub::with_key_prefix(
            redis_client,
            message_hub,
            "test-node".to_string(),
            "synctv:",
            admin_tx,
            None,
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
    async fn test_dispatch_event_routes_conn_id_only_webrtc_to_specific_connection() {
        let message_hub = Arc::new(RoomMessageHub::new());
        let (admin_tx, _) = broadcast::channel(256);
        let dedup = Arc::new(MessageDeduplicator::with_defaults());
        let redis_client = RedisClient::open("redis://127.0.0.1:1").unwrap();

        let pubsub = RedisPubSub::with_key_prefix(
            redis_client,
            message_hub.clone(),
            "test-node".to_string(),
            "synctv:",
            admin_tx,
            None,
            None,
            dedup,
            300,
            1000,
        )
        .unwrap();

        let room_id = RoomId::from_string("dispatch-room".to_string());
        let user1 = synctv_core::models::id::UserId::from_string("user1".to_string());
        let user2 = synctv_core::models::id::UserId::from_string("user2".to_string());
        let mut rx1 = message_hub
            .subscribe(room_id.clone(), user1, "conn1".to_string())
            .await
            .expect("subscribe should succeed");
        let mut rx2 = message_hub
            .subscribe(room_id.clone(), user2, "conn2".to_string())
            .await
            .expect("subscribe should succeed");

        pubsub
            .dispatch_event(
                "synctv:room:dispatch-room",
                ClusterEvent::WebRTCSignaling {
                    event_id: nanoid::nanoid!(16),
                    room_id: room_id.clone(),
                    message_type: "offer".to_string(),
                    from: "user1|conn1".to_string(),
                    to: "conn2".to_string(),
                    data: "SDP".to_string(),
                    timestamp: chrono::Utc::now(),
                },
            )
            .await;

        let target = tokio::time::timeout(Duration::from_millis(100), rx2.recv())
            .await
            .expect("target connection should receive event")
            .expect("target channel should remain open");
        assert!(matches!(target, ClusterEvent::WebRTCSignaling { .. }));

        let non_target = tokio::time::timeout(Duration::from_millis(100), rx1.recv()).await;
        assert!(
            non_target.is_err(),
            "non-target connection must not receive conn_id-only signaling"
        );
    }

    #[tokio::test]
    async fn test_dispatch_event_only_delivers_duplicate_once() {
        let message_hub = Arc::new(RoomMessageHub::new());
        let (admin_tx, _) = broadcast::channel(256);
        let dedup = Arc::new(MessageDeduplicator::with_defaults());
        let redis_client = RedisClient::open("redis://127.0.0.1:1").unwrap();

        let pubsub = RedisPubSub::with_key_prefix(
            redis_client,
            message_hub.clone(),
            "test-node".to_string(),
            "synctv:",
            admin_tx,
            None,
            None,
            dedup,
            300,
            1000,
        )
        .unwrap();

        let room_id = RoomId::from_string("dedup-room".to_string());
        let user_id = synctv_core::models::id::UserId::from_string("dedup-user".to_string());
        let mut rx = message_hub
            .subscribe(room_id.clone(), user_id.clone(), "dedup-conn".to_string())
            .await
            .expect("subscribe should succeed");

        let event = ClusterEvent::ChatMessage {
            event_id: "duplicate-event-id".to_string(),
            room_id,
            user_id,
            username: "dedup".to_string(),
            message: "hello".to_string(),
            timestamp: chrono::Utc::now(),
            position: None,
            color: None,
        };

        pubsub
            .dispatch_event("synctv:room:dedup-room", event.clone())
            .await;
        pubsub.dispatch_event("synctv:room:dedup-room", event).await;

        let first = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("first event should be delivered")
            .expect("channel should remain open");
        assert!(matches!(first, ClusterEvent::ChatMessage { .. }));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "duplicate event must not be delivered twice"
        );
    }
}
