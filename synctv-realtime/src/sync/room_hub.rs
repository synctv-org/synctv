use async_trait::async_trait;
use dashmap::DashMap;
use futures::stream::{FuturesUnordered, StreamExt};
use parking_lot::Mutex;
use redis::AsyncCommands;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use synctv_core::{
    models::id::{RoomId, UserId},
    RedisConnectionRuntime,
};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Timeout for delivering critical events to slow consumers.
/// Critical events (kick, ban, room deletion) use a bounded wait instead of
/// fire-and-forget spawn to ensure they are reliably delivered before the
/// connection is closed.
const CRITICAL_EVENT_SEND_TIMEOUT: Duration = Duration::from_secs(5);

use super::events::RealtimeEvent;
use super::runtime::RoomMessageRuntime;

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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConnectionId(String);

impl ConnectionId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for ConnectionId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ConnectionId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl AsRef<str> for ConnectionId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::ops::Deref for ConnectionId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl Borrow<str> for ConnectionId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

/// Capacity for per-subscriber message channels.
/// Must be large enough to absorb bursts of playback state updates (seek, pause,
/// play) without dropping critical synchronization messages.
const SUBSCRIBER_CHANNEL_CAPACITY: usize = 512;

/// Number of consecutive drops before automatically disconnecting a slow subscriber.
/// Set higher to tolerate transient bursts (e.g., rapid seek operations) without
/// prematurely disconnecting clients on slower networks.
const MAX_CONSECUTIVE_DROPS: u32 = 50;
const ROOM_INDEX_DIRECTORY_KEY_SUFFIX: &str = "room_hub:room_index";

const fn requires_reliable_target_delivery(event: &RealtimeEvent) -> bool {
    event.is_critical() || matches!(event, RealtimeEvent::WebRTCSignaling { .. })
}

async fn run_room_hub_redis_op<T, F>(
    timeout: Duration,
    operation: &str,
    future: F,
) -> Result<T, String>
where
    F: Future<Output = redis::RedisResult<T>>,
{
    match tokio::time::timeout(timeout, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(format!("Redis {operation} failed: {error}")),
        Err(_) => Err(format!(
            "Redis {operation} timed out after {}ms",
            timeout.as_millis()
        )),
    }
}

fn ttl_refresh_interval_secs(ttl_secs: i64) -> u64 {
    let refresh_secs = ttl_secs.saturating_mul(2).div_euclid(5).clamp(30, 120);
    u64::try_from(refresh_secs).unwrap_or(30)
}

fn room_members_after_prune(results: &[i64]) -> Result<i64, String> {
    results.last().copied().ok_or_else(|| {
        "Redis prune stale distributed room subscribers returned no HLEN result".to_string()
    })
}

fn log_best_effort_redis_cleanup(operation: &'static str, result: Result<(), String>) {
    if let Err(error) = result {
        warn!(operation, error = %error, "Best-effort Redis room hub cleanup failed");
    }
}

fn ttl_secs_unsigned(ttl_secs: i64) -> u64 {
    ttl_secs.max(0).cast_unsigned()
}

async fn deliver_reliable_event(
    sender: mpsc::Sender<RealtimeEvent>,
    event: RealtimeEvent,
    room_id: RoomId,
    connection_id: ConnectionId,
) -> ReliableDeliveryOutcome {
    let event_type = event.event_type().to_string();
    match tokio::time::timeout(CRITICAL_EVENT_SEND_TIMEOUT, sender.send(event)).await {
        Ok(Ok(())) => ReliableDeliveryOutcome::Delivered,
        Ok(Err(e)) => {
            warn!(
                room_id = %room_id,
                connection_id = %connection_id,
                event_type = %event_type,
                "Failed to deliver reliable event (channel closed): {e}"
            );
            ReliableDeliveryOutcome::Closed
        }
        Err(_) => {
            warn!(
                room_id = %room_id,
                connection_id = %connection_id,
                event_type = %event_type,
                timeout_secs = CRITICAL_EVENT_SEND_TIMEOUT.as_secs(),
                "Reliable event delivery timed out"
            );
            ReliableDeliveryOutcome::TimedOut
        }
    }
}

enum ReliableDeliveryOutcome {
    Delivered,
    Closed,
    TimedOut,
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

/// Subscriber information
#[derive(Debug)]
pub struct Subscriber {
    pub connection_id: ConnectionId,
    pub user_id: UserId,
    pub sender: mpsc::Sender<RealtimeEvent>,
    /// Consecutive message drops due to a full channel
    consecutive_drops: Arc<AtomicU32>,
}

impl Clone for Subscriber {
    fn clone(&self) -> Self {
        Self {
            connection_id: self.connection_id.clone(),
            user_id: self.user_id,
            sender: self.sender.clone(),
            consecutive_drops: self.consecutive_drops.clone(),
        }
    }
}

enum TargetedDelivery {
    Delivered,
    Dropped,
    Closed(ConnectionId),
    Retry {
        sender: mpsc::Sender<RealtimeEvent>,
        event: Box<RealtimeEvent>,
        room_id: RoomId,
        connection_id: ConnectionId,
    },
}

/// In-memory hub for routing messages to connected clients in rooms
/// This handles local message distribution (single node)
///
/// With Redis configured, subscription state is persisted for cross-replica visibility
/// and recovery after restarts. Local DashMaps serve as a fast cache.
#[derive(Clone)]
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
    redis_conn: Option<Arc<dyn RedisConnectionRuntime>>,

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

