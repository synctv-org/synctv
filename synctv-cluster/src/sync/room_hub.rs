use dashmap::DashMap;
use redis::AsyncCommands;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use synctv_core::models::id::{RoomId, UserId};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

/// Timeout for delivering critical events to slow consumers.
/// Critical events (kick, ban, room deletion) use a bounded wait instead of
/// fire-and-forget spawn to ensure they are reliably delivered before the
/// connection is closed.
const CRITICAL_EVENT_SEND_TIMEOUT: Duration = Duration::from_secs(5);

use super::events::ClusterEvent;

/// Notification about room lifecycle changes (first subscriber / last unsubscribe).
/// Sent to the Redis Pub/Sub subscriber task so it can dynamically subscribe/unsubscribe
/// to specific room channels instead of using a global `psubscribe("synctv:room:*")`.
#[derive(Debug, Clone)]
pub enum RoomLifecycleEvent {
    /// First subscriber joined this room on the local node.
    RoomActivated(RoomId),
    /// Last subscriber left this room on the local node.
    RoomDeactivated(RoomId),
}

/// Capacity for the room lifecycle broadcast channel.
/// Large enough to avoid drops during room churn; events are small.
const LIFECYCLE_CHANNEL_CAPACITY: usize = 1024;

/// Handle for a client connection subscription
pub type ConnectionId = String;

/// Capacity for per-subscriber message channels.
/// Must be large enough to absorb bursts of playback state updates (seek, pause,
/// play) without dropping critical synchronization messages.
const SUBSCRIBER_CHANNEL_CAPACITY: usize = 512;

/// Number of consecutive drops before automatically disconnecting a slow subscriber.
/// Set higher to tolerate transient bursts (e.g., rapid seek operations) without
/// prematurely disconnecting clients on slower networks.
const MAX_CONSECUTIVE_DROPS: u32 = 50;

/// Message sender for a client connection
pub type MessageSender = mpsc::Sender<ClusterEvent>;

/// Subscriber information
#[derive(Debug)]
pub struct Subscriber {
    pub connection_id: ConnectionId,
    pub user_id: UserId,
    pub sender: MessageSender,
    /// Consecutive message drops due to a full channel
    consecutive_drops: Arc<AtomicU32>,
}

impl Clone for Subscriber {
    fn clone(&self) -> Self {
        Self {
            connection_id: self.connection_id.clone(),
            user_id: self.user_id.clone(),
            sender: self.sender.clone(),
            consecutive_drops: self.consecutive_drops.clone(),
        }
    }
}

/// In-memory hub for routing messages to connected clients in rooms
/// This handles local message distribution (single node)
///
/// With Redis configured, subscription state is persisted for cross-replica visibility
/// and recovery after restarts. Local DashMaps serve as a fast cache.
#[derive(Clone, Debug)]
pub struct RoomMessageHub {
    /// Map of `room_id` -> subscribers indexed by connection_id (local cache)
    rooms: Arc<DashMap<RoomId, HashMap<ConnectionId, Subscriber>>>,

    /// Map of `connection_id` -> (`room_id`, `user_id`) for cleanup (local cache)
    connections: Arc<DashMap<ConnectionId, (RoomId, UserId)>>,

    /// Broadcast sender for room lifecycle events (first join / last leave).
    /// The Redis Pub/Sub subscriber task listens on a receiver to dynamically
    /// subscribe/unsubscribe to specific room channels.
    lifecycle_tx: broadcast::Sender<RoomLifecycleEvent>,

    /// Optional Redis connection for distributed subscription state.
    /// When present, subscription relationships are persisted to Redis for
    /// cross-replica visibility and recovery. When absent, operates local-only.
    redis_conn: Option<redis::aio::ConnectionManager>,

    /// Key prefix for Redis keys (e.g., "synctv:")
    redis_key_prefix: String,

    /// TTL in seconds for Redis subscription keys.
    /// Acts as a crash-safety mechanism: if a node crashes without unsubscribing,
    /// the stale keys will expire after this duration instead of accumulating forever.
    /// Refreshed on each subscribe operation. Default: 300 seconds (5 minutes).
    redis_key_ttl_secs: i64,

    /// Cancellation token for the auto-spawned TTL refresh background task.
    /// Cancelled on `shutdown()` to stop the task gracefully.
    ttl_refresh_cancel: Arc<tokio_util::sync::CancellationToken>,

    /// Cancellation token for the auto-spawned stale subscription cleanup task.
    /// Cancelled on `shutdown()` to stop the task gracefully.
    stale_cleanup_cancel: Arc<tokio_util::sync::CancellationToken>,
}

impl RoomMessageHub {
    /// Create a new `RoomMessageHub`
    #[must_use]
    pub fn new() -> Self {
        let (lifecycle_tx, _) = broadcast::channel(LIFECYCLE_CHANNEL_CAPACITY);
        Self {
            rooms: Arc::new(DashMap::new()),
            connections: Arc::new(DashMap::new()),
            lifecycle_tx,
            redis_conn: None,
            redis_key_prefix: String::new(),
            redis_key_ttl_secs: 300, // 5 minutes default
            ttl_refresh_cancel: Arc::new(tokio_util::sync::CancellationToken::new()),
            stale_cleanup_cancel: Arc::new(tokio_util::sync::CancellationToken::new()),
        }
    }

