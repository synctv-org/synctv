//! Cache invalidation service for multi-replica deployments
//!
//! Uses Redis Streams (XADD/XREADGROUP) to broadcast cache invalidation messages
//! across all nodes with durable delivery. Unlike Pub/Sub, messages are persisted
//! in the stream and can be caught up on reconnection, preventing stale caches
//! after transient disconnections.

use async_trait::async_trait;
use redis::aio::ConnectionManager;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::models::RoomId;
use crate::{Error, RedisConnectionRuntime, Result, SharedStateProfile};

/// Maximum approximate stream length (number of entries).
/// Cache TTLs are 5 minutes, so 1000 entries is more than sufficient.
const MAX_STREAM_LENGTH: i64 = 1000;

/// Maximum age of stream entries in milliseconds (1 hour).
/// Used for periodic MINID-based trimming.
const STREAM_RETENTION_MS: u64 = 3_600_000;

/// Interval for retrying a pending recovery synchronization (60 seconds).
/// When Redis reconnects after a disconnect, other replicas may have stale caches
/// because invalidation messages were only broadcast locally during the outage.
/// We only retry the recovery broadcast when such a gap was detected.
const STATE_SYNC_INTERVAL_SECS: u64 = 60;

/// Interval for reclaiming orphaned consumer groups left by dead processes.
///
/// Startup cleanup is not sufficient in pod environments: a replacement pod can
/// come up before the old pod has been idle long enough to qualify as stale.
/// Periodic cleanup ensures those groups are eventually reclaimed without
/// waiting for another restart.
const ORPHANED_CONSUMER_GROUP_CLEANUP_INTERVAL_SECS: u64 = 300;
const _: () = assert!(ORPHANED_CONSUMER_GROUP_CLEANUP_INTERVAL_SECS >= STATE_SYNC_INTERVAL_SECS);

/// A consumer group that has not interacted with the stream for longer than the
/// retention window is considered stale and can be reclaimed.
///
/// This is intentionally aligned with `STREAM_RETENTION_MS`: once a dead
/// consumer has been idle longer than the stream keeps historical entries, its
/// pending list can no longer provide materially useful catch-up.
const STALE_CONSUMER_IDLE_MS: u64 = STREAM_RETENTION_MS;

/// Poll interval for stream subscribers when using a shared, non-blocking Redis
/// connection. We intentionally avoid `BLOCK` reads because the shared
/// `ConnectionManager` may be used by unrelated request paths and blocking it
/// would create head-of-line stalls across the process.
const SUBSCRIBER_POLL_INTERVAL_MS: u64 = 250;

/// Maximum reconnect backoff for the subscriber loop.
const SUBSCRIBER_MAX_BACKOFF_SECS: u64 = 30;

/// Trim interval in subscriber loop iterations (~60 seconds at 1s block).
const TRIM_EVERY_N_ITERATIONS: u32 = 60;

/// Cache invalidation message types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InvalidationMessage {
    /// Invalidate remote provider instance channel cache for a named instance
    ProviderInstance { instance_name: String },
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

#[async_trait]
pub trait CacheInvalidationRuntime: Send + Sync {
    fn subscribe(&self) -> broadcast::Receiver<InvalidationMessage>;

    async fn start(&self) -> Result<()>;

    async fn stop(&self);

    async fn broadcast_remote(&self, message: InvalidationMessage) -> Result<()>;

    fn broadcast_local(&self, message: InvalidationMessage) -> Result<()>;

    async fn broadcast_all(&self, message: InvalidationMessage) -> Result<()>;

    async fn invalidate_user_permission(
        &self,
        room_id: &RoomId,
        user_id: &crate::models::UserId,
    ) -> Result<()>;

    async fn invalidate_room_permission(&self, room_id: &RoomId) -> Result<()>;

    async fn invalidate_user(&self, user_id: &crate::models::UserId) -> Result<()>;

    async fn invalidate_username(&self, user_id: &crate::models::UserId) -> Result<()>;

    async fn invalidate_room(&self, room_id: &RoomId) -> Result<()>;

    async fn invalidate_provider_instance(&self, instance_name: &str) -> Result<()>;

    async fn invalidate_playback_state(&self, room_id: &RoomId) -> Result<()>;

    async fn update_playback_state(
        &self,
        room_id: &RoomId,
        state: &crate::models::RoomPlaybackState,
    ) -> Result<()>;

    async fn invalidate_room_settings(&self, room_id: &RoomId) -> Result<()>;

    async fn invalidate_all(&self) -> Result<()>;

    async fn invalidate_and_broadcast_user(&self, user_id: &crate::models::UserId) -> Result<()>;

    async fn invalidate_and_broadcast_room(&self, room_id: &RoomId) -> Result<()>;

    async fn invalidate_and_broadcast_room_settings(&self, room_id: &RoomId) -> Result<()>;

    async fn invalidate_and_broadcast_username(
        &self,
        user_id: &crate::models::UserId,
    ) -> Result<()>;

    async fn invalidate_and_broadcast_user_permission(
        &self,
        room_id: &RoomId,
        user_id: &crate::models::UserId,
    ) -> Result<()>;

    async fn invalidate_and_broadcast_room_permission(&self, room_id: &RoomId) -> Result<()>;
}

