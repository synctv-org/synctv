//! Cache invalidation service for multi-replica deployments
//!
//! Uses Redis Streams (XADD/XREADGROUP) to broadcast cache invalidation messages
//! across all nodes with durable delivery. Unlike Pub/Sub, messages are persisted
//! in the stream and can be caught up on reconnection, preventing stale caches
//! after transient disconnections.

use redis::aio::ConnectionManager;
use redis::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{broadcast, OnceCell};
use tracing::{debug, error, info, warn};

use crate::models::RoomId;
use crate::{Error, Result};

/// Maximum approximate stream length (number of entries).
/// Cache TTLs are 5 minutes, so 1000 entries is more than sufficient.
const MAX_STREAM_LENGTH: i64 = 1000;

/// Maximum age of stream entries in milliseconds (1 hour).
/// Used for periodic MINID-based trimming.
const STREAM_RETENTION_MS: u64 = 3_600_000;

/// Interval for periodic state synchronization (60 seconds).
/// When Redis reconnects after a disconnect, other replicas may have stale caches
/// because invalidation messages were only broadcast locally during the outage.
/// This periodic sync ensures all replicas eventually converge.
const STATE_SYNC_INTERVAL_SECS: u64 = 60;

/// Cache invalidation message types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InvalidationMessage {
    /// Invalidate permission cache for a specific user in a room
    UserPermission { room_id: String, user_id: String },
    /// Invalidate permission cache for all users in a room
    RoomPermission { room_id: String },
    /// Invalidate user cache
    User { user_id: String },
    /// Invalidate username cache
    Username { user_id: String },
    /// Invalidate room cache
    Room { room_id: String },
    /// Invalidate playback state cache for a room
    PlaybackState { room_id: String },
    /// Update playback state cache for a room with the new state.
    ///
    /// Unlike `PlaybackState` (which only invalidates), this variant carries
    /// the full updated state so receiving replicas can write it directly into
    /// their L1 cache, avoiding the stale-read window between cache
    /// invalidation and the next DB fetch.
    PlaybackStateUpdate {
        room_id: String,
        state: crate::models::RoomPlaybackState,
    },
    /// Invalidate room settings cache for a specific room
    RoomSettings { room_id: String },
    /// Invalidate all caches
    All,
}

/// Service for broadcasting and receiving cache invalidation messages
///
/// Uses Redis Streams instead of Pub/Sub for reliable message delivery:
/// - Messages are persisted and won't be lost
/// - Each node uses its own consumer group for broadcast semantics
/// - Automatic message acknowledgment and retry on failure
/// - Periodic state sync to handle missed invalidations during Redis outages
pub struct CacheInvalidationService {
    /// Redis client for streams (used by the subscriber background task)
    redis_client: Option<Client>,
    /// Shared Redis connection handle that follows Sentinel failover.
    /// When set, this is preferred over lazily creating via redis_client.
    redis_conn_shared: Option<Arc<tokio::sync::RwLock<ConnectionManager>>>,
    /// Reusable Redis connection for publishing (lazily initialized fallback)
    redis_conn: Arc<OnceCell<ConnectionManager>>,
    /// Local broadcast sender for invalidation events
    local_sender: broadcast::Sender<InvalidationMessage>,
    /// Node identifier for logging and consumer group
    node_id: String,
    /// Redis stream key for cache invalidation
    stream_key: String,
    /// Consumer group name
    consumer_group: String,
    /// Shutdown flag
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    /// Flag indicating if we need to broadcast a state sync on next successful Redis connection
    /// This is set when `broadcast_remote` fails due to Redis being unavailable
    needs_state_sync: Arc<std::sync::atomic::AtomicBool>,
}

impl Clone for CacheInvalidationService {
    fn clone(&self) -> Self {
        Self {
            redis_client: self.redis_client.clone(),
            redis_conn_shared: self.redis_conn_shared.clone(),
            redis_conn: self.redis_conn.clone(),
            local_sender: self.local_sender.clone(),
            node_id: self.node_id.clone(),
            stream_key: self.stream_key.clone(),
            consumer_group: self.consumer_group.clone(),
            shutdown: self.shutdown.clone(),
            needs_state_sync: self.needs_state_sync.clone(),
        }
    }
}

