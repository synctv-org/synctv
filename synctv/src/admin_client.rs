use anyhow::{bail, Context, Result};
use hyper_util::rt::TokioIo;
use std::time::Duration;
use synctv_core::bootstrap::{load_config_with_options, LoadConfigOptions};
use synctv_management::proto::management_service_client::ManagementServiceClient;
use tokio::net::UnixStream;
use tonic::transport::Channel;

const MANAGEMENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct AdminConnectionOptions {
    pub endpoint: Option<String>,
    pub config_path: Option<String>,
    pub load_dotenv: bool,
    pub verbose: bool,
    pub resolved_config_endpoint: Option<String>,
}

pub type AuthenticatedManagementClient = ManagementServiceClient<Channel>;

pub struct RemoteAdminSession {
    channel: Channel,
    endpoint: String,
}

impl RemoteAdminSession {
    pub async fn connect(options: AdminConnectionOptions) -> Result<Self> {
        let endpoints = resolve_candidate_endpoints(&options)?;
        let (channel, endpoint) = connect_first_available(&endpoints).await?;

        Ok(Self { channel, endpoint })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn management_client(&self) -> AuthenticatedManagementClient {
        ManagementServiceClient::new(self.channel.clone())
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

    if trimmed.starts_with('/') {
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
    if !path.starts_with('/') {
        bail!("unix endpoint path must be absolute");
    }
    Ok(format!("unix://{path}"))
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
    load_dotenv: bool,
    verbose: bool,
) -> Result<String> {
    let config = load_config_with_options(LoadConfigOptions {
        config_path: config_path.map(str::to_string),
        load_dotenv,
        validate: false,
        verbose,
    })?;
    normalize_endpoint(&config.management_endpoint())
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
        AdminEndpoint::Unix(path) => {
            let error_path = path.clone();
            tonic::transport::Endpoint::try_from("http://[::]:50052")
                .context("invalid synthetic unix endpoint")?
                .connect_timeout(MANAGEMENT_CONNECT_TIMEOUT)
                .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
                    let path = path.clone();
                    async move {
                        let stream = UnixStream::connect(path).await?;
                        Ok::<_, std::io::Error>(TokioIo::new(stream))
                    }
                }))
                .await
                .with_context(|| format!("failed to connect to admin unix socket {error_path}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_endpoint, resolve_candidate_endpoints, AdminConnectionOptions, RemoteAdminSession,
    };
    use std::pin::Pin;
    use synctv_core::config::default_management_unix_socket_path;
    use tempfile::tempdir;

    use futures_util::stream;
    use synctv_management::proto::{
        management_service_server::{ManagementService, ManagementServiceServer},
        AddAdminRequest, AddDirectUrlMediaRequest, AddMediaRequest, AddProviderInstanceRequest,
        ApproveRoomRequest, ApproveUserRequest, BanMemberRequest, BanRoomRequest, BanUserRequest,
        BatchBanRoomsRequest, BatchBanUsersRequest, BatchDeleteRoomsRequest,
        BatchDeleteUsersRequest, CreatePlaylistRequest, CreatePublishKeyRequest, CreateRoomRequest,
        CreateUserRequest, DeleteMediaRequest, DeletePlaylistRequest,
        DeleteProviderInstanceRequest, DeleteRoomRequest, DeleteUserRequest,
        DisableProviderInstanceRequest, EditMediaRequest, EnableProviderInstanceRequest,
        GetPlaybackRequest, GetPlaylistRequest, GetRoomMembersRequest, GetRoomRequest,
        GetRoomSettingsRequest, GetSettingsGroupRequest, GetSettingsRequest, GetStreamInfoRequest,
        GetSystemStatsRequest, GetUserByUsernameRequest, GetUserRequest, GetUserRoomsRequest,
        KickMemberRequest, KickStreamRequest, ListActiveStreamsRequest, ListAdminsRequest,
        ListMediaRequest, ListPlaylistsRequest, ListProviderInstancesRequest,
        ListRoomStreamsRequest, ListRoomsRequest, ListUsersRequest, MoveMediaRequest,
        MovePlaylistRequest, ReconnectProviderInstanceRequest, RemoveAdminRequest,
        ResetRoomSettingsRequest, SendTestEmailRequest, StartPlaybackRequest, StopPlaybackRequest,
        StopServerEvent, StopServerRequest, UnbanMemberRequest, UnbanRoomRequest, UnbanUserRequest,
        UpdateMemberPermissionsRequest, UpdatePlaylistRequest, UpdateProviderInstanceRequest,
        UpdateRoomPasswordRequest, UpdateRoomSettingsRequest, UpdateSettingsRequest,
        UpdateUserPasswordRequest, UpdateUserRoleRequest, UpdateUserUsernameRequest,
    };
    use synctv_proto::{admin as admin_proto, client as client_proto};
    use tonic::transport::Server;
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
            std::env::set_current_dir(&self.previous).expect("current dir should be restored");
        }
    }

