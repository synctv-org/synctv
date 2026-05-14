use dashmap::DashMap;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use synctv_core::{
    config::ConnectionLimitsConfig,
    models::id::{RoomId, UserId},
    RedisConnectionRuntime,
};
#[cfg(test)]
use synctv_core::{DirectRedisConnectionRuntime, SharedRedisConnectionRuntime};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

#[cfg(test)]
type AsyncTestHook =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

type ConnectionDeadline = (Instant, String);

#[derive(Debug, Default)]
struct TimeoutIndex {
    idle_deadlines: BTreeSet<ConnectionDeadline>,
    idle_by_connection: HashMap<String, Instant>,
    max_deadlines: BTreeSet<ConnectionDeadline>,
    max_by_connection: HashMap<String, Instant>,
    rtc_deadlines: BTreeSet<ConnectionDeadline>,
    rtc_by_connection: HashMap<String, Instant>,
}

impl TimeoutIndex {
    fn update_deadline(
        deadlines: &mut BTreeSet<ConnectionDeadline>,
        deadlines_by_connection: &mut HashMap<String, Instant>,
        connection_id: &str,
        deadline: Instant,
    ) {
        if let Some(previous_deadline) =
            deadlines_by_connection.insert(connection_id.to_string(), deadline)
        {
            deadlines.remove(&(previous_deadline, connection_id.to_string()));
        }
        deadlines.insert((deadline, connection_id.to_string()));
    }

    fn clear_deadline(
        deadlines: &mut BTreeSet<ConnectionDeadline>,
        deadlines_by_connection: &mut HashMap<String, Instant>,
        connection_id: &str,
    ) {
        if let Some(previous_deadline) = deadlines_by_connection.remove(connection_id) {
            deadlines.remove(&(previous_deadline, connection_id.to_string()));
        }
    }

    fn schedule_idle(&mut self, connection_id: &str, deadline: Instant) {
        Self::update_deadline(
            &mut self.idle_deadlines,
            &mut self.idle_by_connection,
            connection_id,
            deadline,
        );
    }

    fn schedule_max_duration(&mut self, connection_id: &str, deadline: Instant) {
        Self::update_deadline(
            &mut self.max_deadlines,
            &mut self.max_by_connection,
            connection_id,
            deadline,
        );
    }

    fn schedule_rtc(&mut self, connection_id: &str, deadline: Instant) {
        Self::update_deadline(
            &mut self.rtc_deadlines,
            &mut self.rtc_by_connection,
            connection_id,
            deadline,
        );
    }

    fn clear_rtc(&mut self, connection_id: &str) {
        Self::clear_deadline(
            &mut self.rtc_deadlines,
            &mut self.rtc_by_connection,
            connection_id,
        );
    }

    fn remove_connection(&mut self, connection_id: &str) {
        Self::clear_deadline(
            &mut self.idle_deadlines,
            &mut self.idle_by_connection,
            connection_id,
        );
        Self::clear_deadline(
            &mut self.max_deadlines,
            &mut self.max_by_connection,
            connection_id,
        );
        Self::clear_deadline(
            &mut self.rtc_deadlines,
            &mut self.rtc_by_connection,
            connection_id,
        );
    }

    fn collect_due(deadlines: &mut BTreeSet<ConnectionDeadline>, now: Instant) -> Vec<String> {
        let mut due_connection_ids = Vec::new();
        while let Some((deadline, connection_id)) = deadlines.first().cloned() {
            if deadline >= now {
                break;
            }
            deadlines.pop_first();
            due_connection_ids.push(connection_id);
        }
        due_connection_ids
    }

    fn take_due_idle(&mut self, now: Instant) -> Vec<String> {
        let due = Self::collect_due(&mut self.idle_deadlines, now);
        for connection_id in &due {
            self.idle_by_connection.remove(connection_id);
        }
        due
    }

    fn take_due_max_duration(&mut self, now: Instant) -> Vec<String> {
        let due = Self::collect_due(&mut self.max_deadlines, now);
        for connection_id in &due {
            self.max_by_connection.remove(connection_id);
        }
        due
    }

    fn take_due_rtc(&mut self, now: Instant) -> Vec<String> {
        let due = Self::collect_due(&mut self.rtc_deadlines, now);
        for connection_id in &due {
            self.rtc_by_connection.remove(connection_id);
        }
        due
    }
}

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
    pub registration_token: String,
    pub user_id: UserId,
    pub actor_id: String,
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
    registration_token: String,
    user_id: UserId,
    #[serde(default)]
    actor_id: String,
    room_id: Option<RoomId>,
    connected_at_unix: u64,
    last_activity_unix: u64,
    message_count: u64,
    rtc_joined: bool,
    rtc_joined_at_unix: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RoomTransition {
    previous_room_id: Option<RoomId>,
    room_id: RoomId,
}

fn system_time_to_unix_secs(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .unwrap_or_else(|error| {
            warn!("System clock is before UNIX_EPOCH; using zero timestamp fallback: {error}");
            Duration::ZERO
        })
        .as_secs()
}

fn u64_to_usize_saturating(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn i64_to_usize_saturating(value: i64) -> usize {
    usize::try_from(value).unwrap_or_default()
}

fn i64_to_u64_saturating(value: i64) -> u64 {
    u64::try_from(value).unwrap_or_default()
}

fn usize_to_i64_saturating(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn usize_to_u64_saturating(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
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
            registration_token: info.registration_token.clone(),
            user_id: info.user_id,
            actor_id: info.actor_id.clone(),
            room_id: info.room_id,
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
        Self::new_with_actor_id(connection_id, user_id, user_id.to_string())
    }

    #[must_use]
    pub fn new_with_actor_id(connection_id: String, user_id: UserId, actor_id: String) -> Self {
        let now = Instant::now();
        Self {
            connection_id,
            registration_token: synctv_common::snanoid!(16),
            user_id,
            actor_id,
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
        Self::from(ConnectionLimitsConfig::default())
    }
}

impl From<ConnectionLimitsConfig> for ConnectionLimits {
    fn from(config: ConnectionLimitsConfig) -> Self {
        Self::from(&config)
    }
}

impl From<&ConnectionLimitsConfig> for ConnectionLimits {
    fn from(config: &ConnectionLimitsConfig) -> Self {
        Self {
            max_per_user: config.max_per_user,
            max_per_room: config.max_per_room,
            max_total: config.max_total,
            idle_timeout: Duration::from_secs(config.idle_timeout_seconds),
            max_duration: Duration::from_secs(config.max_duration_seconds),
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

/// TTL for distributed connection metadata and index keys in Redis (seconds).
///
/// These keys back cross-replica presence queries (`conn_mgr:conn:*`,
/// `conn_mgr:user:*`, `conn_mgr:room:*`). They must expire quickly after a pod
/// crash so dead connections do not remain visible for hours, but stay alive
/// through transient missed refreshes while a pod is healthy.
///
/// With a 60-second refresh interval, a 180-second TTL survives two missed
/// refreshes while still letting crashed-pod state drain within a few minutes.
const CONNECTION_METADATA_TTL_SECONDS: i64 = 180; // 3x 60s refresh interval
const USER_INDEX_DIRECTORY_KEY_SUFFIX: &str = "conn_mgr:user_indexes";
const ROOM_INDEX_DIRECTORY_KEY_SUFFIX: &str = "conn_mgr:room_indexes";

static UNREGISTER_CLEANUP_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local current_metadata = redis.call('GET', KEYS[5])
        local metadata_matches = false
        if current_metadata then
            local ok, obj = pcall(cjson.decode, current_metadata)
            if ok and obj and obj.registration_token == ARGV[4] then
                metadata_matches = true
            end
        end

        local first_cleanup = redis.call('SET', KEYS[1], '1', 'NX', 'EX', ARGV[1])
        if first_cleanup then
            local total = redis.call('DECR', KEYS[2])
            if total < 0 then redis.call('DEL', KEYS[2]) end

            local user_total = redis.call('DECR', KEYS[3])
            if user_total < 0 then redis.call('DEL', KEYS[3]) end

            if ARGV[3] == '1' then
                local room_total = redis.call('DECR', KEYS[4])
                if room_total < 0 then redis.call('DEL', KEYS[4]) end
            end
        end

        if metadata_matches then
            redis.call('DEL', KEYS[5])
            redis.call('SREM', KEYS[6], ARGV[2])
            if ARGV[3] == '1' then
                redis.call('SREM', KEYS[7], ARGV[2])
            end
        end

        if first_cleanup then
            return 1
        end
        return 0
        ",
    )
});

static BATCH_REFRESH_TTLS_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
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
    )
});

static SYNC_COUNTER_MIN_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
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
    )
});

static INCR_EXPIRE_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        "local count = redis.call('INCR', KEYS[1]) \
         redis.call('EXPIRE', KEYS[1], ARGV[1]) \
         return count",
    )
});

static DECR_DELETE_NEGATIVE_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"local v = redis.call('DECR', KEYS[1])
          if v < 0 then
            redis.call('DEL', KEYS[1])
          end
          return v",
    )
});

/// A failed Redis counter operation that should be retried.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingRedisOp {
    /// Decrement a counter key
    Decr(String),
    /// Idempotently clean up one unregistered connection's distributed state.
    UnregisterCleanup {
        cleanup_key: String,
        total_key: String,
        user_key: String,
        room_key: String,
        conn_key: String,
        user_index_key: String,
        room_index_key: String,
        connection_id: String,
        registration_token: String,
        has_room: bool,
    },
}

struct UnregisterCleanupScriptArgs<'a> {
    cleanup_key: &'a str,
    total_key: &'a str,
    user_key: &'a str,
    room_key: &'a str,
    conn_key: &'a str,
    user_index_key: &'a str,
    room_index_key: &'a str,
    connection_id: &'a str,
    registration_token: &'a str,
    has_room: bool,
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
    redis_conn: Option<Arc<dyn RedisConnectionRuntime>>,

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