    /// Guards idempotent startup of Redis-backed background tasks.
    background_tasks_started: Arc<AtomicBool>,
    ttl_refresh_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl std::fmt::Debug for RoomMessageHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomMessageHub")
            .field("rooms", &self.rooms.len())
            .field("connections", &self.connections.len())
            .field("redis_enabled", &self.redis_conn.is_some())
            .field("redis_key_prefix", &self.redis_key_prefix)
            .field("redis_key_ttl_secs", &self.redis_key_ttl_secs)
            .finish()
    }
}

impl RoomMessageHub {
    fn room_key(&self, room_id: &RoomId) -> String {
        format!("{}room_hub:room:{room_id}", self.redis_key_prefix)
    }

    fn room_key_prefix(&self) -> String {
        format!("{}room_hub:room:", self.redis_key_prefix)
    }

    fn conn_key(&self, connection_id: &str) -> String {
        format!("{}room_hub:conn:{}", self.redis_key_prefix, connection_id)
    }

    fn room_index_directory_key(&self) -> String {
        format!(
            "{}{}",
            self.redis_key_prefix, ROOM_INDEX_DIRECTORY_KEY_SUFFIX
        )
    }

    fn remaining_shutdown_budget(deadline: tokio::time::Instant) -> Duration {
        deadline.saturating_duration_since(tokio::time::Instant::now())
    }

