//! Client API Implementation
//!
//! Unified implementation for all client API operations.
//! Used by both HTTP and gRPC handlers.
//!
//! Split into sub-modules by domain:
//! - `auth`: register, login, `refresh_token`
//! - `user`: `get_profile`, `set_username`, `set_password`
//! - `room`: create/get/join/leave/delete room, settings, chat, hot rooms
//! - `member`: `get_members`, kick, `set_permissions`
//! - `media`: add/remove/edit/swap media, batch operations, playlist items
//! - `playlist`: create/update/delete/list playlists
//! - `playback`: play, pause, seek, speed, `set_current_media`, `get_playback_state`
//! - `webrtc`: ICE servers, network quality

mod auth;
pub use auth::login_outcome_to_proto;
pub mod file_download;
pub mod live_danmaku;
pub mod media;
mod member;
pub mod passkey;
mod playback;
pub mod playback_lifecycle;
pub mod playlist;
mod report;
mod room;
pub use room::{
    build_search_chat_messages_query, parse_optional_room_category_id,
    parse_proto_chat_attachments, parse_room_label_ids,
};
pub mod stream;
mod user;
mod webrtc;
pub use playback::{build_playback_state_update, build_start_playback_request};
pub use user::{
    token_auth_context_from_claims, user_notification_preferences_to_proto,
    user_preferences_update_from_proto,
};

// Proto conversion helpers used across impls modules within this crate.
pub mod convert;

#[cfg(test)]
mod tests;

use futures::{future::BoxFuture, stream as futures_stream, StreamExt as _, TryStreamExt as _};
use std::collections::HashMap;
use std::sync::Arc;
use synctv_core::models::{RoomId, RoomPermissionSet, RoomStatus};
use synctv_core::service::{
    ChatService, ContentReportService, ReviewService, RoomService, UserService,
};
use synctv_core::service::{GuestTokenValidator, JwtValidator, TokenType};
use synctv_core::RedisConnectionRuntime;

// Re-export conversion helpers within the crate.
pub use convert::{
    proto_role_filter_to_room_role, proto_role_to_assignable_room_role, proto_role_to_room_role,
    proto_role_to_user_role, room_role_to_proto,
};

use crate::chat_event_dispatcher::{default_chat_event_dispatcher, ChatEventDispatcher};
use crate::fanout::{default_room_settings_fanout_service, RoomSettingsFanoutService};
use crate::impls::{
    ApiError, ApiRequestContext, EndpointRateLimitCategory, EndpointRateLimitScope,
    RequestExecutor, RequestMetadata,
};
use crate::media_fanout::{default_media_fanout_service, MediaFanoutService};
use crate::membership_event_fanout::{
    default_membership_event_fanout_service, MembershipEventFanoutService,
};
use crate::playback_fanout::{default_playback_fanout_service, PlaybackFanoutService};
use crate::playlist_fanout::{default_playlist_fanout_service, PlaylistFanoutService};
use crate::realtime_lifecycle::{default_realtime_lifecycle_service, RealtimeLifecycleService};
use crate::room_cache_fanout::{default_room_cache_fanout_service, RoomCacheFanoutService};
use crate::room_lifecycle_fanout::{
    default_room_lifecycle_fanout_service_with_realtime, RoomLifecycleFanoutService,
};
use synctv_realtime::fanout::{
    LocalNoopRealtimeEventService, RealtimeEventService, RealtimeFanoutService,
};
use synctv_realtime::sync::ConnectionRuntime;

const CLIENT_ASSET_LOAD_CONCURRENCY: usize = 16;

/// Options for constructing a [`ClientApiImpl`].
///
/// Groups all dependencies into a single struct to avoid `too_many_arguments`.
pub struct ClientApiOptions {
    pub user_service: Arc<UserService>,
    pub read_pool: Option<sqlx::PgPool>,
    pub room_service: Arc<RoomService>,
    pub chat_service: Option<Arc<ChatService>>,
    pub connection_service: Arc<dyn ConnectionRuntime>,
    pub runtime_settings: Arc<crate::ApiRuntimeSettings>,
    pub publish_key_service: Option<Arc<dyn synctv_core::service::StreamingPublishKeyService>>,
    pub jwt_service: synctv_core::service::JwtService,
    pub live_streaming_infrastructure: Option<Arc<synctv_livestream::LiveStreamingInfrastructure>>,
    pub runtime_settings_store: Option<Arc<synctv_core::service::RuntimeSettingsStore>>,
    pub provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver>,
    pub public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
    pub email_api: Option<Arc<crate::impls::EmailApiImpl>>,
    pub passkey_service: Option<Arc<synctv_core::service::PasskeyService>>,
}

pub struct ClientApiRuntime {
    pub clock: Arc<dyn synctv_core::Clock>,
    pub realtime_fanout: Arc<dyn RealtimeFanoutService>,
    pub realtime_event_service: Arc<dyn RealtimeEventService>,
    pub chat_event_dispatcher: Arc<dyn ChatEventDispatcher>,
    pub redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    pub builtin_stun_url: Option<String>,
    pub webrtc_status: synctv_core::service::WebRtcRuntimeStatus,
    pub provider_access_service: Arc<dyn synctv_core::provider::ProviderAccessService>,
    pub signing_key: Arc<crate::proxy_signature::ProxySigningKey>,
    pub presence_service: Arc<synctv_core::service::OnlinePresenceService>,
    pub jwt_validator: Arc<synctv_core::service::JwtValidator>,
    pub request_executor: Arc<RequestExecutor>,
    pub ws_ticket_service: Arc<dyn synctv_core::service::WebSocketTicketService>,
    pub playback_duration_probe: Option<Arc<synctv_core::service::PlaybackDurationProbeService>>,
}

pub struct ClientApiRuntimeServices {
    pub clock: Arc<dyn synctv_core::Clock>,
    pub realtime_fanout: Arc<dyn RealtimeFanoutService>,
    pub realtime_event_service: Arc<dyn RealtimeEventService>,
    pub redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    pub builtin_stun_url: Option<String>,
    pub webrtc_status: synctv_core::service::WebRtcRuntimeStatus,
    pub provider_access_service: Arc<dyn synctv_core::provider::ProviderAccessService>,
    pub signing_key: Arc<crate::proxy_signature::ProxySigningKey>,
    pub presence_service: Arc<synctv_core::service::OnlinePresenceService>,
    pub jwt_validator: Arc<synctv_core::service::JwtValidator>,
    pub request_executor: Arc<RequestExecutor>,
    pub ws_ticket_service: Arc<dyn synctv_core::service::WebSocketTicketService>,
    pub playback_duration_probe: Option<Arc<synctv_core::service::PlaybackDurationProbeService>>,
}

