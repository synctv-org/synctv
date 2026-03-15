use dashmap::DashMap;
use redis::AsyncCommands;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use synctv_core::models::id::{RoomId, UserId};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Timeout for delivering critical events to slow consumers.
/// Critical events (kick, ban, room deletion) use a bounded wait instead of
/// fire-and-forget spawn to ensure they are reliably delivered before the
/// connection is closed.
const CRITICAL_EVENT_SEND_TIMEOUT: Duration = Duration::from_secs(5);

use super::events::ClusterEvent;

/// Notification about room lifecycle changes (first subscriber / last unsubscribe).
///
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

const fn requires_reliable_target_delivery(event: &ClusterEvent) -> bool {
    event.is_critical() || matches!(event, ClusterEvent::WebRTCSignaling { .. })
}

fn block_on_reliable_delivery(
    sender: MessageSender,
    event: ClusterEvent,
    room_id: RoomId,
    connection_id: ConnectionId,
) -> bool {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if matches!(
            handle.runtime_flavor(),
            tokio::runtime::RuntimeFlavor::MultiThread
        ) {
            tokio::task::block_in_place(|| {
                handle.block_on(deliver_reliable_event(
                    sender,
                    event,
                    room_id,
                    connection_id,
                ))
            })
        } else {
            warn!(
                room_id = %room_id.as_str(),
                connection_id = %connection_id,
                "Reliable targeted delivery cannot block on a current-thread Tokio runtime; falling back to async retry"
            );
            try_spawn(deliver_reliable_event(
                sender,
                event,
                room_id,
                connection_id,
            ))
            .is_some()
        }
    } else {
        false
    }
}

async fn deliver_reliable_event(
    sender: MessageSender,
    event: ClusterEvent,
    room_id: RoomId,
    connection_id: ConnectionId,
) -> bool {
    let event_type = event.event_type().to_string();
    match tokio::time::timeout(CRITICAL_EVENT_SEND_TIMEOUT, sender.send(event)).await {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            warn!(
                room_id = %room_id.as_str(),
                connection_id = %connection_id,
                event_type = %event_type,
                "Failed to deliver reliable event (channel closed): {e}"
            );
            false
        }
        Err(_) => {
            warn!(
                room_id = %room_id.as_str(),
                connection_id = %connection_id,
                event_type = %event_type,
                timeout_secs = CRITICAL_EVENT_SEND_TIMEOUT.as_secs(),
                "Reliable event delivery timed out"
            );
            false
        }
    }
}

fn try_spawn<F>(future: F) -> Option<JoinHandle<F::Output>>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::runtime::Handle::try_current()
        .map(|handle| handle.spawn(future))
        .ok()
}

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
    redis_conn: Option<RedisConnHandle>,

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

    /// Local retry queue for Redis subscription cleanup operations that failed
    /// during `unsubscribe()`.
    ///
    /// This only tracks subscriptions that were previously owned by this hub
    /// instance. It must never be reconstructed by scanning Redis globally,
    /// because replicas share the same key prefix and would otherwise delete
    /// each other's still-active subscriptions.
    pending_redis_cleanup: Arc<DashMap<ConnectionId, RoomId>>,

    /// Guards idempotent startup of Redis-backed background tasks.
    background_tasks_started: Arc<AtomicBool>,
    ttl_refresh_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
    stale_cleanup_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

