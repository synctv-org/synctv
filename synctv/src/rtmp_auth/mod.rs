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
use synctv_livestream::api::StreamTracker;
use synctv_livestream::relay::StreamRegistryTrait;
use synctv_livestream::AuthCallback;
use tokio::sync::RwLock;
// TTL for the per-user rtmp:user_stream:{user_id} Redis key, matching the publisher TTL.
use synctv_livestream::relay::registry::PUBLISHER_TTL_SECS;

use synctv_core::{
    models::{MediaId, Room, RoomStatus, UserId, UserStatus},
    service::{PublishKeyService, RoomService, UserService},
};

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

#[derive(Debug, Clone)]
struct PendingPublishCleanup {
    epoch: u64,
    user_id: String,
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
    publish_key_service: Arc<PublishKeyService>,
    user_stream_tracker: Arc<StreamTracker>,
    /// Publisher registry (Redis) for single-publisher-per-media enforcement
    registry: Arc<dyn StreamRegistryTrait>,
    /// This node's unique identifier for publisher registration
    node_id: String,
    /// Advertised shared API address for cross-node proxying (e.g., "10.0.0.1:8080")
    api_address: String,
    /// Broadcast channel for stream lifecycle events (StreamStarted/StreamStopped)
    stream_event_tx: Option<tokio::sync::broadcast::Sender<StreamLifecycleEvent>>,
    /// Redis key prefix from config (e.g., "synctv:") for multi-instance isolation
    key_prefix: String,
    /// Optional shared restart flag from LivestreamServer. When set, new
    /// publications are rejected during the StreamHub cleanup/re-register window.
    is_restarting: Option<Arc<AtomicBool>>,
    /// Optional Redis connection for cross-replica `user_id → stream_key` mapping.
    ///
    /// When set, each successful publish auth additionally writes:
    ///   `SET {key_prefix}rtmp:user_stream:{user_id} {room_id}|{media_id}`
    /// with a per-key TTL matching the publisher TTL.  This allows any replica to
    /// resolve which stream a user is publishing without querying the local
    /// in-memory tracker (which is only populated on the replica that authenticated
    /// the publisher). Each user gets an individual TTL instead of sharing a hash
    /// where EXPIRE would reset the TTL for all users on every write.
    ///
    /// On unpublish, the key is removed: `DEL {key_prefix}rtmp:user_stream:{user_id}`.
    redis_conn: Option<Arc<RwLock<redis::aio::ConnectionManager>>>,
    /// Epochs captured after successful auth-phase registration and used to fence
    /// later unpublish/rollback cleanup for the same logical stream.
    pending_publish_cleanups: Arc<DashMap<(String, String), VecDeque<PendingPublishCleanup>>>,
}

impl SyncTvRtmpAuth {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        room_service: Arc<RoomService>,
        user_service: Arc<UserService>,
        publish_key_service: Arc<PublishKeyService>,
        user_stream_tracker: Arc<StreamTracker>,
        registry: Arc<dyn StreamRegistryTrait>,
        node_id: String,
        api_address: String,
        stream_event_tx: Option<tokio::sync::broadcast::Sender<StreamLifecycleEvent>>,
        key_prefix: String,
    ) -> Self {
        Self {
            room_service,
            user_service,
            publish_key_service,
            user_stream_tracker,
            registry,
            node_id,
            api_address,
            stream_event_tx,
            key_prefix,
            is_restarting: None,
            redis_conn: None,
            pending_publish_cleanups: Arc::new(DashMap::new()),
        }
    }

    /// Build a per-user Redis key for user stream mapping, including the configured prefix.
    ///
    /// Each user gets their own key (`{prefix}rtmp:user_stream:{user_id}`) with an
    /// individual TTL, instead of sharing a single hash where EXPIRE resets TTL for
    /// all users on every write.
    fn user_stream_key(&self, user_id: &str) -> String {
        format!("{}rtmp:user_stream:{}", self.key_prefix, user_id)
    }

    /// Attach a Redis connection for cross-replica user→stream mapping.
    ///
    /// Call this after construction when a Redis connection is available.
    /// If not called, cross-replica user→stream lookup falls back to the
    /// publisher registry's reverse index (`stream:user_publishers:{user_id}`).
    #[must_use]
    pub fn with_redis(mut self, conn: Arc<RwLock<redis::aio::ConnectionManager>>) -> Self {
        self.redis_conn = Some(conn);
        self
    }

    async fn redis_conn_snapshot(&self) -> Option<redis::aio::ConnectionManager> {
        match &self.redis_conn {
            Some(conn) => Some(conn.read().await.clone()),
            None => None,
        }
    }

    /// Reject new RTMP publications while StreamHub is restarting.
    #[must_use]
    pub fn with_restarting_flag(mut self, is_restarting: Arc<AtomicBool>) -> Self {
        self.is_restarting = Some(is_restarting);
        self
    }

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

    async fn delete_user_stream_key(&self, user_id: &str, context: &'static str) {
        if user_id.is_empty() {
            return;
        }
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let key = self.user_stream_key(user_id);
            let result: Result<(), redis::RedisError> =
                redis::cmd("DEL").arg(&key).query_async(&mut conn).await;
            if let Err(e) = result {
                tracing::warn!(
                    user_id = %user_id,
                    "Failed to remove rtmp:user_stream entry on {} (non-fatal): {}",
                    context,
                    e
                );
            }
        }
    }
}

