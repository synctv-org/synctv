//! Client API Implementation
//!
//! Unified implementation for all client API operations.
//! Used by both HTTP and gRPC handlers.
//!
//! Split into sub-modules by domain:
//! - `auth`: register, login, logout, refresh_token
//! - `user`: get_profile, set_username, set_password
//! - `room`: create/get/join/leave/delete room, settings, chat, hot rooms
//! - `member`: get_members, kick, ban, unban, set_permissions
//! - `media`: add/remove/edit/swap media, batch operations, playlist items
//! - `playlist`: create/update/delete/list playlists
//! - `playback`: play, pause, seek, speed, set_current_media, get_playback_state
//! - `stream`: publish_key, stream_info, live proxy, validate_live_token
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
    media_to_proto, network_stats_to_proto, proto_role_to_room_role, proto_role_to_user_role,
    room_role_to_proto,
};

// Room password limits imported from the single source of truth in synctv-core
use synctv_core::validation::{ROOM_PASSWORD_MIN, ROOM_PASSWORD_MAX};

/// Validate a password that is being **set** (create room, set password, update settings).
fn validate_password_for_set(password: &str) -> Result<(), String> {
    if password.len() < ROOM_PASSWORD_MIN {
        return Err(format!("Password too short (minimum {ROOM_PASSWORD_MIN} characters)"));
    }
    if password.len() > ROOM_PASSWORD_MAX {
        return Err(format!("Password too long (maximum {ROOM_PASSWORD_MAX} characters)"));
    }
    Ok(())
}

/// Validate a password that is being **verified** (join room, check password).
fn validate_password_for_verify(password: &str) -> Result<(), String> {
    if password.len() > ROOM_PASSWORD_MAX {
        return Err(format!("Password too long (maximum {ROOM_PASSWORD_MAX} characters)"));
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
    pub sfu_manager: Option<Arc<synctv_sfu::SfuManager>>,
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
    pub sfu_manager: Option<Arc<synctv_sfu::SfuManager>>,
    pub publish_key_service: Option<Arc<synctv_core::service::PublishKeyService>>,
    pub jwt_service: synctv_core::service::JwtService,
    pub live_streaming_infrastructure: Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    pub providers_manager: Option<Arc<synctv_core::service::ProvidersManager>>,
    pub settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
    pub redis_publish_tx: Option<tokio::sync::mpsc::Sender<synctv_cluster::sync::PublishRequest>>,
    /// Shared Redis connection for playback caching
    pub redis_conn: Option<redis::aio::ConnectionManager>,
    /// Rate limiter for per-endpoint rate limiting (password checks, etc.)
    pub rate_limiter: Option<synctv_core::service::rate_limit::RateLimiter>,
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
        sfu_manager: Option<Arc<synctv_sfu::SfuManager>>,
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
            sfu_manager,
            publish_key_service,
            jwt_service,
            live_streaming_infrastructure,
            providers_manager,
            settings_registry,
            redis_publish_tx: None,
            redis_conn: None,
            rate_limiter: None,
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
            sfu_manager: config.sfu_manager,
            publish_key_service: config.publish_key_service,
            jwt_service: config.jwt_service,
            live_streaming_infrastructure: config.live_streaming_infrastructure,
            providers_manager: config.providers_manager,
            settings_registry: config.settings_registry,
            redis_publish_tx: None,
            redis_conn: None,
            rate_limiter: None,
        }
    }

    /// Set the Redis publish channel for cross-replica cache invalidation
    #[must_use]
    pub fn with_redis_publish_tx(mut self, tx: Option<tokio::sync::mpsc::Sender<synctv_cluster::sync::PublishRequest>>) -> Self {
        self.redis_publish_tx = tx;
        self
    }

    /// Set the Redis connection for playback caching
    #[must_use]
    pub fn with_redis_conn(mut self, conn: Option<redis::aio::ConnectionManager>) -> Self {
        self.redis_conn = conn;
        self
    }

    /// Set the rate limiter for per-endpoint rate limiting (password checks, etc.)
    #[must_use]
    pub fn with_rate_limiter(mut self, rate_limiter: synctv_core::service::rate_limit::RateLimiter) -> Self {
        self.rate_limiter = Some(rate_limiter);
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

    /// Publish a permission change event to other cluster replicas
    fn publish_permission_changed(
        &self,
        room_id: &RoomId,
        target_user_id: &UserId,
        changed_by: &UserId,
    ) {
        if let Some(ref tx) = self.redis_publish_tx {
            let _ = tx.try_send(synctv_cluster::sync::PublishRequest {
                event: synctv_cluster::sync::ClusterEvent::PermissionChanged {
                    event_id: nanoid::nanoid!(16),
                    room_id: room_id.clone(),
                    target_user_id: target_user_id.clone(),
                    target_username: String::new(), // filled by receiver if needed
                    changed_by: changed_by.clone(),
                    changed_by_username: String::new(),
                    new_permissions: synctv_core::models::PermissionBits::empty(),
                    timestamp: chrono::Utc::now(),
                },
            });
        }
    }
}