    #[derive(Clone, Default)]
    struct TestManagementService;

    #[tonic::async_trait]
    impl ManagementService for TestManagementService {
        type StopServerStream =
            Pin<Box<dyn tokio_stream::Stream<Item = Result<StopServerEvent, Status>> + Send>>;

        async fn get_system_stats(
            &self,
            _request: Request<GetSystemStatsRequest>,
        ) -> std::result::Result<Response<admin_proto::GetSystemStatsResponse>, Status> {
            Ok(Response::new(admin_proto::GetSystemStatsResponse::default()))
        }

        async fn list_users(
            &self,
            _: Request<ListUsersRequest>,
        ) -> std::result::Result<Response<admin_proto::ListUsersResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn get_user(
            &self,
            _: Request<GetUserRequest>,
        ) -> std::result::Result<Response<admin_proto::GetUserResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn get_user_by_username(
            &self,
            _: Request<GetUserByUsernameRequest>,
        ) -> std::result::Result<Response<admin_proto::GetUserResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn add_admin(
            &self,
            _: Request<AddAdminRequest>,
        ) -> std::result::Result<Response<admin_proto::AddAdminResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn remove_admin(
            &self,
            _: Request<RemoveAdminRequest>,
        ) -> std::result::Result<Response<admin_proto::RemoveAdminResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn list_admins(
            &self,
            _: Request<ListAdminsRequest>,
        ) -> std::result::Result<Response<admin_proto::ListAdminsResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn create_user(
            &self,
            _: Request<CreateUserRequest>,
        ) -> std::result::Result<Response<admin_proto::CreateUserResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn delete_user(
            &self,
            _: Request<DeleteUserRequest>,
        ) -> std::result::Result<Response<admin_proto::DeleteUserResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn ban_user(
            &self,
            _: Request<BanUserRequest>,
        ) -> std::result::Result<Response<admin_proto::BanUserResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn unban_user(
            &self,
            _: Request<UnbanUserRequest>,
        ) -> std::result::Result<Response<admin_proto::UnbanUserResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn approve_user(
            &self,
            _: Request<ApproveUserRequest>,
        ) -> std::result::Result<Response<admin_proto::ApproveUserResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn update_user_role(
            &self,
            _: Request<UpdateUserRoleRequest>,
        ) -> std::result::Result<Response<admin_proto::UpdateUserRoleResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn update_user_password(
            &self,
            _: Request<UpdateUserPasswordRequest>,
        ) -> std::result::Result<Response<admin_proto::UpdateUserPasswordResponse>, Status>
        {
            Err(Status::unimplemented("test stub"))
        }
        async fn update_user_username(
            &self,
            _: Request<UpdateUserUsernameRequest>,
        ) -> std::result::Result<Response<admin_proto::UpdateUserUsernameResponse>, Status>
        {
            Err(Status::unimplemented("test stub"))
        }
        async fn get_user_rooms(
            &self,
            _: Request<GetUserRoomsRequest>,
        ) -> std::result::Result<Response<admin_proto::GetUserRoomsResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn batch_ban_users(
            &self,
            _: Request<BatchBanUsersRequest>,
        ) -> std::result::Result<Response<admin_proto::BatchBanUsersResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn batch_delete_users(
            &self,
            _: Request<BatchDeleteUsersRequest>,
        ) -> std::result::Result<Response<admin_proto::BatchDeleteUsersResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn create_room(
            &self,
            _: Request<CreateRoomRequest>,
        ) -> std::result::Result<Response<client_proto::CreateRoomResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn list_rooms(
            &self,
            _: Request<ListRoomsRequest>,
        ) -> std::result::Result<Response<admin_proto::ListRoomsResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn get_room(
            &self,
            _: Request<GetRoomRequest>,
        ) -> std::result::Result<Response<admin_proto::GetRoomResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn get_room_members(
            &self,
            _: Request<GetRoomMembersRequest>,
        ) -> std::result::Result<Response<admin_proto::GetRoomMembersResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn update_member_permissions(
            &self,
            _: Request<UpdateMemberPermissionsRequest>,
        ) -> std::result::Result<Response<client_proto::UpdateMemberPermissionsResponse>, Status>
        {
            Err(Status::unimplemented("test stub"))
        }
        async fn kick_member(
            &self,
            _: Request<KickMemberRequest>,
        ) -> std::result::Result<Response<client_proto::KickMemberResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn ban_member(
            &self,
            _: Request<BanMemberRequest>,
        ) -> std::result::Result<Response<client_proto::BanMemberResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn unban_member(
            &self,
            _: Request<UnbanMemberRequest>,
        ) -> std::result::Result<Response<client_proto::UnbanMemberResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn get_room_settings(
            &self,
            _: Request<GetRoomSettingsRequest>,
        ) -> std::result::Result<Response<admin_proto::GetRoomSettingsResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn update_room_settings(
            &self,
            _: Request<UpdateRoomSettingsRequest>,
        ) -> std::result::Result<Response<admin_proto::UpdateRoomSettingsResponse>, Status>
        {
            Err(Status::unimplemented("test stub"))
        }
        async fn reset_room_settings(
            &self,
            _: Request<ResetRoomSettingsRequest>,
        ) -> std::result::Result<Response<admin_proto::ResetRoomSettingsResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn update_room_password(
            &self,
            _: Request<UpdateRoomPasswordRequest>,
        ) -> std::result::Result<Response<admin_proto::UpdateRoomPasswordResponse>, Status>
        {
            Err(Status::unimplemented("test stub"))
        }
        async fn ban_room(
            &self,
            _: Request<BanRoomRequest>,
        ) -> std::result::Result<Response<admin_proto::BanRoomResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn unban_room(
            &self,
            _: Request<UnbanRoomRequest>,
        ) -> std::result::Result<Response<admin_proto::UnbanRoomResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn delete_room(
            &self,
            _: Request<DeleteRoomRequest>,
        ) -> std::result::Result<Response<admin_proto::DeleteRoomResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn approve_room(
            &self,
            _: Request<ApproveRoomRequest>,
        ) -> std::result::Result<Response<admin_proto::ApproveRoomResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn batch_ban_rooms(
            &self,
            _: Request<BatchBanRoomsRequest>,
        ) -> std::result::Result<Response<admin_proto::BatchBanRoomsResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn batch_delete_rooms(
            &self,
            _: Request<BatchDeleteRoomsRequest>,
        ) -> std::result::Result<Response<admin_proto::BatchDeleteRoomsResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn start_playback(
            &self,
            _: Request<StartPlaybackRequest>,
        ) -> std::result::Result<Response<client_proto::StartPlaybackResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn stop_playback(
            &self,
            _: Request<StopPlaybackRequest>,
        ) -> std::result::Result<Response<client_proto::StopPlaybackResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn get_playback(
            &self,
            _: Request<GetPlaybackRequest>,
        ) -> std::result::Result<Response<client_proto::GetPlaybackResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn create_publish_key(
            &self,
            _: Request<CreatePublishKeyRequest>,
        ) -> std::result::Result<Response<client_proto::CreatePublishKeyResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn get_stream_info(
            &self,
            _: Request<GetStreamInfoRequest>,
        ) -> std::result::Result<Response<client_proto::GetStreamInfoResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn list_room_streams(
            &self,
            _: Request<ListRoomStreamsRequest>,
        ) -> std::result::Result<Response<client_proto::ListRoomStreamsResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn list_playlists(
            &self,
            _: Request<ListPlaylistsRequest>,
        ) -> std::result::Result<Response<client_proto::ListPlaylistsResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn get_playlist(
            &self,
            _: Request<GetPlaylistRequest>,
        ) -> std::result::Result<Response<client_proto::GetPlaylistResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn create_playlist(
            &self,
            _: Request<CreatePlaylistRequest>,
        ) -> std::result::Result<Response<client_proto::CreatePlaylistResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn update_playlist(
            &self,
            _: Request<UpdatePlaylistRequest>,
        ) -> std::result::Result<Response<client_proto::UpdatePlaylistResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn move_playlist(
            &self,
            _: Request<MovePlaylistRequest>,
        ) -> std::result::Result<Response<client_proto::MovePlaylistResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn delete_playlist(
            &self,
            _: Request<DeletePlaylistRequest>,
        ) -> std::result::Result<Response<client_proto::DeletePlaylistResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn list_media(
            &self,
            _: Request<ListMediaRequest>,
        ) -> std::result::Result<Response<client_proto::ListPlaylistItemsResponse>, Status>
        {
            Err(Status::unimplemented("test stub"))
        }
        async fn add_media(
            &self,
            _: Request<AddMediaRequest>,
        ) -> std::result::Result<Response<client_proto::AddMediaResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn add_direct_url_media(
            &self,
            _: Request<AddDirectUrlMediaRequest>,
        ) -> std::result::Result<Response<client_proto::AddMediaResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn edit_media(
            &self,
            _: Request<EditMediaRequest>,
        ) -> std::result::Result<Response<client_proto::EditMediaResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn move_media(
            &self,
            _: Request<MoveMediaRequest>,
        ) -> std::result::Result<Response<client_proto::MoveMediaResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn delete_media(
            &self,
            _: Request<DeleteMediaRequest>,
        ) -> std::result::Result<Response<client_proto::DeleteMediaResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn list_provider_instances(
            &self,
            _: Request<ListProviderInstancesRequest>,
        ) -> std::result::Result<Response<admin_proto::ListProviderInstancesResponse>, Status>
        {
            Err(Status::unimplemented("test stub"))
        }
        async fn add_provider_instance(
            &self,
            _: Request<AddProviderInstanceRequest>,
        ) -> std::result::Result<Response<admin_proto::AddProviderInstanceResponse>, Status>
        {
            Err(Status::unimplemented("test stub"))
        }
        async fn update_provider_instance(
            &self,
            _: Request<UpdateProviderInstanceRequest>,
        ) -> std::result::Result<Response<admin_proto::UpdateProviderInstanceResponse>, Status>
        {
            Err(Status::unimplemented("test stub"))
        }
        async fn delete_provider_instance(
            &self,
            _: Request<DeleteProviderInstanceRequest>,
        ) -> std::result::Result<Response<admin_proto::DeleteProviderInstanceResponse>, Status>
        {
            Err(Status::unimplemented("test stub"))
        }
        async fn reconnect_provider_instance(
            &self,
            _: Request<ReconnectProviderInstanceRequest>,
        ) -> std::result::Result<Response<admin_proto::ReconnectProviderInstanceResponse>, Status>
        {
            Err(Status::unimplemented("test stub"))
        }
        async fn enable_provider_instance(
            &self,
            _: Request<EnableProviderInstanceRequest>,
        ) -> std::result::Result<Response<admin_proto::EnableProviderInstanceResponse>, Status>
        {
            Err(Status::unimplemented("test stub"))
        }
        async fn disable_provider_instance(
            &self,
            _: Request<DisableProviderInstanceRequest>,
        ) -> std::result::Result<Response<admin_proto::DisableProviderInstanceResponse>, Status>
        {
            Err(Status::unimplemented("test stub"))
        }
        async fn get_settings(
            &self,
            _: Request<GetSettingsRequest>,
        ) -> std::result::Result<Response<admin_proto::GetSettingsResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn get_settings_group(
            &self,
            _: Request<GetSettingsGroupRequest>,
        ) -> std::result::Result<Response<admin_proto::GetSettingsGroupResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn update_settings(
            &self,
            _: Request<UpdateSettingsRequest>,
        ) -> std::result::Result<Response<admin_proto::UpdateSettingsResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn send_test_email(
            &self,
            _: Request<SendTestEmailRequest>,
        ) -> std::result::Result<Response<admin_proto::SendTestEmailResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn list_active_streams(
            &self,
            _: Request<ListActiveStreamsRequest>,
        ) -> std::result::Result<Response<admin_proto::ListActiveStreamsResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn kick_stream(
            &self,
            _: Request<KickStreamRequest>,
        ) -> std::result::Result<Response<admin_proto::KickStreamResponse>, Status> {
            Err(Status::unimplemented("test stub"))
        }
        async fn stop_server(
            &self,
            _: Request<StopServerRequest>,
        ) -> std::result::Result<Response<Self::StopServerStream>, Status> {
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
    fn normalize_endpoint_preserves_unix_socket_scheme() {
        let raw = format!("unix://{}", default_management_unix_socket_path().display());
        let normalized = normalize_endpoint(&raw).expect("unix socket endpoint should normalize");
        assert_eq!(normalized, raw);
    }

    #[test]
    fn resolve_candidate_endpoints_prefers_default_unix_then_tcp() {
        let dir = tempdir().expect("temp dir should be created");
        let _cwd = CurrentDirGuard::change_to(dir.path());
        let _env = EnvVarGuard::remove("SYNCTV_CONFIG_PATH");

        let endpoints = resolve_candidate_endpoints(&AdminConnectionOptions {
            endpoint: None,
            config_path: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
        })
        .expect("default admin endpoints should resolve");
        assert_eq!(
            endpoints,
            vec![format!(
                "unix://{}",
                default_management_unix_socket_path().display()
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
            config_path: Some(config_path.to_string_lossy().to_string()),
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
        })
        .expect("configured management endpoint should resolve");

        assert_eq!(endpoints, vec!["http://127.0.0.1:50123/".to_string()]);
    }

    #[test]
    fn resolve_candidate_endpoints_errors_for_missing_explicit_config_file() {
        let dir = tempdir().expect("temp dir should be created");
        let missing_path = dir.path().join("missing-synctv.yaml");

        let error = resolve_candidate_endpoints(&AdminConnectionOptions {
            endpoint: None,
            config_path: Some(missing_path.to_string_lossy().to_string()),
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
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
            config_path: None,
            load_dotenv: false,
            verbose: false,
            resolved_config_endpoint: None,
        })
        .await
        .expect("remote admin session should connect via unix socket");

        assert_eq!(session.endpoint(), endpoint);
        session
            .management_client()
            .get_system_stats(GetSystemStatsRequest {})
            .await
            .expect("management client should call get_system_stats over unix socket");

        serve_handle.abort();
        let _ = serve_handle.await;
    }
}
