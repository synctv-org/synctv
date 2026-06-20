use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::{Stream, StreamExt};
use tonic::codec::CompressionEncoding;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Response, Status};

use crate::access::ManagementAccessController;
use crate::lifecycle::{LifecycleEvent, ManagementLifecycleController, ShutdownMode};
use crate::mapping::{
    map_management_user_lookup_error, map_room_list_sort_by, map_room_member_list_sort_by,
    map_room_status, map_sort_direction, map_user_list_sort_by, map_user_role, map_user_status,
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
    EmbyLogoutRequest, EvictExpiredSliceCacheNodeResult, EvictExpiredSliceCacheRequest,
    EvictExpiredSliceCacheResponse, GetPlaybackRequest, GetPlaylistRequest, GetRoomMembersRequest,
    GetRoomRequest, GetRoomSettingsRequest, GetSettingsGroupRequest, GetSettingsRequest,
    GetSliceCacheStatsRequest, GetSliceCacheStatsResponse, GetStreamInfoRequest,
    GetSystemStatsRequest, GetUserPreferencesRequest, GetUserRequest, GetUserRoomsRequest,
    KickMemberRequest, KickRoomStreamRequest, KickStreamRequest, ListActiveStreamsRequest,
    ListAdminsRequest, ListBanRecordsRequest, ListMediaRequest, ListPlaylistsRequest,
    ListRoomCreationReviewsRequest, ListRoomJoinReviewsRequest, ListRoomStreamsRequest,
    ListRoomsRequest, ListUserRegistrationReviewsRequest, ListUsersRequest, MoveMediaRequest,
    MovePlaylistRequest, PurgeSliceCacheNodeResult, PurgeSliceCacheRequest,
    PurgeSliceCacheResponse, RejectRoomCreationReviewRequest, RejectRoomJoinReviewRequest,
    RejectUserRegistrationReviewRequest, RemoveAdminRequest, ResetRoomSettingsRequest,
    SendTestEmailRequest, SetUserPasswordRequest, ShutdownMode as ProtoShutdownMode,
    SliceCacheConfigInfo, SliceCacheNodeFailure, SliceCacheStatsResponse, StartPlaybackRequest,
    StopPlaybackRequest, StopServerEvent, StopServerRequest, TransferRoomOwnershipRequest,
    UnbanRoomRequest, UnbanUserRequest, UpdateMemberPermissionsRequest, UpdatePlaybackStateRequest,
    UpdatePlaylistRequest, UpdateRoomPasswordRequest, UpdateRoomSettingsRequest,
    UpdateSettingsRequest, UpdateUserPreferencesRequest, UpdateUserRoleRequest,
    UpdateUserUsernameRequest, UserRef,
};
use synctv_api::grpc_support::map_api_error;
use synctv_api::impls::admin::{RequestContext, LOCAL_MANAGEMENT_ACTOR_USER_ID};
use synctv_api::impls::{
    AdminApiImpl, AlistApiImpl, ApiError, BilibiliApiImpl, ClientApiImpl, EmbyApiImpl,
    ProviderCommonApiImpl,
};
use synctv_core::models::{UserId, UserRole as CoreUserRole};
use synctv_core::service::UserService;
use synctv_core::Config;
use synctv_proto::{
    admin as admin_proto, client as client_proto,
    providers::{
        alist as alist_proto, bilibili as bilibili_proto, common as provider_common_proto,
        emby as emby_proto, rtmp as rtmp_proto,
    },
};

type ProxySliceCacheClient =
    synctv_proxy::grpc::ProxySliceCacheServiceClient<tonic::transport::Channel>;

const SLICE_CACHE_REMOTE_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const SLICE_CACHE_REMOTE_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

struct ValidatedManagementUser {
    user_id: UserId,
    role: CoreUserRole,
}

struct BatchUserResolution {
    user_ids: Vec<String>,
    failures: Vec<admin_proto::BatchResultItem>,
}

#[tonic::async_trait]
pub trait ManagementSliceCacheRuntime: Send + Sync {
    fn stats(&self) -> synctv_proxy::slice_cache::SliceCacheStats;

    async fn purge_all(&self) -> synctv_proxy::slice_cache::SliceCachePurgeResult;

    async fn evict_expired_entries(&self) -> u64;
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
    slice_cache_runtime: Arc<dyn ManagementSliceCacheRuntime>,
    cluster_client: Option<Arc<synctv_cluster::grpc::ClusterClient>>,
    node_id: String,
    lifecycle_controller: Arc<ManagementLifecycleController>,
    access_controller: ManagementAccessController,
    public_id_codec: Arc<synctv_core::PublicIdCodec>,
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
    pub slice_cache_runtime: Arc<dyn ManagementSliceCacheRuntime>,
    pub cluster_client: Option<Arc<synctv_cluster::grpc::ClusterClient>>,
    pub node_id: String,
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
            cluster_client,
            node_id,
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
            cluster_client,
            node_id,
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

