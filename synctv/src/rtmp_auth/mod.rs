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

use std::sync::Arc;

use async_trait::async_trait;
use percent_encoding::percent_decode_str;
use synctv_livestream::api::UserStreamTracker;
use synctv_livestream::relay::StreamRegistryTrait;
use synctv_livestream::AuthCallback;
// TTL for the per-user rtmp:user_stream:{user_id} Redis key, matching the publisher TTL.
use synctv_livestream::relay::registry::PUBLISHER_TTL_SECS;

use synctv_core::{
    models::{MediaId, RoomStatus, UserId, UserStatus},
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
    user_stream_tracker: UserStreamTracker,
    /// Publisher registry (Redis) for single-publisher-per-media enforcement
    registry: Arc<dyn StreamRegistryTrait>,
    /// This node's unique identifier for publisher registration
    node_id: String,
    /// Advertised gRPC address for cross-node proxying (e.g., "10.0.0.1:50051")
    grpc_address: String,
    /// Broadcast channel for stream lifecycle events (StreamStarted/StreamStopped)
    stream_event_tx: Option<tokio::sync::broadcast::Sender<StreamLifecycleEvent>>,
    /// Redis key prefix from config (e.g., "synctv:") for multi-instance isolation
    key_prefix: String,
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
    redis_conn: Option<redis::aio::ConnectionManager>,
}

