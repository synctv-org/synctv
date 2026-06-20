use dashmap::DashMap;
use redis::AsyncCommands;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use synctv_core::{
    models::id::{RoomId, UserId},
    service::OnlinePresenceService,
    RedisConnectionRuntime,
};
#[cfg(test)]
use synctv_core::{DirectRedisConnectionRuntime, SharedRedisConnectionRuntime};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

mod model;
pub use model::ConnectionInfo;
use model::{
    i64_to_usize_saturating, u64_to_usize_saturating, usize_to_i64_saturating,
    ConnectionInfoPersistent, RoomTransition, TimeoutIndex,
};
mod config;
pub use config::ConnectionLimits;
mod metrics;
pub use metrics::{ConnectionMetrics, ShutdownReport};
mod disconnects;
mod redis_maintenance;
mod redis_state;
use redis_state::{
    run_unregister_cleanup_script, spawn_pending_retries_task, PendingRedisOp,
    UnregisterCleanupScriptArgs, CONNECTION_METADATA_TTL_SECONDS, ROOM_INDEX_DIRECTORY_KEY_SUFFIX,
    USER_INDEX_DIRECTORY_KEY_SUFFIX,
};

#[cfg(test)]
type AsyncTestHook =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

/// Disconnect signal for forcing connections to close
#[derive(Debug, Clone)]
pub enum DisconnectSignal {
    /// Disconnect a specific connection
    Connection(String),
    /// Disconnect all connections for a user
    User(UserId),
    /// Disconnect all connections in a room
    Room(RoomId),
    /// Disconnect a specific user from a specific room
    UserFromRoom { user_id: UserId, room_id: RoomId },
}

struct ConnectionIdClaim<'a> {
    manager: &'a ConnectionManager,
    connection_id: String,
    committed: bool,
}

impl ConnectionIdClaim<'_> {
    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for ConnectionIdClaim<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.manager
                .release_connection_id_claim(&self.connection_id);
        }
    }
}

/// Connection manager for tracking active gRPC streaming connections
#[derive(Clone)]
pub struct ConnectionManager {
    /// All active connections by `connection_id`
    connections: Arc<DashMap<String, ConnectionInfo>>,

    /// Tracks connection IDs that are either fully registered or currently
    /// in-flight through `register()`. This closes the async TOCTOU window
    /// where two concurrent `register()` calls for the same connection_id
    /// could both pass an existence check before either inserts the connection.
    claimed_connection_ids: Arc<std::sync::Mutex<HashSet<String>>>,

    /// Connections by `user_id`
    user_connections: Arc<DashMap<UserId, Vec<String>>>,

    /// Connections by `room_id`
    room_connections: Arc<DashMap<RoomId, Vec<String>>>,

    /// Core-owned online presence lifecycle and queries.
    presence_service: Arc<OnlinePresenceService>,
    node_id: Arc<str>,

    /// Connection limits
    limits: Arc<ConnectionLimits>,

    /// Atomic total connection count for race-free limit enforcement.
    /// Incremented atomically during `register()`, decremented during `unregister()`.
    total_connections: Arc<AtomicUsize>,
    /// Deadline index used by timeout cleanup so periodic checks only examine
    /// connections whose idle/max-duration/RTC deadlines have actually elapsed.
    timeout_index: Arc<parking_lot::Mutex<TimeoutIndex>>,

    /// Metrics
    total_connections_ever: Arc<AtomicU64>,
    total_messages: Arc<AtomicU64>,
    #[cfg(test)]
    users_online_metric_increments: Arc<AtomicUsize>,
    #[cfg(test)]
    users_online_metric_decrements: Arc<AtomicUsize>,

    /// Broadcast channel for disconnect signals
    disconnect_tx: Arc<broadcast::Sender<DisconnectSignal>>,

    /// Pending room slot reservations (pre-upgrade).
    /// Counts how many WebSocket upgrades are in-flight for each room.
    /// Used to prevent TOCTOU race conditions on connection limits.
    pending_room_reservations: Arc<DashMap<RoomId, AtomicUsize>>,

    /// Pending user slot reservations (pre-upgrade).
    /// Counts how many WebSocket upgrades are in-flight for each user.
    pending_user_reservations: Arc<DashMap<UserId, AtomicUsize>>,

    /// Optional Redis connection handle for distributed connection counting.
    /// When present, per-user and per-room limits are enforced across all replicas.
    /// When absent, limits are per-node only (fallback).
    ///
    /// In Sentinel deployments, prefer the shared handle so new method calls
    /// observe failover hot-swaps instead of holding a stale connection snapshot.
    redis_conn: Option<Arc<dyn RedisConnectionRuntime>>,

    /// Key prefix for Redis keys (e.g., "synctv:")
    redis_key_prefix: String,

    /// Cancellation token for the auto-spawned TTL refresh task.
    /// Cancelled on shutdown to stop the background task.
    ttl_refresh_cancel: Arc<tokio_util::sync::CancellationToken>,

    /// JoinHandle for the TTL refresh task.
    ttl_refresh_handle: Arc<parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// JoinHandle for the pending Redis retries task.
    pending_retries_handle: Arc<parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>>,

    /// Channel for queuing failed Redis counter operations for background retry.
    /// When a Redis INCR/DECR fails during register/unregister, the operation is
    /// sent here so a background task can retry it, ensuring eventual consistency
    /// between local and distributed counters.
    ///
    /// Bounded to `PENDING_RETRY_QUEUE_CAPACITY` to prevent unbounded memory
    /// growth during prolonged Redis outages. When full, new entries are dropped
    /// with a warning (TTL-based expiry acts as the safety net).
    pending_retries_tx: mpsc::Sender<PendingRedisOp>,

    /// Receiver half of the pending-retries channel.
    ///
    /// Stored here (wrapped in `Arc<Mutex<Option<...>>>` to satisfy `Clone`) so
    /// the receiver is not dropped at construction. `with_redis()` takes the
    /// receiver out of this slot and hands it to `spawn_pending_retries_task`.
    /// Without this, any retry enqueued before Redis is configured would fail
    /// silently because the channel would be closed.
    pending_retries_rx: Arc<tokio::sync::Mutex<Option<mpsc::Receiver<PendingRedisOp>>>>,

    #[cfg(test)]
    join_room_before_commit_hook: Option<AsyncTestHook>,
    #[cfg(test)]
    join_room_before_capacity_check_hook: Option<AsyncTestHook>,
    #[cfg(test)]
    register_after_lifecycle_lock_hook: Option<AsyncTestHook>,

    /// Striped async mutexes that serialize lifecycle mutations for a single
    /// connection ID across register/join_room/unregister operations.
    connection_lifecycle_locks: Arc<Vec<Arc<tokio::sync::Mutex<()>>>>,
}

/// Maximum capacity of the pending retry queue for failed Redis counter operations.
/// When the queue is full, new entries are dropped with a warning log. The
/// TTL-based expiry on Redis keys ensures eventual consistency even if retries
/// are lost.
const PENDING_RETRY_QUEUE_CAPACITY: usize = 10_000;

const CONNECTION_LIFECYCLE_LOCK_STRIPES: usize = 256;

impl ConnectionManager {
    fn conn_metadata_key(&self, connection_id: &str) -> String {
        format!("{}conn_mgr:conn:{connection_id}", self.redis_key_prefix)
    }