    fn merge_json_object_patch(
        base: &mut serde_json::Value,
        patch: serde_json::Value,
    ) -> Result<(), Status> {
        if !patch.is_object() {
            return Err(Status::invalid_argument(
                "settings_json must be a JSON object patch",
            ));
        }

        Self::merge_json_value(base, patch);
        Ok(())
    }

    fn merge_json_value(base: &mut serde_json::Value, patch: serde_json::Value) {
        match (base, patch) {
            (serde_json::Value::Object(base_map), serde_json::Value::Object(patch_map)) => {
                for (key, patch_value) in patch_map {
                    match base_map.get_mut(&key) {
                        Some(base_value) => Self::merge_json_value(base_value, patch_value),
                        None => {
                            base_map.insert(key, patch_value);
                        }
                    }
                }
            }
            (base_slot, patch_value) => {
                *base_slot = patch_value;
            }
        }
    }

    fn encode_source_config(provider: &str, value: &serde_json::Value) -> Result<Vec<u8>, Status> {
        serde_json::to_vec(&value).map_err(|error| {
            tracing::error!(provider, error = %error, "failed to encode provider source config");
            Status::internal("failed to encode provider source config")
        })
    }

    fn trimmed_required(field_name: &str, value: &str) -> Result<String, Status> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(Status::invalid_argument(format!(
                "{field_name} must not be empty"
            )));
        }
        Ok(trimmed.to_string())
    }

    fn alist_source_config(server_id: &str, path: &str, password: &str) -> Result<Vec<u8>, Status> {
        let mut source_config = serde_json::Map::new();
        source_config.insert(
            "server_id".to_string(),
            serde_json::Value::String(Self::trimmed_required("server_id", server_id)?),
        );
        source_config.insert(
            "path".to_string(),
            serde_json::Value::String(Self::trimmed_required("path", path)?),
        );
        let password = password.trim();
        if !password.is_empty() {
            source_config.insert(
                "password".to_string(),
                serde_json::Value::String(password.to_string()),
            );
        }
        Self::encode_source_config("alist", &serde_json::Value::Object(source_config))
    }

    fn emby_source_config(server_id: &str, item_id: &str) -> Result<Vec<u8>, Status> {
        Self::encode_source_config(
            "emby",
            &serde_json::json!({
                "server_id": Self::trimmed_required("server_id", server_id)?,
                "item_id": Self::trimmed_required("item_id", item_id)?,
            }),
        )
    }

    fn bilibili_video_source_config(
        bvid: &str,
        aid: Option<u64>,
        cid: u64,
        shared: bool,
    ) -> Result<Vec<u8>, Status> {
        if bvid.trim().is_empty() && aid.is_none() {
            return Err(Status::invalid_argument("bvid or aid is required"));
        }
        if cid == 0 {
            return Err(Status::invalid_argument("cid must be non-zero"));
        }

        let mut source_config = serde_json::Map::new();
        source_config.insert(
            "type".to_string(),
            serde_json::Value::String("video".to_string()),
        );
        let bvid = bvid.trim();
        if !bvid.is_empty() {
            source_config.insert(
                "bvid".to_string(),
                serde_json::Value::String(bvid.to_string()),
            );
        }
        if let Some(aid) = aid {
            source_config.insert("aid".to_string(), serde_json::Value::from(aid));
        }
        source_config.insert("cid".to_string(), serde_json::Value::from(cid));
        if shared {
            source_config.insert("shared".to_string(), serde_json::Value::Bool(true));
        }
        Self::encode_source_config("bilibili", &serde_json::Value::Object(source_config))
    }

    fn bilibili_pgc_source_config(epid: u64, cid: u64, shared: bool) -> Result<Vec<u8>, Status> {
        if epid == 0 {
            return Err(Status::invalid_argument("epid must be non-zero"));
        }
        if cid == 0 {
            return Err(Status::invalid_argument("cid must be non-zero"));
        }
        let mut source_config = serde_json::Map::new();
        source_config.insert(
            "type".to_string(),
            serde_json::Value::String("pgc".to_string()),
        );
        source_config.insert("epid".to_string(), serde_json::Value::from(epid));
        source_config.insert("cid".to_string(), serde_json::Value::from(cid));
        if shared {
            source_config.insert("shared".to_string(), serde_json::Value::Bool(true));
        }
        Self::encode_source_config("bilibili", &serde_json::Value::Object(source_config))
    }

    fn bilibili_live_source_config(room_live_id: u64, shared: bool) -> Result<Vec<u8>, Status> {
        if room_live_id == 0 {
            return Err(Status::invalid_argument("room_live_id must be non-zero"));
        }
        let mut source_config = serde_json::Map::new();
        source_config.insert(
            "type".to_string(),
            serde_json::Value::String("live".to_string()),
        );
        source_config.insert("room_id".to_string(), serde_json::Value::from(room_live_id));
        if shared {
            source_config.insert("shared".to_string(), serde_json::Value::Bool(true));
        }
        Self::encode_source_config("bilibili", &serde_json::Value::Object(source_config))
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
        let ip_address = match synctv_api::grpc_support::extract_client_ip(request, &self.config) {
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

    fn slice_cache_stats_response(&self) -> SliceCacheStatsResponse {
        let stats = self.slice_cache_runtime.stats();
        SliceCacheStatsResponse {
            config: Some(SliceCacheConfigInfo {
                engine_enabled: stats.engine_enabled,
                backend: stats.backend,
                file_cache_dir: stats.file_cache_dir.unwrap_or_default(),
                slice_size: stats.slice_size,
                max_cache_size: stats.max_cache_size,
                segment_ttl_secs: stats.segment_ttl_secs,
                stale_max_age_secs: stats.stale_max_age_secs,
                stale_while_revalidate: stats.stale_while_revalidate,
                eviction_interval_secs: stats.eviction_interval_secs,
                watermark_ratio: stats.watermark_ratio,
            }),
            current_size_bytes: stats.current_size_bytes,
            entry_count: stats.entry_count,
            metadata_entries: stats.metadata_entries,
            updating_entries: stats.updating_entries,
            lock_count: stats.lock_count,
            usage_ratio: stats.usage_ratio,
            node_id: self.node_id.clone(),
        }
    }

    fn validate_slice_cache_target(
        node_id: &str,
        all_nodes: bool,
    ) -> Result<Option<String>, Status> {
        let node_id = node_id.trim();
        if all_nodes && !node_id.is_empty() {
            return Err(Status::invalid_argument(
                "node_id and all_nodes are mutually exclusive",
            ));
        }
        Ok((!node_id.is_empty()).then(|| node_id.to_string()))
    }

    fn require_cluster_client(
        &self,
        target_node_id: &str,
    ) -> Result<&Arc<synctv_cluster::grpc::ClusterClient>, Status> {
        self.cluster_client.as_ref().ok_or_else(|| {
            Status::failed_precondition(format!(
                "Cluster client is unavailable; cannot query slice cache for node '{target_node_id}'"
            ))
        })
    }

    fn cluster_failure(node_id: String, error: String) -> SliceCacheNodeFailure {
        SliceCacheNodeFailure { node_id, error }
    }

    async fn cluster_fan_out_all<T, F, Fut>(
        &self,
        local_result: T,
        remote_call: F,
    ) -> Result<(Vec<T>, Vec<SliceCacheNodeFailure>), Status>
    where
        F: Fn(Arc<Self>, synctv_cluster::discovery::NodeInfo) -> Fut + Clone,
        Fut: std::future::Future<Output = Result<T, Status>>,
    {
        let mut results = vec![local_result];
        let mut failures = Vec::new();

        if let Some(cluster_client) = &self.cluster_client {
            let remote_nodes = cluster_client
                .remote_routable_nodes()
                .await
                .map_err(|error| Status::unavailable(error.to_string()))?;
            let mut futures = futures::stream::FuturesUnordered::new();
            let service = Arc::new(self.clone());
            for node in remote_nodes {
                let service = service.clone();
                let call = remote_call.clone();
                futures.push(async move {
                    let node_id = node.node_id.clone();
                    call(service, node)
                        .await
                        .map_err(|error| (node_id, error.to_string()))
                });
            }
            while let Some(result) = futures.next().await {
                match result {
                    Ok(response) => results.push(response),
                    Err((node_id, error)) => {
                        failures.push(Self::cluster_failure(node_id, error));
                    }
                }
            }
        }

        Ok((results, failures))
    }

    fn proxy_slice_cache_stats_to_management(
        stats: synctv_proxy::grpc::SliceCacheStatsResponse,
    ) -> SliceCacheStatsResponse {
        SliceCacheStatsResponse {
            config: stats.config.map(|config| SliceCacheConfigInfo {
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
            }),
            current_size_bytes: stats.current_size_bytes,
            entry_count: stats.entry_count,
            metadata_entries: stats.metadata_entries,
            updating_entries: stats.updating_entries,
            lock_count: stats.lock_count,
            usage_ratio: stats.usage_ratio,
            node_id: stats.node_id,
        }
    }

    fn proxy_slice_cache_uri(address: &str) -> String {
        if address.starts_with("http://") || address.starts_with("https://") {
            address.to_string()
        } else {
            format!("http://{address}")
        }
    }

    async fn proxy_slice_cache_client(
        &self,
        address: &str,
    ) -> Result<ProxySliceCacheClient, Status> {
        let endpoint = Endpoint::from_shared(Self::proxy_slice_cache_uri(address))
            .map_err(|error| Status::unavailable(format!("invalid node address: {error}")))?
            .connect_timeout(SLICE_CACHE_REMOTE_CONNECT_TIMEOUT)
            .timeout(SLICE_CACHE_REMOTE_REQUEST_TIMEOUT);
        let channel: Channel = endpoint.connect().await.map_err(|error| {
            Status::unavailable(format!("failed to connect to {address}: {error}"))
        })?;
        let client = synctv_proxy::grpc::ProxySliceCacheServiceClient::new(channel)
            .max_decoding_message_size(self.config.server.grpc_max_message_size_bytes)
            .max_encoding_message_size(self.config.server.grpc_max_message_size_bytes);
        let client = if self.config.server.grpc_compression_enabled {
            client
                .accept_compressed(CompressionEncoding::Gzip)
                .send_compressed(CompressionEncoding::Gzip)
        } else {
            client
        };
        Ok(client)
    }

    fn attach_cluster_secret<T>(&self, request: &mut Request<T>) -> Result<(), Status> {
        if self.config.cluster.secret.is_empty() {
            return Err(Status::failed_precondition(
                "cluster secret is required for remote slice cache operations",
            ));
        }
        synctv_cluster::grpc::attach_cluster_secret(request, &self.config.cluster.secret)
            .map_err(|_| Status::failed_precondition("invalid cluster secret configuration"))?;
        Ok(())
    }

    async fn remote_slice_cache_stats(
        &self,
        node: &synctv_cluster::discovery::NodeInfo,
    ) -> Result<SliceCacheStatsResponse, Status> {
        let mut request = Request::new(synctv_proxy::grpc::GetSliceCacheStatsRequest {});
        self.attach_cluster_secret(&mut request)?;
        let mut client = self.proxy_slice_cache_client(&node.api_address).await?;
        client
            .get_slice_cache_stats(request)
            .await
            .map(|response| Self::proxy_slice_cache_stats_to_management(response.into_inner()))
            .map_err(|error| {
                Status::unavailable(format!(
                    "slice cache stats RPC failed for node '{}': {error}",
                    node.node_id
                ))
            })
    }

    async fn slice_cache_stats_for_target(
        &self,
        target_node_id: &str,
    ) -> Result<SliceCacheStatsResponse, Status> {
        if target_node_id == self.node_id {
            return Ok(self.slice_cache_stats_response());
        }
        let cluster_client = self.require_cluster_client(target_node_id)?;
        let node = cluster_client
            .resolve_routable_node(target_node_id)
            .await
            .map_err(|error| Status::unavailable(error.to_string()))?;
        self.remote_slice_cache_stats(&node).await
    }

    async fn collect_slice_cache_stats(
        &self,
        target_node_id: Option<String>,
        all_nodes: bool,
    ) -> Result<GetSliceCacheStatsResponse, Status> {
        if all_nodes {
            let local = self.slice_cache_stats_response();
            let (nodes, failures) = self
                .cluster_fan_out_all(local, |service, node| async move {
                    service.remote_slice_cache_stats(&node).await
                })
                .await?;
            return Ok(GetSliceCacheStatsResponse { nodes, failures });
        }

        let node = match target_node_id {
            Some(node_id) => self.slice_cache_stats_for_target(&node_id).await?,
            None => self.slice_cache_stats_response(),
        };
        Ok(GetSliceCacheStatsResponse {
            nodes: vec![node],
            failures: Vec::new(),
        })
    }

    async fn purge_local_slice_cache(&self) -> Result<PurgeSliceCacheNodeResult, Status> {
        let result = self.slice_cache_runtime.purge_all().await;
        Ok(PurgeSliceCacheNodeResult {
            node_id: self.node_id.clone(),
            success: true,
            removed_entries: result.removed_entries,
            freed_bytes: result.freed_bytes,
            stats: Some(self.slice_cache_stats_response()),
        })
    }

    fn proxy_purge_to_management(
        response: synctv_proxy::grpc::PurgeSliceCacheResponse,
    ) -> PurgeSliceCacheNodeResult {
        PurgeSliceCacheNodeResult {
            node_id: response.node_id,
            success: response.success,
            removed_entries: response.removed_entries,
            freed_bytes: response.freed_bytes,
            stats: response
                .stats
                .map(Self::proxy_slice_cache_stats_to_management),
        }
    }

    async fn remote_purge_slice_cache(
        &self,
        node: &synctv_cluster::discovery::NodeInfo,
    ) -> Result<PurgeSliceCacheNodeResult, Status> {
        let mut request = Request::new(synctv_proxy::grpc::PurgeSliceCacheRequest {});
        self.attach_cluster_secret(&mut request)?;
        let mut client = self.proxy_slice_cache_client(&node.api_address).await?;
        client
            .purge_slice_cache(request)
            .await
            .map(|response| Self::proxy_purge_to_management(response.into_inner()))
            .map_err(|error| {
                Status::unavailable(format!(
                    "slice cache purge RPC failed for node '{}': {error}",
                    node.node_id
                ))
            })
    }

    fn purge_response_from_nodes(
        nodes: Vec<PurgeSliceCacheNodeResult>,
        failures: Vec<SliceCacheNodeFailure>,
    ) -> PurgeSliceCacheResponse {
        let removed_entries = nodes.iter().map(|node| node.removed_entries).sum();
        let freed_bytes = nodes.iter().map(|node| node.freed_bytes).sum();
        let stats = (nodes.len() == 1).then(|| nodes[0].stats.clone()).flatten();
        PurgeSliceCacheResponse {
            success: failures.is_empty() && nodes.iter().all(|node| node.success),
            removed_entries,
            freed_bytes,
            stats,
            nodes,
            failures,
        }
    }

    async fn purge_slice_cache_for_selection(
        &self,
        target_node_id: Option<String>,
        all_nodes: bool,
    ) -> Result<PurgeSliceCacheResponse, Status> {
        if all_nodes {
            let local = self.purge_local_slice_cache().await?;
            let (nodes, failures) = self
                .cluster_fan_out_all(local, |service, node| async move {
                    service.remote_purge_slice_cache(&node).await
                })
                .await?;
            return Ok(Self::purge_response_from_nodes(nodes, failures));
        }

        let node = match target_node_id {
            Some(node_id) if node_id != self.node_id => {
                let cluster_client = self.require_cluster_client(&node_id)?;
                let node = cluster_client
                    .resolve_routable_node(&node_id)
                    .await
                    .map_err(|error| Status::unavailable(error.to_string()))?;
                self.remote_purge_slice_cache(&node).await?
            }
            Some(_) | None => self.purge_local_slice_cache().await?,
        };
        Ok(Self::purge_response_from_nodes(vec![node], Vec::new()))
    }

    async fn evict_expired_local_slice_cache(
        &self,
    ) -> Result<EvictExpiredSliceCacheNodeResult, Status> {
        let removed_expired_entries = self.slice_cache_runtime.evict_expired_entries().await;
        Ok(EvictExpiredSliceCacheNodeResult {
            node_id: self.node_id.clone(),
            success: true,
            removed_expired_entries,
            stats: Some(self.slice_cache_stats_response()),
        })
    }

    fn proxy_evict_expired_to_management(
        response: synctv_proxy::grpc::EvictExpiredSliceCacheResponse,
    ) -> EvictExpiredSliceCacheNodeResult {
        EvictExpiredSliceCacheNodeResult {
            node_id: response.node_id,
            success: response.success,
            removed_expired_entries: response.removed_expired_entries,
            stats: response
                .stats
                .map(Self::proxy_slice_cache_stats_to_management),
        }
    }

    async fn remote_evict_expired_slice_cache(
        &self,
        node: &synctv_cluster::discovery::NodeInfo,
    ) -> Result<EvictExpiredSliceCacheNodeResult, Status> {
        let mut request = Request::new(synctv_proxy::grpc::EvictExpiredSliceCacheRequest {});
        self.attach_cluster_secret(&mut request)?;
        let mut client = self.proxy_slice_cache_client(&node.api_address).await?;
        client
            .evict_expired_slice_cache(request)
            .await
            .map(|response| Self::proxy_evict_expired_to_management(response.into_inner()))
            .map_err(|error| {
                Status::unavailable(format!(
                    "slice cache evict-expired RPC failed for node '{}': {error}",
                    node.node_id
                ))
            })
    }

    fn evict_expired_response_from_nodes(
        nodes: Vec<EvictExpiredSliceCacheNodeResult>,
        failures: Vec<SliceCacheNodeFailure>,
    ) -> EvictExpiredSliceCacheResponse {
        let removed_expired_entries = nodes.iter().map(|node| node.removed_expired_entries).sum();
        let stats = (nodes.len() == 1).then(|| nodes[0].stats.clone()).flatten();
        EvictExpiredSliceCacheResponse {
            success: failures.is_empty() && nodes.iter().all(|node| node.success),
            removed_expired_entries,
            stats,
            nodes,
            failures,
        }
    }

    async fn evict_expired_slice_cache_for_selection(
        &self,
        target_node_id: Option<String>,
        all_nodes: bool,
    ) -> Result<EvictExpiredSliceCacheResponse, Status> {
        if all_nodes {
            let local = self.evict_expired_local_slice_cache().await?;
            let (nodes, failures) = self
                .cluster_fan_out_all(local, |service, node| async move {
                    service.remote_evict_expired_slice_cache(&node).await
                })
                .await?;
            return Ok(Self::evict_expired_response_from_nodes(nodes, failures));
        }

        let node = match target_node_id {
            Some(node_id) if node_id != self.node_id => {
                let cluster_client = self.require_cluster_client(&node_id)?;
                let node = cluster_client
                    .resolve_routable_node(&node_id)
                    .await
                    .map_err(|error| Status::unavailable(error.to_string()))?;
                self.remote_evict_expired_slice_cache(&node).await?
            }
            Some(_) | None => self.evict_expired_local_slice_cache().await?,
        };
        Ok(Self::evict_expired_response_from_nodes(
            vec![node],
            Vec::new(),
        ))
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
    ) -> Result<Response<admin_proto::GetUserResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let user_id = self.resolve_required_user_ref(req.user, "user").await?;
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
        let user_id = self.resolve_required_user_ref(req.user, "user").await?;
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
        let user_id = self.resolve_required_user_ref(req.user, "user").await?;
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
    ) -> Result<Response<admin_proto::AddAdminResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self.resolve_required_user_ref(req.user, "user").await?;
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
        let user_id = self.resolve_required_user_ref(req.user, "user").await?;
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
    ) -> Result<Response<admin_proto::CreateUserResponse>, Status> {
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
        let user_id = self.resolve_required_user_ref(req.user, "user").await?;
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
    ) -> Result<Response<admin_proto::BanUserResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self.resolve_required_user_ref(req.user, "user").await?;
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
    ) -> Result<Response<admin_proto::UnbanUserResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self.resolve_required_user_ref(req.user, "user").await?;
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
    ) -> Result<Response<admin_proto::RejectUserRegistrationReviewResponse>, Status> {
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
    ) -> Result<Response<admin_proto::RejectRoomCreationReviewResponse>, Status> {
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
    ) -> Result<Response<admin_proto::RejectRoomJoinReviewResponse>, Status> {
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
    ) -> Result<Response<admin_proto::UpdateUserRoleResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self.resolve_required_user_ref(req.user, "user").await?;
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
        let user_id = self.resolve_required_user_ref(req.user, "user").await?;
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
    ) -> Result<Response<admin_proto::UpdateUserUsernameResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self.resolve_required_user_ref(req.user, "user").await?;
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
        let user_id = self.resolve_required_user_ref(req.user, "user").await?;
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
    ) -> Result<Response<client_proto::CreateRoomResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let actor_user_id = self.resolve_client_actor_user_id(req.actor).await?;
        let settings = if req.settings_json.is_empty() {
            Vec::new()
        } else {
            let patch = serde_json::from_slice(&req.settings_json).map_err(|error| {
                Status::invalid_argument(format!("Invalid settings JSON: {error}"))
            })?;
            let mut merged = serde_json::to_value(synctv_core::models::RoomSettings::default())
                .map_err(|error| {
                    tracing::error!(error = %error, "failed to encode default room settings");
                    Status::internal("failed to encode default room settings")
                })?;
            Self::merge_json_object_patch(&mut merged, patch)?;
            serde_json::to_vec(&merged).map_err(|error| {
                tracing::error!(error = %error, "failed to encode room settings");
                Status::internal("failed to encode room settings")
            })?
        };
        let response = self
            .client_api
            .create_room(
                &actor_user_id,
                client_proto::CreateRoomRequest {
                    name: req.name,
                    settings,
                    description: req.description,
                    password: req.password,
                },
            )
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
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_room(
        &self,
        request: Request<GetRoomRequest>,
    ) -> Result<Response<admin_proto::GetRoomResponse>, Status> {
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

    async fn add_member(
        &self,
        request: Request<AddMemberRequest>,
    ) -> Result<Response<client_proto::AddMemberResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self.resolve_required_user_ref(req.user, "user").await?;
        let response = self
            .admin_api
            .add_member(
                admin_proto::AddMemberRequest {
                    room_id: req.room_id,
                    user_id,
                    role: req.role,
                    notify: req.notify,
                },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(client_proto::AddMemberResponse {
            member: response.member,
        }))
    }

    async fn update_member_permissions(
        &self,
        request: Request<UpdateMemberPermissionsRequest>,
    ) -> Result<Response<client_proto::UpdateMemberPermissionsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self.resolve_required_user_ref(req.user, "user").await?;
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
        Ok(Response::new(
            client_proto::UpdateMemberPermissionsResponse {
                member: response.member,
            },
        ))
    }

    async fn kick_member(
        &self,
        request: Request<KickMemberRequest>,
    ) -> Result<Response<client_proto::KickMemberResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let user_id = self.resolve_required_user_ref(req.user, "user").await?;
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
        request: Request<UpdateRoomSettingsRequest>,
    ) -> Result<Response<admin_proto::UpdateRoomSettingsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let current = self
            .admin_api
            .get_room_settings(admin_proto::GetRoomSettingsRequest {
                room_id: req.room_id.clone(),
            })
            .await
            .map_err(map_api_error)?;
        let mut settings = if current.settings.is_empty() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_slice(&current.settings).map_err(|error| {
                Status::internal(format!("stored room settings are invalid: {error}"))
            })?
        };
        let patch = serde_json::from_slice(&req.settings_json)
            .map_err(|error| Status::invalid_argument(format!("Invalid settings JSON: {error}")))?;
        Self::merge_json_object_patch(&mut settings, patch)?;
        let settings_json = serde_json::to_vec(&settings).map_err(|error| {
            tracing::error!(error = %error, "failed to encode room settings");
            Status::internal("failed to encode room settings")
        })?;
        let response = self
            .admin_api
            .update_room_settings(
                admin_proto::UpdateRoomSettingsRequest {
                    room_id: req.room_id,
                    settings: settings_json,
                },
                &validated.user_id,
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn reset_room_settings(
        &self,
        request: Request<ResetRoomSettingsRequest>,
    ) -> Result<Response<admin_proto::ResetRoomSettingsResponse>, Status> {
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
    ) -> Result<Response<client_proto::TransferRoomOwnershipResponse>, Status> {
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
    ) -> Result<Response<admin_proto::BanRoomResponse>, Status> {
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
    ) -> Result<Response<admin_proto::UnbanRoomResponse>, Status> {
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
                    target: req.target_json,
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
    ) -> Result<Response<client_proto::UpdatePlaybackStateResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .update_playback_state(
                &req.room_id,
                req.update.ok_or_else(|| {
                    Status::invalid_argument("playback state update payload is required")
                })?,
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
                    sort_by: match crate::proto::RoomStreamListSortBy::try_from(req.sort_by) {
                        Ok(crate::proto::RoomStreamListSortBy::MediaId) => {
                            client_proto::RoomStreamListSortBy::MediaId as i32
                        }
                        _ => client_proto::RoomStreamListSortBy::Unspecified as i32,
                    },
                    sort_direction: match crate::proto::SortDirection::try_from(req.sort_direction)
                    {
                        Ok(crate::proto::SortDirection::Desc) => {
                            client_proto::SortDirection::Desc as i32
                        }
                        Ok(crate::proto::SortDirection::Asc) => {
                            client_proto::SortDirection::Asc as i32
                        }
                        _ => client_proto::SortDirection::Unspecified as i32,
                    },
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
    ) -> Result<Response<client_proto::CreatePlaylistResponse>, Status> {
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
                    source_config: req.source_config_json,
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
    ) -> Result<Response<client_proto::CreatePlaylistResponse>, Status> {
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
                    source_provider: "alist".to_string(),
                    source_config: Self::alist_source_config(
                        &req.server_id,
                        &req.path,
                        &req.password,
                    )?,
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
    ) -> Result<Response<client_proto::CreatePlaylistResponse>, Status> {
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
                    source_provider: "emby".to_string(),
                    source_config: Self::emby_source_config(&req.server_id, &req.item_id)?,
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
    ) -> Result<Response<client_proto::UpdatePlaylistResponse>, Status> {
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
    ) -> Result<Response<client_proto::MovePlaylistResponse>, Status> {
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
                    target: req.target_json,
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
    ) -> Result<Response<client_proto::AddMediaResponse>, Status> {
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
                    source_provider: req.source_provider,
                    provider_instance_name: req.provider_instance_name,
                    source_config: req.source_config_json,
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
    ) -> Result<Response<client_proto::AddMediaResponse>, Status> {
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
                    source_provider: "direct_url".to_string(),
                    provider_instance_name: String::new(),
                    source_config: serde_json::to_vec(&serde_json::json!({ "url": req.url }))
                        .map_err(|error| {
                            tracing::error!(error = %error, "failed to encode media source config");
                            Status::internal("failed to encode media source config")
                        })?,
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
    ) -> Result<Response<client_proto::AddMediaResponse>, Status> {
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
                    source_provider: "alist".to_string(),
                    provider_instance_name: req.provider_instance_name,
                    source_config: Self::alist_source_config(
                        &req.server_id,
                        &req.path,
                        &req.password,
                    )?,
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
    ) -> Result<Response<client_proto::AddMediaResponse>, Status> {
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
                    source_provider: "emby".to_string(),
                    provider_instance_name: req.provider_instance_name,
                    source_config: Self::emby_source_config(&req.server_id, &req.item_id)?,
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
    ) -> Result<Response<client_proto::AddMediaResponse>, Status> {
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
                    source_provider: "bilibili".to_string(),
                    provider_instance_name: req.provider_instance_name,
                    source_config: Self::bilibili_video_source_config(
                        &req.bvid, req.aid, req.cid, req.shared,
                    )?,
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
    ) -> Result<Response<client_proto::AddMediaResponse>, Status> {
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
                    source_provider: "bilibili".to_string(),
                    provider_instance_name: req.provider_instance_name,
                    source_config: Self::bilibili_pgc_source_config(req.epid, req.cid, req.shared)?,
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
    ) -> Result<Response<client_proto::AddMediaResponse>, Status> {
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
                    source_provider: "bilibili".to_string(),
                    provider_instance_name: req.provider_instance_name,
                    source_config: Self::bilibili_live_source_config(req.room_live_id, req.shared)?,
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
    ) -> Result<Response<client_proto::EditMediaResponse>, Status> {
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
    ) -> Result<Response<admin_proto::GetSettingsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let response = self
            .admin_api
            .get_settings(admin_proto::GetSettingsRequest {}, &validated.user_id, &ctx)
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_settings_group(
        &self,
        request: Request<GetSettingsGroupRequest>,
    ) -> Result<Response<admin_proto::GetSettingsGroupResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .get_settings_group(
                admin_proto::GetSettingsGroupRequest { group: req.group },
                &validated.user_id,
                &ctx,
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn update_settings(
        &self,
        request: Request<UpdateSettingsRequest>,
    ) -> Result<Response<admin_proto::UpdateSettingsResponse>, Status> {
        let validated = self.check_admin_get_validated(&request)?;
        let ctx = self.grpc_request_context(&request);
        let req = request.into_inner();
        let response = self
            .admin_api
            .update_settings(
                admin_proto::UpdateSettingsRequest {
                    group: req.group,
                    settings: req.settings,
                },
                &validated.user_id,
                &ctx,
            )
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

    async fn get_system_stats(
        &self,
        request: Request<GetSystemStatsRequest>,
    ) -> Result<Response<admin_proto::GetSystemStatsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let response = self
            .admin_api
            .get_system_stats(admin_proto::GetSystemStatsRequest {})
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_slice_cache_stats(
        &self,
        request: Request<GetSliceCacheStatsRequest>,
    ) -> Result<Response<GetSliceCacheStatsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let target_node_id = Self::validate_slice_cache_target(&req.node_id, req.all_nodes)?;
        Ok(Response::new(
            self.collect_slice_cache_stats(target_node_id, req.all_nodes)
                .await?,
        ))
    }

    async fn purge_slice_cache(
        &self,
        request: Request<PurgeSliceCacheRequest>,
    ) -> Result<Response<PurgeSliceCacheResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let target_node_id = Self::validate_slice_cache_target(&req.node_id, req.all_nodes)?;
        Ok(Response::new(
            self.purge_slice_cache_for_selection(target_node_id, req.all_nodes)
                .await?,
        ))
    }

    async fn evict_expired_slice_cache(
        &self,
        request: Request<EvictExpiredSliceCacheRequest>,
    ) -> Result<Response<EvictExpiredSliceCacheResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let target_node_id = Self::validate_slice_cache_target(&req.node_id, req.all_nodes)?;
        Ok(Response::new(
            self.evict_expired_slice_cache_for_selection(target_node_id, req.all_nodes)
                .await?,
        ))
    }

    async fn list_active_streams(
        &self,
        request: Request<ListActiveStreamsRequest>,
    ) -> Result<Response<admin_proto::ListActiveStreamsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let user_id = self.resolve_optional_user_ref(req.user, "user").await?;
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
