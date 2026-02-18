//! RTMP authentication implementation for `SyncTV`
//!
//! This module provides the RTMP authentication callback that integrates
//! with `SyncTV`'s user and room management.
//!
//! On successful publish auth:
//! 1. Atomically registers the publisher in Redis (single-publisher-per-media enforcement)
//! 2. Registers the user→stream mapping in the local `StreamTracker`
//!
//! Ongoing TTL renewal is handled by `PublisherManager::maintain_heartbeats()`.
//!
//! On unpublish:
//! 1. Unregisters the publisher from Redis
//! 2. Removes the user→stream mapping from the local `StreamTracker`

use std::sync::Arc;

use async_trait::async_trait;
use synctv_livestream::AuthCallback;
use synctv_livestream::api::UserStreamTracker;
use synctv_livestream::relay::StreamRegistryTrait;

use synctv_core::{
    models::{RoomStatus, UserStatus, MediaId, UserId},
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
}

impl SyncTvRtmpAuth {
    pub fn new(
        room_service: Arc<RoomService>,
        user_service: Arc<UserService>,
        publish_key_service: Arc<PublishKeyService>,
        user_stream_tracker: UserStreamTracker,
        registry: Arc<dyn StreamRegistryTrait>,
        node_id: String,
        grpc_address: String,
        stream_event_tx: Option<tokio::sync::broadcast::Sender<StreamLifecycleEvent>>,
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
    ) -> Result<Option<synctv_livestream::rtmp_auth::AuthPublishRewrite>, Box<dyn std::error::Error + Send + Sync>> {
        // Phase 1: Validate room, token, user status, and authorization
        let validated = self.validate_publish_request(app_name, stream_name, query).await?;

        // Phase 2: Register in Redis, track mapping, emit event, spawn TTL renewal
        self.register_and_start_ttl(&validated, app_name, stream_name).await?;

        // Phase 3: Return rewrite so StreamHub uses canonical (room_id, media_id)
        // instead of the raw RTMP identifiers (room_id, JWT_TOKEN).
        Ok(Some(synctv_livestream::rtmp_auth::AuthPublishRewrite {
            app_name: validated.room_id,
            stream_name: validated.media_id,
        }))
    }

    /// RTMP pull (play) is unconditionally rejected.
    ///
    /// All viewer access must go through HTTP-FLV (`/api/room/movie/live/flv/`) or
    /// HLS (`/api/room/movie/live/hls/`) endpoints, which enforce JWT + room membership auth.
    async fn on_play(
        &self,
        app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tracing::warn!(
            room_id = %app_name,
            "RTMP play rejected: direct RTMP pull is disabled, use HTTP-FLV or HLS"
        );
        Err("RTMP pull is disabled. Use HTTP-FLV or HLS endpoints for playback.".into())
    }

    async fn on_unplay(
        &self,
        app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) {
        tracing::info!(
            room_id = %app_name,
            "RTMP player disconnected"
        );
    }

    async fn on_unpublish(
        &self,
        app_name: &str,
        stream_name: &str,
        _query: Option<&str>,
    ) {
        // Remove user→stream mapping from tracker (resolves RTMP identifiers to logical stream)
        let tracked = self.user_stream_tracker.remove_by_app_stream(app_name, stream_name);

        if let Some((ref user_id, ref room_id, ref media_id)) = tracked {
            tracing::info!(
                user_id = %user_id,
                room_id = %room_id,
                media_id = %media_id,
                "Publisher unpublished, cleaning up"
            );

            // Unregister from Redis
            if let Err(e) = self.registry.unregister_publisher(room_id, media_id).await {
                tracing::error!(
                    room_id = %room_id,
                    media_id = %media_id,
                    "Failed to unregister publisher from Redis: {}",
                    e
                );
            }
        } else {
            tracing::warn!(
                app_name = %app_name,
                "on_unpublish: no matching stream found in tracker (stream_name redacted)"
            );
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
}

fn extract_token_from_query(query: &str) -> Option<&str> {
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("token=") {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== extract_token_from_query ==========

    #[test]
    fn test_extract_token_single_param() {
        let result = extract_token_from_query("token=abc123");
        assert_eq!(result, Some("abc123"));
    }

    #[test]
    fn test_extract_token_among_multiple_params() {
        let result = extract_token_from_query("foo=bar&token=my_jwt_token&baz=qux");
        assert_eq!(result, Some("my_jwt_token"));
    }

    #[test]
    fn test_extract_token_first_param() {
        let result = extract_token_from_query("token=first_token&other=value");
        assert_eq!(result, Some("first_token"));
    }

    #[test]
    fn test_extract_token_last_param() {
        let result = extract_token_from_query("other=value&token=last_token");
        assert_eq!(result, Some("last_token"));
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
    fn test_extract_token_empty_value() {
        let result = extract_token_from_query("token=");
        assert_eq!(result, Some(""));
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
        assert_eq!(result, Some(jwt));
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
            StreamLifecycleEvent::Started { room_id, media_id, user_id } => {
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
            StreamLifecycleEvent::Stopped { room_id, media_id, user_id } => {
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
        let cloned = event.clone();
        match cloned {
            StreamLifecycleEvent::Started { room_id, .. } => {
                assert_eq!(room_id, "r1");
            }
            other => unreachable!("Expected Started variant, got: {other:?}"),
        }
    }
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
            .get_room(&synctv_core::models::RoomId::from_string(app_name.to_string()))
            .await
            .map_err(|e| format!("Failed to load room: {e}"))?;

        if room.is_banned {
            return Err(format!("Room {app_name} is banned").into());
        }
        if room.status == RoomStatus::Pending {
            return Err(format!("Room {app_name} is pending, need admin approval").into());
        }

        // Extract token: prefer query string parameter, fall back to stream_name as token
        let token = if let Some(q) = query {
            extract_token_from_query(q).unwrap_or(stream_name)
        } else {
            stream_name
        };

        // Validate JWT stream_key
        let claims = self
            .publish_key_service
            .validate_publish_key(token)
            .await
            .map_err(|e| format!("Invalid stream key: {e}"))?;

        // Verify room_id matches
        if claims.room_id != app_name {
            return Err(format!(
                "Room ID mismatch: token for room {}, but connecting to room {}",
                claims.room_id, app_name
            )
            .into());
        }

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
            match self.room_service.member_service().get_member(&room_id, user_id).await {
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
        let media = self.room_service.media_service().get_media(&media_id).await
            .map_err(|e| format!("Failed to load media: {e}"))?
            .ok_or_else(|| format!("Media {} not found", claims.media_id))?;
        if media.room_id != room_id_obj {
            return Err(format!(
                "Media {} does not belong to room {}",
                claims.media_id, app_name
            ).into());
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
        // Enforce single-publisher-per-media: atomically register in Redis
        let registered = self.registry
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
            ).into());
        }

        tracing::info!(
            "Publisher authenticated and registered: user={}, room={}, media={}, node={}, auth={}",
            validated.user_id,
            app_name,
            validated.media_id,
            self.node_id,
            validated.auth_level,
        );

        // Track user->stream mapping for kick-on-ban
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
}
