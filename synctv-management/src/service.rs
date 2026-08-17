use std::pin::Pin;
use std::sync::Arc;

use futures::{stream, Stream, StreamExt as _, TryStreamExt as _};
use tonic::{Request, Response, Status};

const MANAGEMENT_ROOM_LOAD_CONCURRENCY: usize = 16;
const MANAGEMENT_USER_RESOLUTION_CONCURRENCY: usize = 16;

use crate::access::ManagementAccessController;
use crate::admin_runtime::{
    AddAdminCommand, AddMemberCommand, AdminRuntime, AdminSortDirection,
    ApproveRoomCreationReviewCommand, ApproveRoomJoinReviewCommand,
    ApproveUserRegistrationReviewCommand, BanRoomCommand, BanUserCommand, BatchBanRoomsCommand,
    BatchBanUsersCommand, BatchDeleteRoomsCommand, BatchDeleteUsersCommand, CreateUserCommand,
    DeleteMediaCommand, DeletePlaylistCommand, DeleteRoomCategoryCommand, DeleteRoomCommand,
    DeleteRoomLabelCommand, DeleteUserCommand, EditMediaCommand, ExportSettingsQuery,
    GetRoomMembersQuery, GetRoomQuery, GetRoomSettingsQuery, GetServiceStateQuery,
    GetSettingsQuery, GetUserPreferencesQuery, GetUserQuery, GetUserRoomsQuery,
    ImportSettingsCommand, KickMemberCommand, KickStreamCommand, ListActiveStreamsQuery,
    ListAdminsQuery, ListBanRecordsQuery, ListMediaQuery, ListPlaylistsQuery,
    ListRoomCategoriesQuery, ListRoomCreationReviewsQuery, ListRoomJoinReviewsQuery,
    ListRoomLabelsQuery, ListRoomStreamsQuery, ListRoomsQuery, ListUserRegistrationReviewsQuery,
    ListUsersQuery, MoveMediaCommand, MovePlaylistCommand, RejectRoomCreationReviewCommand,
    RejectRoomJoinReviewCommand, RejectUserRegistrationReviewCommand, RemoveAdminCommand,
    ResetRoomSettingsCommand, RestoreUserCommand, SendTestEmailCommand, SetUserPasswordCommand,
    StartPlaybackCommand, UnbanRoomCommand, UnbanUserCommand, UpdateMemberDisplayTagCommand,
    UpdateMemberPermissionsCommand, UpdateMemberRemarkNameCommand, UpdatePlaybackStateCommand,
    UpdatePlaylistCommand, UpdateRoomPasswordCommand, UpdateRoomSettingsCommand,
    UpdateRoomTaxonomyCommand, UpdateSettingsCommand, UpdateUserPreferencesCommand,
    UpdateUserRoleCommand, UpdateUserUsernameCommand, UpsertRoomCategoryCommand,
    UpsertRoomLabelCommand,
};
use crate::lifecycle::{LifecycleEvent, ManagementLifecycleController, ShutdownMode};
use crate::mapping::{
    chat_history_cursor_to_client_proto, created_media_to_client_proto,
    created_playlist_to_client_proto, created_room_to_client_proto,
    evict_expired_slice_cache_to_management, get_slice_cache_stats_to_management, map_api_error,
    map_api_result, map_ban_record_target_type_filter, map_classified_result, map_core_error,
    map_management_core_sort_direction, map_management_room_list_sort_by,
    map_management_sort_direction, map_management_user_list_sort_by,
    map_management_user_lookup_error, map_optional_management_sort_direction,
    map_provider_instance_list_sort_by, map_provider_instance_sort_direction,
    map_required_user_role, map_required_user_status, map_review_status_filter,
    map_room_member_list_sort_by, map_room_status_filter, map_room_stream_list_sort_by,
    map_server_state_error, map_slice_cache_error, map_user_role_filter, map_user_status_filter,
    optional_playlist_id_from_public, optional_room_category_id_from_public,
    purge_slice_cache_to_management, room_id_from_public, room_label_ids_from_public,
    room_settings_from_client_proto, search_chat_messages_query_from_client_proto,
    server_state_to_management, slice_cache_selection, source_provider_from_proto_filter,
    user_id_from_public, user_notification_preferences_from_client_proto,
    validate_client_actor_user,
};
use crate::proto::{
    list_media_request, management_service_server::ManagementService, AddAdminRequest,
    AddAlistMediaRequest, AddBilibiliLiveMediaRequest, AddBilibiliPgcMediaRequest,
    AddBilibiliVideoMediaRequest, AddDirectUrlMediaRequest, AddEmbyMediaRequest, AddMediaRequest,
    AddMemberRequest, AlistGetBindsRequest, AlistGetMeRequest, AlistListRequest, AlistLoginRequest,
    AlistLogoutRequest, AlistSearchRequest, ApproveRoomCreationReviewRequest,
    ApproveRoomJoinReviewRequest, ApproveUserRegistrationReviewRequest, BanRoomRequest,
    BanUserRequest, BatchBanRoomsRequest, BatchBanUsersRequest, BatchDeleteRoomsRequest,
    BatchDeleteUsersRequest, BilibiliCheckQrRequest, BilibiliGetBindsRequest,
    BilibiliGetUserInfoRequest, BilibiliLoginQrRequest, BilibiliLoginSmsRequest,
    BilibiliLogoutRequest, BilibiliParseRequest, BilibiliSendSmsRequest,
    BilibiliStartSmsLoginRequest, CreateAlistPlaylistRequest, CreateEmbyPlaylistRequest,
    CreatePlaylistRequest, CreatePublishKeyRequest, CreateRoomRequest, CreateUserRequest,
    DeleteMediaRequest, DeletePlaylistRequest, DeleteRoomRequest, DeleteUserRequest,
    DouyinBindRequest, DouyinGetBindsRequest, DouyinListUserPostsRequest, DouyinResolveRequest,
    DouyinUnbindRequest, EditMediaRequest, EmbyGetBindsRequest, EmbyGetMeRequest, EmbyListRequest,
    EmbyLoginRequest, EmbyLogoutRequest, EvictExpiredSliceCacheRequest, FavoriteRoomRequest,
    GetPlaybackRequest, GetPlaylistRequest, GetRoomMembersRequest, GetRoomRequest,
    GetRoomSettingsRequest, GetServerStateRequest, GetServerStateResponse, GetServiceStateRequest,
    GetSettingsRequest, GetSliceCacheStatsRequest, GetStreamInfoRequest, GetUserPreferencesRequest,
    GetUserRequest, GetUserRoomsRequest, KickMemberRequest, KickRoomStreamRequest,
    KickStreamRequest, ListActiveStreamsRequest, ListAdminsRequest, ListBanRecordsRequest,
    ListFavoriteRoomsRequest, ListMediaRequest, ListPlaylistsRequest,
    ListRoomCreationReviewsRequest, ListRoomJoinReviewsRequest, ListRoomStreamsRequest,
    ListRoomsRequest, ListUserRegistrationReviewsRequest, ListUsersRequest, MoveMediaRequest,
    MovePlaylistRequest, PurgeSliceCacheRequest, RejectRoomCreationReviewRequest,
    RejectRoomJoinReviewRequest, RejectUserRegistrationReviewRequest, RemoveAdminRequest,
    ResetRoomSettingsRequest, RestoreUserRequest, SearchChatMessagesRequest, SendTestEmailRequest,
    SetUserPasswordRequest, ShutdownMode as ProtoShutdownMode, StartPlaybackRequest,
    StopPlaybackRequest, StopServerEvent, StopServerRequest, TransferRoomOwnershipRequest,
    UnbanRoomRequest, UnbanUserRequest, UnfavoriteRoomRequest, UpdateMemberDisplayTagRequest,
    UpdateMemberPermissionsRequest, UpdateMemberRemarkNameRequest, UpdatePlaybackStateRequest,
    UpdatePlaylistRequest, UpdateRoomPasswordRequest, UpdateUserPreferencesRequest,
    UpdateUserRoleRequest, UpdateUserUsernameRequest, UserRef,
};
use crate::proto::{
    TikTokBindRequest, TikTokGetBindsRequest, TikTokGetUserRequest, TikTokListUserPostsRequest,
    TikTokResolveRequest, TikTokUnbindRequest, TwitchBindRequest, TwitchGetBindsRequest,
    TwitchListChannelItemsRequest, TwitchResolveRequest, TwitchUnbindRequest,
};
use crate::provider_runtime::{
    AcfunRuntime, AddProviderInstanceCommand, AlistListQuery, AlistLoginCommand,
    AlistLoginCredential, AlistRuntime, AlistSearchQuery, BilibiliCheckQrQuery,
    BilibiliLoginQrCommand, BilibiliLoginSmsCommand, BilibiliLogoutCommand, BilibiliParseQuery,
    BilibiliRuntime, BilibiliSendSmsCommand, BilibiliStartSmsLoginCommand, BilibiliUserInfoQuery,
    CctvRuntime, CloudreveRuntime, DouyinRuntime, DouyuRuntime, EmbyListQuery, EmbyLoginCommand,
    EmbyLoginCredential, EmbyRuntime, FnosRuntime, HuyaRuntime,
    ListAvailableProviderInstancesQuery, ListProviderBackendsQuery, NextcloudRuntime,
    ProviderCommonRuntime, ProviderCredentialServerQuery, ProviderInstanceNameCommand, QnapRuntime,
    SeafileRuntime, SynologyRuntime, TikTokRuntime, TruenasRuntime, TwitchRuntime,
    UpdateProviderInstanceCommand, YoutubeRuntime,
};
use crate::request_context::RequestContext;
use crate::server::ManagementRuntimeSettings;
use crate::source_config::{
    alist_media_source_config, alist_playlist_source_config, bilibili_live_source_config,
    bilibili_pgc_source_config, bilibili_video_source_config, direct_url_source_config,
    emby_media_source_config, emby_playlist_source_config,
};
use synctv_adapter::PublicIdCodec;
use synctv_core::models::{
    ChatMessageWithAttachments, Media, PageParams, Playlist, ProviderInstanceListQuery,
    RealtimeEvent, Room, RoomPermission, UserId, UserRole as CoreUserRole,
    LOCAL_MANAGEMENT_ACTOR_USER_ID,
};
use synctv_core::service::{ChatService, RoomService, UserService};
use synctv_proto::{
    admin as admin_proto, client as client_proto, common as common_proto,
    providers::{
        alist as alist_proto, bilibili as bilibili_proto, common as provider_common_proto,
        douyin as douyin_proto, emby as emby_proto, tiktok as tiktok_proto, twitch as twitch_proto,
    },
};
use synctv_realtime::fanout::{
    MembershipEventFanoutService, PreparedOutboxFanout, RealtimeFanoutService,
    RoomCacheFanoutService,
};

mod provider_rpc;

struct ValidatedManagementUser {
    user_id: UserId,
    role: CoreUserRole,
}

struct BatchUserResolution {
    user_ids: Vec<String>,
    failures: Vec<admin_proto::BatchResultItem>,
}

fn defaultable_page_i32_to_u32(value: i32) -> Option<u32> {
    (value > 0).then_some(value.cast_unsigned())
}

fn defaultable_page_size_i32_to_u32(value: i32, max: i32) -> Option<u32> {
    (value > 0).then_some(value.clamp(1, max).cast_unsigned())
}

#[derive(Clone)]
pub struct ManagementServiceImpl {
    settings: Arc<ManagementRuntimeSettings>,
    user_service: Arc<UserService>,
    admin_api: Arc<dyn AdminRuntime>,
    provider_common_api: Arc<dyn ProviderCommonRuntime>,
    acfun_api: Arc<dyn AcfunRuntime>,
    cctv_api: Arc<dyn CctvRuntime>,
    cloudreve_api: Arc<dyn CloudreveRuntime>,
    douyu_api: Arc<dyn DouyuRuntime>,
    fnos_api: Arc<dyn FnosRuntime>,
    huya_api: Arc<dyn HuyaRuntime>,
    nextcloud_api: Arc<dyn NextcloudRuntime>,
    qnap_api: Arc<dyn QnapRuntime>,
    seafile_api: Arc<dyn SeafileRuntime>,
    synology_api: Arc<dyn SynologyRuntime>,
    truenas_api: Arc<dyn TruenasRuntime>,
    youtube_api: Arc<dyn YoutubeRuntime>,
    chat_service: Option<Arc<ChatService>>,
    clock: Arc<dyn synctv_core::Clock>,
    room_service: Arc<RoomService>,
    presence_service: Arc<synctv_core::service::OnlinePresenceService>,
    realtime_fanout: Arc<dyn RealtimeFanoutService>,
    membership_event_fanout: Arc<dyn MembershipEventFanoutService>,
    room_cache_fanout: Arc<dyn RoomCacheFanoutService>,
    alist_api: Arc<dyn AlistRuntime>,
    bilibili_api: Arc<dyn BilibiliRuntime>,
    emby_api: Arc<dyn EmbyRuntime>,
    douyin_api: Arc<dyn DouyinRuntime>,
    tiktok_api: Arc<dyn TikTokRuntime>,
    twitch_api: Arc<dyn TwitchRuntime>,
    slice_cache_runtime: Arc<synctv_core::service::SliceCacheManagementService>,
    server_state_runtime: Arc<synctv_core::service::ServerStateService>,
    lifecycle_controller: Arc<ManagementLifecycleController>,
    access_controller: ManagementAccessController,
    public_id_codec: Arc<PublicIdCodec>,
}

pub struct ManagementServiceDependencies {
    pub settings: Arc<ManagementRuntimeSettings>,
    pub user_service: Arc<UserService>,
    pub admin_api: Arc<dyn AdminRuntime>,
    pub public_id_codec: Arc<PublicIdCodec>,
    pub provider_common_api: Arc<dyn ProviderCommonRuntime>,
    pub acfun_api: Arc<dyn AcfunRuntime>,
    pub cctv_api: Arc<dyn CctvRuntime>,
    pub cloudreve_api: Arc<dyn CloudreveRuntime>,
    pub douyu_api: Arc<dyn DouyuRuntime>,
    pub fnos_api: Arc<dyn FnosRuntime>,
    pub huya_api: Arc<dyn HuyaRuntime>,
    pub nextcloud_api: Arc<dyn NextcloudRuntime>,
    pub qnap_api: Arc<dyn QnapRuntime>,
    pub seafile_api: Arc<dyn SeafileRuntime>,
    pub synology_api: Arc<dyn SynologyRuntime>,
    pub truenas_api: Arc<dyn TruenasRuntime>,
    pub youtube_api: Arc<dyn YoutubeRuntime>,
    pub chat_service: Option<Arc<ChatService>>,
    pub clock: Arc<dyn synctv_core::Clock>,
    pub room_service: Arc<RoomService>,
    pub presence_service: Arc<synctv_core::service::OnlinePresenceService>,
    pub realtime_fanout: Arc<dyn RealtimeFanoutService>,
    pub membership_event_fanout: Arc<dyn MembershipEventFanoutService>,
    pub room_cache_fanout: Arc<dyn RoomCacheFanoutService>,
    pub alist_api: Arc<dyn AlistRuntime>,
    pub bilibili_api: Arc<dyn BilibiliRuntime>,
    pub emby_api: Arc<dyn EmbyRuntime>,
    pub douyin_api: Arc<dyn DouyinRuntime>,
    pub tiktok_api: Arc<dyn TikTokRuntime>,
    pub twitch_api: Arc<dyn TwitchRuntime>,
    pub slice_cache_runtime: Arc<synctv_core::service::SliceCacheManagementService>,
    pub server_state_runtime: Arc<synctv_core::service::ServerStateService>,
    pub lifecycle_controller: Arc<ManagementLifecycleController>,
    pub management_auth_token: String,
}