#[async_trait]
impl AuthCallback for SyncTvRtmpAuth {
    async fn on_publish(
        &self,
        app_name: &str,
        stream_name: &str,
        query: Option<&str>,
    ) -> Result<
        Option<synctv_livestream::rtmp_auth::AuthPublishRewrite>,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        if self
            .is_restarting
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            tracing::warn!(
                room_id = %app_name,
                "RTMP publish rejected: StreamHub is restarting"
            );
            return Err("StreamHub is restarting, please retry in a few seconds".into());
        }

        // Phase 1: Validate room, token, user status, and authorization
        let validated = self
            .validate_publish_request(app_name, stream_name, query)
            .await?;

        // Phase 2: Register in Redis, track mapping, emit event, spawn TTL renewal
        self.register_and_start_ttl(&validated, app_name, stream_name)
            .await?;

        // Phase 3: Return rewrite so StreamHub uses canonical (room_id, media_id)
        // instead of the raw RTMP identifiers (room_id, JWT_TOKEN).
        Ok(Some(synctv_livestream::rtmp_auth::AuthPublishRewrite {
            app_name: validated.room_id,
            stream_name: validated.media_id,
        }))
    }

    /// RTMP pull (play) authorization based on room settings and status.
    ///
    /// By default, RTMP play is disabled (`rtmp_player` = false) because:
    /// - RTMP has no authentication mechanism
    /// - Viewers should use HTTP-FLV or HLS endpoints which enforce JWT + room membership auth
    ///
    /// Room admins can enable RTMP play by setting `rtmp_player = true` if they understand
    /// the security implications (anyone with the RTMP URL can watch).
    ///
    /// This method also validates:
    /// - Room is not banned
    /// - Room status is not Pending or Closed
    async fn on_play(
        &self,
        app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.validate_play_request(app_name).await
    }

    async fn on_unplay(&self, app_name: &str, _stream_name: &str, _query: Option<&str>) {
        tracing::info!(
            room_id = %app_name,
            "RTMP player disconnected"
        );
    }

    async fn on_unpublish(&self, app_name: &str, stream_name: &str, _query: Option<&str>) {
        let Some(resolved) = self.resolve_publish_cleanup(app_name, stream_name, "on_unpublish")
        else {
            return;
        };
        let attempt = resolved;

        let should_cleanup = match self
            .cleanup_publisher_if_current_attempt(app_name, stream_name, attempt.epoch)
            .await
        {
            Ok(should_cleanup) => should_cleanup,
            Err(e) => {
                tracing::error!(
                    room_id = %app_name,
                    media_id = %stream_name,
                    epoch = attempt.epoch,
                    "Failed to fence publisher cleanup on unpublish; keeping pending cleanup for retry: {}",
                    e
                );
                return;
            }
        };

        if !should_cleanup {
            let _ = self.consume_pending_publish_cleanup(app_name, stream_name);
            tracing::info!(
                room_id = %app_name,
                media_id = %stream_name,
                epoch = attempt.epoch,
                "Ignoring stale on_unpublish cleanup for superseded publisher epoch"
            );
            return;
        }

        let _ = self.consume_pending_publish_cleanup(app_name, stream_name);
        let tracked_user = self
            .user_stream_tracker
            .remove_stream(app_name, stream_name);
        tracing::info!(
            user_id = %attempt.user_id,
            room_id = %app_name,
            media_id = %stream_name,
            had_tracker_entry = tracked_user.is_some(),
            "Publisher unpublished, fenced cleanup completed"
        );

        self.delete_user_stream_key(&attempt.user_id, "unpublish")
            .await;

        if let Some(ref tx) = self.stream_event_tx {
            let _ = tx.send(StreamLifecycleEvent::Stopped {
                room_id: app_name.to_string(),
                media_id: stream_name.to_string(),
                user_id: attempt.user_id,
            });
        }
    }

    /// A5: Rollback publisher registration when `StreamHub` publish fails after auth.
    ///
    /// Called when `on_publish` succeeded (registered in Redis, inserted tracker entry,
    /// wrote user_streams hash) but a later step (e.g., `StreamHub` publish) failed.
    /// Cleans up all state changes made during `on_publish`:
    /// 1. Unregister publisher from Redis
    /// 2. Remove user->stream mapping from local tracker
    /// 3. Remove per-user `rtmp:user_stream:{user_id}` Redis key
    async fn on_publish_rollback(&self, app_name: &str, stream_name: &str, _query: Option<&str>) {
        tracing::warn!(
            room_id = %app_name,
            media_id = %stream_name,
            "Rolling back publisher registration due to StreamHub failure"
        );

        let Some(resolved) = self.resolve_publish_cleanup(app_name, stream_name, "rollback") else {
            return;
        };
        let attempt = resolved;

        let should_cleanup = match self
            .cleanup_publisher_if_current_attempt(app_name, stream_name, attempt.epoch)
            .await
        {
            Ok(should_cleanup) => should_cleanup,
            Err(e) => {
                tracing::warn!(
                    room_id = %app_name,
                    media_id = %stream_name,
                    epoch = attempt.epoch,
                    error = %e,
                    "Failed to rollback publisher registration with epoch fence; keeping pending cleanup for retry"
                );
                return;
            }
        };

        if !should_cleanup {
            let _ = self.consume_pending_publish_cleanup(app_name, stream_name);
            tracing::info!(
                room_id = %app_name,
                media_id = %stream_name,
                epoch = attempt.epoch,
                "Ignoring stale rollback cleanup for superseded publisher epoch"
            );
            return;
        }

        let _ = self.consume_pending_publish_cleanup(app_name, stream_name);
        let _ = self
            .user_stream_tracker
            .remove_stream(app_name, stream_name);
        self.delete_user_stream_key(&attempt.user_id, "rollback")
            .await;

        tracing::info!(
            room_id = %app_name,
            media_id = %stream_name,
            "Publisher registration rolled back successfully"
        );
    }
}