impl SyncTvRtmpAuth {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        room_service: Arc<RoomService>,
        user_service: Arc<UserService>,
        publish_key_service: Arc<PublishKeyService>,
        user_stream_tracker: UserStreamTracker,
        registry: Arc<dyn StreamRegistryTrait>,
        node_id: String,
        grpc_address: String,
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
            grpc_address,
            stream_event_tx,
            key_prefix,
            redis_conn: None,
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
    pub fn with_redis(mut self, conn: redis::aio::ConnectionManager) -> Self {
        self.redis_conn = Some(conn);
        self
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
        // Remove user→stream mapping from tracker (resolves RTMP identifiers to logical stream)
        let tracked = self
            .user_stream_tracker
            .remove_by_app_stream(app_name, stream_name);

        if let Some((ref user_id, ref room_id, ref media_id)) = tracked {
            tracing::info!(
                user_id = %user_id,
                room_id = %room_id,
                media_id = %media_id,
                "Publisher unpublished, cleaning up"
            );

            // Unregister from Redis (publisher entry + stream:user_publishers Set)
            if let Err(e) = self.registry.unregister_publisher(room_id, media_id).await {
                tracing::error!(
                    room_id = %room_id,
                    media_id = %media_id,
                    "Failed to unregister publisher from Redis: {}",
                    e
                );
            }

            // Clean up the per-user Redis key ({key_prefix}rtmp:user_stream:{user_id})
            if let Some(ref conn) = self.redis_conn {
                let mut conn = conn.clone();
                let key = self.user_stream_key(user_id);
                let result: Result<(), redis::RedisError> =
                    redis::cmd("DEL").arg(&key).query_async(&mut conn).await;
                if let Err(e) = result {
                    tracing::warn!(
                        user_id = %user_id,
                        "Failed to remove rtmp:user_stream entry on unpublish (non-fatal): {}",
                        e
                    );
                }
            }
        } else {
            // Tracker lookup failed -- the RTMP identifiers after AuthPublishRewrite are
            // (room_id, media_id), so we can still attempt Redis cleanup directly.
            // This handles edge cases where the tracker entry was lost (e.g., process
            // restart, race condition) but Redis still has a stale publisher entry.
            tracing::error!(
                app_name = %app_name,
                "on_unpublish: no matching stream found in tracker. \
                 Attempting direct Redis cleanup using RTMP identifiers (room_id={}, media_id={}). \
                 Redis TTL will serve as fallback if this also fails.",
                app_name, stream_name
            );

            if let Err(e) = self
                .registry
                .unregister_publisher(app_name, stream_name)
                .await
            {
                tracing::error!(
                    room_id = %app_name,
                    media_id = %stream_name,
                    "Failed fallback Redis cleanup on unpublish: {}. \
                     Redis TTL will eventually expire the stale entry.",
                    e
                );
            }
        }

        // Always emit StreamStopped event, even if tracker removal failed.
        // This ensures subscribers are notified regardless of tracker state.
        if let Some(ref tx) = self.stream_event_tx {
            if let Some((ref user_id, ref room_id, ref media_id)) = tracked {
                let _ = tx.send(StreamLifecycleEvent::Stopped {
                    room_id: room_id.clone(),
                    media_id: media_id.clone(),
                    user_id: user_id.clone(),
                });
            }
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

        // 1. Unregister publisher from Redis
        if let Err(e) = self
            .registry
            .unregister_publisher(app_name, stream_name)
            .await
        {
            tracing::warn!(
                room_id = %app_name,
                media_id = %stream_name,
                error = %e,
                "Failed to rollback publisher registration from Redis"
            );
        }

        // 2. Remove user->stream mapping from local tracker
        let tracked = self
            .user_stream_tracker
            .remove_by_app_stream(app_name, stream_name);

        // 3. Clean up per-user Redis key
        if let Some((ref user_id, _, _)) = tracked {
            if let Some(ref conn) = self.redis_conn {
                let mut conn = conn.clone();
                let key = self.user_stream_key(user_id);
                let result: Result<(), redis::RedisError> =
                    redis::cmd("DEL").arg(&key).query_async(&mut conn).await;
                if let Err(e) = result {
                    tracing::warn!(
                        user_id = %user_id,
                        "Failed to remove rtmp:user_stream entry on rollback (non-fatal): {}",
                        e
                    );
                }
            }
        }

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
struct ValidatedPublish {
    room_id: String,
    media_id: String,
    user_id: String,
    auth_level: &'static str,
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

        if room.is_banned {
            return Err(format!("Room {app_name} is banned").into());
        }
        if room.status == RoomStatus::Pending {
            return Err(format!("Room {app_name} is pending, need admin approval").into());
        }

        // Extract token: prefer query string parameter (URL-decoded), fall back to stream_name
        let token_owned: Option<String> = query.and_then(extract_token_from_query);
        let token = token_owned.as_deref().unwrap_or(stream_name);

        // Validate JWT stream_key
        let expected_room_id = synctv_core::models::RoomId::from_string(app_name.to_string());
        let claims = if token_owned.is_some() {
            let expected_media_id =
                synctv_core::models::MediaId::from_string(stream_name.to_string());
            self.publish_key_service
                .validate_publish_key_for_stream_claims(token, &expected_room_id, &expected_media_id)
                .await
        } else {
            // Legacy RTMP publish URLs use /{room_id}/{JWT_TOKEN}, so stream_name is
            // the token itself rather than the media ID. Preserve that format by
            // validating the token and room here, then enforcing media ownership
            // against the claims below before registration.
            self.publish_key_service.validate_publish_key(token).await.and_then(|claims| {
                if claims.room_id != expected_room_id.as_str() {
                    Err(synctv_core::Error::Authorization(format!(
                        "Token room mismatch: expected {}, got {}",
                        expected_room_id.as_str(),
                        claims.room_id
                    )))
                } else {
                    Ok(claims)
                }
            })
        }
        .map_err(|e| format!("Invalid stream key: {e}"))?;

        // Re-verify user status at connection time
        let user_id = UserId::from_string(claims.user_id.clone());
        let user = self
            .user_service
            .get_user(&user_id)
            .await
            .map_err(|e| format!("Failed to load user: {e}"))?;

        if user.status == UserStatus::Banned {
            return Err(format!("User {} is banned", claims.user_id).into());
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

        if room.is_banned {
            tracing::warn!(
                room_id = %app_name,
                "RTMP play rejected: room is banned"
            );
            return Err(format!("Room {app_name} is banned").into());
        }

        if room.status == RoomStatus::Pending {
            tracing::warn!(
                room_id = %app_name,
                "RTMP play rejected: room is pending approval"
            );
            return Err(format!("Room {app_name} is pending, need admin approval").into());
        }

        if room.status == RoomStatus::Closed {
            tracing::warn!(
                room_id = %app_name,
                "RTMP play rejected: room is closed"
            );
            return Err(format!("Room {app_name} is closed").into());
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
                &self.grpc_address,
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

        tracing::info!(
            "Publisher authenticated and registered: user={}, room={}, media={}, node={}, auth={}",
            validated.user_id,
            app_name,
            validated.media_id,
            self.node_id,
            validated.auth_level,
        );

        // Write an additional cross-replica user→stream mapping to Redis.
        // Key: `{prefix}rtmp:user_stream:{user_id}` (per-user key with individual TTL)
        // Value: `{room_id}|{media_id}` (using `|` separator since nanoid IDs only
        //        contain [A-Za-z0-9_-], so `|` is unambiguous)
        //
        // This complements the Set-based `stream:user_publishers:{user_id}` index
        // already written by try_register_publisher_with_user.  The per-user key
        // provides O(1) lookup on any replica when only one active stream per user
        // is expected, with individual TTL per user instead of resetting a shared
        // hash TTL on every write.
        //
        // Issue #45: if SET fails after registration succeeded, we roll back the
        // publisher registration to keep Redis consistent.
        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();
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
                    .unregister_publisher(&validated.room_id, &validated.media_id)
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

        Ok(())
    }

    /// Look up the stream key for a user, checking the local tracker first and
    /// falling back to Redis if not found locally (cross-replica lookup).
    ///
    /// Returns `Some((room_id, media_id))` if the user is actively publishing.
    #[allow(dead_code)]
    pub async fn get_user_stream(&self, user_id: &str) -> Option<(String, String)> {
        // Fast path: check local in-memory tracker
        let local = self.user_stream_tracker.get_user_streams(user_id);
        if !local.is_empty() {
            return local.into_iter().next();
        }

        // Slow path: check Redis cross-replica mapping ({key_prefix}rtmp:user_stream:{user_id})
        if let Some(ref conn) = self.redis_conn {
            let mut conn = conn.clone();
            let key = self.user_stream_key(user_id);
            let result: Result<Option<String>, redis::RedisError> =
                redis::cmd("GET").arg(&key).query_async(&mut conn).await;
            match result {
                Ok(Some(stream_value)) => {
                    // Value format: "{room_id}|{media_id}" — `|` is safe because
                    // nanoid IDs only use [A-Za-z0-9_-] characters.
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
    fn test_legacy_rtmp_publish_without_query_uses_stream_name_as_token() {
        let query = None;
        let stream_name = "legacy_jwt_token";
        let token_owned: Option<String> = query.and_then(extract_token_from_query);
        let token = token_owned.as_deref().unwrap_or(stream_name);

        assert_eq!(
            token, stream_name,
            "legacy RTMP path-token publish must continue treating stream_name as the token"
        );
        assert!(
            token_owned.is_none(),
            "legacy path-token mode must not require a query parameter"
        );
    }

    #[test]
    fn test_query_token_mode_keeps_stream_name_for_media_binding() {
        let query = Some("token=query_jwt_token");
        let stream_name = "media_123";
        let token_owned: Option<String> = query.and_then(extract_token_from_query);
        let token = token_owned.as_deref().unwrap_or(stream_name);

        assert_eq!(token, "query_jwt_token");
        assert_eq!(
            stream_name, "media_123",
            "query token mode must still reserve stream_name for media binding"
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

    #[test]
    fn test_stream_lifecycle_event_clone() {
        let event = StreamLifecycleEvent::Started {
            room_id: "r1".to_string(),
            media_id: "m1".to_string(),
            user_id: "u1".to_string(),
        };
        let cloned = event;
        match cloned {
            StreamLifecycleEvent::Started { room_id, .. } => {
                assert_eq!(room_id, "r1");
            }
            other => unreachable!("Expected Started variant, got: {other:?}"),
        }
    }
}
