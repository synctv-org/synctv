use dashmap::DashMap;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use synctv_core::models::id::{RoomId, UserId};
use tokio::sync::broadcast;
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
}

impl From<&ConnectionInfo> for ConnectionInfoPersistent {
    fn from(info: &ConnectionInfo) -> Self {
        let now = SystemTime::now();
        let connected_at_unix = now.duration_since(UNIX_EPOCH).unwrap().as_secs()
            .saturating_sub(info.connected_at.elapsed().as_secs());
        let last_activity_unix = now.duration_since(UNIX_EPOCH).unwrap().as_secs()
            .saturating_sub(info.last_activity.elapsed().as_secs());

        Self {
            connection_id: info.connection_id.clone(),
            user_id: info.user_id.as_str().to_string(),
            room_id: info.room_id.as_ref().map(|r| r.as_str().to_string()),
            connected_at_unix,
            last_activity_unix,
            message_count: info.message_count,
            rtc_joined: info.rtc_joined,
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
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        Self {
            max_per_user: 5,
            max_per_room: 200,
            max_total: 10000,
            idle_timeout: Duration::from_secs(5 * 60), // 5 minutes
            max_duration: Duration::from_secs(24 * 60 * 60), // 24 hours
        }
    }
}

/// TTL for distributed connection counters in Redis (seconds).
/// Acts as a crash-safety mechanism: if a node crashes without decrementing,
/// the counter will expire after this duration.
const DISTRIBUTED_COUNTER_TTL_SECONDS: i64 = 90; // 3x heartbeat interval (30s)

/// TTL for connection metadata keys in Redis (seconds).
/// Set to max_duration (24h) + buffer (1h) so metadata auto-expires if a node
/// crashes without calling unregister(). The TTL refresh task keeps active
/// connections alive by periodically resetting this TTL.
const CONNECTION_METADATA_TTL_SECONDS: i64 = 90_000; // 25 hours

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

    /// Optional Redis connection for distributed connection counting.
    /// When present, per-user and per-room limits are enforced across all replicas.
    /// When absent, limits are per-node only (fallback).
    redis_conn: Option<redis::aio::ConnectionManager>,

    /// Key prefix for Redis keys (e.g., "synctv:")
    redis_key_prefix: String,
}

impl ConnectionManager {
    /// Create a new `ConnectionManager`
    #[must_use]
    pub fn new(limits: ConnectionLimits) -> Self {
        let (disconnect_tx, _) = broadcast::channel(1000); // Buffer for disconnect signals
        Self {
            connections: Arc::new(DashMap::new()),
            user_connections: Arc::new(DashMap::new()),
            room_connections: Arc::new(DashMap::new()),
            limits: Arc::new(limits),
            total_connections: Arc::new(AtomicUsize::new(0)),
            total_connections_ever: Arc::new(AtomicU64::new(0)),
            total_messages: Arc::new(AtomicU64::new(0)),
            disconnect_tx: Arc::new(disconnect_tx),
            redis_conn: None,
            redis_key_prefix: String::new(),
        }
    }

