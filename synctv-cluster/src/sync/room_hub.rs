use dashmap::DashMap;
use redis::AsyncCommands;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use synctv_core::models::id::{RoomId, UserId};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

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
/// Messages are dropped with a warning when a subscriber is too slow.
const SUBSCRIBER_CHANNEL_CAPACITY: usize = 256;

/// Number of consecutive drops before automatically disconnecting a slow subscriber.
const MAX_CONSECUTIVE_DROPS: u32 = 10;

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
    /// Map of `room_id` -> list of subscribers (local cache)
    rooms: Arc<DashMap<RoomId, Vec<Subscriber>>>,

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
        }
    }

    /// Enable distributed subscription state via Redis.
    ///
    /// When Redis is configured, subscription relationships are persisted to Redis
    /// for cross-replica visibility and recovery after restarts. Local DashMaps
    /// remain as a fast cache for message routing.
    #[must_use]
    pub fn with_redis(mut self, conn: redis::aio::ConnectionManager, key_prefix: &str) -> Self {
        self.redis_conn = Some(conn);
        self.redis_key_prefix = key_prefix.to_string();
        self
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
                entry.get_mut().push(subscriber);
                false
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(vec![subscriber]);
                true
            }
        };

        // Track connection for cleanup
        self.connections
            .insert(connection_id.clone(), (room_id.clone(), user_id.clone()));

        // Persist to Redis for cross-replica visibility (best-effort)
        if let Some(ref conn) = self.redis_conn {
            let room_key = format!("{}room_hub:room:{}", self.redis_key_prefix, room_id.as_str());
            let conn_key = format!("{}room_hub:conn:{}", self.redis_key_prefix, connection_id);

            let mut conn_clone = conn.clone();
            let user_id_str = user_id.as_str().to_string();
            let room_id_str = room_id.as_str().to_string();
            let connection_id_clone = connection_id.clone();
            let ttl_secs = self.redis_key_ttl_secs;

            // Spawn best-effort Redis update (don't block the subscribe path)
            tokio::spawn(async move {
                // Store room -> {connection_id: user_id} mapping
                if let Err(e) = conn_clone.hset::<_, _, _, ()>(&room_key, &connection_id_clone, &user_id_str).await {
                    warn!("Failed to persist room subscription to Redis: {e}");
                }
                // Set TTL on room key so stale data expires if the node crashes
                let _: Result<(), _> = conn_clone.expire::<_, ()>(&room_key, ttl_secs).await;

                // Store connection -> room_id mapping for cleanup, with TTL
                if let Err(e) = conn_clone.set_ex::<_, _, ()>(&conn_key, &room_id_str, ttl_secs as u64).await {
                    warn!("Failed to persist connection mapping to Redis: {e}");
                }
            });
        }

        // Emit lifecycle event if this is the first subscriber for the room
        if is_new_room {
            let _ = self.lifecycle_tx.send(RoomLifecycleEvent::RoomActivated(room_id.clone()));
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

            // Atomically remove subscriber and clean up empty room using
            // DashMap's `remove_if` to avoid the TOCTOU race between checking
            // `is_empty()`, dropping the guard, and calling `remove()`. A
            // concurrent `subscribe` could insert a new subscriber between
            // the guard drop and the remove, causing the new subscriber to
            // be silently lost.
            //
            // We first retain to remove the target subscriber, then use
            // `remove_if` which holds the shard lock while checking emptiness
            // and removing the entry atomically.
            if let Some(mut subscribers) = self.rooms.get_mut(&room_id) {
                subscribers.retain(|sub| sub.connection_id != connection_id);
                // We must drop the RefMut before calling remove_if, since both
                // acquire the same shard lock and would deadlock.
                drop(subscribers);
            }

            // Atomically remove the room entry only if it's still empty.
            // If a concurrent subscribe added a new subscriber between the
            // retain and this call, the entry won't be empty and won't be removed.
            if self.rooms.remove_if(&room_id, |_, subscribers| subscribers.is_empty()).is_some() {
                room_deactivated = true;
                debug!(room_id = %room_id.as_str(), "Room has no more subscribers, removed");
            }

            // Remove from Redis (best-effort, don't block unsubscribe path)
            if let Some(ref conn) = self.redis_conn {
                let room_key = format!("{}room_hub:room:{}", self.redis_key_prefix, room_id.as_str());
                let conn_key = format!("{}room_hub:conn:{}", self.redis_key_prefix, connection_id);
                let mut conn_clone = conn.clone();
                let connection_id_owned = connection_id.to_string();

                tokio::spawn(async move {
                    // Remove connection from room's subscriber hash
                    if let Err(e) = conn_clone.hdel::<_, _, ()>(&room_key, &connection_id_owned).await {
                        warn!("Failed to remove room subscription from Redis: {e}");
                    }
                    // Remove connection mapping
                    if let Err(e) = conn_clone.del::<_, ()>(&conn_key).await {
                        warn!("Failed to remove connection mapping from Redis: {e}");
                    }
                });
            }

            // Emit lifecycle event if the last subscriber left
            if room_deactivated {
                let _ = self.lifecycle_tx.send(RoomLifecycleEvent::RoomDeactivated(room_id.clone()));
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
    pub fn broadcast(&self, room_id: &RoomId, event: ClusterEvent) -> usize {
        let mut sent_count = 0;
        let mut failed_connections = Vec::new();

        // LOCK ORDERING: Acquire `rooms` read guard in a scoped block. The guard
        // MUST be dropped before calling `unsubscribe()`, which acquires write
        // guards on both `rooms` (via `get_mut` / `remove_if`) and `connections`.
        // Holding the read guard across the `unsubscribe` call would deadlock on
        // the same DashMap shard.
        {
            let subscribers_guard = self.rooms.get(room_id);
            if let Some(subscribers) = &subscribers_guard {
                for subscriber in subscribers.iter() {
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
                            let drops = subscriber.consecutive_drops.fetch_add(1, Ordering::Relaxed) + 1;
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

    /// Broadcast an event to a specific user in a room
    pub fn broadcast_to_user(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        event: ClusterEvent,
    ) -> usize {
        let mut sent_count = 0;
        let mut failed_connections = Vec::new();

        // LOCK ORDERING: Same pattern as broadcast() -- scoped read guard on
        // `rooms` must be dropped before calling `unsubscribe()`.
        {
            let subscribers_guard = self.rooms.get(room_id);
            if let Some(subscribers) = &subscribers_guard {
                for subscriber in subscribers.iter() {
                    if subscriber.user_id == *user_id {
                        match subscriber.sender.try_send(event.clone()) {
                            Ok(()) => {
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
                                warn!(
                                    room_id = %room_id.as_str(),
                                    user_id = %subscriber.user_id.as_str(),
                                    connection_id = %subscriber.connection_id,
                                    event_type = %event.event_type(),
                                    "Subscriber channel full, dropping event for slow consumer"
                                );
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
            for subscriber in subscribers.iter() {
                if subscriber.connection_id == connection_id {
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
                    break;
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
            for sub in &subscribers {
                self.connections.remove(&sub.connection_id);
            }
            // Emit lifecycle event since the room is no longer active
            let _ = self.lifecycle_tx.send(RoomLifecycleEvent::RoomDeactivated(room_id.clone()));
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
                    .iter()
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
    pub async fn get_room_subscribers_distributed(&self, room_id: &RoomId) -> Vec<(UserId, ConnectionId)> {
        if let Some(ref conn) = self.redis_conn {
            let room_key = format!("{}room_hub:room:{}", self.redis_key_prefix, room_id.as_str());
            let mut conn_clone = conn.clone();

            match conn_clone.hgetall::<_, Vec<(String, String)>>(&room_key).await {
                Ok(entries) => {
                    return entries
                        .into_iter()
                        .map(|(conn_id, user_id_str)| {
                            (UserId::from_string(user_id_str), conn_id)
                        })
                        .collect();
                }
                Err(e) => {
                    warn!("Failed to fetch room subscribers from Redis, falling back to local: {e}");
                }
            }
        }

        // Fallback to local-only
        self.get_room_subscribers(room_id)
    }

    /// Recover subscription state from Redis on startup.
    ///
    /// This method scans Redis for persisted subscription relationships and
    /// logs the recovered room/subscriber counts. It does **not** populate the
    /// local `rooms` or `connections` DashMaps because:
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
    /// **Usage:** Call on startup to log how many subscriptions exist across the
    /// cluster. Useful for monitoring dashboards and verifying Redis state. Actual
    /// message routing requires clients to reconnect and call `subscribe()`.
    ///
    /// **Alternative approaches for true state recovery:**
    /// - Have clients reconnect automatically via exponential backoff after a
    ///   node restart (recommended).
    /// - Use Redis Streams instead of Pub/Sub so messages are persisted and can
    ///   be replayed on reconnect (requires architectural change).
    pub async fn recover_from_redis(&self) -> Result<usize, String> {
        let Some(ref conn) = self.redis_conn else {
            return Err("Redis not configured".to_string());
        };

        let pattern = format!("{}room_hub:room:*", self.redis_key_prefix);
        let mut conn_clone = conn.clone();
        let mut recovered = 0;

        // Scan for all room keys
        let keys: Vec<String> = conn_clone
            .keys(&pattern)
            .await
            .map_err(|e| format!("Failed to scan Redis keys: {e}"))?;

        for key in keys {
            // Extract room_id from key
            let room_id_str = key.trim_start_matches(&format!("{}room_hub:room:", self.redis_key_prefix));
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
                "Recovered room subscription state from Redis"
            );
        }

        Ok(recovered)
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
        let mut rx = hub.subscribe(room_id.clone(), user_id.clone(), "conn1".to_string()).await;

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
        let _rx = hub.subscribe(room_id.clone(), user_id.clone(), "conn1".to_string()).await;
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
        let mut rx1 = hub.subscribe(room_id.clone(), user1.clone(), "conn1".to_string()).await;
        let mut rx2 = hub.subscribe(room_id.clone(), user2.clone(), "conn2".to_string()).await;

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
        let mut rx1 = hub.subscribe(room_id.clone(), user1.clone(), "conn1".to_string()).await;
        let mut rx2 = hub.subscribe(room_id.clone(), user2.clone(), "conn2".to_string()).await;

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
        let _rx1 = hub.subscribe(room_id.clone(), user1.clone(), "conn1".to_string()).await;
        let event = lifecycle_rx.try_recv().unwrap();
        assert!(matches!(event, RoomLifecycleEvent::RoomActivated(ref rid) if rid.as_str() == "test_room"));

        // Second subscriber does NOT trigger RoomActivated
        let _rx2 = hub.subscribe(room_id.clone(), user2.clone(), "conn2".to_string()).await;
        assert!(lifecycle_rx.try_recv().is_err());

        // Unsubscribe first user: room still has subscribers, no event
        hub.unsubscribe("conn1");
        assert!(lifecycle_rx.try_recv().is_err());

        // Unsubscribe last user: triggers RoomDeactivated
        hub.unsubscribe("conn2");
        let event = lifecycle_rx.try_recv().unwrap();
        assert!(matches!(event, RoomLifecycleEvent::RoomDeactivated(ref rid) if rid.as_str() == "test_room"));
    }

    #[tokio::test]
    async fn test_lifecycle_events_on_remove_room() {
        let hub = RoomMessageHub::new();
        let mut lifecycle_rx = hub.subscribe_lifecycle();

        let room_id = RoomId::from_string("test_room".to_string());
        let user_id = UserId::from_string("user1".to_string());

        // Subscribe triggers RoomActivated
        let _rx = hub.subscribe(room_id.clone(), user_id.clone(), "conn1".to_string()).await;
        let _ = lifecycle_rx.try_recv().unwrap(); // consume RoomActivated

        // remove_room triggers RoomDeactivated
        hub.remove_room(&room_id);
        let event = lifecycle_rx.try_recv().unwrap();
        assert!(matches!(event, RoomLifecycleEvent::RoomDeactivated(ref rid) if rid.as_str() == "test_room"));
    }
}