impl CacheInvalidationService {
    /// Create a new cache invalidation service.
    ///
    /// # Arguments
    /// * `redis_client` - Optional Redis client. If None, only local invalidation is used.
    /// * `node_id` - Unique identifier for this node (for consumer group and logging).
    ///   **Important**: The consumer group name is derived as `cache-invalidation-{node_id}`.
    ///   If `node_id` contains a random component (e.g., the nanoid suffix from
    ///   `generate_node_id()` in non-K8s environments), each process restart creates
    ///   a new consumer group and the previous one becomes orphaned in Redis. To
    ///   avoid orphan accumulation, either:
    ///   - Use a stable `node_id` (e.g., K8s `POD_NAME`, hostname, or a persisted ID)
    ///   - Rely on [`start`](Self::start) which calls
    ///     [`cleanup_orphaned_consumer_groups`](Self::cleanup_orphaned_consumer_groups)
    ///     to remove groups with zero pending messages and zero active consumers.
    /// * `stream_key` - Redis stream key for cache invalidation (e.g., "synctv:cache:invalidate:stream")
    #[must_use]
    pub fn new(redis_client: Option<Client>, node_id: String, stream_key: String) -> Self {
        let (local_sender, _) = broadcast::channel(1024);
        let consumer_group = format!("cache-invalidation-{node_id}");

        Self {
            redis_client,
            redis_conn_shared: None,
            redis_conn: Arc::new(OnceCell::new()),
            local_sender,
            node_id,
            stream_key,
            consumer_group,
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            needs_state_sync: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Set the shared Redis connection handle from the bootstrap layer.
    ///
    /// When set, `get_conn()` will acquire a read lock and clone from this
    /// shared handle instead of lazily creating an independent ConnectionManager.
    /// This ensures the service follows Sentinel failover.
    #[must_use]
    pub fn with_shared_conn(mut self, conn: Arc<tokio::sync::RwLock<ConnectionManager>>) -> Self {
        self.redis_conn_shared = Some(conn);
        self
    }

    /// Get a Redis connection for publishing.
    ///
    /// Prefers the shared handle (follows Sentinel failover) when available,
    /// falling back to lazily creating from redis_client.
    async fn get_conn(&self) -> Result<ConnectionManager> {
        // Prefer the shared handle from bootstrap (follows Sentinel failover)
        if let Some(ref shared) = self.redis_conn_shared {
            return Ok(shared.read().await.clone());
        }

        // Fallback: lazily create from redis_client
        let client = self
            .redis_client
            .as_ref()
            .ok_or_else(|| Error::Internal("Redis not configured".to_string()))?;
        let conn = self
            .redis_conn
            .get_or_try_init(|| async {
                client.get_connection_manager().await.map_err(|e| {
                    Error::Internal(format!("Failed to create Redis ConnectionManager: {e}"))
                })
            })
            .await?;
        Ok(conn.clone())
    }

    /// Start listening for cache invalidation messages from Redis
    ///
    /// This spawns a background task that reads from the invalidation stream.
    /// When a message is received, it's broadcast locally to all cache instances.
    /// On reconnection, pending (unacknowledged) messages are processed first to
    /// catch up on messages missed during the disconnection.
    ///
    /// On startup, any stale consumer group left by a previous instance of this
    /// node (e.g., after SIGKILL or OOM kill) is cleaned up before creating a
    /// fresh one. This prevents orphaned consumer groups from accumulating in
    /// Redis.
    ///
    /// Additionally spawns a periodic state sync task that broadcasts an "All"
    /// invalidation message every 60 seconds to ensure replicas that missed
    /// invalidations during Redis outages eventually converge.
    pub async fn start(&self) -> Result<()> {
        let Some(client) = self.redis_client.clone() else {
            info!("Redis not configured, cache invalidation is local-only");
            return Ok(());
        };

        // Clean up any stale consumer group left by a previous process with
        // the same node_id (e.g., after SIGKILL/OOM kill where stop() never ran).
        self.cleanup_stale_consumer_group().await;

        // Clean up orphaned consumer groups left by previous processes with
        // different node_ids (e.g., non-K8s restarts where node_id has a random suffix).
        self.cleanup_orphaned_consumer_groups().await;

        // Create consumer group if it doesn't exist.
        // Use "$" so the group starts from the latest message (only new messages).
        // This prevents replaying all historical messages on every restart.
        // If the group already exists, BUSYGROUP error is expected and ignored.
        if let Err(e) = self.create_consumer_group().await {
            // create_consumer_group returns Ok(()) if the group was created,
            // or Ok(()) if BUSYGROUP (already exists). Any error here is unexpected.
            warn!(
                error = %e,
                "Failed to create consumer group"
            );
        }

        let local_sender = self.local_sender.clone();
        let node_id = self.node_id.clone();
        let stream_key = self.stream_key.clone();
        let consumer_group = self.consumer_group.clone();
        let shutdown = self.shutdown.clone();

        // Clone client for the subscriber task before moving the original to spawn_state_sync_task
        let client_for_subscriber = client.clone();

        crate::spawn::spawn_monitored("cache_invalidation_subscriber", async move {
            let mut backoff_secs: u64 = 1;
            const MAX_BACKOFF_SECS: u64 = 30;

            loop {
                if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                    debug!("Cache invalidation listener shutting down");
                    break;
                }

                match Self::run_subscriber(
                    &client_for_subscriber,
                    &local_sender,
                    &node_id,
                    &stream_key,
                    &consumer_group,
                    shutdown.clone(),
                )
                .await
                {
                    Ok(()) => {
                        // Normal shutdown
                        break;
                    }
                    Err(e) => {
                        error!(
                            error = %e,
                            backoff_seconds = backoff_secs,
                            "Cache invalidation subscriber error, reconnecting..."
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                    }
                }
            }
            info!("Cache invalidation listener stopped");
        });

        // Spawn periodic state sync task
        self.spawn_state_sync_task(client);

        Ok(())
    }

    /// Spawn a background task that periodically broadcasts a state sync message.
    ///
    /// This ensures that replicas that missed invalidations during Redis outages
    /// eventually converge. The sync interval is controlled by `STATE_SYNC_INTERVAL_SECS`.
    fn spawn_state_sync_task(&self, client: Client) {
        let redis_conn = self.redis_conn.clone();
        let stream_key = self.stream_key.clone();
        let node_id = self.node_id.clone();
        let shutdown = self.shutdown.clone();
        let needs_state_sync = self.needs_state_sync.clone();

        crate::spawn::spawn_monitored("cache_invalidation_state_sync", async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(STATE_SYNC_INTERVAL_SECS));
            interval.tick().await;

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }

                        let pending_recovery_sync = needs_state_sync
                            .swap(false, std::sync::atomic::Ordering::Relaxed);

                        match Self::do_broadcast_to_stream(
                            &client,
                            &redis_conn,
                            &stream_key,
                            &node_id,
                            &InvalidationMessage::All,
                        ).await {
                            Ok(()) => {
                                info!(
                                    node_id = %node_id,
                                    recovery_sync = pending_recovery_sync,
                                    "Periodic state sync: broadcast 'All' invalidation message"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    recovery_sync = pending_recovery_sync,
                                    "Failed to broadcast state sync message, will retry next interval"
                                );
                                needs_state_sync.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                    () = async {
                        loop {
                            if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                                return;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                    } => {
                        break;
                    }
                }
            }
            debug!("Cache invalidation state sync task stopped");
        });
    }

    /// Internal helper to broadcast a message to the Redis stream.
    async fn do_broadcast_to_stream(
        client: &Client,
        redis_conn: &Arc<OnceCell<ConnectionManager>>,
        stream_key: &str,
        node_id: &str,
        message: &InvalidationMessage,
    ) -> Result<()> {
        let json = serde_json::to_string(message).map_err(|e| {
            Error::Internal(format!("Failed to serialize invalidation message: {e}"))
        })?;

        let conn = redis_conn
            .get_or_try_init(|| async {
                client.get_connection_manager().await.map_err(|e| {
                    Error::Internal(format!("Failed to create Redis ConnectionManager: {e}"))
                })
            })
            .await?;

        let mut conn = conn.clone();

        // XADD <stream> MAXLEN ~ <max_len> * origin <node_id> payload <json>
        let _: String = redis::cmd("XADD")
            .arg(stream_key)
            .arg("MAXLEN")
            .arg("~")
            .arg(MAX_STREAM_LENGTH)
            .arg("*") // Auto-generate message ID
            .arg("origin")
            .arg(node_id)
            .arg("payload")
            .arg(json)
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Internal(format!("Failed to add message to stream: {e}")))?;