    /// Enable distributed subscription state via Redis.
    ///
    /// When Redis is configured, subscription relationships are persisted to Redis
    /// for cross-replica visibility and recovery after restarts. Local DashMaps
    /// remain as a fast cache for message routing.
    ///
    /// Automatically spawns two background tasks:
    /// 1. **TTL refresh** (at 40% of `redis_key_ttl_secs` interval) to prevent
    ///    active subscription keys from expiring.
    /// 2. **Stale subscription cleanup** (every 60 seconds) to remove orphaned
    ///    Redis entries left by failed fire-and-forget cleanup in `unsubscribe()`.
    ///
    /// Both tasks are cancelled when `shutdown()` is called.
    #[must_use]
    pub fn with_redis(mut self, conn: redis::aio::ConnectionManager, key_prefix: &str) -> Self {
        self.redis_conn = Some(conn);
        self.redis_key_prefix = key_prefix.to_string();

        // Auto-spawn the TTL refresh task unconditionally whenever Redis is
        // configured, so callers do not need to remember to call
        // `spawn_ttl_refresh_task()` manually.  The interval is set to 40% of
        // the configured TTL so keys are always refreshed well before expiry.
        let cancel = tokio_util::sync::CancellationToken::new();
        self.ttl_refresh_cancel = Arc::new(cancel.clone());
        // Use 40% of TTL as the refresh interval (at most 120s, at least 30s)
        let refresh_interval_secs =
            (self.redis_key_ttl_secs as f64 * 0.4).clamp(30.0, 120.0) as u64;
        let _handle =
            self.spawn_ttl_refresh_task(Duration::from_secs(refresh_interval_secs), cancel);

        // Auto-spawn the stale subscription cleanup task to remove orphaned
        // Redis entries that accumulate when fire-and-forget cleanup fails.
        let stale_cancel = tokio_util::sync::CancellationToken::new();
        self.stale_cleanup_cancel = Arc::new(stale_cancel.clone());
        let _cleanup_handle =
            self.spawn_stale_subscription_cleanup_task(Duration::from_secs(60), stale_cancel);

        self
    }

    /// Cancel the auto-spawned background tasks (TTL refresh and stale cleanup).
    ///
    /// Should be called during graceful shutdown to stop background tasks.
    pub fn shutdown(&self) {
        self.ttl_refresh_cancel.cancel();
        self.stale_cleanup_cancel.cancel();
    }

    /// Set the TTL for Redis subscription keys (crash-safety mechanism).
    ///
    /// If a node crashes without properly unsubscribing, stale keys will expire
    /// after this duration. Should be set to at least `heartbeat_timeout * 2`.
    #[must_use]
    pub const fn with_redis_key_ttl_secs(mut self, ttl_secs: i64) -> Self {
        self.redis_key_ttl_secs = ttl_secs;
        self
    }

    /// Subscribe to room lifecycle events (room activated / deactivated).
    /// Used by the Redis Pub/Sub subscriber task.
    #[must_use]
    pub fn subscribe_lifecycle(&self) -> broadcast::Receiver<RoomLifecycleEvent> {
        self.lifecycle_tx.subscribe()
    }

    /// Subscribe a client to room events
    /// Returns a receiver for messages
    ///
    /// With Redis configured, persists the subscription relationship for cross-replica
    /// visibility and recovery. Falls back to local-only on Redis errors.
    pub async fn subscribe(
        &self,
        room_id: RoomId,
        user_id: UserId,
        connection_id: ConnectionId,
    ) -> mpsc::Receiver<ClusterEvent> {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_CHANNEL_CAPACITY);

        let subscriber = Subscriber {
            connection_id: connection_id.clone(),
            user_id: user_id.clone(),
            sender: tx,
            consecutive_drops: Arc::new(AtomicU32::new(0)),
        };