    async fn await_shutdown_handle(
        task_name: &'static str,
        timeout: Duration,
        mut handle: JoinHandle<()>,
    ) {
        if timeout.is_zero() {
            warn!(
                task = task_name,
                "RoomMessageHub shutdown budget exhausted before task stopped; aborting immediately"
            );
            handle.abort();
            match handle.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    warn!(
                        task = task_name,
                        error = %error,
                        "RoomMessageHub background task returned join error after immediate abort"
                    );
                }
            }
            return;
        }

        match tokio::time::timeout(timeout, &mut handle).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) if error.is_cancelled() => {}
            Ok(Err(error)) => {
                warn!(
                    task = task_name,
                    error = %error,
                    "RoomMessageHub background task ended with join error during shutdown"
                );
            }
            Err(_) => {
                warn!(
                    task = task_name,
                    timeout_secs = timeout.as_secs(),
                    "RoomMessageHub background task did not stop before shutdown timeout; aborting"
                );
                handle.abort();
                match handle.await {
                    Ok(()) => {}
                    Err(error) if error.is_cancelled() => {}
                    Err(error) => {
                        warn!(
                            task = task_name,
                            error = %error,
                            "RoomMessageHub background task returned join error after timeout abort"
                        );
                    }
                }
            }
        }
    }

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
            redis_key_ttl_secs: 180, // 3 minutes default
            ttl_refresh_cancel: Arc::new(tokio_util::sync::CancellationToken::new()),
            background_tasks_started: Arc::new(AtomicBool::new(false)),
            ttl_refresh_handle: Arc::new(Mutex::new(None)),
        }
    }

    /// Build a room message hub from an optional shared runtime.
    ///
    /// When no shared runtime is provided, the hub stays local-only.
    #[must_use]
    pub(crate) fn from_redis_runtime(
        redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
        key_prefix: &str,
    ) -> Self {
        if let Some(redis_runtime) = redis_runtime {
            Self::new_with_redis_runtime(redis_runtime, key_prefix)
        } else {
            Self::new()
        }
    }

    /// Enable distributed subscription state via Redis.
    ///
    /// When Redis is configured, subscription relationships are persisted to Redis
    /// for cross-replica visibility and recovery after restarts. Local DashMaps
    /// remain as a fast cache for message routing.
    ///
    /// Automatically spawns a TTL refresh task at 40% of
    /// `redis_key_ttl_secs` to prevent active subscription keys from expiring.
    /// The task is cancelled when `shutdown()` is called.
    #[must_use]
    pub(crate) fn new_with_redis_runtime(
        conn: Arc<dyn RedisConnectionRuntime>,
        key_prefix: &str,
    ) -> Self {
        let mut hub = Self::new();
        hub.redis_conn = Some(conn);
        hub.redis_key_prefix = key_prefix.to_string();
        hub.start();
        hub
    }

    async fn redis_conn_clone(
        &self,
        operation: &str,
    ) -> Result<Option<redis::aio::ConnectionManager>, String> {
        let Some(conn) = &self.redis_conn else {
            return Ok(None);
        };
        match tokio::time::timeout(conn.operation_timeout(), conn.snapshot()).await {
            Ok(Ok(snapshot)) => Ok(Some(snapshot)),
            Ok(Err(error)) => Err(format!(
                "Redis {operation} connection snapshot failed: {error}"
            )),
            Err(_) => Err(format!(
                "Redis {operation} connection snapshot timed out after {}ms",
                conn.operation_timeout().as_millis()
            )),
        }
    }

    fn redis_operation_timeout(&self) -> Duration {
        self.redis_conn.as_ref().map_or(
            synctv_core::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            |conn| conn.operation_timeout(),
        )
    }

    async fn redis_op<T, F>(&self, operation: &str, future: F) -> Result<T, String>
    where
        F: Future<Output = redis::RedisResult<T>>,
    {
        run_room_hub_redis_op(self.redis_operation_timeout(), operation, future).await
    }

    fn log_redis_rollback_failure(operation: &'static str, error: &str) {
        warn!(
            operation,
            error, "Redis subscription rollback failed after subscribe error"
        );
    }

    async fn rollback_redis_room_subscription(
        &self,
        conn: &mut redis::aio::ConnectionManager,
        room_key: &str,
        connection_id: &str,
        operation: &'static str,
    ) {
        if let Err(error) = self
            .redis_op(operation, conn.hdel::<_, _, ()>(room_key, connection_id))
            .await
        {
            Self::log_redis_rollback_failure(operation, &error);
        }
    }

    async fn rollback_redis_room_index_membership(
        &self,
        conn: &mut redis::aio::ConnectionManager,
        room_index_directory_key: &str,
        room_key: &str,
    ) {
        const OPERATION: &str = "rollback room index directory membership";
        if let Err(error) = self
            .redis_op(
                OPERATION,
                conn.srem::<_, _, ()>(room_index_directory_key, room_key),
            )
            .await
        {
            Self::log_redis_rollback_failure(OPERATION, &error);
        }
    }

    async fn persist_redis_subscription(
        &self,
        room_id: &RoomId,
        user_id: UserId,
        connection_id: &ConnectionId,
    ) -> Result<(), String> {
        let Some(mut conn_clone) = self.redis_conn_clone("persist room subscription").await? else {
            return Ok(());
        };

        let room_key = self.room_key(room_id);
        let conn_key = self.conn_key(connection_id.as_str());
        let room_index_directory_key = self.room_index_directory_key();
        let ttl_secs = self.redis_key_ttl_secs;

        if let Err(e) = self
            .redis_op(
                "persist room subscription",
                conn_clone.hset::<_, _, _, ()>(&room_key, connection_id.as_str(), user_id.get()),
            )
            .await
        {
            return Err(format!("Failed to persist room subscription to Redis: {e}"));
        }

        if let Err(e) = self
            .redis_op(
                "refresh room subscription TTL",
                conn_clone.expire::<_, ()>(&room_key, ttl_secs),
            )
            .await
        {
            self.rollback_redis_room_subscription(
                &mut conn_clone,
                &room_key,
                connection_id.as_str(),
                "rollback room subscription after TTL failure",
            )
            .await;
            return Err(format!(
                "Failed to refresh room subscription TTL in Redis: {e}"
            ));
        }

        if let Err(e) = self
            .redis_op(
                "persist room index directory membership",
                conn_clone.sadd::<_, _, ()>(&room_index_directory_key, &room_key),
            )
            .await
        {
            self.rollback_redis_room_subscription(
                &mut conn_clone,
                &room_key,
                connection_id.as_str(),
                "rollback room subscription after index failure",
            )
            .await;
            return Err(format!(
                "Failed to persist room index directory membership to Redis: {e}"
            ));
        }

        if let Err(e) = self
            .redis_op(
                "refresh room index directory TTL",
                conn_clone.expire::<_, ()>(&room_index_directory_key, ttl_secs),
            )
            .await
        {
            self.rollback_redis_room_subscription(
                &mut conn_clone,
                &room_key,
                connection_id.as_str(),
                "rollback room subscription after index TTL failure",
            )
            .await;
            self.rollback_redis_room_index_membership(
                &mut conn_clone,
                &room_index_directory_key,
                &room_key,
            )
            .await;
            return Err(format!(
                "Failed to refresh room index directory TTL in Redis: {e}"
            ));
        }

        if let Err(e) = self
            .redis_op(
                "persist connection room mapping",
                conn_clone.set_ex::<_, _, ()>(
                    &conn_key,
                    room_id.get(),
                    ttl_secs_unsigned(ttl_secs),
                ),
            )
            .await
        {
            self.rollback_redis_room_subscription(
                &mut conn_clone,
                &room_key,
                connection_id.as_str(),
                "rollback room subscription after connection mapping failure",
            )
            .await;
            return Err(format!(
                "Failed to persist connection mapping to Redis: {e}"
            ));
        }

        Ok(())
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
            warn!(
                "RoomMessageHub::start() called without Tokio runtime; deferring background task startup"
            );
            return;
        }

        let ttl_cancel = (*self.ttl_refresh_cancel).clone();
        // Use 40% of TTL as the refresh interval (at most 120s, at least 30s)
        let refresh_interval_secs = ttl_refresh_interval_secs(self.redis_key_ttl_secs);
        let Some(ttl_handle) =
            self.spawn_ttl_refresh_task(Duration::from_secs(refresh_interval_secs), ttl_cancel)
        else {
            self.background_tasks_started
                .store(false, Ordering::Release);
            warn!(
                "RoomMessageHub TTL refresh task spawn failed; background tasks were not started"
            );
            return;
        };

        *self.ttl_refresh_handle.lock() = Some(ttl_handle);
    }

    /// Cancel the auto-spawned TTL refresh task and wait for it to exit.
    pub async fn shutdown(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        self.ttl_refresh_cancel.cancel();

        let ttl_handle = self.ttl_refresh_handle.lock().take();
        if let Some(handle) = ttl_handle {
            Self::await_shutdown_handle(
                "ttl refresh",
                Self::remaining_shutdown_budget(deadline),
                handle,
            )
            .await;
        }
        self.background_tasks_started
            .store(false, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn background_shutdown_requested(&self) -> bool {
        self.ttl_refresh_cancel.is_cancelled()
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

    fn publish_lifecycle_event(&self, event: &RoomLifecycleEvent) {
        match self.lifecycle_tx.send(event.clone()) {
            Ok(_) => {}
            Err(_) => {
                debug!(
                    event = ?event,
                    "room lifecycle event published with no active subscribers"
                );
            }
        }
    }

    /// Subscribe a client to room events.
    ///
    /// Returns a receiver for messages.
    ///
    /// With Redis configured, persists the subscription relationship for cross-replica
    /// visibility and recovery. Redis-backed subscriptions fail closed when the
    /// shared state write is unavailable so a successful join is visible to peers.
    pub async fn subscribe(
        &self,
        room_id: RoomId,
        user_id: UserId,
        connection_id: ConnectionId,
    ) -> crate::Result<mpsc::Receiver<RealtimeEvent>> {
        self.start();
        let (tx, rx) = mpsc::channel(SUBSCRIBER_CHANNEL_CAPACITY);

        let subscriber = Subscriber {
            connection_id: connection_id.clone(),
            user_id,
            sender: tx,
            consecutive_drops: Arc::new(AtomicU32::new(0)),
        };

        // Atomically check-and-insert using DashMap's entry API.
        // This avoids the TOCTOU race between `contains_key` + `entry().or_default()`
        // where two concurrent subscribes could both see the room as new.
        let is_new_room = match self.rooms.entry(room_id) {
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
            .insert(connection_id.clone(), (room_id, user_id));

        if let Err(e) = self
            .persist_redis_subscription(&room_id, user_id, &connection_id)
            .await
        {
            self.rollback_local_subscription(&room_id, &connection_id);
            return Err(crate::error::Error::Redis(e));
        }

        if is_new_room {
            self.publish_lifecycle_event(&RoomLifecycleEvent::RoomActivated(room_id));
        }

        info!(
            room_id = %room_id,
            user_id = %user_id,
            connection_id = %connection_id,
            "Client subscribed to room"
        );

        Ok(rx)
    }

    fn rollback_local_subscription(&self, room_id: &RoomId, connection_id: &str) {
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
        if let Some((removed_connection_id, (room_id, user_id))) =
            self.connections.remove(connection_id)
        {
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
                debug!(room_id = %room_id, "Room has no more subscribers, removed");
            }

            self.schedule_redis_cleanup(&removed_connection_id, room_id);

            if room_deactivated {
                self.publish_lifecycle_event(&RoomLifecycleEvent::RoomDeactivated(room_id));
            }

            info!(
                room_id = %room_id,
                user_id = %user_id,
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
    /// This synchronous path is non-blocking. Call [`Self::broadcast_reliably`]
    /// for critical events that must wait for subscriber queue space before
    /// destructive follow-up work continues.
    pub fn broadcast(&self, room_id: &RoomId, event: &RealtimeEvent) -> usize {
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
                for subscriber in subscribers.values() {
                    match subscriber.sender.try_send(event.clone()) {
                        Ok(()) => {
                            // Reset consecutive drop counter on successful send
                            subscriber.consecutive_drops.store(0, Ordering::Relaxed);
                            sent_count += 1;
                            debug!(
                                room_id = %room_id,
                                user_id = %subscriber.user_id,
                                connection_id = %subscriber.connection_id,
                                event_type = %event.event_type(),
                                "Event sent to client"
                            );
                        }
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            let drops =
                                subscriber.consecutive_drops.fetch_add(1, Ordering::Relaxed) + 1;
                            if drops >= MAX_CONSECUTIVE_DROPS {
                                warn!(
                                    room_id = %room_id,
                                    user_id = %subscriber.user_id,
                                    connection_id = %subscriber.connection_id,
                                    consecutive_drops = drops,
                                    "Disconnecting persistently slow subscriber after {} consecutive drops",
                                    MAX_CONSECUTIVE_DROPS
                                );
                                failed_connections.push(subscriber.connection_id.clone());
                            } else {
                                warn!(
                                    room_id = %room_id,
                                    user_id = %subscriber.user_id,
                                    connection_id = %subscriber.connection_id,
                                    event_type = %event.event_type(),
                                    consecutive_drops = drops,
                                    "Subscriber channel full, dropping event for slow consumer"
                                );
                            }
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            warn!(
                                room_id = %room_id,
                                user_id = %subscriber.user_id,
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
                room_id = %room_id,
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
    pub async fn broadcast_reliably(&self, room_id: &RoomId, event: RealtimeEvent) -> usize {
        let mut sent_count = 0;
        let mut failed_connections = Vec::new();
        let is_critical = event.is_critical();
        let mut reliable_deliveries = FuturesUnordered::new();

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
                                let sender = subscriber.sender.clone();
                                let event = event.clone();
                                let room_id = *room_id;
                                let connection_id = subscriber.connection_id.clone();
                                reliable_deliveries.push(async move {
                                    let outcome = deliver_reliable_event(
                                        sender,
                                        event,
                                        room_id,
                                        connection_id.clone(),
                                    )
                                    .await;
                                    (connection_id, outcome)
                                });
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

        while let Some((connection_id, delivery)) = reliable_deliveries.next().await {
            match delivery {
                ReliableDeliveryOutcome::Delivered => {
                    sent_count += 1;
                }
                ReliableDeliveryOutcome::Closed | ReliableDeliveryOutcome::TimedOut => {
                    failed_connections.push(connection_id);
                }
            }
        }

        for conn_id in failed_connections {
            self.unsubscribe(&conn_id);
        }

        sent_count
    }

    /// Broadcast an event to a specific connection in a room.
    ///
    /// Used for targeted delivery (e.g., WebRTC signaling to a specific peer).
    /// Returns 1 if sent, 0 if the connection was not found or the channel was full.
    pub async fn broadcast_to_connection(
        &self,
        room_id: &RoomId,
        connection_id: &str,
        event: RealtimeEvent,
    ) -> usize {
        let reliable_target_delivery = requires_reliable_target_delivery(&event);
        let delivery = self
            .rooms
            .get(room_id)
            .and_then(|subscribers| {
                subscribers.get(connection_id).map(|subscriber| {
                    let event_type = event.event_type().to_string();
                    match subscriber.sender.try_send(event.clone()) {
                        Ok(()) => {
                            debug!(
                                room_id = %room_id,
                                connection_id = %connection_id,
                                event_type = %event_type,
                                "Event sent to specific connection"
                            );
                            TargetedDelivery::Delivered
                        }
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            if reliable_target_delivery {
                                TargetedDelivery::Retry {
                                    sender: subscriber.sender.clone(),
                                    event: Box::new(event),
                                    room_id: *room_id,
                                    connection_id: subscriber.connection_id.clone(),
                                }
                            } else {
                                warn!(
                                    room_id = %room_id,
                                    connection_id = %connection_id,
                                    event_type = %event_type,
                                    "Subscriber channel full, dropping targeted event"
                                );
                                TargetedDelivery::Dropped
                            }
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            warn!(
                                room_id = %room_id,
                                connection_id = %connection_id,
                                "Subscriber channel closed for targeted event"
                            );
                            TargetedDelivery::Closed(subscriber.connection_id.clone())
                        }
                    }
                })
            })
            .unwrap_or(TargetedDelivery::Dropped);

        match delivery {
            TargetedDelivery::Delivered => 1,
            TargetedDelivery::Dropped => 0,
            TargetedDelivery::Closed(conn_id) => {
                self.unsubscribe(&conn_id);
                0
            }
            TargetedDelivery::Retry {
                sender,
                event,
                room_id,
                connection_id,
            } => {
                match deliver_reliable_event(sender, *event, room_id, connection_id.clone()).await {
                    ReliableDeliveryOutcome::Delivered => 1,
                    ReliableDeliveryOutcome::Closed | ReliableDeliveryOutcome::TimedOut => {
                        self.unsubscribe(connection_id.as_str());
                        0
                    }
                }
            }
        }
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

    /// Get all active room IDs (rooms with at least one local subscriber).
    ///
    /// This is local runtime state. Playback lifecycle schedulers use it as a
    /// process-scoped input and rely on durable storage for duplicate-work
    /// convergence when several nodes host subscribers for the same room.
    #[must_use]
    pub fn active_room_ids(&self) -> Vec<RoomId> {
        self.rooms.iter().map(|entry| *entry.key()).collect()
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
                    (sub.connection_id.clone(), *room_id)
                })
                .collect();

            for (connection_id, room_id) in &removed_subscribers {
                self.schedule_redis_cleanup(connection_id, *room_id);
            }
            self.publish_lifecycle_event(&RoomLifecycleEvent::RoomDeactivated(*room_id));
            info!(
                room_id = %room_id,
                removed_connections = subscribers.len(),
                "Removed all subscribers for deleted room"
            );
        }
    }

    fn schedule_redis_cleanup(&self, connection_id: &ConnectionId, room_id: RoomId) {
        let Some(redis_conn) = self.redis_conn.clone() else {
            return;
        };

        let room_key = self.room_key(&room_id);
        let conn_key = self.conn_key(connection_id.as_str());
        let room_index_directory_key = self.room_index_directory_key();
        let connection_id_for_log = connection_id.clone();
        let room_id_for_log = room_id;
        let redis_key_ttl_secs = self.redis_key_ttl_secs;
        let cleanup_connection_id = connection_id.clone();

        let cleanup_fut = async move {
            let timeout = redis_conn.operation_timeout();
            let mut conn_clone = match tokio::time::timeout(timeout, redis_conn.snapshot()).await {
                Ok(Ok(conn)) => conn,
                Ok(Err(error)) => {
                    warn!(
                        error = %error,
                        "Failed to acquire Redis connection for scheduled room cleanup"
                    );
                    return;
                }
                Err(_) => {
                    warn!(
                        timeout_ms = timeout.as_millis(),
                        "Timed out acquiring Redis connection for scheduled room cleanup"
                    );
                    return;
                }
            };
            let mut cleanup_failed = false;

            if let Err(e) = run_room_hub_redis_op(
                timeout,
                "remove room subscription",
                conn_clone.hdel::<_, _, ()>(&room_key, cleanup_connection_id.as_str()),
            )
            .await
            {
                cleanup_failed = true;
                warn!("Failed to remove room subscription from Redis: {e}");
            }
            match run_room_hub_redis_op(
                timeout,
                "inspect room subscription hash cardinality",
                conn_clone.hlen::<_, usize>(&room_key),
            )
            .await
            {
                Ok(0) => {
                    if let Err(e) = run_room_hub_redis_op(
                        timeout,
                        "delete empty room subscription hash",
                        conn_clone.del::<_, ()>(&room_key),
                    )
                    .await
                    {
                        cleanup_failed = true;
                        warn!("Failed to delete empty room subscription hash from Redis: {e}");
                    }
                    if let Err(e) = run_room_hub_redis_op(
                        timeout,
                        "remove empty room from room index directory",
                        conn_clone.srem::<_, _, ()>(&room_index_directory_key, &room_key),
                    )
                    .await
                    {
                        cleanup_failed = true;
                        warn!("Failed to remove empty room from room index directory: {e}");
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    cleanup_failed = true;
                    warn!("Failed to inspect room subscription hash cardinality in Redis: {e}");
                }
            }
            if let Err(e) = run_room_hub_redis_op(
                timeout,
                "remove connection mapping",
                conn_clone.del::<_, ()>(&conn_key),
            )
            .await
            {
                cleanup_failed = true;
                warn!("Failed to remove connection mapping from Redis: {e}");
            }

            if cleanup_failed {
                warn!(
                    connection_id = %cleanup_connection_id,
                    room_id = %room_id_for_log,
                    ttl_secs = redis_key_ttl_secs,
                    "Redis room cleanup was incomplete; subscription keys will expire by TTL"
                );
            }
        };

        if try_spawn(cleanup_fut).is_none() {
            warn!(
                connection_id = %connection_id_for_log,
                room_id = %room_id_for_log,
                "No Tokio runtime available for Redis room cleanup; subscription keys will expire by TTL"
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
                    .map(|sub| (sub.user_id, sub.connection_id.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all subscribers in a room across all replicas (from Redis).
    ///
    /// Returns the full subscriber list from Redis, which includes subscriptions
    /// from all replicas in the cluster. Local-only hubs return their local
    /// subscriber list. Redis-backed hubs return an error if the distributed
    /// snapshot cannot be loaded or validated.
    pub async fn get_room_subscribers_distributed(
        &self,
        room_id: &RoomId,
    ) -> crate::Result<Vec<(UserId, ConnectionId)>> {
        if self.redis_conn.is_some() {
            let room_key = self.room_key(room_id);
            let room_index_directory_key = self.room_index_directory_key();
            let mut conn_clone = match self
                .redis_conn_clone("load distributed room subscribers")
                .await
            {
                Ok(Some(conn)) => conn,
                Ok(None) => return Ok(self.get_room_subscribers(room_id)),
                Err(error) => return Err(crate::Error::Redis(error)),
            };

            match self
                .redis_op(
                    "load distributed room subscribers",
                    conn_clone.hgetall::<_, Vec<(String, i64)>>(&room_key),
                )
                .await
            {
                Ok(entries) => {
                    if entries.is_empty() {
                        log_best_effort_redis_cleanup(
                            "prune empty room from room index directory",
                            self.redis_op(
                                "prune empty room from room index directory",
                                conn_clone.srem(&room_index_directory_key, &room_key),
                            )
                            .await,
                        );
                        return Ok(Vec::new());
                    }

                    let conn_keys: Vec<String> = entries
                        .iter()
                        .map(|(conn_id, _)| self.conn_key(conn_id))
                        .collect();
                    let conn_rooms = match self
                        .redis_op(
                            "load connection room mappings",
                            conn_clone.mget::<_, Vec<Option<i64>>>(conn_keys),
                        )
                        .await
                    {
                        Ok(conn_rooms) => conn_rooms,
                        Err(error) => return Err(crate::Error::Redis(error)),
                    };

                    let mut subscribers = Vec::with_capacity(entries.len());
                    let mut stale_connection_ids = Vec::new();

                    for ((conn_id, user_id), mapped_room_id) in entries.into_iter().zip(conn_rooms)
                    {
                        if mapped_room_id == Some(room_id.get()) {
                            if let Ok(user_id) = UserId::try_from(user_id) {
                                subscribers.push((user_id, ConnectionId::new(conn_id)));
                            } else {
                                stale_connection_ids.push(conn_id);
                            }
                        } else {
                            stale_connection_ids.push(conn_id);
                        }
                    }

                    if !stale_connection_ids.is_empty() {
                        let mut pipe = redis::pipe();
                        for conn_id in &stale_connection_ids {
                            pipe.hdel(&room_key, conn_id).ignore();
                        }
                        pipe.cmd("HLEN").arg(&room_key);

                        match self
                            .redis_op(
                                "prune stale distributed room subscribers",
                                pipe.query_async::<Vec<i64>>(&mut conn_clone),
                            )
                            .await
                        {
                            Ok(results) => {
                                let room_members_after_prune = match room_members_after_prune(
                                    &results,
                                ) {
                                    Ok(count) => count,
                                    Err(error) => {
                                        warn!(
                                            room_id = %room_id,
                                            removed_members = stale_connection_ids.len(),
                                            "Failed to validate stale distributed room subscriber prune result: {error}"
                                        );
                                        return Ok(subscribers);
                                    }
                                };
                                if room_members_after_prune == 0 {
                                    log_best_effort_redis_cleanup(
                                        "delete empty room subscription hash",
                                        self.redis_op(
                                            "delete empty room subscription hash",
                                            conn_clone.del(&room_key),
                                        )
                                        .await,
                                    );
                                    log_best_effort_redis_cleanup(
                                        "remove empty room from room index directory",
                                        self.redis_op(
                                            "remove empty room from room index directory",
                                            conn_clone.srem(&room_index_directory_key, &room_key),
                                        )
                                        .await,
                                    );
                                }
                                debug!(
                                    room_id = %room_id,
                                    removed_members = stale_connection_ids.len(),
                                    "Pruned stale distributed room subscribers on read"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    room_id = %room_id,
                                    removed_members = stale_connection_ids.len(),
                                    "Failed to prune stale distributed room subscribers on read: {e}"
                                );
                            }
                        }
                    }

                    return Ok(subscribers);
                }
                Err(error) => return Err(crate::Error::Redis(error)),
            }
        }

        Ok(self.get_room_subscribers(room_id))
    }

    /// Audit replica-wide subscription state from Redis (observability only).
    ///
    /// Scans Redis for persisted subscription relationships and logs room/subscriber
    /// counts. This is an **observability tool**, not a recovery mechanism.
    ///
    /// This method does **not** populate the local `rooms` or `connections` DashMaps
    /// because:
    ///
    /// 1. **`MessageSender` cannot be recovered.** Each subscriber's `mpsc::Sender`
    ///    is only meaningful to the original WebSocket connection. Without a live
    ///    sender, messages cannot be routed, so inserting a synthetic `Subscriber` would
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
        info!(
            "Auditing cluster subscription state from Redis (observability only, clients must reconnect for message routing)"
        );
        let Some(mut conn_clone) = self.redis_conn_clone("audit room subscriptions").await? else {
            return Err("Redis not configured".to_string());
        };

        let room_index_directory_key = self.room_index_directory_key();
        let mut recovered = 0;
        let keys: Vec<String> = self
            .redis_op(
                "load room subscription index directory",
                conn_clone.smembers(&room_index_directory_key),
            )
            .await?;
        let mut stale_directory_members = Vec::new();
        let room_key_prefix = self.room_key_prefix();

        for key in keys {
            // Extract room_id from key
            let room_id_str = key.trim_start_matches(&room_key_prefix);
            let Ok(room_id) = room_id_str.parse::<RoomId>() else {
                tracing::warn!(room_id = %room_id_str, "Ignoring invalid room hub room key");
                continue;
            };

            // Fetch all subscribers for this room
            let entries: Vec<(String, i64)> = match self
                .redis_op("load audited room subscribers", conn_clone.hgetall(&key))
                .await
            {
                Ok(entries) => entries,
                Err(e) => {
                    warn!(
                        room_id = %room_id,
                        error = %e,
                        "Failed to fetch audited room subscribers from Redis"
                    );
                    continue;
                }
            };
            if entries.is_empty() {
                stale_directory_members.push(key.clone());
                log_best_effort_redis_cleanup(
                    "delete empty audited room key",
                    self.redis_op("delete empty audited room key", conn_clone.del(&key))
                        .await,
                );
                continue;
            }

            recovered += entries.len();

            info!(
                room_id = %room_id,
                subscriber_count = entries.len(),
                "Audited room subscription state from Redis (observability only)"
            );
        }

        if !stale_directory_members.is_empty() {
            log_best_effort_redis_cleanup(
                "remove stale room subscription directory members",
                self.redis_op(
                    "remove stale room subscription directory members",
                    conn_clone.srem(&room_index_directory_key, stale_directory_members),
                )
                .await,
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
        let mut conn = match self
            .redis_conn_clone("refresh room subscription TTLs")
            .await
        {
            Ok(Some(conn)) => conn,
            Ok(None) => return,
            Err(error) => {
                warn!(
                    error = %error,
                    "Failed to acquire Redis connection for room subscription TTL refresh"
                );
                return;
            }
        };
        let ttl_secs = self.redis_key_ttl_secs;

        let mut keys_to_refresh = Vec::new();

        // Collect room keys for all active rooms
        for entry in self.rooms.iter() {
            let room_key = self.room_key(entry.key());
            keys_to_refresh.push(room_key);
        }

        if !self.rooms.is_empty() {
            keys_to_refresh.push(self.room_index_directory_key());
        }

        // Collect connection keys for all active connections
        for entry in self.connections.iter() {
            let conn_key = self.conn_key(entry.key());
            keys_to_refresh.push(conn_key);
        }

        if keys_to_refresh.is_empty() {
            return;
        }

        let mut pipe = redis::pipe();
        for key in &keys_to_refresh {
            pipe.expire(key, ttl_secs).ignore();
        }

        if let Err(e) = self
            .redis_op(
                "refresh room_hub Redis key TTLs via pipeline",
                pipe.query_async::<()>(&mut conn),
            )
            .await
        {
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
    ) -> Option<tokio::task::JoinHandle<()>> {
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
    }
}

impl Default for RoomMessageHub {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RoomMessageRuntime for RoomMessageHub {
    fn subscribe_lifecycle(&self) -> broadcast::Receiver<RoomLifecycleEvent> {
        RoomMessageHub::subscribe_lifecycle(self)
    }

    async fn subscribe(
        &self,
        room_id: RoomId,
        user_id: UserId,
        connection_id: ConnectionId,
    ) -> crate::error::Result<mpsc::Receiver<RealtimeEvent>> {
        RoomMessageHub::subscribe(self, room_id, user_id, connection_id).await
    }

    fn unsubscribe(&self, connection_id: &str) {
        RoomMessageHub::unsubscribe(self, connection_id);
    }

    fn broadcast(&self, room_id: &RoomId, event: &RealtimeEvent) -> usize {
        RoomMessageHub::broadcast(self, room_id, event)
    }

    async fn broadcast_reliably(&self, room_id: &RoomId, event: RealtimeEvent) -> usize {
        RoomMessageHub::broadcast_reliably(self, room_id, event).await
    }

    async fn broadcast_to_connection(
        &self,
        room_id: &RoomId,
        connection_id: &str,
        event: RealtimeEvent,
    ) -> usize {
        RoomMessageHub::broadcast_to_connection(self, room_id, connection_id, event).await
    }

    fn room_count(&self) -> usize {
        RoomMessageHub::room_count(self)
    }

    fn active_room_ids(&self) -> Vec<RoomId> {
        RoomMessageHub::active_room_ids(self)
    }

    fn connection_count(&self) -> usize {
        RoomMessageHub::connection_count(self)
    }

    fn remove_room(&self, room_id: &RoomId) {
        RoomMessageHub::remove_room(self, room_id);
    }

    fn get_room_subscribers(&self, room_id: &RoomId) -> Vec<(UserId, ConnectionId)> {
        RoomMessageHub::get_room_subscribers(self, room_id)
    }

    async fn get_room_subscribers_replicas_wide(
        &self,
        room_id: &RoomId,
    ) -> crate::Result<Vec<(UserId, ConnectionId)>> {
        RoomMessageHub::get_room_subscribers_distributed(self, room_id).await
    }

    async fn audit_shared_subscriptions(&self) -> std::result::Result<usize, String> {
        RoomMessageHub::audit_redis_subscriptions(self).await
    }

    async fn shutdown(&self) {
        RoomMessageHub::shutdown(self).await;
    }

    #[cfg(test)]
    fn background_shutdown_requested(&self) -> bool {
        RoomMessageHub::background_shutdown_requested(self)
    }
}

#[cfg(test)]
mod tests;
