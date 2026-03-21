use dashmap::DashMap;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use synctv_core::models::id::{RoomId, UserId};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

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

/// Connection information
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub connection_id: String,
    pub user_id: UserId,
    pub room_id: Option<RoomId>,
    pub connected_at: Instant,
    pub last_activity: Instant,
    pub message_count: u64,
    pub rtc_joined: bool,
    pub rtc_joined_at: Option<Instant>,
}

/// Serializable version of ConnectionInfo for Redis persistence.
/// Uses Unix timestamps instead of Instant for cross-process compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConnectionInfoPersistent {
    connection_id: String,
    user_id: String,
    room_id: Option<String>,
    connected_at_unix: u64,
    last_activity_unix: u64,
    message_count: u64,
    rtc_joined: bool,
    rtc_joined_at_unix: Option<u64>,
}

fn system_time_to_unix_secs(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .unwrap_or_else(|error| {
            warn!("System clock is before UNIX_EPOCH; using zero timestamp fallback: {error}");
            Duration::ZERO
        })
        .as_secs()
}

impl From<&ConnectionInfo> for ConnectionInfoPersistent {
    fn from(info: &ConnectionInfo) -> Self {
        let now = SystemTime::now();
        let now_unix = system_time_to_unix_secs(now);
        let connected_at_unix = now_unix.saturating_sub(info.connected_at.elapsed().as_secs());
        let last_activity_unix = now_unix.saturating_sub(info.last_activity.elapsed().as_secs());
        let rtc_joined_at_unix = info
            .rtc_joined_at
            .map(|joined| now_unix.saturating_sub(joined.elapsed().as_secs()));

        Self {
            connection_id: info.connection_id.clone(),
            user_id: info.user_id.as_str().to_string(),
            room_id: info.room_id.as_ref().map(|r| r.as_str().to_string()),
            connected_at_unix,
            last_activity_unix,
            message_count: info.message_count,
            rtc_joined: info.rtc_joined,
            rtc_joined_at_unix,
        }
    }
}

impl ConnectionInfo {
    #[must_use]
    pub fn new(connection_id: String, user_id: UserId) -> Self {
        let now = Instant::now();
        Self {
            connection_id,
            user_id,
            room_id: None,
            connected_at: now,
            last_activity: now,
            message_count: 0,
            rtc_joined: false,
            rtc_joined_at: None,
        }
    }

    #[must_use]
    pub fn duration(&self) -> Duration {
        self.connected_at.elapsed()
    }

    #[must_use]
    pub fn idle_duration(&self) -> Duration {
        self.last_activity.elapsed()
    }

    /// Get the duration since the WebRTC session was joined, if joined
    #[must_use]
    pub fn rtc_session_duration(&self) -> Option<Duration> {
        self.rtc_joined_at.map(|joined| joined.elapsed())
    }
}

/// Connection limits configuration
#[derive(Debug, Clone)]
pub struct ConnectionLimits {
    /// Maximum connections per user
    pub max_per_user: usize,

    /// Maximum connections per room
    pub max_per_room: usize,

    /// Maximum total connections
    pub max_total: usize,

    /// Idle timeout (disconnect if no activity)
    pub idle_timeout: Duration,

    /// Maximum connection duration
    pub max_duration: Duration,

    /// WebRTC session timeout (remove from RTC-joined set if inactive)
    pub webrtc_session_timeout: Duration,
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        Self {
            max_per_user: 5,
            max_per_room: 200,
            max_total: 10000,
            idle_timeout: Duration::from_mins(5),   // 5 minutes
            max_duration: Duration::from_hours(24), // 24 hours
            webrtc_session_timeout: Duration::from_hours(2), // 2 hours
        }
    }
}

/// TTL for distributed connection counters in Redis (seconds).
///
/// Acts as a crash-safety mechanism: if a node crashes without decrementing,
/// the counter will expire after this duration. Set to 2x the TTL refresh
/// interval (60s) to balance crash recovery speed with tolerance for
/// transient network issues. This allows the counter to survive one missed
/// refresh while detecting crashes more quickly than the previous 3x multiplier.
const DISTRIBUTED_COUNTER_TTL_SECONDS: i64 = 120; // 2x TTL refresh interval (60s)

/// Maximum number of keys to refresh in a single batch during TTL refresh.
///
/// This prevents memory and network pressure when there are many connections.
/// With 10,000 connections, we'll have ~30,000 keys (counter + metadata per connection),
/// which will be processed in ~30 batches of 1000 keys each.
const TTL_REFRESH_BATCH_SIZE: usize = 1000;

/// TTL for connection metadata keys in Redis (seconds).
/// Set to max_duration (24h) + buffer (1h) so metadata auto-expires if a node
/// crashes without calling unregister(). The TTL refresh task keeps active
/// connections alive by periodically resetting this TTL.
const CONNECTION_METADATA_TTL_SECONDS: i64 = 90_000; // 25 hours

/// A failed Redis counter operation that should be retried.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingRedisOp {
    /// Decrement a counter key
    Decr(String),
    /// Delete a key
    Del(String),
    /// Remove a member from a Redis set
    SRem { key: String, member: String },
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

#[derive(Clone)]
enum RedisConnHandle {
    Direct(redis::aio::ConnectionManager),
    Shared(std::sync::Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>),
}