    fn user_index_key(&self, user_id: impl std::fmt::Display) -> String {
        format!("{}conn_mgr:user:{user_id}", self.redis_key_prefix)
    }

    fn room_index_key(&self, room_id: impl std::fmt::Display) -> String {
        format!("{}conn_mgr:room:{room_id}", self.redis_key_prefix)
    }

    fn total_counter_key(&self) -> String {
        format!("{}connections:total", self.redis_key_prefix)
    }

    fn user_counter_key(&self, user_id: impl std::fmt::Display) -> String {
        format!("{}connections:user:{user_id}", self.redis_key_prefix)
    }

    fn room_counter_key(&self, room_id: impl std::fmt::Display) -> String {
        format!("{}connections:room:{room_id}", self.redis_key_prefix)
    }

    fn user_index_directory_key(&self) -> String {
        format!(
            "{}{}",
            self.redis_key_prefix, USER_INDEX_DIRECTORY_KEY_SUFFIX
        )
    }

    fn room_index_directory_key(&self) -> String {
        format!(
            "{}{}",
            self.redis_key_prefix, ROOM_INDEX_DIRECTORY_KEY_SUFFIX
        )
    }

    async fn redis_op<T, F>(&self, operation: &'static str, future: F) -> Result<T, String>
    where
        F: std::future::Future<Output = redis::RedisResult<T>>,
    {
        let timeout = self.redis_conn.as_ref().map_or(
            synctv_core::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            |runtime| runtime.operation_timeout(),
        );
        match tokio::time::timeout(timeout, future).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(format!("Redis {operation} failed: {error}")),
            Err(_) => Err(format!(
                "Redis {operation} timed out after {}ms",
                timeout.as_millis()
            )),
        }
    }

    /// Create a new `ConnectionManager`.
    ///
    #[must_use]
    pub fn new(limits: ConnectionLimits) -> Self {
        // Use a large buffer (10 000) to minimise lag for critical events such as
        // ban/kick signals. A lagging receiver that falls behind by more than the
        // channel capacity would miss signals; the WebSocket handler has a periodic
        // re-validation backstop to handle the rare case where a signal is lost.
        let (disconnect_tx, _) = broadcast::channel(10_000);
        let (pending_retries_tx, pending_retries_rx) = mpsc::channel(PENDING_RETRY_QUEUE_CAPACITY);

        // Store the receiver so it is not dropped here. with_redis() will take it
        // and hand it to spawn_pending_retries_task when Redis is configured.
        Self {
            connections: Arc::new(DashMap::new()),
            claimed_connection_ids: Arc::new(std::sync::Mutex::new(HashSet::new())),
            user_connections: Arc::new(DashMap::new()),
            room_connections: Arc::new(DashMap::new()),
            presence_service: Arc::new(OnlinePresenceService::local()),
            node_id: Arc::from("local"),
            limits: Arc::new(limits),
            total_connections: Arc::new(AtomicUsize::new(0)),
            timeout_index: Arc::new(parking_lot::Mutex::new(TimeoutIndex::default())),
            total_connections_ever: Arc::new(AtomicU64::new(0)),
            total_messages: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            users_online_metric_increments: Arc::new(AtomicUsize::new(0)),
            #[cfg(test)]
            users_online_metric_decrements: Arc::new(AtomicUsize::new(0)),
            disconnect_tx: Arc::new(disconnect_tx),
            pending_room_reservations: Arc::new(DashMap::new()),
            pending_user_reservations: Arc::new(DashMap::new()),
            redis_conn: None,
            redis_key_prefix: String::new(),
            ttl_refresh_cancel: Arc::new(tokio_util::sync::CancellationToken::new()),
            ttl_refresh_handle: Arc::new(parking_lot::Mutex::new(None)),
            pending_retries_handle: Arc::new(parking_lot::Mutex::new(None)),
            pending_retries_tx,
            pending_retries_rx: Arc::new(tokio::sync::Mutex::new(Some(pending_retries_rx))),
            #[cfg(test)]
            join_room_before_commit_hook: None,
            #[cfg(test)]
            join_room_before_capacity_check_hook: None,
            #[cfg(test)]
            register_after_lifecycle_lock_hook: None,
            connection_lifecycle_locks: Arc::new(
                (0..CONNECTION_LIFECYCLE_LOCK_STRIPES)
                    .map(|_| Arc::new(tokio::sync::Mutex::new(())))
                    .collect(),
            ),
        }
    }

    /// Build a connection manager from an optional shared runtime.
    ///
    #[must_use]
    pub(crate) fn from_redis_runtime(
        limits: ConnectionLimits,
        redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
        key_prefix: &str,
    ) -> Self {
        if let Some(redis_runtime) = redis_runtime {
            Self::new_with_redis_runtime(limits, redis_runtime, key_prefix)
        } else {
            Self::new(limits)
        }
    }

    #[must_use]
    pub fn with_presence_service(mut self, presence_service: Arc<OnlinePresenceService>) -> Self {
        self.presence_service = presence_service;
        self
    }

    #[must_use]
    pub fn presence_service(&self) -> Arc<OnlinePresenceService> {
        Arc::clone(&self.presence_service)
    }

    #[must_use]
    pub fn with_node_id(mut self, node_id: impl Into<Arc<str>>) -> Self {
        self.node_id = node_id.into();
        self
    }

    #[cfg(test)]
    fn with_join_room_before_commit_hook(mut self, hook: AsyncTestHook) -> Self {
        self.join_room_before_commit_hook = Some(hook);
        self
    }

    #[cfg(test)]
    fn with_join_room_before_capacity_check_hook(mut self, hook: AsyncTestHook) -> Self {
        self.join_room_before_capacity_check_hook = Some(hook);
        self
    }

    #[cfg(test)]
    fn with_register_after_lifecycle_lock_hook(mut self, hook: AsyncTestHook) -> Self {
        self.register_after_lifecycle_lock_hook = Some(hook);
        self
    }

    fn connection_lifecycle_lock(&self, connection_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        connection_id.hash(&mut hasher);
        let shard_count = self.connection_lifecycle_locks.len();
        debug_assert!(
            shard_count > 0,
            "connection lifecycle lock stripes must exist"
        );
        let index = u64_to_usize_saturating(hasher.finish() % shard_count as u64);
        Arc::clone(&self.connection_lifecycle_locks[index])
    }

    fn schedule_idle_timeout(&self, connection_id: &str, last_activity: Instant) {
        let deadline = last_activity + self.limits.idle_timeout;
        self.timeout_index
            .lock()
            .schedule_idle(connection_id, deadline);
    }

    fn schedule_max_duration_timeout(&self, connection_id: &str, connected_at: Instant) {
        let deadline = connected_at + self.limits.max_duration;
        self.timeout_index
            .lock()
            .schedule_max_duration(connection_id, deadline);
    }

    fn schedule_rtc_timeout(&self, connection_id: &str, rtc_joined_at: Instant) {
        let deadline = rtc_joined_at + self.limits.webrtc_session_timeout;
        self.timeout_index
            .lock()
            .schedule_rtc(connection_id, deadline);
    }

    fn clear_rtc_timeout(&self, connection_id: &str) {
        self.timeout_index.lock().clear_rtc(connection_id);
    }

    fn remove_timeout_tracking(&self, connection_id: &str) {
        self.timeout_index.lock().remove_connection(connection_id);
    }

