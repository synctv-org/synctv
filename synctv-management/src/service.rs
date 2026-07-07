use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use tonic::{Request, Response, Status};

use crate::access::ManagementAccessController;
use crate::lifecycle::{LifecycleEvent, ManagementLifecycleController, ShutdownMode};
use crate::mapping::{
    map_client_sort_direction, map_management_user_lookup_error, map_room_list_sort_by,
    map_room_member_list_sort_by, map_room_status, map_room_stream_list_sort_by,
    map_sort_direction, map_user_list_sort_by, map_user_role, map_user_status,
    validate_client_actor_user,
};
use crate::proto::{
    management_service_server::ManagementService, AddAdminRequest, AddAlistMediaRequest,
    AddBilibiliLiveMediaRequest, AddBilibiliPgcMediaRequest, AddBilibiliVideoMediaRequest,
    AddDirectUrlMediaRequest, AddEmbyMediaRequest, AddMediaRequest, AddMemberRequest,
    AlistGetBindsRequest, AlistGetMeRequest, AlistListRequest, AlistLoginRequest,
    AlistLogoutRequest, AlistSearchRequest, ApproveRoomCreationReviewRequest,
    ApproveRoomJoinReviewRequest, ApproveUserRegistrationReviewRequest, BanRoomRequest,
    BanUserRequest, BatchBanRoomsRequest, BatchBanUsersRequest, BatchDeleteRoomsRequest,
    BatchDeleteUsersRequest, BilibiliCheckQrRequest, BilibiliGetBindsRequest,
    BilibiliGetUserInfoRequest, BilibiliLoginQrRequest, BilibiliLoginSmsRequest,
    BilibiliLogoutRequest, BilibiliParseRequest, BilibiliSendSmsRequest,
    BilibiliStartSmsLoginRequest, CreateAlistPlaylistRequest, CreateEmbyPlaylistRequest,
    CreatePlaylistRequest, CreatePublishKeyRequest, CreateRoomRequest, CreateUserRequest,
    DeleteMediaRequest, DeletePlaylistRequest, DeleteRoomRequest, DeleteUserRequest,
    EditMediaRequest, EmbyGetBindsRequest, EmbyGetMeRequest, EmbyListRequest, EmbyLoginRequest,
    EmbyLogoutRequest, EvictExpiredSliceCacheRequest, GetPlaybackRequest, GetPlaylistRequest,
    GetRoomMembersRequest, GetRoomRequest, GetRoomSettingsRequest, GetServerStateRequest,
    GetServerStateResponse, GetServiceStateRequest, GetSettingsRequest, GetSliceCacheStatsRequest,
    GetStreamInfoRequest, GetUserPreferencesRequest, GetUserRequest, GetUserRoomsRequest,
    KickMemberRequest, KickRoomStreamRequest, KickStreamRequest, ListActiveStreamsRequest,
    ListAdminsRequest, ListBanRecordsRequest, ListMediaRequest, ListPlaylistsRequest,
    ListRoomCreationReviewsRequest, ListRoomJoinReviewsRequest, ListRoomStreamsRequest,
    ListRoomsRequest, ListUserRegistrationReviewsRequest, ListUsersRequest, MoveMediaRequest,
    MovePlaylistRequest, PurgeSliceCacheRequest, RejectRoomCreationReviewRequest,
    RejectRoomJoinReviewRequest, RejectUserRegistrationReviewRequest, RemoveAdminRequest,
    ResetRoomSettingsRequest, SearchChatMessagesRequest, SendTestEmailRequest, ServerStateCluster,
    ServerStateClusterNode, ServerStateClusterStatus as ProtoClusterStatus, ServerStateCpu,
    ServerStateCpuStatus as ProtoCpuStatus, ServerStateDatabase, ServerStateDatabasePool,
    ServerStateDatabaseStatus as ProtoDatabaseStatus, ServerStateEmail,
    ServerStateEmailStatus as ProtoEmailStatus, ServerStateLivestream,
    ServerStateLivestreamStatus as ProtoLivestreamStatus, ServerStateMemory,
    ServerStateMemoryStatus as ProtoMemoryStatus, ServerStateNode, ServerStateNodeFailure,
    ServerStateNodeStatus as ProtoNodeStatus, ServerStateRealtime, ServerStateRedis,
    ServerStateRedisStatus as ProtoRedisStatus, ServerStateSliceCache,
    ServerStateSliceCacheStatus as ProtoSliceCacheStatus, ServerStateSummary, ServerStateWebRtc,
    ServerStateWebRtcStatus as ProtoWebRtcStatus, ServerStateWsTicket,
    ServerStateWsTicketStatus as ProtoWsTicketStatus, SetUserPasswordRequest,
    ShutdownMode as ProtoShutdownMode, StartPlaybackRequest, StopPlaybackRequest, StopServerEvent,
    StopServerRequest, TransferRoomOwnershipRequest, UnbanRoomRequest, UnbanUserRequest,
    UpdateMemberDisplayTagRequest, UpdateMemberPermissionsRequest, UpdateMemberRemarkNameRequest,
    UpdatePlaybackStateRequest, UpdatePlaylistRequest, UpdateRoomPasswordRequest,
    UpdateUserPreferencesRequest, UpdateUserRoleRequest, UpdateUserUsernameRequest, UserRef,
};
use crate::source_config::{
    alist_media_source_config, alist_playlist_source_config, bilibili_live_source_config,
    bilibili_pgc_source_config, bilibili_video_source_config, direct_url_source_config,
    emby_media_source_config, emby_playlist_source_config,
};
use synctv_api::map_api_error;
use synctv_api::PublicIdCodec;
use synctv_api::{
    AdminApiImpl, AdminRequestContext as RequestContext, AlistApiImpl, ApiError, BilibiliApiImpl,
    ClientApiImpl, EmbyApiImpl, ProviderCommonApiImpl, LOCAL_MANAGEMENT_ACTOR_USER_ID,
};
use synctv_core::models::{UserId, UserRole as CoreUserRole};
use synctv_core::service::UserService;
use synctv_core::Config;
use synctv_proto::{
    admin as admin_proto, client as client_proto, common as common_proto,
    providers::{
        alist as alist_proto, bilibili as bilibili_proto, common as provider_common_proto,
        emby as emby_proto, rtmp as rtmp_proto,
    },
};

struct ValidatedManagementUser {
    user_id: UserId,
    role: CoreUserRole,
}

struct BatchUserResolution {
    user_ids: Vec<String>,
    failures: Vec<admin_proto::BatchResultItem>,
}

#[derive(Clone)]
pub struct ManagementServiceImpl {
    config: Arc<Config>,
    user_service: Arc<UserService>,
    admin_api: Arc<AdminApiImpl>,
    provider_common_api: Arc<ProviderCommonApiImpl>,
    client_api: Arc<ClientApiImpl>,
    alist_api: Arc<AlistApiImpl>,
    bilibili_api: Arc<BilibiliApiImpl>,
    emby_api: Arc<EmbyApiImpl>,
    slice_cache_runtime: Arc<synctv_api::status::SliceCacheManagementRuntime>,
    server_state_runtime: Arc<synctv_api::status::ServerStateRuntime>,
    lifecycle_controller: Arc<ManagementLifecycleController>,
    access_controller: ManagementAccessController,
    public_id_codec: Arc<PublicIdCodec>,
}