impl ManagementServiceImpl {
    #[must_use]
    pub fn new(deps: ManagementServiceDependencies) -> Self {
        let ManagementServiceDependencies {
            settings,
            user_service,
            admin_api,
            public_id_codec,
            provider_common_api,
            acfun_api,
            cctv_api,
            cloudreve_api,
            douyu_api,
            fnos_api,
            huya_api,
            nextcloud_api,
            qnap_api,
            seafile_api,
            synology_api,
            truenas_api,
            youtube_api,
            chat_service,
            clock,
            room_service,
            presence_service,
            realtime_fanout,
            membership_event_fanout,
            room_cache_fanout,
            alist_api,
            bilibili_api,
            emby_api,
            douyin_api,
            tiktok_api,
            twitch_api,
            slice_cache_runtime,
            server_state_runtime,
            lifecycle_controller,
            management_auth_token,
        } = deps;

        Self {
            settings,
            user_service,
            admin_api,
            provider_common_api,
            acfun_api,
            cctv_api,
            cloudreve_api,
            douyu_api,
            fnos_api,
            huya_api,
            nextcloud_api,
            qnap_api,
            seafile_api,
            synology_api,
            truenas_api,
            youtube_api,
            chat_service,
            clock,
            room_service,
            presence_service,
            realtime_fanout,
            membership_event_fanout,
            room_cache_fanout,
            alist_api,
            bilibili_api,
            emby_api,
            douyin_api,
            tiktok_api,
            twitch_api,
            slice_cache_runtime,
            server_state_runtime,
            lifecycle_controller,
            access_controller: ManagementAccessController::new(&management_auth_token),
            public_id_codec,
        }
    }