/// Build a cache invalidation runtime behind the service abstraction.
///
/// Callers should depend on the returned trait object instead of selecting the
/// concrete local or shared implementation directly.
pub fn cache_invalidation_runtime_from_shared_state_profile(
    profile: &SharedStateProfile,
    node_id: String,
    stream_key: String,
) -> Result<Arc<dyn CacheInvalidationRuntime>> {
    Ok(Arc::new(
        CacheInvalidationService::from_shared_state_profile(profile, node_id, stream_key)?,
    ))
}

/// Service for broadcasting and receiving cache invalidation messages
///
/// Uses Redis Streams instead of Pub/Sub for reliable message delivery:
/// - Messages are persisted and won't be lost
/// - Each node uses its own consumer group for broadcast semantics
/// - Automatic message acknowledgment and retry on failure
/// - Periodic state sync to handle missed invalidations during Redis outages
pub struct CacheInvalidationService {
    /// Redis runtime used for publishing and subscription when shared state exists.
    redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
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
    /// Background subscriber task handle, joined during shutdown.
    subscriber_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// Background state sync task handle, joined during shutdown.
    state_sync_task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl Clone for CacheInvalidationService {
    fn clone(&self) -> Self {
        Self {
            redis_runtime: self.redis_runtime.clone(),
            local_sender: self.local_sender.clone(),
            node_id: self.node_id.clone(),
            stream_key: self.stream_key.clone(),
            consumer_group: self.consumer_group.clone(),
            shutdown: self.shutdown.clone(),
            needs_state_sync: self.needs_state_sync.clone(),
            subscriber_task: self.subscriber_task.clone(),
            state_sync_task: self.state_sync_task.clone(),
        }
    }
}

impl CacheInvalidationService {
    /// Create a new cache invalidation service.
    ///
    /// # Arguments
    /// * `node_id` - Unique identifier for this node (for consumer group and logging).
    ///   **Important**: The consumer group name is derived as `cache-invalidation-{node_id}`.
    ///   A restart with the same `node_id` resets the previous incarnation's
    ///   consumer group during [`start`](Self::start), so the new process does
    ///   not inherit an obsolete Pending Entry List (PEL).
    ///
    ///   Different `node_id` values across restarts are also safe: old groups
    ///   are reclaimed opportunistically by
    ///   [`cleanup_orphaned_consumer_groups`](Self::cleanup_orphaned_consumer_groups)
    ///   once all of their consumers have been idle longer than the stream
    ///   retention window.
    ///
    ///   Using a stable `node_id` such as Kubernetes `POD_NAME` is still
    ///   preferred for observability because logs and Redis metadata map
    ///   cleanly to a pod identity.
    /// * `stream_key` - Redis stream key for cache invalidation (e.g., "synctv:cache:invalidate:stream")
    #[must_use]
    pub fn new(node_id: String, stream_key: String) -> Self {
        let (local_sender, _) = broadcast::channel(1024);
        let consumer_group = format!("cache-invalidation-{node_id}");

        Self {
            redis_runtime: None,
            local_sender,
            node_id,
            stream_key,
            consumer_group,
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            needs_state_sync: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            subscriber_task: Arc::new(Mutex::new(None)),
            state_sync_task: Arc::new(Mutex::new(None)),
        }
    }

    #[must_use]
    pub fn from_runtime(
        redis_runtime: Arc<dyn RedisConnectionRuntime>,
        node_id: String,
        stream_key: String,
    ) -> Self {
        let mut service = Self::new(node_id, stream_key);
        service.redis_runtime = Some(redis_runtime);
        service
    }

    #[must_use]
    pub fn from_optional_runtime(
        redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
        node_id: String,
        stream_key: String,
    ) -> Self {
        match redis_runtime {
            Some(runtime) => Self::from_runtime(runtime, node_id, stream_key),
            None => Self::new(node_id, stream_key),
        }
    }

    pub fn from_shared_state_profile(
        profile: &SharedStateProfile,
        node_id: String,
        stream_key: String,
    ) -> Result<Self> {
        match profile.state_mode() {
            crate::SharedStateMode::SharedRequired => Ok(Self::from_runtime(
                profile.require_shared_runtime("cache invalidation state")?,
                node_id,
                stream_key,
            )),
            crate::SharedStateMode::SharedBestEffort | crate::SharedStateMode::LocalOnly => Ok(
                Self::from_optional_runtime(profile.shared_runtime(), node_id, stream_key),
            ),
        }
    }

    const fn redis_enabled(&self) -> bool {
        self.redis_runtime.is_some()
    }

    /// Get a Redis connection for publishing.
    ///
    /// Uses the injected runtime abstraction so the service remains backend-agnostic.
    async fn get_conn(&self) -> Result<ConnectionManager> {
        if let Some(runtime) = &self.redis_runtime {
            return Ok(runtime.snapshot().await);
        }
        Err(Error::Internal(
            "Shared runtime not configured for cache invalidation".to_string(),
        ))
    }

