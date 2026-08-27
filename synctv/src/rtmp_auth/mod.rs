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
//! On unpublish, the callback sends a generation-fenced stop command to
//! `PublisherManager`, then removes authentication tracking state.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use async_trait::async_trait;
use dashmap::DashMap;
use percent_encoding::percent_decode_str;
use synctv_adapter::PublicIdCodec;
use synctv_core::{
    models::{MediaId, Room, RoomId, RoomStatus, UserId, UserStatus},
    service::{RoomService, StreamingPublishKeyService, UserService},
    Error as CoreError, RedisConnectionRuntime, SharedStateMode, SharedStateProfile,
};
use synctv_livestream::{
    PublisherControlHandle, PublisherStopOutcome, PublisherStopRequest, StreamRegistryTrait,
    StreamTracker, PUBLISHER_TTL_SECS,
};
// TTL for the per-user rtmp:user_stream:{user_id} Redis key, matching the publisher TTL.
use synctv_xiu::rtmp::auth::{
    AuthCallback, AuthPublishRewrite, PublishAuthError, RtmpStreamMode as XiuRtmpStreamMode,
};
use synctv_xiu::streamhub::utils::Uuid;

const STREAMHUB_RESTARTING_MESSAGE: &str = "StreamHub is restarting, please retry in a few seconds";

fn map_publish_key_validation_error(error: CoreError) -> Box<dyn std::error::Error + Send + Sync> {
    match error {
        error @ CoreError::Authentication(_) => Box::new(PublishAuthError::new(format!(
            "Invalid stream key: {error}"
        ))),
        error => Box::new(error),
    }
}

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

#[async_trait]
trait PublisherStopControl: Send + Sync {
    async fn stop_publisher(
        &self,
        request: PublisherStopRequest,
    ) -> anyhow::Result<PublisherStopOutcome>;
}