    #[cfg(test)]
    pub(crate) fn background_tasks_running(&self) -> bool {
        self.ttl_refresh_handle.lock().is_some() || self.pending_retries_handle.lock().is_some()
    }

    const fn redis_enabled(&self) -> bool {
        self.redis_conn.is_some()
    }

    async fn redis_conn_snapshot(&self) -> Option<redis::aio::ConnectionManager> {
        let runtime = self.redis_conn.as_ref()?;
        match tokio::time::timeout(runtime.operation_timeout(), runtime.snapshot()).await {
            Ok(Ok(conn)) => Some(conn),
            Ok(Err(error)) => {
                warn!(error = %error, "Redis connection snapshot failed");
                None
            }
            Err(_) => {
                warn!(
                    timeout_ms = runtime.operation_timeout().as_millis(),
                    "Redis connection snapshot timed out"
                );
                None
            }
        }
    }

    async fn redis_conn_snapshot_required(
        &self,
        unavailable_message: &str,
    ) -> Result<Option<redis::aio::ConnectionManager>, String> {
        let Some(runtime) = self.redis_conn.as_ref() else {
            return Ok(None);
        };

        match tokio::time::timeout(runtime.operation_timeout(), runtime.snapshot()).await {
            Ok(Ok(conn)) => Ok(Some(conn)),
            Ok(Err(error)) => {
                warn!(error = %error, "Redis connection snapshot failed");
                Err(unavailable_message.to_string())
            }
            Err(_) => {
                warn!(
                    timeout_ms = runtime.operation_timeout().as_millis(),
                    "Redis connection snapshot timed out"
                );
                Err(unavailable_message.to_string())
            }
        }
    }

    #[must_use]
    pub(crate) fn new_with_redis_runtime(
        limits: ConnectionLimits,
        conn: Arc<dyn RedisConnectionRuntime>,
        key_prefix: &str,
    ) -> Self {
        Self::new(limits).configure_redis_runtime(conn, key_prefix)
    }

    fn configure_redis_runtime(
        mut self,
        conn: Arc<dyn RedisConnectionRuntime>,
        key_prefix: &str,
    ) -> Self {
        self.redis_conn = Some(Arc::clone(&conn));
        self.redis_key_prefix = key_prefix.to_string();

        let cancel = tokio_util::sync::CancellationToken::new();
        self.ttl_refresh_cancel = Arc::new(cancel.clone());
        let handle = self.spawn_ttl_refresh_task(Duration::from_mins(1), cancel.clone());
        *self.ttl_refresh_handle.lock() = Some(handle);

        let rx = self
            .pending_retries_rx
            .try_lock()
            .ok()
            .and_then(|mut guard| guard.take());
        let rx = if let Some(rx) = rx {
            rx
        } else {
            let (tx, rx) = mpsc::channel(PENDING_RETRY_QUEUE_CAPACITY);
            self.pending_retries_tx = tx;
            rx
        };
        let handle = spawn_pending_retries_task(conn, rx, cancel);
        *self.pending_retries_handle.lock() = Some(handle);

        self
    }

    /// Enable distributed connection counting via Redis.
    ///
    /// When Redis is configured, per-user and per-room connection limits are
    /// enforced across all replicas. Without Redis, limits are per-node only.
    ///
    /// Automatically spawns background tasks:
    /// - TTL refresh task for long-lived connection counters
    /// - Pending-retries task for failed Redis counter operations
    ///
    /// All tasks are cancelled when `shutdown()` is called.
    #[must_use]
    #[cfg(test)]
    fn with_redis(self, conn: redis::aio::ConnectionManager, key_prefix: &str) -> Self {
        let runtime: Arc<dyn RedisConnectionRuntime> =
            Arc::new(DirectRedisConnectionRuntime::new(conn));
        self.configure_redis_runtime(runtime, key_prefix)
    }

    /// Enable distributed connection counting via a shared Redis handle.
    ///
    /// This variant follows Sentinel failover hot-swaps because each operation
    /// resolves a fresh connection snapshot from the shared `RwLock`.
    #[must_use]
    #[cfg(test)]
    fn with_shared_redis(
        self,
        conn: std::sync::Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
        key_prefix: &str,
    ) -> Self {
        let runtime: Arc<dyn RedisConnectionRuntime> =
            Arc::new(SharedRedisConnectionRuntime::new(conn));
        self.configure_redis_runtime(runtime, key_prefix)
    }

    /// Enqueue a failed Redis counter operation for background retry.
    fn enqueue_retry(&self, op: PendingRedisOp) {
        if let Err(e) = self.pending_retries_tx.try_send(op) {
            warn!("Failed to enqueue pending Redis retry (channel full or closed): {e}");
        }
    }

    fn try_claim_connection_id(
        &self,
        connection_id: &str,
    ) -> Result<ConnectionIdClaim<'_>, String> {
        let mut claimed = self
            .claimed_connection_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if claimed.contains(connection_id) || self.connections.contains_key(connection_id) {
            return Err(format!(
                "Connection '{connection_id}' is already registered"
            ));
        }

        claimed.insert(connection_id.to_string());