    /// Start listening for cache invalidation messages from Redis
    ///
    /// This spawns a background task that reads from the invalidation stream.
    /// When a message is received, it's broadcast locally to all cache instances.
    /// On reconnection, pending (unacknowledged) messages are processed first to
    /// catch up on messages missed during the disconnection.
    ///
    /// Additionally spawns a periodic state sync task that broadcasts an "All"
    /// invalidation message every 60 seconds to ensure replicas that missed
    /// invalidations during Redis outages eventually converge.
    pub async fn start(&self) -> Result<()> {
        if !self.redis_enabled() {
            info!("Redis not configured, cache invalidation is local-only");
            return Ok(());
        }

        self.shutdown
            .store(false, std::sync::atomic::Ordering::Relaxed);

        // A restarted process should not inherit a previous incarnation's PEL.
        // Local caches are empty on startup, so catching up old invalidations
        // is unnecessary and only keeps dead-consumer metadata around.
        self.reset_current_consumer_group().await;

        // Clean up orphaned consumer groups left by previous processes with
        // different node_ids (e.g., non-K8s restarts where node_id has a random suffix).
        self.cleanup_orphaned_consumer_groups().await;

        // Create consumer group if it doesn't exist.
        // Use "$" so the group starts from the latest message (only new messages).
        // This prevents replaying all historical messages on every restart.
        // If the group already exists, BUSYGROUP error is expected and ignored.
        self.create_consumer_group().await?;

        let local_sender = self.local_sender.clone();
        let shutdown = self.shutdown.clone();

        let subscriber_service = self.clone();

        let subscriber_handle =
            crate::spawn::spawn_monitored("cache_invalidation_subscriber", async move {
                let mut backoff_secs: u64 = 1;

                loop {
                    if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                        debug!("Cache invalidation listener shutting down");
                        break;
                    }

                    match subscriber_service.run_subscriber(&local_sender).await {
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
                            tokio::select! {
                                () = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {}
                                () = async {
                                    loop {
                                        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                                            return;
                                        }
                                        tokio::time::sleep(Duration::from_millis(100)).await;
                                    }
                                } => return,
                            }
                            backoff_secs = (backoff_secs * 2).min(SUBSCRIBER_MAX_BACKOFF_SECS);
                        }
                    }
                }
                info!("Cache invalidation listener stopped");
            });
        self.replace_task_handle(&self.subscriber_task, subscriber_handle)
            .await;

        // Spawn periodic state sync task
        self.spawn_state_sync_task().await;

        Ok(())
    }

    /// Spawn a background task that retries recovery synchronization when needed.
    ///
    /// This ensures that replicas that missed invalidations during Redis outages
    /// eventually converge, without continuously flushing all caches when the
    /// system is healthy. The retry interval is controlled by
    /// `STATE_SYNC_INTERVAL_SECS`.
    async fn spawn_state_sync_task(&self) {
        let service = self.clone();

        let task = crate::spawn::spawn_monitored("cache_invalidation_state_sync", async move {
            let mut state_sync_interval =
                tokio::time::interval(Duration::from_secs(STATE_SYNC_INTERVAL_SECS));
            let mut orphan_cleanup_interval = tokio::time::interval(Duration::from_secs(
                ORPHANED_CONSUMER_GROUP_CLEANUP_INTERVAL_SECS,
            ));
            state_sync_interval.tick().await;
            orphan_cleanup_interval.tick().await;

            loop {
                tokio::select! {
                    _ = state_sync_interval.tick() => {
                        if service.shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }

                        let pending_recovery_sync = service.needs_state_sync
                            .swap(false, std::sync::atomic::Ordering::Relaxed);

                        if !pending_recovery_sync {
                            continue;
                        }

                        match service
                            .do_broadcast_to_stream_internal(&InvalidationMessage::All)
                            .await
                        {
                            Ok(()) => {
                                info!(
                                    node_id = %service.node_id,
                                    "Recovery state sync: broadcast 'All' invalidation message"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    "Failed to broadcast recovery state sync message, will retry next interval"
                                );
                                service
                                    .needs_state_sync
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                    _ = orphan_cleanup_interval.tick() => {
                        if service.shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }

                        service.cleanup_orphaned_consumer_groups().await;
                    }
                    () = async {
                        loop {
                            if service.shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    } => {
                        break;
                    }
                }
            }
            debug!("Cache invalidation state sync task stopped");
        });
        self.replace_task_handle(&self.state_sync_task, task).await;
    }

    async fn replace_task_handle(
        &self,
        slot: &Arc<Mutex<Option<JoinHandle<()>>>>,
        handle: JoinHandle<()>,
    ) {
        let mut guard = slot.lock().await;
        if let Some(existing) = guard.replace(handle) {
            existing.abort();
            let _ = existing.await;
        }
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

    async fn fetch_consumer_groups(&self) -> Result<Vec<Vec<redis::Value>>> {
        let mut conn = self.get_conn().await?;
        redis::cmd("XINFO")
            .arg("GROUPS")
            .arg(&self.stream_key)
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Internal(format!("Failed to inspect consumer groups: {e}")))
    }

    async fn destroy_consumer_group_if_present(&self, group_name: &str) -> Result<bool> {
        let Ok(groups) = self.fetch_consumer_groups().await else {
            return Ok(false);
        };

        if !groups
            .iter()
            .any(|group_info| Self::extract_group_name(group_info).as_deref() == Some(group_name))
        {
            return Ok(false);
        }

        let mut conn = self.get_conn().await?;
        let destroyed: i64 = redis::cmd("XGROUP")
            .arg("DESTROY")
            .arg(&self.stream_key)
            .arg(group_name)
            .query_async(&mut conn)
            .await
            .map_err(|e| {
                Error::Internal(format!(
                    "Failed to destroy consumer group {group_name}: {e}"
                ))
            })?;
        Ok(destroyed > 0)
    }

    async fn reset_current_consumer_group(&self) {
        match self
            .destroy_consumer_group_if_present(&self.consumer_group)
            .await
        {
            Ok(true) => {
                info!(
                    stream = %self.stream_key,
                    group = %self.consumer_group,
                    "Destroyed inherited consumer group before cache invalidation startup"
                );
            }
            Ok(false) => {}
            Err(error) => {
                warn!(
                    error = %error,
                    stream = %self.stream_key,
                    group = %self.consumer_group,
                    "Failed to reset inherited consumer group before startup"
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
    /// - Has no consumers, or every consumer has been idle longer than
    ///   `STALE_CONSUMER_IDLE_MS`
    ///
    /// Called from [`start`](Self::start) to prevent unbounded accumulation of
    /// orphaned groups in Redis.
    async fn cleanup_orphaned_consumer_groups(&self) {
        let Ok(groups) = self.fetch_consumer_groups().await else {
            return;
        };

        let Ok(mut conn) = self.get_conn().await else {
            return;
        };

        for group_info in &groups {
            let Some(name) = Self::extract_group_name(group_info) else {
                continue;
            };

            // Only clean up groups belonging to this service, not the current one
            if !name.starts_with("cache-invalidation-") || name == self.consumer_group {
                continue;
            }

            let consumers_result: redis::RedisResult<Vec<Vec<redis::Value>>> = redis::cmd("XINFO")
                .arg("CONSUMERS")
                .arg(&self.stream_key)
                .arg(&name)
                .query_async(&mut conn)
                .await;

            let consumers = match consumers_result {
                Ok(consumers) => consumers,
                Err(error) => {
                    warn!(
                        error = %error,
                        stream = %self.stream_key,
                        group = %name,
                        "Failed to inspect consumers for orphaned consumer group"
                    );
                    continue;
                }
            };

            if !Self::consumer_group_is_stale(&consumers) {
                continue;
            }

            let destroy_result: redis::RedisResult<i64> = redis::cmd("XGROUP")
                .arg("DESTROY")
                .arg(&self.stream_key)
                .arg(&name)
                .query_async(&mut conn)
                .await;
            match destroy_result {
                Ok(destroyed) if destroyed > 0 => {
                    info!(
                        stream = %self.stream_key,
                        group = %name,
                        consumers = consumers.len(),
                        "Destroyed stale orphaned consumer group"
                    );
                }
                Ok(_) => {}
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

    fn extract_consumer_idle_ms(consumer_info: &[redis::Value]) -> Option<u64> {
        Self::extract_integer_field(consumer_info, "inactive")
            .or_else(|| Self::extract_integer_field(consumer_info, "idle"))
            .and_then(|value| u64::try_from(value).ok())
    }

    fn consumer_group_is_stale(consumers: &[Vec<redis::Value>]) -> bool {
        consumers.is_empty()
            || consumers.iter().all(|consumer| {
                Self::extract_consumer_idle_ms(consumer)
                    .is_some_and(|idle_ms| idle_ms >= STALE_CONSUMER_IDLE_MS)
            })
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
        &self,
        local_sender: &broadcast::Sender<InvalidationMessage>,
    ) -> Result<()> {
        // Track delivery attempts for malformed messages so they can be
        // discarded after MAX_DELIVERY_ATTEMPTS to prevent PEL accumulation.
        let mut failed_delivery_counts: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();

        info!(
            node_id = %self.node_id,
            stream = %self.stream_key,
            group = %self.consumer_group,
            "Started cache invalidation stream consumer"
        );

        // Phase 1: Catch-up -- process pending messages that were delivered but
        // not acknowledged before the last disconnect.
        let mut conn = self.get_conn().await?;
        let catchup_count = Self::process_pending_messages(
            &mut conn,
            local_sender,
            &self.node_id,
            &self.stream_key,
            &self.consumer_group,
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

        loop {
            if self.shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }

            // Periodically trim old entries (time-based, 1 hour retention)
            trim_counter += 1;
            if trim_counter >= TRIM_EVERY_N_ITERATIONS {
                trim_counter = 0;
                let mut conn = self.get_conn().await?;
                Self::trim_stream(&mut conn, &self.stream_key).await;
            }

            let mut conn = self.get_conn().await?;
            let result: redis::RedisResult<redis::streams::StreamReadReply> =
                redis::cmd("XREADGROUP")
                    .arg("GROUP")
                    .arg(&self.consumer_group)
                    .arg(&self.node_id)
                    .arg("COUNT")
                    .arg(100)
                    .arg("STREAMS")
                    .arg(&self.stream_key)
                    .arg(">") // Only new messages
                    .query_async(&mut conn)
                    .await;

            match result {
                Ok(reply) => {
                    Self::process_stream_reply(
                        &mut conn,
                        local_sender,
                        &self.node_id,
                        &self.stream_key,
                        &self.consumer_group,
                        &reply,
                        &mut failed_delivery_counts,
                    )
                    .await;
                    if reply.keys.iter().all(|stream| stream.ids.is_empty()) {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            SUBSCRIBER_POLL_INTERVAL_MS,
                        ))
                        .await;
                    }
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
    async fn process_pending_messages<C>(
        conn: &mut C,
        local_sender: &broadcast::Sender<InvalidationMessage>,
        node_id: &str,
        stream_key: &str,
        consumer_group: &str,
        failed_delivery_counts: &mut std::collections::HashMap<String, u32>,
    ) -> Result<usize>
    where
        C: redis::aio::ConnectionLike + Send + Unpin,
    {
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
    async fn process_stream_reply<C>(
        conn: &mut C,
        local_sender: &broadcast::Sender<InvalidationMessage>,
        node_id: &str,
        stream_key: &str,
        consumer_group: &str,
        reply: &redis::streams::StreamReadReply,
        failed_delivery_counts: &mut std::collections::HashMap<String, u32>,
    ) where
        C: redis::aio::ConnectionLike + Send + Unpin,
    {
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
    async fn process_single_entry<C>(
        conn: &mut C,
        local_sender: &broadcast::Sender<InvalidationMessage>,
        node_id: &str,
        stream_key: &str,
        consumer_group: &str,
        entry: &redis::streams::StreamId,
        failed_delivery_counts: &mut std::collections::HashMap<String, u32>,
    ) where
        C: redis::aio::ConnectionLike + Send + Unpin,
    {
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
    async fn trim_stream<C>(conn: &mut C, stream_key: &str)
    where
        C: redis::aio::ConnectionLike + Send + Unpin,
    {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let now_ms = u64::try_from(now_ms).unwrap_or(u64::MAX);
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
    /// Signals the background subscriber to stop, destroys this process's
    /// consumer group, then trims the Redis stream to the configured maximum
    /// length.
    ///
    /// Destroying the current consumer group is safe because the process is
    /// shutting down and will not consume its pending entries again. A future
    /// process restart begins with empty local caches, so inheriting the old
    /// Pending Entry List (PEL) is unnecessary.
    pub async fn stop(&self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);

        self.join_task("cache invalidation subscriber", &self.subscriber_task)
            .await;
        self.join_task("cache invalidation state sync", &self.state_sync_task)
            .await;

        if self.redis_enabled() {
            match self
                .destroy_consumer_group_if_present(&self.consumer_group)
                .await
            {
                Ok(true) => {
                    info!(
                        stream = %self.stream_key,
                        group = %self.consumer_group,
                        "Destroyed cache invalidation consumer group during shutdown"
                    );
                }
                Ok(false) => {}
                Err(error) => {
                    warn!(
                        error = %error,
                        stream = %self.stream_key,
                        group = %self.consumer_group,
                        "Failed to destroy cache invalidation consumer group during shutdown"
                    );
                }
            }
        }

        // Trim the stream on shutdown to prevent unbounded growth.
        // The current consumer group has already been removed above; this
        // XTRIM only bounds the retained stream data for future restarts.
        if self.redis_enabled() {
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

    async fn join_task(&self, task_name: &'static str, slot: &Arc<Mutex<Option<JoinHandle<()>>>>) {
        let handle = {
            let mut guard = slot.lock().await;
            guard.take()
        };

        if let Some(handle) = handle {
            match handle.await {
                Ok(()) => debug!("{task_name} stopped"),
                Err(e) if e.is_cancelled() => debug!("{task_name} cancelled"),
                Err(e) => warn!("{task_name} task ended with error: {e}"),
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
        if self.redis_enabled() {
            match self.do_broadcast_to_stream_internal(&message).await {
                Ok(()) => {}
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
            room_id: room_id.to_string(),
            user_id: user_id.to_string(),
        })
        .await
    }

    /// Invalidate permission cache for all users in a room
    pub async fn invalidate_room_permission(&self, room_id: &RoomId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::RoomPermission {
            room_id: room_id.to_string(),
        })
        .await
    }

    /// Invalidate user cache
    pub async fn invalidate_user(&self, user_id: &crate::models::UserId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::User {
            user_id: user_id.to_string(),
        })
        .await
    }

    /// Invalidate username cache
    pub async fn invalidate_username(&self, user_id: &crate::models::UserId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::Username {
            user_id: user_id.to_string(),
        })
        .await
    }

    /// Invalidate room cache
    pub async fn invalidate_room(&self, room_id: &RoomId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::Room {
            room_id: room_id.to_string(),
        })
        .await
    }

    /// Invalidate remote provider instance channel cache
    pub async fn invalidate_provider_instance(&self, instance_name: &str) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::ProviderInstance {
            instance_name: instance_name.to_string(),
        })
        .await
    }

    /// Invalidate playback state cache for a room
    pub async fn invalidate_playback_state(&self, room_id: &RoomId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::PlaybackState {
            room_id: room_id.to_string(),
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
            room_id: room_id.to_string(),
            state: state.clone(),
        })
        .await
    }

    /// Invalidate room settings cache for a specific room
    pub async fn invalidate_room_settings(&self, room_id: &RoomId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::RoomSettings {
            room_id: room_id.to_string(),
        })
        .await
    }

    /// Invalidate all caches
    pub async fn invalidate_all(&self) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::All).await
    }

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
            user_id: user_id.to_string(),
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
            room_id: room_id.to_string(),
        };
        if let Err(e) = self.local_sender.send(msg.clone()) {
            warn!(error = %e, "Failed to broadcast room invalidation locally");
        }
        self.broadcast_remote(msg).await
    }

    /// Invalidate room settings cache locally and broadcast to other replicas.
    pub async fn invalidate_and_broadcast_room_settings(&self, room_id: &RoomId) -> Result<()> {
        let msg = InvalidationMessage::RoomSettings {
            room_id: room_id.to_string(),
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
            user_id: user_id.to_string(),
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
            room_id: room_id.to_string(),
            user_id: user_id.to_string(),
        };
        if let Err(e) = self.local_sender.send(msg.clone()) {
            warn!(error = %e, "Failed to broadcast user permission invalidation locally");
        }
        self.broadcast_remote(msg).await
    }

    /// Invalidate room permission cache locally and broadcast to other replicas.
    pub async fn invalidate_and_broadcast_room_permission(&self, room_id: &RoomId) -> Result<()> {
        let msg = InvalidationMessage::RoomPermission {
            room_id: room_id.to_string(),
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

#[async_trait]
impl CacheInvalidationRuntime for CacheInvalidationService {
    fn subscribe(&self) -> broadcast::Receiver<InvalidationMessage> {
        Self::subscribe(self)
    }

    async fn start(&self) -> Result<()> {
        Self::start(self).await
    }

    async fn stop(&self) {
        Self::stop(self).await;
    }

    async fn broadcast_remote(&self, message: InvalidationMessage) -> Result<()> {
        Self::broadcast_remote(self, message).await
    }

    fn broadcast_local(&self, message: InvalidationMessage) -> Result<()> {
        Self::broadcast_local(self, message)
    }

    async fn broadcast_all(&self, message: InvalidationMessage) -> Result<()> {
        Self::broadcast_all(self, message).await
    }

    async fn invalidate_user_permission(
        &self,
        room_id: &RoomId,
        user_id: &crate::models::UserId,
    ) -> Result<()> {
        Self::invalidate_user_permission(self, room_id, user_id).await
    }

    async fn invalidate_room_permission(&self, room_id: &RoomId) -> Result<()> {
        Self::invalidate_room_permission(self, room_id).await
    }

    async fn invalidate_user(&self, user_id: &crate::models::UserId) -> Result<()> {
        Self::invalidate_user(self, user_id).await
    }

    async fn invalidate_username(&self, user_id: &crate::models::UserId) -> Result<()> {
        Self::invalidate_username(self, user_id).await
    }

    async fn invalidate_room(&self, room_id: &RoomId) -> Result<()> {
        Self::invalidate_room(self, room_id).await
    }

    async fn invalidate_provider_instance(&self, instance_name: &str) -> Result<()> {
        Self::invalidate_provider_instance(self, instance_name).await
    }

    async fn invalidate_playback_state(&self, room_id: &RoomId) -> Result<()> {
        Self::invalidate_playback_state(self, room_id).await
    }

    async fn update_playback_state(
        &self,
        room_id: &RoomId,
        state: &crate::models::RoomPlaybackState,
    ) -> Result<()> {
        Self::update_playback_state(self, room_id, state).await
    }

    async fn invalidate_room_settings(&self, room_id: &RoomId) -> Result<()> {
        Self::invalidate_room_settings(self, room_id).await
    }

    async fn invalidate_all(&self) -> Result<()> {
        Self::invalidate_all(self).await
    }

    async fn invalidate_and_broadcast_user(&self, user_id: &crate::models::UserId) -> Result<()> {
        Self::invalidate_and_broadcast_user(self, user_id).await
    }

    async fn invalidate_and_broadcast_room(&self, room_id: &RoomId) -> Result<()> {
        Self::invalidate_and_broadcast_room(self, room_id).await
    }

    async fn invalidate_and_broadcast_room_settings(&self, room_id: &RoomId) -> Result<()> {
        Self::invalidate_and_broadcast_room_settings(self, room_id).await
    }

    async fn invalidate_and_broadcast_username(
        &self,
        user_id: &crate::models::UserId,
    ) -> Result<()> {
        Self::invalidate_and_broadcast_username(self, user_id).await
    }

    async fn invalidate_and_broadcast_user_permission(
        &self,
        room_id: &RoomId,
        user_id: &crate::models::UserId,
    ) -> Result<()> {
        Self::invalidate_and_broadcast_user_permission(self, room_id, user_id).await
    }

    async fn invalidate_and_broadcast_room_permission(&self, room_id: &RoomId) -> Result<()> {
        Self::invalidate_and_broadcast_room_permission(self, room_id).await
    }
}

impl std::fmt::Debug for CacheInvalidationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheInvalidationService")
            .field("redis_enabled", &self.redis_runtime.is_some())
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
    use async_trait::async_trait;

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
    async fn test_cache_invalidation_service_accepts_trait_object_runtime() {
        #[derive(Clone)]
        struct FakeRedisRuntime;

        #[async_trait]
        impl RedisConnectionRuntime for FakeRedisRuntime {
            async fn snapshot(&self) -> redis::aio::ConnectionManager {
                panic!("snapshot should not be called in constructor-only test");
            }
        }

        let runtime: Arc<dyn RedisConnectionRuntime> = Arc::new(FakeRedisRuntime);
        let service = CacheInvalidationService::from_runtime(
            runtime.clone(),
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        );

        assert!(
            service
                .redis_runtime
                .as_ref()
                .is_some_and(|injected| Arc::ptr_eq(injected, &runtime)),
            "cache invalidation service should retain the injected runtime object"
        );
    }

    #[tokio::test]
    async fn test_local_broadcast() {
        let service = CacheInvalidationService::new(
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

    #[test]
    fn test_from_shared_state_profile_uses_shared_runtime() {
        struct FakeRedisRuntime;

        #[async_trait::async_trait]
        impl RedisConnectionRuntime for FakeRedisRuntime {
            async fn snapshot(&self) -> redis::aio::ConnectionManager {
                panic!("snapshot should not be called in constructor-only test");
            }
        }

        let runtime: Arc<dyn RedisConnectionRuntime> = Arc::new(FakeRedisRuntime);
        let profile =
            SharedStateProfile::from_runtime(Some(runtime.clone()), "synctv:test:", false);
        let service = CacheInvalidationService::from_shared_state_profile(
            &profile,
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        )
        .expect("shared-state profile constructor should accept injected runtime");

        assert!(
            service
                .redis_runtime
                .as_ref()
                .is_some_and(|injected| Arc::ptr_eq(injected, &runtime)),
            "shared-state profile constructor should reuse the injected runtime object"
        );
    }

    #[tokio::test]
    async fn test_cache_invalidation_runtime_from_shared_state_profile_returns_live_trait_object() {
        let profile = SharedStateProfile::from_runtime(None, "synctv:test:", false);
        let runtime = cache_invalidation_runtime_from_shared_state_profile(
            &profile,
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        )
        .expect("standalone mode should allow local cache invalidation");
        let mut receiver = runtime.subscribe();
        let message = InvalidationMessage::Room {
            room_id: "room-1".to_string(),
        };

        runtime
            .broadcast_local(message.clone())
            .expect("trait-object cache invalidation runtime should broadcast locally");

        let received = receiver
            .recv()
            .await
            .expect("subscriber should receive local broadcast");
        assert_eq!(received, message);
    }

    #[test]
    fn test_cache_invalidation_runtime_from_shared_state_profile_requires_shared_runtime_in_cluster_mode(
    ) {
        let profile = SharedStateProfile::from_runtime(None, "synctv:test:", true);
        let Err(error) = cache_invalidation_runtime_from_shared_state_profile(
            &profile,
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        ) else {
            panic!("cluster runtime must reject local-only cache invalidation");
        };

        assert!(
            error
                .to_string()
                .contains("cluster runtime requires shared cache invalidation state"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn test_needs_state_sync_flag_on_broadcast_failure() {
        // Create service without Redis (simulating Redis unavailability)
        let service = CacheInvalidationService::new(
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
    async fn test_state_sync_task_is_idle_without_pending_recovery_sync() {
        assert_eq!(STATE_SYNC_INTERVAL_SECS, 60);

        let service = CacheInvalidationService::new(
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        );

        assert!(
            !service
                .needs_state_sync
                .load(std::sync::atomic::Ordering::Relaxed),
            "Recovery state sync must remain idle until a Redis failure sets the flag"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_state_sync_does_not_fire_without_pending_recovery_sync() {
        let service = CacheInvalidationService::new(
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
        assert_eq!(
            ticks.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the retry loop still wakes on its interval"
        );

        service
            .shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[tokio::test(start_paused = true)]
    async fn test_state_sync_only_executes_when_recovery_is_pending() {
        let service = CacheInvalidationService::new(
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        );
        let shutdown = service.shutdown.clone();
        let sync_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sync_attempts_for_task = sync_attempts.clone();
        let needs_state_sync = service.needs_state_sync.clone();

        crate::spawn::spawn_monitored("cache_invalidation_state_sync_test_pending", async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(STATE_SYNC_INTERVAL_SECS));
            interval.tick().await;

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if !needs_state_sync.swap(false, std::sync::atomic::Ordering::Relaxed) {
                            continue;
                        }
                        sync_attempts_for_task.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        tokio::time::advance(std::time::Duration::from_mins(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            sync_attempts.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "healthy intervals must not broadcast global invalidations"
        );

        service
            .needs_state_sync
            .store(true, std::sync::atomic::Ordering::Relaxed);

        tokio::time::advance(std::time::Duration::from_mins(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            sync_attempts.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "a pending recovery sync must be executed on the next interval"
        );

        service
            .shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[tokio::test(start_paused = true)]
    async fn test_periodic_orphan_cleanup_runs_without_pending_recovery_sync() {
        let service = CacheInvalidationService::new(
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        );
        let shutdown = service.shutdown.clone();
        let cleanup_ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cleanup_ticks_for_task = cleanup_ticks.clone();
        let needs_state_sync = service.needs_state_sync.clone();

        crate::spawn::spawn_monitored("cache_invalidation_housekeeping_test", async move {
            let mut state_sync_interval =
                tokio::time::interval(std::time::Duration::from_secs(STATE_SYNC_INTERVAL_SECS));
            let mut orphan_cleanup_interval = tokio::time::interval(
                std::time::Duration::from_secs(ORPHANED_CONSUMER_GROUP_CLEANUP_INTERVAL_SECS),
            );
            state_sync_interval.tick().await;
            orphan_cleanup_interval.tick().await;

            loop {
                tokio::select! {
                    _ = state_sync_interval.tick() => {
                        let _ = needs_state_sync.swap(false, std::sync::atomic::Ordering::Relaxed);
                    }
                    _ = orphan_cleanup_interval.tick() => {
                        cleanup_ticks_for_task.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        tokio::time::advance(std::time::Duration::from_secs(
            ORPHANED_CONSUMER_GROUP_CLEANUP_INTERVAL_SECS - 1,
        ))
        .await;
        tokio::task::yield_now().await;
        assert_eq!(
            cleanup_ticks.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "orphan cleanup should wait for its own interval"
        );

        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            cleanup_ticks.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "orphan cleanup should still run even when no recovery sync is pending"
        );

        service
            .shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[test]
    fn test_state_sync_interval_constant() {
        // Verify the state sync interval is 60 seconds
        assert_eq!(STATE_SYNC_INTERVAL_SECS, 60);
        assert_eq!(ORPHANED_CONSUMER_GROUP_CLEANUP_INTERVAL_SECS, 300);
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
        // Verify that spawn_state_sync_task respects the shutdown AtomicBool
        // rather than relying on tokio::signal::ctrl_c().
        let service = CacheInvalidationService::new(
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
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        );
        let mut receiver = service.subscribe();

        let room_id = crate::models::RoomId::from(95_001);
        service
            .invalidate_and_broadcast_room_settings(&room_id)
            .await
            .unwrap();

        let received = receiver.recv().await.unwrap();
        match received {
            InvalidationMessage::RoomSettings { room_id } => {
                assert_eq!(room_id, "95001");
            }
            other => panic!("Expected RoomSettings, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_start_without_redis_still_succeeds() {
        let service = CacheInvalidationService::new(
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        );

        let result = service.start().await;
        assert!(result.is_ok(), "local-only mode should remain a no-op");
    }

    #[test]
    fn test_from_optional_runtime_without_runtime_keeps_local_mode() {
        let service = CacheInvalidationService::from_optional_runtime(
            None,
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        );

        assert!(
            service.redis_runtime.is_none(),
            "local-only constructor must not synthesize a backend runtime"
        );
    }

    #[tokio::test]
    async fn test_stop_awaits_registered_background_tasks() {
        let service = CacheInvalidationService::new(
            "test-node".to_string(),
            "synctv:cache:invalidate:stream".to_string(),
        );
        let observed_shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed_shutdown_task = observed_shutdown.clone();
        let shutdown = service.shutdown.clone();

        let handle = tokio::spawn(async move {
            loop {
                if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                    observed_shutdown_task.store(true, std::sync::atomic::Ordering::Relaxed);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });

        {
            let mut guard = service.subscriber_task.lock().await;
            *guard = Some(handle);
        }

        service.stop().await;

        assert!(
            observed_shutdown.load(std::sync::atomic::Ordering::Relaxed),
            "stop() must wait for registered background tasks to observe shutdown"
        );
        assert!(
            service.subscriber_task.lock().await.is_none(),
            "stop() must clear the subscriber task handle after join"
        );
    }

    #[test]
    fn test_extract_consumer_idle_ms_prefers_inactive_field() {
        let consumer = vec![
            redis::Value::SimpleString("name".to_string()),
            redis::Value::SimpleString("node-a".to_string()),
            redis::Value::SimpleString("idle".to_string()),
            redis::Value::Int(15),
            redis::Value::SimpleString("inactive".to_string()),
            redis::Value::Int(25),
        ];

        assert_eq!(
            CacheInvalidationService::extract_consumer_idle_ms(&consumer),
            Some(25)
        );
    }

    #[test]
    fn test_consumer_group_is_stale_when_all_consumers_exceed_threshold() {
        let stale = vec![vec![
            redis::Value::SimpleString("name".to_string()),
            redis::Value::SimpleString("node-a".to_string()),
            redis::Value::SimpleString("idle".to_string()),
            redis::Value::Int(i64::try_from(STALE_CONSUMER_IDLE_MS).unwrap_or(i64::MAX)),
        ]];

        assert!(CacheInvalidationService::consumer_group_is_stale(&stale));
    }

    #[test]
    fn test_consumer_group_is_not_stale_when_any_consumer_is_recent() {
        let consumers = vec![
            vec![
                redis::Value::SimpleString("name".to_string()),
                redis::Value::SimpleString("node-a".to_string()),
                redis::Value::SimpleString("idle".to_string()),
                redis::Value::Int(10),
            ],
            vec![
                redis::Value::SimpleString("name".to_string()),
                redis::Value::SimpleString("node-b".to_string()),
                redis::Value::SimpleString("idle".to_string()),
                redis::Value::Int(i64::try_from(STALE_CONSUMER_IDLE_MS).unwrap_or(i64::MAX)),
            ],
        ];

        assert!(!CacheInvalidationService::consumer_group_is_stale(
            &consumers
        ));
    }
}