/// Maximum capacity of the pending disconnect signals queue.
/// When the broadcast channel is full (lagging receivers), disconnect signals
/// are stored here for retry. This ensures kick/ban operations are not lost
/// even under high load.
const PENDING_DISCONNECT_QUEUE_CAPACITY: usize = 10_000;
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

    fn room_id_from_index_key(&self, key: &str) -> Option<RoomId> {
        key.strip_prefix(&format!("{}conn_mgr:room:", self.redis_key_prefix))
            .and_then(|value| value.parse::<RoomId>().ok())
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
    /// When no shared runtime is provided, the manager starts in local-only
    /// mode and still launches its local background tasks.
    #[must_use]
    pub(crate) fn from_redis_runtime(
        limits: ConnectionLimits,
        redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
        key_prefix: &str,
    ) -> Self {
        if let Some(redis_runtime) = redis_runtime {
            Self::new(limits).with_redis_runtime(redis_runtime, key_prefix)
        } else {
            let manager = Self::new(limits);
            manager.start();
            manager
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
        let shard_count = u64::try_from(self.connection_lifecycle_locks.len()).unwrap_or(u64::MAX);
        let index = u64_to_usize_saturating(hasher.finish() % shard_count);
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
        let runtime = self.redis_conn.as_ref()?;
        if let Ok(conn) =
            tokio::time::timeout(runtime.operation_timeout(), runtime.snapshot()).await
        {
            Some(conn)
        } else {
            warn!(
                timeout_ms = runtime.operation_timeout().as_millis(),
                "Redis connection snapshot timed out"
            );
            None
        }
    }

    #[must_use]
    pub(crate) fn with_redis_runtime(
        mut self,
        conn: Arc<dyn RedisConnectionRuntime>,
        key_prefix: &str,
    ) -> Self {
        self.redis_conn = Some(Arc::clone(&conn));
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
        let handle = Self::spawn_pending_retries_task(conn, rx, cancel);
        *self
            .pending_retries_handle
            .lock()
            .expect("pending retries handle mutex poisoned") = Some(handle);

        self
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
    #[cfg(test)]
    fn with_redis(self, conn: redis::aio::ConnectionManager, key_prefix: &str) -> Self {
        let runtime: Arc<dyn RedisConnectionRuntime> =
            Arc::new(DirectRedisConnectionRuntime::new(conn));
        self.with_redis_runtime(runtime, key_prefix)
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
        self.with_redis_runtime(runtime, key_prefix)
    }

    /// Spawn a background task that retries failed Redis counter operations.
    ///
    /// Drains the `pending_retries_rx` channel every 5 seconds and retries each
    /// operation. Operations that still fail are re-queued (up to 3 attempts each,
    /// tracked internally) before being dropped with a warning.
    fn spawn_pending_retries_task(
        redis_conn: Arc<dyn RedisConnectionRuntime>,
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
                        let Ok(mut conn) =
                            tokio::time::timeout(redis_conn.operation_timeout(), redis_conn.snapshot()).await
                        else {
                            warn!(
                                timeout_ms = redis_conn.operation_timeout().as_millis(),
                                pending_ops = pending.len(),
                                "Redis connection snapshot timed out while retrying pending counter operations"
                            );
                            for (op, attempts) in pending.drain(..) {
                                let next_attempt = attempts + 1;
                                if next_attempt >= MAX_OP_RETRIES {
                                    tracing::error!(
                                        op = ?op,
                                        attempts = next_attempt,
                                        "ALERT: Dropping pending Redis counter operation after snapshot timeout. \
                                         Distributed connection count may be inaccurate until TTL expiry."
                                    );
                                } else {
                                    still_pending.push((op, next_attempt));
                                }
                            }
                            pending = still_pending;
                            continue;
                        };

                        for (op, attempts) in pending.drain(..) {
                            let result = match &op {
                                PendingRedisOp::Decr(key) => {
                                    // Use raw DECR; don't need the atomic script here since
                                    // this is a compensating retry, not a live operation.
                                    tokio::time::timeout(
                                        redis_conn.operation_timeout(),
                                        conn.decr::<_, _, i64>(key, 1i64),
                                    )
                                    .await
                                    .unwrap_or_else(|_| {
                                        Err(redis::RedisError::from((
                                            redis::ErrorKind::Io,
                                            "Redis timeout: retry distributed counter decrement",
                                        )))
                                    })
                                }
                                PendingRedisOp::UnregisterCleanup {
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
                                } => {
                                    tokio::time::timeout(
                                        redis_conn.operation_timeout(),
                                        Self::run_unregister_cleanup_script(
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
                                    .unwrap_or_else(|_| {
                                        Err(redis::RedisError::from((
                                            redis::ErrorKind::Io,
                                            "Redis timeout: retry unregister cleanup",
                                        )))
                                    })
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
            total_key: format!("{}connections:total", self.redis_key_prefix),
            user_key: format!("{}connections:user:{user_id}", self.redis_key_prefix),
            room_key: room_id.map_or_else(
                || no_room_key.clone(),
                |room_id| format!("{}connections:room:{room_id}", self.redis_key_prefix),
            ),
            conn_key: format!("{}conn_mgr:conn:{connection_id}", self.redis_key_prefix),
            user_index_key: format!("{}conn_mgr:user:{user_id}", self.redis_key_prefix),
            room_index_key: room_id
                .map(|room_id| format!("{}conn_mgr:room:{room_id}", self.redis_key_prefix))
                .unwrap_or(no_room_key),
            connection_id: connection_id.to_string(),
            registration_token: registration_token.to_string(),
            has_room: room_id.is_some(),
        }
    }

    async fn run_unregister_cleanup_script(
        conn: &mut redis::aio::ConnectionManager,
        args: UnregisterCleanupScriptArgs<'_>,
    ) -> redis::RedisResult<i64> {
        UNREGISTER_CLEANUP_SCRIPT
            .key(args.cleanup_key)
            .key(args.total_key)
            .key(args.user_key)
            .key(args.room_key)
            .key(args.conn_key)
            .key(args.user_index_key)
            .key(args.room_index_key)
            .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
            .arg(args.connection_id)
            .arg(if args.has_room { "1" } else { "0" })
            .arg(args.registration_token)
            .invoke_async(conn)
            .await
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
            Self::run_unregister_cleanup_script(
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
    fn send_disconnect_signal(&self, signal: &DisconnectSignal) {
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
                Self::await_shutdown_task("disconnect retry", Duration::from_secs(5), handle).await,
            );
        }

        if !report.all_clean() {
            warn!(
                ?report,
                "ConnectionManager shutdown observed background task failures"
            );
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
        mut handle: tokio::task::JoinHandle<()>,
    ) -> ShutdownTaskOutcome {
        match tokio::time::timeout(timeout_budget, &mut handle).await {
            Ok(Ok(())) => {
                debug!(
                    task = task_name,
                    "ConnectionManager background task stopped"
                );
                ShutdownTaskOutcome::Completed
            }
            Ok(Err(error)) if error.is_cancelled() => {
                debug!(
                    task = task_name,
                    "ConnectionManager background task cancelled"
                );
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
                    "ConnectionManager background task did not stop before shutdown timeout; aborting"
                );
                handle.abort();
                match handle.await {
                    Ok(()) => debug!(
                        task = task_name,
                        "ConnectionManager background task completed after abort"
                    ),
                    Err(error) if error.is_cancelled() => debug!(
                        task = task_name,
                        "ConnectionManager background task aborted after timeout"
                    ),
                    Err(error) => warn!(
                        task = task_name,
                        error = %error,
                        "ConnectionManager background task returned join error after timeout abort"
                    ),
                }
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
        let signal = DisconnectSignal::Connection(connection_id.to_string());
        self.send_disconnect_signal(&signal);
    }

    /// Force disconnect all connections for a user
    ///
    /// Used when a user is banned or kicked from all rooms.
    /// If the broadcast channel is full, the signal is queued for retry.
    pub fn disconnect_user(&self, user_id: &UserId) {
        let conn_count = self.user_connection_count(user_id);
        info!(
            user_id = %user_id,
            connection_count = conn_count,
            "Forcing disconnect of all user connections"
        );
        let signal = DisconnectSignal::User(*user_id);
        self.send_disconnect_signal(&signal);
    }

    /// Force disconnect all connections in a room
    ///
    /// Used when a room is deleted or all users need to be removed.
    /// If the broadcast channel is full, the signal is queued for retry.
    pub fn disconnect_room(&self, room_id: &RoomId) {
        let conn_count = self.room_connection_count(room_id);
        info!(
            room_id = %room_id,
            connection_count = conn_count,
            "Forcing disconnect of all room connections"
        );
        let signal = DisconnectSignal::Room(*room_id);
        self.send_disconnect_signal(&signal);
    }

    /// Force disconnect a specific user from a specific room
    ///
    /// Used when kicking a member from a room (not banning globally).
    /// If the broadcast channel is full, the signal is queued for retry.
    pub fn disconnect_user_from_room(&self, user_id: &UserId, room_id: &RoomId) {
        info!(
            user_id = %user_id,
            room_id = %room_id,
            "Forcing disconnect of user from room"
        );
        let signal = DisconnectSignal::UserFromRoom {
            user_id: *user_id,
            room_id: *room_id,
        };
        self.send_disconnect_signal(&signal);
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

        let total_key = format!("{}connections:total", self.redis_key_prefix);

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
            let redis_key = format!("{}connections:user:{}", self.redis_key_prefix, user_id);
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
            let redis_key = format!("{}connections:room:{}", self.redis_key_prefix, room_id);
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
        let (transition, last_activity) = if let Some(mut conn) =
            self.connections.get_mut(connection_id)
        {
            let current_room_id = conn.room_id;
            if current_room_id.as_ref() == Some(&room_id) {
                drop(lifecycle_guard);
                if redis_room_incremented {
                    let redis_key =
                        format!("{}connections:room:{}", self.redis_key_prefix, room_id);
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
                        let redis_key =
                            format!("{}connections:room:{}", self.redis_key_prefix, room_id);
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
                let redis_key = format!("{}connections:room:{}", self.redis_key_prefix, room_id);
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
            let old_key = format!("{}connections:room:{}", self.redis_key_prefix, old_room);
            self.rollback_distributed_counter(old_key).await;
        }

        // Update Redis metadata with new room_id (best-effort)
        if let Some(transition) = transition.as_ref() {
            self.persist_room_membership_metadata_best_effort(connection_id, transition)
                .await;
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
    /// by `register`/`unregister`. Falls back to the local-only count if Redis
    /// is not configured or unavailable.
    pub async fn connection_count_distributed(&self) -> Result<usize, String> {
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let redis_key = format!("{}connections:total", self.redis_key_prefix);
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
            let redis_key = format!("{}connections:room:{}", self.redis_key_prefix, room_id);
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
                .map(|rid| format!("{}connections:room:{}", self.redis_key_prefix, rid))
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

            let mut room_to_users: HashMap<RoomId, HashSet<UserId>> = room_ids
                .iter()
                .map(|room_id| (**room_id, HashSet::new()))
                .collect();

            for room_id in room_ids {
                let connection_ids = self.get_room_connections_distributed(room_id).await?;
                for connection_id in connection_ids {
                    let conn_key =
                        format!("{}conn_mgr:conn:{connection_id}", self.redis_key_prefix);
                    let metadata: Option<String> = self
                        .redis_op("fetch distributed connection metadata", conn.get(&conn_key))
                        .await?;

                    let Some(metadata) = metadata else {
                        continue;
                    };

                    let info: ConnectionInfoPersistent =
                        serde_json::from_str(&metadata).map_err(|e| {
                            format!("Failed to deserialize distributed connection metadata: {e}")
                        })?;

                    if info.room_id.as_ref() == Some(room_id) {
                        room_to_users
                            .entry(**room_id)
                            .or_default()
                            .insert(info.user_id);
                    }
                }
            }

            return Ok(room_ids
                .iter()
                .map(|room_id| room_to_users.get(room_id).map_or(0, HashSet::len))
                .collect());
        }

        Ok(room_ids
            .iter()
            .map(|room_id| self.room_online_user_count(room_id))
            .collect())
    }

    /// Get distinct online user counts for every room that currently has presence.
    pub async fn hot_room_online_user_counts_distributed(
        &self,
    ) -> Result<Vec<(RoomId, usize)>, String> {
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let directory_key = self.room_index_directory_key();
            let room_index_keys: Vec<String> = self
                .redis_op("fetch room index directory", async {
                    conn.smembers(&directory_key).await
                })
                .await?;
            let mut room_counts = Vec::new();
            for room_index_key in room_index_keys {
                let Some(room_id) = self.room_id_from_index_key(&room_index_key) else {
                    continue;
                };
                let connection_ids = self
                    .load_valid_connection_ids_from_index(
                        &mut conn,
                        &room_index_key,
                        None,
                        Some(&room_id),
                    )
                    .await?;
                if connection_ids.is_empty() {
                    continue;
                }

                let metadata_keys: Vec<String> = connection_ids
                    .iter()
                    .map(|connection_id| self.conn_metadata_key(connection_id))
                    .collect();
                let metadata: Vec<Option<String>> = self
                    .redis_op("fetch distributed connection metadata", async {
                        conn.mget(metadata_keys).await
                    })
                    .await?;

                let mut online_users = HashSet::new();
                for entry in metadata.into_iter().flatten() {
                    let info: ConnectionInfoPersistent =
                        serde_json::from_str(&entry).map_err(|e| {
                            format!("Failed to deserialize distributed connection metadata: {e}")
                        })?;
                    if info.room_id.as_ref() == Some(&room_id) {
                        online_users.insert(info.user_id);
                    }
                }

                if !online_users.is_empty() {
                    room_counts.push((room_id, online_users.len()));
                }
            }

            room_counts.sort_by_key(|(room_id, count)| (std::cmp::Reverse(*count), *room_id));
            return Ok(room_counts);
        }

        let mut room_counts: Vec<(RoomId, usize)> = self
            .room_connections
            .iter()
            .filter_map(|entry| {
                let users: HashSet<UserId> = entry
                    .value()
                    .iter()
                    .filter_map(|connection_id| {
                        self.connections.get(connection_id).map(|conn| conn.user_id)
                    })
                    .collect();
                let count = users.len();
                (count > 0).then_some((*entry.key(), count))
            })
            .collect();
        room_counts.sort_by_key(|(room_id, count)| (std::cmp::Reverse(*count), *room_id));
        Ok(room_counts)
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
        let mut has_user_metadata = false;
        let mut has_room_metadata = false;

        for entry in self.user_connections.iter() {
            if !entry.value().is_empty() {
                counter_keys.insert(format!(
                    "{}connections:user:{}",
                    self.redis_key_prefix,
                    entry.key()
                ));
                metadata_keys.insert(format!(
                    "{}conn_mgr:user:{}",
                    self.redis_key_prefix,
                    entry.key()
                ));
                has_user_metadata = true;
            }
        }
        for entry in self.room_connections.iter() {
            if !entry.value().is_empty() {
                counter_keys.insert(format!(
                    "{}connections:room:{}",
                    self.redis_key_prefix,
                    entry.key()
                ));
                metadata_keys.insert(format!(
                    "{}conn_mgr:room:{}",
                    self.redis_key_prefix,
                    entry.key()
                ));
                has_room_metadata = true;
            }
        }

        if has_user_metadata {
            metadata_keys.insert(self.user_index_directory_key());
        }
        if has_room_metadata {
            metadata_keys.insert(self.room_index_directory_key());
        }

        // Refresh per-connection metadata TTLs alongside aggregate counters.
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
                success_count = usize_to_u64_saturating(refreshed);
            }
            Err(e) => {
                failure_count = usize_to_u64_saturating(total_keys);
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

        let total_refreshed =
            usize_to_i64_saturating(counter_keys.len().saturating_add(metadata_keys.len()));
        synctv_core::metrics::cluster::DISTRIBUTED_COUNTER_TTL_KEYS_REFRESHED.set(total_refreshed);

        if !counter_keys.is_empty() || !metadata_keys.is_empty() {
            debug!(
                counter_keys = counter_keys.len(),
                metadata_keys = metadata_keys.len(),
                failures = failure_count,
                "Refreshed TTLs on distributed counters and connection metadata"
            );
        }

        // Repair this node's local contribution after the TTL refresh.
        // Global stale-index cleanup is intentionally not run on every tick:
        // crashed-pod state now drains via short metadata/index TTLs plus
        // lazy pruning on distributed read paths.
        self.sync_local_counts_to_redis(&mut conn).await;
        self.sync_connection_metadata_to_redis(&mut conn).await;
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
    ) -> Result<usize, String> {
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
            let mut script_invocation = BATCH_REFRESH_TTLS_SCRIPT.prepare_invoke();
            for key in &batch_keys {
                script_invocation.key(*key);
            }
            script_invocation
                .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
                .arg(CONNECTION_METADATA_TTL_SECONDS)
                .arg(usize_to_i64_saturating(batch_counter_count))
                .arg(usize_to_i64_saturating(batch_metadata_count));

            let refreshed: i64 = self
                .redis_op("refresh distributed counter TTL batch", async {
                    script_invocation.invoke_async(conn).await
                })
                .await?;
            total_refreshed += i64_to_usize_saturating(refreshed);

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
                let key = format!("{}connections:user:{}", self.redis_key_prefix, entry.key());
                user_counts.insert(key, count);
            }
        }

        let mut room_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for entry in self.room_connections.iter() {
            let count = entry.value().len();
            if count > 0 {
                let key = format!("{}connections:room:{}", self.redis_key_prefix, entry.key());
                room_counts.insert(key, count);
            }
        }

        let local_total = self.connection_count();
        let total_key = format!("{}connections:total", self.redis_key_prefix);

        // Lua script to atomically repair counters that are missing or lower than
        // this node's observed minimum contribution. It never decreases the
        // current Redis value because other replicas may have active
        // connections that are not visible from this node's local memory.
        // Returns `{current_value, 1}` when the counter was raised and
        // `{current_value, 0}` when no change was needed.
        let mut sync_count = 0u64;
        let mut sync_errors = 0u64;

        // Sync user counters
        for (key, local_count) in &user_counts {
            let script_result: Result<Vec<i64>, _> = self
                .redis_op("sync user connection counter", async {
                    SYNC_COUNTER_MIN_SCRIPT
                        .key(key)
                        .arg(usize_to_i64_saturating(*local_count))
                        .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
                        .invoke_async(conn)
                        .await
                })
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
            let script_result: Result<Vec<i64>, _> = self
                .redis_op("sync room connection counter", async {
                    SYNC_COUNTER_MIN_SCRIPT
                        .key(key)
                        .arg(usize_to_i64_saturating(*local_count))
                        .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
                        .invoke_async(conn)
                        .await
                })
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

        let script_result: Result<Vec<i64>, _> = self
            .redis_op("sync total connection counter", async {
                SYNC_COUNTER_MIN_SCRIPT
                    .key(&total_key)
                    .arg(usize_to_i64_saturating(local_total))
                    .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
                    .invoke_async(conn)
                    .await
            })
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
        let user_index_directory_key = self.user_index_directory_key();
        let room_index_directory_key = self.room_index_directory_key();
        let mut has_user_index = false;
        let mut has_room_index = false;

        for entry in self.connections.iter() {
            let conn_info = entry.value();
            let key = self.conn_metadata_key(&conn_info.connection_id);
            let user_index_key = self.user_index_key(conn_info.user_id);
            let room_index_key = conn_info
                .room_id
                .as_ref()
                .map(|room_id| self.room_index_key(room_id));
            let persistent = ConnectionInfoPersistent::from(conn_info);

            match serde_json::to_string(&persistent) {
                Ok(json_data) => {
                    let result: Result<(), _> = self
                        .redis_op(
                            "sync connection metadata",
                            conn.set_ex(
                                &key,
                                json_data,
                                i64_to_u64_saturating(CONNECTION_METADATA_TTL_SECONDS),
                            ),
                        )
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

            if let Err(e) = self
                .redis_op(
                    "repair user connection index membership",
                    conn.sadd::<_, _, ()>(&user_index_key, &conn_info.connection_id),
                )
                .await
            {
                errors += 1;
                warn!(
                    connection_id = %conn_info.connection_id,
                    user_id = %conn_info.user_id,
                    error = %e,
                    "Failed to repair user connection index membership in Redis"
                );
            }
            let _: Result<(), _> = self
                .redis_op(
                    "repair user connection index directory",
                    conn.sadd(&user_index_directory_key, &user_index_key),
                )
                .await;
            has_user_index = true;
            let _: Result<(), _> = self
                .redis_op(
                    "refresh user connection index TTL",
                    conn.expire(&user_index_key, CONNECTION_METADATA_TTL_SECONDS),
                )
                .await;

            if let Some(room_index_key) = room_index_key.as_ref() {
                if let Err(e) = self
                    .redis_op(
                        "repair room connection index membership",
                        conn.sadd::<_, _, ()>(room_index_key, &conn_info.connection_id),
                    )
                    .await
                {
                    errors += 1;
                    warn!(
                        connection_id = %conn_info.connection_id,
                        room_id = %room_index_key,
                        error = %e,
                        "Failed to repair room connection index membership in Redis"
                    );
                }
                let _: Result<(), _> = self
                    .redis_op(
                        "repair room connection index directory",
                        conn.sadd(&room_index_directory_key, room_index_key),
                    )
                    .await;
                has_room_index = true;
                let _: Result<(), _> = self
                    .redis_op(
                        "refresh room connection index TTL",
                        conn.expire(room_index_key, CONNECTION_METADATA_TTL_SECONDS),
                    )
                    .await;
            }
        }

        if has_user_index {
            let _: Result<(), _> = self
                .redis_op(
                    "refresh user connection index directory TTL",
                    conn.expire(&user_index_directory_key, CONNECTION_METADATA_TTL_SECONDS),
                )
                .await;
        }
        if has_room_index {
            let _: Result<(), _> = self
                .redis_op(
                    "refresh room connection index directory TTL",
                    conn.expire(&room_index_directory_key, CONNECTION_METADATA_TTL_SECONDS),
                )
                .await;
        }

        if synced > 0 || errors > 0 {
            debug!(
                metadata_synced = synced,
                metadata_errors = errors,
                "Synced connection metadata to Redis"
            );
        }
    }

    async fn load_index_directory_members(
        &self,
        conn: &mut redis::aio::ConnectionManager,
        directory_key: &str,
    ) -> Result<Vec<String>, String> {
        use redis::AsyncCommands;

        self.redis_op(
            "fetch distributed index directory",
            conn.smembers(directory_key),
        )
        .await
    }

    async fn prune_index_directory_members(
        &self,
        conn: &mut redis::aio::ConnectionManager,
        directory_key: &str,
        index_keys: &[String],
    ) -> Result<(), String> {
        if index_keys.is_empty() {
            return Ok(());
        }

        let mut pipe = redis::pipe();
        for index_key in index_keys {
            pipe.srem(directory_key, index_key).ignore();
        }

        self.redis_op("prune distributed index directory members", async {
            pipe.query_async::<()>(&mut *conn).await
        })
        .await
    }

    async fn load_valid_connection_ids_from_index(
        &self,
        conn: &mut redis::aio::ConnectionManager,
        index_key: &str,
        expected_user_id: Option<&UserId>,
        expected_room_id: Option<&RoomId>,
    ) -> Result<Vec<String>, String> {
        use redis::AsyncCommands;

        let conn_ids: Vec<String> = self
            .redis_op("fetch distributed connection index", async {
                conn.smembers(index_key).await
            })
            .await?;
        if conn_ids.is_empty() {
            return Ok(Vec::new());
        }

        let metadata_keys: Vec<String> = conn_ids
            .iter()
            .map(|conn_id| format!("{}conn_mgr:conn:{conn_id}", self.redis_key_prefix))
            .collect();
        let metadata: Vec<Option<String>> = self
            .redis_op("fetch distributed connection metadata", async {
                conn.mget(metadata_keys).await
            })
            .await?;

        let mut valid_conn_ids = Vec::with_capacity(conn_ids.len());
        let mut stale_members = Vec::new();

        for (conn_id, metadata_json) in conn_ids.into_iter().zip(metadata) {
            match metadata_json {
                Some(metadata_json) => {
                    match serde_json::from_str::<ConnectionInfoPersistent>(&metadata_json) {
                        Ok(info) => {
                            let matches_user =
                                expected_user_id.is_none_or(|user_id| info.user_id == *user_id);
                            let matches_room = expected_room_id
                                .is_none_or(|room_id| info.room_id.as_ref() == Some(room_id));

                            if matches_user && matches_room {
                                valid_conn_ids.push(conn_id);
                            } else {
                                stale_members.push(conn_id);
                            }
                        }
                        Err(error) => {
                            warn!(
                                index_key = %index_key,
                                connection_id = %conn_id,
                                error = %error,
                                "Failed to deserialize distributed connection metadata; pruning index member"
                            );
                            stale_members.push(conn_id);
                        }
                    }
                }
                None => {
                    stale_members.push(conn_id);
                }
            }
        }

        if !stale_members.is_empty() {
            let mut pipe = redis::pipe();
            for connection_id in &stale_members {
                pipe.srem(index_key, connection_id).ignore();
            }

            match self
                .redis_op("prune stale distributed connection index members", async {
                    pipe.query_async::<()>(&mut *conn).await
                })
                .await
            {
                Ok(()) => {
                    debug!(
                        index_key = %index_key,
                        removed_members = stale_members.len(),
                        "Pruned stale distributed connection index members on read"
                    );
                }
                Err(error) => {
                    warn!(
                        index_key = %index_key,
                        removed_members = stale_members.len(),
                        error = %error,
                        "Failed to prune stale distributed connection index members on read"
                    );
                }
            }
        }

        Ok(valid_conn_ids)
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

        let directories = [
            self.user_index_directory_key(),
            self.room_index_directory_key(),
        ];
        let mut cleaned = 0u64;
        let mut errors = 0u64;

        for directory_key in directories {
            let index_keys = match self
                .load_index_directory_members(conn, &directory_key)
                .await
            {
                Ok(index_keys) => index_keys,
                Err(error) => {
                    errors += 1;
                    warn!(
                        directory_key = %directory_key,
                        error = %error,
                        "Failed to load distributed connection index directory during reconciliation"
                    );
                    continue;
                }
            };

            let mut stale_directory_members = Vec::new();

            for key in index_keys {
                let members: Result<Vec<String>, _> = self
                    .redis_op("fetch Redis index members", conn.smembers(&key))
                    .await;
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
                    let conn_key = self.conn_metadata_key(&conn_id);
                    let exists: Result<bool, _> = self
                        .redis_op(
                            "verify distributed connection metadata",
                            conn.exists(&conn_key),
                        )
                        .await;
                    match exists {
                        Ok(true) => {}
                        Ok(false) => {
                            let remove_result: Result<(), _> = self
                                .redis_op(
                                    "remove stale distributed connection index member",
                                    conn.srem(&key, &conn_id),
                                )
                                .await;
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

                let key_is_empty: Result<bool, _> = self
                    .redis_op(
                        "check Redis index cardinality",
                        conn.scard::<_, usize>(&key),
                    )
                    .await
                    .map(|count| count == 0);
                match key_is_empty {
                    Ok(true) => {
                        let _: Result<(), _> = self
                            .redis_op("delete empty Redis index", conn.del(&key))
                            .await;
                        stale_directory_members.push(key);
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

            if let Err(error) = self
                .prune_index_directory_members(conn, &directory_key, &stale_directory_members)
                .await
            {
                errors += 1;
                warn!(
                    directory_key = %directory_key,
                    error = %error,
                    "Failed to prune stale distributed connection directory members"
                );
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
                if let Some(rtc_joined_at) = conn.rtc_joined_at {
                    self.schedule_rtc_timeout(conn_id, rtc_joined_at);
                } else {
                    self.clear_rtc_timeout(conn_id);
                }
                debug!(
                    connection_id = %conn_id,
                    user_id = %user_id,
                    room_id = %room_id,
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
    /// instead of silently degrading to local-only state. In distributed mode,
    /// a local fallback would return a partial view and break admin/security
    /// operations that require a global connection set.
    pub async fn get_user_connections_distributed(
        &self,
        user_id: &UserId,
    ) -> Result<Vec<String>, String> {
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let user_index_key = format!("{}conn_mgr:user:{}", self.redis_key_prefix, user_id);

            match self
                .load_valid_connection_ids_from_index(
                    &mut conn,
                    &user_index_key,
                    Some(user_id),
                    None,
                )
                .await
            {
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
    /// In standalone mode this uses local in-memory state. In distributed mode it
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
            let room_index_key = format!("{}conn_mgr:room:{}", self.redis_key_prefix, room_id);

            match self
                .load_valid_connection_ids_from_index(
                    &mut conn,
                    &room_index_key,
                    None,
                    Some(room_id),
                )
                .await
            {
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

            let metadata: Vec<Option<String>> = self
                .redis_op(
                    "fetch distributed connection metadata",
                    conn.mget(metadata_keys),
                )
                .await?;

            let mut count = 0usize;
            for entry in metadata.into_iter().flatten() {
                let info: ConnectionInfoPersistent = serde_json::from_str(&entry).map_err(|e| {
                    format!("Failed to deserialize distributed connection metadata: {e}")
                })?;
                if info.user_id == *user_id && info.room_id.as_ref() == Some(room_id) {
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
    /// In Redis-backed distributed mode this reads connection metadata from Redis so the
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

            let metadata: Vec<Option<String>> = self
                .redis_op(
                    "fetch distributed connection metadata",
                    conn.mget(metadata_keys),
                )
                .await?;

            for entry in metadata.into_iter().flatten() {
                match serde_json::from_str::<ConnectionInfoPersistent>(&entry) {
                    Ok(info) => {
                        if info.room_id.as_ref() == Some(room_id) {
                            return Ok(true);
                        }
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            user_id = %user_id,
                            room_id = %room_id,
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
        let count: i64 = self
            .redis_op("increment distributed counter", async {
                INCR_EXPIRE_SCRIPT
                    .key(key)
                    .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
                    .invoke_async(&mut conn)
                    .await
            })
            .await?;

        Ok(count <= usize_to_i64_saturating(max))
    }

    /// Decrement a Redis counter atomically (best-effort, errors are logged but not propagated).
    ///
    /// Uses a Lua script to atomically DECR and DEL if the result is negative,
    /// avoiding a race where a concurrent INCR between DECR and SET(0) would be lost.
    async fn redis_decr(&self, key: &str) -> Result<(), String> {
        let Some(mut conn) = self.redis_conn_snapshot().await else {
            return Err("Redis not configured".to_string());
        };
        self.redis_op("decrement distributed counter", async {
            DECR_DELETE_NEGATIVE_SCRIPT
                .key(key)
                .invoke_async::<i64>(&mut conn)
                .await
        })
        .await?;
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

    pub(crate) const fn all_clean(&self) -> bool {
        matches!(
            (
                self.ttl_refresh.as_ref(),
                self.pending_retries.as_ref(),
                self.disconnect_retry.as_ref(),
            ),
            (
                None | Some(ShutdownTaskOutcome::Completed | ShutdownTaskOutcome::Cancelled),
                None | Some(ShutdownTaskOutcome::Completed | ShutdownTaskOutcome::Cancelled),
                None | Some(ShutdownTaskOutcome::Completed | ShutdownTaskOutcome::Cancelled)
            )
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_core::config::ConnectionLimitsConfig;
    use synctv_core_testing::{start_redis_url_with_label, RedisContainer};

    impl ConnectionManager {
        fn users_online_metric_delta_for_test(&self) -> isize {
            self.users_online_metric_increments
                .load(Ordering::Relaxed)
                .saturating_sub(self.users_online_metric_decrements.load(Ordering::Relaxed))
                .cast_signed()
        }

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

    #[test]
    fn test_connection_limits_default_tracks_core_config() {
        let core = ConnectionLimitsConfig::default();
        let realtime = ConnectionLimits::default();

        assert_eq!(realtime.max_per_user, core.max_per_user);
        assert_eq!(realtime.max_per_room, core.max_per_room);
        assert_eq!(realtime.max_total, core.max_total);
        assert_eq!(
            realtime.idle_timeout,
            Duration::from_secs(core.idle_timeout_seconds)
        );
        assert_eq!(
            realtime.max_duration,
            Duration::from_secs(core.max_duration_seconds)
        );
    }

    #[tokio::test]
    async fn test_register_connection() {
        let manager = ConnectionManager::default();
        let user_id = UserId::expect_positive(10_000_010);

        let result = manager.register("conn1".to_string(), user_id).await;
        assert!(result.is_ok());
        assert_eq!(manager.connection_count(), 1);
        assert_eq!(manager.user_connection_count(&user_id), 1);
    }

    #[tokio::test]
    async fn test_register_duplicate_connection_id_is_rejected_without_double_counting() {
        let manager = ConnectionManager::default();
        let user_id = UserId::expect_positive(10_000_110);

        manager
            .register("dup-conn".to_string(), user_id)
            .await
            .expect("first register should succeed");

        let duplicate = manager.register("dup-conn".to_string(), user_id).await;
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
    async fn test_duplicate_register_fails_fast_while_first_attempt_holds_lifecycle_lock() {
        let first_entered = Arc::new(tokio::sync::Notify::new());
        let release_first = Arc::new(tokio::sync::Notify::new());
        let manager = Arc::new(
            ConnectionManager::default().with_register_after_lifecycle_lock_hook({
                let first_entered = Arc::clone(&first_entered);
                let release_first = Arc::clone(&release_first);
                Arc::new(move || {
                    let first_entered = Arc::clone(&first_entered);
                    let release_first = Arc::clone(&release_first);
                    Box::pin(async move {
                        first_entered.notify_waiters();
                        release_first.notified().await;
                    })
                })
            }),
        );
        let user_id = UserId::expect_positive(10_000_111);

        let first = {
            let manager = Arc::clone(&manager);
            tokio::spawn(async move { manager.register("dup-fast".to_string(), user_id).await })
        };

        first_entered.notified().await;

        let duplicate = tokio::time::timeout(
            Duration::from_millis(100),
            manager.register("dup-fast".to_string(), user_id),
        )
        .await
        .expect("duplicate registration must fail fast instead of waiting on lifecycle lock");
        let duplicate_err = duplicate.expect_err("duplicate registration must be rejected");
        assert!(
            duplicate_err.contains("already registered"),
            "duplicate registration should surface the existing claim error: {duplicate_err}"
        );

        release_first.notify_waiters();
        first
            .await
            .expect("first registration join")
            .expect("first registration should complete");
        assert_eq!(manager.connection_count(), 1);
        assert_eq!(manager.user_connection_count(&user_id), 1);
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
        let cleanup_op = manager.unregister_cleanup_op(
            "conn-123",
            "token-123",
            UserId::expect_positive(40_123_001),
            Some(RoomId::expect_positive(40_123_002)),
        );

        manager.enqueue_pending_retry_for_test(cleanup_op.clone());

        assert_eq!(
            manager.drain_pending_retries_for_test(),
            vec![cleanup_op],
            "metadata, index, and counter cleanup retries must be retained as one idempotent operation"
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
        let user1 = UserId::expect_positive(10_000_112);
        let user2 = UserId::expect_positive(10_000_113);

        let task1 = {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                manager.register("dup-race-conn".to_string(), user1).await
            })
        };
        let task2 = {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
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
        let user_id = UserId::expect_positive(10_000_010);

        // First two should succeed
        assert!(manager.register("conn1".to_string(), user_id).await.is_ok());
        assert!(manager.register("conn2".to_string(), user_id).await.is_ok());

        // Third should fail
        let result = manager.register("conn3".to_string(), user_id).await;
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
        let user_id = UserId::expect_positive(10_000_114);
        let barrier = Arc::new(tokio::sync::Barrier::new(3));

        let task1 = {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                manager.register("conn-race-1".to_string(), user_id).await
            })
        };
        let task2 = {
            let manager = Arc::clone(&manager);
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
        let user_id = UserId::expect_positive(10_000_010);
        let room_id = RoomId::expect_positive(10_000_092);

        manager
            .register("conn1".to_string(), user_id)
            .await
            .unwrap();

        let result = manager.join_room("conn1", room_id).await;
        assert!(result.is_ok());
        assert_eq!(manager.room_connection_count(&room_id), 1);

        let conn = manager.get_connection("conn1").unwrap();
        assert_eq!(conn.room_id.as_ref(), Some(&room_id));
    }

    #[tokio::test]
    async fn test_has_other_connection_for_user_in_room_distributed_uses_local_state_without_redis()
    {
        let manager = ConnectionManager::default();
        let user_id = UserId::expect_positive(10_000_010);
        let room_id = RoomId::expect_positive(10_000_092);

        manager
            .register("conn1".to_string(), user_id)
            .await
            .unwrap();
        manager
            .register("conn2".to_string(), user_id)
            .await
            .unwrap();
        manager.join_room("conn1", room_id).await.unwrap();
        manager.join_room("conn2", room_id).await.unwrap();

        let has_other = manager
            .has_other_connection_for_user_in_room_distributed(&user_id, &room_id, "conn1")
            .await
            .unwrap();

        assert!(has_other, "second local room connection should be detected");
    }

    #[tokio::test]
    async fn test_has_other_connection_for_user_in_room_distributed_ignores_other_rooms() {
        let manager = ConnectionManager::default();
        let user_id = UserId::expect_positive(10_000_010);
        let room_id = RoomId::expect_positive(10_000_092);
        let other_room_id = RoomId::expect_positive(10_000_094);

        manager
            .register("conn1".to_string(), user_id)
            .await
            .unwrap();
        manager
            .register("conn2".to_string(), user_id)
            .await
            .unwrap();
        manager.join_room("conn1", room_id).await.unwrap();
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
        let user_id = UserId::expect_positive(10_000_010);
        let room_id = RoomId::expect_positive(10_000_092);

        manager
            .register("conn1".to_string(), user_id)
            .await
            .unwrap();
        manager
            .register("conn2".to_string(), user_id)
            .await
            .unwrap();
        manager.join_room("conn1", room_id).await.unwrap();
        manager.join_room("conn2", room_id).await.unwrap();

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
        let room_id = RoomId::expect_positive(10_000_092);

        // Register two connections and join room
        let user1 = UserId::expect_positive(10_000_010);
        let user2 = UserId::expect_positive(10_000_095);
        let user3 = UserId::expect_positive(10_000_115);

        manager.register("conn1".to_string(), user1).await.unwrap();
        manager.register("conn2".to_string(), user2).await.unwrap();
        manager.register("conn3".to_string(), user3).await.unwrap();

        assert!(manager.join_room("conn1", room_id).await.is_ok());
        assert!(manager.join_room("conn2", room_id).await.is_ok());

        // Third should fail
        let result = manager.join_room("conn3", room_id).await;
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
        let room_id = RoomId::expect_positive(10_000_116);

        manager
            .register(
                "conn-room-race-1".to_string(),
                UserId::expect_positive(10_000_117),
            )
            .await
            .expect("first registration");
        manager
            .register(
                "conn-room-race-2".to_string(),
                UserId::expect_positive(10_000_118),
            )
            .await
            .expect("second registration");

        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let join1 = {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                manager.join_room("conn-room-race-1", room_id).await
            })
        };
        let join2 = {
            let manager = Arc::clone(&manager);
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
    async fn test_concurrent_room_switch_for_same_connection_keeps_single_room_membership() {
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let manager = Arc::new(
            ConnectionManager::default().with_join_room_before_commit_hook({
                let barrier = Arc::clone(&barrier);
                Arc::new(move || {
                    let barrier = Arc::clone(&barrier);
                    Box::pin(async move {
                        barrier.wait().await;
                    })
                })
            }),
        );
        let user_id = UserId::expect_positive(10_000_119);
        let room_a = RoomId::expect_positive(10_000_120);
        let room_b = RoomId::expect_positive(10_000_121);

        manager
            .register("conn-switch".to_string(), user_id)
            .await
            .expect("registration");

        let join_a = {
            let manager = Arc::clone(&manager);
            tokio::spawn(async move { manager.join_room("conn-switch", room_a).await })
        };
        let join_b = {
            let manager = Arc::clone(&manager);
            tokio::spawn(async move { manager.join_room("conn-switch", room_b).await })
        };

        barrier.wait().await;

        join_a.await.expect("join_a task").expect("join_a");
        join_b.await.expect("join_b task").expect("join_b");

        let conn = manager
            .get_connection("conn-switch")
            .expect("connection should exist after room switch race");
        let final_room = conn.room_id.expect("connection should belong to one room");

        let room_a_connections = manager.get_room_connections(&room_a);
        let room_b_connections = manager.get_room_connections(&room_b);
        let rooms_with_connection = usize::from(
            room_a_connections
                .iter()
                .any(|info| info.connection_id == "conn-switch"),
        ) + usize::from(
            room_b_connections
                .iter()
                .any(|info| info.connection_id == "conn-switch"),
        );

        assert_eq!(
            rooms_with_connection, 1,
            "same connection_id must not remain indexed in multiple rooms after concurrent switches"
        );

        if final_room == room_a {
            assert_eq!(
                room_a_connections.len(),
                1,
                "final room must retain the connection exactly once"
            );
            assert!(
                room_b_connections.is_empty(),
                "non-final room must not retain a stale connection index"
            );
        } else {
            assert_eq!(
                final_room, room_b,
                "final room must be one of the two concurrently requested rooms"
            );
            assert_eq!(
                room_b_connections.len(),
                1,
                "final room must retain the connection exactly once"
            );
            assert!(
                room_a_connections.is_empty(),
                "non-final room must not retain a stale connection index"
            );
        }
    }

    #[tokio::test]
    async fn test_unregister_is_not_blocked_by_join_room_waiting_on_capacity_check() {
        let join_entered = Arc::new(tokio::sync::Notify::new());
        let release_join = Arc::new(tokio::sync::Notify::new());
        let manager = Arc::new(
            ConnectionManager::default().with_join_room_before_capacity_check_hook({
                let join_entered = Arc::clone(&join_entered);
                let release_join = Arc::clone(&release_join);
                Arc::new(move || {
                    let join_entered = Arc::clone(&join_entered);
                    let release_join = Arc::clone(&release_join);
                    Box::pin(async move {
                        join_entered.notify_waiters();
                        release_join.notified().await;
                    })
                })
            }),
        );
        let user_id = UserId::expect_positive(10_000_122);
        let room_id = RoomId::expect_positive(10_000_123);

        manager
            .register("conn-unregister-race".to_string(), user_id)
            .await
            .expect("registration should succeed");

        let join_task = {
            let manager = Arc::clone(&manager);
            tokio::spawn(async move { manager.join_room("conn-unregister-race", room_id).await })
        };

        join_entered.notified().await;

        tokio::time::timeout(
            Duration::from_millis(100),
            manager.unregister("conn-unregister-race"),
        )
        .await
        .expect("unregister must not wait behind join_room capacity checks");

        assert!(
            manager.get_connection("conn-unregister-race").is_none(),
            "unregister should remove the connection immediately"
        );
        assert_eq!(
            manager.user_connection_count(&user_id),
            0,
            "unregister should free the per-user slot immediately"
        );

        release_join.notify_waiters();
        let join_err = join_task
            .await
            .expect("join task join")
            .expect_err("join_room should observe that the connection was unregistered");
        assert_eq!(join_err, "Connection not found");
        assert_eq!(manager.room_connection_count(&room_id), 0);
    }

    #[tokio::test]
    async fn test_record_message() {
        let manager = ConnectionManager::default();
        let user_id = UserId::expect_positive(10_000_010);

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
        let user_id = UserId::expect_positive(10_000_010);

        manager
            .register("conn1".to_string(), user_id)
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
        let user_id = UserId::expect_positive(10_000_124);

        manager
            .register("user-count-1".to_string(), user_id)
            .await
            .unwrap();
        manager
            .register("user-count-2".to_string(), user_id)
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
        let user_id = UserId::expect_positive(10_000_010);
        let room_id = RoomId::expect_positive(10_000_092);

        manager
            .register("conn1".to_string(), user_id)
            .await
            .unwrap();
        manager.join_room("conn1", room_id).await.unwrap();

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
        let user_id = UserId::expect_positive(10_000_010);
        let room_id = RoomId::expect_positive(10_000_092);

        manager
            .register("conn1".to_string(), user_id)
            .await
            .unwrap();
        manager
            .register("conn2".to_string(), user_id)
            .await
            .unwrap();
        manager.join_room("conn1", room_id).await.unwrap();
        manager.join_room("conn2", room_id).await.unwrap();

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
        let user_id = UserId::expect_positive(10_000_125);
        let room_id = RoomId::expect_positive(10_000_126);
        let other_room_id = RoomId::expect_positive(10_000_127);

        manager
            .register("room-count-1".to_string(), user_id)
            .await
            .unwrap();
        manager
            .register("room-count-2".to_string(), user_id)
            .await
            .unwrap();
        manager
            .register("room-count-3".to_string(), user_id)
            .await
            .unwrap();
        manager.join_room("room-count-1", room_id).await.unwrap();
        manager.join_room("room-count-2", room_id).await.unwrap();
        manager
            .join_room("room-count-3", other_room_id)
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
        let user_id = UserId::expect_positive(10_000_010);
        let room_id = RoomId::expect_positive(10_000_092);

        manager
            .register("conn1".to_string(), user_id)
            .await
            .unwrap();
        manager.join_room("conn1", room_id).await.unwrap();

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
        let user_id = UserId::expect_positive(10_000_128);

        manager
            .register("metric-conn-1".to_string(), user_id)
            .await
            .unwrap();
        assert_eq!(
            manager.users_online_metric_delta_for_test(),
            1,
            "first connection for a user should increase online user count"
        );

        manager
            .register("metric-conn-2".to_string(), user_id)
            .await
            .unwrap();
        assert_eq!(
            manager.users_online_metric_delta_for_test(),
            1,
            "second connection for the same user must not double-count online users"
        );

        manager.unregister("metric-conn-1").await;
        manager.unregister("metric-conn-2").await;
        assert_eq!(manager.users_online_metric_delta_for_test(), 0);
    }

    #[tokio::test]
    async fn test_users_online_metric_decrements_only_after_last_connection_leaves() {
        let manager = ConnectionManager::default();
        let user_id = UserId::expect_positive(10_000_129);

        manager
            .register("metric-last-1".to_string(), user_id)
            .await
            .unwrap();
        manager
            .register("metric-last-2".to_string(), user_id)
            .await
            .unwrap();

        manager.unregister("metric-last-1").await;
        assert_eq!(
            manager.users_online_metric_delta_for_test(),
            1,
            "user should remain online while another connection is still active"
        );

        manager.unregister("metric-last-2").await;
        assert_eq!(
            manager.users_online_metric_delta_for_test(),
            0,
            "online user count should drop only after the final connection closes"
        );
    }

    #[tokio::test]
    async fn test_metrics() {
        let manager = ConnectionManager::default();
        let user1 = UserId::expect_positive(10_000_010);
        let user2 = UserId::expect_positive(10_000_095);

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
        let user_id = UserId::expect_positive(10_000_010);

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

    #[tokio::test]
    async fn test_record_message_refreshes_idle_deadline() {
        let limits = ConnectionLimits {
            idle_timeout: Duration::from_millis(100),
            ..Default::default()
        };
        let manager = ConnectionManager::new(limits);
        let user_id = UserId::expect_positive(10_000_010);

        manager
            .register("conn1".to_string(), user_id)
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(60)).await;
        manager.record_message("conn1");
        tokio::time::sleep(Duration::from_millis(60)).await;

        assert!(
            manager.check_timeouts().is_empty(),
            "fresh activity should postpone idle timeout"
        );

        tokio::time::sleep(Duration::from_millis(60)).await;
        let timeouts = manager.check_timeouts();
        assert_eq!(timeouts, vec!["conn1".to_string()]);
    }

    #[tokio::test]
    async fn test_rtc_timeout_marks_connection_left_before_disconnect() {
        let limits = ConnectionLimits {
            idle_timeout: Duration::from_secs(10),
            max_duration: Duration::from_secs(10),
            webrtc_session_timeout: Duration::from_millis(100),
            ..Default::default()
        };
        let manager = ConnectionManager::new(limits);
        let user_id = UserId::expect_positive(10_000_010);
        let room_id = RoomId::expect_positive(10_000_092);

        manager
            .register("conn1".to_string(), user_id)
            .await
            .unwrap();
        manager.join_room("conn1", room_id).await.unwrap();
        manager.mark_rtc_joined(&room_id, &user_id, "conn1", true);

        tokio::time::sleep(Duration::from_millis(150)).await;

        let timeouts = manager.check_timeouts();
        assert_eq!(timeouts, vec!["conn1".to_string()]);
        assert!(
            manager.get_rtc_connections(&room_id).is_empty(),
            "RTC timeout should clear joined state before disconnect handling"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker Redis"]
    async fn test_redis_recovery_reconciles_connection_counts() {
        // This test verifies that after a Redis outage, the ConnectionManager
        // reconciles in-memory connection counts with Redis.

        // Setup: Create manager with Redis
        use redis::AsyncCommands;

        let (_container, client, conn, prefix) = docker_redis_connection("test:").await;
        let manager = ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix);

        let user_id = UserId::expect_positive(10_000_010);
        let room_id = RoomId::expect_positive(10_000_092);
        let user_key = format!("{prefix}connections:user:{user_id}");
        let room_key = format!("{prefix}connections:room:{room_id}");

        // Register connections
        manager
            .register("conn1".to_string(), user_id)
            .await
            .unwrap();
        manager.join_room("conn1", room_id).await.unwrap();

        // Verify Redis has the counts
        let mut redis_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();
        let user_count: i64 = redis_conn.get(&user_key).await.unwrap_or(0);
        assert_eq!(user_count, 1);

        // Simulate Redis outage by clearing Redis keys manually
        // (In real scenario, Redis would be down)
        let _: () = redis_conn.del(&user_key).await.unwrap();
        let _: () = redis_conn.del(&room_key).await.unwrap();

        // At this point, local state has 1 connection but Redis has 0
        assert_eq!(manager.user_connection_count(&user_id), 1);

        // Trigger reconciliation
        manager.reconcile_with_redis().await;

        // After reconciliation, Redis should match local state
        let user_count: i64 = redis_conn.get(&user_key).await.unwrap_or(0);
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
        let user_index_directory_key = format!("{prefix}{USER_INDEX_DIRECTORY_KEY_SUFFIX}");
        let room_index_directory_key = format!("{prefix}{ROOM_INDEX_DIRECTORY_KEY_SUFFIX}");

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
        let _: () = redis_conn
            .sadd(&user_index_directory_key, &stale_user_index)
            .await
            .unwrap();
        let _: () = redis_conn
            .sadd(&room_index_directory_key, &stale_room_index)
            .await
            .unwrap();

        // Also create a metadata key that belongs to another replica. Reconciliation
        // on this node must not delete it just because it is absent from local memory.
        let foreign_meta = ConnectionInfoPersistent {
            connection_id: "other_node_conn".to_string(),
            registration_token: "foreign-token".to_string(),
            user_id: UserId::expect_positive(20_000_201),
            actor_id: "usr_foreign".to_string(),
            room_id: Some(RoomId::expect_positive(20_000_202)),
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

        let user_directory_members: Vec<String> = redis_conn
            .smembers(&user_index_directory_key)
            .await
            .unwrap();
        let room_directory_members: Vec<String> = redis_conn
            .smembers(&room_index_directory_key)
            .await
            .unwrap();
        assert!(
            user_directory_members.is_empty(),
            "stale user index directory entry should be pruned during reconciliation"
        );
        assert!(
            room_directory_members.is_empty(),
            "stale room index directory entry should be pruned during reconciliation"
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

        let user_id = UserId::expect_positive(10_000_010);
        let user_key = format!("{prefix}connections:user:{user_id}");

        // Register a connection (should succeed and write to Redis)
        manager
            .register("conn1".to_string(), user_id)
            .await
            .unwrap();

        // Verify Redis counter
        let mut redis_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();
        let user_count: i64 = redis_conn.get(&user_key).await.unwrap_or(0);
        assert_eq!(user_count, 1);

        // Manually corrupt the counter (simulating partial failure)
        let _: () = redis_conn.set(&user_key, 0).await.unwrap();

        // Local state says 1, Redis says 0
        assert_eq!(manager.user_connection_count(&user_id), 1);

        // Trigger reconciliation
        manager.reconcile_with_redis().await;

        // After reconciliation, Redis should be corrected
        let user_count: i64 = redis_conn.get(&user_key).await.unwrap_or(0);
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
        let user_key = format!("{prefix}connections:user:20000101");
        let room_key = format!("{prefix}connections:room:20000102");
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
    async fn test_distributed_queries_prune_stale_index_members() {
        use redis::AsyncCommands;

        let (_container, client, conn, prefix) = docker_redis_connection("test6:").await;
        let manager = ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix);

        let user_a = UserId::expect_positive(10_000_130);
        let room_a = RoomId::expect_positive(10_000_120);
        let stale_missing = "conn-missing";
        let stale_mismatch = "conn-mismatch";
        let valid = "conn-valid";

        let mut redis_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();
        let user_index_key = format!("{prefix}conn_mgr:user:{user_a}");
        let room_index_key = format!("{prefix}conn_mgr:room:{room_a}");
        let mismatch_conn_key = format!("{prefix}conn_mgr:conn:{stale_mismatch}");
        let valid_conn_key = format!("{prefix}conn_mgr:conn:{valid}");

        let mismatch_metadata = ConnectionInfoPersistent {
            connection_id: stale_mismatch.to_string(),
            registration_token: "mismatch-token".to_string(),
            user_id: UserId::expect_positive(10_000_131),
            actor_id: "usr_mismatch".to_string(),
            room_id: Some(RoomId::expect_positive(10_000_121)),
            connected_at_unix: 0,
            last_activity_unix: 0,
            message_count: 0,
            rtc_joined: false,
            rtc_joined_at_unix: None,
        };
        let valid_metadata = ConnectionInfoPersistent {
            connection_id: valid.to_string(),
            registration_token: "valid-token".to_string(),
            user_id: user_a,
            actor_id: "usr_valid".to_string(),
            room_id: Some(room_a),
            connected_at_unix: 0,
            last_activity_unix: 0,
            message_count: 0,
            rtc_joined: false,
            rtc_joined_at_unix: None,
        };

        let _: () = redis_conn
            .set(
                &mismatch_conn_key,
                serde_json::to_string(&mismatch_metadata).unwrap(),
            )
            .await
            .unwrap();
        let _: () = redis_conn
            .set(
                &valid_conn_key,
                serde_json::to_string(&valid_metadata).unwrap(),
            )
            .await
            .unwrap();

        for conn_id in [stale_missing, stale_mismatch, valid] {
            let _: () = redis_conn.sadd(&user_index_key, conn_id).await.unwrap();
            let _: () = redis_conn.sadd(&room_index_key, conn_id).await.unwrap();
        }
        let _: () = redis_conn
            .expire(&user_index_key, CONNECTION_METADATA_TTL_SECONDS)
            .await
            .unwrap();
        let _: () = redis_conn
            .expire(&room_index_key, CONNECTION_METADATA_TTL_SECONDS)
            .await
            .unwrap();
        let _: () = redis_conn
            .expire(&mismatch_conn_key, CONNECTION_METADATA_TTL_SECONDS)
            .await
            .unwrap();
        let _: () = redis_conn
            .expire(&valid_conn_key, CONNECTION_METADATA_TTL_SECONDS)
            .await
            .unwrap();

        let mut user_connections = manager
            .get_user_connections_distributed(&user_a)
            .await
            .expect("distributed user lookup should succeed");
        let mut room_connections = manager
            .get_room_connections_distributed(&room_a)
            .await
            .expect("distributed room lookup should succeed");
        user_connections.sort();
        room_connections.sort();

        assert_eq!(
            user_connections,
            vec![valid.to_string()],
            "distributed user lookup must prune missing and mismatched index members"
        );
        assert_eq!(
            room_connections,
            vec![valid.to_string()],
            "distributed room lookup must prune missing and mismatched index members"
        );

        let mut user_members: Vec<String> = redis_conn.smembers(&user_index_key).await.unwrap();
        let mut room_members: Vec<String> = redis_conn.smembers(&room_index_key).await.unwrap();
        user_members.sort();
        room_members.sort();
        assert_eq!(
            user_members,
            vec![valid.to_string()],
            "user index should retain only valid members after lazy pruning"
        );
        assert_eq!(
            room_members,
            vec![valid.to_string()],
            "room index should retain only valid members after lazy pruning"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker Redis"]
    async fn test_connection_metadata_ttl_uses_short_crash_safety_window() {
        use redis::AsyncCommands;

        let (_container, client, conn, prefix) = docker_redis_connection("test7:").await;
        let manager = ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix);

        let user_id = UserId::expect_positive(10_000_131);
        let room_id = RoomId::expect_positive(10_000_132);

        manager
            .register("conn-meta-ttl".to_string(), user_id)
            .await
            .unwrap();
        manager.join_room("conn-meta-ttl", room_id).await.unwrap();

        let mut redis_conn = redis::aio::ConnectionManager::new(client).await.unwrap();
        for key in [
            format!("{prefix}conn_mgr:conn:conn-meta-ttl"),
            format!("{prefix}conn_mgr:user:{user_id}"),
            format!("{prefix}conn_mgr:room:{room_id}"),
            format!("{prefix}{USER_INDEX_DIRECTORY_KEY_SUFFIX}"),
            format!("{prefix}{ROOM_INDEX_DIRECTORY_KEY_SUFFIX}"),
        ] {
            let ttl: i64 = redis_conn.ttl(&key).await.unwrap();
            assert!(
                (CONNECTION_METADATA_TTL_SECONDS - 5..=CONNECTION_METADATA_TTL_SECONDS)
                    .contains(&ttl),
                "metadata/index key {key} should use the short crash-safety TTL, got {ttl}s"
            );
        }

        manager.unregister("conn-meta-ttl").await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker Redis"]
    async fn test_reconcile_with_redis_repairs_missing_user_and_room_index_memberships() {
        use redis::AsyncCommands;

        let (_container, client, conn, prefix) = docker_redis_connection("test8:").await;
        let manager = ConnectionManager::new(ConnectionLimits::default()).with_redis(conn, &prefix);

        let user_id = UserId::expect_positive(10_000_133);
        let room_id = RoomId::expect_positive(10_000_134);
        let user_index_key = format!("{prefix}conn_mgr:user:{user_id}");
        let room_index_key = format!("{prefix}conn_mgr:room:{room_id}");
        let user_index_directory_key = format!("{prefix}{USER_INDEX_DIRECTORY_KEY_SUFFIX}");
        let room_index_directory_key = format!("{prefix}{ROOM_INDEX_DIRECTORY_KEY_SUFFIX}");

        manager
            .register("conn-repair".to_string(), user_id)
            .await
            .unwrap();
        manager.join_room("conn-repair", room_id).await.unwrap();

        let mut redis_conn = redis::aio::ConnectionManager::new(client).await.unwrap();
        let _: () = redis_conn.del(&user_index_key).await.unwrap();
        let _: () = redis_conn.del(&room_index_key).await.unwrap();
        let _: () = redis_conn
            .srem(&user_index_directory_key, &user_index_key)
            .await
            .unwrap();
        let _: () = redis_conn
            .srem(&room_index_directory_key, &room_index_key)
            .await
            .unwrap();

        manager.reconcile_with_redis().await;

        let user_connections = manager
            .get_user_connections_distributed(&user_id)
            .await
            .expect("reconciled distributed user lookup should succeed");
        let room_connections = manager
            .get_room_connections_distributed(&room_id)
            .await
            .expect("reconciled distributed room lookup should succeed");
        assert_eq!(
            user_connections,
            vec!["conn-repair".to_string()],
            "reconciliation should restore missing user index membership"
        );
        assert_eq!(
            room_connections,
            vec!["conn-repair".to_string()],
            "reconciliation should restore missing room index membership"
        );

        let user_directory_members: Vec<String> = redis_conn
            .smembers(&user_index_directory_key)
            .await
            .unwrap();
        let room_directory_members: Vec<String> = redis_conn
            .smembers(&room_index_directory_key)
            .await
            .unwrap();
        assert_eq!(
            user_directory_members,
            vec![user_index_key.clone()],
            "reconciliation should restore the user index directory entry"
        );
        assert_eq!(
            room_directory_members,
            vec![room_index_key.clone()],
            "reconciliation should restore the room index directory entry"
        );

        manager.unregister("conn-repair").await;
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
        let user_id = UserId::expect_positive(10_000_135);
        let user_key = format!("{prefix}connections:user:{user_id}");

        manager
            .register("conn1".to_string(), user_id)
            .await
            .unwrap();

        let second = manager.register("conn2".to_string(), user_id).await;
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
        let user_count: i64 = redis_conn.get(&user_key).await.unwrap_or(0);

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
                UserId::expect_positive(10_000_136),
            )
            .await
            .unwrap();
        manager
            .join_room("conn-shared", RoomId::expect_positive(10_000_137))
            .await
            .unwrap();

        let initial_metadata_key = format!("{prefix}conn_mgr:conn:conn-shared");
        let initial_room_key = format!("{prefix}connections:room:10000137");
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

        let moved_room = RoomId::expect_positive(10_000_138);
        manager.join_room("conn-shared", moved_room).await.unwrap();

        let moved_room_key = format!("{prefix}connections:room:{moved_room}");
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
            updated_info.room_id,
            Some(moved_room),
            "post-swap operations must use the replacement shared Redis connection"
        );

        manager.unregister("conn-shared").await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker Redis"]
    async fn test_with_redis_runtime_accepts_trait_object_shared_runtime() {
        use redis::AsyncCommands;

        let (_container, client, conn, prefix) = docker_redis_connection("shared-runtime:").await;
        let shared_conn = Arc::new(tokio::sync::RwLock::new(conn));
        let runtime: Arc<dyn RedisConnectionRuntime> =
            Arc::new(SharedRedisConnectionRuntime::new(shared_conn.clone()));
        let manager = ConnectionManager::new(ConnectionLimits::default())
            .with_redis_runtime(runtime, &prefix);

        manager
            .register(
                "conn-runtime".to_string(),
                UserId::expect_positive(10_000_139),
            )
            .await
            .expect("register should use injected redis runtime");

        let key = format!("{prefix}connections:user:10000139");
        let mut verify_conn = redis::aio::ConnectionManager::new(client).await.unwrap();
        let user_count: i64 = verify_conn.get(&key).await.unwrap_or(0);
        assert_eq!(user_count, 1);

        manager.unregister("conn-runtime").await;
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

        let cleanup_op = manager.unregister_cleanup_op(
            "conn-recover",
            "token-recover",
            UserId::expect_positive(20_000_301),
            Some(RoomId::expect_positive(20_000_302)),
        );
        let PendingRedisOp::UnregisterCleanup {
            total_key,
            user_key,
            room_key,
            conn_key,
            user_index_key,
            room_index_key,
            ..
        } = cleanup_op.clone()
        else {
            unreachable!("unregister_cleanup_op must build an unregister cleanup operation");
        };

        let mut verify_conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();
        let metadata = ConnectionInfoPersistent {
            connection_id: "conn-recover".to_string(),
            registration_token: "token-recover".to_string(),
            user_id: UserId::expect_positive(20_000_301),
            actor_id: "usr_recover".to_string(),
            room_id: Some(RoomId::expect_positive(20_000_302)),
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
        let _: () = verify_conn.set(&total_key, 1i64).await.unwrap();
        let _: () = verify_conn.set(&user_key, 1i64).await.unwrap();
        let _: () = verify_conn.set(&room_key, 1i64).await.unwrap();
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
        manager.enqueue_pending_retry_for_test(cleanup_op);

        tokio::time::sleep(Duration::from_secs(6)).await;

        let metadata_exists: bool = verify_conn.exists(&conn_key).await.unwrap();
        let user_members: Vec<String> = verify_conn.smembers(&user_index_key).await.unwrap();
        let room_members: Vec<String> = verify_conn.smembers(&room_index_key).await.unwrap();
        let total_count: i64 = verify_conn.get(&total_key).await.unwrap_or(0);
        let user_count: i64 = verify_conn.get(&user_key).await.unwrap_or(0);
        let room_count: i64 = verify_conn.get(&room_key).await.unwrap_or(0);

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
        assert_eq!(total_count, 0);
        assert_eq!(user_count, 0);
        assert_eq!(room_count, 0);

        manager.shutdown().await;
    }

    #[test]
    fn test_connection_info_persistent_serialization() {
        // Verify that ConnectionInfoPersistent can be serialized/deserialized
        let persistent = ConnectionInfoPersistent {
            connection_id: "conn1".to_string(),
            registration_token: "token1".to_string(),
            user_id: UserId::expect_positive(20_000_401),
            actor_id: "usr_20000401".to_string(),
            room_id: Some(RoomId::expect_positive(20_000_402)),
            connected_at_unix: 1000,
            last_activity_unix: 2000,
            message_count: 5,
            rtc_joined: true,
            rtc_joined_at_unix: Some(1500),
        };

        let json = serde_json::to_string(&persistent).unwrap();
        let deserialized: ConnectionInfoPersistent = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.connection_id, "conn1");
        assert_eq!(deserialized.registration_token, "token1");
        assert_eq!(deserialized.user_id, UserId::expect_positive(20_000_401));
        assert_eq!(
            deserialized.room_id,
            Some(RoomId::expect_positive(20_000_402))
        );
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

    #[tokio::test]
    async fn test_reserve_room_slot_enforces_limit() {
        let limits = ConnectionLimits {
            max_per_room: 3,
            ..ConnectionLimits::default()
        };
        let mgr = ConnectionManager::new(limits);
        let rid = RoomId::expect_positive(1);

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
        let rid = RoomId::expect_positive(1);

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
        let uid = UserId::expect_positive(1);

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
        let uid = UserId::expect_positive(1);

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
        let rid1 = RoomId::expect_positive(1);
        let rid2 = RoomId::expect_positive(2);

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
        let rid = RoomId::expect_positive(1);

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
        let rid = RoomId::expect_positive(1);

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
        let uid = UserId::expect_positive(1);

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
        let user_id = UserId::expect_positive(10_000_010);
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

    #[tokio::test]
    async fn test_shutdown_aborts_timed_out_background_task() {
        let manager = ConnectionManager::new(ConnectionLimits::default());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            let _ = started_tx.send(());
            futures::future::pending::<()>().await;
        });
        manager.test_set_ttl_refresh_handle(handle);

        started_rx
            .await
            .expect("timeout test task should report that it started");

        let report = manager.shutdown().await;

        assert_eq!(
            report.ttl_refresh,
            Some(ShutdownTaskOutcome::TimedOut),
            "shutdown should report timeout before forcing task abort"
        );
        assert!(
            manager
                .ttl_refresh_handle
                .lock()
                .expect("ttl refresh handle mutex poisoned")
                .is_none(),
            "shutdown must drain the timed-out task handle after aborting it"
        );
    }

    async fn docker_redis_connection(
        prefix: &str,
    ) -> (
        RedisContainer,
        redis::Client,
        redis::aio::ConnectionManager,
        String,
    ) {
        let sanitized_label = prefix.replace(':', "-");
        let (container, redis_url) = start_redis_url_with_label(&sanitized_label).await;
        let client = redis::Client::open(redis_url.as_str()).expect("Failed to open Redis client");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match redis::aio::ConnectionManager::new(client.clone()).await {
                Ok(mut conn) => match redis::cmd("PING").query_async::<String>(&mut conn).await {
                    Ok(_) => {
                        return (container, client, conn, prefix.to_string());
                    }
                    Err(error) => {
                        assert!(
                            tokio::time::Instant::now() < deadline,
                            "Redis test container did not become ready in time: {error}"
                        );
                    }
                },
                Err(error) => {
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "Failed to create Redis ConnectionManager: {error}"
                    );
                }
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