/// Extract and URL-decode the `token` parameter from a query string.
///
/// Returns `None` if:
/// - No `token=` parameter is present
/// - The token value is empty or contains only whitespace
///
/// The token value is percent-decoded (e.g. `%2B` → `+`) so that JWT tokens
/// containing `+` characters survive URL encoding in RTMP query strings.
fn extract_token_from_query(query: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some(encoded_value) = pair.strip_prefix("token=") {
            // Percent-decode the token. Replace encoding errors with U+FFFD
            // (malformed UTF-8 in a JWT is invalid anyway, but we surface it
            // later during JWT validation rather than silently dropping here).
            let decoded = percent_decode_str(encoded_value)
                .decode_utf8_lossy()
                .into_owned();

            // Return None for empty or whitespace-only tokens to avoid
            // meaningless JWT validation errors downstream.
            if decoded.trim().is_empty() {
                return None;
            }

            return Some(decoded);
        }
    }
    None
}

/// Validated publish claims with authorization level
#[derive(Debug)]
struct ValidatedPublish {
    room_id: String,
    media_id: String,
    user_id: String,
    auth_level: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoomAccessRejection {
    Banned,
    Pending,
    Rejected,
    Closed,
}

impl RoomAccessRejection {
    fn into_error(self, app_name: &str) -> Box<dyn std::error::Error + Send + Sync> {
        match self {
            Self::Banned => format!("Room {app_name} is banned").into(),
            Self::Pending => format!("Room {app_name} is pending, need admin approval").into(),
            Self::Rejected => format!("Room {app_name} was rejected by admin").into(),
            Self::Closed => format!("Room {app_name} is closed").into(),
        }
    }

    const fn log_message(self) -> &'static str {
        match self {
            Self::Banned => "RTMP play rejected: room is banned",
            Self::Pending => "RTMP play rejected: room is pending approval",
            Self::Rejected => "RTMP play rejected: room was rejected",
            Self::Closed => "RTMP play rejected: room is closed",
        }
    }
}