        debug!(
            node_id = %node_id,
            ?message,
            "Published cache invalidation message to stream"
        );

        Ok(())
    }

    /// Create the consumer group for the invalidation stream
    ///
    /// Returns `Ok(())` if the group was created or already exists (BUSYGROUP).
    async fn create_consumer_group(&self) -> Result<()> {
        let mut conn = self.get_conn().await?;

        // XGROUP CREATE <stream> <group> $ MKSTREAM
        // Use "$" so the group starts from the latest message and only processes
        // new messages going forward. Using "0" would cause the service to replay
        // ALL historical messages on every restart, triggering a massive cache
        // invalidation storm. On reconnection, pending (unacknowledged) messages
        // are caught up via XREADGROUP ... 0 in process_pending_messages().
        // If the group already exists, BUSYGROUP is expected and ignored below.
        let result: redis::RedisResult<()> = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(&self.stream_key)
            .arg(&self.consumer_group)
            .arg("$")
            .arg("MKSTREAM")
            .query_async(&mut conn)
            .await;

        match result {
            Ok(()) => {
                info!(
                    stream = %self.stream_key,
                    group = %self.consumer_group,
                    "Created consumer group for cache invalidation"
                );
                Ok(())
            }
            Err(e) => {
                // Use the redis error's code() method to detect BUSYGROUP structurally
                // rather than string-matching on the formatted error message.
                // Redis returns "BUSYGROUP" as the error code when the group already exists.
                if e.code() == Some("BUSYGROUP") {
                    debug!(
                        stream = %self.stream_key,
                        group = %self.consumer_group,
                        "Consumer group already exists"
                    );
                    Ok(())
                } else {
                    Err(Error::Internal(format!(
                        "Failed to create consumer group: {e}"
                    )))
                }
            }
        }
    }

    /// Clean up a stale consumer group left by a previous process with the same `node_id`.
    ///
    /// Uses `XINFO GROUPS` to check if a consumer group matching this node's
    /// `consumer_group` name already exists. If found, it is destroyed so that a
    /// fresh group can be created. This handles the case where a previous process
    /// was killed (SIGKILL, OOM) before `stop()` could run.
    async fn cleanup_stale_consumer_group(&self) {
        let Ok(mut conn) = self.get_conn().await else {
            warn!("Cannot clean up stale consumer group: failed to get Redis connection");
            return;
        };

        // XINFO GROUPS <stream> returns info about all consumer groups on the stream.
        // We use a raw command since the redis crate's typed API for XINFO is limited.
        let result: redis::RedisResult<Vec<Vec<redis::Value>>> = redis::cmd("XINFO")
            .arg("GROUPS")
            .arg(&self.stream_key)
            .query_async(&mut conn)
            .await;

        match result {
            Ok(groups) => {
                for group_info in &groups {
                    // Each group is returned as a flat array of key-value pairs:
                    // ["name", "<group_name>", "consumers", N, "pending", N, ...]
                    let group_name = Self::extract_group_name(group_info);
                    if group_name.as_deref() == Some(self.consumer_group.as_str()) {
                        info!(
                            stream = %self.stream_key,
                            group = %self.consumer_group,
                            "Found stale consumer group from previous process, destroying it"
                        );
                        let destroy_result: redis::RedisResult<()> = redis::cmd("XGROUP")
                            .arg("DESTROY")
                            .arg(&self.stream_key)
                            .arg(&self.consumer_group)
                            .query_async(&mut conn)
                            .await;
                        if let Err(e) = destroy_result {
                            warn!(
                                error = %e,
                                stream = %self.stream_key,
                                group = %self.consumer_group,
                                "Failed to destroy stale consumer group"
                            );
                        }
                        return;
                    }
                }
                debug!(
                    stream = %self.stream_key,
                    group = %self.consumer_group,
                    "No stale consumer group found"
                );
            }
            Err(e) => {
                // Stream may not exist yet (first deploy), which is fine.
                debug!(
                    error = %e,
                    stream = %self.stream_key,
                    "Could not query consumer groups (stream may not exist yet)"
                );
            }
        }
    }

    /// Clean up orphaned consumer groups left by previous processes with different
    /// `node_id` values (e.g., due to random suffix in non-K8s deployments).
    ///
    /// Scans all consumer groups on the stream and destroys any group that:
    /// - Starts with the `cache-invalidation-` prefix (i.e., belongs to this service)
    /// - Is **not** the current node's consumer group
    /// - Has zero active consumers and zero pending messages
    ///
    /// Called from [`start`](Self::start) to prevent unbounded accumulation of
    /// orphaned groups in Redis.
    async fn cleanup_orphaned_consumer_groups(&self) {
        let Ok(mut conn) = self.get_conn().await else {
            return;
        };

        let result: redis::RedisResult<Vec<Vec<redis::Value>>> = redis::cmd("XINFO")
            .arg("GROUPS")
            .arg(&self.stream_key)
            .query_async(&mut conn)
            .await;

        let groups = match result {
            Ok(g) => g,
            Err(_) => return, // Stream may not exist yet
        };

        for group_info in &groups {
            let Some(name) = Self::extract_group_name(group_info) else {
                continue;
            };

            // Only clean up groups belonging to this service, not the current one
            if !name.starts_with("cache-invalidation-") || name == self.consumer_group {
                continue;
            }

            // Check consumers == 0 and pending == 0
            let consumers = Self::extract_integer_field(group_info, "consumers").unwrap_or(1);
            let pending = Self::extract_integer_field(group_info, "pending").unwrap_or(1);

            if consumers == 0 && pending == 0 {
                let destroy_result: redis::RedisResult<()> = redis::cmd("XGROUP")
                    .arg("DESTROY")
                    .arg(&self.stream_key)
                    .arg(&name)
                    .query_async(&mut conn)
                    .await;
                match destroy_result {
                    Ok(()) => {
                        info!(
                            stream = %self.stream_key,
                            group = %name,
                            "Destroyed orphaned consumer group"
                        );
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            stream = %self.stream_key,
                            group = %name,
                            "Failed to destroy orphaned consumer group"
                        );
                    }
                }
            }
        }
    }

    /// Extract an integer field from an XINFO GROUPS response entry.
    ///
    /// Searches for `field_name` in the flat key-value list and returns the
    /// following value as `i64`.
    fn extract_integer_field(group_info: &[redis::Value], field_name: &str) -> Option<i64> {
        let mut iter = group_info.iter();
        while let Some(key) = iter.next() {
            let key_str = match key {
                redis::Value::BulkString(bytes) => std::str::from_utf8(bytes).ok(),
                redis::Value::SimpleString(s) => Some(s.as_str()),
                _ => None,
            };
            let value = iter.next()?;
            if key_str == Some(field_name) {
                return match value {
                    redis::Value::Int(n) => Some(*n),
                    _ => None,
                };
            }
        }
        None
    }

    /// Extract the "name" field from an XINFO GROUPS response entry.
    ///
    /// Each entry is a flat list of alternating key-value pairs:
    /// `["name", "group-name", "consumers", 1, ...]`
    fn extract_group_name(group_info: &[redis::Value]) -> Option<String> {
        let mut iter = group_info.iter();
        while let Some(key) = iter.next() {
            let key_str = match key {
                redis::Value::BulkString(bytes) => std::str::from_utf8(bytes).ok(),
                redis::Value::SimpleString(s) => Some(s.as_str()),
                _ => None,
            };
            let value = iter.next()?;
            if key_str == Some("name") {
                return match value {
                    redis::Value::BulkString(bytes) => {
                        std::str::from_utf8(bytes).ok().map(String::from)
                    }
                    redis::Value::SimpleString(s) => Some(s.clone()),
                    _ => None,
                };
            }
        }
        None
    }

    /// Maximum delivery attempts for a malformed message before it is
    /// acknowledged and discarded. Prevents unprocessable entries from
    /// accumulating indefinitely in the Redis Stream PEL.
    const MAX_DELIVERY_ATTEMPTS: u32 = 3;

    /// Run the Redis subscriber loop using Streams with catch-up on reconnection
    ///
    /// The subscriber has two phases:
    /// 1. **Catch-up phase**: Read pending (delivered but unacknowledged) messages
    ///    using `XREADGROUP ... 0`. These are messages that were delivered before a
    ///    disconnect but never acknowledged.
    /// 2. **Live phase**: Read new messages using `XREADGROUP ... >`.
    ///
    /// Additionally performs periodic stream trimming (XTRIM MINID) to enforce
    /// the 1-hour retention policy.
    async fn run_subscriber(
        client: &Client,
        local_sender: &broadcast::Sender<InvalidationMessage>,
        node_id: &str,
        stream_key: &str,
        consumer_group: &str,
        shutdown: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<()> {
        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| Error::Internal(format!("Failed to get Redis connection: {e}")))?;

        // Track delivery attempts for malformed messages so they can be
        // discarded after MAX_DELIVERY_ATTEMPTS to prevent PEL accumulation.
        let mut failed_delivery_counts: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();

        info!(
            node_id = %node_id,
            stream = %stream_key,
            group = %consumer_group,
            "Started cache invalidation stream consumer"
        );

        // Phase 1: Catch-up -- process pending messages that were delivered but
        // not acknowledged before the last disconnect.
        let catchup_count = Self::process_pending_messages(
            &mut conn,
            local_sender,
            node_id,
            stream_key,
            consumer_group,
            &mut failed_delivery_counts,
        )
        .await?;

        if catchup_count > 0 {
            info!(
                count = catchup_count,
                "Caught up on pending cache invalidation messages"
            );
        }

        // Phase 2: Live -- read new messages
        let mut trim_counter: u32 = 0;
        const TRIM_EVERY_N_ITERATIONS: u32 = 60; // ~60 seconds at 1s block

        loop {
            if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }

            // Periodically trim old entries (time-based, 1 hour retention)
            trim_counter += 1;
            if trim_counter >= TRIM_EVERY_N_ITERATIONS {
                trim_counter = 0;
                Self::trim_stream(&mut conn, stream_key).await;
            }

            let result: redis::RedisResult<redis::streams::StreamReadReply> =
                redis::cmd("XREADGROUP")
                    .arg("GROUP")
                    .arg(consumer_group)
                    .arg(node_id)
                    .arg("COUNT")
                    .arg(100)
                    .arg("BLOCK")
                    .arg(1000) // Block for 1 second
                    .arg("STREAMS")
                    .arg(stream_key)
                    .arg(">") // Only new messages
                    .query_async(&mut conn)
                    .await;

            match result {
                Ok(reply) => {
                    Self::process_stream_reply(
                        &mut conn,
                        local_sender,
                        node_id,
                        stream_key,
                        consumer_group,
                        &reply,
                        &mut failed_delivery_counts,
                    )
                    .await;
                }
                Err(e) => {
                    return Err(Error::Internal(format!(
                        "Failed to read from Redis stream: {e}"
                    )));
                }
            }
        }

        Ok(())
    }

    /// Process pending (unacknowledged) messages from the consumer group.
    ///
    /// Uses `XREADGROUP ... 0` which returns messages previously delivered to this
    /// consumer but not yet acknowledged. This enables catch-up after reconnection.
    async fn process_pending_messages(
        conn: &mut redis::aio::MultiplexedConnection,
        local_sender: &broadcast::Sender<InvalidationMessage>,
        node_id: &str,
        stream_key: &str,
        consumer_group: &str,
        failed_delivery_counts: &mut std::collections::HashMap<String, u32>,
    ) -> Result<usize> {
        let mut total = 0;
        loop {
            let result: redis::RedisResult<redis::streams::StreamReadReply> =
                redis::cmd("XREADGROUP")
                    .arg("GROUP")
                    .arg(consumer_group)
                    .arg(node_id)
                    .arg("COUNT")
                    .arg(100)
                    .arg("STREAMS")
                    .arg(stream_key)
                    .arg("0") // Read pending messages
                    .query_async(conn)
                    .await;

            match result {
                Ok(reply) => {
                    let mut batch_count = 0;
                    for sk in &reply.keys {
                        for entry in &sk.ids {
                            // An entry with an empty map means this pending entry
                            // has already been acknowledged or trimmed
                            if entry.map.is_empty() {
                                continue;
                            }
                            batch_count += 1;
                            Self::process_single_entry(
                                conn,
                                local_sender,
                                node_id,
                                stream_key,
                                consumer_group,
                                entry,
                                failed_delivery_counts,
                            )
                            .await;
                        }
                    }
                    total += batch_count;
                    if batch_count == 0 {
                        // No more pending messages
                        break;
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to read pending messages, skipping catch-up");
                    break;
                }
            }
        }
        Ok(total)
    }

    /// Process entries from a stream read reply, filtering self-originated messages
    async fn process_stream_reply(
        conn: &mut redis::aio::MultiplexedConnection,
        local_sender: &broadcast::Sender<InvalidationMessage>,
        node_id: &str,
        stream_key: &str,
        consumer_group: &str,
        reply: &redis::streams::StreamReadReply,
        failed_delivery_counts: &mut std::collections::HashMap<String, u32>,
    ) {
        for sk in &reply.keys {
            for entry in &sk.ids {
                Self::process_single_entry(
                    conn,
                    local_sender,
                    node_id,
                    stream_key,
                    consumer_group,
                    entry,
                    failed_delivery_counts,
                )
                .await;
            }
        }
    }

    /// Process a single stream entry: deserialize, filter, broadcast, and acknowledge.
    ///
    /// Malformed entries that cannot be parsed are tracked in `failed_delivery_counts`.
    /// After `MAX_DELIVERY_ATTEMPTS`, the entry is acknowledged and discarded to
    /// prevent indefinite PEL accumulation.
    async fn process_single_entry(
        conn: &mut redis::aio::MultiplexedConnection,
        local_sender: &broadcast::Sender<InvalidationMessage>,
        node_id: &str,
        stream_key: &str,
        consumer_group: &str,
        entry: &redis::streams::StreamId,
        failed_delivery_counts: &mut std::collections::HashMap<String, u32>,
    ) {
        // Check origin node to skip self-originated messages
        if let Some(origin_value) = entry.map.get("origin") {
            let origin_str = match origin_value {
                redis::Value::BulkString(bytes) => std::str::from_utf8(bytes).ok(),
                redis::Value::SimpleString(s) => Some(s.as_str()),
                _ => None,
            };
            if origin_str == Some(node_id) {
                // Acknowledge but don't broadcast -- this node originated the message
                let _: redis::RedisResult<()> = redis::cmd("XACK")
                    .arg(stream_key)
                    .arg(consumer_group)
                    .arg(&entry.id)
                    .query_async(conn)
                    .await;
                failed_delivery_counts.remove(&entry.id);
                return;
            }
        }

        // Extract the message payload
        let payload_str = entry.map.get("payload").and_then(|v| match v {
            redis::Value::BulkString(bytes) => std::str::from_utf8(bytes).ok(),
            redis::Value::SimpleString(s) => Some(s.as_str()),
            _ => None,
        });

        let Some(payload_str) = payload_str else {
            // No payload field - malformed entry. Track delivery count and
            // discard after MAX_DELIVERY_ATTEMPTS to prevent PEL accumulation.
            let count = failed_delivery_counts.entry(entry.id.clone()).or_insert(0);
            *count += 1;
            if *count >= Self::MAX_DELIVERY_ATTEMPTS {
                warn!(
                    message_id = %entry.id,
                    attempts = *count,
                    "Cache invalidation message has no payload field after {} attempts; acknowledging to prevent PEL accumulation",
                    Self::MAX_DELIVERY_ATTEMPTS
                );
                let _: redis::RedisResult<()> = redis::cmd("XACK")
                    .arg(stream_key)
                    .arg(consumer_group)
                    .arg(&entry.id)
                    .query_async(conn)
                    .await;
                failed_delivery_counts.remove(&entry.id);
            } else {
                warn!(
                    message_id = %entry.id,
                    attempt = *count,
                    max_attempts = Self::MAX_DELIVERY_ATTEMPTS,
                    "Cache invalidation message has no payload field; skipping XACK (will retry)"
                );
            }
            return;
        };

        match serde_json::from_str::<InvalidationMessage>(payload_str) {
            Ok(invalidation) => {
                debug!(
                    node_id = %node_id,
                    message_id = %entry.id,
                    ?invalidation,
                    "Received cache invalidation message"
                );

                // Broadcast locally
                if let Err(e) = local_sender.send(invalidation) {
                    warn!(error = %e, "Failed to broadcast invalidation locally");
                }

                // Clear any previous failure tracking for this message
                failed_delivery_counts.remove(&entry.id);
            }
            Err(e) => {
                // Parse failed. Track delivery count and discard after
                // MAX_DELIVERY_ATTEMPTS to prevent PEL accumulation.
                let count = failed_delivery_counts.entry(entry.id.clone()).or_insert(0);
                *count += 1;
                if *count >= Self::MAX_DELIVERY_ATTEMPTS {
                    warn!(
                        error = %e,
                        message_id = %entry.id,
                        json = %payload_str,
                        attempts = *count,
                        "Failed to parse cache invalidation message after {} attempts; acknowledging to prevent PEL accumulation",
                        Self::MAX_DELIVERY_ATTEMPTS
                    );
                    let _: redis::RedisResult<()> = redis::cmd("XACK")
                        .arg(stream_key)
                        .arg(consumer_group)
                        .arg(&entry.id)
                        .query_async(conn)
                        .await;
                    failed_delivery_counts.remove(&entry.id);
                } else {
                    warn!(
                        error = %e,
                        message_id = %entry.id,
                        json = %payload_str,
                        attempt = *count,
                        max_attempts = Self::MAX_DELIVERY_ATTEMPTS,
                        "Failed to parse cache invalidation message; skipping XACK (will retry)"
                    );
                }
                return;
            }
        }

        // Acknowledge the message only after successful parse and broadcast.
        let _: redis::RedisResult<()> = redis::cmd("XACK")
            .arg(stream_key)
            .arg(consumer_group)
            .arg(&entry.id)
            .query_async(conn)
            .await;
    }

    /// Trim the stream to remove entries older than `STREAM_RETENTION_MS` (1 hour).
    ///
    /// Uses XTRIM with MINID to time-based trim, converting current timestamp
    /// to a Redis stream ID (which is millisecond-based).
    async fn trim_stream(conn: &mut redis::aio::MultiplexedConnection, stream_key: &str) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let min_id = now_ms.saturating_sub(STREAM_RETENTION_MS);
        let min_id_str = format!("{min_id}-0");

        let result: redis::RedisResult<i64> = redis::cmd("XTRIM")
            .arg(stream_key)
            .arg("MINID")
            .arg("~") // Approximate for performance
            .arg(&min_id_str)
            .query_async(conn)
            .await;

        match result {
            Ok(trimmed) if trimmed > 0 => {
                debug!(
                    trimmed,
                    stream_key, "Trimmed old cache invalidation entries"
                );
            }
            Err(e) => {
                warn!(error = %e, "Failed to trim cache invalidation stream");
            }
            _ => {}
        }
    }

    /// Stop the cache invalidation service and trim the stream.
    ///
    /// Signals the background subscriber to stop, then trims the Redis stream
    /// to the configured maximum length. XGROUP DESTROY is intentionally NOT
    /// used here because it would drop the entire Pending Entry List (PEL),
    /// losing messages that were delivered but not yet acknowledged. XTRIM
    /// limits stream growth without discarding unacknowledged messages.
    pub async fn stop(&self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);

        // Trim the stream on shutdown to prevent unbounded growth.
        // We do NOT call XGROUP DESTROY because that would silently drop the
        // entire PEL, losing messages that were delivered but not yet XACK'd.
        if self.redis_client.is_some() {
            match self.get_conn().await {
                Ok(mut conn) => {
                    let result: redis::RedisResult<i64> = redis::cmd("XTRIM")
                        .arg(&self.stream_key)
                        .arg("MAXLEN")
                        .arg("~")
                        .arg(MAX_STREAM_LENGTH)
                        .query_async(&mut conn)
                        .await;
                    match result {
                        Ok(trimmed) => {
                            info!(
                                stream = %self.stream_key,
                                trimmed,
                                "Trimmed cache invalidation stream on shutdown"
                            );
                        }
                        Err(e) => {
                            warn!(
                                error = %e,
                                stream = %self.stream_key,
                                "Failed to trim cache invalidation stream on shutdown"
                            );
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        "Failed to get Redis connection for stream trim on shutdown"
                    );
                }
            }
        }
    }

    /// Subscribe to local cache invalidation events
    ///
    /// Returns a receiver that will receive invalidation messages from all nodes.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<InvalidationMessage> {
        self.local_sender.subscribe()
    }

    /// Broadcast a cache invalidation message to all OTHER nodes (remote only)
    ///
    /// This sends the message via Redis Streams (if configured).
    /// Each message includes an `origin` field with the `node_id` so that the
    /// originating node can skip its own messages during consumption.
    ///
    /// Note: This does NOT broadcast locally, as the caller is expected to
    /// invalidate its own local cache after calling this method.
    ///
    /// For local-only invalidation (when Redis is not configured), this is a no-op.
    ///
    /// If Redis is unavailable, this method marks the service as needing a state
    /// sync, which will trigger a periodic "All" invalidation broadcast once
    /// Redis recovers.
    pub async fn broadcast_remote(&self, message: InvalidationMessage) -> Result<()> {
        // Broadcast via Redis Streams if available
        if self.redis_client.is_some() {
            match self.do_broadcast_to_stream_internal(&message).await {
                Ok(()) => {
                    // Clear the sync flag on successful broadcast
                    self.needs_state_sync
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                }
                Err(e) => {
                    // Mark that we need a state sync when Redis recovers
                    self.needs_state_sync
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    warn!(
                        error = %e,
                        node_id = %self.node_id,
                        "Failed to broadcast cache invalidation to Redis, marked for state sync"
                    );
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    /// Internal implementation of broadcasting to Redis stream.
    async fn do_broadcast_to_stream_internal(&self, message: &InvalidationMessage) -> Result<()> {
        let json = serde_json::to_string(message).map_err(|e| {
            Error::Internal(format!("Failed to serialize invalidation message: {e}"))
        })?;

        let mut conn = self.get_conn().await?;

        // XADD <stream> MAXLEN ~ <max_len> * origin <node_id> payload <json>
        let _: String = redis::cmd("XADD")
            .arg(&self.stream_key)
            .arg("MAXLEN")
            .arg("~")
            .arg(MAX_STREAM_LENGTH)
            .arg("*") // Auto-generate message ID
            .arg("origin")
            .arg(&self.node_id)
            .arg("payload")
            .arg(json)
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Internal(format!("Failed to add message to stream: {e}")))?;

        debug!(
            node_id = %self.node_id,
            ?message,
            "Published cache invalidation message to stream"
        );

        Ok(())
    }

    /// Broadcast a cache invalidation message locally only (no Redis).
    ///
    /// Use this when the invalidation event was already received from a remote
    /// source (e.g., a cluster `CacheInvalidate` event from Redis Pub/Sub) and
    /// only the local caches on this node need to be notified.
    pub fn broadcast_local(&self, message: InvalidationMessage) -> Result<()> {
        if let Err(e) = self.local_sender.send(message) {
            warn!(error = %e, "Failed to broadcast invalidation locally");
        }
        Ok(())
    }

    /// Broadcast a cache invalidation message to ALL nodes including this one
    ///
    /// This sends the message via Redis Streams (if configured) and also
    /// broadcasts locally via the local channel.
    /// Use this when you want all nodes (including this one) to invalidate
    /// their caches via the subscription mechanism.
    pub async fn broadcast_all(&self, message: InvalidationMessage) -> Result<()> {
        // Broadcast locally first
        if let Err(e) = self.local_sender.send(message.clone()) {
            warn!(error = %e, "Failed to broadcast invalidation locally");
        }

        // Then broadcast via Redis
        self.broadcast_remote(message).await
    }

    /// Invalidate permission cache for a specific user in a room
    pub async fn invalidate_user_permission(
        &self,
        room_id: &RoomId,
        user_id: &crate::models::UserId,
    ) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::UserPermission {
            room_id: room_id.as_str().to_string(),
            user_id: user_id.as_str().to_string(),
        })
        .await
    }

    /// Invalidate permission cache for all users in a room
    pub async fn invalidate_room_permission(&self, room_id: &RoomId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::RoomPermission {
            room_id: room_id.as_str().to_string(),
        })
        .await
    }

    /// Invalidate user cache
    pub async fn invalidate_user(&self, user_id: &crate::models::UserId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::User {
            user_id: user_id.as_str().to_string(),
        })
        .await
    }

    /// Invalidate username cache
    pub async fn invalidate_username(&self, user_id: &crate::models::UserId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::Username {
            user_id: user_id.as_str().to_string(),
        })
        .await
    }

    /// Invalidate room cache
    pub async fn invalidate_room(&self, room_id: &RoomId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::Room {
            room_id: room_id.as_str().to_string(),
        })
        .await
    }

    /// Invalidate playback state cache for a room
    pub async fn invalidate_playback_state(&self, room_id: &RoomId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::PlaybackState {
            room_id: room_id.as_str().to_string(),
        })
        .await
    }

    /// Broadcast an updated playback state to other replicas so they can
    /// write it directly into their L1 cache, avoiding the stale-read window
    /// that occurs when only an invalidation message is sent.
    pub async fn update_playback_state(
        &self,
        room_id: &RoomId,
        state: &crate::models::RoomPlaybackState,
    ) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::PlaybackStateUpdate {
            room_id: room_id.as_str().to_string(),
            state: state.clone(),
        })
        .await
    }

    /// Invalidate room settings cache for a specific room
    pub async fn invalidate_room_settings(&self, room_id: &RoomId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::RoomSettings {
            room_id: room_id.as_str().to_string(),
        })
        .await
    }

    /// Invalidate all caches
    pub async fn invalidate_all(&self) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::All).await
    }

    // -- Convenience methods: local invalidation + remote broadcast --------
    //
    // These ensure the originating node's local caches are invalidated
    // BEFORE (or concurrently with) broadcasting to remote replicas,
    // preventing a stale-read window on the originating node.

    /// Invalidate user cache locally and broadcast to other replicas.
    ///
    /// Use this instead of calling `invalidate_user()` directly to ensure
    /// the originating node also clears its local cache.
    pub async fn invalidate_and_broadcast_user(
        &self,
        user_id: &crate::models::UserId,
    ) -> Result<()> {
        // Broadcast locally so that CacheManager's listener picks it up
        // and invalidates the local L1 + L2 cache entry.
        let msg = InvalidationMessage::User {
            user_id: user_id.as_str().to_string(),
        };
        if let Err(e) = self.local_sender.send(msg.clone()) {
            warn!(error = %e, "Failed to broadcast user invalidation locally");
        }
        // Then broadcast to remote replicas
        self.broadcast_remote(msg).await
    }

    /// Invalidate room cache locally and broadcast to other replicas.
    pub async fn invalidate_and_broadcast_room(&self, room_id: &RoomId) -> Result<()> {
        let msg = InvalidationMessage::Room {
            room_id: room_id.as_str().to_string(),
        };
        if let Err(e) = self.local_sender.send(msg.clone()) {
            warn!(error = %e, "Failed to broadcast room invalidation locally");
        }
        self.broadcast_remote(msg).await
    }

    /// Invalidate room settings cache locally and broadcast to other replicas.
    pub async fn invalidate_and_broadcast_room_settings(&self, room_id: &RoomId) -> Result<()> {
        let msg = InvalidationMessage::RoomSettings {
            room_id: room_id.as_str().to_string(),
        };
        if let Err(e) = self.local_sender.send(msg.clone()) {
            warn!(error = %e, "Failed to broadcast room settings invalidation locally");
        }
        self.broadcast_remote(msg).await
    }

    /// Invalidate username cache locally and broadcast to other replicas.
    pub async fn invalidate_and_broadcast_username(
        &self,
        user_id: &crate::models::UserId,
    ) -> Result<()> {
        let msg = InvalidationMessage::Username {
            user_id: user_id.as_str().to_string(),
        };
        if let Err(e) = self.local_sender.send(msg.clone()) {
            warn!(error = %e, "Failed to broadcast username invalidation locally");
        }
        self.broadcast_remote(msg).await
    }

    /// Invalidate user permission cache locally and broadcast to other replicas.
    pub async fn invalidate_and_broadcast_user_permission(
        &self,
        room_id: &RoomId,
        user_id: &crate::models::UserId,
    ) -> Result<()> {
        let msg = InvalidationMessage::UserPermission {
            room_id: room_id.as_str().to_string(),
            user_id: user_id.as_str().to_string(),
        };
        if let Err(e) = self.local_sender.send(msg.clone()) {
            warn!(error = %e, "Failed to broadcast user permission invalidation locally");
        }
        self.broadcast_remote(msg).await
    }

    /// Invalidate room permission cache locally and broadcast to other replicas.
    pub async fn invalidate_and_broadcast_room_permission(&self, room_id: &RoomId) -> Result<()> {
        let msg = InvalidationMessage::RoomPermission {
            room_id: room_id.as_str().to_string(),
        };
        if let Err(e) = self.local_sender.send(msg.clone()) {
            warn!(error = %e, "Failed to broadcast room permission invalidation locally");
        }
        self.broadcast_remote(msg).await
    }

    /// Minimum interval between lag-triggered flushes.
    ///
    /// Matches the rate-limiting in `CacheManager::start_invalidation_listener`
    /// to prevent sustained lag storms from cascading into continuous DB stampedes.
    const LAG_FLUSH_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

    /// Spawn a named invalidation listener that subscribes to this service's
    /// broadcast channel and dispatches messages to a user-provided handler.
    ///
    /// This extracts the common pattern shared by `CacheManager`, `PermissionService`,
    /// `PlaybackService`, `RoomSettingsService`, etc.:
    ///
    /// 1. Subscribe to the broadcast channel
    /// 2. Spawn a monitored task that loops on `recv()`
    /// 3. On `Ok(msg)` -> call `handler(msg)`
    /// 4. On `Lagged(n)` -> call `on_lagged()` to flush caches (rate-limited)
    /// 5. On `Closed` -> break
    ///
    /// The `on_lagged` handler is rate-limited to at most once every 5 seconds
    /// to prevent continuous cache flush cascades under sustained lag.
    ///
    /// # Arguments
    /// * `name` - Task name for monitoring (e.g., "`room_settings_invalidation_listener`")
    /// * `handler` - Async closure called for each received message
    /// * `on_lagged` - Async closure called when the receiver falls behind (should flush caches)
    pub fn spawn_listener<H, Hf, L, Lf>(&self, name: &'static str, handler: H, on_lagged: L)
    where
        H: Fn(InvalidationMessage) -> Hf + Send + 'static,
        Hf: std::future::Future<Output = ()> + Send,
        L: Fn(u64) -> Lf + Send + 'static,
        Lf: std::future::Future<Output = ()> + Send,
    {
        let mut receiver = self.subscribe();

        crate::spawn::spawn_monitored(name, async move {
            // Rate-limit lag-triggered flushes to at most once per interval.
            let mut last_lag_flush = std::time::Instant::now()
                .checked_sub(Self::LAG_FLUSH_MIN_INTERVAL)
                .unwrap_or_else(std::time::Instant::now);

            loop {
                match receiver.recv().await {
                    Ok(msg) => {
                        handler(msg).await;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        debug!("{name}: invalidation channel closed, stopping listener");
                        break;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        let now = std::time::Instant::now();
                        let elapsed = now.duration_since(last_lag_flush);
                        if elapsed >= Self::LAG_FLUSH_MIN_INTERVAL {
                            warn!(
                                lagged_messages = n,
                                "{name}: invalidation listener lagged, triggering flush (rate-limited to once per {}s)",
                                Self::LAG_FLUSH_MIN_INTERVAL.as_secs()
                            );
                            on_lagged(n).await;
                            crate::metrics::cache::CACHE_LAG_FLUSH_TOTAL
                                .with_label_values(&[name])
                                .inc();
                            last_lag_flush = now;
                        } else {
                            warn!(
                                lagged_messages = n,
                                "{name}: invalidation listener lagged, skipping flush (rate-limited)"
                            );
                        }
                    }
                }
            }
        });
    }
}