impl RedisConnHandle {
    async fn snapshot(&self) -> redis::aio::ConnectionManager {
        match self {
            Self::Direct(conn) => conn.clone(),
            Self::Shared(conn) => conn.read().await.clone(),
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

    /// Connection limits
    limits: Arc<ConnectionLimits>,

    /// Atomic total connection count for race-free limit enforcement.
    /// Incremented atomically during `register()`, decremented during `unregister()`.
    total_connections: Arc<AtomicUsize>,

    /// Metrics
    total_connections_ever: Arc<AtomicU64>,
    total_messages: Arc<AtomicU64>,

    /// Broadcast channel for disconnect signals
    disconnect_tx: Arc<broadcast::Sender<DisconnectSignal>>,

    /// Pending room slot reservations (pre-upgrade).
    /// Counts how many WebSocket upgrades are in-flight for each room.
    /// Used to prevent TOCTOU race conditions on connection limits.
    pending_room_reservations: Arc<DashMap<RoomId, AtomicUsize>>,

    /// Pending user slot reservations (pre-upgrade).
    /// Counts how many WebSocket upgrades are in-flight for each user.
    pending_user_reservations: Arc<DashMap<UserId, AtomicUsize>>,

    /// Pending disconnect signals that failed to send (channel full).
    /// These are retried by a background task to ensure reliable delivery.
    pending_disconnects: Arc<DashMap<u64, (DisconnectSignal, Instant)>>,

    /// Counter for generating unique IDs for pending disconnect signals
    pending_disconnect_id: Arc<AtomicU64>,

    /// Counter for tracking dropped disconnect signals (monitoring)
    dropped_disconnect_signals: Arc<AtomicU64>,

    /// Counter for tracking retried disconnect signals (monitoring)
    retried_disconnect_signals: Arc<AtomicU64>,

    /// Optional Redis connection handle for distributed connection counting.
    /// When present, per-user and per-room limits are enforced across all replicas.
    /// When absent, limits are per-node only (fallback).
    ///
    /// In Sentinel deployments, prefer the shared handle so new method calls
    /// observe failover hot-swaps instead of holding a stale connection snapshot.
    redis_conn: Option<RedisConnHandle>,

    /// Key prefix for Redis keys (e.g., "synctv:")
    redis_key_prefix: String,

    /// Cancellation token for the auto-spawned TTL refresh task.
    /// Cancelled on shutdown to stop the background task.
    ttl_refresh_cancel: Arc<tokio_util::sync::CancellationToken>,

    /// Cancellation token for the disconnect signal retry task.
    /// Cancelled on shutdown to stop the background task.
    disconnect_retry_cancel: Arc<tokio_util::sync::CancellationToken>,

    /// Whether the disconnect retry task has already been started.
    /// Guards `start()` against spawning duplicate background tasks when
    /// startup wiring calls it more than once.
    disconnect_retry_started: Arc<std::sync::atomic::AtomicBool>,
    /// JoinHandle for the disconnect retry task so shutdown can await termination.
    disconnect_retry_handle: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// JoinHandle for the TTL refresh task.
    ttl_refresh_handle: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// JoinHandle for the pending Redis retries task.
    pending_retries_handle: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,

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
}

/// Maximum capacity of the pending retry queue for failed Redis counter operations.
/// When the queue is full, new entries are dropped with a warning log. The
/// TTL-based expiry on Redis keys ensures eventual consistency even if retries
/// are lost.
const PENDING_RETRY_QUEUE_CAPACITY: usize = 10_000;

/// Maximum capacity of the pending disconnect signals queue.
/// When the broadcast channel is full (lagging receivers), disconnect signals
/// are stored here for retry. This ensures kick/ban operations are not lost
/// even under high load.
const PENDING_DISCONNECT_QUEUE_CAPACITY: usize = 10_000;

impl ConnectionManager {
    /// Create a new `ConnectionManager`.
    ///
    /// **Note**: this constructor does not spawn any background tasks. Call
    /// [`start`](Self::start) (or [`with_redis`](Self::with_redis), which calls
    /// it internally) after construction to launch the disconnect-signal retry
    /// task and any Redis-related background work.
    #[must_use]
    pub fn new(limits: ConnectionLimits) -> Self {
        // Use a large buffer (10 000) to minimise lag for critical events such as
        // ban/kick signals. A lagging receiver that falls behind by more than the
        // channel capacity would miss signals; the WebSocket handler has a periodic
        // re-validation backstop to handle the rare case where a signal is lost.
        let (disconnect_tx, _) = broadcast::channel(10_000);
        let (pending_retries_tx, pending_retries_rx) = mpsc::channel(PENDING_RETRY_QUEUE_CAPACITY);

        // Create the disconnect retry cancellation token
        let disconnect_retry_cancel = tokio_util::sync::CancellationToken::new();

        // Store the receiver so it is not dropped here. with_redis() will take it
        // and hand it to spawn_pending_retries_task when Redis is configured.
        Self {
            connections: Arc::new(DashMap::new()),
            claimed_connection_ids: Arc::new(std::sync::Mutex::new(HashSet::new())),
            user_connections: Arc::new(DashMap::new()),
            room_connections: Arc::new(DashMap::new()),
            limits: Arc::new(limits),
            total_connections: Arc::new(AtomicUsize::new(0)),
            total_connections_ever: Arc::new(AtomicU64::new(0)),
            total_messages: Arc::new(AtomicU64::new(0)),
            disconnect_tx: Arc::new(disconnect_tx),
            pending_room_reservations: Arc::new(DashMap::new()),
            pending_user_reservations: Arc::new(DashMap::new()),
            pending_disconnects: Arc::new(DashMap::new()),
            pending_disconnect_id: Arc::new(AtomicU64::new(0)),
            dropped_disconnect_signals: Arc::new(AtomicU64::new(0)),
            retried_disconnect_signals: Arc::new(AtomicU64::new(0)),
            redis_conn: None,
            redis_key_prefix: String::new(),
            ttl_refresh_cancel: Arc::new(tokio_util::sync::CancellationToken::new()),
            disconnect_retry_cancel: Arc::new(disconnect_retry_cancel),
            disconnect_retry_started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            disconnect_retry_handle: Arc::new(std::sync::Mutex::new(None)),
            ttl_refresh_handle: Arc::new(std::sync::Mutex::new(None)),
            pending_retries_handle: Arc::new(std::sync::Mutex::new(None)),
            pending_retries_tx,
            pending_retries_rx: Arc::new(tokio::sync::Mutex::new(Some(pending_retries_rx))),
        }
    }

    /// Start background tasks that require a Tokio runtime.
    ///
    /// Launches the disconnect-signal retry task. This must be called from
    /// within an async context (i.e. after the Tokio runtime is available).
    /// [`with_redis`](Self::with_redis) calls this automatically.
    pub fn start(&self) {
        if self.disconnect_retry_started.swap(true, Ordering::AcqRel) {
            debug!("Disconnect retry task already started; skipping duplicate start()");
            return;
        }
        let handle = self.spawn_disconnect_retry_task((*self.disconnect_retry_cancel).clone());
        *self
            .disconnect_retry_handle
            .lock()
            .expect("disconnect retry handle mutex poisoned") = Some(handle);
    }

    #[cfg(test)]
    fn disconnect_retry_task_started(&self) -> bool {
        self.disconnect_retry_started.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn background_tasks_running(&self) -> bool {
        self.disconnect_retry_handle
            .lock()
            .expect("disconnect retry handle mutex poisoned")
            .is_some()
            || self
                .ttl_refresh_handle
                .lock()
                .expect("ttl refresh handle mutex poisoned")
                .is_some()
            || self
                .pending_retries_handle
                .lock()
                .expect("pending retries handle mutex poisoned")
                .is_some()
    }

    const fn redis_enabled(&self) -> bool {
        self.redis_conn.is_some()
    }

    async fn redis_conn_snapshot(&self) -> Option<redis::aio::ConnectionManager> {
        match &self.redis_conn {
            Some(conn) => Some(conn.snapshot().await),
            None => None,
        }
    }

    /// Enable distributed connection counting via Redis.
    ///
    /// When Redis is configured, per-user and per-room connection limits are
    /// enforced across all replicas. Without Redis, limits are per-node only.
    ///
    /// Automatically spawns background tasks:
    /// - Disconnect-signal retry task (via [`start`](Self::start))
    /// - TTL refresh task (every 60s) for long-lived connection counters
    /// - Pending-retries task for failed Redis counter operations
    ///
    /// All tasks are cancelled when `shutdown()` is called.
    #[must_use]
    pub fn with_redis(mut self, conn: redis::aio::ConnectionManager, key_prefix: &str) -> Self {
        self.redis_conn = Some(RedisConnHandle::Direct(conn.clone()));
        self.redis_key_prefix = key_prefix.to_string();

        // Start the disconnect-signal retry task (idempotent if already running)
        self.start();

        // Auto-spawn the TTL refresh task so callers don't need to remember
        // to call spawn_ttl_refresh_task() manually.
        let cancel = tokio_util::sync::CancellationToken::new();
        self.ttl_refresh_cancel = Arc::new(cancel.clone());
        let handle = self.spawn_ttl_refresh_task(Duration::from_mins(1), cancel.clone());
        *self
            .ttl_refresh_handle
            .lock()
            .expect("ttl refresh handle mutex poisoned") = Some(handle);

        // Spawn the pending-retries background task.
        // Take the receiver that was stored in new() so it is not dropped.
        // If for any reason it was already taken (e.g. with_redis called twice),
        // fall back to creating a fresh channel.
        let rx = self
            .pending_retries_rx
            .try_lock()
            .ok()
            .and_then(|mut guard| guard.take());
        let rx = if let Some(rx) = rx {
            rx
        } else {
            // Fallback: create a fresh channel and update the sender.
            let (tx, rx) = mpsc::channel(PENDING_RETRY_QUEUE_CAPACITY);
            self.pending_retries_tx = tx;
            rx
        };
        let handle = Self::spawn_pending_retries_task(RedisConnHandle::Direct(conn), rx, cancel);
        *self
            .pending_retries_handle
            .lock()
            .expect("pending retries handle mutex poisoned") = Some(handle);

        self
    }

    /// Enable distributed connection counting via a shared Redis handle.
    ///
    /// This variant follows Sentinel failover hot-swaps because each operation
    /// resolves a fresh connection snapshot from the shared `RwLock`.
    #[must_use]
    pub fn with_shared_redis(
        mut self,
        conn: std::sync::Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
        key_prefix: &str,
    ) -> Self {
        self.redis_conn = Some(RedisConnHandle::Shared(conn.clone()));
        self.redis_key_prefix = key_prefix.to_string();

        self.start();

        let cancel = tokio_util::sync::CancellationToken::new();
        self.ttl_refresh_cancel = Arc::new(cancel.clone());
        let handle = self.spawn_ttl_refresh_task(Duration::from_mins(1), cancel.clone());
        *self
            .ttl_refresh_handle
            .lock()
            .expect("ttl refresh handle mutex poisoned") = Some(handle);

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
        let handle = Self::spawn_pending_retries_task(RedisConnHandle::Shared(conn), rx, cancel);
        *self
            .pending_retries_handle
            .lock()
            .expect("pending retries handle mutex poisoned") = Some(handle);

        self
    }

    /// Spawn a background task that retries failed Redis counter operations.
    ///
    /// Drains the `pending_retries_rx` channel every 5 seconds and retries each
    /// operation. Operations that still fail are re-queued (up to 3 attempts each,
    /// tracked internally) before being dropped with a warning.
    fn spawn_pending_retries_task(
        redis_conn: RedisConnHandle,
        mut rx: mpsc::Receiver<PendingRedisOp>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            /// Maximum retry attempts for a single failed operation before dropping it.
            const MAX_OP_RETRIES: u32 = 3;
            /// Interval between retry sweeps.
            const RETRY_INTERVAL: Duration = Duration::from_secs(5);

            let mut pending: Vec<(PendingRedisOp, u32)> = Vec::new();
            let mut ticker = tokio::time::interval(RETRY_INTERVAL);
            // Skip the first immediate tick
            ticker.tick().await;

            loop {
                tokio::select! {
                    () = cancel.cancelled() => {
                        info!("Pending Redis retries task shutting down");
                        return;
                    }
                    _ = ticker.tick() => {
                        // Drain all newly-queued operations
                        while let Ok(op) = rx.try_recv() {
                            pending.push((op, 0));
                        }

                        if pending.is_empty() {
                            continue;
                        }

                        let mut still_pending = Vec::new();
                        let mut conn = redis_conn.snapshot().await;

                        for (op, attempts) in pending.drain(..) {
                            let result = match &op {
                                PendingRedisOp::Decr(key) => {
                                    // Use raw DECR; don't need the atomic script here since
                                    // this is a compensating retry, not a live operation.
                                    conn.decr::<_, _, i64>(key, 1i64).await
                                }
                                PendingRedisOp::Del(key) => conn.del::<_, i64>(key).await,
                                PendingRedisOp::SRem { key, member } => {
                                    conn.srem::<_, _, i64>(key, member).await
                                }
                            };

                            match result {
                                Ok(_) => {
                                    debug!(op = ?op, "Pending Redis retry succeeded");
                                }
                                Err(e) => {
                                    let next_attempt = attempts + 1;
                                    if next_attempt >= MAX_OP_RETRIES {
                                        tracing::error!(
                                            op = ?op,
                                            attempts = next_attempt,
                                            error = %e,
                                            "ALERT: Dropping failed Redis counter operation after max retries. \
                                             Distributed connection count may be inaccurate. \
                                             Counter will self-correct when TTL expires."
                                        );
                                    } else {
                                        debug!(
                                            op = ?op,
                                            attempts = next_attempt,
                                            error = %e,
                                            "Redis retry failed, will retry again"
                                        );
                                        still_pending.push((op, next_attempt));
                                    }
                                }
                            }
                        }

                        pending = still_pending;
                    }
                }
            }
        })
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

    /// Spawn a background task that retries pending disconnect signals.
    ///
    /// This task periodically checks for disconnect signals that failed to send
    /// (because the broadcast channel was full) and retries them. This ensures
    /// that kick/ban operations are not lost even under high load.
    fn spawn_disconnect_retry_task(
        &self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let pending_disconnects = self.pending_disconnects.clone();
        let disconnect_tx = self.disconnect_tx.clone();
        let dropped_count = self.dropped_disconnect_signals.clone();
        let retried_count = self.retried_disconnect_signals.clone();

        tokio::spawn(async move {
            /// Interval between retry sweeps for pending disconnect signals.
            const RETRY_INTERVAL: Duration = Duration::from_millis(100);
            /// Maximum age of a pending disconnect signal before it's dropped (5 seconds).
            const MAX_SIGNAL_AGE: Duration = Duration::from_secs(5);
            /// Maximum retry attempts per signal (used by age-based eviction via `MAX_SIGNAL_AGE`).
            #[allow(dead_code)]
            const MAX_RETRIES: u32 = 50; // 50 * 100ms = 5 seconds

            let mut ticker = tokio::time::interval(RETRY_INTERVAL);
            // Skip the first immediate tick
            ticker.tick().await;

            loop {
                tokio::select! {
                    () = cancel.cancelled() => {
                        info!("Disconnect signal retry task shutting down");
                        return;
                    }
                    _ = ticker.tick() => {
                        let now = Instant::now();
                        let mut to_remove = Vec::new();
                        let mut retry_count = 0u64;

                        for entry in pending_disconnects.iter() {
                            let id = *entry.key();
                            let (signal, created_at) = entry.value();
                            let age = now.duration_since(*created_at);

                            // Check if signal is too old
                            if age > MAX_SIGNAL_AGE {
                                to_remove.push(id);
                                dropped_count.fetch_add(1, Ordering::Relaxed);
                                warn!(
                                    signal = ?signal,
                                    age_ms = age.as_millis(),
                                    "Dropping old disconnect signal after max retries"
                                );
                                continue;
                            }

                            // Try to resend the signal
                            if disconnect_tx.send(signal.clone()).is_ok() {
                                to_remove.push(id);
                                retry_count += 1;
                                debug!(
                                    signal = ?signal,
                                    age_ms = age.as_millis(),
                                    "Successfully retried disconnect signal"
                                );
                            } else {
                                // Channel still full, will retry next tick
                                // The signal remains in pending_disconnects
                            }
                        }

                        // Remove processed signals
                        for id in to_remove {
                            pending_disconnects.remove(&id);
                        }

                        if retry_count > 0 {
                            retried_count.fetch_add(retry_count, Ordering::Relaxed);
                        }
                    }
                }
            }
        })
    }

    /// Send a disconnect signal, storing it for retry if the channel is full.
    ///
    /// This method ensures that disconnect signals are not lost even when the
    /// broadcast channel is temporarily full. If the send fails, the signal
    /// is stored in `pending_disconnects` and will be retried by the background
    /// task spawned in `new()`.
    fn send_disconnect_signal(&self, signal: DisconnectSignal) {
        // First try to send directly
        if self.disconnect_tx.send(signal.clone()).is_ok() {
            // Signal sent successfully
        } else {
            // Channel might be full or have no receivers
            // Check if there are any subscribers by trying to get receiver count
            let receiver_count = self.disconnect_tx.receiver_count();

            if receiver_count == 0 {
                // No receivers - this is not an error, just log at debug level
                debug!(
                    signal = ?signal,
                    "Disconnect signal has no receivers (no active connections)"
                );
                return;
            }

            // Channel is full - store for retry
            if self.pending_disconnects.len() >= PENDING_DISCONNECT_QUEUE_CAPACITY {
                // Queue is full, have to drop the signal
                self.dropped_disconnect_signals
                    .fetch_add(1, Ordering::Relaxed);
                warn!(
                    signal = ?signal,
                    queue_size = self.pending_disconnects.len(),
                    "Disconnect signal queue full, dropping signal. \
                     This indicates severe system overload."
                );
                return;
            }

            // Store signal for retry
            let id = self.pending_disconnect_id.fetch_add(1, Ordering::Relaxed);
            self.pending_disconnects
                .insert(id, (signal.clone(), Instant::now()));

            warn!(
                signal = ?signal,
                pending_count = self.pending_disconnects.len(),
                "Disconnect signal queued for retry (broadcast channel full)"
            );
        }
    }

    /// Cancel the auto-spawned background tasks.
    ///
    /// Should be called during graceful shutdown to stop the background tasks.
    pub async fn shutdown(&self) -> ShutdownReport {
        self.ttl_refresh_cancel.cancel();
        self.disconnect_retry_cancel.cancel();

        let mut report = ShutdownReport::new();

        let ttl_refresh_handle = self
            .ttl_refresh_handle
            .lock()
            .expect("ttl refresh handle mutex poisoned")
            .take();
        if let Some(handle) = ttl_refresh_handle {
            report.ttl_refresh = Some(
                Self::await_shutdown_task("ttl refresh", Duration::from_secs(5), handle).await,
            );
        }

        let pending_retries_handle = self
            .pending_retries_handle
            .lock()
            .expect("pending retries handle mutex poisoned")
            .take();
        if let Some(handle) = pending_retries_handle {
            report.pending_retries = Some(
                Self::await_shutdown_task("pending Redis retries", Duration::from_secs(5), handle)
                    .await,
            );
        }

        let disconnect_retry_handle = self
            .disconnect_retry_handle
            .lock()
            .expect("disconnect retry handle mutex poisoned")
            .take();
        if let Some(handle) = disconnect_retry_handle {
            report.disconnect_retry = Some(
                Self::await_shutdown_task(
                    "disconnect retry",
                    Duration::from_secs(5),
                    handle,
                )
                .await,
            );
        }

        if !report.all_clean() {
            warn!(?report, "ConnectionManager shutdown observed background task failures");
        }

        report
    }

    pub(crate) fn abort_background_tasks(&self) {
        self.ttl_refresh_cancel.cancel();
        self.disconnect_retry_cancel.cancel();

        if let Some(handle) = self
            .ttl_refresh_handle
            .lock()
            .expect("ttl refresh handle mutex poisoned")
            .take()
        {
            handle.abort();
        }

        if let Some(handle) = self
            .pending_retries_handle
            .lock()
            .expect("pending retries handle mutex poisoned")
            .take()
        {
            handle.abort();
        }

        if let Some(handle) = self
            .disconnect_retry_handle
            .lock()
            .expect("disconnect retry handle mutex poisoned")
            .take()
        {
            handle.abort();
        }
    }

    async fn await_shutdown_task(
        task_name: &'static str,
        timeout_budget: Duration,
        handle: tokio::task::JoinHandle<()>,
    ) -> ShutdownTaskOutcome {
        match tokio::time::timeout(timeout_budget, handle).await {
            Ok(Ok(())) => {
                debug!(task = task_name, "ConnectionManager background task stopped");
                ShutdownTaskOutcome::Completed
            }
            Ok(Err(error)) if error.is_cancelled() => {
                debug!(task = task_name, "ConnectionManager background task cancelled");
                ShutdownTaskOutcome::Cancelled
            }
            Ok(Err(error)) => {
                let message = error.to_string();
                warn!(
                    task = task_name,
                    error = %message,
                    "ConnectionManager background task ended with join error during shutdown"
                );
                ShutdownTaskOutcome::Failed(message)
            }
            Err(_) => {
                warn!(
                    task = task_name,
                    timeout_secs = timeout_budget.as_secs(),
                    "ConnectionManager background task did not stop before shutdown timeout"
                );
                ShutdownTaskOutcome::TimedOut
            }
        }
    }

    /// Subscribe to disconnect signals
    ///
    /// Each connection should subscribe to this and monitor for disconnect signals
    /// that apply to them (by connection ID, user ID, or room ID)
    #[must_use]
    pub fn subscribe_disconnect(&self) -> broadcast::Receiver<DisconnectSignal> {
        self.disconnect_tx.subscribe()
    }

    /// Force disconnect a specific connection
    ///
    /// Sends a signal to the connection to close immediately.
    /// If the broadcast channel is full, the signal is queued for retry.
    pub fn disconnect_connection(&self, connection_id: &str) {
        info!(
            connection_id = %connection_id,
            "Forcing connection disconnect"
        );
        self.send_disconnect_signal(DisconnectSignal::Connection(connection_id.to_string()));
    }

    /// Force disconnect all connections for a user
    ///
    /// Used when a user is banned or kicked from all rooms.
    /// If the broadcast channel is full, the signal is queued for retry.
    pub fn disconnect_user(&self, user_id: &UserId) {
        let conn_count = self.user_connection_count(user_id);
        info!(
            user_id = %user_id.as_str(),
            connection_count = conn_count,
            "Forcing disconnect of all user connections"
        );
        self.send_disconnect_signal(DisconnectSignal::User(user_id.clone()));
    }

    /// Force disconnect all connections in a room
    ///
    /// Used when a room is deleted or all users need to be removed.
    /// If the broadcast channel is full, the signal is queued for retry.
    pub fn disconnect_room(&self, room_id: &RoomId) {
        let conn_count = self.room_connection_count(room_id);
        info!(
            room_id = %room_id.as_str(),
            connection_count = conn_count,
            "Forcing disconnect of all room connections"
        );
        self.send_disconnect_signal(DisconnectSignal::Room(room_id.clone()));
    }

    /// Force disconnect a specific user from a specific room
    ///
    /// Used when kicking a member from a room (not banning globally).
    /// If the broadcast channel is full, the signal is queued for retry.
    pub fn disconnect_user_from_room(&self, user_id: &UserId, room_id: &RoomId) {
        info!(
            user_id = %user_id.as_str(),
            room_id = %room_id.as_str(),
            "Forcing disconnect of user from room"
        );
        self.send_disconnect_signal(DisconnectSignal::UserFromRoom {
            user_id: user_id.clone(),
            room_id: room_id.clone(),
        });
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
            .entry(room_id.clone())
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
                        room_id = %room_id.as_str(),
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
            .entry(user_id.clone())
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
                        user_id = %user_id.as_str(),
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
    /// limits. In cluster mode, allowing local-only admission would let replicas
    /// oversubscribe the same user concurrently.
    pub async fn register(&self, connection_id: String, user_id: UserId) -> Result<(), String> {
        let claim = self.try_claim_connection_id(&connection_id)?;

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

        let total_key = format!("{}connections:total", self.redis_key_prefix);

        // Enforce the total connection limit across replicas when Redis is
        // configured. This must fail closed: in cluster mode, a best-effort
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
        //
        // When Redis is configured, use the atomic INCR return value as the
        // single source of truth for the cross-replica count. If the new count
        // exceeds the limit we immediately DECR and reject, avoiding any TOCTOU
        // window where two replicas could both pass the check concurrently.
        //
        // When Redis is not configured, fall back to the local DashMap count.
        // App wiring only enables Redis-backed ConnectionManager in cluster mode,
        // so a Redis error here means distributed state is unavailable and we
        // must fail closed instead of weakening enforcement.
        if self.redis_enabled() {
            let redis_key = format!(
                "{}connections:user:{}",
                self.redis_key_prefix,
                user_id.as_str()
            );
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
            let mut user_entry = self.user_connections.entry(user_id.clone()).or_default();
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
        let conn_info = ConnectionInfo::new(connection_id.clone(), user_id.clone());
        self.connections
            .insert(connection_id.clone(), conn_info.clone());

        // Persist connection metadata to Redis (best-effort)
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let conn_key = format!("{}conn_mgr:conn:{}", self.redis_key_prefix, connection_id);
            let user_index_key = format!(
                "{}conn_mgr:user:{}",
                self.redis_key_prefix,
                user_id.as_str()
            );

            let persistent = ConnectionInfoPersistent::from(&conn_info);
            let connection_id_clone = connection_id.clone();
            match serde_json::to_string(&persistent) {
                Ok(json) => {
                    let result: Result<(), _> = redis::cmd("SET")
                        .arg(&conn_key)
                        .arg(&json)
                        .arg("EX")
                        .arg(CONNECTION_METADATA_TTL_SECONDS)
                        .query_async(&mut conn)
                        .await;
                    if let Err(e) = result {
                        warn!("Failed to persist connection metadata to Redis: {e}");
                    }
                }
                Err(e) => {
                    warn!("Failed to serialize connection metadata for Redis: {e}");
                }
            }

            if let Err(e) = conn
                .sadd::<_, _, ()>(&user_index_key, &connection_id_clone)
                .await
            {
                warn!("Failed to add connection to user index: {e}");
            }
            let _: Result<(), _> = conn
                .expire(&user_index_key, CONNECTION_METADATA_TTL_SECONDS)
                .await;
        }

        // Update metrics
        self.total_connections_ever.fetch_add(1, Ordering::Relaxed);
        synctv_core::metrics::ACTIVE_CONNECTIONS.inc();
        if is_first_connection_for_user {
            synctv_core::metrics::http::USERS_ONLINE.inc();
        }
        synctv_core::metrics::cluster::CLUSTER_CONNECTIONS
            .set(self.total_connections.load(Ordering::Relaxed) as i64);

        info!(
            connection_id = %connection_id,
            user_id = %user_id.as_str(),
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
        let old_room_id: Option<RoomId> = {
            let old = self
                .connections
                .get(connection_id)
                .and_then(|c| c.room_id.clone());

            if let Some(ref old_room) = old {
                if old_room == &room_id {
                    return Ok(());
                }
            }
            old
        };

        // Check distributed per-room capacity first when Redis is enabled,
        // then commit the local room index update. In local mode, the room
        // limit is enforced inside the commit step under the room shard lock
        // so concurrent joins cannot oversubscribe the room.

        // Step 1: Check distributed per-room limit via Redis (when enabled).
        // In local mode we enforce the room limit inside the commit step below
        // under the room shard lock, which closes the TOCTOU race for
        // concurrent same-room joins.
        let redis_room_incremented = if self.redis_enabled() {
            let redis_key = format!(
                "{}connections:room:{}",
                self.redis_key_prefix,
                room_id.as_str()
            );
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

        // Step 2: Update connection info first. If the connection disappeared,
        // roll back the distributed increment and leave prior room membership intact.
        let conn_info_updated = if let Some(mut conn) = self.connections.get_mut(connection_id) {
            conn.room_id = Some(room_id.clone());
            conn.last_activity = Instant::now();
            Some(conn.clone())
        } else {
            if redis_room_incremented {
                let redis_key = format!(
                    "{}connections:room:{}",
                    self.redis_key_prefix,
                    room_id.as_str()
                );
                self.rollback_distributed_counter(redis_key).await;
            }
            return Err("Connection not found".to_string());
        };

        // Step 3: Commit the room move locally after all checks have passed.
        // Without Redis, enforce the room limit under the same shard lock as
        // the insert so concurrent local joins cannot oversubscribe the room.
        {
            let mut room_entry = self.room_connections.entry(room_id.clone()).or_default();
            if !self.redis_enabled() && room_entry.len() >= self.limits.max_per_room {
                if let Some(mut conn) = self.connections.get_mut(connection_id) {
                    conn.room_id = old_room_id.clone();
                }
                if redis_room_incremented {
                    let redis_key = format!(
                        "{}connections:room:{}",
                        self.redis_key_prefix,
                        room_id.as_str()
                    );
                    self.rollback_distributed_counter(redis_key).await;
                }
                return Err(format!(
                    "Room at capacity ({} connections)",
                    self.limits.max_per_room
                ));
            }
            room_entry.push(connection_id.to_string());
        }

        if let Some(ref old_room) = old_room_id {
            if let Some(mut old_room_conns) = self.room_connections.get_mut(old_room) {
                old_room_conns.retain(|id| id != connection_id);
                if old_room_conns.is_empty() {
                    drop(old_room_conns);
                    self.room_connections.remove(old_room);
                }
            }
        }

        // Step 4: Decrement the old room's distributed counter only after the
        // move succeeds. If it fails, enqueue a retry so Redis eventually matches
        // the in-memory truth.
        if let Some(old_room) = &old_room_id {
            let old_key = format!(
                "{}connections:room:{}",
                self.redis_key_prefix,
                old_room.as_str()
            );
            self.rollback_distributed_counter(old_key).await;
        }

        // Update Redis metadata with new room_id (best-effort)
        if let Some(info) = conn_info_updated {
            if let Some(mut conn) = self.redis_conn_snapshot().await {
                let conn_key = format!("{}conn_mgr:conn:{}", self.redis_key_prefix, connection_id);
                let room_index_key = format!(
                    "{}conn_mgr:room:{}",
                    self.redis_key_prefix,
                    room_id.as_str()
                );
                let old_room_index_key = old_room_id.as_ref().map(|old_room| {
                    format!(
                        "{}conn_mgr:room:{}",
                        self.redis_key_prefix,
                        old_room.as_str()
                    )
                });

                let persistent = ConnectionInfoPersistent::from(&info);
                let connection_id_clone = connection_id.to_string();
                match serde_json::to_string(&persistent) {
                    Ok(json) => {
                        let result: Result<(), _> = redis::cmd("SET")
                            .arg(&conn_key)
                            .arg(&json)
                            .arg("EX")
                            .arg(CONNECTION_METADATA_TTL_SECONDS)
                            .query_async(&mut conn)
                            .await;
                        if let Err(e) = result {
                            warn!("Failed to update connection metadata in Redis: {e}");
                        }
                    }
                    Err(e) => {
                        warn!("Failed to serialize updated connection metadata for Redis: {e}");
                    }
                }

                if let Err(e) = conn
                    .sadd::<_, _, ()>(&room_index_key, &connection_id_clone)
                    .await
                {
                    warn!("Failed to add connection to room index: {e}");
                }
                if let Some(old_room_index_key) = old_room_index_key.as_ref() {
                    if let Err(e) = conn
                        .srem::<_, _, ()>(old_room_index_key, &connection_id_clone)
                        .await
                    {
                        warn!("Failed to remove connection from previous room index: {e}");
                    }
                }
                let _: Result<(), _> = conn
                    .expire(&room_index_key, CONNECTION_METADATA_TTL_SECONDS)
                    .await;
                if let Some(old_room_index_key) = old_room_index_key.as_ref() {
                    let _: Result<(), _> = conn
                        .expire(old_room_index_key, CONNECTION_METADATA_TTL_SECONDS)
                        .await;
                }
            }
        }

        synctv_core::metrics::cluster::NODE_ACTIVE_ROOMS.set(self.room_connections.len() as i64);

        debug!(
            connection_id = %connection_id,
            room_id = %room_id.as_str(),
            "Connection joined room"
        );

        Ok(())
    }

    /// Record message activity for a connection
    pub fn record_message(&self, connection_id: &str) {
        if let Some(mut conn) = self.connections.get_mut(connection_id) {
            conn.last_activity = Instant::now();
            conn.message_count += 1;
        }
        self.total_messages.fetch_add(1, Ordering::Relaxed);
    }

    /// Unregister a connection
    ///
    /// Decrements both local and distributed (Redis) connection counters.
    pub async fn unregister(&self, connection_id: &str) {
        if let Some((_, conn_info)) = self.connections.remove(connection_id) {
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

            // Decrement distributed Redis counters and remove metadata.
            // Use a timeout to ensure cleanup completes promptly during normal
            // unregister. If the timeout expires (e.g., Redis is slow/down), the
            // TTL on Redis keys acts as a safety net for eventual cleanup.
            if let Some(conn_clone) = self.redis_conn_snapshot().await {
                let key_prefix = self.redis_key_prefix.clone();
                let user_id_str = conn_info.user_id.as_str().to_string();
                let room_id_str = conn_info.room_id.as_ref().map(|r| r.as_str().to_string());
                let connection_id_owned = connection_id.to_string();
                let retry_tx = self.pending_retries_tx.clone();

                let cleanup = async {
                    let this = self;

                    // Decrement total distributed counter
                    let total_key = format!("{key_prefix}connections:total");
                    if let Err(_e) = this.redis_decr(&total_key).await {
                        let _ = retry_tx.try_send(PendingRedisOp::Decr(total_key));
                    }

                    // Decrement user counter
                    let user_key = format!("{key_prefix}connections:user:{user_id_str}");
                    if let Err(_e) = this.redis_decr(&user_key).await {
                        let _ = retry_tx.try_send(PendingRedisOp::Decr(user_key));
                    }

                    // Decrement room counter
                    if let Some(ref room_id) = room_id_str {
                        let room_key = format!("{key_prefix}connections:room:{room_id}");
                        if let Err(_e) = this.redis_decr(&room_key).await {
                            let _ = retry_tx.try_send(PendingRedisOp::Decr(room_key));
                        }
                    }

                    // Remove metadata and index entries
                    let conn_key = format!("{key_prefix}conn_mgr:conn:{connection_id_owned}");
                    let user_index_key = format!("{key_prefix}conn_mgr:user:{user_id_str}");
                    let room_index_key = room_id_str
                        .as_ref()
                        .map(|r| format!("{key_prefix}conn_mgr:room:{r}"));

                    let mut mc = conn_clone.clone();
                    if mc.del::<_, i64>(&conn_key).await.is_err() {
                        let _ = retry_tx.try_send(PendingRedisOp::Del(conn_key));
                    }
                    if mc
                        .srem::<_, _, i64>(&user_index_key, &connection_id_owned)
                        .await
                        .is_err()
                    {
                        let _ = retry_tx.try_send(PendingRedisOp::SRem {
                            key: user_index_key,
                            member: connection_id_owned.clone(),
                        });
                    }
                    if let Some(room_key) = room_index_key {
                        if mc
                            .srem::<_, _, i64>(&room_key, &connection_id_owned)
                            .await
                            .is_err()
                        {
                            let _ = retry_tx.try_send(PendingRedisOp::SRem {
                                key: room_key,
                                member: connection_id_owned.clone(),
                            });
                        }
                    }
                };

                if tokio::time::timeout(Duration::from_secs(2), cleanup)
                    .await
                    .is_err()
                {
                    warn!(
                        connection_id = %connection_id,
                        "Redis cleanup timed out during unregister, enqueueing retries"
                    );
                    // Enqueue all decrement operations for retry
                    let total_key = format!("{}connections:total", self.redis_key_prefix);
                    self.enqueue_retry(PendingRedisOp::Decr(total_key));
                    let user_key =
                        format!("{}connections:user:{}", self.redis_key_prefix, user_id_str);
                    self.enqueue_retry(PendingRedisOp::Decr(user_key));
                    if let Some(ref room_id) = room_id_str {
                        let room_key =
                            format!("{}connections:room:{room_id}", self.redis_key_prefix);
                        self.enqueue_retry(PendingRedisOp::Decr(room_key));
                        let room_index_key =
                            format!("{}conn_mgr:room:{room_id}", self.redis_key_prefix);
                        self.enqueue_retry(PendingRedisOp::SRem {
                            key: room_index_key,
                            member: connection_id.to_string(),
                        });
                    }
                    let conn_key =
                        format!("{}conn_mgr:conn:{connection_id}", self.redis_key_prefix);
                    self.enqueue_retry(PendingRedisOp::Del(conn_key));
                    let user_index_key =
                        format!("{}conn_mgr:user:{}", self.redis_key_prefix, user_id_str);
                    self.enqueue_retry(PendingRedisOp::SRem {
                        key: user_index_key,
                        member: connection_id.to_string(),
                    });
                }
            }

            synctv_core::metrics::ACTIVE_CONNECTIONS.dec();
            if user_went_offline {
                synctv_core::metrics::http::USERS_ONLINE.dec();
            }
            synctv_core::metrics::cluster::CLUSTER_CONNECTIONS
                .set(self.total_connections.load(Ordering::Relaxed) as i64);
            synctv_core::metrics::cluster::NODE_ACTIVE_ROOMS
                .set(self.room_connections.len() as i64);

            info!(
                connection_id = %connection_id,
                user_id = %conn_info.user_id.as_str(),
                duration = ?conn_info.duration(),
                message_count = conn_info.message_count,
                "Connection unregistered"
            );

            self.release_connection_id_claim(connection_id);
        }
    }

    /// Check for idle or expired connections
    ///
    /// Returns list of connection IDs that should be disconnected
    pub fn check_timeouts(&self) -> Vec<String> {
        let mut to_disconnect = Vec::new();
        // Collect RTC timeout mutations to apply after iteration to avoid
        // DashMap deadlock (iter() holds a read lock, mark_rtc_joined needs write).
        let mut rtc_timeouts: Vec<(RoomId, UserId, String)> = Vec::new();

        for entry in self.connections.iter() {
            let conn = entry.value();

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
                            user_id = %conn.user_id.as_str(),
                            room_id = ?conn.room_id,
                            rtc_session_duration = ?rtc_duration,
                            webrtc_session_timeout = ?self.limits.webrtc_session_timeout,
                            "WebRTC session timeout"
                        );
                        // Defer mutation to after iteration to avoid DashMap deadlock
                        if let Some(room_id) = &conn.room_id {
                            rtc_timeouts.push((
                                room_id.clone(),
                                conn.user_id.clone(),
                                conn.connection_id.clone(),
                            ));
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
    /// by `register`/`unregister`. Falls back to the local-only count if Redis
    /// is not configured or unavailable.
    pub async fn connection_count_distributed(&self) -> Result<usize, String> {
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let redis_key = format!("{}connections:total", self.redis_key_prefix);
            match conn.get::<_, Option<i64>>(&redis_key).await {
                Ok(Some(count)) if count > 0 => return Ok(count as usize),
                Ok(_) => return Ok(0),
                Err(e) => {
                    warn!("Failed to read distributed total connection count from Redis: {e}");
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

    /// Get connection count for a room across all replicas (distributed).
    ///
    /// Reads the Redis atomic counter (`connections:room:{room_id}`) which is
    /// maintained by `register`/`unregister`/`join_room`. Falls back to the
    /// local-only count if Redis is not configured or unavailable.
    pub async fn room_connection_count_distributed(
        &self,
        room_id: &RoomId,
    ) -> Result<usize, String> {
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let redis_key = format!(
                "{}connections:room:{}",
                self.redis_key_prefix,
                room_id.as_str()
            );
            match conn.get::<_, Option<i64>>(&redis_key).await {
                Ok(Some(count)) if count > 0 => return Ok(count as usize),
                Ok(_) => return Ok(0),
                Err(e) => {
                    warn!("Failed to read distributed room connection count from Redis: {e}");
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
    /// avoiding N+1 queries. Falls back to sequential local-only counts if
    /// Redis is not configured or unavailable.
    pub async fn room_connection_count_distributed_batch(
        &self,
        room_ids: &[&RoomId],
    ) -> Result<Vec<usize>, String> {
        if room_ids.is_empty() {
            return Ok(Vec::new());
        }

        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let keys: Vec<String> = room_ids
                .iter()
                .map(|rid| format!("{}connections:room:{}", self.redis_key_prefix, rid.as_str()))
                .collect();

            match redis::cmd("MGET")
                .arg(&keys)
                .query_async::<Vec<Option<i64>>>(&mut conn)
                .await
            {
                Ok(values) => {
                    return Ok(values
                        .into_iter()
                        .map(|v| v.filter(|&c| c > 0).unwrap_or(0) as usize)
                        .collect());
                }
                Err(e) => {
                    warn!(
                        "Failed to read distributed room connection counts from Redis (MGET): {e}"
                    );
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

    /// Get the number of distinct online users in a room on the local node.
    #[must_use]
    pub fn room_online_user_count(&self, room_id: &RoomId) -> usize {
        use std::collections::HashSet;

        self.get_room_connections(room_id)
            .into_iter()
            .map(|conn| conn.user_id)
            .collect::<HashSet<_>>()
            .len()
    }

    /// Get the number of distinct online users in a room across all replicas.
    pub async fn room_online_user_count_distributed(
        &self,
        room_id: &RoomId,
    ) -> Result<usize, String> {
        let counts = self
            .room_online_user_count_distributed_batch(&[room_id])
            .await?;
        Ok(counts.into_iter().next().unwrap_or(0))
    }

    /// Get distinct online user counts for multiple rooms across all replicas.
    pub async fn room_online_user_count_distributed_batch(
        &self,
        room_ids: &[&RoomId],
    ) -> Result<Vec<usize>, String> {
        if room_ids.is_empty() {
            return Ok(Vec::new());
        }

        if let Some(mut conn) = self.redis_conn_snapshot().await {
            use std::collections::{HashMap, HashSet};

            let mut room_to_users: HashMap<&str, HashSet<String>> = room_ids
                .iter()
                .map(|room_id| (room_id.as_str(), HashSet::new()))
                .collect();

            for room_id in room_ids {
                let connection_ids = self.get_room_connections_distributed(room_id).await?;
                for connection_id in connection_ids {
                    let conn_key =
                        format!("{}conn_mgr:conn:{connection_id}", self.redis_key_prefix);
                    let metadata: Option<String> = conn.get(&conn_key).await.map_err(|e| {
                        format!("Failed to fetch distributed connection metadata: {e}")
                    })?;

                    let Some(metadata) = metadata else {
                        continue;
                    };

                    let info: ConnectionInfoPersistent =
                        serde_json::from_str(&metadata).map_err(|e| {
                            format!("Failed to deserialize distributed connection metadata: {e}")
                        })?;

                    if info.room_id.as_deref() == Some(room_id.as_str()) {
                        room_to_users
                            .entry(room_id.as_str())
                            .or_default()
                            .insert(info.user_id);
                    }
                }
            }

            return Ok(room_ids
                .iter()
                .map(|room_id| room_to_users.get(room_id.as_str()).map_or(0, HashSet::len))
                .collect());
        }

        Ok(room_ids
            .iter()
            .map(|room_id| self.room_online_user_count(room_id))
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

    /// Get disconnect signal reliability metrics.
    ///
    /// Returns metrics for monitoring the disconnect signal retry mechanism:
    /// - `pending_count`: Number of signals currently queued for retry
    /// - `dropped_count`: Total signals dropped due to queue overflow or timeout
    /// - `retried_count`: Total signals successfully retried
    #[must_use]
    pub fn disconnect_signal_metrics(&self) -> DisconnectSignalMetrics {
        DisconnectSignalMetrics {
            pending_count: self.pending_disconnects.len(),
            dropped_count: self.dropped_disconnect_signals.load(Ordering::Relaxed),
            retried_count: self.retried_disconnect_signals.load(Ordering::Relaxed),
        }
    }

    /// Refresh TTLs on all active distributed connection counters and metadata in Redis.
    ///
    /// Long-lived connections (up to 24 hours) outlive the crash-safety TTL
    /// (`DISTRIBUTED_COUNTER_TTL_SECONDS`). Without periodic refreshes, the
    /// counter expires while the connection is still alive, causing distributed
    /// rate limiting to silently stop working.
    ///
    /// Also refreshes TTLs on connection metadata keys (`conn_mgr:conn:*`,
    /// `conn_mgr:user:*`, `conn_mgr:room:*`) to prevent them from expiring
    /// while the connection is still active.
    ///
    /// Additionally, synchronizes local connection counts to Redis counters to handle
    /// cases where Redis was temporarily unavailable during connection registration.
    /// This ensures eventual consistency between local and distributed counters.
    ///
    /// # Performance
    ///
    /// Uses a Lua script to batch refresh TTLs in groups of `TTL_REFRESH_BATCH_SIZE`
    /// keys at a time, reducing memory pressure and network round-trips compared to
    /// refreshing all keys at once.
    async fn refresh_distributed_counter_ttls(&self) {
        let Some(mut conn) = self.redis_conn_snapshot().await else {
            return;
        };

        // Collect unique user and room keys from active connections
        let mut counter_keys = std::collections::HashSet::new();
        let mut metadata_keys = std::collections::HashSet::new();

        for entry in self.user_connections.iter() {
            if !entry.value().is_empty() {
                counter_keys.insert(format!(
                    "{}connections:user:{}",
                    self.redis_key_prefix,
                    entry.key().as_str()
                ));
                // R-P2-4: Also refresh user index metadata TTL
                metadata_keys.insert(format!(
                    "{}conn_mgr:user:{}",
                    self.redis_key_prefix,
                    entry.key().as_str()
                ));
            }
        }
        for entry in self.room_connections.iter() {
            if !entry.value().is_empty() {
                counter_keys.insert(format!(
                    "{}connections:room:{}",
                    self.redis_key_prefix,
                    entry.key().as_str()
                ));
                // R-P2-4: Also refresh room index metadata TTL
                metadata_keys.insert(format!(
                    "{}conn_mgr:room:{}",
                    self.redis_key_prefix,
                    entry.key().as_str()
                ));
            }
        }

        // R-P2-4: Refresh per-connection metadata TTLs
        for entry in self.connections.iter() {
            metadata_keys.insert(format!(
                "{}conn_mgr:conn:{}",
                self.redis_key_prefix,
                entry.key()
            ));
        }

        // Also refresh the total connections counter TTL
        if self.connection_count() > 0 {
            let total_key = format!("{}connections:total", self.redis_key_prefix);
            counter_keys.insert(total_key);
        }

        let total_keys = counter_keys.len() + metadata_keys.len();
        if total_keys == 0 {
            return;
        }

        let mut failure_count = 0u64;
        let mut success_count = 0u64;

        // Use batched Lua script for efficient TTL refresh
        // This reduces network round-trips compared to individual EXPIRE commands
        let result = self
            .batch_refresh_ttls_with_lua(&mut conn, &counter_keys, &metadata_keys)
            .await;

        match result {
            Ok(refreshed) => {
                success_count = refreshed as u64;
            }
            Err(e) => {
                failure_count = total_keys as u64;
                warn!("Failed to refresh TTLs via Lua script ({total_keys} keys): {e}");
            }
        }

        // Update monitoring metrics
        if success_count > 0 {
            synctv_core::metrics::cluster::DISTRIBUTED_COUNTER_TTL_REFRESHES
                .with_label_values(&["success"])
                .inc_by(success_count);
        }
        if failure_count > 0 {
            synctv_core::metrics::cluster::DISTRIBUTED_COUNTER_TTL_REFRESHES
                .with_label_values(&["failure"])
                .inc_by(failure_count);
            let consecutive =
                synctv_core::metrics::cluster::DISTRIBUTED_COUNTER_TTL_CONSECUTIVE_FAILURES.get()
                    + 1;
            synctv_core::metrics::cluster::DISTRIBUTED_COUNTER_TTL_CONSECUTIVE_FAILURES
                .set(consecutive);
            if consecutive >= 3 {
                warn!(
                    consecutive_failures = consecutive,
                    "ALERT: Distributed counter TTL refresh has failed {} consecutive times. \
                     Connection rate limiting across replicas may stop working if counters expire.",
                    consecutive
                );
            }
        } else if !counter_keys.is_empty() || !metadata_keys.is_empty() {
            // Reset consecutive failure counter on full success
            synctv_core::metrics::cluster::DISTRIBUTED_COUNTER_TTL_CONSECUTIVE_FAILURES.set(0);
        }

        let total_refreshed = (counter_keys.len() + metadata_keys.len()) as i64;
        synctv_core::metrics::cluster::DISTRIBUTED_COUNTER_TTL_KEYS_REFRESHED.set(total_refreshed);

        if !counter_keys.is_empty() || !metadata_keys.is_empty() {
            debug!(
                counter_keys = counter_keys.len(),
                metadata_keys = metadata_keys.len(),
                failures = failure_count,
                "Refreshed TTLs on distributed counters and connection metadata"
            );
        }

        // Perform full reconciliation with Redis after TTL refresh.
        // This handles cases where Redis was temporarily unavailable during
        // connection registration, ensuring eventual consistency.
        // Note: This includes sync_local_counts_to_redis plus metadata sync and cleanup.
        self.reconcile_with_redis().await;
    }

    /// Batch refresh TTLs using a Lua script for efficiency.
    ///
    /// Processes keys in batches of `TTL_REFRESH_BATCH_SIZE` to avoid
    /// excessive memory usage and network payload sizes.
    async fn batch_refresh_ttls_with_lua(
        &self,
        conn: &mut redis::aio::ConnectionManager,
        counter_keys: &std::collections::HashSet<String>,
        metadata_keys: &std::collections::HashSet<String>,
    ) -> Result<usize, redis::RedisError> {
        // Lua script that refreshes TTLs for multiple keys in a single call.
        // Takes key prefixes and TTL values, returns number of keys refreshed.
        let lua_script = redis::Script::new(
            r#"
            local counter_ttl = tonumber(ARGV[1])
            local metadata_ttl = tonumber(ARGV[2])
            local refreshed = 0

            -- Refresh counter keys (KEYS[1] to KEYS[N] where N = #counter_keys)
            local num_counter_keys = tonumber(ARGV[3])
            for i = 1, num_counter_keys do
                local key = KEYS[i]
                if redis.call("EXISTS", key) == 1 then
                    redis.call("EXPIRE", key, counter_ttl)
                    refreshed = refreshed + 1
                end
            end

            -- Refresh metadata keys
            local num_metadata_keys = tonumber(ARGV[4])
            for i = 1, num_metadata_keys do
                local key = KEYS[num_counter_keys + i]
                if redis.call("EXISTS", key) == 1 then
                    redis.call("EXPIRE", key, metadata_ttl)
                    refreshed = refreshed + 1
                end
            end

            return refreshed
            "#,
        );

        let counter_keys_vec: Vec<&String> = counter_keys.iter().collect();
        let metadata_keys_vec: Vec<&String> = metadata_keys.iter().collect();
        let total_keys = counter_keys_vec.len() + metadata_keys_vec.len();
        let mut total_refreshed = 0usize;

        // Process in batches to avoid oversized Lua script payloads
        let mut counter_offset = 0usize;
        let mut metadata_offset = 0usize;

        while counter_offset < counter_keys_vec.len() || metadata_offset < metadata_keys_vec.len() {
            // Collect a batch of keys
            let mut batch_keys: Vec<&String> = Vec::with_capacity(TTL_REFRESH_BATCH_SIZE);
            let mut batch_counter_count = 0usize;
            let mut batch_metadata_count = 0usize;

            // Add counter keys to batch
            while counter_offset < counter_keys_vec.len()
                && batch_keys.len() < TTL_REFRESH_BATCH_SIZE
            {
                batch_keys.push(counter_keys_vec[counter_offset]);
                batch_counter_count += 1;
                counter_offset += 1;
            }

            // Add metadata keys to batch
            while metadata_offset < metadata_keys_vec.len()
                && batch_keys.len() < TTL_REFRESH_BATCH_SIZE
            {
                batch_keys.push(metadata_keys_vec[metadata_offset]);
                batch_metadata_count += 1;
                metadata_offset += 1;
            }

            if batch_keys.is_empty() {
                break;
            }

            // Build and execute the Lua script for this batch
            let mut script_invocation = lua_script.prepare_invoke();
            for key in &batch_keys {
                script_invocation.key(*key);
            }
            script_invocation
                .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
                .arg(CONNECTION_METADATA_TTL_SECONDS)
                .arg(batch_counter_count as i64)
                .arg(batch_metadata_count as i64);

            let refreshed: i64 = script_invocation.invoke_async(conn).await?;
            total_refreshed += refreshed as usize;

            debug!(
                batch_size = batch_keys.len(),
                refreshed = refreshed,
                total_refreshed = total_refreshed,
                remaining = total_keys.saturating_sub(total_refreshed),
                "Batch TTL refresh completed"
            );
        }

        Ok(total_refreshed)
    }

    /// Synchronize local connection counts to Redis distributed counters.
    ///
    /// This method compares local connection counts with Redis counter values
    /// and corrects any discrepancies. This is important for recovering from
    /// situations where Redis was temporarily unavailable during connection
    /// registration or unregistration.
    ///
    /// The synchronization is intentionally one-sided: it repairs counters that
    /// are missing or lower than this node's local contribution, but never
    /// decreases a Redis counter based only on local state. Lowering a
    /// distributed counter from one replica would overwrite connections that are
    /// still legitimately active on other replicas.
    async fn sync_local_counts_to_redis(&self, conn: &mut redis::aio::ConnectionManager) {
        // Collect local counts first (avoid holding locks during Redis operations)
        let mut user_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for entry in self.user_connections.iter() {
            let count = entry.value().len();
            if count > 0 {
                let key = format!(
                    "{}connections:user:{}",
                    self.redis_key_prefix,
                    entry.key().as_str()
                );
                user_counts.insert(key, count);
            }
        }

        let mut room_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for entry in self.room_connections.iter() {
            let count = entry.value().len();
            if count > 0 {
                let key = format!(
                    "{}connections:room:{}",
                    self.redis_key_prefix,
                    entry.key().as_str()
                );
                room_counts.insert(key, count);
            }
        }

        let local_total = self.connection_count();
        let total_key = format!("{}connections:total", self.redis_key_prefix);

        // Lua script to atomically repair counters that are missing or lower than
        // this node's observed minimum contribution. It never decreases the
        // current Redis value because other replicas may have active
        // connections that are not visible from this node's local memory.
        //
        // Returns `{current_value, 1}` when the counter was raised and
        // `{current_value, 0}` when no change was needed.
        let sync_script = redis::Script::new(
            r"local current = redis.call('GET', KEYS[1])
              local current_num = 0
              if current ~= false then
                current_num = tonumber(current)
              end
              local expected_min = tonumber(ARGV[1])
              if current_num < expected_min then
                redis.call('SET', KEYS[1], ARGV[1])
                redis.call('EXPIRE', KEYS[1], ARGV[2])
                return {current_num, 1}
              end
              return {current_num, 0}",
        );

        let mut sync_count = 0u64;
        let mut sync_errors = 0u64;

        // Sync user counters
        for (key, local_count) in &user_counts {
            let script_result: Result<Vec<i64>, _> = sync_script
                .key(key)
                .arg(*local_count as i64)
                .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
                .invoke_async(conn)
                .await;
            match script_result {
                Ok(result) if result.len() >= 2 => {
                    let old_value = result[0];
                    let was_changed = result[1];
                    if was_changed == 1 {
                        sync_count += 1;
                        debug!(
                            key = %key,
                            old_value = old_value,
                            new_value = *local_count,
                            "Raised user connection counter in Redis to cover local connections"
                        );
                    }
                }
                Ok(_) => {
                    // Unexpected result format
                    warn!(key = %key, "Unexpected result format from Redis sync script");
                }
                Err(e) => {
                    sync_errors += 1;
                    warn!(key = %key, error = %e, "Failed to sync user counter to Redis");
                }
            }
        }

        // Sync room counters
        for (key, local_count) in &room_counts {
            let script_result: Result<Vec<i64>, _> = sync_script
                .key(key)
                .arg(*local_count as i64)
                .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
                .invoke_async(conn)
                .await;
            match script_result {
                Ok(result) if result.len() >= 2 => {
                    let old_value = result[0];
                    let was_changed = result[1];
                    if was_changed == 1 {
                        sync_count += 1;
                        debug!(
                            key = %key,
                            old_value = old_value,
                            new_value = *local_count,
                            "Raised room connection counter in Redis to cover local connections"
                        );
                    }
                }
                Ok(_) => {
                    warn!(key = %key, "Unexpected result format from Redis sync script");
                }
                Err(e) => {
                    sync_errors += 1;
                    warn!(key = %key, error = %e, "Failed to sync room counter to Redis");
                }
            }
        }

        let script_result: Result<Vec<i64>, _> = sync_script
            .key(&total_key)
            .arg(local_total as i64)
            .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
            .invoke_async(conn)
            .await;
        match script_result {
            Ok(result) if result.len() >= 2 => {
                let old_value = result[0];
                let was_changed = result[1];
                if was_changed == 1 {
                    sync_count += 1;
                    warn!(
                        key = %total_key,
                        old_value = old_value,
                        new_value = local_total,
                        "Raised total connection counter in Redis to cover local connections"
                    );
                }
            }
            Ok(_) => {
                warn!(key = %total_key, "Unexpected result format from Redis sync script");
            }
            Err(e) => {
                sync_errors += 1;
                warn!(key = %total_key, error = %e, "Failed to sync total counter to Redis");
            }
        }

        for key in [
            format!("{}connections:user:*", self.redis_key_prefix),
            format!("{}connections:room:*", self.redis_key_prefix),
        ] {
            let mut cursor: u64 = 0;
            loop {
                let scan_result: Result<(u64, Vec<String>), _> = redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg(&key)
                    .arg("COUNT")
                    .arg(100)
                    .query_async(conn)
                    .await;

                match scan_result {
                    Ok((new_cursor, keys)) => {
                        cursor = new_cursor;
                        for redis_key in keys {
                            let is_known = user_counts.contains_key(&redis_key)
                                || room_counts.contains_key(&redis_key);
                            if is_known {
                                continue;
                            }

                            // Never zero a distributed counter based only on this
                            // node's local view. Another replica may still own the
                            // corresponding active connections. Stale cleanup is
                            // handled by TTL expiry and targeted metadata/index
                            // reconciliation elsewhere.
                        }

                        if cursor == 0 {
                            break;
                        }
                    }
                    Err(e) => {
                        sync_errors += 1;
                        warn!(pattern = %key, error = %e, "Failed to scan distributed counters");
                        break;
                    }
                }
            }
        }

        if sync_count > 0 || sync_errors > 0 {
            info!(
                counters_synced = sync_count,
                sync_errors = sync_errors,
                "Completed distributed counter synchronization"
            );
        }
    }

    /// Reconcile in-memory connection state with Redis after an outage recovery.
    ///
    /// This method performs a full reconciliation between local state and Redis:
    /// 1. Syncs local connection counts to Redis counters
    /// 2. Writes missing connection metadata to Redis
    /// 3. Cleans up stale Redis user/room index members that reference missing metadata
    ///
    /// # When to Call
    ///
    /// This method should be called:
    /// - Periodically by a background task (every 60s by default)
    /// - After detecting Redis has recovered from an outage
    /// - On startup to recover from previous unclean shutdowns
    ///
    /// # Trade-offs
    ///
    /// - **Pros**: Ensures eventual consistency, handles partial failures
    /// - **Cons**: Can be expensive with many connections; uses Redis round-trips
    ///
    /// # Errors
    ///
    /// Errors are logged but do not propagate. The method is designed to be
    /// eventually consistent - failures are retried on the next call.
    pub async fn reconcile_with_redis(&self) {
        let Some(mut conn) = self.redis_conn_snapshot().await else {
            // No Redis configured - nothing to reconcile
            return;
        };

        // Step 1: Sync connection counters (existing logic)
        self.sync_local_counts_to_redis(&mut conn).await;

        // Step 2: Sync connection metadata to Redis
        self.sync_connection_metadata_to_redis(&mut conn).await;

        // Step 3: Clean up stale Redis user/room index members that point to
        // missing connection metadata.
        //
        // Important: this must NOT delete `conn_mgr:conn:*` keys globally just
        // because this replica does not know about them. Those keys may belong
        // to healthy connections on other replicas.
        self.cleanup_stale_redis_indexes(&mut conn).await;
    }

    /// Sync local connection metadata to Redis.
    ///
    /// Writes metadata for all active connections. Uses SET with TTL to ensure
    /// keys are eventually cleaned up even if the node crashes.
    async fn sync_connection_metadata_to_redis(&self, conn: &mut redis::aio::ConnectionManager) {
        use redis::AsyncCommands;

        let mut synced = 0u64;
        let mut errors = 0u64;

        for entry in self.connections.iter() {
            let conn_info = entry.value();
            let key = format!(
                "{}conn_mgr:conn:{}",
                self.redis_key_prefix, conn_info.connection_id
            );
            let persistent = ConnectionInfoPersistent::from(conn_info);

            match serde_json::to_string(&persistent) {
                Ok(json_data) => {
                    let result: Result<(), _> = conn
                        .set_ex(&key, json_data, CONNECTION_METADATA_TTL_SECONDS as u64)
                        .await;

                    match result {
                        Ok(()) => {
                            synced += 1;
                        }
                        Err(e) => {
                            errors += 1;
                            warn!(
                                connection_id = %conn_info.connection_id,
                                error = %e,
                                "Failed to sync connection metadata to Redis"
                            );
                        }
                    }
                }
                Err(e) => {
                    errors += 1;
                    warn!(
                        connection_id = %conn_info.connection_id,
                        error = %e,
                        "Failed to serialize connection metadata"
                    );
                }
            }
        }

        if synced > 0 || errors > 0 {
            debug!(
                metadata_synced = synced,
                metadata_errors = errors,
                "Synced connection metadata to Redis"
            );
        }
    }

    /// Clean up stale Redis user/room index members whose metadata key is gone.
    ///
    /// This only removes index members that are provably invalid:
    /// - `conn_mgr:user:*` set members without a matching `conn_mgr:conn:*`
    /// - `conn_mgr:room:*` set members without a matching `conn_mgr:conn:*`
    ///
    /// It deliberately does not delete arbitrary `conn_mgr:conn:*` keys by
    /// scanning Redis and comparing against local memory. In a multi-replica
    /// cluster, metadata for connections on other replicas is valid and must
    /// not be removed by this node.
    async fn cleanup_stale_redis_indexes(&self, conn: &mut redis::aio::ConnectionManager) {
        use redis::AsyncCommands;

        let patterns = [
            format!("{}conn_mgr:user:*", self.redis_key_prefix),
            format!("{}conn_mgr:room:*", self.redis_key_prefix),
        ];
        let mut cleaned = 0u64;
        let mut errors = 0u64;

        for pattern in patterns {
            let mut cursor: u64 = 0;
            loop {
                let result: Result<(u64, Vec<String>), _> = redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg(&pattern)
                    .arg("COUNT")
                    .arg(100)
                    .query_async(conn)
                    .await;

                match result {
                    Ok((new_cursor, keys)) => {
                        cursor = new_cursor;

                        for key in keys {
                            let members: Result<Vec<String>, _> = conn.smembers(&key).await;
                            let members = match members {
                                Ok(members) => members,
                                Err(e) => {
                                    errors += 1;
                                    warn!(
                                        key = %key,
                                        error = %e,
                                        "Failed to fetch Redis index members during reconciliation"
                                    );
                                    continue;
                                }
                            };

                            for conn_id in members {
                                let conn_key =
                                    format!("{}conn_mgr:conn:{conn_id}", self.redis_key_prefix);
                                let exists: Result<bool, _> = conn.exists(&conn_key).await;
                                match exists {
                                    Ok(true) => {}
                                    Ok(false) => {
                                        let remove_result: Result<(), _> =
                                            conn.srem(&key, &conn_id).await;
                                        match remove_result {
                                            Ok(()) => {
                                                cleaned += 1;
                                                debug!(
                                                    index_key = %key,
                                                    connection_id = %conn_id,
                                                    "Removed stale distributed connection index member"
                                                );
                                            }
                                            Err(e) => {
                                                errors += 1;
                                                warn!(
                                                    index_key = %key,
                                                    connection_id = %conn_id,
                                                    error = %e,
                                                    "Failed to remove stale distributed connection index member"
                                                );
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        errors += 1;
                                        warn!(
                                            index_key = %key,
                                            connection_id = %conn_id,
                                            error = %e,
                                            "Failed to verify distributed connection metadata during reconciliation"
                                        );
                                    }
                                }
                            }

                            let key_is_empty: Result<bool, _> =
                                conn.scard::<_, usize>(&key).await.map(|count| count == 0);
                            match key_is_empty {
                                Ok(true) => {
                                    let _: Result<(), _> = conn.del(&key).await;
                                }
                                Ok(false) => {}
                                Err(e) => {
                                    errors += 1;
                                    warn!(
                                        key = %key,
                                        error = %e,
                                        "Failed to check Redis index cardinality during reconciliation"
                                    );
                                }
                            }
                        }

                        if cursor == 0 {
                            break;
                        }
                    }
                    Err(e) => {
                        errors += 1;
                        warn!(
                            pattern = %pattern,
                            error = %e,
                            "Failed to SCAN Redis for stale distributed connection indexes"
                        );
                        break;
                    }
                }
            }
        }

        if cleaned > 0 || errors > 0 {
            info!(
                stale_index_members_cleaned = cleaned,
                cleanup_errors = errors,
                "Cleaned up stale distributed connection indexes from Redis"
            );
        }
    }

    /// Spawn a background task that periodically refreshes TTLs on distributed
    /// connection counters in Redis.
    ///
    /// This prevents the crash-safety TTL from expiring while long-lived
    /// connections are still active. Runs every 60 seconds by default.
    #[must_use]
    pub fn spawn_ttl_refresh_task(
        &self,
        interval: Duration,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Skip the first immediate tick
            ticker.tick().await;
            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        info!("Distributed counter TTL refresh task shutting down");
                        return;
                    }
                    _ = ticker.tick() => {
                        manager.refresh_distributed_counter_ttls().await;
                    }
                }
            }
        })
    }

    /// Get connection ID for a user in a specific room
    ///
    /// Returns the first active connection ID found for the user in the room.
    /// For WebRTC, this allows us to identify which connection a user is using in a room.
    #[must_use]
    pub fn get_connection_id(&self, room_id: &RoomId, user_id: &UserId) -> Option<String> {
        // Collect IDs first to avoid holding cross-DashMap locks
        let conn_ids: Vec<String> = self
            .user_connections
            .get(user_id)
            .map(|ids| ids.clone())
            .unwrap_or_default();

        // Find the first connection that's in the specified room
        for conn_id in &conn_ids {
            if let Some(conn) = self.connections.get(conn_id) {
                if conn.room_id.as_ref() == Some(room_id) {
                    return Some(conn.connection_id.clone());
                }
            }
        }
        None
    }

    /// Mark a connection as joined or left WebRTC session
    ///
    /// This is used to track which connections are actively participating in WebRTC calls.
    pub fn mark_rtc_joined(&self, room_id: &RoomId, user_id: &UserId, conn_id: &str, joined: bool) {
        // Verify the connection belongs to the user and room
        if let Some(mut conn) = self.connections.get_mut(conn_id) {
            if &conn.user_id == user_id && conn.room_id.as_ref() == Some(room_id) {
                conn.rtc_joined = joined;
                // Set or clear the RTC join timestamp
                conn.rtc_joined_at = if joined { Some(Instant::now()) } else { None };
                debug!(
                    connection_id = %conn_id,
                    user_id = %user_id.as_str(),
                    room_id = %room_id.as_str(),
                    joined = joined,
                    "WebRTC join status updated"
                );
            }
        }
    }

    /// Get all connections in a room that have joined WebRTC
    #[must_use]
    pub fn get_rtc_connections(&self, room_id: &RoomId) -> Vec<ConnectionInfo> {
        // Collect IDs first to avoid holding cross-DashMap locks
        let conn_ids: Vec<String> = self
            .room_connections
            .get(room_id)
            .map(|ids| ids.clone())
            .unwrap_or_default();

        conn_ids
            .iter()
            .filter_map(|id| self.connections.get(id).map(|c| c.clone()))
            .filter(|conn| conn.rtc_joined)
            .collect()
    }

    /// Spawn a background task that periodically checks for idle/expired connections
    /// and sends disconnect signals for them.
    ///
    /// The task runs every `interval` and stops gracefully when `cancel_token` is cancelled.
    #[must_use]
    pub fn spawn_cleanup_task(
        &self,
        interval: Duration,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Skip the first immediate tick
            ticker.tick().await;
            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        info!("Connection cleanup task shutting down");
                        return;
                    }
                    _ = ticker.tick() => {
                        let stale = manager.check_timeouts();
                        if !stale.is_empty() {
                            info!(
                                count = stale.len(),
                                "Cleaning up stale connections"
                            );
                            for conn_id in &stale {
                                manager.disconnect_connection(conn_id);
                                manager.unregister(conn_id).await;
                            }
                        }
                    }
                }
            }
        })
    }

    /// Get all connection IDs for a user across all replicas (from Redis).
    ///
    /// Returns connection IDs from Redis, which includes connections from
    /// all replicas in the cluster.
    ///
    /// When Redis-backed distributed state is unavailable, this fails closed
    /// instead of silently degrading to local-only state. In cluster mode,
    /// a local fallback would return a partial view and break admin/security
    /// operations that require a global connection set.
    pub async fn get_user_connections_distributed(
        &self,
        user_id: &UserId,
    ) -> Result<Vec<String>, String> {
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let user_index_key = format!(
                "{}conn_mgr:user:{}",
                self.redis_key_prefix,
                user_id.as_str()
            );

            match conn.smembers::<_, Vec<String>>(&user_index_key).await {
                Ok(conn_ids) => return Ok(conn_ids),
                Err(e) => {
                    warn!("Failed to fetch user connections from Redis: {e}");
                    return Err(
                        "Distributed user connection lookup unavailable while Redis is degraded"
                            .to_string(),
                    );
                }
            }
        }

        Ok(self
            .get_user_connections(user_id)
            .into_iter()
            .map(|c| c.connection_id)
            .collect())
    }

    /// Get the total number of active connections for a user across all replicas.
    ///
    /// In standalone mode this uses local in-memory state. In cluster mode it
    /// derives the count from the Redis-backed distributed connection index.
    pub async fn user_connection_count_distributed(
        &self,
        user_id: &UserId,
    ) -> Result<usize, String> {
        Ok(self.get_user_connections_distributed(user_id).await?.len())
    }

    /// Get all connections in a room across all replicas (from Redis).
    ///
    /// Returns connection IDs from Redis, which includes connections from
    /// all replicas in the cluster.
    ///
    /// When Redis-backed distributed state is unavailable, this fails closed
    /// instead of silently degrading to local-only state.
    pub async fn get_room_connections_distributed(
        &self,
        room_id: &RoomId,
    ) -> Result<Vec<String>, String> {
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let room_index_key = format!(
                "{}conn_mgr:room:{}",
                self.redis_key_prefix,
                room_id.as_str()
            );

            match conn.smembers::<_, Vec<String>>(&room_index_key).await {
                Ok(conn_ids) => return Ok(conn_ids),
                Err(e) => {
                    warn!("Failed to fetch room connections from Redis: {e}");
                    return Err(
                        "Distributed room connection lookup unavailable while Redis is degraded"
                            .to_string(),
                    );
                }
            }
        }

        Ok(self
            .get_room_connections(room_id)
            .into_iter()
            .map(|c| c.connection_id)
            .collect())
    }

    /// Get the number of active client connections for a user in a room across all replicas.
    ///
    /// This differs from `room_online_user_count_distributed`: it counts every
    /// client connection for the specific user, not distinct users.
    pub async fn user_connection_count_in_room_distributed(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
    ) -> Result<usize, String> {
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let conn_ids = self.get_user_connections_distributed(user_id).await?;
            if conn_ids.is_empty() {
                return Ok(0);
            }

            let metadata_keys: Vec<String> = conn_ids
                .iter()
                .map(|conn_id| format!("{}conn_mgr:conn:{conn_id}", self.redis_key_prefix))
                .collect();

            let metadata: Vec<Option<String>> = conn
                .mget(metadata_keys)
                .await
                .map_err(|e| format!("Failed to fetch distributed connection metadata: {e}"))?;

            let mut count = 0usize;
            for entry in metadata.into_iter().flatten() {
                let info: ConnectionInfoPersistent = serde_json::from_str(&entry).map_err(|e| {
                    format!("Failed to deserialize distributed connection metadata: {e}")
                })?;
                if info.user_id == user_id.as_str()
                    && info.room_id.as_deref() == Some(room_id.as_str())
                {
                    count += 1;
                }
            }
            return Ok(count);
        }

        Ok(self
            .get_user_connections(user_id)
            .into_iter()
            .filter(|conn| conn.room_id.as_ref() == Some(room_id))
            .count())
    }

    /// Returns true if the user still has another active connection in the same room,
    /// potentially on another replica.
    ///
    /// In Redis-backed cluster mode this reads connection metadata from Redis so the
    /// answer reflects all replicas. When Redis is not configured, it falls back to
    /// local in-memory state.
    pub async fn has_other_connection_for_user_in_room_distributed(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        excluding_connection_id: &str,
    ) -> Result<bool, String> {
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let conn_ids = self.get_user_connections_distributed(user_id).await?;
            let other_conn_ids: Vec<String> = conn_ids
                .into_iter()
                .filter(|conn_id| conn_id != excluding_connection_id)
                .collect();

            if other_conn_ids.is_empty() {
                return Ok(false);
            }

            let metadata_keys: Vec<String> = other_conn_ids
                .iter()
                .map(|conn_id| format!("{}conn_mgr:conn:{conn_id}", self.redis_key_prefix))
                .collect();

            let metadata: Vec<Option<String>> = conn
                .mget(metadata_keys)
                .await
                .map_err(|e| format!("Failed to fetch distributed connection metadata: {e}"))?;

            for entry in metadata.into_iter().flatten() {
                match serde_json::from_str::<ConnectionInfoPersistent>(&entry) {
                    Ok(info) => {
                        if info.room_id.as_deref() == Some(room_id.as_str()) {
                            return Ok(true);
                        }
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            user_id = %user_id.as_str(),
                            room_id = %room_id.as_str(),
                            "Failed to deserialize distributed connection metadata"
                        );
                    }
                }
            }

            return Ok(false);
        }

        Ok(self.get_user_connections(user_id).into_iter().any(|conn| {
            conn.connection_id != excluding_connection_id && conn.room_id.as_ref() == Some(room_id)
        }))
    }

    /// Returns true if the user already has at least one active connection in the same room,
    /// excluding the provided connection id.
    pub async fn has_existing_presence_for_user_in_room_distributed(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        excluding_connection_id: &str,
    ) -> Result<bool, String> {
        self.has_other_connection_for_user_in_room_distributed(
            user_id,
            room_id,
            excluding_connection_id,
        )
        .await
    }

    /// Atomically increment a Redis counter, set its TTL, and check if the new
    /// value exceeds the limit.
    ///
    /// Uses a Lua script to make INCR + EXPIRE atomic, preventing a crash between
    /// the two operations from leaving a key without a TTL.
    ///
    /// Returns `Ok(true)` if the counter was incremented and is within the limit,
    /// `Ok(false)` if the limit was exceeded (counter was still incremented and must be rolled back),
    /// or `Err` on Redis failure.
    async fn redis_incr_and_check(&self, key: &str, max: usize) -> Result<bool, String> {
        let Some(mut conn) = self.redis_conn_snapshot().await else {
            return Err("Redis not configured".to_string());
        };

        // Lua script: atomically INCR the key and set TTL in a single round-trip.
        // Returns the new counter value after increment.
        let script = redis::Script::new(
            "local count = redis.call('INCR', KEYS[1]) \
             redis.call('EXPIRE', KEYS[1], ARGV[1]) \
             return count",
        );
        let count: i64 = script
            .key(key)
            .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| format!("Redis INCR+EXPIRE script failed: {e}"))?;

        Ok(count <= max as i64)
    }

    /// Decrement a Redis counter atomically (best-effort, errors are logged but not propagated).
    ///
    /// Uses a Lua script to atomically DECR and DEL if the result is negative,
    /// avoiding a race where a concurrent INCR between DECR and SET(0) would be lost.
    async fn redis_decr(&self, key: &str) -> Result<(), String> {
        let Some(mut conn) = self.redis_conn_snapshot().await else {
            return Err("Redis not configured".to_string());
        };
        let script = redis::Script::new(
            r"local v = redis.call('DECR', KEYS[1])
              if v < 0 then
                redis.call('DEL', KEYS[1])
              end
              return v",
        );
        script
            .key(key)
            .invoke_async::<i64>(&mut conn)
            .await
            .map_err(|e| format!("Redis atomic DECR script failed: {e}"))?;
        Ok(())
    }

    /// Test-only accessor for `refresh_distributed_counter_ttls`.
    ///
    /// **WARNING**: This method is for internal testing only. Do not use in production code.
    /// It exposes the internal TTL refresh mechanism for integration tests that verify
    /// the distributed counter TTL refresh behavior.
    #[doc(hidden)]
    pub async fn test_refresh_distributed_counter_ttls(&self) {
        self.refresh_distributed_counter_ttls().await;
    }
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new(ConnectionLimits::default())
    }
}

/// Connection metrics
#[derive(Debug, Clone)]
pub struct ConnectionMetrics {
    pub active_connections: usize,
    pub total_connections_ever: u64,
    pub total_messages: u64,
    pub active_users: usize,
    pub active_rooms: usize,
}

/// Disconnect signal reliability metrics
#[derive(Debug, Clone)]
pub struct DisconnectSignalMetrics {
    /// Number of disconnect signals currently pending retry
    pub pending_count: usize,
    /// Total number of disconnect signals dropped due to queue overflow or timeout
    pub dropped_count: u64,
    /// Total number of disconnect signals successfully retried
    pub retried_count: u64,
}

/// Outcome of awaiting a single background task during shutdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutdownTaskOutcome {
    Completed,
    Cancelled,
    TimedOut,
    Failed(String),
}

/// Aggregated outcomes for all `ConnectionManager` background tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    pub ttl_refresh: Option<ShutdownTaskOutcome>,
    pub pending_retries: Option<ShutdownTaskOutcome>,
    pub disconnect_retry: Option<ShutdownTaskOutcome>,
}

impl ShutdownReport {
    const fn new() -> Self {
        Self {
            ttl_refresh: None,
            pending_retries: None,
            disconnect_retry: None,
        }
    }

    const fn all_clean(&self) -> bool {
        matches!(
            (
                self.ttl_refresh.as_ref(),
                self.pending_retries.as_ref(),
                self.disconnect_retry.as_ref(),
            ),
            (
                None | Some(ShutdownTaskOutcome::Completed) | Some(ShutdownTaskOutcome::Cancelled),
                None | Some(ShutdownTaskOutcome::Completed) | Some(ShutdownTaskOutcome::Cancelled),
                None | Some(ShutdownTaskOutcome::Completed) | Some(ShutdownTaskOutcome::Cancelled),
            )
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::redis::Redis;

    impl ConnectionManager {
        fn drain_pending_retries_for_test(&self) -> Vec<PendingRedisOp> {
            let mut guard = self
                .pending_retries_rx
                .try_lock()
                .expect("pending retries receiver should be lockable in tests");
            let rx = guard
                .as_mut()
                .expect("pending retries receiver is only available before with_redis()");

            let mut ops = Vec::new();
            while let Ok(op) = rx.try_recv() {
                ops.push(op);
            }
            ops
        }

        fn enqueue_pending_retry_for_test(&self, op: PendingRedisOp) {
            self.pending_retries_tx
                .try_send(op)
                .expect("test should enqueue pending retry");
        }

        fn test_set_disconnect_retry_handle(&self, handle: tokio::task::JoinHandle<()>) {
            *self
                .disconnect_retry_handle
                .lock()
                .expect("disconnect retry handle mutex poisoned") = Some(handle);
            self.disconnect_retry_started.store(true, Ordering::Release);
        }

        fn test_set_ttl_refresh_handle(&self, handle: tokio::task::JoinHandle<()>) {
            *self
                .ttl_refresh_handle
                .lock()
                .expect("ttl refresh handle mutex poisoned") = Some(handle);
        }
    }

    #[tokio::test]
    async fn test_register_connection() {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("user1".to_string());

        let result = manager.register("conn1".to_string(), user_id.clone()).await;
        assert!(result.is_ok());
        assert_eq!(manager.connection_count(), 1);
        assert_eq!(manager.user_connection_count(&user_id), 1);
    }

    #[tokio::test]
    async fn test_register_duplicate_connection_id_is_rejected_without_double_counting() {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("dup-user".to_string());

        manager
            .register("dup-conn".to_string(), user_id.clone())
            .await
            .expect("first register should succeed");

        let duplicate = manager
            .register("dup-conn".to_string(), user_id.clone())
            .await;
        assert!(
            duplicate.is_err(),
            "duplicate connection_id must be rejected deterministically"
        );
        assert!(
            duplicate.unwrap_err().contains("already registered"),
            "duplicate register should report an already-registered error"
        );

        assert_eq!(manager.connection_count(), 1);
        assert_eq!(manager.user_connection_count(&user_id), 1);

        let conn = manager
            .get_connection("dup-conn")
            .expect("original connection should remain intact");
        assert_eq!(conn.user_id, user_id);
    }

    #[tokio::test]
    async fn test_connection_id_claim_rejects_concurrent_duplicate_attempts() {
        let manager = Arc::new(ConnectionManager::default());
        let claimed = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());

        let first = {
            let manager = Arc::clone(&manager);
            let claimed = Arc::clone(&claimed);
            let release = Arc::clone(&release);
            tokio::spawn(async move {
                let claim = manager
                    .try_claim_connection_id("local-claim-race")
                    .expect("first claim should succeed");
                claimed.notify_one();
                release.notified().await;
                drop(claim);
            })
        };

        claimed.notified().await;

        let duplicate = manager.try_claim_connection_id("local-claim-race");
        assert!(
            duplicate.is_err(),
            "concurrent duplicate claim must fail while the first registration is in flight"
        );

        release.notify_one();
        first.await.expect("first claim task");

        let retry = manager.try_claim_connection_id("local-claim-race");
        assert!(
            retry.is_ok(),
            "connection_id claim should be released after the in-flight registration finishes"
        );
    }

    #[tokio::test]
    async fn test_failed_rollback_enqueues_retry_operation() {
        let manager = ConnectionManager::default();

        manager
            .rollback_distributed_counter("rollback:test:key".to_string())
            .await;

        assert_eq!(
            manager.drain_pending_retries_for_test(),
            vec![PendingRedisOp::Decr("rollback:test:key".to_string())],
            "failed rollback must enqueue a retry instead of silently dropping the counter repair"
        );
    }

    #[test]
    fn test_pending_retry_queue_preserves_metadata_cleanup_operations() {
        let manager = ConnectionManager::default();

        manager.enqueue_pending_retry_for_test(PendingRedisOp::Del(
            "retry:test:conn_meta".to_string(),
        ));
        manager.enqueue_pending_retry_for_test(PendingRedisOp::SRem {
            key: "retry:test:user_index".to_string(),
            member: "conn-123".to_string(),
        });
        manager.enqueue_pending_retry_for_test(PendingRedisOp::SRem {
            key: "retry:test:room_index".to_string(),
            member: "conn-123".to_string(),
        });

        assert_eq!(
            manager.drain_pending_retries_for_test(),
            vec![
                PendingRedisOp::Del("retry:test:conn_meta".to_string()),
                PendingRedisOp::SRem {
                    key: "retry:test:user_index".to_string(),
                    member: "conn-123".to_string(),
                },
                PendingRedisOp::SRem {
                    key: "retry:test:room_index".to_string(),
                    member: "conn-123".to_string(),
                },
            ],
            "metadata and index cleanup retries must be retained alongside counter repairs"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker Redis"]
    async fn test_register_same_connection_id_concurrently_with_redis_rejects_one_attempt() {
        use redis::AsyncCommands;

        let (_container, client, conn, prefix) = docker_redis_connection("dup-race:").await;
        let manager =
            Arc::new(ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix));
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let user1 = UserId::from_string("dup-race-user-1".to_string());
        let user2 = UserId::from_string("dup-race-user-2".to_string());

        let task1 = {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            let user1 = user1.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                manager.register("dup-race-conn".to_string(), user1).await
            })
        };
        let task2 = {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            let user2 = user2.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                manager.register("dup-race-conn".to_string(), user2).await
            })
        };

        barrier.wait().await;

        let result1 = task1.await.expect("task1 join");
        let result2 = task2.await.expect("task2 join");
        let success_count = usize::from(result1.is_ok()) + usize::from(result2.is_ok());

        assert_eq!(
            success_count, 1,
            "only one concurrent register should succeed for the same connection_id"
        );
        assert_eq!(
            manager.connection_count(),
            1,
            "duplicate concurrent register must not double-count local connections"
        );
        assert_eq!(
            manager.user_connection_count(&user1) + manager.user_connection_count(&user2),
            1,
            "duplicate concurrent register must not corrupt per-user indexes"
        );

        let registered = manager
            .get_connection("dup-race-conn")
            .expect("winning registration should remain present");
        assert!(
            registered.user_id == user1 || registered.user_id == user2,
            "the surviving connection must belong to exactly one of the contenders"
        );

        let mut redis_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("redis verification connection");
        let total_count: i64 = redis_conn
            .get(format!("{prefix}connections:total"))
            .await
            .unwrap_or(0);
        assert_eq!(
            total_count, 1,
            "duplicate concurrent register must not over-increment distributed total count"
        );

        manager.unregister("dup-race-conn").await;
    }