pub struct ManagementServiceDependencies {
    pub config: Arc<Config>,
    pub user_service: Arc<UserService>,
    pub admin_api: Arc<AdminApiImpl>,
    pub provider_common_api: Arc<ProviderCommonApiImpl>,
    pub client_api: Arc<ClientApiImpl>,
    pub alist_api: Arc<AlistApiImpl>,
    pub bilibili_api: Arc<BilibiliApiImpl>,
    pub emby_api: Arc<EmbyApiImpl>,
    pub slice_cache_runtime: Arc<synctv_api::status::SliceCacheManagementRuntime>,
    pub server_state_runtime: Arc<synctv_api::status::ServerStateRuntime>,
    pub lifecycle_controller: Arc<ManagementLifecycleController>,
    pub management_auth_token: String,
}

impl ManagementServiceImpl {
    #[must_use]
    pub fn new(deps: ManagementServiceDependencies) -> Self {
        let ManagementServiceDependencies {
            config,
            user_service,
            admin_api,
            provider_common_api,
            client_api,
            alist_api,
            bilibili_api,
            emby_api,
            slice_cache_runtime,
            server_state_runtime,
            lifecycle_controller,
            management_auth_token,
        } = deps;

        let public_id_codec = admin_api.public_id_codec.clone();

        Self {
            config,
            user_service,
            admin_api,
            provider_common_api,
            client_api,
            alist_api,
            bilibili_api,
            emby_api,
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
            None => {
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

    async fn resolve_client_actor_user_id(&self, actor: Option<UserRef>) -> Result<UserId, Status> {
        // Management creation commands execute as this real client actor, not as
        // the authenticated admin/management process. The downstream client API
        // enforces room membership and permissions for room-scoped resources.
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

    async fn resolve_client_actor_and_request<T>(
        &self,
        actor: Option<UserRef>,
        request: Option<T>,
    ) -> Result<(UserId, T), Status> {
        let actor_user_id = self.resolve_client_actor_user_id(actor).await?;
        let request = Self::required_nested_request(request, "request")?;
        Ok((actor_user_id, request))
    }

    async fn resolve_batch_user_refs(&self, users: Vec<UserRef>) -> BatchUserResolution {
        let mut resolved = Vec::with_capacity(users.len());
        let mut failures = Vec::new();
        let mut seen = std::collections::HashSet::with_capacity(users.len());

        for user in users {
            match user.value {
                Some(crate::proto::user_ref::Value::Username(username)) => {
                    let username = username.trim();
                    if username.is_empty() {
                        failures.push(Self::batch_user_ref_failure(
                            "",
                            "username values must not be empty",
                        ));
                        continue;
                    }

                    match self.user_service.get_user_by_username(username).await {
                        Ok(user) => match self.public_id_codec.encode_user_id(user.id) {
                            Ok(user_id) => {
                                if seen.insert(user_id.clone()) {
                                    resolved.push(user_id);
                                }
                            }
                            Err(error) => {
                                failures.push(Self::batch_user_ref_failure(
                                    username,
                                    format!("Failed to encode resolved user id: {error}"),
                                ));
                            }
                        },
                        Err(synctv_core::Error::NotFound(_)) => {
                            failures.push(Self::batch_user_ref_failure(
                                username,
                                format!("User '{username}' was not found"),
                            ));
                        }
                        Err(error) => {
                            failures.push(Self::batch_user_ref_failure(
                                username,
                                format!("Failed to resolve user '{username}': {error}"),
                            ));
                        }
                    }
                }
                Some(crate::proto::user_ref::Value::UserId(user_id)) => {
                    let trimmed = user_id.trim();
                    if trimmed.is_empty() {
                        failures.push(Self::batch_user_ref_failure(
                            "",
                            "user_id values must not be empty",
                        ));
                        continue;
                    }
                    if let Err(error) = self.public_id_codec.decode_user_id(trimmed) {
                        failures.push(Self::batch_user_ref_failure(
                            trimmed,
                            format!("user_id is invalid: {error}"),
                        ));
                        continue;
                    }
                    if seen.insert(trimmed.to_string()) {
                        resolved.push(trimmed.to_string());
                    }
                }
                None => {
                    failures.push(Self::batch_user_ref_failure(
                        "",
                        "user ref must contain either user_id or username",
                    ));
                }
            }
        }

        BatchUserResolution {
            user_ids: resolved,
            failures,
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
        let ip_address = match synctv_api::extract_client_ip(request, &self.config) {
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

    async fn collect_server_state_response(
        &self,
        target_node_id: Option<String>,
        all_nodes: bool,
    ) -> Result<GetServerStateResponse, Status> {
        let response = self
            .server_state_runtime
            .collect_server_state(synctv_api::status::ServerStateSelection {
                node_id: target_node_id,
                all_nodes,
            })
            .await
            .map_err(|error| Self::map_server_state_error(&error))?;
        Ok(Self::server_state_to_management(response))
    }

    fn map_server_state_error(error: &synctv_api::status::ServerStateError) -> Status {
        match error {
            synctv_api::status::ServerStateError::InvalidSelection => {
                Status::invalid_argument(error.to_string())
            }
            synctv_api::status::ServerStateError::ClusterUnavailable(_)
            | synctv_api::status::ServerStateError::MissingClusterSecret
            | synctv_api::status::ServerStateError::InvalidClusterSecret => {
                Status::failed_precondition(error.to_string())
            }
            synctv_api::status::ServerStateError::Cluster(_)
            | synctv_api::status::ServerStateError::RemoteRequest { .. }
            | synctv_api::status::ServerStateError::RemoteDecode { .. } => {
                Status::unavailable(error.to_string())
            }
        }
    }

    fn server_state_to_management(
        response: synctv_api::status::ServerStateResponse,
    ) -> GetServerStateResponse {
        GetServerStateResponse {
            scope: response.scope.as_str().to_string(),
            summary: Some(ServerStateSummary {
                status: Self::node_status_to_management(response.summary.status),
                healthy_nodes: response.summary.healthy_nodes,
                degraded_nodes: response.summary.degraded_nodes,
                unhealthy_nodes: response.summary.unhealthy_nodes,
                failed_nodes: response.summary.failed_nodes,
            }),
            nodes: response
                .nodes
                .into_iter()
                .map(Self::server_state_node_to_management)
                .collect(),
            failures: response
                .failures
                .into_iter()
                .map(|failure| ServerStateNodeFailure {
                    node_id: failure.node_id,
                    error: failure.error,
                })
                .collect(),
        }
    }

    fn server_state_node_to_management(
        node: synctv_api::status::ServerStateNode,
    ) -> ServerStateNode {
        ServerStateNode {
            node_id: node.node_id,
            status: Self::node_status_to_management(node.status),
            updated_at: node.updated_at,
            version: node.version,
            api_address: node.api_address,
            realtime: Some(ServerStateRealtime {
                distributed_enabled: node.realtime.distributed_enabled,
                connection_count: node.realtime.connection_count,
            }),
            database: Some(ServerStateDatabase {
                status: Self::database_status_to_management(node.database.status),
                host: node.database.host,
                port: node.database.port,
                database: node.database.database,
                max_connections: node.database.max_connections,
                min_connections: node.database.min_connections,
                connect_timeout_seconds: node.database.connect_timeout_seconds,
                idle_timeout_seconds: node.database.idle_timeout_seconds,
                max_lifetime_seconds: node.database.max_lifetime_seconds,
                primary_pool: Some(Self::server_state_database_pool_to_management(
                    &node.database.primary_pool,
                )),
                read_pool_enabled: node.database.read_pool_enabled,
                read_host: node.database.read_host,
                read_port: node.database.read_port,
                read_pool: Some(Self::server_state_database_pool_to_management(
                    &node.database.read_pool,
                )),
                message: node.database.message.unwrap_or_default(),
            }),
            redis: Some(ServerStateRedis {
                status: Self::redis_status_to_management(node.redis.status),
                configured: node.redis.configured,
                deployment_mode: node.redis.deployment_mode,
                database: node.redis.database,
                key_prefix: node.redis.key_prefix,
                connect_timeout_seconds: node.redis.connect_timeout_seconds,
                response_timeout_seconds: node.redis.response_timeout_seconds,
                pipeline_buffer_size: node.redis.pipeline_buffer_size,
                sentinel_master_name: node.redis.sentinel_master_name,
                sentinel_node_count: node.redis.sentinel_node_count,
                ping_latency_ms: node.redis.ping_latency_ms,
                message: node.redis.message.unwrap_or_default(),
            }),
            cluster: Some(ServerStateCluster {
                status: Self::cluster_status_to_management(node.cluster.status),
                enabled: node.cluster.enabled,
                discovery_mode: node.cluster.discovery_mode,
                distributed_realtime_enabled: node.cluster.distributed_realtime_enabled,
                node_id_empty: node.cluster.node_id_empty,
                routable_node_count: node.cluster.routable_node_count,
                nodes: node
                    .cluster
                    .nodes
                    .into_iter()
                    .map(|cluster_node| ServerStateClusterNode {
                        node_id: cluster_node.node_id,
                        api_address: cluster_node.api_address,
                        last_heartbeat: cluster_node.last_heartbeat,
                        epoch: cluster_node.epoch,
                    })
                    .collect(),
                message: node.cluster.message.unwrap_or_default(),
            }),
            ws_ticket: Some(ServerStateWsTicket {
                status: Self::ws_ticket_status_to_management(node.ws_ticket.status),
                cross_node_capable: node.ws_ticket.cross_node_capable,
                message: node.ws_ticket.message.unwrap_or_default(),
            }),
            email: Some(ServerStateEmail {
                status: Self::email_status_to_management(node.email.status),
                configured: node.email.configured,
            }),
            livestream: Some(ServerStateLivestream {
                status: Self::livestream_status_to_management(node.livestream.status),
                configured: node.livestream.configured,
                active_publisher_count: node.livestream.active_publisher_count,
                active_room_count: node.livestream.active_room_count,
                rtmp_port: node.livestream.rtmp_port,
                public_rtmp_host: node.livestream.public_rtmp_host,
                gop_cache_size: node.livestream.gop_cache_size,
                gop_cache_max_memory_mb: node.livestream.gop_cache_max_memory_mb,
                stream_timeout_seconds: node.livestream.stream_timeout_seconds,
                hls_storage_backend: node.livestream.hls_storage_backend,
                hls_storage_path: node.livestream.hls_storage_path,
                hls_memory_max_mb: node.livestream.hls_memory_max_mb,
            }),
            memory: Some(ServerStateMemory {
                status: Self::memory_status_to_management(node.memory.status),
                used_bytes: node.memory.used_bytes,
                total_bytes: node.memory.total_bytes,
                available_bytes: node.memory.available_bytes,
                usage_percent: node.memory.usage_percent,
            }),
            webrtc: Some(ServerStateWebRtc {
                status: Self::webrtc_status_to_management(node.webrtc.status),
                mode: node.webrtc.mode,
                builtin_stun_configured: node.webrtc.builtin_stun_configured,
                builtin_stun_state: node.webrtc.builtin_stun_state,
                reason: node.webrtc.reason,
                local_addr: node.webrtc.local_addr,
                external_addr: node.webrtc.external_addr,
                message: node.webrtc.message.unwrap_or_default(),
            }),
            cpu: Some(ServerStateCpu {
                status: Self::cpu_status_to_management(node.cpu.status),
                available_parallelism: node.cpu.available_parallelism,
                current_load_1m: node.cpu.current_load_1m,
                load_ratio_1m: node.cpu.load_ratio_1m,
                load_average_1m: node.cpu.load_average_1m,
                load_average_5m: node.cpu.load_average_5m,
                load_average_15m: node.cpu.load_average_15m,
            }),
            slice_cache: Some(ServerStateSliceCache {
                status: Self::slice_cache_status_to_management(node.slice_cache.status),
                engine_enabled: node.slice_cache.engine_enabled,
                backend: node.slice_cache.backend,
                file_cache_dir: node.slice_cache.file_cache_dir,
                slice_size: node.slice_cache.slice_size,
                max_cache_size: node.slice_cache.max_cache_size,
                segment_ttl_secs: node.slice_cache.segment_ttl_secs,
                stale_max_age_secs: node.slice_cache.stale_max_age_secs,
                stale_while_revalidate: node.slice_cache.stale_while_revalidate,
                eviction_interval_secs: node.slice_cache.eviction_interval_secs,
                watermark_ratio: node.slice_cache.watermark_ratio,
                current_size_bytes: node.slice_cache.current_size_bytes,
                entry_count: node.slice_cache.entry_count,
                metadata_entries: node.slice_cache.metadata_entries,
                updating_entries: node.slice_cache.updating_entries,
                lock_count: node.slice_cache.lock_count,
                usage_ratio: node.slice_cache.usage_ratio,
            }),
        }
    }

    fn server_state_database_pool_to_management(
        pool: &synctv_api::status::DatabasePoolStatus,
    ) -> ServerStateDatabasePool {
        ServerStateDatabasePool {
            size: pool.size,
            idle_connections: pool.idle_connections,
            active_connections: pool.active_connections,
        }
    }

    fn node_status_to_management(status: synctv_api::status::ServerStateNodeStatus) -> i32 {
        match status {
            synctv_api::status::ServerStateNodeStatus::Healthy => ProtoNodeStatus::Healthy,
            synctv_api::status::ServerStateNodeStatus::Degraded => ProtoNodeStatus::Degraded,
            synctv_api::status::ServerStateNodeStatus::Unhealthy => ProtoNodeStatus::Unhealthy,
        }
        .into()
    }

    fn database_status_to_management(status: synctv_api::status::ServerStateDatabaseStatus) -> i32 {
        match status {
            synctv_api::status::ServerStateDatabaseStatus::Healthy => ProtoDatabaseStatus::Healthy,
            synctv_api::status::ServerStateDatabaseStatus::Unhealthy => {
                ProtoDatabaseStatus::Unhealthy
            }
        }
        .into()
    }

    fn redis_status_to_management(status: synctv_api::status::ServerStateRedisStatus) -> i32 {
        match status {
            synctv_api::status::ServerStateRedisStatus::Healthy => ProtoRedisStatus::Healthy,
            synctv_api::status::ServerStateRedisStatus::NotConfigured => {
                ProtoRedisStatus::NotConfigured
            }
            synctv_api::status::ServerStateRedisStatus::Unhealthy => ProtoRedisStatus::Unhealthy,
        }
        .into()
    }

    fn cluster_status_to_management(status: synctv_api::status::ServerStateClusterStatus) -> i32 {
        match status {
            synctv_api::status::ServerStateClusterStatus::Healthy => ProtoClusterStatus::Healthy,
            synctv_api::status::ServerStateClusterStatus::Unhealthy => {
                ProtoClusterStatus::Unhealthy
            }
            synctv_api::status::ServerStateClusterStatus::Disabled => ProtoClusterStatus::Disabled,
        }
        .into()
    }

    fn ws_ticket_status_to_management(
        status: synctv_api::status::ServerStateWsTicketStatus,
    ) -> i32 {
        match status {
            synctv_api::status::ServerStateWsTicketStatus::Healthy => ProtoWsTicketStatus::Healthy,
            synctv_api::status::ServerStateWsTicketStatus::Unhealthy => {
                ProtoWsTicketStatus::Unhealthy
            }
        }
        .into()
    }

    fn email_status_to_management(status: synctv_api::status::ServerStateEmailStatus) -> i32 {
        match status {
            synctv_api::status::ServerStateEmailStatus::Configured => ProtoEmailStatus::Configured,
            synctv_api::status::ServerStateEmailStatus::NotConfigured => {
                ProtoEmailStatus::NotConfigured
            }
        }
        .into()
    }

    fn livestream_status_to_management(
        status: synctv_api::status::ServerStateLivestreamStatus,
    ) -> i32 {
        match status {
            synctv_api::status::ServerStateLivestreamStatus::Configured => {
                ProtoLivestreamStatus::Configured
            }
            synctv_api::status::ServerStateLivestreamStatus::NotConfigured => {
                ProtoLivestreamStatus::NotConfigured
            }
        }
        .into()
    }

    fn memory_status_to_management(status: synctv_api::status::ServerStateMemoryStatus) -> i32 {
        match status {
            synctv_api::status::ServerStateMemoryStatus::Healthy => ProtoMemoryStatus::Healthy,
            synctv_api::status::ServerStateMemoryStatus::Unhealthy => ProtoMemoryStatus::Unhealthy,
            synctv_api::status::ServerStateMemoryStatus::Unknown => ProtoMemoryStatus::Unknown,
        }
        .into()
    }

    fn webrtc_status_to_management(status: synctv_api::status::ServerStateWebRtcStatus) -> i32 {
        match status {
            synctv_api::status::ServerStateWebRtcStatus::Healthy => ProtoWebRtcStatus::Healthy,
            synctv_api::status::ServerStateWebRtcStatus::Degraded => ProtoWebRtcStatus::Degraded,
            synctv_api::status::ServerStateWebRtcStatus::Disabled => ProtoWebRtcStatus::Disabled,
        }
        .into()
    }

    fn cpu_status_to_management(status: synctv_api::status::ServerStateCpuStatus) -> i32 {
        match status {
            synctv_api::status::ServerStateCpuStatus::Healthy => ProtoCpuStatus::Healthy,
            synctv_api::status::ServerStateCpuStatus::Degraded => ProtoCpuStatus::Degraded,
            synctv_api::status::ServerStateCpuStatus::Unhealthy => ProtoCpuStatus::Unhealthy,
            synctv_api::status::ServerStateCpuStatus::Unknown => ProtoCpuStatus::Unknown,
        }
        .into()
    }

    fn slice_cache_status_to_management(
        status: synctv_api::status::ServerStateSliceCacheStatus,
    ) -> i32 {
        match status {
            synctv_api::status::ServerStateSliceCacheStatus::Healthy => {
                ProtoSliceCacheStatus::Healthy
            }
            synctv_api::status::ServerStateSliceCacheStatus::Disabled => {
                ProtoSliceCacheStatus::Disabled
            }
        }
        .into()
    }

    fn map_slice_cache_error(error: &synctv_api::status::SliceCacheManagementError) -> Status {
        match error {
            synctv_api::status::SliceCacheManagementError::InvalidSelection => {
                Status::invalid_argument(error.to_string())
            }
            synctv_api::status::SliceCacheManagementError::ClusterUnavailable(_)
            | synctv_api::status::SliceCacheManagementError::MissingClusterSecret
            | synctv_api::status::SliceCacheManagementError::InvalidClusterSecret => {
                Status::failed_precondition(error.to_string())
            }
            synctv_api::status::SliceCacheManagementError::Cluster(_)
            | synctv_api::status::SliceCacheManagementError::RemoteRequest { .. } => {
                Status::unavailable(error.to_string())
            }
        }
    }

    fn slice_cache_selection(
        node_id: String,
        all_nodes: bool,
    ) -> synctv_api::status::SliceCacheSelection {
        synctv_api::status::SliceCacheSelection {
            node_id: (!node_id.trim().is_empty()).then_some(node_id),
            all_nodes,
        }
    }

    fn slice_cache_config_to_management(
        config: synctv_api::status::SliceCacheConfigInfo,
    ) -> admin_proto::SliceCacheConfigInfo {
        admin_proto::SliceCacheConfigInfo {
            engine_enabled: config.engine_enabled,
            backend: config.backend,
            file_cache_dir: config.file_cache_dir,
            slice_size: config.slice_size,
            max_cache_size: config.max_cache_size,
            segment_ttl_secs: config.segment_ttl_secs,
            stale_max_age_secs: config.stale_max_age_secs,
            stale_while_revalidate: config.stale_while_revalidate,
            eviction_interval_secs: config.eviction_interval_secs,
            watermark_ratio: config.watermark_ratio,
        }
    }

    fn slice_cache_stats_node_to_management(
        stats: synctv_api::status::SliceCacheStatsNode,
    ) -> admin_proto::SliceCacheStatsNode {
        admin_proto::SliceCacheStatsNode {
            node_id: stats.node_id,
            config: Some(Self::slice_cache_config_to_management(stats.config)),
            current_size_bytes: stats.current_size_bytes,
            entry_count: stats.entry_count,
            metadata_entries: stats.metadata_entries,
            updating_entries: stats.updating_entries,
            lock_count: stats.lock_count,
            usage_ratio: stats.usage_ratio,
        }
    }

    fn slice_cache_failure_to_management(
        failure: synctv_api::status::SliceCacheNodeFailure,
    ) -> admin_proto::SliceCacheNodeFailure {
        admin_proto::SliceCacheNodeFailure {
            node_id: failure.node_id,
            error: failure.error,
        }
    }

    fn get_slice_cache_stats_to_management(
        response: synctv_api::status::SliceCacheStatsResponse,
    ) -> admin_proto::GetSliceCacheStatsResponse {
        admin_proto::GetSliceCacheStatsResponse {
            nodes: response
                .nodes
                .into_iter()
                .map(Self::slice_cache_stats_node_to_management)
                .collect(),
            failures: response
                .failures
                .into_iter()
                .map(Self::slice_cache_failure_to_management)
                .collect(),
        }
    }

    fn purge_slice_cache_node_to_management(
        response: synctv_api::status::SliceCachePurgeNodeResult,
    ) -> admin_proto::PurgeSliceCacheNodeResult {
        admin_proto::PurgeSliceCacheNodeResult {
            node_id: response.node_id,
            success: response.success,
            removed_entries: response.removed_entries,
            freed_bytes: response.freed_bytes,
            stats: response
                .stats
                .map(Self::slice_cache_stats_node_to_management),
        }
    }

    fn purge_slice_cache_to_management(
        response: synctv_api::status::SliceCachePurgeResponse,
    ) -> admin_proto::PurgeSliceCacheResponse {
        admin_proto::PurgeSliceCacheResponse {
            success: response.success,
            removed_entries: response.removed_entries,
            freed_bytes: response.freed_bytes,
            stats: response
                .stats
                .map(Self::slice_cache_stats_node_to_management),
            nodes: response
                .nodes
                .into_iter()
                .map(Self::purge_slice_cache_node_to_management)
                .collect(),
            failures: response
                .failures
                .into_iter()
                .map(Self::slice_cache_failure_to_management)
                .collect(),
        }
    }

    fn evict_expired_slice_cache_node_to_management(
        response: synctv_api::status::SliceCacheEvictExpiredNodeResult,
    ) -> admin_proto::EvictExpiredSliceCacheNodeResult {
        admin_proto::EvictExpiredSliceCacheNodeResult {
            node_id: response.node_id,
            success: response.success,
            removed_expired_entries: response.removed_expired_entries,
            stats: response
                .stats
                .map(Self::slice_cache_stats_node_to_management),
        }
    }

    fn evict_expired_slice_cache_to_management(
        response: synctv_api::status::SliceCacheEvictExpiredResponse,
    ) -> admin_proto::EvictExpiredSliceCacheResponse {
        admin_proto::EvictExpiredSliceCacheResponse {
            success: response.success,
            removed_expired_entries: response.removed_expired_entries,
            stats: response
                .stats
                .map(Self::slice_cache_stats_node_to_management),
            nodes: response
                .nodes
                .into_iter()
                .map(Self::evict_expired_slice_cache_node_to_management)
                .collect(),
            failures: response
                .failures
                .into_iter()
                .map(Self::slice_cache_failure_to_management)
                .collect(),
        }
    }

    fn map_api_result<T>(result: Result<T, ApiError>) -> Result<T, Status> {
        result.map_err(map_api_error)
    }

    fn map_into_api_result<T, E>(result: Result<T, E>) -> Result<T, Status>
    where
        ApiError: From<E>,
    {
        result.map_err(map_api_error)
    }
}

#[tonic::async_trait]
impl ManagementService for ManagementServiceImpl {
    type StopServerStream =
        Pin<Box<dyn Stream<Item = Result<StopServerEvent, Status>> + Send + 'static>>;

    async fn list_users(
        &self,
        request: Request<ListUsersRequest>,
    ) -> Result<Response<admin_proto::ListUsersResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .list_users(admin_proto::ListUsersRequest {
                page: req.page,
                page_size: req.page_size,
                status: map_user_status(req.status)?,
                role: map_user_role(req.role)?,
                search: req.search,
                sort_by: map_user_list_sort_by(req.sort_by)?,
                is_banned: req.is_banned,
                sort_direction: map_sort_direction(
                    req.sort_direction,
                    admin_proto::SortDirection::Desc,
                )?,
            })
            .await
            .map_err(map_api_error)?;
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
            .get_user(admin_proto::GetUserRequest { user_id })
            .await
            .map_err(map_api_error)?;
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
            .get_user_preferences(admin_proto::GetUserPreferencesRequest { user_id })
            .await
            .map_err(map_api_error)?;
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
                admin_proto::UpdateUserPreferencesRequest {
                    user_id,
                    two_factor_enabled: req.two_factor_enabled,
                    notifications: req.notifications,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
            .add_admin(
                admin_proto::AddAdminRequest { user_id },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
            .remove_admin(
                admin_proto::RemoveAdminRequest { user_id },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
            .list_admins(admin_proto::ListAdminsRequest {
                page: req.page,
                page_size: req.page_size,
                search: req.search,
                sort_by: req.sort_by,
                sort_direction: req.sort_direction,
            })
            .await
            .map_err(map_api_error)?;
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
                admin_proto::CreateUserRequest {
                    username: req.username,
                    email: req.email,
                    role: map_user_role(req.role)?,
                    status: map_user_status(req.status)?,
                    password: req.password,
                },
                validated.role,
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
            .delete_user(
                admin_proto::DeleteUserRequest { user_id },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::BanUserRequest {
                    user_id,
                    reason: req.reason,
                },
                &validated.user_id,
                validated.role,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
            .unban_user(
                admin_proto::UnbanUserRequest { user_id },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::ListUserRegistrationReviewsRequest {
                    page: req.page,
                    page_size: req.page_size,
                    status: req.status,
                    search: req.search,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::ApproveUserRegistrationReviewRequest {
                    request_id: req.request_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::RejectUserRegistrationReviewRequest {
                    request_id: req.request_id,
                    reason: req.reason,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::ListRoomCreationReviewsRequest {
                    page: req.page,
                    page_size: req.page_size,
                    status: req.status,
                    requested_by: req.requested_by,
                    search: req.search,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::ApproveRoomCreationReviewRequest {
                    request_id: req.request_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::RejectRoomCreationReviewRequest {
                    request_id: req.request_id,
                    reason: req.reason,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::ListRoomJoinReviewsRequest {
                    page: req.page,
                    page_size: req.page_size,
                    status: req.status,
                    room_id: req.room_id,
                    user_id: req.user_id,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::ApproveRoomJoinReviewRequest {
                    request_id: req.request_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::RejectRoomJoinReviewRequest {
                    request_id: req.request_id,
                    reason: req.reason,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::ListBanRecordsRequest {
                    page: req.page,
                    page_size: req.page_size,
                    target_type: req.target_type,
                    active: req.active,
                    user_id: req.user_id,
                    room_id: req.room_id,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::UpdateUserRoleRequest {
                    user_id,
                    role: map_user_role(req.role)?,
                },
                &validated.user_id,
                validated.role,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::SetUserPasswordRequest {
                    user_id,
                    password: req.password,
                    reason: req.reason,
                },
                validated.user_id,
                validated.role,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::UpdateUserUsernameRequest {
                    user_id,
                    new_username: req.new_username,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
            .get_user_rooms(admin_proto::GetUserRoomsRequest {
                user_id,
                page: req.page,
                page_size: req.page_size,
                status: map_room_status(req.status)?,
                search: req.search,
                is_banned: req.is_banned,
                sort_by: map_room_list_sort_by(req.sort_by)?,
                sort_direction: map_sort_direction(
                    req.sort_direction,
                    admin_proto::SortDirection::Desc,
                )?,
            })
            .await
            .map_err(map_api_error)?;
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
                    admin_proto::BatchBanUsersRequest {
                        user_ids: resolved.user_ids,
                        reason: req.reason,
                    },
                    &validated.user_id,
                    validated.role,
                    &ctx,
                )
                .await
                .map_err(map_api_error)?;
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
                    admin_proto::BatchDeleteUsersRequest {
                        user_ids: resolved.user_ids,
                    },
                    &validated.user_id,
                    validated.role,
                    &ctx,
                )
                .await
                .map_err(map_api_error)?;
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
        let response = Box::pin(self.client_api.create_room(
            &actor_user_id,
            client_proto::CreateRoomRequest {
                name: req.name,
                settings: req.settings,
                description: req.description,
                password: req.password,
                category_id: req.category_id,
                label_ids: req.label_ids,
            },
        ))
        .await
        .map_err(map_api_error)?;
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
            .list_rooms(admin_proto::ListRoomsRequest {
                page: req.page,
                page_size: req.page_size,
                status: map_room_status(req.status)?,
                search: req.search,
                creator_id,
                is_banned: req.is_banned,
                sort_by: map_room_list_sort_by(req.sort_by)?,
                sort_direction: map_sort_direction(
                    req.sort_direction,
                    admin_proto::SortDirection::Desc,
                )?,
                category_id: req.category_id,
                label_ids: req.label_ids,
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_room_categories(
        &self,
        request: Request<admin_proto::ListRoomCategoriesRequest>,
    ) -> Result<Response<admin_proto::ListRoomCategoriesResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        self.admin_api
            .list_room_categories(request.into_inner())
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn upsert_room_category(
        &self,
        request: Request<admin_proto::UpsertRoomCategoryRequest>,
    ) -> Result<Response<client_proto::RoomCategory>, Status> {
        self.check_admin_get_validated(&request)?;
        self.admin_api
            .upsert_room_category(request.into_inner())
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn delete_room_category(
        &self,
        request: Request<admin_proto::DeleteRoomCategoryRequest>,
    ) -> Result<Response<admin_proto::DeleteRoomCategoryResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        self.admin_api
            .delete_room_category(request.into_inner())
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn list_room_labels(
        &self,
        request: Request<admin_proto::ListRoomLabelsRequest>,
    ) -> Result<Response<admin_proto::ListRoomLabelsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        self.admin_api
            .list_room_labels(request.into_inner())
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn upsert_room_label(
        &self,
        request: Request<admin_proto::UpsertRoomLabelRequest>,
    ) -> Result<Response<client_proto::RoomLabel>, Status> {
        self.check_admin_get_validated(&request)?;
        self.admin_api
            .upsert_room_label(request.into_inner())
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn delete_room_label(
        &self,
        request: Request<admin_proto::DeleteRoomLabelRequest>,
    ) -> Result<Response<admin_proto::DeleteRoomLabelResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        self.admin_api
            .delete_room_label(request.into_inner())
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn update_room_taxonomy(
        &self,
        request: Request<admin_proto::UpdateRoomTaxonomyRequest>,
    ) -> Result<Response<admin_proto::Room>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        self.admin_api
            .update_room_taxonomy(request.into_inner(), &validated.user_id)
            .await
            .map(Response::new)
            .map_err(map_api_error)
    }

    async fn get_room(
        &self,
        request: Request<GetRoomRequest>,
    ) -> Result<Response<admin_proto::Room>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_room(admin_proto::GetRoomRequest {
                room_id: req.room_id,
            })
            .await
            .map_err(map_api_error)?;
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
            .get_room_members(admin_proto::GetRoomMembersRequest {
                room_id: req.room_id,
                page: req.page,
                page_size: req.page_size,
                search: req.search,
                role: req.role,
                sort_by: map_room_member_list_sort_by(req.sort_by)?,
                sort_direction: map_sort_direction(
                    req.sort_direction,
                    admin_proto::SortDirection::Asc,
                )?,
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn search_chat_messages(
        &self,
        request: Request<SearchChatMessagesRequest>,
    ) -> Result<Response<client_proto::SearchChatMessagesResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let actor = self
            .client_api
            .room_actor_for_user(&actor_user_id, &req.room_id)
            .await
            .map_err(map_api_error)?;
        let user_id = self
            .resolve_optional_user_selector(&req.user_id, &req.username, "user")
            .await?;
        let response = self
            .client_api
            .search_chat_messages_for_actor(
                &actor,
                client_proto::SearchChatMessagesRequest {
                    query: req.query,
                    cursor: req.cursor,
                    limit: req.limit,
                    include_deleted: req.include_deleted,
                    user_id,
                },
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::AddMemberRequest {
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
            .map_err(map_api_error)?;
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
                admin_proto::UpdateMemberRemarkNameRequest {
                    room_id: req.room_id,
                    user_id,
                    remark_name: req.remark_name,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::UpdateMemberDisplayTagRequest {
                    room_id: req.room_id,
                    user_id,
                    display_tag: req.display_tag,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::UpdateMemberPermissionsRequest {
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
            .map_err(map_api_error)?;
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
                admin_proto::KickMemberRequest {
                    room_id: req.room_id,
                    user_id,
                    kick_cooldown_seconds: req.kick_cooldown_seconds,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
            .get_room_settings(admin_proto::GetRoomSettingsRequest {
                room_id: req.room_id,
            })
            .await
            .map_err(map_api_error)?;
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
            .update_room_settings(req, &validated.user_id)
            .await
            .map_err(map_api_error)?;
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
                admin_proto::ResetRoomSettingsRequest {
                    room_id: req.room_id,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
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
            .client_api
            .transfer_room_ownership(
                &actor_user_id,
                &req.room_id,
                client_proto::TransferRoomOwnershipRequest { new_owner_user_id },
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::UpdateRoomPasswordRequest {
                    room_id: req.room_id,
                    new_password,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::BanRoomRequest {
                    room_id: req.room_id,
                    reason: req.reason,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::UnbanRoomRequest {
                    room_id: req.room_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::DeleteRoomRequest {
                    room_id: req.room_id,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::BatchBanRoomsRequest {
                    room_ids: req.room_ids,
                    reason: req.reason,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::BatchDeleteRoomsRequest {
                    room_ids: req.room_ids,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_playback(
        &self,
        request: Request<StartPlaybackRequest>,
    ) -> Result<Response<client_proto::StartPlaybackResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .start_playback(
                &req.room_id,
                client_proto::StartPlaybackRequest {
                    media_id: req.media_id,
                    playlist_id: req.playlist_id,
                    target: req.target,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn stop_playback(
        &self,
        request: Request<StopPlaybackRequest>,
    ) -> Result<Response<client_proto::StopPlaybackResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .stop_playback(&req.room_id, &validated.user_id, &ctx)
            .await
            .map_err(map_api_error)?;
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
            .map_err(map_api_error)?;
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
                &req.room_id,
                client_proto::UpdatePlaybackStateRequest {
                    r#type: req.r#type,
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
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn create_publish_key(
        &self,
        request: Request<CreatePublishKeyRequest>,
    ) -> Result<Response<rtmp_proto::CreatePublishKeyResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let response = self
            .admin_api
            .create_publish_key_for_actor(
                &req.room_id,
                &req.media_id,
                &actor_user_id,
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_stream_info(
        &self,
        request: Request<GetStreamInfoRequest>,
    ) -> Result<Response<rtmp_proto::GetStreamInfoResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_stream_info(&req.room_id, &req.media_id)
            .await
            .map_err(map_api_error)?;
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
            .list_room_streams(
                &req.room_id,
                client_proto::ListRoomStreamsRequest {
                    page: req.page,
                    page_size: req.page_size,
                    search: req.search,
                    sort_by: map_room_stream_list_sort_by(req.sort_by)?,
                    sort_direction: map_client_sort_direction(
                        req.sort_direction,
                        client_proto::SortDirection::Unspecified,
                    )?,
                },
            )
            .await
            .map_err(map_api_error)?;
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
                admin_proto::KickStreamRequest {
                    room_id: req.room_id,
                    media_id: req.media_id,
                    reason: req.reason,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
                &req.room_id,
                client_proto::ListPlaylistsRequest {
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
            .map_err(map_api_error)?;
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
            .map_err(map_api_error)?;
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
            .client_api
            .create_playlist(
                &actor_user_id,
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
            .await
            .map_err(map_api_error)?;
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
            .client_api
            .create_playlist(
                &actor_user_id,
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
            .await
            .map_err(map_api_error)?;
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
            .client_api
            .create_playlist(
                &actor_user_id,
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
            .await
            .map_err(map_api_error)?;
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
                &req.room_id,
                client_proto::UpdatePlaylistRequest {
                    playlist_id: req.playlist_id,
                    name,
                    description: String::new(),
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
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
                &req.room_id,
                client_proto::MovePlaylistRequest {
                    playlist_id: req.playlist_id,
                    anchor: req.anchor.map(|anchor| match anchor {
                        crate::proto::move_playlist_request::Anchor::BeforePlaylistId(id) => {
                            client_proto::move_playlist_request::Anchor::BeforePlaylistId(id)
                        }
                        crate::proto::move_playlist_request::Anchor::AfterPlaylistId(id) => {
                            client_proto::move_playlist_request::Anchor::AfterPlaylistId(id)
                        }
                    }),
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
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
                &req.room_id,
                client_proto::DeletePlaylistRequest {
                    playlist_id: req.playlist_id,
                    force: req.force,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
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
                &req.room_id,
                client_proto::ListPlaylistItemsRequest {
                    playlist_id: req.playlist_id,
                    target: req.target,
                    page: req.page,
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
            .map_err(map_api_error)?;
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
            .client_api
            .add_media(
                &actor_user_id,
                &req.room_id,
                client_proto::AddMediaRequest {
                    playlist_id: (!req.playlist_id.is_empty()).then_some(req.playlist_id),
                    description: String::new(),
                    provider_instance_name: req.provider_instance_name,
                    source_config: req.source_config,
                    name: req.name,
                },
            )
            .await
            .map_err(map_api_error)?;
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
            .client_api
            .add_media(
                &actor_user_id,
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
            .await
            .map_err(map_api_error)?;
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
            .client_api
            .add_media(
                &actor_user_id,
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
            .await
            .map_err(map_api_error)?;
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
            .client_api
            .add_media(
                &actor_user_id,
                &req.room_id,
                client_proto::AddMediaRequest {
                    playlist_id: (!req.playlist_id.is_empty()).then_some(req.playlist_id),
                    description: String::new(),
                    provider_instance_name: req.provider_instance_name,
                    source_config: Some(emby_media_source_config(&req.server_id, &req.item_id)?),
                    name: req.name,
                },
            )
            .await
            .map_err(map_api_error)?;
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
            .client_api
            .add_media(
                &actor_user_id,
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
            .await
            .map_err(map_api_error)?;
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
            .client_api
            .add_media(
                &actor_user_id,
                &req.room_id,
                client_proto::AddMediaRequest {
                    playlist_id: (!req.playlist_id.is_empty()).then_some(req.playlist_id),
                    description: String::new(),
                    provider_instance_name: req.provider_instance_name,
                    source_config: Some(bilibili_pgc_source_config(req.epid, req.cid, req.shared)?),
                    name: req.name,
                },
            )
            .await
            .map_err(map_api_error)?;
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
            .client_api
            .add_media(
                &actor_user_id,
                &req.room_id,
                client_proto::AddMediaRequest {
                    playlist_id: (!req.playlist_id.is_empty()).then_some(req.playlist_id),
                    description: String::new(),
                    provider_instance_name: req.provider_instance_name,
                    source_config: Some(bilibili_live_source_config(req.room_live_id, req.shared)?),
                    name: req.name,
                },
            )
            .await
            .map_err(map_api_error)?;
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
                &req.room_id,
                client_proto::EditMediaRequest {
                    media_id: req.media_id,
                    name: req.name,
                    description: String::new(),
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
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
                &req.room_id,
                client_proto::DeleteMediaRequest {
                    media_id: req.media_id,
                    force: req.force,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
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
                &req.room_id,
                client_proto::MoveMediaRequest {
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
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn alist_login(
        &self,
        request: Request<AlistLoginRequest>,
    ) -> Result<Response<alist_proto::LoginResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = Self::map_into_api_result(
            self.alist_api
                .login_with_context(
                    &actor_user_id,
                    provider_request,
                    instance_name.as_deref(),
                    None,
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn alist_list(
        &self,
        request: Request<AlistListRequest>,
    ) -> Result<Response<alist_proto::ListResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = Self::map_into_api_result(
            self.alist_api
                .list_with_context(
                    &actor_user_id,
                    provider_request,
                    instance_name.as_deref(),
                    None,
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn alist_search(
        &self,
        request: Request<AlistSearchRequest>,
    ) -> Result<Response<alist_proto::SearchResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = Self::map_into_api_result(
            self.alist_api
                .search_with_context(
                    &actor_user_id,
                    provider_request,
                    instance_name.as_deref(),
                    None,
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn alist_get_me(
        &self,
        request: Request<AlistGetMeRequest>,
    ) -> Result<Response<alist_proto::GetMeResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = Self::map_into_api_result(
            self.alist_api
                .get_me_with_context(
                    &actor_user_id,
                    provider_request,
                    instance_name.as_deref(),
                    None,
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn alist_logout(
        &self,
        request: Request<AlistLogoutRequest>,
    ) -> Result<Response<alist_proto::LogoutResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = Self::map_into_api_result(
            self.alist_api
                .logout(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn alist_get_binds(
        &self,
        request: Request<AlistGetBindsRequest>,
    ) -> Result<Response<alist_proto::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = Self::map_api_result(
            self.alist_api
                .get_binds(&actor_user_id, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn emby_login(
        &self,
        request: Request<EmbyLoginRequest>,
    ) -> Result<Response<emby_proto::LoginResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = Self::map_into_api_result(
            self.emby_api
                .login_with_context(
                    &actor_user_id,
                    provider_request,
                    instance_name.as_deref(),
                    None,
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn emby_list(
        &self,
        request: Request<EmbyListRequest>,
    ) -> Result<Response<emby_proto::ListResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = Self::map_into_api_result(
            self.emby_api
                .list_with_context(
                    &actor_user_id,
                    provider_request,
                    instance_name.as_deref(),
                    None,
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn emby_get_me(
        &self,
        request: Request<EmbyGetMeRequest>,
    ) -> Result<Response<emby_proto::GetMeResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = Self::map_into_api_result(
            self.emby_api
                .get_me_with_context(
                    &actor_user_id,
                    provider_request,
                    instance_name.as_deref(),
                    None,
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn emby_logout(
        &self,
        request: Request<EmbyLogoutRequest>,
    ) -> Result<Response<emby_proto::LogoutResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = Self::map_into_api_result(
            self.emby_api.logout(&actor_user_id, provider_request).await,
        )?;
        Ok(Response::new(response))
    }

    async fn emby_get_binds(
        &self,
        request: Request<EmbyGetBindsRequest>,
    ) -> Result<Response<emby_proto::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = Self::map_api_result(
            self.emby_api
                .get_binds(&actor_user_id, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn bilibili_parse(
        &self,
        request: Request<BilibiliParseRequest>,
    ) -> Result<Response<bilibili_proto::ParseResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = Self::map_into_api_result(
            self.bilibili_api
                .parse_with_context(
                    &actor_user_id,
                    provider_request,
                    instance_name.as_deref(),
                    None,
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn bilibili_login_qr(
        &self,
        request: Request<BilibiliLoginQrRequest>,
    ) -> Result<Response<bilibili_proto::QrCodeResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (_actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = Self::map_into_api_result(
            self.bilibili_api
                .login_qr_with_context(provider_request, instance_name.as_deref(), None)
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn bilibili_check_qr(
        &self,
        request: Request<BilibiliCheckQrRequest>,
    ) -> Result<Response<bilibili_proto::QrStatusResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = Self::map_into_api_result(
            self.bilibili_api
                .check_qr_with_context(
                    &actor_user_id,
                    provider_request,
                    instance_name.as_deref(),
                    None,
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn bilibili_start_sms_login(
        &self,
        request: Request<BilibiliStartSmsLoginRequest>,
    ) -> Result<Response<bilibili_proto::StartSmsLoginResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (_actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = Self::map_into_api_result(
            self.bilibili_api
                .start_sms_login_with_context(provider_request, instance_name.as_deref(), None)
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn bilibili_send_sms(
        &self,
        request: Request<BilibiliSendSmsRequest>,
    ) -> Result<Response<bilibili_proto::SendSmsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (_actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = Self::map_into_api_result(
            self.bilibili_api
                .send_sms_with_context(provider_request, None, None)
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn bilibili_login_sms(
        &self,
        request: Request<BilibiliLoginSmsRequest>,
    ) -> Result<Response<bilibili_proto::LoginSmsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = Self::map_into_api_result(
            self.bilibili_api
                .login_sms_with_context(&actor_user_id, provider_request, None, None)
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn bilibili_get_user_info(
        &self,
        request: Request<BilibiliGetUserInfoRequest>,
    ) -> Result<Response<bilibili_proto::UserInfoResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = Self::map_into_api_result(
            self.bilibili_api
                .get_user_info_with_context(
                    &actor_user_id,
                    provider_request,
                    instance_name.as_deref(),
                    None,
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn bilibili_logout(
        &self,
        request: Request<BilibiliLogoutRequest>,
    ) -> Result<Response<bilibili_proto::LogoutResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = Self::map_into_api_result(
            self.bilibili_api
                .logout(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn bilibili_get_binds(
        &self,
        request: Request<BilibiliGetBindsRequest>,
    ) -> Result<Response<bilibili_proto::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = Self::map_api_result(
            self.bilibili_api
                .get_binds(&actor_user_id, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    async fn list_available_provider_instances(
        &self,
        request: Request<provider_common_proto::ListAvailableProviderInstancesRequest>,
    ) -> Result<Response<provider_common_proto::ProviderInstancesResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .provider_common_api
            .list_available_provider_instances(req)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_provider_backends(
        &self,
        request: Request<provider_common_proto::ListProviderBackendsRequest>,
    ) -> Result<Response<provider_common_proto::ProviderBackendsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .provider_common_api
            .list_provider_backends(req)
            .await
            .map_err(map_api_error)?;
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
            .list_provider_instances(req)
            .await
            .map_err(map_api_error)?;
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
            .add_provider_instance(req, &validated.user_id, &ctx, None)
            .await
            .map_err(map_api_error)?;
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
            .update_provider_instance(req, &validated.user_id, &ctx, None)
            .await
            .map_err(map_api_error)?;
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
            .delete_provider_instance(req, &validated.user_id, &ctx)
            .await
            .map_err(map_api_error)?;
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
            .reconnect_provider_instance(req, &validated.user_id, &ctx, None)
            .await
            .map_err(map_api_error)?;
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
            .enable_provider_instance(req, None)
            .await
            .map_err(map_api_error)?;
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
            .disable_provider_instance(req, None)
            .await
            .map_err(map_api_error)?;
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
            .get_settings(admin_proto::GetSettingsRequest {}, &validated.user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn update_settings(
        &self,
        request: Request<admin_proto::UpdateSettingsRequest>,
    ) -> Result<Response<admin_proto::RuntimeSettings>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .update_settings(req, &validated.user_id, &ctx)
            .await
            .map_err(map_api_error)?;
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
            .send_test_email(admin_proto::SendTestEmailRequest { to: req.to })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_service_state(
        &self,
        request: Request<GetServiceStateRequest>,
    ) -> Result<Response<admin_proto::GetServiceStateResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let response = self
            .admin_api
            .get_service_state(admin_proto::GetServiceStateRequest {})
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_server_state(
        &self,
        request: Request<GetServerStateRequest>,
    ) -> Result<Response<GetServerStateResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let target_node_id =
            synctv_api::status::validate_server_state_selection(Some(&req.node_id), req.all_nodes)
                .map_err(|error| Self::map_server_state_error(&error))?;
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
            .get_stats(Self::slice_cache_selection(req.node_id, req.all_nodes))
            .await
            .map_err(|error| Self::map_slice_cache_error(&error))?;
        Ok(Response::new(Self::get_slice_cache_stats_to_management(
            response,
        )))
    }

    async fn purge_slice_cache(
        &self,
        request: Request<PurgeSliceCacheRequest>,
    ) -> Result<Response<admin_proto::PurgeSliceCacheResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .slice_cache_runtime
            .purge(Self::slice_cache_selection(req.node_id, req.all_nodes))
            .await
            .map_err(|error| Self::map_slice_cache_error(&error))?;
        Ok(Response::new(Self::purge_slice_cache_to_management(
            response,
        )))
    }

    async fn evict_expired_slice_cache(
        &self,
        request: Request<EvictExpiredSliceCacheRequest>,
    ) -> Result<Response<admin_proto::EvictExpiredSliceCacheResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let response = self
            .slice_cache_runtime
            .evict_expired(Self::slice_cache_selection(req.node_id, req.all_nodes))
            .await
            .map_err(|error| Self::map_slice_cache_error(&error))?;
        Ok(Response::new(
            Self::evict_expired_slice_cache_to_management(response),
        ))
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
            .list_active_streams(admin_proto::ListActiveStreamsRequest {
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
            .map_err(map_api_error)?;
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
                admin_proto::KickStreamRequest {
                    room_id: req.room_id,
                    media_id: req.media_id,
                    reason: req.reason,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
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