    fn management_actor(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<ValidatedManagementUser, Status> {
        self.access_controller.authorize(request)?;
        Ok(ValidatedManagementUser {
            user_id: LOCAL_MANAGEMENT_ACTOR_USER_ID,
            role: CoreUserRole::Root,
        })
    }

    fn check_admin_get_validated(
        &self,
        request: &Request<impl std::fmt::Debug>,
    ) -> Result<ValidatedManagementUser, Status> {
        self.management_actor(request)
    }

    async fn resolve_required_user_ref(
        &self,
        user: Option<UserRef>,
        field_name: &str,
    ) -> Result<String, Status> {
        let user = user.ok_or_else(|| {
            Status::invalid_argument(format!("{field_name} is required for this command"))
        })?;
        self.resolve_user_ref_value(user, field_name, true).await
    }

    async fn resolve_optional_user_ref(
        &self,
        user: Option<UserRef>,
        field_name: &str,
    ) -> Result<String, Status> {
        let Some(user) = user else {
            return Ok(String::new());
        };
        self.resolve_user_ref_value(user, field_name, false).await
    }

    async fn resolve_required_user_selector(
        &self,
        user_id: &str,
        username: &str,
        field_name: &str,
    ) -> Result<String, Status> {
        self.resolve_user_selector_value(user_id, username, field_name, true)
            .await
    }

    async fn resolve_optional_user_selector(
        &self,
        user_id: &str,
        username: &str,
        field_name: &str,
    ) -> Result<String, Status> {
        self.resolve_user_selector_value(user_id, username, field_name, false)
            .await
    }

    async fn resolve_user_selector_value(
        &self,
        user_id: &str,
        username: &str,
        field_name: &str,
        required: bool,
    ) -> Result<String, Status> {
        let user_id = user_id.trim();
        let username = username.trim();
        match (!user_id.is_empty(), !username.is_empty()) {
            (true, true) => Err(Status::invalid_argument(format!(
                "{field_name} must contain either user_id or username"
            ))),
            (true, false) => {
                self.public_id_codec
                    .decode_user_id(user_id)
                    .map_err(|error| {
                        Status::invalid_argument(format!(
                            "{field_name}.user_id is invalid: {error}"
                        ))
                    })?;
                Ok(user_id.to_string())
            }
            (false, true) => {
                let user = self
                    .user_service
                    .get_user_by_username(username)
                    .await
                    .map_err(map_management_user_lookup_error)?;
                self.public_id_codec
                    .encode_user_id(user.id)
                    .map_err(|error| {
                        Status::internal(format!("failed to encode resolved user id: {error}"))
                    })
            }
            (false, false) => {
                if required {
                    Err(Status::invalid_argument(format!(
                        "{field_name} must contain either user_id or username"
                    )))
                } else {
                    Ok(String::new())
                }
            }
        }
    }

    async fn resolve_user_ref_value(
        &self,
        user: UserRef,
        field_name: &str,
        required: bool,
    ) -> Result<String, Status> {
        match user.value {
            Some(crate::proto::user_ref::Value::UserId(user_id)) => {
                let trimmed = user_id.trim();
                if trimmed.is_empty() {
                    if required {
                        Err(Status::invalid_argument(format!(
                            "{field_name}.user_id must not be empty"
                        )))
                    } else {
                        Ok(String::new())
                    }
                } else {
                    self.public_id_codec
                        .decode_user_id(trimmed)
                        .map_err(|error| {
                            Status::invalid_argument(format!(
                                "{field_name}.user_id is invalid: {error}"
                            ))
                        })?;
                    Ok(trimmed.to_string())
                }
            }
            Some(crate::proto::user_ref::Value::Username(username)) => {
                let username = username.trim();
                if username.is_empty() {
                    if required {
                        return Err(Status::invalid_argument(format!(
                            "{field_name}.username must not be empty"
                        )));
                    }
                    return Ok(String::new());
                }

                let user = self
                    .user_service
                    .get_user_by_username(username)
                    .await
                    .map_err(map_management_user_lookup_error)?;
                self.public_id_codec
                    .encode_user_id(user.id)
                    .map_err(|error| {
                        Status::internal(format!("failed to encode resolved user id: {error}"))
                    })
            }
            Some(crate::proto::user_ref::Value::Email(email)) => {
                let email = email.trim();
                if email.is_empty() {
                    if required {
                        return Err(Status::invalid_argument(format!(
                            "{field_name}.email must not be empty"
                        )));
                    }
                    return Ok(String::new());
                }

                let user = self
                    .user_service
                    .get_by_email(email)
                    .await
                    .map_err(map_management_user_lookup_error)?
                    .ok_or_else(|| Status::not_found("User not found"))?;
                self.public_id_codec
                    .encode_user_id(user.id)
                    .map_err(|error| {
                        Status::internal(format!("failed to encode resolved user id: {error}"))
                    })
            }
            None => {
                if required {
                    Err(Status::invalid_argument(format!(
                        "{field_name} must contain user_id, username, or email"
                    )))
                } else {
                    Ok(String::new())
                }
            }
        }
    }

    async fn resolve_client_actor_user_id(&self, actor: Option<UserRef>) -> Result<UserId, Status> {
        // Resolve resource ownership and attribution to a persistent user before
        // the management principal reaches client or administrative workflows.
        let actor_user_id = self.resolve_required_user_ref(actor, "actor").await?;
        let actor_user_id = self
            .public_id_codec
            .decode_user_id(&actor_user_id)
            .map_err(|error| Status::invalid_argument(format!("Invalid actor.user_id: {error}")))?;
        let user = self
            .user_service
            .get_user(&actor_user_id)
            .await
            .map_err(map_management_user_lookup_error)?;
        validate_client_actor_user(&user)?;
        Ok(user.id)
    }

    async fn resolve_optional_client_actor_user_id(
        &self,
        actor: Option<UserRef>,
    ) -> Result<Option<UserId>, Status> {
        match actor {
            Some(actor) => self
                .resolve_client_actor_user_id(Some(actor))
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    async fn resolve_client_actor_and_request<T>(
        &self,
        actor: Option<UserRef>,
        request: Option<T>,
    ) -> Result<(UserId, T), Status> {
        let actor_user_id = self.resolve_client_actor_user_id(actor).await?;
        let request = Self::required_nested_request(request, "request")?;
        Ok((actor_user_id, request))
    }

    async fn create_playlist_for_actor(
        &self,
        actor_user_id: UserId,
        room_id: &str,
        req: client_proto::CreatePlaylistRequest,
    ) -> Result<client_proto::Playlist, Status> {
        synctv_proto::validate(&req)
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let room_id = self
            .public_id_codec
            .decode_room_id(room_id)
            .map_err(Status::invalid_argument)?;
        let parent_id = optional_playlist_id_from_public(req.parent_id, &self.public_id_codec)?;
        let source_provider = source_provider_from_proto_filter(req.source_provider)?;
        let source_config = match req.source_config {
            Some(source_config) => {
                let (config_provider, config) =
                    synctv_adapter::source_config::playlist_source_config_from_proto(Some(
                        source_config,
                    ))
                    .map_err(|error| Status::invalid_argument(error.to_string()))?;
                if source_provider != Some(config_provider) {
                    return Err(Status::invalid_argument(format!(
                        "source_provider '{}' does not match source_config provider '{}'",
                        source_provider.map_or("", synctv_core::models::SourceProvider::as_str),
                        config_provider.as_str()
                    )));
                }
                Some(config)
            }
            None => None,
        };
        let provider_instance_name = {
            let trimmed = req.provider_instance_name.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        };
        let actor = self
            .user_service
            .get_user(&actor_user_id)
            .await
            .map_err(map_core_error)?;
        let actor_username = actor.username.clone();
        let clock = self.clock.clone();
        let prepared_outbox_fanout =
            PreparedOutboxFanout::new(self.realtime_fanout.clone(), move |playlist: &Playlist| {
                RealtimeEvent::PlaylistCreated {
                    event_id: synctv_common::snanoid!(16),
                    room_id,
                    user_id: actor_user_id,
                    username: actor_username.clone(),
                    playlist: playlist.clone(),
                    timestamp: clock.now(),
                }
            });
        let playlist = self
            .room_service
            .playlist_service()
            .create_playlist_with_outbox(
                room_id,
                actor_user_id,
                synctv_core::service::CreatePlaylistRequest {
                    room_id,
                    name: req.name,
                    description: req.description,
                    parent_id,
                    source_provider,
                    source_config,
                    provider_instance_name,
                },
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(map_core_error)?;
        prepared_outbox_fanout.publish_after_outbox_commit();
        self.room_cache_fanout.publish_invalidation(&room_id);

        let item_count = self
            .room_service
            .media_service()
            .count_room_playlist_media(&room_id, &playlist.id)
            .await
            .map_err(map_core_error)?;
        created_playlist_to_client_proto(
            &playlist,
            item_count,
            actor_user_id,
            &self.public_id_codec,
        )
    }

    async fn add_media_for_actor(
        &self,
        actor_user_id: UserId,
        room_id: &str,
        req: client_proto::AddMediaRequest,
    ) -> Result<client_proto::Media, Status> {
        let room_id = self
            .public_id_codec
            .decode_room_id(room_id)
            .map_err(Status::invalid_argument)?;
        let service_req =
            synctv_adapter::client::add_media_request_from_client_proto(req, &self.public_id_codec)
                .map_err(|error| Status::invalid_argument(error.to_string()))?;

        let existing_count = if let Some(ref playlist_id) = service_req.playlist_id {
            self.room_service
                .media_service()
                .count_room_playlist_media(&room_id, playlist_id)
                .await
        } else {
            self.room_service
                .media_service()
                .count_room_root_media(&room_id)
                .await
        }
        .map_err(map_core_error)?;
        let existing_count = usize::try_from(existing_count)
            .map_err(|_| Status::internal("media count exceeds usize::MAX"))?;
        if existing_count >= synctv_core::validation::MEDIA_PLAYLIST_MAX_ITEMS {
            return Err(Status::invalid_argument(format!(
                "Playlist has reached maximum size of {} items",
                synctv_core::validation::MEDIA_PLAYLIST_MAX_ITEMS
            )));
        }

        let actor = self
            .user_service
            .get_user(&actor_user_id)
            .await
            .map_err(map_core_error)?;
        let actor_username = actor.username.clone();
        let clock = self.clock.clone();
        let prepared_outbox_fanout =
            PreparedOutboxFanout::new(self.realtime_fanout.clone(), move |media: &Media| {
                RealtimeEvent::MediaAdded {
                    event_id: synctv_common::snanoid!(16),
                    room_id,
                    user_id: actor_user_id,
                    username: actor_username.clone(),
                    media_id: media.id,
                    media_title: media.name.clone(),
                    timestamp: clock.now(),
                }
            });
        let media = self
            .room_service
            .media_service()
            .add_media_with_outbox(
                room_id,
                actor_user_id,
                service_req,
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(map_core_error)?;
        prepared_outbox_fanout.publish_after_outbox_commit();

        created_media_to_client_proto(&media, actor_user_id, &self.public_id_codec)
    }

    async fn transfer_room_ownership_for_actor(
        &self,
        current_owner_id: UserId,
        room_id: &str,
        req: client_proto::TransferRoomOwnershipRequest,
    ) -> Result<client_proto::Room, Status> {
        synctv_proto::validate(&req)
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let room_id = room_id_from_public(room_id, &self.public_id_codec)?;
        let new_owner_id = user_id_from_public(req.new_owner_user_id, &self.public_id_codec)?;

        let target_presence = self
            .presence_service
            .user_room_stats_fresh(current_owner_id, room_id)
            .await
            .map_err(map_core_error)?;
        let prepared_membership_fanout = self
            .membership_event_fanout
            .prepare_permission_changed_outbox_fanout(
                target_presence.is_online,
                target_presence.connection_count,
            );
        let room = self
            .room_service
            .transfer_room_ownership_with_outbox(
                room_id,
                current_owner_id,
                new_owner_id,
                Some(prepared_membership_fanout.outbox_factory()),
            )
            .await
            .map_err(Self::map_room_access_error)?;
        prepared_membership_fanout.publish_after_outbox_commit();
        self.room_cache_fanout.publish_invalidation(&room_id);

        let (settings, member_count, creator) = tokio::join!(
            self.room_service.get_room_settings(&room_id),
            self.room_service.get_member_count(&room_id),
            self.user_service.get_user(&room.created_by),
        );
        let settings = settings.map_err(map_core_error)?;
        let member_count = member_count.map_err(map_core_error)?;
        let creator = creator.map_err(map_core_error)?;

        created_room_to_client_proto(
            &room,
            &settings,
            member_count,
            &creator,
            &self.public_id_codec,
        )
    }

    async fn client_room_for_favorite_response(
        &self,
        room: &Room,
    ) -> Result<client_proto::Room, Status> {
        let (settings, member_count, creator) = tokio::join!(
            self.room_service.get_room_settings(&room.id),
            self.room_service.get_member_count(&room.id),
            self.user_service.get_user(&room.created_by),
        );
        let settings = settings.map_err(map_core_error)?;
        let member_count = member_count.map_err(map_core_error)?;
        let creator = creator.map_err(map_core_error)?;
        created_room_to_client_proto(
            room,
            &settings,
            member_count,
            &creator,
            &self.public_id_codec,
        )
    }

    async fn favorite_room_for_actor(
        &self,
        actor_user_id: UserId,
        req: client_proto::FavoriteRoomRequest,
    ) -> Result<client_proto::FavoriteRoomResponse, Status> {
        synctv_proto::validate(&req)
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let room_id = room_id_from_public(&req.room_id, &self.public_id_codec)?;
        let room = self
            .room_service
            .favorite_room(&actor_user_id, &room_id)
            .await
            .map_err(map_core_error)?;
        Ok(client_proto::FavoriteRoomResponse {
            room: Some(self.client_room_for_favorite_response(&room).await?),
        })
    }

    async fn unfavorite_room_for_actor(
        &self,
        actor_user_id: UserId,
        req: client_proto::UnfavoriteRoomRequest,
    ) -> Result<client_proto::UnfavoriteRoomResponse, Status> {
        synctv_proto::validate(&req)
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let room_id = room_id_from_public(&req.room_id, &self.public_id_codec)?;
        let room = self
            .room_service
            .unfavorite_room(&actor_user_id, &room_id)
            .await
            .map_err(map_core_error)?;
        Ok(client_proto::UnfavoriteRoomResponse {
            room: Some(self.client_room_for_favorite_response(&room).await?),
        })
    }

    async fn list_favorite_rooms_for_actor(
        &self,
        actor_user_id: UserId,
        req: client_proto::ListFavoriteRoomsRequest,
    ) -> Result<client_proto::ListFavoriteRoomsResponse, Status> {
        synctv_proto::validate(&req)
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let page = defaultable_page_i32_to_u32(req.page);
        let page_size = defaultable_page_size_i32_to_u32(req.page_size, 100);
        let search = (!req.search.is_empty()).then_some(req.search);
        let (rooms, total) = self
            .room_service
            .list_favorite_rooms(
                &actor_user_id,
                PageParams::new(page, page_size),
                search.as_deref(),
            )
            .await
            .map_err(map_core_error)?;

        let rooms_ref = &rooms;
        let response_rooms = stream::iter(0..rooms.len())
            .map(|index| async move {
                self.client_room_for_favorite_response(&rooms_ref[index])
                    .await
            })
            .buffered(MANAGEMENT_ROOM_LOAD_CONCURRENCY)
            .try_collect()
            .await?;

        Ok(client_proto::ListFavoriteRoomsResponse {
            rooms: response_rooms,
            total: i32::try_from(total)
                .map_err(|_| Status::internal("favorite room total exceeds i32::MAX"))?,
        })
    }

    async fn chat_messages_to_client_proto(
        &self,
        messages: Vec<ChatMessageWithAttachments>,
    ) -> Result<Vec<client_proto::ChatMessageReceive>, Status> {
        let user_ids: Vec<UserId> = messages
            .iter()
            .filter_map(|message| message.message.user_id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let username_map = self
            .user_service
            .get_usernames(&user_ids)
            .await
            .map_err(map_core_error)?;

        messages
            .into_iter()
            .map(|message| {
                let (user_id, username) = match &message.message.user_id {
                    Some(uid) => {
                        let user_id =
                            self.public_id_codec.encode_user_id(*uid).map_err(|error| {
                                Status::internal(format!(
                                    "failed to encode chat message user id: {error}"
                                ))
                            })?;
                        let username = username_map.get(uid).cloned();
                        (user_id, username)
                    }
                    None => (String::new(), None),
                };

                let mut proto = synctv_adapter::chat::chat_message_receive_to_proto(
                    &message,
                    &self.public_id_codec,
                    username,
                )
                .map_err(|error| Status::internal(error.to_string()))?;
                proto.user_id = user_id;
                Ok(proto)
            })
            .collect()
    }

    async fn search_chat_messages_for_actor(
        &self,
        actor_user_id: UserId,
        room_id: &str,
        req: client_proto::SearchChatMessagesRequest,
    ) -> Result<client_proto::SearchChatMessagesResponse, Status> {
        let room_id = room_id_from_public(room_id, &self.public_id_codec)?;
        self.room_service
            .check_membership(&room_id, &actor_user_id)
            .await
            .map_err(Self::map_room_access_error)?;
        self.room_service
            .check_permission(&room_id, &actor_user_id, RoomPermission::VIEW_CHAT_HISTORY)
            .await
            .map_err(Self::map_room_access_error)?;

        let query =
            search_chat_messages_query_from_client_proto(room_id, &req, &self.public_id_codec)?;
        let chat_service = self
            .chat_service
            .as_ref()
            .ok_or_else(|| Status::unavailable("Chat service is not available on this server."))?;
        let page = chat_service
            .search_messages_with_attachments_for_viewer(query, Some(&actor_user_id))
            .await
            .map_err(map_core_error)?;
        let next_cursor = page
            .next_cursor
            .map(chat_history_cursor_to_client_proto)
            .unwrap_or_default();
        let messages = self.chat_messages_to_client_proto(page.messages).await?;

        Ok(client_proto::SearchChatMessagesResponse {
            messages,
            next_cursor,
            event_cursor: Some(client_proto::EventCursor {
                event_id: page.event_cursor.event_id,
                sequence: page.event_cursor.sequence,
            }),
        })
    }

    async fn resolve_batch_user_refs(&self, users: Vec<UserRef>) -> BatchUserResolution {
        let mut resolved = Vec::with_capacity(users.len());
        let mut failures = Vec::new();
        let mut seen = std::collections::HashSet::with_capacity(users.len());

        let resolutions = stream::iter(
            users
                .into_iter()
                .map(|user| self.resolve_batch_user_ref(user)),
        )
        .buffered(MANAGEMENT_USER_RESOLUTION_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

        for resolution in resolutions {
            match resolution {
                Ok(user_id) if seen.insert(user_id.clone()) => resolved.push(user_id),
                Ok(_) => {}
                Err(failure) => failures.push(failure),
            }
        }

        BatchUserResolution {
            user_ids: resolved,
            failures,
        }
    }

    async fn resolve_batch_user_ref(
        &self,
        user: UserRef,
    ) -> Result<String, admin_proto::BatchResultItem> {
        match user.value {
            Some(crate::proto::user_ref::Value::Username(username)) => {
                let username = username.trim();
                if username.is_empty() {
                    return Err(Self::batch_user_ref_failure(
                        "",
                        "username values must not be empty",
                    ));
                }

                match self.user_service.get_user_by_username(username).await {
                    Ok(user) => self
                        .public_id_codec
                        .encode_user_id(user.id)
                        .map_err(|error| {
                            Self::batch_user_ref_failure(
                                username,
                                format!("Failed to encode resolved user id: {error}"),
                            )
                        }),
                    Err(synctv_core::Error::NotFound(_)) => Err(Self::batch_user_ref_failure(
                        username,
                        format!("User '{username}' was not found"),
                    )),
                    Err(error) => Err(Self::batch_user_ref_failure(
                        username,
                        format!("Failed to resolve user '{username}': {error}"),
                    )),
                }
            }
            Some(crate::proto::user_ref::Value::UserId(user_id)) => {
                let trimmed = user_id.trim();
                if trimmed.is_empty() {
                    return Err(Self::batch_user_ref_failure(
                        "",
                        "user_id values must not be empty",
                    ));
                }
                if let Err(error) = self.public_id_codec.decode_user_id(trimmed) {
                    return Err(Self::batch_user_ref_failure(
                        trimmed,
                        format!("user_id is invalid: {error}"),
                    ));
                }
                Ok(trimmed.to_string())
            }
            Some(crate::proto::user_ref::Value::Email(email)) => {
                let email = email.trim();
                if email.is_empty() {
                    return Err(Self::batch_user_ref_failure(
                        "",
                        "email values must not be empty",
                    ));
                }
                match self.user_service.get_by_email(email).await {
                    Ok(Some(user)) => {
                        self.public_id_codec
                            .encode_user_id(user.id)
                            .map_err(|error| {
                                Self::batch_user_ref_failure(
                                    email,
                                    format!("Failed to encode resolved user id: {error}"),
                                )
                            })
                    }
                    Ok(None) => Err(Self::batch_user_ref_failure(
                        email,
                        format!("User with email '{email}' was not found"),
                    )),
                    Err(error) => Err(Self::batch_user_ref_failure(
                        email,
                        format!("Failed to resolve user email '{email}': {error}"),
                    )),
                }
            }
            None => Err(Self::batch_user_ref_failure(
                "",
                "user ref must contain user_id, username, or email",
            )),
        }
    }

    fn batch_user_ref_failure(
        id: impl Into<String>,
        error: impl Into<String>,
    ) -> admin_proto::BatchResultItem {
        admin_proto::BatchResultItem {
            id: id.into(),
            success: false,
            error: error.into(),
        }
    }

    fn append_batch_user_ref_failures(
        results: &mut Vec<admin_proto::BatchResultItem>,
        failed: &mut i32,
        failures: Vec<admin_proto::BatchResultItem>,
    ) {
        *failed = failed.saturating_add(i32::try_from(failures.len()).unwrap_or(i32::MAX));
        results.extend(failures);
    }

    fn empty_batch_ban_users_response(
        failures: Vec<admin_proto::BatchResultItem>,
    ) -> admin_proto::BatchBanUsersResponse {
        admin_proto::BatchBanUsersResponse {
            succeeded: 0,
            failed: i32::try_from(failures.len()).unwrap_or(i32::MAX),
            results: failures,
        }
    }

    fn empty_batch_delete_users_response(
        failures: Vec<admin_proto::BatchResultItem>,
    ) -> admin_proto::BatchDeleteUsersResponse {
        admin_proto::BatchDeleteUsersResponse {
            succeeded: 0,
            failed: i32::try_from(failures.len()).unwrap_or(i32::MAX),
            results: failures,
        }
    }

    fn grpc_request_context<T: std::fmt::Debug>(&self, request: &Request<T>) -> RequestContext {
        let ip_address = match synctv_adapter::grpc::extract_client_ip(request, |ip| {
            self.settings.is_trusted_proxy(ip)
        }) {
            Ok(ip_address) => ip_address.map(|ip| ip.to_string()),
            Err(error) => {
                tracing::warn!(error = %error, "Failed to extract management request client IP");
                None
            }
        };
        let user_agent = request
            .metadata()
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .map(std::string::ToString::to_string);
        RequestContext {
            ip_address,
            user_agent,
        }
    }

    fn required_nested_request<T>(
        request: Option<T>,
        request_name: &'static str,
    ) -> Result<T, Status> {
        request.ok_or_else(|| Status::invalid_argument(format!("{request_name} is required")))
    }

    fn optional_instance_name(raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn alist_login_command(req: alist_proto::LoginRequest) -> AlistLoginCommand {
        AlistLoginCommand {
            host: req.host,
            username: req.username,
            credential: req.credential.map(|credential| match credential {
                alist_proto::login_request::Credential::Password(password) => {
                    AlistLoginCredential::Password(password)
                }
                alist_proto::login_request::Credential::HashedPassword(hashed_password) => {
                    AlistLoginCredential::HashedPassword(hashed_password)
                }
            }),
            otp_code: req.otp_code,
            otp_secret: req.otp_secret,
        }
    }

    fn emby_login_command(req: emby_proto::LoginRequest) -> EmbyLoginCommand {
        EmbyLoginCommand {
            host: req.host,
            username: req.username,
            credential: req.credential.map(|credential| match credential {
                emby_proto::login_request::Credential::Password(password) => {
                    EmbyLoginCredential::Password(password)
                }
                emby_proto::login_request::Credential::ApiKey(api_key) => {
                    EmbyLoginCredential::ApiKey(api_key)
                }
            }),
        }
    }

    async fn collect_server_state_response(
        &self,
        target_node_id: Option<String>,
        all_nodes: bool,
    ) -> Result<GetServerStateResponse, Status> {
        let response = self
            .server_state_runtime
            .collect_server_state(synctv_core::service::ServerStateSelection {
                node_id: target_node_id,
                all_nodes,
            })
            .await
            .map_err(|error| map_server_state_error(&error))?;
        Ok(server_state_to_management(response))
    }

    fn map_room_access_error(error: synctv_core::Error) -> Status {
        match error {
            synctv_core::Error::Authorization(message) => {
                Status::permission_denied(format!("Forbidden: {message}"))
            }
            other => map_core_error(other),
        }
    }
}

#[tonic::async_trait]
impl ManagementService for ManagementServiceImpl {
    type StopServerStream =
        Pin<Box<dyn Stream<Item = Result<StopServerEvent, Status>> + Send + 'static>>;

    async fn acfun_resolve(
        &self,
        request: Request<crate::proto::AcfunResolveRequest>,
    ) -> Result<Response<synctv_proto::providers::acfun::ResolveResponse>, Status> {
        self.provider_acfun_resolve(request).await
    }

    async fn cctv_resolve(
        &self,
        request: Request<crate::proto::CctvResolveRequest>,
    ) -> Result<Response<synctv_proto::providers::cctv::ResolveResponse>, Status> {
        self.provider_cctv_resolve(request).await
    }

    async fn douyu_resolve(
        &self,
        request: Request<crate::proto::DouyuResolveRequest>,
    ) -> Result<Response<synctv_proto::providers::douyu::ResolveResponse>, Status> {
        self.provider_douyu_resolve(request).await
    }

    async fn huya_resolve(
        &self,
        request: Request<crate::proto::HuyaResolveRequest>,
    ) -> Result<Response<synctv_proto::providers::huya::ResolveResponse>, Status> {
        self.provider_huya_resolve(request).await
    }

    async fn youtube_bind(
        &self,
        request: Request<crate::proto::YoutubeBindRequest>,
    ) -> Result<Response<synctv_proto::providers::youtube::BindResponse>, Status> {
        self.provider_youtube_bind(request).await
    }

    async fn youtube_get_binds(
        &self,
        request: Request<crate::proto::YoutubeGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::youtube::GetBindsResponse>, Status> {
        self.provider_youtube_get_binds(request).await
    }

    async fn youtube_unbind(
        &self,
        request: Request<crate::proto::YoutubeUnbindRequest>,
    ) -> Result<Response<synctv_proto::providers::youtube::UnbindResponse>, Status> {
        self.provider_youtube_unbind(request).await
    }

    async fn youtube_resolve(
        &self,
        request: Request<crate::proto::YoutubeResolveRequest>,
    ) -> Result<Response<synctv_proto::providers::youtube::ResolveResponse>, Status> {
        self.provider_youtube_resolve(request).await
    }

    async fn youtube_list(
        &self,
        request: Request<crate::proto::YoutubeListRequest>,
    ) -> Result<Response<synctv_proto::providers::youtube::ListResponse>, Status> {
        self.provider_youtube_list(request).await
    }

    async fn cloudreve_login(
        &self,
        request: Request<crate::proto::CloudreveLoginRequest>,
    ) -> Result<Response<synctv_proto::providers::cloudreve::LoginResponse>, Status> {
        self.provider_cloudreve_login(request).await
    }

    async fn cloudreve_list(
        &self,
        request: Request<crate::proto::CloudreveListRequest>,
    ) -> Result<Response<synctv_proto::providers::cloudreve::ListResponse>, Status> {
        self.provider_cloudreve_list(request).await
    }

    async fn cloudreve_search(
        &self,
        request: Request<crate::proto::CloudreveSearchRequest>,
    ) -> Result<Response<synctv_proto::providers::cloudreve::SearchResponse>, Status> {
        self.provider_cloudreve_search(request).await
    }

    async fn cloudreve_get_me(
        &self,
        request: Request<crate::proto::CloudreveGetMeRequest>,
    ) -> Result<Response<synctv_proto::providers::cloudreve::GetMeResponse>, Status> {
        self.provider_cloudreve_get_me(request).await
    }

    async fn cloudreve_logout(
        &self,
        request: Request<crate::proto::CloudreveLogoutRequest>,
    ) -> Result<Response<synctv_proto::providers::cloudreve::LogoutResponse>, Status> {
        self.provider_cloudreve_logout(request).await
    }

    async fn cloudreve_get_binds(
        &self,
        request: Request<crate::proto::CloudreveGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::cloudreve::GetBindsResponse>, Status> {
        self.provider_cloudreve_get_binds(request).await
    }

    async fn fnos_login(
        &self,
        request: Request<crate::proto::FnosLoginRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::LoginResponse>, Status> {
        self.provider_fnos_login(request).await
    }

    async fn fnos_list(
        &self,
        request: Request<crate::proto::FnosListRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::ListResponse>, Status> {
        self.provider_fnos_list(request).await
    }

    async fn fnos_list_media_libraries(
        &self,
        request: Request<crate::proto::FnosListMediaLibrariesRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::ListMediaLibrariesResponse>, Status> {
        self.provider_fnos_list_media_libraries(request).await
    }

    async fn fnos_list_media_items(
        &self,
        request: Request<crate::proto::FnosListMediaItemsRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::ListMediaItemsResponse>, Status> {
        self.provider_fnos_list_media_items(request).await
    }

    async fn fnos_set_favorite(
        &self,
        request: Request<crate::proto::FnosSetFavoriteRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::SetFavoriteResponse>, Status> {
        self.provider_fnos_set_favorite(request).await
    }

    async fn fnos_set_watched(
        &self,
        request: Request<crate::proto::FnosSetWatchedRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::SetWatchedResponse>, Status> {
        self.provider_fnos_set_watched(request).await
    }

    async fn fnos_get_server_info(
        &self,
        request: Request<crate::proto::FnosGetServerInfoRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::GetServerInfoResponse>, Status> {
        self.provider_fnos_get_server_info(request).await
    }

    async fn fnos_logout(
        &self,
        request: Request<crate::proto::FnosLogoutRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::LogoutResponse>, Status> {
        self.provider_fnos_logout(request).await
    }

    async fn fnos_get_binds(
        &self,
        request: Request<crate::proto::FnosGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::GetBindsResponse>, Status> {
        self.provider_fnos_get_binds(request).await
    }

    async fn nextcloud_login(
        &self,
        request: Request<crate::proto::NextcloudLoginRequest>,
    ) -> Result<Response<synctv_proto::providers::nextcloud::LoginResponse>, Status> {
        self.provider_nextcloud_login(request).await
    }

    async fn nextcloud_start_login_flow(
        &self,
        request: Request<crate::proto::NextcloudStartLoginFlowRequest>,
    ) -> Result<Response<synctv_proto::providers::nextcloud::StartLoginFlowResponse>, Status> {
        self.provider_nextcloud_start_login_flow(request).await
    }

    async fn nextcloud_poll_login_flow(
        &self,
        request: Request<crate::proto::NextcloudPollLoginFlowRequest>,
    ) -> Result<Response<synctv_proto::providers::nextcloud::LoginResponse>, Status> {
        self.provider_nextcloud_poll_login_flow(request).await
    }

    async fn nextcloud_list(
        &self,
        request: Request<crate::proto::NextcloudListRequest>,
    ) -> Result<Response<synctv_proto::providers::nextcloud::ListResponse>, Status> {
        self.provider_nextcloud_list(request).await
    }

    async fn nextcloud_list_favorites(
        &self,
        request: Request<crate::proto::NextcloudListFavoritesRequest>,
    ) -> Result<Response<synctv_proto::providers::nextcloud::ListResponse>, Status> {
        self.provider_nextcloud_list_favorites(request).await
    }

    async fn nextcloud_logout(
        &self,
        request: Request<crate::proto::NextcloudLogoutRequest>,
    ) -> Result<Response<synctv_proto::providers::nextcloud::LogoutResponse>, Status> {
        self.provider_nextcloud_logout(request).await
    }

    async fn nextcloud_get_binds(
        &self,
        request: Request<crate::proto::NextcloudGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::nextcloud::GetBindsResponse>, Status> {
        self.provider_nextcloud_get_binds(request).await
    }

    async fn qnap_login(
        &self,
        request: Request<crate::proto::QnapLoginRequest>,
    ) -> Result<Response<synctv_proto::providers::qnap::LoginResponse>, Status> {
        self.provider_qnap_login(request).await
    }

    async fn qnap_list(
        &self,
        request: Request<crate::proto::QnapListRequest>,
    ) -> Result<Response<synctv_proto::providers::qnap::ListResponse>, Status> {
        self.provider_qnap_list(request).await
    }

    async fn qnap_get_capabilities(
        &self,
        request: Request<crate::proto::QnapGetCapabilitiesRequest>,
    ) -> Result<Response<synctv_proto::providers::qnap::GetCapabilitiesResponse>, Status> {
        self.provider_qnap_get_capabilities(request).await
    }

    async fn qnap_logout(
        &self,
        request: Request<crate::proto::QnapLogoutRequest>,
    ) -> Result<Response<synctv_proto::providers::qnap::LogoutResponse>, Status> {
        self.provider_qnap_logout(request).await
    }

    async fn qnap_get_binds(
        &self,
        request: Request<crate::proto::QnapGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::qnap::GetBindsResponse>, Status> {
        self.provider_qnap_get_binds(request).await
    }

    async fn seafile_login(
        &self,
        request: Request<crate::proto::SeafileLoginRequest>,
    ) -> Result<Response<synctv_proto::providers::seafile::LoginResponse>, Status> {
        self.provider_seafile_login(request).await
    }

    async fn seafile_unlock_library(
        &self,
        request: Request<crate::proto::SeafileUnlockLibraryRequest>,
    ) -> Result<Response<synctv_proto::providers::seafile::UnlockLibraryResponse>, Status> {
        self.provider_seafile_unlock_library(request).await
    }

    async fn seafile_list_repositories(
        &self,
        request: Request<crate::proto::SeafileListRepositoriesRequest>,
    ) -> Result<Response<synctv_proto::providers::seafile::ListResponse>, Status> {
        self.provider_seafile_list_repositories(request).await
    }

    async fn seafile_list(
        &self,
        request: Request<crate::proto::SeafileListRequest>,
    ) -> Result<Response<synctv_proto::providers::seafile::ListResponse>, Status> {
        self.provider_seafile_list(request).await
    }

    async fn seafile_list_starred(
        &self,
        request: Request<crate::proto::SeafileListStarredRequest>,
    ) -> Result<Response<synctv_proto::providers::seafile::ListResponse>, Status> {
        self.provider_seafile_list_starred(request).await
    }

    async fn seafile_logout(
        &self,
        request: Request<crate::proto::SeafileLogoutRequest>,
    ) -> Result<Response<synctv_proto::providers::seafile::LogoutResponse>, Status> {
        self.provider_seafile_logout(request).await
    }

    async fn seafile_get_binds(
        &self,
        request: Request<crate::proto::SeafileGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::seafile::GetBindsResponse>, Status> {
        self.provider_seafile_get_binds(request).await
    }

    async fn synology_login(
        &self,
        request: Request<crate::proto::SynologyLoginRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::LoginResponse>, Status> {
        self.provider_synology_login(request).await
    }

    async fn synology_list_files(
        &self,
        request: Request<crate::proto::SynologyListFilesRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::ListFilesResponse>, Status> {
        self.provider_synology_list_files(request).await
    }

    async fn synology_list_libraries(
        &self,
        request: Request<crate::proto::SynologyListLibrariesRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::ListLibrariesResponse>, Status> {
        self.provider_synology_list_libraries(request).await
    }

    async fn synology_list_movies(
        &self,
        request: Request<crate::proto::SynologyListMoviesRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::ListVideoItemsResponse>, Status> {
        self.provider_synology_list_movies(request).await
    }

    async fn synology_list_tv_shows(
        &self,
        request: Request<crate::proto::SynologyListTvShowsRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::ListVideoItemsResponse>, Status> {
        self.provider_synology_list_tv_shows(request).await
    }

    async fn synology_list_episodes(
        &self,
        request: Request<crate::proto::SynologyListEpisodesRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::ListVideoItemsResponse>, Status> {
        self.provider_synology_list_episodes(request).await
    }

    async fn synology_list_home_videos(
        &self,
        request: Request<crate::proto::SynologyListHomeVideosRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::ListVideoItemsResponse>, Status> {
        self.provider_synology_list_home_videos(request).await
    }

    async fn synology_list_tv_recordings(
        &self,
        request: Request<crate::proto::SynologyListTvRecordingsRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::ListVideoItemsResponse>, Status> {
        self.provider_synology_list_tv_recordings(request).await
    }

    async fn synology_logout(
        &self,
        request: Request<crate::proto::SynologyLogoutRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::LogoutResponse>, Status> {
        self.provider_synology_logout(request).await
    }

    async fn synology_get_binds(
        &self,
        request: Request<crate::proto::SynologyGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::GetBindsResponse>, Status> {
        self.provider_synology_get_binds(request).await
    }

    async fn truenas_login(
        &self,
        request: Request<crate::proto::TruenasLoginRequest>,
    ) -> Result<Response<synctv_proto::providers::truenas::LoginResponse>, Status> {
        self.provider_truenas_login(request).await
    }

    async fn truenas_list(
        &self,
        request: Request<crate::proto::TruenasListRequest>,
    ) -> Result<Response<synctv_proto::providers::truenas::ListResponse>, Status> {
        self.provider_truenas_list(request).await
    }

    async fn truenas_logout(
        &self,
        request: Request<crate::proto::TruenasLogoutRequest>,
    ) -> Result<Response<synctv_proto::providers::truenas::LogoutResponse>, Status> {
        self.provider_truenas_logout(request).await
    }

    async fn truenas_get_binds(
        &self,
        request: Request<crate::proto::TruenasGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::truenas::GetBindsResponse>, Status> {
        self.provider_truenas_get_binds(request).await
    }

    async fn bilibili_list_live_areas(
        &self,
        request: Request<crate::proto::BilibiliListLiveAreasRequest>,
    ) -> Result<Response<synctv_proto::providers::bilibili::ListLiveAreasResponse>, Status> {
        self.provider_bilibili_list_live_areas(request).await
    }

    async fn bilibili_list_playlist(
        &self,
        request: Request<crate::proto::BilibiliListPlaylistRequest>,
    ) -> Result<Response<synctv_proto::providers::bilibili::ListPlaylistResponse>, Status> {
        self.provider_bilibili_list_playlist(request).await
    }

    async fn bilibili_list_favorite_folders(
        &self,
        request: Request<crate::proto::BilibiliListFavoriteFoldersRequest>,
    ) -> Result<Response<synctv_proto::providers::bilibili::ListFavoriteFoldersResponse>, Status>
    {
        self.provider_bilibili_list_favorite_folders(request).await
    }

    async fn bilibili_list_followed_pgc(
        &self,
        request: Request<crate::proto::BilibiliListFollowedPgcRequest>,
    ) -> Result<Response<synctv_proto::providers::bilibili::ListFollowedPgcResponse>, Status> {
        self.provider_bilibili_list_followed_pgc(request).await
    }

    async fn bilibili_list_history(
        &self,
        request: Request<crate::proto::BilibiliListHistoryRequest>,
    ) -> Result<Response<synctv_proto::providers::bilibili::ListHistoryResponse>, Status> {
        self.provider_bilibili_list_history(request).await
    }

    async fn bilibili_list_pgc_timeline(
        &self,
        request: Request<crate::proto::BilibiliListPgcTimelineRequest>,
    ) -> Result<Response<synctv_proto::providers::bilibili::ListPgcTimelineResponse>, Status> {
        self.provider_bilibili_list_pgc_timeline(request).await
    }

    async fn bilibili_list_pgc_seasons(
        &self,
        request: Request<crate::proto::BilibiliListPgcSeasonsRequest>,
    ) -> Result<Response<synctv_proto::providers::bilibili::ListPgcSeasonsResponse>, Status> {
        self.provider_bilibili_list_pgc_seasons(request).await
    }

    async fn twitch_list_followed_live(
        &self,
        request: Request<crate::proto::TwitchListFollowedLiveRequest>,
    ) -> Result<Response<synctv_proto::providers::twitch::ListFollowedLiveResponse>, Status> {
        self.provider_twitch_list_followed_live(request).await
    }

    async fn twitch_list_category_streams(
        &self,
        request: Request<crate::proto::TwitchListCategoryStreamsRequest>,
    ) -> Result<Response<synctv_proto::providers::twitch::ListCategoryStreamsResponse>, Status>
    {
        self.provider_twitch_list_category_streams(request).await
    }

    async fn twitch_list_top_categories(
        &self,
        request: Request<crate::proto::TwitchListTopCategoriesRequest>,
    ) -> Result<Response<synctv_proto::providers::twitch::ListTopCategoriesResponse>, Status> {
        self.provider_twitch_list_top_categories(request).await
    }

    async fn twitch_search_live_channels(
        &self,
        request: Request<crate::proto::TwitchSearchLiveChannelsRequest>,
    ) -> Result<Response<synctv_proto::providers::twitch::SearchLiveChannelsResponse>, Status> {
        self.provider_twitch_search_live_channels(request).await
    }

    async fn twitch_list_schedule(
        &self,
        request: Request<crate::proto::TwitchListScheduleRequest>,
    ) -> Result<Response<synctv_proto::providers::twitch::ListScheduleResponse>, Status> {
        self.provider_twitch_list_schedule(request).await
    }

    async fn list_users(
        &self,
        request: Request<ListUsersRequest>,
    ) -> Result<Response<admin_proto::ListUsersResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_users(ListUsersQuery {
                page: req.page,
                page_size: req.page_size,
                status: map_user_status_filter(req.status)?,
                role: map_user_role_filter(req.role)?,
                search: req.search,
                sort_by: map_management_user_list_sort_by(req.sort_by)?,
                is_banned: req.is_banned,
                include_deleted: req.include_deleted,
                sort_direction: map_management_sort_direction(
                    req.sort_direction,
                    AdminSortDirection::Desc,
                )?,
            })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn get_user(
        &self,
        request: Request<GetUserRequest>,
    ) -> Result<Response<admin_proto::AdminUser>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .get_user(GetUserQuery { user_id })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn get_user_preferences(
        &self,
        request: Request<GetUserPreferencesRequest>,
    ) -> Result<Response<admin_proto::GetUserPreferencesResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .get_user_preferences(GetUserPreferencesQuery { user_id })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn update_user_preferences(
        &self,
        request: Request<UpdateUserPreferencesRequest>,
    ) -> Result<Response<admin_proto::UpdateUserPreferencesResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .update_user_preferences(
                UpdateUserPreferencesCommand {
                    user_id,
                    two_factor_enabled: req.two_factor_enabled,
                    notifications: req
                        .notifications
                        .map(user_notification_preferences_from_client_proto),
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn add_admin(
        &self,
        request: Request<AddAdminRequest>,
    ) -> Result<Response<admin_proto::AdminUser>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .add_admin(AddAdminCommand { user_id }, &validated.user_id, &ctx)
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn remove_admin(
        &self,
        request: Request<RemoveAdminRequest>,
    ) -> Result<Response<admin_proto::RemoveAdminResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .remove_admin(RemoveAdminCommand { user_id }, &validated.user_id, &ctx)
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn list_admins(
        &self,
        request: Request<ListAdminsRequest>,
    ) -> Result<Response<admin_proto::ListAdminsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_admins(ListAdminsQuery {
                page: req.page,
                page_size: req.page_size,
                search: req.search,
                sort_by: map_management_user_list_sort_by(req.sort_by)?,
                sort_direction: map_management_sort_direction(
                    req.sort_direction,
                    AdminSortDirection::Desc,
                )?,
            })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn create_user(
        &self,
        request: Request<CreateUserRequest>,
    ) -> Result<Response<admin_proto::AdminUser>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .create_user(
                CreateUserCommand {
                    username: req.username,
                    email: req.email,
                    role: map_required_user_role(req.role)?,
                    status: map_required_user_status(req.status)?,
                    password: req.password,
                },
                validated.role,
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn delete_user(
        &self,
        request: Request<DeleteUserRequest>,
    ) -> Result<Response<admin_proto::DeleteUserResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .delete_user(DeleteUserCommand { user_id }, &validated.user_id, &ctx)
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn restore_user(
        &self,
        request: Request<RestoreUserRequest>,
    ) -> Result<Response<admin_proto::RestoreUserResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = if req.user_id.trim().is_empty() {
            let username = req.username.trim();
            if username.is_empty() {
                return Err(Status::invalid_argument(
                    "user must contain either user_id or username",
                ));
            }
            let deleted_user_id = self
                .user_service
                .find_deleted_user_id_by_username(username)
                .await
                .map_err(map_management_user_lookup_error)?
                .ok_or_else(|| Status::not_found("Deleted user not found"))?;
            self.public_id_codec
                .encode_user_id(deleted_user_id)
                .map_err(|error| Status::internal(format!("failed to encode user id: {error}")))?
        } else {
            self.resolve_required_user_selector(&req.user_id, "", "user")
                .await?
        };
        let response = self
            .admin_api
            .restore_user(
                RestoreUserCommand {
                    user_id,
                    ignore_identity_conflicts: req.ignore_identity_conflicts,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn ban_user(
        &self,
        request: Request<BanUserRequest>,
    ) -> Result<Response<admin_proto::AdminUser>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .ban_user(
                BanUserCommand {
                    user_id,
                    reason: req.reason,
                },
                &validated.user_id,
                validated.role,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn unban_user(
        &self,
        request: Request<UnbanUserRequest>,
    ) -> Result<Response<admin_proto::AdminUser>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .unban_user(UnbanUserCommand { user_id }, &validated.user_id, &ctx)
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn list_user_registration_reviews(
        &self,
        request: Request<ListUserRegistrationReviewsRequest>,
    ) -> Result<Response<admin_proto::ListUserRegistrationReviewsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_user_registration_reviews(
                ListUserRegistrationReviewsQuery {
                    page: req.page,
                    page_size: req.page_size,
                    status: map_review_status_filter(req.status)?,
                    search: req.search,
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn approve_user_registration_review(
        &self,
        request: Request<ApproveUserRegistrationReviewRequest>,
    ) -> Result<Response<admin_proto::ApproveUserRegistrationReviewResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .approve_user_registration_review(
                ApproveUserRegistrationReviewCommand {
                    request_id: req.request_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn reject_user_registration_review(
        &self,
        request: Request<RejectUserRegistrationReviewRequest>,
    ) -> Result<Response<admin_proto::UserRegistrationReview>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .reject_user_registration_review(
                RejectUserRegistrationReviewCommand {
                    request_id: req.request_id,
                    reason: req.reason,
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn list_room_creation_reviews(
        &self,
        request: Request<ListRoomCreationReviewsRequest>,
    ) -> Result<Response<admin_proto::ListRoomCreationReviewsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_room_creation_reviews(
                ListRoomCreationReviewsQuery {
                    page: req.page,
                    page_size: req.page_size,
                    status: map_review_status_filter(req.status)?,
                    requested_by: req.requested_by,
                    search: req.search,
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn approve_room_creation_review(
        &self,
        request: Request<ApproveRoomCreationReviewRequest>,
    ) -> Result<Response<admin_proto::ApproveRoomCreationReviewResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .approve_room_creation_review(
                ApproveRoomCreationReviewCommand {
                    request_id: req.request_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn reject_room_creation_review(
        &self,
        request: Request<RejectRoomCreationReviewRequest>,
    ) -> Result<Response<admin_proto::RoomCreationReview>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .reject_room_creation_review(
                RejectRoomCreationReviewCommand {
                    request_id: req.request_id,
                    reason: req.reason,
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn list_room_join_reviews(
        &self,
        request: Request<ListRoomJoinReviewsRequest>,
    ) -> Result<Response<admin_proto::ListRoomJoinReviewsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_room_join_reviews(
                ListRoomJoinReviewsQuery {
                    page: req.page,
                    page_size: req.page_size,
                    status: map_review_status_filter(req.status)?,
                    room_id: req.room_id,
                    user_id: req.user_id,
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn approve_room_join_review(
        &self,
        request: Request<ApproveRoomJoinReviewRequest>,
    ) -> Result<Response<admin_proto::ApproveRoomJoinReviewResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .approve_room_join_review(
                ApproveRoomJoinReviewCommand {
                    request_id: req.request_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn reject_room_join_review(
        &self,
        request: Request<RejectRoomJoinReviewRequest>,
    ) -> Result<Response<admin_proto::RoomJoinReview>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .reject_room_join_review(
                RejectRoomJoinReviewCommand {
                    request_id: req.request_id,
                    reason: req.reason,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn list_ban_records(
        &self,
        request: Request<ListBanRecordsRequest>,
    ) -> Result<Response<admin_proto::ListBanRecordsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_ban_records(
                ListBanRecordsQuery {
                    page: req.page,
                    page_size: req.page_size,
                    target_type: map_ban_record_target_type_filter(req.target_type)?,
                    active: req.active,
                    user_id: req.user_id,
                    room_id: req.room_id,
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn update_user_role(
        &self,
        request: Request<UpdateUserRoleRequest>,
    ) -> Result<Response<admin_proto::AdminUser>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .update_user_role(
                UpdateUserRoleCommand {
                    user_id,
                    role: map_required_user_role(req.role)?,
                },
                &validated.user_id,
                validated.role,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn set_user_password(
        &self,
        request: Request<SetUserPasswordRequest>,
    ) -> Result<Response<admin_proto::SetUserPasswordResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .set_user_password(
                SetUserPasswordCommand {
                    user_id,
                    password: req.password,
                    reason: req.reason,
                },
                validated.user_id,
                validated.role,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn update_user_username(
        &self,
        request: Request<UpdateUserUsernameRequest>,
    ) -> Result<Response<admin_proto::AdminUser>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .update_user_username(
                UpdateUserUsernameCommand {
                    user_id,
                    new_username: req.new_username,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn get_user_rooms(
        &self,
        request: Request<GetUserRoomsRequest>,
    ) -> Result<Response<admin_proto::GetUserRoomsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .get_user_rooms(GetUserRoomsQuery {
                user_id,
                page: req.page,
                page_size: req.page_size,
                status: map_room_status_filter(req.status)?,
                search: req.search,
                is_banned: req.is_banned,
                sort_by: map_management_room_list_sort_by(req.sort_by)?,
                sort_direction: map_management_core_sort_direction(
                    req.sort_direction,
                    synctv_core::models::SortDirection::Desc,
                )?,
            })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn batch_ban_users(
        &self,
        request: Request<BatchBanUsersRequest>,
    ) -> Result<Response<admin_proto::BatchBanUsersResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let resolved = self.resolve_batch_user_refs(req.users).await;
        let mut response = if resolved.user_ids.is_empty() {
            Self::empty_batch_ban_users_response(resolved.failures)
        } else {
            let mut response = self
                .admin_api
                .batch_ban_users(
                    BatchBanUsersCommand {
                        user_ids: resolved.user_ids,
                        reason: req.reason,
                    },
                    &validated.user_id,
                    validated.role,
                    &ctx,
                )
                .await
                .map_err(|error| map_api_error(&error))?;
            Self::append_batch_user_ref_failures(
                &mut response.results,
                &mut response.failed,
                resolved.failures,
            );
            response
        };
        response.failed = response.failed.max(0);
        Ok(Response::new(response))
    }

    async fn batch_delete_users(
        &self,
        request: Request<BatchDeleteUsersRequest>,
    ) -> Result<Response<admin_proto::BatchDeleteUsersResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let resolved = self.resolve_batch_user_refs(req.users).await;
        let mut response = if resolved.user_ids.is_empty() {
            Self::empty_batch_delete_users_response(resolved.failures)
        } else {
            let mut response = self
                .admin_api
                .batch_delete_users(
                    BatchDeleteUsersCommand {
                        user_ids: resolved.user_ids,
                    },
                    &validated.user_id,
                    validated.role,
                    &ctx,
                )
                .await
                .map_err(|error| map_api_error(&error))?;
            Self::append_batch_user_ref_failures(
                &mut response.results,
                &mut response.failed,
                resolved.failures,
            );
            response
        };
        response.failed = response.failed.max(0);
        Ok(Response::new(response))
    }

    async fn create_room(
        &self,
        request: Request<CreateRoomRequest>,
    ) -> Result<Response<client_proto::Room>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let mut client_request = client_proto::CreateRoomRequest {
            name: req.name,
            settings: req.settings,
            description: req.description,
            password: req.password,
            category_id: req.category_id,
            label_ids: req.label_ids,
        };

        client_request.name =
            synctv_core::validation::validate_room_name_input(&client_request.name)
                .map_err(|error| Status::invalid_argument(error.to_string()))?;
        if !client_request.description.is_empty() {
            client_request.description = synctv_core::validation::validate_room_description_input(
                &client_request.description,
            )
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        }
        synctv_proto::validate(&client_request)
            .map_err(|error| Status::invalid_argument(error.to_string()))?;

        let settings = client_request
            .settings
            .map(room_settings_from_client_proto)
            .transpose()?;
        let response_settings = settings.clone().unwrap_or_default();
        let password = (!client_request.password.is_empty()).then_some(client_request.password);
        let category_id = optional_room_category_id_from_public(
            &client_request.category_id,
            &self.public_id_codec,
        )?;
        let label_ids =
            room_label_ids_from_public(&client_request.label_ids, &self.public_id_codec)?;
        let clock = self.clock.clone();
        let prepared_outbox_fanout =
            PreparedOutboxFanout::new(self.realtime_fanout.clone(), move |room: &Room| {
                RealtimeEvent::RoomCreated {
                    event_id: synctv_common::snanoid!(16),
                    room_id: room.id,
                    room_name: room.name.clone(),
                    creator_id: actor_user_id,
                    timestamp: clock.now(),
                }
            });

        let (room, _member) = self
            .room_service
            .create_room_with_taxonomy_outbox(
                synctv_core::service::CreateRoomWithTaxonomyRequest {
                    name: client_request.name,
                    description: client_request.description,
                    created_by: actor_user_id,
                    password,
                    settings,
                    category_id,
                    label_ids,
                },
                Some(prepared_outbox_fanout.outbox_factory()),
            )
            .await
            .map_err(map_core_error)?;
        prepared_outbox_fanout.publish_after_outbox_commit();

        let (member_count, creator) = tokio::join!(
            self.room_service.get_member_count(&room.id),
            self.user_service.get_user(&room.created_by),
        );
        let member_count = member_count.map_err(map_core_error)?;
        let creator = creator.map_err(map_core_error)?;
        let response = created_room_to_client_proto(
            &room,
            &response_settings,
            member_count,
            &creator,
            &self.public_id_codec,
        )?;
        Ok(Response::new(response))
    }

    async fn list_rooms(
        &self,
        request: Request<ListRoomsRequest>,
    ) -> Result<Response<admin_proto::ListRoomsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let creator_id = self
            .resolve_optional_user_ref(req.creator, "creator")
            .await?;
        let response = self
            .admin_api
            .list_rooms(ListRoomsQuery {
                page: req.page,
                page_size: req.page_size,
                status: map_room_status_filter(req.status)?,
                search: req.search,
                creator_id,
                is_banned: req.is_banned,
                sort_by: map_management_room_list_sort_by(req.sort_by)?,
                sort_direction: map_management_core_sort_direction(
                    req.sort_direction,
                    synctv_core::models::SortDirection::Desc,
                )?,
                category_id: req.category_id,
                label_ids: req.label_ids,
            })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn list_room_categories(
        &self,
        request: Request<admin_proto::ListRoomCategoriesRequest>,
    ) -> Result<Response<admin_proto::ListRoomCategoriesResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        self.admin_api
            .list_room_categories(ListRoomCategoriesQuery {
                include_disabled: req.include_disabled,
            })
            .await
            .map(Response::new)
            .map_err(|error| map_api_error(&error))
    }

    async fn upsert_room_category(
        &self,
        request: Request<admin_proto::UpsertRoomCategoryRequest>,
    ) -> Result<Response<client_proto::RoomCategory>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        self.admin_api
            .upsert_room_category(UpsertRoomCategoryCommand {
                key: req.key,
                name: req.name,
                description: req.description,
                sort_order: req.sort_order,
                is_enabled: req.is_enabled,
            })
            .await
            .map(Response::new)
            .map_err(|error| map_api_error(&error))
    }

    async fn delete_room_category(
        &self,
        request: Request<admin_proto::DeleteRoomCategoryRequest>,
    ) -> Result<Response<admin_proto::DeleteRoomCategoryResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        self.admin_api
            .delete_room_category(DeleteRoomCategoryCommand {
                category_id: req.category_id,
            })
            .await
            .map(Response::new)
            .map_err(|error| map_api_error(&error))
    }

    async fn list_room_labels(
        &self,
        request: Request<admin_proto::ListRoomLabelsRequest>,
    ) -> Result<Response<admin_proto::ListRoomLabelsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        self.admin_api
            .list_room_labels(ListRoomLabelsQuery {
                include_disabled: req.include_disabled,
                category_id: req.category_id,
            })
            .await
            .map(Response::new)
            .map_err(|error| map_api_error(&error))
    }

    async fn upsert_room_label(
        &self,
        request: Request<admin_proto::UpsertRoomLabelRequest>,
    ) -> Result<Response<client_proto::RoomLabel>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        self.admin_api
            .upsert_room_label(UpsertRoomLabelCommand {
                key: req.key,
                name: req.name,
                description: req.description,
                color: req.color,
                category_id: req.category_id,
                sort_order: req.sort_order,
                is_enabled: req.is_enabled,
            })
            .await
            .map(Response::new)
            .map_err(|error| map_api_error(&error))
    }

    async fn delete_room_label(
        &self,
        request: Request<admin_proto::DeleteRoomLabelRequest>,
    ) -> Result<Response<admin_proto::DeleteRoomLabelResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        self.admin_api
            .delete_room_label(DeleteRoomLabelCommand {
                label_id: req.label_id,
            })
            .await
            .map(Response::new)
            .map_err(|error| map_api_error(&error))
    }

    async fn update_room_taxonomy(
        &self,
        request: Request<admin_proto::UpdateRoomTaxonomyRequest>,
    ) -> Result<Response<admin_proto::Room>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        self.admin_api
            .update_room_taxonomy(
                UpdateRoomTaxonomyCommand {
                    room_id: req.room_id,
                    category_id: req.category_id,
                    label_ids: req.label_ids,
                    clear_category: req.clear_category,
                },
                &validated.user_id,
            )
            .await
            .map(Response::new)
            .map_err(|error| map_api_error(&error))
    }

    async fn get_room(
        &self,
        request: Request<GetRoomRequest>,
    ) -> Result<Response<admin_proto::Room>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_room(GetRoomQuery {
                room_id: req.room_id,
            })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn get_room_members(
        &self,
        request: Request<GetRoomMembersRequest>,
    ) -> Result<Response<admin_proto::GetRoomMembersResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_room_members(GetRoomMembersQuery {
                room_id: req.room_id,
                page: req.page,
                page_size: req.page_size,
                search: req.search,
                role: req.role,
                sort_by: map_room_member_list_sort_by(req.sort_by)?,
                sort_direction: map_management_sort_direction(
                    req.sort_direction,
                    AdminSortDirection::Asc,
                )?,
            })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn search_chat_messages(
        &self,
        request: Request<SearchChatMessagesRequest>,
    ) -> Result<Response<client_proto::SearchChatMessagesResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let user_id = self
            .resolve_optional_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .search_chat_messages_for_actor(
                actor_user_id,
                &req.room_id,
                client_proto::SearchChatMessagesRequest {
                    query: req.query,
                    cursor: req.cursor,
                    limit: req.limit,
                    include_deleted: req.include_deleted,
                    user_id,
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn add_member(
        &self,
        request: Request<AddMemberRequest>,
    ) -> Result<Response<common_proto::RoomMember>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .add_member(
                AddMemberCommand {
                    room_id: req.room_id,
                    user_id,
                    role: req.role,
                    notify: req.notify,
                    remark_name: req.remark_name,
                    display_tag: req.display_tag,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn update_member_remark_name(
        &self,
        request: Request<UpdateMemberRemarkNameRequest>,
    ) -> Result<Response<common_proto::RoomMember>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .update_member_remark_name(
                UpdateMemberRemarkNameCommand {
                    room_id: req.room_id,
                    user_id,
                    remark_name: req.remark_name,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn update_member_display_tag(
        &self,
        request: Request<UpdateMemberDisplayTagRequest>,
    ) -> Result<Response<common_proto::RoomMember>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .update_member_display_tag(
                UpdateMemberDisplayTagCommand {
                    room_id: req.room_id,
                    user_id,
                    display_tag: req.display_tag,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn update_member_permissions(
        &self,
        request: Request<UpdateMemberPermissionsRequest>,
    ) -> Result<Response<common_proto::RoomMember>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .update_member_permissions(
                UpdateMemberPermissionsCommand {
                    room_id: req.room_id,
                    user_id,
                    role: req.role,
                    added_permissions: req.added_permissions,
                    removed_permissions: req.removed_permissions,
                    admin_added_permissions: req.admin_added_permissions,
                    admin_removed_permissions: req.admin_removed_permissions,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn kick_member(
        &self,
        request: Request<KickMemberRequest>,
    ) -> Result<Response<client_proto::KickMemberResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self
            .resolve_required_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .kick_member(
                KickMemberCommand {
                    room_id: req.room_id,
                    user_id,
                    kick_cooldown_seconds: req.kick_cooldown_seconds,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(client_proto::KickMemberResponse {
            success: response.success,
        }))
    }

    async fn get_room_settings(
        &self,
        request: Request<GetRoomSettingsRequest>,
    ) -> Result<Response<admin_proto::GetRoomSettingsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_room_settings(GetRoomSettingsQuery {
                room_id: req.room_id,
            })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn update_room_settings(
        &self,
        request: Request<admin_proto::UpdateRoomSettingsRequest>,
    ) -> Result<Response<admin_proto::Room>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .update_room_settings(
                UpdateRoomSettingsCommand {
                    room_id: req.room_id,
                    settings: req
                        .settings
                        .ok_or_else(|| Status::invalid_argument("settings is required"))?,
                    update_mask: req
                        .update_mask
                        .ok_or_else(|| Status::invalid_argument("update_mask is required"))?,
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn reset_room_settings(
        &self,
        request: Request<ResetRoomSettingsRequest>,
    ) -> Result<Response<admin_proto::Room>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .reset_room_settings(
                ResetRoomSettingsCommand {
                    room_id: req.room_id,
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn transfer_room_ownership(
        &self,
        request: Request<TransferRoomOwnershipRequest>,
    ) -> Result<Response<client_proto::Room>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let new_owner_user_id = self
            .resolve_required_user_ref(req.new_owner, "new_owner")
            .await?;
        let response = self
            .transfer_room_ownership_for_actor(
                actor_user_id,
                &req.room_id,
                client_proto::TransferRoomOwnershipRequest { new_owner_user_id },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn favorite_room(
        &self,
        request: Request<FavoriteRoomRequest>,
    ) -> Result<Response<client_proto::FavoriteRoomResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = self.favorite_room_for_actor(actor_user_id, request).await?;
        Ok(Response::new(response))
    }

    async fn unfavorite_room(
        &self,
        request: Request<UnfavoriteRoomRequest>,
    ) -> Result<Response<client_proto::UnfavoriteRoomResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = self
            .unfavorite_room_for_actor(actor_user_id, request)
            .await?;
        Ok(Response::new(response))
    }

    async fn list_favorite_rooms(
        &self,
        request: Request<ListFavoriteRoomsRequest>,
    ) -> Result<Response<client_proto::ListFavoriteRoomsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = self
            .list_favorite_rooms_for_actor(actor_user_id, request)
            .await?;
        Ok(Response::new(response))
    }

    async fn update_room_password(
        &self,
        request: Request<UpdateRoomPasswordRequest>,
    ) -> Result<Response<admin_proto::UpdateRoomPasswordResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let new_password = match (req.clear, req.new_password) {
            (true, None) => String::new(),
            (true, Some(_)) => {
                return Err(Status::invalid_argument(
                    "new_password must be omitted when clear is true",
                ));
            }
            (false, Some(password)) => password,
            (false, None) => {
                return Err(Status::invalid_argument(
                    "new_password is required when clear is false",
                ));
            }
        };
        let response = self
            .admin_api
            .update_room_password(
                UpdateRoomPasswordCommand {
                    room_id: req.room_id,
                    new_password,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn ban_room(
        &self,
        request: Request<BanRoomRequest>,
    ) -> Result<Response<admin_proto::Room>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .ban_room(
                BanRoomCommand {
                    room_id: req.room_id,
                    reason: req.reason,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn unban_room(
        &self,
        request: Request<UnbanRoomRequest>,
    ) -> Result<Response<admin_proto::Room>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .unban_room(
                UnbanRoomCommand {
                    room_id: req.room_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn delete_room(
        &self,
        request: Request<DeleteRoomRequest>,
    ) -> Result<Response<admin_proto::DeleteRoomResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .delete_room(
                DeleteRoomCommand {
                    room_id: req.room_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn batch_ban_rooms(
        &self,
        request: Request<BatchBanRoomsRequest>,
    ) -> Result<Response<admin_proto::BatchBanRoomsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .batch_ban_rooms(
                BatchBanRoomsCommand {
                    room_ids: req.room_ids,
                    reason: req.reason,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn batch_delete_rooms(
        &self,
        request: Request<BatchDeleteRoomsRequest>,
    ) -> Result<Response<admin_proto::BatchDeleteRoomsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .batch_delete_rooms(
                BatchDeleteRoomsCommand {
                    room_ids: req.room_ids,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn start_playback(
        &self,
        request: Request<StartPlaybackRequest>,
    ) -> Result<Response<client_proto::PlaybackState>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let actor_user_id = self
            .resolve_optional_client_actor_user_id(req.actor)
            .await?;
        let response = self
            .admin_api
            .start_playback(
                StartPlaybackCommand {
                    actor_user_id,
                    room_id: req.room_id,
                    media_id: req.media_id,
                    playlist_id: req.playlist_id,
                    target: req.target,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn stop_playback(
        &self,
        request: Request<StopPlaybackRequest>,
    ) -> Result<Response<client_proto::PlaybackState>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .stop_playback(&req.room_id, &validated.user_id, &ctx)
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn get_playback(
        &self,
        request: Request<GetPlaybackRequest>,
    ) -> Result<Response<client_proto::GetPlaybackResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_playback(
                &req.room_id,
                &validated.user_id,
                req.playback_client_profile,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn update_playback_state(
        &self,
        request: Request<UpdatePlaybackStateRequest>,
    ) -> Result<Response<client_proto::PlaybackState>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .update_playback_state(
                UpdatePlaybackStateCommand {
                    room_id: req.room_id,
                    update_type: req.r#type,
                    playing: req.playing,
                    position: req.position,
                    speed: req.speed,
                    version: req.version,
                    expected_media_id: req.expected_media_id,
                    expected_playlist_id: req.expected_playlist_id,
                    expected_target_hash: req.expected_target_hash,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn create_publish_key(
        &self,
        request: Request<CreatePublishKeyRequest>,
    ) -> Result<Response<client_proto::CreateRoomPublishKeyResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let publish_key_request = client_proto::CreateRoomPublishKeyRequest {
            media_id: req.media_id,
            r#type: req.r#type,
            expires_at: req.expires_at,
        };
        let response = self
            .admin_api
            .create_publish_key_for_actor(
                &req.room_id,
                publish_key_request,
                &actor_user_id,
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn get_stream_info(
        &self,
        request: Request<GetStreamInfoRequest>,
    ) -> Result<Response<client_proto::GetRoomStreamInfoResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_stream_info(&req.room_id, &req.media_id)
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn list_room_streams(
        &self,
        request: Request<ListRoomStreamsRequest>,
    ) -> Result<Response<client_proto::ListRoomStreamsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_room_streams(ListRoomStreamsQuery {
                room_id: req.room_id,
                page: req.page,
                page_size: req.page_size,
                search: req.search,
                sort_by: map_room_stream_list_sort_by(req.sort_by)?,
                sort_direction: map_optional_management_sort_direction(req.sort_direction)?,
            })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn kick_room_stream(
        &self,
        request: Request<KickRoomStreamRequest>,
    ) -> Result<Response<client_proto::KickRoomStreamResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        self.admin_api
            .kick_stream(
                KickStreamCommand {
                    room_id: req.room_id,
                    media_id: req.media_id,
                    reason: req.reason,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(client_proto::KickRoomStreamResponse {}))
    }

    async fn list_playlists(
        &self,
        request: Request<ListPlaylistsRequest>,
    ) -> Result<Response<client_proto::ListPlaylistsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_playlists(
                ListPlaylistsQuery {
                    room_id: req.room_id,
                    parent_id: req.parent_id,
                    page: req.page,
                    page_size: req.page_size,
                    search: req.search,
                    source_provider: req.source_provider,
                    provider_instance_name: req.provider_instance_name,
                    dynamic_only: req.dynamic_only,
                    sort_by: req.sort_by,
                    sort_direction: req.sort_direction,
                    availability: req.availability,
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn get_playlist(
        &self,
        request: Request<GetPlaylistRequest>,
    ) -> Result<Response<client_proto::GetPlaylistResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_playlist(&req.room_id, &req.playlist_id, &validated.user_id)
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn create_playlist(
        &self,
        request: Request<CreatePlaylistRequest>,
    ) -> Result<Response<client_proto::Playlist>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let response = self
            .create_playlist_for_actor(
                actor_user_id,
                &req.room_id,
                client_proto::CreatePlaylistRequest {
                    name: req.name,
                    description: String::new(),
                    parent_id: req.parent_id,
                    source_provider: req.source_provider,
                    source_config: req.source_config,
                    provider_instance_name: req.provider_instance_name,
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn create_alist_playlist(
        &self,
        request: Request<CreateAlistPlaylistRequest>,
    ) -> Result<Response<client_proto::Playlist>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let response = self
            .create_playlist_for_actor(
                actor_user_id,
                &req.room_id,
                client_proto::CreatePlaylistRequest {
                    name: req.name,
                    description: String::new(),
                    parent_id: req.parent_id,
                    source_provider: synctv_proto::source_config::SourceProvider::Alist as i32,
                    source_config: Some(alist_playlist_source_config(
                        &req.server_id,
                        &req.path,
                        &req.password,
                    )?),
                    provider_instance_name: req.provider_instance_name,
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn create_emby_playlist(
        &self,
        request: Request<CreateEmbyPlaylistRequest>,
    ) -> Result<Response<client_proto::Playlist>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let response = self
            .create_playlist_for_actor(
                actor_user_id,
                &req.room_id,
                client_proto::CreatePlaylistRequest {
                    name: req.name,
                    description: String::new(),
                    parent_id: req.parent_id,
                    source_provider: synctv_proto::source_config::SourceProvider::Emby as i32,
                    source_config: Some(emby_playlist_source_config(&req.server_id, &req.item_id)?),
                    provider_instance_name: req.provider_instance_name,
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn update_playlist(
        &self,
        request: Request<UpdatePlaylistRequest>,
    ) -> Result<Response<client_proto::Playlist>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let name = req
            .name
            .ok_or_else(|| Status::invalid_argument("name is required"))?;
        let response = self
            .admin_api
            .update_playlist(
                UpdatePlaylistCommand {
                    room_id: req.room_id,
                    playlist_id: req.playlist_id,
                    name,
                    description: String::new(),
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn move_playlist(
        &self,
        request: Request<MovePlaylistRequest>,
    ) -> Result<Response<client_proto::Playlist>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .move_playlist(
                MovePlaylistCommand {
                    room_id: req.room_id,
                    playlist_id: req.playlist_id,
                    before_playlist_id: req.anchor.as_ref().and_then(|anchor| match anchor {
                        crate::proto::move_playlist_request::Anchor::BeforePlaylistId(id) => {
                            Some(id.clone())
                        }
                        crate::proto::move_playlist_request::Anchor::AfterPlaylistId(_) => None,
                    }),
                    after_playlist_id: req.anchor.and_then(|anchor| match anchor {
                        crate::proto::move_playlist_request::Anchor::BeforePlaylistId(_) => None,
                        crate::proto::move_playlist_request::Anchor::AfterPlaylistId(id) => {
                            Some(id)
                        }
                    }),
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn delete_playlist(
        &self,
        request: Request<DeletePlaylistRequest>,
    ) -> Result<Response<client_proto::DeletePlaylistResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .delete_playlist(
                DeletePlaylistCommand {
                    room_id: req.room_id,
                    playlist_id: req.playlist_id,
                    force: req.force,
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn list_media(
        &self,
        request: Request<ListMediaRequest>,
    ) -> Result<Response<client_proto::ListPlaylistItemsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_media(
                ListMediaQuery {
                    room_id: req.room_id,
                    playlist_id: req.playlist_id,
                    target: req.target,
                    pagination: req.pagination.map(|pagination| match pagination {
                        list_media_request::Pagination::Page(page) => {
                            client_proto::list_playlist_items_request::Pagination::Page(page)
                        }
                        list_media_request::Pagination::Cursor(cursor) => {
                            client_proto::list_playlist_items_request::Pagination::Cursor(cursor)
                        }
                    }),
                    page_size: req.page_size,
                    search: req.search,
                    source_provider: req.source_provider,
                    provider_instance_name: req.provider_instance_name,
                    sort_by: req.sort_by,
                    sort_direction: req.sort_direction,
                    availability: req.availability,
                    refresh: req.refresh,
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn add_media(
        &self,
        request: Request<AddMediaRequest>,
    ) -> Result<Response<client_proto::Media>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let response = self
            .add_media_for_actor(
                actor_user_id,
                &req.room_id,
                client_proto::AddMediaRequest {
                    playlist_id: (!req.playlist_id.is_empty()).then_some(req.playlist_id),
                    description: String::new(),
                    provider_instance_name: req.provider_instance_name,
                    source_config: req.source_config,
                    name: req.name,
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn add_direct_url_media(
        &self,
        request: Request<AddDirectUrlMediaRequest>,
    ) -> Result<Response<client_proto::Media>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let response = self
            .add_media_for_actor(
                actor_user_id,
                &req.room_id,
                client_proto::AddMediaRequest {
                    playlist_id: (!req.playlist_id.is_empty()).then_some(req.playlist_id),
                    description: String::new(),
                    provider_instance_name: String::new(),
                    source_config: Some(direct_url_source_config(
                        req.source_config
                            .ok_or_else(|| Status::invalid_argument("source_config is required"))?,
                    )?),
                    name: req.name,
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn add_alist_media(
        &self,
        request: Request<AddAlistMediaRequest>,
    ) -> Result<Response<client_proto::Media>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let response = self
            .add_media_for_actor(
                actor_user_id,
                &req.room_id,
                client_proto::AddMediaRequest {
                    playlist_id: (!req.playlist_id.is_empty()).then_some(req.playlist_id),
                    description: String::new(),
                    provider_instance_name: req.provider_instance_name,
                    source_config: Some(alist_media_source_config(
                        &req.server_id,
                        &req.path,
                        &req.password,
                    )?),
                    name: req.name,
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn add_emby_media(
        &self,
        request: Request<AddEmbyMediaRequest>,
    ) -> Result<Response<client_proto::Media>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let response = self
            .add_media_for_actor(
                actor_user_id,
                &req.room_id,
                client_proto::AddMediaRequest {
                    playlist_id: (!req.playlist_id.is_empty()).then_some(req.playlist_id),
                    description: String::new(),
                    provider_instance_name: req.provider_instance_name,
                    source_config: Some(emby_media_source_config(&req.server_id, &req.item_id)?),
                    name: req.name,
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn add_bilibili_video_media(
        &self,
        request: Request<AddBilibiliVideoMediaRequest>,
    ) -> Result<Response<client_proto::Media>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let response = self
            .add_media_for_actor(
                actor_user_id,
                &req.room_id,
                client_proto::AddMediaRequest {
                    playlist_id: (!req.playlist_id.is_empty()).then_some(req.playlist_id),
                    description: String::new(),
                    provider_instance_name: req.provider_instance_name,
                    source_config: Some(bilibili_video_source_config(
                        &req.bvid, req.aid, req.cid, req.shared,
                    )?),
                    name: req.name,
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn add_bilibili_pgc_media(
        &self,
        request: Request<AddBilibiliPgcMediaRequest>,
    ) -> Result<Response<client_proto::Media>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let response = self
            .add_media_for_actor(
                actor_user_id,
                &req.room_id,
                client_proto::AddMediaRequest {
                    playlist_id: (!req.playlist_id.is_empty()).then_some(req.playlist_id),
                    description: String::new(),
                    provider_instance_name: req.provider_instance_name,
                    source_config: Some(bilibili_pgc_source_config(req.epid, req.cid, req.shared)?),
                    name: req.name,
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn add_bilibili_live_media(
        &self,
        request: Request<AddBilibiliLiveMediaRequest>,
    ) -> Result<Response<client_proto::Media>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let response = self
            .add_media_for_actor(
                actor_user_id,
                &req.room_id,
                client_proto::AddMediaRequest {
                    playlist_id: (!req.playlist_id.is_empty()).then_some(req.playlist_id),
                    description: String::new(),
                    provider_instance_name: req.provider_instance_name,
                    source_config: Some(bilibili_live_source_config(req.room_live_id, req.shared)?),
                    name: req.name,
                },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn edit_media(
        &self,
        request: Request<EditMediaRequest>,
    ) -> Result<Response<client_proto::Media>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .edit_media(
                EditMediaCommand {
                    room_id: req.room_id,
                    media_id: req.media_id,
                    name: req.name,
                    description: String::new(),
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn delete_media(
        &self,
        request: Request<DeleteMediaRequest>,
    ) -> Result<Response<client_proto::DeleteMediaResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .delete_media(
                DeleteMediaCommand {
                    room_id: req.room_id,
                    media_id: req.media_id,
                    force: req.force,
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn move_media(
        &self,
        request: Request<MoveMediaRequest>,
    ) -> Result<Response<client_proto::MoveMediaResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .move_media(
                MoveMediaCommand {
                    room_id: req.room_id,
                    media_ids: req.media_ids,
                    source_playlist_id: req.source_playlist_id,
                    target_playlist_id: req.target_playlist_id,
                    all_from_scope: req.all_from_scope,
                    before_media_id: req.anchor.as_ref().and_then(|anchor| match anchor {
                        crate::proto::move_media_request::Anchor::BeforeMediaId(id) => {
                            Some(id.clone())
                        }
                        crate::proto::move_media_request::Anchor::AfterMediaId(_) => None,
                    }),
                    after_media_id: req.anchor.and_then(|anchor| match anchor {
                        crate::proto::move_media_request::Anchor::BeforeMediaId(_) => None,
                        crate::proto::move_media_request::Anchor::AfterMediaId(id) => Some(id),
                    }),
                },
                &validated.user_id,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn alist_login(
        &self,
        request: Request<AlistLoginRequest>,
    ) -> Result<Response<alist_proto::LoginResponse>, Status> {
        self.provider_alist_login(request).await
    }

    async fn alist_list(
        &self,
        request: Request<AlistListRequest>,
    ) -> Result<Response<alist_proto::ListResponse>, Status> {
        self.provider_alist_list(request).await
    }

    async fn alist_search(
        &self,
        request: Request<AlistSearchRequest>,
    ) -> Result<Response<alist_proto::SearchResponse>, Status> {
        self.provider_alist_search(request).await
    }

    async fn alist_get_me(
        &self,
        request: Request<AlistGetMeRequest>,
    ) -> Result<Response<alist_proto::GetMeResponse>, Status> {
        self.provider_alist_get_me(request).await
    }

    async fn alist_logout(
        &self,
        request: Request<AlistLogoutRequest>,
    ) -> Result<Response<alist_proto::LogoutResponse>, Status> {
        self.provider_alist_logout(request).await
    }

    async fn alist_get_binds(
        &self,
        request: Request<AlistGetBindsRequest>,
    ) -> Result<Response<alist_proto::GetBindsResponse>, Status> {
        self.provider_alist_get_binds(request).await
    }

    async fn emby_login(
        &self,
        request: Request<EmbyLoginRequest>,
    ) -> Result<Response<emby_proto::LoginResponse>, Status> {
        self.provider_emby_login(request).await
    }

    async fn emby_list(
        &self,
        request: Request<EmbyListRequest>,
    ) -> Result<Response<emby_proto::ListResponse>, Status> {
        self.provider_emby_list(request).await
    }

    async fn emby_get_me(
        &self,
        request: Request<EmbyGetMeRequest>,
    ) -> Result<Response<emby_proto::GetMeResponse>, Status> {
        self.provider_emby_get_me(request).await
    }

    async fn emby_logout(
        &self,
        request: Request<EmbyLogoutRequest>,
    ) -> Result<Response<emby_proto::LogoutResponse>, Status> {
        self.provider_emby_logout(request).await
    }

    async fn emby_get_binds(
        &self,
        request: Request<EmbyGetBindsRequest>,
    ) -> Result<Response<emby_proto::GetBindsResponse>, Status> {
        self.provider_emby_get_binds(request).await
    }

    async fn douyin_bind(
        &self,
        request: Request<DouyinBindRequest>,
    ) -> Result<Response<douyin_proto::BindResponse>, Status> {
        self.provider_douyin_bind(request).await
    }

    async fn douyin_get_binds(
        &self,
        request: Request<DouyinGetBindsRequest>,
    ) -> Result<Response<douyin_proto::GetBindsResponse>, Status> {
        self.provider_douyin_get_binds(request).await
    }

    async fn douyin_unbind(
        &self,
        request: Request<DouyinUnbindRequest>,
    ) -> Result<Response<douyin_proto::UnbindResponse>, Status> {
        self.provider_douyin_unbind(request).await
    }

    async fn douyin_resolve(
        &self,
        request: Request<DouyinResolveRequest>,
    ) -> Result<Response<douyin_proto::ResolveResponse>, Status> {
        self.provider_douyin_resolve(request).await
    }

    async fn douyin_list_user_posts(
        &self,
        request: Request<DouyinListUserPostsRequest>,
    ) -> Result<Response<douyin_proto::ListUserPostsResponse>, Status> {
        self.provider_douyin_list_user_posts(request).await
    }

    async fn tik_tok_bind(
        &self,
        request: Request<TikTokBindRequest>,
    ) -> Result<Response<tiktok_proto::BindResponse>, Status> {
        self.provider_tik_tok_bind(request).await
    }

    async fn tik_tok_get_binds(
        &self,
        request: Request<TikTokGetBindsRequest>,
    ) -> Result<Response<tiktok_proto::GetBindsResponse>, Status> {
        self.provider_tik_tok_get_binds(request).await
    }

    async fn tik_tok_unbind(
        &self,
        request: Request<TikTokUnbindRequest>,
    ) -> Result<Response<tiktok_proto::UnbindResponse>, Status> {
        self.provider_tik_tok_unbind(request).await
    }

    async fn tik_tok_resolve(
        &self,
        request: Request<TikTokResolveRequest>,
    ) -> Result<Response<tiktok_proto::ResolveResponse>, Status> {
        self.provider_tik_tok_resolve(request).await
    }

    async fn tik_tok_get_user(
        &self,
        request: Request<TikTokGetUserRequest>,
    ) -> Result<Response<tiktok_proto::GetUserResponse>, Status> {
        self.provider_tik_tok_get_user(request).await
    }

    async fn tik_tok_list_user_posts(
        &self,
        request: Request<TikTokListUserPostsRequest>,
    ) -> Result<Response<tiktok_proto::ListUserPostsResponse>, Status> {
        self.provider_tik_tok_list_user_posts(request).await
    }

    async fn twitch_bind(
        &self,
        request: Request<TwitchBindRequest>,
    ) -> Result<Response<twitch_proto::BindResponse>, Status> {
        self.provider_twitch_bind(request).await
    }

    async fn twitch_get_binds(
        &self,
        request: Request<TwitchGetBindsRequest>,
    ) -> Result<Response<twitch_proto::GetBindsResponse>, Status> {
        self.provider_twitch_get_binds(request).await
    }

    async fn twitch_unbind(
        &self,
        request: Request<TwitchUnbindRequest>,
    ) -> Result<Response<twitch_proto::UnbindResponse>, Status> {
        self.provider_twitch_unbind(request).await
    }

    async fn twitch_resolve(
        &self,
        request: Request<TwitchResolveRequest>,
    ) -> Result<Response<twitch_proto::ResolveResponse>, Status> {
        self.provider_twitch_resolve(request).await
    }

    async fn twitch_list_channel_items(
        &self,
        request: Request<TwitchListChannelItemsRequest>,
    ) -> Result<Response<twitch_proto::ListChannelItemsResponse>, Status> {
        self.provider_twitch_list_channel_items(request).await
    }

    async fn bilibili_parse(
        &self,
        request: Request<BilibiliParseRequest>,
    ) -> Result<Response<bilibili_proto::ParseResponse>, Status> {
        self.provider_bilibili_parse(request).await
    }

    async fn bilibili_login_qr(
        &self,
        request: Request<BilibiliLoginQrRequest>,
    ) -> Result<Response<bilibili_proto::QrCodeResponse>, Status> {
        self.provider_bilibili_login_qr(request).await
    }

    async fn bilibili_check_qr(
        &self,
        request: Request<BilibiliCheckQrRequest>,
    ) -> Result<Response<bilibili_proto::QrStatusResponse>, Status> {
        self.provider_bilibili_check_qr(request).await
    }

    async fn bilibili_start_sms_login(
        &self,
        request: Request<BilibiliStartSmsLoginRequest>,
    ) -> Result<Response<bilibili_proto::StartSmsLoginResponse>, Status> {
        self.provider_bilibili_start_sms_login(request).await
    }

    async fn bilibili_send_sms(
        &self,
        request: Request<BilibiliSendSmsRequest>,
    ) -> Result<Response<bilibili_proto::SendSmsResponse>, Status> {
        self.provider_bilibili_send_sms(request).await
    }

    async fn bilibili_login_sms(
        &self,
        request: Request<BilibiliLoginSmsRequest>,
    ) -> Result<Response<bilibili_proto::LoginSmsResponse>, Status> {
        self.provider_bilibili_login_sms(request).await
    }

    async fn bilibili_get_user_info(
        &self,
        request: Request<BilibiliGetUserInfoRequest>,
    ) -> Result<Response<bilibili_proto::UserInfoResponse>, Status> {
        self.provider_bilibili_get_user_info(request).await
    }

    async fn bilibili_logout(
        &self,
        request: Request<BilibiliLogoutRequest>,
    ) -> Result<Response<bilibili_proto::LogoutResponse>, Status> {
        self.provider_bilibili_logout(request).await
    }

    async fn bilibili_get_binds(
        &self,
        request: Request<BilibiliGetBindsRequest>,
    ) -> Result<Response<bilibili_proto::GetBindsResponse>, Status> {
        self.provider_bilibili_get_binds(request).await
    }

    async fn list_available_provider_instances(
        &self,
        request: Request<provider_common_proto::ListAvailableProviderInstancesRequest>,
    ) -> Result<Response<provider_common_proto::ProviderInstancesResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .provider_common_api
            .list_available_provider_instances(ListAvailableProviderInstancesQuery {
                provider_type: source_provider_from_proto_filter(req.provider_type)?,
            })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn list_provider_backends(
        &self,
        request: Request<provider_common_proto::ListProviderBackendsRequest>,
    ) -> Result<Response<provider_common_proto::ProviderBackendsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let provider_type = source_provider_from_proto_filter(req.provider_type)?
            .ok_or_else(|| Status::invalid_argument("provider_type is required"))?;
        let response = self
            .provider_common_api
            .list_provider_backends(ListProviderBackendsQuery { provider_type })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn list_provider_instances(
        &self,
        request: Request<provider_common_proto::ListProviderInstancesRequest>,
    ) -> Result<Response<provider_common_proto::ListProviderInstancesResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .provider_common_api
            .list_provider_instances(ProviderInstanceListQuery {
                pagination: PageParams::new(
                    defaultable_page_i32_to_u32(req.page),
                    defaultable_page_size_i32_to_u32(req.page_size, 100),
                ),
                provider_type: source_provider_from_proto_filter(req.provider_type)?,
                search: (!req.search.trim().is_empty()).then(|| req.search.trim().to_string()),
                enabled: req.enabled,
                tls: req.tls,
                sort_by: map_provider_instance_list_sort_by(req.sort_by)?,
                sort_direction: map_provider_instance_sort_direction(req.sort_direction)?,
            })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn add_provider_instance(
        &self,
        request: Request<provider_common_proto::AddProviderInstanceRequest>,
    ) -> Result<Response<provider_common_proto::AddProviderInstanceResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .provider_common_api
            .add_provider_instance(
                AddProviderInstanceCommand {
                    name: req.name,
                    endpoint: req.endpoint,
                    comment: req.comment,
                    timeout_seconds: req.timeout_seconds,
                    tls: req.tls,
                    insecure_tls: req.insecure_tls,
                    providers: req.providers,
                    jwt_secret: req.jwt_secret,
                    custom_ca: req.custom_ca,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn update_provider_instance(
        &self,
        request: Request<provider_common_proto::UpdateProviderInstanceRequest>,
    ) -> Result<Response<provider_common_proto::UpdateProviderInstanceResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .provider_common_api
            .update_provider_instance(
                UpdateProviderInstanceCommand {
                    name: req.name,
                    endpoint: req.endpoint,
                    comment: req.comment,
                    timeout_seconds: req.timeout_seconds,
                    tls: req.tls,
                    insecure_tls: req.insecure_tls,
                    providers: req.providers,
                    jwt_secret: req.jwt_secret,
                    custom_ca: req.custom_ca,
                    clear_comment: req.clear_comment,
                    clear_jwt_secret: req.clear_jwt_secret,
                    clear_custom_ca: req.clear_custom_ca,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn delete_provider_instance(
        &self,
        request: Request<provider_common_proto::DeleteProviderInstanceRequest>,
    ) -> Result<Response<provider_common_proto::DeleteProviderInstanceResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .provider_common_api
            .delete_provider_instance(
                ProviderInstanceNameCommand { name: req.name },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn reconnect_provider_instance(
        &self,
        request: Request<provider_common_proto::ReconnectProviderInstanceRequest>,
    ) -> Result<Response<provider_common_proto::ReconnectProviderInstanceResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .provider_common_api
            .reconnect_provider_instance(
                ProviderInstanceNameCommand { name: req.name },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn enable_provider_instance(
        &self,
        request: Request<provider_common_proto::EnableProviderInstanceRequest>,
    ) -> Result<Response<provider_common_proto::EnableProviderInstanceResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .provider_common_api
            .enable_provider_instance(ProviderInstanceNameCommand { name: req.name })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn disable_provider_instance(
        &self,
        request: Request<provider_common_proto::DisableProviderInstanceRequest>,
    ) -> Result<Response<provider_common_proto::DisableProviderInstanceResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .provider_common_api
            .disable_provider_instance(ProviderInstanceNameCommand { name: req.name })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn get_settings(
        &self,
        request: Request<GetSettingsRequest>,
    ) -> Result<Response<admin_proto::RuntimeSettings>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let response = self
            .admin_api
            .get_settings(GetSettingsQuery, &validated.user_id, &ctx)
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn update_settings(
        &self,
        request: Request<admin_proto::UpdateSettingsRequest>,
    ) -> Result<Response<admin_proto::RuntimeSettings>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let settings = req
            .settings
            .ok_or_else(|| Status::invalid_argument("settings is required"))?;
        let update_mask = req
            .update_mask
            .ok_or_else(|| Status::invalid_argument("update_mask is required"))?;
        let response = self
            .admin_api
            .update_settings(
                UpdateSettingsCommand {
                    settings,
                    update_mask,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn export_settings(
        &self,
        request: Request<admin_proto::ExportSettingsRequest>,
    ) -> Result<Response<admin_proto::RuntimeSettingsSnapshot>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let response = self
            .admin_api
            .export_settings(ExportSettingsQuery, &validated.user_id, &ctx)
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn import_settings(
        &self,
        request: Request<admin_proto::ImportSettingsRequest>,
    ) -> Result<Response<admin_proto::ImportSettingsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let snapshot = req
            .snapshot
            .ok_or_else(|| Status::invalid_argument("snapshot is required"))?;
        let response = self
            .admin_api
            .import_settings(
                ImportSettingsCommand {
                    snapshot,
                    dry_run: req.dry_run,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn send_test_email(
        &self,
        request: Request<SendTestEmailRequest>,
    ) -> Result<Response<admin_proto::SendTestEmailResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .send_test_email(SendTestEmailCommand { to: req.to })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn get_service_state(
        &self,
        request: Request<GetServiceStateRequest>,
    ) -> Result<Response<admin_proto::GetServiceStateResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let response = self
            .admin_api
            .get_service_state(GetServiceStateQuery)
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn get_server_state(
        &self,
        request: Request<GetServerStateRequest>,
    ) -> Result<Response<GetServerStateResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let target_node_id = synctv_core::service::validate_server_state_selection(
            Some(&req.node_id),
            req.all_nodes,
        )
        .map_err(|error| map_server_state_error(&error))?;
        Ok(Response::new(
            self.collect_server_state_response(target_node_id, req.all_nodes)
                .await?,
        ))
    }

    async fn get_slice_cache_stats(
        &self,
        request: Request<GetSliceCacheStatsRequest>,
    ) -> Result<Response<admin_proto::GetSliceCacheStatsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .slice_cache_runtime
            .get_stats(slice_cache_selection(req.node_id, req.all_nodes))
            .await
            .map_err(|error| map_slice_cache_error(&error))?;
        Ok(Response::new(get_slice_cache_stats_to_management(response)))
    }

    async fn purge_slice_cache(
        &self,
        request: Request<PurgeSliceCacheRequest>,
    ) -> Result<Response<admin_proto::PurgeSliceCacheResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .slice_cache_runtime
            .purge(slice_cache_selection(req.node_id, req.all_nodes))
            .await
            .map_err(|error| map_slice_cache_error(&error))?;
        Ok(Response::new(purge_slice_cache_to_management(response)))
    }

    async fn evict_expired_slice_cache(
        &self,
        request: Request<EvictExpiredSliceCacheRequest>,
    ) -> Result<Response<admin_proto::EvictExpiredSliceCacheResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .slice_cache_runtime
            .evict_expired(slice_cache_selection(req.node_id, req.all_nodes))
            .await
            .map_err(|error| map_slice_cache_error(&error))?;
        Ok(Response::new(evict_expired_slice_cache_to_management(
            response,
        )))
    }

    async fn list_active_streams(
        &self,
        request: Request<ListActiveStreamsRequest>,
    ) -> Result<Response<admin_proto::ListActiveStreamsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let user_id = self
            .resolve_optional_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .admin_api
            .list_active_streams(ListActiveStreamsQuery {
                page: req.page,
                page_size: req.page_size,
                room_id: req.room_id,
                user_id,
                node_id: req.node_id,
                search: req.search,
                sort_by: req.sort_by,
                sort_direction: req.sort_direction,
            })
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(response))
    }

    async fn kick_stream(
        &self,
        request: Request<KickStreamRequest>,
    ) -> Result<Response<admin_proto::KickStreamResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        self.admin_api
            .kick_stream(
                KickStreamCommand {
                    room_id: req.room_id,
                    media_id: req.media_id,
                    reason: req.reason,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(|error| map_api_error(&error))?;
        Ok(Response::new(admin_proto::KickStreamResponse {}))
    }

    async fn stop_server(
        &self,
        request: Request<StopServerRequest>,
    ) -> Result<Response<Self::StopServerStream>, Status> {
        self.check_admin_get_validated(&request)?;

        let request = request.into_inner();
        let requested_mode = parse_shutdown_mode(request.mode)?;

        let subscription = self.lifecycle_controller.subscribe();
        let requested_event = self.lifecycle_controller.request_shutdown(requested_mode);
        let events = stop_server_event_stream(
            subscription.snapshot,
            requested_event,
            subscription.receiver,
        );
        Ok(Response::new(Box::pin(events)))
    }
}

fn parse_shutdown_mode(mode: i32) -> Result<ShutdownMode, Status> {
    match ProtoShutdownMode::try_from(mode) {
        Ok(ProtoShutdownMode::Force) => Ok(ShutdownMode::Force),
        Ok(ProtoShutdownMode::Graceful | ProtoShutdownMode::Unspecified) => {
            Ok(ShutdownMode::Graceful)
        }
        Err(_) => Err(Status::invalid_argument(format!(
            "invalid shutdown mode: {mode}"
        ))),
    }
}

fn stop_server_event_stream(
    snapshot: LifecycleEvent,
    requested_event: LifecycleEvent,
    receiver: tokio::sync::broadcast::Receiver<LifecycleEvent>,
) -> impl Stream<Item = Result<StopServerEvent, Status>> + Send + 'static {
    futures::stream::unfold(
        (
            Some(snapshot),
            Some(requested_event),
            receiver,
            None::<u64>,
            false,
        ),
        |(snapshot, requested_event, mut receiver, last_sequence, done)| async move {
            if done {
                return None;
            }

            if let Some(snapshot) = snapshot {
                let (event, done) = stop_server_stream_event(&snapshot);
                return Some((
                    Ok(event),
                    (
                        None,
                        requested_event,
                        receiver,
                        Some(snapshot.sequence),
                        done,
                    ),
                ));
            }

            if let Some(requested_event) = requested_event {
                if last_sequence == Some(requested_event.sequence) {
                    // The broadcast receiver may observe the same shutdown-request event.
                    // Suppress the duplicate and continue with later lifecycle updates.
                } else {
                    let (event, done) = stop_server_stream_event(&requested_event);
                    return Some((
                        Ok(event),
                        (None, None, receiver, Some(requested_event.sequence), done),
                    ));
                }
            }

            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        if last_sequence == Some(event.sequence) {
                            continue;
                        }
                        let sequence = event.sequence;
                        let (event, done) = stop_server_stream_event(&event);
                        return Some((Ok(event), (None, None, receiver, Some(sequence), done)));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    )
}

fn stop_server_stream_event(event: &LifecycleEvent) -> (StopServerEvent, bool) {
    let terminal =
        event.terminal || matches!(event.stage, crate::lifecycle::LifecycleStage::Finalizing);
    let mut proto = event.to_proto();
    proto.terminal = terminal;
    (proto, terminal)
}

#[cfg(test)]
mod tests;