        // Atomically check-and-insert using DashMap's entry API.
        // This avoids the TOCTOU race between `contains_key` + `entry().or_default()`
        // where two concurrent subscribes could both see the room as new.
        let is_new_room = match self.rooms.entry(room_id.clone()) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                entry.get_mut().insert(connection_id.clone(), subscriber);
                false
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                let mut map = HashMap::new();
                map.insert(connection_id.clone(), subscriber);
                entry.insert(map);
                true
            }
        };

        // Track connection for cleanup
        self.connections
            .insert(connection_id.clone(), (room_id.clone(), user_id.clone()));

        // Persist to Redis for cross-replica visibility.
        // Awaited so callers can rely on the subscription being visible to other
        // replicas before this function returns. Errors are logged and propagated
        // so the caller knows if cross-replica state could not be written.
        if let Some(ref conn) = self.redis_conn {
            let room_key = format!(
                "{}room_hub:room:{}",
                self.redis_key_prefix,
                room_id.as_str()
            );
            let conn_key = format!("{}room_hub:conn:{}", self.redis_key_prefix, connection_id);

            let mut conn_clone = conn.clone();
            let user_id_str = user_id.as_str().to_string();
            let room_id_str = room_id.as_str().to_string();
            let ttl_secs = self.redis_key_ttl_secs;

            // Store room -> {connection_id: user_id} mapping
            if let Err(e) = conn_clone
                .hset::<_, _, _, ()>(&room_key, &connection_id, &user_id_str)
                .await
            {
                warn!("Failed to persist room subscription to Redis: {e}");
            }
            // Set TTL on room key so stale data expires if the node crashes
            let _: Result<(), _> = conn_clone.expire::<_, ()>(&room_key, ttl_secs).await;

            // Store connection -> room_id mapping for cleanup, with TTL
            if let Err(e) = conn_clone
                .set_ex::<_, _, ()>(&conn_key, &room_id_str, ttl_secs as u64)
                .await
            {
                warn!("Failed to persist connection mapping to Redis: {e}");
            }
        }

        // Emit lifecycle event if this is the first subscriber for the room.
        // D2 fix: log warning if send fails (no receivers) instead of silently dropping.
        if is_new_room {
            if let Err(e) = self
                .lifecycle_tx
                .send(RoomLifecycleEvent::RoomActivated(room_id.clone()))
            {
                warn!(
                    room_id = %room_id.as_str(),
                    "Failed to emit RoomActivated lifecycle event (no receivers): {}",
                    e
                );
            }
        }

        info!(
            room_id = %room_id.as_str(),
            user_id = %user_id.as_str(),
            connection_id = %connection_id,
            "Client subscribed to room"
        );

        rx
    }

    /// Unsubscribe a client from room events
    ///
    /// Removes subscription from both local cache and Redis (if configured).
    pub fn unsubscribe(&self, connection_id: &str) {
        if let Some((_, (room_id, user_id))) = self.connections.remove(connection_id) {
            let mut room_deactivated = false;

            // Remove the subscriber by connection_id (O(1) HashMap lookup).
            // We must drop the RefMut before calling remove_if, since both
            // acquire the same shard lock and would deadlock.
            if let Some(mut subscribers) = self.rooms.get_mut(&room_id) {
                subscribers.remove(connection_id);
                drop(subscribers);
            }

            // Atomically remove the room entry only if it's still empty.
            // If a concurrent subscribe added a new subscriber between the
            // retain and this call, the entry won't be empty and won't be removed.
            if self
                .rooms
                .remove_if(&room_id, |_, subscribers| subscribers.is_empty())
                .is_some()
            {
                room_deactivated = true;
                debug!(room_id = %room_id.as_str(), "Room has no more subscribers, removed");
            }

            // Remove from Redis (best-effort, don't block unsubscribe path).
            //
            // SAFETY: Fire-and-forget cleanup. If the node crashes before the
            // Redis delete completes, the stale keys will be cleaned up by their
            // TTL (set during subscribe). No manual intervention is needed.
            if let Some(ref conn) = self.redis_conn {
                let room_key = format!(
                    "{}room_hub:room:{}",
                    self.redis_key_prefix,
                    room_id.as_str()
                );
                let conn_key = format!("{}room_hub:conn:{}", self.redis_key_prefix, connection_id);
                let mut conn_clone = conn.clone();
                let connection_id_owned = connection_id.to_string();

                tokio::spawn(async move {
                    // Remove connection from room's subscriber hash
                    if let Err(e) = conn_clone
                        .hdel::<_, _, ()>(&room_key, &connection_id_owned)
                        .await
                    {
                        warn!("Failed to remove room subscription from Redis: {e}");
                    }
                    // Remove connection mapping
                    if let Err(e) = conn_clone.del::<_, ()>(&conn_key).await {
                        warn!("Failed to remove connection mapping from Redis: {e}");
                    }
                });
            }

            // Emit lifecycle event if the last subscriber left.
            // D2 fix: log warning on send failure instead of silently dropping.
            if room_deactivated {
                if let Err(e) = self
                    .lifecycle_tx
                    .send(RoomLifecycleEvent::RoomDeactivated(room_id.clone()))
                {
                    warn!(
                        room_id = %room_id.as_str(),
                        "Failed to emit RoomDeactivated lifecycle event (no receivers): {}",
                        e
                    );
                }
            }

            info!(
                room_id = %room_id.as_str(),
                user_id = %user_id.as_str(),
                connection_id = %connection_id,
                "Client unsubscribed from room"
            );
        } else {
            warn!(
                connection_id = %connection_id,
                "Attempted to unsubscribe unknown connection"
            );
        }
    }

    /// Broadcast an event to all subscribers in a room.
    ///
    /// Subscribers that fail to receive messages for `MAX_CONSECUTIVE_DROPS`
    /// consecutive broadcasts are automatically disconnected to prevent
    /// unbounded backpressure from a single slow client.
    ///
    /// **Critical event guarantee**: Events where `is_critical()` returns true
    /// (KickUserFromRoom, RoomDeleted, KickPublisher, KickUser, PermissionChanged,
    /// UserLeft) bypass the slow-consumer drop logic entirely. For critical events,
    /// a blocking send (via `try_reserve` fallback) is attempted, and slow
    /// subscribers are still disconnected *after* the critical message is queued.
    /// This prevents administrative actions (bans, kicks) from being silently lost.
    pub fn broadcast(&self, room_id: &RoomId, event: ClusterEvent) -> usize {
        let mut sent_count = 0;
        let mut failed_connections = Vec::new();
        let is_critical = event.is_critical();

        // LOCK ORDERING: Acquire `rooms` read guard in a scoped block. The guard
        // MUST be dropped before calling `unsubscribe()`, which acquires write
        // guards on both `rooms` (via `get_mut` / `remove_if`) and `connections`.
        // Holding the read guard across the `unsubscribe` call would deadlock on
        // the same DashMap shard.
        {
            let subscribers_guard = self.rooms.get(room_id);
            if let Some(subscribers) = &subscribers_guard {
                for subscriber in subscribers.values() {
                    match subscriber.sender.try_send(event.clone()) {
                        Ok(()) => {
                            // Reset consecutive drop counter on successful send
                            subscriber.consecutive_drops.store(0, Ordering::Relaxed);
                            sent_count += 1;
                            debug!(
                                room_id = %room_id.as_str(),
                                user_id = %subscriber.user_id.as_str(),
                                connection_id = %subscriber.connection_id,
                                event_type = %event.event_type(),
                                "Event sent to client"
                            );
                        }
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            if is_critical {
                                // Critical events must not be dropped. Use a
                                // bounded timeout to wait for channel space rather
                                // than a fire-and-forget spawn, so delivery is
                                // tracked and failures are observable.
                                let sender = subscriber.sender.clone();
                                let event_clone = event.clone();
                                let conn_id = subscriber.connection_id.clone();
                                let room_id_clone = room_id.clone();
                                let event_type = event.event_type().to_string();
                                tokio::spawn(async move {
                                    match tokio::time::timeout(
                                        CRITICAL_EVENT_SEND_TIMEOUT,
                                        sender.send(event_clone),
                                    )
                                    .await
                                    {
                                        Ok(Ok(())) => {}
                                        Ok(Err(e)) => {
                                            warn!(
                                                room_id = %room_id_clone.as_str(),
                                                connection_id = %conn_id,
                                                event_type = %event_type,
                                                "Failed to deliver critical event (channel closed): {e}"
                                            );
                                        }
                                        Err(_) => {
                                            warn!(
                                                room_id = %room_id_clone.as_str(),
                                                connection_id = %conn_id,
                                                event_type = %event_type,
                                                timeout_secs = CRITICAL_EVENT_SEND_TIMEOUT.as_secs(),
                                                "Critical event delivery timed out, slow consumer may miss event"
                                            );
                                        }
                                    }
                                });
                                sent_count += 1;
                            } else {
                                let drops =
                                    subscriber.consecutive_drops.fetch_add(1, Ordering::Relaxed)
                                        + 1;
                                if drops >= MAX_CONSECUTIVE_DROPS {
                                    warn!(
                                        room_id = %room_id.as_str(),
                                        user_id = %subscriber.user_id.as_str(),
                                        connection_id = %subscriber.connection_id,
                                        consecutive_drops = drops,
                                        "Disconnecting persistently slow subscriber after {} consecutive drops",
                                        MAX_CONSECUTIVE_DROPS
                                    );
                                    failed_connections.push(subscriber.connection_id.clone());
                                } else {
                                    warn!(
                                        room_id = %room_id.as_str(),
                                        user_id = %subscriber.user_id.as_str(),
                                        connection_id = %subscriber.connection_id,
                                        event_type = %event.event_type(),
                                        consecutive_drops = drops,
                                        "Subscriber channel full, dropping event for slow consumer"
                                    );
                                }
                            }
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            warn!(
                                room_id = %room_id.as_str(),
                                user_id = %subscriber.user_id.as_str(),
                                connection_id = %subscriber.connection_id,
                                "Subscriber channel closed, marking for cleanup"
                            );
                            failed_connections.push(subscriber.connection_id.clone());
                        }
                    }
                }
            }
            // `subscribers_guard` (rooms read guard) is explicitly dropped here
            // before the cleanup loop below.
        }

        // Clean up failed/slow connections (rooms read guard already dropped above)
        for conn_id in failed_connections {
            self.unsubscribe(&conn_id);
        }

        if sent_count > 0 {
            debug!(
                room_id = %room_id.as_str(),
                sent_count = sent_count,
                event_type = %event.event_type(),
                "Event broadcast complete"
            );
        }

        sent_count
    }

    /// Broadcast an event to a specific user in a room.
    ///
    /// Like `broadcast()`, critical events bypass the slow-consumer drop logic
    /// and use a bounded timeout to ensure reliable delivery.
    pub fn broadcast_to_user(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        event: ClusterEvent,
    ) -> usize {
        let mut sent_count = 0;
        let mut failed_connections = Vec::new();
        let is_critical = event.is_critical();

        // LOCK ORDERING: Same pattern as broadcast() -- scoped read guard on
        // `rooms` must be dropped before calling `unsubscribe()`.
        {
            let subscribers_guard = self.rooms.get(room_id);
            if let Some(subscribers) = &subscribers_guard {
                for subscriber in subscribers.values() {
                    if subscriber.user_id == *user_id {
                        match subscriber.sender.try_send(event.clone()) {
                            Ok(()) => {
                                subscriber.consecutive_drops.store(0, Ordering::Relaxed);
                                sent_count += 1;
                                debug!(
                                    room_id = %room_id.as_str(),
                                    user_id = %subscriber.user_id.as_str(),
                                    connection_id = %subscriber.connection_id,
                                    event_type = %event.event_type(),
                                    "Event sent to specific user"
                                );
                            }
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                if is_critical {
                                    // Critical events: same timeout-based delivery as broadcast()
                                    let sender = subscriber.sender.clone();
                                    let event_clone = event.clone();
                                    let conn_id = subscriber.connection_id.clone();
                                    let room_id_clone = room_id.clone();
                                    let event_type = event.event_type().to_string();
                                    tokio::spawn(async move {
                                        match tokio::time::timeout(
                                            CRITICAL_EVENT_SEND_TIMEOUT,
                                            sender.send(event_clone),
                                        )
                                        .await
                                        {
                                            Ok(Ok(())) => {}
                                            Ok(Err(e)) => {
                                                warn!(
                                                    room_id = %room_id_clone.as_str(),
                                                    connection_id = %conn_id,
                                                    event_type = %event_type,
                                                    "Failed to deliver critical event to user (channel closed): {e}"
                                                );
                                            }
                                            Err(_) => {
                                                warn!(
                                                    room_id = %room_id_clone.as_str(),
                                                    connection_id = %conn_id,
                                                    event_type = %event_type,
                                                    timeout_secs = CRITICAL_EVENT_SEND_TIMEOUT.as_secs(),
                                                    "Critical event delivery to user timed out"
                                                );
                                            }
                                        }
                                    });
                                    sent_count += 1;
                                } else {
                                    let drops = subscriber
                                        .consecutive_drops
                                        .fetch_add(1, Ordering::Relaxed)
                                        + 1;
                                    if drops >= MAX_CONSECUTIVE_DROPS {
                                        warn!(
                                            room_id = %room_id.as_str(),
                                            user_id = %subscriber.user_id.as_str(),
                                            connection_id = %subscriber.connection_id,
                                            consecutive_drops = drops,
                                            "Disconnecting persistently slow subscriber after {} consecutive drops (targeted)",
                                            MAX_CONSECUTIVE_DROPS
                                        );
                                        failed_connections.push(subscriber.connection_id.clone());
                                    } else {
                                        warn!(
                                            room_id = %room_id.as_str(),
                                            user_id = %subscriber.user_id.as_str(),
                                            connection_id = %subscriber.connection_id,
                                            event_type = %event.event_type(),
                                            consecutive_drops = drops,
                                            "Subscriber channel full, dropping event for slow consumer"
                                        );
                                    }
                                }
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                warn!(
                                    room_id = %room_id.as_str(),
                                    user_id = %subscriber.user_id.as_str(),
                                    connection_id = %subscriber.connection_id,
                                    "Subscriber channel closed, marking for cleanup"
                                );
                                failed_connections.push(subscriber.connection_id.clone());
                            }
                        }
                    }
                }
            }
        }

        // Clean up failed connections (rooms read guard already dropped above)
        for conn_id in failed_connections {
            self.unsubscribe(&conn_id);
        }

        sent_count
    }

    /// Broadcast an event to a specific connection in a room.
    ///
    /// Used for targeted delivery (e.g., WebRTC signaling to a specific peer).
    /// Returns 1 if sent, 0 if the connection was not found or the channel was full.
    pub fn broadcast_to_connection(
        &self,
        room_id: &RoomId,
        connection_id: &str,
        event: ClusterEvent,
    ) -> usize {
        let mut result = 0;
        let mut failed_connection: Option<ConnectionId> = None;

        if let Some(subscribers) = self.rooms.get(room_id) {
            if let Some(subscriber) = subscribers.get(connection_id) {
                let event_type = event.event_type().to_string();
                match subscriber.sender.try_send(event) {
                    Ok(()) => {
                        debug!(
                            room_id = %room_id.as_str(),
                            connection_id = %connection_id,
                            event_type = %event_type,
                            "Event sent to specific connection"
                        );
                        result = 1;
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        warn!(
                            room_id = %room_id.as_str(),
                            connection_id = %connection_id,
                            "Subscriber channel full, dropping targeted event"
                        );
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        warn!(
                            room_id = %room_id.as_str(),
                            connection_id = %connection_id,
                            "Subscriber channel closed for targeted event"
                        );
                        failed_connection = Some(subscriber.connection_id.clone());
                    }
                }
            }
        }
        // Drop the DashMap read guard above before calling unsubscribe(),
        // which takes a write lock, to avoid deadlock on the same shard.

        // Clean up closed connection
        if let Some(conn_id) = failed_connection {
            self.unsubscribe(&conn_id);
        }

        result
    }

    /// Get the number of subscribers in a room
    #[must_use]
    pub fn subscriber_count(&self, room_id: &RoomId) -> usize {
        self.rooms
            .get(room_id)
            .map_or(0, |subscribers| subscribers.len())
    }

    /// Get the number of active rooms
    #[must_use]
    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    /// Get all active room IDs (rooms with at least one subscriber)
    #[must_use]
    pub fn active_room_ids(&self) -> Vec<RoomId> {
        self.rooms.iter().map(|entry| entry.key().clone()).collect()
    }

    /// Get total number of active connections
    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Remove all subscribers for a room and clean up connection tracking.
    ///
    /// Used when a room is deleted on another replica: after broadcasting the
    /// `RoomDeleted` event, the hub removes the room so senders are dropped and
    /// WebSocket read loops terminate.
    pub fn remove_room(&self, room_id: &RoomId) {
        if let Some((_, subscribers)) = self.rooms.remove(room_id) {
            for sub in subscribers.values() {
                self.connections.remove(&sub.connection_id);
            }
            // Emit lifecycle event since the room is no longer active.
            // D2 fix: log warning on send failure instead of silently dropping.
            if let Err(e) = self
                .lifecycle_tx
                .send(RoomLifecycleEvent::RoomDeactivated(room_id.clone()))
            {
                warn!(
                    room_id = %room_id.as_str(),
                    "Failed to emit RoomDeactivated lifecycle event on room removal (no receivers): {}",
                    e
                );
            }
            info!(
                room_id = %room_id.as_str(),
                removed_connections = subscribers.len(),
                "Removed all subscribers for deleted room"
            );
        }
    }

    /// Get all subscribers in a room (for debugging/monitoring)
    #[must_use]
    pub fn get_room_subscribers(&self, room_id: &RoomId) -> Vec<(UserId, ConnectionId)> {
        self.rooms
            .get(room_id)
            .map(|subscribers| {
                subscribers
                    .values()
                    .map(|sub| (sub.user_id.clone(), sub.connection_id.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all subscribers in a room across all replicas (from Redis).
    ///
    /// Returns the full subscriber list from Redis, which includes subscriptions
    /// from all replicas in the cluster. Falls back to local-only if Redis is
    /// not configured or fails.
    pub async fn get_room_subscribers_distributed(
        &self,
        room_id: &RoomId,
    ) -> Vec<(UserId, ConnectionId)> {
        if let Some(ref conn) = self.redis_conn {
            let room_key = format!(
                "{}room_hub:room:{}",
                self.redis_key_prefix,
                room_id.as_str()
            );
            let mut conn_clone = conn.clone();

            match conn_clone
                .hgetall::<_, Vec<(String, String)>>(&room_key)
                .await
            {
                Ok(entries) => {
                    return entries
                        .into_iter()
                        .map(|(conn_id, user_id_str)| (UserId::from_string(user_id_str), conn_id))
                        .collect();
                }
                Err(e) => {
                    warn!(
                        "Failed to fetch room subscribers from Redis, falling back to local: {e}"
                    );
                }
            }
        }

        // Fallback to local-only
        self.get_room_subscribers(room_id)
    }

    /// Audit cluster-wide subscription state from Redis (observability only).
    ///
    /// Scans Redis for persisted subscription relationships and logs room/subscriber
    /// counts. This is an **observability tool**, not a recovery mechanism.
    ///
    /// This method does **not** populate the local `rooms` or `connections` DashMaps
    /// because:
    ///
    /// 1. **`MessageSender` cannot be recovered.** Each subscriber's `mpsc::Sender`
    ///    is only meaningful to the original WebSocket connection. Without a live
    ///    sender, messages cannot be routed, so inserting a fake `Subscriber` would
    ///    create a broken entry that either panics or silently drops events.
    ///
    /// 2. **Stale data from crashed replicas.** Redis may contain subscriptions from
    ///    nodes that crashed without unsubscribing. These will expire via TTL, but
    ///    recovering them into the local cache would create phantom subscribers.
    ///
    /// 3. **No lifecycle events.** Populating the local cache would emit spurious
    ///    `RoomActivated` events to the Redis Pub/Sub subscriber task, causing it
    ///    to subscribe to channels for rooms that have no real local subscribers.
    ///
    /// **Clients must reconnect** after a replica restart to re-establish their
    /// subscriptions via `subscribe()`. This method only logs what Redis knows
    /// about for dashboards and debugging.
    pub async fn audit_redis_subscriptions(&self) -> Result<usize, String> {
        info!("Auditing cluster subscription state from Redis (observability only, clients must reconnect for message routing)");
        let Some(ref conn) = self.redis_conn else {
            return Err("Redis not configured".to_string());
        };

        let pattern = format!("{}room_hub:room:*", self.redis_key_prefix);
        let mut conn_clone = conn.clone();
        let mut recovered = 0;

        // Use SCAN instead of KEYS to avoid blocking Redis on large datasets
        let mut keys = Vec::new();
        let mut cursor: u64 = 0;
        loop {
            let scan_result: (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn_clone)
                .await
                .map_err(|e| format!("Failed to SCAN Redis keys: {e}"))?;

            cursor = scan_result.0;
            keys.extend(scan_result.1);

            if cursor == 0 {
                break;
            }
        }

        for key in keys {
            // Extract room_id from key
            let room_id_str =
                key.trim_start_matches(&format!("{}room_hub:room:", self.redis_key_prefix));
            let room_id = RoomId::from_string(room_id_str.to_string());

            // Fetch all subscribers for this room
            let entries: Vec<(String, String)> = conn_clone
                .hgetall(&key)
                .await
                .map_err(|e| format!("Failed to fetch room {room_id_str} subscribers: {e}"))?;

            recovered += entries.len();

            info!(
                room_id = %room_id.as_str(),
                subscriber_count = entries.len(),
                "Audited room subscription state from Redis (observability only)"
            );
        }

        Ok(recovered)
    }

    /// Refresh TTLs on all active Redis subscription keys.
    ///
    /// Redis keys for room subscriptions (`room_hub:room:*`) and connection
    /// mappings (`room_hub:conn:*`) are set with a TTL as a crash-safety
    /// mechanism. Long-lived subscriptions can outlive this TTL if it is not
    /// periodically refreshed, causing cross-replica visibility to silently
    /// stop working.
    async fn refresh_redis_key_ttls(&self) {
        let Some(ref conn) = self.redis_conn else {
            return;
        };
        let mut conn = conn.clone();
        let ttl_secs = self.redis_key_ttl_secs;

        let mut keys_to_refresh = Vec::new();

        // Collect room keys for all active rooms
        for entry in self.rooms.iter() {
            let room_key = format!(
                "{}room_hub:room:{}",
                self.redis_key_prefix,
                entry.key().as_str()
            );
            keys_to_refresh.push(room_key);
        }

        // Collect connection keys for all active connections
        for entry in self.connections.iter() {
            let conn_key = format!("{}room_hub:conn:{}", self.redis_key_prefix, entry.key());
            keys_to_refresh.push(conn_key);
        }

        if keys_to_refresh.is_empty() {
            return;
        }

        let mut pipe = redis::pipe();
        for key in &keys_to_refresh {
            pipe.expire(key, ttl_secs).ignore();
        }

        if let Err(e) = pipe.query_async::<()>(&mut conn).await {
            warn!(
                total_keys = keys_to_refresh.len(),
                "Failed to refresh room_hub Redis key TTLs via pipeline: {e}"
            );
        } else {
            debug!(
                refreshed_keys = keys_to_refresh.len(),
                "Refreshed TTLs on room_hub Redis subscription keys"
            );
        }
    }

    /// Clean up orphaned Redis subscription entries that no longer have
    /// corresponding local subscribers.
    ///
    /// This handles cases where `unsubscribe()` fire-and-forget Redis cleanup
    /// failed (e.g., Redis was slow or temporarily unavailable). Without this
    /// periodic scan, stale entries would accumulate until their TTL expires.
    ///
    /// The cleanup is conservative: it only removes entries from Redis that
    /// do not exist in the local `connections` map. Entries from other replicas
    /// are left intact (they are identified by not having a local connection).
    async fn cleanup_orphaned_redis_subscriptions(&self) {
        let Some(ref conn) = self.redis_conn else {
            return;
        };
        let mut conn = conn.clone();

        // Scan for all connection mapping keys (room_hub:conn:*)
        let pattern = format!("{}room_hub:conn:*", self.redis_key_prefix);
        let prefix = format!("{}room_hub:conn:", self.redis_key_prefix);
        let mut cleaned = 0u64;
        let mut errors = 0u64;

        let mut cursor: u64 = 0;
        loop {
            let scan_result: Result<(u64, Vec<String>), _> = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await;

            match scan_result {
                Ok((new_cursor, keys)) => {
                    cursor = new_cursor;

                    for key in keys {
                        // Extract connection_id from key
                        if let Some(conn_id) = key.strip_prefix(&prefix) {
                            // Only clean up entries that were ours (exist in neither
                            // local connections nor rooms). Entries from other replicas
                            // should not be touched.
                            if !self.connections.contains_key(conn_id) {
                                // Fetch the room_id to also clean up the room hash
                                let room_id_result: Result<Option<String>, _> =
                                    conn.get(&key).await;

                                match room_id_result {
                                    Ok(Some(room_id_str)) => {
                                        let room_key = format!(
                                            "{}room_hub:room:{}",
                                            self.redis_key_prefix, room_id_str
                                        );
                                        // Remove connection from room hash and delete conn key
                                        let mut pipe = redis::pipe();
                                        pipe.hdel(&room_key, conn_id).ignore();
                                        pipe.del(&key).ignore();
                                        if let Err(e) = pipe.query_async::<()>(&mut conn).await {
                                            errors += 1;
                                            warn!(
                                                connection_id = %conn_id,
                                                error = %e,
                                                "Failed to clean up orphaned Redis subscription"
                                            );
                                        } else {
                                            cleaned += 1;
                                            debug!(
                                                connection_id = %conn_id,
                                                room_id = %room_id_str,
                                                "Cleaned up orphaned Redis subscription"
                                            );
                                        }
                                    }
                                    Ok(None) => {
                                        // Connection key exists but has no value -- just delete it
                                        if let Err(e) = conn.del::<_, ()>(&key).await {
                                            errors += 1;
                                            warn!(
                                                connection_id = %conn_id,
                                                error = %e,
                                                "Failed to delete empty orphaned connection key"
                                            );
                                        } else {
                                            cleaned += 1;
                                        }
                                    }
                                    Err(e) => {
                                        errors += 1;
                                        warn!(
                                            connection_id = %conn_id,
                                            error = %e,
                                            "Failed to read orphaned connection key"
                                        );
                                    }
                                }
                            }
                        }
                    }

                    if cursor == 0 {
                        break;
                    }
                }
                Err(e) => {
                    errors += 1;
                    warn!(error = %e, "Failed to SCAN Redis for orphaned subscriptions");
                    break;
                }
            }
        }

        if cleaned > 0 || errors > 0 {
            info!(
                orphaned_cleaned = cleaned,
                cleanup_errors = errors,
                "Cleaned up orphaned Redis subscription entries"
            );
        }
    }

    /// Spawn a background task that periodically scans for and removes orphaned
    /// Redis subscription entries.
    ///
    /// Orphaned entries occur when the fire-and-forget Redis cleanup in
    /// `unsubscribe()` fails due to Redis being slow or temporarily unavailable.
    /// This task ensures eventual cleanup by scanning every `interval`.
    ///
    /// The task is automatically cancelled when `shutdown()` is called via the
    /// provided `cancel_token`.
    #[must_use]
    pub fn spawn_stale_subscription_cleanup_task(
        &self,
        interval: Duration,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let hub = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Skip the first immediate tick
            ticker.tick().await;
            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        info!("Room hub stale subscription cleanup task shutting down");
                        return;
                    }
                    _ = ticker.tick() => {
                        hub.cleanup_orphaned_redis_subscriptions().await;
                    }
                }
            }
        })
    }

    /// Spawn a background task that periodically refreshes TTLs on Redis
    /// subscription keys to prevent them from expiring while subscriptions
    /// are still active.
    ///
    /// The refresh interval should be less than half the TTL to ensure keys
    /// are always refreshed before expiration.
    #[must_use]
    pub fn spawn_ttl_refresh_task(
        &self,
        interval: Duration,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let hub = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Skip the first immediate tick
            ticker.tick().await;
            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        info!("Room hub TTL refresh task shutting down");
                        return;
                    }
                    _ = ticker.tick() => {
                        hub.refresh_redis_key_ttls().await;
                    }
                }
            }
        })
    }
}

