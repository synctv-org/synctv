use anyhow::{Context, Result};
use futures::stream::StreamExt;
use redis::streams::StreamReadReply;
use redis::AsyncCommands;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use synctv_core::RedisCoordinationRuntime;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Timeout for Redis operations in seconds
const REDIS_TIMEOUT_SECS: u64 = 5;
/// Maximum time to wait for the subscriber to finish its initial subscriptions.
const SUBSCRIBER_READY_TIMEOUT_SECS: u64 = 5;
const CURSOR_REFRESH_BATCH_SIZE: usize = 64;

/// Returns `true` if the Redis error looks like a Sentinel failover.
pub fn is_sentinel_failover_error(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        let msg = cause.to_string();
        msg.contains("READONLY") || msg.contains("LOADING")
    })
}

async fn log_join_result<T>(task_name: &'static str, handle: tokio::task::JoinHandle<T>) {
    if let Err(error) = handle.await {
        warn!(task = task_name, "Redis Pub/Sub task join failed: {error}");
    }
}

/// Helper to read, dispatch, and update cursor for a single stream attempt during catch-up
async fn process_stream_catchup(
    self_ref: &RedisPubSub,
    stream_key: &str,
    start_cursor: &str,
    stream_cursors: &mut HashMap<String, String>,
) -> Result<usize> {
    let mut caught_up = 0usize;
    let events = self_ref
        .read_missed_events_from(stream_key, start_cursor)
        .await?;
    for (stream_id, channel, event) in events {
        self_ref.dispatch_event(&channel, event).await;
        caught_up += 1;
        stream_cursors.insert(stream_key.to_string(), stream_id);
    }
    // If no events, snapshot the latest stream ID
    if !stream_cursors.contains_key(stream_key) {
        match self_ref.get_latest_stream_id_for(stream_key).await {
            Ok(Some(id)) => {
                stream_cursors.insert(stream_key.to_string(), id);
            }
            _ => {
                stream_cursors.insert(stream_key.to_string(), "0".to_string());
            }
        }
    }
    Ok(caught_up)
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

/// Minimum TTL for inactive per-room Redis Streams.
///
/// Streams are only needed for bounded catch-up after Pub/Sub disconnects. Once a
/// room has been inactive for well beyond the catch-up window, keeping its stream
/// key forever only grows Redis metadata without improving recovery guarantees.
const MIN_ROOM_STREAM_TTL_SECS: u64 = 900;

/// Maximum number of failed events to buffer for retry after reconnection.
const MAX_RETRY_BUFFER: usize = 10_000;

/// Maximum number of critical events to buffer separately.
const MAX_CRITICAL_BUFFER: usize = 5_000;

/// Warning threshold for critical buffer (50% of `MAX_CRITICAL_BUFFER`).
const CRITICAL_BUFFER_WARN_THRESHOLD: usize = 2_500;

/// Warning threshold for normal buffer (80% of `MAX_RETRY_BUFFER`).
const BUFFER_WARN_THRESHOLD: usize =
    super::backpressure::retry_buffer_warn_threshold(MAX_RETRY_BUFFER);

// Both admin and room events use the same channel naming scheme and are published
// via PUBLISH + XADD in `publish_event()`. The subscription strategy differs:
//   - **Admin events**: Pattern subscription (`PSUBSCRIBE {prefix}admin:*`)
//     because admin events are global and infrequent. All nodes receive all admin
//     events regardless of which rooms they serve.
//   - **Room events**: Per-room subscriptions (`SUBSCRIBE {prefix}room:{room_id}`)
//     managed dynamically via `RoomLifecycleEvent`s. This avoids receiving traffic
//     for rooms the node does not serve, which is important in large deployments
//     with many active rooms.
// Dispatch for both paths converges in `dispatch_event()`, which handles
// deduplication, application-owned remote-event side effects, and local
// broadcast uniformly.

use super::backpressure::BufferPressureState;
use super::backpressure::PublishBackpressure;
use super::dedup::{DedupKey, MessageDeduplicator};
use super::room_hub::RoomLifecycleEvent;
use super::runtime::RoomMessageRuntime;
use super::stream_id::stream_id_gt;
use super::transport::{
    RealtimeEventHandler, RealtimeMessageTransport, RealtimeMessageTransportConfig,
    RealtimeMessageTransportFactory, RealtimeMessageTransportRuntime,
};
use super::RealtimeEvent;
use synctv_core::models::id::RoomId;

enum SelectResult {
    Message(redis::Msg),
    LifecycleEvent(RoomLifecycleEvent),
    CursorRefresh,
    Cancelled,
    StreamEnded,
}

fn remove_successfully_unsubscribed_rooms(
    subscribed_rooms: &mut HashSet<RoomId>,
    successfully_unsubscribed: &[RoomId],
) {
    for room_id in successfully_unsubscribed {
        subscribed_rooms.remove(room_id);
    }
}

/// Redis Pub/Sub service for cross-node event synchronization
///
/// This service enables multi-replica deployments by:
/// 1. Publishing local room events to Redis channels
/// 2. Subscribing to Redis channels for events from other nodes
/// 3. Forwarding received events to the local `RoomMessageHub`
///
/// Comprehensive error handling for realtime pub/sub:
/// - Automatic reconnection with exponential backoff (1s → 30s max)
/// - Failed publish retry logic: saves failed events and retries after reconnection
/// - Stream-based catch-up mechanism: recovers missed events during disconnection
/// - Timeout protection: 5s timeout on all Redis operations
/// - Critical event guarantee: XADD operations retry up to 3 times with backoff
/// - Graceful degradation: logs warnings but continues operation on non-critical failures
/// - Connection health checks: periodic PING to detect stale connections
///
/// Channel naming: `room:{room_id}` for room-specific events
pub struct RedisPubSub {
    redis_runtime: Arc<dyn RedisCoordinationRuntime>,
    /// Shared multiplexed connection for non-Pub/Sub operations (stream reads).
    ///
    /// `MultiplexedConnection` is clone-safe (internally `Arc`-based) and handles
    /// automatic reconnection, so we use `OnceCell` for lazy one-time init
    /// instead of a `Mutex<Option<_>>`.  Each caller clones the connection for
    /// concurrent use without lock contention.
    shared_conn: tokio::sync::OnceCell<redis::aio::MultiplexedConnection>,
    message_hub: Arc<dyn RoomMessageRuntime>,
    node_id: String,
    /// Key prefix for all Redis keys and channels (e.g., "synctv:")
    key_prefix: String,
    admin_event_tx: broadcast::Sender<RealtimeEvent>,
    event_handler: Option<Arc<dyn RealtimeEventHandler>>,
    deduplicator: Arc<MessageDeduplicator>,
    cancel_token: CancellationToken,
    /// How far back (in milliseconds) to replay Redis Stream events on first connect.
    /// Configurable via `ClusterChannelConfig::catchup_window_secs`.
    catchup_window_ms: u128,
    /// TTL for inactive per-room streams.
    ///
    /// Admin events intentionally use a single global stream without TTL because
    /// that key is bounded to one stream and acts as replica-wide infrastructure.
    room_stream_ttl_secs: u64,
    /// Maximum number of entries per Redis Stream (approximate).
    /// Configurable via `ClusterChannelConfig::stream_max_length`.
    stream_max_length: usize,
    /// JoinHandle for the subscriber task, stored so it can be awaited during shutdown.
    subscriber_handle: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct RedisRealtimeMessageTransportFactory {
    redis_runtime: Arc<dyn RedisCoordinationRuntime>,
}

impl RedisRealtimeMessageTransportFactory {
    #[must_use]
    pub fn new(redis_runtime: Arc<dyn RedisCoordinationRuntime>) -> Self {
        Self { redis_runtime }
    }
}

impl RealtimeMessageTransportFactory for RedisRealtimeMessageTransportFactory {
    fn build(
        &self,
        config: RealtimeMessageTransportConfig,
    ) -> crate::error::Result<Arc<dyn RealtimeMessageTransport>> {
        Ok(Arc::new(RedisPubSub::from_config(RedisPubSubConfig {
            redis_runtime: self.redis_runtime.clone(),
            message_hub: config.message_runtime,
            node_id: config.node_id,
            key_prefix: config.key_prefix,
            admin_event_tx: config.admin_event_tx,
            event_handler: config.event_handler,
            deduplicator: config.deduplicator,
            catchup_window_secs: config.catchup_window_secs,
            stream_max_length: config.stream_max_length,
        })?))
    }
}

pub struct RedisPubSubConfig {
    pub redis_runtime: Arc<dyn RedisCoordinationRuntime>,
    pub message_hub: Arc<dyn RoomMessageRuntime>,
    pub node_id: String,
    pub key_prefix: String,
    pub admin_event_tx: broadcast::Sender<RealtimeEvent>,
    pub event_handler: Option<Arc<dyn RealtimeEventHandler>>,
    pub deduplicator: Arc<MessageDeduplicator>,
    pub catchup_window_secs: u64,
    pub stream_max_length: usize,
}

impl RedisPubSubConfig {
    #[must_use]
    pub fn new(
        redis_runtime: Arc<dyn RedisCoordinationRuntime>,
        message_hub: Arc<dyn RoomMessageRuntime>,
        node_id: impl Into<String>,
        admin_event_tx: broadcast::Sender<RealtimeEvent>,
        deduplicator: Arc<MessageDeduplicator>,
    ) -> Self {
        Self {
            redis_runtime,
            message_hub,
            node_id: node_id.into(),
            key_prefix: "synctv:".to_string(),
            admin_event_tx,
            event_handler: None,
            deduplicator,
            catchup_window_secs: 300,
            stream_max_length: DEFAULT_MAX_STREAM_LENGTH,
        }
    }

    #[must_use]
    pub fn key_prefix(mut self, key_prefix: impl Into<String>) -> Self {
        self.key_prefix = key_prefix.into();
        self
    }

    #[must_use]
    pub fn event_handler(mut self, handler: Arc<dyn RealtimeEventHandler>) -> Self {
        self.event_handler = Some(handler);
        self
    }

    #[must_use]
    pub fn catchup_window_secs(mut self, catchup_window_secs: u64) -> Self {
        self.catchup_window_secs = catchup_window_secs;
        self
    }

    #[must_use]
    pub fn stream_max_length(mut self, stream_max_length: usize) -> Self {
        self.stream_max_length = stream_max_length;
        self
    }
}

impl RedisPubSub {
    /// Create a new `RedisPubSub` service.
    pub fn new(
        redis_runtime: Arc<dyn RedisCoordinationRuntime>,
        message_hub: Arc<dyn RoomMessageRuntime>,
        node_id: String,
        admin_event_tx: broadcast::Sender<RealtimeEvent>,
        event_handler: Option<Arc<dyn RealtimeEventHandler>>,
        deduplicator: Arc<MessageDeduplicator>,
    ) -> Result<Self> {
        let mut config = RedisPubSubConfig::new(
            redis_runtime,
            message_hub,
            node_id,
            admin_event_tx,
            deduplicator,
        );
        config.event_handler = event_handler;
        Self::from_config(config)
    }

    /// Create a new `RedisPubSub` service from explicit runtime configuration.
    ///
    /// `catchup_window_secs` controls how far back to replay Redis Stream events
    /// when this node first connects.  Pass `300` for the default (5 minutes).
    /// `stream_max_length` controls the maximum number of entries per Redis Stream.
    pub fn from_config(config: RedisPubSubConfig) -> Result<Self> {
        Ok(Self {
            redis_runtime: config.redis_runtime,
            shared_conn: tokio::sync::OnceCell::new(),
            message_hub: config.message_hub,
            node_id: config.node_id,
            key_prefix: config.key_prefix,
            admin_event_tx: config.admin_event_tx,
            event_handler: config.event_handler,
            deduplicator: config.deduplicator,
            cancel_token: CancellationToken::new(),
            catchup_window_ms: u128::from(config.catchup_window_secs) * 1000,
            room_stream_ttl_secs: Self::room_stream_ttl_secs(config.catchup_window_secs),
            stream_max_length: config.stream_max_length,
            subscriber_handle: tokio::sync::Mutex::new(None),
        })
    }

    /// Build the Redis Stream key for admin events
    fn admin_stream_key(&self) -> String {
        format!("{}admin:events:stream", self.key_prefix)
    }

    /// Build the Redis Stream key for a specific room
    fn room_stream_key(&self, room_id: impl std::fmt::Display) -> String {
        format!("{}room:{}:events", self.key_prefix, room_id)
    }

    fn room_stream_ttl_secs(catchup_window_secs: u64) -> u64 {
        catchup_window_secs
            .saturating_mul(2)
            .max(MIN_ROOM_STREAM_TTL_SECS)
    }

    /// Build the admin Pub/Sub pattern
    fn admin_pubsub_pattern(&self) -> String {
        format!("{}admin:*", self.key_prefix)
    }

    /// Build the room Pub/Sub channel
    fn room_pubsub_channel(&self, room_id: impl std::fmt::Display) -> String {
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
        let now_ms = u128::try_from(synctv_core::SystemClock.now_millis()).unwrap_or(0);
        let start_ms = now_ms.saturating_sub(self.catchup_window_ms);
        format!("{start_ms}-0")
    }

    /// Extract room_id from a channel name (e.g., "synctv:room:42" -> Some("42"))
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
        if let Some(mut handle) = handle {
            match tokio::time::timeout(Duration::from_secs(5), &mut handle).await {
                Ok(Ok(())) => info!("Redis subscriber task completed"),
                Ok(Err(e)) => warn!("Redis subscriber task panicked: {}", e),
                Err(_) => {
                    warn!("Redis subscriber task did not finish within 5s timeout, aborting");
                    handle.abort();
                    match handle.await {
                        Ok(()) => info!("Redis subscriber task completed after abort"),
                        Err(e) if e.is_cancelled() => {
                            info!("Redis subscriber task aborted after timeout");
                        }
                        Err(e) => warn!(
                            "Redis subscriber task returned join error after timeout abort: {}",
                            e
                        ),
                    }
                }
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
        let publish_runtime = self.redis_runtime.clone();
        let node_id = self.node_id.clone();
        let key_prefix = self.key_prefix.clone();
        let cancel_publisher = self.cancel_token.clone();
        let room_stream_ttl_secs = self.room_stream_ttl_secs;
        let stream_max_length = self.stream_max_length;

        // Create shared buffer pressure state for backpressure signaling
        let buffer_pressure_state = BufferPressureState::new(MAX_RETRY_BUFFER);
        let backpressure = PublishBackpressure::new(buffer_pressure_state.clone());

        // Spawn task to handle publishing with reconnection logic.
        // The handle is returned to the caller so shutdown() can await completion.
        let publisher_handle = tokio::spawn(async move {
            let mut backoff_secs = INITIAL_BACKOFF_SECS;
            // Buffer for retrying failed non-critical publishes after reconnection.
            // Using a Vec instead of Option<PublishRequest> ensures that multiple
            // events that fail during a connection interruption window are all
            // preserved for retry, not just the last one.
            let mut retry_buffer: Vec<PublishRequest> = Vec::new();
            // Separate bounded buffer for critical events (kick/ban). Normal retry
            // pressure never evicts this buffer; confirmed requests fail back to the
            // durable outbox when the critical buffer itself is full.
            let mut critical_retry_buffer: VecDeque<PublishRequest> = VecDeque::new();
            // Helper to update buffer pressure state
            let update_pressure = |retry_len: usize, critical_len: usize| {
                buffer_pressure_state.set_retry_size(retry_len);
                buffer_pressure_state.set_critical_size(critical_len);
            };

            loop {
                let conn = match timeout(
                    Duration::from_secs(REDIS_TIMEOUT_SECS),
                    publish_runtime.multiplexed_connection(),
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

                // CRITICAL EVENTS: Always retry first (highest priority)
                if !critical_retry_buffer.is_empty() {
                    let critical_batch: Vec<_> = critical_retry_buffer.drain(..).collect();
                    update_pressure(retry_buffer.len(), 0); // Critical buffer is empty during retry
                    info!(
                        critical_count = critical_batch.len(),
                        "Retrying critical events after reconnection"
                    );
                    let (failed, _success_count) = retry_publish_batch(
                        critical_batch,
                        &mut conn,
                        &node_id,
                        &key_prefix,
                        room_stream_ttl_secs,
                        stream_max_length,
                    )
                    .await;
                    if !failed.is_empty() {
                        warn!(
                            failed_count = failed.len(),
                            "Some critical events failed to retry, keeping in buffer"
                        );
                        critical_retry_buffer = failed.into();
                        update_pressure(retry_buffer.len(), critical_retry_buffer.len());
                        let cancelled = tokio::select! {
                            () = cancel_publisher.cancelled() => true,
                            () = tokio::time::sleep(Duration::from_secs(backoff_secs)) => false,
                        };
                        if cancelled {
                            info!("Redis publisher task cancelled during critical retry backoff");
                            return;
                        }
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
                    let (failed, _success_count) = retry_publish_batch(
                        buffered,
                        &mut conn,
                        &node_id,
                        &key_prefix,
                        room_stream_ttl_secs,
                        stream_max_length,
                    )
                    .await;
                    if !failed.is_empty() {
                        retry_buffer = failed;
                        update_pressure(retry_buffer.len(), critical_retry_buffer.len());
                        let cancelled = tokio::select! {
                            () = cancel_publisher.cancelled() => true,
                            () = tokio::time::sleep(Duration::from_secs(backoff_secs)) => false,
                        };
                        if cancelled {
                            info!("Redis publisher task cancelled during normal retry backoff");
                            return;
                        }
                        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                        continue;
                    }
                }

                // Buffers are empty after successful retries - update pressure
                update_pressure(0, 0);

                // Track whether this session was healthy (at least one event sent)
                let mut session_healthy = false;
                let mut buffer_warn_logged = false;

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

                            // Macro to reduce duplication in shutdown flush logic
                            macro_rules! flush_event {
                                ($req:expr, $is_critical:expr, $label:expr) => {{
                                    let mut req = $req;
                                    if flush_failed {
                                        if $is_critical {
                                            error!(
                                                event_type = req.event.event_type(),
                                                "CRITICAL event lost during shutdown (connection broken)"
                                            );
                                        } else {
                                            debug!(
                                                event_type = req.event.event_type(),
                                                "Non-critical event skipped during shutdown (connection broken)"
                                            );
                                        }
                                        req.acknowledge_failure(
                                            if $is_critical {
                                                "Redis publisher shutdown lost critical event"
                                            } else {
                                                "Redis publisher shutdown skipped event after connection failure"
                                            }
                                        );
                                        continue;
                                    }
                                    let event_type = req.event.event_type();
                                    match Self::publish_event(&mut conn, &node_id, &key_prefix, &req.event, room_stream_ttl_secs, stream_max_length).await {
                                        Ok(_) => {
                                            req.acknowledge_success();
                                            debug!(event_type = event_type, label = $label, "Event flushed on shutdown");
                                        }
                                        Err(e) => {
                                            if $is_critical {
                                                error!(error = %e, event_type = event_type, "Failed to flush CRITICAL event on shutdown");
                                            } else {
                                                warn!(error = %e, event_type = event_type, "Failed to flush event on shutdown");
                                            }
                                            req.acknowledge_failure(format!("Failed to flush event on shutdown: {e}"));
                                            flush_failed = true;
                                        }
                                    }
                                }};
                            }

                            // CRITICAL: Flush critical_retry_buffer FIRST (highest priority)
                            for req in critical_retry_buffer.drain(..) {
                                flush_event!(req, true, "critical_buffer");
                            }
                            // Then flush normal retry_buffer
                            for req in std::mem::take(&mut retry_buffer) {
                                flush_event!(req, false, "retry_buffer");
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
                                flush_event!(req, true, "critical_drain");
                            }
                            // Then flush normal drained events
                            for req in normal_drain {
                                flush_event!(req, false, "normal_drain");
                            }
                            return;
                        }
                        req = publish_rx.recv() => req,
                    };
                    if let Some(mut req) = req {
                        let event_type = req.event.event_type();
                        match Self::publish_event(
                            &mut conn,
                            &node_id,
                            &key_prefix,
                            &req.event,
                            room_stream_ttl_secs,
                            stream_max_length,
                        )
                        .await
                        {
                            Ok(subscribers) => {
                                session_healthy = true;
                                req.acknowledge_success();
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
                                // Confirmed publish requests are backed by the durable
                                // database outbox, so fail them back to that retry loop instead
                                // of also retaining them in the in-memory Redis retry buffer.
                                if req.expects_ack() {
                                    req.acknowledge_failure(format!(
                                        "Failed to publish event to Redis: {e}"
                                    ));
                                } else if req.event.is_critical() {
                                    push_critical_retry_buffer(&mut critical_retry_buffer, req);
                                } else {
                                    retry_buffer.push(req);
                                }

                                // Drain remaining events from channel into appropriate buffers
                                // (connection is broken, no point trying to publish more)
                                while let Ok(mut req) = publish_rx.try_recv() {
                                    let is_critical = req.event.is_critical();
                                    let event_type = req.event.event_type();

                                    if req.expects_ack() {
                                        req.acknowledge_failure(
                                            "Redis publisher connection failed before confirmed publish",
                                        );
                                    } else if is_critical {
                                        push_critical_retry_buffer(&mut critical_retry_buffer, req);

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
                                            synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
                                                .with_label_values(&["retry_buffer_full"])
                                                .inc();
                                            req.acknowledge_failure("Redis retry buffer full");
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
                let cancelled = tokio::select! {
                    () = cancel_publisher.cancelled() => true,
                    () = tokio::time::sleep(Duration::from_secs(backoff_secs)) => false,
                };
                if cancelled {
                    info!("Redis publisher task cancelled during reconnection backoff");
                    return;
                }
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
            let mut pending_subscriptions: HashSet<RoomId> = HashSet::new();
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
                            pending_subscriptions.insert(room_id);
                        }
                        RoomLifecycleEvent::RoomDeactivated(room_id) => {
                            pending_subscriptions.remove(&room_id);
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
                    SubscriberExit::Cancelled => return,
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
                    log_join_result("subscriber", handle).await;
                }
                log_join_result("publisher", publisher_handle).await;
                return Err(anyhow::anyhow!(
                    "Redis subscriber exited before reporting readiness"
                ));
            }
            Err(_) => {
                self.cancel_token.cancel();
                if let Some(handle) = self.subscriber_handle.lock().await.take() {
                    log_join_result("subscriber", handle).await;
                }
                log_join_result("publisher", publisher_handle).await;
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
        pending_subscriptions: &mut HashSet<RoomId>,
        subscriber_ready_tx: &mut Option<tokio::sync::oneshot::Sender<()>>,
    ) -> SubscriberExit {
        let mut pubsub = match timeout(
            Duration::from_secs(REDIS_TIMEOUT_SECS),
            self.redis_runtime.async_pubsub(),
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

        // Always subscribe to admin channel pattern (needed for replica-wide events)
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
        let mut subscribed_rooms: HashSet<RoomId> = HashSet::new();

        // Merge active rooms with pending subscriptions (rooms that were activated
        // while we were disconnected). This ensures we don't lose subscriptions
        // to rooms that became active during the disconnection period.
        let mut rooms_to_subscribe: HashSet<RoomId> = active_rooms.iter().copied().collect();
        rooms_to_subscribe.extend(pending_subscriptions.iter().copied());

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
                        subscribed_rooms.insert(*rid);
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
                    match timeout(
                        Duration::from_secs(REDIS_TIMEOUT_SECS),
                        pubsub.psubscribe(&room_pattern),
                    )
                    .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            return SubscriberExit::ConnectFailed(anyhow::anyhow!(e).context(
                                format!("Failed to fallback psubscribe to {room_pattern}"),
                            ));
                        }
                        Err(_) => {
                            return SubscriberExit::ConnectFailed(anyhow::anyhow!(
                                "Timed out fallback psubscribe to {room_pattern}"
                            ));
                        }
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
                    match timeout(
                        Duration::from_secs(REDIS_TIMEOUT_SECS),
                        pubsub.psubscribe(&room_pattern),
                    )
                    .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            return SubscriberExit::ConnectFailed(anyhow::anyhow!(e).context(
                                format!("Failed to fallback psubscribe to {room_pattern}"),
                            ));
                        }
                        Err(_) => {
                            return SubscriberExit::ConnectFailed(anyhow::anyhow!(
                                "Timed out fallback psubscribe to {room_pattern}"
                            ));
                        }
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

        // Note: lifecycle_rx is passed from outside so it persists across reconnections.
        // Any pending lifecycle events were already drained into pending_subscriptions
        // before this call, and we'll handle new events in the message loop below.

        if *is_first_connect {
            *is_first_connect = false;

            // First connection: snapshot the current stream tips for active rooms
            // and the admin stream so we can catch up from these points if the
            // connection drops later.
            // IMPORTANT: This snapshot is taken AFTER PubSub subscription is
            // established (above), which means any events written to the stream
            // after this point will ALSO be delivered via PubSub. On reconnect,
            // catch-up reads from the snapshotted cursor may re-deliver events
            // that were already processed via PubSub. The MessageDeduplicator
            // handles this overlap, filtering out duplicate events.
            // The alternative (snapshotting before subscription) would create a
            // gap: events written between snapshot and subscription start would
            // be missed by both PubSub (not yet subscribed) and catch-up (cursor
            // already past them). The current order (subscribe first, then
            // snapshot) is correct because duplicates are safe (deduped) while
            // gaps are not.
            let mut streams_to_catchup: Vec<String> = active_rooms
                .iter()
                .map(|rid| self.room_stream_key(rid))
                .collect();
            streams_to_catchup.push(self.admin_stream_key());

            // New node: catch up on recent historical events from each stream.
            // Instead of reading from "0" (all history), we start from `catchup_window_ms`
            // ago to avoid processing a large backlog in big clusters.
            let catchup_start_id = self.catchup_start_id();
            let mut total_caught_up = 0usize;
            let total_skipped = 0usize;

            for stream_key in &streams_to_catchup {
                match process_stream_catchup(
                    self,
                    stream_key,
                    &catchup_start_id,
                    &mut *stream_cursors,
                )
                .await
                {
                    Ok(caught_up) => {
                        total_caught_up += caught_up;
                    }
                    Err(e) => {
                        // Retry catch-up read up to 3 times with short delay before
                        // falling back. Use "0" (stream beginning within catchup
                        // window) instead of "$" so events are not silently skipped.
                        let mut retry_ok = false;
                        for retry in 1_u64..=3 {
                            warn!(
                                error = %e,
                                stream_key = %stream_key,
                                attempt = retry,
                                "Failed to catch up on historical events, retrying"
                            );
                            tokio::time::sleep(Duration::from_millis(500_u64 * retry)).await;
                            match process_stream_catchup(
                                self,
                                stream_key,
                                &catchup_start_id,
                                &mut *stream_cursors,
                            )
                            .await
                            {
                                Ok(caught_up) => {
                                    total_caught_up += caught_up;
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
                .map(|rid| self.room_stream_key(rid))
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
                let key = self.room_stream_key(rid);
                stream_cursors
                    .entry(key)
                    .or_insert_with(|| catchup_start.clone());
            }

            // Build the set of streams to catch up from (active rooms + admin)
            let active_stream_keys: Vec<String> = {
                let mut keys: Vec<String> = active_rooms
                    .iter()
                    .map(|rid| self.room_stream_key(rid))
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

        // Signal readiness AFTER catch-up completes so that callers can rely on
        // the subscriber having processed all historical events before considering
        // it ready. Firing before catch-up (the previous location) could cause
        // events to be published between the signal and snapshot completion,
        // leading to them being delivered via PubSub while catch-up is still
        // in progress and potentially processed out of order.
        if let Some(ready_tx) = subscriber_ready_tx.take() {
            if ready_tx.send(()).is_err() {
                debug!("Redis subscriber readiness receiver was dropped after catch-up");
            }
        }

        // Process incoming messages with dynamic room subscription management.
        // We loop between processing Redis messages and handling lifecycle events.
        // When a lifecycle event arrives, we drop the message stream (releasing the
        // mutable borrow on `pubsub`), perform the subscribe/unsubscribe, then
        // re-create the message stream.
        // A periodic cursor refresh ensures that stream cursors stay up-to-date
        // during long-lived sessions, so reconnect catch-up only reads truly missed
        // events (avoiding replay of events already delivered via live Pub/Sub).
        let mut cursor_refresh_interval = tokio::time::interval(Duration::from_mins(1));
        cursor_refresh_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Skip the first immediate tick
        cursor_refresh_interval.tick().await;

        loop {
            let mut stream = pubsub.on_message();

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
                () = self.cancel_token.cancelled() => SelectResult::Cancelled,
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
                                if subscribed_rooms.insert(room_id) {
                                    let channel = self.room_pubsub_channel(room_id);
                                    match timeout(
                                        Duration::from_secs(REDIS_TIMEOUT_SECS),
                                        pubsub.subscribe(&channel),
                                    )
                                    .await
                                    {
                                        Ok(Ok(())) => {
                                            let sk = self.room_stream_key(room_id);
                                            let catchup_start = self.catchup_start_id();
                                            match self
                                                .read_missed_events_from(&sk, &catchup_start)
                                                .await
                                            {
                                                Ok(events) => {
                                                    let mut last_stream_id = None;
                                                    let caught_up = events.len();
                                                    for (stream_id, channel, event) in events {
                                                        self.dispatch_event(&channel, event).await;
                                                        last_stream_id = Some(stream_id);
                                                    }
                                                    let cursor = match last_stream_id {
                                                        Some(stream_id) => stream_id,
                                                        None => self
                                                            .get_latest_stream_id_for(&sk)
                                                            .await
                                                            .ok()
                                                            .flatten()
                                                            .unwrap_or_else(|| "0".to_string()),
                                                    };
                                                    debug!(
                                                        room_id = %room_id,
                                                        stream_id = %cursor,
                                                        caught_up = caught_up,
                                                        "Dynamically subscribed to room channel and caught up stream"
                                                    );
                                                    stream_cursors.insert(sk, cursor);
                                                }
                                                Err(error) => {
                                                    warn!(
                                                        error = %error,
                                                        room_id = %room_id,
                                                        "Dynamically subscribed but failed to catch up stream, using catchup_start_id"
                                                    );
                                                    stream_cursors.insert(sk, catchup_start);
                                                }
                                            }
                                        }
                                        Ok(Err(e)) => {
                                            warn!(
                                                error = %e,
                                                room_id = %room_id,
                                                "Failed to subscribe to room channel"
                                            );
                                            subscribed_rooms.remove(&room_id);
                                        }
                                        Err(_) => {
                                            warn!(
                                                room_id = %room_id,
                                                "Timed out subscribing to room channel"
                                            );
                                            subscribed_rooms.remove(&room_id);
                                        }
                                    }
                                }
                            }
                            RoomLifecycleEvent::RoomDeactivated(room_id) => {
                                if subscribed_rooms.remove(&room_id) {
                                    let channel = self.room_pubsub_channel(room_id);
                                    match timeout(
                                        Duration::from_secs(REDIS_TIMEOUT_SECS),
                                        pubsub.unsubscribe(&channel),
                                    )
                                    .await
                                    {
                                        Ok(Ok(())) => {
                                            debug!(
                                                room_id = %room_id,
                                                "Dynamically unsubscribed from room channel"
                                            );
                                            stream_cursors.remove(&self.room_stream_key(room_id));
                                        }
                                        Ok(Err(e)) => {
                                            warn!(
                                                error = %e,
                                                room_id = %room_id,
                                                "Failed to unsubscribe from room channel"
                                            );
                                        }
                                        Err(_) => {
                                            warn!(
                                                room_id = %room_id,
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
                    let mut keys_to_refresh: Vec<String> = stream_cursors.keys().cloned().collect();
                    keys_to_refresh.sort_unstable();
                    match self.get_latest_stream_ids_for(&keys_to_refresh).await {
                        Ok(results) => {
                            let mut updates = Vec::with_capacity(results.len());
                            for (stream_key, result) in keys_to_refresh.into_iter().zip(results) {
                                match result {
                                    Ok(Some(id)) => updates.push((stream_key, id)),
                                    Ok(None) => {}
                                    Err(error) => debug!(
                                        error = %error,
                                        %stream_key,
                                        "Failed to refresh stream cursor, keeping existing"
                                    ),
                                }
                            }
                            let updated = updates.len();
                            stream_cursors.extend(updates);
                            if updated > 0 {
                                debug!(
                                    updated_cursors = updated,
                                    "Periodic stream cursor refresh completed"
                                );
                            }
                        }
                        Err(error) => debug!(
                            error = %error,
                            "Periodic stream cursor refresh failed, keeping existing cursors"
                        ),
                    }
                }
                SelectResult::Cancelled => return SubscriberExit::Cancelled,
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
        subscribed_rooms: &mut HashSet<RoomId>,
        stream_cursors: &mut HashMap<String, String>,
    ) {
        let active_rooms: HashSet<RoomId> =
            self.message_hub.active_room_ids().into_iter().collect();

        // Subscribe to newly active rooms
        let new_rooms: Vec<RoomId> = active_rooms.difference(subscribed_rooms).copied().collect();
        for room_id in new_rooms {
            let channel = self.room_pubsub_channel(room_id);
            match timeout(
                Duration::from_secs(REDIS_TIMEOUT_SECS),
                pubsub.subscribe(&channel),
            )
            .await
            {
                Ok(Ok(())) => {
                    subscribed_rooms.insert(room_id);
                    // Snapshot stream cursor for the newly subscribed room so that
                    // reconnect catch-up reads from the right position instead of "0".
                    let sk = self.room_stream_key(room_id);
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

        // Unsubscribe from deactivated rooms. Keep failed unsubscribes tracked
        // locally so the next resync can retry instead of forgetting the live
        // Redis subscription.
        let stale_rooms: Vec<RoomId> = subscribed_rooms
            .difference(&active_rooms)
            .copied()
            .collect();
        let mut successfully_unsubscribed = Vec::new();
        for room_id in stale_rooms {
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
                    successfully_unsubscribed.push(room_id);
                }
                _ => {
                    warn!(room_id = %room_id, "Re-sync: failed to unsubscribe from room channel");
                }
            }
        }
        remove_successfully_unsubscribed_rooms(subscribed_rooms, &successfully_unsubscribed);

        info!(
            subscribed_rooms = subscribed_rooms.len(),
            "Re-synced room subscriptions with hub"
        );
    }

    /// Dispatch a single event received from Redis (either live or from catch-up).
    ///
    /// Handles deduplication, admin channel routing, application-owned remote
    /// event side effects, and local broadcast to room subscribers.
    async fn dispatch_event(&self, channel: &str, event: RealtimeEvent) {
        // Deduplicate events (prevents duplicate delivery during catch-up + live overlap)
        let dedup_key = match DedupKey::try_from_event(&event) {
            Ok(key) => key,
            Err(error) => {
                warn!(
                    channel = %channel,
                    event_type = %event.event_type(),
                    error = %error,
                    "Dropping Redis realtime event with invalid dedup identity"
                );
                return;
            }
        };
        if !self.deduplicator.should_process(&dedup_key) {
            debug!(
                channel = %channel,
                event_type = %event.event_type(),
                "Skipping duplicate event from Redis"
            );
            return;
        }

        // Record received metric
        synctv_core::metrics::cluster::REALTIME_EVENTS_RECEIVED
            .with_label_values(&[event.event_type()])
            .inc();

        debug!(
            channel = %channel,
            event_type = %event.event_type(),
            "Dispatching event from Redis"
        );

        // Handle CacheInvalidate events: notify application-owned handlers and
        // admin subscribers such as resource observers.
        if matches!(&event, RealtimeEvent::CacheInvalidate { .. }) {
            self.handle_remote_event(None, &event).await;
            super::events::publish_admin_event(&self.admin_event_tx, event, "Redis");
            return;
        }

        // Handle admin channel events. Some critical room-scoped lifecycle
        // events intentionally use the admin channel so every replica receives
        // stream/member cleanup even when it has no active room subscribers.
        if self.is_admin_channel(channel) {
            let room_id = event.room_id().copied();
            self.handle_remote_event(room_id, &event).await;
            super::events::publish_admin_event(&self.admin_event_tx, event.clone(), "Redis admin");
            if let Some(room_id) = room_id {
                if matches!(
                    &event,
                    RealtimeEvent::RoomDeleted { .. }
                        | RealtimeEvent::RoomBanned { .. }
                        | RealtimeEvent::RoomOwnerInactive { .. }
                ) {
                    let sent_count = self.message_hub.broadcast_reliably(&room_id, event).await;
                    self.message_hub.remove_room(&room_id);
                    info!(
                        room_id = %room_id,
                        notified = sent_count,
                        "Handled terminal room lifecycle event from admin channel"
                    );
                } else {
                    let sent_count = self.message_hub.broadcast(&room_id, &event);
                    debug!(
                        room_id = %room_id,
                        local_subscribers = sent_count,
                        "Forwarded non-terminal admin room event to local subscribers"
                    );
                }
            }
            return;
        }

        // Extract room_id from channel name ({prefix}room:{room_id})
        if let Some(room_id_str) = self.extract_room_id_from_channel(channel) {
            let Ok(room_id) = room_id_str.parse::<RoomId>() else {
                tracing::warn!(room_id = %room_id_str, "Ignoring invalid room id from Redis pubsub channel");
                return;
            };

            self.handle_remote_event(Some(room_id), &event).await;

            // Forward kick/leave events to admin channel for cross-replica disconnect handling.
            // UserLeft is included so other replicas disconnect the user's connections
            // from the room (same behavior as KickUserFromRoom but with correct semantics).
            if matches!(
                &event,
                RealtimeEvent::KickPublisher { .. }
                    | RealtimeEvent::KickUserFromRoom { .. }
                    | RealtimeEvent::RoomBanned { .. }
                    | RealtimeEvent::RoomOwnerInactive { .. }
                    | RealtimeEvent::UserLeft { .. }
            ) {
                super::events::publish_admin_event(
                    &self.admin_event_tx,
                    event.clone(),
                    "Redis room",
                );
            }

            // Handle terminal room-wide admin events before dropping local room state.
            // Critical delivery must complete before local senders are dropped;
            // otherwise the queued notification can be lost for slow subscribers
            // even though the room cleanup proceeds.
            if matches!(
                &event,
                RealtimeEvent::RoomDeleted { .. }
                    | RealtimeEvent::RoomBanned { .. }
                    | RealtimeEvent::RoomOwnerInactive { .. }
            ) {
                let sent_count = self.message_hub.broadcast_reliably(&room_id, event).await;
                // Remove all local subscriptions for the deleted room
                self.message_hub.remove_room(&room_id);
                info!(
                    room_id = %room_id,
                    notified = sent_count,
                    "Handled RoomDeleted: notified local subscribers and cleaned up room"
                );
                return;
            }

            // Route WebRTC signaling to the specific target connection instead
            // of broadcasting SDP/ICE data to all room subscribers.
            if let RealtimeEvent::WebRTCVoiceSignaling { ref to, .. }
            | RealtimeEvent::WebRTCMediaSignaling { ref to, .. } = event
            {
                let Some((_target_user, target_conn)) = to.rsplit_once(':') else {
                    warn!(
                        room_id = %room_id,
                        target = %to,
                        "Dropping malformed WebRTC signaling target"
                    );
                    return;
                };
                let target_conn = target_conn.to_string();
                let sent = self
                    .message_hub
                    .broadcast_to_connection(&room_id, &target_conn, event)
                    .await;
                debug!(
                        room_id = %room_id,
                        target_connection = %target_conn,
                        sent = sent,
                        "Routed WebRTC signaling to specific connection"
                );
                return;
            }

            // Broadcast to local subscribers
            let sent_count = self.message_hub.broadcast(&room_id, &event);

            debug!(
                room_id = %room_id,
                local_subscribers = sent_count,
                "Forwarded Redis event to local subscribers"
            );
        } else {
            warn!(channel = %channel, "Invalid channel format");
        }
    }

    async fn handle_remote_event(&self, room_id: Option<RoomId>, event: &RealtimeEvent) {
        if let Some(handler) = &self.event_handler {
            handler.handle_remote_event(room_id, event).await;
        }
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
        event: &RealtimeEvent,
        room_stream_ttl_secs: u64,
        stream_max_length: usize,
    ) -> Result<usize> {
        let force_admin_channel = event.delivers_to_admin_channel();
        let channel = if force_admin_channel {
            format!("{key_prefix}admin:events")
        } else if let Some(room_id) = event.room_id() {
            format!("{key_prefix}room:{room_id}")
        } else {
            format!("{key_prefix}admin:events")
        };

        // Wrap event in envelope with node_id
        let envelope = EventEnvelopeRef { node_id, event };

        let payload =
            serde_json::to_string(&envelope).context("Failed to serialize event envelope")?;

        // Stream key for reliable delivery (catch-up after disconnect)
        // Room events go to {prefix}room:{room_id}:events, admin events to {prefix}admin:events:stream
        let stream_key = if force_admin_channel {
            format!("{key_prefix}admin:events:stream")
        } else if let Some(room_id) = event.room_id() {
            format!("{key_prefix}room:{room_id}:events")
        } else {
            format!("{key_prefix}admin:events:stream")
        };
        let stream_ttl_secs = (!force_admin_channel)
            .then(|| event.room_id().map(|_| room_stream_ttl_secs))
            .flatten();

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
                    stream_ttl_secs,
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
            synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
                .with_label_values(&["stream_write_failed"])
                .inc();
            // Fall through: return error so the caller can buffer for retry
            anyhow::bail!("Critical event publish failed after retries");
        }

        // Non-critical events: single atomic attempt
        match Self::publish_event_atomic(
            conn,
            &stream_key,
            &channel,
            &payload,
            stream_ttl_secs,
            stream_max_length,
        )
        .await
        {
            Ok(subscribers) => Ok(subscribers),
            Err(e) => {
                warn!(
                    error = %e,
                    stream_key = %stream_key,
                    "Atomic XADD+PUBLISH failed for non-critical event"
                );
                synctv_core::metrics::cluster::REALTIME_EVENTS_RECEIVED
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
        stream_ttl_secs: Option<u64>,
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
        if let Some(ttl_secs) = stream_ttl_secs {
            pipe.cmd("EXPIRE").arg(stream_key).arg(ttl_secs);
        }
        pipe.publish::<_, _>(channel, payload);

        let subscriber_count = if stream_ttl_secs.is_some() {
            let results: (String, bool, usize) = timeout(
                Duration::from_secs(REDIS_TIMEOUT_SECS),
                pipe.query_async(conn),
            )
            .await
            .context("Timed out executing atomic XADD+PUBLISH")?
            .context("Failed to execute atomic XADD+PUBLISH")?;
            results.2
        } else {
            let results: (String, usize) = timeout(
                Duration::from_secs(REDIS_TIMEOUT_SECS),
                pipe.query_async(conn),
            )
            .await
            .context("Timed out executing atomic XADD+PUBLISH")?
            .context("Failed to execute atomic XADD+PUBLISH")?;
            results.1
        };

        Ok(subscriber_count)
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
                self.redis_runtime
                    .multiplexed_connection()
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

    async fn get_latest_stream_ids_for(
        &self,
        stream_keys: &[String],
    ) -> Result<Vec<redis::RedisResult<Option<String>>>> {
        use redis::streams::StreamRangeReply;

        let mut conn = self.get_shared_conn().await?;
        let mut results = Vec::with_capacity(stream_keys.len());
        for stream_keys in stream_keys.chunks(CURSOR_REFRESH_BATCH_SIZE) {
            let mut pipe = redis::pipe();
            pipe.ignore_errors();
            for stream_key in stream_keys {
                pipe.cmd("XREVRANGE")
                    .arg(stream_key)
                    .arg("+")
                    .arg("-")
                    .arg("COUNT")
                    .arg(1);
            }
            let replies: Vec<redis::RedisResult<StreamRangeReply>> = tokio::select! {
                () = self.cancel_token.cancelled() => {
                    return Err(anyhow::anyhow!("Redis stream cursor refresh cancelled"));
                }
                result = timeout(
                    Duration::from_secs(REDIS_TIMEOUT_SECS),
                    pipe.query_async(&mut conn),
                ) => {
                    result
                        .context("Timed out refreshing Redis stream cursors")?
                        .context("Failed to refresh Redis stream cursors")?
                }
            };
            if replies.len() != stream_keys.len() {
                return Err(anyhow::anyhow!(
                    "Redis stream cursor refresh returned {} results for {} streams",
                    replies.len(),
                    stream_keys.len()
                ));
            }
            results.extend(replies.into_iter().map(|reply| {
                reply.map(|reply| reply.ids.into_iter().next().map(|entry| entry.id))
            }));
        }
        Ok(results)
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
    ) -> Result<Vec<(String, String, RealtimeEvent)>> {
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
                    cursor.clone_from(&entry.id);

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

#[async_trait::async_trait]
impl RealtimeMessageTransport for RedisPubSub {
    async fn start(
        self: Arc<Self>,
        publish_channel_capacity: usize,
    ) -> crate::error::Result<RealtimeMessageTransportRuntime> {
        let (publish_tx, _backpressure, publisher_handle) =
            RedisPubSub::start(self, publish_channel_capacity).await?;
        Ok(RealtimeMessageTransportRuntime {
            publish_tx,
            publisher_handle,
        })
    }

    async fn shutdown(&self) {
        RedisPubSub::shutdown(self).await;
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
    Cancelled,
}

/// Request to publish an event.
/// The channel is derived from `event.room_id()` in `publish_event`.
pub struct PublishRequest {
    pub event: RealtimeEvent,
    ack: Option<oneshot::Sender<std::result::Result<(), String>>>,
}

impl PublishRequest {
    #[must_use]
    pub const fn new(event: RealtimeEvent) -> Self {
        Self { event, ack: None }
    }

    #[must_use]
    pub fn with_ack(
        event: RealtimeEvent,
    ) -> (Self, oneshot::Receiver<std::result::Result<(), String>>) {
        let (tx, rx) = oneshot::channel();
        (
            Self {
                event,
                ack: Some(tx),
            },
            rx,
        )
    }

    pub fn acknowledge_success(&mut self) {
        if let Some(ack) = self.ack.take() {
            if ack.send(Ok(())).is_err() {
                debug!(
                    event_type = %self.event.event_type(),
                    "Redis publish ack receiver dropped before success acknowledgement"
                );
            }
        }
    }

    pub fn acknowledge_failure(&mut self, error: impl Into<String>) {
        if let Some(ack) = self.ack.take() {
            let error = error.into();
            if ack.send(Err(error.clone())).is_err() {
                debug!(
                    event_type = %self.event.event_type(),
                    error = %error,
                    "Redis publish ack receiver dropped before failure acknowledgement"
                );
            }
        }
    }

    fn expects_ack(&self) -> bool {
        self.ack.is_some()
    }
}

fn push_critical_retry_buffer(buffer: &mut VecDeque<PublishRequest>, req: PublishRequest) {
    if buffer.len() >= MAX_CRITICAL_BUFFER {
        let mut dropped = buffer
            .pop_front()
            .expect("full critical retry buffer must contain an oldest request");
        warn!(
            critical_buffer_len = buffer.len(),
            max = MAX_CRITICAL_BUFFER,
            "Critical event buffer full, dropping oldest event"
        );
        synctv_core::metrics::cluster::REALTIME_EVENTS_DROPPED
            .with_label_values(&["critical_retry_buffer_full"])
            .inc();
        dropped.acknowledge_failure("Redis critical retry buffer full");
    }
    buffer.push_back(req);
}

async fn retry_publish_batch(
    batch: Vec<PublishRequest>,
    conn: &mut redis::aio::MultiplexedConnection,
    node_id: &str,
    key_prefix: &str,
    room_stream_ttl_secs: u64,
    stream_max_length: usize,
) -> (Vec<PublishRequest>, usize) {
    let mut failed = Vec::new();
    let mut success_count = 0;
    for mut req in batch {
        let event_type = req.event.event_type();
        match RedisPubSub::publish_event(
            conn,
            node_id,
            key_prefix,
            &req.event,
            room_stream_ttl_secs,
            stream_max_length,
        )
        .await
        {
            Ok(subscribers) => {
                success_count += 1;
                req.acknowledge_success();
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

/// Envelope for events published to Redis
/// Includes `node_id` to avoid echo (each node ignores its own events)
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct EventEnvelope {
    node_id: String,
    event: RealtimeEvent,
}

#[derive(serde::Serialize)]
struct EventEnvelopeRef<'a> {
    node_id: &'a str,
    event: &'a RealtimeEvent,
}

#[cfg(test)]
mod tests;