    /// Enable distributed connection counting via Redis.
    ///
    /// When Redis is configured, per-user and per-room connection limits are
    /// enforced across all replicas. Without Redis, limits are per-node only.
    #[must_use]
    pub fn with_redis(mut self, conn: redis::aio::ConnectionManager, key_prefix: &str) -> Self {
        self.redis_conn = Some(conn);
        self.redis_key_prefix = key_prefix.to_string();
        self
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
    /// Sends a signal to the connection to close immediately
    pub fn disconnect_connection(&self, connection_id: &str) {
        info!(
            connection_id = %connection_id,
            "Forcing connection disconnect"
        );
        if self.disconnect_tx.send(DisconnectSignal::Connection(connection_id.to_string())).is_err() {
            warn!(
                connection_id = %connection_id,
                "Failed to send disconnect signal: no active receivers"
            );
        }
    }

    /// Force disconnect all connections for a user
    ///
    /// Used when a user is banned or kicked from all rooms
    pub fn disconnect_user(&self, user_id: &UserId) {
        let conn_count = self.user_connection_count(user_id);
        info!(
            user_id = %user_id.as_str(),
            connection_count = conn_count,
            "Forcing disconnect of all user connections"
        );
        if self.disconnect_tx.send(DisconnectSignal::User(user_id.clone())).is_err() {
            warn!(
                user_id = %user_id.as_str(),
                "Failed to send user disconnect signal: no active receivers"
            );
        }
    }

    /// Force disconnect all connections in a room
    ///
    /// Used when a room is deleted or all users need to be removed
    pub fn disconnect_room(&self, room_id: &RoomId) {
        let conn_count = self.room_connection_count(room_id);
        info!(
            room_id = %room_id.as_str(),
            connection_count = conn_count,
            "Forcing disconnect of all room connections"
        );
        if self.disconnect_tx.send(DisconnectSignal::Room(room_id.clone())).is_err() {
            warn!(
                room_id = %room_id.as_str(),
                "Failed to send room disconnect signal: no active receivers"
            );
        }
    }

    /// Force disconnect a specific user from a specific room
    ///
    /// Used when kicking a member from a room (not banning globally)
    pub fn disconnect_user_from_room(&self, user_id: &UserId, room_id: &RoomId) {
        info!(
            user_id = %user_id.as_str(),
            room_id = %room_id.as_str(),
            "Forcing disconnect of user from room"
        );
        if self.disconnect_tx.send(DisconnectSignal::UserFromRoom {
            user_id: user_id.clone(),
            room_id: room_id.clone(),
        }).is_err() {
            warn!(
                user_id = %user_id.as_str(),
                room_id = %room_id.as_str(),
                "Failed to send user-from-room disconnect signal: no active receivers"
            );
        }
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
        if let Some(ref conn) = self.redis_conn {
            let total_key = format!("{}connections:total", self.redis_key_prefix);
            let mut conn_clone = conn.clone();
            let _ = conn_clone.incr::<_, _, i64>(&total_key, 1i64).await;
            let _ = conn_clone.expire::<_, ()>(&total_key, DISTRIBUTED_COUNTER_TTL_SECONDS).await;
        }

        // Check distributed per-user limit via Redis (if configured).
        // On Redis failure, fall back to local-only check.
        let redis_user_incremented = if let Some(ref conn) = self.redis_conn {
            let redis_key = format!("{}connections:user:{}", self.redis_key_prefix, user_id.as_str());
            match self.redis_incr_and_check(&redis_key, self.limits.max_per_user).await {
                Ok(true) => true,  // Allowed, counter incremented
                Ok(false) => {
                    // Distributed limit exceeded -- roll back
                    self.total_connections.fetch_sub(1, Ordering::AcqRel);
                    let _ = self.redis_decr(conn, &redis_key).await;
                    return Err(format!(
                        "Too many connections for this user across all replicas (max {})",
                        self.limits.max_per_user
                    ));
                }
                Err(e) => {
                    // Redis error -- fall back to local-only check
                    warn!("Distributed user connection check failed, using local fallback: {e}");
                    false
                }
            }
        } else {
            false
        };

        // Atomically check per-user limit locally and add connection ID.
        // Holding the entry ref-mut prevents concurrent registrations for the same
        // user from both passing the limit check.
        {
            let mut user_entry = self.user_connections.entry(user_id.clone()).or_default();
            if user_entry.len() >= self.limits.max_per_user {
                // Roll back the total connection reservation
                self.total_connections.fetch_sub(1, Ordering::AcqRel);
                // Roll back Redis counter if we incremented it
                if redis_user_incremented {
                    if let Some(ref conn) = self.redis_conn {
                        let redis_key = format!("{}connections:user:{}", self.redis_key_prefix, user_id.as_str());
                        let _ = self.redis_decr(conn, &redis_key).await;
                    }
                }
                return Err(format!(
                    "Too many connections for this user (max {})",
                    self.limits.max_per_user
                ));
            }
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

        // Atomically check per-room limit locally, increment Redis, and add connection.
        //
        // Order of operations (fixes TOCTOU race):
        // 1. Acquire DashMap entry lock (prevents concurrent local joins)
        // 2. Check local limit
        // 3. Increment Redis counter and verify distributed limit
        // 4. Add to local DashMap
        //
        // This ensures the Redis counter is only incremented while holding the
        // local lock, preventing a window where the counter is inflated but the
        // local entry hasn't been added yet.
        let redis_room_incremented;
        {
            let mut room_entry = self.room_connections.entry(room_id.clone()).or_default();
            if room_entry.len() >= self.limits.max_per_room {
                return Err(format!(
                    "Room at capacity ({} connections)",
                    self.limits.max_per_room
                ));
            }

            // Check distributed per-room limit via Redis while holding the local lock.
            redis_room_incremented = if let Some(ref _conn) = self.redis_conn {
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

            room_entry.push(connection_id.to_string());
            // Drop the shard lock before accessing `connections` DashMap
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

        synctv_core::metrics::cluster::CLUSTER_ROOMS.set(
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

            // Decrement distributed Redis counters and remove metadata (best-effort)
            if let Some(ref conn) = self.redis_conn {
                // Decrement total distributed counter
                let total_key = format!("{}connections:total", self.redis_key_prefix);
                let _ = self.redis_decr(conn, &total_key).await;

                let user_key = format!("{}connections:user:{}", self.redis_key_prefix, conn_info.user_id.as_str());
                let _ = self.redis_decr(conn, &user_key).await;

                if let Some(ref room_id) = conn_info.room_id {
                    let room_key = format!("{}connections:room:{}", self.redis_key_prefix, room_id.as_str());
                    let _ = self.redis_decr(conn, &room_key).await;
                }

                // Remove metadata and index entries (best-effort, spawn background task)
                let conn_key = format!("{}conn_mgr:conn:{}", self.redis_key_prefix, connection_id);
                let user_index_key = format!("{}conn_mgr:user:{}", self.redis_key_prefix, conn_info.user_id.as_str());
                let room_index_key = conn_info.room_id.as_ref()
                    .map(|r| format!("{}conn_mgr:room:{}", self.redis_key_prefix, r.as_str()));

                let mut conn_clone = conn.clone();
                let connection_id_owned = connection_id.to_string();

                tokio::spawn(async move {
                    // Remove connection metadata
                    let _: Result<(), _> = conn_clone.del(&conn_key).await;

                    // Remove from user index
                    let _: Result<(), _> = conn_clone.srem(&user_index_key, &connection_id_owned).await;

                    // Remove from room index if applicable
                    if let Some(room_key) = room_index_key {
                        let _: Result<(), _> = conn_clone.srem(&room_key, &connection_id_owned).await;
                    }
                });
            }

            synctv_core::metrics::ACTIVE_CONNECTIONS.dec();
            synctv_core::metrics::cluster::CLUSTER_CONNECTIONS.set(
                self.total_connections.load(Ordering::Relaxed) as i64,
            );
            synctv_core::metrics::cluster::CLUSTER_ROOMS.set(
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

        let mut failure_count = 0u64;
        let mut success_count = 0u64;

        for key in &counter_keys {
            let result: Result<(), _> = conn.expire(key, DISTRIBUTED_COUNTER_TTL_SECONDS).await;
            if let Err(e) = result {
                failure_count += 1;
                warn!("Failed to refresh TTL for distributed counter {key}: {e}");
            } else {
                success_count += 1;
            }
        }

        for key in &metadata_keys {
            let result: Result<(), _> = conn.expire(key, CONNECTION_METADATA_TTL_SECONDS).await;
            if let Err(e) = result {
                failure_count += 1;
                warn!("Failed to refresh TTL for connection metadata {key}: {e}");
            } else {
                success_count += 1;
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

    /// Get all connections for a user across all replicas (from Redis).
    ///
    /// Returns connection metadata from Redis, which includes connections from
    /// all replicas in the cluster. Falls back to local-only if Redis fails.
    pub async fn get_user_connections_distributed(&self, user_id: &UserId) -> Vec<ConnectionInfo> {
        // Note: We can't fully reconstruct ConnectionInfo from Redis because
        // Instant can't be deserialized across processes. For distributed
        // queries, use get_room_connections_distributed which returns connection IDs.
        // For now, this method returns local connections only.
        // TODO: Return ConnectionInfoPersistent or redesign to use connection IDs.

        // Fallback to local-only
        self.get_user_connections(user_id)
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

    /// Decrement a Redis counter (best-effort, errors are logged but not propagated).
    async fn redis_decr(&self, conn: &redis::aio::ConnectionManager, key: &str) -> Result<(), String> {
        let mut conn = conn.clone();
        let count: i64 = conn.decr(key, 1i64).await.map_err(|e| format!("Redis DECR failed: {e}"))?;
        // Prevent counter from going negative (shouldn't happen, but defensive)
        if count < 0 {
            let _: Result<(), _> = conn.set::<_, _, ()>(key, 0i64).await;
        }
        Ok(())
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
}
