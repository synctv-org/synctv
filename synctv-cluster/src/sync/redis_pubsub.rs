use anyhow::{Context, Result};
use futures::stream::StreamExt;
use redis::{AsyncCommands, Client as RedisClient};
use redis::streams::StreamReadReply;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Timeout for Redis operations in seconds
const REDIS_TIMEOUT_SECS: u64 = 5;

/// Returns `true` if the Redis error indicates the current connection has become
/// read-only or is still loading data — both symptoms of a Sentinel failover in
/// progress.  When detected, callers should drop the connection and reconnect
/// immediately rather than treating the error as a retryable publish failure.
fn is_sentinel_failover_error(e: &anyhow::Error) -> bool {
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

/// Milliseconds to wait after broadcasting a RoomDeleted event before removing
/// room subscriptions, giving WebSocket read loops time to drain queued messages.
const ROOM_DELETED_BROADCAST_DRAIN_MS: u64 = 100;

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
    /// Avoids creating a fresh connection for every get_latest_stream_id / read_missed_events call.
    shared_conn: tokio::sync::Mutex<Option<redis::aio::MultiplexedConnection>>,
    /// Timestamp of last successful connection health check (Unix seconds)
    last_health_check: AtomicU64,
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
        Self::with_key_prefix(redis_client, message_hub, node_id, "synctv:", admin_event_tx, permission_service, cache_invalidation, deduplicator, 300, DEFAULT_MAX_STREAM_LENGTH)
    }

    /// Create a new `RedisPubSub` service with a custom key prefix.
    ///
    /// `catchup_window_secs` controls how far back to replay Redis Stream events
    /// when this node first connects.  Pass `300` for the default (5 minutes).
    /// `stream_max_length` controls the maximum number of entries per Redis Stream.
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
            shared_conn: tokio::sync::Mutex::new(None),
            last_health_check: AtomicU64::new(0),
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

    /// Shut down the Pub/Sub service (cancels subscriber and publisher tasks)
    pub fn shutdown(&self) {
        info!("Shutting down RedisPubSub service");
        self.cancel_token.cancel();
    }

    /// Start the Pub/Sub service
    /// This spawns a background task that subscribes to all room channels
    ///
    /// # Arguments
    /// * `publish_channel_capacity` - Capacity for the publish channel. Events are
    ///   dropped with a warning when full (e.g., during a prolonged Redis outage).
    pub async fn start(self: Arc<Self>, publish_channel_capacity: usize) -> Result<(mpsc::Sender<PublishRequest>, tokio::task::JoinHandle<()>)> {
        // Create bounded channel for publishing events to prevent OOM under Redis outage
        let (publish_tx, mut publish_rx) = mpsc::channel::<PublishRequest>(publish_channel_capacity);

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

        // Spawn task to handle publishing with reconnection logic.
        // The handle is returned to the caller so shutdown() can await completion.
        let publisher_handle = tokio::spawn(async move {
            let mut backoff_secs = INITIAL_BACKOFF_SECS;
            // Buffer for retrying failed publishes after reconnection.
            // Using a Vec instead of Option<PublishRequest> ensures that multiple
            // events that fail during a connection interruption window are all
            // preserved for retry, not just the last one.
            let mut retry_buffer: Vec<PublishRequest> = Vec::new();

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
                        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                        continue;
                    }
                    Err(_) => {
                        error!(
                            backoff_secs = backoff_secs,
                            "Timed out getting Redis connection for publishing, retrying"
                        );
                        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                        continue;
                    }
                };

                info!("Redis publisher task (re)connected");
                let mut conn = conn;

                // Retry all buffered failed publish requests
                if !retry_buffer.is_empty() {
                    let buffered = std::mem::take(&mut retry_buffer);
                    info!(
                        buffered_count = buffered.len(),
                        "Retrying buffered events after reconnection"
                    );
                    let mut retry_failed = false;
                    for req in buffered {
                        if retry_failed {
                            // Connection broke mid-retry; keep remaining events
                            retry_buffer.push(req);
                            continue;
                        }
                        let event_type = req.event.event_type();
                        match Self::publish_event(&mut conn, &node_id, &key_prefix, req.event.clone(), stream_max_length).await {
                            Ok(subscribers) => {
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
                                retry_buffer.push(req);
                                retry_failed = true;
                            }
                        }
                    }
                    if !retry_buffer.is_empty() {
                        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                        continue;
                    }
                }

                // Track whether this session was healthy (at least one event sent)
                let mut session_healthy = false;

                // Process events until connection breaks or cancelled
                loop {
                    let req = tokio::select! {
                        () = cancel_publisher.cancelled() => {
                            // Flush retry_buffer first, then drain channel
                            info!(
                                retry_buffer_len = retry_buffer.len(),
                                "Redis publisher task cancelled, flushing retry buffer and draining remaining events"
                            );
                            let mut flush_failed = false;
                            // Flush retry_buffer (events from previous failed publishes)
                            for req in std::mem::take(&mut retry_buffer) {
                                if flush_failed {
                                    // Connection broken; skip remaining events
                                    if req.event.is_critical() {
                                        warn!(
                                            event_type = req.event.event_type(),
                                            "Critical retry_buffer event lost during shutdown (connection broken)"
                                        );
                                    }
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
                            // Then drain remaining events from channel
                            while let Ok(req) = publish_rx.try_recv() {
                                if flush_failed {
                                    if req.event.is_critical() {
                                        warn!(
                                            event_type = req.event.event_type(),
                                            "Critical drained event lost during shutdown (connection broken)"
                                        );
                                    }
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
                        match Self::publish_event(&mut conn, &node_id, &key_prefix, req.event.clone(), stream_max_length).await {
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
                                // Buffer failed request for retry after reconnection
                                retry_buffer.push(req);

                                // Drain remaining events from channel into retry buffer
                                // (connection is broken, no point trying to publish more)
                                while let Ok(req) = publish_rx.try_recv() {
                                    if retry_buffer.len() >= MAX_RETRY_BUFFER {
                                        let event_type = req.event.event_type();
                                        let is_critical = req.event.is_critical();
                                        if is_critical {
                                            error!(
                                                max = MAX_RETRY_BUFFER,
                                                event_type = event_type,
                                                "CRITICAL EVENT DROPPED: retry buffer full, dropping critical event"
                                            );
                                            synctv_core::metrics::cluster::CLUSTER_EVENTS_DROPPED
                                                .with_label_values(&["critical_retry_buffer_full"])
                                                .inc();
                                        } else {
                                            warn!(
                                                max = MAX_RETRY_BUFFER,
                                                event_type = event_type,
                                                "Retry buffer full, dropping event"
                                            );
                                        }
                                        synctv_core::metrics::cluster::CLUSTER_EVENTS_DROPPED
                                            .with_label_values(&["retry_buffer_full"])
                                            .inc();
                                        break;
                                    }
                                    retry_buffer.push(req);
                                }
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
        let self_clone = self;
        let cancel_subscriber = self_clone.cancel_token.clone();

        // Spawn task to handle subscribing with exponential backoff on reconnection
        tokio::spawn(async move {
            let mut backoff_secs = INITIAL_BACKOFF_SECS;
            // Track per-stream cursors (per-room + admin) across reconnections.
            // On first connect, cursors are snapshotted from stream tips.
            // On reconnect, catch-up reads only active rooms' streams.
            let mut stream_cursors: HashMap<String, String> = HashMap::new();
            let mut is_first_connect = true;

            loop {
                // Check cancellation before each reconnect attempt
                if cancel_subscriber.is_cancelled() {
                    info!("Redis subscriber task cancelled");
                    return;
                }

                match self_clone.run_subscriber(&mut stream_cursors, &mut is_first_connect).await {
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

        Ok((publish_tx, publisher_handle))
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
    /// Returns `SubscriberExit::Disconnected` if the connection was established but then
    /// the stream ended (Redis disconnected). Returns `SubscriberExit::ConnectFailed` if
    /// the initial connection or subscription failed.
    async fn run_subscriber(
        &self,
        stream_cursors: &mut HashMap<String, String>,
        is_first_connect: &mut bool,
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
                    anyhow::anyhow!(e).context(format!("Failed to subscribe to {admin_pattern} pattern")),
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
        let active_rooms = self.message_hub.active_room_ids();
        let mut subscribed_rooms: HashSet<String> = HashSet::new();

        if !active_rooms.is_empty() {
            let room_channels: Vec<String> = active_rooms
                .iter()
                .map(|rid| self.room_pubsub_channel(rid.as_str()))
                .collect();
            let channel_refs: Vec<&str> = room_channels.iter().map(|s| s.as_str()).collect();

            match timeout(
                Duration::from_secs(REDIS_TIMEOUT_SECS),
                pubsub.subscribe(channel_refs.as_slice()),
            )
            .await
            {
                Ok(Ok(())) => {
                    for rid in &active_rooms {
                        subscribed_rooms.insert(rid.as_str().to_string());
                    }
                }
                Ok(Err(e)) => {
                    warn!(
                        error = %e,
                        room_count = active_rooms.len(),
                        "Failed to subscribe to room channels, falling back to pattern"
                    );
                    // Fallback: use pattern subscription if individual subscribes fail
                    let room_pattern = self.room_pubsub_pattern();
                    if let Err(e) = pubsub.psubscribe(&room_pattern).await {
                        return SubscriberExit::ConnectFailed(
                            anyhow::anyhow!(e).context(format!("Failed to fallback psubscribe to {room_pattern}")),
                        );
                    }
                }
                Err(_) => {
                    warn!(
                        room_count = active_rooms.len(),
                        "Timed out subscribing to room channels, falling back to pattern"
                    );
                    let room_pattern = self.room_pubsub_pattern();
                    if let Err(e) = pubsub.psubscribe(&room_pattern).await {
                        return SubscriberExit::ConnectFailed(
                            anyhow::anyhow!(e).context(format!("Failed to fallback psubscribe to {room_pattern}")),
                        );
                    }
                }
            }
        }

        info!(
            subscribed_rooms = subscribed_rooms.len(),
            "Redis subscriber connected, listening to {} pattern and {} room channels",
            admin_pattern,
            subscribed_rooms.len()
        );

        // Subscribe to room lifecycle events for dynamic channel management
        let mut lifecycle_rx = self.message_hub.subscribe_lifecycle();

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
            let mut total_skipped = 0usize;
            for stream_key in &streams_to_catchup {
                match self.read_missed_events_from(stream_key, &catchup_start_id).await {
                    Ok(events) => {
                        for (stream_id, channel, event) in events {
                            let dedup_key = DedupKey::from_event(&event);
                            if self.deduplicator.should_process(&dedup_key) {
                                self.dispatch_event(&channel, event).await;
                                total_caught_up += 1;
                            } else {
                                total_skipped += 1;
                            }
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
                            match self.read_missed_events_from(stream_key, &catchup_start_id).await {
                                Ok(events) => {
                                    for (stream_id, channel, event) in events {
                                        let dedup_key = DedupKey::from_event(&event);
                                        if self.deduplicator.should_process(&dedup_key) {
                                            self.dispatch_event(&channel, event).await;
                                            total_caught_up += 1;
                                        } else {
                                            total_skipped += 1;
                                        }
                                        stream_cursors.insert(stream_key.clone(), stream_id);
                                    }
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
            stream_cursors.retain(|key, _| {
                *key == admin_sk || active_stream_keys_set.contains(key)
            });

            // Ensure admin stream is always included
            if !stream_cursors.contains_key(&admin_sk) {
                stream_cursors.insert(admin_sk.clone(), "0".to_string());
            }

            // Add cursors for any new rooms that appeared while disconnected
            for rid in &active_rooms {
                let key = self.room_stream_key(rid.as_str());
                stream_cursors.entry(key).or_insert_with(|| "0".to_string());
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
                let cursor = stream_cursors.get(stream_key).cloned().unwrap_or_else(|| "0".to_string());
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
        let mut cursor_refresh_interval = tokio::time::interval(Duration::from_secs(60));
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
                                    ).await {
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
                                                        "Dynamically subscribed but failed to snapshot cursor, using '$' (skip catch-up)"
                                                    );
                                                    stream_cursors.insert(sk, "$".to_string());
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
                                    ).await {
                                        Ok(Ok(())) => {
                                            debug!(
                                                room_id = %room_id_str,
                                                "Dynamically unsubscribed from room channel"
                                            );
                                            stream_cursors.remove(&self.room_stream_key(&room_id_str));
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
        for room_id in active_rooms.difference(subscribed_rooms).cloned().collect::<Vec<_>>() {
            let channel = self.room_pubsub_channel(&room_id);
            match timeout(Duration::from_secs(REDIS_TIMEOUT_SECS), pubsub.subscribe(&channel)).await {
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
                                "Re-synced: subscribed but failed to snapshot cursor, using '$'"
                            );
                            stream_cursors.insert(sk, "$".to_string());
                        }
                    }
                }
                _ => {
                    warn!(room_id = %room_id, "Re-sync: failed to subscribe to room channel");
                }
            }
        }

        // Unsubscribe from deactivated rooms
        for room_id in subscribed_rooms.difference(&active_rooms).cloned().collect::<Vec<_>>() {
            let channel = self.room_pubsub_channel(&room_id);
            match timeout(Duration::from_secs(REDIS_TIMEOUT_SECS), pubsub.unsubscribe(&channel)).await {
                Ok(Ok(())) => {
                    debug!(room_id = %room_id, "Re-synced: unsubscribed from room channel");
                    stream_cursors.remove(&self.room_stream_key(&room_id));
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
            if matches!(&event, ClusterEvent::KickPublisher { .. } | ClusterEvent::KickUserFromRoom { .. } | ClusterEvent::UserLeft { .. }) {
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
                    ClusterEvent::RoomSettingsChanged { .. }
                    | ClusterEvent::RoomDeleted { .. } => {
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
                    ClusterEvent::RoomSettingsChanged { .. }
                    | ClusterEvent::RoomCreated { .. } => {
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
                            if let Err(e) = cache_svc.broadcast_local(InvalidationMessage::PlaybackState {
                                room_id: room_id.as_str().to_string(),
                            }) {
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
            // A small delay between broadcast and remove_room ensures that the
            // WebSocket read loops have time to dequeue and forward the RoomDeleted
            // event to clients before the senders are dropped.
            if matches!(&event, ClusterEvent::RoomDeleted { .. }) {
                // Notify local subscribers so WebSocket clients learn the room is gone
                let sent_count = self.message_hub.broadcast(&room_id, event);
                // Allow WebSocket tasks to process the queued event before cleanup
                tokio::time::sleep(Duration::from_millis(ROOM_DELETED_BROADCAST_DRAIN_MS)).await;
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
            // broadcasting to all subscribers. The `to` field is formatted as
            // "user_id:conn_id" -- we parse the conn_id and use targeted delivery.
            if let ClusterEvent::WebRTCSignaling { ref to, .. } = event {
                let to_owned = to.clone();
                // Parse "user_id:conn_id" format
                if let Some((_target_user, target_conn)) = to_owned.rsplit_once(':') {
                    let target_conn = target_conn.to_string();
                    let sent = self.message_hub.broadcast_to_connection(
                        &room_id,
                        &target_conn,
                        event,
                    );
                    debug!(
                        room_id = %room_id.as_str(),
                        target_connection = %target_conn,
                        sent = sent,
                        "Routed WebRTC signaling to specific connection"
                    );
                } else {
                    // Fallback: if `to` doesn't contain ':', broadcast to user
                    let target_user_id = synctv_core::models::UserId::from_string(to_owned.clone());
                    let sent = self.message_hub.broadcast_to_user(&room_id, &target_user_id, event);
                    debug!(
                        room_id = %room_id.as_str(),
                        target_user = %to_owned,
                        sent = sent,
                        "Routed WebRTC signaling to user (no conn_id)"
                    );
                }
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
                    conn, &stream_key, &channel, &payload, stream_max_length,
                ).await;

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
            synctv_core::metrics::cluster::CLUSTER_EVENTS_RECEIVED
                .with_label_values(&["stream_write_failed"])
                .inc();
            // Fall through: return error so the caller can buffer for retry
            anyhow::bail!("Critical event publish failed after retries");
        }

        // Non-critical events: single atomic attempt
        match Self::publish_event_atomic(conn, &stream_key, &channel, &payload, stream_max_length).await {
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
    /// Includes periodic PING health checks (every 30s) to detect stale connections
    /// early, matching the pattern used by `NodeRegistry::get_conn()`.
    async fn get_shared_conn(&self) -> Result<redis::aio::MultiplexedConnection> {
        const HEALTH_CHECK_INTERVAL_SECS: u64 = 30;

        let mut guard = self.shared_conn.lock().await;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let last_check = self.last_health_check.load(Ordering::Relaxed);
        let needs_health_check = now.saturating_sub(last_check) >= HEALTH_CHECK_INTERVAL_SECS;

        if let Some(ref conn) = *guard {
            if !needs_health_check {
                return Ok(conn.clone());
            }

            // Perform health check with PING
            let mut conn_clone = conn.clone();
            drop(guard); // Release lock during PING

            let ping_result = timeout(
                Duration::from_secs(2),
                redis::cmd("PING").query_async::<String>(&mut conn_clone),
            ).await;

            guard = self.shared_conn.lock().await; // Re-acquire

            match ping_result {
                Ok(Ok(_)) => {
                    self.last_health_check.store(now, Ordering::Relaxed);
                    if let Some(ref current_conn) = *guard {
                        return Ok(current_conn.clone());
                    }
                    // Connection was cleared while we released the lock, fall through
                }
                Ok(Err(ref e)) => {
                    debug!("Redis shared connection PING failed: {}, reconnecting", e);
                    *guard = None;
                }
                Err(_) => {
                    debug!("Redis shared connection PING timeout, reconnecting");
                    *guard = None;
                }
            }
        }

        let conn = self.redis_client
            .get_multiplexed_async_connection()
            .await
            .context("Failed to get Redis shared connection")?;
        *guard = Some(conn.clone());
        self.last_health_check.store(now, Ordering::Relaxed);
        Ok(conn)
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

                    let channel = entry.map.get("channel")
                        .and_then(|v| redis::from_redis_value::<String>(v.clone()).ok());
                    let payload = entry.map.get("payload")
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

    // Integration tests require Redis running
    #[tokio::test]
    #[ignore = "Requires Redis server"]
    async fn test_pubsub_integration() {
        let redis_url = "redis://127.0.0.1:6379";
        let redis_client = RedisClient::open(redis_url).unwrap();
        let message_hub = Arc::new(RoomMessageHub::new());

        let (admin_tx, _) = broadcast::channel(256);

        // Create two PubSub instances simulating different nodes
        let dedup1 = Arc::new(MessageDeduplicator::with_defaults());
        let dedup2 = Arc::new(MessageDeduplicator::with_defaults());
        let pubsub1 = Arc::new(
            RedisPubSub::new(redis_client.clone(), message_hub.clone(), "node1".to_string(), admin_tx.clone(), None, None, dedup1).unwrap(),
        );
        let pubsub2 = Arc::new(
            RedisPubSub::new(redis_client, message_hub.clone(), "node2".to_string(), admin_tx.clone(), None, None, dedup2).unwrap(),
        );

        // Start both
        let publish_tx1 = pubsub1.start(10_000).await.unwrap();
        let _publish_tx2 = pubsub2.start(10_000).await.unwrap();

        // Wait for connections to establish
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Subscribe a client to the room
        let room_id = RoomId::from_string("test_room".to_string());
        let user_id = UserId::from_string("test_user".to_string());
        let rx = message_hub.subscribe(room_id.clone(), user_id.clone(), "conn1".to_string());

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

        publish_tx1
            .send(PublishRequest {
                event,
            })
            .await
            .unwrap();

        // Wait for event propagation
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Client should receive the event
        let mut rx = rx.await;
        let received = tokio::time::timeout(tokio::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(received.event_type(), "chat_message");
    }
}
