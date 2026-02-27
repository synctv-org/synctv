use dashmap::DashMap;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
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

impl From<&ConnectionInfo> for ConnectionInfoPersistent {
    fn from(info: &ConnectionInfo) -> Self {
        let now = SystemTime::now();
        let connected_at_unix = now.duration_since(UNIX_EPOCH).unwrap().as_secs()
            .saturating_sub(info.connected_at.elapsed().as_secs());
        let last_activity_unix = now.duration_since(UNIX_EPOCH).unwrap().as_secs()
            .saturating_sub(info.last_activity.elapsed().as_secs());
        let rtc_joined_at_unix = info.rtc_joined_at.map(|joined| {
            now.duration_since(UNIX_EPOCH).unwrap().as_secs()
                .saturating_sub(joined.elapsed().as_secs())
        });

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
            idle_timeout: Duration::from_secs(5 * 60), // 5 minutes
            max_duration: Duration::from_secs(24 * 60 * 60), // 24 hours
            webrtc_session_timeout: Duration::from_secs(2 * 60 * 60), // 2 hours
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
#[derive(Debug, Clone)]
enum PendingRedisOp {
    /// Decrement a counter key
    Decr(String),
}

/// Connection manager for tracking active gRPC streaming connections
#[derive(Clone)]
pub struct ConnectionManager {
    /// All active connections by `connection_id`
    connections: Arc<DashMap<String, ConnectionInfo>>,

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

    /// Pending disconnect signals that failed to send (channel full).
    /// These are retried by a background task to ensure reliable delivery.
    pending_disconnects: Arc<DashMap<u64, (DisconnectSignal, Instant)>>,

    /// Counter for generating unique IDs for pending disconnect signals
    pending_disconnect_id: Arc<AtomicU64>,

    /// Counter for tracking dropped disconnect signals (monitoring)
    dropped_disconnect_signals: Arc<AtomicU64>,

    /// Counter for tracking retried disconnect signals (monitoring)
    retried_disconnect_signals: Arc<AtomicU64>,

    /// Optional Redis connection for distributed connection counting.
    /// When present, per-user and per-room limits are enforced across all replicas.
    /// When absent, limits are per-node only (fallback).
    redis_conn: Option<redis::aio::ConnectionManager>,

    /// Key prefix for Redis keys (e.g., "synctv:")
    redis_key_prefix: String,

    /// Cancellation token for the auto-spawned TTL refresh task.
    /// Cancelled on shutdown to stop the background task.
    ttl_refresh_cancel: Arc<tokio_util::sync::CancellationToken>,

    /// Cancellation token for the disconnect signal retry task.
    /// Cancelled on shutdown to stop the background task.
    disconnect_retry_cancel: Arc<tokio_util::sync::CancellationToken>,

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
    /// Create a new `ConnectionManager`
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
        let mgr = Self {
            connections: Arc::new(DashMap::new()),
            user_connections: Arc::new(DashMap::new()),
            room_connections: Arc::new(DashMap::new()),
            limits: Arc::new(limits),
            total_connections: Arc::new(AtomicUsize::new(0)),
            total_connections_ever: Arc::new(AtomicU64::new(0)),
            total_messages: Arc::new(AtomicU64::new(0)),
            disconnect_tx: Arc::new(disconnect_tx),
            pending_disconnects: Arc::new(DashMap::new()),
            pending_disconnect_id: Arc::new(AtomicU64::new(0)),
            dropped_disconnect_signals: Arc::new(AtomicU64::new(0)),
            retried_disconnect_signals: Arc::new(AtomicU64::new(0)),
            redis_conn: None,
            redis_key_prefix: String::new(),
            ttl_refresh_cancel: Arc::new(tokio_util::sync::CancellationToken::new()),
            disconnect_retry_cancel: Arc::new(disconnect_retry_cancel.clone()),
            pending_retries_tx,
            pending_retries_rx: Arc::new(tokio::sync::Mutex::new(Some(pending_retries_rx))),
        };

        // Spawn the disconnect signal retry task
        mgr.spawn_disconnect_retry_task(disconnect_retry_cancel);

        mgr
    }

    /// Enable distributed connection counting via Redis.
    ///
    /// When Redis is configured, per-user and per-room connection limits are
    /// enforced across all replicas. Without Redis, limits are per-node only.
    ///
    /// Automatically spawns a background TTL refresh task (every 60s) to keep
    /// Redis connection counters alive for long-lived connections, and a
    /// pending-retries task that periodically retries failed Redis counter
    /// operations. Both tasks are cancelled when `shutdown()` is called.
    #[must_use]
    pub fn with_redis(mut self, conn: redis::aio::ConnectionManager, key_prefix: &str) -> Self {
        self.redis_conn = Some(conn.clone());
        self.redis_key_prefix = key_prefix.to_string();

        // Auto-spawn the TTL refresh task so callers don't need to remember
        // to call spawn_ttl_refresh_task() manually.
        let cancel = tokio_util::sync::CancellationToken::new();
        self.ttl_refresh_cancel = Arc::new(cancel.clone());
        let _handle = self.spawn_ttl_refresh_task(Duration::from_secs(60), cancel.clone());

        // Spawn the pending-retries background task.
        // Take the receiver that was stored in new() so it is not dropped.
        // If for any reason it was already taken (e.g. with_redis called twice),
        // fall back to creating a fresh channel.
        let rx = self.pending_retries_rx
            .try_lock()
            .ok()
            .and_then(|mut guard| guard.take());
        let rx = match rx {
            Some(rx) => rx,
            None => {
                // Fallback: create a fresh channel and update the sender.
                let (tx, rx) = mpsc::channel(PENDING_RETRY_QUEUE_CAPACITY);
                self.pending_retries_tx = tx;
                rx
            }
        };
        Self::spawn_pending_retries_task(conn, rx, cancel);

        self
    }

    /// Spawn a background task that retries failed Redis counter operations.
    ///
    /// Drains the `pending_retries_rx` channel every 5 seconds and retries each
    /// operation. Operations that still fail are re-queued (up to 3 attempts each,
    /// tracked internally) before being dropped with a warning.
    fn spawn_pending_retries_task(
        redis_conn: redis::aio::ConnectionManager,
        mut rx: mpsc::Receiver<PendingRedisOp>,
        cancel: tokio_util::sync::CancellationToken,
    ) {
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
                        let mut conn = redis_conn.clone();

                        for (op, attempts) in pending.drain(..) {
                            let result = match &op {
                                PendingRedisOp::Decr(key) => {
                                    // Use raw DECR; don't need the atomic script here since
                                    // this is a compensating retry, not a live operation.
                                    conn.decr::<_, _, i64>(key, 1i64).await
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
        });
    }

    /// Enqueue a failed Redis counter operation for background retry.
    fn enqueue_retry(&self, op: PendingRedisOp) {
        if let Err(e) = self.pending_retries_tx.try_send(op) {
            warn!("Failed to enqueue pending Redis retry (channel full or closed): {e}");
        }
    }

    /// Spawn a background task that retries pending disconnect signals.
    ///
    /// This task periodically checks for disconnect signals that failed to send
    /// (because the broadcast channel was full) and retries them. This ensures
    /// that kick/ban operations are not lost even under high load.
    fn spawn_disconnect_retry_task(&self, cancel: tokio_util::sync::CancellationToken) {
        let pending_disconnects = self.pending_disconnects.clone();
        let disconnect_tx = self.disconnect_tx.clone();
        let dropped_count = self.dropped_disconnect_signals.clone();
        let retried_count = self.retried_disconnect_signals.clone();

        tokio::spawn(async move {
            /// Interval between retry sweeps for pending disconnect signals.
            const RETRY_INTERVAL: Duration = Duration::from_millis(100);
            /// Maximum age of a pending disconnect signal before it's dropped (5 seconds).
            const MAX_SIGNAL_AGE: Duration = Duration::from_secs(5);
            /// Maximum retry attempts per signal.
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
                            match disconnect_tx.send(signal.clone()) {
                                Ok(_) => {
                                    to_remove.push(id);
                                    retry_count += 1;
                                    debug!(
                                        signal = ?signal,
                                        age_ms = age.as_millis(),
                                        "Successfully retried disconnect signal"
                                    );
                                }
                                Err(_) => {
                                    // Channel still full, will retry next tick
                                    // The signal remains in pending_disconnects
                                }
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
        });
    }

    /// Send a disconnect signal, storing it for retry if the channel is full.
    ///
    /// This method ensures that disconnect signals are not lost even when the
    /// broadcast channel is temporarily full. If the send fails, the signal
    /// is stored in `pending_disconnects` and will be retried by the background
    /// task spawned in `new()`.
    fn send_disconnect_signal(&self, signal: DisconnectSignal) {
        // First try to send directly
        match self.disconnect_tx.send(signal.clone()) {
            Ok(_) => {
                // Signal sent successfully
                return;
            }
            Err(_) => {
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
                    self.dropped_disconnect_signals.fetch_add(1, Ordering::Relaxed);
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
                self.pending_disconnects.insert(id, (signal.clone(), Instant::now()));

                warn!(
                    signal = ?signal,
                    pending_count = self.pending_disconnects.len(),
                    "Disconnect signal queued for retry (broadcast channel full)"
                );
            }
        }
    }

    /// Cancel the auto-spawned background tasks.
    ///
    /// Should be called during graceful shutdown to stop the background tasks.
    pub fn shutdown(&self) {
        self.ttl_refresh_cancel.cancel();
        self.disconnect_retry_cancel.cancel();
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
    #[must_use]
    pub fn can_accept_user_connection(&self, user_id: &UserId) -> Result<(), String> {
        // Check local user connection limit
        let user_entry = self.user_connections.get(user_id);
        let current_count = user_entry.as_ref().map(|v| v.len()).unwrap_or(0);

        if current_count >= self.limits.max_per_user {
            return Err(format!(
                "User at capacity ({} connections, max: {})",
                current_count,
                self.limits.max_per_user
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
    #[must_use]
    pub fn can_accept_room_connection(&self, room_id: &RoomId) -> Result<(), String> {
        // Check local room connection limit
        let room_entry = self.room_connections.get(room_id);
        let current_count = room_entry.as_ref().map(|v| v.len()).unwrap_or(0);

        if current_count >= self.limits.max_per_room {
            return Err(format!(
                "Room at capacity ({} connections, max: {})",
                current_count,
                self.limits.max_per_room
            ));
        }

        Ok(())
    }

    /// Register a new connection
    ///
    /// Returns Ok(()) if connection is allowed, or Err with reason if rejected.
    ///
    /// When Redis is configured, enforces per-user limits across all replicas.
    /// Falls back to per-node limits if Redis is unavailable.
    pub async fn register(&self, connection_id: String, user_id: UserId) -> Result<(), String> {
        // Atomically reserve a slot in the total connection count.
        // fetch_add returns the previous value; if it was already at the limit,
        // roll back and reject.
        let prev = self.total_connections.fetch_add(1, Ordering::AcqRel);
        if prev >= self.limits.max_total {
            self.total_connections.fetch_sub(1, Ordering::AcqRel);
            return Err(format!(
                "Server at capacity ({} connections)",
                self.limits.max_total
            ));
        }

        // Increment distributed total connection counter (best-effort).
        // Uses the same atomic INCR+EXPIRE Lua script as redis_incr_and_check()
        // to prevent a crash between the two operations from leaving a key
        // without a TTL.
        if let Some(ref conn) = self.redis_conn {
            let total_key = format!("{}connections:total", self.redis_key_prefix);
            let mut conn_clone = conn.clone();
            let script = redis::Script::new(
                "local count = redis.call('INCR', KEYS[1]) \
                 redis.call('EXPIRE', KEYS[1], ARGV[1]) \
                 return count"
            );
            let _ = script
                .key(&total_key)
                .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
                .invoke_async::<i64>(&mut conn_clone)
                .await;
        }

        // Enforce per-user connection limit.
        //
        // When Redis is configured, use the atomic INCR return value as the
        // single source of truth for the cross-replica count. If the new count
        // exceeds the limit we immediately DECR and reject, avoiding any TOCTOU
        // window where two replicas could both pass the check concurrently.
        //
        // When Redis is not configured, fall back to the local DashMap count.
        if let Some(ref conn) = self.redis_conn {
            let redis_key = format!("{}connections:user:{}", self.redis_key_prefix, user_id.as_str());
            match self.redis_incr_and_check(&redis_key, self.limits.max_per_user).await {
                Ok(true) => {
                    // Distributed limit not exceeded; proceed.
                }
                Ok(false) => {
                    // Distributed limit exceeded -- roll back total counter and
                    // the Redis per-user counter that was just incremented.
                    self.total_connections.fetch_sub(1, Ordering::AcqRel);
                    let _ = self.redis_decr(conn, &redis_key).await;
                    return Err(format!(
                        "Too many connections for this user across all replicas (max {})",
                        self.limits.max_per_user
                    ));
                }
                Err(e) => {
                    // Redis error -- fall back to local-only check below.
                    warn!("Distributed user connection check failed, using local fallback: {e}");
                    // Fall through to local check.
                    let user_count = self.user_connections
                        .get(&user_id)
                        .map_or(0, |c| c.len());
                    if user_count >= self.limits.max_per_user {
                        self.total_connections.fetch_sub(1, Ordering::AcqRel);
                        return Err(format!(
                            "Too many connections for this user (max {})",
                            self.limits.max_per_user
                        ));
                    }
                }
            }
        } else {
            // No Redis: enforce limit using the local DashMap count only.
            let user_count = self.user_connections
                .get(&user_id)
                .map_or(0, |c| c.len());
            if user_count >= self.limits.max_per_user {
                self.total_connections.fetch_sub(1, Ordering::AcqRel);
                return Err(format!(
                    "Too many connections for this user (max {})",
                    self.limits.max_per_user
                ));
            }
        }

        // Add the connection to the local user index (used for routing and cleanup).
        {
            let mut user_entry = self.user_connections.entry(user_id.clone()).or_default();
            user_entry.push(connection_id.clone());
            // Drop the shard lock before inserting into another DashMap
        }

        // Create and register connection info
        let conn_info = ConnectionInfo::new(connection_id.clone(), user_id.clone());
        self.connections.insert(connection_id.clone(), conn_info.clone());

        // Persist connection metadata to Redis (best-effort)
        if let Some(ref conn) = self.redis_conn {
            let conn_key = format!("{}conn_mgr:conn:{}", self.redis_key_prefix, connection_id);
            let user_index_key = format!("{}conn_mgr:user:{}", self.redis_key_prefix, user_id.as_str());

            let persistent = ConnectionInfoPersistent::from(&conn_info);
            let mut conn_clone = conn.clone();
            let connection_id_clone = connection_id.clone();

            tokio::spawn(async move {
                // Store connection metadata as JSON with TTL for crash-safety.
                // If the node crashes without calling unregister(), the key
                // auto-expires instead of leaking indefinitely.
                if let Ok(json) = serde_json::to_string(&persistent) {
                    let result: Result<(), _> = redis::cmd("SET")
                        .arg(&conn_key)
                        .arg(&json)
                        .arg("EX")
                        .arg(CONNECTION_METADATA_TTL_SECONDS)
                        .query_async(&mut conn_clone)
                        .await;
                    if let Err(e) = result {
                        warn!("Failed to persist connection metadata to Redis: {e}");
                    }
                }

                // Add to user's connection set for distributed queries
                if let Err(e) = conn_clone.sadd::<_, _, ()>(&user_index_key, &connection_id_clone).await {
                    warn!("Failed to add connection to user index: {e}");
                }
                // Set TTL on user index set
                let _: Result<(), _> = conn_clone.expire(&user_index_key, CONNECTION_METADATA_TTL_SECONDS).await;
            });
        }

        // Update metrics
        self.total_connections_ever.fetch_add(1, Ordering::Relaxed);
        synctv_core::metrics::ACTIVE_CONNECTIONS.inc();
        synctv_core::metrics::cluster::CLUSTER_CONNECTIONS.set(
            self.total_connections.load(Ordering::Relaxed) as i64,
        );

        info!(
            connection_id = %connection_id,
            user_id = %user_id.as_str(),
            total_connections = self.total_connections.load(Ordering::Relaxed),
            "Connection registered"
        );

        Ok(())
    }

    /// Associate a connection with a room
    ///
    /// Enforces per-room connection limits to prevent resource exhaustion.
    /// When Redis is configured, limits are enforced across all replicas.
    ///
    /// If the connection is already in a different room, it is removed from
    /// the old room first (preventing a double-join / leaked entry).
    pub async fn join_room(&self, connection_id: &str, room_id: RoomId) -> Result<(), String> {
        // Check if connection already belongs to a room and remove it from the old
        // room's entry before adding to the new one (prevent double-join).
        let old_room_id: Option<RoomId> = {
            let old = self
                .connections
                .get(connection_id)
                .and_then(|c| c.room_id.clone());

            if let Some(ref old_room) = old {
                if old_room == &room_id {
                    // Already in the target room -- nothing to do
                    return Ok(());
                }
                // Remove from old room's connection list
                if let Some(mut old_room_conns) = self.room_connections.get_mut(old_room) {
                    old_room_conns.retain(|id| id != connection_id);
                    if old_room_conns.is_empty() {
                        drop(old_room_conns);
                        self.room_connections.remove(old_room);
                    }
                }
            }
            old
        };

        // Decrement old room's distributed counter if we left a room
        if let Some(ref old_room) = old_room_id {
            if let Some(ref conn) = self.redis_conn {
                let old_key = format!("{}connections:room:{}", self.redis_key_prefix, old_room.as_str());
                let _ = self.redis_decr(conn, &old_key).await;
            }
        }

        // Check per-room limit locally, then increment Redis, then add to local map.
        //
        // The DashMap entry lock is NOT held across the Redis await to avoid
        // blocking the entire shard during Redis RTT. This means there is a
        // small TOCTOU window where concurrent local joins could both pass the
        // local check, but the Redis counter still enforces the distributed
        // limit correctly, and the local overshoot is bounded to the number of
        // concurrent join_room calls (typically very small).

        // Step 1: Check local limit (short-lived lock)
        {
            let room_entry = self.room_connections.entry(room_id.clone()).or_default();
            if room_entry.len() >= self.limits.max_per_room {
                return Err(format!(
                    "Room at capacity ({} connections)",
                    self.limits.max_per_room
                ));
            }
            // Lock dropped here
        }

        // Step 2: Check distributed per-room limit via Redis (no DashMap lock held)
        let redis_room_incremented = if let Some(ref _conn) = self.redis_conn {
            let redis_key = format!("{}connections:room:{}", self.redis_key_prefix, room_id.as_str());
            match self.redis_incr_and_check(&redis_key, self.limits.max_per_room).await {
                Ok(true) => true,
                Ok(false) => {
                    let _ = self.redis_decr(_conn, &redis_key).await;
                    return Err(format!(
                        "Room at capacity across all replicas ({} connections)",
                        self.limits.max_per_room
                    ));
                }
                Err(e) => {
                    warn!("Distributed room connection check failed, using local fallback: {e}");
                    false
                }
            }
        } else {
            false
        };

        // Step 3: Add connection to local map (short-lived lock)
        {
            let mut room_entry = self.room_connections.entry(room_id.clone()).or_default();
            room_entry.push(connection_id.to_string());
        }

        // Update connection info
        let conn_info_updated = if let Some(mut conn) = self.connections.get_mut(connection_id) {
            conn.room_id = Some(room_id.clone());
            conn.last_activity = Instant::now();
            Some(conn.clone())
        } else {
            // Connection disappeared -- roll back the room_connections entry
            if let Some(mut room_conns) = self.room_connections.get_mut(&room_id) {
                room_conns.retain(|id| id != connection_id);
            }
            // Roll back Redis counter
            if redis_room_incremented {
                if let Some(ref conn) = self.redis_conn {
                    let redis_key = format!("{}connections:room:{}", self.redis_key_prefix, room_id.as_str());
                    let _ = self.redis_decr(conn, &redis_key).await;
                }
            }
            return Err("Connection not found".to_string());
        };

        // Update Redis metadata with new room_id (best-effort)
        if let (Some(info), Some(ref conn)) = (conn_info_updated, &self.redis_conn) {
            let conn_key = format!("{}conn_mgr:conn:{}", self.redis_key_prefix, connection_id);
            let room_index_key = format!("{}conn_mgr:room:{}", self.redis_key_prefix, room_id.as_str());

            let persistent = ConnectionInfoPersistent::from(&info);
            let mut conn_clone = conn.clone();
            let connection_id_clone = connection_id.to_string();

            tokio::spawn(async move {
                // Update connection metadata with new room_id (with TTL for crash-safety)
                if let Ok(json) = serde_json::to_string(&persistent) {
                    let result: Result<(), _> = redis::cmd("SET")
                        .arg(&conn_key)
                        .arg(&json)
                        .arg("EX")
                        .arg(CONNECTION_METADATA_TTL_SECONDS)
                        .query_async(&mut conn_clone)
                        .await;
                    if let Err(e) = result {
                        warn!("Failed to update connection metadata in Redis: {e}");
                    }
                }

                // Add to room's connection set
                if let Err(e) = conn_clone.sadd::<_, _, ()>(&room_index_key, &connection_id_clone).await {
                    warn!("Failed to add connection to room index: {e}");
                }
                // Set TTL on room index set
                let _: Result<(), _> = conn_clone.expire(&room_index_key, CONNECTION_METADATA_TTL_SECONDS).await;
            });
        }

        synctv_core::metrics::cluster::NODE_ACTIVE_ROOMS.set(
            self.room_connections.len() as i64,
        );

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

            // Remove from user connections
            if let Some(mut user_conns) = self.user_connections.get_mut(&conn_info.user_id) {
                user_conns.retain(|id| id != connection_id);
                if user_conns.is_empty() {
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
            if let Some(ref conn) = self.redis_conn {
                let conn_clone = conn.clone();
                let key_prefix = self.redis_key_prefix.clone();
                let user_id_str = conn_info.user_id.as_str().to_string();
                let room_id_str = conn_info.room_id.as_ref().map(|r| r.as_str().to_string());
                let connection_id_owned = connection_id.to_string();
                let retry_tx = self.pending_retries_tx.clone();

                let cleanup = async {
                    let this = &*self;

                    // Decrement total distributed counter
                    let total_key = format!("{key_prefix}connections:total");
                    if let Err(_e) = this.redis_decr(&conn_clone, &total_key).await {
                        let _ = retry_tx.try_send(PendingRedisOp::Decr(total_key));
                    }

                    // Decrement user counter
                    let user_key = format!("{key_prefix}connections:user:{user_id_str}");
                    if let Err(_e) = this.redis_decr(&conn_clone, &user_key).await {
                        let _ = retry_tx.try_send(PendingRedisOp::Decr(user_key));
                    }

                    // Decrement room counter
                    if let Some(ref room_id) = room_id_str {
                        let room_key = format!("{key_prefix}connections:room:{room_id}");
                        if let Err(_e) = this.redis_decr(&conn_clone, &room_key).await {
                            let _ = retry_tx.try_send(PendingRedisOp::Decr(room_key));
                        }
                    }

                    // Remove metadata and index entries
                    let conn_key = format!("{key_prefix}conn_mgr:conn:{connection_id_owned}");
                    let user_index_key = format!("{key_prefix}conn_mgr:user:{user_id_str}");
                    let room_index_key = room_id_str.as_ref()
                        .map(|r| format!("{key_prefix}conn_mgr:room:{r}"));

                    let mut mc = conn_clone.clone();
                    let _: Result<(), _> = mc.del(&conn_key).await;
                    let _: Result<(), _> = mc.srem(&user_index_key, &connection_id_owned).await;
                    if let Some(room_key) = room_index_key {
                        let _: Result<(), _> = mc.srem(&room_key, &connection_id_owned).await;
                    }
                };

                if tokio::time::timeout(Duration::from_secs(2), cleanup).await.is_err() {
                    warn!(
                        connection_id = %connection_id,
                        "Redis cleanup timed out during unregister, enqueueing retries"
                    );
                    // Enqueue all decrement operations for retry
                    let total_key = format!("{}connections:total", self.redis_key_prefix);
                    self.enqueue_retry(PendingRedisOp::Decr(total_key));
                    let user_key = format!("{}connections:user:{}", self.redis_key_prefix, user_id_str);
                    self.enqueue_retry(PendingRedisOp::Decr(user_key));
                    if let Some(ref room_id) = room_id_str {
                        let room_key = format!("{}connections:room:{room_id}", self.redis_key_prefix);
                        self.enqueue_retry(PendingRedisOp::Decr(room_key));
                    }
                }
            }

            synctv_core::metrics::ACTIVE_CONNECTIONS.dec();
            synctv_core::metrics::cluster::CLUSTER_CONNECTIONS.set(
                self.total_connections.load(Ordering::Relaxed) as i64,
            );
            synctv_core::metrics::cluster::NODE_ACTIVE_ROOMS.set(
                self.room_connections.len() as i64,
            );

            info!(
                connection_id = %connection_id,
                user_id = %conn_info.user_id.as_str(),
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
        let mut to_disconnect = Vec::new();

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
                        // Mark as left WebRTC session
                        if let Some(room_id) = &conn.room_id {
                            self.mark_rtc_joined(room_id, &conn.user_id, &conn.connection_id, false);
                        }
                        // Add to disconnect list to force reconnection
                        to_disconnect.push(conn.connection_id.clone());
                    }
                }
            }
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
    pub async fn connection_count_distributed(&self) -> usize {
        if let Some(ref conn) = self.redis_conn {
            let redis_key = format!("{}connections:total", self.redis_key_prefix);
            let mut conn_clone = conn.clone();
            match conn_clone.get::<_, Option<i64>>(&redis_key).await {
                Ok(Some(count)) if count > 0 => return count as usize,
                Ok(_) => return 0,
                Err(e) => {
                    warn!("Failed to read distributed total connection count from Redis, falling back to local: {e}");
                }
            }
        }
        // Fallback to local-only count
        self.connection_count()
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
    pub async fn room_connection_count_distributed(&self, room_id: &RoomId) -> usize {
        if let Some(ref conn) = self.redis_conn {
            let redis_key = format!("{}connections:room:{}", self.redis_key_prefix, room_id.as_str());
            let mut conn_clone = conn.clone();
            match conn_clone.get::<_, Option<i64>>(&redis_key).await {
                Ok(Some(count)) if count > 0 => return count as usize,
                Ok(_) => return 0,
                Err(e) => {
                    warn!("Failed to read distributed room connection count from Redis, falling back to local: {e}");
                }
            }
        }
        // Fallback to local-only count
        self.room_connection_count(room_id)
    }

    /// Get connection counts for multiple rooms across all replicas (distributed).
    ///
    /// Uses Redis MGET to fetch all room counters in a single round-trip,
    /// avoiding N+1 queries. Falls back to sequential local-only counts if
    /// Redis is not configured or unavailable.
    pub async fn room_connection_count_distributed_batch(&self, room_ids: &[&RoomId]) -> Vec<usize> {
        if room_ids.is_empty() {
            return Vec::new();
        }

        if let Some(ref conn) = self.redis_conn {
            let keys: Vec<String> = room_ids
                .iter()
                .map(|rid| format!("{}connections:room:{}", self.redis_key_prefix, rid.as_str()))
                .collect();

            let mut conn_clone = conn.clone();
            match redis::cmd("MGET")
                .arg(&keys)
                .query_async::<Vec<Option<i64>>>(&mut conn_clone)
                .await
            {
                Ok(values) => {
                    return values
                        .into_iter()
                        .map(|v| v.filter(|&c| c > 0).unwrap_or(0) as usize)
                        .collect();
                }
                Err(e) => {
                    warn!("Failed to read distributed room connection counts from Redis (MGET), falling back to local: {e}");
                }
            }
        }

        // Fallback to local-only counts
        room_ids
            .iter()
            .map(|rid| self.room_connection_count(rid))
            .collect()
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
        let conn_ids: Vec<String> = self.user_connections
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
        let conn_ids: Vec<String> = self.room_connections
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
        let Some(ref conn) = self.redis_conn else {
            return;
        };
        let mut conn = conn.clone();

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
        let result = self.batch_refresh_ttls_with_lua(
            &mut conn,
            &counter_keys,
            &metadata_keys,
        ).await;

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
            let consecutive = synctv_core::metrics::cluster::DISTRIBUTED_COUNTER_TTL_CONSECUTIVE_FAILURES.get() + 1;
            synctv_core::metrics::cluster::DISTRIBUTED_COUNTER_TTL_CONSECUTIVE_FAILURES.set(consecutive);
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
            "#
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
            while counter_offset < counter_keys_vec.len() && batch_keys.len() < TTL_REFRESH_BATCH_SIZE {
                batch_keys.push(&counter_keys_vec[counter_offset]);
                batch_counter_count += 1;
                counter_offset += 1;
            }

            // Add metadata keys to batch
            while metadata_offset < metadata_keys_vec.len() && batch_keys.len() < TTL_REFRESH_BATCH_SIZE {
                batch_keys.push(&metadata_keys_vec[metadata_offset]);
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
    /// The synchronization uses a Lua script that atomically sets the counter
    /// to the correct value if a discrepancy is detected, preventing race
    /// conditions with concurrent connection operations.
    async fn sync_local_counts_to_redis(&self, conn: &mut redis::aio::ConnectionManager) {
        // Collect local counts first (avoid holding locks during Redis operations)
        let mut user_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for entry in self.user_connections.iter() {
            let count = entry.value().len();
            if count > 0 {
                let key = format!("{}connections:user:{}", self.redis_key_prefix, entry.key().as_str());
                user_counts.insert(key, count);
            }
        }

        let mut room_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for entry in self.room_connections.iter() {
            let count = entry.value().len();
            if count > 0 {
                let key = format!("{}connections:room:{}", self.redis_key_prefix, entry.key().as_str());
                room_counts.insert(key, count);
            }
        }

        let local_total = self.connection_count();
        let total_key = format!("{}connections:total", self.redis_key_prefix);

        // Lua script to atomically set a counter if it differs from expected value.
        // Returns the old value (or 0 if key didn't exist) and whether it was changed (1 or 0).
        let sync_script = redis::Script::new(
            r"local current = redis.call('GET', KEYS[1])
              local current_num = 0
              if current ~= false then
                current_num = tonumber(current)
              end
              if current_num ~= tonumber(ARGV[1]) then
                redis.call('SET', KEYS[1], ARGV[1])
                redis.call('EXPIRE', KEYS[1], ARGV[2])
                return {current_num, 1}
              end
              return {current_num, 0}"
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
                            "Synchronized user connection counter to Redis"
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
                            "Synchronized room connection counter to Redis"
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

        // Sync total counter (only if we have local connections)
        if local_total > 0 {
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
                            "Synchronized total connection counter to Redis (was out of sync)"
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
    /// 3. Cleans up stale Redis connection metadata that doesn't exist locally
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
        let Some(ref conn) = self.redis_conn else {
            // No Redis configured - nothing to reconcile
            return;
        };
        let mut conn = conn.clone();

        // Step 1: Sync connection counters (existing logic)
        self.sync_local_counts_to_redis(&mut conn).await;

        // Step 2: Sync connection metadata to Redis
        self.sync_connection_metadata_to_redis(&mut conn).await;

        // Step 3: Clean up stale Redis metadata (keys that don't exist locally)
        self.cleanup_stale_redis_metadata(&mut conn).await;
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
                self.redis_key_prefix,
                conn_info.connection_id
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

    /// Clean up stale Redis connection metadata that doesn't exist locally.
    ///
    /// Scans Redis for connection metadata keys and deletes any that don't
    /// correspond to active local connections. This handles the case where
    /// a connection was unregistered locally but the Redis deletion failed.
    async fn cleanup_stale_redis_metadata(&self, conn: &mut redis::aio::ConnectionManager) {
        use redis::AsyncCommands;

        let pattern = format!("{}conn_mgr:conn:*", self.redis_key_prefix);
        let mut cleaned = 0u64;
        let mut errors = 0u64;

        // Use SCAN to iterate over matching keys
        let mut cursor: u64 = 0;
        loop {
            let result: Result<(u64, Vec<String>), _> = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100) // Batch size
                .query_async(conn)
                .await;

            match result {
                Ok((new_cursor, keys)) => {
                    cursor = new_cursor;

                    for key in keys {
                        // Extract connection_id from key
                        // Key format: {prefix}conn_mgr:conn:{connection_id}
                        if let Some(conn_id) = key.strip_prefix(&format!(
                            "{}conn_mgr:conn:",
                            self.redis_key_prefix
                        )) {
                            // Check if this connection exists locally
                            if !self.connections.contains_key(conn_id) {
                                // Connection doesn't exist locally - delete from Redis
                                let del_result: Result<(), _> = conn.del(&key).await;
                                match del_result {
                                    Ok(()) => {
                                        cleaned += 1;
                                        debug!(
                                            connection_id = %conn_id,
                                            key = %key,
                                            "Cleaned up stale connection metadata from Redis"
                                        );
                                    }
                                    Err(e) => {
                                        errors += 1;
                                        warn!(
                                            key = %key,
                                            error = %e,
                                            "Failed to delete stale connection metadata"
                                        );
                                    }
                                }
                            }
                        }
                    }

                    // SCAN returns cursor 0 when done
                    if cursor == 0 {
                        break;
                    }
                }
                Err(e) => {
                    errors += 1;
                    warn!(error = %e, "Failed to SCAN Redis for stale metadata");
                    break;
                }
            }
        }

        if cleaned > 0 || errors > 0 {
            info!(
                stale_cleaned = cleaned,
                cleanup_errors = errors,
                "Cleaned up stale connection metadata from Redis"
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
        let conn_ids: Vec<String> = self.user_connections
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
                conn.rtc_joined_at = if joined {
                    Some(Instant::now())
                } else {
                    None
                };
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
        let conn_ids: Vec<String> = self.room_connections
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
    /// all replicas in the cluster. Falls back to local-only if Redis fails.
    pub async fn get_user_connections_distributed(&self, user_id: &UserId) -> Vec<String> {
        if let Some(ref conn) = self.redis_conn {
            let user_index_key = format!("{}conn_mgr:user:{}", self.redis_key_prefix, user_id.as_str());
            let mut conn_clone = conn.clone();

            match conn_clone.smembers::<_, Vec<String>>(&user_index_key).await {
                Ok(conn_ids) => return conn_ids,
                Err(e) => {
                    warn!("Failed to fetch user connections from Redis, falling back to local: {e}");
                }
            }
        }

        // Fallback to local-only
        self.get_user_connections(user_id)
            .into_iter()
            .map(|c| c.connection_id)
            .collect()
    }

    /// Get all connections in a room across all replicas (from Redis).
    ///
    /// Returns connection IDs from Redis, which includes connections from
    /// all replicas in the cluster. Falls back to local-only if Redis fails.
    pub async fn get_room_connections_distributed(&self, room_id: &RoomId) -> Vec<String> {
        if let Some(ref conn) = self.redis_conn {
            let room_index_key = format!("{}conn_mgr:room:{}", self.redis_key_prefix, room_id.as_str());
            let mut conn_clone = conn.clone();

            match conn_clone.smembers::<_, Vec<String>>(&room_index_key).await {
                Ok(conn_ids) => return conn_ids,
                Err(e) => {
                    warn!("Failed to fetch room connections from Redis, falling back to local: {e}");
                }
            }
        }

        // Fallback to local-only
        self.get_room_connections(room_id)
            .into_iter()
            .map(|c| c.connection_id)
            .collect()
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
        let Some(ref conn) = self.redis_conn else {
            return Err("Redis not configured".to_string());
        };
        let mut conn = conn.clone();

        // Lua script: atomically INCR the key and set TTL in a single round-trip.
        // Returns the new counter value after increment.
        let script = redis::Script::new(
            "local count = redis.call('INCR', KEYS[1]) \
             redis.call('EXPIRE', KEYS[1], ARGV[1]) \
             return count"
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
    async fn redis_decr(&self, conn: &redis::aio::ConnectionManager, key: &str) -> Result<(), String> {
        let mut conn = conn.clone();
        let script = redis::Script::new(
            r"local v = redis.call('DECR', KEYS[1])
              if v < 0 then
                redis.call('DEL', KEYS[1])
              end
              return v"
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

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn test_record_message() {
        let manager = ConnectionManager::default();
        let user_id = UserId::from_string("user1".to_string());

        manager.register("conn1".to_string(), user_id).await.unwrap();

        manager.record_message("conn1");
        manager.record_message("conn1");

        let conn = manager.get_connection("conn1").unwrap();
        assert_eq!(conn.message_count, 2);
        assert_eq!(manager.total_messages(), 2);
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

        manager.register("conn1".to_string(), user_id).await.unwrap();

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

        let client = redis::Client::open("redis://127.0.0.1:6379").unwrap();
        let conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();

        let manager = ConnectionManager::new(ConnectionLimits::default())
            .with_redis(conn, "test:");

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
            .get("test:connections:user:user1")
            .await
            .unwrap_or(0);
        assert_eq!(user_count, 1);

        // Simulate Redis outage by clearing Redis keys manually
        // (In real scenario, Redis would be down)
        let _: () = redis_conn.del("test:connections:user:user1").await.unwrap();
        let _: () = redis_conn.del("test:connections:room:room1").await.unwrap();

        // At this point, local state has 1 connection but Redis has 0
        assert_eq!(manager.user_connection_count(&user_id), 1);

        // Trigger reconciliation
        manager.reconcile_with_redis().await;

        // After reconciliation, Redis should match local state
        let user_count: i64 = redis_conn
            .get("test:connections:user:user1")
            .await
            .unwrap_or(0);
        assert_eq!(user_count, 1);

        // Cleanup
        manager.unregister("conn1").await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker Redis"]
    async fn test_redis_recovery_reconciles_stale_connections() {
        // This test verifies that stale Redis connection metadata is cleaned up
        // during reconciliation.

        use redis::AsyncCommands;

        let client = redis::Client::open("redis://127.0.0.1:6379").unwrap();
        let conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();

        let manager = ConnectionManager::new(ConnectionLimits::default())
            .with_redis(conn, "test2:");

        let _user_id = UserId::from_string("user1".to_string());

        // Manually inject stale connection metadata into Redis
        // (simulating a connection that was never cleaned up)
        let mut redis_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();
        let stale_key = "test2:conn_mgr:conn:stale_conn";
        let stale_meta = ConnectionInfoPersistent {
            connection_id: "stale_conn".to_string(),
            user_id: "user_stale".to_string(),
            room_id: None,
            connected_at_unix: 0,
            last_activity_unix: 0,
            message_count: 0,
            rtc_joined: false,
            rtc_joined_at_unix: None,
        };
        let _: () = redis_conn
            .set(stale_key, serde_json::to_string(&stale_meta).unwrap())
            .await
            .unwrap();

        // Verify stale data exists
        let exists: bool = redis_conn.exists(stale_key).await.unwrap();
        assert!(exists);

        // Trigger reconciliation
        manager.reconcile_with_redis().await;

        // Stale connection should be cleaned up since it doesn't exist locally
        let exists: bool = redis_conn.exists(stale_key).await.unwrap();
        assert!(!exists);
    }

    #[tokio::test]
    #[ignore = "Requires Docker Redis"]
    async fn test_redis_outage_during_register_eventually_consistent() {
        // This test verifies that failed Redis operations during register
        // are eventually reconciled.

        use redis::AsyncCommands;

        let client = redis::Client::open("redis://127.0.0.1:6379").unwrap();
        let conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();

        let manager = ConnectionManager::new(ConnectionLimits::default())
            .with_redis(conn, "test3:");

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
            .get("test3:connections:user:user1")
            .await
            .unwrap_or(0);
        assert_eq!(user_count, 1);

        // Manually corrupt the counter (simulating partial failure)
        let _: () = redis_conn.set("test3:connections:user:user1", 0).await.unwrap();

        // Local state says 1, Redis says 0
        assert_eq!(manager.user_connection_count(&user_id), 1);

        // Trigger reconciliation
        manager.reconcile_with_redis().await;

        // After reconciliation, Redis should be corrected
        let user_count: i64 = redis_conn
            .get("test3:connections:user:user1")
            .await
            .unwrap_or(0);
        assert_eq!(user_count, 1);

        // Cleanup
        manager.unregister("conn1").await;
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
}