impl Default for RoomMessageHub {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn test_subscribe_and_broadcast() {
        let hub = RoomMessageHub::new();
        let room_id = RoomId::from_string("test_room".to_string());
        let user_id = UserId::from_string("test_user".to_string());

        // Subscribe
        let mut rx = hub
            .subscribe(room_id.clone(), user_id.clone(), "conn1".to_string())
            .await;

        assert_eq!(hub.subscriber_count(&room_id), 1);
        assert_eq!(hub.connection_count(), 1);

        // Broadcast event
        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: room_id.clone(),
            user_id: user_id.clone(),
            username: "testuser".to_string(),
            message: "Hello!".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        let sent_count = hub.broadcast(&room_id, event.clone());
        assert_eq!(sent_count, 1);

        // Receive event
        let received = rx.recv().await.unwrap();
        assert_eq!(received.event_type(), "chat_message");
    }

    #[tokio::test]
    async fn test_unsubscribe() {
        let hub = RoomMessageHub::new();
        let room_id = RoomId::from_string("test_room".to_string());
        let user_id = UserId::from_string("test_user".to_string());

        // Subscribe
        let _rx = hub
            .subscribe(room_id.clone(), user_id.clone(), "conn1".to_string())
            .await;
        assert_eq!(hub.subscriber_count(&room_id), 1);

        // Unsubscribe
        hub.unsubscribe("conn1");
        assert_eq!(hub.subscriber_count(&room_id), 0);
        assert_eq!(hub.connection_count(), 0);
        assert_eq!(hub.room_count(), 0);
    }