impl ClientApiRuntime {
    #[must_use]
    pub fn new_with_services(services: ClientApiRuntimeServices) -> Self {
        Self {
            clock: services.clock,
            chat_event_dispatcher: default_chat_event_dispatcher(
                services.realtime_event_service.clone(),
            ),
            realtime_fanout: services.realtime_fanout,
            realtime_event_service: services.realtime_event_service,
            redis_runtime: services.redis_runtime,
            builtin_stun_url: services.builtin_stun_url,
            webrtc_status: services.webrtc_status,
            provider_access_service: services.provider_access_service,
            signing_key: services.signing_key,
            presence_service: services.presence_service,
            jwt_validator: services.jwt_validator,
            request_executor: services.request_executor,
            ws_ticket_service: services.ws_ticket_service,
            playback_duration_probe: services.playback_duration_probe,
        }
    }

    #[must_use]
    pub fn local_disabled(
        request_executor: Arc<RequestExecutor>,
        signing_key: Arc<crate::proxy_signature::ProxySigningKey>,
    ) -> Self {
        let realtime_event_service = Arc::new(LocalNoopRealtimeEventService::new());
        Self {
            clock: Arc::new(synctv_core::SystemClock),
            realtime_fanout: crate::realtime_fanout::local_realtime_fanout_service(
                realtime_event_service.clone(),
            ),
            chat_event_dispatcher: default_chat_event_dispatcher(realtime_event_service.clone()),
            realtime_event_service,
            redis_runtime: None,
            builtin_stun_url: None,
            webrtc_status: synctv_core::service::WebRtcRuntimeStatus::peer_to_peer_stun_disabled(),
            provider_access_service: crate::impls::disabled_provider_access_service(),
            signing_key,
            presence_service: Arc::new(synctv_core::service::OnlinePresenceService::local()),
            jwt_validator: request_executor.jwt_validator().clone(),
            request_executor,
            ws_ticket_service: Arc::new(synctv_core::service::WsTicketService::local_only(None)),
            playback_duration_probe: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GuestRoomAccess {
    pub room_id: RoomId,
    pub guest_id: String,
    pub display_name: String,
    pub session_id: String,
    pub token_jti: String,
    pub permissions: RoomPermissionSet,
    pub room_guest_version: i64,
}

#[derive(Debug, Clone)]
pub enum RoomActor {
    User {
        room_id: RoomId,
        user_id: synctv_core::models::UserId,
    },
    Guest(GuestRoomAccess),
}

impl RoomActor {
    #[must_use]
    pub const fn room_id(&self) -> RoomId {
        match self {
            Self::User { room_id, .. } => *room_id,
            Self::Guest(access) => access.room_id,
        }
    }

    #[must_use]
    pub const fn user_id(&self) -> Option<synctv_core::models::UserId> {
        match self {
            Self::User { user_id, .. } => Some(*user_id),
            Self::Guest(_) => None,
        }
    }

    pub fn require_user_id(&self) -> Result<synctv_core::models::UserId, ApiError> {
        self.user_id().ok_or_else(|| {
            ApiError::Authorization("This room operation requires a signed-in user".to_string())
        })
    }
}

/// Client API implementation
#[derive(Clone)]
pub struct ClientApiImpl {
    pub clock: Arc<dyn synctv_core::Clock>,
    pub user_service: Arc<UserService>,
    pub room_service: Arc<RoomService>,
    pub chat_service: Option<Arc<ChatService>>,
    pub review_service: Arc<ReviewService>,
    pub content_report_service: Arc<ContentReportService>,
    pub connection_service: Arc<dyn ConnectionRuntime>,
    pub presence_service: Arc<synctv_core::service::OnlinePresenceService>,
    pub runtime_settings: Arc<crate::ApiRuntimeSettings>,
    pub publish_key_service: Option<Arc<dyn synctv_core::service::StreamingPublishKeyService>>,
    pub jwt_service: synctv_core::service::JwtService,
    pub live_streaming_infrastructure: Option<Arc<synctv_livestream::LiveStreamingInfrastructure>>,
    pub runtime_settings_store: Option<Arc<synctv_core::service::RuntimeSettingsStore>>,
    pub realtime_fanout: Arc<dyn RealtimeFanoutService>,
    pub room_settings_fanout: Arc<dyn RoomSettingsFanoutService>,
    pub membership_event_fanout: Arc<dyn MembershipEventFanoutService>,
    pub media_fanout: Arc<dyn MediaFanoutService>,
    pub playback_fanout: Arc<dyn PlaybackFanoutService>,
    pub playlist_fanout: Arc<dyn PlaylistFanoutService>,
    pub room_cache_fanout: Arc<dyn RoomCacheFanoutService>,
    pub realtime_lifecycle: Arc<dyn RealtimeLifecycleService>,
    pub room_lifecycle_fanout: Arc<dyn RoomLifecycleFanoutService>,
    pub realtime_event_service: Arc<dyn RealtimeEventService>,
    pub chat_event_dispatcher: Arc<dyn ChatEventDispatcher>,
    /// Redis runtime abstraction derived from the shared connection when available.
    pub redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    /// Resolved built-in STUN URL (e.g. "stun:203.0.113.1:3478"), set only when the
    /// built-in STUN server started successfully with a valid external address.
    /// When `None`, the built-in STUN entry is omitted from ICE server lists.
    pub builtin_stun_url: Option<String>,
    /// Structured WebRTC/STUN runtime state for diagnostics.
    pub webrtc_status: synctv_core::service::WebRtcRuntimeStatus,
    /// Typed provider credential/session access cache
    pub provider_access_service: Arc<dyn synctv_core::provider::ProviderAccessService>,
    /// Proxy signing key for generating HMAC-signed proxy URLs
    pub signing_key: Arc<crate::proxy_signature::ProxySigningKey>,
    /// Per-provider stores for signed playback version mappings
    pub provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver>,
    /// JWT validator for token validation (e.g. live streaming tokens)
    pub jwt_validator: Arc<synctv_core::service::JwtValidator>,
    /// Shared sqids codec for API-facing resource identifiers.
    pub public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
    pub playback_duration_probe: Option<Arc<synctv_core::service::PlaybackDurationProbeService>>,
    pub request_executor: Arc<RequestExecutor>,
    /// Shared email API for email-token flows that are exposed by multiple transports.
    pub email_api: Option<Arc<crate::impls::EmailApiImpl>>,
    /// Shared WebAuthn service for passkey flows that are exposed by multiple transports.
    pub passkey_service: Option<Arc<synctv_core::service::PasskeyService>>,
    /// Shared WebSocket ticket service for issuing short-lived room-bound tickets.
    pub ws_ticket_service: Arc<dyn synctv_core::service::WebSocketTicketService>,
}

impl ClientApiImpl {
    pub async fn load_stored_file_reference(
        &self,
        file_reference_id: Option<i64>,
    ) -> Result<Option<synctv_core::models::StoredFileReference>, ApiError> {
        let Some(file_reference_id) = file_reference_id else {
            return Ok(None);
        };
        self.user_service
            .get_stored_file_reference(file_reference_id)
            .await
            .map_err(ApiError::from)
    }

    pub fn stored_file_reference_access(
        &self,
        file: &synctv_core::models::StoredFileReference,
        policy: &synctv_core::models::FileUploadPolicy,
    ) -> Result<Option<crate::impls::stored_files::StoredFileObjectAccess>, ApiError> {
        let Some(storage) = crate::impls::stored_files::first_file_storage([
            self.user_service.file_storage_service(),
            self.room_service.file_storage_service(),
            self.room_service.playlist_service().file_storage_service(),
            self.room_service.media_service().file_storage_service(),
        ]) else {
            return Ok(None);
        };
        crate::impls::stored_files::stored_file_reference_access(storage.as_ref(), file, policy)
    }

    pub fn source_cover_url(
        &self,
        room_id: synctv_core::models::RoomId,
        viewer_id: Option<synctv_core::models::UserId>,
        cover: synctv_core::provider::SourceCover,
    ) -> Result<Option<String>, ApiError> {
        match cover {
            synctv_core::provider::SourceCover::Url { url } => {
                Ok((!url.trim().is_empty()).then_some(url))
            }
            synctv_core::provider::SourceCover::Emby {
                server_id,
                credential_owner_id,
                item_id,
            } => {
                let Some(viewer_id) = viewer_id else {
                    return Ok(None);
                };
                let public_room_id =
                    self.public_id_codec
                        .encode_room_id(room_id)
                        .map_err(|error| {
                            ApiError::Internal(format!("Failed to encode room public id: {error}"))
                        })?;
                let public_user_id =
                    self.public_id_codec
                        .encode_user_id(viewer_id)
                        .map_err(|error| {
                            ApiError::Internal(format!("Failed to encode user public id: {error}"))
                        })?;
                let public_credential_owner_id = self
                    .public_id_codec
                    .encode_user_id(credential_owner_id)
                    .map_err(|error| {
                        ApiError::Internal(format!(
                            "Failed to encode credential owner public id: {error}"
                        ))
                    })?;
                let thumbnail = crate::emby_thumbnail_urls::emby_thumbnail_url(
                    &server_id,
                    &public_credential_owner_id,
                    &item_id,
                );
                crate::emby_thumbnail_urls::sign_emby_thumbnail_url(
                    &thumbnail,
                    &public_room_id,
                    &public_user_id,
                    self.signing_key.as_ref(),
                )
                .map(Some)
                .map_err(ApiError::Internal)
            }
            synctv_core::provider::SourceCover::Fnos {
                server_id,
                credential_owner_id,
                image_path,
            } => {
                let Some(viewer_id) = viewer_id else {
                    return Ok(None);
                };
                let public_room_id = self
                    .public_id_codec
                    .encode_room_id(room_id)
                    .map_err(ApiError::Internal)?;
                let public_user_id = self
                    .public_id_codec
                    .encode_user_id(viewer_id)
                    .map_err(ApiError::Internal)?;
                let public_owner_id = self
                    .public_id_codec
                    .encode_user_id(credential_owner_id)
                    .map_err(ApiError::Internal)?;
                let thumbnail = crate::fnos_thumbnail_urls::fnos_thumbnail_url(
                    &server_id,
                    &public_owner_id,
                    &image_path,
                    800,
                );
                crate::fnos_thumbnail_urls::sign_fnos_thumbnail_url(
                    &thumbnail,
                    &public_room_id,
                    &public_user_id,
                    self.signing_key.as_ref(),
                )
                .map(Some)
                .map_err(ApiError::Internal)
            }
            synctv_core::provider::SourceCover::Qnap {
                server_id,
                credential_owner_id,
                path,
            } => {
                let Some(viewer_id) = viewer_id else {
                    return Ok(None);
                };
                let room_id = self
                    .public_id_codec
                    .encode_room_id(room_id)
                    .map_err(ApiError::Internal)?;
                let user_id = self
                    .public_id_codec
                    .encode_user_id(viewer_id)
                    .map_err(ApiError::Internal)?;
                let owner_id = self
                    .public_id_codec
                    .encode_user_id(credential_owner_id)
                    .map_err(ApiError::Internal)?;
                let thumbnail = crate::qnap_thumbnail_urls::qnap_thumbnail_url(
                    &server_id, &owner_id, &path, 640,
                );
                crate::qnap_thumbnail_urls::sign_qnap_thumbnail_url(
                    &thumbnail,
                    &room_id,
                    &user_id,
                    self.signing_key.as_ref(),
                )
                .map(Some)
                .map_err(ApiError::Internal)
            }
            synctv_core::provider::SourceCover::Nextcloud {
                server_id,
                credential_owner_id,
                file_id,
            } => {
                let Some(viewer_id) = viewer_id else {
                    return Ok(None);
                };
                let room_id = self
                    .public_id_codec
                    .encode_room_id(room_id)
                    .map_err(ApiError::Internal)?;
                let user_id = self
                    .public_id_codec
                    .encode_user_id(viewer_id)
                    .map_err(ApiError::Internal)?;
                let owner_id = self
                    .public_id_codec
                    .encode_user_id(credential_owner_id)
                    .map_err(ApiError::Internal)?;
                let preview = crate::nextcloud_preview_urls::nextcloud_preview_url(
                    &server_id, &owner_id, file_id, 640, 640, true,
                );
                crate::nextcloud_preview_urls::sign_nextcloud_preview_url(
                    &preview,
                    &room_id,
                    &user_id,
                    self.signing_key.as_ref(),
                )
                .map(Some)
                .map_err(ApiError::Internal)
            }
            synctv_core::provider::SourceCover::Seafile {
                server_id,
                credential_owner_id,
                repository_id,
                path,
            } => {
                let Some(viewer_id) = viewer_id else {
                    return Ok(None);
                };
                let room_id = self
                    .public_id_codec
                    .encode_room_id(room_id)
                    .map_err(ApiError::Internal)?;
                let user_id = self
                    .public_id_codec
                    .encode_user_id(viewer_id)
                    .map_err(ApiError::Internal)?;
                let owner_id = self
                    .public_id_codec
                    .encode_user_id(credential_owner_id)
                    .map_err(ApiError::Internal)?;
                let thumbnail = crate::seafile_thumbnail_urls::seafile_thumbnail_url(
                    &server_id,
                    &owner_id,
                    &repository_id,
                    &path,
                    640,
                );
                crate::seafile_thumbnail_urls::sign_seafile_thumbnail_url(
                    &thumbnail,
                    &room_id,
                    &user_id,
                    self.signing_key.as_ref(),
                )
                .map(Some)
                .map_err(ApiError::Internal)
            }
            synctv_core::provider::SourceCover::SynologyFile {
                server_id,
                credential_owner_id,
                path,
            } => {
                let Some(viewer_id) = viewer_id else {
                    return Ok(None);
                };
                let room_id = self
                    .public_id_codec
                    .encode_room_id(room_id)
                    .map_err(ApiError::Internal)?;
                let user_id = self
                    .public_id_codec
                    .encode_user_id(viewer_id)
                    .map_err(ApiError::Internal)?;
                let owner_id = self
                    .public_id_codec
                    .encode_user_id(credential_owner_id)
                    .map_err(ApiError::Internal)?;
                let image = crate::synology_image_urls::synology_file_image_url(
                    &server_id, &owner_id, &path, "large",
                );
                crate::synology_image_urls::sign_synology_image_url(
                    &image,
                    crate::synology_image_urls::SynologyImageScope::File {
                        server_id: &server_id,
                        credential_owner_id: &owner_id,
                        path: &path,
                        size: "large",
                    },
                    &room_id,
                    &user_id,
                    self.signing_key.as_ref(),
                )
                .map(Some)
                .map_err(ApiError::Internal)
            }
            synctv_core::provider::SourceCover::SynologyPoster {
                server_id,
                credential_owner_id,
                item_id,
                media_type,
                poster_mtime,
            } => {
                let Some(viewer_id) = viewer_id else {
                    return Ok(None);
                };
                let room_id = self
                    .public_id_codec
                    .encode_room_id(room_id)
                    .map_err(ApiError::Internal)?;
                let user_id = self
                    .public_id_codec
                    .encode_user_id(viewer_id)
                    .map_err(ApiError::Internal)?;
                let owner_id = self
                    .public_id_codec
                    .encode_user_id(credential_owner_id)
                    .map_err(ApiError::Internal)?;
                let image = crate::synology_image_urls::synology_poster_url(
                    &server_id,
                    &owner_id,
                    item_id,
                    &media_type,
                    poster_mtime.as_deref(),
                );
                crate::synology_image_urls::sign_synology_image_url(
                    &image,
                    crate::synology_image_urls::SynologyImageScope::Poster {
                        server_id: &server_id,
                        credential_owner_id: &owner_id,
                        item_id,
                        media_type: &media_type,
                        poster_mtime: poster_mtime.as_deref(),
                    },
                    &room_id,
                    &user_id,
                    self.signing_key.as_ref(),
                )
                .map(Some)
                .map_err(ApiError::Internal)
            }
        }
    }

    pub async fn user_public_view_with_loaded_avatar(
        &self,
        user: &synctv_core::models::User,
    ) -> Result<synctv_proto::client::UserPublicView, ApiError> {
        let avatar = self
            .load_stored_file_reference(user.avatar_file_reference_id)
            .await?;
        let avatar_access = avatar
            .as_ref()
            .map(|file| {
                self.stored_file_reference_access(
                    file,
                    &synctv_core::service::user_avatar_upload_policy(),
                )
            })
            .transpose()?
            .flatten();
        convert::try_user_public_view_to_proto(user, avatar_access.as_ref(), &self.public_id_codec)
    }

    /// Batch load user public views with avatars loaded in parallel
    ///
    /// This is a drop-in replacement for calling `user_public_view_with_loaded_avatar`
    /// in a loop, but loads all avatars in parallel for better performance.
    ///
    /// # Performance
    /// - Serial: N users × ~10-50ms per avatar = 100-500ms for 10 users
    /// - Batch: ~10-50ms total regardless of user count
    pub async fn batch_user_public_views_with_loaded_avatars(
        &self,
        users: &[synctv_core::models::User],
    ) -> Result<Vec<synctv_proto::client::UserPublicView>, ApiError> {
        // Collect all unique avatar file reference IDs
        let avatar_refs: Vec<i64> = users
            .iter()
            .filter_map(|u| u.avatar_file_reference_id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let avatar_files = futures_stream::iter(avatar_refs)
            .map(|ref_id| async move {
                self.load_stored_file_reference(Some(ref_id))
                    .await
                    .map(|file| (ref_id, file))
            })
            .buffered(CLIENT_ASSET_LOAD_CONCURRENCY)
            .try_collect::<HashMap<_, _>>()
            .await?;

        // Generate URLs and convert to proto
        let policy = synctv_core::service::user_avatar_upload_policy();
        users
            .iter()
            .map(|user| {
                let avatar_access = user
                    .avatar_file_reference_id
                    .and_then(|ref_id| avatar_files.get(&ref_id))
                    .and_then(|opt| opt.as_ref())
                    .map(|file| self.stored_file_reference_access(file, &policy))
                    .transpose()?
                    .flatten();

                convert::try_user_public_view_to_proto(
                    user,
                    avatar_access.as_ref(),
                    &self.public_id_codec,
                )
            })
            .collect()
    }

    pub async fn room_creator_public_view(
        &self,
        room: &synctv_core::models::Room,
    ) -> Result<synctv_proto::client::UserPublicView, ApiError> {
        let creator = self
            .user_service
            .get_user(&room.created_by)
            .await
            .map_err(ApiError::from)?;
        self.user_public_view_with_loaded_avatar(&creator).await
    }

    pub async fn room_to_proto_basic_with_loaded_cover(
        &self,
        room: &synctv_core::models::Room,
        settings: Option<&synctv_core::models::RoomSettings>,
        member_count: Option<i32>,
    ) -> Result<synctv_proto::client::Room, ApiError> {
        let (cover, creator) = tokio::join!(
            self.load_stored_file_reference(room.cover_file_reference_id),
            self.room_creator_public_view(room),
        );
        let cover = cover?;
        let creator = creator?;
        let cover_access = cover
            .as_ref()
            .map(|file| {
                self.stored_file_reference_access(
                    file,
                    &synctv_core::service::room_cover_upload_policy(),
                )
            })
            .transpose()?
            .flatten();
        convert::try_room_to_proto_basic_with_cover(
            room,
            settings,
            member_count,
            Some(creator),
            cover.as_ref(),
            cover_access.as_ref(),
            &self.public_id_codec,
        )
    }

    pub async fn room_to_proto_with_availability_presence_and_loaded_cover(
        &self,
        room: &synctv_core::models::Room,
        settings: Option<&synctv_core::models::RoomSettings>,
        member_count: Option<i32>,
        availability: synctv_core::service::ClientResourceAvailability,
        presence: Option<&synctv_core::service::OnlineRoomStats>,
        creator: Option<synctv_proto::client::UserPublicView>,
    ) -> Result<synctv_proto::client::Room, ApiError> {
        let creator = async {
            match creator {
                Some(creator) => Ok(creator),
                None => self.room_creator_public_view(room).await,
            }
        };
        let (cover, creator) = tokio::join!(
            self.load_stored_file_reference(room.cover_file_reference_id),
            creator,
        );
        let cover = cover?;
        let creator = creator?;
        let cover_access = cover
            .as_ref()
            .map(|file| {
                self.stored_file_reference_access(
                    file,
                    &synctv_core::service::room_cover_upload_policy(),
                )
            })
            .transpose()?
            .flatten();
        convert::try_room_to_proto_with_availability_presence_and_cover(
            room,
            settings,
            member_count,
            availability,
            presence,
            Some(creator),
            cover.as_ref(),
            cover_access.as_ref(),
            &self.public_id_codec,
        )
    }

    pub async fn media_to_proto_for_viewer_with_loaded_cover(
        &self,
        media: &synctv_core::models::Media,
        is_available: bool,
        viewer_id: Option<synctv_core::models::UserId>,
    ) -> Result<synctv_proto::client::Media, ApiError> {
        let (cover, thumbnail) = tokio::join!(
            self.load_stored_file_reference(media.cover_file_reference_id),
            self.load_stored_file_reference(media.thumbnail_file_reference_id),
        );
        let cover = cover?;
        let thumbnail = thumbnail?;
        let cover_access = cover
            .as_ref()
            .map(|file| {
                self.stored_file_reference_access(
                    file,
                    &synctv_core::service::media_cover_upload_policy(),
                )
            })
            .transpose()?
            .flatten();
        let source_cover_url = if cover.is_none() {
            match self
                .room_service
                .media_service()
                .media_source_cover(viewer_id, media)
                .await
            {
                Ok(Some(source_cover)) => {
                    self.source_cover_url(media.room_id, viewer_id, source_cover)?
                }
                Ok(None) => None,
                Err(error) => {
                    tracing::debug!(
                        media_id = %media.id,
                        error = %error,
                        "failed to resolve media source cover"
                    );
                    None
                }
            }
        } else {
            None
        };
        let thumbnail_access = thumbnail
            .as_ref()
            .map(|file| {
                self.stored_file_reference_access(
                    file,
                    &synctv_core::service::media_thumbnail_upload_policy(),
                )
            })
            .transpose()?
            .flatten();
        let mut proto = convert::try_media_to_proto_for_viewer_with_cover(
            media,
            convert::MediaProtoView {
                is_available,
                viewer_id,
                cover: cover.as_ref(),
                cover_access: cover_access.as_ref(),
                thumbnail: thumbnail.as_ref(),
                thumbnail_access: thumbnail_access.as_ref(),
                public_id_codec: &self.public_id_codec,
            },
        )?;
        if proto.cover.is_none() {
            proto.cover = source_cover_url.map(convert::source_url_to_media_cover);
        }
        Ok(proto)
    }

    pub async fn playlist_to_proto_for_viewer_with_loaded_cover(
        &self,
        playlist: &synctv_core::models::Playlist,
        item_count: i32,
        is_available: bool,
        viewer_id: Option<synctv_core::models::UserId>,
    ) -> Result<synctv_proto::client::Playlist, ApiError> {
        let cover = self
            .load_stored_file_reference(playlist.cover_file_reference_id)
            .await?;
        let cover_access = cover
            .as_ref()
            .map(|file| {
                self.stored_file_reference_access(
                    file,
                    &synctv_core::service::playlist_cover_upload_policy(),
                )
            })
            .transpose()?
            .flatten();
        let source_cover_url = if cover.is_none() {
            match self
                .room_service
                .media_service()
                .playlist_source_cover(viewer_id, playlist)
                .await
            {
                Ok(Some(source_cover)) => {
                    self.source_cover_url(playlist.room_id, viewer_id, source_cover)?
                }
                Ok(None) => None,
                Err(error) => {
                    tracing::debug!(
                        playlist_id = %playlist.id,
                        error = %error,
                        "failed to resolve playlist source cover"
                    );
                    None
                }
            }
        } else {
            None
        };
        let mut proto = convert::try_playlist_to_proto_for_viewer_with_cover(
            playlist,
            item_count,
            is_available,
            viewer_id,
            cover.as_ref(),
            cover_access.as_ref(),
            &self.public_id_codec,
        )?;
        if proto.cover.is_none() {
            proto.cover = source_cover_url.map(convert::source_url_to_resource_cover);
        }
        Ok(proto)
    }

    fn parse_room_id(&self, room_id: &str) -> Result<RoomId, ApiError> {
        self.public_id_codec
            .decode_room_id(room_id)
            .map_err(|err| ApiError::InvalidInput(format!("Invalid room_id: {err}")))
    }

    pub fn map_room_access_error(err: synctv_core::Error) -> ApiError {
        match err {
            synctv_core::Error::Authorization(msg) => {
                ApiError::Authorization(format!("Forbidden: {msg}"))
            }
            other => ApiError::from(other),
        }
    }

    pub async fn validate_guest_room_access(
        &self,
        guest_token: &str,
        public_room_id: &str,
    ) -> Result<GuestRoomAccess, ApiError> {
        if guest_token.trim().is_empty() {
            return Err(ApiError::Authentication("Missing guest token".to_string()));
        }

        let room_id = self.parse_room_id(public_room_id)?;
        let room = self
            .room_service
            .get_room(&room_id)
            .await
            .map_err(ApiError::from)?;
        if room.is_banned {
            return Err(ApiError::Authorization(
                "This room has been banned".to_string(),
            ));
        }
        if room.status == RoomStatus::Closed {
            return Err(ApiError::Authorization(
                "This room is closed and not accepting new connections".to_string(),
            ));
        }

        let (room_settings, guest_version) = tokio::try_join!(
            async {
                self.room_service
                    .check_guest_allowed(
                        &room_id,
                        self.runtime_settings_store.as_ref().map(AsRef::as_ref),
                    )
                    .await
                    .map_err(Self::map_room_access_error)
            },
            async {
                self.room_service
                    .get_room_guest_version(&room_id)
                    .await
                    .map_err(ApiError::from)
            },
        )?;
        let validator = GuestTokenValidator::new(
            Arc::new(self.jwt_service.clone()),
            self.user_service.token_blacklist_store(),
            self.user_service.key_builder().clone(),
        );
        let claims = validator
            .validate_with_version_async(guest_token, guest_version)
            .await
            .map_err(ApiError::from)?;
        if claims.room_id().map_err(ApiError::from)? != room_id {
            return Err(ApiError::Authentication(
                "Guest token is not valid for this room".to_string(),
            ));
        }

        let permissions = self
            .room_service
            .guest_permissions_for_settings(&room_settings);
        Ok(GuestRoomAccess {
            room_id,
            guest_id: crate::impls::messaging::guest_public_id(&claims.session_id),
            display_name: crate::impls::messaging::guest_display_name(&claims.session_id),
            session_id: claims.session_id.clone(),
            token_jti: claims.jti.clone(),
            permissions,
            room_guest_version: claims.gv,
        })
    }

    pub async fn room_actor_for_user(
        &self,
        user_id: &synctv_core::models::UserId,
        public_room_id: &str,
    ) -> Result<RoomActor, ApiError> {
        let room_id = self.parse_room_id(public_room_id)?;
        self.room_service
            .check_membership(&room_id, user_id)
            .await
            .map_err(Self::map_room_access_error)?;
        Ok(RoomActor::User {
            room_id,
            user_id: *user_id,
        })
    }

    fn bearer_token_from_authorization(authorization: &str) -> Result<String, ApiError> {
        JwtValidator::extract_bearer_token(authorization).map_err(|_| {
            ApiError::Authentication(
                synctv_common::messages::INVALID_AUTHORIZATION_HEADER.to_string(),
            )
        })
    }

    fn required_authorization(metadata: &RequestMetadata) -> Result<&str, ApiError> {
        metadata.authorization.as_deref().ok_or_else(|| {
            ApiError::Authentication(synctv_common::messages::AUTHENTICATION_REQUIRED.to_string())
        })
    }

    fn is_guest_token(token: &str) -> bool {
        synctv_core::service::JwtService::token_type_hint(token) == Some(TokenType::Guest)
    }

    async fn room_actor_for_bearer_token(
        &self,
        token: &str,
        public_room_id: &str,
    ) -> Result<RoomActor, ApiError> {
        if Self::is_guest_token(token) {
            return self
                .validate_guest_room_access(token, public_room_id)
                .await
                .map(RoomActor::Guest);
        }

        let claims = self.jwt_validator.validate_token(token).map_err(|_| {
            ApiError::Authentication(synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string())
        })?;
        let authenticated = self
            .request_executor()
            .security_check_claims(&claims)
            .await?;
        self.room_actor_for_user(&authenticated.user_id, public_room_id)
            .await
    }

    pub async fn room_actor_for_authorization(
        &self,
        authorization: &str,
        public_room_id: &str,
    ) -> Result<RoomActor, ApiError> {
        let token = Self::bearer_token_from_authorization(authorization)?;
        self.room_actor_for_bearer_token(&token, public_room_id)
            .await
    }

    pub fn require_guest_permission(
        access: &GuestRoomAccess,
        permission: synctv_core::models::RoomPermission,
    ) -> Result<(), ApiError> {
        if access.permissions.has(permission) {
            Ok(())
        } else {
            Err(ApiError::Authorization(
                "Guests do not have permission to access this media resource".to_string(),
            ))
        }
    }

    pub async fn require_room_permission(
        &self,
        actor: &RoomActor,
        permission: synctv_core::models::RoomPermission,
    ) -> Result<(), ApiError> {
        match actor {
            RoomActor::User { room_id, user_id } => self
                .room_service
                .check_permission(room_id, user_id, permission)
                .await
                .map_err(Self::map_room_access_error),
            RoomActor::Guest(access) => Self::require_guest_permission(access, permission),
        }
    }

    pub(crate) fn map_media_lookup_error(
        err: synctv_core::Error,
        not_found_message: &'static str,
    ) -> ApiError {
        match err {
            synctv_core::Error::NotFound(_) => ApiError::NotFound(not_found_message.to_string()),
            other => ApiError::from(other),
        }
    }

    pub(crate) fn map_membership_probe_error(err: synctv_core::Error) -> ApiError {
        ApiError::from(err)
    }

    pub(super) fn map_livestream_backend_error(
        error: &(dyn std::error::Error + 'static),
    ) -> ApiError {
        crate::impls::map_livestream_backend_error(error)
    }

    #[must_use]
    pub fn new_with_runtime(options: ClientApiOptions, runtime: ClientApiRuntime) -> Self {
        let read_pool = options
            .read_pool
            .clone()
            .unwrap_or_else(|| options.user_service.eventually_consistent_pool().clone());
        let review_service = Arc::new(ReviewService::new_with_read_pool(
            options.user_service.pool().clone(),
            read_pool.clone(),
        ));
        let content_report_service = Arc::new(ContentReportService::new_with_read_pool(
            options.user_service.pool().clone(),
            read_pool,
        ));
        let realtime_fanout = runtime.realtime_fanout;
        let realtime_event_service = runtime.realtime_event_service;
        let room_settings_fanout = default_room_settings_fanout_service(realtime_fanout.clone());
        let membership_event_fanout = default_membership_event_fanout_service(
            realtime_fanout.clone(),
            realtime_event_service.clone(),
        );
        let media_fanout = default_media_fanout_service(realtime_fanout.clone());
        let playback_fanout = default_playback_fanout_service(realtime_fanout.clone());
        let playlist_fanout = default_playlist_fanout_service(realtime_fanout.clone());
        let room_cache_fanout = default_room_cache_fanout_service(realtime_fanout.clone());
        let realtime_lifecycle = default_realtime_lifecycle_service(
            options.connection_service.clone(),
            options.live_streaming_infrastructure.clone(),
            realtime_fanout.clone(),
        );
        let room_lifecycle_fanout = default_room_lifecycle_fanout_service_with_realtime(
            realtime_fanout.clone(),
            realtime_event_service.clone(),
        );
        let chat_event_dispatcher = runtime.chat_event_dispatcher;
        Self {
            clock: runtime.clock,
            user_service: options.user_service,
            room_service: options.room_service,
            chat_service: options.chat_service,
            content_report_service,
            review_service,
            connection_service: options.connection_service,
            presence_service: runtime.presence_service,
            runtime_settings: options.runtime_settings,
            publish_key_service: options.publish_key_service,
            jwt_service: options.jwt_service,
            live_streaming_infrastructure: options.live_streaming_infrastructure,
            runtime_settings_store: options.runtime_settings_store,
            realtime_fanout,
            room_settings_fanout,
            membership_event_fanout,
            media_fanout,
            playback_fanout,
            playlist_fanout,
            room_cache_fanout,
            realtime_lifecycle,
            room_lifecycle_fanout,
            realtime_event_service,
            chat_event_dispatcher,
            redis_runtime: runtime.redis_runtime,
            builtin_stun_url: runtime.builtin_stun_url,
            webrtc_status: runtime.webrtc_status,
            provider_access_service: runtime.provider_access_service,
            signing_key: runtime.signing_key,
            provider_stores: options.provider_stores,
            jwt_validator: runtime.jwt_validator,
            public_id_codec: options.public_id_codec,
            playback_duration_probe: runtime.playback_duration_probe,
            request_executor: runtime.request_executor,
            email_api: options.email_api,
            passkey_service: options.passkey_service,
            ws_ticket_service: runtime.ws_ticket_service,
        }
    }

    /// Resolve a fresh Redis `ConnectionManager` clone from the shared `RwLock`.
    ///
    /// Returns `None` when Redis is not configured. The returned clone is cheap
    /// (internally Arc-backed) and always points to the current Redis master,
    /// even after a Sentinel failover.
    pub async fn resolve_redis_conn(&self) -> Option<redis::aio::ConnectionManager> {
        match &self.redis_runtime {
            Some(runtime) => match runtime.snapshot().await {
                Ok(conn) => Some(conn),
                Err(error) => {
                    tracing::warn!(error = %error, "Redis connection snapshot failed");
                    None
                }
            },
            None => None,
        }
    }

    fn request_executor(&self) -> &Arc<RequestExecutor> {
        &self.request_executor
    }

    pub fn execute_public_endpoint<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        self.request_executor()
            .execute_public(metadata, category, move || async move {
                operation().await.map_err(Into::into)
            })
    }

    pub fn execute_scoped_public_endpoint<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        scope: EndpointRateLimitScope,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce() -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        let metadata = metadata.clone().with_endpoint_scope(Some(scope));
        Box::pin(async move {
            self.execute_public_endpoint(&metadata, category, operation)
                .await
        })
    }

    pub fn execute_public_endpoint_with_context<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(ApiRequestContext) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        self.request_executor().execute_public_with_context(
                metadata,
                category,
                move |request_context| async move {
                    operation(request_context).await.map_err(Into::into)
                },
            )
    }

    pub fn execute_public_endpoint_with_control<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(synctv_core::provider::ExecutionControl) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        self.request_executor().execute_public_with_control(
                metadata,
                category,
                move |request_control| async move {
                    operation(request_control).await.map_err(Into::into)
                },
            )
    }

    pub fn execute_scoped_public_endpoint_with_control<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        scope: EndpointRateLimitScope,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(synctv_core::provider::ExecutionControl) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        let metadata = metadata.clone().with_endpoint_scope(Some(scope));
        Box::pin(async move {
            self.execute_public_endpoint_with_control(&metadata, category, operation)
                .await
        })
    }

