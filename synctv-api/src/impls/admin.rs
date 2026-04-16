//! Admin API Implementation
//!
//! Unified implementation for all admin API operations.
//! Used by both HTTP and gRPC handlers.

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use std::cmp::Ordering;
use std::sync::Arc;
use synctv_core::models::{
    MediaListQuery as CoreMediaListQuery, MediaListSortBy as CoreMediaListSortBy, PlaylistId,
    PlaylistListQuery as CorePlaylistListQuery, PlaylistListSortBy as CorePlaylistListSortBy,
    RoomId, SortDirection as CoreSortDirection, UserId, UserRole, UserStatus,
};
use synctv_core::service::{
    AuditService, AuthorizedAdminActor, EmailService, RemoteProviderManager, RoomService,
    SettingsRegistry, SettingsService, UserService,
};
use synctv_core::Error as CoreError;
use synctv_livestream::api::LiveStreamingInfrastructure;

use super::client::convert::{
    bilibili_live_danmaku_for_static_media, direct_url_embedded_playback_result_to_model,
    media_to_proto, media_to_proto_with_availability, member_status_to_proto,
    playback_snapshot_to_proto, playback_state_to_proto, playlist_path_node_to_proto,
    playlist_to_proto, playlist_to_proto_with_availability, provider_playback_info_to_model,
    room_to_proto_basic, sign_local_bilibili_danmaku_urls, user_status_to_proto,
};
use super::ApiError;
use crate::cluster_fanout::{default_cluster_fanout_service, ClusterFanoutService};
use crate::fanout::{default_room_settings_fanout_service, RoomSettingsFanoutService};
use crate::impls::client::media::{build_move_media_fanout_plan, publish_move_media_fanout};
use crate::impls::client::proto_role_to_room_role;
use crate::impls::playback_snapshot::{
    dynamic_playback_snapshot_version, playback_snapshot_expires_at,
    static_playback_snapshot_version,
};
use crate::media_fanout::{default_media_fanout_service, MediaFanoutService};
use crate::member_fanout::{default_member_fanout_service, MemberFanoutService};
use crate::membership_event_fanout::{
    default_membership_event_fanout_service, MembershipEventFanoutService,
};
use crate::playlist_fanout::{default_playlist_fanout_service, PlaylistFanoutService};
use crate::realtime_lifecycle::{
    default_realtime_lifecycle_service, DeletedRoomFanoutReservation, RealtimeLifecycleService,
};
use crate::room_cache_fanout::{default_room_cache_fanout_service, RoomCacheFanoutService};
use crate::room_lifecycle_fanout::{
    default_room_lifecycle_fanout_service, RoomLifecycleFanoutService,
};
use crate::runtime::{RealtimeConnectionService, RealtimeEventService};

/// HTTP request context for audit logging (IP address and User-Agent).
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

struct ReservedRoomOwnerInactiveFanout {
    room_id: RoomId,
    reservation: Option<crate::impls::ClusterEventPublishReservation>,
}

pub const LOCAL_MANAGEMENT_ACTOR_USER_ID: &str = "mgmt_local01";

fn page_i32_to_usize(value: i32) -> usize {
    usize::try_from(value.max(1)).unwrap_or(usize::MAX)
}

fn page_size_i32_to_usize(value: i32, max: i32) -> usize {
    usize::try_from(value.clamp(1, max)).unwrap_or(usize::MAX)
}

fn page_i32_to_u32(value: i32) -> u32 {
    value.max(1).cast_unsigned()
}

fn page_size_i32_to_u32(value: i32, max: i32) -> u32 {
    value.clamp(1, max).cast_unsigned()
}

fn usize_to_i32_saturating(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn i64_to_i32_saturating(value: i64) -> i32 {
    i32::try_from(value).unwrap_or_else(|_| {
        if value.is_negative() {
            i32::MIN
        } else {
            i32::MAX
        }
    })
}

fn usize_to_i64_saturating(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn i64_to_usize_saturating(value: i64) -> usize {
    if value.is_negative() {
        0
    } else {
        usize::try_from(value).unwrap_or(usize::MAX)
    }
}

fn u64_to_i64_saturating(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

async fn load_creator_status_map(
    user_service: &UserService,
    creator_ids: &[UserId],
) -> Result<std::collections::HashMap<UserId, UserStatus>, ApiError> {
    let users = user_service
        .get_users_by_ids(creator_ids)
        .await
        .map_err(ApiError::from)?;
    Ok(users
        .into_iter()
        .map(|user| (user.id, user.status))
        .collect())
}

async fn load_room_creator_status(
    user_service: &UserService,
    room: &synctv_core::models::Room,
) -> Result<UserStatus, ApiError> {
    match user_service.get_user(&room.created_by).await {
        Ok(user) => Ok(user.status),
        Err(synctv_core::Error::NotFound(_)) => Ok(UserStatus::Rejected),
        Err(error) => Err(ApiError::from(error)),
    }
}

fn map_batch_result_error(error: impl Into<ApiError>) -> String {
    let error = error.into();
    match error.classify() {
        super::ErrorKind::Internal => "Operation failed due to an internal error".to_string(),
        super::ErrorKind::ServiceUnavailable => {
            "Operation failed because the service is temporarily unavailable".to_string()
        }
        _ => error.message().to_string(),
    }
}

fn live_streaming_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable("Live streaming is not available on this server.".to_string())
}

fn parse_batch_user_ids(user_ids: &[String]) -> Result<Vec<UserId>, ApiError> {
    if user_ids.is_empty() {
        return Err(ApiError::InvalidInput(
            "user_ids cannot be empty".to_string(),
        ));
    }
    if user_ids.len() > UserService::BATCH_SIZE_LIMIT {
        return Err(ApiError::InvalidInput(format!(
            "Batch size {} exceeds limit of {}",
            user_ids.len(),
            UserService::BATCH_SIZE_LIMIT
        )));
    }

    user_ids
        .iter()
        .map(|user_id| crate::impls::parse_user_id_param(user_id, "user_ids"))
        .collect()
}

fn publish_key_service_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable("Publish key service is not available on this server.".to_string())
}

struct AdminActor {
    username: String,
    role: UserRole,
}

/// Result of validating an admin user's authentication.
///
/// Returned by [`validate_admin_auth`] and consumed by both HTTP and gRPC
/// admin auth layers.
pub struct ValidatedAdmin {
    pub user_id: UserId,
    pub role: UserRole,
}

/// Shared admin auth validation: look up the user, check banned/deleted
/// status, and verify the token has not been invalidated by a password change.
///
/// Both transports must resolve `user_id` and `token_iat` from their own
/// auth mechanism (HTTP Authorization header / gRPC interceptor) before
/// calling this function.
pub async fn validate_admin_auth(
    user_service: &UserService,
    user_id: UserId,
    token_pv: i32,
    _token_iat: i64,
) -> Result<ValidatedAdmin, ApiError> {
    let user = user_service.get_user(&user_id).await.map_err(|e| {
        tracing::debug!(
            user_id = %user_id.as_str(),
            error = %e,
            "Admin auth rejected: failed to look up user"
        );
        AdminApiImpl::map_admin_auth_user_lookup_error(e)
    })?;

    if user.is_deleted()
        || user.status == UserStatus::Banned
        || user.status == UserStatus::Pending
        || user.status == UserStatus::Rejected
    {
        tracing::debug!(
            user_id = %user_id.as_str(),
            status = ?user.status,
            deleted = user.is_deleted(),
            "Admin auth rejected: user is deleted or not in an active status"
        );
        return Err(ApiError::Authentication(
            "Authentication failed".to_string(),
        ));
    }

    // Check password version
    if token_pv < user.password_version {
        tracing::debug!(
            user_id = %user_id.as_str(),
            token_pv = token_pv,
            current_pv = user.password_version,
            "Admin auth rejected: token password version outdated"
        );
        return Err(ApiError::Authentication(
            "Token invalidated due to password change. Please log in again.".to_string(),
        ));
    }

    Ok(ValidatedAdmin {
        user_id,
        role: user.role,
    })
}

/// Admin API implementation
#[derive(Clone)]
pub struct AdminApiImpl {
    pub room_service: Arc<RoomService>,
    pub user_service: Arc<UserService>,
    pub settings_service: Arc<SettingsService>,
    pub settings_registry: Option<Arc<SettingsRegistry>>,
    pub email_service: Arc<EmailService>,
    pub connection_service: Arc<dyn RealtimeConnectionService>,
    pub provider_instance_manager: Arc<RemoteProviderManager>,
    pub live_streaming_infrastructure: Option<Arc<LiveStreamingInfrastructure>>,
    pub publish_key_service: Option<Arc<dyn synctv_core::service::StreamingPublishKeyService>>,
    pub config: Arc<synctv_core::Config>,
    pub cluster_fanout: Arc<dyn ClusterFanoutService>,
    pub room_settings_fanout: Arc<dyn RoomSettingsFanoutService>,
    pub member_fanout: Arc<dyn MemberFanoutService>,
    pub membership_event_fanout: Arc<dyn MembershipEventFanoutService>,
    pub media_fanout: Arc<dyn MediaFanoutService>,
    pub playlist_fanout: Arc<dyn PlaylistFanoutService>,
    pub room_cache_fanout: Arc<dyn RoomCacheFanoutService>,
    pub realtime_lifecycle: Arc<dyn RealtimeLifecycleService>,
    pub room_lifecycle_fanout: Arc<dyn RoomLifecycleFanoutService>,
    pub realtime_event_service: Option<Arc<dyn RealtimeEventService>>,
    pub audit_service: Arc<AuditService>,
    pub provider_stores: Option<Arc<dyn synctv_core::provider::store::ProviderStoreResolver>>,
}

fn normalize_non_empty_filter(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed.to_string())
}

fn map_client_sort_direction(sort_direction: i32) -> CoreSortDirection {
    match crate::proto::client::SortDirection::try_from(sort_direction) {
        Ok(crate::proto::client::SortDirection::Desc) => CoreSortDirection::Desc,
        _ => CoreSortDirection::Asc,
    }
}

fn map_admin_playlist_sort(sort_by: i32) -> CorePlaylistListSortBy {
    match crate::proto::client::PlaylistListSortBy::try_from(sort_by)
        .unwrap_or(crate::proto::client::PlaylistListSortBy::Position)
    {
        crate::proto::client::PlaylistListSortBy::Name => CorePlaylistListSortBy::Name,
        crate::proto::client::PlaylistListSortBy::CreatedAt => CorePlaylistListSortBy::CreatedAt,
        crate::proto::client::PlaylistListSortBy::UpdatedAt => CorePlaylistListSortBy::UpdatedAt,
        _ => CorePlaylistListSortBy::Position,
    }
}

fn map_admin_playlist_sort_from_media_sort(sort_by: i32) -> CorePlaylistListSortBy {
    match crate::proto::client::MediaListSortBy::try_from(sort_by)
        .unwrap_or(crate::proto::client::MediaListSortBy::Position)
    {
        crate::proto::client::MediaListSortBy::Name => CorePlaylistListSortBy::Name,
        crate::proto::client::MediaListSortBy::AddedAt => CorePlaylistListSortBy::CreatedAt,
        crate::proto::client::MediaListSortBy::UpdatedAt => CorePlaylistListSortBy::UpdatedAt,
        _ => CorePlaylistListSortBy::Position,
    }
}

fn map_admin_media_sort(sort_by: i32) -> CoreMediaListSortBy {
    match crate::proto::client::MediaListSortBy::try_from(sort_by)
        .unwrap_or(crate::proto::client::MediaListSortBy::Position)
    {
        crate::proto::client::MediaListSortBy::Name => CoreMediaListSortBy::Name,
        crate::proto::client::MediaListSortBy::AddedAt => CoreMediaListSortBy::AddedAt,
        crate::proto::client::MediaListSortBy::UpdatedAt => CoreMediaListSortBy::UpdatedAt,
        crate::proto::client::MediaListSortBy::SourceProvider => {
            CoreMediaListSortBy::SourceProvider
        }
        crate::proto::client::MediaListSortBy::ProviderInstanceName => {
            CoreMediaListSortBy::ProviderInstanceName
        }
        _ => CoreMediaListSortBy::Position,
    }
}

fn map_resource_availability_filter(filter: i32) -> Option<bool> {
    match crate::proto::client::ResourceAvailabilityFilter::try_from(filter)
        .unwrap_or(crate::proto::client::ResourceAvailabilityFilter::All)
    {
        crate::proto::client::ResourceAvailabilityFilter::All => None,
        crate::proto::client::ResourceAvailabilityFilter::Available => Some(true),
        crate::proto::client::ResourceAvailabilityFilter::Unavailable => Some(false),
    }
}

fn paginate_vec<T>(items: Vec<T>, page: i32, page_size: i32) -> Vec<T> {
    let page = page_i32_to_usize(page);
    let page_size = page_size_i32_to_usize(page_size, 100);
    let offset = (page - 1) * page_size;
    items.into_iter().skip(offset).take(page_size).collect()
}

fn build_room_stream_list_response(
    mut media_ids: Vec<String>,
    req: &crate::proto::client::ListRoomStreamsRequest,
) -> crate::proto::client::ListRoomStreamsResponse {
    if let Some(search) = (!req.search.trim().is_empty()).then(|| req.search.to_ascii_lowercase()) {
        media_ids.retain(|media_id| media_id.to_ascii_lowercase().contains(&search));
    }

    media_ids.sort_unstable();
    if matches!(
        crate::proto::client::SortDirection::try_from(req.sort_direction),
        Ok(crate::proto::client::SortDirection::Desc)
    ) {
        media_ids.reverse();
    }

    let total = usize_to_i32_saturating(media_ids.len());
    let streams = paginate_vec(media_ids, req.page, req.page_size)
        .into_iter()
        .map(|media_id| crate::proto::client::StreamEntry {
            media_id,
            active: true,
        })
        .collect::<Vec<_>>();

    crate::proto::client::ListRoomStreamsResponse { total, streams }
}

fn compare_active_streams(
    left: &crate::proto::admin::ActiveStreamInfo,
    right: &crate::proto::admin::ActiveStreamInfo,
    sort_by: crate::proto::admin::ActiveStreamListSortBy,
    sort_direction: crate::proto::admin::SortDirection,
) -> Ordering {
    let ordering = match sort_by {
        crate::proto::admin::ActiveStreamListSortBy::RoomId => left
            .room_id
            .cmp(&right.room_id)
            .then_with(|| left.media_id.cmp(&right.media_id)),
        crate::proto::admin::ActiveStreamListSortBy::MediaId => left
            .media_id
            .cmp(&right.media_id)
            .then_with(|| left.room_id.cmp(&right.room_id)),
        crate::proto::admin::ActiveStreamListSortBy::UserId => left
            .user_id
            .cmp(&right.user_id)
            .then_with(|| left.started_at.cmp(&right.started_at)),
        crate::proto::admin::ActiveStreamListSortBy::NodeId => left
            .node_id
            .cmp(&right.node_id)
            .then_with(|| left.started_at.cmp(&right.started_at)),
        _ => left
            .started_at
            .cmp(&right.started_at)
            .then_with(|| left.room_id.cmp(&right.room_id))
            .then_with(|| left.media_id.cmp(&right.media_id)),
    };

    match sort_direction {
        crate::proto::admin::SortDirection::Asc => ordering,
        _ => ordering.reverse(),
    }
}

impl AdminApiImpl {
    async fn reserve_room_cache_invalidation(
        &self,
    ) -> Result<Option<crate::impls::ClusterEventPublishReservation>, ApiError> {
        self.room_cache_fanout.reserve_invalidation().await
    }

    fn publish_room_cache_invalidation(
        &self,
        reservation: Option<crate::impls::ClusterEventPublishReservation>,
        room_id: &RoomId,
    ) {
        self.room_cache_fanout
            .publish_invalidation(reservation, room_id);
    }

    fn map_admin_auth_user_lookup_error(err: synctv_core::Error) -> ApiError {
        match err {
            synctv_core::Error::NotFound(_) => {
                ApiError::Authentication("Authentication failed".to_string())
            }
            other => ApiError::from(other),
        }
    }

    fn map_target_user_lookup_error(err: synctv_core::Error) -> ApiError {
        match err {
            synctv_core::Error::NotFound(_) => ApiError::NotFound("User not found".to_string()),
            other => ApiError::from(other),
        }
    }

    async fn ban_user_with_cleanup(
        &self,
        target_user_id: &UserId,
        admin_user_id: &UserId,
        caller_role: UserRole,
    ) -> Result<synctv_core::models::User, ApiError> {
        let affected_room_ids = list_active_user_room_ids(&self.room_service, target_user_id)
            .await
            .map_err(ApiError::from)?;
        let owned_room_ids = list_owned_room_ids(&self.room_service, target_user_id)
            .await
            .map_err(ApiError::from)?;
        let mut owner_inactive_fanout = Vec::with_capacity(owned_room_ids.len());
        for room_id in owned_room_ids {
            owner_inactive_fanout.push(ReservedRoomOwnerInactiveFanout {
                room_id,
                reservation: self
                    .room_lifecycle_fanout
                    .reserve_room_owner_inactive()
                    .await?,
            });
        }
        let user = self
            .user_service
            .get_user(target_user_id)
            .await
            .map_err(ApiError::from)?;

        if user.role == UserRole::Root && caller_role != UserRole::Root {
            return Err(ApiError::Authorization(
                "Only root users can ban other root users".to_string(),
            ));
        }

        if user.role == UserRole::Admin && caller_role != UserRole::Root {
            return Err(ApiError::Authorization(
                "Only root users can ban admin users".to_string(),
            ));
        }

        if user.status == UserStatus::Banned {
            return Err(ApiError::InvalidInput("User is already banned".to_string()));
        }

        let updated = self
            .user_service
            .ban_user_and_cleanup_memberships(target_user_id)
            .await
            .map_err(ApiError::from)?;

        self.room_service
            .playback_service()
            .reset_playback_for_creator(target_user_id)
            .await
            .map_err(ApiError::from)?;

        for reserved_room in owner_inactive_fanout {
            let room_id = reserved_room.room_id;
            self.room_service
                .finalize_room_owner_inactive_after_commit(&room_id)
                .await;

            self.room_lifecycle_fanout.publish_room_owner_inactive(
                reserved_room.reservation,
                &room_id,
                target_user_id,
                admin_user_id,
            );

            self.realtime_lifecycle
                .disconnect_room(&room_id, "room_owner_inactive")
                .await;
        }

        invalidate_user_room_permission_caches(
            &self.room_service,
            target_user_id,
            &affected_room_ids,
        )
        .await;

        self.realtime_lifecycle
            .disconnect_user(target_user_id, "user_banned")
            .await;

        Ok(updated)
    }

    async fn load_admin_actor(&self, admin_user_id: &UserId) -> Result<AdminActor, ApiError> {
        if admin_user_id.as_str() == LOCAL_MANAGEMENT_ACTOR_USER_ID {
            return Ok(AdminActor {
                username: "local-management".to_string(),
                role: UserRole::Root,
            });
        }

        let user = self.user_service.get_user(admin_user_id).await?;
        Ok(AdminActor {
            username: user.username,
            role: user.role,
        })
    }

    async fn require_admin_actor(&self, admin_user_id: &UserId) -> Result<AdminActor, ApiError> {
        let actor = self.load_admin_actor(admin_user_id).await?;
        if !actor.role.is_admin_or_above() {
            return Err(ApiError::Authorization(
                "Admin role required for this operation".to_string(),
            ));
        }
        Ok(actor)
    }

    async fn require_authorized_admin_actor(
        &self,
        admin_user_id: &UserId,
    ) -> Result<AuthorizedAdminActor, ApiError> {
        let actor = self.require_admin_actor(admin_user_id).await?;
        AuthorizedAdminActor::new(admin_user_id.clone(), actor.username, actor.role)
            .map_err(ApiError::from)
    }

    fn effective_settings_by_key(
        &self,
    ) -> Result<std::collections::BTreeMap<String, String>, ApiError> {
        let mut effective = std::collections::BTreeMap::new();
        let mut registered_keys = None;

        if let Some(registry) = &self.settings_registry {
            let defaults = registry.storage.registered_defaults();
            registered_keys = Some(
                defaults
                    .iter()
                    .map(|(key, _)| key.clone())
                    .collect::<std::collections::HashSet<_>>(),
            );
            effective.extend(defaults);
        }

        for setting in self.settings_service.get_all().map_err(ApiError::from)? {
            if let Some(registered_keys) = &registered_keys {
                if registered_keys.contains(&setting.key) {
                    effective.insert(setting.key, setting.value);
                    continue;
                }
                tracing::warn!(
                    key = %setting.key,
                    group = %setting.group_name,
                    "Ignoring unsupported persisted setting during admin settings projection"
                );
                continue;
            }
            effective.insert(setting.key, setting.value);
        }

        Ok(effective)
    }

    async fn build_static_media_playback_result(
        &self,
        user_id: &str,
        room_id: &str,
        media: synctv_core::models::Media,
    ) -> Result<crate::proto::client::PlaybackSnapshot, ApiError> {
        let signing_key =
            synctv_core::service::ProxySigningKey::derive_from(self.config.jwt.secret.as_bytes());
        if let Some(mut embedded_result) = direct_url_embedded_playback_result_to_model(&media)? {
            sign_local_bilibili_danmaku_urls(
                &mut embedded_result,
                user_id,
                Some(&signing_key),
                None,
            );
            let mut snapshot = playback_snapshot_to_proto(&embedded_result);
            snapshot.version = static_playback_snapshot_version(&media);
            snapshot.expires_at = playback_snapshot_expires_at(&snapshot);
            return Ok(snapshot);
        }

        let providers_manager = self.room_service.media_service().providers_manager();
        let provider = if let Some(instance_name) =
            (!media.provider_instance_name.trim().is_empty())
                .then_some(media.provider_instance_name.trim())
        {
            providers_manager.get(instance_name).await.ok_or_else(|| {
                ApiError::NotFound(format!("Provider instance '{instance_name}' not found"))
            })?
        } else {
            let provider_name = media.source_provider.trim();
            if provider_name.is_empty() {
                return Err(ApiError::Internal(format!(
                    "Static media '{}' is missing source_provider",
                    media.id
                )));
            }
            providers_manager
                .get_by_type(provider_name)
                .await
                .ok_or_else(|| {
                    ApiError::NotFound(format!("Provider '{provider_name}' not found"))
                })?
        };

        let mut ctx = synctv_core::provider::ProviderContext::new("synctv")
            .with_user_id(user_id)
            .with_room_id(room_id)
            .with_signing_key(&signing_key);
        if let Some(repo) = self.room_service.media_service().credential_repo() {
            ctx = ctx.with_credential_repo(repo.as_ref());
        }
        if let Some(enc) = self.room_service.media_service().credential_encryption() {
            ctx = ctx.with_credential_encryption(enc);
        }
        if let Some(stores) = &self.provider_stores {
            ctx = ctx.with_store(stores.load(provider.name()));
        }
        let provider_result = provider
            .generate_playback(&ctx, &media.source_config)
            .await
            .map_err(ApiError::from)?;
        let default_mode_expires_at = provider_result
            .playback_infos
            .get(&provider_result.default_mode)
            .and_then(|info| info.expires_at);
        let live_danmaku = bilibili_live_danmaku_for_static_media(
            &media,
            user_id,
            Some(&signing_key),
            default_mode_expires_at,
        );

        let mut builder = synctv_core::models::media::PlaybackResult::builder(
            media.playlist_id.clone(),
            media.room_id.clone(),
            media.name.clone(),
            media.position,
        )
        .id(media.id.clone())
        .default_mode(provider_result.default_mode.clone());

        for (mode_name, provider_info) in provider_result.playback_infos {
            let mut info = provider_playback_info_to_model(&provider_info);
            if let Some(ref danmaku) = live_danmaku {
                info.danmakus.push(danmaku.clone());
            }
            builder = builder.add_mode(mode_name, info);
        }
        for (key, value) in provider_result.metadata {
            builder = builder.add_metadata(key, value);
        }

        let mut full_result = builder
            .build()
            .ok_or_else(|| ApiError::Internal("Failed to build PlaybackResult".to_string()))?;
        sign_local_bilibili_danmaku_urls(
            &mut full_result,
            user_id,
            Some(&signing_key),
            default_mode_expires_at,
        );
        let mut snapshot = playback_snapshot_to_proto(&full_result);
        snapshot.version = static_playback_snapshot_version(&media);
        snapshot.expires_at = playback_snapshot_expires_at(&snapshot);
        Ok(snapshot)
    }

    async fn build_dynamic_playlist_playback_result(
        &self,
        user_id: &str,
        room_id: &str,
        room_id_model: &RoomId,
        user_id_model: &UserId,
        playlist_id: &PlaylistId,
        target: &[u8],
    ) -> Result<crate::proto::client::PlaybackSnapshot, ApiError> {
        let item = self
            .room_service
            .media_service()
            .resolve_dynamic_playlist_item(
                room_id_model.clone(),
                user_id_model.clone(),
                playlist_id,
                target,
            )
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound("Dynamic playlist item not found".to_string()))?;

        let playlist = self
            .room_service
            .playlist_service()
            .get_playlist(playlist_id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound("Playlist not found".to_string()))?;

        let provider_name = playlist
            .source_provider
            .as_deref()
            .ok_or_else(|| ApiError::Internal("Dynamic playlist missing provider".to_string()))?;
        let providers_manager = self.room_service.media_service().providers_manager();
        let bound_instance = playlist.provider_instance_name.as_deref().and_then(|name| {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        });
        let provider = if let Some(instance_name) = bound_instance {
            providers_manager.get(instance_name).await.ok_or_else(|| {
                ApiError::NotFound(format!("Provider instance '{instance_name}' not found"))
            })?
        } else {
            providers_manager
                .get_by_type(provider_name)
                .await
                .ok_or_else(|| {
                    ApiError::NotFound(format!("Provider '{provider_name}' not found"))
                })?
        };

        let signing_key =
            synctv_core::service::ProxySigningKey::derive_from(self.config.jwt.secret.as_bytes());
        let mut ctx = synctv_core::provider::ProviderContext::new("synctv")
            .with_user_id(user_id)
            .with_room_id(room_id)
            .with_signing_key(&signing_key);
        if let Some(repo) = self.room_service.media_service().credential_repo() {
            ctx = ctx.with_credential_repo(repo.as_ref());
        }
        if let Some(enc) = self.room_service.media_service().credential_encryption() {
            ctx = ctx.with_credential_encryption(enc);
        }
        if let Some(stores) = &self.provider_stores {
            ctx = ctx.with_store(stores.load(provider.name()));
        }
        let provider_result = provider
            .generate_playback(&ctx, &item.source_config)
            .await
            .map_err(ApiError::from)?;

        let mut builder = synctv_core::models::media::PlaybackResult::builder(
            Some(playlist_id.clone()),
            room_id_model.clone(),
            item.name.clone(),
            0.0,
        )
        .default_mode(provider_result.default_mode.clone());

        for (mode_name, provider_info) in provider_result.playback_infos {
            let info = provider_playback_info_to_model(&provider_info);
            builder = builder.add_mode(mode_name, info);
        }
        for (key, value) in provider_result.metadata {
            builder = builder.add_metadata(key, value);
        }