    #[tokio::test]
    async fn test_per_user_limit() {
        let limits = ConnectionLimits {
            max_per_user: 2,
            ..Default::default()
        };
        let manager = ConnectionManager::new(limits);
        let user_id = UserId::from_string("user1".to_string());

        // First two should succeed
        assert!(manager
            .register("conn1".to_string(), user_id.clone())
            .await
            .is_ok());
        assert!(manager
            .register("conn2".to_string(), user_id.clone())
            .await
            .is_ok());

        // Third should fail
        let result = manager.register("conn3".to_string(), user_id.clone()).await;
        assert!(result.is_err());
        assert_eq!(manager.connection_count(), 2);
    }

    #[tokio::test]
    async fn test_per_user_limit_holds_under_concurrent_registers_without_redis() {
        let limits = ConnectionLimits {
            max_per_user: 1,
            max_total: 10,
            ..Default::default()
        };
        let manager = Arc::new(ConnectionManager::new(limits));
        let user_id = UserId::from_string("race-user".to_string());
        let barrier = Arc::new(tokio::sync::Barrier::new(3));

        let task1 = {
            let manager = Arc::clone(&manager);
            let user_id = user_id.clone();
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                manager.register("conn-race-1".to_string(), user_id).await
            })
        };
        let task2 = {
            let manager = Arc::clone(&manager);
            let user_id = user_id.clone();
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                manager.register("conn-race-2".to_string(), user_id).await
            })
        };

        barrier.wait().await;

        let result1 = task1.await.expect("task1 join");
        let result2 = task2.await.expect("task2 join");
        let success_count = usize::from(result1.is_ok()) + usize::from(result2.is_ok());

        assert_eq!(
            success_count, 1,
            "only one concurrent register should succeed when max_per_user=1"
        );
        assert_eq!(
            manager.user_connection_count(&user_id),
            1,
            "local user index must not oversubscribe the per-user limit"
        );
        assert_eq!(manager.connection_count(), 1);
    }

    #[tokio::test]
    async fn test_join_room() {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("user1".to_string());
        let room_id = RoomId::from_string("room1".to_string());

        manager
            .register("conn1".to_string(), user_id.clone())
            .await
            .unwrap();

        let result = manager.join_room("conn1", room_id.clone()).await;
        assert!(result.is_ok());
        assert_eq!(manager.room_connection_count(&room_id), 1);

        let conn = manager.get_connection("conn1").unwrap();
        assert_eq!(conn.room_id.as_ref().unwrap().as_str(), "room1");
    }

    #[tokio::test]
    async fn test_has_other_connection_for_user_in_room_distributed_uses_local_state_without_redis()
    {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("user1".to_string());
        let room_id = RoomId::from_string("room1".to_string());

        manager
            .register("conn1".to_string(), user_id.clone())
            .await
            .unwrap();
        manager
            .register("conn2".to_string(), user_id.clone())
            .await
            .unwrap();
        manager.join_room("conn1", room_id.clone()).await.unwrap();
        manager.join_room("conn2", room_id.clone()).await.unwrap();

        let has_other = manager
            .has_other_connection_for_user_in_room_distributed(&user_id, &room_id, "conn1")
            .await
            .unwrap();

        assert!(has_other, "second local room connection should be detected");
    }

    #[tokio::test]
    async fn test_has_other_connection_for_user_in_room_distributed_ignores_other_rooms() {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("user1".to_string());
        let room_id = RoomId::from_string("room1".to_string());
        let other_room_id = RoomId::from_string("room2".to_string());

        manager
            .register("conn1".to_string(), user_id.clone())
            .await
            .unwrap();
        manager
            .register("conn2".to_string(), user_id.clone())
            .await
            .unwrap();
        manager.join_room("conn1", room_id.clone()).await.unwrap();
        manager.join_room("conn2", other_room_id).await.unwrap();

        let has_other = manager
            .has_other_connection_for_user_in_room_distributed(&user_id, &room_id, "conn1")
            .await
            .unwrap();

        assert!(
            !has_other,
            "connection in another room must not keep room presence alive"
        );
    }

    #[tokio::test]
    async fn test_has_existing_presence_for_user_in_room_distributed_uses_same_logic() {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("user1".to_string());
        let room_id = RoomId::from_string("room1".to_string());

        manager
            .register("conn1".to_string(), user_id.clone())
            .await
            .unwrap();
        manager
            .register("conn2".to_string(), user_id.clone())
            .await
            .unwrap();
        manager.join_room("conn1", room_id.clone()).await.unwrap();
        manager.join_room("conn2", room_id.clone()).await.unwrap();

        let has_existing_presence = manager
            .has_existing_presence_for_user_in_room_distributed(&user_id, &room_id, "conn2")
            .await
            .unwrap();

        assert!(
            has_existing_presence,
            "existing same-user room presence should be detected before broadcasting UserJoined"
        );
    }

    #[tokio::test]
    async fn test_per_room_limit() {
        let limits = ConnectionLimits {
            max_per_room: 2,
            ..Default::default()
        };
        let manager = ConnectionManager::new(limits);
        let room_id = RoomId::from_string("room1".to_string());

        // Register two connections and join room
        let user1 = UserId::from_string("user1".to_string());
        let user2 = UserId::from_string("user2".to_string());
        let user3 = UserId::from_string("user3".to_string());

        manager.register("conn1".to_string(), user1).await.unwrap();
        manager.register("conn2".to_string(), user2).await.unwrap();
        manager.register("conn3".to_string(), user3).await.unwrap();

        assert!(manager.join_room("conn1", room_id.clone()).await.is_ok());
        assert!(manager.join_room("conn2", room_id.clone()).await.is_ok());

        // Third should fail
        let result = manager.join_room("conn3", room_id.clone()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_per_room_limit_holds_under_concurrent_join_without_redis() {
        let limits = ConnectionLimits {
            max_per_room: 1,
            max_total: 10,
            max_per_user: 10,
            ..Default::default()
        };
        let manager = Arc::new(ConnectionManager::new(limits));
        let room_id = RoomId::from_string("race-room".to_string());

        manager
            .register(
                "conn-room-race-1".to_string(),
                UserId::from_string("user-room-race-1".to_string()),
            )
            .await
            .expect("first registration");
        manager
            .register(
                "conn-room-race-2".to_string(),
                UserId::from_string("user-room-race-2".to_string()),
            )
            .await
            .expect("second registration");

        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let join1 = {
            let manager = Arc::clone(&manager);
            let room_id = room_id.clone();
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                manager.join_room("conn-room-race-1", room_id).await
            })
        };
        let join2 = {
            let manager = Arc::clone(&manager);
            let room_id = room_id.clone();
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                manager.join_room("conn-room-race-2", room_id).await
            })
        };

        barrier.wait().await;

        let result1 = join1.await.expect("join1 task");
        let result2 = join2.await.expect("join2 task");
        let success_count = usize::from(result1.is_ok()) + usize::from(result2.is_ok());

        assert_eq!(
            success_count, 1,
            "only one concurrent room join should succeed when max_per_room=1"
        );
        assert_eq!(manager.room_connection_count(&room_id), 1);
    }

    #[tokio::test]
    async fn test_record_message() {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("user1".to_string());

        manager
            .register("conn1".to_string(), user_id)
            .await
            .unwrap();

        manager.record_message("conn1");
        manager.record_message("conn1");

        let conn = manager.get_connection("conn1").unwrap();
        assert_eq!(conn.message_count, 2);
        assert_eq!(manager.total_messages(), 2);
    }

    #[tokio::test]
    async fn test_get_user_connections_distributed_without_redis_uses_local_state() {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("user1".to_string());

        manager
            .register("conn1".to_string(), user_id.clone())
            .await
            .unwrap();

        let conn_ids = manager
            .get_user_connections_distributed(&user_id)
            .await
            .expect("standalone mode should read local state");

        assert_eq!(conn_ids, vec!["conn1".to_string()]);
    }

    #[tokio::test]
    async fn test_user_connection_count_distributed_without_redis_uses_local_state() {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("user-count".to_string());

        manager
            .register("user-count-1".to_string(), user_id.clone())
            .await
            .unwrap();
        manager
            .register("user-count-2".to_string(), user_id.clone())
            .await
            .unwrap();

        let count = manager
            .user_connection_count_distributed(&user_id)
            .await
            .expect("standalone mode should use local user connection count");
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_room_connection_count_distributed_without_redis_uses_local_state() {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("user1".to_string());
        let room_id = RoomId::from_string("room1".to_string());

        manager
            .register("conn1".to_string(), user_id)
            .await
            .unwrap();
        manager.join_room("conn1", room_id.clone()).await.unwrap();

        let count = manager
            .room_connection_count_distributed(&room_id)
            .await
            .expect("standalone mode should read local room count");
        assert_eq!(count, 1);

        let counts = manager
            .room_connection_count_distributed_batch(&[&room_id])
            .await
            .expect("standalone mode should read local room counts");
        assert_eq!(counts, vec![1]);
    }

    #[tokio::test]
    async fn test_room_online_user_count_deduplicates_same_user_connections() {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("user1".to_string());
        let room_id = RoomId::from_string("room1".to_string());

        manager
            .register("conn1".to_string(), user_id.clone())
            .await
            .unwrap();
        manager
            .register("conn2".to_string(), user_id.clone())
            .await
            .unwrap();
        manager.join_room("conn1", room_id.clone()).await.unwrap();
        manager.join_room("conn2", room_id.clone()).await.unwrap();

        assert_eq!(manager.room_connection_count(&room_id), 2);
        assert_eq!(manager.room_online_user_count(&room_id), 1);

        let distributed = manager
            .room_online_user_count_distributed(&room_id)
            .await
            .expect("standalone mode should use local distinct user count");
        assert_eq!(distributed, 1);

        let batch = manager
            .room_online_user_count_distributed_batch(&[&room_id])
            .await
            .expect("standalone mode should use local batch distinct user counts");
        assert_eq!(batch, vec![1]);
    }

    #[tokio::test]
    async fn test_user_connection_count_in_room_distributed_counts_all_connections() {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("user-room-count".to_string());
        let room_id = RoomId::from_string("room-conn-count".to_string());
        let other_room_id = RoomId::from_string("other-room".to_string());

        manager
            .register("room-count-1".to_string(), user_id.clone())
            .await
            .unwrap();
        manager
            .register("room-count-2".to_string(), user_id.clone())
            .await
            .unwrap();
        manager
            .register("room-count-3".to_string(), user_id.clone())
            .await
            .unwrap();
        manager
            .join_room("room-count-1", room_id.clone())
            .await
            .unwrap();
        manager
            .join_room("room-count-2", room_id.clone())
            .await
            .unwrap();
        manager
            .join_room("room-count-3", other_room_id.clone())
            .await
            .unwrap();

        let count = manager
            .user_connection_count_in_room_distributed(&user_id, &room_id)
            .await
            .expect("standalone mode should use local per-room connection count");
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_unregister() {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("user1".to_string());
        let room_id = RoomId::from_string("room1".to_string());

        manager
            .register("conn1".to_string(), user_id.clone())
            .await
            .unwrap();
        manager.join_room("conn1", room_id.clone()).await.unwrap();

        assert_eq!(manager.connection_count(), 1);
        assert_eq!(manager.user_connection_count(&user_id), 1);
        assert_eq!(manager.room_connection_count(&room_id), 1);

        manager.unregister("conn1").await;

        assert_eq!(manager.connection_count(), 0);
        assert_eq!(manager.user_connection_count(&user_id), 0);
        assert_eq!(manager.room_connection_count(&room_id), 0);
    }

    #[tokio::test]
    async fn test_users_online_metric_deduplicates_multiple_connections_per_user() {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("metric_user".to_string());
        let baseline = synctv_core::metrics::http::USERS_ONLINE.get();

        manager
            .register("metric-conn-1".to_string(), user_id.clone())
            .await
            .unwrap();
        assert_eq!(
            synctv_core::metrics::http::USERS_ONLINE.get(),
            baseline + 1,
            "first connection for a user should increase online user count"
        );

        manager
            .register("metric-conn-2".to_string(), user_id.clone())
            .await
            .unwrap();
        assert_eq!(
            synctv_core::metrics::http::USERS_ONLINE.get(),
            baseline + 1,
            "second connection for the same user must not double-count online users"
        );

        manager.unregister("metric-conn-1").await;
        manager.unregister("metric-conn-2").await;
        assert_eq!(synctv_core::metrics::http::USERS_ONLINE.get(), baseline);
    }

    #[tokio::test]
    async fn test_users_online_metric_decrements_only_after_last_connection_leaves() {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("metric_user_last".to_string());
        let baseline = synctv_core::metrics::http::USERS_ONLINE.get();

        manager
            .register("metric-last-1".to_string(), user_id.clone())
            .await
            .unwrap();
        manager
            .register("metric-last-2".to_string(), user_id.clone())
            .await
            .unwrap();

        manager.unregister("metric-last-1").await;
        assert_eq!(
            synctv_core::metrics::http::USERS_ONLINE.get(),
            baseline + 1,
            "user should remain online while another connection is still active"
        );

        manager.unregister("metric-last-2").await;
        assert_eq!(
            synctv_core::metrics::http::USERS_ONLINE.get(),
            baseline,
            "online user count should drop only after the final connection closes"
        );
    }

    #[tokio::test]
    async fn test_metrics() {
        let manager = ConnectionManager::default();
        let user1 = UserId::from_string("user1".to_string());
        let user2 = UserId::from_string("user2".to_string());

        manager.register("conn1".to_string(), user1).await.unwrap();
        manager.register("conn2".to_string(), user2).await.unwrap();

        manager.record_message("conn1");
        manager.record_message("conn2");

        let metrics = manager.metrics();
        assert_eq!(metrics.active_connections, 2);
        assert_eq!(metrics.total_connections_ever, 2);
        assert_eq!(metrics.total_messages, 2);
        assert_eq!(metrics.active_users, 2);
    }

    #[tokio::test]
    async fn test_idle_timeout() {
        let limits = ConnectionLimits {
            idle_timeout: Duration::from_millis(100),
            ..Default::default()
        };
        let manager = ConnectionManager::new(limits);
        let user_id = UserId::from_string("user1".to_string());

        manager
            .register("conn1".to_string(), user_id)
            .await
            .unwrap();

        // Wait for idle timeout
        tokio::time::sleep(Duration::from_millis(150)).await;

        let timeouts = manager.check_timeouts();
        assert_eq!(timeouts.len(), 1);
        assert_eq!(timeouts[0], "conn1");
    }

    // ========== Redis Reconciliation Tests ==========

    #[tokio::test]
    #[ignore = "Requires Docker Redis"]
    async fn test_redis_recovery_reconciles_connection_counts() {
        // This test verifies that after a Redis outage, the ConnectionManager
        // reconciles in-memory connection counts with Redis.

        // Setup: Create manager with Redis
        use redis::AsyncCommands;

        let (_container, client, conn, prefix) = docker_redis_connection("test:").await;
        let manager = ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix);

        let user_id = UserId::from_string("user1".to_string());
        let room_id = RoomId::from_string("room1".to_string());

        // Register connections
        manager
            .register("conn1".to_string(), user_id.clone())
            .await
            .unwrap();
        manager.join_room("conn1", room_id.clone()).await.unwrap();

        // Verify Redis has the counts
        let mut redis_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();
        let user_count: i64 = redis_conn
            .get(format!("{prefix}connections:user:user1"))
            .await
            .unwrap_or(0);
        assert_eq!(user_count, 1);

        // Simulate Redis outage by clearing Redis keys manually
        // (In real scenario, Redis would be down)
        let _: () = redis_conn
            .del(format!("{prefix}connections:user:user1"))
            .await
            .unwrap();
        let _: () = redis_conn
            .del(format!("{prefix}connections:room:room1"))
            .await
            .unwrap();

        // At this point, local state has 1 connection but Redis has 0
        assert_eq!(manager.user_connection_count(&user_id), 1);

        // Trigger reconciliation
        manager.reconcile_with_redis().await;

        // After reconciliation, Redis should match local state
        let user_count: i64 = redis_conn
            .get(format!("{prefix}connections:user:user1"))
            .await
            .unwrap_or(0);
        assert_eq!(user_count, 1);

        // Cleanup
        manager.unregister("conn1").await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker Redis"]
    async fn test_redis_recovery_reconciles_stale_connections() {
        // This test verifies that stale Redis index members are cleaned up
        // during reconciliation without deleting unrelated metadata keys.

        use redis::AsyncCommands;

        let (_container, client, conn, prefix) = docker_redis_connection("test2:").await;
        let manager = ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix);

        // Manually inject a stale connection id into the distributed user/room indexes
        // without creating a matching conn_mgr:conn:* metadata key.
        let mut redis_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();
        let stale_user_index = format!("{prefix}conn_mgr:user:user_stale");
        let stale_room_index = format!("{prefix}conn_mgr:room:room_stale");
        let stale_conn_key = format!("{prefix}conn_mgr:conn:stale_conn");
        let unrelated_conn_key = format!("{prefix}conn_mgr:conn:other_node_conn");

        let _: () = redis_conn
            .sadd(&stale_user_index, "stale_conn")
            .await
            .unwrap();
        let _: () = redis_conn
            .sadd(&stale_room_index, "stale_conn")
            .await
            .unwrap();
        let _: () = redis_conn
            .expire(&stale_user_index, CONNECTION_METADATA_TTL_SECONDS)
            .await
            .unwrap();
        let _: () = redis_conn
            .expire(&stale_room_index, CONNECTION_METADATA_TTL_SECONDS)
            .await
            .unwrap();

        // Also create a metadata key that belongs to another replica. Reconciliation
        // on this node must not delete it just because it is absent from local memory.
        let foreign_meta = ConnectionInfoPersistent {
            connection_id: "other_node_conn".to_string(),
            user_id: "user_foreign".to_string(),
            room_id: Some("room_foreign".to_string()),
            connected_at_unix: 0,
            last_activity_unix: 0,
            message_count: 0,
            rtc_joined: false,
            rtc_joined_at_unix: None,
        };
        let _: () = redis_conn
            .set(
                &unrelated_conn_key,
                serde_json::to_string(&foreign_meta).unwrap(),
            )
            .await
            .unwrap();

        let stale_user_members: Vec<String> = redis_conn.smembers(&stale_user_index).await.unwrap();
        let stale_room_members: Vec<String> = redis_conn.smembers(&stale_room_index).await.unwrap();
        assert_eq!(stale_user_members, vec!["stale_conn".to_string()]);
        assert_eq!(stale_room_members, vec!["stale_conn".to_string()]);

        // Trigger reconciliation
        manager.reconcile_with_redis().await;

        // Stale index members should be cleaned up since the metadata key is missing.
        let stale_user_exists: bool = redis_conn.exists(&stale_user_index).await.unwrap();
        let stale_room_exists: bool = redis_conn.exists(&stale_room_index).await.unwrap();
        let stale_conn_exists: bool = redis_conn.exists(&stale_conn_key).await.unwrap();
        let unrelated_conn_exists: bool = redis_conn.exists(&unrelated_conn_key).await.unwrap();

        assert!(
            !stale_user_exists,
            "Empty stale user index should be removed during reconciliation"
        );
        assert!(
            !stale_room_exists,
            "Empty stale room index should be removed during reconciliation"
        );
        assert!(
            !stale_conn_exists,
            "Missing metadata key must remain absent"
        );
        assert!(
            unrelated_conn_exists,
            "Reconciliation must not delete connection metadata that may belong to another replica"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker Redis"]
    async fn test_redis_outage_during_register_eventually_consistent() {
        // This test verifies that failed Redis operations during register
        // are eventually reconciled.

        use redis::AsyncCommands;

        let (_container, client, conn, prefix) = docker_redis_connection("test3:").await;
        let manager = ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix);

        let user_id = UserId::from_string("user1".to_string());

        // Register a connection (should succeed and write to Redis)
        manager
            .register("conn1".to_string(), user_id.clone())
            .await
            .unwrap();

        // Verify Redis counter
        let mut redis_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();
        let user_count: i64 = redis_conn
            .get(format!("{prefix}connections:user:user1"))
            .await
            .unwrap_or(0);
        assert_eq!(user_count, 1);

        // Manually corrupt the counter (simulating partial failure)
        let _: () = redis_conn
            .set(format!("{prefix}connections:user:user1"), 0)
            .await
            .unwrap();

        // Local state says 1, Redis says 0
        assert_eq!(manager.user_connection_count(&user_id), 1);

        // Trigger reconciliation
        manager.reconcile_with_redis().await;

        // After reconciliation, Redis should be corrected
        let user_count: i64 = redis_conn
            .get(format!("{prefix}connections:user:user1"))
            .await
            .unwrap_or(0);
        assert_eq!(user_count, 1);

        // Cleanup
        manager.unregister("conn1").await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker Redis"]
    async fn test_reconcile_with_redis_does_not_overwrite_other_replica_counters() {
        use redis::AsyncCommands;

        let (_container, client, conn, prefix) = docker_redis_connection("test5:").await;
        let manager = ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix);

        // Simulate another healthy replica already having active connections.
        let mut redis_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();
        let user_key = format!("{prefix}connections:user:shared-user");
        let room_key = format!("{prefix}connections:room:shared-room");
        let total_key = format!("{prefix}connections:total");

        let _: () = redis_conn.set(&user_key, 3).await.unwrap();
        let _: () = redis_conn
            .expire(&user_key, DISTRIBUTED_COUNTER_TTL_SECONDS)
            .await
            .unwrap();
        let _: () = redis_conn.set(&room_key, 4).await.unwrap();
        let _: () = redis_conn
            .expire(&room_key, DISTRIBUTED_COUNTER_TTL_SECONDS)
            .await
            .unwrap();
        let _: () = redis_conn.set(&total_key, 7).await.unwrap();
        let _: () = redis_conn
            .expire(&total_key, DISTRIBUTED_COUNTER_TTL_SECONDS)
            .await
            .unwrap();

        // This node has no local connections. Reconciliation must not zero out
        // counters that may belong to other replicas.
        manager.reconcile_with_redis().await;

        let user_count: i64 = redis_conn.get(&user_key).await.unwrap_or(0);
        let room_count: i64 = redis_conn.get(&room_key).await.unwrap_or(0);
        let total_count: i64 = redis_conn.get(&total_key).await.unwrap_or(0);

        assert_eq!(
            user_count, 3,
            "reconciliation must preserve user counters that may belong to other replicas"
        );
        assert_eq!(
            room_count, 4,
            "reconciliation must preserve room counters that may belong to other replicas"
        );
        assert_eq!(
            total_count, 7,
            "reconciliation must preserve total counters that may belong to other replicas"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker Redis"]
    async fn test_register_user_limit_rejection_rolls_back_distributed_total_counter() {
        use redis::AsyncCommands;

        let (_container, client, conn, prefix) = docker_redis_connection("test4:").await;

        let limits = ConnectionLimits {
            max_per_user: 1,
            ..ConnectionLimits::default()
        };
        let manager = ConnectionManager::new(limits).with_redis(conn, &prefix);
        let user_id = UserId::from_string("user-total-rollback".to_string());

        manager
            .register("conn1".to_string(), user_id.clone())
            .await
            .unwrap();

        let second = manager.register("conn2".to_string(), user_id.clone()).await;
        assert!(
            second.is_err(),
            "second connection should be rejected by distributed per-user limit"
        );

        let mut redis_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();
        let total_count: i64 = redis_conn
            .get(format!("{prefix}connections:total"))
            .await
            .unwrap_or(0);
        let user_count: i64 = redis_conn
            .get(format!("{prefix}connections:user:user-total-rollback"))
            .await
            .unwrap_or(0);

        assert_eq!(
            total_count, 1,
            "distributed total counter must be rolled back when register is rejected"
        );
        assert_eq!(
            user_count, 1,
            "distributed per-user counter should only reflect the accepted connection"
        );

        manager.unregister("conn1").await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker Redis"]
    async fn test_shared_redis_handle_observes_hot_swapped_connection() {
        use redis::AsyncCommands;

        let (_container, client, conn, prefix) = docker_redis_connection("shared-test:").await;
        let shared_conn = Arc::new(tokio::sync::RwLock::new(conn));
        let manager = ConnectionManager::new(ConnectionLimits::default())
            .with_shared_redis(shared_conn.clone(), &prefix);

        manager
            .register(
                "conn-shared".to_string(),
                UserId::from_string("user-shared".to_string()),
            )
            .await
            .unwrap();
        manager
            .join_room(
                "conn-shared",
                RoomId::from_string("room-shared".to_string()),
            )
            .await
            .unwrap();

        let initial_metadata_key = format!("{prefix}conn_mgr:conn:conn-shared");
        let initial_room_key = format!("{prefix}connections:room:room-shared");
        let mut verify_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();
        let initial_metadata: Option<String> =
            verify_conn.get(&initial_metadata_key).await.unwrap();
        let initial_room_count: i64 = verify_conn.get(&initial_room_key).await.unwrap_or(0);
        assert!(
            initial_metadata.is_some(),
            "initial shared handle should write metadata"
        );
        assert_eq!(
            initial_room_count, 1,
            "initial shared handle should write room counter"
        );

        let replacement_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();
        *shared_conn.write().await = replacement_conn;

        let moved_room = RoomId::from_string("room-shared-2".to_string());
        manager
            .join_room("conn-shared", moved_room.clone())
            .await
            .unwrap();

        let moved_room_key = format!("{prefix}connections:room:{}", moved_room.as_str());
        let old_room_count: i64 = verify_conn.get(&initial_room_key).await.unwrap_or(0);
        let new_room_count: i64 = verify_conn.get(&moved_room_key).await.unwrap_or(0);
        let updated_metadata: String = verify_conn.get(&initial_metadata_key).await.unwrap();
        let updated_info: ConnectionInfoPersistent =
            serde_json::from_str(&updated_metadata).unwrap();

        assert_eq!(
            old_room_count, 0,
            "old room counter should be decremented after move"
        );
        assert_eq!(
            new_room_count, 1,
            "new room counter should be incremented after move"
        );
        assert_eq!(
            updated_info.room_id.as_deref(),
            Some(moved_room.as_str()),
            "post-swap operations must use the replacement shared Redis connection"
        );

        manager.unregister("conn-shared").await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker Redis"]
    async fn test_pending_retries_cleanup_metadata_and_indexes_after_recovery() {
        use redis::AsyncCommands;

        let (_container, client, conn, prefix) =
            docker_redis_connection("shared-unregister:").await;
        let shared_conn = Arc::new(tokio::sync::RwLock::new(conn));
        let manager = ConnectionManager::new(ConnectionLimits::default())
            .with_shared_redis(shared_conn.clone(), &prefix);

        let conn_key = format!("{prefix}conn_mgr:conn:conn-recover");
        let user_index_key = format!("{prefix}conn_mgr:user:user-recover");
        let room_index_key = format!("{prefix}conn_mgr:room:room-recover");

        let mut verify_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();
        let metadata = ConnectionInfoPersistent {
            connection_id: "conn-recover".to_string(),
            user_id: "user-recover".to_string(),
            room_id: Some("room-recover".to_string()),
            connected_at_unix: 0,
            last_activity_unix: 0,
            message_count: 0,
            rtc_joined: false,
            rtc_joined_at_unix: None,
        };
        let _: () = verify_conn
            .set(&conn_key, serde_json::to_string(&metadata).unwrap())
            .await
            .unwrap();
        let _: () = verify_conn
            .sadd(&user_index_key, "conn-recover")
            .await
            .unwrap();
        let _: () = verify_conn
            .sadd(&room_index_key, "conn-recover")
            .await
            .unwrap();

        assert!(
            verify_conn.exists::<_, bool>(&conn_key).await.unwrap(),
            "metadata should exist before retry processing"
        );
        manager.enqueue_pending_retry_for_test(PendingRedisOp::Del(conn_key.clone()));
        manager.enqueue_pending_retry_for_test(PendingRedisOp::SRem {
            key: user_index_key.clone(),
            member: "conn-recover".to_string(),
        });
        manager.enqueue_pending_retry_for_test(PendingRedisOp::SRem {
            key: room_index_key.clone(),
            member: "conn-recover".to_string(),
        });

        tokio::time::sleep(Duration::from_secs(6)).await;

        let metadata_exists: bool = verify_conn.exists(&conn_key).await.unwrap();
        let user_members: Vec<String> = verify_conn.smembers(&user_index_key).await.unwrap();
        let room_members: Vec<String> = verify_conn.smembers(&room_index_key).await.unwrap();

        assert!(
            !metadata_exists,
            "pending retry processing must delete stale connection metadata"
        );
        assert!(
            user_members.is_empty(),
            "pending retry processing must remove stale user index members"
        );
        assert!(
            room_members.is_empty(),
            "pending retry processing must remove stale room index members"
        );

        manager.shutdown().await;
    }

    #[tokio::test]
    async fn test_reconcile_without_redis_is_noop() {
        // Reconciliation should be a no-op when Redis is not configured
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("user1".to_string());

        manager
            .register("conn1".to_string(), user_id.clone())
            .await
            .unwrap();

        // Should not panic or error
        manager.reconcile_with_redis().await;

        assert_eq!(manager.connection_count(), 1);
    }

    #[test]
    fn test_connection_info_persistent_serialization() {
        // Verify that ConnectionInfoPersistent can be serialized/deserialized
        let persistent = ConnectionInfoPersistent {
            connection_id: "conn1".to_string(),
            user_id: "user1".to_string(),
            room_id: Some("room1".to_string()),
            connected_at_unix: 1000,
            last_activity_unix: 2000,
            message_count: 5,
            rtc_joined: true,
            rtc_joined_at_unix: Some(1500),
        };

        let json = serde_json::to_string(&persistent).unwrap();
        let deserialized: ConnectionInfoPersistent = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.connection_id, "conn1");
        assert_eq!(deserialized.user_id, "user1");
        assert_eq!(deserialized.room_id, Some("room1".to_string()));
        assert_eq!(deserialized.message_count, 5);
        assert!(deserialized.rtc_joined);
    }

    #[test]
    fn test_system_time_to_unix_secs_handles_pre_epoch_without_panicking() {
        let pre_epoch = UNIX_EPOCH
            .checked_sub(Duration::from_secs(1))
            .expect("pre-epoch time should be constructible");

        let result = std::panic::catch_unwind(|| system_time_to_unix_secs(pre_epoch));

        assert!(
            result.is_ok(),
            "cluster connection metadata conversion must not panic on clock rollback"
        );
    }

    // ========== Connection Reservation Tests (P1#6) ==========

    #[tokio::test]
    async fn test_reserve_room_slot_enforces_limit() {
        let limits = ConnectionLimits {
            max_per_room: 3,
            ..ConnectionLimits::default()
        };
        let mgr = ConnectionManager::new(limits);
        let rid = RoomId("test_room".to_string());

        assert!(mgr.reserve_room_slot(&rid).is_ok());
        assert!(mgr.reserve_room_slot(&rid).is_ok());
        assert!(mgr.reserve_room_slot(&rid).is_ok());
        assert!(
            mgr.reserve_room_slot(&rid).is_err(),
            "Fourth reservation should fail (limit=3)"
        );
    }

    #[tokio::test]
    async fn test_release_room_reservation_frees_slot() {
        let limits = ConnectionLimits {
            max_per_room: 1,
            ..ConnectionLimits::default()
        };
        let mgr = ConnectionManager::new(limits);
        let rid = RoomId("test_room".to_string());

        assert!(mgr.reserve_room_slot(&rid).is_ok());
        assert!(mgr.reserve_room_slot(&rid).is_err());

        mgr.release_room_reservation(&rid);
        assert!(
            mgr.reserve_room_slot(&rid).is_ok(),
            "Should succeed after releasing reservation"
        );
    }

    #[tokio::test]
    async fn test_reserve_user_slot_enforces_limit() {
        let limits = ConnectionLimits {
            max_per_user: 2,
            ..ConnectionLimits::default()
        };
        let mgr = ConnectionManager::new(limits);
        let uid = UserId("test_user".to_string());

        assert!(mgr.reserve_user_slot(&uid).is_ok());
        assert!(mgr.reserve_user_slot(&uid).is_ok());
        assert!(
            mgr.reserve_user_slot(&uid).is_err(),
            "Third reservation should fail (limit=2)"
        );
    }

    #[tokio::test]
    async fn test_release_user_reservation_frees_slot() {
        let limits = ConnectionLimits {
            max_per_user: 1,
            ..ConnectionLimits::default()
        };
        let mgr = ConnectionManager::new(limits);
        let uid = UserId("test_user".to_string());

        assert!(mgr.reserve_user_slot(&uid).is_ok());
        assert!(mgr.reserve_user_slot(&uid).is_err());

        mgr.release_user_reservation(&uid);
        assert!(
            mgr.reserve_user_slot(&uid).is_ok(),
            "Should succeed after releasing reservation"
        );
    }

    #[tokio::test]
    async fn test_reserve_room_slot_independent_rooms() {
        let limits = ConnectionLimits {
            max_per_room: 1,
            ..ConnectionLimits::default()
        };
        let mgr = ConnectionManager::new(limits);
        let rid1 = RoomId("room_a".to_string());
        let rid2 = RoomId("room_b".to_string());

        assert!(mgr.reserve_room_slot(&rid1).is_ok());
        assert!(
            mgr.reserve_room_slot(&rid2).is_ok(),
            "Different rooms should have independent limits"
        );
        assert!(mgr.reserve_room_slot(&rid1).is_err());
        assert!(mgr.reserve_room_slot(&rid2).is_err());
    }

    #[tokio::test]
    async fn test_reserve_release_idempotent() {
        let limits = ConnectionLimits {
            max_per_room: 2,
            ..ConnectionLimits::default()
        };
        let mgr = ConnectionManager::new(limits);
        let rid = RoomId("test_room".to_string());

        // Release without prior reservation should not panic
        mgr.release_room_reservation(&rid);

        // Normal reserve/release cycle
        assert!(mgr.reserve_room_slot(&rid).is_ok());
        mgr.release_room_reservation(&rid);

        // Should still be able to reserve up to the limit
        assert!(mgr.reserve_room_slot(&rid).is_ok());
        assert!(mgr.reserve_room_slot(&rid).is_ok());
        assert!(mgr.reserve_room_slot(&rid).is_err());
    }

    #[tokio::test]
    async fn test_release_room_reservation_removes_zero_counter_entry() {
        let mgr = ConnectionManager::new(ConnectionLimits::default());
        let rid = RoomId("cleanup_room".to_string());

        assert!(mgr.reserve_room_slot(&rid).is_ok());
        assert_eq!(mgr.pending_room_reservations.len(), 1);

        mgr.release_room_reservation(&rid);

        assert!(
            mgr.pending_room_reservations.get(&rid).is_none(),
            "room reservation entry should be removed after the count returns to zero"
        );
        assert_eq!(mgr.pending_room_reservations.len(), 0);
    }

    #[tokio::test]
    async fn test_release_user_reservation_removes_zero_counter_entry() {
        let mgr = ConnectionManager::new(ConnectionLimits::default());
        let uid = UserId("cleanup_user".to_string());

        assert!(mgr.reserve_user_slot(&uid).is_ok());
        assert_eq!(mgr.pending_user_reservations.len(), 1);

        mgr.release_user_reservation(&uid);

        assert!(
            mgr.pending_user_reservations.get(&uid).is_none(),
            "user reservation entry should be removed after the count returns to zero"
        );
        assert_eq!(mgr.pending_user_reservations.len(), 0);
    }

    #[tokio::test]
    async fn test_new_does_not_spawn_tasks() {
        // ConnectionManager::new() should not call tokio::spawn.
        // It should be safe to call outside of a Tokio runtime (though we
        // run this inside one for convenience). The key invariant is that
        // the disconnect retry task is NOT started until start() is called.
        let manager = ConnectionManager::new(ConnectionLimits::default());
        // Verify manager is functional for basic operations without start()
        let user_id = UserId::from_string("user1".to_string());
        assert!(manager.register("conn1".to_string(), user_id).await.is_ok());
        assert_eq!(manager.connection_count(), 1);
    }

    #[tokio::test]
    async fn test_start_spawns_disconnect_retry_task() {
        let manager = ConnectionManager::new(ConnectionLimits::default());
        // start() should be callable within a Tokio runtime without panicking
        manager.start();
        // Give the spawned task a moment to initialize
        tokio::time::sleep(Duration::from_millis(10)).await;
        // Shutdown should cancel the retry task cleanly
        let report = manager.shutdown().await;
        assert_eq!(
            report.disconnect_retry,
            Some(ShutdownTaskOutcome::Completed),
            "shutdown should await the disconnect retry task to completion"
        );
    }

    #[tokio::test]
    async fn test_start_is_idempotent() {
        let manager = ConnectionManager::new(ConnectionLimits::default());

        assert!(
            !manager.disconnect_retry_task_started(),
            "disconnect retry task should not be started before start()"
        );

        manager.start();
        assert!(
            manager.disconnect_retry_task_started(),
            "start() should mark the disconnect retry task as started"
        );

        manager.start();
        manager.start();

        assert!(
            manager.disconnect_retry_task_started(),
            "duplicate start() calls must be a no-op"
        );

        let report = manager.shutdown().await;
        assert_eq!(
            report.disconnect_retry,
            Some(ShutdownTaskOutcome::Completed),
            "shutdown should report a clean disconnect retry task exit"
        );
    }

    #[tokio::test]
    async fn test_shutdown_awaits_disconnect_retry_task_exit() {
        let manager = ConnectionManager::new(ConnectionLimits::default());
        manager.start();

        tokio::time::sleep(Duration::from_millis(10)).await;

        let report = manager.shutdown().await;

        assert!(
            manager
                .disconnect_retry_handle
                .lock()
                .expect("disconnect retry handle mutex poisoned")
                .is_none(),
            "shutdown must drain the disconnect retry task handle"
        );
        assert_eq!(
            report.disconnect_retry,
            Some(ShutdownTaskOutcome::Completed),
            "shutdown should return the disconnect retry task outcome"
        );
    }

    #[tokio::test]
    async fn test_shutdown_reports_background_task_panic() {
        let manager = ConnectionManager::new(ConnectionLimits::default());
        manager.test_set_disconnect_retry_handle(tokio::spawn(async {
            panic!("disconnect retry panic");
        }));

        let report = manager.shutdown().await;

        match report.disconnect_retry {
            Some(ShutdownTaskOutcome::Failed(message)) => {
                assert!(
                    message.contains("panic"),
                    "panic outcome should surface join error details: {message}"
                );
            }
            other => panic!("expected panic failure outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_shutdown_reports_cancelled_background_task() {
        let manager = ConnectionManager::new(ConnectionLimits::default());
        let handle = tokio::spawn(async {
            futures::future::pending::<()>().await;
        });
        handle.abort();
        manager.test_set_ttl_refresh_handle(handle);

        let report = manager.shutdown().await;

        assert_eq!(
            report.ttl_refresh,
            Some(ShutdownTaskOutcome::Cancelled),
            "aborted background tasks must not be silently swallowed during shutdown"
        );
    }

    async fn docker_redis_connection(
        prefix: &str,
    ) -> (
        testcontainers::ContainerAsync<Redis>,
        redis::Client,
        redis::aio::ConnectionManager,
        String,
    ) {
        let container = Redis::default()
            .start()
            .await
            .expect("Failed to start Redis container");
        let host = container
            .get_host()
            .await
            .expect("Failed to get Redis host");
        let port = container
            .get_host_port_ipv4(6379)
            .await
            .expect("Failed to get Redis port");
        let redis_url = format!("redis://{host}:{port}");
        let client = redis::Client::open(redis_url.as_str()).expect("Failed to open Redis client");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match redis::aio::ConnectionManager::new(client.clone()).await {
                Ok(mut conn) => match redis::cmd("PING").query_async::<String>(&mut conn).await {
                    Ok(_) => {
                        return (container, client, conn, prefix.to_string());
                    }
                    Err(error) => {
                        if tokio::time::Instant::now() >= deadline {
                            panic!("Redis test container did not become ready in time: {error}");
                        }
                    }
                },
                Err(error) => {
                    if tokio::time::Instant::now() >= deadline {
                        panic!("Failed to create Redis ConnectionManager: {error}");
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
