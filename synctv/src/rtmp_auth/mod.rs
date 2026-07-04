//! RTMP authentication implementation for `SyncTV`
//!
//! This module provides the RTMP authentication callback that integrates
//! with `SyncTV`'s user and room management.
//!
//! On successful publish auth:
//! 1. Atomically registers the publisher in Redis (single-publisher-per-media enforcement)
//! 2. Registers the user→stream mapping in the local `StreamTracker`
//! 3. Writes `user_id → stream_key` to per-user Redis key `rtmp:user_stream:{user_id}` for cross-replica lookup
//!
//! Ongoing TTL renewal is handled by `PublisherManager::maintain_heartbeats()`.
//!
//! On unpublish:
//! 1. Unregisters the publisher from Redis
//! 2. Removes the user→stream mapping from the local `StreamTracker`
//! 3. Removes the per-user Redis key `rtmp:user_stream:{user_id}`

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use async_trait::async_trait;
use dashmap::DashMap;
use percent_encoding::percent_decode_str;
use std::collections::VecDeque;
use synctv_api::PublicIdCodec;
use synctv_core::{
    models::{MediaId, Room, RoomId, RoomStatus, UserId, UserStatus},
    service::{RoomService, StreamingPublishKeyService, UserService},
    RedisConnectionRuntime, SharedStateMode, SharedStateProfile,
};
use synctv_livestream::{StreamRegistryTrait, StreamTracker, PUBLISHER_TTL_SECS};
// TTL for the per-user rtmp:user_stream:{user_id} Redis key, matching the publisher TTL.
use synctv_xiu::rtmp::auth::{AuthCallback, AuthPublishRewrite};

const STREAMHUB_RESTARTING_MESSAGE: &str = "StreamHub is restarting, please retry in a few seconds";

#[async_trait]
pub trait UserStreamIndex: Send + Sync {
    async fn put(
        &self,
        user_id: UserId,
        room_id: RoomId,
        media_id: MediaId,
        ttl_secs: i64,
    ) -> anyhow::Result<()>;

    async fn delete(&self, user_id: UserId) -> anyhow::Result<()>;

    fn supports_cross_node_lookup(&self) -> bool;
}

#[derive(Default)]
struct LocalOnlyUserStreamIndex;

#[async_trait]
impl UserStreamIndex for LocalOnlyUserStreamIndex {
    async fn put(
        &self,
        _user_id: UserId,
        _room_id: RoomId,
        _media_id: MediaId,
        _ttl_secs: i64,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn delete(&self, _user_id: UserId) -> anyhow::Result<()> {
        Ok(())
    }

    fn supports_cross_node_lookup(&self) -> bool {
        false
    }
}

struct SharedUserStreamIndex {
    redis_runtime: Arc<dyn RedisConnectionRuntime>,
    key_prefix: String,
}

impl SharedUserStreamIndex {
    #[must_use]
    fn from_runtime(redis_runtime: Arc<dyn RedisConnectionRuntime>, key_prefix: String) -> Self {
        Self {
            redis_runtime,
            key_prefix,
        }
    }

    fn user_stream_key(&self, user_id: &str) -> String {
        format!("{}rtmp:user_stream:{}", self.key_prefix, user_id)
    }

    async fn redis_conn_snapshot(
        &self,
        operation: &'static str,
    ) -> anyhow::Result<redis::aio::ConnectionManager> {
        Ok(synctv_core::redis_runtime_snapshot(&*self.redis_runtime, operation).await?)
    }
}

#[async_trait]
impl UserStreamIndex for SharedUserStreamIndex {
    async fn put(
        &self,
        user_id: UserId,
        room_id: RoomId,
        media_id: MediaId,
        ttl_secs: i64,
    ) -> anyhow::Result<()> {
        let stream_value = format!("{room_id}|{media_id}");
        let redis_key = self.user_stream_key(&user_id.to_string());
        let operation = "store RTMP user stream index";
        let mut conn = self.redis_conn_snapshot(operation).await?;
        let _: ((), i64) = tokio::time::timeout(
            self.redis_runtime.operation_timeout(),
            redis::pipe()
                .set(&redis_key, &stream_value)
                .expire(&redis_key, ttl_secs)
                .query_async(&mut conn),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Redis timeout: {operation}"))??;
        Ok(())
    }

    async fn delete(&self, user_id: UserId) -> anyhow::Result<()> {
        let operation = "delete RTMP user stream index";
        let mut conn = self.redis_conn_snapshot(operation).await?;
        let key = self.user_stream_key(&user_id.to_string());
        let _: () = tokio::time::timeout(
            self.redis_runtime.operation_timeout(),
            redis::cmd("DEL").arg(&key).query_async(&mut conn),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Redis timeout: {operation}"))??;
        Ok(())
    }

    fn supports_cross_node_lookup(&self) -> bool {
        true
    }
}

pub(crate) fn user_stream_index_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> synctv_core::Result<Arc<dyn UserStreamIndex>> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => Ok(Arc::new(SharedUserStreamIndex::from_runtime(
            profile.require_shared_runtime("RTMP user stream index")?,
            profile.key_prefix().to_string(),
        ))),
        SharedStateMode::SharedBestEffort | SharedStateMode::LocalOnly => {
            Ok(Arc::new(LocalOnlyUserStreamIndex))
        }
    }
}

/// Stream lifecycle event emitted on publish/unpublish
#[derive(Debug, Clone)]
pub enum StreamLifecycleEvent {
    /// A publisher successfully started streaming
    Started {
        room_id: String,
        media_id: String,
        user_id: String,
    },
    /// A publisher stopped streaming
    Stopped {
        room_id: String,
        media_id: String,
        user_id: String,
    },
}

impl StreamLifecycleEvent {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Started { .. } => "started",
            Self::Stopped { .. } => "stopped",
        }
    }

    fn room_id(&self) -> &str {
        match self {
            Self::Started { room_id, .. } | Self::Stopped { room_id, .. } => room_id,
        }
    }

    fn media_id(&self) -> &str {
        match self {
            Self::Started { media_id, .. } | Self::Stopped { media_id, .. } => media_id,
        }
    }

    fn user_id(&self) -> &str {
        match self {
            Self::Started { user_id, .. } | Self::Stopped { user_id, .. } => user_id,
        }
    }
}