    pub fn execute_optional_user_endpoint<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(Option<synctv_core::service::AuthenticatedToken>) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        self.request_executor().execute_optional_user(
            metadata,
            category,
            move |authenticated| async move { operation(authenticated).await.map_err(Into::into) },
        )
    }

    pub fn execute_optional_user_endpoint_with_context<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(ApiRequestContext, Option<synctv_core::service::AuthenticatedToken>) -> Fut
            + Send
            + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        self.request_executor().execute_optional_user_with_context(
            metadata,
            category,
            move |request_context, authenticated| async move {
                operation(request_context, authenticated)
                    .await
                    .map_err(Into::into)
            },
        )
    }

    pub fn execute_optional_user_endpoint_with_control<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(
                synctv_core::provider::ExecutionControl,
                Option<synctv_core::service::AuthenticatedToken>,
            ) -> Fut
            + Send
            + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        self.request_executor().execute_optional_user_with_control(
            metadata,
            category,
            move |request_control, authenticated| async move {
                operation(request_control, authenticated)
                    .await
                    .map_err(Into::into)
            },
        )
    }

    pub fn execute_user_endpoint<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(synctv_core::service::AuthenticatedToken) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        self.request_executor()
            .execute_user(metadata, category, move |authenticated| async move {
                operation(authenticated).await.map_err(Into::into)
            })
    }

    pub fn execute_scoped_user_endpoint<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        scope: EndpointRateLimitScope,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(synctv_core::service::AuthenticatedToken) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        let metadata = metadata.clone().with_endpoint_scope(Some(scope));
        Box::pin(async move {
            self.execute_user_endpoint(&metadata, category, operation)
                .await
        })
    }

    pub fn execute_user_endpoint_with_context<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(ApiRequestContext, synctv_core::service::AuthenticatedToken) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        self.request_executor().execute_user_with_context(
            metadata,
            category,
            move |request_context, authenticated| async move {
                operation(request_context, authenticated)
                    .await
                    .map_err(Into::into)
            },
        )
    }

    pub fn execute_user_endpoint_with_control<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(
                synctv_core::provider::ExecutionControl,
                synctv_core::service::AuthenticatedToken,
            ) -> Fut
            + Send
            + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        self.request_executor().execute_user_with_control(
            metadata,
            category,
            move |request_control, authenticated| async move {
                operation(request_control, authenticated)
                    .await
                    .map_err(Into::into)
            },
        )
    }

    pub fn execute_scoped_user_endpoint_with_control<'a, T, E, F, Fut>(
        &'a self,
        metadata: &'a RequestMetadata,
        category: EndpointRateLimitCategory,
        scope: EndpointRateLimitScope,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(
                synctv_core::provider::ExecutionControl,
                synctv_core::service::AuthenticatedToken,
            ) -> Fut
            + Send
            + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        let metadata = metadata.clone().with_endpoint_scope(Some(scope));
        Box::pin(async move {
            self.execute_user_endpoint_with_control(&metadata, category, operation)
                .await
        })
    }

    pub fn execute_room_actor_endpoint<'a, T, E, F, Fut>(
        client_api: Arc<Self>,
        metadata: &'a RequestMetadata,
        public_room_id: String,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(Arc<Self>, RoomActor) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        Box::pin(async move {
            let token = match Self::required_authorization(metadata) {
                Ok(authorization) => Some(Self::bearer_token_from_authorization(authorization)?),
                Err(error) => match error {
                    ApiError::Authentication(message)
                        if message == synctv_common::messages::AUTHENTICATION_REQUIRED =>
                    {
                        None
                    }
                    other => return Err(other),
                },
            };

            let executor = client_api.clone();
            match token {
                Some(token) if Self::is_guest_token(&token) => {
                    executor
                        .execute_public_endpoint(metadata, category, move || {
                            let client_api = client_api.clone();
                            async move {
                                let access = client_api
                                    .validate_guest_room_access(&token, &public_room_id)
                                    .await?;
                                operation(client_api, RoomActor::Guest(access))
                                    .await
                                    .map_err(Into::into)
                            }
                        })
                        .await
                }
                Some(token) => {
                    let executor = executor.request_executor();
                    executor
                        .execute_authenticated_token_with_control(
                            metadata,
                            category,
                            &token,
                            move |_, authenticated| {
                                let client_api = client_api.clone();
                                async move {
                                    let actor = client_api
                                        .room_actor_for_user(
                                            &authenticated.user_id,
                                            &public_room_id,
                                        )
                                        .await?;
                                    operation(client_api, actor).await.map_err(Into::into)
                                }
                            },
                        )
                        .await
                }
                None => {
                    executor
                        .execute_public_endpoint(metadata, category, || async move {
                            Err::<T, ApiError>(ApiError::Authentication(
                                synctv_common::messages::AUTHENTICATION_REQUIRED.to_string(),
                            ))
                        })
                        .await
                }
            }
        })
    }

    pub fn execute_scoped_room_actor_endpoint<'a, T, E, F, Fut>(
        client_api: Arc<Self>,
        metadata: &'a RequestMetadata,
        public_room_id: String,
        category: EndpointRateLimitCategory,
        scope: EndpointRateLimitScope,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(Arc<Self>, RoomActor) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        let metadata = metadata.clone().with_endpoint_scope(Some(scope));
        Box::pin(async move {
            Self::execute_room_actor_endpoint(
                client_api,
                &metadata,
                public_room_id,
                category,
                operation,
            )
            .await
        })
    }

    pub fn execute_room_actor_endpoint_with_control<'a, T, E, F, Fut>(
        client_api: Arc<Self>,
        metadata: &'a RequestMetadata,
        public_room_id: String,
        category: EndpointRateLimitCategory,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(Arc<Self>, synctv_core::provider::ExecutionControl, RoomActor) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        Box::pin(async move {
            let token = match Self::required_authorization(metadata) {
                Ok(authorization) => Some(Self::bearer_token_from_authorization(authorization)?),
                Err(error) => match error {
                    ApiError::Authentication(message)
                        if message == synctv_common::messages::AUTHENTICATION_REQUIRED =>
                    {
                        None
                    }
                    other => return Err(other),
                },
            };

            let executor = client_api.clone();
            match token {
                Some(token) if Self::is_guest_token(&token) => {
                    executor
                        .execute_public_endpoint_with_control(
                            metadata,
                            category,
                            move |request_control| {
                                let client_api = client_api.clone();
                                async move {
                                    let access = client_api
                                        .validate_guest_room_access(&token, &public_room_id)
                                        .await?;
                                    operation(client_api, request_control, RoomActor::Guest(access))
                                        .await
                                        .map_err(Into::into)
                                }
                            },
                        )
                        .await
                }
                Some(token) => {
                    let executor = executor.request_executor();
                    executor
                        .execute_authenticated_token_with_control(
                            metadata,
                            category,
                            &token,
                            move |request_control, authenticated| {
                                let client_api = client_api.clone();
                                async move {
                                    let actor = client_api
                                        .room_actor_for_user(
                                            &authenticated.user_id,
                                            &public_room_id,
                                        )
                                        .await?;
                                    operation(client_api, request_control, actor)
                                        .await
                                        .map_err(Into::into)
                                }
                            },
                        )
                        .await
                }
                None => {
                    executor
                        .execute_public_endpoint_with_control(metadata, category, |_| async move {
                            Err::<T, ApiError>(ApiError::Authentication(
                                synctv_common::messages::AUTHENTICATION_REQUIRED.to_string(),
                            ))
                        })
                        .await
                }
            }
        })
    }

    pub fn execute_scoped_room_actor_endpoint_with_control<'a, T, E, F, Fut>(
        client_api: Arc<Self>,
        metadata: &'a RequestMetadata,
        public_room_id: String,
        category: EndpointRateLimitCategory,
        scope: EndpointRateLimitScope,
        operation: F,
    ) -> BoxFuture<'a, Result<T, ApiError>>
    where
        T: Send + 'a,
        E: Into<ApiError> + Send + 'a,
        F: FnOnce(Arc<Self>, synctv_core::provider::ExecutionControl, RoomActor) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, E>> + Send + 'a,
    {
        let metadata = metadata.clone().with_endpoint_scope(Some(scope));
        Box::pin(async move {
            Self::execute_room_actor_endpoint_with_control(
                client_api,
                &metadata,
                public_room_id,
                category,
                operation,
            )
            .await
        })
    }
}