fn validate_rtmp_room_state(room: &Room) -> Result<(), RoomAccessRejection> {
    if room.is_banned {
        return Err(RoomAccessRejection::Banned);
    }
    if room.status == RoomStatus::Pending {
        return Err(RoomAccessRejection::Pending);
    }
    if room.status == RoomStatus::Rejected {
        return Err(RoomAccessRejection::Rejected);
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
        // Validate room
        let room = self
            .room_service
            .get_room(&synctv_core::models::RoomId::from_string(
                app_name.to_string(),
            ))
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
        let expected_room_id = synctv_core::models::RoomId::from_string(app_name.to_string());
        let expected_media_id = synctv_core::models::MediaId::from_string(stream_name.to_string());
        let claims = self
            .publish_key_service
            .validate_publish_key_for_stream_claims(token, &expected_room_id, &expected_media_id)
            .await
            .map_err(|e| format!("Invalid stream key: {e}"))?;

        // Re-verify user status at connection time
        let user_id = UserId::from_string(claims.user_id.clone());
        let user = self
            .user_service
            .get_user(&user_id)
            .await
            .map_err(|e| format!("Failed to load user: {e}"))?;

        if user.status == UserStatus::Banned || user.status == UserStatus::Rejected {
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
            .check_publish_authorization(&user, app_name, &user_id, &claims)
            .await?;

        Ok(ValidatedPublish {
            room_id: claims.room_id,
            media_id: claims.media_id,
            user_id: claims.user_id,
            auth_level,
        })
    }

    /// Validate room status and settings for RTMP play requests.
    ///
    /// Checks:
    /// - Room exists
    /// - Room is not banned
    /// - Room status is not Pending or Closed
    /// - Room has rtmp_player enabled in settings
    pub async fn validate_play_request(
        &self,
        app_name: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let room_id = synctv_core::models::RoomId::from_string(app_name.to_string());

        // Validate room exists and check status
        let room = self
            .room_service
            .get_room(&room_id)
            .await
            .map_err(|e| format!("Failed to load room: {e}"))?;

        if let Err(reason) = validate_rtmp_room_state(&room) {
            tracing::warn!(room_id = %app_name, "{}", reason.log_message());
            return Err(reason.into_error(app_name));
        }

        // Check room settings for RTMP player
        let settings = self
            .room_service
            .get_room_settings(&room_id)
            .await
            .map_err(|e| format!("Failed to load room settings: {e}"))?;

        if !settings.rtmp_player.0 {
            tracing::warn!(
                room_id = %app_name,
                "RTMP play rejected: rtmp_player is disabled in room settings"
            );
            return Err(
                format!("RTMP play rejected for room {app_name}: rtmp_player is disabled in room settings. Use HTTP-FLV or HLS.").into()
            );
        }

        // All checks passed - allow the connection
        tracing::info!(
            room_id = %app_name,
            "RTMP play allowed: room is active and rtmp_player is enabled"
        );
        Ok(())
    }

    /// Check that the user has permission to publish to this room/media.
    /// Returns the authorization level string on success.
    async fn check_publish_authorization(
        &self,
        user: &synctv_core::models::User,
        app_name: &str,
        user_id: &UserId,
        claims: &synctv_core::service::publish_key::PublishClaims,
    ) -> Result<&'static str, Box<dyn std::error::Error + Send + Sync>> {
        let is_global_admin = user.role.is_admin_or_above();

        let is_room_admin_or_creator = if is_global_admin {
            false
        } else {
            let room_id = synctv_core::models::RoomId::from_string(app_name.to_string());
            match self
                .room_service
                .member_service()
                .get_member(&room_id, user_id)
                .await
            {
                Ok(Some(member)) => matches!(
                    member.role,
                    synctv_core::models::RoomRole::Creator | synctv_core::models::RoomRole::Admin
                ),
                _ => false,
            }
        };

        // Verify media belongs to this room
        let media_id = MediaId::from_string(claims.media_id.clone());
        let room_id_obj = synctv_core::models::RoomId::from_string(app_name.to_string());
        let media = self
            .room_service
            .media_service()
            .get_media(&media_id)
            .await
            .map_err(|e| format!("Failed to load media: {e}"))?
            .ok_or_else(|| format!("Media {} not found", claims.media_id))?;
        if media.room_id != room_id_obj {
            return Err(format!(
                "Media {} does not belong to room {}",
                claims.media_id, app_name
            )
            .into());
        }

        let is_media_creator = if !is_global_admin && !is_room_admin_or_creator {
            media.creator_id.as_ref() == Some(user_id)
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

    /// Register the publisher in Redis and set up tracking.
    ///
    /// Ongoing TTL renewal is handled by `PublisherManager::maintain_heartbeats()`,
    /// not here. This method only performs the initial registration.
    async fn register_and_start_ttl(
        &self,
        validated: &ValidatedPublish,
        app_name: &str,
        stream_name: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Enforce single-publisher-per-media: atomically register in Redis.
        // This also writes user→stream to `stream:user_publishers:{user_id}` via
        // the Lua script in try_register_publisher_with_user, providing a cross-replica
        // reverse index for user→stream lookups via get_user_publishers().
        let registered = self
            .registry
            .try_register_publisher(
                &validated.room_id,
                &validated.media_id,
                &self.node_id,
                &validated.user_id,
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
            .lookup_registered_epoch(&validated.room_id, &validated.media_id)
            .await?;

        tracing::info!(
            "Publisher authenticated and registered: user={}, room={}, media={}, node={}, auth={}, epoch={}",
            validated.user_id,
            app_name,
            validated.media_id,
            self.node_id,
            validated.auth_level,
            registered_epoch,
        );

        // Write an additional cross-replica user→stream mapping to Redis.
        // Key: `{prefix}rtmp:user_stream:{user_id}` (per-user key with individual TTL)
        // Value: `{room_id}|{media_id}` (using `|` separator since shared base62 IDs only
        //        contain ASCII alphanumeric characters, so `|` is unambiguous)
        //
        // This complements the Set-based `stream:user_publishers:{user_id}` index
        // already written by try_register_publisher_with_user.  The per-user key
        // provides O(1) lookup on any replica when only one active stream per user
        // is expected, with individual TTL per user instead of resetting a shared
        // hash TTL on every write.
        //
        // Issue #45: if SET fails after registration succeeded, we roll back the
        // publisher registration to keep Redis consistent.
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let stream_value = format!("{}|{}", validated.room_id, validated.media_id);
            let redis_key = self.user_stream_key(&validated.user_id);
            // SET + EXPIRE in a single pipeline for atomicity
            let set_result: Result<((), i64), redis::RedisError> = redis::pipe()
                .set(&redis_key, &stream_value)
                .expire(&redis_key, PUBLISHER_TTL_SECS)
                .query_async(&mut conn)
                .await;
            if let Err(e) = set_result {
                // Issue #45: SET failed after registration — roll back the publisher
                // registration so we don't leave an inconsistent state where the
                // publisher slot is occupied but the user→stream mapping is absent.
                tracing::error!(
                    user_id = %validated.user_id,
                    stream_value = %stream_value,
                    "Failed to write rtmp:user_stream to Redis after publisher registration: {}. \
                     Rolling back publisher registration to maintain consistency.",
                    e
                );
                if let Err(unreg_err) = self
                    .registry
                    .unregister_publisher_if_epoch_matches(
                        &validated.room_id,
                        &validated.media_id,
                        registered_epoch,
                    )
                    .await
                {
                    tracing::error!(
                        room_id = %validated.room_id,
                        media_id = %validated.media_id,
                        "Rollback of publisher registration also failed: {}. \
                         Redis TTL will eventually expire the stale entry.",
                        unreg_err
                    );
                }
                return Err(format!("Failed to write user stream mapping to Redis: {e}").into());
            }
        }

        // Track user->stream mapping locally for kick-on-ban (O(1) local lookup)
        self.user_stream_tracker.insert(
            validated.user_id.clone(),
            app_name.to_string(),
            validated.media_id.clone(),
            app_name,
            stream_name,
        );

        // Emit stream lifecycle event
        if let Some(ref tx) = self.stream_event_tx {
            let _ = tx.send(StreamLifecycleEvent::Started {
                room_id: validated.room_id.clone(),
                media_id: validated.media_id.clone(),
                user_id: validated.user_id.clone(),
            });
        }

        self.remember_pending_publish_cleanup(
            &validated.room_id,
            &validated.media_id,
            PendingPublishCleanup {
                epoch: registered_epoch,
                user_id: validated.user_id.clone(),
            },
        );

        Ok(())
    }

    /// Look up the stream key for a user, checking the local tracker first and
    /// falling back to Redis if not found locally (cross-replica lookup).
    ///
    /// Returns `Some((room_id, media_id))` if the user is actively publishing.
    #[cfg(test)]
    pub async fn get_user_stream(&self, user_id: &str) -> Option<(String, String)> {
        // Fast path: check local in-memory tracker
        let local = self.user_stream_tracker.get_user_streams(user_id);
        if !local.is_empty() {
            return local.into_iter().next();
        }

        // Slow path: check Redis cross-replica mapping ({key_prefix}rtmp:user_stream:{user_id})
        if let Some(mut conn) = self.redis_conn_snapshot().await {
            let key = self.user_stream_key(user_id);
            let result: Result<Option<String>, redis::RedisError> =
                redis::cmd("GET").arg(&key).query_async(&mut conn).await;
            match result {
                Ok(Some(stream_value)) => {
                    // Value format: "{room_id}|{media_id}" — `|` is safe because
                    // Shared base62 IDs only use ASCII alphanumeric characters.
                    if let Some((room_id, media_id)) = stream_value.split_once('|') {
                        return Some((room_id.to_string(), media_id.to_string()));
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        user_id = %user_id,
                        "Failed to query rtmp:user_stream from Redis: {}",
                        e
                    );
                }
            }
        }

        // Final fallback: check Set-based publisher reverse index
        if let Ok(publishers) = self.registry.get_user_publishers(user_id).await {
            return publishers.into_iter().next();
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use synctv_livestream::relay::{InMemoryStreamRegistry, PublisherInfo, StreamRegistryTrait};
    use tokio::sync::RwLock;

    #[derive(Debug)]
    struct FlakyUnregisterRegistry {
        inner: Arc<InMemoryStreamRegistry>,
        fail_unregister_if_epoch_matches_times: AtomicUsize,
    }

    impl FlakyUnregisterRegistry {
        fn new(inner: Arc<InMemoryStreamRegistry>) -> Self {
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
        async fn register_publisher(
            &self,
            room_id: &str,
            media_id: &str,
            node_id: &str,
            app_name: &str,
            api_address: &str,
        ) -> anyhow::Result<bool> {
            self.inner
                .register_publisher(room_id, media_id, node_id, app_name, api_address)
                .await
        }

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
        ) -> anyhow::Result<synctv_livestream::relay::registry_trait::PublisherRefreshOutcome>
        {
            self.inner
                .refresh_publisher_ttl(room_id, media_id, user_id)
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

        async fn list_active_streams(&self) -> anyhow::Result<Vec<(String, String)>> {
            self.inner.list_active_streams().await
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

        async fn unregister_all_user_publishers(&self, user_id: &str) -> anyhow::Result<()> {
            self.inner.unregister_all_user_publishers(user_id).await
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

    // ========== extract_token_from_query ==========

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
        // JWT tokens with `+` encoded as `%2B` must round-trip correctly (Issue #44)
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
    async fn test_on_publish_rejects_during_streamhub_restart() {
        let restarting = Arc::new(AtomicBool::new(true));
        let auth = SyncTvRtmpAuth::new(
            Arc::new(RoomService::new(
                sqlx::postgres::PgPoolOptions::new()
                    .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
                    .expect("lazy pool"),
                UserService::new(
                    sqlx::postgres::PgPoolOptions::new()
                        .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
                        .expect("lazy pool"),
                    synctv_core::service::JwtService::new(
                        "test-secret-key-for-http-router-tests-minimum-32-chars",
                    )
                    .expect("jwt"),
                    synctv_core::cache::UsernameCache::new(
                        Arc::new(synctv_core::cache::NoopCacheL2),
                        "test:username:".to_string(),
                        16,
                        60,
                    ),
                    synctv_core::config::PasswordComplexityConfig::default(),
                    Arc::new(synctv_core::service::InMemoryTokenBlacklistStore::new(
                        128, 3600, 86400,
                    )),
                    synctv_core::cache::KeyBuilder::new("test"),
                    synctv_core::service::auth::BruteForceProtection::in_memory("test".to_string()),
                ),
            )),
            Arc::new(UserService::new(
                sqlx::postgres::PgPoolOptions::new()
                    .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
                    .expect("lazy pool"),
                synctv_core::service::JwtService::new(
                    "test-secret-key-for-http-router-tests-minimum-32-chars",
                )
                .expect("jwt"),
                synctv_core::cache::UsernameCache::new(
                    Arc::new(synctv_core::cache::NoopCacheL2),
                    "test:username:".to_string(),
                    16,
                    60,
                ),
                synctv_core::config::PasswordComplexityConfig::default(),
                Arc::new(synctv_core::service::InMemoryTokenBlacklistStore::new(
                    128, 3600, 86400,
                )),
                synctv_core::cache::KeyBuilder::new("test"),
                synctv_core::service::auth::BruteForceProtection::in_memory("test".to_string()),
            )),
            Arc::new(synctv_core::service::PublishKeyService::with_default_ttl(
                synctv_core::service::JwtService::new(
                    "test-secret-key-for-http-router-tests-minimum-32-chars",
                )
                .expect("jwt"),
            )),
            Arc::new(synctv_livestream::api::StreamTracker::new()),
            Arc::new(synctv_livestream::relay::InMemoryStreamRegistry::new()),
            "node-1".to_string(),
            "127.0.0.1:50051".to_string(),
            None,
            "test:".to_string(),
        )
        .with_restarting_flag(restarting);

        let result = auth.on_publish("room", "stream", None).await;
        let err = result.expect_err("publish must be rejected while restarting");
        assert!(
            err.to_string().contains("StreamHub is restarting"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_delayed_unpublish_does_not_remove_newer_registration() {
        let registry = Arc::new(InMemoryStreamRegistry::new());
        let auth = make_test_auth_with_registry(registry.clone());

        let room_id = "room-delayed-unpublish";
        let media_id = "media-delayed-unpublish";
        let second_user_id = "user-second";

        auth.register_and_start_ttl(
            &validated_publish(room_id, media_id, "user-first"),
            room_id,
            media_id,
        )
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

        auth.register_and_start_ttl(
            &validated_publish(room_id, media_id, second_user_id),
            room_id,
            media_id,
        )
        .await
        .expect("second publish registration should succeed");

        auth.on_unpublish(room_id, media_id, None).await;

        let current = registry
            .get_publisher(room_id, media_id)
            .await
            .expect("publisher lookup should succeed after stale unpublish")
            .expect("stale unpublish must not remove the replacement publisher");
        assert_eq!(current.user_id, second_user_id);
        assert_eq!(
            auth.user_stream_tracker.get_stream_user(room_id, media_id),
            Some(second_user_id.to_string()),
            "stale unpublish must not remove the replacement stream tracker entry"
        );
    }

    #[tokio::test]
    async fn test_delayed_unpublish_preserves_newer_rollback_fence() {
        let registry = Arc::new(InMemoryStreamRegistry::new());
        let auth = make_test_auth_with_registry(registry.clone());

        let room_id = "room-delayed-fence";
        let media_id = "media-delayed-fence";

        auth.register_and_start_ttl(
            &validated_publish(room_id, media_id, "user-first"),
            room_id,
            media_id,
        )
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

        auth.register_and_start_ttl(
            &validated_publish(room_id, media_id, "user-second"),
            room_id,
            media_id,
        )
        .await
        .expect("second publish registration should succeed");

        auth.on_unpublish(room_id, media_id, None).await;
        auth.on_publish_rollback(room_id, media_id, None).await;

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
        let registry = Arc::new(InMemoryStreamRegistry::new());
        let auth = make_test_auth_with_registry(registry.clone());

        let room_id = "room-delayed-rollback";
        let media_id = "media-delayed-rollback";
        let second_user_id = "user-second";

        auth.register_and_start_ttl(
            &validated_publish(room_id, media_id, "user-first"),
            room_id,
            media_id,
        )
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

        auth.register_and_start_ttl(
            &validated_publish(room_id, media_id, second_user_id),
            room_id,
            media_id,
        )
        .await
        .expect("second publish registration should succeed");

        auth.on_publish_rollback(room_id, media_id, None).await;

        let current = registry
            .get_publisher(room_id, media_id)
            .await
            .expect("publisher lookup should succeed after stale rollback")
            .expect("stale rollback must not remove the replacement publisher");
        assert_eq!(current.user_id, second_user_id);
    }

    #[tokio::test]
    async fn test_unpublish_retry_preserves_fence_until_cleanup_succeeds() {
        let registry = Arc::new(FlakyUnregisterRegistry::new(Arc::new(
            InMemoryStreamRegistry::new(),
        )));
        let auth = make_test_auth_with_registry_dyn(registry.clone());

        let room_id = "room-retry-unpublish";
        let media_id = "media-retry-unpublish";

        auth.register_and_start_ttl(
            &validated_publish(room_id, media_id, "user-retry"),
            room_id,
            media_id,
        )
        .await
        .expect("publish registration should succeed");

        registry.set_fail_unregister_if_epoch_matches_times(1);

        auth.on_unpublish(room_id, media_id, None).await;
        assert!(
            registry
                .is_stream_active(room_id, media_id)
                .await
                .expect("publisher activity lookup should succeed after failed cleanup"),
            "failed cleanup attempt should leave publisher registered for retry"
        );

        auth.on_unpublish(room_id, media_id, None).await;
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
        let registry = Arc::new(FlakyUnregisterRegistry::new(Arc::new(
            InMemoryStreamRegistry::new(),
        )));
        let auth = make_test_auth_with_registry_dyn(registry.clone());

        let room_id = "room-retry-rollback";
        let media_id = "media-retry-rollback";

        auth.register_and_start_ttl(
            &validated_publish(room_id, media_id, "user-retry"),
            room_id,
            media_id,
        )
        .await
        .expect("publish registration should succeed");

        registry.set_fail_unregister_if_epoch_matches_times(1);

        auth.on_publish_rollback(room_id, media_id, None).await;
        assert!(
            registry
                .is_stream_active(room_id, media_id)
                .await
                .expect("publisher activity lookup should succeed after failed rollback"),
            "failed rollback attempt should leave publisher registered for retry"
        );

        auth.on_publish_rollback(room_id, media_id, None).await;
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
        let registry = Arc::new(InMemoryStreamRegistry::new());
        let auth = make_test_auth_with_registry(registry.clone());
        let restarted_auth = make_test_auth_with_registry(registry.clone());

        let room_id = "room-restarted-unpublish";
        let media_id = "media-restarted-unpublish";
        let user_id = "user-restarted";

        auth.register_and_start_ttl(
            &validated_publish(room_id, media_id, user_id),
            room_id,
            media_id,
        )
        .await
        .expect("publish registration should succeed");

        restarted_auth.on_unpublish(room_id, media_id, None).await;

        assert!(
            registry
                .is_stream_active(room_id, media_id)
                .await
                .expect("publisher activity lookup should succeed after restarted unpublish"),
            "restarted unpublish must not guess a cleanup epoch from the live publisher"
        );
        assert!(
            restarted_auth
                .get_user_stream(user_id)
                .await
                .is_some(),
            "restarted unpublish must leave the persisted user stream mapping intact without a fence"
        );
    }

    #[tokio::test]
    async fn test_publish_rollback_without_in_memory_fence_does_not_guess_cleanup_target() {
        let registry = Arc::new(InMemoryStreamRegistry::new());
        let auth = make_test_auth_with_registry(registry.clone());
        let restarted_auth = make_test_auth_with_registry(registry.clone());

        let room_id = "room-restarted-rollback";
        let media_id = "media-restarted-rollback";
        let user_id = "user-restarted-rollback";

        auth.register_and_start_ttl(
            &validated_publish(room_id, media_id, user_id),
            room_id,
            media_id,
        )
        .await
        .expect("publish registration should succeed");

        restarted_auth
            .on_publish_rollback(room_id, media_id, None)
            .await;

        assert!(
            registry
                .is_stream_active(room_id, media_id)
                .await
                .expect("publisher activity lookup should succeed after restarted rollback"),
            "restarted rollback must not guess a cleanup epoch from the live publisher"
        );
        assert!(
            restarted_auth
                .get_user_stream(user_id)
                .await
                .is_some(),
            "restarted rollback must leave the persisted user stream mapping intact without a fence"
        );
    }

    #[tokio::test]
    async fn test_restarted_unpublish_does_not_remove_replacement_publisher() {
        let registry = Arc::new(InMemoryStreamRegistry::new());
        let auth = make_test_auth_with_registry(registry.clone());
        let restarted_auth = make_test_auth_with_registry(registry.clone());

        let room_id = "room-restarted-stale-unpublish";
        let media_id = "media-restarted-stale-unpublish";
        let first_user_id = "user-first";
        let second_user_id = "user-second";

        auth.register_and_start_ttl(
            &validated_publish(room_id, media_id, first_user_id),
            room_id,
            media_id,
        )
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

        auth.register_and_start_ttl(
            &validated_publish(room_id, media_id, second_user_id),
            room_id,
            media_id,
        )
        .await
        .expect("second publish registration should succeed");

        restarted_auth.on_unpublish(room_id, media_id, None).await;

        let current = registry
            .get_publisher(room_id, media_id)
            .await
            .expect("publisher lookup should succeed after restarted stale unpublish")
            .expect("restarted stale unpublish must not remove the replacement publisher");
        assert_eq!(current.user_id, second_user_id);
        assert_eq!(
            restarted_auth.get_user_stream(second_user_id).await,
            Some((room_id.to_string(), media_id.to_string())),
            "replacement publisher mapping must remain after restarted stale unpublish"
        );
    }

    fn validated_publish(room_id: &str, media_id: &str, user_id: &str) -> ValidatedPublish {
        ValidatedPublish {
            room_id: room_id.to_string(),
            media_id: media_id.to_string(),
            user_id: user_id.to_string(),
            auth_level: "test",
        }
    }

    fn make_test_auth_with_registry(registry: Arc<InMemoryStreamRegistry>) -> SyncTvRtmpAuth {
        make_test_auth_with_registry_dyn(registry)
    }

    fn make_test_auth_with_registry_dyn(registry: Arc<dyn StreamRegistryTrait>) -> SyncTvRtmpAuth {
        let lazy_pool = || {
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
                .expect("lazy pool")
        };
        let make_user_service = || {
            UserService::new(
                lazy_pool(),
                synctv_core::service::JwtService::new(
                    "test-secret-key-for-http-router-tests-minimum-32-chars",
                )
                .expect("jwt"),
                synctv_core::cache::UsernameCache::new(
                    Arc::new(synctv_core::cache::NoopCacheL2),
                    "test:username:".to_string(),
                    16,
                    60,
                ),
                synctv_core::config::PasswordComplexityConfig::default(),
                Arc::new(synctv_core::service::InMemoryTokenBlacklistStore::new(
                    128, 3600, 86400,
                )),
                synctv_core::cache::KeyBuilder::new("test"),
                synctv_core::service::auth::BruteForceProtection::in_memory("test".to_string()),
            )
        };

        SyncTvRtmpAuth::new(
            Arc::new(RoomService::new(lazy_pool(), make_user_service())),
            Arc::new(make_user_service()),
            Arc::new(synctv_core::service::PublishKeyService::with_default_ttl(
                synctv_core::service::JwtService::new(
                    "test-secret-key-for-http-router-tests-minimum-32-chars",
                )
                .expect("jwt"),
            )),
            Arc::new(synctv_livestream::api::StreamTracker::new()),
            registry,
            "node-1".to_string(),
            "127.0.0.1:50051".to_string(),
            None,
            "test:".to_string(),
        )
    }

    #[test]
    fn test_validate_rtmp_room_state_rejects_closed_room() {
        let room = Room::new_with_status(
            "Closed room".to_string(),
            String::new(),
            UserId::from_string("user-1".to_string()),
            RoomStatus::Closed,
        );
        let err =
            validate_rtmp_room_state(&room).expect_err("closed room must reject RTMP publish/play");
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
    fn test_validate_rtmp_room_state_rejects_pending_room() {
        let room = Room::new_with_status(
            "Pending room".to_string(),
            String::new(),
            UserId::from_string("user-1".to_string()),
            RoomStatus::Pending,
        );

        assert_eq!(
            validate_rtmp_room_state(&room),
            Err(RoomAccessRejection::Pending)
        );
    }

    #[test]
    fn test_validate_rtmp_room_state_rejects_banned_room() {
        let mut room = Room::new(
            "Banned room".to_string(),
            UserId::from_string("user-1".to_string()),
        );
        room.ban();

        assert_eq!(
            validate_rtmp_room_state(&room),
            Err(RoomAccessRejection::Banned)
        );
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_get_user_stream_uses_hot_swapped_shared_redis_connection() {
        use redis::AsyncCommands;

        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let shared = Arc::new(RwLock::new(
            redis::aio::ConnectionManager::new(client.clone())
                .await
                .expect("initial connection manager should build"),
        ));

        let auth = SyncTvRtmpAuth::new(
            Arc::new(RoomService::new(
                sqlx::postgres::PgPoolOptions::new()
                    .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
                    .expect("lazy pool"),
                UserService::new(
                    sqlx::postgres::PgPoolOptions::new()
                        .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
                        .expect("lazy pool"),
                    synctv_core::service::JwtService::new(
                        "test-secret-key-for-http-router-tests-minimum-32-chars",
                    )
                    .expect("jwt"),
                    synctv_core::cache::UsernameCache::new(
                        Arc::new(synctv_core::cache::NoopCacheL2),
                        "test:username:".to_string(),
                        16,
                        60,
                    ),
                    synctv_core::config::PasswordComplexityConfig::default(),
                    Arc::new(synctv_core::service::InMemoryTokenBlacklistStore::new(
                        128, 3600, 86400,
                    )),
                    synctv_core::cache::KeyBuilder::new("test"),
                    synctv_core::service::auth::BruteForceProtection::in_memory("test".to_string()),
                ),
            )),
            Arc::new(UserService::new(
                sqlx::postgres::PgPoolOptions::new()
                    .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
                    .expect("lazy pool"),
                synctv_core::service::JwtService::new(
                    "test-secret-key-for-http-router-tests-minimum-32-chars",
                )
                .expect("jwt"),
                synctv_core::cache::UsernameCache::new(
                    Arc::new(synctv_core::cache::NoopCacheL2),
                    "test:username:".to_string(),
                    16,
                    60,
                ),
                synctv_core::config::PasswordComplexityConfig::default(),
                Arc::new(synctv_core::service::InMemoryTokenBlacklistStore::new(
                    128, 3600, 86400,
                )),
                synctv_core::cache::KeyBuilder::new("test"),
                synctv_core::service::auth::BruteForceProtection::in_memory("test".to_string()),
            )),
            Arc::new(synctv_core::service::PublishKeyService::with_default_ttl(
                synctv_core::service::JwtService::new(
                    "test-secret-key-for-http-router-tests-minimum-32-chars",
                )
                .expect("jwt"),
            )),
            Arc::new(synctv_livestream::api::StreamTracker::new()),
            Arc::new(synctv_livestream::relay::InMemoryStreamRegistry::new()),
            "node-1".to_string(),
            "127.0.0.1:50051".to_string(),
            None,
            "test-rtmp:".to_string(),
        )
        .with_redis(shared.clone());

        let replacement = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("replacement connection manager should build");
        *shared.write().await = replacement;

        let mut verify_conn = redis::aio::ConnectionManager::new(client)
            .await
            .expect("verification connection should build");
        let _: () = verify_conn
            .set("test-rtmp:rtmp:user_stream:user-1", "room-1|media-1")
            .await
            .expect("seed user stream mapping");

        let user_stream = auth.get_user_stream("user-1").await;
        assert_eq!(
            user_stream,
            Some(("room-1".to_string(), "media-1".to_string())),
            "RTMP auth must re-read the shared Redis handle after a hot swap"
        );
    }

    // ========== StreamLifecycleEvent ==========

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