fn publish_stream_lifecycle_event(
    tx: &tokio::sync::broadcast::Sender<StreamLifecycleEvent>,
    event: StreamLifecycleEvent,
) {
    let lifecycle_event = event.kind();
    let room_id = event.room_id().to_string();
    let media_id = event.media_id().to_string();
    let user_id = event.user_id().to_string();
    match tx.send(event) {
        Ok(receiver_count) => {
            tracing::debug!(
                lifecycle_event,
                room_id = %room_id,
                media_id = %media_id,
                user_id = %user_id,
                receiver_count,
                "Published RTMP stream lifecycle event"
            );
        }
        Err(error) => {
            tracing::warn!(
                lifecycle_event,
                room_id = %room_id,
                media_id = %media_id,
                user_id = %user_id,
                error = %error,
                "Failed to publish RTMP stream lifecycle event: no active receivers"
            );
        }
    }
}

#[derive(Debug, Clone)]
struct PendingPublishCleanup {
    epoch: u64,
    user_id: UserId,
}

#[derive(Clone)]
struct PublisherCleanupRuntime {
    user_stream_tracker: Arc<StreamTracker>,
    registry: Arc<dyn StreamRegistryTrait>,
    node_id: String,
    api_address: String,
    stream_event_tx: Option<tokio::sync::broadcast::Sender<StreamLifecycleEvent>>,
    user_stream_index: Arc<dyn UserStreamIndex>,
    pending_publish_cleanups: Arc<DashMap<(String, String), VecDeque<PendingPublishCleanup>>>,
}

struct PublisherCleanupRuntimeConfig {
    user_stream_tracker: Arc<StreamTracker>,
    registry: Arc<dyn StreamRegistryTrait>,
    node_id: String,
    api_address: String,
    stream_event_tx: Option<tokio::sync::broadcast::Sender<StreamLifecycleEvent>>,
    user_stream_index: Arc<dyn UserStreamIndex>,
}

impl PublisherCleanupRuntime {
    fn new(config: PublisherCleanupRuntimeConfig) -> Self {
        Self {
            user_stream_tracker: config.user_stream_tracker,
            registry: config.registry,
            node_id: config.node_id,
            api_address: config.api_address,
            stream_event_tx: config.stream_event_tx,
            user_stream_index: config.user_stream_index,
            pending_publish_cleanups: Arc::new(DashMap::new()),
        }
    }
}

/// Maximum number of pending publish cleanup entries retained per (room, media) key.
/// This bounds memory growth under pathological retry scenarios while allowing
/// a small number of retries to preserve their epoch fences.
const MAX_PENDING_PUBLISH_CLEANUPS: usize = 3;

/// RTMP authentication implementation for `SyncTV`
///
/// Validates RTMP publish/play requests against:
/// - Room existence and status (not banned/pending)
/// - JWT publish keys for publishers (validates `room_id` match)
/// - User status (not banned/deleted)
/// - Authorization (global admin, room admin/creator, or media creator)
/// - Single-publisher-per-media (atomic Redis registration)
/// - RTMP pull (play) is unconditionally rejected — viewers must use HTTP-FLV or HLS
///
/// On successful publish auth, registers the publisher in Redis and
/// spawns a TTL renewal task. On unpublish, cleans up Redis and tracker state.
pub struct SyncTvRtmpAuth {
    room_service: Arc<RoomService>,
    user_service: Arc<UserService>,
    publish_key_service: Arc<dyn StreamingPublishKeyService>,
    publisher_cleanup: PublisherCleanupRuntime,
    /// Broadcast channel for stream lifecycle events (StreamStarted/StreamStopped)
    /// Shared codec for client-visible RTMP app/stream identifiers.
    public_id_codec: Arc<PublicIdCodec>,
    /// Optional shared restart flag from LivestreamServer. When set, new
    /// publications are rejected during the StreamHub cleanup/re-register window.
    is_restarting: Option<Arc<AtomicBool>>,
}

pub struct SyncTvRtmpAuthConfig {
    pub room_service: Arc<RoomService>,
    pub user_service: Arc<UserService>,
    pub publish_key_service: Arc<dyn StreamingPublishKeyService>,
    pub user_stream_tracker: Arc<StreamTracker>,
    pub registry: Arc<dyn StreamRegistryTrait>,
    pub node_id: String,
    pub api_address: String,
    pub public_id_codec: Arc<PublicIdCodec>,
    pub stream_event_tx: Option<tokio::sync::broadcast::Sender<StreamLifecycleEvent>>,
    pub is_restarting: Option<Arc<AtomicBool>>,
    pub user_stream_index: Arc<dyn UserStreamIndex>,
}

impl SyncTvRtmpAuth {
    pub fn new(config: SyncTvRtmpAuthConfig) -> Self {
        Self {
            room_service: config.room_service,
            user_service: config.user_service,
            publish_key_service: config.publish_key_service,
            publisher_cleanup: PublisherCleanupRuntime::new(PublisherCleanupRuntimeConfig {
                user_stream_tracker: config.user_stream_tracker,
                registry: config.registry,
                node_id: config.node_id.clone(),
                api_address: config.api_address.clone(),
                stream_event_tx: config.stream_event_tx,
                user_stream_index: config.user_stream_index,
            }),
            public_id_codec: config.public_id_codec,
            is_restarting: config.is_restarting,
        }
    }

    fn decode_rtmp_room_id(
        &self,
        app_name: &str,
    ) -> Result<RoomId, Box<dyn std::error::Error + Send + Sync>> {
        self.public_id_codec
            .decode_room_id(app_name)
            .map_err(|error| format!("Invalid RTMP room id: {error}").into())
    }

    fn decode_rtmp_media_id(
        &self,
        stream_name: &str,
    ) -> Result<MediaId, Box<dyn std::error::Error + Send + Sync>> {
        self.public_id_codec
            .decode_media_id(stream_name)
            .map_err(|error| format!("Invalid RTMP media id: {error}").into())
    }
}

impl PublisherCleanupRuntime {
    fn remember_pending_publish_cleanup(
        &self,
        room_id: &str,
        media_id: &str,
        attempt: PendingPublishCleanup,
    ) {
        let mut deque = self
            .pending_publish_cleanups
            .entry((room_id.to_string(), media_id.to_string()))
            .or_default();
        if deque.len() >= MAX_PENDING_PUBLISH_CLEANUPS {
            deque.pop_front();
        }
        deque.push_back(attempt);
    }