        Ok(ConnectionIdClaim {
            manager: self,
            connection_id: connection_id.to_string(),
            committed: false,
        })
    }

    fn release_connection_id_claim(&self, connection_id: &str) {
        self.claimed_connection_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(connection_id);
    }

    async fn rollback_distributed_counter(&self, key: String) {
        if let Err(error) = self.redis_decr(&key).await {
            warn!(
                key = %key,
                error = %error,
                "Failed to roll back distributed Redis counter; enqueueing retry"
            );
            self.enqueue_retry(PendingRedisOp::Decr(key));
        }
    }

    fn unregister_cleanup_op(
        &self,
        connection_id: &str,
        registration_token: &str,
        user_id: UserId,
        room_id: Option<RoomId>,
    ) -> PendingRedisOp {
        let no_room_key = format!(
            "{}conn_mgr:cleanup_no_room:{connection_id}",
            self.redis_key_prefix
        );
        PendingRedisOp::UnregisterCleanup {
            cleanup_key: format!(
                "{}conn_mgr:cleanup:{connection_id}:{registration_token}",
                self.redis_key_prefix
            ),
            total_key: self.total_counter_key(),
            user_key: self.user_counter_key(user_id),
            room_key: room_id.map_or_else(
                || no_room_key.clone(),
                |room_id| self.room_counter_key(room_id),
            ),
            conn_key: self.conn_metadata_key(connection_id),
            user_index_key: self.user_index_key(user_id),
            room_index_key: room_id
                .map(|room_id| self.room_index_key(room_id))
                .unwrap_or(no_room_key),
            connection_id: connection_id.to_string(),
            registration_token: registration_token.to_string(),
            has_room: room_id.is_some(),
        }
    }

    async fn redis_unregister_cleanup(&self, op: &PendingRedisOp) -> Result<(), String> {
        let PendingRedisOp::UnregisterCleanup {
            cleanup_key,
            total_key,
            user_key,
            room_key,
            conn_key,
            user_index_key,
            room_index_key,
            connection_id,
            registration_token,
            has_room,
        } = op
        else {
            return Err("invalid unregister cleanup operation".to_string());
        };

        let Some(mut conn) = self.redis_conn_snapshot().await else {
            return Err("Redis not configured".to_string());
        };

        self.redis_op(
            "unregister cleanup",
            run_unregister_cleanup_script(
                &mut conn,
                UnregisterCleanupScriptArgs {
                    cleanup_key,
                    total_key,
                    user_key,
                    room_key,
                    conn_key,
                    user_index_key,
                    room_index_key,
                    connection_id,
                    registration_token,
                    has_room: *has_room,
                },
            ),
        )
        .await
        .map(|_| ())
    }

    async fn persist_registration_metadata_best_effort(
        &self,
        connection_id: &str,
        user_id: &UserId,
    ) {
        let Some(conn_info) = self.get_connection(connection_id) else {
            return;
        };

        let Some(mut conn) = self.redis_conn_snapshot().await else {
            return;
        };

        let conn_key = self.conn_metadata_key(connection_id);
        let user_index_key = self.user_index_key(user_id);
        let user_index_directory_key = self.user_index_directory_key();

        let persistent = ConnectionInfoPersistent::from(&conn_info);
        match serde_json::to_string(&persistent) {
            Ok(json) => {
                let result: Result<(), _> = self
                    .redis_op(
                        "persist connection metadata",
                        redis::cmd("SET")
                            .arg(&conn_key)
                            .arg(&json)
                            .arg("EX")
                            .arg(CONNECTION_METADATA_TTL_SECONDS)
                            .query_async(&mut conn),
                    )
                    .await;
                if let Err(e) = result {
                    warn!("Failed to persist connection metadata to Redis: {e}");
                }
            }
            Err(e) => {
                warn!("Failed to serialize connection metadata for Redis: {e}");
            }
        }

        if let Err(e) = self
            .redis_op(
                "add connection to user index",
                conn.sadd::<_, _, ()>(&user_index_key, connection_id),
            )
            .await
        {
            warn!("Failed to add connection to user index: {e}");
        }
        let _: Result<(), _> = self
            .redis_op(
                "add user index to directory",
                conn.sadd(&user_index_directory_key, &user_index_key),
            )
            .await;
        let _: Result<(), _> = self
            .redis_op(
                "refresh user index directory TTL",
                conn.expire(&user_index_directory_key, CONNECTION_METADATA_TTL_SECONDS),
            )
            .await;
        let _: Result<(), _> = self
            .redis_op(
                "refresh user index TTL",
                conn.expire(&user_index_key, CONNECTION_METADATA_TTL_SECONDS),
            )
            .await;
    }

    async fn persist_room_membership_metadata_best_effort(
        &self,
        connection_id: &str,
        transition: &RoomTransition,
    ) {
        let Some(conn_info) = self.get_connection(connection_id) else {
            return;
        };

        if conn_info.room_id.as_ref() != Some(&transition.room_id) {
            return;
        }

        let Some(mut conn) = self.redis_conn_snapshot().await else {
            return;
        };

        let conn_key = self.conn_metadata_key(connection_id);
        let room_index_key = self.room_index_key(transition.room_id);
        let room_index_directory_key = self.room_index_directory_key();
        let previous_room_index_key = transition
            .previous_room_id
            .as_ref()
            .map(|room_id| self.room_index_key(room_id));

        let persistent = ConnectionInfoPersistent::from(&conn_info);
        match serde_json::to_string(&persistent) {
            Ok(json) => {
                let result: Result<(), _> = self
                    .redis_op(
                        "update connection metadata",
                        redis::cmd("SET")
                            .arg(&conn_key)
                            .arg(&json)
                            .arg("EX")
                            .arg(CONNECTION_METADATA_TTL_SECONDS)
                            .query_async(&mut conn),
                    )
                    .await;
                if let Err(e) = result {
                    warn!("Failed to update connection metadata in Redis: {e}");
                }
            }
            Err(e) => {
                warn!("Failed to serialize updated connection metadata for Redis: {e}");
            }
        }

        if let Err(e) = self
            .redis_op(
                "add connection to room index",
                conn.sadd::<_, _, ()>(&room_index_key, connection_id),
            )
            .await
        {
            warn!("Failed to add connection to room index: {e}");
        }
        let _: Result<(), _> = self
            .redis_op(
                "add room index to directory",
                conn.sadd(&room_index_directory_key, &room_index_key),
            )
            .await;
        let _: Result<(), _> = self
            .redis_op(
                "refresh room index directory TTL",
                conn.expire(&room_index_directory_key, CONNECTION_METADATA_TTL_SECONDS),
            )
            .await;
        if let Some(previous_room_index_key) = previous_room_index_key.as_ref() {
            if let Err(e) = self
                .redis_op(
                    "remove connection from previous room index",
                    conn.srem::<_, _, ()>(previous_room_index_key, connection_id),
                )
                .await
            {
                warn!("Failed to remove connection from previous room index: {e}");
            }
        }
        let _: Result<(), _> = self
            .redis_op(
                "refresh room index TTL",
                conn.expire(&room_index_key, CONNECTION_METADATA_TTL_SECONDS),
            )
            .await;
        if let Some(previous_room_index_key) = previous_room_index_key.as_ref() {
            let _: Result<(), _> = self
                .redis_op(
                    "refresh previous room index TTL",
                    conn.expire(previous_room_index_key, CONNECTION_METADATA_TTL_SECONDS),
                )
                .await;
        }
    }

    /// Check if a user can accept a new connection (without registering)
    ///
    /// This is used to enforce per-user connection limits BEFORE WebSocket upgrade,
    /// preventing users from exceeding their connection limit.
    ///
    /// Returns Ok(()) if the user can accept a connection, or Err with reason if at limit.
    pub fn can_accept_user_connection(&self, user_id: &UserId) -> Result<(), String> {
        // Check local user connection limit
        let user_entry = self.user_connections.get(user_id);
        let current_count = user_entry.as_ref().map_or(0, |v| v.len());

        if current_count >= self.limits.max_per_user {
            return Err(format!(
                "User at capacity ({} connections, max: {})",
                current_count, self.limits.max_per_user
            ));
        }

        Ok(())
    }

    /// Check if a room can accept a new connection (without registering)
    ///
    /// This is used to enforce connection limits BEFORE WebSocket upgrade,
    /// preventing unauthorized connections from being upgraded.
    ///
    /// Returns Ok(()) if the room can accept a connection, or Err with reason if at capacity.
    pub fn can_accept_room_connection(&self, room_id: &RoomId) -> Result<(), String> {
        // Check local room connection limit
        let room_entry = self.room_connections.get(room_id);
        let current_count = room_entry.as_ref().map_or(0, |v| v.len());

        if current_count >= self.limits.max_per_room {
            return Err(format!(
                "Room at capacity ({} connections, max: {})",
                current_count, self.limits.max_per_room
            ));
        }

        Ok(())
    }

    /// Atomically reserve a room connection slot BEFORE WebSocket upgrade.
    ///
    /// This prevents the TOCTOU race where `can_accept_room_connection` succeeds
    /// for N concurrent requests that all pass the check before any registers.
    /// The reservation counter is checked alongside the actual connection count.
    ///
    /// The caller MUST call `release_room_reservation` after `join_room` completes
    /// (success or failure) or if the WebSocket upgrade fails.
    ///
    /// Returns Ok(()) if the slot was reserved, or Err if the room is at capacity.
    pub fn reserve_room_slot(&self, room_id: &RoomId) -> Result<(), String> {
        let counter = self
            .pending_room_reservations
            .entry(*room_id)
            .or_insert_with(|| AtomicUsize::new(0));

        // Atomically try to reserve a slot by checking combined count
        loop {
            let pending = counter.load(Ordering::Acquire);
            let registered = self.room_connections.get(room_id).map_or(0, |v| v.len());
            let effective = registered + pending;

            if effective >= self.limits.max_per_room {
                return Err(format!(
                    "Room at capacity ({effective} connections, max: {})",
                    self.limits.max_per_room
                ));
            }

            // Try to atomically increment the pending count
            if counter
                .compare_exchange_weak(pending, pending + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(());
            }
            // CAS failed, retry with fresh values
        }
    }

    /// Release a room connection slot reservation.
    ///
    /// Must be called after `join_room` completes (success or failure) or
    /// if the WebSocket upgrade fails.
    pub fn release_room_reservation(&self, room_id: &RoomId) {
        let mut should_remove_entry = false;
        if let Some(counter) = self.pending_room_reservations.get(room_id) {
            let result = counter.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                if current > 0 {
                    Some(current - 1)
                } else {
                    None // refuse to decrement below zero
                }
            });
            match result {
                Ok(previous) => {
                    should_remove_entry = previous == 1;
                }
                Err(_) => {
                    warn!(
                        room_id = %room_id,
                        "release_room_reservation called but counter is already 0 (double-release?)"
                    );
                }
            }
        }

        if should_remove_entry {
            self.pending_room_reservations
                .remove_if(room_id, |_, counter| counter.load(Ordering::Acquire) == 0);
        }
    }

    /// Atomically reserve a user connection slot BEFORE WebSocket upgrade.
    ///
    /// Same semantics as `reserve_room_slot` but for per-user limits.
    /// The caller MUST call `release_user_reservation` after registration
    /// completes or on failure.
    pub fn reserve_user_slot(&self, user_id: &UserId) -> Result<(), String> {
        let counter = self
            .pending_user_reservations
            .entry(*user_id)
            .or_insert_with(|| AtomicUsize::new(0));

        loop {
            let pending = counter.load(Ordering::Acquire);
            let registered = self.user_connections.get(user_id).map_or(0, |v| v.len());
            let effective = registered + pending;

            if effective >= self.limits.max_per_user {
                return Err(format!(
                    "User at capacity ({effective} connections, max: {})",
                    self.limits.max_per_user
                ));
            }

            if counter
                .compare_exchange_weak(pending, pending + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(());
            }
        }
    }

    /// Release a user connection slot reservation.
    pub fn release_user_reservation(&self, user_id: &UserId) {
        let mut should_remove_entry = false;
        if let Some(counter) = self.pending_user_reservations.get(user_id) {
            let result = counter.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                if current > 0 {
                    Some(current - 1)
                } else {
                    None // refuse to decrement below zero
                }
            });
            match result {
                Ok(previous) => {
                    should_remove_entry = previous == 1;
                }
                Err(_) => {
                    warn!(
                        user_id = %user_id,
                        "release_user_reservation called but counter is already 0 (double-release?)"
                    );
                }
            }
        }

        if should_remove_entry {
            self.pending_user_reservations
                .remove_if(user_id, |_, counter| counter.load(Ordering::Acquire) == 0);
        }
    }

    /// Register a new connection
    ///
    /// Returns Ok(()) if connection is allowed, or Err with reason if rejected.
    ///
    /// When Redis is configured, enforces per-user limits across all replicas.
    ///
    /// If Redis becomes unavailable while distributed enforcement is enabled,
    /// registration fails closed instead of silently degrading to per-node-only
    /// limits. In distributed mode, allowing local-only admission would let replicas
    /// oversubscribe the same user concurrently.
    pub async fn register(&self, connection_id: String, user_id: UserId) -> Result<(), String> {
        self.register_actor(connection_id, user_id, user_id.to_string())
            .await
    }

    pub async fn register_actor(
        &self,
        connection_id: String,
        user_id: UserId,
        actor_id: String,
    ) -> Result<(), String> {
        let claim = self.try_claim_connection_id(&connection_id)?;
        let lifecycle_lock = self.connection_lifecycle_lock(&connection_id);
        let lifecycle_guard = lifecycle_lock.lock().await;
        #[cfg(test)]
        if let Some(hook) = &self.register_after_lifecycle_lock_hook {
            hook().await;
        }

        // Atomically reserve a slot in the total connection count.
        // fetch_add returns the previous value; if it was already at the limit,
        // roll back and reject.
        let prev = self.total_connections.fetch_add(1, Ordering::AcqRel);
        if prev >= self.limits.max_total {
            self.total_connections.fetch_sub(1, Ordering::AcqRel);
            return Err(if self.redis_enabled() {
                format!(
                    "Server at capacity across all replicas ({} connections)",
                    self.limits.max_total
                )
            } else {
                format!("Server at capacity ({} connections)", self.limits.max_total)
            });
        }

        let total_key = self.total_counter_key();

        // Enforce the total connection limit across replicas when Redis is
        // configured. This must fail closed: in distributed mode, a best-effort
        // counter would let N replicas each admit up to max_total locally.
        if self.redis_enabled() {
            match self
                .redis_incr_and_check(&total_key, self.limits.max_total)
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    self.total_connections.fetch_sub(1, Ordering::AcqRel);
                    self.rollback_distributed_counter(total_key.clone()).await;
                    return Err(format!(
                        "Server at capacity across all replicas ({} connections)",
                        self.limits.max_total
                    ));
                }
                Err(e) => {
                    self.total_connections.fetch_sub(1, Ordering::AcqRel);
                    warn!("Distributed total connection check failed; rejecting connection: {e}");
                    return Err(
                        "Distributed total connection check unavailable; refusing new connection while cluster Redis is degraded"
                            .to_string(),
                    );
                }
            }
        }

        // Enforce per-user connection limit.
        // When Redis is configured, use the atomic INCR return value as the
        // single source of truth for the cross-replica count. If the new count
        // exceeds the limit we immediately DECR and reject, avoiding any TOCTOU
        // window where two replicas could both pass the check concurrently.
        // When Redis is not configured, fall back to the local DashMap count.
        // App wiring only enables Redis-backed ConnectionManager in distributed mode,
        // so a Redis error here means distributed state is unavailable and we
        // must fail closed instead of weakening enforcement.
        if self.redis_enabled() {
            let redis_key = self.user_counter_key(user_id);
            match self
                .redis_incr_and_check(&redis_key, self.limits.max_per_user)
                .await
            {
                Ok(true) => {
                    // Distributed limit not exceeded; proceed.
                }
                Ok(false) => {
                    // Distributed limit exceeded -- roll back total counter and
                    // the Redis per-user counter that was just incremented.
                    self.total_connections.fetch_sub(1, Ordering::AcqRel);
                    self.rollback_distributed_counter(total_key.clone()).await;
                    self.rollback_distributed_counter(redis_key.clone()).await;
                    return Err(format!(
                        "Too many connections for this user across all replicas (max {})",
                        self.limits.max_per_user
                    ));
                }
                Err(e) => {
                    self.total_connections.fetch_sub(1, Ordering::AcqRel);
                    self.rollback_distributed_counter(total_key.clone()).await;
                    warn!("Distributed user connection check failed; rejecting connection: {e}");
                    return Err(
                        "Distributed user connection check unavailable; refusing new connection while cluster Redis is degraded"
                            .to_string(),
                    );
                }
            }
        }

        // Add the connection to the local user index (used for routing and
        // cleanup) under the same shard lock that enforces the local per-user
        // limit. Without Redis, this closes the TOCTOU race where concurrent
        // registrations could both observe the old count before either inserts.
        let is_first_connection_for_user = {
            let mut user_entry = self.user_connections.entry(user_id).or_default();
            if !self.redis_enabled() && user_entry.len() >= self.limits.max_per_user {
                self.total_connections.fetch_sub(1, Ordering::AcqRel);
                return Err(format!(
                    "Too many connections for this user (max {})",
                    self.limits.max_per_user
                ));
            }

            let is_first = user_entry.is_empty();
            user_entry.push(connection_id.clone());
            is_first
        };

        // Create and register connection info
        let conn_info = ConnectionInfo::new_with_actor_id(connection_id.clone(), user_id, actor_id);
        self.connections
            .insert(connection_id.clone(), conn_info.clone());
        self.schedule_idle_timeout(&connection_id, conn_info.last_activity);
        self.schedule_max_duration_timeout(&connection_id, conn_info.connected_at);
        drop(lifecycle_guard);

        // Persist connection metadata to Redis (best-effort)
        self.persist_registration_metadata_best_effort(&connection_id, &user_id)
            .await;
        if let Err(error) = self
            .presence_service
            .register_connection(
                connection_id.clone(),
                self.node_id.to_string(),
                user_id,
                conn_info.actor_id.clone(),
            )
            .await
        {
            warn!(
                connection_id = %connection_id,
                user_id = %user_id,
                error = %error,
                "Failed to register core presence"
            );
        }

        // Update metrics
        self.total_connections_ever.fetch_add(1, Ordering::Relaxed);
        synctv_core::metrics::ACTIVE_CONNECTIONS.inc();
        if is_first_connection_for_user {
            synctv_core::metrics::http::USERS_ONLINE.inc();
            #[cfg(test)]
            self.users_online_metric_increments
                .fetch_add(1, Ordering::Relaxed);
        }
        synctv_core::metrics::cluster::CLUSTER_CONNECTIONS.set(usize_to_i64_saturating(
            self.total_connections.load(Ordering::Relaxed),
        ));

        info!(
            connection_id = %connection_id,
            user_id = %user_id,
            total_connections = self.total_connections.load(Ordering::Relaxed),
            "Connection registered"
        );

        claim.commit();

        Ok(())
    }

    /// Associate a connection with a room
    ///
    /// Enforces per-room connection limits to prevent resource exhaustion.
    /// When Redis is configured, limits are enforced across all replicas.
    ///
    /// If the connection already belongs to a different room, the move is only
    /// committed after the target room passes all capacity checks. This avoids
    /// dropping the existing room membership when the new room rejects the move.
    pub async fn join_room(&self, connection_id: &str, room_id: RoomId) -> Result<(), String> {
        #[cfg(test)]
        if let Some(hook) = &self.join_room_before_commit_hook {
            hook().await;
        }

        let old_room_id: Option<RoomId> =
            self.connections.get(connection_id).and_then(|c| c.room_id);

        if let Some(ref old_room) = old_room_id {
            if old_room == &room_id {
                return Ok(());
            }
        }

        // Check distributed per-room capacity first when Redis is enabled,
        // then commit the local room index update. In local mode, the room
        // limit is enforced inside the commit step under the room shard lock
        // so concurrent joins cannot oversubscribe the room.

        // Step 1: Check distributed per-room limit via Redis (when enabled).
        // In local mode we enforce the room limit inside the commit step below
        // under the room shard lock, which closes the TOCTOU race for
        // concurrent same-room joins.
        #[cfg(test)]
        if let Some(hook) = &self.join_room_before_capacity_check_hook {
            hook().await;
        }
        let redis_room_incremented = if self.redis_enabled() {
            let redis_key = self.room_counter_key(room_id);
            match self
                .redis_incr_and_check(&redis_key, self.limits.max_per_room)
                .await
            {
                Ok(true) => true,
                Ok(false) => {
                    self.rollback_distributed_counter(redis_key.clone()).await;
                    return Err(format!(
                        "Room at capacity across all replicas ({} connections)",
                        self.limits.max_per_room
                    ));
                }
                Err(e) => {
                    warn!("Distributed room connection check failed; rejecting room join: {e}");
                    return Err(
                        "Distributed room capacity check unavailable; refusing room join while cluster Redis is degraded"
                            .to_string(),
                    );
                }
            }
        } else {
            false
        };

        let lifecycle_lock = self.connection_lifecycle_lock(connection_id);
        let lifecycle_guard = lifecycle_lock.lock().await;
        let (transition, last_activity) =
            if let Some(mut conn) = self.connections.get_mut(connection_id) {
                let current_room_id = conn.room_id;
                if current_room_id.as_ref() == Some(&room_id) {
                    drop(lifecycle_guard);
                    if redis_room_incremented {
                        let redis_key = self.room_counter_key(room_id);
                        self.rollback_distributed_counter(redis_key).await;
                    }
                    return Ok(());
                }

                // Step 3: Commit the room move locally after all checks have passed.
                // Without Redis, enforce the room limit under the same shard lock as
                // the insert so concurrent local joins cannot oversubscribe the room.
                {
                    let mut room_entry = self.room_connections.entry(room_id).or_default();
                    if !self.redis_enabled() && room_entry.len() >= self.limits.max_per_room {
                        drop(lifecycle_guard);
                        if redis_room_incremented {
                            let redis_key = self.room_counter_key(room_id);
                            self.rollback_distributed_counter(redis_key).await;
                        }
                        return Err(format!(
                            "Room at capacity ({} connections)",
                            self.limits.max_per_room
                        ));
                    }
                    room_entry.push(connection_id.to_string());
                }

                if let Some(ref old_room) = current_room_id {
                    if let Some(mut old_room_conns) = self.room_connections.get_mut(old_room) {
                        old_room_conns.retain(|id| id != connection_id);
                        if old_room_conns.is_empty() {
                            drop(old_room_conns);
                            self.room_connections.remove(old_room);
                        }
                    }
                }

                conn.room_id = Some(room_id);
                conn.last_activity = Instant::now();
                (
                    Some(RoomTransition {
                        previous_room_id: current_room_id,
                        room_id,
                    }),
                    Some(conn.last_activity),
                )
            } else {
                drop(lifecycle_guard);
                if redis_room_incremented {
                    let redis_key = self.room_counter_key(room_id);
                    self.rollback_distributed_counter(redis_key).await;
                }
                return Err("Connection not found".to_string());
            };
        drop(lifecycle_guard);
        if let Some(last_activity) = last_activity {
            self.schedule_idle_timeout(connection_id, last_activity);
        }

        // Step 4: Decrement the old room's distributed counter only after the
        // move succeeds. If it fails, enqueue a retry so Redis eventually matches
        // the in-memory truth.
        if let Some(old_room) = transition
            .as_ref()
            .and_then(|transition| transition.previous_room_id.as_ref())
        {
            let old_key = self.room_counter_key(old_room);
            self.rollback_distributed_counter(old_key).await;
        }

        // Update Redis metadata with new room_id (best-effort)
        if let Some(transition) = transition.as_ref() {
            self.persist_room_membership_metadata_best_effort(connection_id, transition)
                .await;
        }
        if let Err(error) = self
            .presence_service
            .join_room(connection_id, room_id)
            .await
        {
            warn!(
                connection_id = %connection_id,
                room_id = %room_id,
                error = %error,
                "Failed to update core presence room membership"
            );
        }

        synctv_core::metrics::cluster::NODE_ACTIVE_ROOMS
            .set(usize_to_i64_saturating(self.room_connections.len()));

        debug!(
            connection_id = %connection_id,
            room_id = %room_id,
            "Connection joined room"
        );

        Ok(())
    }

    /// Record message activity for a connection
    pub fn record_message(&self, connection_id: &str) {
        if let Some(mut conn) = self.connections.get_mut(connection_id) {
            conn.last_activity = Instant::now();
            conn.message_count += 1;
            self.schedule_idle_timeout(connection_id, conn.last_activity);
        }
        if self.presence_service.mark_seen_for_renewal(connection_id) {
            let presence_service = self.presence_service.clone();
            synctv_core::spawn::spawn_monitored("presence_renewal_flush", async move {
                if let Err(error) = presence_service.flush_pending_renewals().await {
                    warn!(error = %error, "Failed to flush core presence renewals");
                }
            });
        }
        self.total_messages.fetch_add(1, Ordering::Relaxed);
    }

    /// Unregister a connection
    ///
    /// Decrements both local and distributed (Redis) connection counters.
    pub async fn unregister(&self, connection_id: &str) {
        let lifecycle_lock = self.connection_lifecycle_lock(connection_id);
        let lifecycle_guard = lifecycle_lock.lock().await;
        let removed = if let Some((_, conn_info)) = self.connections.remove(connection_id) {
            self.remove_timeout_tracking(connection_id);
            // Decrement the atomic total connection count
            self.total_connections.fetch_sub(1, Ordering::AcqRel);
            let mut user_went_offline = false;

            // Remove from user connections
            if let Some(mut user_conns) = self.user_connections.get_mut(&conn_info.user_id) {
                user_conns.retain(|id| id != connection_id);
                if user_conns.is_empty() {
                    user_went_offline = true;
                    drop(user_conns);
                    self.user_connections.remove(&conn_info.user_id);
                }
            }

            // Remove from room connections
            if let Some(room_id) = &conn_info.room_id {
                if let Some(mut room_conns) = self.room_connections.get_mut(room_id) {
                    room_conns.retain(|id| id != connection_id);
                    if room_conns.is_empty() {
                        drop(room_conns);
                        self.room_connections.remove(room_id);
                    }
                }
            }

            // Decrement distributed Redis counters and remove metadata with one
            // idempotent script. The cleanup marker prevents retries from
            // double-applying non-idempotent counter decrements.
            if self.redis_enabled() {
                let cleanup_op = self.unregister_cleanup_op(
                    connection_id,
                    &conn_info.registration_token,
                    conn_info.user_id,
                    conn_info.room_id,
                );
                let cleanup_result = tokio::time::timeout(
                    Duration::from_secs(2),
                    self.redis_unregister_cleanup(&cleanup_op),
                )
                .await;

                match cleanup_result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        warn!(
                            connection_id = %connection_id,
                            error = %error,
                            "Redis cleanup failed during unregister, enqueueing idempotent retry"
                        );
                        self.enqueue_retry(cleanup_op);
                    }
                    Err(_) => {
                        warn!(
                            connection_id = %connection_id,
                            "Redis cleanup timed out during unregister, enqueueing idempotent retry"
                        );
                        self.enqueue_retry(cleanup_op);
                    }
                }
            }
            self.release_connection_id_claim(connection_id);
            Some((conn_info, user_went_offline))
        } else {
            None
        };
        drop(lifecycle_guard);

        if let Some((conn_info, user_went_offline)) = removed {
            synctv_core::metrics::ACTIVE_CONNECTIONS.dec();
            if user_went_offline {
                synctv_core::metrics::http::USERS_ONLINE.dec();
                #[cfg(test)]
                self.users_online_metric_decrements
                    .fetch_add(1, Ordering::Relaxed);
            }
            if let Err(error) = self
                .presence_service
                .unregister_connection(connection_id)
                .await
            {
                warn!(
                    connection_id = %connection_id,
                    error = %error,
                    "Failed to unregister core presence"
                );
            }
            synctv_core::metrics::cluster::CLUSTER_CONNECTIONS.set(usize_to_i64_saturating(
                self.total_connections.load(Ordering::Relaxed),
            ));
            synctv_core::metrics::cluster::NODE_ACTIVE_ROOMS
                .set(usize_to_i64_saturating(self.room_connections.len()));

            info!(
                connection_id = %connection_id,
                user_id = %conn_info.user_id,
                duration = ?conn_info.duration(),
                message_count = conn_info.message_count,
                "Connection unregistered"
            );
        }
    }

    /// Check for idle or expired connections
    ///
    /// Returns list of connection IDs that should be disconnected
    pub fn check_timeouts(&self) -> Vec<String> {
        let now = Instant::now();
        let (due_idle, due_max_duration, due_rtc) = {
            let mut timeout_index = self.timeout_index.lock();
            (
                timeout_index.take_due_idle(now),
                timeout_index.take_due_max_duration(now),
                timeout_index.take_due_rtc(now),
            )
        };

        let mut due_connections = HashSet::new();
        due_connections.extend(due_idle);
        due_connections.extend(due_max_duration);
        due_connections.extend(due_rtc);

        let mut to_disconnect = Vec::new();
        let mut rtc_timeouts: Vec<(RoomId, UserId, String)> = Vec::new();
        for connection_id in due_connections {
            let Some(conn) = self.connections.get(&connection_id) else {
                continue;
            };

            // Check idle timeout
            if conn.idle_duration() > self.limits.idle_timeout {
                warn!(
                    connection_id = %conn.connection_id,
                    idle_duration = ?conn.idle_duration(),
                    "Connection idle timeout"
                );
                to_disconnect.push(conn.connection_id.clone());
                continue;
            }

            // Check max duration
            if conn.duration() > self.limits.max_duration {
                warn!(
                    connection_id = %conn.connection_id,
                    duration = ?conn.duration(),
                    "Connection max duration reached"
                );
                to_disconnect.push(conn.connection_id.clone());
                continue;
            }

            // Check WebRTC session timeout
            // Only check if the connection is marked as RTC-joined
            if conn.rtc_joined {
                if let Some(rtc_duration) = conn.rtc_session_duration() {
                    if rtc_duration > self.limits.webrtc_session_timeout {
                        warn!(
                            connection_id = %conn.connection_id,
                            user_id = %conn.user_id,
                            room_id = ?conn.room_id,
                            rtc_session_duration = ?rtc_duration,
                            webrtc_session_timeout = ?self.limits.webrtc_session_timeout,
                            "WebRTC session timeout"
                        );
                        if let Some(room_id) = &conn.room_id {
                            rtc_timeouts.push((*room_id, conn.user_id, conn.connection_id.clone()));
                        }
                        // Add to disconnect list to force reconnection
                        to_disconnect.push(conn.connection_id.clone());
                    }
                }
            }
        }

        // Now apply RTC state mutations outside the DashMap iteration
        for (room_id, user_id, conn_id) in rtc_timeouts {
            self.mark_rtc_joined(&room_id, &user_id, &conn_id, false);
        }

        to_disconnect
    }

    /// Get connection count (local node only)
    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Get total connection count across all replicas (distributed).
    ///
    /// Reads the Redis atomic counter (`connections:total`) which is maintained
    /// by `register`/`unregister`. Local-only managers return the local count;
    /// Redis-backed managers return an error when Redis is unavailable.
    pub async fn connection_count_distributed(&self) -> Result<usize, String> {
        if let Some(mut conn) = self
            .redis_conn_snapshot_required(
                "Distributed total connection count unavailable while Redis is degraded",
            )
            .await?
        {
            let redis_key = self.total_counter_key();
            match self
                .redis_op(
                    "read distributed total connection count",
                    conn.get::<_, Option<i64>>(&redis_key),
                )
                .await
            {
                Ok(Some(count)) if count > 0 => return Ok(i64_to_usize_saturating(count)),
                Ok(_) => return Ok(0),
                Err(e) => {
                    warn!("{e}");
                    return Err(
                        "Distributed total connection count unavailable while Redis is degraded"
                            .to_string(),
                    );
                }
            }
        }
        Ok(self.connection_count())
    }

    /// Get connection count for a user
    #[must_use]
    pub fn user_connection_count(&self, user_id: &UserId) -> usize {
        self.user_connections
            .get(user_id)
            .map_or(0, |conns| conns.len())
    }

    /// Get connection count for a room (local node only)
    #[must_use]
    pub fn room_connection_count(&self, room_id: &RoomId) -> usize {
        self.room_connections
            .get(room_id)
            .map_or(0, |conns| conns.len())
    }

    /// List rooms that have at least one connection on this process.
    ///
    /// This is local-node lifecycle ownership. Playback background workers use
    /// this set for duration probing, auto-advance, RTMP, and live-proxy
    /// resource maintenance. Presence, hot-room indexes, and distributed room
    /// counters are read models for lists, admin views, analytics, and metrics.
    /// Cross-node duplicates converge through database row locks, `SKIP
    /// LOCKED`, and playback-state optimistic writes.
    #[must_use]
    pub fn active_room_ids(&self) -> Vec<RoomId> {
        self.room_connections
            .iter()
            .filter_map(|entry| (!entry.value().is_empty()).then_some(*entry.key()))
            .collect()
    }

    /// Get connection count for a room across all replicas (distributed).
    ///
    /// Reads the Redis atomic counter (`connections:room:{room_id}`) which is
    /// maintained by `register`/`unregister`/`join_room`. Local-only managers
    /// return the local count; Redis-backed managers return an error when Redis
    /// is unavailable.
    pub async fn room_connection_count_distributed(
        &self,
        room_id: &RoomId,
    ) -> Result<usize, String> {
        if let Some(mut conn) = self
            .redis_conn_snapshot_required(
                "Distributed room connection count unavailable while Redis is degraded",
            )
            .await?
        {
            let redis_key = self.room_counter_key(room_id);
            match self
                .redis_op(
                    "read distributed room connection count",
                    conn.get::<_, Option<i64>>(&redis_key),
                )
                .await
            {
                Ok(Some(count)) if count > 0 => return Ok(i64_to_usize_saturating(count)),
                Ok(_) => return Ok(0),
                Err(e) => {
                    warn!("{e}");
                    return Err(
                        "Distributed room connection count unavailable while Redis is degraded"
                            .to_string(),
                    );
                }
            }
        }
        Ok(self.room_connection_count(room_id))
    }

    /// Get connection counts for multiple rooms across all replicas (distributed).
    ///
    /// Uses Redis MGET to fetch all room counters in a single round-trip,
    /// avoiding N+1 queries. Local-only managers return local counts;
    /// Redis-backed managers return an error when Redis is unavailable.
    pub async fn room_connection_count_distributed_batch(
        &self,
        room_ids: &[&RoomId],
    ) -> Result<Vec<usize>, String> {
        if room_ids.is_empty() {
            return Ok(Vec::new());
        }

        if let Some(mut conn) = self
            .redis_conn_snapshot_required(
                "Distributed room connection counts unavailable while Redis is degraded",
            )
            .await?
        {
            let keys: Vec<String> = room_ids
                .iter()
                .map(|rid| self.room_counter_key(rid))
                .collect();

            match self
                .redis_op("read distributed room connection counts", async {
                    redis::cmd("MGET")
                        .arg(&keys)
                        .query_async::<Vec<Option<i64>>>(&mut conn)
                        .await
                })
                .await
            {
                Ok(values) => {
                    return Ok(values
                        .into_iter()
                        .map(|v| i64_to_usize_saturating(v.filter(|&c| c > 0).unwrap_or(0)))
                        .collect());
                }
                Err(e) => {
                    warn!("{e}");
                    return Err(
                        "Distributed room connection counts unavailable while Redis is degraded"
                            .to_string(),
                    );
                }
            }
        }

        Ok(room_ids
            .iter()
            .map(|rid| self.room_connection_count(rid))
            .collect())
    }

    /// Get total connections ever established
    #[must_use]
    pub fn total_connections_ever(&self) -> u64 {
        self.total_connections_ever.load(Ordering::Relaxed)
    }

    /// Get total messages processed
    #[must_use]
    pub fn total_messages(&self) -> u64 {
        self.total_messages.load(Ordering::Relaxed)
    }

    /// Get connection info
    #[must_use]
    pub fn get_connection(&self, connection_id: &str) -> Option<ConnectionInfo> {
        self.connections.get(connection_id).map(|c| c.clone())
    }

    /// Get all connections for a user
    #[must_use]
    pub fn get_user_connections(&self, user_id: &UserId) -> Vec<ConnectionInfo> {
        // Collect IDs first, then release the index DashMap lock before accessing
        // `connections` to avoid cross-DashMap lock ordering issues.
        let conn_ids: Vec<String> = self
            .user_connections
            .get(user_id)
            .map(|ids| ids.clone())
            .unwrap_or_default();

        conn_ids
            .iter()
            .filter_map(|id| self.connections.get(id).map(|c| c.clone()))
            .collect()
    }

    /// Get all connections in a room
    #[must_use]
    pub fn get_room_connections(&self, room_id: &RoomId) -> Vec<ConnectionInfo> {
        // Collect IDs first, then release the index DashMap lock before accessing
        // `connections` to avoid cross-DashMap lock ordering issues.
        let conn_ids: Vec<String> = self
            .room_connections
            .get(room_id)
            .map(|ids| ids.clone())
            .unwrap_or_default();

        conn_ids
            .iter()
            .filter_map(|id| self.connections.get(id).map(|c| c.clone()))
            .collect()
    }

    /// Get metrics summary
    #[must_use]
    pub fn metrics(&self) -> ConnectionMetrics {
        ConnectionMetrics {
            active_connections: self.connection_count(),
            total_connections_ever: self.total_connections_ever(),
            total_messages: self.total_messages(),
            active_users: self.user_connections.len(),
            active_rooms: self.room_connections.len(),
        }
    }
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new(ConnectionLimits::default())
    }
}

#[cfg(test)]
mod tests;
