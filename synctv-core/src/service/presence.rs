use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use futures::{stream, StreamExt, TryStreamExt};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::models::{RealtimeActor, RoomId, UserId};
use crate::{
    redis_runtime_snapshot, Error, RedisConnectionRuntime, Result, SharedStateMode,
    SharedStateProfile,
};

const PRESENCE_METADATA_TTL_SECONDS: i64 = 120;
const PRESENCE_RENEWAL_INTERVAL_MS: i64 = 30_000;
const PRESENCE_CHANNEL_CAPACITY: usize = 1024;
const PRESENCE_L1_CACHE_TTL_MS: i64 = 3_000;
const PRESENCE_BATCH_CONCURRENCY: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceConnection {
    pub connection_id: String,
    pub node_id: String,
    pub actor: RealtimeActor,
    pub room_id: Option<RoomId>,
    pub connected_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub last_renewed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnlineRoomStats {
    pub room_id: RoomId,
    pub online_member_count: usize,
    pub online_guest_count: usize,
    pub connection_count: usize,
    pub node_connection_counts: BTreeMap<String, usize>,
    pub sampled_at_ms: i64,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnlineUserStats {
    pub user_id: UserId,
    pub connection_count: usize,
    pub node_connection_counts: BTreeMap<String, usize>,
    pub room_count: usize,
    pub rooms: Vec<RoomId>,
    pub sampled_at_ms: i64,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnlineNodeStats {
    pub node_id: String,
    pub connection_count: usize,
    pub online_member_count: usize,
    pub online_guest_count: usize,
    pub room_count: usize,
    pub sampled_at_ms: i64,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceOverview {
    pub online_member_count: usize,
    pub online_guest_count: usize,
    pub connection_count: usize,
    pub active_room_count: usize,
    pub nodes: Vec<OnlineNodeStats>,
    pub sampled_at_ms: i64,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnlineUserRoomStats {
    pub room_id: RoomId,
    pub user_id: UserId,
    pub is_online: bool,
    pub connection_count: usize,
    pub last_seen_at_ms: Option<i64>,
    pub sampled_at_ms: i64,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresenceEvent {
    RoomStatsChanged(OnlineRoomStats),
    UserStatsChanged(OnlineUserStats),
    UserRoomStatsChanged(OnlineUserRoomStats),
    ConnectionOpened(PresenceConnection),
    ConnectionClosed(PresenceConnection),
    ConnectionMovedRoom {
        connection: PresenceConnection,
        previous_room_id: Option<RoomId>,
    },
}

#[derive(Debug, Default)]
struct PresenceState {
    connections: HashMap<String, PresenceConnection>,
    actor_connections: HashMap<String, HashSet<String>>,
    user_connections: HashMap<UserId, HashSet<String>>,
    room_connections: HashMap<RoomId, HashSet<String>>,
    user_room_connections: HashMap<(UserId, RoomId), HashSet<String>>,
    node_connections: HashMap<String, HashSet<String>>,
    pending_renewals: HashSet<String>,
}

fn record_actor_kind(
    actor: &RealtimeActor,
    member_actors: &mut HashSet<String>,
    guest_actors: &mut HashSet<String>,
) {
    let actor_key = actor.connection_key();
    if actor.is_guest() {
        guest_actors.insert(actor_key);
    } else {
        member_actors.insert(actor_key);
    }
}

#[derive(Debug, Clone)]
struct PresenceCacheEntry<T> {
    value: T,
    expires_at_ms: i64,
}

impl<T: Clone> PresenceCacheEntry<T> {
    fn fresh_value(&self, now_ms: i64) -> Option<T> {
        (self.expires_at_ms > now_ms).then(|| self.value.clone())
    }
}

#[derive(Debug, Default)]
struct PresenceL1Cache {
    room_stats: HashMap<RoomId, PresenceCacheEntry<OnlineRoomStats>>,
    hot_room_stats: Option<PresenceCacheEntry<Vec<OnlineRoomStats>>>,
    room_online_user_ids: HashMap<(RoomId, Vec<UserId>), PresenceCacheEntry<Vec<UserId>>>,
    user_stats: HashMap<UserId, PresenceCacheEntry<OnlineUserStats>>,
    node_stats: HashMap<String, PresenceCacheEntry<OnlineNodeStats>>,
    all_node_stats: Option<PresenceCacheEntry<Vec<OnlineNodeStats>>>,
    overview: Option<PresenceCacheEntry<PresenceOverview>>,
    user_room_stats: HashMap<(UserId, RoomId), PresenceCacheEntry<OnlineUserRoomStats>>,
}

#[derive(Clone)]
pub struct OnlinePresenceService {
    state: Arc<parking_lot::Mutex<PresenceState>>,
    cache: Arc<parking_lot::Mutex<PresenceL1Cache>>,
    redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    shared_required: bool,
    key_prefix: String,
    event_tx: broadcast::Sender<PresenceEvent>,
    version: Arc<AtomicU64>,
    renewal_flush_scheduled: Arc<AtomicBool>,
}

impl OnlinePresenceService {
    #[must_use]
    pub fn local() -> Self {
        Self::new(None, false, "")
    }

    pub fn from_shared_state_profile(profile: &SharedStateProfile) -> Result<Self> {
        match profile.state_mode() {
            SharedStateMode::SharedRequired => {
                let runtime = profile.require_shared_runtime("presence state")?;
                Ok(Self::new(Some(runtime), true, profile.key_prefix()))
            }
            SharedStateMode::SharedBestEffort => Ok(Self::new(
                profile.shared_runtime(),
                false,
                profile.key_prefix(),
            )),
            SharedStateMode::LocalOnly => Ok(Self::new(None, false, profile.key_prefix())),
        }
    }

    fn new(
        redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
        shared_required: bool,
        key_prefix: impl Into<String>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(PRESENCE_CHANNEL_CAPACITY);
        Self {
            state: Arc::new(parking_lot::Mutex::new(PresenceState::default())),
            cache: Arc::new(parking_lot::Mutex::new(PresenceL1Cache::default())),
            redis_runtime,
            shared_required,
            key_prefix: key_prefix.into(),
            event_tx,
            version: Arc::new(AtomicU64::new(1)),
            renewal_flush_scheduled: Arc::new(AtomicBool::new(false)),
        }
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<PresenceEvent> {
        self.event_tx.subscribe()
    }

    pub async fn register_connection(
        &self,
        connection_id: String,
        node_id: String,
        actor: RealtimeActor,
    ) -> Result<()> {
        let now = now_ms();
        let connection = PresenceConnection {
            connection_id: connection_id.clone(),
            node_id: node_id.clone(),
            actor: actor.clone(),
            room_id: None,
            connected_at_ms: now,
            last_seen_at_ms: now,
            last_renewed_at_ms: now,
        };

        {
            let mut state = self.state.lock();
            state
                .connections
                .insert(connection_id.clone(), connection.clone());
            state
                .actor_connections
                .entry(actor.connection_key())
                .or_default()
                .insert(connection_id.clone());
            if let Some(user_id) = actor.user_id() {
                state
                    .user_connections
                    .entry(user_id)
                    .or_default()
                    .insert(connection_id.clone());
            }
            state
                .node_connections
                .entry(node_id)
                .or_default()
                .insert(connection.connection_id.clone());
        }

        self.persist_connection(&connection).await?;
        self.publish(PresenceEvent::ConnectionOpened(connection));
        if let Some(user_id) = actor.user_id() {
            self.publish_user_stats(user_id).await;
        }
        Ok(())
    }

    pub async fn join_room(&self, connection_id: &str, room_id: RoomId) -> Result<()> {
        let (connection, previous_room_id, affected_users, affected_rooms, room_changed) = {
            let mut state = self.state.lock();
            let Some(existing) = state.connections.get(connection_id) else {
                return Err(Error::NotFound("presence connection not found".to_string()));
            };
            let user_id = existing.actor.user_id();
            let previous_room_id = existing.room_id;
            if previous_room_id == Some(room_id) {
                let Some(connection) = state.connections.get_mut(connection_id) else {
                    return Err(Error::NotFound("presence connection not found".to_string()));
                };
                connection.last_seen_at_ms = now_ms();
                connection.last_renewed_at_ms = connection.last_seen_at_ms;
                let connection = connection.clone();
                (connection, previous_room_id, Vec::new(), Vec::new(), false)
            } else {
                if let Some(previous_room_id) = previous_room_id {
                    if let Some(connections) = state.room_connections.get_mut(&previous_room_id) {
                        connections.remove(connection_id);
                        if connections.is_empty() {
                            state.room_connections.remove(&previous_room_id);
                        }
                    }
                    if let Some(user_id) = user_id {
                        if let Some(connections) = state
                            .user_room_connections
                            .get_mut(&(user_id, previous_room_id))
                        {
                            connections.remove(connection_id);
                            if connections.is_empty() {
                                state
                                    .user_room_connections
                                    .remove(&(user_id, previous_room_id));
                            }
                        }
                    }
                }

                let Some(connection) = state.connections.get_mut(connection_id) else {
                    return Err(Error::NotFound("presence connection not found".to_string()));
                };
                connection.room_id = Some(room_id);
                connection.last_seen_at_ms = now_ms();
                connection.last_renewed_at_ms = connection.last_seen_at_ms;
                let connection = connection.clone();
                state
                    .room_connections
                    .entry(room_id)
                    .or_default()
                    .insert(connection_id.to_string());
                if let Some(user_id) = user_id {
                    state
                        .user_room_connections
                        .entry((user_id, room_id))
                        .or_default()
                        .insert(connection_id.to_string());
                }

                let affected_rooms = previous_room_id
                    .into_iter()
                    .chain([room_id])
                    .collect::<Vec<_>>();
                (
                    connection.clone(),
                    previous_room_id,
                    user_id.into_iter().collect(),
                    affected_rooms,
                    true,
                )
            }
        };

        if room_changed {
            self.persist_room_move(&connection, previous_room_id)
                .await?;
        } else {
            self.persist_connection(&connection).await?;
            return Ok(());
        }
        self.publish(PresenceEvent::ConnectionMovedRoom {
            connection,
            previous_room_id,
        });
        for room_id in affected_rooms {
            self.publish_room_stats(room_id).await;
        }
        for user_id in affected_users {
            self.publish_user_stats(user_id).await;
        }
        Ok(())
    }

    pub async fn unregister_connection(&self, connection_id: &str) -> Result<()> {
        let removed = {
            let mut state = self.state.lock();
            let Some(connection) = state.connections.remove(connection_id) else {
                return Ok(());
            };

            let actor_key = connection.actor.connection_key();
            if let Some(connections) = state.actor_connections.get_mut(&actor_key) {
                connections.remove(connection_id);
                if connections.is_empty() {
                    state.actor_connections.remove(&actor_key);
                }
            }
            if let Some(user_id) = connection.actor.user_id() {
                if let Some(connections) = state.user_connections.get_mut(&user_id) {
                    connections.remove(connection_id);
                    if connections.is_empty() {
                        state.user_connections.remove(&user_id);
                    }
                }
            }
            if let Some(connections) = state.node_connections.get_mut(&connection.node_id) {
                connections.remove(connection_id);
                if connections.is_empty() {
                    state.node_connections.remove(&connection.node_id);
                }
            }
            if let Some(room_id) = connection.room_id {
                if let Some(connections) = state.room_connections.get_mut(&room_id) {
                    connections.remove(connection_id);
                    if connections.is_empty() {
                        state.room_connections.remove(&room_id);
                    }
                }
                if let Some(user_id) = connection.actor.user_id() {
                    if let Some(connections) =
                        state.user_room_connections.get_mut(&(user_id, room_id))
                    {
                        connections.remove(connection_id);
                        if connections.is_empty() {
                            state.user_room_connections.remove(&(user_id, room_id));
                        }
                    }
                }
            }
            connection
        };

        self.delete_connection(&removed).await?;
        self.publish(PresenceEvent::ConnectionClosed(removed.clone()));
        if let Some(room_id) = removed.room_id {
            self.publish_room_stats(room_id).await;
        }
        if let Some(user_id) = removed.actor.user_id() {
            self.publish_user_stats(user_id).await;
        }
        Ok(())
    }

    pub async fn touch(&self, connection_id: &str) -> Result<()> {
        let connection = {
            let mut state = self.state.lock();
            let Some(connection) = state.connections.get_mut(connection_id) else {
                return Ok(());
            };
            connection.last_seen_at_ms = now_ms();
            connection.last_renewed_at_ms = connection.last_seen_at_ms;
            connection.clone()
        };
        self.persist_connection(&connection).await
    }

    #[must_use]
    pub fn mark_seen_for_renewal(&self, connection_id: &str) -> bool {
        let now = now_ms();
        {
            let mut state = self.state.lock();
            let Some(connection) = state.connections.get_mut(connection_id) else {
                return false;
            };
            connection.last_seen_at_ms = now;
            if now.saturating_sub(connection.last_renewed_at_ms) < PRESENCE_RENEWAL_INTERVAL_MS {
                return false;
            }
            connection.last_renewed_at_ms = now;
            if self.redis_runtime.is_none() {
                return false;
            }
            state.pending_renewals.insert(connection_id.to_string());
        }
        !self.renewal_flush_scheduled.swap(true, Ordering::AcqRel)
    }

    pub async fn flush_pending_renewals(&self) -> Result<()> {
        loop {
            let connections = {
                let mut state = self.state.lock();
                if state.pending_renewals.is_empty() {
                    self.renewal_flush_scheduled.store(false, Ordering::Release);
                    return Ok(());
                }
                let connection_ids = state.pending_renewals.drain().collect::<Vec<_>>();
                connection_ids
                    .into_iter()
                    .filter_map(|connection_id| state.connections.get(&connection_id).cloned())
                    .collect::<Vec<_>>()
            };

            if connections.is_empty() {
                continue;
            }
            self.persist_connections(&connections).await?;
        }
    }

    pub async fn room_stats(&self, room_id: RoomId) -> Result<OnlineRoomStats> {
        let now = now_ms();
        if let Some(stats) = self
            .cache
            .lock()
            .room_stats
            .get(&room_id)
            .and_then(|entry| entry.fresh_value(now))
        {
            return Ok(stats);
        }
        if let Some(stats) = self.redis_room_stats(room_id).await? {
            self.cache
                .lock()
                .room_stats
                .insert(room_id, cache_entry(stats.clone(), now));
            return Ok(stats);
        }
        let stats = self.local_room_stats(room_id);
        self.cache
            .lock()
            .room_stats
            .insert(room_id, cache_entry(stats.clone(), now));
        Ok(stats)
    }

    pub async fn room_stats_batch(&self, room_ids: &[RoomId]) -> Result<Vec<OnlineRoomStats>> {
        stream::iter(room_ids.iter().copied())
            .map(|room_id| self.room_stats(room_id))
            .buffered(PRESENCE_BATCH_CONCURRENCY)
            .try_collect()
            .await
    }

    pub async fn hot_room_stats(&self) -> Result<Vec<OnlineRoomStats>> {
        let now = now_ms();
        if let Some(stats) = self
            .cache
            .lock()
            .hot_room_stats
            .as_ref()
            .and_then(|entry| entry.fresh_value(now))
        {
            return Ok(stats);
        }
        if let Some(stats) = self.redis_hot_room_stats().await? {
            self.cache.lock().hot_room_stats = Some(cache_entry(stats.clone(), now));
            return Ok(stats);
        }

        let room_ids = {
            let state = self.state.lock();
            state.room_connections.keys().copied().collect::<Vec<_>>()
        };
        let mut stats = Vec::with_capacity(room_ids.len());
        for room_id in room_ids {
            let stat = self.local_room_stats(room_id);
            if stat.online_member_count > 0 || stat.online_guest_count > 0 {
                stats.push(stat);
            }
        }
        stats.sort_by_key(|stat| {
            (
                std::cmp::Reverse(stat.online_member_count + stat.online_guest_count),
                stat.room_id,
            )
        });
        self.cache.lock().hot_room_stats = Some(cache_entry(stats.clone(), now));
        Ok(stats)
    }

    pub async fn room_online_user_ids(
        &self,
        room_id: RoomId,
        user_ids: &[UserId],
    ) -> Result<Vec<UserId>> {
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }
        let now = now_ms();
        let cache_key = room_online_users_cache_key(room_id, user_ids);
        if let Some(ids) = self
            .cache
            .lock()
            .room_online_user_ids
            .get(&cache_key)
            .and_then(|entry| entry.fresh_value(now))
        {
            return Ok(filter_requested_user_order(user_ids, &ids));
        }
        if let Some(ids) = self.redis_room_online_user_ids(room_id, user_ids).await? {
            self.cache
                .lock()
                .room_online_user_ids
                .insert(cache_key, cache_entry(ids.clone(), now));
            return Ok(ids);
        }
        let state = self.state.lock();
        let ids = user_ids
            .iter()
            .copied()
            .filter(|user_id| {
                state
                    .user_room_connections
                    .get(&(*user_id, room_id))
                    .is_some_and(|connections| !connections.is_empty())
            })
            .collect::<Vec<_>>();
        drop(state);
        self.cache
            .lock()
            .room_online_user_ids
            .insert(cache_key, cache_entry(ids.clone(), now));
        Ok(ids)
    }

    pub async fn user_stats(&self, user_id: UserId) -> Result<OnlineUserStats> {
        let now = now_ms();
        if let Some(stats) = self
            .cache
            .lock()
            .user_stats
            .get(&user_id)
            .and_then(|entry| entry.fresh_value(now))
        {
            return Ok(stats);
        }
        if let Some(stats) = self.redis_user_stats(user_id).await? {
            self.cache
                .lock()
                .user_stats
                .insert(user_id, cache_entry(stats.clone(), now));
            return Ok(stats);
        }
        let stats = self.local_user_stats(user_id);
        self.cache
            .lock()
            .user_stats
            .insert(user_id, cache_entry(stats.clone(), now));
        Ok(stats)
    }

    pub async fn node_stats(&self, node_id: &str) -> Result<OnlineNodeStats> {
        let now = now_ms();
        if let Some(stats) = self
            .cache
            .lock()
            .node_stats
            .get(node_id)
            .and_then(|entry| entry.fresh_value(now))
        {
            return Ok(stats);
        }
        if let Some(stats) = self.redis_node_stats(node_id).await? {
            self.cache
                .lock()
                .node_stats
                .insert(node_id.to_string(), cache_entry(stats.clone(), now));
            return Ok(stats);
        }
        let stats = self.local_node_stats(node_id);
        self.cache
            .lock()
            .node_stats
            .insert(node_id.to_string(), cache_entry(stats.clone(), now));
        Ok(stats)
    }

    pub async fn all_node_stats(&self) -> Result<Vec<OnlineNodeStats>> {
        let now = now_ms();
        if let Some(stats) = self
            .cache
            .lock()
            .all_node_stats
            .as_ref()
            .and_then(|entry| entry.fresh_value(now))
        {
            return Ok(stats);
        }
        if let Some(stats) = self.redis_all_node_stats().await? {
            self.cache.lock().all_node_stats = Some(cache_entry(stats.clone(), now));
            return Ok(stats);
        }
        let node_ids = {
            let state = self.state.lock();
            state.node_connections.keys().cloned().collect::<Vec<_>>()
        };
        let stats = node_ids
            .into_iter()
            .map(|node_id| self.local_node_stats(&node_id))
            .collect::<Vec<_>>();
        self.cache.lock().all_node_stats = Some(cache_entry(stats.clone(), now));
        Ok(stats)
    }

    pub async fn overview(&self) -> Result<PresenceOverview> {
        let now = now_ms();
        if let Some(stats) = self
            .cache
            .lock()
            .overview
            .as_ref()
            .and_then(|entry| entry.fresh_value(now))
        {
            return Ok(stats);
        }
        let nodes = self.all_node_stats().await?;
        let connection_count = nodes.iter().map(|node| node.connection_count).sum();
        let active_room_count = self.hot_room_stats().await?.len();
        let (online_member_count, online_guest_count) = if self.redis_runtime.is_some() {
            self.redis_all_online_actor_counts()
                .await?
                .unwrap_or_else(|| self.local_actor_counts())
        } else {
            self.local_actor_counts()
        };
        let stats = PresenceOverview {
            online_member_count,
            online_guest_count,
            connection_count,
            active_room_count,
            nodes,
            sampled_at_ms: now_ms(),
            version: self.next_version(),
        };
        self.cache.lock().overview = Some(cache_entry(stats.clone(), now));
        Ok(stats)
    }

    pub async fn user_room_stats(
        &self,
        user_id: UserId,
        room_id: RoomId,
    ) -> Result<OnlineUserRoomStats> {
        let now = now_ms();
        let cache_key = (user_id, room_id);
        if let Some(stats) = self
            .cache
            .lock()
            .user_room_stats
            .get(&cache_key)
            .and_then(|entry| entry.fresh_value(now))
        {
            return Ok(stats);
        }
        if let Some(stats) = self.redis_user_room_stats(user_id, room_id).await? {
            self.cache
                .lock()
                .user_room_stats
                .insert(cache_key, cache_entry(stats.clone(), now));
            return Ok(stats);
        }
        let stats = self.local_user_room_stats(user_id, room_id);
        self.cache
            .lock()
            .user_room_stats
            .insert(cache_key, cache_entry(stats.clone(), now));
        Ok(stats)
    }

    pub async fn user_room_stats_batch(
        &self,
        user_ids: &[UserId],
        room_id: RoomId,
    ) -> Result<Vec<OnlineUserRoomStats>> {
        stream::iter(user_ids.iter().copied())
            .map(|user_id| self.user_room_stats(user_id, room_id))
            .buffered(PRESENCE_BATCH_CONCURRENCY)
            .try_collect()
            .await
    }

    pub async fn room_stats_fresh(&self, room_id: RoomId) -> Result<OnlineRoomStats> {
        if let Some(stats) = self.redis_room_stats(room_id).await? {
            return Ok(stats);
        }
        Ok(self.local_room_stats(room_id))
    }

    pub async fn user_room_stats_fresh(
        &self,
        user_id: UserId,
        room_id: RoomId,
    ) -> Result<OnlineUserRoomStats> {
        if let Some(stats) = self.redis_user_room_stats(user_id, room_id).await? {
            return Ok(stats);
        }
        Ok(self.local_user_room_stats(user_id, room_id))
    }

    pub async fn user_has_other_connection_in_room(
        &self,
        user_id: UserId,
        room_id: RoomId,
        excluding_connection_id: &str,
    ) -> Result<bool> {
        if let Some(has_other) = self
            .redis_user_has_other_connection_in_room(user_id, room_id, excluding_connection_id)
            .await?
        {
            return Ok(has_other);
        }
        let state = self.state.lock();
        Ok(state
            .user_room_connections
            .get(&(user_id, room_id))
            .is_some_and(|connections| {
                connections
                    .iter()
                    .any(|connection_id| connection_id != excluding_connection_id)
            }))
    }

    pub async fn actor_has_other_connection_in_room(
        &self,
        actor: &RealtimeActor,
        room_id: RoomId,
        excluding_connection_id: &str,
    ) -> Result<bool> {
        if let Some(has_other) = self
            .redis_actor_has_other_connection_in_room(actor, room_id, excluding_connection_id)
            .await?
        {
            return Ok(has_other);
        }
        let state = self.state.lock();
        Ok(state
            .actor_connections
            .get(&actor.connection_key())
            .is_some_and(|connections| {
                connections.iter().any(|connection_id| {
                    connection_id != excluding_connection_id
                        && state
                            .connections
                            .get(connection_id)
                            .is_some_and(|connection| connection.room_id == Some(room_id))
                })
            }))
    }

    pub async fn actor_connection_count_in_room(
        &self,
        actor: &RealtimeActor,
        room_id: RoomId,
    ) -> Result<usize> {
        if let Some(count) = self
            .redis_actor_connection_count_in_room(actor, room_id)
            .await?
        {
            return Ok(count);
        }
        let state = self.state.lock();
        Ok(state
            .actor_connections
            .get(&actor.connection_key())
            .into_iter()
            .flatten()
            .filter(|connection_id| {
                state
                    .connections
                    .get(*connection_id)
                    .is_some_and(|connection| connection.room_id == Some(room_id))
            })
            .count())
    }

    fn publish(&self, event: PresenceEvent) {
        if let Err(error) = self.event_tx.send(event) {
            debug!(%error, "presence event had no subscribers");
        }
    }

    async fn publish_room_stats(&self, room_id: RoomId) {
        match self.room_stats_fresh(room_id).await {
            Ok(stats) => self.publish(PresenceEvent::RoomStatsChanged(stats)),
            Err(error) => warn!(%error, %room_id, "failed to publish room presence stats"),
        }
    }

    async fn publish_user_stats(&self, user_id: UserId) {
        match self.fresh_user_stats(user_id).await {
            Ok(stats) => self.publish(PresenceEvent::UserStatsChanged(stats)),
            Err(error) => warn!(%error, %user_id, "failed to publish user presence stats"),
        }
    }

    async fn fresh_user_stats(&self, user_id: UserId) -> Result<OnlineUserStats> {
        if let Some(stats) = self.redis_user_stats(user_id).await? {
            return Ok(stats);
        }
        Ok(self.local_user_stats(user_id))
    }

    fn local_actor_counts(&self) -> (usize, usize) {
        let state = self.state.lock();
        let mut member_actors = HashSet::new();
        let mut guest_actors = HashSet::new();
        for connection in state.connections.values() {
            record_actor_kind(&connection.actor, &mut member_actors, &mut guest_actors);
        }
        (member_actors.len(), guest_actors.len())
    }

    fn local_room_stats(&self, room_id: RoomId) -> OnlineRoomStats {
        let state = self.state.lock();
        let connection_ids = state
            .room_connections
            .get(&room_id)
            .cloned()
            .unwrap_or_default();
        let mut member_actors = HashSet::new();
        let mut guest_actors = HashSet::new();
        let mut node_connection_counts = BTreeMap::new();
        for connection_id in &connection_ids {
            if let Some(connection) = state.connections.get(connection_id) {
                record_actor_kind(&connection.actor, &mut member_actors, &mut guest_actors);
                *node_connection_counts
                    .entry(connection.node_id.clone())
                    .or_default() += 1;
            }
        }
        OnlineRoomStats {
            room_id,
            online_member_count: member_actors.len(),
            online_guest_count: guest_actors.len(),
            connection_count: connection_ids.len(),
            node_connection_counts,
            sampled_at_ms: now_ms(),
            version: self.next_version(),
        }
    }

    fn local_user_stats(&self, user_id: UserId) -> OnlineUserStats {
        let state = self.state.lock();
        let connection_ids = state
            .user_connections
            .get(&user_id)
            .cloned()
            .unwrap_or_default();
        let mut rooms = HashSet::new();
        let mut node_connection_counts = BTreeMap::new();
        for connection_id in &connection_ids {
            if let Some(connection) = state.connections.get(connection_id) {
                if let Some(room_id) = connection.room_id {
                    rooms.insert(room_id);
                }
                *node_connection_counts
                    .entry(connection.node_id.clone())
                    .or_default() += 1;
            }
        }
        let mut rooms = rooms.into_iter().collect::<Vec<_>>();
        rooms.sort();
        OnlineUserStats {
            user_id,
            connection_count: connection_ids.len(),
            node_connection_counts,
            room_count: rooms.len(),
            rooms,
            sampled_at_ms: now_ms(),
            version: self.next_version(),
        }
    }

    fn local_node_stats(&self, node_id: &str) -> OnlineNodeStats {
        let state = self.state.lock();
        let connection_ids = state
            .node_connections
            .get(node_id)
            .cloned()
            .unwrap_or_default();
        let mut member_actors = HashSet::new();
        let mut guest_actors = HashSet::new();
        let mut rooms = HashSet::new();
        for connection_id in &connection_ids {
            if let Some(connection) = state.connections.get(connection_id) {
                record_actor_kind(&connection.actor, &mut member_actors, &mut guest_actors);
                if let Some(room_id) = connection.room_id {
                    rooms.insert(room_id);
                }
            }
        }
        OnlineNodeStats {
            node_id: node_id.to_string(),
            connection_count: connection_ids.len(),
            online_member_count: member_actors.len(),
            online_guest_count: guest_actors.len(),
            room_count: rooms.len(),
            sampled_at_ms: now_ms(),
            version: self.next_version(),
        }
    }

    fn local_user_room_stats(&self, user_id: UserId, room_id: RoomId) -> OnlineUserRoomStats {
        let state = self.state.lock();
        let connection_ids = state
            .user_room_connections
            .get(&(user_id, room_id))
            .cloned()
            .unwrap_or_default();
        let last_seen_at_ms = connection_ids
            .iter()
            .filter_map(|connection_id| state.connections.get(connection_id))
            .map(|connection| connection.last_seen_at_ms)
            .max();
        OnlineUserRoomStats {
            room_id,
            user_id,
            is_online: !connection_ids.is_empty(),
            connection_count: connection_ids.len(),
            last_seen_at_ms,
            sampled_at_ms: now_ms(),
            version: self.next_version(),
        }
    }

    fn next_version(&self) -> u64 {
        self.version.fetch_add(1, Ordering::Relaxed)
    }

    async fn redis_connection(
        &self,
        operation: &str,
    ) -> Result<Option<redis::aio::ConnectionManager>> {
        let Some(runtime) = self.redis_runtime.as_ref() else {
            return Ok(None);
        };
        redis_runtime_snapshot(runtime.as_ref(), operation)
            .await
            .map(Some)
            .or_else(|error| {
                if self.shared_required {
                    Err(error)
                } else {
                    warn!(%error, operation, "presence redis operation degraded to local state");
                    Ok(None)
                }
            })
    }

    fn conn_key(&self, connection_id: &str) -> String {
        format!("{}presence:conn:{connection_id}", self.key_prefix)
    }

    fn user_key(&self, user_id: UserId) -> String {
        format!("{}presence:user:{user_id}:connections", self.key_prefix)
    }

    fn room_key(&self, room_id: RoomId) -> String {
        format!("{}presence:room:{room_id}:connections", self.key_prefix)
    }

    fn user_room_key(&self, user_id: UserId, room_id: RoomId) -> String {
        format!(
            "{}presence:user_room:{user_id}:{room_id}:connections",
            self.key_prefix
        )
    }

    fn node_key(&self, node_id: &str) -> String {
        format!("{}presence:node:{node_id}:connections", self.key_prefix)
    }

    fn rooms_key(&self) -> String {
        format!("{}presence:rooms", self.key_prefix)
    }

    fn nodes_key(&self) -> String {
        format!("{}presence:nodes", self.key_prefix)
    }

    async fn prune_index_members(
        &self,
        redis: &mut redis::aio::ConnectionManager,
        index_key: impl Into<String>,
        connection_ids: &[String],
    ) {
        if connection_ids.is_empty() {
            return;
        }
        let result: redis::RedisResult<()> = redis::cmd("SREM")
            .arg(index_key.into())
            .arg(connection_ids)
            .query_async(redis)
            .await;
        if let Err(error) = result {
            warn!(%error, "failed to prune stale presence index members");
        }
    }

    async fn load_connections_for_index(
        &self,
        redis: &mut redis::aio::ConnectionManager,
        index_key: String,
    ) -> Result<Vec<PresenceConnection>> {
        let connection_ids: Vec<String> = redis
            .smembers(&index_key)
            .await
            .map_err(|error| Error::Internal(format!("read presence index: {error}")))?;
        if connection_ids.is_empty() {
            return Ok(Vec::new());
        }
        let metadata_keys = connection_ids
            .iter()
            .map(|connection_id| self.conn_key(connection_id))
            .collect::<Vec<_>>();
        let metadata: Vec<Option<String>> = redis
            .mget(metadata_keys)
            .await
            .map_err(|error| Error::Internal(format!("read presence metadata: {error}")))?;
        let mut stale_connection_ids = Vec::new();
        let mut connections = Vec::new();
        for (connection_id, payload) in connection_ids.into_iter().zip(metadata) {
            let Some(payload) = payload else {
                stale_connection_ids.push(connection_id);
                continue;
            };
            match serde_json::from_str::<PresenceConnection>(&payload) {
                Ok(connection) if connection.connection_id == connection_id => {
                    connections.push(connection);
                }
                Ok(_) => stale_connection_ids.push(connection_id),
                Err(error) => {
                    warn!(%error, connection_id, "invalid presence metadata");
                    stale_connection_ids.push(connection_id);
                }
            }
        }
        self.prune_index_members(redis, index_key, &stale_connection_ids)
            .await;
        Ok(connections)
    }

    async fn load_scoped_connections_for_index(
        &self,
        redis: &mut redis::aio::ConnectionManager,
        index_key: String,
        matches_scope: impl Fn(&PresenceConnection) -> bool,
    ) -> Result<Vec<PresenceConnection>> {
        let connections = self
            .load_connections_for_index(redis, index_key.clone())
            .await?;
        let mut stale_connection_ids = Vec::new();
        let scoped_connections = connections
            .into_iter()
            .filter_map(|connection| {
                if matches_scope(&connection) {
                    Some(connection)
                } else {
                    stale_connection_ids.push(connection.connection_id);
                    None
                }
            })
            .collect::<Vec<_>>();
        self.prune_index_members(redis, index_key, &stale_connection_ids)
            .await;
        Ok(scoped_connections)
    }

    async fn persist_connection(&self, connection: &PresenceConnection) -> Result<()> {
        self.persist_connections(std::slice::from_ref(connection))
            .await
    }

    async fn persist_connections(&self, connections: &[PresenceConnection]) -> Result<()> {
        if connections.is_empty() {
            return Ok(());
        }
        let Some(mut redis) = self.redis_connection("persist presence connection").await? else {
            return Ok(());
        };

        let nodes_key = self.nodes_key();
        let rooms_key = self.rooms_key();
        let mut pipe = redis::pipe();
        for connection in connections {
            let payload = serde_json::to_string(connection).map_err(|error| {
                Error::Internal(format!("serialize presence connection: {error}"))
            })?;
            let conn_key = self.conn_key(&connection.connection_id);
            let node_key = self.node_key(&connection.node_id);
            pipe.cmd("SET")
                .arg(&conn_key)
                .arg(payload)
                .arg("EX")
                .arg(PRESENCE_METADATA_TTL_SECONDS)
                .ignore()
                .cmd("SADD")
                .arg(&node_key)
                .arg(&connection.connection_id)
                .ignore()
                .cmd("EXPIRE")
                .arg(&node_key)
                .arg(PRESENCE_METADATA_TTL_SECONDS)
                .ignore()
                .cmd("SADD")
                .arg(&nodes_key)
                .arg(&connection.node_id)
                .ignore()
                .cmd("EXPIRE")
                .arg(&nodes_key)
                .arg(PRESENCE_METADATA_TTL_SECONDS)
                .ignore();
            if let Some(user_id) = connection.actor.user_id() {
                let user_key = self.user_key(user_id);
                pipe.cmd("SADD")
                    .arg(&user_key)
                    .arg(&connection.connection_id)
                    .ignore()
                    .cmd("EXPIRE")
                    .arg(&user_key)
                    .arg(PRESENCE_METADATA_TTL_SECONDS)
                    .ignore();
            }
            if let Some(room_id) = connection.room_id {
                let room_key = self.room_key(room_id);
                pipe.cmd("SADD")
                    .arg(&room_key)
                    .arg(&connection.connection_id)
                    .ignore()
                    .cmd("EXPIRE")
                    .arg(&room_key)
                    .arg(PRESENCE_METADATA_TTL_SECONDS)
                    .ignore()
                    .cmd("SADD")
                    .arg(&rooms_key)
                    .arg(room_id.to_string())
                    .ignore()
                    .cmd("EXPIRE")
                    .arg(&rooms_key)
                    .arg(PRESENCE_METADATA_TTL_SECONDS)
                    .ignore();
                if let Some(user_id) = connection.actor.user_id() {
                    let user_room_key = self.user_room_key(user_id, room_id);
                    pipe.cmd("SADD")
                        .arg(&user_room_key)
                        .arg(&connection.connection_id)
                        .ignore()
                        .cmd("EXPIRE")
                        .arg(&user_room_key)
                        .arg(PRESENCE_METADATA_TTL_SECONDS)
                        .ignore();
                }
            }
        }
        pipe.query_async::<()>(&mut redis)
            .await
            .map_err(|error| Error::Internal(format!("persist presence connections: {error}")))?;
        Ok(())
    }

    async fn persist_room_move(
        &self,
        connection: &PresenceConnection,
        previous_room_id: Option<RoomId>,
    ) -> Result<()> {
        self.persist_connection(connection).await?;
        let Some(room_id) = connection.room_id else {
            return Ok(());
        };
        let Some(mut redis) = self.redis_connection("persist presence room move").await? else {
            return Ok(());
        };
        let room_key = self.room_key(room_id);
        let rooms_key = self.rooms_key();
        let mut pipe = redis::pipe();
        pipe.cmd("SADD")
            .arg(&room_key)
            .arg(&connection.connection_id)
            .ignore()
            .cmd("EXPIRE")
            .arg(&room_key)
            .arg(PRESENCE_METADATA_TTL_SECONDS)
            .ignore()
            .cmd("SADD")
            .arg(&rooms_key)
            .arg(room_id.to_string())
            .ignore()
            .cmd("EXPIRE")
            .arg(&rooms_key)
            .arg(PRESENCE_METADATA_TTL_SECONDS)
            .ignore();
        if let Some(user_id) = connection.actor.user_id() {
            let user_room_key = self.user_room_key(user_id, room_id);
            pipe.cmd("SADD")
                .arg(&user_room_key)
                .arg(&connection.connection_id)
                .ignore()
                .cmd("EXPIRE")
                .arg(&user_room_key)
                .arg(PRESENCE_METADATA_TTL_SECONDS)
                .ignore();
        }
        if let Some(previous_room_id) = previous_room_id {
            pipe.cmd("SREM")
                .arg(self.room_key(previous_room_id))
                .arg(&connection.connection_id)
                .ignore();
            if let Some(user_id) = connection.actor.user_id() {
                pipe.cmd("SREM")
                    .arg(self.user_room_key(user_id, previous_room_id))
                    .arg(&connection.connection_id)
                    .ignore();
            }
        }
        pipe.query_async::<()>(&mut redis)
            .await
            .map_err(|error| Error::Internal(format!("persist presence room move: {error}")))?;
        Ok(())
    }

    async fn delete_connection(&self, connection: &PresenceConnection) -> Result<()> {
        let Some(mut redis) = self.redis_connection("delete presence connection").await? else {
            return Ok(());
        };
        let mut pipe = redis::pipe();
        pipe.cmd("DEL")
            .arg(self.conn_key(&connection.connection_id))
            .ignore();
        if let Some(user_id) = connection.actor.user_id() {
            pipe.cmd("SREM")
                .arg(self.user_key(user_id))
                .arg(&connection.connection_id)
                .ignore();
        }
        pipe.cmd("SREM")
            .arg(self.node_key(&connection.node_id))
            .arg(&connection.connection_id)
            .ignore();
        if let Some(room_id) = connection.room_id {
            pipe.cmd("SREM")
                .arg(self.room_key(room_id))
                .arg(&connection.connection_id)
                .ignore();
            if let Some(user_id) = connection.actor.user_id() {
                pipe.cmd("SREM")
                    .arg(self.user_room_key(user_id, room_id))
                    .arg(&connection.connection_id)
                    .ignore();
            }
        }
        pipe.query_async::<()>(&mut redis)
            .await
            .map_err(|error| Error::Internal(format!("delete presence connection: {error}")))?;
        Ok(())
    }

    async fn redis_room_stats(&self, room_id: RoomId) -> Result<Option<OnlineRoomStats>> {
        let Some(mut redis) = self.redis_connection("read room presence stats").await? else {
            return Ok(None);
        };
        let connections = self
            .load_scoped_connections_for_index(&mut redis, self.room_key(room_id), |connection| {
                connection.room_id == Some(room_id)
            })
            .await?;
        let mut member_actors = HashSet::new();
        let mut guest_actors = HashSet::new();
        let mut node_connection_counts = BTreeMap::new();
        for connection in &connections {
            record_actor_kind(&connection.actor, &mut member_actors, &mut guest_actors);
            *node_connection_counts
                .entry(connection.node_id.clone())
                .or_default() += 1;
        }
        Ok(Some(OnlineRoomStats {
            room_id,
            online_member_count: member_actors.len(),
            online_guest_count: guest_actors.len(),
            connection_count: connections.len(),
            node_connection_counts,
            sampled_at_ms: now_ms(),
            version: self.next_version(),
        }))
    }

    async fn redis_room_online_user_ids(
        &self,
        room_id: RoomId,
        user_ids: &[UserId],
    ) -> Result<Option<Vec<UserId>>> {
        let Some(mut redis) = self.redis_connection("read room member presence").await? else {
            return Ok(None);
        };
        let mut online = Vec::new();
        for user_id in user_ids {
            let connections = self
                .load_scoped_connections_for_index(
                    &mut redis,
                    self.user_room_key(*user_id, room_id),
                    |connection| {
                        connection.actor.user_id() == Some(*user_id)
                            && connection.room_id == Some(room_id)
                    },
                )
                .await?;
            if !connections.is_empty() {
                online.push(*user_id);
            }
        }
        Ok(Some(online))
    }

    async fn redis_hot_room_stats(&self) -> Result<Option<Vec<OnlineRoomStats>>> {
        let Some(mut redis) = self.redis_connection("read hot room presence").await? else {
            return Ok(None);
        };
        let room_ids: Vec<String> = redis
            .smembers(self.rooms_key())
            .await
            .map_err(|error| Error::Internal(format!("read hot room presence index: {error}")))?;
        drop(redis);
        let mut stats = stream::iter(
            room_ids
                .into_iter()
                .filter_map(|room_id| room_id.parse::<RoomId>().ok()),
        )
        .map(|room_id| self.redis_room_stats(room_id))
        .buffered(PRESENCE_BATCH_CONCURRENCY)
        .try_filter_map(|stat| async move {
            Ok(stat.filter(|stat| stat.online_member_count > 0 || stat.online_guest_count > 0))
        })
        .try_collect::<Vec<_>>()
        .await?;
        stats.sort_by_key(|stat| {
            (
                std::cmp::Reverse(stat.online_member_count + stat.online_guest_count),
                stat.room_id,
            )
        });
        Ok(Some(stats))
    }

    async fn redis_user_stats(&self, user_id: UserId) -> Result<Option<OnlineUserStats>> {
        let Some(mut redis) = self.redis_connection("read user presence stats").await? else {
            return Ok(None);
        };
        let connections = self
            .load_scoped_connections_for_index(&mut redis, self.user_key(user_id), |connection| {
                connection.actor.user_id() == Some(user_id)
            })
            .await?;
        let mut rooms = HashSet::new();
        let mut node_connection_counts = BTreeMap::new();
        for connection in &connections {
            if let Some(room_id) = connection.room_id {
                rooms.insert(room_id);
            }
            *node_connection_counts
                .entry(connection.node_id.clone())
                .or_default() += 1;
        }
        let mut rooms = rooms.into_iter().collect::<Vec<_>>();
        rooms.sort();
        Ok(Some(OnlineUserStats {
            user_id,
            connection_count: connections.len(),
            node_connection_counts,
            room_count: rooms.len(),
            rooms,
            sampled_at_ms: now_ms(),
            version: self.next_version(),
        }))
    }

    async fn redis_node_stats(&self, node_id: &str) -> Result<Option<OnlineNodeStats>> {
        let Some(mut redis) = self.redis_connection("read node presence stats").await? else {
            return Ok(None);
        };
        let connections = self
            .load_scoped_connections_for_index(&mut redis, self.node_key(node_id), |connection| {
                connection.node_id == node_id
            })
            .await?;
        let mut member_actors = HashSet::new();
        let mut guest_actors = HashSet::new();
        let mut rooms = HashSet::new();
        for connection in &connections {
            record_actor_kind(&connection.actor, &mut member_actors, &mut guest_actors);
            if let Some(room_id) = connection.room_id {
                rooms.insert(room_id);
            }
        }
        Ok(Some(OnlineNodeStats {
            node_id: node_id.to_string(),
            connection_count: connections.len(),
            online_member_count: member_actors.len(),
            online_guest_count: guest_actors.len(),
            room_count: rooms.len(),
            sampled_at_ms: now_ms(),
            version: self.next_version(),
        }))
    }

    async fn redis_all_node_stats(&self) -> Result<Option<Vec<OnlineNodeStats>>> {
        let Some(mut redis) = self
            .redis_connection("read all node presence stats")
            .await?
        else {
            return Ok(None);
        };
        let node_ids: Vec<String> = redis
            .smembers(self.nodes_key())
            .await
            .map_err(|error| Error::Internal(format!("read presence node directory: {error}")))?;
        let mut stats = Vec::new();
        for node_id in node_ids {
            let Some(stat) = self.redis_node_stats(&node_id).await? else {
                continue;
            };
            if stat.connection_count > 0 {
                stats.push(stat);
            }
        }
        stats.sort_by_key(|stat| stat.node_id.clone());
        Ok(Some(stats))
    }

    async fn redis_all_online_actor_counts(&self) -> Result<Option<(usize, usize)>> {
        let Some(mut redis) = self
            .redis_connection("read global user presence stats")
            .await?
        else {
            return Ok(None);
        };
        let node_ids: Vec<String> = redis
            .smembers(self.nodes_key())
            .await
            .map_err(|error| Error::Internal(format!("read presence node directory: {error}")))?;
        let mut member_actors = HashSet::new();
        let mut guest_actors = HashSet::new();
        for node_id in node_ids {
            let connections = self
                .load_scoped_connections_for_index(
                    &mut redis,
                    self.node_key(&node_id),
                    |connection| connection.node_id == node_id,
                )
                .await?;
            for connection in connections {
                record_actor_kind(&connection.actor, &mut member_actors, &mut guest_actors);
            }
        }
        Ok(Some((member_actors.len(), guest_actors.len())))
    }

    async fn redis_user_room_stats(
        &self,
        user_id: UserId,
        room_id: RoomId,
    ) -> Result<Option<OnlineUserRoomStats>> {
        let Some(mut redis) = self
            .redis_connection("read user room presence stats")
            .await?
        else {
            return Ok(None);
        };
        let connections = self
            .load_scoped_connections_for_index(
                &mut redis,
                self.user_room_key(user_id, room_id),
                |connection| {
                    connection.actor.user_id() == Some(user_id)
                        && connection.room_id == Some(room_id)
                },
            )
            .await?;
        let last_seen_at_ms = connections
            .iter()
            .map(|connection| connection.last_seen_at_ms)
            .max();
        Ok(Some(OnlineUserRoomStats {
            room_id,
            user_id,
            is_online: !connections.is_empty(),
            connection_count: connections.len(),
            last_seen_at_ms,
            sampled_at_ms: now_ms(),
            version: self.next_version(),
        }))
    }

    async fn redis_user_has_other_connection_in_room(
        &self,
        user_id: UserId,
        room_id: RoomId,
        excluding_connection_id: &str,
    ) -> Result<Option<bool>> {
        let Some(mut redis) = self.redis_connection("read user room presence").await? else {
            return Ok(None);
        };
        let connections = self
            .load_scoped_connections_for_index(
                &mut redis,
                self.user_room_key(user_id, room_id),
                |connection| {
                    connection.actor.user_id() == Some(user_id)
                        && connection.room_id == Some(room_id)
                },
            )
            .await?;
        Ok(Some(connections.iter().any(|connection| {
            connection.connection_id != excluding_connection_id
        })))
    }

    async fn redis_actor_has_other_connection_in_room(
        &self,
        actor: &RealtimeActor,
        room_id: RoomId,
        excluding_connection_id: &str,
    ) -> Result<Option<bool>> {
        let Some(mut redis) = self.redis_connection("read actor room presence").await? else {
            return Ok(None);
        };
        let connections = self
            .load_scoped_connections_for_index(&mut redis, self.room_key(room_id), |connection| {
                &connection.actor == actor && connection.room_id == Some(room_id)
            })
            .await?;
        Ok(Some(connections.iter().any(|connection| {
            connection.connection_id != excluding_connection_id
        })))
    }

    async fn redis_actor_connection_count_in_room(
        &self,
        actor: &RealtimeActor,
        room_id: RoomId,
    ) -> Result<Option<usize>> {
        let Some(mut redis) = self.redis_connection("read actor room presence").await? else {
            return Ok(None);
        };
        let connections = self
            .load_scoped_connections_for_index(&mut redis, self.room_key(room_id), |connection| {
                &connection.actor == actor && connection.room_id == Some(room_id)
            })
            .await?;
        Ok(Some(connections.len()))
    }
}

fn now_ms() -> i64 {
    crate::SystemClock.now_millis()
}

fn cache_entry<T>(value: T, now_ms: i64) -> PresenceCacheEntry<T> {
    PresenceCacheEntry {
        value,
        expires_at_ms: now_ms.saturating_add(PRESENCE_L1_CACHE_TTL_MS),
    }
}

fn room_online_users_cache_key(room_id: RoomId, user_ids: &[UserId]) -> (RoomId, Vec<UserId>) {
    let mut user_ids = user_ids.to_vec();
    user_ids.sort();
    user_ids.dedup();
    (room_id, user_ids)
}

fn filter_requested_user_order(requested: &[UserId], online: &[UserId]) -> Vec<UserId> {
    let online = online.iter().copied().collect::<HashSet<_>>();
    requested
        .iter()
        .copied()
        .filter(|user_id| online.contains(user_id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use synctv_core_testing::{redis_connection_manager, start_redis_with_client};

    fn room_id(value: i64) -> RoomId {
        RoomId::try_from(value).expect("positive room id")
    }

    fn user_id(value: i64) -> UserId {
        UserId::try_from(value).expect("positive user id")
    }

    async fn redis_presence_service(
        client: &redis::Client,
        key_prefix: &str,
    ) -> anyhow::Result<OnlinePresenceService> {
        let redis = redis_connection_manager(client).await;
        let profile = SharedStateProfile::for_cluster_runtime(
            Some(crate::direct_runtime(redis)),
            key_prefix,
            true,
        );
        Ok(OnlinePresenceService::from_shared_state_profile(&profile)?)
    }

    #[derive(Clone)]
    struct UnavailableRedisRuntime;

    #[async_trait::async_trait]
    impl RedisConnectionRuntime for UnavailableRedisRuntime {
        async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
            Err(redis::RedisError::from((
                redis::ErrorKind::Io,
                "presence redis unavailable",
            )))
        }

        fn operation_timeout(&self) -> Duration {
            Duration::from_millis(10)
        }
    }

    #[tokio::test]
    async fn room_stats_deduplicate_user_connections() -> anyhow::Result<()> {
        let presence = OnlinePresenceService::local();
        presence
            .register_connection(
                "conn-a".to_string(),
                "node-a".to_string(),
                RealtimeActor::user(user_id(1), "1"),
            )
            .await?;
        presence
            .register_connection(
                "conn-b".to_string(),
                "node-b".to_string(),
                RealtimeActor::user(user_id(1), "1"),
            )
            .await?;
        presence.join_room("conn-a", room_id(10)).await?;
        presence.join_room("conn-b", room_id(10)).await?;

        let stats = presence.room_stats(room_id(10)).await?;
        assert_eq!(stats.connection_count, 2);
        assert_eq!(stats.online_member_count, 1);
        assert_eq!(stats.online_guest_count, 0);
        assert_eq!(stats.node_connection_counts.get("node-a"), Some(&1));
        assert_eq!(stats.node_connection_counts.get("node-b"), Some(&1));
        Ok(())
    }

    #[tokio::test]
    async fn room_stats_use_short_l1_cache_until_expired() -> anyhow::Result<()> {
        let presence = OnlinePresenceService::local();
        presence
            .register_connection(
                "conn-a".to_string(),
                "node-a".to_string(),
                RealtimeActor::user(user_id(1), "1"),
            )
            .await?;
        presence.join_room("conn-a", room_id(10)).await?;

        let cached = presence.room_stats(room_id(10)).await?;
        assert_eq!(cached.connection_count, 1);

        presence
            .register_connection(
                "conn-b".to_string(),
                "node-a".to_string(),
                RealtimeActor::user(user_id(2), "2"),
            )
            .await?;
        presence.join_room("conn-b", room_id(10)).await?;

        let still_cached = presence.room_stats(room_id(10)).await?;
        assert_eq!(still_cached.connection_count, 1);

        let fresh = presence.room_stats_fresh(room_id(10)).await?;
        assert_eq!(fresh.connection_count, 2);
        assert_eq!(fresh.online_member_count, 2);
        assert_eq!(fresh.online_guest_count, 0);

        presence
            .cache
            .lock()
            .room_stats
            .get_mut(&room_id(10))
            .expect("room stats should be cached")
            .expires_at_ms = 0;

        let refreshed = presence.room_stats(room_id(10)).await?;
        assert_eq!(refreshed.connection_count, 2);
        assert_eq!(refreshed.online_member_count, 2);
        assert_eq!(refreshed.online_guest_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn room_online_user_ids_filter_requested_users() -> anyhow::Result<()> {
        let presence = OnlinePresenceService::local();
        presence
            .register_connection(
                "conn-a".to_string(),
                "node-a".to_string(),
                RealtimeActor::user(user_id(1), "1"),
            )
            .await?;
        presence.join_room("conn-a", room_id(10)).await?;

        let online = presence
            .room_online_user_ids(room_id(10), &[user_id(1), user_id(2)])
            .await?;
        assert_eq!(online, vec![user_id(1)]);
        Ok(())
    }

    #[tokio::test]
    async fn node_and_overview_stats_separate_members_and_guests() -> anyhow::Result<()> {
        let presence = OnlinePresenceService::local();
        presence
            .register_connection(
                "conn-a".to_string(),
                "node-a".to_string(),
                RealtimeActor::user(user_id(1), "1"),
            )
            .await?;
        presence
            .register_connection(
                "conn-b".to_string(),
                "node-a".to_string(),
                RealtimeActor::user(user_id(2), "2"),
            )
            .await?;
        presence
            .register_connection(
                "conn-c".to_string(),
                "node-b".to_string(),
                RealtimeActor::user(user_id(2), "2"),
            )
            .await?;
        presence
            .register_connection(
                "conn-guest-a".to_string(),
                "node-a".to_string(),
                RealtimeActor::guest("gst_local"),
            )
            .await?;
        presence
            .register_connection(
                "conn-guest-b".to_string(),
                "node-a".to_string(),
                RealtimeActor::guest("gst_local"),
            )
            .await?;
        presence.join_room("conn-a", room_id(10)).await?;
        presence.join_room("conn-b", room_id(11)).await?;
        presence.join_room("conn-c", room_id(11)).await?;
        presence.join_room("conn-guest-a", room_id(10)).await?;
        presence.join_room("conn-guest-b", room_id(10)).await?;

        let node_a = presence.node_stats("node-a").await?;
        assert_eq!(node_a.connection_count, 4);
        assert_eq!(node_a.online_member_count, 2);
        assert_eq!(node_a.online_guest_count, 1);
        assert_eq!(node_a.room_count, 2);

        let node_b = presence.node_stats("node-b").await?;
        assert_eq!(node_b.connection_count, 1);
        assert_eq!(node_b.online_member_count, 1);
        assert_eq!(node_b.online_guest_count, 0);
        assert_eq!(node_b.room_count, 1);

        let overview = presence.overview().await?;
        assert_eq!(overview.online_member_count, 2);
        assert_eq!(overview.online_guest_count, 1);
        assert_eq!(overview.connection_count, 5);
        assert_eq!(overview.active_room_count, 2);
        Ok(())
    }

    #[tokio::test]
    async fn mark_seen_for_renewal_is_windowed() -> anyhow::Result<()> {
        let presence = OnlinePresenceService::local();
        presence
            .register_connection(
                "conn-a".to_string(),
                "node-a".to_string(),
                RealtimeActor::user(user_id(1), "1"),
            )
            .await?;

        assert!(!presence.mark_seen_for_renewal("conn-a"));
        assert!(!presence.mark_seen_for_renewal("missing"));
        Ok(())
    }

    #[tokio::test]
    async fn user_has_other_connection_in_room_excludes_current_connection() -> anyhow::Result<()> {
        let presence = OnlinePresenceService::local();
        presence
            .register_connection(
                "conn-a".to_string(),
                "node-a".to_string(),
                RealtimeActor::user(user_id(1), "1"),
            )
            .await?;
        presence
            .register_connection(
                "conn-b".to_string(),
                "node-a".to_string(),
                RealtimeActor::user(user_id(1), "1"),
            )
            .await?;
        presence
            .register_connection(
                "conn-c".to_string(),
                "node-a".to_string(),
                RealtimeActor::user(user_id(1), "1"),
            )
            .await?;
        presence.join_room("conn-a", room_id(10)).await?;
        presence.join_room("conn-b", room_id(10)).await?;
        presence.join_room("conn-c", room_id(11)).await?;

        assert!(
            presence
                .user_has_other_connection_in_room(user_id(1), room_id(10), "conn-a")
                .await?
        );
        assert!(
            presence
                .user_has_other_connection_in_room(user_id(1), room_id(10), "conn-b")
                .await?
        );
        assert!(
            !presence
                .user_has_other_connection_in_room(user_id(1), room_id(11), "conn-c")
                .await?
        );
        Ok(())
    }

    #[tokio::test]
    async fn guest_room_presence_is_counted_by_actor() -> anyhow::Result<()> {
        let presence = OnlinePresenceService::local();
        let guest = RealtimeActor::guest("gst_session");
        presence
            .register_connection("guest-a".to_string(), "node-a".to_string(), guest.clone())
            .await?;
        presence
            .register_connection("guest-b".to_string(), "node-b".to_string(), guest.clone())
            .await?;
        presence.join_room("guest-a", room_id(10)).await?;
        presence.join_room("guest-b", room_id(10)).await?;
        presence
            .register_connection(
                "member-a".to_string(),
                "node-a".to_string(),
                RealtimeActor::user(user_id(1), "1"),
            )
            .await?;
        presence.join_room("member-a", room_id(10)).await?;

        assert_eq!(guest.user_id(), None);
        let stats = presence.room_stats(room_id(10)).await?;
        assert_eq!(stats.online_member_count, 1);
        assert_eq!(stats.online_guest_count, 1);
        assert_eq!(stats.connection_count, 3);
        assert_eq!(
            presence
                .actor_connection_count_in_room(&guest, room_id(10))
                .await?,
            2
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn redis_presence_reads_cross_replica_room_and_node_stats() -> anyhow::Result<()> {
        let (_container, client) = start_redis_with_client().await;
        let key_prefix = synctv_core_testing::test_redis_key_prefix("presence-cross-replica");
        let presence_a = redis_presence_service(&client, &key_prefix).await?;
        let presence_b = redis_presence_service(&client, &key_prefix).await?;

        presence_a
            .register_connection(
                "conn-a".to_string(),
                "node-a".to_string(),
                RealtimeActor::user(user_id(1), "1"),
            )
            .await?;
        presence_b
            .register_connection(
                "conn-b".to_string(),
                "node-b".to_string(),
                RealtimeActor::user(user_id(2), "2"),
            )
            .await?;
        presence_b
            .register_connection(
                "conn-c".to_string(),
                "node-b".to_string(),
                RealtimeActor::user(user_id(3), "3"),
            )
            .await?;
        presence_b
            .register_connection(
                "conn-guest".to_string(),
                "node-b".to_string(),
                RealtimeActor::guest("gst_cross_replica"),
            )
            .await?;
        presence_a.join_room("conn-a", room_id(10)).await?;
        presence_b.join_room("conn-b", room_id(10)).await?;
        presence_b.join_room("conn-c", room_id(11)).await?;
        presence_b.join_room("conn-guest", room_id(10)).await?;

        let room = presence_a.room_stats(room_id(10)).await?;
        assert_eq!(room.connection_count, 3);
        assert_eq!(room.online_member_count, 2);
        assert_eq!(room.online_guest_count, 1);
        assert_eq!(room.node_connection_counts.get("node-a"), Some(&1));
        assert_eq!(room.node_connection_counts.get("node-b"), Some(&2));

        let online = presence_a
            .room_online_user_ids(room_id(10), &[user_id(1), user_id(2), user_id(3)])
            .await?;
        assert_eq!(online, vec![user_id(1), user_id(2)]);

        let node_b = presence_a.node_stats("node-b").await?;
        assert_eq!(node_b.connection_count, 3);
        assert_eq!(node_b.online_member_count, 2);
        assert_eq!(node_b.online_guest_count, 1);
        assert_eq!(node_b.room_count, 2);

        let overview = presence_a.overview().await?;
        assert_eq!(overview.online_member_count, 3);
        assert_eq!(overview.online_guest_count, 1);
        assert_eq!(overview.connection_count, 4);
        assert_eq!(overview.active_room_count, 2);

        let all_nodes = presence_a.all_node_stats().await?;
        assert_eq!(
            all_nodes
                .iter()
                .map(|stats| stats.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["node-a", "node-b"]
        );

        let hot = presence_a.hot_room_stats().await?;
        assert_eq!(hot[0].room_id, room_id(10));
        assert_eq!(hot[0].online_member_count, 2);
        assert_eq!(hot[0].online_guest_count, 1);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn redis_presence_prunes_indexes_when_metadata_expires() -> anyhow::Result<()> {
        let (_container, client) = start_redis_with_client().await;
        let key_prefix = synctv_core_testing::test_redis_key_prefix("presence-stale-index");
        let presence = redis_presence_service(&client, &key_prefix).await?;

        presence
            .register_connection(
                "conn-stale".to_string(),
                "node-a".to_string(),
                RealtimeActor::user(user_id(1), "1"),
            )
            .await?;
        presence.join_room("conn-stale", room_id(10)).await?;

        let mut redis = redis_connection_manager(&client).await;
        redis
            .del::<_, ()>(format!("{key_prefix}presence:conn:conn-stale"))
            .await?;

        let room = presence.room_stats(room_id(10)).await?;
        assert_eq!(room.connection_count, 0);
        assert_eq!(room.online_member_count, 0);
        assert_eq!(room.online_guest_count, 0);

        let user_room = presence.user_room_stats(user_id(1), room_id(10)).await?;
        assert!(!user_room.is_online);
        assert_eq!(user_room.connection_count, 0);

        let user = presence.user_stats(user_id(1)).await?;
        assert_eq!(user.connection_count, 0);
        assert_eq!(user.room_count, 0);

        let node = presence.node_stats("node-a").await?;
        assert_eq!(node.connection_count, 0);

        let room_members: Vec<String> = redis
            .smembers(format!(
                "{key_prefix}presence:room:{}:connections",
                room_id(10)
            ))
            .await?;
        let user_members: Vec<String> = redis
            .smembers(format!(
                "{key_prefix}presence:user:{}:connections",
                user_id(1)
            ))
            .await?;
        let user_room_members: Vec<String> = redis
            .smembers(format!(
                "{key_prefix}presence:user_room:{}:{}:connections",
                user_id(1),
                room_id(10)
            ))
            .await?;
        let node_members: Vec<String> = redis
            .smembers(format!("{key_prefix}presence:node:node-a:connections"))
            .await?;

        assert!(room_members.is_empty());
        assert!(user_members.is_empty());
        assert!(user_room_members.is_empty());
        assert!(node_members.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn shared_required_presence_queries_fail_when_redis_is_unavailable() -> anyhow::Result<()>
    {
        let profile = SharedStateProfile::for_cluster_runtime(
            Some(Arc::new(UnavailableRedisRuntime)),
            "presence-fail-closed:",
            true,
        );
        let presence = OnlinePresenceService::from_shared_state_profile(&profile)?;

        let error = presence
            .room_stats(room_id(10))
            .await
            .expect_err("shared-required presence query should fail when Redis is unavailable");
        assert!(
            error.to_string().contains("Redis") || error.to_string().contains("redis"),
            "error should expose redis failure context: {error}"
        );
        Ok(())
    }
}