    fn peek_pending_publish_cleanup(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Option<PendingPublishCleanup> {
        self.pending_publish_cleanups
            .get(&(room_id.to_string(), media_id.to_string()))
            .and_then(|attempts| attempts.front().cloned())
    }

    fn consume_pending_publish_cleanup(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Option<PendingPublishCleanup> {
        let key = (room_id.to_string(), media_id.to_string());
        self.pending_publish_cleanups
            .get_mut(&key)
            .and_then(|mut attempts| {
                let attempt = attempts.pop_front();
                let empty = attempts.is_empty();
                drop(attempts);
                if empty {
                    self.pending_publish_cleanups.remove(&key);
                }
                attempt
            })
    }

    fn resolve_publish_cleanup(
        &self,
        room_id: &str,
        media_id: &str,
        context: &'static str,
    ) -> Option<PendingPublishCleanup> {
        if let Some(attempt) = self.peek_pending_publish_cleanup(room_id, media_id) {
            return Some(attempt);
        }

        tracing::warn!(
            room_id = %room_id,
            media_id = %media_id,
            "Skipping {context} cleanup because no in-memory publish fence is available"
        );

        None
    }

    async fn lookup_registered_epoch(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        self.registry
            .get_publisher(room_id, media_id)
            .await
            .map_err(|e| format!("Failed to fetch publisher epoch after registration: {e}"))?
            .ok_or_else(|| {
                format!(
                    "Publisher registration disappeared before epoch capture: room={room_id}, media={media_id}"
                )
                .into()
            })
            .map(|publisher| publisher.epoch)
    }

    async fn cleanup_publisher_if_current_attempt(
        &self,
        room_id: &str,
        media_id: &str,
        expected_epoch: u64,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let current = self
            .registry
            .get_publisher(room_id, media_id)
            .await
            .map_err(|e| format!("Failed to inspect current publisher before cleanup: {e}"))?;

        let Some(current) = current else {
            return Ok(true);
        };

        if current.epoch != expected_epoch {
            return Ok(false);
        }

        self.registry
            .unregister_publisher_if_epoch_matches(room_id, media_id, expected_epoch)
            .await
            .map_err(|e| format!("Failed to unregister publisher with epoch fence: {e}"))?;

        let remaining = self
            .registry
            .get_publisher(room_id, media_id)
            .await
            .map_err(|e| format!("Failed to inspect current publisher after cleanup: {e}"))?;

        Ok(remaining.is_none())
    }

    async fn delete_user_stream_key(&self, user_id: UserId, context: &'static str) {
        if let Err(error) = self.user_stream_index.delete(user_id).await {
            tracing::warn!(
                user_id = %user_id,
                cross_node_lookup = self.user_stream_index.supports_cross_node_lookup(),
                "Failed to remove user-stream index entry on {} (non-fatal): {}",
                context,
                error
            );
        }
    }

    /// Register the publisher in Redis and set up tracking.
    ///
    /// Ongoing TTL renewal is handled by `PublisherManager::maintain_heartbeats()`.
    /// This method only performs the initial registration.
    async fn register_and_start_ttl(
        &self,
        validated: &ValidatedPublish,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let registered = self
            .registry
            .try_register_publisher(
                &validated.room_id.to_string(),
                &validated.media_id.to_string(),
                &self.node_id,
                &validated.user_id.to_string(),
                &self.api_address,
            )
            .await
            .map_err(|e| format!("Failed to register publisher in Redis: {e}"))?;

        if !registered {
            return Err(format!(
                "Another publisher is already active for media {} in room {}",
                validated.media_id, validated.room_id
            )
            .into());
        }

        let registered_epoch = self
            .lookup_registered_epoch(
                &validated.room_id.to_string(),
                &validated.media_id.to_string(),
            )
            .await?;

        tracing::info!(
            "Publisher authenticated and registered: user={}, room={}, media={}, node={}, auth={}, epoch={}",
            validated.user_id,
            validated.room_id,
            validated.media_id,
            self.node_id,
            validated.auth_level,
            registered_epoch,
        );

        if let Err(error) = self
            .user_stream_index
            .put(
                validated.user_id,
                validated.room_id,
                validated.media_id,
                PUBLISHER_TTL_SECS,
            )
            .await
        {
            tracing::error!(
                user_id = %validated.user_id,
                cross_node_lookup = self.user_stream_index.supports_cross_node_lookup(),
                "Failed to write shared user-stream index after publisher registration: {}. \
                 Rolling back publisher registration to maintain consistency.",
                error
            );
            if let Err(unreg_err) = self
                .registry
                .unregister_publisher_if_epoch_matches(
                    &validated.room_id.to_string(),
                    &validated.media_id.to_string(),
                    registered_epoch,
                )
                .await
            {
                tracing::error!(
                    room_id = %validated.room_id,
                    media_id = %validated.media_id,
                    "Rollback of publisher registration also failed: {}. \
                     Registry TTL will eventually expire the stale entry.",
                    unreg_err
                );
            }
            return Err(format!("Failed to write shared user-stream index: {error}").into());
        }

        self.user_stream_tracker.insert(
            validated.user_id.to_string(),
            validated.room_id.to_string(),
            validated.media_id.to_string(),
        );

        if let Some(ref tx) = self.stream_event_tx {
            publish_stream_lifecycle_event(
                tx,
                StreamLifecycleEvent::Started {
                    room_id: validated.room_id.to_string(),
                    media_id: validated.media_id.to_string(),
                    user_id: validated.user_id.to_string(),
                },
            );
        }

        self.remember_pending_publish_cleanup(
            &validated.room_id.to_string(),
            &validated.media_id.to_string(),
            PendingPublishCleanup {
                epoch: registered_epoch,
                user_id: validated.user_id,
            },
        );

        Ok(())
    }

    async fn cleanup_on_unpublish(&self, room_id: &str, media_id: &str) {
        let Some(attempt) = self.resolve_publish_cleanup(room_id, media_id, "on_unpublish") else {
            return;
        };

        let should_cleanup = match self
            .cleanup_publisher_if_current_attempt(room_id, media_id, attempt.epoch)
            .await
        {
            Ok(should_cleanup) => should_cleanup,
            Err(e) => {
                tracing::error!(
                    room_id = %room_id,
                    media_id = %media_id,
                    epoch = attempt.epoch,
                    "Failed to fence publisher cleanup on unpublish; keeping pending cleanup for retry: {}",
                    e
                );
                return;
            }
        };

        if !should_cleanup {
            let _ = self.consume_pending_publish_cleanup(room_id, media_id);
            tracing::info!(
                room_id = %room_id,
                media_id = %media_id,
                epoch = attempt.epoch,
                "Ignoring stale on_unpublish cleanup for superseded publisher epoch"
            );
            return;
        }

        let _ = self.consume_pending_publish_cleanup(room_id, media_id);
        let tracked_user = self.user_stream_tracker.remove_stream(room_id, media_id);
        tracing::info!(
            user_id = %attempt.user_id,
            room_id = %room_id,
            media_id = %media_id,
            had_tracker_entry = tracked_user.is_some(),
            "Publisher unpublished, fenced cleanup completed"
        );

        self.delete_user_stream_key(attempt.user_id, "unpublish")
            .await;

        if let Some(ref tx) = self.stream_event_tx {
            publish_stream_lifecycle_event(
                tx,
                StreamLifecycleEvent::Stopped {
                    room_id: room_id.to_string(),
                    media_id: media_id.to_string(),
                    user_id: attempt.user_id.to_string(),
                },
            );
        }
    }

    async fn cleanup_on_publish_rollback(&self, room_id: &str, media_id: &str) {
        tracing::warn!(
            room_id = %room_id,
            media_id = %media_id,
            "Rolling back publisher registration due to StreamHub failure"
        );

        let Some(attempt) = self.resolve_publish_cleanup(room_id, media_id, "rollback") else {
            return;
        };

        let should_cleanup = match self
            .cleanup_publisher_if_current_attempt(room_id, media_id, attempt.epoch)
            .await
        {
            Ok(should_cleanup) => should_cleanup,
            Err(e) => {
                tracing::warn!(
                    room_id = %room_id,
                    media_id = %media_id,
                    epoch = attempt.epoch,
                    error = %e,
                    "Failed to rollback publisher registration with epoch fence; keeping pending cleanup for retry"
                );
                return;
            }
        };

        if !should_cleanup {
            let _ = self.consume_pending_publish_cleanup(room_id, media_id);
            tracing::info!(
                room_id = %room_id,
                media_id = %media_id,
                epoch = attempt.epoch,
                "Ignoring stale rollback cleanup for superseded publisher epoch"
            );
            return;
        }

        let _ = self.consume_pending_publish_cleanup(room_id, media_id);
        let _ = self.user_stream_tracker.remove_stream(room_id, media_id);
        self.delete_user_stream_key(attempt.user_id, "rollback")
            .await;

        tracing::info!(
            room_id = %room_id,
            media_id = %media_id,
            "Publisher registration rolled back successfully"
        );
    }
}

#[async_trait]
impl AuthCallback for SyncTvRtmpAuth {
    async fn on_publish(
        &self,
        app_name: &str,
        stream_name: &str,
        query: Option<&str>,
    ) -> Result<Option<AuthPublishRewrite>, Box<dyn std::error::Error + Send + Sync>> {
        if streamhub_restart_in_progress(self.is_restarting.as_ref()) {
            tracing::warn!(
                room_id = %app_name,
                "RTMP publish rejected: StreamHub is restarting"
            );
            return Err(STREAMHUB_RESTARTING_MESSAGE.into());
        }

        // Phase 1: Validate room, token, user status, and authorization
        let validated = self
            .validate_publish_request(app_name, stream_name, query)
            .await?;

        // Phase 2: Register in Redis, track mapping, emit event, spawn TTL renewal
        self.publisher_cleanup
            .register_and_start_ttl(&validated)
            .await?;

        // Phase 3: Return rewrite so StreamHub uses canonical (room_id, media_id)
        // instead of the raw RTMP identifiers (room_id, JWT_TOKEN).
        Ok(Some(AuthPublishRewrite {
            app_name: validated.room_id.to_string(),
            stream_name: validated.media_id.to_string(),
        }))
    }

    /// RTMP pull (play) is permanently disabled.
    ///
    /// SyncTV only accepts RTMP for publishing. Viewers must use HTTP-FLV, HLS,
    /// or provider playback URLs that go through normal authorization paths.
    async fn on_play(
        &self,
        app_name: &str,
        stream_name: &str,
        _query: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tracing::warn!(
            room_id = %app_name,
            media_id = %stream_name,
            "RTMP play rejected: RTMP playback is not supported"
        );
        Err("RTMP playback is disabled. Use HTTP-FLV or HLS endpoints for playback.".into())
    }

    async fn on_unplay(&self, app_name: &str, _stream_name: &str, _query: Option<&str>) {
        tracing::info!(
            room_id = %app_name,
            "RTMP play session disconnected"
        );
    }

    async fn on_unpublish(&self, app_name: &str, stream_name: &str, _query: Option<&str>) {
        self.publisher_cleanup
            .cleanup_on_unpublish(app_name, stream_name)
            .await;
    }

    /// Roll back publisher registration when `StreamHub` publish fails after auth.
    ///
    /// Called when `on_publish` succeeded (registered in Redis, inserted tracker entry,
    /// wrote user_streams hash) but a later step (e.g., `StreamHub` publish) failed.
    /// Cleans up all state changes made during `on_publish`:
    /// 1. Unregister publisher from Redis
    /// 2. Remove user->stream mapping from local tracker
    /// 3. Remove per-user `rtmp:user_stream:{user_id}` Redis key
    async fn on_publish_rollback(&self, app_name: &str, stream_name: &str, _query: Option<&str>) {
        self.publisher_cleanup
            .cleanup_on_publish_rollback(app_name, stream_name)
            .await;
    }
}

fn extract_token_from_query(query: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some(encoded_value) = pair.strip_prefix("token=") {
            let decoded = percent_decode_str(encoded_value)
                .decode_utf8_lossy()
                .into_owned();

            if decoded.trim().is_empty() {
                return None;
            }

            return Some(decoded);
        }
    }
    None
}

fn streamhub_restart_in_progress(is_restarting: Option<&Arc<AtomicBool>>) -> bool {
    is_restarting.is_some_and(|flag| flag.load(Ordering::Acquire))
}

/// Validated publish claims with authorization level
#[derive(Debug)]
struct ValidatedPublish {
    room_id: RoomId,
    media_id: MediaId,
    user_id: UserId,
    auth_level: &'static str,
}

fn media_creator_publish_authorized(
    is_global_admin: bool,
    is_room_admin_or_creator: bool,
    is_room_member: bool,
    media_creator_id: Option<&UserId>,
    user_id: &UserId,
) -> bool {
    !is_global_admin
        && !is_room_admin_or_creator
        && is_room_member
        && media_creator_id == Some(user_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoomAccessRejection {
    Banned,
    Closed,
}

impl RoomAccessRejection {
    fn into_error(self, app_name: &str) -> Box<dyn std::error::Error + Send + Sync> {
        match self {
            Self::Banned => format!("Room {app_name} is banned").into(),
            Self::Closed => format!("Room {app_name} is closed").into(),
        }
    }
}

fn validate_rtmp_room_state(room: &Room) -> Result<(), RoomAccessRejection> {
    if room.is_banned {
        return Err(RoomAccessRejection::Banned);
    }
    if room.status == RoomStatus::Closed {
        return Err(RoomAccessRejection::Closed);
    }

    Ok(())
}

impl SyncTvRtmpAuth {
    /// Validate room status, JWT token, user status, and authorization.
    /// Returns the validated claims with authorization level on success.
    async fn validate_publish_request(
        &self,
        app_name: &str,
        stream_name: &str,
        query: Option<&str>,
    ) -> Result<ValidatedPublish, Box<dyn std::error::Error + Send + Sync>> {
        let expected_room_id = self.decode_rtmp_room_id(app_name)?;
        let expected_media_id = self.decode_rtmp_media_id(stream_name)?;

        // Validate room
        let room = self
            .room_service
            .get_room(&expected_room_id)
            .await
            .map_err(|e| format!("Failed to load room: {e}"))?;

        validate_rtmp_room_state(&room).map_err(|reason| reason.into_error(app_name))?;

        // RTMP publish requires an explicit token query parameter. The stream name
        // is reserved for the media binding and must not be overloaded as a token.
        let token_owned: Option<String> = query.and_then(extract_token_from_query);
        let token = token_owned.as_deref().ok_or_else(|| {
            "Missing token query parameter; RTMP publish must use ?token=<publish_key>".to_string()
        })?;

        // Validate JWT stream_key
        let claims = self
            .publish_key_service
            .validate_publish_key_for_stream_claims(token, &expected_room_id, &expected_media_id)
            .await
            .map_err(|e| format!("Invalid stream key: {e}"))?;

        // Re-verify user status at connection time
        let user_id = claims
            .user_id
            .parse::<UserId>()
            .map_err(|error| format!("Invalid user id in stream key: {error}"))?;
        let user = self
            .user_service
            .get_user(&user_id)
            .await
            .map_err(|e| format!("Failed to load user: {e}"))?;

        if user.status == UserStatus::Banned {
            return Err(format!(
                "User {} is not allowed to publish while account status is {}",
                claims.user_id, user.status
            )
            .into());
        }
        if user.deleted_at.is_some() {
            return Err(format!("User {} is deleted", claims.user_id).into());
        }

        // Authorization check
        let auth_level = self
            .check_publish_authorization(
                &user,
                &expected_room_id,
                &expected_media_id,
                &user_id,
                &claims,
            )
            .await?;

        Ok(ValidatedPublish {
            room_id: expected_room_id,
            media_id: expected_media_id,
            user_id,
            auth_level,
        })
    }

    /// Check that the user has permission to publish to this room/media.
    /// Returns the authorization level string on success.
    async fn check_publish_authorization(
        &self,
        user: &synctv_core::models::User,
        room_id: &RoomId,
        media_id: &MediaId,
        user_id: &UserId,
        claims: &synctv_core::service::PublishClaims,
    ) -> Result<&'static str, Box<dyn std::error::Error + Send + Sync>> {
        let is_global_admin = user.role.is_admin_or_above();

        let room_member = if is_global_admin {
            None
        } else {
            self.room_service
                .member_service()
                .get_member(room_id, user_id)
                .await
                .ok()
                .flatten()
        };

        let is_room_admin_or_creator = if is_global_admin {
            false
        } else {
            room_member.as_ref().is_some_and(|member| {
                matches!(
                    member.role,
                    synctv_core::models::RoomRole::Creator | synctv_core::models::RoomRole::Admin
                )
            })
        };

        // Verify media belongs to this room
        let media = self
            .room_service
            .media_service()
            .get_media(media_id)
            .await
            .map_err(|e| format!("Failed to load media: {e}"))?
            .ok_or_else(|| format!("Media {} not found", claims.media_id))?;
        if media.room_id != *room_id {
            return Err(format!(
                "Media {} does not belong to room {}",
                claims.media_id, room_id
            )
            .into());
        }

        let is_media_creator = if !is_global_admin && !is_room_admin_or_creator {
            if room_member.is_none() {
                return Err(format!(
                    "Insufficient permissions to publish: user {} is not a member of room {}",
                    claims.user_id, room_id
                )
                .into());
            }
            media_creator_publish_authorized(
                is_global_admin,
                is_room_admin_or_creator,
                room_member.is_some(),
                media.creator_id.as_ref(),
                user_id,
            )
        } else {
            false
        };

        if !is_global_admin && !is_room_admin_or_creator && !is_media_creator {
            return Err(format!(
                "Insufficient permissions to publish: user {} is not admin, room admin/creator, or media creator",
                claims.user_id
            ).into());
        }

        Ok(if is_global_admin {
            "global_admin"
        } else if is_room_admin_or_creator {
            "room_admin"
        } else {
            "media_creator"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use synctv_livestream::{
        local_stream_registry, ActivePublisherEntry, PublisherInfo, PublisherRefreshOutcome,
        StreamRegistryTrait,
    };

    struct FlakyUnregisterRegistry {
        inner: Arc<dyn StreamRegistryTrait>,
        fail_unregister_if_epoch_matches_times: AtomicUsize,
    }

    impl FlakyUnregisterRegistry {
        fn new(inner: Arc<dyn StreamRegistryTrait>) -> Self {
            Self {
                inner,
                fail_unregister_if_epoch_matches_times: AtomicUsize::new(0),
            }
        }

        fn set_fail_unregister_if_epoch_matches_times(&self, times: usize) {
            self.fail_unregister_if_epoch_matches_times
                .store(times, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl StreamRegistryTrait for FlakyUnregisterRegistry {
        async fn try_register_publisher(
            &self,
            room_id: &str,
            media_id: &str,
            node_id: &str,
            user_id: &str,
            api_address: &str,
        ) -> anyhow::Result<bool> {
            self.inner
                .try_register_publisher(room_id, media_id, node_id, user_id, api_address)
                .await
        }

        async fn refresh_publisher_ttl(
            &self,
            room_id: &str,
            media_id: &str,
            user_id: &str,
            node_id: &str,
            expected_epoch: u64,
        ) -> anyhow::Result<PublisherRefreshOutcome> {
            self.inner
                .refresh_publisher_ttl(room_id, media_id, user_id, node_id, expected_epoch)
                .await
        }

        async fn unregister_publisher(&self, room_id: &str, media_id: &str) -> anyhow::Result<()> {
            self.inner.unregister_publisher(room_id, media_id).await
        }

        async fn unregister_publisher_if_epoch_matches(
            &self,
            room_id: &str,
            media_id: &str,
            expected_epoch: u64,
        ) -> anyhow::Result<()> {
            let remaining_failures = self
                .fail_unregister_if_epoch_matches_times
                .load(Ordering::SeqCst);
            if remaining_failures > 0 {
                self.fail_unregister_if_epoch_matches_times
                    .fetch_sub(1, Ordering::SeqCst);
                return Err(anyhow::anyhow!(
                    "simulated Redis failure in unregister_publisher_if_epoch_matches"
                ));
            }

            self.inner
                .unregister_publisher_if_epoch_matches(room_id, media_id, expected_epoch)
                .await
        }

        async fn get_publisher(
            &self,
            room_id: &str,
            media_id: &str,
        ) -> anyhow::Result<Option<PublisherInfo>> {
            self.inner.get_publisher(room_id, media_id).await
        }

        async fn is_stream_active(&self, room_id: &str, media_id: &str) -> anyhow::Result<bool> {
            self.inner.is_stream_active(room_id, media_id).await
        }

        async fn list_active_publishers(&self) -> anyhow::Result<Vec<ActivePublisherEntry>> {
            self.inner.list_active_publishers().await
        }

        async fn list_streams_for_room(&self, room_id: &str) -> anyhow::Result<Vec<String>> {
            self.inner.list_streams_for_room(room_id).await
        }

        async fn get_user_publishers(
            &self,
            user_id: &str,
        ) -> anyhow::Result<Vec<(String, String)>> {
            self.inner.get_user_publishers(user_id).await
        }

        async fn get_user_publishers_for_room(
            &self,
            room_id: &str,
            user_id: &str,
        ) -> anyhow::Result<Vec<(String, String)>> {
            self.inner
                .get_user_publishers_for_room(room_id, user_id)
                .await
        }

        async fn validate_epoch(
            &self,
            room_id: &str,
            media_id: &str,
            epoch: u64,
        ) -> anyhow::Result<bool> {
            self.inner.validate_epoch(room_id, media_id, epoch).await
        }

        async fn cleanup_all_publishers_for_node(&self, node_id: &str) -> anyhow::Result<()> {
            self.inner.cleanup_all_publishers_for_node(node_id).await
        }
    }

    #[test]
    fn test_extract_token_single_param() {
        let result = extract_token_from_query("token=abc123");
        assert_eq!(result.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_extract_token_among_multiple_params() {
        let result = extract_token_from_query("foo=bar&token=my_jwt_token&baz=qux");
        assert_eq!(result.as_deref(), Some("my_jwt_token"));
    }

    #[test]
    fn test_extract_token_first_param() {
        let result = extract_token_from_query("token=first_token&other=value");
        assert_eq!(result.as_deref(), Some("first_token"));
    }

    #[test]
    fn test_extract_token_last_param() {
        let result = extract_token_from_query("other=value&token=last_token");
        assert_eq!(result.as_deref(), Some("last_token"));
    }

    #[test]
    fn test_extract_token_missing_returns_none() {
        let result = extract_token_from_query("foo=bar&baz=qux");
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_token_empty_query_returns_none() {
        let result = extract_token_from_query("");
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_token_empty_value_returns_none() {
        // Empty token should return None to avoid meaningless JWT validation errors
        let result = extract_token_from_query("token=");
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_token_empty_value_with_other_params() {
        // Empty token among other params should also return None
        let result = extract_token_from_query("foo=bar&token=&baz=qux");
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_token_whitespace_only_returns_none() {
        // Whitespace-only token should return None (whitespace is not meaningful)
        let result = extract_token_from_query("token=   ");
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_token_url_encoded_empty_returns_none() {
        // URL-encoded empty/whitespace should return None
        let result = extract_token_from_query("token=%20%20");
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_token_similar_key_not_matched() {
        // "mytoken=" should not match "token="
        let result = extract_token_from_query("mytoken=abc&stream_token=def");
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_token_jwt_like_value() {
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let query = format!("token={jwt}");
        let result = extract_token_from_query(&query);
        assert_eq!(result.as_deref(), Some(jwt));
    }

    #[test]
    fn test_extract_token_percent_encoded_plus() {
        let result = extract_token_from_query("token=foo%2Bbar");
        assert_eq!(result.as_deref(), Some("foo+bar"));
    }

    #[test]
    fn test_extract_token_plus_sign_in_token() {
        // `+` used literally in query strings (not encoded) should be preserved
        let result = extract_token_from_query("token=abc+def");
        // percent_decode_str does NOT convert `+` to space (only %20 is space in strict mode)
        assert_eq!(result.as_deref(), Some("abc+def"));
    }

    #[test]
    fn test_publish_requires_query_token() {
        let query = None;
        let token_owned: Option<String> = query.and_then(extract_token_from_query);

        assert!(
            token_owned.is_none(),
            "RTMP publish without ?token= must not produce a token"
        );
    }

    #[test]
    fn test_query_token_mode_keeps_stream_name_for_media_binding() {
        let query = Some("token=query_jwt_token");
        let stream_name = "media_123";
        let token_owned: Option<String> = query.and_then(extract_token_from_query);
        let token = token_owned
            .as_deref()
            .expect("query token mode must require an explicit token");

        assert_eq!(token, "query_jwt_token");
        assert_eq!(
            stream_name, "media_123",
            "query token mode must still reserve stream_name for media binding"
        );
    }

    #[tokio::test]
    async fn test_rtmp_ids_require_public_id_prefixes() {
        let codec = PublicIdCodec::plain();

        assert_eq!(
            codec
                .decode_room_id("room_42")
                .expect("room_42 should decode as a public room id"),
            RoomId::expect_positive(42)
        );
        assert_eq!(
            codec
                .decode_media_id("med_99")
                .expect("med_99 should decode as a public media id"),
            MediaId::expect_positive(99)
        );
        assert!(codec.decode_room_id("42").is_err());
        assert!(codec.decode_media_id("99").is_err());
    }

    #[test]
    fn test_publish_restart_guard_rejects_during_streamhub_restart() {
        let restarting = Arc::new(AtomicBool::new(true));

        assert!(streamhub_restart_in_progress(Some(&restarting)));
        assert_eq!(
            STREAMHUB_RESTARTING_MESSAGE,
            "StreamHub is restarting, please retry in a few seconds"
        );
    }

    #[tokio::test]
    async fn test_delayed_unpublish_does_not_remove_newer_registration() {
        let registry = synctv_livestream::local_stream_registry();
        let runtime = make_publisher_cleanup_runtime(registry.clone());

        let room_id = "101";
        let media_id = "201";
        let second_user_id = "302";

        runtime
            .register_and_start_ttl(&validated_publish(room_id, media_id, "301"))
            .await
            .expect("first publish registration should succeed");

        let first_epoch = registry
            .get_publisher(room_id, media_id)
            .await
            .expect("first publisher lookup should succeed")
            .expect("first publisher should exist")
            .epoch;

        registry
            .unregister_publisher_if_epoch_matches(room_id, media_id, first_epoch)
            .await
            .expect("test setup should remove first publisher");

        runtime
            .register_and_start_ttl(&validated_publish(room_id, media_id, second_user_id))
            .await
            .expect("second publish registration should succeed");

        runtime.cleanup_on_unpublish(room_id, media_id).await;

        let current = registry
            .get_publisher(room_id, media_id)
            .await
            .expect("publisher lookup should succeed after stale unpublish")
            .expect("stale unpublish must not remove the replacement publisher");
        assert_eq!(current.user_id, second_user_id);
        assert_eq!(
            runtime
                .user_stream_tracker
                .get_stream_user(room_id, media_id),
            Some(second_user_id.to_string()),
            "stale unpublish must not remove the replacement stream tracker entry"
        );
    }

    #[tokio::test]
    async fn test_delayed_unpublish_preserves_newer_rollback_fence() {
        let registry = synctv_livestream::local_stream_registry();
        let runtime = make_publisher_cleanup_runtime(registry.clone());

        let room_id = "102";
        let media_id = "202";

        runtime
            .register_and_start_ttl(&validated_publish(room_id, media_id, "303"))
            .await
            .expect("first publish registration should succeed");

        let first_epoch = registry
            .get_publisher(room_id, media_id)
            .await
            .expect("first publisher lookup should succeed")
            .expect("first publisher should exist")
            .epoch;

        registry
            .unregister_publisher_if_epoch_matches(room_id, media_id, first_epoch)
            .await
            .expect("test setup should remove first publisher");

        runtime
            .register_and_start_ttl(&validated_publish(room_id, media_id, "304"))
            .await
            .expect("second publish registration should succeed");

        runtime.cleanup_on_unpublish(room_id, media_id).await;
        runtime.cleanup_on_publish_rollback(room_id, media_id).await;

        assert!(
            !registry
                .is_stream_active(room_id, media_id)
                .await
                .expect("publisher activity lookup should succeed"),
            "stale unpublish must not consume the newer rollback fence"
        );
    }

    #[tokio::test]
    async fn test_delayed_rollback_does_not_remove_newer_registration() {
        let registry = synctv_livestream::local_stream_registry();
        let runtime = make_publisher_cleanup_runtime(registry.clone());

        let room_id = "103";
        let media_id = "203";
        let second_user_id = "306";

        runtime
            .register_and_start_ttl(&validated_publish(room_id, media_id, "305"))
            .await
            .expect("first publish registration should succeed");

        let first_epoch = registry
            .get_publisher(room_id, media_id)
            .await
            .expect("first publisher lookup should succeed")
            .expect("first publisher should exist")
            .epoch;

        registry
            .unregister_publisher_if_epoch_matches(room_id, media_id, first_epoch)
            .await
            .expect("test setup should remove first publisher");

        runtime
            .register_and_start_ttl(&validated_publish(room_id, media_id, second_user_id))
            .await
            .expect("second publish registration should succeed");

        runtime.cleanup_on_publish_rollback(room_id, media_id).await;

        let current = registry
            .get_publisher(room_id, media_id)
            .await
            .expect("publisher lookup should succeed after stale rollback")
            .expect("stale rollback must not remove the replacement publisher");
        assert_eq!(current.user_id, second_user_id);
    }

    #[tokio::test]
    async fn test_unpublish_retry_preserves_fence_until_cleanup_succeeds() {
        let registry = Arc::new(FlakyUnregisterRegistry::new(local_stream_registry()));
        let runtime = make_publisher_cleanup_runtime(registry.clone());

        let room_id = "104";
        let media_id = "204";

        runtime
            .register_and_start_ttl(&validated_publish(room_id, media_id, "307"))
            .await
            .expect("publish registration should succeed");

        registry.set_fail_unregister_if_epoch_matches_times(1);

        runtime.cleanup_on_unpublish(room_id, media_id).await;
        assert!(
            registry
                .is_stream_active(room_id, media_id)
                .await
                .expect("publisher activity lookup should succeed after failed cleanup"),
            "failed cleanup attempt should leave publisher registered for retry"
        );

        runtime.cleanup_on_unpublish(room_id, media_id).await;
        assert!(
            !registry
                .is_stream_active(room_id, media_id)
                .await
                .expect("publisher activity lookup should succeed after retry"),
            "retry must still have access to the fenced cleanup and remove the stale publisher"
        );
    }

    #[tokio::test]
    async fn test_publish_rollback_retry_preserves_fence_until_cleanup_succeeds() {
        let registry = Arc::new(FlakyUnregisterRegistry::new(local_stream_registry()));
        let runtime = make_publisher_cleanup_runtime(registry.clone());

        let room_id = "105";
        let media_id = "205";

        runtime
            .register_and_start_ttl(&validated_publish(room_id, media_id, "308"))
            .await
            .expect("publish registration should succeed");

        registry.set_fail_unregister_if_epoch_matches_times(1);

        runtime.cleanup_on_publish_rollback(room_id, media_id).await;
        assert!(
            registry
                .is_stream_active(room_id, media_id)
                .await
                .expect("publisher activity lookup should succeed after failed rollback"),
            "failed rollback attempt should leave publisher registered for retry"
        );

        runtime.cleanup_on_publish_rollback(room_id, media_id).await;
        assert!(
            !registry
                .is_stream_active(room_id, media_id)
                .await
                .expect("publisher activity lookup should succeed after rollback retry"),
            "rollback retry must still have access to the fenced cleanup and remove the stale publisher"
        );
    }

    #[tokio::test]
    async fn test_unpublish_without_in_memory_fence_does_not_guess_cleanup_target() {
        let registry = synctv_livestream::local_stream_registry();
        let runtime = make_publisher_cleanup_runtime(registry.clone());
        let restarted_runtime = make_publisher_cleanup_runtime(registry.clone());

        let room_id = "106";
        let media_id = "206";
        let user_id = "309";

        runtime
            .register_and_start_ttl(&validated_publish(room_id, media_id, user_id))
            .await
            .expect("publish registration should succeed");

        restarted_runtime
            .cleanup_on_unpublish(room_id, media_id)
            .await;

        assert!(
            registry
                .is_stream_active(room_id, media_id)
                .await
                .expect("publisher activity lookup should succeed after restarted unpublish"),
            "restarted unpublish must not guess a cleanup epoch from the live publisher"
        );
    }

    #[tokio::test]
    async fn test_publish_rollback_without_in_memory_fence_does_not_guess_cleanup_target() {
        let registry = synctv_livestream::local_stream_registry();
        let runtime = make_publisher_cleanup_runtime(registry.clone());
        let restarted_runtime = make_publisher_cleanup_runtime(registry.clone());

        let room_id = "107";
        let media_id = "207";
        let user_id = "310";

        runtime
            .register_and_start_ttl(&validated_publish(room_id, media_id, user_id))
            .await
            .expect("publish registration should succeed");

        restarted_runtime
            .cleanup_on_publish_rollback(room_id, media_id)
            .await;

        assert!(
            registry
                .is_stream_active(room_id, media_id)
                .await
                .expect("publisher activity lookup should succeed after restarted rollback"),
            "restarted rollback must not guess a cleanup epoch from the live publisher"
        );
    }

    #[tokio::test]
    async fn test_restarted_unpublish_does_not_remove_replacement_publisher() {
        let registry = synctv_livestream::local_stream_registry();
        let runtime = make_publisher_cleanup_runtime(registry.clone());
        let restarted_runtime = make_publisher_cleanup_runtime(registry.clone());

        let room_id = "108";
        let media_id = "208";
        let first_user_id = "311";
        let second_user_id = "312";

        runtime
            .register_and_start_ttl(&validated_publish(room_id, media_id, first_user_id))
            .await
            .expect("first publish registration should succeed");

        let first_epoch = registry
            .get_publisher(room_id, media_id)
            .await
            .expect("first publisher lookup should succeed")
            .expect("first publisher should exist")
            .epoch;

        registry
            .unregister_publisher_if_epoch_matches(room_id, media_id, first_epoch)
            .await
            .expect("test setup should remove first publisher");

        runtime
            .register_and_start_ttl(&validated_publish(room_id, media_id, second_user_id))
            .await
            .expect("second publish registration should succeed");

        restarted_runtime
            .cleanup_on_unpublish(room_id, media_id)
            .await;

        let current = registry
            .get_publisher(room_id, media_id)
            .await
            .expect("publisher lookup should succeed after restarted stale unpublish")
            .expect("restarted stale unpublish must not remove the replacement publisher");
        assert_eq!(current.user_id, second_user_id);
    }

    fn validated_publish(room_id: &str, media_id: &str, user_id: &str) -> ValidatedPublish {
        ValidatedPublish {
            room_id: room_id.parse().expect("numeric test room id"),
            media_id: media_id.parse().expect("numeric test media id"),
            user_id: user_id.parse().expect("numeric test user id"),
            auth_level: "test",
        }
    }

    #[test]
    fn test_media_creator_publish_authorization_requires_room_membership() {
        let user_id = UserId::expect_positive(301);

        assert!(!media_creator_publish_authorized(
            false,
            false,
            false,
            Some(&user_id),
            &user_id
        ));
        assert!(media_creator_publish_authorized(
            false,
            false,
            true,
            Some(&user_id),
            &user_id
        ));
    }

    fn make_publisher_cleanup_runtime(
        registry: Arc<dyn StreamRegistryTrait>,
    ) -> PublisherCleanupRuntime {
        PublisherCleanupRuntime::new(PublisherCleanupRuntimeConfig {
            user_stream_tracker: Arc::new(StreamTracker::new()),
            registry,
            node_id: "node-1".to_string(),
            api_address: "127.0.0.1:50051".to_string(),
            stream_event_tx: None,
            user_stream_index: Arc::new(LocalOnlyUserStreamIndex),
        })
    }

    #[test]
    fn test_validate_rtmp_room_state_rejects_closed_room() {
        let room = Room::new_with_status(
            "Closed room".to_string(),
            String::new(),
            UserId::expect_positive(1),
            RoomStatus::Closed,
        );
        let err =
            validate_rtmp_room_state(&room).expect_err("closed room must reject RTMP publish");
        assert!(
            matches!(err, RoomAccessRejection::Closed),
            "unexpected rejection: {err:?}"
        );
        assert_eq!(
            err.into_error("closed-room").to_string(),
            "Room closed-room is closed"
        );
    }

    #[test]
    fn test_validate_rtmp_room_state_rejects_banned_room() {
        let mut room = Room::new("Banned room".to_string(), UserId::expect_positive(1));
        room.ban();

        assert_eq!(
            validate_rtmp_room_state(&room),
            Err(RoomAccessRejection::Banned)
        );
    }

    struct TestRedisRuntime;

    #[async_trait::async_trait]
    impl RedisConnectionRuntime for TestRedisRuntime {
        async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
            panic!("test redis runtime snapshot should not be called in factory tests");
        }
    }

    #[test]
    fn test_user_stream_index_factory_uses_local_backend_without_shared_runtime() {
        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", false);

        let index = user_stream_index_from_shared_state_profile(&profile)
            .expect("local-only profile should build local RTMP index");

        assert!(!index.supports_cross_node_lookup());
    }

    #[test]
    fn test_user_stream_index_factory_keeps_standalone_mode_local_even_with_runtime() {
        let profile = SharedStateProfile::new(
            SharedStateMode::SharedBestEffort,
            Some(Arc::new(TestRedisRuntime)),
            "test:",
        );

        let index = user_stream_index_from_shared_state_profile(&profile)
            .expect("standalone profile should keep RTMP index local");

        assert!(!index.supports_cross_node_lookup());
    }

    #[test]
    fn test_user_stream_index_factory_requires_shared_runtime_in_cluster_mode() {
        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", true);

        let Err(error) = user_stream_index_from_shared_state_profile(&profile) else {
            panic!("cluster profile without runtime must be rejected");
        };

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared RTMP user stream index"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_user_stream_index_factory_uses_shared_backend_in_cluster_mode() {
        let profile = SharedStateProfile::new(
            SharedStateMode::SharedRequired,
            Some(Arc::new(TestRedisRuntime)),
            "test:",
        );

        let index = user_stream_index_from_shared_state_profile(&profile)
            .expect("cluster profile with runtime should build shared RTMP index");

        assert!(index.supports_cross_node_lookup());
    }

    #[test]
    fn test_stream_lifecycle_event_started_fields() {
        let event = StreamLifecycleEvent::Started {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            user_id: "user1".to_string(),
        };
        match event {
            StreamLifecycleEvent::Started {
                room_id,
                media_id,
                user_id,
            } => {
                assert_eq!(room_id, "room1");
                assert_eq!(media_id, "media1");
                assert_eq!(user_id, "user1");
            }
            other => unreachable!("Expected Started variant, got: {other:?}"),
        }
    }

    #[test]
    fn test_stream_lifecycle_event_stopped_fields() {
        let event = StreamLifecycleEvent::Stopped {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            user_id: "user1".to_string(),
        };
        match event {
            StreamLifecycleEvent::Stopped {
                room_id,
                media_id,
                user_id,
            } => {
                assert_eq!(room_id, "room1");
                assert_eq!(media_id, "media1");
                assert_eq!(user_id, "user1");
            }
            other => unreachable!("Expected Stopped variant, got: {other:?}"),
        }
    }
}
