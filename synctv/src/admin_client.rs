use anyhow::{bail, Context, Result};
#[cfg(unix)]
use hyper_util::rt::TokioIo;
use std::path::{Path, PathBuf};
use std::time::Duration;
use synctv_management::proto::management_service_client::ManagementServiceClient;
use tonic::transport::Channel;
use tonic::{metadata::MetadataValue, service::Interceptor, Request, Status};

use crate::config_loader::{
    load_config_with_options, load_dotenv, public_id_config_extensions, LoadConfigOptions,
};

const MANAGEMENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct AdminConnectionOptions {
    pub endpoint: Option<String>,
    pub auth_token: Option<String>,
    pub auth_token_file: Option<String>,
    pub config_path: Option<String>,
    pub data_dir: Option<String>,
    pub load_dotenv: bool,
    pub verbose: bool,
    pub resolved_config_endpoint: Option<String>,
    pub allow_config_auth_for_explicit_endpoint: bool,
}

type AuthenticatedManagementClient = ManagementServiceClient<
    tonic::service::interceptor::InterceptedService<Channel, ManagementAuthInterceptor>,
>;

pub struct RemoteAdminSession {
    channel: Channel,
    endpoint: String,
    authorization: Option<MetadataValue<tonic::metadata::Ascii>>,
}

impl RemoteAdminSession {
    pub async fn connect(options: AdminConnectionOptions) -> Result<Self> {
        let endpoints = resolve_candidate_endpoints(&options)?;
        let (channel, endpoint) = connect_first_available(&endpoints).await?;
        let authorization = resolve_management_auth_token(&options)?
            .map(|token| format!("Bearer {token}").parse())
            .transpose()
            .context("invalid management auth token metadata")?;

        Ok(Self {
            channel,
            endpoint,
            authorization,
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn management_client(&self) -> AuthenticatedManagementClient {
        ManagementServiceClient::with_interceptor(
            self.channel.clone(),
            ManagementAuthInterceptor {
                authorization: self.authorization.clone(),
            },
        )
        .max_decoding_message_size(synctv_core::service::MAX_RUNTIME_SETTINGS_IMPORT_REQUEST_BYTES)
        .max_encoding_message_size(synctv_core::service::MAX_RUNTIME_SETTINGS_IMPORT_REQUEST_BYTES)
    }
}

#[derive(Clone, Debug)]
pub struct ManagementAuthInterceptor {
    authorization: Option<MetadataValue<tonic::metadata::Ascii>>,
}

impl Interceptor for ManagementAuthInterceptor {
    fn call(&mut self, mut request: Request<()>) -> std::result::Result<Request<()>, Status> {
        if let Some(authorization) = &self.authorization {
            request
                .metadata_mut()
                .insert("authorization", authorization.clone());
        }
        Ok(request)
    }
}

#[derive(Clone, Debug)]
enum AdminEndpoint {
    Tcp(String),
    Unix(String),
}

fn normalize_endpoint(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("endpoint is empty");
    }

    if let Some(path) = trimmed.strip_prefix("unix://") {
        return normalize_unix_endpoint(path);
    }

    if Path::new(trimmed).is_absolute() || (!trimmed.contains("://") && trimmed.ends_with(".sock"))
    {
        return normalize_unix_endpoint(trimmed);
    }

    let candidate = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    };
    let parsed = url::Url::parse(&candidate)
        .with_context(|| format!("failed to parse endpoint {candidate}"))?;

    if parsed.host_str().is_none() {
        bail!("endpoint {candidate} does not contain a host");
    }

    Ok(parsed.to_string())
}

fn normalize_unix_endpoint(path: &str) -> Result<String> {
    let path = path.trim();
    if path.is_empty() {
        bail!("unix endpoint path is empty");
    }

    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory for unix endpoint")?
            .join(path)
    };
    let normalized: PathBuf = absolute.components().collect();
    Ok(format!("unix://{}", normalized.display()))
}

fn parse_admin_endpoint(raw: &str) -> Result<AdminEndpoint> {
    let normalized = normalize_endpoint(raw)?;
    if let Some(path) = normalized.strip_prefix("unix://") {
        return Ok(AdminEndpoint::Unix(path.to_string()));
    }
    Ok(AdminEndpoint::Tcp(normalized))
}

fn resolve_management_endpoint_from_config(
    config_path: Option<&str>,
    data_dir: Option<&str>,
    load_dotenv: bool,
    verbose: bool,
) -> Result<String> {
    let config = load_config_with_options(&LoadConfigOptions {
        config_path: config_path.map(str::to_string),
        data_dir: data_dir.map(str::to_string),
        load_dotenv,
        validate: false,
        verbose,
        extensions: public_id_config_extensions(),
    })?;
    normalize_endpoint(&config.management_endpoint())
}

fn resolve_management_auth_token(options: &AdminConnectionOptions) -> Result<Option<String>> {
    if let Some(token) = options.auth_token.as_deref() {
        let token = token.trim();
        if token.is_empty() {
            bail!("management auth token passed via --auth-token must not be empty");
        }
        return Ok(Some(token.to_string()));
    }

    if let Some(path) = options.auth_token_file.as_deref() {
        return read_management_auth_token_file(path, "--auth-token-file").map(Some);
    }

    let explicit_endpoint = options
        .endpoint
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());

    if explicit_endpoint {
        if options.load_dotenv {
            load_dotenv(options.verbose)?;
        }
        if let Ok(token) = std::env::var("SYNCTV_MANAGEMENT_AUTH_TOKEN") {
            let token = token.trim();
            if !token.is_empty() {
                return Ok(Some(token.to_string()));
            }
        }
        if let Ok(path) = std::env::var("SYNCTV_MANAGEMENT_AUTH_TOKEN_FILE") {
            let path = path.trim();
            if !path.is_empty() {
                return read_management_auth_token_file(path, "SYNCTV_MANAGEMENT_AUTH_TOKEN_FILE")
                    .map(Some);
            }
        }

        if !options.allow_config_auth_for_explicit_endpoint {
            return Ok(None);
        }
    }

    if let Ok(token) = std::env::var("SYNCTV_MANAGEMENT_AUTH_TOKEN") {
        let token = token.trim();
        if !token.is_empty() {
            return Ok(Some(token.to_string()));
        }
    }
    if let Ok(path) = std::env::var("SYNCTV_MANAGEMENT_AUTH_TOKEN_FILE") {
        let path = path.trim();
        if !path.is_empty() {
            return read_management_auth_token_file(path, "SYNCTV_MANAGEMENT_AUTH_TOKEN_FILE")
                .map(Some);
        }
    }

    let explicit_config_source =
        options.config_path.is_some() || std::env::var_os("SYNCTV_CONFIG_PATH").is_some();

    if explicit_endpoint && !explicit_config_source {
        return Ok(None);
    }

    let config = load_config_with_options(&LoadConfigOptions {
        config_path: options.config_path.clone(),
        data_dir: options.data_dir.clone(),
        load_dotenv: options.load_dotenv,
        validate: false,
        verbose: options.verbose,
        extensions: public_id_config_extensions(),
    })?;
    let token = config.management.auth_token.trim();
    if token.is_empty() {
        Ok(None)
    } else {
        Ok(Some(token.to_string()))
    }
}

fn read_management_auth_token_file(path: &str, source: &str) -> Result<String> {
    let token = std::fs::read_to_string(path).with_context(|| {
        format!("failed to read management auth token file from {source}: {path}")
    })?;
    let token = token.trim();
    if token.is_empty() {
        bail!("management auth token file from {source} is empty: {path}");
    }
    Ok(token.to_string())
}

fn resolve_candidate_endpoints(options: &AdminConnectionOptions) -> Result<Vec<String>> {
    if let Some(endpoint) = options
        .endpoint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(vec![normalize_endpoint(endpoint)?]);
    }

    if let Some(endpoint) = options
        .resolved_config_endpoint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(vec![normalize_endpoint(endpoint)?]);
    }

    Ok(vec![resolve_management_endpoint_from_config(
        options.config_path.as_deref(),
        options.data_dir.as_deref(),
        options.load_dotenv,
        options.verbose,
    )?])
}

async fn connect_first_available(endpoints: &[String]) -> Result<(Channel, String)> {
    let mut errors = Vec::new();

    for endpoint in endpoints {
        match connect_channel(endpoint).await {
            Ok(channel) => return Ok((channel, endpoint.clone())),
            Err(error) => errors.push(format!("{endpoint}: {error}")),
        }
    }

    bail!(
        "failed to connect to any management endpoint: {}",
        errors.join("; ")
    );
}

async fn connect_channel(endpoint: &str) -> Result<Channel> {
    match parse_admin_endpoint(endpoint)? {
        AdminEndpoint::Tcp(endpoint) => Channel::from_shared(endpoint.clone())
            .context("invalid admin endpoint")?
            .connect_timeout(MANAGEMENT_CONNECT_TIMEOUT)
            .connect()
            .await
            .with_context(|| format!("failed to connect to admin endpoint {endpoint}")),
        AdminEndpoint::Unix(path) => connect_unix_channel(&path).await,
    }
}

