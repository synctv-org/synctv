//! Client API Implementation
//!
//! Unified implementation for all client API operations.
//! Used by both HTTP and gRPC handlers.
//!
//! Split into sub-modules by domain:
//! - `auth`: register, login, `refresh_token`
//! - `user`: `get_profile`, `set_username`, `set_password`
//! - `room`: create/get/join/leave/delete room, settings, chat, hot rooms
//! - `member`: `get_members`, kick, ban, unban, `set_permissions`
//! - `media`: add/remove/edit/swap media, batch operations, playlist items
//! - `playlist`: create/update/delete/list playlists
//! - `playback`: play, pause, seek, speed, `set_current_media`, `get_playback_state`
//! - `stream`: `publish_key`, `stream_info`, live proxy, `validate_live_token`
//! - `webrtc`: ICE servers, network quality

mod auth;
mod media;
mod member;
mod playback;
mod playlist;
mod room;
mod stream;
mod user;
mod webrtc;

// Proto conversion helpers used across sub-modules
mod convert;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use synctv_cluster::sync::ConnectionManager;
use synctv_core::models::{RoomId, UserId};
use synctv_core::service::{UserService, RoomService};

// Re-export public items from convert module
pub use convert::{
    media_to_proto, proto_role_to_room_role, proto_role_to_user_role,
    room_role_to_proto,
};

// Room password limits imported from the single source of truth in synctv-core
use synctv_core::validation::{ROOM_PASSWORD_MIN, ROOM_PASSWORD_MAX};

use crate::impls::ApiError;

/// Validate a password that is being **set** (create room, set password, update settings).
fn validate_password_for_set(password: &str) -> Result<(), ApiError> {
    // Issue #72: Reject passwords that are purely whitespace. A password of e.g. "   "
    // looks non-empty to a length check but provides no protection and confuses users.
    let trimmed = password.trim();
    if trimmed.is_empty() {
        return Err(ApiError::InvalidInput(
            "Room password cannot be empty or whitespace only".to_string(),
        ));
    }
    if trimmed.chars().count() < ROOM_PASSWORD_MIN {
        return Err(ApiError::InvalidInput(format!("Password too short (minimum {ROOM_PASSWORD_MIN} characters)")));
    }
    if password.chars().count() > ROOM_PASSWORD_MAX {
        return Err(ApiError::InvalidInput(format!("Password too long (maximum {ROOM_PASSWORD_MAX} characters)")));
    }
    Ok(())
}

/// Validate a password that is being **verified** (join room, check password).
fn validate_password_for_verify(password: &str) -> Result<(), ApiError> {
    if password.chars().count() > ROOM_PASSWORD_MAX {
        return Err(ApiError::InvalidInput(format!("Password too long (maximum {ROOM_PASSWORD_MAX} characters)")));
    }
    Ok(())
}

/// Configuration for constructing a [`ClientApiImpl`].
///
/// Groups all dependencies into a single struct to avoid `too_many_arguments`.
pub struct ClientApiConfig {
    pub user_service: Arc<UserService>,
    pub room_service: Arc<RoomService>,
    pub connection_manager: Arc<ConnectionManager>,
    pub config: Arc<synctv_core::Config>,
    pub publish_key_service: Option<Arc<synctv_core::service::PublishKeyService>>,
    pub jwt_service: synctv_core::service::JwtService,
    pub live_streaming_infrastructure: Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    pub providers_manager: Option<Arc<synctv_core::service::ProvidersManager>>,
    pub settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
}

/// Client API implementation
#[derive(Clone)]
pub struct ClientApiImpl {
    pub user_service: Arc<UserService>,
    pub room_service: Arc<RoomService>,
    pub connection_manager: Arc<ConnectionManager>,
    pub config: Arc<synctv_core::Config>,
    pub publish_key_service: Option<Arc<synctv_core::service::PublishKeyService>>,
    pub jwt_service: synctv_core::service::JwtService,
    pub live_streaming_infrastructure: Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    pub providers_manager: Option<Arc<synctv_core::service::ProvidersManager>>,
    pub settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
    pub redis_publish_tx: Option<tokio::sync::mpsc::Sender<synctv_cluster::sync::PublishRequest>>,
    /// Shared Redis connection for playback caching (Sentinel-failover safe)
    pub redis_conn: Option<crate::SharedRedisConn>,
    /// Rate limiter for per-endpoint rate limiting (password checks, etc.)
    pub rate_limiter: Option<synctv_core::service::rate_limit::RateLimiter>,
    /// Resolved built-in STUN URL (e.g. "stun:203.0.113.1:3478"), set only when the
    /// built-in STUN server started successfully with a valid external address.
    /// When `None`, the built-in STUN entry is omitted from ICE server lists.
    pub builtin_stun_url: Option<String>,
}