        let full_result = builder
            .add_metadata(
                "target".to_string(),
                serde_json::Value::String(BASE64_STANDARD.encode(target)),
            )
            .build()
            .ok_or_else(|| ApiError::Internal("Failed to build PlaybackResult".to_string()))?;
        let mut snapshot = playback_snapshot_to_proto(&full_result);
        snapshot.version = dynamic_playback_snapshot_version(&playlist);
        snapshot.expires_at = playback_snapshot_expires_at(&snapshot);
        Ok(snapshot)
    }

    async fn build_playback_snapshot_from_state(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        state: &synctv_core::models::RoomPlaybackState,
    ) -> Result<crate::proto::client::PlaybackSnapshot, ApiError> {
        if let Some(ref media_id) = state.playing_media_id {
            let media = self
                .room_service
                .media_service()
                .get_media(media_id)
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::NotFound("Media not found".to_string()))?;
            return self
                .build_static_media_playback_result(user_id.as_str(), room_id.as_str(), media)
                .await;
        }

        if let Some(ref playlist_id) = state.playing_playlist_id {
            return self
                .build_dynamic_playlist_playback_result(
                    user_id.as_str(),
                    room_id.as_str(),
                    room_id,
                    user_id,
                    playlist_id,
                    &state.target,
                )
                .await;
        }

        Ok(crate::proto::client::PlaybackSnapshot {
            media_id: String::new(),
            playlist_id: String::new(),
            room_id: room_id.as_str().to_string(),
            name: String::new(),
            position: 0.0,
            playback_infos: std::collections::HashMap::new(),
            default_mode: String::new(),
            metadata: std::collections::HashMap::new(),
            version: state.version.to_string(),
            expires_at: None,
        })
    }

    fn serialize_admin_settings_group(
        name: String,
        object: serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::proto::admin::SettingsGroup, ApiError> {
        let settings = serde_json::to_vec(&serde_json::Value::Object(object)).map_err(|error| {
            ApiError::Internal(format!("Failed to encode settings group: {error}"))
        })?;
        Ok(crate::proto::admin::SettingsGroup { name, settings })
    }

    fn project_settings_groups(
        effective: std::collections::BTreeMap<String, String>,
    ) -> Result<Vec<crate::proto::admin::SettingsGroup>, ApiError> {
        let mut groups =
            std::collections::BTreeMap::<String, serde_json::Map<String, serde_json::Value>>::new();

        for (key, raw_value) in effective {
            let Some((group_name, setting_name)) = key.split_once('.') else {
                tracing::warn!(
                    key = %key,
                    "Skipping unsupported setting key without group prefix during admin projection"
                );
                continue;
            };
            groups.entry(group_name.to_string()).or_default().insert(
                setting_name.to_string(),
                parse_raw_setting_value(&raw_value),
            );
        }

        groups
            .into_iter()
            .map(|(name, object)| Self::serialize_admin_settings_group(name, object))
            .collect()
    }

    fn fully_qualified_setting_updates(
        group_name: &str,
        settings: std::collections::HashMap<String, String>,
    ) -> Result<Vec<(String, String)>, ApiError> {
        if group_name.trim().is_empty() {
            return Err(ApiError::InvalidInput(
                "settings group must not be empty".to_string(),
            ));
        }
        if settings.is_empty() {
            return Err(ApiError::InvalidInput(
                "settings update must contain at least one entry".to_string(),
            ));
        }

        let mut updates = Vec::with_capacity(settings.len());
        for (setting_name, value) in settings {
            let setting_name = setting_name.trim();
            if setting_name.is_empty() {
                return Err(ApiError::InvalidInput(
                    "settings key must not be empty".to_string(),
                ));
            }
            if setting_name.contains('.') {
                return Err(ApiError::InvalidInput(format!(
                    "settings key '{setting_name}' must not contain '.'"
                )));
            }
            updates.push((format!("{group_name}.{setting_name}"), value));
        }
        updates.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(updates)
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        room_service: Arc<RoomService>,
        user_service: Arc<UserService>,
        settings_service: Arc<SettingsService>,
        settings_registry: Option<Arc<SettingsRegistry>>,
        email_service: Arc<EmailService>,
        connection_service: Arc<dyn RealtimeConnectionService>,
        provider_instance_manager: Arc<RemoteProviderManager>,
        live_streaming_infrastructure: Option<Arc<LiveStreamingInfrastructure>>,
        publish_key_service: Option<Arc<dyn synctv_core::service::StreamingPublishKeyService>>,
        config: Arc<synctv_core::Config>,
        audit_service: Arc<AuditService>,
    ) -> Self {
        let cluster_fanout = default_cluster_fanout_service(None, config.cluster_runtime_enabled());
        let room_settings_fanout =
            default_room_settings_fanout_service(cluster_fanout.clone(), None);
        let member_fanout = default_member_fanout_service(cluster_fanout.clone());
        let membership_event_fanout = default_membership_event_fanout_service(
            cluster_fanout.clone(),
            room_service.clone(),
            user_service.clone(),
            None,
        );
        let media_fanout = default_media_fanout_service(cluster_fanout.clone(), None);
        let playlist_fanout = default_playlist_fanout_service(cluster_fanout.clone());
        let room_cache_fanout = default_room_cache_fanout_service(cluster_fanout.clone());
        let realtime_lifecycle = default_realtime_lifecycle_service(
            connection_service.clone(),
            live_streaming_infrastructure.clone(),
            cluster_fanout.clone(),
        );
        let room_lifecycle_fanout = default_room_lifecycle_fanout_service(cluster_fanout.clone());
        Self {
            room_service,
            user_service,
            settings_service,
            settings_registry,
            email_service,
            connection_service,
            provider_instance_manager,
            live_streaming_infrastructure,
            publish_key_service,
            config,
            cluster_fanout,
            room_settings_fanout,
            member_fanout,
            membership_event_fanout,
            media_fanout,
            playlist_fanout,
            room_cache_fanout,
            realtime_lifecycle,
            room_lifecycle_fanout,
            realtime_event_service: None,
            audit_service,
            provider_stores: None,
        }
    }

    #[must_use]
    pub fn with_cluster_fanout_service(
        mut self,
        cluster_fanout: Arc<dyn ClusterFanoutService>,
    ) -> Self {
        self.room_settings_fanout = default_room_settings_fanout_service(
            cluster_fanout.clone(),
            self.realtime_event_service.clone(),
        );
        self.member_fanout = default_member_fanout_service(cluster_fanout.clone());
        self.membership_event_fanout = default_membership_event_fanout_service(
            cluster_fanout.clone(),
            self.room_service.clone(),
            self.user_service.clone(),
            self.realtime_event_service.clone(),
        );
        self.media_fanout = default_media_fanout_service(
            cluster_fanout.clone(),
            self.realtime_event_service.clone(),
        );
        self.playlist_fanout = default_playlist_fanout_service(cluster_fanout.clone());
        self.room_cache_fanout = default_room_cache_fanout_service(cluster_fanout.clone());
        self.realtime_lifecycle = default_realtime_lifecycle_service(
            self.connection_service.clone(),
            self.live_streaming_infrastructure.clone(),
            cluster_fanout.clone(),
        );
        self.room_lifecycle_fanout = default_room_lifecycle_fanout_service(cluster_fanout.clone());
        self.cluster_fanout = cluster_fanout;
        self
    }

    #[must_use]
    pub fn with_realtime_event_service(
        mut self,
        event_service: Arc<dyn RealtimeEventService>,
    ) -> Self {
        self.membership_event_fanout = default_membership_event_fanout_service(
            self.cluster_fanout.clone(),
            self.room_service.clone(),
            self.user_service.clone(),
            Some(event_service.clone()),
        );
        self.room_settings_fanout = default_room_settings_fanout_service(
            self.cluster_fanout.clone(),
            Some(event_service.clone()),
        );
        self.media_fanout =
            default_media_fanout_service(self.cluster_fanout.clone(), Some(event_service.clone()));
        self.realtime_event_service = Some(event_service);
        self
    }

    #[must_use]
    pub fn with_provider_stores(
        mut self,
        stores: Arc<dyn synctv_core::provider::store::ProviderStoreResolver>,
    ) -> Self {
        self.provider_stores = Some(stores);
        self
    }

    /// Best-effort admin audit log helper.
    ///
    /// Resolves the admin username (falling back to the raw ID on lookup failure),
    /// then writes an audit entry. If the audit write fails, it logs an ERROR
    /// but does **not** propagate the error to the caller -- the primary operation
    /// has already succeeded and should not be rolled back by an audit failure.
    async fn log_admin_action(
        &self,
        admin_user_id: &UserId,
        action: synctv_core::service::AuditAction,
        target_type: synctv_core::service::AuditTargetType,
        target_id: Option<String>,
        details: serde_json::Value,
        ctx: &RequestContext,
    ) {
        let admin_username = match self.load_admin_actor(admin_user_id).await {
            Ok(actor) => actor.username,
            Err(_) => admin_user_id.as_str().to_string(),
        };

        if let Err(e) = self
            .audit_service
            .log(
                admin_user_id.as_str().to_string(),
                admin_username.clone(),
                action,
                target_type,
                target_id,
                details,
                ctx.ip_address.clone(),
                ctx.user_agent.clone(),
            )
            .await
        {
            tracing::error!(
                error = %e,
                admin_user_id = %admin_user_id.as_str(),
                admin_username = %admin_username,
                "AUDIT LOG FAILURE: failed to record admin action. Manual review required.",
            );
        }
    }

    pub async fn list_rooms(
        &self,
        req: crate::proto::admin::ListRoomsRequest,
    ) -> Result<crate::proto::admin::ListRoomsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let page = if req.page > 0 { req.page } else { 1 };
        let page_size = if req.page_size > 0 { req.page_size } else { 50 };

        // Parse status filter (0/Unspecified = show all statuses for admin)
        let status = if req.status == 0 {
            None
        } else {
            Some(
                match synctv_proto::common::RoomStatus::try_from(req.status) {
                    Ok(synctv_proto::common::RoomStatus::Active) => {
                        synctv_core::models::RoomStatus::Active
                    }
                    Ok(synctv_proto::common::RoomStatus::Pending) => {
                        synctv_core::models::RoomStatus::Pending
                    }
                    Ok(synctv_proto::common::RoomStatus::Rejected) => {
                        synctv_core::models::RoomStatus::Rejected
                    }
                    Ok(synctv_proto::common::RoomStatus::Closed) => {
                        synctv_core::models::RoomStatus::Closed
                    }
                    _ => synctv_core::models::RoomStatus::Active,
                },
            )
        };

        let query = synctv_core::models::RoomListQuery {
            pagination: synctv_core::models::PageParams::new(
                Some(page_i32_to_u32(page)),
                Some(page_size_i32_to_u32(page_size, 100)),
            ),
            status,
            search: if req.search.is_empty() {
                None
            } else {
                Some(req.search)
            },
            is_banned: req.is_banned,
            creator_id: if req.creator_id.is_empty() {
                None
            } else {
                Some(req.creator_id)
            },
            sort_by: match crate::proto::admin::RoomListSortBy::try_from(req.sort_by) {
                Ok(crate::proto::admin::RoomListSortBy::Name) => {
                    synctv_core::models::RoomListSortBy::Name
                }
                Ok(crate::proto::admin::RoomListSortBy::UpdatedAt) => {
                    synctv_core::models::RoomListSortBy::UpdatedAt
                }
                Ok(crate::proto::admin::RoomListSortBy::LastActivityAt) => {
                    synctv_core::models::RoomListSortBy::LastActivityAt
                }
                _ => synctv_core::models::RoomListSortBy::CreatedAt,
            },
            sort_direction: match crate::proto::admin::SortDirection::try_from(req.sort_direction) {
                Ok(crate::proto::admin::SortDirection::Asc) => {
                    synctv_core::models::SortDirection::Asc
                }
                _ => synctv_core::models::SortDirection::Desc,
            },
        };

        let (rooms, total) = self
            .room_service
            .list_rooms(&query)
            .await
            .map_err(ApiError::from)?;

        // Batch-fetch creator usernames for all rooms
        let creator_ids: Vec<synctv_core::models::UserId> = rooms
            .iter()
            .map(|r| r.created_by.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let username_map = self
            .user_service
            .get_usernames(&creator_ids)
            .await
            .unwrap_or_default();
        let creator_status_map = load_creator_status_map(&self.user_service, &creator_ids).await?;

        // Batch-fetch distributed online user counts so same-user multi-connection
        // sessions are not overcounted as multiple online members.
        let room_id_refs: Vec<&synctv_core::models::RoomId> = rooms.iter().map(|r| &r.id).collect();
        let counts = self
            .connection_service
            .room_online_user_count_distributed_batch(&room_id_refs)
            .await
            .map_err(ApiError::Internal)?;

        let room_list: Vec<_> = rooms
            .into_iter()
            .zip(counts)
            .map(|(r, count)| {
                let member_count: Option<i32> = count.try_into().ok();
                let creator_username = username_map.get(&r.created_by).map(String::as_str);
                let creator_status = creator_status_map
                    .get(&r.created_by)
                    .copied()
                    .unwrap_or(UserStatus::Rejected);
                admin_room_to_proto(&r, None, member_count, creator_username, creator_status)
            })
            .collect();

        Ok(crate::proto::admin::ListRoomsResponse {
            rooms: room_list,
            total: i32::try_from(total).unwrap_or(i32::MAX),
        })
    }

    pub async fn get_room(
        &self,
        req: crate::proto::admin::GetRoomRequest,
    ) -> Result<crate::proto::admin::GetRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = crate::impls::proto_validated_room_id(req.room_id);
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;
        let creator_username = self
            .user_service
            .get_usernames(std::slice::from_ref(&room.created_by))
            .await
            .ok()
            .and_then(|m| m.into_values().next());
        let creator_status = load_room_creator_status(&self.user_service, &room).await?;

        Ok(crate::proto::admin::GetRoomResponse {
            room: Some(admin_room_to_proto(
                &room,
                None,
                self.connection_service
                    .room_online_user_count_distributed(&room.id)
                    .await
                    .map_err(ApiError::Internal)?
                    .try_into()
                    .ok(),
                creator_username.as_deref(),
                creator_status,
            )),
        })
    }

    pub async fn create_room(
        &self,
        req: crate::proto::client::CreateRoomRequest,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::client::CreateRoomResponse, ApiError> {
        self.require_admin_actor(admin_user_id).await?;

        let name = crate::http::validation::validate_room_name(&req.name)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let description = if req.description.is_empty() {
            String::new()
        } else {
            crate::http::validation::validate_room_description(&req.description)
                .map_err(|e| ApiError::InvalidInput(e.to_string()))?
        };
        let settings = if req.settings.is_empty() {
            None
        } else {
            Some(serde_json::from_slice(&req.settings)?)
        };
        let password = if req.password.is_empty() {
            None
        } else {
            crate::impls::client::validate_password_for_set(&req.password)?;
            Some(req.password)
        };

        let cluster_event = self.room_lifecycle_fanout.reserve_room_created().await?;

        let (room, _member) = self
            .room_service
            .admin_create_room(name, description, admin_user_id.clone(), password, settings)
            .await
            .map_err(ApiError::from)?;

        self.room_lifecycle_fanout.publish_room_created(
            cluster_event,
            &room.id,
            &room.name,
            admin_user_id,
        );

        let member_count = self
            .connection_service
            .room_online_user_count_distributed(&room.id)
            .await
            .map_err(ApiError::Internal)?
            .try_into()
            .ok();

        Ok(crate::proto::client::CreateRoomResponse {
            room: Some(room_to_proto_basic(&room, None, member_count)),
        })
    }

    pub async fn delete_room(
        &self,
        req: crate::proto::admin::DeleteRoomRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::DeleteRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = crate::impls::proto_validated_room_id(req.room_id);
        let actor = self.require_authorized_admin_actor(admin_user_id).await?;
        let cluster_event = self.room_lifecycle_fanout.reserve_room_deleted().await?;

        self.room_service
            .admin_delete_room_as(&rid, &actor)
            .await
            .map_err(ApiError::from)?;

        self.room_lifecycle_fanout
            .publish_room_deleted(cluster_event, &rid, admin_user_id);

        // Force disconnect all connections and publishers in the deleted room.
        self.realtime_lifecycle
            .disconnect_room(&rid, "room_deleted")
            .await;

        // Audit log: delete_room is a critical operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::RoomDeleted,
            synctv_core::service::AuditTargetType::Room,
            Some(rid.as_str().to_string()),
            serde_json::json!({ "room_id": rid.as_str() }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::DeleteRoomResponse { success: true })
    }

    pub async fn update_room_password(
        &self,
        req: crate::proto::admin::UpdateRoomPasswordRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UpdateRoomPasswordResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let room_id = crate::impls::proto_validated_room_id(req.room_id.clone());
        let new_password = if req.new_password.is_empty() {
            None
        } else {
            Some(req.new_password.as_str())
        };
        let cache_invalidation = self.reserve_room_cache_invalidation().await?;
        let room_settings_fanout = self.room_settings_fanout.reserve_settings_changed().await?;
        let admin_username = self.load_admin_actor(admin_user_id).await.map_or_else(
            |_| admin_user_id.as_str().to_string(),
            |actor| actor.username,
        );
        let snapshot = self
            .room_service
            .admin_set_room_password_as(
                &room_id,
                new_password,
                Some(admin_user_id),
                &admin_username,
            )
            .await
            .map_err(ApiError::from)?;
        let settings_json = serde_json::to_vec(&snapshot.settings).map_err(ApiError::from)?;
        self.room_settings_fanout.publish_settings_changed(
            room_settings_fanout,
            &room_id,
            admin_user_id,
            &admin_username,
            settings_json,
            snapshot.version,
        );
        self.publish_room_cache_invalidation(cache_invalidation, &room_id);

        // Audit log: room password change is a security-relevant operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::RoomPasswordUpdated,
            synctv_core::service::AuditTargetType::Room,
            Some(room_id.as_str().to_string()),
            serde_json::json!({
                "room_id": room_id.as_str(),
                "password_set": new_password.is_some(),
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UpdateRoomPasswordResponse { success: true })
    }

    pub async fn get_room_members(
        &self,
        req: crate::proto::admin::GetRoomMembersRequest,
    ) -> Result<crate::proto::admin::GetRoomMembersResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let rid = crate::impls::proto_validated_room_id(req.room_id.clone());
        let role = match synctv_proto::common::RoomMemberRole::try_from(req.role) {
            Ok(synctv_proto::common::RoomMemberRole::Guest) => {
                Some(synctv_core::models::RoomRole::Guest)
            }
            Ok(synctv_proto::common::RoomMemberRole::Member) => {
                Some(synctv_core::models::RoomRole::Member)
            }
            Ok(synctv_proto::common::RoomMemberRole::Admin) => {
                Some(synctv_core::models::RoomRole::Admin)
            }
            Ok(synctv_proto::common::RoomMemberRole::Creator) => {
                Some(synctv_core::models::RoomRole::Creator)
            }
            _ => None,
        };
        let status = match synctv_proto::common::MemberStatus::try_from(req.status) {
            Ok(synctv_proto::common::MemberStatus::Active) => {
                Some(synctv_core::models::MemberStatus::Active)
            }
            Ok(synctv_proto::common::MemberStatus::Pending) => {
                Some(synctv_core::models::MemberStatus::Pending)
            }
            Ok(synctv_proto::common::MemberStatus::Rejected) => {
                Some(synctv_core::models::MemberStatus::Rejected)
            }
            Ok(synctv_proto::common::MemberStatus::Banned) => {
                Some(synctv_core::models::MemberStatus::Banned)
            }
            Ok(synctv_proto::common::MemberStatus::Left) => {
                Some(synctv_core::models::MemberStatus::Left)
            }
            _ => None,
        };
        let query = synctv_core::models::RoomMemberListQuery {
            pagination: synctv_core::models::PageParams::new(
                Some(u32::try_from(req.page).unwrap_or(1)),
                Some(u32::try_from(req.page_size).unwrap_or(50)),
            ),
            search: (!req.search.is_empty()).then_some(req.search),
            role,
            status,
            is_online: None,
            sort_by: match crate::proto::admin::RoomMemberListSortBy::try_from(req.sort_by) {
                Ok(crate::proto::admin::RoomMemberListSortBy::Username) => {
                    synctv_core::models::RoomMemberListSortBy::Username
                }
                Ok(crate::proto::admin::RoomMemberListSortBy::Role) => {
                    synctv_core::models::RoomMemberListSortBy::Role
                }
                Ok(crate::proto::admin::RoomMemberListSortBy::Status) => {
                    synctv_core::models::RoomMemberListSortBy::Status
                }
                _ => synctv_core::models::RoomMemberListSortBy::JoinedAt,
            },
            sort_direction: match crate::proto::admin::SortDirection::try_from(req.sort_direction) {
                Ok(crate::proto::admin::SortDirection::Desc) => {
                    synctv_core::models::SortDirection::Desc
                }
                _ => synctv_core::models::SortDirection::Asc,
            },
        };
        let (members, total) = self
            .room_service
            .get_room_members_query(&rid, query)
            .await
            .map_err(ApiError::from)?;

        let proto_members: Vec<_> = members.iter().map(admin_room_member_to_proto).collect();

        Ok(crate::proto::admin::GetRoomMembersResponse {
            members: proto_members,
            total: i32::try_from(total).unwrap_or(i32::MAX),
        })
    }

    pub async fn add_member(
        &self,
        req: crate::proto::admin::AddMemberRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::AddMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::admin::AddMemberRequest {
            room_id,
            user_id,
            role,
            notify,
        } = req;
        let actor = self.require_admin_actor(admin_user_id).await?;
        let rid = crate::impls::proto_validated_room_id(room_id);
        let target_uid = crate::impls::proto_validated_user_id(user_id);
        let role = if role == synctv_proto::common::RoomMemberRole::Unspecified as i32 {
            synctv_core::models::RoomRole::Member
        } else {
            proto_role_to_room_role(role)?
        };
        let changed_by = admin_user_id.clone();

        let member = self
            .room_service
            .admin_add_member(
                rid.clone(),
                admin_user_id.clone(),
                &actor.username,
                target_uid.clone(),
                role,
                notify,
            )
            .await
            .map_err(ApiError::from)?;

        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, &changed_by, None)
            .await?;

        let username = self
            .user_service
            .get_user(&target_uid)
            .await
            .map_or_else(|_| format!("user_{}", target_uid.as_str()), |u| u.username);
        let is_online = self
            .connection_service
            .get_connection_id(&rid, &target_uid)
            .is_some();
        let member_with_user = synctv_core::models::RoomMemberWithUser {
            room_id: member.room_id,
            user_id: member.user_id,
            username,
            role: member.role,
            status: member.status,
            added_permissions: member.added_permissions,
            removed_permissions: member.removed_permissions,
            admin_added_permissions: member.admin_added_permissions,
            admin_removed_permissions: member.admin_removed_permissions,
            joined_at: member.joined_at,
            is_online,
            is_active: member.status.is_active(),
            banned_at: member.banned_at,
            banned_reason: member.banned_reason,
        };

        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::MemberStatusUpdated,
            synctv_core::service::AuditTargetType::Member,
            Some(target_uid.as_str().to_string()),
            serde_json::json!({
                "room_id": rid.as_str(),
                "new_status": "active",
                "role": role.to_string(),
                "notify": notify,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::AddMemberResponse {
            member: Some(admin_room_member_to_proto(&member_with_user)),
        })
    }

    pub async fn approve_member(
        &self,
        req: crate::proto::admin::ApproveMemberRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::ApproveMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::admin::ApproveMemberRequest { room_id, user_id } = req;
        let actor = self.require_admin_actor(admin_user_id).await?;
        let rid = crate::impls::proto_validated_room_id(room_id);
        let target_uid = crate::impls::proto_validated_user_id(user_id);
        let changed_by = admin_user_id.clone();

        let member = self
            .room_service
            .admin_approve_member(
                rid.clone(),
                admin_user_id.clone(),
                &actor.username,
                target_uid.clone(),
            )
            .await
            .map_err(ApiError::from)?;

        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, &changed_by, None)
            .await?;

        let username = self
            .user_service
            .get_user(&target_uid)
            .await
            .map_or_else(|_| format!("user_{}", target_uid.as_str()), |u| u.username);
        let is_online = self
            .connection_service
            .get_connection_id(&rid, &target_uid)
            .is_some();
        let member_with_user = synctv_core::models::RoomMemberWithUser {
            room_id: member.room_id,
            user_id: member.user_id,
            username,
            role: member.role,
            status: member.status,
            added_permissions: member.added_permissions,
            removed_permissions: member.removed_permissions,
            admin_added_permissions: member.admin_added_permissions,
            admin_removed_permissions: member.admin_removed_permissions,
            joined_at: member.joined_at,
            is_online,
            is_active: member.status.is_active(),
            banned_at: member.banned_at,
            banned_reason: member.banned_reason,
        };

        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::MemberStatusUpdated,
            synctv_core::service::AuditTargetType::Member,
            Some(target_uid.as_str().to_string()),
            serde_json::json!({
                "room_id": rid.as_str(),
                "old_status": "pending",
                "new_status": "active",
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::ApproveMemberResponse {
            member: Some(admin_room_member_to_proto(&member_with_user)),
        })
    }

    pub async fn reject_member(
        &self,
        req: crate::proto::admin::RejectMemberRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::RejectMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::admin::RejectMemberRequest {
            room_id,
            user_id,
            reason,
        } = req;
        let actor = self.require_admin_actor(admin_user_id).await?;
        let rid = crate::impls::proto_validated_room_id(room_id);
        let target_uid = crate::impls::proto_validated_user_id(user_id);
        let reason_for_service = (!reason.trim().is_empty()).then_some(reason.as_str());
        let changed_by = admin_user_id.clone();

        self.room_service
            .admin_reject_member(
                rid.clone(),
                admin_user_id.clone(),
                &actor.username,
                target_uid.clone(),
                reason_for_service,
            )
            .await
            .map_err(ApiError::from)?;

        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, &changed_by, None)
            .await?;

        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::MemberStatusUpdated,
            synctv_core::service::AuditTargetType::Member,
            Some(target_uid.as_str().to_string()),
            serde_json::json!({
                "room_id": rid.as_str(),
                "old_status": "pending",
                "new_status": "rejected",
                "reason": reason,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::RejectMemberResponse { success: true })
    }

    pub async fn update_member_permissions(
        &self,
        req: crate::proto::admin::UpdateMemberPermissionsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UpdateMemberPermissionsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::admin::UpdateMemberPermissionsRequest {
            room_id,
            user_id,
            role,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        } = req;
        let rid = crate::impls::proto_validated_room_id(room_id);
        let target_uid = crate::impls::proto_validated_user_id(user_id);
        let role = if role == synctv_proto::common::RoomMemberRole::Unspecified as i32 {
            None
        } else {
            Some(proto_role_to_room_role(role)?)
        };
        let permission_fanout = self
            .membership_event_fanout
            .reserve_permission_changed()
            .await?;

        let admin_username = self
            .load_admin_actor(admin_user_id)
            .await
            .map_err(|_| ApiError::NotFound("User not found".to_string()))?
            .username;

        let updated_member = self
            .room_service
            .member_service()
            .admin_update_member(synctv_core::service::member::AdminMemberUpdate {
                room_id: rid.clone(),
                actor_id: admin_user_id.clone(),
                actor_username: admin_username,
                target_user_id: target_uid.clone(),
                role,
                added_permissions,
                removed_permissions,
                admin_added_permissions,
                admin_removed_permissions,
            })
            .await
            .map_err(ApiError::from)?;

        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, admin_user_id, permission_fanout)
            .await?;

        let username = self
            .user_service
            .get_user(&target_uid)
            .await
            .map_or_else(|_| format!("user_{}", target_uid.as_str()), |u| u.username);

        let is_online = self
            .connection_service
            .get_connection_id(&rid, &target_uid)
            .is_some();
        let member_with_user = synctv_core::models::RoomMemberWithUser {
            room_id: updated_member.room_id,
            user_id: updated_member.user_id,
            username,
            role: updated_member.role,
            status: updated_member.status,
            added_permissions: updated_member.added_permissions,
            removed_permissions: updated_member.removed_permissions,
            admin_added_permissions: updated_member.admin_added_permissions,
            admin_removed_permissions: updated_member.admin_removed_permissions,
            joined_at: updated_member.joined_at,
            is_online,
            is_active: true,
            banned_at: updated_member.banned_at,
            banned_reason: updated_member.banned_reason,
        };

        let room_settings = self
            .room_service
            .get_room_settings(&rid)
            .await
            .map_err(ApiError::from)?;
        let role_default = self
            .room_service
            .permission_service()
            .calculate_role_default_permissions(&member_with_user.role, &room_settings);

        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::MemberPermissionUpdated,
            synctv_core::service::AuditTargetType::Member,
            Some(target_uid.as_str().to_string()),
            serde_json::json!({
                "room_id": rid.as_str(),
                "role": role
                    .map(crate::impls::client::room_role_to_proto)
                    .unwrap_or_default(),
                "added_permissions": added_permissions,
                "removed_permissions": removed_permissions,
                "admin_added_permissions": admin_added_permissions,
                "admin_removed_permissions": admin_removed_permissions,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UpdateMemberPermissionsResponse {
            member: Some(synctv_proto::common::RoomMember {
                room_id: member_with_user.room_id.as_str().to_string(),
                user_id: member_with_user.user_id.as_str().to_string(),
                username: member_with_user.username.clone(),
                role: crate::impls::client::room_role_to_proto(member_with_user.role),
                permissions: member_with_user.effective_permissions(role_default).0,
                status: member_status_to_proto(member_with_user.status),
                added_permissions: member_with_user.added_permissions,
                removed_permissions: member_with_user.removed_permissions,
                admin_added_permissions: member_with_user.admin_added_permissions,
                admin_removed_permissions: member_with_user.admin_removed_permissions,
                joined_at: member_with_user.joined_at.timestamp(),
                is_online: member_with_user.is_online,
            }),
        })
    }

    pub async fn kick_member(
        &self,
        req: crate::proto::admin::KickMemberRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::KickMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::admin::KickMemberRequest { room_id, user_id } = req;
        let rid = crate::impls::proto_validated_room_id(room_id);
        let target_uid = crate::impls::proto_validated_user_id(user_id);
        let cluster_event = self.member_fanout.reserve_kick_user_from_room().await?;
        let permission_fanout = self
            .membership_event_fanout
            .reserve_permission_changed()
            .await?;
        let admin_username = self
            .load_admin_actor(admin_user_id)
            .await
            .map_err(|_| ApiError::NotFound("User not found".to_string()))?
            .username;

        self.room_service
            .member_service()
            .admin_kick_member(
                rid.clone(),
                admin_user_id.clone(),
                &admin_username,
                target_uid.clone(),
            )
            .await
            .map_err(ApiError::from)?;

        self.realtime_lifecycle
            .disconnect_user_from_room(&rid, &target_uid)
            .await;

        self.member_fanout
            .publish_kick_user_from_room(cluster_event, &rid, &target_uid, "kicked");

        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, admin_user_id, permission_fanout)
            .await?;

        self.room_service
            .permission_service()
            .invalidate_cache(&rid, &target_uid)
            .await;

        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::MemberKicked,
            synctv_core::service::AuditTargetType::Member,
            Some(target_uid.as_str().to_string()),
            serde_json::json!({
                "room_id": rid.as_str(),
                "mode": "admin_override",
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::KickMemberResponse { success: true })
    }

    pub async fn ban_member(
        &self,
        req: crate::proto::admin::BanMemberRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::BanMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::admin::BanMemberRequest {
            room_id,
            user_id,
            reason,
        } = req;

        let rid = crate::impls::proto_validated_room_id(room_id);
        let target_uid = crate::impls::proto_validated_user_id(user_id);
        let reason = if reason.is_empty() {
            None
        } else {
            Some(reason)
        };
        let persisted_banned_by = (admin_user_id.as_str() != LOCAL_MANAGEMENT_ACTOR_USER_ID)
            .then(|| admin_user_id.clone());
        let cluster_event = self.member_fanout.reserve_kick_user_from_room().await?;
        let permission_fanout = self
            .membership_event_fanout
            .reserve_permission_changed()
            .await?;
        let admin_username = self
            .load_admin_actor(admin_user_id)
            .await
            .map_err(|_| ApiError::NotFound("User not found".to_string()))?
            .username;

        self.room_service
            .member_service()
            .admin_ban_member(
                rid.clone(),
                admin_user_id.clone(),
                &admin_username,
                target_uid.clone(),
                persisted_banned_by,
                reason.clone(),
            )
            .await
            .map_err(ApiError::from)?;

        self.realtime_lifecycle
            .disconnect_user_from_room(&rid, &target_uid)
            .await;

        self.member_fanout
            .publish_kick_user_from_room(cluster_event, &rid, &target_uid, "banned");

        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, admin_user_id, permission_fanout)
            .await?;

        self.room_service
            .permission_service()
            .invalidate_cache(&rid, &target_uid)
            .await;

        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::MemberBanned,
            synctv_core::service::AuditTargetType::Member,
            Some(target_uid.as_str().to_string()),
            serde_json::json!({
                "room_id": rid.as_str(),
                "reason": reason,
                "mode": "admin_override",
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::BanMemberResponse { success: true })
    }

    pub async fn unban_member(
        &self,
        req: crate::proto::admin::UnbanMemberRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UnbanMemberResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let crate::proto::admin::UnbanMemberRequest { room_id, user_id } = req;
        let rid = crate::impls::proto_validated_room_id(room_id);
        let target_uid = crate::impls::proto_validated_user_id(user_id);
        let permission_fanout = self
            .membership_event_fanout
            .reserve_permission_changed()
            .await?;
        let admin_username = self
            .load_admin_actor(admin_user_id)
            .await
            .map_err(|_| ApiError::NotFound("User not found".to_string()))?
            .username;

        self.room_service
            .member_service()
            .admin_unban_member(
                rid.clone(),
                admin_user_id.clone(),
                &admin_username,
                target_uid.clone(),
            )
            .await
            .map_err(ApiError::from)?;

        self.membership_event_fanout
            .publish_permission_changed(&rid, &target_uid, admin_user_id, permission_fanout)
            .await?;

        self.room_service
            .permission_service()
            .invalidate_cache(&rid, &target_uid)
            .await;

        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::MemberUnbanned,
            synctv_core::service::AuditTargetType::Member,
            Some(target_uid.as_str().to_string()),
            serde_json::json!({
                "room_id": rid.as_str(),
                "mode": "admin_override",
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UnbanMemberResponse { success: true })
    }

    pub async fn list_users(
        &self,
        req: crate::proto::admin::ListUsersRequest,
    ) -> Result<crate::proto::admin::ListUsersResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let page = if req.page > 0 { req.page } else { 1 };
        let page_size = if req.page_size > 0 { req.page_size } else { 50 };

        // Convert proto enum i32 values to typed enums for UserListQuery
        let status = match synctv_proto::common::UserStatus::try_from(req.status) {
            Ok(synctv_proto::common::UserStatus::Active) => {
                Some(synctv_core::models::UserStatus::Active)
            }
            Ok(synctv_proto::common::UserStatus::Pending) => {
                Some(synctv_core::models::UserStatus::Pending)
            }
            Ok(synctv_proto::common::UserStatus::Rejected) => {
                Some(synctv_core::models::UserStatus::Rejected)
            }
            Ok(synctv_proto::common::UserStatus::Banned) => {
                Some(synctv_core::models::UserStatus::Banned)
            }
            _ => None, // Unspecified or unknown => no filter
        };
        let role = match synctv_proto::common::UserRole::try_from(req.role) {
            Ok(synctv_proto::common::UserRole::Root) => Some(synctv_core::models::UserRole::Root),
            Ok(synctv_proto::common::UserRole::Admin) => Some(synctv_core::models::UserRole::Admin),
            Ok(synctv_proto::common::UserRole::User) => Some(synctv_core::models::UserRole::User),
            _ => None, // Unspecified or unknown => no filter
        };
        let search = if req.search.is_empty() {
            None
        } else {
            Some(req.search)
        };
        let sort_by = match crate::proto::admin::UserListSortBy::try_from(req.sort_by) {
            Ok(crate::proto::admin::UserListSortBy::Username) => {
                synctv_core::models::UserListSortBy::Username
            }
            Ok(crate::proto::admin::UserListSortBy::Email) => {
                synctv_core::models::UserListSortBy::Email
            }
            Ok(crate::proto::admin::UserListSortBy::Status) => {
                synctv_core::models::UserListSortBy::Status
            }
            Ok(crate::proto::admin::UserListSortBy::Role) => {
                synctv_core::models::UserListSortBy::Role
            }
            Ok(crate::proto::admin::UserListSortBy::UpdatedAt) => {
                synctv_core::models::UserListSortBy::UpdatedAt
            }
            _ => synctv_core::models::UserListSortBy::CreatedAt,
        };
        let sort_direction = match crate::proto::admin::SortDirection::try_from(req.sort_direction)
        {
            Ok(crate::proto::admin::SortDirection::Asc) => synctv_core::models::SortDirection::Asc,
            _ => synctv_core::models::SortDirection::Desc,
        };

        let query = synctv_core::models::UserListQuery {
            pagination: synctv_core::models::PageParams::new(
                Some(page_i32_to_u32(page)),
                Some(page_size_i32_to_u32(page_size, 100)),
            ),
            search,
            status,
            role,
            sort_by,
            sort_direction,
        };

        let (users, total) = self
            .user_service
            .list_users(&query)
            .await
            .map_err(ApiError::from)?;

        let user_list: Vec<_> = users.into_iter().map(|u| admin_user_to_proto(&u)).collect();

        Ok(crate::proto::admin::ListUsersResponse {
            users: user_list,
            total: i32::try_from(total).unwrap_or(i32::MAX),
        })
    }

    pub async fn get_user(
        &self,
        req: crate::proto::admin::GetUserRequest,
    ) -> Result<crate::proto::admin::GetUserResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id);
        let user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::admin::GetUserResponse {
            user: Some(admin_user_to_proto(&user)),
        })
    }

    pub async fn update_user_role(
        &self,
        req: crate::proto::admin::UpdateUserRoleRequest,
        admin_user_id: &UserId,
        caller_role: synctv_core::models::UserRole,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UpdateUserRoleResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id.clone());
        let user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        // Parse role from proto enum
        let new_role = crate::impls::client::proto_role_to_user_role(req.role)?;

        // Only root can promote to root
        if new_role == synctv_core::models::UserRole::Root
            && caller_role != synctv_core::models::UserRole::Root
        {
            return Err(ApiError::Authorization(
                "Only root users can promote to root".to_string(),
            ));
        }

        // Only root can change another root user's role
        if user.role == synctv_core::models::UserRole::Root
            && caller_role != synctv_core::models::UserRole::Root
        {
            return Err(ApiError::Authorization(
                "Only root users can change root user roles".to_string(),
            ));
        }

        // Only root can change admin user roles
        if user.role == synctv_core::models::UserRole::Admin
            && caller_role != synctv_core::models::UserRole::Root
        {
            return Err(ApiError::Authorization(
                "Only root users can change admin user roles".to_string(),
            ));
        }

        // Only root can promote users to admin
        if new_role == synctv_core::models::UserRole::Admin
            && caller_role != synctv_core::models::UserRole::Root
        {
            return Err(ApiError::Authorization(
                "Only root users can promote to admin".to_string(),
            ));
        }

        let old_version = user.version;
        let updated_user = synctv_core::models::User {
            role: new_role,
            ..user
        };

        self.user_service
            .update_user(&updated_user, old_version)
            .await
            .map_err(ApiError::from)?;

        // Audit log: role change is a critical operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserRoleUpdated,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({
                "target_user_id": uid.as_str(),
                "target_username": updated_user.username,
                "new_role": format!("{new_role:?}"),
                "caller_role": format!("{caller_role:?}"),
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UpdateUserRoleResponse {
            user: Some(admin_user_to_proto(&updated_user)),
        })
    }

    pub async fn update_user_password(
        &self,
        req: crate::proto::admin::UpdateUserPasswordRequest,
        caller_user_id: UserId,
        caller_role: synctv_core::models::UserRole,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UpdateUserPasswordResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let uid = crate::impls::proto_validated_user_id(req.user_id.clone());

        // Fetch target user to check role hierarchy
        let target_user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(Self::map_target_user_lookup_error)?;

        // Only root can reset another root user's password
        if target_user.role == UserRole::Root && caller_role != UserRole::Root {
            return Err(ApiError::Authorization(
                "Only root users can reset root user passwords".to_string(),
            ));
        }

        // Only root can reset admin user passwords (admins cannot reset each other's passwords)
        if target_user.role == UserRole::Admin && caller_role != UserRole::Root {
            return Err(ApiError::Authorization(
                "Only root users can reset admin user passwords".to_string(),
            ));
        }

        self.user_service
            .set_password(&uid, &req.new_password)
            .await
            .map_err(ApiError::from)?;

        // Log to audit trail (best-effort: D11 - audit failure should not propagate)
        {
            let mut details_map = serde_json::Map::new();
            details_map.insert(
                "target_user_id".to_string(),
                serde_json::Value::String(uid.as_str().to_string()),
            );
            details_map.insert(
                "target_username".to_string(),
                serde_json::Value::String(target_user.username.clone()),
            );
            if !req.reason.is_empty() {
                details_map.insert("reason".to_string(), serde_json::Value::String(req.reason));
            }
            self.log_admin_action(
                &caller_user_id,
                synctv_core::service::AuditAction::UserPasswordUpdated,
                synctv_core::service::AuditTargetType::User,
                Some(uid.as_str().to_string()),
                serde_json::Value::Object(details_map),
                ctx,
            )
            .await;
        }

        Ok(crate::proto::admin::UpdateUserPasswordResponse { success: true })
    }

    pub async fn get_settings(
        &self,
        _req: crate::proto::admin::GetSettingsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::GetSettingsResponse, ApiError> {
        let group_list = Self::project_settings_groups(self.effective_settings_by_key()?)?;
        let group_names: Vec<String> = group_list.iter().map(|g| g.name.clone()).collect();

        // Audit log for settings view (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::SettingsViewed,
            synctv_core::service::AuditTargetType::Settings,
            None,
            serde_json::json!({
                "group_count": group_names.len(),
                "groups": group_names,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::GetSettingsResponse { groups: group_list })
    }

    pub async fn get_settings_group(
        &self,
        req: crate::proto::admin::GetSettingsGroupRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::GetSettingsGroupResponse, ApiError> {
        let requested_group = req.group.trim();
        if requested_group.is_empty() {
            return Err(ApiError::InvalidInput(
                "settings group must not be empty".to_string(),
            ));
        }

        let group = Self::project_settings_groups(self.effective_settings_by_key()?)?
            .into_iter()
            .find(|group| group.name == requested_group)
            .ok_or_else(|| {
                ApiError::NotFound(format!("Settings group '{requested_group}' not found"))
            })?;

        let group_name = group.name.clone();

        // Audit log for settings group view (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::SettingsGroupViewed,
            synctv_core::service::AuditTargetType::Settings,
            None,
            serde_json::json!({ "group": group_name }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::GetSettingsGroupResponse { group: Some(group) })
    }

    pub async fn update_settings(
        &self,
        req: crate::proto::admin::UpdateSettingsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UpdateSettingsResponse, ApiError> {
        let group_name = req.group.trim().to_string();
        let updates = Self::fully_qualified_setting_updates(&group_name, req.settings)?;
        let changed_keys: Vec<String> = updates.iter().map(|(key, _)| key.clone()).collect();
        // Wrap all setting writes in a single atomic transaction so that a partial
        // failure cannot leave the settings table in an inconsistent state.
        self.settings_service
            .update_batch(updates)
            .await
            .map_err(ApiError::from)?;

        // Broadcast CacheInvalidate so other replicas refresh their settings caches
        let _ = self.room_cache_fanout.try_publish_all_invalidation().await;

        // Audit log for settings update (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::SettingsUpdated,
            synctv_core::service::AuditTargetType::Settings,
            None,
            serde_json::json!({ "changed_keys": changed_keys }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UpdateSettingsResponse {})
    }

    fn map_send_test_email_result(
        to: &str,
        result: Result<(), CoreError>,
    ) -> crate::proto::admin::SendTestEmailResponse {
        match result {
            Ok(()) => crate::proto::admin::SendTestEmailResponse {
                message: format!("Test email sent successfully to {to}"),
                success: true,
            },
            Err(error) => {
                tracing::error!(email = %to, error = %error, "Failed to send test email");
                crate::proto::admin::SendTestEmailResponse {
                    message: "Failed to send test email. Please verify the email configuration and try again.".to_string(),
                    success: false,
                }
            }
        }
    }

    pub async fn send_test_email(
        &self,
        req: crate::proto::admin::SendTestEmailRequest,
    ) -> Result<crate::proto::admin::SendTestEmailResponse, ApiError> {
        Ok(Self::map_send_test_email_result(
            &req.to,
            self.email_service.send_test_email(&req.to).await,
        ))
    }

    pub async fn list_provider_instances(
        &self,
        req: crate::proto::admin::ListProviderInstancesRequest,
    ) -> Result<crate::proto::admin::ListProviderInstancesResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let query = synctv_core::models::ProviderInstanceListQuery {
            pagination: synctv_core::models::PageParams::new(
                Some(page_i32_to_u32(req.page)),
                Some(page_size_i32_to_u32(req.page_size, 100)),
            ),
            provider_type: normalize_non_empty_filter(&req.provider_type),
            search: normalize_non_empty_filter(&req.search),
            enabled: req.enabled,
            tls: req.tls,
            sort_by: match crate::proto::admin::ProviderInstanceListSortBy::try_from(req.sort_by)
                .unwrap_or(crate::proto::admin::ProviderInstanceListSortBy::CreatedAt)
            {
                crate::proto::admin::ProviderInstanceListSortBy::Name => {
                    synctv_core::models::ProviderInstanceListSortBy::Name
                }
                crate::proto::admin::ProviderInstanceListSortBy::Endpoint => {
                    synctv_core::models::ProviderInstanceListSortBy::Endpoint
                }
                crate::proto::admin::ProviderInstanceListSortBy::UpdatedAt => {
                    synctv_core::models::ProviderInstanceListSortBy::UpdatedAt
                }
                crate::proto::admin::ProviderInstanceListSortBy::CreatedAt
                | crate::proto::admin::ProviderInstanceListSortBy::Unspecified => {
                    synctv_core::models::ProviderInstanceListSortBy::CreatedAt
                }
            },
            sort_direction: match crate::proto::admin::SortDirection::try_from(req.sort_direction)
                .unwrap_or(crate::proto::admin::SortDirection::Desc)
            {
                crate::proto::admin::SortDirection::Asc => CoreSortDirection::Asc,
                crate::proto::admin::SortDirection::Desc
                | crate::proto::admin::SortDirection::Unspecified => CoreSortDirection::Desc,
            },
        };

        let instances = self
            .provider_instance_manager
            .list_instances(&query)
            .await
            .map_err(ApiError::from)?;
        let proto_instances: Vec<_> = instances
            .into_iter()
            .map(provider_instance_to_proto)
            .collect();

        Ok(crate::proto::admin::ListProviderInstancesResponse {
            instances: proto_instances,
        })
    }

    pub async fn add_provider_instance(
        &self,
        req: crate::proto::admin::AddProviderInstanceRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::AddProviderInstanceResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        // Parse config if provided
        let (jwt_secret, custom_ca) = if req.config.is_empty() {
            (None, None)
        } else {
            let config: serde_json::Value = serde_json::from_slice(&req.config)
                .map_err(|e| ApiError::InvalidInput(format!("Invalid config JSON: {e}")))?;
            (
                config
                    .get("jwt_secret")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                config
                    .get("custom_ca")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            )
        };

        let instance = synctv_core::models::ProviderInstance {
            name: req.name,
            endpoint: req.endpoint,
            comment: if req.comment.is_empty() {
                None
            } else {
                Some(req.comment)
            },
            jwt_secret,
            custom_ca,
            timeout: seconds_to_timeout_string(if req.timeout_seconds > 0 {
                req.timeout_seconds
            } else {
                10
            }),
            tls: req.tls,
            insecure_tls: req.insecure_tls,
            providers: req.providers,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        self.provider_instance_manager
            .add(instance.clone())
            .await
            .map_err(ApiError::from)?;

        // Audit log: provider instance creation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::ProviderInstanceCreated,
            synctv_core::service::AuditTargetType::ProviderInstance,
            Some(instance.name.clone()),
            serde_json::json!({
                "instance_name": instance.name,
                "endpoint": mask_url_credentials(&instance.endpoint),
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::AddProviderInstanceResponse {
            instance: Some(provider_instance_to_proto(instance)),
        })
    }

    pub async fn update_provider_instance(
        &self,
        req: crate::proto::admin::UpdateProviderInstanceRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UpdateProviderInstanceResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        // Get existing instance
        let mut instance = self
            .provider_instance_manager
            .get_instance(&req.name)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| {
                ApiError::NotFound(format!("Provider instance '{}' not found", req.name))
            })?;

        // Update fields if explicitly provided (optional fields)
        if let Some(endpoint) = req.endpoint {
            instance.endpoint = endpoint;
        }
        if let Some(comment) = req.comment {
            instance.comment = Some(comment);
        }
        if let Some(timeout_seconds) = req.timeout_seconds {
            instance.timeout = seconds_to_timeout_string(timeout_seconds);
        }
        if !req.providers.is_empty() {
            instance.providers = req.providers;
        }

        // Update boolean fields (optional means explicit intent)
        if let Some(tls) = req.tls {
            instance.tls = tls;
        }
        if let Some(insecure_tls) = req.insecure_tls {
            instance.insecure_tls = insecure_tls;
        }

        // Parse config if provided for additional settings
        if !req.config.is_empty() {
            let config: serde_json::Value = serde_json::from_slice(&req.config)
                .map_err(|e| ApiError::InvalidInput(format!("Invalid config JSON: {e}")))?;
            if let Some(jwt_secret) = config.get("jwt_secret").and_then(|v| v.as_str()) {
                instance.jwt_secret = Some(jwt_secret.to_string());
            }
            if let Some(custom_ca) = config.get("custom_ca").and_then(|v| v.as_str()) {
                instance.custom_ca = Some(custom_ca.to_string());
            }
            if let Some(tls) = config.get("tls").and_then(serde_json::Value::as_bool) {
                instance.tls = tls;
            }
            if let Some(insecure_tls) = config
                .get("insecure_tls")
                .and_then(serde_json::Value::as_bool)
            {
                instance.insecure_tls = insecure_tls;
            }
        }

        instance.updated_at = chrono::Utc::now();

        self.provider_instance_manager
            .update(instance.clone())
            .await
            .map_err(ApiError::from)?;

        // Audit log: provider instance update (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::ProviderInstanceUpdated,
            synctv_core::service::AuditTargetType::ProviderInstance,
            Some(instance.name.clone()),
            serde_json::json!({ "instance_name": instance.name }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UpdateProviderInstanceResponse {
            instance: Some(provider_instance_to_proto(instance)),
        })
    }

    pub async fn delete_provider_instance(
        &self,
        req: crate::proto::admin::DeleteProviderInstanceRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::DeleteProviderInstanceResponse, ApiError> {
        self.provider_instance_manager
            .delete(&req.name)
            .await
            .map_err(ApiError::from)?;

        // Audit log: provider instance deletion (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::ProviderInstanceDeleted,
            synctv_core::service::AuditTargetType::ProviderInstance,
            Some(req.name.clone()),
            serde_json::json!({ "instance_name": req.name }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::DeleteProviderInstanceResponse { success: true })
    }

    pub async fn reconnect_provider_instance(
        &self,
        req: crate::proto::admin::ReconnectProviderInstanceRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::ReconnectProviderInstanceResponse, ApiError> {
        // Atomic reconnect: invalidate cached channel and re-create from DB config
        self.provider_instance_manager
            .reconnect(&req.name)
            .await
            .map_err(ApiError::from)?;

        // Get updated instance
        let instances = self
            .provider_instance_manager
            .get_instance(&req.name)
            .await
            .map_err(ApiError::from)?;
        let instance = instances.ok_or_else(|| {
            ApiError::NotFound(format!("Provider instance '{}' not found", req.name))
        })?;

        // Audit log: provider instance reconnection (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::ProviderInstanceReconnected,
            synctv_core::service::AuditTargetType::ProviderInstance,
            Some(instance.name.clone()),
            serde_json::json!({
                "instance_name": instance.name,
                "endpoint": mask_url_credentials(&instance.endpoint),
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::ReconnectProviderInstanceResponse {
            instance: Some(provider_instance_to_proto(instance)),
        })
    }

    pub async fn enable_provider_instance(
        &self,
        req: crate::proto::admin::EnableProviderInstanceRequest,
    ) -> Result<crate::proto::admin::EnableProviderInstanceResponse, ApiError> {
        self.provider_instance_manager
            .enable(&req.name)
            .await
            .map_err(ApiError::from)?;

        // Get updated instance
        let instances = self
            .provider_instance_manager
            .get_instance(&req.name)
            .await
            .map_err(ApiError::from)?;
        let instance = instances.ok_or_else(|| {
            ApiError::NotFound(format!("Provider instance '{}' not found", req.name))
        })?;

        Ok(crate::proto::admin::EnableProviderInstanceResponse {
            instance: Some(provider_instance_to_proto(instance)),
        })
    }

    pub async fn disable_provider_instance(
        &self,
        req: crate::proto::admin::DisableProviderInstanceRequest,
    ) -> Result<crate::proto::admin::DisableProviderInstanceResponse, ApiError> {
        self.provider_instance_manager
            .disable(&req.name)
            .await
            .map_err(ApiError::from)?;

        // Get updated instance
        let instances = self
            .provider_instance_manager
            .get_instance(&req.name)
            .await
            .map_err(ApiError::from)?;
        let instance = instances.ok_or_else(|| {
            ApiError::NotFound(format!("Provider instance '{}' not found", req.name))
        })?;

        Ok(crate::proto::admin::DisableProviderInstanceResponse {
            instance: Some(provider_instance_to_proto(instance)),
        })
    }

    pub async fn create_user(
        &self,
        req: crate::proto::admin::CreateUserRequest,
        caller_role: synctv_core::models::UserRole,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::CreateUserResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let email = match req.email.trim() {
            "" => None,
            value => Some(value.to_string()),
        };

        // Validate role BEFORE registration to fail fast
        let target_role = if req.role != synctv_proto::common::UserRole::Unspecified as i32
            && req.role != synctv_proto::common::UserRole::User as i32
        {
            let new_role = crate::impls::client::proto_role_to_user_role(req.role)?;
            // Only root can create root users
            if new_role == synctv_core::models::UserRole::Root
                && caller_role != synctv_core::models::UserRole::Root
            {
                return Err(ApiError::Authorization(
                    "Only root users can create root users".to_string(),
                ));
            }
            Some(new_role)
        } else {
            None
        };

        let target_status = if req.status == synctv_proto::common::UserStatus::Unspecified as i32 {
            None
        } else {
            Some(
                match synctv_proto::common::UserStatus::try_from(req.status) {
                    Ok(synctv_proto::common::UserStatus::Active) => UserStatus::Active,
                    Ok(synctv_proto::common::UserStatus::Pending) => UserStatus::Pending,
                    Ok(synctv_proto::common::UserStatus::Rejected) => UserStatus::Rejected,
                    Ok(synctv_proto::common::UserStatus::Banned) => UserStatus::Banned,
                    _ => {
                        return Err(ApiError::InvalidInput(
                            "Unsupported user status".to_string(),
                        ))
                    }
                },
            )
        };

        // Delegate to UserService which handles validation, hashing, creation,
        // and username cache population atomically.
        let user = self
            .user_service
            .create_user_with_role_and_status(
                req.username.clone(),
                email,
                req.password,
                target_role,
                target_status,
            )
            .await
            .map_err(ApiError::from)?;

        // Audit log: user creation via admin panel (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserCreated,
            synctv_core::service::AuditTargetType::User,
            Some(user.id.as_str().to_string()),
            serde_json::json!({"reason": "User created via admin panel"}),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::CreateUserResponse {
            user: Some(admin_user_to_proto(&user)),
        })
    }

    pub async fn delete_user(
        &self,
        req: crate::proto::admin::DeleteUserRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::DeleteUserResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id);
        let owned_room_ids = list_owned_room_ids(&self.room_service, &uid)
            .await
            .map_err(ApiError::from)?;
        let mut deleted_room_fanout = Vec::with_capacity(owned_room_ids.len());
        for room_id in owned_room_ids {
            deleted_room_fanout.push(DeletedRoomFanoutReservation {
                room_id,
                reservation: self.room_lifecycle_fanout.reserve_room_deleted().await?,
            });
        }
        let summary = self
            .user_service
            .delete_user_with_summary(&uid)
            .await
            .map_err(ApiError::from)?;

        self.realtime_lifecycle
            .finalize_user_deletion(
                self.room_service.as_ref(),
                &summary,
                admin_user_id,
                "user_deleted",
                deleted_room_fanout,
            )
            .await;

        // Audit log: user deletion is a critical operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserDeleted,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({ "target_user_id": uid.as_str() }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::DeleteUserResponse { success: true })
    }

    pub async fn update_user_username(
        &self,
        req: crate::proto::admin::UpdateUserUsernameRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UpdateUserUsernameResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let uid = crate::impls::proto_validated_user_id(req.user_id.clone());

        // Apply the same validation rules as client-facing set_username:
        // trim, check length, charset, and leading character restrictions.
        let username = req.new_username.trim().to_string();
        if username.chars().count() < synctv_core::validation::USERNAME_MIN {
            return Err(ApiError::InvalidInput(format!(
                "Username must be at least {} characters",
                synctv_core::validation::USERNAME_MIN,
            )));
        }
        if username.chars().count() > synctv_core::validation::USERNAME_MAX {
            return Err(ApiError::InvalidInput(format!(
                "Username must be at most {} characters",
                synctv_core::validation::USERNAME_MAX,
            )));
        }
        if !username
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        {
            return Err(ApiError::InvalidInput(
                "Username can only contain letters, numbers, underscores, and hyphens".to_string(),
            ));
        }
        if username.starts_with('_') || username.starts_with('-') {
            return Err(ApiError::InvalidInput(
                "Username cannot start with underscore or hyphen".to_string(),
            ));
        }

        let mut user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;
        let old_username = user.username.clone();
        let old_version = user.version;
        user.username = username;
        let updated = self
            .user_service
            .update_user(&user, old_version)
            .await
            .map_err(ApiError::from)?;

        // Audit log: admin changing another user's username (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserUsernameUpdated,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({
                "target_user_id": uid.as_str(),
                "old_username": old_username,
                "new_username": updated.username,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UpdateUserUsernameResponse {
            user: Some(admin_user_to_proto(&updated)),
        })
    }

    pub async fn ban_user(
        &self,
        req: crate::proto::admin::BanUserRequest,
        admin_user_id: &UserId,
        caller_role: synctv_core::models::UserRole,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::BanUserResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id.clone());
        let updated = self
            .ban_user_with_cleanup(&uid, admin_user_id, caller_role)
            .await?;

        // Audit log: ban_user is a critical operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserBanned,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({
                "target_user_id": uid.as_str(),
                "target_username": updated.username,
                "reason": req.reason,
                "caller_role": format!("{caller_role:?}"),
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::BanUserResponse {
            user: Some(admin_user_to_proto(&updated)),
        })
    }

    pub async fn unban_user(
        &self,
        req: crate::proto::admin::UnbanUserRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UnbanUserResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id);
        let mut user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        if user.status != UserStatus::Banned {
            return Err(ApiError::InvalidInput("User is not banned".to_string()));
        }

        let old_version = user.version;
        user.status = UserStatus::Active;
        let updated = self
            .user_service
            .update_user(&user, old_version)
            .await
            .map_err(ApiError::from)?;

        // Audit log: unban is a security-relevant operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserUnbanned,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({
                "target_user_id": uid.as_str(),
                "target_username": updated.username,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UnbanUserResponse {
            user: Some(admin_user_to_proto(&updated)),
        })
    }

    pub async fn approve_user(
        &self,
        req: crate::proto::admin::ApproveUserRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::ApproveUserResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id);
        let mut user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        if user.status != UserStatus::Pending {
            return Err(ApiError::InvalidInput(
                "User is not pending approval".to_string(),
            ));
        }

        let old_version = user.version;
        user.status = UserStatus::Active;
        let updated = self
            .user_service
            .update_user(&user, old_version)
            .await
            .map_err(ApiError::from)?;

        // Audit log: approving a user is a security-relevant operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserApproved,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({
                "target_user_id": uid.as_str(),
                "target_username": updated.username,
                "previous_status": "pending",
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::ApproveUserResponse {
            user: Some(admin_user_to_proto(&updated)),
        })
    }

    pub async fn get_user_rooms(
        &self,
        req: crate::proto::admin::GetUserRoomsRequest,
    ) -> Result<crate::proto::admin::GetUserRoomsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let uid = crate::impls::proto_validated_user_id(req.user_id.clone());
        let page = u32::try_from(req.page).unwrap_or(1);
        let page_size = u32::try_from(req.page_size).unwrap_or(50).min(100);
        let status = match synctv_proto::common::RoomStatus::try_from(req.status) {
            Ok(synctv_proto::common::RoomStatus::Active) => {
                Some(synctv_core::models::RoomStatus::Active)
            }
            Ok(synctv_proto::common::RoomStatus::Pending) => {
                Some(synctv_core::models::RoomStatus::Pending)
            }
            Ok(synctv_proto::common::RoomStatus::Rejected) => {
                Some(synctv_core::models::RoomStatus::Rejected)
            }
            Ok(synctv_proto::common::RoomStatus::Closed) => {
                Some(synctv_core::models::RoomStatus::Closed)
            }
            _ => None,
        };
        let query = synctv_core::models::RoomListQuery {
            pagination: synctv_core::models::PageParams::new(Some(page), Some(page_size)),
            status,
            search: normalize_non_empty_filter(&req.search),
            is_banned: req.is_banned,
            sort_by: match crate::proto::admin::RoomListSortBy::try_from(req.sort_by) {
                Ok(crate::proto::admin::RoomListSortBy::Name) => {
                    synctv_core::models::RoomListSortBy::Name
                }
                Ok(crate::proto::admin::RoomListSortBy::UpdatedAt) => {
                    synctv_core::models::RoomListSortBy::UpdatedAt
                }
                Ok(crate::proto::admin::RoomListSortBy::LastActivityAt) => {
                    synctv_core::models::RoomListSortBy::LastActivityAt
                }
                _ => synctv_core::models::RoomListSortBy::CreatedAt,
            },
            sort_direction: match crate::proto::admin::SortDirection::try_from(req.sort_direction) {
                Ok(crate::proto::admin::SortDirection::Asc) => {
                    synctv_core::models::SortDirection::Asc
                }
                _ => synctv_core::models::SortDirection::Desc,
            },
            ..Default::default()
        };

        let (rooms, total) = self
            .room_service
            .list_related_rooms_for_user(&uid, &query)
            .await
            .map_err(ApiError::from)?;

        // Batch-fetch creator usernames for all rooms in a single query.
        let creator_ids: Vec<synctv_core::models::UserId> = rooms
            .iter()
            .map(|r| r.created_by.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let username_map = self
            .user_service
            .get_usernames(&creator_ids)
            .await
            .unwrap_or_default();
        let creator_status_map = load_creator_status_map(&self.user_service, &creator_ids).await?;

        // Batch-fetch distributed online user counts for all rooms
        let room_id_refs: Vec<&synctv_core::models::RoomId> =
            rooms.iter().map(|room| &room.id).collect();
        let counts = self
            .connection_service
            .room_online_user_count_distributed_batch(&room_id_refs)
            .await
            .map_err(ApiError::Internal)?;
        let admin_rooms: Vec<crate::proto::admin::AdminRoom> = rooms
            .iter()
            .zip(counts)
            .map(|(room, count)| {
                let creator_username = username_map.get(&room.created_by).map(String::as_str);
                let creator_status = creator_status_map
                    .get(&room.created_by)
                    .copied()
                    .unwrap_or(UserStatus::Rejected);
                admin_room_to_proto(
                    room,
                    None,
                    count.try_into().ok(),
                    creator_username,
                    creator_status,
                )
            })
            .collect();

        Ok(crate::proto::admin::GetUserRoomsResponse {
            rooms: admin_rooms,
            total: i32::try_from(total).unwrap_or(i32::MAX),
        })
    }

    pub async fn ban_room(
        &self,
        req: crate::proto::admin::BanRoomRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::BanRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = crate::impls::proto_validated_room_id(req.room_id.clone());
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;

        if room.is_banned {
            return Err(ApiError::InvalidInput("Room is already banned".to_string()));
        }

        let cluster_event = self.room_lifecycle_fanout.reserve_room_banned().await?;

        let updated = self
            .room_service
            .ban_room(&rid, admin_user_id)
            .await
            .map_err(ApiError::from)?;

        self.room_lifecycle_fanout
            .publish_room_banned(cluster_event, &rid, admin_user_id);

        self.realtime_lifecycle
            .disconnect_room(&rid, "room_banned")
            .await;

        // Audit log: ban_room is a critical operation (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::RoomBanned,
            synctv_core::service::AuditTargetType::Room,
            Some(rid.as_str().to_string()),
            serde_json::json!({
                "room_id": rid.as_str(),
                "room_name": room.name,
                "reason": req.reason,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::BanRoomResponse {
            room: Some(admin_room_to_proto(
                &updated,
                None,
                self.connection_service
                    .room_online_user_count_distributed(&rid)
                    .await
                    .map_err(ApiError::Internal)?
                    .try_into()
                    .ok(),
                None,
                load_room_creator_status(&self.user_service, &updated).await?,
            )),
        })
    }

    pub async fn unban_room(
        &self,
        req: crate::proto::admin::UnbanRoomRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::UnbanRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = crate::impls::proto_validated_room_id(req.room_id);
        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;

        if !room.is_banned {
            return Err(ApiError::InvalidInput("Room is not banned".to_string()));
        }

        let updated = self
            .room_service
            .unban_room(&rid, admin_user_id)
            .await
            .map_err(ApiError::from)?;

        // Audit log: unban_room (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::RoomUnbanned,
            synctv_core::service::AuditTargetType::Room,
            Some(rid.as_str().to_string()),
            serde_json::json!({
                "room_id": rid.as_str(),
                "room_name": room.name,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::UnbanRoomResponse {
            room: Some(admin_room_to_proto(
                &updated,
                None,
                self.connection_service
                    .room_online_user_count_distributed(&rid)
                    .await
                    .map_err(ApiError::Internal)?
                    .try_into()
                    .ok(),
                None,
                load_room_creator_status(&self.user_service, &updated).await?,
            )),
        })
    }

    pub async fn approve_room(
        &self,
        req: crate::proto::admin::ApproveRoomRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::ApproveRoomResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = crate::impls::proto_validated_room_id(req.room_id);
        let room = self
            .room_service
            .approve_room(&rid)
            .await
            .map_err(ApiError::from)?;

        // Audit log: approving a room (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::RoomApproved,
            synctv_core::service::AuditTargetType::Room,
            Some(rid.as_str().to_string()),
            serde_json::json!({
                "room_id": rid.as_str(),
                "room_name": room.name,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::ApproveRoomResponse {
            room: Some(admin_room_to_proto(
                &room,
                None,
                self.connection_service
                    .room_online_user_count_distributed(&rid)
                    .await
                    .map_err(ApiError::Internal)?
                    .try_into()
                    .ok(),
                None,
                load_room_creator_status(&self.user_service, &room).await?,
            )),
        })
    }

    pub async fn get_room_settings(
        &self,
        req: crate::proto::admin::GetRoomSettingsRequest,
    ) -> Result<crate::proto::admin::GetRoomSettingsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = crate::impls::proto_validated_room_id(req.room_id);
        let (settings, version) = self
            .room_service
            .get_room_settings_with_version(&rid)
            .await
            .map_err(ApiError::from)?;
        let settings_json = serde_json::to_vec(&settings).map_err(ApiError::from)?;

        Ok(crate::proto::admin::GetRoomSettingsResponse {
            settings: settings_json,
            version,
        })
    }

    pub async fn update_room_settings(
        &self,
        req: crate::proto::admin::UpdateRoomSettingsRequest,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::admin::UpdateRoomSettingsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = crate::impls::proto_validated_room_id(req.room_id.clone());
        let settings: synctv_core::models::RoomSettings = serde_json::from_slice(&req.settings)
            .map_err(|e| ApiError::InvalidInput(format!("Invalid settings JSON: {e}")))?;
        let cache_invalidation = self.reserve_room_cache_invalidation().await?;
        let room_settings_fanout = self.room_settings_fanout.reserve_settings_changed().await?;
        let snapshot = self
            .room_service
            .set_room_settings(&rid, &settings)
            .await
            .map_err(ApiError::from)?;
        let settings_json = serde_json::to_vec(&snapshot.settings).map_err(ApiError::from)?;

        // Look up admin username for cluster event
        let admin_username = self.load_admin_actor(admin_user_id).await.map_or_else(
            |_| admin_user_id.as_str().to_string(),
            |actor| actor.username,
        );

        self.room_settings_fanout.publish_settings_changed(
            room_settings_fanout,
            &rid,
            admin_user_id,
            &admin_username,
            settings_json,
            snapshot.version,
        );
        self.publish_room_cache_invalidation(cache_invalidation, &rid);

        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::admin::UpdateRoomSettingsResponse {
            room: Some(admin_room_to_proto(
                &room,
                Some(&snapshot.settings),
                self.connection_service
                    .room_online_user_count_distributed(&rid)
                    .await
                    .map_err(ApiError::Internal)?
                    .try_into()
                    .ok(),
                None,
                load_room_creator_status(&self.user_service, &room).await?,
            )),
        })
    }

    pub async fn reset_room_settings(
        &self,
        req: crate::proto::admin::ResetRoomSettingsRequest,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::admin::ResetRoomSettingsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let rid = crate::impls::proto_validated_room_id(req.room_id);
        let default_settings = synctv_core::models::RoomSettings::default();
        let cache_invalidation = self.reserve_room_cache_invalidation().await?;
        let room_settings_fanout = self.room_settings_fanout.reserve_settings_changed().await?;
        let snapshot = self
            .room_service
            .set_room_settings(&rid, &default_settings)
            .await
            .map_err(ApiError::from)?;

        let room = self
            .room_service
            .get_room(&rid)
            .await
            .map_err(ApiError::from)?;
        // Look up admin username for cluster event
        let admin_username = self.load_admin_actor(admin_user_id).await.map_or_else(
            |_| admin_user_id.as_str().to_string(),
            |actor| actor.username,
        );

        let settings_json = serde_json::to_vec(&snapshot.settings).map_err(ApiError::from)?;
        self.room_settings_fanout.publish_settings_changed(
            room_settings_fanout,
            &rid,
            admin_user_id,
            &admin_username,
            settings_json,
            snapshot.version,
        );
        self.publish_room_cache_invalidation(cache_invalidation, &rid);

        Ok(crate::proto::admin::ResetRoomSettingsResponse {
            room: Some(admin_room_to_proto(
                &room,
                Some(&snapshot.settings),
                self.connection_service
                    .room_online_user_count_distributed(&rid)
                    .await
                    .map_err(ApiError::Internal)?
                    .try_into()
                    .ok(),
                None,
                load_room_creator_status(&self.user_service, &room).await?,
            )),
        })
    }

    pub async fn add_admin(
        &self,
        req: crate::proto::admin::AddAdminRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::AddAdminResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id);
        let mut user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        if user.role.is_admin_or_above() {
            return Err(ApiError::InvalidInput(
                "User is already an admin or root".to_string(),
            ));
        }

        let old_version = user.version;
        user.role = UserRole::Admin;
        let updated = self
            .user_service
            .update_user(&user, old_version)
            .await
            .map_err(ApiError::from)?;

        // Audit log: granting admin role (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserRoleUpdated,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({
                "target_user_id": uid.as_str(),
                "target_username": updated.username,
                "new_role": "Admin",
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::AddAdminResponse {
            user: Some(admin_user_to_proto(&updated)),
        })
    }

    pub async fn remove_admin(
        &self,
        req: crate::proto::admin::RemoveAdminRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::RemoveAdminResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let uid = crate::impls::proto_validated_user_id(req.user_id);
        let mut user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        if matches!(user.role, UserRole::Root) {
            return Err(ApiError::Authorization(
                "Cannot remove admin role from root user".to_string(),
            ));
        }
        if !user.role.is_admin_or_above() {
            return Err(ApiError::InvalidInput("User is not an admin".to_string()));
        }

        let target_username = user.username.clone();
        let old_version = user.version;
        user.role = UserRole::User;
        self.user_service
            .update_user(&user, old_version)
            .await
            .map_err(ApiError::from)?;

        // Audit log: revoking admin role (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserRoleUpdated,
            synctv_core::service::AuditTargetType::User,
            Some(uid.as_str().to_string()),
            serde_json::json!({
                "target_user_id": uid.as_str(),
                "target_username": target_username,
                "new_role": "User",
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::RemoveAdminResponse { success: true })
    }

    pub async fn list_admins(
        &self,
        req: crate::proto::admin::ListAdminsRequest,
    ) -> Result<crate::proto::admin::ListAdminsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let page = if req.page > 0 { req.page } else { 1 };
        let page_size = req.page_size.clamp(1, 100);
        let query = synctv_core::models::UserListQuery {
            pagination: synctv_core::models::PageParams::new(
                Some(page_i32_to_u32(page)),
                Some(page_size_i32_to_u32(page_size, 100)),
            ),
            search: (!req.search.is_empty()).then_some(req.search),
            sort_by: match crate::proto::admin::UserListSortBy::try_from(req.sort_by) {
                Ok(crate::proto::admin::UserListSortBy::Username) => {
                    synctv_core::models::UserListSortBy::Username
                }
                Ok(crate::proto::admin::UserListSortBy::Email) => {
                    synctv_core::models::UserListSortBy::Email
                }
                Ok(crate::proto::admin::UserListSortBy::Status) => {
                    synctv_core::models::UserListSortBy::Status
                }
                Ok(crate::proto::admin::UserListSortBy::Role) => {
                    synctv_core::models::UserListSortBy::Role
                }
                Ok(crate::proto::admin::UserListSortBy::UpdatedAt) => {
                    synctv_core::models::UserListSortBy::UpdatedAt
                }
                _ => synctv_core::models::UserListSortBy::CreatedAt,
            },
            sort_direction: match crate::proto::admin::SortDirection::try_from(req.sort_direction) {
                Ok(crate::proto::admin::SortDirection::Asc) => {
                    synctv_core::models::SortDirection::Asc
                }
                _ => synctv_core::models::SortDirection::Desc,
            },
            ..Default::default()
        };

        let (users, _) = self
            .user_service
            .list_admins(&query)
            .await
            .map_err(ApiError::from)?;

        let admins: Vec<_> = users.into_iter().map(|u| admin_user_to_proto(&u)).collect();

        Ok(crate::proto::admin::ListAdminsResponse { admins })
    }

    pub async fn get_system_stats(
        &self,
        _req: crate::proto::admin::GetSystemStatsRequest,
    ) -> Result<crate::proto::admin::GetSystemStatsResponse, ApiError> {
        // M-4: Run all 7 independent DB queries in parallel
        let stats_pagination = synctv_core::models::PageParams::new(Some(1), Some(1));
        let query_all = synctv_core::models::UserListQuery {
            pagination: stats_pagination,
            ..Default::default()
        };
        let query_active = synctv_core::models::UserListQuery {
            pagination: stats_pagination,
            status: Some(synctv_core::models::UserStatus::Active),
            ..Default::default()
        };
        let query_banned = synctv_core::models::UserListQuery {
            pagination: stats_pagination,
            status: Some(synctv_core::models::UserStatus::Banned),
            ..Default::default()
        };
        let room_query_all = synctv_core::models::RoomListQuery {
            pagination: stats_pagination,
            ..Default::default()
        };
        let room_query_active = synctv_core::models::RoomListQuery {
            pagination: stats_pagination,
            status: Some(synctv_core::models::RoomStatus::Active),
            ..Default::default()
        };
        let room_query_banned = synctv_core::models::RoomListQuery {
            pagination: stats_pagination,
            is_banned: Some(true),
            ..Default::default()
        };

        let pool = self.user_service.pool();
        let (
            total_users_res,
            active_users_res,
            banned_users_res,
            total_rooms_res,
            active_rooms_res,
            banned_rooms_res,
            provider_count_res,
            total_media_res,
        ) = tokio::join!(
            self.user_service.list_users(&query_all),
            self.user_service.list_users(&query_active),
            self.user_service.list_users(&query_banned),
            self.room_service.list_rooms(&room_query_all),
            self.room_service.list_rooms(&room_query_active),
            self.room_service.list_rooms(&room_query_banned),
            self.provider_instance_manager.get_all_instances(),
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media").fetch_one(pool),
        );

        let (_, total_users) = total_users_res.unwrap_or((vec![], 0));
        let (_, active_users) = active_users_res.unwrap_or((vec![], 0));
        let (_, banned_users) = banned_users_res.unwrap_or((vec![], 0));
        let (_, total_rooms) = total_rooms_res.unwrap_or((vec![], 0));
        let (_, active_rooms) = active_rooms_res.unwrap_or((vec![], 0));
        let (_, banned_rooms) = banned_rooms_res.unwrap_or((vec![], 0));
        let provider_count = provider_count_res.map_or(0, |i| usize_to_i32_saturating(i.len()));
        let total_media = i64_to_i32_saturating(total_media_res.unwrap_or(0));

        Ok(crate::proto::admin::GetSystemStatsResponse {
            total_users: i64_to_i32_saturating(total_users),
            active_users: i64_to_i32_saturating(active_users),
            banned_users: i64_to_i32_saturating(banned_users),
            total_rooms: i64_to_i32_saturating(total_rooms),
            active_rooms: i64_to_i32_saturating(active_rooms),
            banned_rooms: i64_to_i32_saturating(banned_rooms),
            total_media,
            provider_instances: provider_count,
            additional_stats: vec![],
        })
    }

    // Livestream Management

    /// List active streams with filtering, sorting and pagination.
    pub async fn list_active_streams(
        &self,
        req: crate::proto::admin::ListActiveStreamsRequest,
    ) -> Result<crate::proto::admin::ListActiveStreamsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(live_streaming_unavailable_error)?;

        let registry = infrastructure.registry();
        let active_pairs = registry.list_active_streams().await.map_err(|error| {
            ApiError::Internal(format!("Failed to list active streams: {error}"))
        })?;
        let room_id = normalize_non_empty_filter(&req.room_id);
        let user_filter = normalize_non_empty_filter(&req.user_id);
        let node_filter = normalize_non_empty_filter(&req.node_id);
        let search =
            normalize_non_empty_filter(&req.search).map(|value| value.to_ascii_lowercase());
        let sort_by = crate::proto::admin::ActiveStreamListSortBy::try_from(req.sort_by)
            .unwrap_or(crate::proto::admin::ActiveStreamListSortBy::StartedAt);
        let sort_direction = crate::proto::admin::SortDirection::try_from(req.sort_direction)
            .unwrap_or(crate::proto::admin::SortDirection::Desc);

        let mut streams = Vec::new();
        for (rid, mid) in active_pairs {
            if let Some(filter_room) = room_id.as_deref() {
                if rid != filter_room {
                    continue;
                }
            }

            let (user_id, node_id, started_at) = match registry.get_publisher(&rid, &mid).await {
                Ok(Some(info)) => (info.user_id, info.node_id, info.started_at.timestamp()),
                _ => (String::new(), String::new(), 0i64),
            };

            let stream = crate::proto::admin::ActiveStreamInfo {
                room_id: rid,
                media_id: mid,
                user_id,
                node_id,
                started_at,
            };

            if let Some(filter_user) = user_filter.as_deref() {
                if stream.user_id != filter_user {
                    continue;
                }
            }
            if let Some(filter_node) = node_filter.as_deref() {
                if stream.node_id != filter_node {
                    continue;
                }
            }
            if let Some(search) = &search {
                let haystack = format!(
                    "{}\n{}\n{}\n{}",
                    stream.room_id.to_ascii_lowercase(),
                    stream.media_id.to_ascii_lowercase(),
                    stream.user_id.to_ascii_lowercase(),
                    stream.node_id.to_ascii_lowercase(),
                );
                if !haystack.contains(search) {
                    continue;
                }
            }

            streams.push(stream);
        }

        streams.sort_by(|left, right| compare_active_streams(left, right, sort_by, sort_direction));
        let streams = paginate_vec(streams, req.page, req.page_size);

        Ok(crate::proto::admin::ListActiveStreamsResponse { streams })
    }

    /// Kick an active stream
    pub async fn kick_stream(
        &self,
        req: crate::proto::admin::KickStreamRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<(), ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let room_id = req.room_id;
        let media_id = req.media_id;
        let reason = req.reason;
        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(live_streaming_unavailable_error)?;

        tracing::info!(
            room_id = %room_id,
            media_id = %media_id,
            reason = %reason,
            admin_user_id = %admin_user_id.as_str(),
            "Admin kicking stream"
        );

        infrastructure
            .kick_stream(&room_id, &media_id)
            .await
            .map_err(|error| ApiError::Internal(format!("Failed to kick stream: {error}")))?;

        // Audit log: kick_stream is a critical operation
        {
            let admin_username = self.load_admin_actor(admin_user_id).await.map_or_else(
                |_| admin_user_id.as_str().to_string(),
                |actor| actor.username,
            );
            if let Err(e) = self
                .audit_service
                .log_stream_kicked(
                    admin_user_id.as_str().to_string(),
                    admin_username.clone(),
                    room_id.clone(),
                    media_id.clone(),
                    if reason.is_empty() {
                        None
                    } else {
                        Some(reason)
                    },
                    ctx.ip_address.clone(),
                    ctx.user_agent.clone(),
                )
                .await
            {
                tracing::error!(
                    error = %e,
                    admin_user_id = %admin_user_id.as_str(),
                    admin_username = %admin_username,
                    room_id = %room_id,
                    media_id = %media_id,
                    "AUDIT LOG FAILURE: failed to record stream kick. Manual review required."
                );
            }
        }

        Ok(())
    }

    pub async fn create_publish_key(
        &self,
        room_id: &str,
        media_id: &str,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::client::CreatePublishKeyResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let media_id = crate::impls::parse_media_id_param(media_id, "media_id")?;

        let media = self
            .room_service
            .media_service()
            .get_media(&media_id)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to load media: {e}")))?
            .ok_or_else(|| ApiError::NotFound(format!("Media {} not found", media_id.as_str())))?;

        if media.room_id != rid {
            return Err(ApiError::InvalidInput(
                "Media does not belong to this room".to_string(),
            ));
        }

        let publish_key_service = self
            .publish_key_service
            .as_ref()
            .ok_or_else(publish_key_service_unavailable_error)?;

        let publish_key = publish_key_service
            .generate_publish_key(&rid, &media_id, admin_user_id)
            .map_err(|e| ApiError::Internal(format!("Failed to generate publish key: {e}")))?;
        let token = publish_key.token.clone();
        let stream_key = format!("{}?token={}", media_id.as_str(), token);

        tracing::info!(
            room_id = %rid.as_str(),
            media_id = %media_id.as_str(),
            admin_user_id = %admin_user_id.as_str(),
            ip_address = ctx.ip_address.as_deref().unwrap_or(""),
            user_agent = ctx.user_agent.as_deref().unwrap_or(""),
            "Admin created publish key"
        );

        Ok(crate::proto::client::CreatePublishKeyResponse {
            publish_key: token,
            rtmp_url: build_publish_rtmp_url(&self.config, rid.as_str()),
            stream_key,
            expires_at: publish_key.expires_at,
        })
    }

    pub async fn get_stream_info(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Result<crate::proto::client::GetStreamInfoResponse, ApiError> {
        crate::http::validation::validate_nanoid_id(media_id, "media_id")
            .map_err(|e| ApiError::InvalidInput(format!("Invalid media_id: {e}")))?;

        let _rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let infrastructure = self
            .live_streaming_infrastructure
            .as_ref()
            .ok_or_else(live_streaming_unavailable_error)?;

        match infrastructure
            .registry
            .get_publisher(room_id, media_id)
            .await
        {
            Ok(Some(pub_info)) => Ok(crate::proto::client::GetStreamInfoResponse {
                active: true,
                publisher: Some(crate::proto::client::StreamPublisherInfo {
                    user_id: pub_info.user_id,
                    started_at: pub_info.started_at.timestamp(),
                }),
            }),
            Ok(None) => Ok(crate::proto::client::GetStreamInfoResponse {
                active: false,
                publisher: None,
            }),
            Err(error) => Err(ApiError::Internal(format!(
                "Failed to get stream info: {error}"
            ))),
        }
    }

    pub async fn list_room_streams(
        &self,
        room_id: &str,
        req: crate::proto::client::ListRoomStreamsRequest,
    ) -> Result<crate::proto::client::ListRoomStreamsResponse, ApiError> {
        let req = crate::impls::client::build_room_streams_request(req)?;
        let _rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        if self.live_streaming_infrastructure.is_none() {
            return Err(live_streaming_unavailable_error());
        }

        let media_ids = self
            .realtime_lifecycle
            .active_room_stream_media_ids(room_id)
            .await;

        Ok(build_room_stream_list_response(media_ids, &req))
    }

    pub async fn start_playback(
        &self,
        room_id: &str,
        req: crate::proto::client::StartPlaybackRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::client::StartPlaybackResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let target = crate::impls::client::build_start_playback_request(req)?;
        let actor = self.require_authorized_admin_actor(admin_user_id).await?;

        self.room_service
            .admin_start_playback_as(
                rid.clone(),
                &actor,
                target.media_id.clone(),
                target.playlist_id.clone(),
                target.target,
            )
            .await
            .map_err(ApiError::from)?;

        self.room_service.touch_room_activity(rid.clone()).await;

        tracing::info!(
            room_id = %rid.as_str(),
            admin_user_id = %admin_user_id.as_str(),
            media_id = target.media_id.as_ref().map_or("", synctv_core::models::MediaId::as_str),
            playlist_id = target.playlist_id.as_ref().map_or("", synctv_core::models::PlaylistId::as_str),
            ip_address = ctx.ip_address.as_deref().unwrap_or(""),
            user_agent = ctx.user_agent.as_deref().unwrap_or(""),
            "Admin started playback"
        );

        Ok(crate::proto::client::StartPlaybackResponse {})
    }

    pub async fn stop_playback(
        &self,
        room_id: &str,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::client::StopPlaybackResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let actor = self.require_authorized_admin_actor(admin_user_id).await?;

        self.room_service
            .admin_stop_playback_as(rid.clone(), &actor)
            .await
            .map_err(ApiError::from)?;

        tracing::info!(
            room_id = %rid.as_str(),
            admin_user_id = %admin_user_id.as_str(),
            ip_address = ctx.ip_address.as_deref().unwrap_or(""),
            user_agent = ctx.user_agent.as_deref().unwrap_or(""),
            "Admin stopped playback"
        );

        Ok(crate::proto::client::StopPlaybackResponse {})
    }

    pub async fn get_playback(
        &self,
        room_id: &str,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::client::GetPlaybackResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;

        self.require_admin_actor(admin_user_id).await?;

        let state = self
            .room_service
            .get_playback_state(&rid)
            .await
            .map_err(ApiError::from)?;
        let playback_snapshot = match self
            .build_playback_snapshot_from_state(admin_user_id, &rid, &state)
            .await
        {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                tracing::warn!(
                    room_id = %rid.as_str(),
                    admin_user_id = %admin_user_id.as_str(),
                    error = %error,
                    "Admin playback snapshot generation failed; returning playback state only"
                );
                None
            }
        };

        Ok(crate::proto::client::GetPlaybackResponse {
            playback_state: Some(playback_state_to_proto(&state)),
            playback_snapshot,
        })
    }

    pub async fn get_playlist(
        &self,
        room_id: &str,
        playlist_id: &str,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::client::GetPlaylistResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let pid = crate::impls::parse_playlist_id_param(playlist_id, "playlist_id")?;

        self.require_admin_actor(admin_user_id).await?;

        let playlist = self
            .room_service
            .playlist_service()
            .get_playlist(&pid)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound(format!("Playlist {playlist_id} not found")))?;

        if playlist.room_id != rid {
            return Err(ApiError::Authorization(
                "Playlist does not belong to this room".to_string(),
            ));
        }

        let child_folder_count = i64_to_i32_saturating(
            self.room_service
                .playlist_service()
                .count_children(&pid)
                .await
                .map_err(ApiError::from)?,
        );
        let media_count = i64_to_i32_saturating(
            self.room_service
                .media_service()
                .count_playlist_media(&pid)
                .await
                .unwrap_or(0),
        );

        Ok(crate::proto::client::GetPlaylistResponse {
            playlist: Some(playlist_to_proto(&playlist, media_count)),
            child_folder_count,
            media_count,
        })
    }

    pub async fn list_playlists(
        &self,
        room_id: &str,
        req: crate::proto::client::ListPlaylistsRequest,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::client::ListPlaylistsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;

        self.require_admin_actor(admin_user_id).await?;

        let page = page_i32_to_usize(req.page);
        let page_size = if req.page_size <= 0 {
            page_size_i32_to_usize(50, 100)
        } else {
            page_size_i32_to_usize(req.page_size, 100)
        };
        let parent_id = if req.parent_id.is_empty() {
            None
        } else {
            let parent_id = crate::impls::proto_validated_playlist_id(req.parent_id.clone());
            let parent = self
                .room_service
                .playlist_service()
                .get_playlist(&parent_id)
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::NotFound("Parent playlist not found".to_string()))?;
            if parent.room_id != rid {
                return Err(ApiError::Authorization(
                    "Parent playlist does not belong to this room".to_string(),
                ));
            }
            Some(parent_id)
        };
        let query = CorePlaylistListQuery {
            pagination: synctv_core::models::PageParams::new(
                Some(u32::try_from(page).unwrap_or(u32::MAX)),
                Some(u32::try_from(page_size).unwrap_or(u32::MAX)),
            ),
            search: normalize_non_empty_filter(&req.search),
            source_provider: normalize_non_empty_filter(&req.source_provider),
            provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
            dynamic_only: req.dynamic_only,
            availability: map_resource_availability_filter(req.availability),
            sort_by: map_admin_playlist_sort(req.sort_by),
            sort_direction: map_client_sort_direction(req.sort_direction),
        };
        let total = i64_to_i32_saturating(
            self.room_service
                .count_client_playlists(&rid, parent_id.as_ref(), &query)
                .await
                .map_err(ApiError::from)?,
        );
        let offset = (page - 1) * page_size;
        let playlists = self
            .room_service
            .list_client_playlists(
                &rid,
                parent_id.as_ref(),
                &query,
                usize_to_i64_saturating(page_size),
                usize_to_i64_saturating(offset),
            )
            .await
            .map_err(ApiError::from)?;

        let playlist_ids: Vec<&str> = playlists.iter().map(|pl| pl.playlist.id.as_str()).collect();
        let counts = self
            .room_service
            .media_service()
            .count_playlist_media_batch(&playlist_ids)
            .await
            .unwrap_or_default();

        let playlists = playlists
            .iter()
            .map(|entry| {
                let item_count = i64_to_i32_saturating(
                    counts.get(entry.playlist.id.as_str()).copied().unwrap_or(0),
                );
                playlist_to_proto_with_availability(&entry.playlist, item_count, entry.is_available)
            })
            .collect();

        Ok(crate::proto::client::ListPlaylistsResponse { playlists, total })
    }

    pub async fn create_playlist(
        &self,
        room_id: &str,
        req: crate::proto::client::CreatePlaylistRequest,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::client::CreatePlaylistResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        self.require_admin_actor(admin_user_id).await?;
        let service_req = crate::impls::client::playlist::build_create_playlist_request(&rid, req)?;

        let playlist_cluster_event = self.playlist_fanout.reserve_created().await?;
        let cache_invalidation = self.reserve_room_cache_invalidation().await?;

        let playlist = self
            .room_service
            .playlist_service()
            .admin_create_playlist(rid.clone(), admin_user_id.clone(), service_req)
            .await
            .map_err(ApiError::from)?;

        let actor_username = self.load_admin_actor(admin_user_id).await.map_or_else(
            |_| admin_user_id.as_str().to_string(),
            |actor| actor.username,
        );
        self.playlist_fanout.publish_created(
            playlist_cluster_event,
            &rid,
            admin_user_id,
            &actor_username,
            &playlist,
        );

        self.publish_room_cache_invalidation(cache_invalidation, &rid);

        let item_count = self
            .room_service
            .media_service()
            .count_playlist_media(&playlist.id)
            .await
            .map_or(0, i64_to_i32_saturating);

        Ok(crate::proto::client::CreatePlaylistResponse {
            playlist: Some(playlist_to_proto(&playlist, item_count)),
        })
    }

    pub async fn update_playlist(
        &self,
        room_id: &str,
        req: crate::proto::client::UpdatePlaylistRequest,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::client::UpdatePlaylistResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        self.require_admin_actor(admin_user_id).await?;
        let service_req = crate::impls::client::playlist::build_update_playlist_request(req)?;

        let playlist_cluster_event = self.playlist_fanout.reserve_updated().await?;
        let cache_invalidation = self.reserve_room_cache_invalidation().await?;

        let playlist = self
            .room_service
            .playlist_service()
            .admin_set_playlist(rid.clone(), admin_user_id.clone(), service_req)
            .await
            .map_err(ApiError::from)?;

        let actor_username = self.load_admin_actor(admin_user_id).await.map_or_else(
            |_| admin_user_id.as_str().to_string(),
            |actor| actor.username,
        );
        self.playlist_fanout.publish_updated(
            playlist_cluster_event,
            &rid,
            admin_user_id,
            &actor_username,
            &playlist,
        );

        self.publish_room_cache_invalidation(cache_invalidation, &rid);

        let item_count = self
            .room_service
            .media_service()
            .count_playlist_media(&playlist.id)
            .await
            .map_or(0, i64_to_i32_saturating);

        Ok(crate::proto::client::UpdatePlaylistResponse {
            playlist: Some(playlist_to_proto(&playlist, item_count)),
        })
    }

    pub async fn move_playlist(
        &self,
        room_id: &str,
        req: crate::proto::client::MovePlaylistRequest,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::client::MovePlaylistResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        self.require_admin_actor(admin_user_id).await?;
        let service_req = crate::impls::client::playlist::build_move_playlist_request(req)?;

        let playlist_cluster_event = self.playlist_fanout.reserve_updated().await?;
        let cache_invalidation = self.reserve_room_cache_invalidation().await?;

        let playlist = self
            .room_service
            .playlist_service()
            .admin_move_playlist(rid.clone(), admin_user_id.clone(), service_req)
            .await
            .map_err(ApiError::from)?;

        let actor_username = self.load_admin_actor(admin_user_id).await.map_or_else(
            |_| admin_user_id.as_str().to_string(),
            |actor| actor.username,
        );
        self.playlist_fanout.publish_updated(
            playlist_cluster_event,
            &rid,
            admin_user_id,
            &actor_username,
            &playlist,
        );

        self.publish_room_cache_invalidation(cache_invalidation, &rid);

        let item_count = self
            .room_service
            .media_service()
            .count_playlist_media(&playlist.id)
            .await
            .map_or(0, i64_to_i32_saturating);

        Ok(crate::proto::client::MovePlaylistResponse {
            playlist: Some(playlist_to_proto(&playlist, item_count)),
        })
    }

    pub async fn delete_playlist(
        &self,
        room_id: &str,
        req: crate::proto::client::DeletePlaylistRequest,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::client::DeletePlaylistResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let actor = self.require_authorized_admin_actor(admin_user_id).await?;

        let cache_invalidation = self.reserve_room_cache_invalidation().await?;
        let (playlist_id, force) =
            crate::impls::client::playlist::build_delete_playlist_request(req)?;
        let (result, (media_cluster_events, playlist_cluster_events)) = self
            .room_service
            .admin_delete_entries_as_with_precommit(
                rid.clone(),
                &actor,
                synctv_core::service::room::DeleteEntriesRequest {
                    playlist_ids: vec![playlist_id],
                    media_ids: Vec::new(),
                    force,
                },
                |plan| async move {
                    let media_cluster_events = self
                        .media_fanout
                        .reserve_removed(plan.deleted_media_ids.len())
                        .await
                        .map_err(|error| {
                            synctv_core::Error::ServiceUnavailable(error.message().to_string())
                        })?;
                    let mut playlist_cluster_events =
                        Vec::with_capacity(plan.deleted_playlist_ids.len());
                    for _ in &plan.deleted_playlist_ids {
                        playlist_cluster_events.push(
                            self.playlist_fanout
                                .reserve_deleted()
                                .await
                                .map_err(|error| {
                                    synctv_core::Error::ServiceUnavailable(
                                        error.message().to_string(),
                                    )
                                })?,
                        );
                    }
                    Ok((media_cluster_events, playlist_cluster_events))
                },
            )
            .await
            .map_err(ApiError::from)?;

        for (media_id, cluster_event) in result.deleted_media_ids.iter().zip(media_cluster_events) {
            self.media_fanout.publish_removed(
                cluster_event,
                &rid,
                admin_user_id,
                actor.username(),
                media_id,
            );
        }
        for (deleted_playlist_id, cluster_event) in result
            .deleted_playlist_ids
            .iter()
            .zip(playlist_cluster_events)
        {
            self.playlist_fanout.publish_deleted(
                cluster_event,
                &rid,
                admin_user_id,
                actor.username(),
                deleted_playlist_id,
            );
        }

        self.publish_room_cache_invalidation(cache_invalidation, &rid);

        for media_id in &result.deleted_media_ids {
            self.realtime_lifecycle
                .kick_stream(room_id, media_id.as_str(), "media_deleted")
                .await;
        }

        Ok(crate::proto::client::DeletePlaylistResponse { success: true })
    }

    pub async fn list_media(
        &self,
        room_id: &str,
        req: crate::proto::client::ListPlaylistItemsRequest,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::client::ListPlaylistItemsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;

        let rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;

        self.require_admin_actor(admin_user_id).await?;

        let Some(playlist_id) = (!req.playlist_id.is_empty())
            .then(|| crate::impls::proto_validated_playlist_id(req.playlist_id.clone()))
        else {
            if !req.target.is_empty() {
                return Err(ApiError::InvalidInput(
                    "target must be empty when browsing the room root".to_string(),
                ));
            }
            let playlist_query = CorePlaylistListQuery {
                pagination: synctv_core::models::PageParams::new(
                    Some(page_i32_to_u32(req.page)),
                    Some(page_size_i32_to_u32(req.page_size, 100)),
                ),
                search: normalize_non_empty_filter(&req.search),
                source_provider: normalize_non_empty_filter(&req.source_provider),
                provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
                dynamic_only: None,
                availability: map_resource_availability_filter(req.availability),
                sort_by: map_admin_playlist_sort_from_media_sort(req.sort_by),
                sort_direction: map_client_sort_direction(req.sort_direction),
            };
            let media_query = CoreMediaListQuery {
                pagination: synctv_core::models::PageParams::new(
                    Some(page_i32_to_u32(req.page)),
                    Some(page_size_i32_to_u32(req.page_size, 100)),
                ),
                search: normalize_non_empty_filter(&req.search),
                source_provider: normalize_non_empty_filter(&req.source_provider),
                provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
                availability: map_resource_availability_filter(req.availability),
                sort_by: map_admin_media_sort(req.sort_by),
                sort_direction: map_client_sort_direction(req.sort_direction),
            };
            let folder_count = self
                .room_service
                .count_client_playlists(&rid, None, &playlist_query)
                .await
                .map_err(ApiError::from)
                .map(i64_to_usize_saturating)?;
            let file_count = self
                .room_service
                .count_client_media(&rid, None, &media_query)
                .await
                .map_err(ApiError::from)
                .map(i64_to_usize_saturating)?;
            let total = folder_count + file_count;
            let page_size = page_size_i32_to_usize(req.page_size, 100);
            let skip = (page_i32_to_usize(req.page) - 1) * page_size;
            let (playlists, media) = if skip < folder_count {
                let playlists = self
                    .room_service
                    .list_client_playlists(
                        &rid,
                        None,
                        &playlist_query,
                        usize_to_i64_saturating(page_size),
                        usize_to_i64_saturating(skip),
                    )
                    .await
                    .map_err(ApiError::from)?;
                let remaining = page_size.saturating_sub(playlists.len());
                let media = if remaining > 0 {
                    self.room_service
                        .list_client_media(
                            &rid,
                            None,
                            &media_query,
                            usize_to_i64_saturating(remaining),
                            0,
                        )
                        .await
                        .map_err(ApiError::from)?
                } else {
                    Vec::new()
                };
                (playlists, media)
            } else {
                let media_skip = skip - folder_count;
                let media = self
                    .room_service
                    .list_client_media(
                        &rid,
                        None,
                        &media_query,
                        usize_to_i64_saturating(page_size),
                        usize_to_i64_saturating(media_skip),
                    )
                    .await
                    .map_err(ApiError::from)?;
                (Vec::new(), media)
            };
            let folder_ids: Vec<&str> =
                playlists.iter().map(|pl| pl.playlist.id.as_str()).collect();
            let counts = self
                .room_service
                .media_service()
                .count_playlist_media_batch(&folder_ids)
                .await
                .unwrap_or_default();
            let proto_playlists = playlists
                .iter()
                .map(|entry| {
                    let item_count = i64_to_i32_saturating(
                        counts.get(entry.playlist.id.as_str()).copied().unwrap_or(0),
                    );
                    playlist_to_proto_with_availability(
                        &entry.playlist,
                        item_count,
                        entry.is_available,
                    )
                })
                .collect();
            let proto_media = media
                .iter()
                .map(|entry| media_to_proto_with_availability(&entry.media, entry.is_available))
                .collect();

            let mut response = crate::proto::client::ListPlaylistItemsResponse {
                playlists: proto_playlists,
                media: proto_media,
                total: usize_to_i32_saturating(total),
                folder_count: usize_to_i32_saturating(folder_count),
                file_count: usize_to_i32_saturating(file_count),
                dynamic_items: Vec::new(),
                current_path: Vec::new(),
                version: String::new(),
            };
            response.version =
                crate::impls::client::media::compute_playlist_items_response_version(&response);
            return Ok(response);
        };

        let playlist = self
            .room_service
            .playlist_service()
            .get_playlist(&playlist_id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::NotFound(format!("Playlist {} not found", req.playlist_id)))?;
        if playlist.room_id != rid {
            return Err(ApiError::Authorization(
                "Playlist does not belong to this room".to_string(),
            ));
        }

        let static_path = self
            .room_service
            .playlist_service()
            .get_playlist_path(&playlist_id)
            .await
            .map_err(ApiError::from)?;
        let mut current_path = static_path
            .iter()
            .map(playlist_path_node_to_proto)
            .collect::<Vec<_>>();

        if playlist.is_dynamic() {
            if !crate::impls::client::media::validate_dynamic_playlist_query_support(
                &playlist, &req,
            )? {
                let mut response = crate::proto::client::ListPlaylistItemsResponse {
                    playlists: Vec::new(),
                    media: Vec::new(),
                    total: 0,
                    folder_count: 0,
                    file_count: 0,
                    dynamic_items: Vec::new(),
                    current_path,
                    version: String::new(),
                };
                response.version =
                    crate::impls::client::media::compute_playlist_items_response_version(&response);
                return Ok(response);
            }

            let page = page_i32_to_usize(req.page);
            let page_size = page_size_i32_to_usize(req.page_size, 100);
            let items = self
                .room_service
                .media_service()
                .admin_list_dynamic_playlist_items(
                    rid.clone(),
                    admin_user_id.clone(),
                    &playlist_id,
                    (!req.target.is_empty()).then_some(req.target.as_slice()),
                    page,
                    page_size,
                )
                .await
                .map_err(ApiError::from)?;

            let dynamic_items = items
                .into_iter()
                .map(|item| {
                    use synctv_core::provider::ItemType;
                    let item_type = match item.item_type {
                        ItemType::Playlist => crate::proto::client::ItemType::Playlist as i32,
                        ItemType::Media => crate::proto::client::ItemType::Media as i32,
                    };

                    Ok(crate::proto::client::PlaylistItem {
                        name: item.name,
                        item_type,
                        target: item.target,
                        size: item.size.map(u64_to_i64_saturating),
                        thumbnail: Some(item.thumbnail.unwrap_or_default()),
                        modified_at: Some(item.modified_at.unwrap_or(0)),
                    })
                })
                .collect::<Result<Vec<_>, ApiError>>()?;

            let browse_path = self
                .room_service
                .media_service()
                .admin_get_dynamic_playlist_browse_path(
                    rid,
                    admin_user_id.clone(),
                    &playlist_id,
                    (!req.target.is_empty()).then_some(req.target.as_slice()),
                )
                .await
                .map_err(ApiError::from)?;
            current_path.extend(browse_path.into_iter().map(|segment| {
                crate::proto::client::PlaylistBrowsePathNode {
                    playlist_id: String::new(),
                    name: segment.name,
                    target: segment.target,
                }
            }));

            let mut response = crate::proto::client::ListPlaylistItemsResponse {
                playlists: Vec::new(),
                media: Vec::new(),
                total: -1,
                folder_count: 0,
                file_count: 0,
                dynamic_items,
                current_path,
                version: String::new(),
            };
            response.version =
                crate::impls::client::media::compute_playlist_items_response_version(&response);
            return Ok(response);
        }

        if !req.target.is_empty() {
            return Err(ApiError::InvalidInput(
                "target must be empty when browsing a static playlist".to_string(),
            ));
        }

        let playlist_query = CorePlaylistListQuery {
            pagination: synctv_core::models::PageParams::new(
                Some(page_i32_to_u32(req.page)),
                Some(page_size_i32_to_u32(req.page_size, 100)),
            ),
            search: normalize_non_empty_filter(&req.search),
            source_provider: normalize_non_empty_filter(&req.source_provider),
            provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
            dynamic_only: None,
            availability: map_resource_availability_filter(req.availability),
            sort_by: map_admin_playlist_sort_from_media_sort(req.sort_by),
            sort_direction: map_client_sort_direction(req.sort_direction),
        };
        let media_query = CoreMediaListQuery {
            pagination: synctv_core::models::PageParams::new(
                Some(page_i32_to_u32(req.page)),
                Some(page_size_i32_to_u32(req.page_size, 100)),
            ),
            search: normalize_non_empty_filter(&req.search),
            source_provider: normalize_non_empty_filter(&req.source_provider),
            provider_instance_name: normalize_non_empty_filter(&req.provider_instance_name),
            availability: map_resource_availability_filter(req.availability),
            sort_by: map_admin_media_sort(req.sort_by),
            sort_direction: map_client_sort_direction(req.sort_direction),
        };
        let folder_count = self
            .room_service
            .count_client_playlists(&rid, Some(&playlist_id), &playlist_query)
            .await
            .map_err(ApiError::from)
            .map(i64_to_usize_saturating)?;
        let file_count = self
            .room_service
            .count_client_media(&rid, Some(&playlist_id), &media_query)
            .await
            .map_err(ApiError::from)
            .map(i64_to_usize_saturating)?;
        let total = folder_count + file_count;
        let page_size = page_size_i32_to_usize(req.page_size, 100);
        let skip = (page_i32_to_usize(req.page) - 1) * page_size;
        let (playlists, media) = if skip < folder_count {
            let playlists = self
                .room_service
                .list_client_playlists(
                    &rid,
                    Some(&playlist_id),
                    &playlist_query,
                    usize_to_i64_saturating(page_size),
                    usize_to_i64_saturating(skip),
                )
                .await
                .map_err(ApiError::from)?;
            let remaining = page_size.saturating_sub(playlists.len());
            let media = if remaining > 0 {
                self.room_service
                    .list_client_media(
                        &rid,
                        Some(&playlist_id),
                        &media_query,
                        usize_to_i64_saturating(remaining),
                        0,
                    )
                    .await
                    .map_err(ApiError::from)?
            } else {
                Vec::new()
            };
            (playlists, media)
        } else {
            let media_skip = skip - folder_count;
            let media = self
                .room_service
                .list_client_media(
                    &rid,
                    Some(&playlist_id),
                    &media_query,
                    usize_to_i64_saturating(page_size),
                    usize_to_i64_saturating(media_skip),
                )
                .await
                .map_err(ApiError::from)?;
            (Vec::new(), media)
        };
        let folder_ids: Vec<&str> = playlists.iter().map(|pl| pl.playlist.id.as_str()).collect();
        let counts = self
            .room_service
            .media_service()
            .count_playlist_media_batch(&folder_ids)
            .await
            .unwrap_or_default();
        let proto_playlists = playlists
            .iter()
            .map(|entry| {
                let item_count = i64_to_i32_saturating(
                    counts.get(entry.playlist.id.as_str()).copied().unwrap_or(0),
                );
                playlist_to_proto_with_availability(&entry.playlist, item_count, entry.is_available)
            })
            .collect();
        let proto_media = media
            .iter()
            .map(|entry| media_to_proto_with_availability(&entry.media, entry.is_available))
            .collect();

        let mut response = crate::proto::client::ListPlaylistItemsResponse {
            playlists: proto_playlists,
            media: proto_media,
            total: usize_to_i32_saturating(total),
            folder_count: usize_to_i32_saturating(folder_count),
            file_count: usize_to_i32_saturating(file_count),
            dynamic_items: Vec::new(),
            current_path,
            version: String::new(),
        };
        response.version =
            crate::impls::client::media::compute_playlist_items_response_version(&response);
        Ok(response)
    }

    pub async fn add_media(
        &self,
        room_id: &str,
        req: crate::proto::client::AddMediaRequest,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::client::AddMediaResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let service_req = crate::impls::client::media::build_add_media_request(req)?;
        let actor = self.require_admin_actor(admin_user_id).await?;
        let playlist_id = service_req.playlist_id.clone();
        let existing_count = if let Some(ref playlist_id) = playlist_id {
            self.room_service
                .media_service()
                .count_playlist_media(playlist_id)
                .await
                .map_err(ApiError::from)
                .map(i64_to_usize_saturating)?
        } else {
            self.room_service
                .media_service()
                .count_room_root_media(&rid)
                .await
                .map_err(ApiError::from)
                .map(i64_to_usize_saturating)?
        };
        if existing_count >= crate::impls::ClientApiImpl::MAX_PLAYLIST_SIZE {
            return Err(ApiError::InvalidInput(format!(
                "Playlist has reached maximum size of {} items",
                crate::impls::ClientApiImpl::MAX_PLAYLIST_SIZE
            )));
        }

        let cache_invalidation = self.reserve_room_cache_invalidation().await?;
        let mut cluster_events = self.media_fanout.reserve_added(1).await?;

        let media = self
            .room_service
            .media_service()
            .admin_add_media(
                rid.clone(),
                admin_user_id.clone(),
                &actor.username,
                service_req,
            )
            .await
            .map_err(ApiError::from)?;

        if let Some(cluster_event) = cluster_events.pop() {
            self.media_fanout.publish_added(
                cluster_event,
                &rid,
                admin_user_id,
                &actor.username,
                &media.id,
                &media.name,
            );
        }

        self.publish_room_cache_invalidation(cache_invalidation, &rid);

        Ok(crate::proto::client::AddMediaResponse {
            media: Some(crate::impls::client::convert::media_to_proto(&media)),
        })
    }

    pub async fn edit_media(
        &self,
        room_id: &str,
        req: crate::proto::client::EditMediaRequest,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::client::EditMediaResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let service_req = crate::impls::client::media::build_edit_media_request(req)?;
        let actor = self.require_admin_actor(admin_user_id).await?;

        let cache_invalidation = self.reserve_room_cache_invalidation().await?;
        let mut cluster_events = self.media_fanout.reserve_updated(1).await?;

        let media = self
            .room_service
            .media_service()
            .admin_edit_media(
                rid.clone(),
                admin_user_id.clone(),
                &actor.username,
                service_req,
            )
            .await
            .map_err(ApiError::from)?;

        if let Some(cluster_event) = cluster_events.pop() {
            self.media_fanout.publish_updated(
                cluster_event,
                &rid,
                admin_user_id,
                &actor.username,
                &media.id,
                &media.name,
            );
        }

        self.publish_room_cache_invalidation(cache_invalidation, &rid);

        Ok(crate::proto::client::EditMediaResponse {
            media: Some(crate::impls::client::convert::media_to_proto(&media)),
        })
    }

    pub async fn delete_media(
        &self,
        room_id: &str,
        req: crate::proto::client::DeleteMediaRequest,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::client::DeleteMediaResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let actor = self.require_authorized_admin_actor(admin_user_id).await?;

        let cache_invalidation = self.reserve_room_cache_invalidation().await?;

        let (result, media_cluster_events) = self
            .room_service
            .admin_delete_entries_as_with_precommit(
                rid.clone(),
                &actor,
                crate::impls::client::media::build_delete_entries_request(
                    crate::impls::client::media::build_delete_media_request(req)?,
                )?
                .0,
                |plan| async move {
                    self.media_fanout
                        .reserve_removed(plan.deleted_media_ids.len())
                        .await
                        .map_err(|error| {
                            synctv_core::Error::ServiceUnavailable(error.message().to_string())
                        })
                },
            )
            .await
            .map_err(ApiError::from)?;

        for (media_id, cluster_event) in result.deleted_media_ids.iter().zip(media_cluster_events) {
            self.media_fanout.publish_removed(
                cluster_event,
                &rid,
                admin_user_id,
                actor.username(),
                media_id,
            );
        }

        self.publish_room_cache_invalidation(cache_invalidation, &rid);

        for media_id in &result.deleted_media_ids {
            self.realtime_lifecycle
                .kick_stream(room_id, media_id.as_str(), "media_deleted")
                .await;
        }

        Ok(crate::proto::client::DeleteMediaResponse { success: true })
    }

    pub async fn move_media(
        &self,
        room_id: &str,
        req: crate::proto::client::MoveMediaRequest,
        admin_user_id: &UserId,
    ) -> Result<crate::proto::client::MoveMediaResponse, ApiError> {
        let rid = crate::room_id_validation::parse_room_id(room_id)
            .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        let service_req = crate::impls::client::media::build_move_media_request(req)?;
        let actor = self.require_admin_actor(admin_user_id).await?;

        let media_fanout_plan = build_move_media_fanout_plan(
            self.room_service.media_service(),
            &self.media_fanout,
            &rid,
            &service_req,
        )
        .await?;
        let cache_invalidation = self.reserve_room_cache_invalidation().await?;

        let media = self
            .room_service
            .media_service()
            .admin_move_media(
                rid.clone(),
                admin_user_id.clone(),
                &actor.username,
                service_req,
            )
            .await
            .map_err(ApiError::from)?;

        publish_move_media_fanout(
            &self.media_fanout,
            media_fanout_plan,
            &rid,
            admin_user_id,
            &actor.username,
            &media,
        );

        self.publish_room_cache_invalidation(cache_invalidation, &rid);

        Ok(crate::proto::client::MoveMediaResponse {
            moved_count: usize_to_i32_saturating(media.len()),
            media: media.iter().map(media_to_proto).collect(),
        })
    }

    // Batch Operations

    /// Batch ban multiple users.
    ///
    /// Each user is processed individually. If a user cannot be banned (e.g., not found,
    /// already banned, or permission denied), the error is recorded but processing continues.
    /// Returns per-user results with success/failure status.
    pub async fn batch_ban_users(
        &self,
        req: crate::proto::admin::BatchBanUsersRequest,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::BatchBanUsersResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let parsed_user_ids = parse_batch_user_ids(&req.user_ids)?;

        // Pre-filter: check role hierarchy for each target user before delegating
        // to the service layer. Users that violate hierarchy are skipped with an error.
        let mut proto_results = Vec::with_capacity(req.user_ids.len());
        let mut succeeded = 0i32;
        let mut failed = 0i32;

        for (user_id_str, uid) in req.user_ids.iter().zip(parsed_user_ids.iter()) {
            match self.user_service.get_user(uid).await {
                Ok(target_user) => {
                    if let Err(e) = check_role_hierarchy(caller_role, target_user.role, "ban") {
                        proto_results.push(crate::proto::admin::BatchResultItem {
                            id: user_id_str.clone(),
                            success: false,
                            error: map_batch_result_error(e),
                        });
                        failed += 1;
                        continue;
                    }
                    match self
                        .ban_user_with_cleanup(uid, admin_user_id, caller_role)
                        .await
                    {
                        Ok(_) => {
                            proto_results.push(crate::proto::admin::BatchResultItem {
                                id: user_id_str.clone(),
                                success: true,
                                error: String::new(),
                            });
                            succeeded += 1;
                        }
                        Err(e) => {
                            proto_results.push(crate::proto::admin::BatchResultItem {
                                id: user_id_str.clone(),
                                success: false,
                                error: map_batch_result_error(e),
                            });
                            failed += 1;
                        }
                    }
                }
                Err(e) => {
                    proto_results.push(crate::proto::admin::BatchResultItem {
                        id: user_id_str.clone(),
                        success: false,
                        error: map_batch_result_error(e),
                    });
                    failed += 1;
                }
            }
        }

        // Audit log (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserBanned,
            synctv_core::service::AuditTargetType::User,
            None,
            serde_json::json!({
                "action": "batch_ban",
                "total": req.user_ids.len(),
                "succeeded": succeeded,
                "failed": failed,
                "reason": req.reason,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::BatchBanUsersResponse {
            results: proto_results,
            succeeded,
            failed,
        })
    }

    /// Batch delete multiple users.
    ///
    /// Each user is processed individually. If a user cannot be deleted, the error is
    /// recorded but processing continues.
    pub async fn batch_delete_users(
        &self,
        req: crate::proto::admin::BatchDeleteUsersRequest,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::BatchDeleteUsersResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let parsed_user_ids = parse_batch_user_ids(&req.user_ids)?;

        // Pre-filter: check role hierarchy for each target user before delegating
        // to the service layer. Users that violate hierarchy are skipped with an error.
        let mut allowed_ids = Vec::with_capacity(req.user_ids.len());
        let mut proto_results = Vec::with_capacity(req.user_ids.len());
        let mut succeeded = 0i32;
        let mut failed = 0i32;

        for (user_id_str, uid) in req.user_ids.iter().zip(parsed_user_ids) {
            match self.user_service.get_user(&uid).await {
                Ok(target_user) => {
                    if let Err(e) = check_role_hierarchy(caller_role, target_user.role, "delete") {
                        proto_results.push(crate::proto::admin::BatchResultItem {
                            id: user_id_str.clone(),
                            success: false,
                            error: map_batch_result_error(e),
                        });
                        failed += 1;
                        continue;
                    }
                    allowed_ids.push((user_id_str.clone(), uid));
                }
                Err(e) => {
                    proto_results.push(crate::proto::admin::BatchResultItem {
                        id: user_id_str.clone(),
                        success: false,
                        error: map_batch_result_error(e),
                    });
                    failed += 1;
                }
            }
        }

        // Process the allowed users through the service layer
        if !allowed_ids.is_empty() {
            for (user_id, uid) in allowed_ids {
                let owned_room_ids = match list_owned_room_ids(&self.room_service, &uid).await {
                    Ok(room_ids) => room_ids,
                    Err(error) => {
                        proto_results.push(crate::proto::admin::BatchResultItem {
                            id: user_id,
                            success: false,
                            error: map_batch_result_error(ApiError::from(error)),
                        });
                        failed += 1;
                        continue;
                    }
                };

                let mut deleted_room_fanout = Vec::with_capacity(owned_room_ids.len());
                let mut reservation_error = None;
                for room_id in owned_room_ids {
                    match self.room_lifecycle_fanout.reserve_room_deleted().await {
                        Ok(reservation) => deleted_room_fanout.push(DeletedRoomFanoutReservation {
                            room_id,
                            reservation,
                        }),
                        Err(error) => {
                            reservation_error = Some(error);
                            break;
                        }
                    }
                }

                if let Some(error) = reservation_error {
                    proto_results.push(crate::proto::admin::BatchResultItem {
                        id: user_id,
                        success: false,
                        error: map_batch_result_error(error),
                    });
                    failed += 1;
                    continue;
                }

                match self.user_service.delete_user_with_summary(&uid).await {
                    Ok(summary) => {
                        proto_results.push(crate::proto::admin::BatchResultItem {
                            id: user_id.clone(),
                            success: true,
                            error: String::new(),
                        });
                        succeeded += 1;

                        self.realtime_lifecycle
                            .finalize_user_deletion(
                                self.room_service.as_ref(),
                                &summary,
                                admin_user_id,
                                "batch_deleted",
                                deleted_room_fanout,
                            )
                            .await;
                    }
                    Err(e) => {
                        proto_results.push(crate::proto::admin::BatchResultItem {
                            id: user_id,
                            success: false,
                            error: map_batch_result_error(ApiError::from(e)),
                        });
                        failed += 1;
                    }
                }
            }
        }

        // Audit log (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::UserDeleted,
            synctv_core::service::AuditTargetType::User,
            None,
            serde_json::json!({
                "action": "batch_delete",
                "total": req.user_ids.len(),
                "succeeded": succeeded,
                "failed": failed,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::BatchDeleteUsersResponse {
            results: proto_results,
            succeeded,
            failed,
        })
    }

    /// Batch ban multiple rooms.
    ///
    /// Each room is processed individually. If a room cannot be banned, the error is
    /// recorded but processing continues.
    pub async fn batch_ban_rooms(
        &self,
        req: crate::proto::admin::BatchBanRoomsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::BatchBanRoomsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let mut proto_results = Vec::with_capacity(req.room_ids.len());
        let mut succeeded = 0i32;
        let mut failed = 0i32;

        for room_id in &req.room_ids {
            let rid = RoomId::from_string(room_id.clone());
            let result = async {
                let room = self
                    .room_service
                    .get_room(&rid)
                    .await
                    .map_err(ApiError::from)?;
                if room.is_banned {
                    return Err(ApiError::InvalidInput("Room is already banned".to_string()));
                }

                let cluster_event = self.room_lifecycle_fanout.reserve_room_banned().await?;
                self.room_service
                    .ban_room(&rid, admin_user_id)
                    .await
                    .map_err(ApiError::from)?;

                self.room_lifecycle_fanout
                    .publish_room_banned(cluster_event, &rid, admin_user_id);
                self.realtime_lifecycle
                    .disconnect_room(&rid, "room_batch_banned")
                    .await;

                Ok::<(), ApiError>(())
            }
            .await;

            match result {
                Ok(()) => {
                    proto_results.push(crate::proto::admin::BatchResultItem {
                        id: room_id.clone(),
                        success: true,
                        error: String::new(),
                    });
                    succeeded += 1;
                }
                Err(e) => {
                    proto_results.push(crate::proto::admin::BatchResultItem {
                        id: room_id.clone(),
                        success: false,
                        error: map_batch_result_error(e),
                    });
                    failed += 1;
                }
            }
        }

        // Audit log (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::RoomBanned,
            synctv_core::service::AuditTargetType::Room,
            None,
            serde_json::json!({
                "action": "batch_ban",
                "total": req.room_ids.len(),
                "succeeded": succeeded,
                "failed": failed,
                "reason": req.reason,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::BatchBanRoomsResponse {
            results: proto_results,
            succeeded,
            failed,
        })
    }

    /// Batch delete multiple rooms.
    ///
    /// Each room is processed individually. If a room cannot be deleted, the error is
    /// recorded but processing continues.
    pub async fn batch_delete_rooms(
        &self,
        req: crate::proto::admin::BatchDeleteRoomsRequest,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<crate::proto::admin::BatchDeleteRoomsResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let actor = self.require_authorized_admin_actor(admin_user_id).await?;
        let mut proto_results = Vec::with_capacity(req.room_ids.len());
        let mut succeeded = 0i32;
        let mut failed = 0i32;

        for room_id in &req.room_ids {
            let rid = RoomId::from_string(room_id.clone());
            let result = async {
                let cluster_event = self.room_lifecycle_fanout.reserve_room_deleted().await?;
                self.room_service
                    .admin_delete_room_as(&rid, &actor)
                    .await
                    .map_err(ApiError::from)?;

                self.room_lifecycle_fanout
                    .publish_room_deleted(cluster_event, &rid, admin_user_id);
                self.realtime_lifecycle
                    .disconnect_room(&rid, "room_batch_deleted")
                    .await;

                Ok::<(), ApiError>(())
            }
            .await;

            match result {
                Ok(()) => {
                    proto_results.push(crate::proto::admin::BatchResultItem {
                        id: room_id.clone(),
                        success: true,
                        error: String::new(),
                    });
                    succeeded += 1;
                }
                Err(e) => {
                    proto_results.push(crate::proto::admin::BatchResultItem {
                        id: room_id.clone(),
                        success: false,
                        error: map_batch_result_error(e),
                    });
                    failed += 1;
                }
            }
        }

        // Audit log (best-effort)
        self.log_admin_action(
            admin_user_id,
            synctv_core::service::AuditAction::RoomDeleted,
            synctv_core::service::AuditTargetType::Room,
            None,
            serde_json::json!({
                "action": "batch_delete",
                "total": req.room_ids.len(),
                "succeeded": succeeded,
                "failed": failed,
            }),
            ctx,
        )
        .await;

        Ok(crate::proto::admin::BatchDeleteRoomsResponse {
            results: proto_results,
            succeeded,
            failed,
        })
    }
}

#[cfg(test)]
async fn active_room_stream_media_ids_for_infra(
    live_streaming_infrastructure: Option<&Arc<LiveStreamingInfrastructure>>,
    room_id: &str,
) -> Vec<String> {
    let connection_service: Arc<dyn RealtimeConnectionService> =
        Arc::new(synctv_cluster::sync::ConnectionManager::new(
            synctv_cluster::sync::ConnectionLimits::default(),
        ));
    let realtime_lifecycle = default_realtime_lifecycle_service(
        connection_service,
        live_streaming_infrastructure.cloned(),
        crate::cluster_fanout::default_cluster_fanout_service(None, false),
    );
    realtime_lifecycle
        .active_room_stream_media_ids(room_id)
        .await
}

const USER_ROOM_CLEANUP_PAGE_SIZE: u32 = 100;

async fn list_active_user_room_ids(
    room_service: &RoomService,
    user_id: &UserId,
) -> synctv_core::Result<Vec<RoomId>> {
    let mut page = 1;
    let mut room_ids = Vec::new();

    loop {
        let (page_room_ids, total) = room_service
            .member_service()
            .list_user_rooms(
                user_id,
                synctv_core::models::PageParams::new(Some(page), Some(USER_ROOM_CLEANUP_PAGE_SIZE)),
            )
            .await?;

        if page_room_ids.is_empty() {
            break;
        }

        room_ids.extend(page_room_ids);
        if usize_to_i64_saturating(room_ids.len()) >= total {
            break;
        }

        page += 1;
    }

    Ok(room_ids)
}

async fn list_owned_room_ids(
    room_service: &RoomService,
    user_id: &UserId,
) -> synctv_core::Result<Vec<RoomId>> {
    let mut page = 1;
    let mut room_ids = Vec::new();

    loop {
        let (rooms, total) = room_service
            .list_rooms_by_creator(
                user_id,
                synctv_core::models::PageParams::new(Some(page), Some(USER_ROOM_CLEANUP_PAGE_SIZE)),
            )
            .await?;

        if rooms.is_empty() {
            break;
        }

        room_ids.extend(rooms.into_iter().map(|room| room.id));
        if usize_to_i64_saturating(room_ids.len()) >= total {
            break;
        }

        page += 1;
    }

    Ok(room_ids)
}

async fn invalidate_user_room_permission_caches(
    room_service: &RoomService,
    user_id: &UserId,
    room_ids: &[RoomId],
) {
    for room_id in room_ids {
        room_service
            .permission_service()
            .invalidate_cache(room_id, user_id)
            .await;
    }
}

fn admin_room_to_proto(
    room: &synctv_core::models::Room,
    settings: Option<&synctv_core::models::RoomSettings>,
    member_count: Option<i32>,
    creator_username: Option<&str>,
    creator_status: UserStatus,
) -> crate::proto::admin::AdminRoom {
    let room_settings = settings.cloned().unwrap_or_default();
    crate::proto::admin::AdminRoom {
        id: room.id.to_string(),
        name: room.name.clone(),
        description: room.description.clone(),
        creator_id: room.created_by.to_string(),
        creator_username: creator_username.unwrap_or("").to_string(),
        status: synctv_proto::common::RoomStatus::from(room.status) as i32,
        settings: serde_json::to_vec(&room_settings).unwrap_or_default(),
        member_count: member_count.unwrap_or(0),
        created_at: room.created_at.timestamp(),
        updated_at: room.updated_at.timestamp(),
        is_banned: room.is_banned,
        creator_status: user_status_to_proto(creator_status),
        version: i64::from(room.version),
    }
}

fn admin_room_member_to_proto(
    member: &synctv_core::models::RoomMemberWithUser,
) -> synctv_proto::common::RoomMember {
    synctv_proto::common::RoomMember {
        room_id: member.room_id.to_string(),
        user_id: member.user_id.to_string(),
        username: member.username.clone(),
        role: crate::impls::client::room_role_to_proto(member.role),
        permissions: member.effective_permissions(member.role.permissions()).0,
        status: member_status_to_proto(member.status),
        added_permissions: member.added_permissions,
        removed_permissions: member.removed_permissions,
        admin_added_permissions: member.admin_added_permissions,
        admin_removed_permissions: member.admin_removed_permissions,
        joined_at: member.joined_at.timestamp(),
        is_online: member.is_online,
    }
}

fn build_publish_rtmp_url(config: &synctv_core::Config, room_id: &str) -> String {
    let rtmp_host = config.public_rtmp_host();
    let rtmp_port = config.livestream.rtmp_port;
    format!("rtmp://{rtmp_host}:{rtmp_port}/{room_id}")
}

fn admin_user_to_proto(user: &synctv_core::models::User) -> crate::proto::admin::AdminUser {
    let role = match user.role {
        synctv_core::models::UserRole::Root => synctv_proto::common::UserRole::Root as i32,
        synctv_core::models::UserRole::Admin => synctv_proto::common::UserRole::Admin as i32,
        synctv_core::models::UserRole::User => synctv_proto::common::UserRole::User as i32,
    };

    let status = match user.status {
        synctv_core::models::UserStatus::Active => synctv_proto::common::UserStatus::Active as i32,
        synctv_core::models::UserStatus::Pending => {
            synctv_proto::common::UserStatus::Pending as i32
        }
        synctv_core::models::UserStatus::Rejected => {
            synctv_proto::common::UserStatus::Rejected as i32
        }
        synctv_core::models::UserStatus::Banned => synctv_proto::common::UserStatus::Banned as i32,
    };

    crate::proto::admin::AdminUser {
        id: user.id.to_string(),
        username: user.username.clone(),
        email: user.email.clone().unwrap_or_default(),
        role,
        status,
        created_at: user.created_at.timestamp(),
        updated_at: user.updated_at.timestamp(),
    }
}

fn provider_instance_to_proto(
    instance: synctv_core::models::ProviderInstance,
) -> crate::proto::admin::ProviderInstance {
    use crate::proto::admin::ProviderInstanceStatus;

    // The `status` field represents actual connection state, not enabled/disabled.
    // Since we don't track real connection status here, use Disconnected for disabled
    // instances and Unspecified for enabled ones (actual connectivity is unknown).
    let status: i32 = if instance.enabled {
        ProviderInstanceStatus::Unspecified.into()
    } else {
        ProviderInstanceStatus::Disconnected.into()
    };

    // Parse timeout string (e.g., "10s", "30s") to seconds
    let timeout_seconds = parse_timeout_to_seconds(&instance.timeout);

    crate::proto::admin::ProviderInstance {
        name: instance.name,
        endpoint: instance.endpoint,
        comment: instance.comment.unwrap_or_default(),
        timeout_seconds,
        tls: instance.tls,
        insecure_tls: instance.insecure_tls,
        providers: instance.providers,
        enabled: instance.enabled,
        status,
        created_at: instance.created_at.timestamp(),
        updated_at: instance.updated_at.timestamp(),
    }
}

fn parse_timeout_to_seconds(timeout: &str) -> u32 {
    timeout.trim_end_matches('s').parse::<u32>().unwrap_or(10)
}

fn seconds_to_timeout_string(seconds: u32) -> String {
    format!("{seconds}s")
}

/// Strip credentials (username/password) from a URL before including it in audit logs.
///
/// If the input is not a valid URL, returns it unchanged (best-effort masking).
fn mask_url_credentials(endpoint: &str) -> String {
    match url::Url::parse(endpoint) {
        Ok(mut parsed) => {
            if !parsed.username().is_empty() || parsed.password().is_some() {
                // Clear credentials - set_username/set_password return Err only for cannot-be-a-base URLs
                let _ = parsed.set_username("");
                let _ = parsed.set_password(None);
            }
            parsed.to_string()
        }
        Err(_) => endpoint.to_string(),
    }
}

fn parse_raw_setting_value(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

/// Check if the caller has sufficient role to operate on a target user.
///
/// Returns `Ok(())` if the caller's role is high enough to modify the target user,
/// or `Err(ApiError::Authorization(...))` if the role hierarchy would be violated.
///
/// Rules:
/// - Only Root can operate on Root users
/// - Only Root can operate on Admin users
/// - Admin and Root can operate on regular Users
fn check_role_hierarchy(
    caller_role: UserRole,
    target_role: UserRole,
    action: &str,
) -> Result<(), ApiError> {
    if target_role == UserRole::Root && caller_role != UserRole::Root {
        return Err(ApiError::Authorization(format!(
            "Only root users can {action} root users"
        )));
    }
    if target_role == UserRole::Admin && caller_role != UserRole::Root {
        return Err(ApiError::Authorization(format!(
            "Only root users can {action} admin users"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impls::ErrorKind;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    use synctv_cluster::sync::{ClusterEvent, ConnectionLimits, ConnectionManager};
    use synctv_core::models::{
        MemberStatus, RoomId, RoomRole, RoomStatus, UserId, UserRole, UserStatus,
    };
    use synctv_core::{
        cache::{KeyBuilder, UsernameCache},
        config::{Config, PasswordComplexityConfig},
        repository::{
            MediaRepository, ProviderInstanceRepository, RoomMemberRepository, RoomRepository,
            SettingsRepository, UserRepository,
        },
        service::{
            auth::{BruteForceProtection, JwtService, TestPasswordHasher},
            AuditService, EmailService, InMemoryTokenBlacklistStore, PublishKeyService,
            RemoteProviderManager, SettingsRegistry, SettingsService, UserService,
        },
    };
    use synctv_core_testing::create_test_pool;
    use synctv_livestream::{
        api::{LiveStreamingInfrastructure, StreamTracker},
        livestream::{external_publish_manager::ExternalPublishManager, PullStreamManager},
    };
    use tokio::sync::mpsc;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum MembershipEventFanoutCall {
        ReservePermissionChanged,
        PublishPermissionChanged {
            room_id: String,
            target_user_id: String,
            changed_by: String,
        },
        ReserveUserLeft,
        PublishUserLeft {
            room_id: String,
            user_id: String,
        },
    }

    #[derive(Default)]
    struct RecordingMembershipEventFanout {
        calls: Mutex<Vec<MembershipEventFanoutCall>>,
    }

    impl RecordingMembershipEventFanout {
        fn take_calls(&self) -> Vec<MembershipEventFanoutCall> {
            let mut calls = self.calls.lock().expect("membership fanout calls lock");
            std::mem::take(&mut *calls)
        }

        fn push(&self, call: MembershipEventFanoutCall) {
            self.calls
                .lock()
                .expect("membership fanout calls lock")
                .push(call);
        }
    }

    #[async_trait]
    impl MembershipEventFanoutService for RecordingMembershipEventFanout {
        async fn reserve_permission_changed(
            &self,
        ) -> Result<Option<crate::impls::ClusterEventPublishReservation>, ApiError> {
            self.push(MembershipEventFanoutCall::ReservePermissionChanged);
            Ok(None)
        }

        async fn publish_permission_changed(
            &self,
            room_id: &RoomId,
            target_user_id: &UserId,
            changed_by: &UserId,
            _reservation: Option<crate::impls::ClusterEventPublishReservation>,
        ) -> Result<(), ApiError> {
            self.push(MembershipEventFanoutCall::PublishPermissionChanged {
                room_id: room_id.as_str().to_string(),
                target_user_id: target_user_id.as_str().to_string(),
                changed_by: changed_by.as_str().to_string(),
            });
            Ok(())
        }

        async fn reserve_user_left(
            &self,
        ) -> Result<Option<crate::impls::ClusterEventPublishReservation>, ApiError> {
            self.push(MembershipEventFanoutCall::ReserveUserLeft);
            Ok(None)
        }

        async fn publish_user_left(
            &self,
            room_id: &RoomId,
            user_id: &UserId,
            _reservation: Option<crate::impls::ClusterEventPublishReservation>,
        ) -> Result<(), ApiError> {
            self.push(MembershipEventFanoutCall::PublishUserLeft {
                room_id: room_id.as_str().to_string(),
                user_id: user_id.as_str().to_string(),
            });
            Ok(())
        }
    }

    #[test]
    fn local_management_actor_id_fits_fixed_width_audit_columns() {
        assert!(
            LOCAL_MANAGEMENT_ACTOR_USER_ID.len() <= 12,
            "local management actor id must fit audit_logs.actor_id CHAR(12)"
        );
    }

    #[test]
    fn test_map_batch_result_error_preserves_business_message() {
        let message = map_batch_result_error(ApiError::Authorization(
            "Only root users can ban admin users".to_string(),
        ));

        assert_eq!(message, "Only root users can ban admin users");
    }

    #[test]
    fn test_map_batch_result_error_sanitizes_internal_error() {
        let message = map_batch_result_error(ApiError::Internal(
            "sqlx error: relation users does not exist".to_string(),
        ));

        assert_eq!(message, "Operation failed due to an internal error");
        assert!(!message.contains("sqlx"));
    }

    #[test]
    fn test_map_batch_result_error_sanitizes_service_unavailable_error() {
        let message = map_batch_result_error(ApiError::ServiceUnavailable(
            "Redis timeout while publishing cluster event".to_string(),
        ));

        assert_eq!(
            message,
            "Operation failed because the service is temporarily unavailable"
        );
        assert!(!message.contains("Redis timeout"));
    }

    #[test]
    fn test_live_streaming_unavailable_error_is_service_unavailable() {
        let err = live_streaming_unavailable_error();
        assert!(matches!(err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(
            err.message(),
            "Live streaming is not available on this server."
        );
    }

    #[test]
    fn test_publish_key_service_unavailable_error_is_service_unavailable() {
        let err = publish_key_service_unavailable_error();
        assert!(matches!(err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(
            err.message(),
            "Publish key service is not available on this server."
        );
    }

    #[test]
    fn test_map_send_test_email_result_success_is_human_readable() {
        let response = AdminApiImpl::map_send_test_email_result("test@example.com", Ok(()));

        assert!(response.success);
        assert_eq!(
            response.message,
            "Test email sent successfully to test@example.com"
        );
    }

    #[test]
    fn test_map_send_test_email_result_failure_is_sanitized() {
        let response = AdminApiImpl::map_send_test_email_result(
            "test@example.com",
            Err(CoreError::Internal(
                "smtp connect ECONNREFUSED 127.0.0.1:587".to_string(),
            )),
        );

        assert!(!response.success);
        assert_eq!(
            response.message,
            "Failed to send test email. Please verify the email configuration and try again."
        );
        assert!(
            !response.message.contains("ECONNREFUSED"),
            "internal transport details must not leak to clients"
        );
    }

    fn make_user_service(pool: sqlx::PgPool) -> UserService {
        let jwt_service =
            JwtService::new("test-secret-key-for-admin-impl-tests-minimum-32-chars").expect("jwt");
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 128, 60);
        let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> =
            Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400));

        let mut user_service = UserService::new(
            pool,
            jwt_service,
            username_cache,
            PasswordComplexityConfig::default(),
            token_blacklist,
            KeyBuilder::new("test"),
            BruteForceProtection::in_memory("test".to_string()),
        );
        user_service.set_password_hasher(Arc::new(TestPasswordHasher::new()));
        user_service
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_validate_admin_auth_rejects_rejected_user() {
        let (_postgres, pool) = create_test_pool().await;
        let user_service = make_user_service(pool.clone());
        let user_repo = UserRepository::new(pool);

        let rejected_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "rejected_admin_auth".to_string(),
            email: Some("rejected_admin_auth@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Admin,
            status: UserStatus::Rejected,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&rejected_admin)
            .await
            .expect("create rejected admin");

        let err = validate_admin_auth(&user_service, rejected_admin.id.clone(), 0, 0)
            .await
            .err()
            .expect("rejected admin must not pass admin auth");

        assert!(
            matches!(err, ApiError::Authentication(ref msg) if msg == "Authentication failed"),
            "rejected admin auth must fail with generic authentication error, got: {err:?}"
        );
    }

    async fn make_admin_api_for_delete_user_test(
        pool: sqlx::PgPool,
    ) -> (
        AdminApiImpl,
        tokio::sync::mpsc::Receiver<synctv_cluster::sync::PublishRequest>,
    ) {
        let user_service = Arc::new(make_user_service(pool.clone()));
        let mut room_service =
            synctv_core::service::RoomService::new(pool.clone(), (*user_service).clone());
        let settings_service = Arc::new(SettingsService::new(
            SettingsRepository::new(pool.clone()),
            pool.clone(),
        ));
        settings_service
            .initialize()
            .await
            .expect("settings initialized");
        let settings_registry = Arc::new(SettingsRegistry::new(settings_service.clone()));
        room_service.set_settings_registry(settings_registry.clone());
        let email_service = Arc::new(EmailService::new(None).expect("email service"));
        let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        connection_manager.start();
        let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
            ProviderInstanceRepository::new(pool.clone()),
        )));
        let room_service = Arc::new(room_service);
        room_service
            .media_service()
            .providers_manager()
            .create_builtin_defaults()
            .await
            .expect("builtin provider defaults should initialize");
        let audit_service = Arc::new(AuditService::new_unbuffered(pool));
        let config = Arc::new(Config::default());
        let publish_key_service = Arc::new(PublishKeyService::new(
            JwtService::new("test-secret-key-for-admin-impl-tests-minimum-32-chars").expect("jwt"),
            24,
        ));
        let (redis_publish_tx, redis_publish_rx) = tokio::sync::mpsc::channel(8);
        let provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver> = Arc::new(
            synctv_core::provider::ProviderStoreRegistry::local_only("test:provider:".to_string()),
        );

        (
            AdminApiImpl::new(
                room_service,
                user_service,
                settings_service,
                Some(settings_registry),
                email_service,
                connection_manager,
                provider_instance_manager,
                None,
                Some(publish_key_service),
                config,
                audit_service,
            )
            .with_cluster_fanout_service(crate::cluster_fanout::default_cluster_fanout_service(
                Some(redis_publish_tx),
                true,
            ))
            .with_provider_stores(provider_stores),
            redis_publish_rx,
        )
    }

    async fn make_admin_api_with_livestream_for_test(
        pool: sqlx::PgPool,
    ) -> (
        AdminApiImpl,
        Arc<LiveStreamingInfrastructure>,
        tokio::sync::mpsc::Receiver<synctv_cluster::sync::PublishRequest>,
    ) {
        let user_service = Arc::new(make_user_service(pool.clone()));
        let room_service = Arc::new(synctv_core::service::RoomService::new(
            pool.clone(),
            (*user_service).clone(),
        ));
        let settings_service = Arc::new(SettingsService::new(
            SettingsRepository::new(pool.clone()),
            pool.clone(),
        ));
        settings_service
            .initialize()
            .await
            .expect("settings initialized");
        let settings_registry = Arc::new(SettingsRegistry::new(settings_service.clone()));
        let email_service = Arc::new(EmailService::new(None).expect("email service"));
        let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        connection_manager.start();
        let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
            ProviderInstanceRepository::new(pool.clone()),
        )));
        room_service
            .media_service()
            .providers_manager()
            .create_builtin_defaults()
            .await
            .expect("builtin provider defaults should initialize");
        let audit_service = Arc::new(AuditService::new_unbuffered(pool));
        let config = Arc::new(Config::default());
        let publish_key_service = Arc::new(PublishKeyService::new(
            JwtService::new("test-secret-key-for-admin-impl-tests-minimum-32-chars").expect("jwt"),
            24,
        ));
        let (redis_publish_tx, redis_publish_rx) = tokio::sync::mpsc::channel(8);
        let provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver> = Arc::new(
            synctv_core::provider::ProviderStoreRegistry::local_only("test:provider:".to_string()),
        );

        let tracker = Arc::new(StreamTracker::new());
        let registry = synctv_livestream::relay::local_stream_registry();
        let (event_sender, _event_receiver) = mpsc::channel(64);
        let pull_manager = Arc::new(PullStreamManager::new(
            registry.clone(),
            event_sender.clone(),
        ));
        let external_publish_manager = Arc::new(
            ExternalPublishManager::new(
                registry.clone(),
                "node-local".to_string(),
                event_sender.clone(),
            )
            .expect("external publish manager should build"),
        );
        let live_streaming_infrastructure = Arc::new(LiveStreamingInfrastructure::new(
            registry,
            event_sender,
            pull_manager,
            external_publish_manager,
            tracker,
        ));

        (
            AdminApiImpl::new(
                room_service,
                user_service,
                settings_service,
                Some(settings_registry),
                email_service,
                connection_manager,
                provider_instance_manager,
                Some(live_streaming_infrastructure.clone()),
                Some(publish_key_service),
                config,
                audit_service,
            )
            .with_cluster_fanout_service(crate::cluster_fanout::default_cluster_fanout_service(
                Some(redis_publish_tx),
                true,
            ))
            .with_provider_stores(provider_stores),
            live_streaming_infrastructure,
            redis_publish_rx,
        )
    }

    async fn create_room_with_member(
        admin_api: &AdminApiImpl,
        owner_id: &UserId,
        member_id: &UserId,
    ) -> synctv_core::models::Room {
        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room for user lifecycle test".to_string(),
                owner_id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        admin_api
            .room_service
            .join_room(room.id.clone(), member_id.clone(), None)
            .await
            .expect("member should join room");

        room
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_add_member_publishes_permission_changed_membership_event() {
        let (_postgres, pool) = create_test_pool().await;
        let (mut admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_add_member_fanout".to_string(),
            email: Some("global_admin_add_member_fanout@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_add_member_fanout".to_string(),
            email: Some("room_owner_add_member_fanout@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target = synctv_core::models::User {
            id: UserId::new(),
            username: "target_add_member_fanout".to_string(),
            email: Some("target_add_member_fanout@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");
        user_repo.create(&target).await.expect("create target");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room for add member fanout test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let fanout = Arc::new(RecordingMembershipEventFanout::default());
        admin_api.membership_event_fanout = fanout.clone();

        let response = admin_api
            .add_member(
                crate::proto::admin::AddMemberRequest {
                    room_id: room.id.as_str().to_string(),
                    user_id: target.id.as_str().to_string(),
                    role: synctv_proto::common::RoomMemberRole::Member as i32,
                    notify: false,
                },
                &global_admin.id,
                &RequestContext::default(),
            )
            .await
            .expect("admin add member should succeed");

        assert_eq!(
            response.member.expect("member should be returned").user_id,
            target.id.as_str()
        );
        assert_eq!(
            fanout.take_calls(),
            vec![MembershipEventFanoutCall::PublishPermissionChanged {
                room_id: room.id.as_str().to_string(),
                target_user_id: target.id.as_str().to_string(),
                changed_by: global_admin.id.as_str().to_string(),
            }]
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_update_member_permissions_reserves_and_publishes_membership_event() {
        let (_postgres, pool) = create_test_pool().await;
        let (mut admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_update_member_fanout".to_string(),
            email: Some("global_admin_update_member_fanout@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_update_member_fanout".to_string(),
            email: Some("room_owner_update_member_fanout@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target = synctv_core::models::User {
            id: UserId::new(),
            username: "target_update_member_fanout".to_string(),
            email: Some("target_update_member_fanout@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");
        user_repo.create(&target).await.expect("create target");

        let room = create_room_with_member(&admin_api, &owner.id, &target.id).await;

        let fanout = Arc::new(RecordingMembershipEventFanout::default());
        admin_api.membership_event_fanout = fanout.clone();

        let response = admin_api
            .update_member_permissions(
                crate::proto::admin::UpdateMemberPermissionsRequest {
                    room_id: room.id.as_str().to_string(),
                    user_id: target.id.as_str().to_string(),
                    role: synctv_proto::common::RoomMemberRole::Member as i32,
                    added_permissions: 0b100,
                    removed_permissions: 0b010,
                    admin_added_permissions: 0,
                    admin_removed_permissions: 0,
                },
                &global_admin.id,
                &RequestContext::default(),
            )
            .await
            .expect("admin update member permissions should succeed");

        assert_eq!(
            response.member.expect("member should be returned").user_id,
            target.id.as_str()
        );
        assert_eq!(
            fanout.take_calls(),
            vec![
                MembershipEventFanoutCall::ReservePermissionChanged,
                MembershipEventFanoutCall::PublishPermissionChanged {
                    room_id: room.id.as_str().to_string(),
                    target_user_id: target.id.as_str().to_string(),
                    changed_by: global_admin.id.as_str().to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_kick_member_reserves_and_publishes_membership_event() {
        let (_postgres, pool) = create_test_pool().await;
        let (mut admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_kick_member_fanout".to_string(),
            email: Some("global_admin_kick_member_fanout@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_kick_member_fanout".to_string(),
            email: Some("room_owner_kick_member_fanout@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target = synctv_core::models::User {
            id: UserId::new(),
            username: "target_kick_member_fanout".to_string(),
            email: Some("target_kick_member_fanout@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");
        user_repo.create(&target).await.expect("create target");

        let room = create_room_with_member(&admin_api, &owner.id, &target.id).await;

        let fanout = Arc::new(RecordingMembershipEventFanout::default());
        admin_api.membership_event_fanout = fanout.clone();

        let response = admin_api
            .kick_member(
                crate::proto::admin::KickMemberRequest {
                    room_id: room.id.as_str().to_string(),
                    user_id: target.id.as_str().to_string(),
                },
                &global_admin.id,
                &RequestContext::default(),
            )
            .await
            .expect("admin kick member should succeed");

        assert!(response.success);
        assert_eq!(
            fanout.take_calls(),
            vec![
                MembershipEventFanoutCall::ReservePermissionChanged,
                MembershipEventFanoutCall::PublishPermissionChanged {
                    room_id: room.id.as_str().to_string(),
                    target_user_id: target.id.as_str().to_string(),
                    changed_by: global_admin.id.as_str().to_string(),
                },
            ]
        );
    }

    async fn create_room_media(
        pool: &sqlx::PgPool,
        room_id: RoomId,
        creator_id: UserId,
        name: &str,
    ) -> synctv_core::models::Media {
        let media_repo = MediaRepository::new(pool.clone());
        let mut tx = pool.begin().await.expect("begin media test transaction");
        let position = media_repo
            .get_next_append_position_with_tx(&room_id, None, &mut tx)
            .await
            .expect("compute next media position");
        let media = synctv_core::models::Media::from_direct_single_mode(
            None,
            room_id,
            Some(creator_id),
            name.to_string(),
            "direct",
            synctv_core::models::PlaybackInfo::single_url(
                "https://example.com/video.mp4".to_string(),
                "default".to_string(),
            ),
            position,
        );
        let created = media_repo
            .create_with_executor(&media, &mut *tx)
            .await
            .expect("create media");
        tx.commit().await.expect("commit media test transaction");
        created
    }

    fn make_test_room_model(created_by: &UserId) -> synctv_core::models::Room {
        let now = chrono::Utc::now();
        synctv_core::models::Room {
            id: RoomId::new(),
            name: "room-ban-test".to_string(),
            description: "room for admin ban test".to_string(),
            created_by: created_by.clone(),
            status: RoomStatus::Active,
            is_banned: false,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 0,
            last_activity_at: now,
        }
    }

    #[test]
    fn test_parse_timeout_to_seconds_valid() {
        assert_eq!(parse_timeout_to_seconds("10s"), 10);
        assert_eq!(parse_timeout_to_seconds("30s"), 30);
        assert_eq!(parse_timeout_to_seconds("0s"), 0);
        assert_eq!(parse_timeout_to_seconds("300s"), 300);
    }

    #[test]
    fn test_parse_timeout_to_seconds_no_suffix() {
        assert_eq!(parse_timeout_to_seconds("10"), 10);
    }

    #[test]
    fn test_parse_timeout_to_seconds_invalid() {
        assert_eq!(parse_timeout_to_seconds("abc"), 10); // Default fallback
        assert_eq!(parse_timeout_to_seconds(""), 10); // Empty string
    }

    #[test]
    fn test_seconds_to_timeout_string() {
        assert_eq!(seconds_to_timeout_string(10), "10s");
        assert_eq!(seconds_to_timeout_string(0), "0s");
        assert_eq!(seconds_to_timeout_string(300), "300s");
    }

    #[test]
    fn test_timeout_roundtrip() {
        for secs in [0, 1, 10, 30, 60, 300] {
            let s = seconds_to_timeout_string(secs);
            assert_eq!(parse_timeout_to_seconds(&s), secs);
        }
    }

    fn make_test_room(status: RoomStatus) -> synctv_core::models::Room {
        let now = chrono::Utc::now();
        synctv_core::models::Room {
            id: RoomId::from_string("admin_room_1".to_string()),
            name: "Admin Test Room".to_string(),
            description: "Room for admin tests".to_string(),
            created_by: UserId::from_string("creator_1".to_string()),
            status,
            is_banned: false,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 1,
            last_activity_at: now,
        }
    }

    #[test]
    fn test_admin_room_to_proto_basic() {
        let room = make_test_room(RoomStatus::Active);
        let proto = admin_room_to_proto(
            &room,
            None,
            Some(10),
            Some("creator_user"),
            UserStatus::Active,
        );

        assert_eq!(proto.id, "admin_room_1");
        assert_eq!(proto.name, "Admin Test Room");
        assert_eq!(proto.description, "Room for admin tests");
        assert_eq!(proto.creator_id, "creator_1");
        assert_eq!(proto.creator_username, "creator_user");
        assert_eq!(
            proto.creator_status,
            synctv_proto::common::UserStatus::Active as i32
        );
        assert_eq!(proto.member_count, 10);
        assert!(!proto.is_banned);
    }

    #[test]
    fn test_admin_room_to_proto_banned() {
        let mut room = make_test_room(RoomStatus::Active);
        room.is_banned = true;
        let proto = admin_room_to_proto(&room, None, None, None, UserStatus::Banned);
        assert!(proto.is_banned);
        assert_eq!(proto.member_count, 0);
        assert_eq!(
            proto.creator_status,
            synctv_proto::common::UserStatus::Banned as i32
        );
    }

    #[test]
    fn test_admin_room_to_proto_different_statuses() {
        for status in [RoomStatus::Active, RoomStatus::Pending, RoomStatus::Closed] {
            let room = make_test_room(status);
            let proto = admin_room_to_proto(&room, None, None, None, UserStatus::Active);
            assert_eq!(
                proto.status,
                synctv_proto::common::RoomStatus::from(status) as i32
            );
        }
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_load_room_creator_status_maps_true_missing_creator_to_rejected() {
        let (_postgres, pool) = create_test_pool().await;
        let user_service = make_user_service(pool.clone());
        let room = make_test_room(RoomStatus::Active);

        let status = load_room_creator_status(&user_service, &room)
            .await
            .expect("missing creator should map to rejected");

        assert_eq!(status, UserStatus::Rejected);
        pool.close().await;
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_load_room_creator_status_propagates_backend_failures() {
        let (_postgres, pool) = create_test_pool().await;
        let user_service = make_user_service(pool.clone());
        let room = make_test_room(RoomStatus::Active);

        pool.close().await;

        let error = load_room_creator_status(&user_service, &room)
            .await
            .expect_err("creator lookup backend failures must propagate");

        assert!(
            matches!(
                error,
                ApiError::Internal(_) | ApiError::ServiceUnavailable(_)
            ),
            "unexpected error kind: {error:?}"
        );
    }

    #[tokio::test]
    async fn test_active_room_stream_media_ids_unions_local_and_registry_streams() {
        let tracker = Arc::new(StreamTracker::new());
        tracker.insert(
            "user-local".to_string(),
            "room-1".to_string(),
            "local-media".to_string(),
            "rtmp-room",
            "rtmp-stream",
        );
        tracker.insert(
            "user-overlap".to_string(),
            "room-1".to_string(),
            "shared-media".to_string(),
            "rtmp-room-2",
            "rtmp-stream-2",
        );

        let registry = synctv_livestream::relay::local_stream_registry();
        registry
            .try_register_publisher(
                "room-1",
                "shared-media",
                "node-a",
                "user-overlap",
                "127.0.0.1:50051",
            )
            .await
            .expect("shared publisher should register");
        registry
            .try_register_publisher(
                "room-1",
                "remote-media",
                "node-b",
                "user-remote",
                "127.0.0.1:50052",
            )
            .await
            .expect("remote publisher should register");
        registry
            .try_register_publisher(
                "other-room",
                "other-media",
                "node-c",
                "user-other",
                "127.0.0.1:50053",
            )
            .await
            .expect("other room publisher should register");

        let (event_sender, _event_receiver) = mpsc::channel(64);
        let pull_manager = Arc::new(PullStreamManager::new(
            registry.clone(),
            event_sender.clone(),
        ));
        let external_publish_manager = Arc::new(
            ExternalPublishManager::new(
                registry.clone(),
                "node-local".to_string(),
                event_sender.clone(),
            )
            .expect("external publish manager should build"),
        );
        let infra = Arc::new(LiveStreamingInfrastructure::new(
            registry,
            event_sender,
            pull_manager,
            external_publish_manager,
            tracker,
        ));

        let media_ids = active_room_stream_media_ids_for_infra(Some(&infra), "room-1").await;

        assert_eq!(
            media_ids,
            vec![
                "local-media".to_string(),
                "remote-media".to_string(),
                "shared-media".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn test_force_disconnect_user_publishes_cluster_kick_event() {
        let connection_service: Arc<dyn RealtimeConnectionService> = Arc::new(
            ConnectionManager::new(synctv_cluster::sync::ConnectionLimits::default()),
        );
        connection_service.start();

        let (publish_tx, mut publish_rx) = mpsc::channel(4);
        let user_id = UserId::from_string("user-force-disconnect".to_string());
        let realtime_lifecycle = default_realtime_lifecycle_service(
            connection_service,
            None,
            crate::cluster_fanout::default_cluster_fanout_service(Some(publish_tx), true),
        );

        realtime_lifecycle
            .disconnect_user(&user_id, "user_deleted")
            .await;

        let published = publish_rx
            .recv()
            .await
            .expect("force_disconnect_user should publish a kick event");
        match published.event {
            ClusterEvent::KickUser {
                user_id: published_user_id,
                reason,
                ..
            } => {
                assert_eq!(published_user_id, user_id);
                assert_eq!(reason, "user_deleted");
            }
            other => panic!("expected KickUser event, got {other:?}"),
        }
    }

    fn make_test_user(role: UserRole, status: UserStatus) -> synctv_core::models::User {
        synctv_core::models::User {
            id: UserId::from_string("admin_user_1".to_string()),
            username: "admin_test".to_string(),
            email: Some("admin@test.com".to_string()),
            password_hash: "hash".to_string(),
            role,
            status,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        }
    }

    #[test]
    fn test_admin_user_to_proto_all_roles() {
        for (role, expected) in [
            (UserRole::Root, synctv_proto::common::UserRole::Root as i32),
            (
                UserRole::Admin,
                synctv_proto::common::UserRole::Admin as i32,
            ),
            (UserRole::User, synctv_proto::common::UserRole::User as i32),
        ] {
            let user = make_test_user(role, UserStatus::Active);
            let proto = admin_user_to_proto(&user);
            assert_eq!(proto.role, expected);
        }
    }

    #[test]
    fn test_admin_user_to_proto_all_statuses() {
        for (status, expected) in [
            (
                UserStatus::Active,
                synctv_proto::common::UserStatus::Active as i32,
            ),
            (
                UserStatus::Pending,
                synctv_proto::common::UserStatus::Pending as i32,
            ),
            (
                UserStatus::Banned,
                synctv_proto::common::UserStatus::Banned as i32,
            ),
        ] {
            let user = make_test_user(UserRole::User, status);
            let proto = admin_user_to_proto(&user);
            assert_eq!(proto.status, expected);
        }
    }

    #[test]
    fn test_admin_user_to_proto_fields() {
        let user = make_test_user(UserRole::Admin, UserStatus::Active);
        let proto = admin_user_to_proto(&user);

        assert_eq!(proto.id, "admin_user_1");
        assert_eq!(proto.username, "admin_test");
        assert_eq!(proto.email, "admin@test.com");
    }

    #[test]
    fn test_admin_user_to_proto_no_email() {
        let mut user = make_test_user(UserRole::User, UserStatus::Active);
        user.email = None;
        let proto = admin_user_to_proto(&user);
        assert_eq!(proto.email, "");
    }

    #[test]
    fn test_update_user_password_user_lookup_backend_failure_stays_service_unavailable() {
        let mapped = AdminApiImpl::map_target_user_lookup_error(
            synctv_core::Error::ServiceUnavailable("user lookup unavailable".to_string()),
        );

        assert!(
            matches!(mapped, ApiError::ServiceUnavailable(ref msg) if msg == "user lookup unavailable"),
            "user lookup backend failures must not be reported as not found, got: {mapped:?}"
        );
    }

    #[test]
    fn test_update_user_password_user_lookup_not_found_stays_not_found() {
        let mapped = AdminApiImpl::map_target_user_lookup_error(synctv_core::Error::NotFound(
            "missing row".to_string(),
        ));

        assert!(
            matches!(mapped, ApiError::NotFound(ref msg) if msg == "User not found"),
            "true user misses must remain not found, got: {mapped:?}"
        );
    }

    #[test]
    fn test_admin_auth_user_lookup_backend_failure_stays_service_unavailable() {
        let mapped = AdminApiImpl::map_admin_auth_user_lookup_error(
            synctv_core::Error::ServiceUnavailable("user backend unavailable".to_string()),
        );

        assert!(
            matches!(mapped, ApiError::ServiceUnavailable(ref msg) if msg == "user backend unavailable"),
            "admin auth backend failures must not be reported as authentication failures, got: {mapped:?}"
        );
    }

    #[test]
    fn test_admin_auth_user_lookup_not_found_stays_authentication_failed() {
        let mapped = AdminApiImpl::map_admin_auth_user_lookup_error(synctv_core::Error::NotFound(
            "missing row".to_string(),
        ));

        assert!(
            matches!(mapped, ApiError::Authentication(ref msg) if msg == "Authentication failed"),
            "missing admin users should still be treated as authentication failure, got: {mapped:?}"
        );
    }

    fn make_test_member(role: RoomRole) -> synctv_core::models::RoomMemberWithUser {
        synctv_core::models::RoomMemberWithUser {
            room_id: RoomId::from_string("room1".to_string()),
            user_id: UserId::from_string("user1".to_string()),
            username: "testmember".to_string(),
            role,
            status: MemberStatus::Active,
            added_permissions: 0,
            removed_permissions: 0,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            joined_at: chrono::Utc::now(),
            is_online: false,
            is_active: true,
            banned_at: None,
            banned_reason: None,
        }
    }

    #[test]
    fn test_admin_room_member_to_proto() {
        let member = make_test_member(RoomRole::Admin);
        let proto = admin_room_member_to_proto(&member);

        assert_eq!(proto.room_id, "room1");
        assert_eq!(proto.user_id, "user1");
        assert_eq!(proto.username, "testmember");
        assert_eq!(
            proto.role,
            synctv_proto::common::RoomMemberRole::Admin as i32
        );
        assert!(!proto.is_online);
    }

    #[test]
    fn test_admin_room_member_to_proto_with_permissions() {
        let mut member = make_test_member(RoomRole::Member);
        member.added_permissions = 0xAA;
        member.removed_permissions = 0x55;
        member.admin_added_permissions = 0xCC;
        member.admin_removed_permissions = 0x33;
        let proto = admin_room_member_to_proto(&member);

        assert_eq!(proto.added_permissions, 0xAA);
        assert_eq!(proto.removed_permissions, 0x55);
        assert_eq!(proto.admin_added_permissions, 0xCC);
        assert_eq!(proto.admin_removed_permissions, 0x33);
    }

    #[test]
    fn test_provider_instance_to_proto_enabled() {
        let instance = synctv_core::models::ProviderInstance {
            name: "test_provider".to_string(),
            endpoint: "https://example.com".to_string(),
            comment: Some("A test provider".to_string()),
            jwt_secret: None,
            custom_ca: None,
            timeout: "30s".to_string(),
            tls: true,
            insecure_tls: false,
            providers: vec!["bilibili".to_string(), "alist".to_string()],
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let proto = provider_instance_to_proto(instance);

        assert_eq!(proto.name, "test_provider");
        assert_eq!(proto.endpoint, "https://example.com");
        assert_eq!(proto.comment, "A test provider");
        assert_eq!(proto.timeout_seconds, 30);
        assert!(proto.tls);
        assert!(!proto.insecure_tls);
        assert_eq!(proto.providers, vec!["bilibili", "alist"]);
        assert!(proto.enabled);
        assert_eq!(
            proto.status,
            i32::from(crate::proto::admin::ProviderInstanceStatus::Unspecified)
        );
    }

    #[test]
    fn test_provider_instance_to_proto_disabled() {
        let instance = synctv_core::models::ProviderInstance {
            name: "disabled_provider".to_string(),
            endpoint: "https://disabled.example.com".to_string(),
            comment: None,
            jwt_secret: None,
            custom_ca: None,
            timeout: "10s".to_string(),
            tls: false,
            insecure_tls: false,
            providers: vec![],
            enabled: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let proto = provider_instance_to_proto(instance);

        assert_eq!(
            proto.status,
            i32::from(crate::proto::admin::ProviderInstanceStatus::Disconnected)
        );
        assert_eq!(proto.comment, ""); // None -> empty
        assert!(!proto.enabled);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_provider_instances_filters_by_provider_type() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let repository = ProviderInstanceRepository::new(pool);

        for instance in [
            synctv_core::models::ProviderInstance {
                name: "alist-edge".to_string(),
                endpoint: "https://alist.example.com".to_string(),
                comment: Some("alist provider".to_string()),
                jwt_secret: None,
                custom_ca: None,
                timeout: "10s".to_string(),
                tls: true,
                insecure_tls: false,
                providers: vec!["alist".to_string()],
                enabled: false,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
            synctv_core::models::ProviderInstance {
                name: "emby-edge".to_string(),
                endpoint: "https://emby.example.com".to_string(),
                comment: Some("emby provider".to_string()),
                jwt_secret: None,
                custom_ca: None,
                timeout: "10s".to_string(),
                tls: true,
                insecure_tls: false,
                providers: vec!["emby".to_string()],
                enabled: false,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        ] {
            repository
                .create(&instance)
                .await
                .expect("provider instance should be created");
        }

        let response = admin_api
            .list_provider_instances(crate::proto::admin::ListProviderInstancesRequest {
                page: 1,
                page_size: 50,
                provider_type: "alist".to_string(),
                search: String::new(),
                enabled: None,
                tls: None,
                sort_by: crate::proto::admin::ProviderInstanceListSortBy::CreatedAt as i32,
                sort_direction: crate::proto::admin::SortDirection::Desc as i32,
            })
            .await
            .expect("provider list should succeed");

        let names: Vec<String> = response
            .instances
            .into_iter()
            .map(|instance| instance.name)
            .collect();
        assert_eq!(names, vec!["alist-edge".to_string()]);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_admins_includes_root_and_admin_only() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool);
        let now = chrono::Utc::now();

        for user in [
            synctv_core::models::User {
                id: UserId::new(),
                username: "root-zeta".to_string(),
                email: Some("root-zeta@example.com".to_string()),
                password_hash: "hash".to_string(),
                role: UserRole::Root,
                status: UserStatus::Active,
                signup_method: synctv_core::models::SignupMethod::Email,
                email_verified: true,
                created_at: now,
                updated_at: now,
                deleted_at: None,
                password_changed_at: now,
                password_version: 0,
                version: 0,
            },
            synctv_core::models::User {
                id: UserId::new(),
                username: "admin-alpha".to_string(),
                email: Some("admin-alpha@example.com".to_string()),
                password_hash: "hash".to_string(),
                role: UserRole::Admin,
                status: UserStatus::Active,
                signup_method: synctv_core::models::SignupMethod::Email,
                email_verified: true,
                created_at: now,
                updated_at: now,
                deleted_at: None,
                password_changed_at: now,
                password_version: 0,
                version: 0,
            },
            synctv_core::models::User {
                id: UserId::new(),
                username: "user-ignored".to_string(),
                email: Some("user-ignored@example.com".to_string()),
                password_hash: "hash".to_string(),
                role: UserRole::User,
                status: UserStatus::Active,
                signup_method: synctv_core::models::SignupMethod::Email,
                email_verified: true,
                created_at: now,
                updated_at: now,
                deleted_at: None,
                password_changed_at: now,
                password_version: 0,
                version: 0,
            },
        ] {
            user_repo
                .create(&user)
                .await
                .expect("user should be created");
        }

        let response = admin_api
            .list_admins(crate::proto::admin::ListAdminsRequest {
                page: 1,
                page_size: 10,
                search: String::new(),
                sort_by: crate::proto::admin::UserListSortBy::Username as i32,
                sort_direction: crate::proto::admin::SortDirection::Asc as i32,
            })
            .await
            .expect("list admins should succeed");

        let usernames: Vec<String> = response
            .admins
            .into_iter()
            .map(|user| user.username)
            .collect();
        assert_eq!(
            usernames,
            vec!["admin-alpha".to_string(), "root-zeta".to_string()]
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_provider_instances_respects_search_filters_sort_and_pagination() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let repository = ProviderInstanceRepository::new(pool);

        for instance in [
            synctv_core::models::ProviderInstance {
                name: "beta-edge".to_string(),
                endpoint: "https://beta.example.com".to_string(),
                comment: Some("edge provider".to_string()),
                jwt_secret: None,
                custom_ca: None,
                timeout: "10s".to_string(),
                tls: true,
                insecure_tls: false,
                providers: vec!["alist".to_string()],
                enabled: true,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
            synctv_core::models::ProviderInstance {
                name: "alpha-edge".to_string(),
                endpoint: "https://alpha.example.com".to_string(),
                comment: Some("edge provider".to_string()),
                jwt_secret: None,
                custom_ca: None,
                timeout: "10s".to_string(),
                tls: true,
                insecure_tls: false,
                providers: vec!["alist".to_string()],
                enabled: true,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
            synctv_core::models::ProviderInstance {
                name: "gamma-edge".to_string(),
                endpoint: "https://gamma.example.com".to_string(),
                comment: Some("edge provider".to_string()),
                jwt_secret: None,
                custom_ca: None,
                timeout: "10s".to_string(),
                tls: true,
                insecure_tls: false,
                providers: vec!["alist".to_string()],
                enabled: false,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
            synctv_core::models::ProviderInstance {
                name: "delta-off".to_string(),
                endpoint: "https://delta.example.com".to_string(),
                comment: Some("other provider".to_string()),
                jwt_secret: None,
                custom_ca: None,
                timeout: "10s".to_string(),
                tls: false,
                insecure_tls: false,
                providers: vec!["alist".to_string()],
                enabled: true,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        ] {
            repository
                .create(&instance)
                .await
                .expect("provider instance should be created");
        }

        let response = admin_api
            .list_provider_instances(crate::proto::admin::ListProviderInstancesRequest {
                page: 2,
                page_size: 1,
                provider_type: "alist".to_string(),
                search: "edge".to_string(),
                enabled: Some(true),
                tls: Some(true),
                sort_by: crate::proto::admin::ProviderInstanceListSortBy::Name as i32,
                sort_direction: crate::proto::admin::SortDirection::Asc as i32,
            })
            .await
            .expect("provider list should succeed");

        let names: Vec<String> = response
            .instances
            .into_iter()
            .map(|instance| instance.name)
            .collect();
        assert_eq!(names, vec!["beta-edge".to_string()]);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_list_endpoints_reject_invalid_proto_requests() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;

        let list_rooms_error = admin_api
            .list_rooms(crate::proto::admin::ListRoomsRequest {
                page: -1,
                page_size: 101,
                status: synctv_proto::common::RoomStatus::Unspecified as i32,
                search: String::new(),
                creator_id: String::new(),
                is_banned: None,
                sort_by: crate::proto::admin::RoomListSortBy::Unspecified as i32,
                sort_direction: crate::proto::admin::SortDirection::Unspecified as i32,
            })
            .await
            .expect_err("invalid list_rooms request must be rejected");
        assert!(matches!(list_rooms_error, ApiError::InvalidInput(_)));

        let get_room_members_error = admin_api
            .get_room_members(crate::proto::admin::GetRoomMembersRequest {
                room_id: "abc123def456".to_string(),
                page: -1,
                page_size: 101,
                search: "a".repeat(101),
                role: synctv_proto::common::RoomMemberRole::Unspecified as i32,
                status: synctv_proto::common::MemberStatus::Unspecified as i32,
                sort_by: crate::proto::admin::RoomMemberListSortBy::Unspecified as i32,
                sort_direction: crate::proto::admin::SortDirection::Unspecified as i32,
            })
            .await
            .expect_err("invalid get_room_members request must be rejected");
        assert!(matches!(get_room_members_error, ApiError::InvalidInput(_)));

        let list_users_error = admin_api
            .list_users(crate::proto::admin::ListUsersRequest {
                page: -1,
                page_size: 101,
                status: synctv_proto::common::UserStatus::Unspecified as i32,
                role: synctv_proto::common::UserRole::Unspecified as i32,
                search: "a".repeat(101),
                sort_by: crate::proto::admin::UserListSortBy::Unspecified as i32,
                sort_direction: crate::proto::admin::SortDirection::Unspecified as i32,
            })
            .await
            .expect_err("invalid list_users request must be rejected");
        assert!(matches!(list_users_error, ApiError::InvalidInput(_)));

        let list_provider_instances_error = admin_api
            .list_provider_instances(crate::proto::admin::ListProviderInstancesRequest {
                page: -1,
                page_size: 101,
                provider_type: String::new(),
                search: String::new(),
                enabled: None,
                tls: None,
                sort_by: crate::proto::admin::ProviderInstanceListSortBy::Unspecified as i32,
                sort_direction: crate::proto::admin::SortDirection::Unspecified as i32,
            })
            .await
            .expect_err("invalid list_provider_instances request must be rejected");
        assert!(matches!(
            list_provider_instances_error,
            ApiError::InvalidInput(_)
        ));

        let get_user_rooms_error = admin_api
            .get_user_rooms(crate::proto::admin::GetUserRoomsRequest {
                user_id: "abc123def456".to_string(),
                page: -1,
                page_size: 101,
                status: synctv_proto::common::RoomStatus::Unspecified as i32,
                search: "a".repeat(101),
                is_banned: None,
                sort_by: crate::proto::admin::RoomListSortBy::Unspecified as i32,
                sort_direction: crate::proto::admin::SortDirection::Unspecified as i32,
            })
            .await
            .expect_err("invalid get_user_rooms request must be rejected");
        assert!(matches!(get_user_rooms_error, ApiError::InvalidInput(_)));

        let list_admins_error = admin_api
            .list_admins(crate::proto::admin::ListAdminsRequest {
                page: -1,
                page_size: 101,
                search: "a".repeat(101),
                sort_by: crate::proto::admin::UserListSortBy::Unspecified as i32,
                sort_direction: crate::proto::admin::SortDirection::Unspecified as i32,
            })
            .await
            .expect_err("invalid list_admins request must be rejected");
        assert!(matches!(list_admins_error, ApiError::InvalidInput(_)));

        let list_active_streams_error = admin_api
            .list_active_streams(crate::proto::admin::ListActiveStreamsRequest {
                page: -1,
                page_size: 101,
                room_id: String::new(),
                user_id: String::new(),
                node_id: String::new(),
                search: "a".repeat(101),
                sort_by: crate::proto::admin::ActiveStreamListSortBy::Unspecified as i32,
                sort_direction: crate::proto::admin::SortDirection::Unspecified as i32,
            })
            .await
            .expect_err("invalid list_active_streams request must be rejected");
        assert!(matches!(
            list_active_streams_error,
            ApiError::InvalidInput(_)
        ));
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_client_list_endpoints_reject_invalid_proto_requests() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool);
        let now = chrono::Utc::now();
        let admin_user = synctv_core::models::User {
            id: UserId::new(),
            username: "proto_list_admin".to_string(),
            email: Some("proto_list_admin@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            password_changed_at: now,
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&admin_user)
            .await
            .expect("create admin user");

        let list_playlists_error = admin_api
            .list_playlists(
                "abc123def456",
                crate::proto::client::ListPlaylistsRequest {
                    parent_id: String::new(),
                    page: 1,
                    page_size: 20,
                    search: String::new(),
                    source_provider: "Bad Provider".to_string(),
                    provider_instance_name: "bad name".to_string(),
                    dynamic_only: None,
                    sort_by: crate::proto::client::PlaylistListSortBy::Unspecified as i32,
                    sort_direction: crate::proto::client::SortDirection::Unspecified as i32,
                    availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
                },
                &admin_user.id,
            )
            .await
            .expect_err("invalid list_playlists request must be rejected");
        assert!(matches!(list_playlists_error, ApiError::InvalidInput(_)));

        let list_media_error = admin_api
            .list_media(
                "abc123def456",
                crate::proto::client::ListPlaylistItemsRequest {
                    playlist_id: String::new(),
                    target: Vec::new(),
                    page: 1,
                    page_size: 20,
                    search: String::new(),
                    source_provider: "Bad Provider".to_string(),
                    provider_instance_name: "bad name".to_string(),
                    sort_by: crate::proto::client::MediaListSortBy::Unspecified as i32,
                    sort_direction: crate::proto::client::SortDirection::Unspecified as i32,
                    availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
                },
                &admin_user.id,
            )
            .await
            .expect_err("invalid list_media request must be rejected");
        assert!(matches!(list_media_error, ApiError::InvalidInput(_)));
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_admins_respects_search_sort_and_pagination() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool);

        let now = chrono::Utc::now();
        for (username, role) in [
            ("zzz-admin", UserRole::Admin),
            ("alpha-admin", UserRole::Admin),
            ("plain-user", UserRole::User),
        ] {
            user_repo
                .create(&synctv_core::models::User {
                    id: UserId::new(),
                    username: username.to_string(),
                    email: Some(format!("{username}@example.com")),
                    password_hash: "hash".to_string(),
                    role,
                    status: UserStatus::Active,
                    signup_method: synctv_core::models::SignupMethod::Email,
                    email_verified: true,
                    created_at: now,
                    updated_at: now,
                    deleted_at: None,
                    password_changed_at: now,
                    password_version: 0,
                    version: 0,
                })
                .await
                .expect("test user should be created");
        }

        let response = admin_api
            .list_admins(crate::proto::admin::ListAdminsRequest {
                page: 1,
                page_size: 1,
                search: "admin".to_string(),
                sort_by: crate::proto::admin::UserListSortBy::Username as i32,
                sort_direction: crate::proto::admin::SortDirection::Asc as i32,
            })
            .await
            .expect("admin list should succeed");

        assert_eq!(response.admins.len(), 1);
        assert_eq!(response.admins[0].username, "alpha-admin");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_user_rooms_respects_related_room_query_semantics() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let now = chrono::Utc::now();
        let target_user = synctv_core::models::User {
            id: UserId::new(),
            username: "target-user-rooms".to_string(),
            email: Some("target-user-rooms@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            password_changed_at: now,
            password_version: 0,
            version: 0,
        };
        let other_owner = synctv_core::models::User {
            id: UserId::new(),
            username: "other-owner-rooms".to_string(),
            email: Some("other-owner-rooms@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            password_changed_at: now,
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&target_user)
            .await
            .expect("target user should be created");
        user_repo
            .create(&other_owner)
            .await
            .expect("other owner should be created");

        let owned_room = admin_api
            .room_service
            .create_room(
                "Beta Owned Room".to_string(),
                "owned room".to_string(),
                target_user.id.clone(),
                None,
                None,
            )
            .await
            .expect("owned room should be created")
            .0;
        let joined_room = admin_api
            .room_service
            .create_room(
                "Alpha Joined Room".to_string(),
                "joined room".to_string(),
                other_owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("joined room should be created")
            .0;
        admin_api
            .room_service
            .join_room(joined_room.id.clone(), target_user.id.clone(), None)
            .await
            .expect("target user should join related room");

        let response = admin_api
            .get_user_rooms(crate::proto::admin::GetUserRoomsRequest {
                user_id: target_user.id.as_str().to_string(),
                page: 1,
                page_size: 1,
                status: synctv_proto::common::RoomStatus::Unspecified as i32,
                search: "room".to_string(),
                is_banned: Some(false),
                sort_by: crate::proto::admin::RoomListSortBy::Name as i32,
                sort_direction: crate::proto::admin::SortDirection::Asc as i32,
            })
            .await
            .expect("related room list should succeed");

        assert_eq!(response.total, 2);
        assert_eq!(response.rooms.len(), 1);
        assert_eq!(response.rooms[0].name, "Alpha Joined Room");
        assert_eq!(response.rooms[0].id, joined_room.id.as_str());

        let page2 = admin_api
            .get_user_rooms(crate::proto::admin::GetUserRoomsRequest {
                user_id: target_user.id.as_str().to_string(),
                page: 2,
                page_size: 1,
                status: synctv_proto::common::RoomStatus::Unspecified as i32,
                search: "room".to_string(),
                is_banned: Some(false),
                sort_by: crate::proto::admin::RoomListSortBy::Name as i32,
                sort_direction: crate::proto::admin::SortDirection::Asc as i32,
            })
            .await
            .expect("second page should succeed");

        assert_eq!(page2.total, 2);
        assert_eq!(page2.rooms.len(), 1);
        assert_eq!(page2.rooms[0].name, "Beta Owned Room");
        assert_eq!(page2.rooms[0].id, owned_room.id.as_str());
    }

    // These verify the role hierarchy rules enforced by update_user_password:
    // - Root can reset anyone's password (root, admin, user)
    // - Admin can only reset regular user passwords
    // - Admin CANNOT reset root or other admin passwords

    /// Helper: check if a `caller_role` can reset a `target_role`'s password
    /// Returns true if the operation should be allowed.
    fn password_reset_allowed(caller_role: UserRole, target_role: UserRole) -> bool {
        if target_role == UserRole::Root && caller_role != UserRole::Root {
            return false;
        }
        if target_role == UserRole::Admin && caller_role != UserRole::Root {
            return false;
        }
        true
    }

    #[test]
    fn test_root_can_reset_any_password() {
        assert!(password_reset_allowed(UserRole::Root, UserRole::Root));
        assert!(password_reset_allowed(UserRole::Root, UserRole::Admin));
        assert!(password_reset_allowed(UserRole::Root, UserRole::User));
    }

    #[test]
    fn test_admin_cannot_reset_root_password() {
        assert!(!password_reset_allowed(UserRole::Admin, UserRole::Root));
    }

    #[test]
    fn test_admin_cannot_reset_other_admin_password() {
        assert!(!password_reset_allowed(UserRole::Admin, UserRole::Admin));
    }

    #[test]
    fn test_admin_can_reset_user_password() {
        assert!(password_reset_allowed(UserRole::Admin, UserRole::User));
    }

    #[test]
    fn test_mask_url_credentials_with_user_and_password() {
        let url = "https://user:secretpass@server.example.com/api";
        let masked = mask_url_credentials(url);
        assert!(
            !masked.contains("user:"),
            "Username should be stripped: {masked}"
        );
        assert!(
            !masked.contains("secretpass"),
            "Password should be stripped: {masked}"
        );
        assert!(
            masked.contains("server.example.com/api"),
            "Host and path should remain: {masked}"
        );
    }

    #[test]
    fn test_mask_url_credentials_with_user_only() {
        let url = "https://admin@server.example.com:8096/emby";
        let masked = mask_url_credentials(url);
        assert!(
            !masked.contains("admin@"),
            "Username should be stripped: {masked}"
        );
        assert!(
            masked.contains("server.example.com:8096/emby"),
            "Host and path should remain: {masked}"
        );
    }

    #[test]
    fn test_mask_url_credentials_without_credentials() {
        let url = "https://server.example.com/api";
        let masked = mask_url_credentials(url);
        assert_eq!(masked, "https://server.example.com/api");
    }

    #[test]
    fn test_mask_url_credentials_invalid_url_passthrough() {
        let url = "not-a-valid-url";
        let masked = mask_url_credentials(url);
        assert_eq!(masked, "not-a-valid-url");
    }

    #[test]
    fn test_mask_url_credentials_empty_string() {
        let masked = mask_url_credentials("");
        assert_eq!(masked, "");
    }

    #[test]
    fn test_mask_url_credentials_preserves_port_and_query() {
        let url = "https://user:pass@host.com:9090/path?key=val";
        let masked = mask_url_credentials(url);
        assert!(
            !masked.contains("user"),
            "Username should be stripped: {masked}"
        );
        assert!(
            !masked.contains("pass"),
            "Password should be stripped: {masked}"
        );
        assert!(
            masked.contains("host.com:9090"),
            "Host and port should remain: {masked}"
        );
        assert!(masked.contains("key=val"), "Query should remain: {masked}");
    }

    #[test]
    fn test_check_role_hierarchy_root_can_operate_on_all() {
        assert!(check_role_hierarchy(UserRole::Root, UserRole::Root, "ban").is_ok());
        assert!(check_role_hierarchy(UserRole::Root, UserRole::Admin, "ban").is_ok());
        assert!(check_role_hierarchy(UserRole::Root, UserRole::User, "ban").is_ok());
    }

    #[test]
    fn test_check_role_hierarchy_admin_cannot_operate_on_root() {
        let result = check_role_hierarchy(UserRole::Admin, UserRole::Root, "ban");
        assert!(
            result.is_err(),
            "Admin should not be able to ban Root users"
        );
        match result {
            Err(ApiError::Authorization(msg)) => {
                assert!(msg.contains("root"), "Error should mention root: {msg}");
            }
            other => panic!("Expected Authorization error, got: {other:?}"),
        }
    }

    #[test]
    fn test_check_role_hierarchy_admin_cannot_operate_on_admin() {
        let result = check_role_hierarchy(UserRole::Admin, UserRole::Admin, "delete");
        assert!(
            result.is_err(),
            "Admin should not be able to delete Admin users"
        );
        match result {
            Err(ApiError::Authorization(msg)) => {
                assert!(msg.contains("root"), "Error should mention root: {msg}");
            }
            other => panic!("Expected Authorization error, got: {other:?}"),
        }
    }

    #[test]
    fn test_check_role_hierarchy_admin_can_operate_on_user() {
        assert!(check_role_hierarchy(UserRole::Admin, UserRole::User, "ban").is_ok());
    }

    /// Verify that proto_role_to_user_role maps Admin role correctly
    /// (prerequisite for the role elevation check).
    #[test]
    fn test_proto_role_to_user_role_admin() {
        let admin_role = crate::impls::client::proto_role_to_user_role(
            synctv_proto::common::UserRole::Admin as i32,
        )
        .expect("should parse admin role");
        assert_eq!(admin_role, UserRole::Admin);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_settings_group_projects_registered_defaults() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;

        let response = admin_api
            .get_settings_group(
                crate::proto::admin::GetSettingsGroupRequest {
                    group: "server".to_string(),
                },
                &UserId::new(),
                &RequestContext::default(),
            )
            .await
            .expect("get_settings_group should project registered defaults");

        let group = response.group.expect("settings group response");
        assert_eq!(group.name, "server");

        let payload: serde_json::Value =
            serde_json::from_slice(&group.settings).expect("settings payload should be valid JSON");
        assert_eq!(payload["signup_enabled"], true);
        assert_eq!(payload["allow_room_creation"], true);
        assert_eq!(payload["max_rooms_per_user"], 10);
        assert_eq!(payload["max_members_per_room"], 100);
        assert_eq!(payload["max_chat_messages"], 500);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_settings_maps_group_entries_to_flat_keys_and_upserts_missing_rows() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) = make_admin_api_for_delete_user_test(pool).await;

        admin_api
            .update_settings(
                crate::proto::admin::UpdateSettingsRequest {
                    group: "server".to_string(),
                    settings: std::collections::HashMap::from([
                        ("signup_enabled".to_string(), "false".to_string()),
                        ("max_rooms_per_user".to_string(), "42".to_string()),
                    ]),
                },
                &UserId::new(),
                &RequestContext::default(),
            )
            .await
            .expect("update_settings should upsert missing flat settings");

        let signup_enabled = admin_api
            .settings_service
            .get("server.signup_enabled")
            .await
            .expect("signup_enabled should be persisted");
        assert_eq!(signup_enabled.group_name, "server");
        assert_eq!(signup_enabled.value, "false");

        let max_rooms_per_user = admin_api
            .settings_service
            .get("server.max_rooms_per_user")
            .await
            .expect("max_rooms_per_user should be persisted");
        assert_eq!(max_rooms_per_user.group_name, "server");
        assert_eq!(max_rooms_per_user.value, "42");

        let response = admin_api
            .get_settings_group(
                crate::proto::admin::GetSettingsGroupRequest {
                    group: "server".to_string(),
                },
                &UserId::new(),
                &RequestContext::default(),
            )
            .await
            .expect("get_settings_group should reflect persisted overrides");
        let group = response.group.expect("settings group response");
        let payload: serde_json::Value =
            serde_json::from_slice(&group.settings).expect("settings payload should be valid JSON");
        assert_eq!(payload["signup_enabled"], false);
        assert_eq!(payload["max_rooms_per_user"], 42);
        assert_eq!(payload["allow_room_creation"], true);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_user_publishes_kick_user_cluster_event() {
        let (_postgres, pool) = create_test_pool().await;
        let admin_api = {
            let (admin_api, redis_publish_rx) =
                make_admin_api_for_delete_user_test(pool.clone()).await;
            (admin_api, redis_publish_rx)
        };
        let user_repo = UserRepository::new(pool.clone());

        let admin_user = synctv_core::models::User {
            id: UserId::new(),
            username: "root_admin".to_string(),
            email: Some("root_admin@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target_user = synctv_core::models::User {
            id: UserId::new(),
            username: "victim_user".to_string(),
            email: Some("victim_user@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&admin_user).await.expect("create admin");
        user_repo.create(&target_user).await.expect("create user");

        let request = crate::proto::admin::DeleteUserRequest {
            user_id: target_user.id.as_str().to_string(),
        };
        let ctx = RequestContext::default();
        let (admin_api, mut redis_publish_rx) = admin_api;

        admin_api
            .delete_user(request, &admin_user.id, &ctx)
            .await
            .expect("delete user should succeed");

        let publish =
            tokio::time::timeout(std::time::Duration::from_secs(1), redis_publish_rx.recv())
                .await
                .expect("expected cluster publish")
                .expect("publish request");

        match publish.event {
            ClusterEvent::KickUser {
                user_id, reason, ..
            } => {
                assert_eq!(user_id, target_user.id);
                assert_eq!(reason, "user_deleted");
            }
            other => panic!("expected KickUser event, got {other:?}"),
        }
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_user_allows_missing_email_and_explicit_status() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let admin_user = synctv_core::models::User {
            id: UserId::new(),
            username: "root_create_user_attrs".to_string(),
            email: Some("root_create_user_attrs@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        UserRepository::new(pool.clone())
            .create(&admin_user)
            .await
            .expect("create root admin");

        let response = admin_api
            .create_user(
                crate::proto::admin::CreateUserRequest {
                    username: "attr_user".to_string(),
                    password: "StrongPwd12345!".to_string(),
                    email: String::new(),
                    role: synctv_proto::common::UserRole::Admin as i32,
                    status: synctv_proto::common::UserStatus::Active as i32,
                },
                UserRole::Root,
                &admin_user.id,
                &RequestContext::default(),
            )
            .await
            .expect("create_user should accept optional email and explicit status");

        let created = response.user.expect("created user");
        assert_eq!(created.username, "attr_user");
        assert_eq!(created.email, "");
        assert_eq!(created.role, synctv_proto::common::UserRole::Admin as i32);
        assert_eq!(
            created.status,
            synctv_proto::common::UserStatus::Active as i32
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_user_cleans_memberships_and_preserves_kick_user_event() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, mut redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let admin_user = synctv_core::models::User {
            id: UserId::new(),
            username: "root_delete_membership".to_string(),
            email: Some("root_delete_membership@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target_user = synctv_core::models::User {
            id: UserId::new(),
            username: "victim_membership".to_string(),
            email: Some("victim_membership@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&admin_user).await.expect("create admin");
        user_repo
            .create(&target_user)
            .await
            .expect("create target user");

        let owner_one = user_repo
            .create(&synctv_core::models::User {
                id: UserId::new(),
                username: "room_owner_one".to_string(),
                email: Some("room_owner_one@example.com".to_string()),
                password_hash: "hash".to_string(),
                role: UserRole::User,
                status: UserStatus::Active,
                signup_method: synctv_core::models::SignupMethod::Email,
                email_verified: true,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                deleted_at: None,
                password_changed_at: chrono::Utc::now(),
                password_version: 0,
                version: 0,
            })
            .await
            .expect("create owner one");
        let owner_two = user_repo
            .create(&synctv_core::models::User {
                id: UserId::new(),
                username: "room_owner_two".to_string(),
                email: Some("room_owner_two@example.com".to_string()),
                password_hash: "hash".to_string(),
                role: UserRole::User,
                status: UserStatus::Active,
                signup_method: synctv_core::models::SignupMethod::Email,
                email_verified: true,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                deleted_at: None,
                password_changed_at: chrono::Utc::now(),
                password_version: 0,
                version: 0,
            })
            .await
            .expect("create owner two");

        let room_one = create_room_with_member(&admin_api, &owner_one.id, &target_user.id).await;
        let room_two = create_room_with_member(&admin_api, &owner_two.id, &target_user.id).await;

        admin_api
            .delete_user(
                crate::proto::admin::DeleteUserRequest {
                    user_id: target_user.id.as_str().to_string(),
                },
                &admin_user.id,
                &RequestContext::default(),
            )
            .await
            .expect("delete user should succeed");

        let room_one_member = admin_api
            .room_service
            .get_member(&room_one.id, &target_user.id)
            .await
            .expect("room one membership query should succeed");
        assert!(
            room_one_member.is_none(),
            "deleted user must no longer appear as an active room member"
        );

        let room_two_member = admin_api
            .room_service
            .get_member(&room_two.id, &target_user.id)
            .await
            .expect("room two membership query should succeed");
        assert!(
            room_two_member.is_none(),
            "deleted user must no longer appear as an active room member"
        );

        let mut saw_kick_user = false;
        while let Ok(Some(publish)) = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            redis_publish_rx.recv(),
        )
        .await
        {
            if let ClusterEvent::KickUser {
                user_id, reason, ..
            } = publish.event
            {
                assert_eq!(user_id, target_user.id);
                assert_eq!(reason, "user_deleted");
                saw_kick_user = true;
            }
        }

        assert!(saw_kick_user, "delete_user must still publish KickUser");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_user_deletes_owned_rooms_and_publishes_room_deleted() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, mut redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        let admin_user = synctv_core::models::User {
            id: UserId::new(),
            username: "root_delete_owned_room".to_string(),
            email: Some("root_delete_owned_room@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target_user = synctv_core::models::User {
            id: UserId::new(),
            username: "owned_room_victim".to_string(),
            email: Some("owned_room_victim@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&admin_user).await.expect("create admin");
        user_repo
            .create(&target_user)
            .await
            .expect("create target user");

        let room = admin_api
            .room_service
            .create_room(
                "victim owned room".to_string(),
                "will be deleted with owner".to_string(),
                target_user.id.clone(),
                None,
                None,
            )
            .await
            .expect("create owned room")
            .0;

        admin_api
            .delete_user(
                crate::proto::admin::DeleteUserRequest {
                    user_id: target_user.id.as_str().to_string(),
                },
                &admin_user.id,
                &RequestContext::default(),
            )
            .await
            .expect("delete user should succeed");

        assert!(
            room_repo
                .get_by_id(&room.id)
                .await
                .expect("query room")
                .is_none(),
            "owned room must be deleted with the user"
        );

        let mut saw_room_deleted = false;
        let mut saw_kick_user = false;
        while let Ok(Some(publish)) = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            redis_publish_rx.recv(),
        )
        .await
        {
            match publish.event {
                ClusterEvent::RoomDeleted {
                    room_id,
                    deleted_by,
                    ..
                } => {
                    assert_eq!(room_id, room.id);
                    assert_eq!(deleted_by, admin_user.id);
                    saw_room_deleted = true;
                }
                ClusterEvent::KickUser {
                    user_id, reason, ..
                } => {
                    assert_eq!(user_id, target_user.id);
                    assert_eq!(reason, "user_deleted");
                    saw_kick_user = true;
                }
                _ => {}
            }
        }

        assert!(
            saw_room_deleted,
            "delete_user must publish RoomDeleted for owned rooms"
        );
        assert!(saw_kick_user, "delete_user must still publish KickUser");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ban_user_cleans_memberships_and_preserves_kick_user_event() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, mut redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let admin_user = synctv_core::models::User {
            id: UserId::new(),
            username: "root_ban_membership".to_string(),
            email: Some("root_ban_membership@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target_user = synctv_core::models::User {
            id: UserId::new(),
            username: "banned_membership".to_string(),
            email: Some("banned_membership@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&admin_user).await.expect("create admin");
        user_repo
            .create(&target_user)
            .await
            .expect("create target user");

        let owner = user_repo
            .create(&synctv_core::models::User {
                id: UserId::new(),
                username: "room_owner_ban".to_string(),
                email: Some("room_owner_ban@example.com".to_string()),
                password_hash: "hash".to_string(),
                role: UserRole::User,
                status: UserStatus::Active,
                signup_method: synctv_core::models::SignupMethod::Email,
                email_verified: true,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                deleted_at: None,
                password_changed_at: chrono::Utc::now(),
                password_version: 0,
                version: 0,
            })
            .await
            .expect("create owner");

        let room = create_room_with_member(&admin_api, &owner.id, &target_user.id).await;

        let response = admin_api
            .ban_user(
                crate::proto::admin::BanUserRequest {
                    user_id: target_user.id.as_str().to_string(),
                    reason: "policy".to_string(),
                },
                &admin_user.id,
                UserRole::Root,
                &RequestContext::default(),
            )
            .await
            .expect("ban user should succeed");

        let banned_user = response
            .user
            .expect("ban_user must return the updated user");
        assert_eq!(
            banned_user.status,
            synctv_proto::common::UserStatus::Banned as i32
        );

        let room_member = admin_api
            .room_service
            .get_member(&room.id, &target_user.id)
            .await
            .expect("room membership query should succeed");
        assert!(
            room_member.is_none(),
            "banned user must no longer appear as an active room member"
        );

        let mut saw_kick_user = false;
        while let Ok(Some(publish)) = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            redis_publish_rx.recv(),
        )
        .await
        {
            if let ClusterEvent::KickUser {
                user_id, reason, ..
            } = publish.event
            {
                assert_eq!(user_id, target_user.id);
                assert_eq!(reason, "user_banned");
                saw_kick_user = true;
            }
        }

        assert!(saw_kick_user, "ban_user must still publish KickUser");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ban_user_resets_playback_for_media_created_by_target() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let admin_user = synctv_core::models::User {
            id: UserId::new(),
            username: "root_ban_playback".to_string(),
            email: Some("root_ban_playback@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target_user = synctv_core::models::User {
            id: UserId::new(),
            username: "banned_playback_creator".to_string(),
            email: Some("banned_playback_creator@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let room_owner = synctv_core::models::User {
            id: UserId::new(),
            username: "playback_room_owner".to_string(),
            email: Some("playback_room_owner@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };

        user_repo.create(&admin_user).await.expect("create admin");
        user_repo
            .create(&target_user)
            .await
            .expect("create target user");
        user_repo
            .create(&room_owner)
            .await
            .expect("create room owner");

        let room = admin_api
            .room_service
            .create_room(
                "ban-playback-room".to_string(),
                String::new(),
                room_owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("create room")
            .0;

        let media = create_room_media(
            &pool,
            room.id.clone(),
            target_user.id.clone(),
            "banned-media",
        )
        .await;

        admin_api
            .room_service
            .playback_service()
            .switch(
                room.id.clone(),
                room_owner.id.clone(),
                Some(media.id.clone()),
                None,
                Vec::new(),
            )
            .await
            .expect("start playback");

        admin_api
            .ban_user(
                crate::proto::admin::BanUserRequest {
                    user_id: target_user.id.as_str().to_string(),
                    reason: "policy".to_string(),
                },
                &admin_user.id,
                UserRole::Root,
                &RequestContext::default(),
            )
            .await
            .expect("ban user should succeed");

        let state = admin_api
            .room_service
            .playback_service()
            .get_state(&room.id)
            .await
            .expect("load playback state after ban");

        assert!(
            state.playing_media_id.is_none(),
            "playback media must be cleared after banning its creator"
        );
        assert!(
            state.playing_playlist_id.is_none(),
            "playback playlist must be cleared after banning the media creator"
        );
        assert!(
            !state.is_playing,
            "playback must be stopped after banning the media creator"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ban_user_disconnects_owned_room_connections() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let admin_user = synctv_core::models::User {
            id: UserId::new(),
            username: "root_ban_owned_room".to_string(),
            email: Some("root_ban_owned_room@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target_user = synctv_core::models::User {
            id: UserId::new(),
            username: "banned_owned_room_creator".to_string(),
            email: Some("banned_owned_room_creator@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let member_user = synctv_core::models::User {
            id: UserId::new(),
            username: "owned_room_member".to_string(),
            email: Some("owned_room_member@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };

        user_repo.create(&admin_user).await.expect("create admin");
        user_repo
            .create(&target_user)
            .await
            .expect("create target user");
        user_repo
            .create(&member_user)
            .await
            .expect("create member user");

        let room = create_room_with_member(&admin_api, &target_user.id, &member_user.id).await;

        let mut disconnect_rx = admin_api.connection_service.subscribe_disconnect();
        admin_api
            .connection_service
            .register("owned-room-conn".to_string(), member_user.id.clone())
            .await
            .expect("register connection");
        admin_api
            .connection_service
            .join_room("owned-room-conn", room.id.clone())
            .await
            .expect("join room connection");

        admin_api
            .ban_user(
                crate::proto::admin::BanUserRequest {
                    user_id: target_user.id.as_str().to_string(),
                    reason: "policy".to_string(),
                },
                &admin_user.id,
                UserRole::Root,
                &RequestContext::default(),
            )
            .await
            .expect("ban user should succeed");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut saw_room_disconnect = false;
        while std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let signal = tokio::time::timeout(remaining, disconnect_rx.recv())
                .await
                .expect("disconnect signal must arrive before timeout")
                .expect("disconnect channel must stay open");

            if let synctv_cluster::sync::DisconnectSignal::Room(room_id) = signal {
                assert_eq!(room_id, room.id, "owned room must be disconnected");
                saw_room_disconnect = true;
                break;
            }
        }

        assert!(
            saw_room_disconnect,
            "banning a room owner must emit a room-scoped disconnect signal"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ban_user_publishes_room_owner_inactive_event_for_owned_rooms() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, mut redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let admin_user = synctv_core::models::User {
            id: UserId::new(),
            username: "root_ban_owned_room_event".to_string(),
            email: Some("root_ban_owned_room_event@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target_user = synctv_core::models::User {
            id: UserId::new(),
            username: "owned_room_event_creator".to_string(),
            email: Some("owned_room_event_creator@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };

        user_repo.create(&admin_user).await.expect("create admin");
        user_repo
            .create(&target_user)
            .await
            .expect("create target user");

        let room = admin_api
            .room_service
            .create_room(
                "owned-room-event".to_string(),
                String::new(),
                target_user.id.clone(),
                None,
                None,
            )
            .await
            .expect("create room")
            .0;

        admin_api
            .ban_user(
                crate::proto::admin::BanUserRequest {
                    user_id: target_user.id.as_str().to_string(),
                    reason: "policy".to_string(),
                },
                &admin_user.id,
                UserRole::Root,
                &RequestContext::default(),
            )
            .await
            .expect("ban user should succeed");

        let mut saw_owner_inactive = false;
        while let Ok(Some(publish)) = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            redis_publish_rx.recv(),
        )
        .await
        {
            if let ClusterEvent::RoomOwnerInactive {
                room_id,
                owner_id,
                triggered_by,
                ..
            } = publish.event
            {
                assert_eq!(room_id, room.id);
                assert_eq!(owner_id, target_user.id);
                assert_eq!(triggered_by, admin_user.id);
                saw_owner_inactive = true;
            }
        }

        assert!(
            saw_owner_inactive,
            "banning a room owner must publish RoomOwnerInactive for owned rooms"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_batch_ban_users_resets_playback_for_media_created_by_target() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let admin_user = synctv_core::models::User {
            id: UserId::new(),
            username: "root_batch_ban_playback".to_string(),
            email: Some("root_batch_ban_playback@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target_user = synctv_core::models::User {
            id: UserId::new(),
            username: "batch_banned_media_creator".to_string(),
            email: Some("batch_banned_media_creator@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let room_owner = synctv_core::models::User {
            id: UserId::new(),
            username: "batch_ban_playback_owner".to_string(),
            email: Some("batch_ban_playback_owner@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };

        user_repo.create(&admin_user).await.expect("create admin");
        user_repo
            .create(&target_user)
            .await
            .expect("create target user");
        user_repo
            .create(&room_owner)
            .await
            .expect("create room owner");

        let room = admin_api
            .room_service
            .create_room(
                "batch-ban-playback-room".to_string(),
                String::new(),
                room_owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("create room")
            .0;

        let media = create_room_media(
            &pool,
            room.id.clone(),
            target_user.id.clone(),
            "batch-banned-media",
        )
        .await;

        admin_api
            .room_service
            .playback_service()
            .switch(
                room.id.clone(),
                room_owner.id.clone(),
                Some(media.id.clone()),
                None,
                Vec::new(),
            )
            .await
            .expect("start playback");

        let response = admin_api
            .batch_ban_users(
                crate::proto::admin::BatchBanUsersRequest {
                    user_ids: vec![target_user.id.as_str().to_string()],
                    reason: "policy".to_string(),
                },
                &admin_user.id,
                UserRole::Root,
                &RequestContext::default(),
            )
            .await
            .expect("batch ban should succeed");

        assert_eq!(response.succeeded, 1);
        assert_eq!(response.failed, 0);

        let state = admin_api
            .room_service
            .playback_service()
            .get_state(&room.id)
            .await
            .expect("load playback state after batch ban");

        assert!(
            state.playing_media_id.is_none(),
            "batch ban must clear playback media created by the banned user"
        );
        assert!(
            state.playing_playlist_id.is_none(),
            "batch ban must clear playlist context created by the banned user"
        );
        assert!(
            !state.is_playing,
            "batch ban must stop playback for media created by the banned user"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_batch_ban_users_publishes_room_owner_inactive_event_for_owned_rooms() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, mut redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let admin_user = synctv_core::models::User {
            id: UserId::new(),
            username: "root_batch_ban_owned_room_event".to_string(),
            email: Some("root_batch_ban_owned_room_event@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target_user = synctv_core::models::User {
            id: UserId::new(),
            username: "batch_owned_room_creator".to_string(),
            email: Some("batch_owned_room_creator@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };

        user_repo.create(&admin_user).await.expect("create admin");
        user_repo
            .create(&target_user)
            .await
            .expect("create target user");

        let room = admin_api
            .room_service
            .create_room(
                "batch-owned-room-event".to_string(),
                String::new(),
                target_user.id.clone(),
                None,
                None,
            )
            .await
            .expect("create room")
            .0;

        let response = admin_api
            .batch_ban_users(
                crate::proto::admin::BatchBanUsersRequest {
                    user_ids: vec![target_user.id.as_str().to_string()],
                    reason: "policy".to_string(),
                },
                &admin_user.id,
                UserRole::Root,
                &RequestContext::default(),
            )
            .await
            .expect("batch ban should succeed");

        assert_eq!(response.succeeded, 1);
        assert_eq!(response.failed, 0);

        let mut saw_owner_inactive = false;
        while let Ok(Some(publish)) = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            redis_publish_rx.recv(),
        )
        .await
        {
            if let ClusterEvent::RoomOwnerInactive {
                room_id,
                owner_id,
                triggered_by,
                ..
            } = publish.event
            {
                assert_eq!(room_id, room.id);
                assert_eq!(owner_id, target_user.id);
                assert_eq!(triggered_by, admin_user.id);
                saw_owner_inactive = true;
            }
        }

        assert!(
            saw_owner_inactive,
            "batch ban must publish RoomOwnerInactive for rooms owned by banned users"
        );
    }

    #[test]
    fn test_parse_batch_user_ids_trims_and_preserves_order() {
        let parsed =
            parse_batch_user_ids(&["  AbC123xYz890  ".to_string(), "987zyxWVutSR".to_string()])
                .expect("batch ids should parse");

        let ids: Vec<_> = parsed.into_iter().map(|id| id.to_string()).collect();
        assert_eq!(ids, vec!["AbC123xYz890", "987zyxWVutSR"]);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_member_permissions_bypasses_room_creator_constraint_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_member_update".to_string(),
            email: Some("global_admin_member_update@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_member_update".to_string(),
            email: Some("room_owner_member_update@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target = synctv_core::models::User {
            id: UserId::new(),
            username: "target_member_update".to_string(),
            email: Some("target_member_update@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");
        user_repo.create(&target).await.expect("create target");

        let room = create_room_with_member(&admin_api, &owner.id, &target.id).await;

        let response = admin_api
            .update_member_permissions(
                crate::proto::admin::UpdateMemberPermissionsRequest {
                    room_id: room.id.as_str().to_string(),
                    user_id: target.id.as_str().to_string(),
                    role: synctv_proto::common::RoomMemberRole::Admin as i32,
                    added_permissions: 0,
                    removed_permissions: 0,
                    admin_added_permissions: 0b1010,
                    admin_removed_permissions: 0b0101,
                },
                &global_admin.id,
                &RequestContext::default(),
            )
            .await
            .expect("global admin should update member permissions without room creator identity");

        let member = response.member.expect("updated member response");
        assert_eq!(member.user_id, target.id.as_str());
        assert_eq!(
            member.role,
            synctv_proto::common::RoomMemberRole::Admin as i32
        );
        assert_eq!(member.admin_added_permissions, 0b1010);
        assert_eq!(member.admin_removed_permissions, 0b0101);

        let persisted = admin_api
            .room_service
            .get_member(&room.id, &target.id)
            .await
            .expect("persisted member query should succeed")
            .expect("target should remain a member");
        assert_eq!(persisted.role, RoomRole::Admin);
        assert_eq!(persisted.admin_added_permissions, 0b1010);
        assert_eq!(persisted.admin_removed_permissions, 0b0101);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_kick_member_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_member_kick".to_string(),
            email: Some("global_admin_member_kick@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_member_kick".to_string(),
            email: Some("room_owner_member_kick@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target = synctv_core::models::User {
            id: UserId::new(),
            username: "target_member_kick".to_string(),
            email: Some("target_member_kick@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");
        user_repo.create(&target).await.expect("create target");

        let room = create_room_with_member(&admin_api, &owner.id, &target.id).await;

        let response = admin_api
            .kick_member(
                crate::proto::admin::KickMemberRequest {
                    room_id: room.id.as_str().to_string(),
                    user_id: target.id.as_str().to_string(),
                },
                &global_admin.id,
                &RequestContext::default(),
            )
            .await
            .expect("global admin should kick member without being in the room");
        assert!(response.success);

        let persisted = admin_api
            .room_service
            .get_member(&room.id, &target.id)
            .await
            .expect("persisted member query should succeed");
        assert!(
            persisted.is_none(),
            "kicked member should no longer appear as an active room member"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ban_member_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());
        let member_repo = RoomMemberRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_member_ban".to_string(),
            email: Some("global_admin_member_ban@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_member_ban".to_string(),
            email: Some("room_owner_member_ban@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target = synctv_core::models::User {
            id: UserId::new(),
            username: "target_member_ban".to_string(),
            email: Some("target_member_ban@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");
        user_repo.create(&target).await.expect("create target");

        let room = create_room_with_member(&admin_api, &owner.id, &target.id).await;

        let response = admin_api
            .ban_member(
                crate::proto::admin::BanMemberRequest {
                    room_id: room.id.as_str().to_string(),
                    user_id: target.id.as_str().to_string(),
                    reason: "policy".to_string(),
                },
                &global_admin.id,
                &RequestContext::default(),
            )
            .await
            .expect("global admin should ban member without being in the room");
        assert!(response.success);

        let persisted = member_repo
            .get_any(&room.id, &target.id)
            .await
            .expect("persisted member query should succeed")
            .expect("banned member row should remain");
        assert_eq!(persisted.status, MemberStatus::Banned);
        assert_eq!(persisted.banned_reason.as_deref(), Some("policy"));
        assert_eq!(persisted.banned_by.as_ref(), Some(&global_admin.id));
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_kick_member_fails_closed_when_cluster_fanout_fails() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
        drop(redis_publish_rx);

        let mut config = Config::default();
        config.cluster.enabled = true;
        config.redis.url = "redis://127.0.0.1:6379".to_string();
        let admin_api = AdminApiImpl {
            config: Arc::new(config),
            ..admin_api
        };
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_kick_fail_closed".to_string(),
            email: Some("global_admin_kick_fail_closed@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_kick_fail_closed".to_string(),
            email: Some("room_owner_kick_fail_closed@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target = synctv_core::models::User {
            id: UserId::new(),
            username: "target_kick_fail_closed".to_string(),
            email: Some("target_kick_fail_closed@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");
        user_repo.create(&target).await.expect("create target");

        let room = create_room_with_member(&admin_api, &owner.id, &target.id).await;

        let err = admin_api
            .kick_member(
                crate::proto::admin::KickMemberRequest {
                    room_id: room.id.as_str().to_string(),
                    user_id: target.id.as_str().to_string(),
                },
                &global_admin.id,
                &RequestContext::default(),
            )
            .await
            .expect_err("cluster mode must fail closed when admin kick fanout fails");

        assert!(matches!(err, ApiError::ServiceUnavailable(_)));
        assert_eq!(
            err.message(),
            "failed to fan out KickUserFromRoom to cluster replicas"
        );

        let persisted = admin_api
            .room_service
            .get_member(&room.id, &target.id)
            .await
            .expect("persisted member query should succeed")
            .expect("target should remain a member after failed admin kick");
        assert_eq!(persisted.status, MemberStatus::Active);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_ban_member_fails_closed_when_cluster_fanout_fails() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
        drop(redis_publish_rx);

        let mut config = Config::default();
        config.cluster.enabled = true;
        config.redis.url = "redis://127.0.0.1:6379".to_string();
        let admin_api = AdminApiImpl {
            config: Arc::new(config),
            ..admin_api
        };
        let user_repo = UserRepository::new(pool.clone());
        let member_repo = RoomMemberRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_ban_fail_closed".to_string(),
            email: Some("global_admin_ban_fail_closed@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_ban_fail_closed".to_string(),
            email: Some("room_owner_ban_fail_closed@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target = synctv_core::models::User {
            id: UserId::new(),
            username: "target_ban_fail_closed".to_string(),
            email: Some("target_ban_fail_closed@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");
        user_repo.create(&target).await.expect("create target");

        let room = create_room_with_member(&admin_api, &owner.id, &target.id).await;

        let err = admin_api
            .ban_member(
                crate::proto::admin::BanMemberRequest {
                    room_id: room.id.as_str().to_string(),
                    user_id: target.id.as_str().to_string(),
                    reason: "policy".to_string(),
                },
                &global_admin.id,
                &RequestContext::default(),
            )
            .await
            .expect_err("cluster mode must fail closed when admin ban fanout fails");

        assert!(matches!(err, ApiError::ServiceUnavailable(_)));
        assert_eq!(
            err.message(),
            "failed to fan out KickUserFromRoom to cluster replicas"
        );

        let persisted = member_repo
            .get_any(&room.id, &target.id)
            .await
            .expect("persisted member query should succeed")
            .expect("target should remain an active member after failed admin ban");
        assert_eq!(persisted.status, MemberStatus::Active);
        assert!(persisted.banned_reason.is_none());
        assert!(persisted.banned_by.is_none());
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_unban_member_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_member_unban".to_string(),
            email: Some("global_admin_member_unban@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_member_unban".to_string(),
            email: Some("room_owner_member_unban@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target = synctv_core::models::User {
            id: UserId::new(),
            username: "target_member_unban".to_string(),
            email: Some("target_member_unban@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");
        user_repo.create(&target).await.expect("create target");

        let room = create_room_with_member(&admin_api, &owner.id, &target.id).await;
        admin_api
            .room_service
            .member_service()
            .ban_member(
                room.id.clone(),
                owner.id.clone(),
                target.id.clone(),
                Some("temporary".to_string()),
            )
            .await
            .expect("owner should be able to seed a banned membership");

        let response = admin_api
            .unban_member(
                crate::proto::admin::UnbanMemberRequest {
                    room_id: room.id.as_str().to_string(),
                    user_id: target.id.as_str().to_string(),
                },
                &global_admin.id,
                &RequestContext::default(),
            )
            .await
            .expect("global admin should unban member without being in the room");
        assert!(response.success);

        let persisted = admin_api
            .room_service
            .member_service()
            .get_member(&room.id, &target.id)
            .await
            .expect("persisted member query should succeed")
            .expect("unbanned member row should remain");
        assert_eq!(persisted.status, MemberStatus::Active);
        assert!(persisted.banned_reason.is_none());
        assert!(persisted.banned_by.is_none());
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ban_member_uses_nullable_banned_by_for_local_management_actor() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let member_repo = RoomMemberRepository::new(pool.clone());
        let user_repo = UserRepository::new(pool.clone());

        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_local_mgmt_ban".to_string(),
            email: Some("room_owner_local_mgmt_ban@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target = synctv_core::models::User {
            id: UserId::new(),
            username: "target_local_mgmt_ban".to_string(),
            email: Some("target_local_mgmt_ban@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&owner).await.expect("create owner");
        user_repo.create(&target).await.expect("create target");

        let room = create_room_with_member(&admin_api, &owner.id, &target.id).await;
        let management_actor = UserId::from(LOCAL_MANAGEMENT_ACTOR_USER_ID.to_string());

        let response = admin_api
            .ban_member(
                crate::proto::admin::BanMemberRequest {
                    room_id: room.id.as_str().to_string(),
                    user_id: target.id.as_str().to_string(),
                    reason: "local-management-ban".to_string(),
                },
                &management_actor,
                &RequestContext::default(),
            )
            .await
            .expect("local management actor should ban room members");
        assert!(response.success);

        let persisted = member_repo
            .get_any(&room.id, &target.id)
            .await
            .expect("persisted member query should succeed")
            .expect("banned member row should remain");
        assert_eq!(persisted.status, MemberStatus::Banned);
        assert!(
            persisted.banned_by.is_none(),
            "local management actor must not be written to banned_by foreign key"
        );
        assert_eq!(
            persisted.banned_reason.as_deref(),
            Some("local-management-ban")
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_stream_info_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, infra, _redis_publish_rx) =
            make_admin_api_with_livestream_for_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_stream_info".to_string(),
            email: Some("global_admin_stream_info@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_stream_info".to_string(),
            email: Some("room_owner_stream_info@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room stream info test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;
        let media =
            create_room_media(&pool, room.id.clone(), owner.id.clone(), "stream-media").await;

        infra
            .registry()
            .try_register_publisher(
                room.id.as_str(),
                media.id.as_str(),
                "node-a",
                owner.id.as_str(),
                "127.0.0.1:50051",
            )
            .await
            .expect("publisher should register");

        let response = admin_api
            .get_stream_info(room.id.as_str(), media.id.as_str())
            .await
            .expect("global admin should inspect stream info without room membership");
        assert!(response.active);
        let publisher = response.publisher.expect("publisher info");
        assert_eq!(publisher.user_id, owner.id.as_str());
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_room_streams_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, infra, _redis_publish_rx) =
            make_admin_api_with_livestream_for_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_stream_list".to_string(),
            email: Some("global_admin_stream_list@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_stream_list".to_string(),
            email: Some("room_owner_stream_list@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room stream list test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;
        let media =
            create_room_media(&pool, room.id.clone(), owner.id.clone(), "stream-media").await;

        infra
            .registry()
            .try_register_publisher(
                room.id.as_str(),
                media.id.as_str(),
                "node-a",
                owner.id.as_str(),
                "127.0.0.1:50051",
            )
            .await
            .expect("publisher should register");

        let response = admin_api
            .list_room_streams(
                room.id.as_str(),
                crate::proto::client::ListRoomStreamsRequest {
                    page: 1,
                    page_size: 50,
                    search: String::new(),
                    sort_by: crate::proto::client::RoomStreamListSortBy::Unspecified as i32,
                    sort_direction: crate::proto::client::SortDirection::Asc as i32,
                },
            )
            .await
            .expect("global admin should list room streams without room membership");
        assert_eq!(response.total, 1);
        assert_eq!(response.streams.len(), 1);
        assert_eq!(response.streams[0].media_id, media.id.as_str());
        assert!(response.streams[0].active);
    }

    #[test]
    fn build_room_stream_list_response_applies_search_sort_and_pagination() {
        let response = build_room_stream_list_response(
            vec![
                "beta-02".to_string(),
                "alpha-01".to_string(),
                "beta-01".to_string(),
            ],
            &crate::proto::client::ListRoomStreamsRequest {
                page: 2,
                page_size: 1,
                search: "beta".to_string(),
                sort_by: crate::proto::client::RoomStreamListSortBy::MediaId as i32,
                sort_direction: crate::proto::client::SortDirection::Desc as i32,
            },
        );

        assert_eq!(response.total, 2);
        assert_eq!(response.streams.len(), 1);
        assert_eq!(response.streams[0].media_id, "beta-01");
        assert!(response.streams[0].active);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_publish_key_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _infra, _redis_publish_rx) =
            make_admin_api_with_livestream_for_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_publish_key".to_string(),
            email: Some("global_admin_publish_key@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_publish_key".to_string(),
            email: Some("room_owner_publish_key@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room publish key test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;
        let media =
            create_room_media(&pool, room.id.clone(), owner.id.clone(), "stream-media").await;

        let response = admin_api
            .create_publish_key(
                room.id.as_str(),
                media.id.as_str(),
                &global_admin.id,
                &RequestContext::default(),
            )
            .await
            .expect("global admin should create publish key without room membership");
        assert!(!response.publish_key.is_empty());
        assert!(response.rtmp_url.contains(room.id.as_str()));
        assert!(response.stream_key.contains(media.id.as_str()));
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_start_playback_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_playback_start".to_string(),
            email: Some("global_admin_playback_start@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_playback_start".to_string(),
            email: Some("room_owner_playback_start@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room playback start test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;
        let media =
            create_room_media(&pool, room.id.clone(), owner.id.clone(), "playback-media").await;

        admin_api
            .start_playback(
                room.id.as_str(),
                crate::proto::client::StartPlaybackRequest {
                    media_id: media.id.as_str().to_string(),
                    playlist_id: String::new(),
                    target: Vec::new(),
                },
                &global_admin.id,
                &RequestContext::default(),
            )
            .await
            .expect("global admin should start playback without room membership");

        let state = admin_api
            .room_service
            .get_playback_state(&room.id)
            .await
            .expect("playback state query should succeed");
        assert_eq!(state.playing_media_id.as_ref(), Some(&media.id));
        assert!(state.is_playing);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_stop_playback_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_playback_stop".to_string(),
            email: Some("global_admin_playback_stop@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_playback_stop".to_string(),
            email: Some("room_owner_playback_stop@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room playback stop test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;
        let media =
            create_room_media(&pool, room.id.clone(), owner.id.clone(), "playback-media").await;

        admin_api
            .room_service
            .playback_service()
            .switch(
                room.id.clone(),
                owner.id.clone(),
                Some(media.id.clone()),
                None,
                Vec::new(),
            )
            .await
            .expect("owner should be able to seed playback state");

        admin_api
            .stop_playback(
                room.id.as_str(),
                &global_admin.id,
                &RequestContext::default(),
            )
            .await
            .expect("global admin should stop playback without room membership");

        let state = admin_api
            .room_service
            .get_playback_state(&room.id)
            .await
            .expect("playback state query should succeed");
        assert!(state.playing_media_id.is_none());
        assert!(!state.is_playing);
        assert!((state.current_time - 0.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_playback_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_playback_get".to_string(),
            email: Some("global_admin_playback_get@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_playback_get".to_string(),
            email: Some("room_owner_playback_get@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room playback get test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;
        let media =
            create_room_media(&pool, room.id.clone(), owner.id.clone(), "playback-media").await;

        admin_api
            .room_service
            .playback_service()
            .switch(
                room.id.clone(),
                owner.id.clone(),
                Some(media.id.clone()),
                None,
                Vec::new(),
            )
            .await
            .expect("owner should be able to seed playback state");

        let response = admin_api
            .get_playback(room.id.as_str(), &global_admin.id)
            .await
            .expect("global admin should get playback without room membership");

        let state = response
            .playback_state
            .expect("playback state should be present");
        assert!(state.is_playing);
        assert_eq!(state.playing_media_id, media.id.as_str());

        let result = response
            .playback_snapshot
            .expect("playback snapshot should be present");
        assert_eq!(result.media_id, media.id.as_str());
        assert_eq!(result.room_id, room.id.as_str());
        assert_eq!(result.name, media.name);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_playback_returns_state_when_snapshot_generation_fails_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());
        let media_repo = MediaRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_playback_state_only".to_string(),
            email: Some("global_admin_playback_state_only@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_playback_state_only".to_string(),
            email: Some("room_owner_playback_state_only@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room playback degrade test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let media = synctv_core::models::Media::from_provider(
            None,
            room.id.clone(),
            Some(owner.id.clone()),
            "Broken Playback Provider".to_string(),
            serde_json::json!({ "opaque": true }),
            "missing_provider",
            String::new(),
            0.0,
        );
        media_repo.create(&media).await.expect("create media");

        admin_api
            .room_service
            .playback_service()
            .switch(
                room.id.clone(),
                owner.id.clone(),
                Some(media.id.clone()),
                None,
                Vec::new(),
            )
            .await
            .expect("owner should seed playback state");

        let response = admin_api
            .get_playback(room.id.as_str(), &global_admin.id)
            .await
            .expect("global admin should get playback state even if snapshot generation fails");

        let state = response
            .playback_state
            .expect("playback state should be present");
        assert!(state.is_playing);
        assert_eq!(state.playing_media_id, media.id.as_str());
        assert!(
            response.playback_snapshot.is_none(),
            "admin playback queries should degrade to state-only responses on snapshot failures"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_playback_for_provider_media_signs_proxy_urls_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());
        let media_repo = MediaRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_playback_get_signed".to_string(),
            email: Some("global_admin_playback_get_signed@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_playback_get_signed".to_string(),
            email: Some("room_owner_playback_get_signed@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room provider playback get test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let media = synctv_core::models::Media::from_provider(
            None,
            room.id.clone(),
            Some(owner.id.clone()),
            "provider-playback-media".to_string(),
            serde_json::json!({
                "url": "https://example.com/video.mp4",
                "headers": {
                    "Authorization": "Bearer admin-provider-token"
                }
            }),
            "direct_url",
            String::new(),
            0.0,
        );
        let media = media_repo
            .create(&media)
            .await
            .expect("create provider media");

        admin_api
            .room_service
            .playback_service()
            .switch(
                room.id.clone(),
                owner.id.clone(),
                Some(media.id.clone()),
                None,
                Vec::new(),
            )
            .await
            .expect("owner should be able to seed playback state");

        let response = admin_api
            .get_playback(room.id.as_str(), &global_admin.id)
            .await
            .expect("global admin should get signed provider playback");

        let result = response
            .playback_snapshot
            .expect("playback snapshot should be present");
        let direct = result
            .playback_infos
            .get("direct")
            .expect("direct mode should be present");
        assert_eq!(direct.urls.len(), 1);
        assert!(
            direct.urls[0]
                .url
                .starts_with("/api/providers/proxy/direct_url/"),
            "signed provider playback should expose proxy URL, got {}",
            direct.urls[0].url
        );
        assert!(
            direct.urls[0].url.contains("/stream?"),
            "signed direct-url playback should use stream proxy contract, got {}",
            direct.urls[0].url
        );
        assert!(
            direct.urls[0].headers.is_empty(),
            "proxy-backed playback should not require client-side secret headers"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_playlists_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_list_playlists".to_string(),
            email: Some("global_admin_list_playlists@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_list_playlists".to_string(),
            email: Some("room_owner_list_playlists@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room list playlists test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let playlist = admin_api
            .room_service
            .playlist_service()
            .create_playlist(
                room.id.clone(),
                owner.id.clone(),
                synctv_core::service::playlist::CreatePlaylistRequest {
                    room_id: room.id.clone(),
                    name: "playlist-a".to_string(),
                    parent_id: None,
                    source_provider: None,
                    source_config: None,
                    provider_instance_name: None,
                },
            )
            .await
            .expect("owner should create playlist");

        let response = admin_api
            .list_playlists(
                room.id.as_str(),
                crate::proto::client::ListPlaylistsRequest {
                    parent_id: String::new(),
                    page: 1,
                    page_size: 20,
                    search: String::new(),
                    source_provider: String::new(),
                    provider_instance_name: String::new(),
                    dynamic_only: None,
                    sort_by: crate::proto::client::PlaylistListSortBy::Position as i32,
                    sort_direction: crate::proto::client::SortDirection::Asc as i32,
                    availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
                },
                &global_admin.id,
            )
            .await
            .expect("global admin should list playlists without room membership");

        assert_eq!(response.total, 1);
        assert_eq!(response.playlists.len(), 1);
        assert_eq!(response.playlists[0].id, playlist.id.as_str());
        assert_eq!(response.playlists[0].name, "playlist-a");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_playlist_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_get_playlist".to_string(),
            email: Some("global_admin_get_playlist@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_get_playlist".to_string(),
            email: Some("room_owner_get_playlist@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room get playlist test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let playlist = admin_api
            .room_service
            .playlist_service()
            .create_playlist(
                room.id.clone(),
                owner.id.clone(),
                synctv_core::service::playlist::CreatePlaylistRequest {
                    room_id: room.id.clone(),
                    name: "playlist-b".to_string(),
                    parent_id: None,
                    source_provider: None,
                    source_config: None,
                    provider_instance_name: None,
                },
            )
            .await
            .expect("owner should create playlist");

        let response = admin_api
            .get_playlist(room.id.as_str(), playlist.id.as_str(), &global_admin.id)
            .await
            .expect("global admin should get playlist without room membership");

        let response_playlist = response.playlist.expect("playlist should be returned");
        assert_eq!(response_playlist.id, playlist.id.as_str());
        assert_eq!(response_playlist.name, "playlist-b");
        assert_eq!(response.media_count, 0);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_playlist_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_create_playlist".to_string(),
            email: Some("global_admin_create_playlist@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_create_playlist".to_string(),
            email: Some("room_owner_create_playlist@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room create playlist test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let response = admin_api
            .create_playlist(
                room.id.as_str(),
                crate::proto::client::CreatePlaylistRequest {
                    name: "playlist-create".to_string(),
                    parent_id: String::new(),
                    source_provider: String::new(),
                    source_config: Vec::new(),
                    provider_instance_name: String::new(),
                },
                &global_admin.id,
            )
            .await
            .expect("global admin should create playlist without room membership");

        let playlist = response.playlist.expect("playlist should be returned");
        assert_eq!(playlist.name, "playlist-create");
        assert_eq!(playlist.room_id, room.id.as_str());
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_playlist_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_update_playlist".to_string(),
            email: Some("global_admin_update_playlist@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_update_playlist".to_string(),
            email: Some("room_owner_update_playlist@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room update playlist test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let playlist = admin_api
            .room_service
            .playlist_service()
            .create_playlist(
                room.id.clone(),
                owner.id.clone(),
                synctv_core::service::playlist::CreatePlaylistRequest {
                    room_id: room.id.clone(),
                    name: "playlist-before".to_string(),
                    parent_id: None,
                    source_provider: None,
                    source_config: None,
                    provider_instance_name: None,
                },
            )
            .await
            .expect("owner should create playlist");

        let response = admin_api
            .update_playlist(
                room.id.as_str(),
                crate::proto::client::UpdatePlaylistRequest {
                    playlist_id: playlist.id.as_str().to_string(),
                    name: "playlist-after".to_string(),
                },
                &global_admin.id,
            )
            .await
            .expect("global admin should update playlist without room membership");

        let updated = response.playlist.expect("playlist should be returned");
        assert_eq!(updated.id, playlist.id.as_str());
        assert_eq!(updated.name, "playlist-after");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_playlist_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_delete_playlist".to_string(),
            email: Some("global_admin_delete_playlist@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_delete_playlist".to_string(),
            email: Some("room_owner_delete_playlist@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room delete playlist test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let playlist = admin_api
            .room_service
            .playlist_service()
            .create_playlist(
                room.id.clone(),
                owner.id.clone(),
                synctv_core::service::playlist::CreatePlaylistRequest {
                    room_id: room.id.clone(),
                    name: "playlist-delete".to_string(),
                    parent_id: None,
                    source_provider: None,
                    source_config: None,
                    provider_instance_name: None,
                },
            )
            .await
            .expect("owner should create playlist");

        let response = admin_api
            .delete_playlist(
                room.id.as_str(),
                crate::proto::client::DeletePlaylistRequest {
                    playlist_id: playlist.id.as_str().to_string(),
                    force: true,
                },
                &global_admin.id,
            )
            .await
            .expect("global admin should delete playlist without room membership");

        assert!(response.success);
        let playlist_after = admin_api
            .room_service
            .playlist_service()
            .get_playlist(&playlist.id)
            .await
            .expect("playlist lookup should succeed");
        assert!(playlist_after.is_none());
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_playlist_publishes_cascaded_playlist_and_media_events_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, mut redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_delete_playlist_cascade".to_string(),
            email: Some("global_admin_delete_playlist_cascade@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_delete_playlist_cascade".to_string(),
            email: Some("room_owner_delete_playlist_cascade@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room delete playlist cascade test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let parent_playlist = admin_api
            .room_service
            .playlist_service()
            .create_playlist(
                room.id.clone(),
                owner.id.clone(),
                synctv_core::service::playlist::CreatePlaylistRequest {
                    room_id: room.id.clone(),
                    name: "playlist-delete-parent".to_string(),
                    parent_id: None,
                    source_provider: None,
                    source_config: None,
                    provider_instance_name: None,
                },
            )
            .await
            .expect("owner should create parent playlist");
        let child_playlist = admin_api
            .room_service
            .playlist_service()
            .create_playlist(
                room.id.clone(),
                owner.id.clone(),
                synctv_core::service::playlist::CreatePlaylistRequest {
                    room_id: room.id.clone(),
                    name: "playlist-delete-child".to_string(),
                    parent_id: Some(parent_playlist.id.clone()),
                    source_provider: None,
                    source_config: None,
                    provider_instance_name: None,
                },
            )
            .await
            .expect("owner should create child playlist");
        let nested_media = admin_api
            .room_service
            .media_service()
            .add_media(
                room.id.clone(),
                owner.id.clone(),
                synctv_core::service::media::AddMediaRequest {
                    playlist_id: Some(child_playlist.id.clone()),
                    name: "playlist-delete-cascade-media".to_string(),
                    source_provider: "direct_url".to_string(),
                    provider_instance_name: String::new(),
                    source_config: serde_json::json!({
                        "url": "https://example.com/admin-playlist-delete-cascade.mp4"
                    }),
                },
            )
            .await
            .expect("owner should create nested media");

        let response = admin_api
            .delete_playlist(
                room.id.as_str(),
                crate::proto::client::DeletePlaylistRequest {
                    playlist_id: parent_playlist.id.as_str().to_string(),
                    force: true,
                },
                &global_admin.id,
            )
            .await
            .expect("global admin should delete playlist without room membership");

        assert!(response.success);

        let mut deleted_playlist_ids = Vec::new();
        let mut deleted_media_ids = Vec::new();
        let mut kicked_media_ids = Vec::new();

        while let Ok(request) = redis_publish_rx.try_recv() {
            match request.event {
                ClusterEvent::PlaylistDeleted { playlist_id, .. } => {
                    deleted_playlist_ids.push(playlist_id.as_str().to_string());
                }
                ClusterEvent::MediaRemoved { media_id, .. } => {
                    deleted_media_ids.push(media_id.as_str().to_string());
                }
                ClusterEvent::KickPublisher { media_id, .. } => {
                    kicked_media_ids.push(media_id.as_str().to_string());
                }
                ClusterEvent::CacheInvalidate { .. } => {}
                other => panic!("unexpected admin delete_playlist cascade event: {other:?}"),
            }
        }

        deleted_playlist_ids.sort_unstable();
        deleted_media_ids.sort_unstable();
        kicked_media_ids.sort_unstable();
        let mut expected_playlist_ids = vec![
            child_playlist.id.as_str().to_string(),
            parent_playlist.id.as_str().to_string(),
        ];
        expected_playlist_ids.sort_unstable();

        assert_eq!(
            deleted_playlist_ids,
            expected_playlist_ids,
            "admin delete_playlist must publish PlaylistDeleted for every playlist removed by cascade"
        );
        assert_eq!(
            deleted_media_ids,
            vec![nested_media.id.as_str().to_string()],
            "admin delete_playlist must publish MediaRemoved for media deleted through playlist cascade"
        );
        assert_eq!(
            kicked_media_ids,
            vec![nested_media.id.as_str().to_string()],
            "admin delete_playlist must kick publishers for media deleted through playlist cascade"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_media_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_list_media".to_string(),
            email: Some("global_admin_list_media@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_list_media".to_string(),
            email: Some("room_owner_list_media@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room list media test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let media = create_room_media(&pool, room.id.clone(), owner.id.clone(), "media-a").await;

        let response = admin_api
            .list_media(
                room.id.as_str(),
                crate::proto::client::ListPlaylistItemsRequest {
                    playlist_id: String::new(),
                    target: Vec::new(),
                    page: 1,
                    page_size: 20,
                    search: String::new(),
                    source_provider: String::new(),
                    provider_instance_name: String::new(),
                    sort_by: crate::proto::client::MediaListSortBy::Position as i32,
                    sort_direction: crate::proto::client::SortDirection::Asc as i32,
                    availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
                },
                &global_admin.id,
            )
            .await
            .expect("global admin should list media without room membership");

        assert_eq!(response.media.len(), 1);
        assert_eq!(response.media[0].id, media.id.as_str());
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_reset_room_settings_bypasses_room_membership_for_local_management_actor() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_reset_room_settings".to_string(),
            email: Some("room_owner_reset_room_settings@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room settings reset test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let customized = synctv_core::models::RoomSettings {
            chat_enabled: synctv_core::models::room_settings::ChatEnabled(false),
            allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(true),
            ..synctv_core::models::RoomSettings::default()
        };
        admin_api
            .room_service
            .set_room_settings(&room.id, &customized)
            .await
            .expect("room settings should be updated");

        let management_actor = UserId::from(LOCAL_MANAGEMENT_ACTOR_USER_ID.to_string());
        let response = admin_api
            .reset_room_settings(
                crate::proto::admin::ResetRoomSettingsRequest {
                    room_id: room.id.as_str().to_string(),
                },
                &management_actor,
            )
            .await
            .expect("local management actor should reset room settings without membership");

        let response_room = response.room.expect("response should include room");
        let room_id = RoomId::from(response_room.id);
        let settings = admin_api
            .room_service
            .get_room_settings(&room_id)
            .await
            .expect("room settings should be readable");
        assert!(settings.chat_enabled.0);
        assert!(!settings.allow_guest_join.0);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_room_bypasses_room_membership_for_local_management_actor() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_delete_room".to_string(),
            email: Some("room_owner_delete_room@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room delete test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let management_actor = UserId::from(LOCAL_MANAGEMENT_ACTOR_USER_ID.to_string());
        let response = admin_api
            .delete_room(
                crate::proto::admin::DeleteRoomRequest {
                    room_id: room.id.as_str().to_string(),
                },
                &management_actor,
                &RequestContext::default(),
            )
            .await
            .expect("local management actor should delete room without membership");

        assert!(response.success);
        assert!(
            room_repo
                .get_by_id(&room.id)
                .await
                .expect("room lookup should succeed")
                .is_none(),
            "room should be deleted by local management actor"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_update_room_settings_fails_closed_when_cluster_fanout_fails() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _default_redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;

        let (redis_publish_tx, _redis_publish_rx) = tokio::sync::mpsc::channel(1);

        let mut cluster_config = Config::default();
        cluster_config.cluster.enabled = true;
        cluster_config.redis.url = "redis://127.0.0.1:6379".to_string();
        let admin_api = AdminApiImpl {
            config: Arc::new(cluster_config),
            ..admin_api
        }
        .with_cluster_fanout_service(
            crate::cluster_fanout::default_cluster_fanout_service(Some(redis_publish_tx), true),
        );
        let user_repo = UserRepository::new(pool.clone());

        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_admin_update_room_settings".to_string(),
            email: Some("room_owner_admin_update_room_settings@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room settings admin update test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let original = synctv_core::models::RoomSettings {
            chat_enabled: synctv_core::models::room_settings::ChatEnabled(false),
            allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(true),
            ..synctv_core::models::RoomSettings::default()
        };
        admin_api
            .room_service
            .set_room_settings(&room.id, &original)
            .await
            .expect("room settings should be updated");

        let updated = synctv_core::models::RoomSettings {
            chat_enabled: synctv_core::models::room_settings::ChatEnabled(true),
            allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(false),
            ..synctv_core::models::RoomSettings::default()
        };
        let management_actor = UserId::from(LOCAL_MANAGEMENT_ACTOR_USER_ID.to_string());
        let err = admin_api
            .update_room_settings(
                crate::proto::admin::UpdateRoomSettingsRequest {
                    room_id: room.id.as_str().to_string(),
                    settings: serde_json::to_vec(&updated).expect("serialize settings"),
                },
                &management_actor,
            )
            .await
            .expect_err("cluster mode must fail closed when room settings fanout fails");

        assert!(matches!(err, ApiError::ServiceUnavailable(_)));
        assert_eq!(
            err.message(),
            "failed to fan out RoomSettingsChanged to cluster replicas"
        );

        let settings = admin_api
            .room_service
            .get_room_settings(&room.id)
            .await
            .expect("room settings should still be readable");
        assert!(
            !settings.chat_enabled.0,
            "settings update must not commit before cluster fanout capacity is reserved"
        );
        assert!(
            settings.allow_guest_join.0,
            "existing customized settings must remain unchanged after failed update"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_update_room_settings_fails_closed_when_room_cache_invalidation_fanout_fails(
    ) {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
        drop(redis_publish_rx);

        let mut cluster_config = Config::default();
        cluster_config.cluster.enabled = true;
        cluster_config.redis.url = "redis://127.0.0.1:6379".to_string();
        let admin_api = AdminApiImpl {
            config: Arc::new(cluster_config),
            ..admin_api
        };
        let user_repo = UserRepository::new(pool.clone());

        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_admin_update_room_settings_cache".to_string(),
            email: Some("room_owner_admin_update_room_settings_cache@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room settings admin update cache test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let original = admin_api
            .room_service
            .get_room_settings(&room.id)
            .await
            .expect("room settings should be readable");
        let updated = synctv_core::models::RoomSettings {
            chat_enabled: synctv_core::models::room_settings::ChatEnabled(false),
            ..synctv_core::models::RoomSettings::default()
        };
        let management_actor = UserId::from(LOCAL_MANAGEMENT_ACTOR_USER_ID.to_string());
        let err = admin_api
            .update_room_settings(
                crate::proto::admin::UpdateRoomSettingsRequest {
                    room_id: room.id.as_str().to_string(),
                    settings: serde_json::to_vec(&updated).expect("serialize settings"),
                },
                &management_actor,
            )
            .await
            .expect_err("cluster mode must fail closed when room cache invalidation fanout fails");

        assert!(matches!(err, ApiError::ServiceUnavailable(_)));
        assert_eq!(
            err.message(),
            "failed to fan out room cache invalidation to cluster replicas"
        );

        let settings = admin_api
            .room_service
            .get_room_settings(&room.id)
            .await
            .expect("room settings should still be readable");
        assert_eq!(
            settings.chat_enabled.0, original.chat_enabled.0,
            "settings update must not commit before room cache invalidation capacity is reserved"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_update_room_password_fails_closed_when_room_settings_fanout_fails() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _default_redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;

        let (redis_publish_tx, _redis_publish_rx) = tokio::sync::mpsc::channel(1);

        let mut cluster_config = Config::default();
        cluster_config.cluster.enabled = true;
        cluster_config.redis.url = "redis://127.0.0.1:6379".to_string();
        let admin_api = AdminApiImpl {
            config: Arc::new(cluster_config),
            ..admin_api
        }
        .with_cluster_fanout_service(
            crate::cluster_fanout::default_cluster_fanout_service(Some(redis_publish_tx), true),
        );
        let user_repo = UserRepository::new(pool.clone());

        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_admin_update_room_password".to_string(),
            email: Some("room_owner_admin_update_room_password@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room password admin update test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let management_actor = UserId::from(LOCAL_MANAGEMENT_ACTOR_USER_ID.to_string());
        let err = admin_api
            .update_room_password(
                crate::proto::admin::UpdateRoomPasswordRequest {
                    room_id: room.id.as_str().to_string(),
                    new_password: "NewPassword123".to_string(),
                },
                &management_actor,
                &RequestContext::default(),
            )
            .await
            .expect_err("cluster mode must fail closed when room settings fanout fails");

        assert!(matches!(err, ApiError::ServiceUnavailable(_)));
        assert_eq!(
            err.message(),
            "failed to fan out RoomSettingsChanged to cluster replicas"
        );

        let settings = admin_api
            .room_service
            .get_room_settings(&room.id)
            .await
            .expect("room settings should still be readable");
        assert!(
            !settings.require_password.0,
            "room password update must not commit before room settings fanout capacity is reserved"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_update_room_password_fails_closed_when_room_cache_invalidation_fanout_fails(
    ) {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
        drop(redis_publish_rx);

        let mut cluster_config = Config::default();
        cluster_config.cluster.enabled = true;
        cluster_config.redis.url = "redis://127.0.0.1:6379".to_string();
        let admin_api = AdminApiImpl {
            config: Arc::new(cluster_config),
            ..admin_api
        };
        let user_repo = UserRepository::new(pool.clone());

        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_admin_update_room_password_cache".to_string(),
            email: Some("room_owner_admin_update_room_password_cache@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room password admin cache invalidation test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let management_actor = UserId::from(LOCAL_MANAGEMENT_ACTOR_USER_ID.to_string());
        let err = admin_api
            .update_room_password(
                crate::proto::admin::UpdateRoomPasswordRequest {
                    room_id: room.id.as_str().to_string(),
                    new_password: "NewPassword123".to_string(),
                },
                &management_actor,
                &RequestContext::default(),
            )
            .await
            .expect_err("cluster mode must fail closed when room cache invalidation fanout fails");

        assert!(matches!(err, ApiError::ServiceUnavailable(_)));
        assert_eq!(
            err.message(),
            "failed to fan out room cache invalidation to cluster replicas"
        );

        let settings = admin_api
            .room_service
            .get_room_settings(&room.id)
            .await
            .expect("room settings should still be readable");
        assert!(
            !settings.require_password.0,
            "room password update must not commit before room cache invalidation capacity is reserved"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_reset_room_settings_fails_closed_when_cluster_fanout_fails() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _default_redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;

        let (redis_publish_tx, _redis_publish_rx) = tokio::sync::mpsc::channel(1);

        let mut cluster_config = Config::default();
        cluster_config.cluster.enabled = true;
        cluster_config.redis.url = "redis://127.0.0.1:6379".to_string();
        let admin_api = AdminApiImpl {
            config: Arc::new(cluster_config),
            ..admin_api
        }
        .with_cluster_fanout_service(
            crate::cluster_fanout::default_cluster_fanout_service(Some(redis_publish_tx), true),
        );
        let user_repo = UserRepository::new(pool.clone());

        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_admin_reset_room_settings".to_string(),
            email: Some("room_owner_admin_reset_room_settings@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room settings admin reset test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let customized = synctv_core::models::RoomSettings {
            chat_enabled: synctv_core::models::room_settings::ChatEnabled(false),
            allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(true),
            ..synctv_core::models::RoomSettings::default()
        };
        admin_api
            .room_service
            .set_room_settings(&room.id, &customized)
            .await
            .expect("room settings should be updated");

        let management_actor = UserId::from(LOCAL_MANAGEMENT_ACTOR_USER_ID.to_string());
        let err = admin_api
            .reset_room_settings(
                crate::proto::admin::ResetRoomSettingsRequest {
                    room_id: room.id.as_str().to_string(),
                },
                &management_actor,
            )
            .await
            .expect_err("cluster mode must fail closed when room settings fanout fails");

        assert!(matches!(err, ApiError::ServiceUnavailable(_)));
        assert_eq!(
            err.message(),
            "failed to fan out RoomSettingsChanged to cluster replicas"
        );

        let settings = admin_api
            .room_service
            .get_room_settings(&room.id)
            .await
            .expect("room settings should still be readable");
        assert!(
            !settings.chat_enabled.0,
            "settings reset must not commit before cluster fanout capacity is reserved"
        );
        assert!(
            settings.allow_guest_join.0,
            "customized settings must remain unchanged after failed reset"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_reset_room_settings_fails_closed_when_room_cache_invalidation_fanout_fails()
    {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
        drop(redis_publish_rx);

        let mut cluster_config = Config::default();
        cluster_config.cluster.enabled = true;
        cluster_config.redis.url = "redis://127.0.0.1:6379".to_string();
        let admin_api = AdminApiImpl {
            config: Arc::new(cluster_config),
            ..admin_api
        };
        let user_repo = UserRepository::new(pool.clone());

        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_admin_reset_room_settings_cache".to_string(),
            email: Some("room_owner_admin_reset_room_settings_cache@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room settings admin reset cache test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let customized = synctv_core::models::RoomSettings {
            chat_enabled: synctv_core::models::room_settings::ChatEnabled(false),
            allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(true),
            ..synctv_core::models::RoomSettings::default()
        };
        admin_api
            .room_service
            .set_room_settings(&room.id, &customized)
            .await
            .expect("room settings should be updated");

        let management_actor = UserId::from(LOCAL_MANAGEMENT_ACTOR_USER_ID.to_string());
        let err = admin_api
            .reset_room_settings(
                crate::proto::admin::ResetRoomSettingsRequest {
                    room_id: room.id.as_str().to_string(),
                },
                &management_actor,
            )
            .await
            .expect_err("cluster mode must fail closed when room cache invalidation fanout fails");

        assert!(matches!(err, ApiError::ServiceUnavailable(_)));
        assert_eq!(
            err.message(),
            "failed to fan out room cache invalidation to cluster replicas"
        );

        let settings = admin_api
            .room_service
            .get_room_settings(&room.id)
            .await
            .expect("room settings should still be readable");
        assert!(
            !settings.chat_enabled.0,
            "reset must not commit before room cache invalidation capacity is reserved"
        );
        assert!(
            settings.allow_guest_join.0,
            "customized settings must remain unchanged after failed reset"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_create_room_fails_closed_when_cluster_fanout_fails() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
        drop(redis_publish_rx);

        let mut cluster_config = Config::default();
        cluster_config.cluster.enabled = true;
        cluster_config.redis.url = "redis://127.0.0.1:6379".to_string();
        let admin_api = AdminApiImpl {
            config: Arc::new(cluster_config),
            ..admin_api
        };

        let management_actor = UserId::from(LOCAL_MANAGEMENT_ACTOR_USER_ID.to_string());
        let err = admin_api
            .create_room(
                crate::proto::client::CreateRoomRequest {
                    name: "Admin Fanout Room".to_string(),
                    description: "admin room creation fail-closed regression".to_string(),
                    password: String::new(),
                    settings: Vec::new(),
                },
                &management_actor,
            )
            .await
            .expect_err("cluster mode must fail closed when admin room fanout fails");

        assert!(matches!(err, ApiError::ServiceUnavailable(_)));
        assert_eq!(
            err.message(),
            "failed to fan out RoomCreated to cluster replicas"
        );
        assert_eq!(
            admin_api
                .room_service
                .list_accessible_rooms(&synctv_core::models::RoomListQuery::default())
                .await
                .expect("rooms should remain queryable")
                .1,
            0,
            "admin room creation must not commit before cluster fanout capacity is reserved"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_delete_room_fails_closed_when_cluster_fanout_fails() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
        drop(redis_publish_rx);

        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "delete_room_fail_closed_owner".to_string(),
            email: Some("delete_room_fail_closed_owner@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&owner).await.expect("create owner");
        let room = admin_api
            .room_service
            .create_room(
                "admin-delete-room-fail-closed".to_string(),
                String::new(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("create room")
            .0;

        let management_actor = UserId::from(LOCAL_MANAGEMENT_ACTOR_USER_ID.to_string());
        let err = admin_api
            .delete_room(
                crate::proto::admin::DeleteRoomRequest {
                    room_id: room.id.as_str().to_string(),
                },
                &management_actor,
                &RequestContext::default(),
            )
            .await
            .expect_err("delete room must fail closed when room fanout reservation fails");

        assert!(matches!(err, ApiError::ServiceUnavailable(_)));
        assert_eq!(
            err.message(),
            "failed to fan out RoomDeleted to cluster replicas"
        );
        assert!(
            room_repo
                .get_by_id(&room.id)
                .await
                .expect("query room")
                .is_some(),
            "room deletion must not commit before cluster fanout capacity is reserved"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_admin_ban_room_fails_closed_when_cluster_fanout_fails() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
        drop(redis_publish_rx);

        let user_repo = UserRepository::new(pool.clone());
        let admin_user = synctv_core::models::User {
            id: UserId::new(),
            username: "ban_room_fail_closed_admin".to_string(),
            email: Some("ban_room_fail_closed_admin@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&admin_user).await.expect("create admin");
        let room = admin_api
            .room_service
            .create_room(
                "admin-ban-room-fail-closed".to_string(),
                String::new(),
                admin_user.id.clone(),
                None,
                None,
            )
            .await
            .expect("create room")
            .0;

        let err = admin_api
            .ban_room(
                crate::proto::admin::BanRoomRequest {
                    room_id: room.id.as_str().to_string(),
                    reason: "policy".to_string(),
                },
                &admin_user.id,
                &RequestContext::default(),
            )
            .await
            .expect_err("ban room must fail closed when room fanout reservation fails");

        assert!(matches!(err, ApiError::ServiceUnavailable(_)));
        assert_eq!(
            err.message(),
            "failed to fan out RoomBanned to cluster replicas"
        );
        assert!(
            !admin_api
                .room_service
                .get_room(&room.id)
                .await
                .expect("load room after failed ban")
                .is_banned,
            "room ban must not commit before cluster fanout capacity is reserved"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_user_fails_closed_when_owned_room_room_deleted_fanout_fails() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
        drop(redis_publish_rx);

        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let admin_user = synctv_core::models::User {
            id: UserId::new(),
            username: "delete_user_fail_closed_admin".to_string(),
            email: Some("delete_user_fail_closed_admin@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target_user = synctv_core::models::User {
            id: UserId::new(),
            username: "delete_user_fail_closed_target".to_string(),
            email: Some("delete_user_fail_closed_target@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&admin_user).await.expect("create admin");
        user_repo
            .create(&target_user)
            .await
            .expect("create target user");
        let room = admin_api
            .room_service
            .create_room(
                "delete-user-owned-room-fail-closed".to_string(),
                String::new(),
                target_user.id.clone(),
                None,
                None,
            )
            .await
            .expect("create room")
            .0;

        let err = admin_api
            .delete_user(
                crate::proto::admin::DeleteUserRequest {
                    user_id: target_user.id.as_str().to_string(),
                },
                &admin_user.id,
                &RequestContext::default(),
            )
            .await
            .expect_err("delete user must fail closed when owned-room deletion fanout fails");

        assert!(matches!(err, ApiError::ServiceUnavailable(_)));
        assert_eq!(
            err.message(),
            "failed to fan out RoomDeleted to cluster replicas"
        );
        assert!(
            user_repo
                .get_by_id(&target_user.id)
                .await
                .expect("query target user")
                .is_some(),
            "user deletion must not commit before owned room fanout capacity is reserved"
        );
        assert!(
            room_repo
                .get_by_id(&room.id)
                .await
                .expect("query owned room")
                .is_some(),
            "owned room must still exist after fail-closed delete_user"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ban_user_fails_closed_when_room_owner_inactive_fanout_fails() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, redis_publish_rx) = make_admin_api_for_delete_user_test(pool.clone()).await;
        drop(redis_publish_rx);

        let user_repo = UserRepository::new(pool.clone());
        let admin_user = synctv_core::models::User {
            id: UserId::new(),
            username: "ban_user_fail_closed_admin".to_string(),
            email: Some("ban_user_fail_closed_admin@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let target_user = synctv_core::models::User {
            id: UserId::new(),
            username: "ban_user_fail_closed_target".to_string(),
            email: Some("ban_user_fail_closed_target@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&admin_user).await.expect("create admin");
        user_repo
            .create(&target_user)
            .await
            .expect("create target user");
        let room = admin_api
            .room_service
            .create_room(
                "ban-user-owned-room-fail-closed".to_string(),
                String::new(),
                target_user.id.clone(),
                None,
                None,
            )
            .await
            .expect("create room")
            .0;

        let err = admin_api
            .ban_user(
                crate::proto::admin::BanUserRequest {
                    user_id: target_user.id.as_str().to_string(),
                    reason: "policy".to_string(),
                },
                &admin_user.id,
                UserRole::Root,
                &RequestContext::default(),
            )
            .await
            .expect_err("ban user must fail closed when RoomOwnerInactive fanout fails");

        assert!(matches!(err, ApiError::ServiceUnavailable(_)));
        assert_eq!(
            err.message(),
            "failed to fan out RoomOwnerInactive to cluster replicas"
        );
        assert_eq!(
            user_repo
                .get_by_id(&target_user.id)
                .await
                .expect("load target user")
                .expect("target user should still exist")
                .status,
            UserStatus::Active,
            "user ban must not commit before room-owner-inactive fanout capacity is reserved"
        );
        assert!(
            !admin_api
                .room_service
                .get_room(&room.id)
                .await
                .expect("load room after failed user ban")
                .is_banned,
            "owned room must remain active after fail-closed user ban"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_media_respects_search_filters_and_sort_for_static_root() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_list_media_filters".to_string(),
            email: Some("global_admin_list_media_filters@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_list_media_filters".to_string(),
            email: Some("room_owner_list_media_filters@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room list media filter test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        admin_api
            .room_service
            .playlist_service()
            .create_playlist(
                room.id.clone(),
                owner.id.clone(),
                synctv_core::service::playlist::CreatePlaylistRequest {
                    room_id: room.id.clone(),
                    name: "Alpha Folder".to_string(),
                    parent_id: None,
                    source_provider: None,
                    source_config: None,
                    provider_instance_name: None,
                },
            )
            .await
            .expect("playlist should be created");

        create_room_media(&pool, room.id.clone(), owner.id.clone(), "Alpha Media").await;
        create_room_media(&pool, room.id.clone(), owner.id.clone(), "Beta Media").await;

        let response = admin_api
            .list_media(
                room.id.as_str(),
                crate::proto::client::ListPlaylistItemsRequest {
                    playlist_id: String::new(),
                    target: Vec::new(),
                    page: 1,
                    page_size: 10,
                    search: "alpha".to_string(),
                    source_provider: "direct_url".to_string(),
                    provider_instance_name: "direct_url".to_string(),
                    sort_by: crate::proto::client::MediaListSortBy::Name as i32,
                    sort_direction: crate::proto::client::SortDirection::Asc as i32,
                    availability: crate::proto::client::ResourceAvailabilityFilter::All as i32,
                },
                &global_admin.id,
            )
            .await
            .expect("list media should succeed");

        assert_eq!(response.total, 1);
        assert_eq!(response.folder_count, 0);
        assert_eq!(response.file_count, 1);
        assert!(response.playlists.is_empty());
        assert_eq!(response.media.len(), 1);
        assert_eq!(response.media[0].title, "Alpha Media");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_add_media_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, mut redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_add_media".to_string(),
            email: Some("global_admin_add_media@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_add_media".to_string(),
            email: Some("room_owner_add_media@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room add media test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let response = admin_api
            .add_media(
                room.id.as_str(),
                crate::proto::client::AddMediaRequest {
                    playlist_id: None,
                    provider: "direct_url".to_string(),
                    provider_instance_name: String::new(),
                    source_config: serde_json::to_vec(&serde_json::json!({
                        "url": "https://example.com/added.mp4"
                    }))
                    .expect("encode source config"),
                    title: "added-media".to_string(),
                },
                &global_admin.id,
            )
            .await
            .expect("global admin should add media without room membership");

        let media = response.media.expect("media should be returned");
        assert_eq!(media.title, "added-media");
        assert_eq!(media.room_id, room.id.as_str());

        let mut saw_media_added = false;
        while let Ok(request) = redis_publish_rx.try_recv() {
            match request.event {
                ClusterEvent::MediaAdded {
                    media_id,
                    media_title,
                    ..
                } => {
                    assert_eq!(media_id.as_str(), media.id);
                    assert_eq!(media_title, "added-media");
                    saw_media_added = true;
                }
                ClusterEvent::CacheInvalidate { .. } => {}
                other => panic!("unexpected admin add_media cluster event: {other:?}"),
            }
        }
        assert!(
            saw_media_added,
            "admin add_media must publish MediaAdded to cluster subscribers"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_edit_media_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, mut redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_edit_media".to_string(),
            email: Some("global_admin_edit_media@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_edit_media".to_string(),
            email: Some("room_owner_edit_media@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room edit media test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let media = create_room_media(&pool, room.id.clone(), owner.id.clone(), "media-edit").await;

        let response = admin_api
            .edit_media(
                room.id.as_str(),
                crate::proto::client::EditMediaRequest {
                    media_id: media.id.as_str().to_string(),
                    title: "media-edited".to_string(),
                },
                &global_admin.id,
            )
            .await
            .expect("global admin should edit media without room membership");

        let updated = response.media.expect("media should be returned");
        assert_eq!(updated.id, media.id.as_str());
        assert_eq!(updated.title, "media-edited");

        let mut saw_media_updated = false;
        while let Ok(request) = redis_publish_rx.try_recv() {
            match request.event {
                ClusterEvent::MediaUpdated {
                    media_id,
                    media_title,
                    ..
                } => {
                    assert_eq!(media_id.as_str(), media.id.as_str());
                    assert_eq!(media_title, "media-edited");
                    saw_media_updated = true;
                }
                ClusterEvent::CacheInvalidate { .. } => {}
                other => panic!("unexpected admin edit_media cluster event: {other:?}"),
            }
        }
        assert!(
            saw_media_updated,
            "admin edit_media must publish MediaUpdated to cluster subscribers"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_local_management_actor_preserves_username_in_media_notifications() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_management_media_notifications".to_string(),
            email: Some("room_owner_management_media_notifications@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room management media notification test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let media =
            create_room_media(&pool, room.id.clone(), owner.id.clone(), "management-media").await;
        let management_actor = UserId::from(LOCAL_MANAGEMENT_ACTOR_USER_ID.to_string());
        let mut notification_rx = admin_api.room_service.notification_service().subscribe();

        admin_api
            .edit_media(
                room.id.as_str(),
                crate::proto::client::EditMediaRequest {
                    media_id: media.id.as_str().to_string(),
                    title: "management-media-updated".to_string(),
                },
                &management_actor,
            )
            .await
            .expect("local management actor should edit media");

        let updated_event = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let (_, event) = notification_rx
                    .recv()
                    .await
                    .expect("notification should arrive");
                match event {
                    synctv_core::service::notification::RoomEvent::MediaUpdated {
                        media_id,
                        username,
                        ..
                    } if media_id == media.id.as_str() => break username,
                    _ => {}
                }
            }
        })
        .await
        .expect("media updated notification should arrive");
        assert_eq!(updated_event, "local-management");

        admin_api
            .delete_media(
                room.id.as_str(),
                crate::proto::client::DeleteMediaRequest {
                    media_id: media.id.as_str().to_string(),
                    force: false,
                },
                &management_actor,
            )
            .await
            .expect("local management actor should delete media");

        let removed_event = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let (_, event) = notification_rx
                    .recv()
                    .await
                    .expect("notification should arrive");
                match event {
                    synctv_core::service::notification::RoomEvent::MediaRemoved {
                        media_id,
                        username,
                        user_id,
                    } if media_id == media.id.as_str() => break (username, user_id),
                    _ => {}
                }
            }
        })
        .await
        .expect("media removed notification should arrive");
        assert_eq!(removed_event.0, "local-management");
        assert_eq!(removed_event.1, Some(management_actor));
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_media_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_delete_media".to_string(),
            email: Some("global_admin_delete_media@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_delete_media".to_string(),
            email: Some("room_owner_delete_media@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room delete media test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let media =
            create_room_media(&pool, room.id.clone(), owner.id.clone(), "media-delete").await;

        let response = admin_api
            .delete_media(
                room.id.as_str(),
                crate::proto::client::DeleteMediaRequest {
                    media_id: media.id.as_str().to_string(),
                    force: true,
                },
                &global_admin.id,
            )
            .await
            .expect("global admin should delete media without room membership");

        assert!(response.success);
        let media_after = admin_api
            .room_service
            .media_service()
            .get_media(&media.id)
            .await
            .expect("media lookup should succeed");
        assert!(media_after.is_none());
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_move_media_bypasses_room_membership_requirement_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_move_media".to_string(),
            email: Some("global_admin_move_media@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        let owner = synctv_core::models::User {
            id: UserId::new(),
            username: "room_owner_move_media".to_string(),
            email: Some("room_owner_move_media@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");
        user_repo.create(&owner).await.expect("create owner");

        let room = admin_api
            .room_service
            .create_room(
                format!("room-{}", synctv_common::snanoid!(6)),
                "room move media test".to_string(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;

        let media_a =
            create_room_media(&pool, room.id.clone(), owner.id.clone(), "media-move-a").await;
        let media_b =
            create_room_media(&pool, room.id.clone(), owner.id.clone(), "media-move-b").await;

        admin_api
            .move_media(
                room.id.as_str(),
                crate::proto::client::MoveMediaRequest {
                    media_ids: vec![media_b.id.as_str().to_string()],
                    source_playlist_id: None,
                    target_playlist_id: None,
                    all_from_scope: false,
                    before_media_id: Some(media_a.id.as_str().to_string()),
                    after_media_id: None,
                },
                &global_admin.id,
            )
            .await
            .expect("global admin should move media without room membership");

        let media_a_after = admin_api
            .room_service
            .media_service()
            .get_media(&media_a.id)
            .await
            .expect("media lookup should succeed")
            .expect("media_a should exist");
        let media_b_after = admin_api
            .room_service
            .media_service()
            .get_media(&media_b.id)
            .await
            .expect("media lookup should succeed")
            .expect("media_b should exist");
        assert!(media_b_after.position < media_a_after.position);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_room_bypasses_user_room_creation_policy_for_global_admin() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, _redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());

        let global_admin = synctv_core::models::User {
            id: UserId::new(),
            username: "global_admin_create_room".to_string(),
            email: Some("global_admin_create_room@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo
            .create(&global_admin)
            .await
            .expect("create global admin");

        let registry = admin_api
            .room_service
            .settings_registry()
            .expect("room service should expose settings registry for tests");
        registry
            .allow_room_creation
            .set(false)
            .await
            .expect("disable user room creation");

        let response = admin_api
            .create_room(
                crate::proto::client::CreateRoomRequest {
                    name: format!("admin-room-{}", synctv_common::snanoid!(6)),
                    password: String::new(),
                    settings: Vec::new(),
                    description: "management create room".to_string(),
                },
                &global_admin.id,
            )
            .await
            .expect("global admin should create room even when user room creation is disabled");

        let room = response.room.expect("created room");
        assert_eq!(room.description, "management create room");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_ban_room_publishes_room_banned_cluster_event() {
        let (_postgres, pool) = create_test_pool().await;
        let (admin_api, mut redis_publish_rx) =
            make_admin_api_for_delete_user_test(pool.clone()).await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        let admin_user = synctv_core::models::User {
            id: UserId::new(),
            username: "room_admin".to_string(),
            email: Some("room_admin@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: UserRole::Root,
            status: UserStatus::Active,
            signup_method: synctv_core::models::SignupMethod::Email,
            email_verified: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            password_changed_at: chrono::Utc::now(),
            password_version: 0,
            version: 0,
        };
        user_repo.create(&admin_user).await.expect("create admin");

        let room = make_test_room_model(&admin_user.id);
        room_repo.create(&room).await.expect("create room");

        admin_api
            .ban_room(
                crate::proto::admin::BanRoomRequest {
                    room_id: room.id.as_str().to_string(),
                    reason: "moderation".to_string(),
                },
                &admin_user.id,
                &RequestContext::default(),
            )
            .await
            .expect("ban room should succeed");

        let publish =
            tokio::time::timeout(std::time::Duration::from_secs(1), redis_publish_rx.recv())
                .await
                .expect("expected cluster publish")
                .expect("publish request");

        match publish.event {
            ClusterEvent::RoomBanned {
                room_id, banned_by, ..
            } => {
                assert_eq!(room_id, room.id);
                assert_eq!(banned_by, admin_user.id);
            }
            other => panic!("expected RoomBanned event, got {other:?}"),
        }
    }
}