#[async_trait]
impl PublisherStopControl for PublisherControlHandle {
    async fn stop_publisher(
        &self,
        request: PublisherStopRequest,
    ) -> anyhow::Result<PublisherStopOutcome> {
        PublisherControlHandle::stop_publisher(self, request).await
    }
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

#[derive(Debug, Clone)]
struct PendingPublishCleanup {
    lease_epoch: u64,
    user_id: UserId,
    room_id: String,
    media_id: String,
}

#[derive(Clone)]
struct PublisherCleanupRuntime {
    user_stream_tracker: Arc<StreamTracker>,
    registry: Arc<dyn StreamRegistryTrait>,
    node_id: String,
    cluster_address: String,
    user_stream_index: Arc<dyn UserStreamIndex>,
    publisher_stop_control: Arc<dyn PublisherStopControl>,
    pending_publish_cleanups: Arc<DashMap<Uuid, PendingPublishCleanup>>,
}

struct PublisherCleanupRuntimeConfig {
    user_stream_tracker: Arc<StreamTracker>,
    registry: Arc<dyn StreamRegistryTrait>,
    node_id: String,
    cluster_address: String,
    user_stream_index: Arc<dyn UserStreamIndex>,
    publisher_stop_control: Arc<dyn PublisherStopControl>,
}

impl PublisherCleanupRuntime {
    fn new(config: PublisherCleanupRuntimeConfig) -> Self {
        Self {
            user_stream_tracker: config.user_stream_tracker,
            registry: config.registry,
            node_id: config.node_id,
            cluster_address: config.cluster_address,
            user_stream_index: config.user_stream_index,
            publisher_stop_control: config.publisher_stop_control,
            pending_publish_cleanups: Arc::new(DashMap::new()),
        }
    }
}

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
    pub cluster_address: String,
    pub public_id_codec: Arc<PublicIdCodec>,
    pub is_restarting: Option<Arc<AtomicBool>>,
    pub user_stream_index: Arc<dyn UserStreamIndex>,
    pub publisher_control: PublisherControlHandle,
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
                cluster_address: config.cluster_address.clone(),
                user_stream_index: config.user_stream_index,
                publisher_stop_control: Arc::new(config.publisher_control),
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
        generation_id: Uuid,
        attempt: PendingPublishCleanup,
    ) {
        self.pending_publish_cleanups.insert(generation_id, attempt);
    }

    fn resolve_publish_cleanup(
        &self,
        generation_id: Uuid,
        room_id: &str,
        media_id: &str,
        context: &'static str,
    ) -> Option<PendingPublishCleanup> {
        let Some(attempt) = self.pending_publish_cleanups.get(&generation_id) else {
            tracing::warn!(
                generation_id = %generation_id,
                room_id = %room_id,
                media_id = %media_id,
                "Skipping {context} cleanup because the publication generation is unknown"
            );
            return None;
        };
        let attempt = attempt.clone();
        if attempt.room_id == room_id && attempt.media_id == media_id {
            return Some(attempt);
        }

        tracing::warn!(
            generation_id = %generation_id,
            room_id = %room_id,
            media_id = %media_id,
            expected_room_id = %attempt.room_id,
            expected_media_id = %attempt.media_id,
            "Skipping {context} cleanup because the publication identity does not match"
        );

        None
    }

    async fn lookup_registered_epoch(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: Uuid,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let generation = self
            .registry
            .get_active_generation(room_id, media_id)
            .await
            .map_err(|e| format!("Failed to fetch publisher lease_epoch after registration: {e}"))?
            .ok_or_else(|| -> Box<dyn std::error::Error + Send + Sync> {
                format!(
                    "Publisher registration disappeared before lease_epoch capture: room={room_id}, media={media_id}"
                )
                .into()
            })?;

        if generation.generation_id != generation_id.to_string() {
            return Err(format!(
                "Publisher generation changed before lease_epoch capture: room={room_id}, media={media_id}, expected={generation_id}, actual={}",
                generation.generation_id
            )
            .into());
        }

        Ok(generation.lease_epoch)
    }

    async fn cleanup_publisher_if_current_attempt(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: Uuid,
        expected_lease_epoch: u64,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let current = self
            .registry
            .get_active_generation(room_id, media_id)
            .await
            .map_err(|e| format!("Failed to inspect current publisher before cleanup: {e}"))?;

        let Some(current) = current else {
            return Ok(true);
        };

        if current.generation_id != generation_id.to_string()
            || current.lease_epoch != expected_lease_epoch
        {
            return Ok(false);
        }

        self.registry
            .deactivate_generation_if_lease_matches(
                room_id,
                media_id,
                &generation_id.to_string(),
                expected_lease_epoch,
            )
            .await
            .map_err(|e| format!("Failed to unregister publisher with lease_epoch fence: {e}"))?;

        let remaining = self
            .registry
            .get_active_generation(room_id, media_id)
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
        generation_id: Uuid,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let registered = self
            .registry
            .try_activate_generation(
                &validated.room_id.to_string(),
                &validated.media_id.to_string(),
                &self.node_id,
                &validated.user_id.to_string(),
                &self.cluster_address,
                &generation_id.to_string(),
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
                generation_id,
            )
            .await?;

        tracing::info!(
            "Publisher authenticated and registered: user={}, room={}, media={}, node={}, auth={}, lease_epoch={}",
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
                .deactivate_generation_if_lease_matches(
                    &validated.room_id.to_string(),
                    &validated.media_id.to_string(),
                    &generation_id.to_string(),
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

        self.remember_pending_publish_cleanup(
            generation_id,
            PendingPublishCleanup {
                lease_epoch: registered_epoch,
                user_id: validated.user_id,
                room_id: validated.room_id.to_string(),
                media_id: validated.media_id.to_string(),
            },
        );

        Ok(())
    }

    async fn cleanup_on_unpublish(&self, generation_id: Uuid, room_id: &str, media_id: &str) {
        let Some(attempt) =
            self.resolve_publish_cleanup(generation_id, room_id, media_id, "on_unpublish")
        else {
            return;
        };
        let stop_request = PublisherStopRequest::new(
            room_id,
            media_id,
            generation_id.to_string(),
            attempt.lease_epoch,
        );
        match self
            .publisher_stop_control
            .stop_publisher(stop_request)
            .await
        {
            Ok(outcome) => {
                tracing::info!(
                    generation_id = %generation_id,
                    lease_epoch = attempt.lease_epoch,
                    room_id,
                    media_id,
                    ?outcome,
                    "Publisher stop committed after RTMP unpublish"
                );
            }
            Err(error) => {
                tracing::error!(
                    generation_id = %generation_id,
                    lease_epoch = attempt.lease_epoch,
                    room_id,
                    media_id,
                    %error,
                    "Failed to commit publisher stop after RTMP unpublish"
                );
            }
        }
        self.pending_publish_cleanups.remove(&generation_id);
        let tracked_user = self
            .user_stream_tracker
            .get_stream_user(room_id, media_id)
            .filter(|tracked_user| tracked_user == &attempt.user_id.to_string())
            .and_then(|_| self.user_stream_tracker.remove_stream(room_id, media_id));
        tracing::info!(
            generation_id = %generation_id,
            user_id = %attempt.user_id,
            room_id = %room_id,
            media_id = %media_id,
            had_tracker_entry = tracked_user.is_some(),
            "Publisher authentication state cleaned after unpublish"
        );

        self.delete_user_stream_key(attempt.user_id, "unpublish")
            .await;
    }

    async fn cleanup_on_publish_rollback(
        &self,
        generation_id: Uuid,
        room_id: &str,
        media_id: &str,
    ) {
        tracing::warn!(
            room_id = %room_id,
            media_id = %media_id,
            "Rolling back publisher registration due to StreamHub failure"
        );

        let Some(attempt) =
            self.resolve_publish_cleanup(generation_id, room_id, media_id, "rollback")
        else {
            return;
        };

        let should_cleanup = match self
            .cleanup_publisher_if_current_attempt(
                room_id,
                media_id,
                generation_id,
                attempt.lease_epoch,
            )
            .await
        {
            Ok(should_cleanup) => should_cleanup,
            Err(e) => {
                tracing::warn!(
                    room_id = %room_id,
                    media_id = %media_id,
                    lease_epoch = attempt.lease_epoch,
                    error = %e,
                    "Failed to rollback publisher registration with lease_epoch fence; keeping pending cleanup for retry"
                );
                return;
            }
        };

        if !should_cleanup {
            self.pending_publish_cleanups.remove(&generation_id);
            tracing::info!(
                room_id = %room_id,
                media_id = %media_id,
                lease_epoch = attempt.lease_epoch,
                "Ignoring stale rollback cleanup for superseded publisher lease_epoch"
            );
            return;
        }

        self.pending_publish_cleanups.remove(&generation_id);
        if self
            .user_stream_tracker
            .get_stream_user(room_id, media_id)
            .is_some_and(|tracked_user| tracked_user == attempt.user_id.to_string())
        {
            let _ = self.user_stream_tracker.remove_stream(room_id, media_id);
        }
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
        generation_id: Uuid,
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

        // Phase 2: Reserve publisher ownership and track cleanup state.
        self.publisher_cleanup
            .register_and_start_ttl(&validated, generation_id)
            .await?;

        if let Err(error) = self.revalidate_registered_publish(&validated).await {
            self.publisher_cleanup
                .cleanup_on_publish_rollback(
                    generation_id,
                    &validated.room_id.to_string(),
                    &validated.media_id.to_string(),
                )
                .await;
            return Err(error);
        }

        // Phase 3: Return rewrite so StreamHub uses canonical (room_id, media_id)
        // instead of the raw RTMP identifiers (room_id, JWT_TOKEN).
        Ok(Some(AuthPublishRewrite {
            app_name: validated.room_id.to_string(),
            stream_name: validated.media_id.to_string(),
            media_mode: match validated.media_mode {
                synctv_core::models::RtmpStreamMode::Default => XiuRtmpStreamMode::Default,
                synctv_core::models::RtmpStreamMode::VideoOnly => XiuRtmpStreamMode::VideoOnly,
                synctv_core::models::RtmpStreamMode::AudioOnly => XiuRtmpStreamMode::AudioOnly,
            },
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

    async fn on_unpublish(
        &self,
        generation_id: Uuid,
        app_name: &str,
        stream_name: &str,
        _query: Option<&str>,
    ) {
        self.publisher_cleanup
            .cleanup_on_unpublish(generation_id, app_name, stream_name)
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
    async fn on_publish_rollback(
        &self,
        generation_id: Uuid,
        app_name: &str,
        stream_name: &str,
        _query: Option<&str>,
    ) {
        self.publisher_cleanup
            .cleanup_on_publish_rollback(generation_id, app_name, stream_name)
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
    media_mode: synctv_core::models::RtmpStreamMode,
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
            PublishAuthError::new(
                "Missing token query parameter; RTMP publish must use ?token=<publish_key>",
            )
        })?;

        // Validate JWT stream_key
        let claims = self
            .publish_key_service
            .validate_publish_key_for_stream_claims(token, &expected_room_id, &expected_media_id)
            .await
            .map_err(map_publish_key_validation_error)?;

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
        let (auth_level, media_mode) = self
            .check_publish_authorization(&user, &expected_room_id, &expected_media_id, &user_id)
            .await?;

        Ok(ValidatedPublish {
            room_id: expected_room_id,
            media_id: expected_media_id,
            user_id,
            auth_level,
            media_mode,
        })
    }

    /// Check that the user has permission to publish to this room/media.
    /// Returns the authorization level and RTMP media filtering policy.
    async fn check_publish_authorization(
        &self,
        user: &synctv_core::models::User,
        room_id: &RoomId,
        media_id: &MediaId,
        user_id: &UserId,
    ) -> Result<
        (&'static str, synctv_core::models::RtmpStreamMode),
        Box<dyn std::error::Error + Send + Sync>,
    > {
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
            .ok_or_else(|| format!("Media {media_id} not found"))?;
        if media.room_id != *room_id {
            return Err(format!("Media {media_id} does not belong to room {room_id}").into());
        }
        self.room_service
            .ensure_client_usable_media(&media)
            .await
            .map_err(|error| format!("Media is unavailable: {error}"))?;

        let is_media_creator = if !is_global_admin && !is_room_admin_or_creator {
            if room_member.is_none() {
                return Err(format!(
                    "Insufficient permissions to publish: user {user_id} is not a member of room {room_id}"
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
                "Insufficient permissions to publish: user {user_id} is not admin, room admin/creator, or media creator"
            ).into());
        }

        let auth_level = if is_global_admin {
            "global_admin"
        } else if is_room_admin_or_creator {
            "room_admin"
        } else {
            "media_creator"
        };
        let media_mode = match &media.source_config {
            synctv_core::models::MediaSourceConfig::Rtmp(config) => config.mode,
            _ => synctv_core::models::RtmpStreamMode::Default,
        };
        Ok((auth_level, media_mode))
    }

    async fn revalidate_registered_publish(
        &self,
        validated: &ValidatedPublish,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let room = self
            .room_service
            .get_room(&validated.room_id)
            .await
            .map_err(|error| {
                format!("Failed to reload room after publisher registration: {error}")
            })?;
        validate_rtmp_room_state(&room)
            .map_err(|reason| reason.into_error(&validated.room_id.to_string()))?;

        let user = self
            .user_service
            .get_user(&validated.user_id)
            .await
            .map_err(|error| {
                format!("Failed to reload publisher after publisher registration: {error}")
            })?;
        if user.status == UserStatus::Banned || user.deleted_at.is_some() {
            return Err(format!(
                "User {} became unavailable during publisher registration",
                validated.user_id
            )
            .into());
        }

        self.check_publish_authorization(
            &user,
            &validated.room_id,
            &validated.media_id,
            &validated.user_id,
        )
        .await?;
        Ok(())
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
        local_stream_registry, ActiveStreamGeneration, LeaseRefreshOutcome, StreamGeneration,
        StreamRegistryTrait,
    };

    #[test]
    fn publish_key_error_mapping_only_marks_authentication_failures() {
        let authentication = map_publish_key_validation_error(CoreError::Authentication(
            "publish key expired".to_string(),
        ));
        assert!(authentication.downcast_ref::<PublishAuthError>().is_some());

        for error in [
            CoreError::Authorization("publish key has insufficient scope".to_string()),
            CoreError::Internal("publish-key store unavailable".to_string()),
        ] {
            let mapped = map_publish_key_validation_error(error);
            assert!(mapped.downcast_ref::<PublishAuthError>().is_none());
            assert!(mapped.downcast_ref::<CoreError>().is_some());
        }
    }

    struct FlakyUnregisterRegistry {
        inner: Arc<dyn StreamRegistryTrait>,
        fail_unregister_if_lease_matches_times: AtomicUsize,
    }

    impl FlakyUnregisterRegistry {
        fn new(inner: Arc<dyn StreamRegistryTrait>) -> Self {
            Self {
                inner,
                fail_unregister_if_lease_matches_times: AtomicUsize::new(0),
            }
        }

        fn set_fail_unregister_if_lease_matches_times(&self, times: usize) {
            self.fail_unregister_if_lease_matches_times
                .store(times, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl StreamRegistryTrait for FlakyUnregisterRegistry {
        async fn try_activate_generation(
            &self,
            room_id: &str,
            media_id: &str,
            node_id: &str,
            user_id: &str,
            cluster_address: &str,
            generation_id: &str,
        ) -> anyhow::Result<bool> {
            self.inner
                .try_activate_generation(
                    room_id,
                    media_id,
                    node_id,
                    user_id,
                    cluster_address,
                    generation_id,
                )
                .await
        }

        async fn refresh_generation_lease(
            &self,
            room_id: &str,
            media_id: &str,
            generation_id: &str,
            user_id: &str,
            node_id: &str,
            expected_lease_epoch: u64,
        ) -> anyhow::Result<LeaseRefreshOutcome> {
            self.inner
                .refresh_generation_lease(
                    room_id,
                    media_id,
                    generation_id,
                    user_id,
                    node_id,
                    expected_lease_epoch,
                )
                .await
        }

        async fn mark_generation_ready(
            &self,
            room_id: &str,
            media_id: &str,
            generation_id: &str,
            expected_lease_epoch: u64,
        ) -> anyhow::Result<bool> {
            self.inner
                .mark_generation_ready(room_id, media_id, generation_id, expected_lease_epoch)
                .await
        }

        async fn deactivate_current_generation(
            &self,
            room_id: &str,
            media_id: &str,
        ) -> anyhow::Result<()> {
            self.inner
                .deactivate_current_generation(room_id, media_id)
                .await
        }

        async fn deactivate_generation_if_lease_matches(
            &self,
            room_id: &str,
            media_id: &str,
            generation_id: &str,
            expected_lease_epoch: u64,
        ) -> anyhow::Result<bool> {
            let remaining_failures = self
                .fail_unregister_if_lease_matches_times
                .load(Ordering::SeqCst);
            if remaining_failures > 0 {
                self.fail_unregister_if_lease_matches_times
                    .fetch_sub(1, Ordering::SeqCst);
                return Err(anyhow::anyhow!(
                    "simulated Redis failure in deactivate_generation_if_lease_matches"
                ));
            }

            self.inner
                .deactivate_generation_if_lease_matches(
                    room_id,
                    media_id,
                    generation_id,
                    expected_lease_epoch,
                )
                .await
        }

        async fn get_active_generation(
            &self,
            room_id: &str,
            media_id: &str,
        ) -> anyhow::Result<Option<StreamGeneration>> {
            self.inner.get_active_generation(room_id, media_id).await
        }

        async fn get_generation(
            &self,
            room_id: &str,
            media_id: &str,
            generation_id: &str,
        ) -> anyhow::Result<Option<StreamGeneration>> {
            self.inner
                .get_generation(room_id, media_id, generation_id)
                .await
        }

        async fn is_stream_active(&self, room_id: &str, media_id: &str) -> anyhow::Result<bool> {
            self.inner.is_stream_active(room_id, media_id).await
        }

        async fn list_active_generations(&self) -> anyhow::Result<Vec<ActiveStreamGeneration>> {
            self.inner.list_active_generations().await
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

        async fn validate_lease(
            &self,
            room_id: &str,
            media_id: &str,
            generation_id: &str,
            lease_epoch: u64,
        ) -> anyhow::Result<bool> {
            self.inner
                .validate_lease(room_id, media_id, generation_id, lease_epoch)
                .await
        }

        async fn cleanup_all_generations_for_node(&self, node_id: &str) -> anyhow::Result<()> {
            self.inner.cleanup_all_generations_for_node(node_id).await
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
    async fn unpublish_commits_registry_stop_and_cleans_auth_state() {
        let registry = local_stream_registry();
        let runtime = make_publisher_cleanup_runtime(registry.clone());
        let generation_id = Uuid::new();

        runtime
            .register_and_start_ttl(&validated_publish("101", "201", "301"), generation_id)
            .await
            .expect("publish registration should succeed");
        runtime
            .cleanup_on_unpublish(generation_id, "101", "201")
            .await;

        assert!(!registry
            .is_stream_active("101", "201")
            .await
            .expect("publisher activity lookup should succeed"));
        assert_eq!(
            runtime.user_stream_tracker.get_stream_user("101", "201"),
            None
        );
        assert!(!runtime
            .pending_publish_cleanups
            .contains_key(&generation_id));
    }

    #[tokio::test]
    async fn unpublish_then_manager_cleanup_keeps_exact_generation() {
        let registry = local_stream_registry();
        let runtime = make_publisher_cleanup_runtime(registry.clone());
        let generation_id = Uuid::new();

        runtime
            .register_and_start_ttl(&validated_publish("102", "202", "302"), generation_id)
            .await
            .expect("publish registration should succeed");
        let lease_epoch = registry
            .get_active_generation("102", "202")
            .await
            .expect("publisher lookup should succeed")
            .expect("publisher should exist")
            .lease_epoch;

        runtime
            .cleanup_on_unpublish(generation_id, "102", "202")
            .await;
        registry
            .deactivate_generation_preserving_hls_if_lease_matches(
                "102",
                "202",
                &generation_id.to_string(),
                lease_epoch,
            )
            .await
            .expect("PublisherManager cleanup should succeed");

        let ended = registry
            .get_generation("102", "202", &generation_id.to_string())
            .await
            .expect("ended route lookup should succeed")
            .expect("exact ended route should remain");
        assert_eq!(ended.generation_id, generation_id.to_string());
    }

    #[tokio::test]
    async fn manager_cleanup_then_unpublish_keeps_exact_generation() {
        let registry = local_stream_registry();
        let runtime = make_publisher_cleanup_runtime(registry.clone());
        let generation_id = Uuid::new();

        runtime
            .register_and_start_ttl(&validated_publish("103", "203", "303"), generation_id)
            .await
            .expect("publish registration should succeed");
        let lease_epoch = registry
            .get_active_generation("103", "203")
            .await
            .expect("publisher lookup should succeed")
            .expect("publisher should exist")
            .lease_epoch;

        registry
            .deactivate_generation_preserving_hls_if_lease_matches(
                "103",
                "203",
                &generation_id.to_string(),
                lease_epoch,
            )
            .await
            .expect("PublisherManager cleanup should succeed");
        runtime
            .cleanup_on_unpublish(generation_id, "103", "203")
            .await;

        let ended = registry
            .get_generation("103", "203", &generation_id.to_string())
            .await
            .expect("ended route lookup should succeed")
            .expect("exact ended route should remain");
        assert_eq!(ended.generation_id, generation_id.to_string());
        assert_eq!(
            runtime.user_stream_tracker.get_stream_user("103", "203"),
            None
        );
    }

    #[tokio::test]
    async fn delayed_unpublish_only_cleans_its_exact_generation() {
        let registry = local_stream_registry();
        let runtime = make_publisher_cleanup_runtime(registry.clone());
        let old_generation_id = Uuid::new();
        let new_generation_id = Uuid::new();

        runtime
            .register_and_start_ttl(&validated_publish("104", "204", "304"), old_generation_id)
            .await
            .expect("old publish registration should succeed");
        let old_epoch = registry
            .get_active_generation("104", "204")
            .await
            .expect("old publisher lookup should succeed")
            .expect("old publisher should exist")
            .lease_epoch;
        registry
            .deactivate_generation_if_lease_matches(
                "104",
                "204",
                &old_generation_id.to_string(),
                old_epoch,
            )
            .await
            .expect("test setup should remove old publisher");
        runtime
            .register_and_start_ttl(&validated_publish("104", "204", "305"), new_generation_id)
            .await
            .expect("replacement registration should succeed");

        runtime
            .cleanup_on_unpublish(old_generation_id, "104", "204")
            .await;

        let current = registry
            .get_active_generation("104", "204")
            .await
            .expect("publisher lookup should succeed")
            .expect("replacement publisher should remain");
        assert_eq!(current.generation_id, new_generation_id.to_string());
        assert_eq!(
            runtime.user_stream_tracker.get_stream_user("104", "204"),
            Some("305".to_string())
        );
        assert!(runtime
            .pending_publish_cleanups
            .contains_key(&new_generation_id));
    }

    #[tokio::test]
    async fn delayed_rollback_does_not_remove_newer_registration() {
        let registry = local_stream_registry();
        let runtime = make_publisher_cleanup_runtime(registry.clone());
        let old_generation_id = Uuid::new();
        let new_generation_id = Uuid::new();

        runtime
            .register_and_start_ttl(&validated_publish("105", "205", "306"), old_generation_id)
            .await
            .expect("old publish registration should succeed");
        let old_epoch = registry
            .get_active_generation("105", "205")
            .await
            .expect("old publisher lookup should succeed")
            .expect("old publisher should exist")
            .lease_epoch;
        registry
            .deactivate_generation_if_lease_matches(
                "105",
                "205",
                &old_generation_id.to_string(),
                old_epoch,
            )
            .await
            .expect("test setup should remove old publisher");
        runtime
            .register_and_start_ttl(&validated_publish("105", "205", "307"), new_generation_id)
            .await
            .expect("replacement registration should succeed");

        runtime
            .cleanup_on_publish_rollback(old_generation_id, "105", "205")
            .await;

        let current = registry
            .get_active_generation("105", "205")
            .await
            .expect("publisher lookup should succeed")
            .expect("replacement publisher should remain");
        assert_eq!(current.generation_id, new_generation_id.to_string());
        assert!(runtime
            .pending_publish_cleanups
            .contains_key(&new_generation_id));
    }

    #[tokio::test]
    async fn publish_rollback_retry_preserves_exact_generation_fence() {
        let registry = Arc::new(FlakyUnregisterRegistry::new(local_stream_registry()));
        let runtime = make_publisher_cleanup_runtime(registry.clone());
        let generation_id = Uuid::new();

        runtime
            .register_and_start_ttl(&validated_publish("106", "206", "308"), generation_id)
            .await
            .expect("publish registration should succeed");
        registry.set_fail_unregister_if_lease_matches_times(1);

        runtime
            .cleanup_on_publish_rollback(generation_id, "106", "206")
            .await;
        assert!(registry
            .is_stream_active("106", "206")
            .await
            .expect("publisher activity lookup should succeed"));
        assert!(runtime
            .pending_publish_cleanups
            .contains_key(&generation_id));

        runtime
            .cleanup_on_publish_rollback(generation_id, "106", "206")
            .await;
        assert!(!registry
            .is_stream_active("106", "206")
            .await
            .expect("publisher activity lookup should succeed"));
        assert!(!runtime
            .pending_publish_cleanups
            .contains_key(&generation_id));
    }

    #[tokio::test]
    async fn unknown_generation_callbacks_leave_current_publisher_untouched() {
        let registry = local_stream_registry();
        let runtime = make_publisher_cleanup_runtime(registry.clone());
        let generation_id = Uuid::new();
        let unknown_generation_id = Uuid::new();

        runtime
            .register_and_start_ttl(&validated_publish("107", "207", "309"), generation_id)
            .await
            .expect("publish registration should succeed");
        runtime
            .cleanup_on_unpublish(unknown_generation_id, "107", "207")
            .await;
        runtime
            .cleanup_on_publish_rollback(unknown_generation_id, "107", "207")
            .await;

        let current = registry
            .get_active_generation("107", "207")
            .await
            .expect("publisher lookup should succeed")
            .expect("current publisher should remain");
        assert_eq!(current.generation_id, generation_id.to_string());
        assert_eq!(
            runtime.user_stream_tracker.get_stream_user("107", "207"),
            Some("309".to_string())
        );
        assert!(runtime
            .pending_publish_cleanups
            .contains_key(&generation_id));
    }

    fn validated_publish(room_id: &str, media_id: &str, user_id: &str) -> ValidatedPublish {
        ValidatedPublish {
            room_id: room_id.parse().expect("numeric test room id"),
            media_id: media_id.parse().expect("numeric test media id"),
            user_id: user_id.parse().expect("numeric test user id"),
            auth_level: "test",
            media_mode: synctv_core::models::RtmpStreamMode::Default,
        }
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn registered_publish_is_rolled_back_when_media_creator_becomes_banned() {
        use synctv_core::{
            models::{FromProviderParams, Media, SignupMethod, SourceProvider, User},
            repository::{MediaRepository, UserRepository},
            service::PublishKeyService,
        };
        use synctv_core_testing::{
            create_test_jwt_service, create_test_pool, create_test_room_service,
            create_test_user_service, direct_url_media_source_config,
        };

        let (_postgres, pool) = create_test_pool().await;
        let user_repository = UserRepository::new(pool.clone());
        let owner = user_repository
            .create(&User::new(
                "rtmp_race_owner".to_string(),
                SignupMethod::AdminCreated,
            ))
            .await
            .expect("room owner should be created");
        let creator = user_repository
            .create(&User::new(
                "rtmp_race_creator".to_string(),
                SignupMethod::AdminCreated,
            ))
            .await
            .expect("media creator should be created");

        let room_service = Arc::new(create_test_room_service(pool.clone()));
        let room = room_service
            .create_room(
                "rtmp lifecycle race".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room should be created");
        room_service
            .join_room(room.id, creator.id, None)
            .await
            .expect("media creator should join the room");
        let media = MediaRepository::new(pool.clone())
            .create(&Media::from_provider_with_params(FromProviderParams {
                playlist_id: None,
                room_id: room.id,
                creator_id: Some(creator.id),
                name: "creator-owned live media".to_string(),
                description: String::new(),
                source_provider: SourceProvider::DirectUrl,
                source_config: direct_url_media_source_config("https://example.com/live.m3u8"),
                provider_instance_name: None,
                position: 0.0,
            }))
            .await
            .expect("media should be created");

        let registry = local_stream_registry();
        let auth = SyncTvRtmpAuth {
            room_service: room_service.clone(),
            user_service: Arc::new(create_test_user_service(pool)),
            publish_key_service: Arc::new(
                PublishKeyService::new(
                    create_test_jwt_service(),
                    Arc::new(synctv_core::SystemClock),
                    24,
                )
                .expect("publish key service should be created"),
            ),
            publisher_cleanup: make_publisher_cleanup_runtime(registry.clone()),
            public_id_codec: Arc::new(PublicIdCodec::plain()),
            is_restarting: None,
        };
        let validated = ValidatedPublish {
            room_id: room.id,
            media_id: media.id,
            user_id: owner.id,
            auth_level: "room_admin",
            media_mode: synctv_core::models::RtmpStreamMode::Default,
        };
        let generation_id = Uuid::new();
        auth.publisher_cleanup
            .register_and_start_ttl(&validated, generation_id)
            .await
            .expect("publisher registration should succeed");
        auth.revalidate_registered_publish(&validated)
            .await
            .expect("publisher should initially remain authorized");

        room_service
            .ban_user_and_reset_owned_playback_with_outbox(
                &creator.id,
                None,
                Some("race test".to_string()),
                None,
                &[],
            )
            .await
            .expect("media creator should be banned");

        let error = auth
            .revalidate_registered_publish(&validated)
            .await
            .expect_err("registered publisher must fail when its media creator is banned");
        assert!(error.to_string().contains("Media is unavailable"));
        auth.publisher_cleanup
            .cleanup_on_publish_rollback(generation_id, &room.id.to_string(), &media.id.to_string())
            .await;
        assert!(!registry
            .is_stream_active(&room.id.to_string(), &media.id.to_string())
            .await
            .expect("publisher activity lookup should succeed"));
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

    struct RegistryPublisherStopControl {
        registry: Arc<dyn StreamRegistryTrait>,
    }

    #[async_trait]
    impl PublisherStopControl for RegistryPublisherStopControl {
        async fn stop_publisher(
            &self,
            request: PublisherStopRequest,
        ) -> anyhow::Result<PublisherStopOutcome> {
            let current = self
                .registry
                .get_active_generation(&request.room_id, &request.media_id)
                .await?;
            let Some(current) = current else {
                return Ok(PublisherStopOutcome::AlreadyStopped);
            };
            if current.generation_id != request.generation_id
                || current.lease_epoch != request.lease_epoch
            {
                return Ok(PublisherStopOutcome::Superseded);
            }
            let stopped = self
                .registry
                .deactivate_generation_preserving_hls_if_lease_matches(
                    &request.room_id,
                    &request.media_id,
                    &request.generation_id,
                    request.lease_epoch,
                )
                .await?;
            Ok(if stopped {
                PublisherStopOutcome::Stopped
            } else {
                PublisherStopOutcome::AlreadyStopped
            })
        }
    }

    fn make_publisher_cleanup_runtime(
        registry: Arc<dyn StreamRegistryTrait>,
    ) -> PublisherCleanupRuntime {
        PublisherCleanupRuntime::new(PublisherCleanupRuntimeConfig {
            user_stream_tracker: Arc::new(StreamTracker::new()),
            registry: registry.clone(),
            node_id: "node-1".to_string(),
            cluster_address: "127.0.0.1:50051".to_string(),
            user_stream_index: Arc::new(LocalOnlyUserStreamIndex),
            publisher_stop_control: Arc::new(RegistryPublisherStopControl { registry }),
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
}