impl ClientApiImpl {
    /// Create a new `ClientApiImpl` from individual parameters.
    ///
    /// Prefer [`ClientApiImpl::from_config`] for new code.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn new(
        user_service: Arc<UserService>,
        room_service: Arc<RoomService>,
        connection_manager: Arc<ConnectionManager>,
        config: Arc<synctv_core::Config>,
        publish_key_service: Option<Arc<synctv_core::service::PublishKeyService>>,
        jwt_service: synctv_core::service::JwtService,
        live_streaming_infrastructure: Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
        providers_manager: Option<Arc<synctv_core::service::ProvidersManager>>,
        settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
    ) -> Self {
        Self {
            user_service,
            room_service,
            connection_manager,
            config,
            publish_key_service,
            jwt_service,
            live_streaming_infrastructure,
            providers_manager,
            settings_registry,
            redis_publish_tx: None,
            redis_conn: None,
            rate_limiter: None,
            builtin_stun_url: None,
        }
    }

    /// Create a new `ClientApiImpl` from a config struct.
    #[must_use]
    pub fn from_config(config: ClientApiConfig) -> Self {
        Self {
            user_service: config.user_service,
            room_service: config.room_service,
            connection_manager: config.connection_manager,
            config: config.config,
            publish_key_service: config.publish_key_service,
            jwt_service: config.jwt_service,
            live_streaming_infrastructure: config.live_streaming_infrastructure,
            providers_manager: config.providers_manager,
            settings_registry: config.settings_registry,
            redis_publish_tx: None,
            redis_conn: None,
            rate_limiter: None,
            builtin_stun_url: None,
        }
    }

    /// Set the Redis publish channel for cross-replica cache invalidation
    #[must_use]
    pub fn with_redis_publish_tx(mut self, tx: Option<tokio::sync::mpsc::Sender<synctv_cluster::sync::PublishRequest>>) -> Self {
        self.redis_publish_tx = tx;
        self
    }

    /// Set the shared Redis connection for playback caching (Sentinel-failover safe)
    #[must_use]
    pub fn with_redis_conn(mut self, conn: Option<crate::SharedRedisConn>) -> Self {
        self.redis_conn = conn;
        self
    }

    /// Resolve a fresh Redis `ConnectionManager` clone from the shared `RwLock`.
    ///
    /// Returns `None` when Redis is not configured. The returned clone is cheap
    /// (internally Arc-backed) and always points to the current Redis master,
    /// even after a Sentinel failover.
    pub async fn resolve_redis_conn(&self) -> Option<redis::aio::ConnectionManager> {
        match &self.redis_conn {
            Some(shared) => Some(shared.read().await.clone()),
            None => None,
        }
    }

    /// Set the rate limiter for per-endpoint rate limiting (password checks, etc.)
    #[must_use]
    pub fn with_rate_limiter(mut self, rate_limiter: synctv_core::service::rate_limit::RateLimiter) -> Self {
        self.rate_limiter = Some(rate_limiter);
        self
    }

    /// Set the resolved built-in STUN URL for ICE server lists.
    /// Should be called with the external address from a successfully started `StunServer`.
    #[must_use]
    pub fn with_builtin_stun_url(mut self, url: String) -> Self {
        self.builtin_stun_url = Some(url);
        self
    }

    /// Kick a stream both locally and cluster-wide via Redis Pub/Sub.
    ///
    /// Used after media deletion to terminate any active RTMP stream.
    fn kick_stream_cluster(&self, room_id: &str, media_id: &str, reason: &str) {
        super::kick_stream_cluster(
            self.live_streaming_infrastructure.as_ref(),
            self.redis_publish_tx.as_ref(),
            room_id,
            media_id,
            reason,
        );
    }

    /// Publish a permission change event to other cluster replicas.
    ///
    /// Fetches actual usernames and effective permissions before broadcasting
    /// so that receivers get correct data without needing additional lookups.
    async fn publish_permission_changed(
        &self,
        room_id: &RoomId,
        target_user_id: &UserId,
        changed_by: &UserId,
    ) {
        if let Some(ref tx) = self.redis_publish_tx {
            // Fetch room settings for proper three-layer permission calculation
            let room_settings = self.room_service.get_room_settings(room_id).await
                .unwrap_or_default();

            // Fetch actual usernames and permissions for the event
            let (target_username, new_permissions, role, added_permissions, removed_permissions) = match self
                .room_service
                .member_service()
                .get_member(room_id, target_user_id)
                .await
            {
                Ok(Some(member)) => {
                    let username = self
                        .user_service
                        .get_user(target_user_id)
                        .await
                        .map(|u| u.username)
                        .unwrap_or_default();
                    let role_default = self.room_service.permission_service()
                        .calculate_role_default_permissions(&member.role, &room_settings);
                    let perms = member.effective_permissions(role_default);
                    let role_i32 = match member.role {
                        synctv_core::models::RoomRole::Creator => synctv_proto::common::RoomMemberRole::Creator as i32,
                        synctv_core::models::RoomRole::Admin => synctv_proto::common::RoomMemberRole::Admin as i32,
                        synctv_core::models::RoomRole::Member => synctv_proto::common::RoomMemberRole::Member as i32,
                        synctv_core::models::RoomRole::Guest => synctv_proto::common::RoomMemberRole::Guest as i32,
                    };
                    (username, perms, role_i32, member.added_permissions, member.removed_permissions)
                }
                _ => (String::new(), synctv_core::models::PermissionBits::empty(), synctv_proto::common::RoomMemberRole::Member as i32, 0u64, 0u64),
            };

            let changed_by_username = self
                .user_service
                .get_user(changed_by)
                .await
                .map(|u| u.username)
                .unwrap_or_default();

            crate::impls::try_publish_cluster_event(tx, synctv_cluster::sync::PublishRequest {
                event: synctv_cluster::sync::ClusterEvent::PermissionChanged {
                    event_id: nanoid::nanoid!(16),
                    room_id: room_id.clone(),
                    target_user_id: target_user_id.clone(),
                    target_username,
                    changed_by: changed_by.clone(),
                    changed_by_username,
                    new_permissions,
                    role,
                    added_permissions: synctv_core::models::PermissionBits(added_permissions),
                    removed_permissions: synctv_core::models::PermissionBits(removed_permissions),
                    timestamp: chrono::Utc::now(),
                },
            });
        }
    }
}