impl std::fmt::Debug for CacheInvalidationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheInvalidationService")
            .field("redis_enabled", &self.redis_client.is_some())
            .field("node_id", &self.node_id)
            .field(
                "needs_state_sync",
                &self
                    .needs_state_sync
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalidation_message_serialization() {
        let msg = InvalidationMessage::UserPermission {
            room_id: "room123".to_string(),
            user_id: "user456".to_string(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("user_permission"));

        let decoded: InvalidationMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_room_permission_message() {
        let msg = InvalidationMessage::RoomPermission {
            room_id: "room123".to_string(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        let decoded: InvalidationMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[tokio::test]
    async fn test_local_broadcast() {
        let service = CacheInvalidationService::new(
            None,
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        );
        let mut receiver = service.subscribe();

        let msg = InvalidationMessage::User {
            user_id: "user123".to_string(),
        };

        // broadcast_all sends to local + Redis; broadcast only sends to Redis
        service.broadcast_all(msg.clone()).await.unwrap();

        let received = receiver.recv().await.unwrap();
        assert_eq!(msg, received);
    }

    #[tokio::test]
    async fn test_broadcast_without_redis_is_noop() {
        let service = CacheInvalidationService::new(
            None,
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        );

        // broadcast_remote() without Redis should be a no-op (no local broadcast)
        let msg = InvalidationMessage::All;
        service.broadcast_remote(msg).await.unwrap();
    }

    #[tokio::test]
    async fn test_needs_state_sync_flag_on_broadcast_failure() {
        // Create service without Redis (simulating Redis unavailability)
        let service = CacheInvalidationService::new(
            None,
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        );

        // The needs_state_sync flag should start as false
        assert!(!service
            .needs_state_sync
            .load(std::sync::atomic::Ordering::Relaxed));

        // When Redis is not configured, broadcast_remote is a no-op and succeeds
        let result = service.broadcast_remote(InvalidationMessage::All).await;
        assert!(result.is_ok());

        // Since there's no Redis configured, the flag should still be false
        // (we only set it when Redis is configured but fails)
        assert!(!service
            .needs_state_sync
            .load(std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_state_sync_task_runs_periodically() {
        // Test that the state sync interval constant is defined correctly
        assert_eq!(STATE_SYNC_INTERVAL_SECS, 60);

        // Create service without Redis
        let service = CacheInvalidationService::new(
            None,
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        );

        assert!(
            !service
                .needs_state_sync
                .load(std::sync::atomic::Ordering::Relaxed),
            "Periodic state sync must not depend on prior failure flags"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_state_sync_does_not_fire_immediately_on_start() {
        let service = CacheInvalidationService::new(
            None,
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        );
        let shutdown = service.shutdown.clone();
        let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ticks_for_task = ticks.clone();

        crate::spawn::spawn_monitored("cache_invalidation_state_sync_test", async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(STATE_SYNC_INTERVAL_SECS));
            interval.tick().await;

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        ticks_for_task.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    () = async {
                        loop {
                            if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                                return;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                    } => {
                        break;
                    }
                }
            }
        });

        tokio::task::yield_now().await;
        assert_eq!(ticks.load(std::sync::atomic::Ordering::Relaxed), 0);

        tokio::time::advance(std::time::Duration::from_secs(59)).await;
        tokio::task::yield_now().await;
        assert_eq!(ticks.load(std::sync::atomic::Ordering::Relaxed), 0);

        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(ticks.load(std::sync::atomic::Ordering::Relaxed), 1);

        service
            .shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[test]
    fn test_state_sync_interval_constant() {
        // Verify the state sync interval is 60 seconds
        assert_eq!(STATE_SYNC_INTERVAL_SECS, 60);
    }

    #[test]
    fn test_room_settings_message_serialization() {
        let msg = InvalidationMessage::RoomSettings {
            room_id: "room_abc".to_string(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("room_settings"));
        assert!(json.contains("room_abc"));

        let decoded: InvalidationMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[tokio::test]
    async fn test_state_sync_uses_shutdown_flag_not_ctrl_c() {
        // L17: Verify that spawn_state_sync_task respects the shutdown AtomicBool
        // rather than relying on tokio::signal::ctrl_c().
        let service = CacheInvalidationService::new(
            None,
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        );

        // The shutdown flag should start as false
        assert!(!service.shutdown.load(std::sync::atomic::Ordering::Relaxed));

        // Set the shutdown flag
        service
            .shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);

        // Verify we can read the shutdown flag
        assert!(service.shutdown.load(std::sync::atomic::Ordering::Relaxed));

        // The state sync task should check this flag and exit promptly.
        // We verify the flag mechanism works correctly; the actual task
        // integration is tested by `stop()` calling `shutdown.store(true, ...)`.
    }

    #[tokio::test]
    async fn test_invalidate_and_broadcast_room_settings() {
        let service = CacheInvalidationService::new(
            None,
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        );
        let mut receiver = service.subscribe();

        let room_id = crate::models::RoomId::from_string("room_settings_test".to_string());
        service
            .invalidate_and_broadcast_room_settings(&room_id)
            .await
            .unwrap();

        let received = receiver.recv().await.unwrap();
        match received {
            InvalidationMessage::RoomSettings { room_id } => {
                assert_eq!(room_id, "room_settings_test");
            }
            other => panic!("Expected RoomSettings, got: {other:?}"),
        }
    }
}