#[derive(Clone, Debug)]
enum RedisConnHandle {
    Direct(redis::aio::ConnectionManager),
    Shared(std::sync::Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>),
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
            pending_redis_cleanup: Arc::new(DashMap::new()),
            background_tasks_started: Arc::new(AtomicBool::new(false)),
            ttl_refresh_handle: Arc::new(Mutex::new(None)),
            stale_cleanup_handle: Arc::new(Mutex::new(None)),
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
        self.redis_conn = Some(RedisConnHandle::Direct(conn));
        self.redis_key_prefix = key_prefix.to_string();
        self.start();
        self
    }

    #[must_use]
    pub fn with_shared_redis(
        mut self,
        conn: std::sync::Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
        key_prefix: &str,
    ) -> Self {
        self.redis_conn = Some(RedisConnHandle::Shared(conn));
        self.redis_key_prefix = key_prefix.to_string();
        self.start();
        self
    }

    async fn redis_conn_clone(&self) -> Option<redis::aio::ConnectionManager> {
        match &self.redis_conn {
            Some(RedisConnHandle::Direct(conn)) => Some(conn.clone()),
            Some(RedisConnHandle::Shared(conn)) => Some(conn.read().await.clone()),
            None => None,
        }
    }

    /// Start Redis-backed background tasks if Redis is configured and a Tokio runtime exists.
    ///
    /// Safe to call multiple times. When called without a Tokio runtime, no
    /// tasks are started and the hub remains usable; the next call from within
    /// an async runtime will start the tasks.
    pub fn start(&self) {
        if self.redis_conn.is_none() {
            return;
        }

        if self.background_tasks_started.swap(true, Ordering::AcqRel) {
            return;
        }

        if tokio::runtime::Handle::try_current().is_err() {
            self.background_tasks_started
                .store(false, Ordering::Release);
            warn!("RoomMessageHub::start() called without Tokio runtime; deferring background task startup");
            return;
        }

        let ttl_cancel = (*self.ttl_refresh_cancel).clone();
        // Use 40% of TTL as the refresh interval (at most 120s, at least 30s)
        let refresh_interval_secs =
            (self.redis_key_ttl_secs as f64 * 0.4).clamp(30.0, 120.0) as u64;
        let stale_cancel = (*self.stale_cleanup_cancel).clone();
        let ttl_handle =
            self.spawn_ttl_refresh_task(Duration::from_secs(refresh_interval_secs), ttl_cancel);
        let cleanup_handle =
            self.spawn_stale_subscription_cleanup_task(Duration::from_mins(1), stale_cancel);

        *self
            .ttl_refresh_handle
            .lock()
            .expect("ttl refresh handle mutex poisoned") = Some(ttl_handle);
        *self
            .stale_cleanup_handle
            .lock()
            .expect("stale cleanup handle mutex poisoned") = Some(cleanup_handle);
    }

    /// Cancel the auto-spawned background tasks (TTL refresh and stale cleanup)
    /// and wait for them to exit.
    pub async fn shutdown(&self) {
        self.ttl_refresh_cancel.cancel();
        self.stale_cleanup_cancel.cancel();

        let ttl_handle = self
            .ttl_refresh_handle
            .lock()
            .expect("ttl refresh handle mutex poisoned")
            .take();
        if let Some(handle) = ttl_handle {
            let _ = handle.await;
        }
        let stale_cleanup_handle = self
            .stale_cleanup_handle
            .lock()
            .expect("stale cleanup handle mutex poisoned")
            .take();
        if let Some(handle) = stale_cleanup_handle {
            let _ = handle.await;
        }
        self.background_tasks_started
            .store(false, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn background_shutdown_requested(&self) -> bool {
        self.ttl_refresh_cancel.is_cancelled() && self.stale_cleanup_cancel.is_cancelled()
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
    ) -> crate::Result<mpsc::Receiver<ClusterEvent>> {
        self.start();
        let (tx, rx) = mpsc::channel(SUBSCRIBER_CHANNEL_CAPACITY);

        let subscriber = Subscriber {
            connection_id: connection_id.clone(),
            user_id: user_id.clone(),
            sender: tx,
            consecutive_drops: Arc::new(AtomicU32::new(0)),
        };

        // A reused connection ID must never inherit a failed cleanup retry from
        // an older subscription lifecycle. Clear any stale local retry entry
        // before making the new subscription visible.
        self.pending_redis_cleanup.remove(&connection_id);

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
        // In Redis-backed mode this is part of the subscription contract: if the
        // distributed state cannot be written, the local subscription must be
        // rolled back so callers do not observe a false-success join.
        if let Some(mut conn_clone) = self.redis_conn_clone().await {
            let room_key = format!(
                "{}room_hub:room:{}",
                self.redis_key_prefix,
                room_id.as_str()
            );
            let conn_key = format!("{}room_hub:conn:{}", self.redis_key_prefix, connection_id);
            let user_id_str = user_id.as_str().to_string();
            let room_id_str = room_id.as_str().to_string();
            let ttl_secs = self.redis_key_ttl_secs;

            // Store room -> {connection_id: user_id} mapping
            if let Err(e) = conn_clone
                .hset::<_, _, _, ()>(&room_key, &connection_id, &user_id_str)
                .await
            {
                self.rollback_local_subscription(&room_id, &connection_id);
                return Err(crate::error::Error::Redis(format!(
                    "Failed to persist room subscription to Redis: {e}"
                )));
            }
            if let Err(e) = conn_clone.expire::<_, ()>(&room_key, ttl_secs).await {
                let _ = conn_clone.hdel::<_, _, ()>(&room_key, &connection_id).await;
                self.rollback_local_subscription(&room_id, &connection_id);
                return Err(crate::error::Error::Redis(format!(
                    "Failed to refresh room subscription TTL in Redis: {e}"
                )));
            }

            if let Err(e) = conn_clone
                .set_ex::<_, _, ()>(&conn_key, &room_id_str, ttl_secs as u64)
                .await
            {
                let _ = conn_clone.hdel::<_, _, ()>(&room_key, &connection_id).await;
                self.rollback_local_subscription(&room_id, &connection_id);
                return Err(crate::error::Error::Redis(format!(
                    "Failed to persist connection mapping to Redis: {e}"
                )));
            }
        }

        // Emit lifecycle event if this is the first subscriber for the room
        if is_new_room {
            let _ = self
                .lifecycle_tx
                .send(RoomLifecycleEvent::RoomActivated(room_id.clone()));
        }

        info!(
            room_id = %room_id.as_str(),
            user_id = %user_id.as_str(),
            connection_id = %connection_id,
            "Client subscribed to room"
        );

        Ok(rx)
    }

    fn rollback_local_subscription(&self, room_id: &RoomId, connection_id: &str) {
        self.pending_redis_cleanup.remove(connection_id);
        self.connections.remove(connection_id);
        if let Some(mut subscribers) = self.rooms.get_mut(room_id) {
            subscribers.remove(connection_id);
            drop(subscribers);
        }
        self.rooms
            .remove_if(room_id, |_, subscribers| subscribers.is_empty());
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
            if let Some(redis_conn) = self.redis_conn.clone() {
                let room_key = format!(
                    "{}room_hub:room:{}",
                    self.redis_key_prefix,
                    room_id.as_str()
                );
                let conn_key = format!("{}room_hub:conn:{}", self.redis_key_prefix, connection_id);
                let connection_id_owned = connection_id.to_string();
                let room_id_for_retry = room_id.clone();
                let pending_redis_cleanup = Arc::clone(&self.pending_redis_cleanup);
                let connection_id_for_log = connection_id_owned.clone();
                let room_id_for_log = room_id_for_retry.clone();
                let cleanup_connection_id = connection_id_owned.clone();
                let cleanup_room_id = room_id_for_retry.clone();
                let cleanup_pending_redis_cleanup = Arc::clone(&pending_redis_cleanup);

                let cleanup_fut = async move {
                    let mut conn_clone = match redis_conn {
                        RedisConnHandle::Direct(conn) => conn,
                        RedisConnHandle::Shared(conn) => conn.read().await.clone(),
                    };
                    let mut cleanup_failed = false;

                    // Remove connection from room's subscriber hash
                    if let Err(e) = conn_clone
                        .hdel::<_, _, ()>(&room_key, &cleanup_connection_id)
                        .await
                    {
                        cleanup_failed = true;
                        warn!("Failed to remove room subscription from Redis: {e}");
                    }
                    // Remove connection mapping
                    if let Err(e) = conn_clone.del::<_, ()>(&conn_key).await {
                        cleanup_failed = true;
                        warn!("Failed to remove connection mapping from Redis: {e}");
                    }

                    if cleanup_failed {
                        cleanup_pending_redis_cleanup
                            .insert(cleanup_connection_id, cleanup_room_id);
                    } else {
                        cleanup_pending_redis_cleanup.remove(&cleanup_connection_id);
                    }
                };

                if try_spawn(cleanup_fut).is_none() {
                    pending_redis_cleanup.insert(connection_id_owned, room_id_for_retry);
                    warn!(
                        connection_id = %connection_id_for_log,
                        room_id = %room_id_for_log.as_str(),
                        "No Tokio runtime available for Redis unsubscribe cleanup; deferred to retry loop/TTL"
                    );
                }
            }

            // Emit lifecycle event if the last subscriber left
            if room_deactivated {
                let _ = self
                    .lifecycle_tx
                    .send(RoomLifecycleEvent::RoomDeactivated(room_id.clone()));
            }

            info!(
                room_id = %room_id.as_str(),
                user_id = %user_id.as_str(),
                connection_id = %connection_id,
                "Client unsubscribed from room"
            );
        } else {
            debug!(
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
                                if block_on_reliable_delivery(
                                    subscriber.sender.clone(),
                                    event.clone(),
                                    room_id.clone(),
                                    subscriber.connection_id.clone(),
                                ) {
                                    sent_count += 1;
                                } else {
                                    failed_connections.push(subscriber.connection_id.clone());
                                }
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

    /// Broadcast an event to all subscribers in a room and await reliable
    /// delivery for critical events whose senders must remain alive until the
    /// message is queued.
    ///
    /// This is used for destructive follow-up actions such as `RoomDeleted`,
    /// where callers need to know that critical notifications were either
    /// queued or timed out before they tear down the room state.
    pub async fn broadcast_reliably(&self, room_id: &RoomId, event: ClusterEvent) -> usize {
        let mut sent_count = 0;
        let mut failed_connections = Vec::new();
        let is_critical = event.is_critical();
        let mut reliable_deliveries = Vec::new();

        {
            let subscribers_guard = self.rooms.get(room_id);
            if let Some(subscribers) = &subscribers_guard {
                for subscriber in subscribers.values() {
                    match subscriber.sender.try_send(event.clone()) {
                        Ok(()) => {
                            subscriber.consecutive_drops.store(0, Ordering::Relaxed);
                            sent_count += 1;
                        }
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            if is_critical {
                                reliable_deliveries.push(deliver_reliable_event(
                                    subscriber.sender.clone(),
                                    event.clone(),
                                    room_id.clone(),
                                    subscriber.connection_id.clone(),
                                ));
                            } else {
                                let drops =
                                    subscriber.consecutive_drops.fetch_add(1, Ordering::Relaxed)
                                        + 1;
                                if drops >= MAX_CONSECUTIVE_DROPS {
                                    failed_connections.push(subscriber.connection_id.clone());
                                }
                            }
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            failed_connections.push(subscriber.connection_id.clone());
                        }
                    }
                }
            }
        }

        for delivery in reliable_deliveries {
            if delivery.await {
                sent_count += 1;
            }
        }

        for conn_id in failed_connections {
            self.unsubscribe(&conn_id);
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
                                    if block_on_reliable_delivery(
                                        subscriber.sender.clone(),
                                        event.clone(),
                                        room_id.clone(),
                                        subscriber.connection_id.clone(),
                                    ) {
                                        sent_count += 1;
                                    } else {
                                        failed_connections.push(subscriber.connection_id.clone());
                                    }
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
        let reliable_target_delivery = requires_reliable_target_delivery(&event);

        if let Some(subscribers) = self.rooms.get(room_id) {
            if let Some(subscriber) = subscribers.get(connection_id) {
                let event_type = event.event_type().to_string();
                match subscriber.sender.try_send(event.clone()) {
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
                        if reliable_target_delivery {
                            let delivered = block_on_reliable_delivery(
                                subscriber.sender.clone(),
                                event,
                                room_id.clone(),
                                subscriber.connection_id.clone(),
                            );
                            if delivered {
                                result = 1;
                            } else {
                                failed_connection = Some(subscriber.connection_id.clone());
                            }
                        } else {
                            warn!(
                                room_id = %room_id.as_str(),
                                connection_id = %connection_id,
                                event_type = %event_type,
                                "Subscriber channel full, dropping targeted event"
                            );
                        }
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
            let removed_subscribers: Vec<(ConnectionId, RoomId)> = subscribers
                .values()
                .map(|sub| {
                    self.connections.remove(&sub.connection_id);
                    (sub.connection_id.clone(), room_id.clone())
                })
                .collect();

            for (connection_id, room_id) in &removed_subscribers {
                self.schedule_redis_cleanup(connection_id.clone(), room_id.clone());
            }
            // Emit lifecycle event since the room is no longer active
            let _ = self
                .lifecycle_tx
                .send(RoomLifecycleEvent::RoomDeactivated(room_id.clone()));
            info!(
                room_id = %room_id.as_str(),
                removed_connections = subscribers.len(),
                "Removed all subscribers for deleted room"
            );
        }
    }

    fn schedule_redis_cleanup(&self, connection_id: ConnectionId, room_id: RoomId) {
        let Some(redis_conn) = self.redis_conn.clone() else {
            return;
        };

        let room_key = format!(
            "{}room_hub:room:{}",
            self.redis_key_prefix,
            room_id.as_str()
        );
        let conn_key = format!("{}room_hub:conn:{}", self.redis_key_prefix, connection_id);
        let pending_redis_cleanup = Arc::clone(&self.pending_redis_cleanup);
        let connection_id_for_log = connection_id.clone();
        let room_id_for_log = room_id.clone();
        let cleanup_connection_id = connection_id.clone();
        let cleanup_room_id = room_id.clone();
        let cleanup_pending_redis_cleanup = Arc::clone(&pending_redis_cleanup);

        let cleanup_fut = async move {
            let mut conn_clone = match redis_conn {
                RedisConnHandle::Direct(conn) => conn,
                RedisConnHandle::Shared(conn) => conn.read().await.clone(),
            };
            let mut cleanup_failed = false;

            if let Err(e) = conn_clone
                .hdel::<_, _, ()>(&room_key, &cleanup_connection_id)
                .await
            {
                cleanup_failed = true;
                warn!("Failed to remove room subscription from Redis: {e}");
            }
            if let Err(e) = conn_clone.del::<_, ()>(&conn_key).await {
                cleanup_failed = true;
                warn!("Failed to remove connection mapping from Redis: {e}");
            }

            if cleanup_failed {
                cleanup_pending_redis_cleanup.insert(cleanup_connection_id, cleanup_room_id);
            } else {
                cleanup_pending_redis_cleanup.remove(&cleanup_connection_id);
            }
        };

        if try_spawn(cleanup_fut).is_none() {
            pending_redis_cleanup.insert(connection_id, room_id);
            warn!(
                connection_id = %connection_id_for_log,
                room_id = %room_id_for_log.as_str(),
                "No Tokio runtime available for Redis room cleanup; deferred to retry loop/TTL"
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
            let mut conn_clone = match conn {
                RedisConnHandle::Direct(conn) => conn.clone(),
                RedisConnHandle::Shared(conn) => conn.read().await.clone(),
            };

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
        let Some(mut conn_clone) = self.redis_conn_clone().await else {
            return Err("Redis not configured".to_string());
        };

        let pattern = format!("{}room_hub:room:*", self.redis_key_prefix);
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
        let Some(mut conn) = self.redis_conn_clone().await else {
            return;
        };
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
    /// retry loop, stale entries would accumulate until their TTL expires.
    ///
    /// Only locally-owned failed cleanups are retried here. Global Redis scans
    /// are intentionally avoided because replicas share a key namespace and one
    /// node cannot reliably distinguish its own stale entries from another
    /// replica's still-active subscriptions.
    async fn cleanup_orphaned_redis_subscriptions(&self) {
        let Some(mut conn) = self.redis_conn_clone().await else {
            return;
        };
        let mut cleaned = 0u64;
        let mut errors = 0u64;

        let pending: Vec<(ConnectionId, RoomId)> = self
            .pending_redis_cleanup
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();

        for (connection_id, room_id) in pending {
            if self.connections.contains_key(&connection_id) {
                continue;
            }

            let room_key = format!(
                "{}room_hub:room:{}",
                self.redis_key_prefix,
                room_id.as_str()
            );
            let conn_key = format!("{}room_hub:conn:{}", self.redis_key_prefix, connection_id);

            let mut pipe = redis::pipe();
            pipe.hdel(&room_key, &connection_id).ignore();
            pipe.del(&conn_key).ignore();

            match pipe.query_async::<()>(&mut conn).await {
                Ok(()) => {
                    cleaned += 1;
                    self.pending_redis_cleanup.remove(&connection_id);
                    debug!(
                        connection_id = %connection_id,
                        room_id = %room_id.as_str(),
                        "Retried failed Redis subscription cleanup"
                    );
                }
                Err(e) => {
                    errors += 1;
                    warn!(
                        connection_id = %connection_id,
                        room_id = %room_id.as_str(),
                        error = %e,
                        "Failed to retry Redis subscription cleanup"
                    );
                }
            }
        }

        if cleaned > 0 || errors > 0 {
            info!(
                cleanup_retried = cleaned,
                cleanup_errors = errors,
                pending_cleanup = self.pending_redis_cleanup.len(),
                "Retried failed Redis subscription cleanup entries"
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
        try_spawn(async move {
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
        .expect("spawn_stale_subscription_cleanup_task requires a Tokio runtime")
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
        try_spawn(async move {
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
        .expect("spawn_ttl_refresh_task requires a Tokio runtime")
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
            .await
            .expect("subscribe should succeed");

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
            .await
            .expect("subscribe should succeed");
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
            .await
            .expect("subscribe should succeed");
        let mut rx2 = hub
            .subscribe(room_id.clone(), user2.clone(), "conn2".to_string())
            .await
            .expect("subscribe should succeed");

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
            .await
            .expect("subscribe should succeed");
        let mut rx2 = hub
            .subscribe(room_id.clone(), user2.clone(), "conn2".to_string())
            .await
            .expect("subscribe should succeed");

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
            .await
            .expect("subscribe should succeed");
        let event = lifecycle_rx.try_recv().unwrap();
        assert!(
            matches!(event, RoomLifecycleEvent::RoomActivated(ref rid) if rid.as_str() == "test_room")
        );

        // Second subscriber does NOT trigger RoomActivated
        let _rx2 = hub
            .subscribe(room_id.clone(), user2.clone(), "conn2".to_string())
            .await
            .expect("subscribe should succeed");
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
            .await
            .expect("subscribe should succeed");
        let _ = lifecycle_rx.try_recv().unwrap(); // consume RoomActivated

        // remove_room triggers RoomDeactivated
        hub.remove_room(&room_id);
        let event = lifecycle_rx.try_recv().unwrap();
        assert!(
            matches!(event, RoomLifecycleEvent::RoomDeactivated(ref rid) if rid.as_str() == "test_room")
        );
    }

    #[tokio::test]
    async fn test_broadcast_reliably_waits_for_critical_event_queue_space() {
        let hub = RoomMessageHub::new();
        let room_id = RoomId::from_string("room-critical".to_string());
        let deleted_by = UserId::from_string("admin".to_string());
        let filler_user = UserId::from_string("user".to_string());

        let mut rx = hub
            .subscribe(room_id.clone(), filler_user, "conn-critical".to_string())
            .await
            .expect("subscribe should succeed");

        for _ in 0..SUBSCRIBER_CHANNEL_CAPACITY {
            let sent = hub.broadcast(
                &room_id,
                ClusterEvent::ChatMessage {
                    event_id: nanoid::nanoid!(16),
                    room_id: room_id.clone(),
                    user_id: deleted_by.clone(),
                    username: "filler".to_string(),
                    message: "fill".to_string(),
                    timestamp: Utc::now(),
                    position: None,
                    color: None,
                },
            );
            assert_eq!(sent, 1, "filler message should enqueue");
        }

        let room_deleted = ClusterEvent::RoomDeleted {
            event_id: nanoid::nanoid!(16),
            room_id: room_id.clone(),
            deleted_by,
            timestamp: Utc::now(),
        };

        let hub_for_task = hub.clone();
        let room_for_task = room_id.clone();
        let broadcast_task = tokio::spawn(async move {
            hub_for_task
                .broadcast_reliably(&room_for_task, room_deleted)
                .await
        });

        tokio::task::yield_now().await;
        assert!(
            !broadcast_task.is_finished(),
            "critical broadcast should wait until the subscriber channel has capacity"
        );

        let drained = rx.recv().await.expect("filler message should be present");
        assert!(matches!(drained, ClusterEvent::ChatMessage { .. }));

        let sent = tokio::time::timeout(Duration::from_secs(1), broadcast_task)
            .await
            .expect("reliable broadcast should complete after capacity is freed")
            .expect("broadcast task should not panic");
        assert_eq!(
            sent, 1,
            "critical event should count as delivered once queued"
        );

        let mut saw_room_deleted = false;
        for _ in 0..SUBSCRIBER_CHANNEL_CAPACITY {
            let msg = tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect("queued message should arrive")
                .expect("channel should stay open");
            if matches!(msg, ClusterEvent::RoomDeleted { .. }) {
                saw_room_deleted = true;
                break;
            }
        }

        assert!(
            saw_room_deleted,
            "critical room deletion event should be queued before cleanup proceeds"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_broadcast_waits_for_critical_event_queue_space() {
        let hub = Arc::new(RoomMessageHub::new());
        let room_id = RoomId::from_string("room-critical-broadcast".to_string());
        let user_id = UserId::from_string("user-critical-broadcast".to_string());
        let mut rx = hub
            .subscribe(
                room_id.clone(),
                user_id,
                "conn-critical-broadcast".to_string(),
            )
            .await
            .expect("subscribe should succeed");

        for _ in 0..SUBSCRIBER_CHANNEL_CAPACITY {
            let sent = hub.broadcast(
                &room_id,
                ClusterEvent::ChatMessage {
                    event_id: nanoid::nanoid!(16),
                    room_id: room_id.clone(),
                    user_id: UserId::from_string("filler-user".to_string()),
                    username: "filler".to_string(),
                    message: "fill".to_string(),
                    timestamp: Utc::now(),
                    position: None,
                    color: None,
                },
            );
            assert_eq!(sent, 1, "filler message should enqueue");
        }

        let critical = ClusterEvent::RoomDeleted {
            event_id: nanoid::nanoid!(16),
            room_id: room_id.clone(),
            deleted_by: UserId::from_string("deleter".to_string()),
            timestamp: Utc::now(),
        };

        let hub_for_task = hub.clone();
        let room_for_task = room_id.clone();
        let broadcast_task =
            tokio::spawn(async move { hub_for_task.broadcast(&room_for_task, critical) });

        tokio::task::yield_now().await;
        assert!(
            !broadcast_task.is_finished(),
            "critical broadcast should wait until channel capacity is freed"
        );

        let drained = rx.recv().await.expect("filler message should be present");
        assert!(matches!(drained, ClusterEvent::ChatMessage { .. }));

        let sent = tokio::time::timeout(Duration::from_secs(1), broadcast_task)
            .await
            .expect("critical broadcast should complete after capacity is freed")
            .expect("broadcast task should not panic");
        assert_eq!(
            sent, 1,
            "critical broadcast must only count deliveries that were actually queued"
        );

        let mut saw_room_deleted = false;
        for _ in 0..SUBSCRIBER_CHANNEL_CAPACITY {
            let msg = tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect("queued message should arrive")
                .expect("channel should stay open");
            if matches!(msg, ClusterEvent::RoomDeleted { .. }) {
                saw_room_deleted = true;
                break;
            }
        }

        assert!(
            saw_room_deleted,
            "critical event must be queued before broadcast() returns success"
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
            .await
            .expect("subscribe should succeed");
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
    async fn test_cleanup_orphaned_subscriptions_only_tracks_local_failed_cleanup() {
        let hub = RoomMessageHub::new();

        hub.pending_redis_cleanup.insert(
            "conn_local".to_string(),
            RoomId::from_string("room_local".to_string()),
        );

        assert_eq!(hub.pending_redis_cleanup.len(), 1);

        hub.cleanup_orphaned_redis_subscriptions().await;

        assert_eq!(
            hub.pending_redis_cleanup.len(),
            1,
            "Without Redis, cleanup must not mutate locally tracked retry state"
        );
        assert!(hub.pending_redis_cleanup.contains_key("conn_local"));
    }

    #[tokio::test]
    async fn test_subscribe_clears_stale_pending_cleanup_for_reused_connection_id() {
        let hub = RoomMessageHub::new();
        let room_id = RoomId::from_string("room_reuse".to_string());
        let user_id = UserId::from_string("user_reuse".to_string());

        hub.pending_redis_cleanup.insert(
            "conn_reuse".to_string(),
            RoomId::from_string("old_room".to_string()),
        );

        let _rx = hub
            .subscribe(room_id, user_id, "conn_reuse".to_string())
            .await
            .expect("subscribe should succeed");

        assert!(
            !hub.pending_redis_cleanup.contains_key("conn_reuse"),
            "New subscription must clear stale pending cleanup for reused connection IDs"
        );
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

    #[test]
    fn test_start_without_redis_is_noop_even_without_runtime() {
        let hub = RoomMessageHub::new();
        hub.start();
        futures::executor::block_on(hub.shutdown());
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
            .await
            .expect("subscribe should succeed");
        let _rx2 = hub
            .subscribe(room_id.clone(), user2.clone(), "conn2".to_string())
            .await
            .expect("subscribe should succeed");

        assert_eq!(hub.connection_count(), 2);

        hub.remove_room(&room_id);

        assert_eq!(hub.connection_count(), 0);
        assert_eq!(hub.room_count(), 0);
    }
}