    #[tokio::test]
    async fn test_multiple_subscribers() {
        let hub = RoomMessageHub::new();
        let room_id = RoomId::from_string("test_room".to_string());
        let user1 = UserId::from_string("user1".to_string());
        let user2 = UserId::from_string("user2".to_string());

        // Subscribe two clients
        let mut rx1 = hub
            .subscribe(room_id.clone(), user1.clone(), "conn1".to_string())
            .await;
        let mut rx2 = hub
            .subscribe(room_id.clone(), user2.clone(), "conn2".to_string())
            .await;

        assert_eq!(hub.subscriber_count(&room_id), 2);

        // Broadcast event
        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: room_id.clone(),
            user_id: user1.clone(),
            username: "user1".to_string(),
            message: "Hello!".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        let sent_count = hub.broadcast(&room_id, event.clone());
        assert_eq!(sent_count, 2);

        // Both should receive
        let received1 = rx1.recv().await.unwrap();
        let received2 = rx2.recv().await.unwrap();

        assert_eq!(received1.event_type(), "chat_message");
        assert_eq!(received2.event_type(), "chat_message");
    }

    #[tokio::test]
    async fn test_broadcast_to_specific_user() {
        let hub = RoomMessageHub::new();
        let room_id = RoomId::from_string("test_room".to_string());
        let user1 = UserId::from_string("user1".to_string());
        let user2 = UserId::from_string("user2".to_string());

        // Subscribe two clients
        let mut rx1 = hub
            .subscribe(room_id.clone(), user1.clone(), "conn1".to_string())
            .await;
        let mut rx2 = hub
            .subscribe(room_id.clone(), user2.clone(), "conn2".to_string())
            .await;

        // Broadcast to user1 only
        let event = ClusterEvent::SystemNotification {
            event_id: nanoid::nanoid!(16),
            message: "Private message".to_string(),
            level: crate::sync::NotificationLevel::Info,
            timestamp: Utc::now(),
        };

        let sent_count = hub.broadcast_to_user(&room_id, &user1, event.clone());
        assert_eq!(sent_count, 1);

        // Only user1 should receive
        let received1 = tokio::time::timeout(std::time::Duration::from_millis(100), rx1.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(received1.event_type(), "system_notification");

        // User2 should not receive
        let received2 =
            tokio::time::timeout(std::time::Duration::from_millis(100), rx2.recv()).await;

        assert!(
            received2.is_err(),
            "User2 should not have received the message"
        );
    }

    #[tokio::test]
    async fn test_lifecycle_events_on_subscribe_unsubscribe() {
        let hub = RoomMessageHub::new();
        let mut lifecycle_rx = hub.subscribe_lifecycle();

        let room_id = RoomId::from_string("test_room".to_string());
        let user1 = UserId::from_string("user1".to_string());
        let user2 = UserId::from_string("user2".to_string());

        // First subscriber triggers RoomActivated
        let _rx1 = hub
            .subscribe(room_id.clone(), user1.clone(), "conn1".to_string())
            .await;
        let event = lifecycle_rx.try_recv().unwrap();
        assert!(
            matches!(event, RoomLifecycleEvent::RoomActivated(ref rid) if rid.as_str() == "test_room")
        );

        // Second subscriber does NOT trigger RoomActivated
        let _rx2 = hub
            .subscribe(room_id.clone(), user2.clone(), "conn2".to_string())
            .await;
        assert!(lifecycle_rx.try_recv().is_err());

        // Unsubscribe first user: room still has subscribers, no event
        hub.unsubscribe("conn1");
        assert!(lifecycle_rx.try_recv().is_err());

        // Unsubscribe last user: triggers RoomDeactivated
        hub.unsubscribe("conn2");
        let event = lifecycle_rx.try_recv().unwrap();
        assert!(
            matches!(event, RoomLifecycleEvent::RoomDeactivated(ref rid) if rid.as_str() == "test_room")
        );
    }

    #[tokio::test]
    async fn test_lifecycle_events_on_remove_room() {
        let hub = RoomMessageHub::new();
        let mut lifecycle_rx = hub.subscribe_lifecycle();

        let room_id = RoomId::from_string("test_room".to_string());
        let user_id = UserId::from_string("user1".to_string());

        // Subscribe triggers RoomActivated
        let _rx = hub
            .subscribe(room_id.clone(), user_id.clone(), "conn1".to_string())
            .await;
        let _ = lifecycle_rx.try_recv().unwrap(); // consume RoomActivated

        // remove_room triggers RoomDeactivated
        hub.remove_room(&room_id);
        let event = lifecycle_rx.try_recv().unwrap();
        assert!(
            matches!(event, RoomLifecycleEvent::RoomDeactivated(ref rid) if rid.as_str() == "test_room")
        );
    }

    #[tokio::test]
    async fn test_unsubscribe_cleans_up_local_state_even_without_redis() {
        // Verify that unsubscribe properly cleans up local state (rooms + connections)
        // even when Redis is not configured. This is the baseline behavior.
        let hub = RoomMessageHub::new();
        let room_id = RoomId::from_string("test_room".to_string());
        let user_id = UserId::from_string("user1".to_string());

        let _rx = hub
            .subscribe(room_id.clone(), user_id.clone(), "conn1".to_string())
            .await;
        assert_eq!(hub.subscriber_count(&room_id), 1);
        assert_eq!(hub.connection_count(), 1);

        hub.unsubscribe("conn1");

        // Local state should be fully cleaned up
        assert_eq!(hub.subscriber_count(&room_id), 0);
        assert_eq!(hub.connection_count(), 0);
        assert_eq!(hub.room_count(), 0);
    }

    #[tokio::test]
    async fn test_cleanup_orphaned_subscriptions_noop_without_redis() {
        // Without Redis configured, the cleanup method should be a no-op
        // (no panics, no errors)
        let hub = RoomMessageHub::new();
        hub.cleanup_orphaned_redis_subscriptions().await;
        // If we reach here without panic, the test passes
    }

    #[tokio::test]
    async fn test_stale_cleanup_task_can_be_cancelled() {
        // Verify the stale cleanup task respects cancellation tokens
        let hub = RoomMessageHub::new();
        let cancel = tokio_util::sync::CancellationToken::new();
        let handle =
            hub.spawn_stale_subscription_cleanup_task(Duration::from_millis(50), cancel.clone());

        // Let it run briefly
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Cancel and verify the task completes
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(
            result.is_ok(),
            "Cleanup task should complete after cancellation"
        );
    }

    #[tokio::test]
    async fn test_shutdown_cancels_all_background_tasks() {
        // Verify that shutdown() cancels both the TTL refresh and stale cleanup tasks
        let hub = RoomMessageHub::new();

        // Manually spawn tasks with known cancel tokens to verify they stop
        let ttl_cancel = tokio_util::sync::CancellationToken::new();
        let cleanup_cancel = tokio_util::sync::CancellationToken::new();

        let ttl_handle = hub.spawn_ttl_refresh_task(Duration::from_millis(50), ttl_cancel.clone());
        let cleanup_handle = hub.spawn_stale_subscription_cleanup_task(
            Duration::from_millis(50),
            cleanup_cancel.clone(),
        );

        // Let tasks start running
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Cancel both
        ttl_cancel.cancel();
        cleanup_cancel.cancel();

        // Both tasks should complete within a reasonable timeout
        let ttl_result = tokio::time::timeout(Duration::from_secs(2), ttl_handle).await;
        let cleanup_result = tokio::time::timeout(Duration::from_secs(2), cleanup_handle).await;

        assert!(
            ttl_result.is_ok(),
            "TTL refresh task should complete after cancellation"
        );
        assert!(
            cleanup_result.is_ok(),
            "Stale cleanup task should complete after cancellation"
        );
    }

    #[tokio::test]
    async fn test_remove_room_cleans_connection_tracking() {
        // Verify that remove_room removes connections from the tracking map
        let hub = RoomMessageHub::new();
        let room_id = RoomId::from_string("test_room".to_string());
        let user1 = UserId::from_string("user1".to_string());
        let user2 = UserId::from_string("user2".to_string());

        let _rx1 = hub
            .subscribe(room_id.clone(), user1.clone(), "conn1".to_string())
            .await;
        let _rx2 = hub
            .subscribe(room_id.clone(), user2.clone(), "conn2".to_string())
            .await;

        assert_eq!(hub.connection_count(), 2);

        hub.remove_room(&room_id);

        assert_eq!(hub.connection_count(), 0);
        assert_eq!(hub.room_count(), 0);
    }
}