#[cfg(unix)]
async fn connect_unix_channel(path: &str) -> Result<Channel> {
    let socket_path = path.to_owned();
    let error_path = socket_path.clone();
    tonic::transport::Endpoint::try_from("http://[::]:50052")
        .context("invalid synthetic unix endpoint")?
        .connect_timeout(MANAGEMENT_CONNECT_TIMEOUT)
        .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
            let path = socket_path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(path).await?;
                Ok::<_, std::io::Error>(TokioIo::new(stream))
            }
        }))
        .await
        .with_context(|| format!("failed to connect to admin unix socket {error_path}"))
}

#[cfg(not(unix))]
fn connect_unix_channel(_path: &str) -> std::future::Ready<Result<Channel>> {
    std::future::ready(Err(anyhow::anyhow!(
        "unix management endpoints are not supported on this platform"
    )))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::RemoteAdminSession;
    use super::{
        normalize_endpoint, resolve_candidate_endpoints, resolve_management_auth_token,
        AdminConnectionOptions,
    };
    #[cfg(unix)]
    use std::pin::Pin;

    #[cfg(unix)]
    use crate::app_config::default_management_unix_socket_path;
    use tempfile::tempdir;

    #[cfg(unix)]
    use futures_util::stream;
    #[cfg(unix)]
    use std::sync::Arc;
    #[cfg(unix)]
    use std::sync::Mutex;
    use std::sync::OnceLock;
    #[cfg(unix)]
    use synctv_management::proto::{
        management_service_server::{ManagementService, ManagementServiceServer},
        AddAdminRequest, AddAlistMediaRequest, AddBilibiliLiveMediaRequest,
        AddBilibiliPgcMediaRequest, AddBilibiliVideoMediaRequest, AddDirectUrlMediaRequest,
        AddEmbyMediaRequest, AddMediaRequest, AddMemberRequest, AlistGetBindsRequest,
        AlistGetMeRequest, AlistListRequest, AlistLoginRequest, AlistLogoutRequest,
        AlistSearchRequest, ApproveRoomCreationReviewRequest, ApproveRoomJoinReviewRequest,
        ApproveUserRegistrationReviewRequest, BanRoomRequest, BanUserRequest, BatchBanRoomsRequest,
        BatchBanUsersRequest, BatchDeleteRoomsRequest, BatchDeleteUsersRequest,
        BilibiliCheckQrRequest, BilibiliGetBindsRequest, BilibiliGetUserInfoRequest,
        BilibiliLoginQrRequest, BilibiliLoginSmsRequest, BilibiliLogoutRequest,
        BilibiliParseRequest, BilibiliSendSmsRequest, BilibiliStartSmsLoginRequest,
        CreateAlistPlaylistRequest, CreateEmbyPlaylistRequest, CreatePlaylistRequest,
        CreatePublishKeyRequest, CreateRoomRequest, CreateUserRequest, DeleteMediaRequest,
        DeletePlaylistRequest, DeleteRoomRequest, DeleteUserRequest, DouyinBindRequest,
        DouyinGetBindsRequest, DouyinListUserPostsRequest, DouyinResolveRequest,
        DouyinUnbindRequest, EditMediaRequest, EmbyGetBindsRequest, EmbyGetMeRequest,
        EmbyListRequest, EmbyLoginRequest, EmbyLogoutRequest, EvictExpiredSliceCacheRequest,
        FavoriteRoomRequest, GetPlaybackRequest, GetPlaylistRequest, GetRoomMembersRequest,
        GetRoomRequest, GetRoomSettingsRequest, GetServerStateRequest, GetServerStateResponse,
        GetServiceStateRequest, GetSettingsRequest, GetSliceCacheStatsRequest,
        GetStreamInfoRequest, GetUserPreferencesRequest, GetUserRequest, GetUserRoomsRequest,
        KickMemberRequest, KickRoomStreamRequest, KickStreamRequest, ListActiveStreamsRequest,
        ListAdminsRequest, ListBanRecordsRequest, ListFavoriteRoomsRequest, ListMediaRequest,
        ListPlaylistsRequest, ListRoomCreationReviewsRequest, ListRoomJoinReviewsRequest,
        ListRoomStreamsRequest, ListRoomsRequest, ListUserRegistrationReviewsRequest,
        ListUsersRequest, MoveMediaRequest, MovePlaylistRequest, PurgeSliceCacheRequest,
        RejectRoomCreationReviewRequest, RejectRoomJoinReviewRequest,
        RejectUserRegistrationReviewRequest, RemoveAdminRequest, ResetRoomSettingsRequest,
        SearchChatMessagesRequest, SendTestEmailRequest, SetUserPasswordRequest,
        StartPlaybackRequest, StopPlaybackRequest, StopServerEvent, StopServerRequest,
        TransferRoomOwnershipRequest, UnbanRoomRequest, UnbanUserRequest, UnfavoriteRoomRequest,
        UpdateMemberDisplayTagRequest, UpdateMemberPermissionsRequest,
        UpdateMemberRemarkNameRequest, UpdatePlaybackStateRequest, UpdatePlaylistRequest,
        UpdateRoomPasswordRequest, UpdateUserPreferencesRequest, UpdateUserRoleRequest,
        UpdateUserUsernameRequest,
    };
    #[cfg(unix)]
    use synctv_proto::{
        admin as admin_proto, client as client_proto, common as common_proto,
        providers::{
            alist as alist_proto, bilibili as bilibili_proto, common as provider_common_proto,
            douyin as douyin_proto, emby as emby_proto, rtmp as rtmp_proto,
        },
    };
    #[cfg(unix)]
    use tonic::transport::Server;
    #[cfg(unix)]
    use tonic::{Request, Response, Status};

    struct EnvVarGuard {
        key: &'static str,
        value: Option<String>,
    }

    impl EnvVarGuard {
        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self {
                key,
                value: previous,
            }
        }

        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self {
                key,
                value: previous,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.value.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    struct CurrentDirGuard {
        previous: std::path::PathBuf,
    }

    impl CurrentDirGuard {
        fn change_to(path: &std::path::Path) -> Self {
            let previous = std::env::current_dir().expect("current dir should be readable");
            std::env::set_current_dir(path).expect("current dir should be settable");
            Self { previous }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            if std::env::set_current_dir(&self.previous).is_err() {
                std::env::set_current_dir(env!("CARGO_MANIFEST_DIR"))
                    .expect("crate root should be available as current dir fallback");
            }
        }
    }

    fn process_env_test_lock() -> &'static tokio::sync::Mutex<()> {
        static PROCESS_ENV_TEST_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        PROCESS_ENV_TEST_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    #[cfg(unix)]
    #[derive(Clone, Default)]
    struct TestManagementService {
        seen_authorization: Option<Arc<Mutex<Vec<Option<String>>>>>,
    }

    #[cfg(unix)]
    fn unavailable_test_management_response<T>() -> std::result::Result<Response<T>, Status> {
        Err(Status::unavailable(
            "test management service method is not configured",
        ))
    }

    #[cfg(unix)]
    #[tonic::async_trait]
    impl ManagementService for TestManagementService {
        type StopServerStream = Pin<
            Box<dyn tokio_stream::Stream<Item = Result<StopServerEvent, Status>> + Send + 'static>,
        >;

        async fn acfun_resolve(
            &self,
            _: Request<synctv_management::proto::AcfunResolveRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::acfun::ResolveResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn cctv_resolve(
            &self,
            _: Request<synctv_management::proto::CctvResolveRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::cctv::ResolveResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn douyu_resolve(
            &self,
            _: Request<synctv_management::proto::DouyuResolveRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::douyu::ResolveResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn huya_resolve(
            &self,
            _: Request<synctv_management::proto::HuyaResolveRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::huya::ResolveResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn youtube_bind(
            &self,
            _: Request<synctv_management::proto::YoutubeBindRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::youtube::BindResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn youtube_get_binds(
            &self,
            _: Request<synctv_management::proto::YoutubeGetBindsRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::youtube::GetBindsResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn youtube_unbind(
            &self,
            _: Request<synctv_management::proto::YoutubeUnbindRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::youtube::UnbindResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn youtube_resolve(
            &self,
            _: Request<synctv_management::proto::YoutubeResolveRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::youtube::ResolveResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn cloudreve_login(
            &self,
            _: Request<synctv_management::proto::CloudreveLoginRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::cloudreve::LoginResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn cloudreve_list(
            &self,
            _: Request<synctv_management::proto::CloudreveListRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::cloudreve::ListResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn cloudreve_search(
            &self,
            _: Request<synctv_management::proto::CloudreveSearchRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::cloudreve::SearchResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn cloudreve_get_me(
            &self,
            _: Request<synctv_management::proto::CloudreveGetMeRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::cloudreve::GetMeResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn cloudreve_logout(
            &self,
            _: Request<synctv_management::proto::CloudreveLogoutRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::cloudreve::LogoutResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn cloudreve_get_binds(
            &self,
            _: Request<synctv_management::proto::CloudreveGetBindsRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::cloudreve::GetBindsResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn fnos_login(
            &self,
            _: Request<synctv_management::proto::FnosLoginRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::fnos::LoginResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn fnos_list(
            &self,
            _: Request<synctv_management::proto::FnosListRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::fnos::ListResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn fnos_list_media_libraries(
            &self,
            _: Request<synctv_management::proto::FnosListMediaLibrariesRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::fnos::ListMediaLibrariesResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn fnos_list_media_items(
            &self,
            _: Request<synctv_management::proto::FnosListMediaItemsRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::fnos::ListMediaItemsResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn fnos_set_favorite(
            &self,
            _: Request<synctv_management::proto::FnosSetFavoriteRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::fnos::SetFavoriteResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn fnos_set_watched(
            &self,
            _: Request<synctv_management::proto::FnosSetWatchedRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::fnos::SetWatchedResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn fnos_get_server_info(
            &self,
            _: Request<synctv_management::proto::FnosGetServerInfoRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::fnos::GetServerInfoResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn fnos_logout(
            &self,
            _: Request<synctv_management::proto::FnosLogoutRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::fnos::LogoutResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn fnos_get_binds(
            &self,
            _: Request<synctv_management::proto::FnosGetBindsRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::fnos::GetBindsResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn nextcloud_login(
            &self,
            _: Request<synctv_management::proto::NextcloudLoginRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::nextcloud::LoginResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn nextcloud_start_login_flow(
            &self,
            _: Request<synctv_management::proto::NextcloudStartLoginFlowRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::nextcloud::StartLoginFlowResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn nextcloud_poll_login_flow(
            &self,
            _: Request<synctv_management::proto::NextcloudPollLoginFlowRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::nextcloud::LoginResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn nextcloud_list(
            &self,
            _: Request<synctv_management::proto::NextcloudListRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::nextcloud::ListResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn nextcloud_list_favorites(
            &self,
            _: Request<synctv_management::proto::NextcloudListFavoritesRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::nextcloud::ListResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn nextcloud_logout(
            &self,
            _: Request<synctv_management::proto::NextcloudLogoutRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::nextcloud::LogoutResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn nextcloud_get_binds(
            &self,
            _: Request<synctv_management::proto::NextcloudGetBindsRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::nextcloud::GetBindsResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn qnap_login(
            &self,
            _: Request<synctv_management::proto::QnapLoginRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::qnap::LoginResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn qnap_list(
            &self,
            _: Request<synctv_management::proto::QnapListRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::qnap::ListResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn qnap_get_capabilities(
            &self,
            _: Request<synctv_management::proto::QnapGetCapabilitiesRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::qnap::GetCapabilitiesResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn qnap_logout(
            &self,
            _: Request<synctv_management::proto::QnapLogoutRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::qnap::LogoutResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn qnap_get_binds(
            &self,
            _: Request<synctv_management::proto::QnapGetBindsRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::qnap::GetBindsResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn seafile_login(
            &self,
            _: Request<synctv_management::proto::SeafileLoginRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::seafile::LoginResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn seafile_unlock_library(
            &self,
            _: Request<synctv_management::proto::SeafileUnlockLibraryRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::seafile::UnlockLibraryResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn seafile_list_repositories(
            &self,
            _: Request<synctv_management::proto::SeafileListRepositoriesRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::seafile::ListResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn seafile_list(
            &self,
            _: Request<synctv_management::proto::SeafileListRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::seafile::ListResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn seafile_list_starred(
            &self,
            _: Request<synctv_management::proto::SeafileListStarredRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::seafile::ListResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn seafile_logout(
            &self,
            _: Request<synctv_management::proto::SeafileLogoutRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::seafile::LogoutResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn seafile_get_binds(
            &self,
            _: Request<synctv_management::proto::SeafileGetBindsRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::seafile::GetBindsResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn synology_login(
            &self,
            _: Request<synctv_management::proto::SynologyLoginRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::synology::LoginResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn synology_list_files(
            &self,
            _: Request<synctv_management::proto::SynologyListFilesRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::synology::ListFilesResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn synology_list_libraries(
            &self,
            _: Request<synctv_management::proto::SynologyListLibrariesRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::synology::ListLibrariesResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn synology_list_movies(
            &self,
            _: Request<synctv_management::proto::SynologyListMoviesRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::synology::ListVideoItemsResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn synology_list_tv_shows(
            &self,
            _: Request<synctv_management::proto::SynologyListTvShowsRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::synology::ListVideoItemsResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn synology_list_episodes(
            &self,
            _: Request<synctv_management::proto::SynologyListEpisodesRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::synology::ListVideoItemsResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn synology_list_home_videos(
            &self,
            _: Request<synctv_management::proto::SynologyListHomeVideosRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::synology::ListVideoItemsResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn synology_list_tv_recordings(
            &self,
            _: Request<synctv_management::proto::SynologyListTvRecordingsRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::synology::ListVideoItemsResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn synology_logout(
            &self,
            _: Request<synctv_management::proto::SynologyLogoutRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::synology::LogoutResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn synology_get_binds(
            &self,
            _: Request<synctv_management::proto::SynologyGetBindsRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::synology::GetBindsResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn truenas_login(
            &self,
            _: Request<synctv_management::proto::TruenasLoginRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::truenas::LoginResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn truenas_list(
            &self,
            _: Request<synctv_management::proto::TruenasListRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::truenas::ListResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn truenas_logout(
            &self,
            _: Request<synctv_management::proto::TruenasLogoutRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::truenas::LogoutResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn truenas_get_binds(
            &self,
            _: Request<synctv_management::proto::TruenasGetBindsRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::truenas::GetBindsResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn bilibili_list_live_areas(
            &self,
            _: Request<synctv_management::proto::BilibiliListLiveAreasRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::bilibili::ListLiveAreasResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn bilibili_list_favorite_folders(
            &self,
            _: Request<synctv_management::proto::BilibiliListFavoriteFoldersRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::bilibili::ListFavoriteFoldersResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn bilibili_list_followed_pgc(
            &self,
            _: Request<synctv_management::proto::BilibiliListFollowedPgcRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::bilibili::ListFollowedPgcResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn bilibili_list_history(
            &self,
            _: Request<synctv_management::proto::BilibiliListHistoryRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::bilibili::ListHistoryResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn bilibili_list_pgc_timeline(
            &self,
            _: Request<synctv_management::proto::BilibiliListPgcTimelineRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::bilibili::ListPgcTimelineResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn bilibili_list_pgc_seasons(
            &self,
            _: Request<synctv_management::proto::BilibiliListPgcSeasonsRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::bilibili::ListPgcSeasonsResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn twitch_list_followed_live(
            &self,
            _: Request<synctv_management::proto::TwitchListFollowedLiveRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::twitch::ListFollowedLiveResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn twitch_list_category_streams(
            &self,
            _: Request<synctv_management::proto::TwitchListCategoryStreamsRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::twitch::ListCategoryStreamsResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn twitch_list_top_categories(
            &self,
            _: Request<synctv_management::proto::TwitchListTopCategoriesRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::twitch::ListTopCategoriesResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn twitch_search_live_channels(
            &self,
            _: Request<synctv_management::proto::TwitchSearchLiveChannelsRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::twitch::SearchLiveChannelsResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn twitch_list_schedule(
            &self,
            _: Request<synctv_management::proto::TwitchListScheduleRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::twitch::ListScheduleResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }

        async fn get_service_state(
            &self,
            request: Request<GetServiceStateRequest>,
        ) -> std::result::Result<Response<admin_proto::GetServiceStateResponse>, Status> {
            if let Some(seen_authorization) = &self.seen_authorization {
                seen_authorization
                    .lock()
                    .expect("authorization sink mutex should not be poisoned")
                    .push(
                        request
                            .metadata()
                            .get("authorization")
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string),
                    );
            }
            Ok(Response::new(
                admin_proto::GetServiceStateResponse::default(),
            ))
        }

        async fn get_server_state(
            &self,
            _: Request<GetServerStateRequest>,
        ) -> std::result::Result<Response<GetServerStateResponse>, Status> {
            unavailable_test_management_response()
        }

        async fn get_slice_cache_stats(
            &self,
            _: Request<GetSliceCacheStatsRequest>,
        ) -> std::result::Result<Response<admin_proto::GetSliceCacheStatsResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn purge_slice_cache(
            &self,
            _: Request<PurgeSliceCacheRequest>,
        ) -> std::result::Result<Response<admin_proto::PurgeSliceCacheResponse>, Status> {
            unavailable_test_management_response()
        }

        async fn evict_expired_slice_cache(
            &self,
            _: Request<EvictExpiredSliceCacheRequest>,
        ) -> std::result::Result<Response<admin_proto::EvictExpiredSliceCacheResponse>, Status>
        {
            unavailable_test_management_response()
        }

        async fn list_users(
            &self,
            _: Request<ListUsersRequest>,
        ) -> std::result::Result<Response<admin_proto::ListUsersResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn get_user(
            &self,
            _: Request<GetUserRequest>,
        ) -> std::result::Result<Response<admin_proto::AdminUser>, Status> {
            unavailable_test_management_response()
        }
        async fn get_user_preferences(
            &self,
            _: Request<GetUserPreferencesRequest>,
        ) -> std::result::Result<Response<admin_proto::GetUserPreferencesResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn update_user_preferences(
            &self,
            _: Request<UpdateUserPreferencesRequest>,
        ) -> std::result::Result<Response<admin_proto::UpdateUserPreferencesResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn add_admin(
            &self,
            _: Request<AddAdminRequest>,
        ) -> std::result::Result<Response<admin_proto::AdminUser>, Status> {
            unavailable_test_management_response()
        }
        async fn remove_admin(
            &self,
            _: Request<RemoveAdminRequest>,
        ) -> std::result::Result<Response<admin_proto::RemoveAdminResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn list_admins(
            &self,
            _: Request<ListAdminsRequest>,
        ) -> std::result::Result<Response<admin_proto::ListAdminsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn create_user(
            &self,
            _: Request<CreateUserRequest>,
        ) -> std::result::Result<Response<admin_proto::AdminUser>, Status> {
            unavailable_test_management_response()
        }
        async fn delete_user(
            &self,
            _: Request<DeleteUserRequest>,
        ) -> std::result::Result<Response<admin_proto::DeleteUserResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn ban_user(
            &self,
            _: Request<BanUserRequest>,
        ) -> std::result::Result<Response<admin_proto::AdminUser>, Status> {
            unavailable_test_management_response()
        }
        async fn unban_user(
            &self,
            _: Request<UnbanUserRequest>,
        ) -> std::result::Result<Response<admin_proto::AdminUser>, Status> {
            unavailable_test_management_response()
        }
        async fn list_user_registration_reviews(
            &self,
            _: Request<ListUserRegistrationReviewsRequest>,
        ) -> std::result::Result<Response<admin_proto::ListUserRegistrationReviewsResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn approve_user_registration_review(
            &self,
            _: Request<ApproveUserRegistrationReviewRequest>,
        ) -> std::result::Result<Response<admin_proto::ApproveUserRegistrationReviewResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn reject_user_registration_review(
            &self,
            _: Request<RejectUserRegistrationReviewRequest>,
        ) -> std::result::Result<Response<admin_proto::UserRegistrationReview>, Status> {
            unavailable_test_management_response()
        }
        async fn list_room_creation_reviews(
            &self,
            _: Request<ListRoomCreationReviewsRequest>,
        ) -> std::result::Result<Response<admin_proto::ListRoomCreationReviewsResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn approve_room_creation_review(
            &self,
            _: Request<ApproveRoomCreationReviewRequest>,
        ) -> std::result::Result<Response<admin_proto::ApproveRoomCreationReviewResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn reject_room_creation_review(
            &self,
            _: Request<RejectRoomCreationReviewRequest>,
        ) -> std::result::Result<Response<admin_proto::RoomCreationReview>, Status> {
            unavailable_test_management_response()
        }
        async fn list_room_join_reviews(
            &self,
            _: Request<ListRoomJoinReviewsRequest>,
        ) -> std::result::Result<Response<admin_proto::ListRoomJoinReviewsResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn approve_room_join_review(
            &self,
            _: Request<ApproveRoomJoinReviewRequest>,
        ) -> std::result::Result<Response<admin_proto::ApproveRoomJoinReviewResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn reject_room_join_review(
            &self,
            _: Request<RejectRoomJoinReviewRequest>,
        ) -> std::result::Result<Response<admin_proto::RoomJoinReview>, Status> {
            unavailable_test_management_response()
        }
        async fn list_ban_records(
            &self,
            _: Request<ListBanRecordsRequest>,
        ) -> std::result::Result<Response<admin_proto::ListBanRecordsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn update_user_role(
            &self,
            _: Request<UpdateUserRoleRequest>,
        ) -> std::result::Result<Response<admin_proto::AdminUser>, Status> {
            unavailable_test_management_response()
        }
        async fn set_user_password(
            &self,
            _: Request<SetUserPasswordRequest>,
        ) -> std::result::Result<Response<admin_proto::SetUserPasswordResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn update_user_username(
            &self,
            _: Request<UpdateUserUsernameRequest>,
        ) -> std::result::Result<Response<admin_proto::AdminUser>, Status> {
            unavailable_test_management_response()
        }
        async fn get_user_rooms(
            &self,
            _: Request<GetUserRoomsRequest>,
        ) -> std::result::Result<Response<admin_proto::GetUserRoomsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn batch_ban_users(
            &self,
            _: Request<BatchBanUsersRequest>,
        ) -> std::result::Result<Response<admin_proto::BatchBanUsersResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn batch_delete_users(
            &self,
            _: Request<BatchDeleteUsersRequest>,
        ) -> std::result::Result<Response<admin_proto::BatchDeleteUsersResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn create_room(
            &self,
            _: Request<CreateRoomRequest>,
        ) -> std::result::Result<Response<client_proto::Room>, Status> {
            unavailable_test_management_response()
        }
        async fn list_rooms(
            &self,
            _: Request<ListRoomsRequest>,
        ) -> std::result::Result<Response<admin_proto::ListRoomsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn get_room(
            &self,
            _: Request<GetRoomRequest>,
        ) -> std::result::Result<Response<admin_proto::Room>, Status> {
            unavailable_test_management_response()
        }
        async fn favorite_room(
            &self,
            _: Request<FavoriteRoomRequest>,
        ) -> std::result::Result<Response<client_proto::FavoriteRoomResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn unfavorite_room(
            &self,
            _: Request<UnfavoriteRoomRequest>,
        ) -> std::result::Result<Response<client_proto::UnfavoriteRoomResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn list_favorite_rooms(
            &self,
            _: Request<ListFavoriteRoomsRequest>,
        ) -> std::result::Result<Response<client_proto::ListFavoriteRoomsResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn list_room_categories(
            &self,
            _: Request<admin_proto::ListRoomCategoriesRequest>,
        ) -> std::result::Result<Response<admin_proto::ListRoomCategoriesResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn upsert_room_category(
            &self,
            _: Request<admin_proto::UpsertRoomCategoryRequest>,
        ) -> std::result::Result<Response<client_proto::RoomCategory>, Status> {
            unavailable_test_management_response()
        }
        async fn delete_room_category(
            &self,
            _: Request<admin_proto::DeleteRoomCategoryRequest>,
        ) -> std::result::Result<Response<admin_proto::DeleteRoomCategoryResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn list_room_labels(
            &self,
            _: Request<admin_proto::ListRoomLabelsRequest>,
        ) -> std::result::Result<Response<admin_proto::ListRoomLabelsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn upsert_room_label(
            &self,
            _: Request<admin_proto::UpsertRoomLabelRequest>,
        ) -> std::result::Result<Response<client_proto::RoomLabel>, Status> {
            unavailable_test_management_response()
        }
        async fn delete_room_label(
            &self,
            _: Request<admin_proto::DeleteRoomLabelRequest>,
        ) -> std::result::Result<Response<admin_proto::DeleteRoomLabelResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn update_room_taxonomy(
            &self,
            _: Request<admin_proto::UpdateRoomTaxonomyRequest>,
        ) -> std::result::Result<Response<admin_proto::Room>, Status> {
            unavailable_test_management_response()
        }
        async fn get_room_members(
            &self,
            _: Request<GetRoomMembersRequest>,
        ) -> std::result::Result<Response<admin_proto::GetRoomMembersResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn search_chat_messages(
            &self,
            _: Request<SearchChatMessagesRequest>,
        ) -> std::result::Result<Response<client_proto::SearchChatMessagesResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn add_member(
            &self,
            _: Request<AddMemberRequest>,
        ) -> std::result::Result<Response<common_proto::RoomMember>, Status> {
            unavailable_test_management_response()
        }
        async fn update_member_remark_name(
            &self,
            _: Request<UpdateMemberRemarkNameRequest>,
        ) -> std::result::Result<Response<common_proto::RoomMember>, Status> {
            unavailable_test_management_response()
        }
        async fn update_member_display_tag(
            &self,
            _: Request<UpdateMemberDisplayTagRequest>,
        ) -> std::result::Result<Response<common_proto::RoomMember>, Status> {
            unavailable_test_management_response()
        }
        async fn update_member_permissions(
            &self,
            _: Request<UpdateMemberPermissionsRequest>,
        ) -> std::result::Result<Response<common_proto::RoomMember>, Status> {
            unavailable_test_management_response()
        }
        async fn kick_member(
            &self,
            _: Request<KickMemberRequest>,
        ) -> std::result::Result<Response<client_proto::KickMemberResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn get_room_settings(
            &self,
            _: Request<GetRoomSettingsRequest>,
        ) -> std::result::Result<Response<admin_proto::GetRoomSettingsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn update_room_settings(
            &self,
            _: Request<admin_proto::UpdateRoomSettingsRequest>,
        ) -> std::result::Result<Response<admin_proto::Room>, Status> {
            unavailable_test_management_response()
        }
        async fn reset_room_settings(
            &self,
            _: Request<ResetRoomSettingsRequest>,
        ) -> std::result::Result<Response<admin_proto::Room>, Status> {
            unavailable_test_management_response()
        }
        async fn transfer_room_ownership(
            &self,
            _: Request<TransferRoomOwnershipRequest>,
        ) -> std::result::Result<Response<client_proto::Room>, Status> {
            unavailable_test_management_response()
        }
        async fn update_room_password(
            &self,
            _: Request<UpdateRoomPasswordRequest>,
        ) -> std::result::Result<Response<admin_proto::UpdateRoomPasswordResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn ban_room(
            &self,
            _: Request<BanRoomRequest>,
        ) -> std::result::Result<Response<admin_proto::Room>, Status> {
            unavailable_test_management_response()
        }
        async fn unban_room(
            &self,
            _: Request<UnbanRoomRequest>,
        ) -> std::result::Result<Response<admin_proto::Room>, Status> {
            unavailable_test_management_response()
        }
        async fn delete_room(
            &self,
            _: Request<DeleteRoomRequest>,
        ) -> std::result::Result<Response<admin_proto::DeleteRoomResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn batch_ban_rooms(
            &self,
            _: Request<BatchBanRoomsRequest>,
        ) -> std::result::Result<Response<admin_proto::BatchBanRoomsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn batch_delete_rooms(
            &self,
            _: Request<BatchDeleteRoomsRequest>,
        ) -> std::result::Result<Response<admin_proto::BatchDeleteRoomsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn start_playback(
            &self,
            _: Request<StartPlaybackRequest>,
        ) -> std::result::Result<Response<client_proto::PlaybackState>, Status> {
            unavailable_test_management_response()
        }
        async fn stop_playback(
            &self,
            _: Request<StopPlaybackRequest>,
        ) -> std::result::Result<Response<client_proto::PlaybackState>, Status> {
            unavailable_test_management_response()
        }
        async fn get_playback(
            &self,
            _: Request<GetPlaybackRequest>,
        ) -> std::result::Result<Response<client_proto::GetPlaybackResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn update_playback_state(
            &self,
            _: Request<UpdatePlaybackStateRequest>,
        ) -> std::result::Result<Response<client_proto::PlaybackState>, Status> {
            unavailable_test_management_response()
        }
        async fn create_publish_key(
            &self,
            _: Request<CreatePublishKeyRequest>,
        ) -> std::result::Result<Response<rtmp_proto::CreatePublishKeyResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn get_stream_info(
            &self,
            _: Request<GetStreamInfoRequest>,
        ) -> std::result::Result<Response<rtmp_proto::GetStreamInfoResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn list_room_streams(
            &self,
            _: Request<ListRoomStreamsRequest>,
        ) -> std::result::Result<Response<client_proto::ListRoomStreamsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn kick_room_stream(
            &self,
            _: Request<KickRoomStreamRequest>,
        ) -> std::result::Result<Response<client_proto::KickRoomStreamResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn list_playlists(
            &self,
            _: Request<ListPlaylistsRequest>,
        ) -> std::result::Result<Response<client_proto::ListPlaylistsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn get_playlist(
            &self,
            _: Request<GetPlaylistRequest>,
        ) -> std::result::Result<Response<client_proto::GetPlaylistResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn create_playlist(
            &self,
            _: Request<CreatePlaylistRequest>,
        ) -> std::result::Result<Response<client_proto::Playlist>, Status> {
            unavailable_test_management_response()
        }
        async fn create_alist_playlist(
            &self,
            _: Request<CreateAlistPlaylistRequest>,
        ) -> std::result::Result<Response<client_proto::Playlist>, Status> {
            unavailable_test_management_response()
        }
        async fn create_emby_playlist(
            &self,
            _: Request<CreateEmbyPlaylistRequest>,
        ) -> std::result::Result<Response<client_proto::Playlist>, Status> {
            unavailable_test_management_response()
        }
        async fn update_playlist(
            &self,
            _: Request<UpdatePlaylistRequest>,
        ) -> std::result::Result<Response<client_proto::Playlist>, Status> {
            unavailable_test_management_response()
        }
        async fn move_playlist(
            &self,
            _: Request<MovePlaylistRequest>,
        ) -> std::result::Result<Response<client_proto::Playlist>, Status> {
            unavailable_test_management_response()
        }
        async fn delete_playlist(
            &self,
            _: Request<DeletePlaylistRequest>,
        ) -> std::result::Result<Response<client_proto::DeletePlaylistResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn list_media(
            &self,
            _: Request<ListMediaRequest>,
        ) -> std::result::Result<Response<client_proto::ListPlaylistItemsResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn add_media(
            &self,
            _: Request<AddMediaRequest>,
        ) -> std::result::Result<Response<client_proto::Media>, Status> {
            unavailable_test_management_response()
        }
        async fn add_direct_url_media(
            &self,
            _: Request<AddDirectUrlMediaRequest>,
        ) -> std::result::Result<Response<client_proto::Media>, Status> {
            unavailable_test_management_response()
        }
        async fn add_alist_media(
            &self,
            _: Request<AddAlistMediaRequest>,
        ) -> std::result::Result<Response<client_proto::Media>, Status> {
            unavailable_test_management_response()
        }
        async fn add_emby_media(
            &self,
            _: Request<AddEmbyMediaRequest>,
        ) -> std::result::Result<Response<client_proto::Media>, Status> {
            unavailable_test_management_response()
        }
        async fn add_bilibili_video_media(
            &self,
            _: Request<AddBilibiliVideoMediaRequest>,
        ) -> std::result::Result<Response<client_proto::Media>, Status> {
            unavailable_test_management_response()
        }
        async fn add_bilibili_pgc_media(
            &self,
            _: Request<AddBilibiliPgcMediaRequest>,
        ) -> std::result::Result<Response<client_proto::Media>, Status> {
            unavailable_test_management_response()
        }
        async fn add_bilibili_live_media(
            &self,
            _: Request<AddBilibiliLiveMediaRequest>,
        ) -> std::result::Result<Response<client_proto::Media>, Status> {
            unavailable_test_management_response()
        }
        async fn edit_media(
            &self,
            _: Request<EditMediaRequest>,
        ) -> std::result::Result<Response<client_proto::Media>, Status> {
            unavailable_test_management_response()
        }
        async fn move_media(
            &self,
            _: Request<MoveMediaRequest>,
        ) -> std::result::Result<Response<client_proto::MoveMediaResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn delete_media(
            &self,
            _: Request<DeleteMediaRequest>,
        ) -> std::result::Result<Response<client_proto::DeleteMediaResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn alist_login(
            &self,
            _: Request<AlistLoginRequest>,
        ) -> std::result::Result<Response<alist_proto::LoginResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn alist_list(
            &self,
            _: Request<AlistListRequest>,
        ) -> std::result::Result<Response<alist_proto::ListResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn alist_search(
            &self,
            _: Request<AlistSearchRequest>,
        ) -> std::result::Result<Response<alist_proto::SearchResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn alist_get_me(
            &self,
            _: Request<AlistGetMeRequest>,
        ) -> std::result::Result<Response<alist_proto::GetMeResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn alist_logout(
            &self,
            _: Request<AlistLogoutRequest>,
        ) -> std::result::Result<Response<alist_proto::LogoutResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn alist_get_binds(
            &self,
            _: Request<AlistGetBindsRequest>,
        ) -> std::result::Result<Response<alist_proto::GetBindsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn emby_login(
            &self,
            _: Request<EmbyLoginRequest>,
        ) -> std::result::Result<Response<emby_proto::LoginResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn emby_list(
            &self,
            _: Request<EmbyListRequest>,
        ) -> std::result::Result<Response<emby_proto::ListResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn emby_get_me(
            &self,
            _: Request<EmbyGetMeRequest>,
        ) -> std::result::Result<Response<emby_proto::GetMeResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn emby_logout(
            &self,
            _: Request<EmbyLogoutRequest>,
        ) -> std::result::Result<Response<emby_proto::LogoutResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn emby_get_binds(
            &self,
            _: Request<EmbyGetBindsRequest>,
        ) -> std::result::Result<Response<emby_proto::GetBindsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn douyin_bind(
            &self,
            _: Request<DouyinBindRequest>,
        ) -> std::result::Result<Response<douyin_proto::BindResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn douyin_get_binds(
            &self,
            _: Request<DouyinGetBindsRequest>,
        ) -> std::result::Result<Response<douyin_proto::GetBindsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn douyin_unbind(
            &self,
            _: Request<DouyinUnbindRequest>,
        ) -> std::result::Result<Response<douyin_proto::UnbindResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn douyin_resolve(
            &self,
            _: Request<DouyinResolveRequest>,
        ) -> std::result::Result<Response<douyin_proto::ResolveResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn douyin_list_user_posts(
            &self,
            _: Request<DouyinListUserPostsRequest>,
        ) -> std::result::Result<Response<douyin_proto::ListUserPostsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn tik_tok_bind(
            &self,
            _: Request<synctv_management::proto::TikTokBindRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::tiktok::BindResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn tik_tok_get_binds(
            &self,
            _: Request<synctv_management::proto::TikTokGetBindsRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::tiktok::GetBindsResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn tik_tok_unbind(
            &self,
            _: Request<synctv_management::proto::TikTokUnbindRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::tiktok::UnbindResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn tik_tok_resolve(
            &self,
            _: Request<synctv_management::proto::TikTokResolveRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::tiktok::ResolveResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn tik_tok_get_user(
            &self,
            _: Request<synctv_management::proto::TikTokGetUserRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::tiktok::GetUserResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn tik_tok_list_user_posts(
            &self,
            _: Request<synctv_management::proto::TikTokListUserPostsRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::tiktok::ListUserPostsResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }
        async fn twitch_bind(
            &self,
            _: Request<synctv_management::proto::TwitchBindRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::twitch::BindResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn twitch_get_binds(
            &self,
            _: Request<synctv_management::proto::TwitchGetBindsRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::twitch::GetBindsResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn twitch_unbind(
            &self,
            _: Request<synctv_management::proto::TwitchUnbindRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::twitch::UnbindResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn twitch_resolve(
            &self,
            _: Request<synctv_management::proto::TwitchResolveRequest>,
        ) -> std::result::Result<Response<synctv_proto::providers::twitch::ResolveResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn twitch_list_channel_items(
            &self,
            _: Request<synctv_management::proto::TwitchListChannelItemsRequest>,
        ) -> std::result::Result<
            Response<synctv_proto::providers::twitch::ListChannelItemsResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }
        async fn bilibili_parse(
            &self,
            _: Request<BilibiliParseRequest>,
        ) -> std::result::Result<Response<bilibili_proto::ParseResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn bilibili_login_qr(
            &self,
            _: Request<BilibiliLoginQrRequest>,
        ) -> std::result::Result<Response<bilibili_proto::QrCodeResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn bilibili_check_qr(
            &self,
            _: Request<BilibiliCheckQrRequest>,
        ) -> std::result::Result<Response<bilibili_proto::QrStatusResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn bilibili_start_sms_login(
            &self,
            _: Request<BilibiliStartSmsLoginRequest>,
        ) -> std::result::Result<Response<bilibili_proto::StartSmsLoginResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn bilibili_send_sms(
            &self,
            _: Request<BilibiliSendSmsRequest>,
        ) -> std::result::Result<Response<bilibili_proto::SendSmsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn bilibili_login_sms(
            &self,
            _: Request<BilibiliLoginSmsRequest>,
        ) -> std::result::Result<Response<bilibili_proto::LoginSmsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn bilibili_get_user_info(
            &self,
            _: Request<BilibiliGetUserInfoRequest>,
        ) -> std::result::Result<Response<bilibili_proto::UserInfoResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn bilibili_logout(
            &self,
            _: Request<BilibiliLogoutRequest>,
        ) -> std::result::Result<Response<bilibili_proto::LogoutResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn bilibili_get_binds(
            &self,
            _: Request<BilibiliGetBindsRequest>,
        ) -> std::result::Result<Response<bilibili_proto::GetBindsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn list_available_provider_instances(
            &self,
            _: Request<provider_common_proto::ListAvailableProviderInstancesRequest>,
        ) -> std::result::Result<Response<provider_common_proto::ProviderInstancesResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn list_provider_backends(
            &self,
            _: Request<provider_common_proto::ListProviderBackendsRequest>,
        ) -> std::result::Result<Response<provider_common_proto::ProviderBackendsResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn list_provider_instances(
            &self,
            _: Request<provider_common_proto::ListProviderInstancesRequest>,
        ) -> std::result::Result<
            Response<provider_common_proto::ListProviderInstancesResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }
        async fn add_provider_instance(
            &self,
            _: Request<provider_common_proto::AddProviderInstanceRequest>,
        ) -> std::result::Result<Response<provider_common_proto::AddProviderInstanceResponse>, Status>
        {
            unavailable_test_management_response()
        }
        async fn update_provider_instance(
            &self,
            _: Request<provider_common_proto::UpdateProviderInstanceRequest>,
        ) -> std::result::Result<
            Response<provider_common_proto::UpdateProviderInstanceResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }
        async fn delete_provider_instance(
            &self,
            _: Request<provider_common_proto::DeleteProviderInstanceRequest>,
        ) -> std::result::Result<
            Response<provider_common_proto::DeleteProviderInstanceResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }
        async fn reconnect_provider_instance(
            &self,
            _: Request<provider_common_proto::ReconnectProviderInstanceRequest>,
        ) -> std::result::Result<
            Response<provider_common_proto::ReconnectProviderInstanceResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }
        async fn enable_provider_instance(
            &self,
            _: Request<provider_common_proto::EnableProviderInstanceRequest>,
        ) -> std::result::Result<
            Response<provider_common_proto::EnableProviderInstanceResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }
        async fn disable_provider_instance(
            &self,
            _: Request<provider_common_proto::DisableProviderInstanceRequest>,
        ) -> std::result::Result<
            Response<provider_common_proto::DisableProviderInstanceResponse>,
            Status,
        > {
            unavailable_test_management_response()
        }
        async fn get_settings(
            &self,
            _: Request<GetSettingsRequest>,
        ) -> std::result::Result<Response<admin_proto::RuntimeSettings>, Status> {
            unavailable_test_management_response()
        }
        async fn update_settings(
            &self,
            _: Request<admin_proto::UpdateSettingsRequest>,
        ) -> std::result::Result<Response<admin_proto::RuntimeSettings>, Status> {
            unavailable_test_management_response()
        }
        async fn export_settings(
            &self,
            _: Request<admin_proto::ExportSettingsRequest>,
        ) -> std::result::Result<Response<admin_proto::RuntimeSettingsSnapshot>, Status> {
            unavailable_test_management_response()
        }
        async fn import_settings(
            &self,
            _: Request<admin_proto::ImportSettingsRequest>,
        ) -> std::result::Result<Response<admin_proto::ImportSettingsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn send_test_email(
            &self,
            _: Request<SendTestEmailRequest>,
        ) -> std::result::Result<Response<admin_proto::SendTestEmailResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn list_active_streams(
            &self,
            _: Request<ListActiveStreamsRequest>,
        ) -> std::result::Result<Response<admin_proto::ListActiveStreamsResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn kick_stream(
            &self,
            _: Request<KickStreamRequest>,
        ) -> std::result::Result<Response<admin_proto::KickStreamResponse>, Status> {
            unavailable_test_management_response()
        }
        async fn stop_server(
            &self,
            _: Request<StopServerRequest>,
        ) -> std::result::Result<
            Response<
                Pin<Box<dyn tokio_stream::Stream<Item = Result<StopServerEvent, Status>> + Send>>,
            >,
            Status,
        > {
            Ok(Response::new(Box::pin(stream::empty())))
        }
    }

    #[test]
    fn normalize_endpoint_adds_http_scheme() {
        let normalized =
            normalize_endpoint("127.0.0.1:8080").expect("endpoint without scheme should normalize");
        assert_eq!(normalized, "http://127.0.0.1:8080/");
    }

    #[test]
    fn normalize_endpoint_preserves_https_scheme() {
        let normalized =
            normalize_endpoint("https://example.com/api").expect("https endpoint should normalize");
        assert_eq!(normalized, "https://example.com/api");
    }

    #[test]
    fn resolve_management_auth_token_prefers_explicit_cli_token() {
        let _env_lock = process_env_test_lock().blocking_lock();
        let _env_guard = EnvVarGuard::set("SYNCTV_MANAGEMENT_AUTH_TOKEN", "env-token");

        let token = resolve_management_auth_token(&AdminConnectionOptions {
            endpoint: Some("http://127.0.0.1:50052".to_string()),
            auth_token: Some("cli-token".to_string()),
            auth_token_file: None,
            config_path: None,
            data_dir: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: false,
        })
        .expect("explicit CLI token should resolve");

        assert_eq!(token.as_deref(), Some("cli-token"));
    }

    #[test]
    fn resolve_management_auth_token_reads_explicit_cli_token_file() {
        let _env_lock = process_env_test_lock().blocking_lock();
        let _env_guard = EnvVarGuard::remove("SYNCTV_MANAGEMENT_AUTH_TOKEN");
        let _env_file_guard = EnvVarGuard::remove("SYNCTV_MANAGEMENT_AUTH_TOKEN_FILE");
        let dir = tempdir().expect("temp dir should be created");
        let token_path = dir.path().join("management.token");
        std::fs::write(&token_path, "file-token\n").expect("token file should be written");

        let token = resolve_management_auth_token(&AdminConnectionOptions {
            endpoint: Some("http://127.0.0.1:50052".to_string()),
            auth_token: None,
            auth_token_file: Some(token_path.to_string_lossy().to_string()),
            config_path: None,
            data_dir: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: false,
        })
        .expect("explicit CLI token file should resolve");

        assert_eq!(token.as_deref(), Some("file-token"));
    }

    #[test]
    fn resolve_management_auth_token_reads_env_token_file_for_explicit_endpoint() {
        let _env_lock = process_env_test_lock().blocking_lock();
        let _env_guard = EnvVarGuard::remove("SYNCTV_MANAGEMENT_AUTH_TOKEN");
        let dir = tempdir().expect("temp dir should be created");
        let token_path = dir.path().join("management.token");
        std::fs::write(&token_path, "env-file-token\n").expect("token file should be written");
        let token_path = token_path.to_string_lossy().to_string();
        let _env_file_guard =
            EnvVarGuard::set("SYNCTV_MANAGEMENT_AUTH_TOKEN_FILE", token_path.as_str());

        let token = resolve_management_auth_token(&AdminConnectionOptions {
            endpoint: Some("http://127.0.0.1:50052".to_string()),
            auth_token: None,
            auth_token_file: None,
            config_path: None,
            data_dir: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: false,
        })
        .expect("env token file should resolve in explicit endpoint mode");

        assert_eq!(token.as_deref(), Some("env-file-token"));
    }

    #[cfg(unix)]
    #[test]
    fn normalize_endpoint_preserves_unix_socket_scheme() {
        let raw = format!("unix://{}", default_management_unix_socket_path().display());
        let normalized = normalize_endpoint(&raw).expect("unix socket endpoint should normalize");
        assert_eq!(normalized, raw);
    }

    #[test]
    fn normalize_endpoint_resolves_relative_unix_socket_path() {
        let _env_lock = process_env_test_lock().blocking_lock();
        let dir = tempdir().expect("temp dir should be created");
        let _cwd = CurrentDirGuard::change_to(dir.path());

        let normalized = normalize_endpoint("./run/synctv.sock")
            .expect("relative unix socket endpoint should normalize");
        let current_dir = std::env::current_dir().expect("current dir should be readable");

        assert_eq!(
            normalized,
            format!(
                "unix://{}",
                current_dir.join("run").join("synctv.sock").display()
            )
        );
    }

    #[test]
    fn normalize_endpoint_recognizes_unix_socket_filename() {
        let _env_lock = process_env_test_lock().blocking_lock();
        let dir = tempdir().expect("temp dir should be created");
        let _cwd = CurrentDirGuard::change_to(dir.path());

        let normalized =
            normalize_endpoint("synctv.sock").expect("unix socket filename should normalize");
        let current_dir = std::env::current_dir().expect("current dir should be readable");

        assert_eq!(
            normalized,
            format!("unix://{}", current_dir.join("synctv.sock").display())
        );
    }

    #[test]
    fn normalize_endpoint_preserves_explicit_http_url_ending_in_sock() {
        let normalized = normalize_endpoint("http://example.com/synctv.sock")
            .expect("explicit HTTP endpoint should normalize");

        assert_eq!(normalized, "http://example.com/synctv.sock");
    }

    #[test]
    fn normalize_endpoint_resolves_relative_unix_socket_scheme() {
        let _env_lock = process_env_test_lock().blocking_lock();
        let dir = tempdir().expect("temp dir should be created");
        let _cwd = CurrentDirGuard::change_to(dir.path());

        let normalized = normalize_endpoint("unix://./run/synctv.sock")
            .expect("relative unix socket endpoint with scheme should normalize");
        let current_dir = std::env::current_dir().expect("current dir should be readable");

        assert_eq!(
            normalized,
            format!(
                "unix://{}",
                current_dir.join("run").join("synctv.sock").display()
            )
        );
    }

    #[test]
    fn resolve_candidate_endpoints_prefers_default_unix_then_tcp() {
        let _env_lock = process_env_test_lock().blocking_lock();
        let dir = tempdir().expect("temp dir should be created");
        let _cwd = CurrentDirGuard::change_to(dir.path());
        let _env = EnvVarGuard::remove("SYNCTV_CONFIG_PATH");

        let endpoints = resolve_candidate_endpoints(&AdminConnectionOptions {
            endpoint: None,
            auth_token: None,
            auth_token_file: None,
            config_path: None,
            data_dir: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: false,
        })
        .expect("default admin endpoints should resolve");
        #[cfg(unix)]
        assert_eq!(
            endpoints,
            vec![format!(
                "unix://{}",
                default_management_unix_socket_path().display()
            )]
        );

        #[cfg(not(unix))]
        assert_eq!(endpoints, vec!["http://127.0.0.1:50052/".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_candidate_endpoints_uses_explicit_data_dir_for_default_unix_socket() {
        let _env_lock = process_env_test_lock().blocking_lock();
        let dir = tempdir().expect("temp dir should be created");
        let _cwd = CurrentDirGuard::change_to(dir.path());
        let _env = EnvVarGuard::remove("SYNCTV_CONFIG_PATH");
        let data_dir = dir.path().join("state");

        let endpoints = resolve_candidate_endpoints(&AdminConnectionOptions {
            endpoint: None,
            auth_token: None,
            auth_token_file: None,
            config_path: None,
            data_dir: Some(data_dir.to_string_lossy().to_string()),
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: false,
        })
        .expect("data_dir-derived admin endpoint should resolve");

        assert_eq!(
            endpoints,
            vec![format!(
                "unix://{}",
                data_dir.join("run").join("synctv.sock").display()
            )]
        );
    }

    #[test]
    fn resolve_candidate_endpoints_prefers_explicit_config_file_management_endpoint() {
        let dir = tempdir().expect("temp dir should be created");
        let config_path = dir.path().join("synctv.yaml");
        std::fs::write(
            &config_path,
            r#"
management:
  transport: "tcp"
  port: 50123
"#,
        )
        .expect("config should be written");

        let endpoints = resolve_candidate_endpoints(&AdminConnectionOptions {
            endpoint: None,
            auth_token: None,
            auth_token_file: None,
            config_path: Some(config_path.to_string_lossy().to_string()),
            data_dir: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: false,
        })
        .expect("configured management endpoint should resolve");

        assert_eq!(endpoints, vec!["http://127.0.0.1:50123/".to_string()]);
    }

    #[test]
    fn resolve_candidate_endpoints_errors_for_missing_explicit_config_file() {
        let _env_lock = process_env_test_lock().blocking_lock();
        let dir = tempdir().expect("temp dir should be created");
        let missing_path = dir.path().join("missing-synctv.yaml");

        let error = resolve_candidate_endpoints(&AdminConnectionOptions {
            endpoint: None,
            auth_token: None,
            auth_token_file: None,
            config_path: Some(missing_path.to_string_lossy().to_string()),
            data_dir: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: false,
        })
        .expect_err("missing explicit config path must fail closed");

        assert!(
            error
                .to_string()
                .contains("Config file not found at explicitly set CLI --config"),
            "unexpected error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_admin_session_connects_over_unix_socket() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let socket_path = temp_dir.path().join("management.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path)
            .expect("unix management listener should bind");
        let service = TestManagementService::default();

        let serve_handle = tokio::spawn(async move {
            Server::builder()
                .add_service(ManagementServiceServer::new(service))
                .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
                .await
                .expect("unix management server should serve");
        });

        let endpoint = format!("unix://{}", socket_path.display());
        let session = RemoteAdminSession::connect(AdminConnectionOptions {
            endpoint: Some(endpoint.clone()),
            auth_token: None,
            auth_token_file: None,
            config_path: None,
            data_dir: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: false,
        })
        .await
        .expect("remote admin session should connect via unix socket");

        assert_eq!(session.endpoint(), endpoint);
        session
            .management_client()
            .get_service_state(GetServiceStateRequest {})
            .await
            .expect("management client should call get_service_state over unix socket");

        serve_handle.abort();
        let _ = serve_handle.await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_admin_session_injects_management_bearer_token_from_config() {
        let _env_lock = process_env_test_lock().lock().await;
        let _env_guard = EnvVarGuard::remove("SYNCTV_MANAGEMENT_AUTH_TOKEN");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let socket_path = temp_dir.path().join("management.sock");
        let config_path = temp_dir.path().join("synctv.yaml");
        std::fs::write(
            &config_path,
            format!(
                r#"
management:
  transport: "unix"
  unix_socket_path: "{}"
  auth_token: "management-secret-token"
"#,
                socket_path.display()
            ),
        )
        .expect("config should be written");

        let listener = tokio::net::UnixListener::bind(&socket_path)
            .expect("unix management listener should bind");
        let seen_authorization = Arc::new(Mutex::new(Vec::new()));
        let service = TestManagementService {
            seen_authorization: Some(Arc::clone(&seen_authorization)),
        };

        let serve_handle = tokio::spawn(async move {
            Server::builder()
                .add_service(ManagementServiceServer::new(service))
                .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
                .await
                .expect("unix management server should serve");
        });

        let session = RemoteAdminSession::connect(AdminConnectionOptions {
            endpoint: None,
            auth_token: None,
            auth_token_file: None,
            config_path: Some(config_path.to_string_lossy().to_string()),
            data_dir: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: false,
        })
        .await
        .expect("remote admin session should connect via unix socket");

        session
            .management_client()
            .get_service_state(GetServiceStateRequest {})
            .await
            .expect("management client should send get_service_state");

        assert_eq!(
            seen_authorization
                .lock()
                .expect("authorization sink mutex should not be poisoned")
                .as_slice(),
            &[Some("Bearer management-secret-token".to_string())],
            "management client must inject the configured management bearer token",
        );

        serve_handle.abort();
        let _ = serve_handle.await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_admin_session_does_not_inherit_config_token_for_explicit_endpoint() {
        let _env_lock = process_env_test_lock().lock().await;
        let _env_guard = EnvVarGuard::remove("SYNCTV_MANAGEMENT_AUTH_TOKEN");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let socket_path = temp_dir.path().join("management.sock");
        let config_path = temp_dir.path().join("synctv.yaml");
        std::fs::write(
            &config_path,
            format!(
                r#"
management:
  transport: "unix"
  unix_socket_path: "{}"
  auth_token: "management-secret-token"
"#,
                socket_path.display()
            ),
        )
        .expect("config should be written");

        let listener = tokio::net::UnixListener::bind(&socket_path)
            .expect("unix management listener should bind");
        let seen_authorization = Arc::new(Mutex::new(Vec::new()));
        let service = TestManagementService {
            seen_authorization: Some(Arc::clone(&seen_authorization)),
        };

        let serve_handle = tokio::spawn(async move {
            Server::builder()
                .add_service(ManagementServiceServer::new(service))
                .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
                .await
                .expect("unix management server should serve");
        });

        let endpoint = format!("unix://{}", socket_path.display());
        let session = RemoteAdminSession::connect(AdminConnectionOptions {
            endpoint: Some(endpoint),
            auth_token: None,
            auth_token_file: None,
            config_path: Some(config_path.to_string_lossy().to_string()),
            data_dir: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: false,
        })
        .await
        .expect("remote admin session should connect via explicit unix socket");

        session
            .management_client()
            .get_service_state(GetServiceStateRequest {})
            .await
            .expect("management client should send get_service_state");

        assert_eq!(
            seen_authorization
                .lock()
                .expect("authorization sink mutex should not be poisoned")
                .as_slice(),
            &[None],
            "explicit endpoint overrides must not reuse config-managed auth tokens",
        );

        serve_handle.abort();
        let _ = serve_handle.await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_admin_session_can_opt_in_to_config_token_for_explicit_endpoint() {
        let _env_lock = process_env_test_lock().lock().await;
        let _env_guard = EnvVarGuard::remove("SYNCTV_MANAGEMENT_AUTH_TOKEN");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let socket_path = temp_dir.path().join("management.sock");
        let config_path = temp_dir.path().join("synctv.yaml");
        std::fs::write(
            &config_path,
            format!(
                r#"
management:
  transport: "unix"
  unix_socket_path: "{}"
  auth_token: "management-secret-token"
"#,
                socket_path.display()
            ),
        )
        .expect("config should be written");

        let listener = tokio::net::UnixListener::bind(&socket_path)
            .expect("unix management listener should bind");
        let seen_authorization = Arc::new(Mutex::new(Vec::new()));
        let service = TestManagementService {
            seen_authorization: Some(Arc::clone(&seen_authorization)),
        };

        let serve_handle = tokio::spawn(async move {
            Server::builder()
                .add_service(ManagementServiceServer::new(service))
                .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
                .await
                .expect("unix management server should serve");
        });

        let endpoint = format!("unix://{}", socket_path.display());
        let session = RemoteAdminSession::connect(AdminConnectionOptions {
            endpoint: Some(endpoint),
            auth_token: None,
            auth_token_file: None,
            config_path: Some(config_path.to_string_lossy().to_string()),
            data_dir: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: true,
        })
        .await
        .expect("opted-in explicit unix socket should connect");

        session
            .management_client()
            .get_service_state(GetServiceStateRequest {})
            .await
            .expect("management client should send get_service_state");

        assert_eq!(
            seen_authorization
                .lock()
                .expect("authorization sink mutex should not be poisoned")
                .as_slice(),
            &[Some("Bearer management-secret-token".to_string())],
            "explicit endpoint config auth opt-in must forward the configured management token",
        );

        serve_handle.abort();
        let _ = serve_handle.await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_admin_session_uses_env_token_for_explicit_endpoint() {
        let _env_lock = process_env_test_lock().lock().await;
        let _env_guard =
            EnvVarGuard::set("SYNCTV_MANAGEMENT_AUTH_TOKEN", "explicit-endpoint-token");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let socket_path = temp_dir.path().join("management.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path)
            .expect("unix management listener should bind");
        let seen_authorization = Arc::new(Mutex::new(Vec::new()));
        let service = TestManagementService {
            seen_authorization: Some(Arc::clone(&seen_authorization)),
        };

        let serve_handle = tokio::spawn(async move {
            Server::builder()
                .add_service(ManagementServiceServer::new(service))
                .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
                .await
                .expect("unix management server should serve");
        });

        let endpoint = format!("unix://{}", socket_path.display());
        let session = RemoteAdminSession::connect(AdminConnectionOptions {
            endpoint: Some(endpoint),
            auth_token: None,
            auth_token_file: None,
            config_path: None,
            data_dir: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: false,
        })
        .await
        .expect("remote admin session should connect via explicit unix socket");

        session
            .management_client()
            .get_service_state(GetServiceStateRequest {})
            .await
            .expect("management client should send get_service_state");

        assert_eq!(
            seen_authorization
                .lock()
                .expect("authorization sink mutex should not be poisoned")
                .as_slice(),
            &[Some("Bearer explicit-endpoint-token".to_string())],
            "explicit endpoint overrides should only use explicitly provided auth tokens",
        );

        serve_handle.abort();
        let _ = serve_handle.await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_admin_session_with_explicit_endpoint_ignores_missing_config_file() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let socket_path = temp_dir.path().join("management.sock");
        let missing_config_path = temp_dir.path().join("missing-synctv.yaml");
        let listener = tokio::net::UnixListener::bind(&socket_path)
            .expect("unix management listener should bind");
        let service = TestManagementService::default();

        let serve_handle = tokio::spawn(async move {
            Server::builder()
                .add_service(ManagementServiceServer::new(service))
                .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
                .await
                .expect("unix management server should serve");
        });

        let endpoint = format!("unix://{}", socket_path.display());
        let session = RemoteAdminSession::connect(AdminConnectionOptions {
            endpoint: Some(endpoint.clone()),
            auth_token: None,
            auth_token_file: None,
            config_path: Some(missing_config_path.to_string_lossy().to_string()),
            data_dir: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: false,
        })
        .await
        .expect("explicit management endpoint should remain usable without a readable config file");

        assert_eq!(session.endpoint(), endpoint);
        session
            .management_client()
            .get_service_state(GetServiceStateRequest {})
            .await
            .expect("management client should work with explicit endpoint even when config file is missing");

        serve_handle.abort();
        let _ = serve_handle.await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_admin_session_with_explicit_endpoint_does_not_require_auto_discovered_config() {
        let _env_lock = process_env_test_lock().lock().await;
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        std::fs::write(temp_dir.path().join("synctv.yaml"), "not: [valid")
            .expect("invalid config should be written");
        let _cwd = CurrentDirGuard::change_to(temp_dir.path());

        let socket_path = temp_dir.path().join("explicit-endpoint.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path)
            .expect("unix management listener should bind");

        let serve_handle = tokio::spawn(async move {
            Server::builder()
                .add_service(ManagementServiceServer::new(
                    TestManagementService::default(),
                ))
                .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
                .await
                .expect("unix management server should serve");
        });

        let endpoint = format!("unix://{}", socket_path.display());
        let session = RemoteAdminSession::connect(AdminConnectionOptions {
            endpoint: Some(endpoint),
            auth_token: None,
            auth_token_file: None,
            config_path: None,
            data_dir: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: false,
        })
        .await
        .expect("explicit management endpoint should not depend on auto-discovered config");

        session
            .management_client()
            .get_service_state(GetServiceStateRequest {})
            .await
            .expect("management call should succeed via explicit endpoint");

        serve_handle.abort();
        let _ = serve_handle.await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn remote_admin_session_with_explicit_endpoint_does_not_use_auto_discovered_auth_token() {
        let _env_lock = process_env_test_lock().lock().await;
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        std::fs::write(
            temp_dir.path().join("synctv.yaml"),
            r#"
management:
  transport: "unix"
  auth_token: "auto-discovered-secret"
"#,
        )
        .expect("config should be written");
        let _cwd = CurrentDirGuard::change_to(temp_dir.path());

        let socket_path = temp_dir.path().join("explicit-endpoint-auth.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path)
            .expect("unix management listener should bind");
        let seen_authorization = Arc::new(Mutex::new(Vec::new()));
        let service = TestManagementService {
            seen_authorization: Some(Arc::clone(&seen_authorization)),
        };

        let serve_handle = tokio::spawn(async move {
            Server::builder()
                .add_service(ManagementServiceServer::new(service))
                .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
                .await
                .expect("unix management server should serve");
        });

        let endpoint = format!("unix://{}", socket_path.display());
        let session = RemoteAdminSession::connect(AdminConnectionOptions {
            endpoint: Some(endpoint),
            auth_token: None,
            auth_token_file: None,
            config_path: None,
            data_dir: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
            allow_config_auth_for_explicit_endpoint: false,
        })
        .await
        .expect("explicit management endpoint should connect without auto-discovered auth");

        session
            .management_client()
            .get_service_state(GetServiceStateRequest {})
            .await
            .expect("management call should succeed via explicit endpoint");

        assert_eq!(
            seen_authorization
                .lock()
                .expect("authorization sink mutex should not be poisoned")
                .as_slice(),
            &[None],
            "explicit endpoint mode must not inject auto-discovered management auth tokens",
        );

        serve_handle.abort();
        let _ = serve_handle.await;
    }
}
