#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::Once;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use futures_util::{stream, SinkExt, StreamExt};
use opaque_ke::argon2::Argon2 as OpaqueArgon2Ksf;
use opaque_ke::ciphersuite::CipherSuite;
use opaque_ke::rand::rngs::OsRng;
use opaque_ke::{
    ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialResponse, RegistrationResponse,
};
use prost::Message;
use reqwest::StatusCode;
use serde::Serialize;
use serde_json::Value;
use sha2_010::Sha512;
use synctv::{
    app_config::{AppConfig as Config, ManagementTransport},
    Application, ApplicationBuildOptions,
};
use synctv_core_testing::{
    connect_test_pool_url, create_test_database_url_with_label,
    postgres_connection_url_with_credentials, start_redis_url_with_label, test_redis_key_prefix,
    RedisContainer, TestContainer,
};
use synctv_management::proto as management_proto;
use synctv_media_providers::grpc::alist::{alist_server::AlistServer, MeResp as AlistMeResp};
use synctv_proto::client::{server_message, ServerMessage};
use synctv_xiu::bytesio::bytesio_errors::BytesIOErrorValue;
use synctv_xiu::bytesio::net_io::{TNetIO, TcpIO};
use synctv_xiu::flv::amf0::define::Amf0ValueType;
use synctv_xiu::rtmp::chunk::unpacketizer::{ChunkUnpacketizer, UnpackResult};
use synctv_xiu::rtmp::handshake::{
    define::ClientHandshakeState, handshake_client::SimpleHandshakeClient,
};
use synctv_xiu::rtmp::messages::{define::RtmpMessageData, parser::MessageParser};
use synctv_xiu::rtmp::netconnection::writer::{ConnectProperties, NetConnection};
use synctv_xiu::rtmp::netstream::writer::NetStreamWriter;
use synctv_xiu::rtmp::protocol_control_messages::writer::ProtocolControlMessagesWriter;
use tokio_tungstenite::tungstenite;
use tonic::metadata::MetadataValue;
use tonic::transport::Server;
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::health_client::HealthClient;
use tonic_health::pb::HealthCheckRequest;

const PROVIDER_PROBE_HOST: &str = "provider-test.example.com";
const PROVIDER_PROBE_SECRET: &str = "provider-remote-e2e-secret";
const MANAGEMENT_E2E_AUTH_TOKEN: &str = "management-e2e-secret";
const BOOTSTRAP_ROOT_USERNAME: &str = "e2e_root";
const TEST_CREDENTIAL_ENCRYPTION_KEY: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn trusted_dynamic_sql(sql: String) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(sql)
}

fn page_pagination(
    page: u32,
) -> Option<synctv_proto::client::list_playlist_items_request::Pagination> {
    Some(
        synctv_proto::client::list_playlist_items_request::Pagination::Page(
            synctv_proto::client::PagePagination { page },
        ),
    )
}

fn quote_pg_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn quote_pg_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

struct TestOpaqueCipherSuite;

impl CipherSuite for TestOpaqueCipherSuite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeyExchange = opaque_ke::TripleDh<opaque_ke::Ristretto255, Sha512>;
    type Ksf = OpaqueArgon2Ksf<'static>;
}

fn reserve_local_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("ephemeral local_addr")
        .port()
}

fn unique_test_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    nanos.to_string()
}

fn bounded_test_username(label: &str, role: &str, suffix: &str) -> String {
    const MAX_USERNAME_LEN: usize = 50;

    let reserved_len = role.len() + suffix.len() + 2;
    let max_label_len = MAX_USERNAME_LEN.saturating_sub(reserved_len).max(1);
    let mut bounded_label: String = label
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .take(max_label_len)
        .collect();
    if bounded_label.is_empty() {
        bounded_label.push('t');
    }

    format!("{bounded_label}_{role}_{suffix}")
}

fn json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))
}

fn optional_event_sequence(sequence: impl Into<String>) -> Option<i64> {
    let sequence = sequence.into();
    let sequence = sequence.trim();
    if sequence.is_empty() {
        None
    } else {
        Some(
            sequence
                .parse()
                .expect("observe event sequence must be numeric"),
        )
    }
}

fn observe_playback_message(observe_id: &str) -> synctv_proto::client::ClientMessage {
    use synctv_proto::client::{
        client_message, observe_resource, ObservePlayback, ObserveResource, ResourceDeliveryMode,
    };

    synctv_proto::client::ClientMessage {
        message: Some(client_message::Message::ObserveResource(ObserveResource {
            observe_id: observe_id.to_string(),
            delivery_mode: ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(observe_resource::Resource::Playback(ObservePlayback {
                playback_client_profile: None,
            })),
        })),
    }
}

fn observe_playback_state_message(observe_id: &str) -> synctv_proto::client::ClientMessage {
    use synctv_proto::client::{
        client_message, observe_resource, ObservePlaybackState, ObserveResource,
        ResourceDeliveryMode,
    };

    synctv_proto::client::ClientMessage {
        message: Some(client_message::Message::ObserveResource(ObserveResource {
            observe_id: observe_id.to_string(),
            delivery_mode: ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(observe_resource::Resource::PlaybackState(
                ObservePlaybackState {
                    event_sequence: None,
                },
            )),
        })),
    }
}

fn observe_room_settings_message(
    observe_id: &str,
    after_event_sequence: impl Into<String>,
) -> synctv_proto::client::ClientMessage {
    use synctv_proto::client::{
        client_message, observe_resource, ObserveResource, ObserveRoomSettings,
        ResourceDeliveryMode,
    };

    synctv_proto::client::ClientMessage {
        message: Some(client_message::Message::ObserveResource(ObserveResource {
            observe_id: observe_id.to_string(),
            delivery_mode: ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(observe_resource::Resource::RoomSettings(
                ObserveRoomSettings {
                    after_event_sequence: optional_event_sequence(after_event_sequence),
                },
            )),
        })),
    }
}

fn observe_playlist_items_message(
    observe_id: &str,
    after_event_sequence: impl Into<String>,
    request: synctv_proto::client::ListPlaylistItemsRequest,
) -> synctv_proto::client::ClientMessage {
    use synctv_proto::client::{
        client_message, observe_resource, ObservePlaylistItems, ObserveResource,
        ResourceDeliveryMode,
    };

    synctv_proto::client::ClientMessage {
        message: Some(client_message::Message::ObserveResource(ObserveResource {
            observe_id: observe_id.to_string(),
            delivery_mode: ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(observe_resource::Resource::PlaylistItems(
                ObservePlaylistItems {
                    request: Some(request),
                    after_event_sequence: optional_event_sequence(after_event_sequence),
                },
            )),
        })),
    }
}

fn observe_room_members_message(
    observe_id: &str,
    after_event_sequence: impl Into<String>,
    _request: synctv_proto::client::GetRoomMembersRequest,
) -> synctv_proto::client::ClientMessage {
    use synctv_proto::client::{
        client_message, observe_resource, ObserveResource, ObserveRoomMemberEvents,
        ResourceDeliveryMode,
    };

    synctv_proto::client::ClientMessage {
        message: Some(client_message::Message::ObserveResource(ObserveResource {
            observe_id: observe_id.to_string(),
            delivery_mode: ResourceDeliveryMode::PushSnapshot as i32,
            resource: Some(observe_resource::Resource::RoomMemberEvents(
                ObserveRoomMemberEvents {
                    after_event_sequence: optional_event_sequence(after_event_sequence),
                },
            )),
        })),
    }
}

fn resource_playback(message: &ServerMessage) -> Option<&synctv_proto::client::Playback> {
    match &message.message {
        Some(server_message::Message::ResourceEvent(event)) => match event.payload.as_ref() {
            Some(synctv_proto::client::resource_event::Payload::Playback(snapshot)) => {
                Some(snapshot)
            }
            _ => None,
        },
        _ => None,
    }
}

fn resource_playback_state(
    message: &ServerMessage,
) -> Option<&synctv_proto::client::PlaybackState> {
    match &message.message {
        Some(server_message::Message::ResourceEvent(event)) => match event.payload.as_ref() {
            Some(synctv_proto::client::resource_event::Payload::PlaybackState(state)) => {
                Some(state)
            }
            _ => None,
        },
        _ => None,
    }
}

fn resource_room_settings(
    message: &ServerMessage,
) -> Option<&synctv_proto::client::GetRoomSettingsResponse> {
    match &message.message {
        Some(server_message::Message::ResourceEvent(event)) => match event.payload.as_ref() {
            Some(synctv_proto::client::resource_event::Payload::RoomSettings(settings)) => {
                Some(settings)
            }
            _ => None,
        },
        _ => None,
    }
}

fn room_settings_value(
    settings: Option<&synctv_proto::client::RoomSettings>,
    context: &str,
) -> Value {
    serde_json::to_value(settings.expect(context)).expect("room settings should encode")
}

fn create_ticket_request(room_id: &str) -> synctv_proto::client::CreateWebSocketTicketRequest {
    synctv_proto::client::CreateWebSocketTicketRequest {
        room_id: room_id.to_string(),
    }
}

fn create_room_request(
    name: &str,
    description: &str,
    password: &str,
) -> synctv_proto::client::CreateRoomRequest {
    synctv_proto::client::CreateRoomRequest {
        name: name.to_string(),
        settings: None,
        description: description.to_string(),
        password: password.to_string(),
        category_id: String::new(),
        label_ids: Vec::new(),
    }
}

fn resource_playlist_items(
    message: &ServerMessage,
) -> Option<&synctv_proto::client::ListPlaylistItemsResponse> {
    match &message.message {
        Some(server_message::Message::ResourceEvent(event)) => match event.payload.as_ref() {
            Some(synctv_proto::client::resource_event::Payload::PlaylistItems(snapshot)) => {
                Some(snapshot)
            }
            _ => None,
        },
        _ => None,
    }
}

fn resource_room_member_event(
    message: &ServerMessage,
) -> Option<&synctv_proto::client::RoomMemberEvent> {
    match &message.message {
        Some(server_message::Message::ResourceEvent(resource_event)) => {
            match resource_event.payload.as_ref() {
                Some(synctv_proto::client::resource_event::Payload::RoomMemberEvent(event)) => {
                    Some(event)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn resource_room_member_joined(
    message: &ServerMessage,
) -> Option<&synctv_proto::client::RoomMemberEvent> {
    resource_room_member_event(message)
        .filter(|event| event.kind == synctv_proto::client::RoomMemberEventKind::Joined as i32)
}

fn resource_room_member_left(
    message: &ServerMessage,
) -> Option<&synctv_proto::client::RoomMemberEvent> {
    resource_room_member_event(message)
        .filter(|event| event.kind == synctv_proto::client::RoomMemberEventKind::Left as i32)
}

fn test_config(
    database_url: String,
    redis_url: String,
    api_port: u16,
    management_port: u16,
    rtmp_port: u16,
) -> Config {
    let mut config = Config::default();
    config.server.host = "127.0.0.1".to_string();
    config.server.port = api_port;
    config.server.enable_reflection = false;
    config.metrics.enabled = false;
    config.server.advertise_host = "127.0.0.1".to_string();
    config.server.shutdown_drain_timeout_seconds = 3;
    config.management.enabled = true;
    config.management.transport = ManagementTransport::Tcp;
    config.management.port = management_port;
    config.management.auth_token = MANAGEMENT_E2E_AUTH_TOKEN.to_string();
    config.management.enable_reflection = false;
    config.database.url = database_url;
    config.redis.url = redis_url;
    config.redis.key_prefix = test_redis_key_prefix("full-stack");
    config.jwt.secret = "test-jwt-secret-key-for-full-stack-e2e-123456".to_string();
    config.security.opaque_server_setup_secret =
        "test-opaque-server-setup-secret-for-full-stack-e2e".to_string();
    config.bootstrap.create_root_user = true;
    config.bootstrap.root_username = BOOTSTRAP_ROOT_USERNAME.to_string();
    config.bootstrap.root_password = "StrongPwd12345!".to_string();
    config.livestream.rtmp_port = rtmp_port;
    config.webrtc.enable_builtin_stun = false;

    // Raise rate limits well above defaults for avoid cross-test interference
    // when 20 tests share the same Redis + server.
    config.request_rate_limits.auth_max_requests = 10_000;
    config.request_rate_limits.auth_window_seconds = 1;
    config.request_rate_limits.write_max_requests = 5_000;
    config.request_rate_limits.write_window_seconds = 1;
    config.request_rate_limits.read_max_requests = 5_000;
    config.request_rate_limits.read_window_seconds = 1;
    config.request_rate_limits.media_max_requests = 5_000;
    config.request_rate_limits.media_window_seconds = 1;
    config.request_rate_limits.admin_max_requests = 5_000;
    config.request_rate_limits.admin_window_seconds = 1;
    config.request_rate_limits.streaming_max_requests = 5_000;
    config.request_rate_limits.streaming_window_seconds = 1;
    config.request_rate_limits.websocket_max_requests = 5_000;
    config.request_rate_limits.websocket_window_seconds = 1;
    config
}

fn write_cli_test_config(path: &std::path::Path, config: &Config) {
    let yaml = serde_yaml::to_string(config).expect("CLI test config should serialize to YAML");
    std::fs::write(path, yaml).expect("CLI test config should be written");
}

#[cfg(unix)]
fn configure_management_unix_socket(config: &mut Config, socket_path: &std::path::Path) {
    config.management.transport = ManagementTransport::Unix;
    config.management.unix_socket_path = socket_path.display().to_string();
    config.management.auth_token.clear();
}

#[cfg(unix)]
fn configure_management_unix_socket_with_auth_token(
    config: &mut Config,
    socket_path: &std::path::Path,
    auth_token: &str,
) {
    config.management.transport = ManagementTransport::Unix;
    config.management.unix_socket_path = socket_path.display().to_string();
    config.management.auth_token = auth_token.to_string();
}

#[cfg(unix)]
fn isolated_default_management_socket(
    root: &std::path::Path,
) -> (std::path::PathBuf, Vec<(String, String)>) {
    isolated_default_management_socket_impl(root)
}

#[cfg(target_os = "macos")]
fn isolated_default_management_socket_impl(
    root: &std::path::Path,
) -> (std::path::PathBuf, Vec<(String, String)>) {
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("isolated home dir should be created");
    (
        home.join(".synctv").join("run").join("synctv.sock"),
        vec![("HOME".to_string(), home.display().to_string())],
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn isolated_default_management_socket_impl(
    root: &std::path::Path,
) -> (std::path::PathBuf, Vec<(String, String)>) {
    let state_home = root.join("state");
    std::fs::create_dir_all(&state_home).expect("isolated state dir should be created");
    (
        state_home.join("synctv").join("run").join("synctv.sock"),
        vec![(
            "XDG_STATE_HOME".to_string(),
            state_home.display().to_string(),
        )],
    )
}

static TEST_LOGGING: Once = Once::new();

fn ensure_test_logging() {
    TEST_LOGGING.call_once(|| {
        let logging = synctv_core::logging::LoggingOptions {
            level: "debug".to_string(),
            filter: Some("debug,synctv=debug,synctv_core=debug".to_string()),
            ..Default::default()
        };
        synctv_core::logging::init_logging(&logging)
            .expect("test tracing subscriber should initialize");
    });
}

struct TestServer {
    api_base_url: String,
    management_base_url: String,
    provider_probe_endpoint: String,
    provider_probe_secret: String,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    server_handle: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    provider_probe_handle: Option<tokio::task::JoinHandle<()>>,
    _postgres: TestContainer,
    _redis: RedisContainer,
}

type StartupExitRx = tokio::sync::watch::Receiver<Option<Result<(), String>>>;

type TestWebSocketStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

impl TestServer {
    async fn shutdown(mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }

        if let Some(server_handle) = self.server_handle.take() {
            let mut server_handle = server_handle;
            if tokio::time::timeout(Duration::from_secs(5), &mut server_handle)
                .await
                .is_err()
            {
                server_handle.abort();
                let _ = server_handle.await;
            }
        }

        if let Some(provider_probe_handle) = self.provider_probe_handle.take() {
            provider_probe_handle.abort();
            let _ = provider_probe_handle.await;
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(server_handle) = self.server_handle.take() {
            server_handle.abort();
        }
        if let Some(provider_probe_handle) = self.provider_probe_handle.take() {
            provider_probe_handle.abort();
        }
    }
}

async fn start_test_server() -> TestServer {
    ensure_test_logging();
    let suffix = unique_test_suffix();
    let database_name = format!("synctv_e2e_{suffix}");
    let container_label = format!("full-stack-{suffix}");
    let (postgres, database_url) =
        create_test_database_url_with_label(&database_name, &container_label).await;
    let (redis, redis_url) = start_redis_url_with_label(&container_label).await;
    let (provider_probe_addr, provider_probe_handle) =
        spawn_authenticated_provider_server(PROVIDER_PROBE_SECRET).await;
    let provider_probe_endpoint = format!(
        "http://{PROVIDER_PROBE_HOST}:{}",
        provider_probe_addr.port()
    );

    for attempt in 1..=3 {
        let api_port = reserve_local_port();
        let management_port = reserve_local_port();
        let rtmp_port = reserve_local_port();
        let config = test_config(
            database_url.clone(),
            redis_url.clone(),
            api_port,
            management_port,
            rtmp_port,
        );

        let provider_probe_host = PROVIDER_PROBE_HOST.to_string();
        let app = Box::pin(Application::build_with_options(
            config,
            ApplicationBuildOptions {
                provider_address_overrides: HashMap::from([(
                    provider_probe_host,
                    provider_probe_addr,
                )]),
                credential_encryption_key_override: Some(
                    TEST_CREDENTIAL_ENCRYPTION_KEY.to_string(),
                ),
                allow_password_registration: true,
                ..ApplicationBuildOptions::default()
            },
        ))
        .await
        .expect("test application build");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let (server_handle, startup_exit_rx) = spawn_server_task(async move {
            Box::pin(app.run_with_shutdown_signal(async move {
                let _ = shutdown_rx.await;
            }))
            .await
        });

        let api_base_url = format!("http://127.0.0.1:{api_port}");
        let management_base_url = format!("http://127.0.0.1:{management_port}");

        let startup_result: Result<(), String> = async {
            wait_until_live_or_server_exit(&api_base_url, &startup_exit_rx).await?;
            wait_until_grpc_ready_or_server_exit(&api_base_url, &startup_exit_rx).await?;
            wait_until_grpc_ready_or_server_exit(&management_base_url, &startup_exit_rx).await
        }
        .await;

        match startup_result {
            Ok(()) => {
                return TestServer {
                    api_base_url,
                    management_base_url,
                    provider_probe_endpoint,
                    provider_probe_secret: PROVIDER_PROBE_SECRET.to_string(),
                    shutdown_tx: Some(shutdown_tx),
                    server_handle: Some(server_handle),
                    provider_probe_handle: Some(provider_probe_handle),
                    _postgres: postgres,
                    _redis: redis,
                };
            }
            Err(error) if attempt < 3 && startup_error_is_retryable(&error) => {
                let _ = shutdown_tx.send(());
                let join_result = server_handle
                    .await
                    .expect("retryable startup failure task should join");
                tracing::warn!(
                    attempt,
                    error = %error,
                    result = ?join_result.as_ref(),
                    "retrying full-stack test server startup after transient bind failure"
                );
            }
            Err(error) => {
                let _ = shutdown_tx.send(());
                let join_result = server_handle
                    .await
                    .expect("failed startup task should join");
                panic!(
                    "full-stack test server failed to start on attempt {attempt}: {error}\nrun result: {join_result:?}"
                );
            }
        }
    }

    unreachable!("full-stack test server startup loop should return or panic")
}

async fn spawn_authenticated_provider_server(
    auth_secret: &str,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let auth_secret = auth_secret.to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("provider auth test server should bind to an ephemeral port");
    let addr = listener
        .local_addr()
        .expect("provider auth test server should expose a local address");
    let handle = tokio::spawn(async move {
        let (reporter, health_service) = tonic_health::server::health_reporter();
        reporter
            .set_service_status("", tonic_health::ServingStatus::Serving)
            .await;
        reporter
            .set_serving::<AlistServer<GrpcAuthProbeAlistService>>()
            .await;

        Server::builder()
            .add_service(health_service)
            .add_service(AlistServer::new(GrpcAuthProbeAlistService::new(
                auth_secret,
            )))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .expect("provider auth test server should run");
    });

    wait_until_grpc_ready(&format!("http://127.0.0.1:{}", addr.port())).await;
    (addr, handle)
}

#[derive(Clone)]
struct GrpcAuthProbeAlistService {
    expected_secret: Arc<str>,
}

impl GrpcAuthProbeAlistService {
    fn new(expected_secret: String) -> Self {
        Self {
            expected_secret: Arc::<str>::from(expected_secret),
        }
    }

    fn validate_secret<T>(&self, request: &tonic::Request<T>) -> Result<(), tonic::Status> {
        let value = request
            .metadata()
            .get("x-provider-secret")
            .ok_or_else(|| tonic::Status::unauthenticated("Missing x-provider-secret header"))?;
        let provided = value
            .to_str()
            .map_err(|_| tonic::Status::unauthenticated("Invalid x-provider-secret header"))?;
        if provided != self.expected_secret.as_ref() {
            return Err(tonic::Status::unauthenticated("Invalid provider secret"));
        }
        Ok(())
    }
}

#[tonic::async_trait]
impl synctv_media_providers::grpc::alist::alist_server::Alist for GrpcAuthProbeAlistService {
    async fn login(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::alist::LoginReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::alist::LoginResp>, tonic::Status>
    {
        Err(tonic::Status::unimplemented(
            "login not needed for health probe",
        ))
    }

    async fn me(
        &self,
        request: tonic::Request<synctv_media_providers::grpc::alist::MeReq>,
    ) -> Result<tonic::Response<AlistMeResp>, tonic::Status> {
        self.validate_secret(&request)?;
        Ok(tonic::Response::new(AlistMeResp {
            id: 1,
            username: "health-check".to_string(),
            base_path: String::new(),
            role: 0,
            disabled: false,
            permission: 0,
            sso_id: String::new(),
            otp: false,
        }))
    }

    async fn fs_get(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::alist::FsGetReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::alist::FsGetResp>, tonic::Status>
    {
        Err(tonic::Status::unimplemented(
            "fs_get not needed for health probe",
        ))
    }

    async fn fs_list(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::alist::FsListReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::alist::FsListResp>, tonic::Status>
    {
        Err(tonic::Status::unimplemented(
            "fs_list not needed for health probe",
        ))
    }

    async fn fs_other(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::alist::FsOtherReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::alist::FsOtherResp>, tonic::Status>
    {
        Err(tonic::Status::unimplemented(
            "fs_other not needed for health probe",
        ))
    }

    async fn fs_search(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::alist::FsSearchReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::alist::FsSearchResp>, tonic::Status>
    {
        Err(tonic::Status::unimplemented(
            "fs_search not needed for health probe",
        ))
    }
}

fn spawn_server_task<F>(future: F) -> (tokio::task::JoinHandle<anyhow::Result<()>>, StartupExitRx)
where
    F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let (exit_tx, exit_rx) = tokio::sync::watch::channel(None);
    let handle = tokio::spawn(async move {
        let result = future.await;
        let _ = exit_tx.send(Some(
            result
                .as_ref()
                .map_err(|error| format!("{error:#}"))
                .copied(),
        ));
        result
    });
    (handle, exit_rx)
}

fn server_startup_exit_error(startup_exit_rx: &StartupExitRx) -> Option<String> {
    startup_exit_rx
        .borrow()
        .as_ref()
        .map(|result| match result {
            Ok(()) => "server exited before becoming ready".to_string(),
            Err(error) => format!("server exited before becoming ready: {error}"),
        })
}

fn startup_error_is_retryable(error: &str) -> bool {
    error.contains("Failed to bind HTTP address")
        || error.contains("failed to bind management TCP address")
        || error.contains("Address already in use")
        || error.contains("address already in use")
}

async fn wait_until_live_or_server_exit(
    http_base_url: &str,
    startup_exit_rx: &StartupExitRx,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|error| format!("failed to build HTTP client: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let url = format!("{http_base_url}/health/live");

    loop {
        if let Some(error) = server_startup_exit_error(startup_exit_rx) {
            return Err(error);
        }

        match client.get(&url).send().await {
            Ok(response) if response.status() == StatusCode::OK => return Ok(()),
            Ok(_) | Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok(response) => {
                return Err(format!(
                    "health endpoint never became live, last status: {}",
                    response.status()
                ));
            }
            Err(error) => {
                return Err(format!("health endpoint never became live: {error}"));
            }
        }
    }
}

async fn wait_until_grpc_ready(grpc_base_url: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        let status = match tonic::transport::Endpoint::from_shared(grpc_base_url.to_string())
            .expect("valid gRPC endpoint")
            .connect()
            .await
        {
            Ok(channel) => {
                let mut health = HealthClient::new(channel);
                health
                    .check(HealthCheckRequest {
                        service: String::new(),
                    })
                    .await
                    .map(|response| response.into_inner().status)
            }
            Err(error) => Err(tonic::Status::unavailable(error.to_string())),
        };

        match status {
            Ok(status) if status == ServingStatus::Serving as i32 => return,
            Ok(_) | Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok(status) => panic!("gRPC health never became serving, last status: {status}"),
            Err(error) => panic!("gRPC health never became ready: {error}"),
        }
    }
}

async fn wait_until_grpc_ready_or_server_exit(
    grpc_base_url: &str,
    startup_exit_rx: &StartupExitRx,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        if let Some(error) = server_startup_exit_error(startup_exit_rx) {
            return Err(error);
        }

        let status = match tonic::transport::Endpoint::from_shared(grpc_base_url.to_string())
            .map_err(|error| format!("invalid gRPC endpoint {grpc_base_url}: {error}"))?
            .connect()
            .await
        {
            Ok(channel) => {
                let mut health = HealthClient::new(channel);
                health
                    .check(HealthCheckRequest {
                        service: String::new(),
                    })
                    .await
                    .map(|response| response.into_inner().status)
            }
            Err(error) => Err(tonic::Status::unavailable(error.to_string())),
        };

        match status {
            Ok(status) if status == ServingStatus::Serving as i32 => return Ok(()),
            Ok(_) | Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok(status) => {
                return Err(format!(
                    "gRPC health never became serving, last status: {status}"
                ));
            }
            Err(error) => return Err(format!("gRPC health never became ready: {error}")),
        }
    }
}

#[cfg(unix)]
async fn wait_until_unix_grpc_ready_or_server_exit(
    socket_path: &std::path::Path,
    startup_exit_rx: &StartupExitRx,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        if let Some(error) = server_startup_exit_error(startup_exit_rx) {
            return Err(error);
        }

        let path = socket_path.to_path_buf();
        let status = match tonic::transport::Endpoint::try_from("http://[::]:50052")
            .map_err(|error| format!("invalid synthetic unix gRPC endpoint: {error}"))?
            .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
                let path = path.clone();
                async move {
                    let stream = tokio::net::UnixStream::connect(path).await?;
                    Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                }
            }))
            .await
        {
            Ok(channel) => {
                let mut health = HealthClient::new(channel);
                health
                    .check(HealthCheckRequest {
                        service: String::new(),
                    })
                    .await
                    .map(|response| response.into_inner().status)
            }
            Err(error) => Err(tonic::Status::unavailable(error.to_string())),
        };

        match status {
            Ok(status) if status == ServingStatus::Serving as i32 => return Ok(()),
            Ok(_) | Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok(status) => {
                return Err(format!(
                    "unix gRPC health never became serving, last status: {status}"
                ));
            }
            Err(error) => return Err(format!("unix gRPC health never became ready: {error}")),
        }
    }
}

async fn grpc_health_status(
    grpc_base_url: &str,
    service: &str,
) -> Result<ServingStatus, tonic::Code> {
    let channel = tonic::transport::Endpoint::from_shared(grpc_base_url.to_string())
        .expect("valid gRPC endpoint")
        .connect()
        .await
        .unwrap_or_else(|error| panic!("connect health client to {grpc_base_url} failed: {error}"));
    let mut health = HealthClient::new(channel);

    match health
        .check(HealthCheckRequest {
            service: service.to_string(),
        })
        .await
    {
        Ok(response) => Ok(ServingStatus::try_from(response.into_inner().status)
            .expect("health response should contain a valid serving status")),
        Err(status) => Err(status.code()),
    }
}

async fn run_synctv_remote_cli(server: &TestServer, args: &[&str]) -> std::process::Output {
    let mut structured_args = args.to_vec();
    structured_args.extend(["--output", "json"]);
    run_synctv_cli_with_env(
        &structured_args,
        &[
            (
                "SYNCTV_MANAGEMENT_ENDPOINT",
                server.management_base_url.as_str(),
            ),
            ("SYNCTV_MANAGEMENT_AUTH_TOKEN", MANAGEMENT_E2E_AUTH_TOKEN),
        ],
    )
    .await
}

fn cli_stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn cli_stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn cli_json_output(output: &std::process::Output, context: &str) -> Value {
    assert!(
        output.status.success(),
        "{context} should succeed\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(output),
        cli_stderr(output),
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{context} should emit JSON output: {error}"))
}

async fn run_synctv_remote_cli_json(server: &TestServer, args: &[&str], context: &str) -> Value {
    let output = run_synctv_remote_cli(server, args).await;
    cli_json_output(&output, context)
}

async fn run_synctv_remote_cli_failure(
    server: &TestServer,
    args: &[&str],
    context: &str,
) -> String {
    let output = run_synctv_remote_cli(server, args).await;
    assert!(
        !output.status.success(),
        "{context} should fail\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&output),
        cli_stderr(&output),
    );
    cli_stderr(&output)
}

async fn run_synctv_cli_with_env(args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
    run_synctv_cli_with_env_async(args, envs).await
}

async fn run_synctv_cli_with_env_async(
    args: &[&str],
    envs: &[(&str, &str)],
) -> std::process::Output {
    let binary = synctv_binary_path();
    let owned_args = args
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    let owned_envs = envs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect::<Vec<_>>();

    tokio::time::timeout(Duration::from_secs(90), async move {
        let mut command = tokio::process::Command::new(binary);
        command.args(&owned_args);
        command.env_remove("SYNCTV_CONFIG_PATH");
        command.env_remove("SYNCTV_MANAGEMENT_ENDPOINT");
        command.env_remove("SYNCTV_MANAGEMENT_AUTH_TOKEN");
        command.env_remove("XDG_CONFIG_HOME");
        command.env_remove("XDG_RUNTIME_DIR");
        for (name, value) in owned_envs {
            command.env(name, value);
        }
        command
            .output()
            .await
            .expect("synctv CLI async process should start")
    })
    .await
    .expect("synctv CLI async process should finish within timeout")
}

struct SpawnedSynctvProcess {
    child: tokio::process::Child,
    stdout_log_path: std::path::PathBuf,
    stderr_log_path: std::path::PathBuf,
}

fn cli_process_failure_details(
    stdout_log_path: &std::path::Path,
    stderr_log_path: &std::path::Path,
) -> String {
    let stdout = std::fs::read_to_string(stdout_log_path)
        .unwrap_or_else(|error| format!("<failed to read stdout log: {error}>"));
    let stderr = std::fs::read_to_string(stderr_log_path)
        .unwrap_or_else(|error| format!("<failed to read stderr log: {error}>"));
    format!("stdout:\n{stdout}\n\nstderr:\n{stderr}")
}

fn process_exit_error(
    child: &mut tokio::process::Child,
    stdout_log_path: &std::path::Path,
    stderr_log_path: &std::path::Path,
) -> Result<Option<String>, String> {
    child
        .try_wait()
        .map_err(|error| format!("failed to inspect synctv process: {error}"))
        .map(|status| {
            status.map(|status| {
                format!(
                    "synctv serve exited before becoming ready with status {status}\n{}",
                    cli_process_failure_details(stdout_log_path, stderr_log_path)
                )
            })
        })
}

fn spawn_synctv_process(
    args: &[&str],
    envs: &[(&str, &str)],
    log_dir: &std::path::Path,
) -> SpawnedSynctvProcess {
    let binary = synctv_binary_path();
    let stdout_log_path = log_dir.join("synctv-serve.stdout.log");
    let stderr_log_path = log_dir.join("synctv-serve.stderr.log");
    let stdout_file =
        std::fs::File::create(&stdout_log_path).expect("synctv serve stdout log should be created");
    let stderr_file =
        std::fs::File::create(&stderr_log_path).expect("synctv serve stderr log should be created");

    let mut command = tokio::process::Command::new(binary);
    command.args(args);
    command.env_remove("SYNCTV_CONFIG_PATH");
    command.env_remove("SYNCTV_MANAGEMENT_ENDPOINT");
    command.env_remove("SYNCTV_MANAGEMENT_AUTH_TOKEN");
    command.env_remove("XDG_CONFIG_HOME");
    command.env_remove("XDG_RUNTIME_DIR");
    for (name, value) in envs {
        command.env(name, value);
    }
    command.kill_on_drop(true);
    command.stdout(std::process::Stdio::from(stdout_file));
    command.stderr(std::process::Stdio::from(stderr_file));

    let child = command
        .spawn()
        .expect("synctv serve process should start from the freshly built binary");

    SpawnedSynctvProcess {
        child,
        stdout_log_path,
        stderr_log_path,
    }
}

async fn wait_until_http_live_or_process_exit(
    http_base_url: &str,
    child: &mut tokio::process::Child,
    stdout_log_path: &std::path::Path,
    stderr_log_path: &std::path::Path,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|error| format!("failed to build HTTP client: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let url = format!("{http_base_url}/health/live");

    loop {
        if let Some(error) = process_exit_error(child, stdout_log_path, stderr_log_path)? {
            return Err(error);
        }

        match client.get(&url).send().await {
            Ok(response) if response.status() == StatusCode::OK => return Ok(()),
            Ok(_) | Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok(response) => {
                return Err(format!(
                    "health endpoint never became live, last status: {}\n{}",
                    response.status(),
                    cli_process_failure_details(stdout_log_path, stderr_log_path)
                ));
            }
            Err(error) => {
                return Err(format!(
                    "health endpoint never became live: {error}\n{}",
                    cli_process_failure_details(stdout_log_path, stderr_log_path)
                ));
            }
        }
    }
}

async fn wait_until_grpc_ready_or_process_exit(
    grpc_base_url: &str,
    child: &mut tokio::process::Child,
    stdout_log_path: &std::path::Path,
    stderr_log_path: &std::path::Path,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        if let Some(error) = process_exit_error(child, stdout_log_path, stderr_log_path)? {
            return Err(error);
        }

        let status = match tonic::transport::Endpoint::from_shared(grpc_base_url.to_string())
            .map_err(|error| format!("invalid gRPC endpoint {grpc_base_url}: {error}"))?
            .connect()
            .await
        {
            Ok(channel) => {
                let mut health = HealthClient::new(channel);
                health
                    .check(HealthCheckRequest {
                        service: String::new(),
                    })
                    .await
                    .map(|response| response.into_inner().status)
            }
            Err(error) => Err(tonic::Status::unavailable(error.to_string())),
        };

        match status {
            Ok(status) if status == ServingStatus::Serving as i32 => return Ok(()),
            Ok(_) | Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok(status) => {
                return Err(format!(
                    "gRPC health never became serving, last status: {status}\n{}",
                    cli_process_failure_details(stdout_log_path, stderr_log_path)
                ));
            }
            Err(error) => {
                return Err(format!(
                    "gRPC health never became ready: {error}\n{}",
                    cli_process_failure_details(stdout_log_path, stderr_log_path)
                ));
            }
        }
    }
}

fn synctv_binary_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_synctv") {
        return path.into();
    }

    if let Some(path) = option_env!("CARGO_BIN_EXE_synctv") {
        return path.into();
    }

    panic!(
        "failed to locate the freshly built synctv binary: CARGO_BIN_EXE_synctv is not available"
    );
}

/// Creates a test HTTP client with reasonable timeouts for E2E tests.
fn test_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("HTTP client should build")
}

async fn post_json(
    client: &reqwest::Client,
    url: &str,
    body: impl Serialize,
    bearer: Option<&str>,
) -> reqwest::Response {
    let request = client.post(url).json(&body);
    let request = if let Some(token) = bearer {
        request.bearer_auth(token)
    } else {
        request
    };
    request.send().await.unwrap_or_else(|e| {
        panic!("HTTP POST to {url} failed: {e}");
    })
}

async fn opaque_grpc_register(
    auth_client: &mut synctv_proto::client::auth_service_client::AuthServiceClient<
        tonic::transport::Channel,
    >,
    username: &str,
    email: &str,
    password: &str,
) -> synctv_proto::client::RegisterResponse {
    use synctv_proto::client::{FinishOpaqueRegistrationRequest, StartOpaqueRegistrationRequest};

    let mut rng = OsRng;
    let client_start =
        ClientRegistration::<TestOpaqueCipherSuite>::start(&mut rng, password.as_bytes())
            .expect("client OPAQUE registration start should succeed");
    let challenge = auth_client
        .start_opaque_registration(StartOpaqueRegistrationRequest {
            username: username.to_string(),
            email: Some(email.to_string()),
            registration_request: client_start.message.serialize().to_vec(),
        })
        .await
        .expect("grpc OPAQUE registration start should succeed")
        .into_inner();
    let registration_response = RegistrationResponse::<TestOpaqueCipherSuite>::deserialize(
        &challenge.registration_response,
    )
    .expect("server registration response should deserialize");
    let client_finish = client_start
        .state
        .finish(
            &mut rng,
            password.as_bytes(),
            registration_response,
            ClientRegistrationFinishParameters::default(),
        )
        .expect("client OPAQUE registration finish should succeed");

    auth_client
        .finish_opaque_registration(FinishOpaqueRegistrationRequest {
            session_id: challenge.session_id,
            registration_upload: client_finish.message.serialize().to_vec(),
        })
        .await
        .expect("grpc OPAQUE registration finish should succeed")
        .into_inner()
}

async fn opaque_grpc_login(
    auth_client: &mut synctv_proto::client::auth_service_client::AuthServiceClient<
        tonic::transport::Channel,
    >,
    username: &str,
    password: &str,
) -> synctv_proto::client::LoginResponse {
    use synctv_proto::client::{FinishOpaqueLoginRequest, StartOpaqueLoginRequest};

    let mut rng = OsRng;
    let client_start = ClientLogin::<TestOpaqueCipherSuite>::start(&mut rng, password.as_bytes())
        .expect("client OPAQUE login start should succeed");
    let challenge = auth_client
        .start_opaque_login(StartOpaqueLoginRequest {
            identifier: Some(
                synctv_proto::client::start_opaque_login_request::Identifier::Username(
                    username.to_string(),
                ),
            ),
            credential_request: client_start.message.serialize().to_vec(),
        })
        .await
        .expect("grpc OPAQUE login start should succeed")
        .into_inner();
    let credential_response =
        CredentialResponse::<TestOpaqueCipherSuite>::deserialize(&challenge.credential_response)
            .expect("server credential response should deserialize");
    let client_finish = client_start
        .state
        .finish(
            &mut rng,
            password.as_bytes(),
            credential_response,
            ClientLoginFinishParameters::default(),
        )
        .expect("client OPAQUE login finish should succeed");

    auth_client
        .finish_opaque_login(FinishOpaqueLoginRequest {
            session_id: challenge.session_id,
            credential_finalization: client_finish.message.serialize().to_vec(),
        })
        .await
        .expect("grpc OPAQUE login finish should succeed")
        .into_inner()
}

async fn opaque_grpc_set_room_password(
    room_client: &mut synctv_proto::client::room_service_client::RoomServiceClient<
        tonic::transport::Channel,
    >,
    room_id: &str,
    bearer: &str,
    password: &str,
) -> synctv_proto::client::SetRoomPasswordResponse {
    use synctv_proto::client::{
        FinishRoomPasswordRegistrationRequest, StartRoomPasswordRegistrationRequest,
    };

    let mut rng = OsRng;
    let client_start =
        ClientRegistration::<TestOpaqueCipherSuite>::start(&mut rng, password.as_bytes())
            .expect("client room OPAQUE registration start should succeed");
    let mut start_request = tonic::Request::new(StartRoomPasswordRegistrationRequest {
        registration_request: client_start.message.serialize().to_vec(),
    });
    start_request
        .metadata_mut()
        .insert("authorization", bearer_metadata(bearer));
    start_request
        .metadata_mut()
        .insert("x-room-id", room_id_metadata(room_id));
    let challenge = room_client
        .start_room_password_registration(start_request)
        .await
        .expect("room OPAQUE registration start should succeed")
        .into_inner();
    let registration_response = RegistrationResponse::<TestOpaqueCipherSuite>::deserialize(
        &challenge.registration_response,
    )
    .expect("room registration response should deserialize");
    let client_finish = client_start
        .state
        .finish(
            &mut rng,
            password.as_bytes(),
            registration_response,
            ClientRegistrationFinishParameters::default(),
        )
        .expect("client room OPAQUE registration finish should succeed");
    let mut finish_request = tonic::Request::new(FinishRoomPasswordRegistrationRequest {
        session_id: challenge.session_id,
        registration_upload: client_finish.message.serialize().to_vec(),
    });
    finish_request
        .metadata_mut()
        .insert("authorization", bearer_metadata(bearer));
    finish_request
        .metadata_mut()
        .insert("x-room-id", room_id_metadata(room_id));

    room_client
        .finish_room_password_registration(finish_request)
        .await
        .expect("room OPAQUE registration finish should succeed")
        .into_inner()
}

async fn opaque_grpc_join_room(
    user_client: &mut synctv_proto::client::user_service_client::UserServiceClient<
        tonic::transport::Channel,
    >,
    room_id: &str,
    bearer: &str,
    password: &str,
) -> synctv_proto::client::JoinRoomResponse {
    use synctv_proto::client::{FinishRoomPasswordLoginRequest, StartRoomPasswordLoginRequest};

    let mut rng = OsRng;
    let client_start = ClientLogin::<TestOpaqueCipherSuite>::start(&mut rng, password.as_bytes())
        .expect("client room OPAQUE login start should succeed");
    let mut start_request = tonic::Request::new(StartRoomPasswordLoginRequest {
        room_id: room_id.to_string(),
        credential_request: client_start.message.serialize().to_vec(),
    });
    start_request
        .metadata_mut()
        .insert("authorization", bearer_metadata(bearer));
    let challenge = user_client
        .start_room_password_login(start_request)
        .await
        .expect("room OPAQUE login start should succeed")
        .into_inner();
    let credential_response =
        CredentialResponse::<TestOpaqueCipherSuite>::deserialize(&challenge.credential_response)
            .expect("room credential response should deserialize");
    let client_finish = client_start
        .state
        .finish(
            &mut rng,
            password.as_bytes(),
            credential_response,
            ClientLoginFinishParameters::default(),
        )
        .expect("client room OPAQUE login finish should succeed");
    let mut finish_request = tonic::Request::new(FinishRoomPasswordLoginRequest {
        session_id: challenge.session_id,
        credential_finalization: client_finish.message.serialize().to_vec(),
    });
    finish_request
        .metadata_mut()
        .insert("authorization", bearer_metadata(bearer));

    user_client
        .finish_room_password_login(finish_request)
        .await
        .expect("room OPAQUE login finish should succeed")
        .into_inner()
}

async fn put_json(
    client: &reqwest::Client,
    url: &str,
    body: impl Serialize,
    bearer: &str,
) -> reqwest::Response {
    client
        .put(url)
        .bearer_auth(bearer)
        .json(&body)
        .send()
        .await
        .unwrap_or_else(|e| {
            panic!("HTTP PUT to {url} failed: {e}");
        })
}

async fn get_with_bearer(client: &reqwest::Client, url: &str, bearer: &str) -> reqwest::Response {
    client
        .get(url)
        .bearer_auth(bearer)
        .send()
        .await
        .unwrap_or_else(|e| {
            panic!("HTTP GET from {url} failed: {e}");
        })
}

async fn response_json(response: reqwest::Response) -> Value {
    let url = response.url().clone();
    let status = response.status();
    response.json::<Value>().await.unwrap_or_else(|e| {
        panic!("failed to parse JSON from {url} (status {status}): {e}");
    })
}

async fn opaque_http_register(
    server: &TestServer,
    username: &str,
    email: &str,
    password: &str,
) -> reqwest::Response {
    use synctv_proto::client::{FinishOpaqueRegistrationRequest, StartOpaqueRegistrationRequest};

    let client = test_http_client();
    let mut rng = OsRng;
    let client_start =
        ClientRegistration::<TestOpaqueCipherSuite>::start(&mut rng, password.as_bytes())
            .expect("client OPAQUE registration start should succeed");

    let start = post_json(
        &client,
        &format!("{}/api/auth/opaque/registration/start", server.api_base_url),
        StartOpaqueRegistrationRequest {
            username: username.to_string(),
            email: Some(email.to_string()),
            registration_request: client_start.message.serialize().to_vec(),
        },
        None,
    )
    .await;
    if start.status() != StatusCode::OK {
        return start;
    }

    let challenge = response_json(start).await;
    let session_id = challenge["sessionId"]
        .as_str()
        .expect("OPAQUE registration start should return session_id")
        .to_string();
    let registration_response = BASE64_STANDARD
        .decode(
            challenge["registrationResponse"]
                .as_str()
                .expect("OPAQUE registration start should return base64 registration_response"),
        )
        .expect("registration_response should decode as base64 bytes");
    let registration_response =
        RegistrationResponse::<TestOpaqueCipherSuite>::deserialize(&registration_response)
            .expect("server registration response should deserialize");
    let client_finish = client_start
        .state
        .finish(
            &mut rng,
            password.as_bytes(),
            registration_response,
            ClientRegistrationFinishParameters::default(),
        )
        .expect("client OPAQUE registration finish should succeed");

    post_json(
        &client,
        &format!(
            "{}/api/auth/opaque/registration/finish",
            server.api_base_url
        ),
        FinishOpaqueRegistrationRequest {
            session_id,
            registration_upload: client_finish.message.serialize().to_vec(),
        },
        None,
    )
    .await
}

async fn opaque_http_login_token(
    server: &TestServer,
    username: &str,
    password: &str,
) -> Result<String, String> {
    use synctv_proto::client::{
        start_opaque_login_request::Identifier, FinishOpaqueLoginRequest, StartOpaqueLoginRequest,
    };

    let client = test_http_client();
    let mut rng = OsRng;
    let client_start = ClientLogin::<TestOpaqueCipherSuite>::start(&mut rng, password.as_bytes())
        .expect("client OPAQUE login start should succeed");
    let start = post_json(
        &client,
        &format!("{}/api/auth/opaque/login/start", server.api_base_url),
        StartOpaqueLoginRequest {
            identifier: Some(Identifier::Username(username.to_string())),
            credential_request: client_start.message.serialize().to_vec(),
        },
        None,
    )
    .await;
    let start_status = start.status();
    if start_status != StatusCode::OK {
        let body = response_json(start).await;
        return Err(format!(
            "OPAQUE login start for {username} failed with {start_status}: {body}"
        ));
    }

    let challenge = response_json(start).await;
    let session_id = challenge["sessionId"]
        .as_str()
        .expect("OPAQUE login start should return session_id")
        .to_string();
    let credential_response = BASE64_STANDARD
        .decode(
            challenge["credentialResponse"]
                .as_str()
                .expect("OPAQUE login start should return base64 credential_response"),
        )
        .expect("credential_response should decode as base64 bytes");
    let credential_response =
        CredentialResponse::<TestOpaqueCipherSuite>::deserialize(&credential_response)
            .expect("server credential response should deserialize");
    let client_finish = client_start
        .state
        .finish(
            &mut rng,
            password.as_bytes(),
            credential_response,
            ClientLoginFinishParameters::default(),
        )
        .map_err(|_| format!("OPAQUE login finalization failed for {username}"))?;

    let finish = post_json(
        &client,
        &format!("{}/api/auth/opaque/login/finish", server.api_base_url),
        FinishOpaqueLoginRequest {
            session_id,
            credential_finalization: client_finish.message.serialize().to_vec(),
        },
        None,
    )
    .await;
    let finish_status = finish.status();
    let body = response_json(finish).await;
    if finish_status != StatusCode::OK {
        return Err(format!(
            "OPAQUE login finish for {username} failed with {finish_status}: {body}"
        ));
    }
    body["accessToken"]
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| format!("OPAQUE login response for {username} lacked accessToken: {body}"))
}

async fn login_http_ok_token(server: &TestServer, username: &str, password: &str) -> String {
    opaque_http_login_token(server, username, password)
        .await
        .unwrap_or_else(|error| panic!("login for {username} should succeed: {error}"))
}

async fn join_room_http(
    server: &TestServer,
    room_id: &str,
    password: &str,
    bearer: &str,
) -> reqwest::Response {
    let client = test_http_client();
    put_json(
        &client,
        &format!("{}/api/rooms/{room_id}/members/@me", server.api_base_url),
        synctv_proto::client::JoinRoomRequest {
            room_id: room_id.to_string(),
            password: password.to_string(),
            remark_name: String::new(),
            display_tag: String::new(),
        },
        bearer,
    )
    .await
}

async fn create_cli_user(
    server: &TestServer,
    username: &str,
    email: &str,
    password: &str,
    status: Option<&str>,
    context: &str,
) -> Value {
    let mut args = vec![
        "user",
        "create",
        username,
        "--email",
        email,
        "--password",
        password,
    ];
    if let Some(status) = status {
        args.extend(["--status", status]);
    }

    run_synctv_remote_cli_json(server, &args, context).await
}

struct IdleRtmpPublisher {
    session_task: Option<tokio::task::JoinHandle<()>>,
}

impl IdleRtmpPublisher {
    async fn shutdown(&mut self, context: &str) {
        if let Some(task) = self.session_task.take() {
            task.abort();
            tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .unwrap_or_else(|_| panic!("{context} RTMP publisher task should stop"))
                .unwrap_or_else(|error| assert!(error.is_cancelled()));
        }
    }
}

impl Drop for IdleRtmpPublisher {
    fn drop(&mut self) {
        if let Some(task) = self.session_task.take() {
            task.abort();
        }
    }
}

async fn perform_rtmp_handshake(
    io: std::sync::Arc<tokio::sync::Mutex<Box<dyn TNetIO + Send + Sync>>>,
) -> Result<(), String> {
    let mut handshaker = SimpleHandshakeClient::new(std::sync::Arc::clone(&io));
    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        handshaker
            .handshake()
            .await
            .map_err(|error| format!("RTMP handshake step failed: {error}"))?;
        if handshaker.state == ClientHandshakeState::Finish {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err("RTMP handshake timed out".to_string());
        }

        let data = io
            .lock()
            .await
            .read_timeout(Duration::from_secs(2))
            .await
            .map_err(|error| format!("RTMP handshake read failed: {error}"))?;
        handshaker
            .extend_data(&data)
            .map_err(|error| format!("RTMP handshake buffer update failed: {error}"))?;
    }
}

fn is_rtmp_transaction_id(value: &Amf0ValueType, expected: u8) -> bool {
    matches!(
        value,
        Amf0ValueType::Number(number) if (*number - f64::from(expected)).abs() < f64::EPSILON
    )
}

fn rtmp_command_name(value: &Amf0ValueType) -> Option<&str> {
    match value {
        Amf0ValueType::UTF8String(value) => Some(value.as_str()),
        _ => None,
    }
}

fn rtmp_status_code(others: &[Amf0ValueType]) -> Option<&str> {
    match others.first() {
        Some(Amf0ValueType::Object(object)) => match object.get("code") {
            Some(Amf0ValueType::UTF8String(code)) => Some(code.as_str()),
            _ => None,
        },
        _ => None,
    }
}

async fn run_idle_rtmp_publisher(
    host: String,
    port: u16,
    app_name: String,
    raw_stream_name: String,
    started_tx: tokio::sync::oneshot::Sender<Result<(), String>>,
) -> Result<(), String> {
    let tcp_stream = tokio::net::TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|error| format!("RTMP publisher should connect to {host}:{port}: {error}"))?;
    let io: std::sync::Arc<tokio::sync::Mutex<Box<dyn TNetIO + Send + Sync>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(Box::new(TcpIO::new(tcp_stream))));
    perform_rtmp_handshake(std::sync::Arc::clone(&io)).await?;

    let mut control_messages = ProtocolControlMessagesWriter::new(
        synctv_xiu::bytesio::bytes_writer::AsyncBytesWriter::new(std::sync::Arc::clone(&io)),
    );
    control_messages
        .write_set_chunk_size(4096)
        .await
        .map_err(|error| format!("RTMP publisher should send chunk size: {error}"))?;

    let mut connect_properties = ConnectProperties::new_none();
    connect_properties.app = Some(app_name.clone());
    connect_properties.pub_type = Some("nonprivate".to_string());
    connect_properties.flash_ver = Some("FMLE/3.0 (compatible; xiu)".to_string());
    connect_properties.fpad = Some(false);
    connect_properties.tc_url = Some(format!("rtmp://{host}:{port}/{app_name}"));

    let mut netconnection = NetConnection::new(std::sync::Arc::clone(&io));
    netconnection
        .write_connect(&1.0, &connect_properties)
        .await
        .map_err(|error| format!("RTMP publisher should send connect: {error}"))?;

    let mut unpackizer = ChunkUnpacketizer::new();
    let mut create_stream_sent = false;
    let mut publish_sent = false;
    let mut publish_started = false;
    let mut started_tx = Some(started_tx);
    let mut keepalive = tokio::time::interval(Duration::from_millis(500));
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let read_result = if publish_started {
            tokio::select! {
                read_result = async {
                    io.lock().await.read_timeout(Duration::from_millis(500)).await
                } => read_result,
                _ = keepalive.tick() => {
                    let mut control_messages = ProtocolControlMessagesWriter::new(
                        synctv_xiu::bytesio::bytes_writer::AsyncBytesWriter::new(
                            std::sync::Arc::clone(&io),
                        ),
                    );
                    control_messages
                        .write_acknowledgement(0)
                        .await
                        .map_err(|error| {
                            format!("RTMP publisher should send keepalive acknowledgement: {error}")
                        })?;
                    continue;
                }
            }
        } else {
            io.lock().await.read_timeout(Duration::from_secs(15)).await
        };

        let data = match read_result {
            Ok(data) => data,
            Err(error)
                if publish_started && matches!(error.value, BytesIOErrorValue::TimeoutError(_)) =>
            {
                continue;
            }
            Err(error)
                if publish_started
                    && matches!(
                        error.value,
                        BytesIOErrorValue::EmptyStream
                            | BytesIOErrorValue::ConnectionClosed
                            | BytesIOErrorValue::IOError(_)
                    ) =>
            {
                return Ok(());
            }
            Err(error) => {
                return Err(format!("RTMP publisher read failed after connect: {error}"));
            }
        };

        unpackizer
            .extend_data(&data)
            .map_err(|error| format!("RTMP publisher failed to buffer server data: {error}"))?;

        loop {
            match unpackizer.read_chunks() {
                Ok(UnpackResult::Chunks(chunks)) => {
                    for chunk in chunks {
                        let mut message = MessageParser::new(chunk)
                            .parse()
                            .map_err(|error| {
                                format!("RTMP publisher failed to parse message: {error}")
                            })?
                            .ok_or_else(|| {
                                "RTMP publisher received an empty message".to_string()
                            })?;

                        match &mut message {
                            RtmpMessageData::SetChunkSize { chunk_size } => {
                                unpackizer.update_max_chunk_size((*chunk_size).clamp(128, 65536) as usize);
                            }
                            RtmpMessageData::SetPeerBandwidth { .. } => {
                                let mut control_messages = ProtocolControlMessagesWriter::new(
                                    synctv_xiu::bytesio::bytes_writer::AsyncBytesWriter::new(
                                        std::sync::Arc::clone(&io),
                                    ),
                                );
                                control_messages
                                    .write_window_acknowledgement_size(5_000_000)
                                    .await
                                    .map_err(|error| {
                                        format!("RTMP publisher should acknowledge peer bandwidth: {error}")
                                    })?;
                            }
                            RtmpMessageData::Amf0Command(command) => {
                                match rtmp_command_name(&command.command_name) {
                                    Some("_result")
                                        if is_rtmp_transaction_id(&command.transaction_id, 1)
                                            && !create_stream_sent =>
                                    {
                                        create_stream_sent = true;
                                        let mut netconnection =
                                            NetConnection::new(std::sync::Arc::clone(&io));
                                        netconnection.write_create_stream(&2.0).await.map_err(
                                            |error| {
                                                format!(
                                                "RTMP publisher should send createStream: {error}"
                                            )
                                            },
                                        )?;
                                    }
                                    Some("_result")
                                        if is_rtmp_transaction_id(&command.transaction_id, 2)
                                            && !publish_sent =>
                                    {
                                        publish_sent = true;
                                        let mut netstream =
                                            NetStreamWriter::new(std::sync::Arc::clone(&io));
                                        netstream
                                            .write_publish(
                                                &3.0,
                                                &raw_stream_name,
                                                &"live".to_string(),
                                            )
                                            .await
                                            .map_err(|error| {
                                                format!(
                                                    "RTMP publisher should send publish: {error}"
                                                )
                                            })?;
                                    }
                                    Some("onStatus")
                                        if rtmp_status_code(&command.others)
                                            == Some("NetStream.Publish.Start") =>
                                    {
                                        publish_started = true;
                                        if let Some(started_tx) = started_tx.take() {
                                            let _ = started_tx.send(Ok(()));
                                        }
                                    }
                                    Some("_error") => {
                                        return Err(format!(
                                        "RTMP server returned an error during publish setup: {:?}",
                                        command.others
                                    ));
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Ok(
                    UnpackResult::ChunkBasicHeaderResult(_)
                    | UnpackResult::ChunkMessageHeaderResult(_)
                    | UnpackResult::ChunkInfo(_)
                    | UnpackResult::Success
                    | UnpackResult::NotEnoughBytes
                    | UnpackResult::Empty,
                ) => {}
                Err(_) => break,
            }
        }
    }
}

async fn spawn_idle_rtmp_publisher(rtmp_url: &str, stream_key: &str) -> IdleRtmpPublisher {
    let parsed = url::Url::parse(rtmp_url)
        .unwrap_or_else(|error| panic!("RTMP publish URL should parse: {error}"));
    let host = parsed
        .host_str()
        .unwrap_or_else(|| panic!("RTMP publish URL should contain a host: {rtmp_url}"))
        .to_string();
    let port = parsed.port().unwrap_or(1935);
    let mut path_segments = parsed
        .path_segments()
        .unwrap_or_else(|| panic!("RTMP publish URL should expose path segments: {rtmp_url}"));
    let app_name = path_segments
        .next()
        .unwrap_or_else(|| panic!("RTMP publish URL should include an app name: {rtmp_url}"))
        .to_string();
    assert!(
        path_segments.next().is_none(),
        "RTMP publish URL should map directly to the room app path: {rtmp_url}"
    );
    assert!(
        !app_name.is_empty(),
        "RTMP publish URL should include the RTMP app path: {rtmp_url}"
    );
    let raw_stream_name = stream_key.to_string();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let session_task = tokio::spawn(async move {
        if let Err(error) =
            run_idle_rtmp_publisher(host, port, app_name, raw_stream_name, started_tx).await
        {
            panic!("RTMP idle publisher failed: {error}");
        }
    });

    tokio::time::timeout(Duration::from_secs(15), started_rx)
        .await
        .unwrap_or_else(|_| panic!("RTMP publisher should reach NetStream.Publish.Start"))
        .unwrap_or_else(|_| panic!("RTMP publisher setup channel should stay open"))
        .unwrap_or_else(|error| panic!("RTMP publisher should start successfully: {error}"));

    IdleRtmpPublisher {
        session_task: Some(session_task),
    }
}

async fn wait_for_remote_cli_json<F>(
    server: &TestServer,
    args: &[&str],
    context: &str,
    predicate: F,
) -> Value
where
    F: Fn(&Value) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(15);

    loop {
        let response = run_synctv_remote_cli_json(server, args, context).await;
        if predicate(&response) {
            return response;
        }

        assert!(
            Instant::now() < deadline,
            "{context} did not reach the expected state before timeout; last response: {response}"
        );

        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_room_stream_total(
    server: &TestServer,
    room_id: &str,
    expected_total: i64,
) -> Value {
    wait_for_remote_cli_json(
        server,
        &["room", "stream", "list", "--room-id", room_id],
        "wait for room stream total",
        |response| response["total"].as_i64().unwrap_or(0) == expected_total,
    )
    .await
}

async fn wait_for_system_stream_count(
    server: &TestServer,
    room_id: &str,
    expected_count: usize,
) -> Value {
    wait_for_remote_cli_json(
        server,
        &["system", "stream", "list", "--room-id", room_id],
        "wait for system stream count",
        |response| {
            response["streams"]
                .as_array()
                .map_or(expected_count == 0, |streams| {
                    streams.len() == expected_count
                })
        },
    )
    .await
}

async fn ws_connect_with_ticket(
    addr: &str,
    room_id: &str,
    ticket: &str,
) -> Result<
    (
        TestWebSocketStream,
        tungstenite::handshake::client::Response,
    ),
    tokio_tungstenite::tungstenite::Error,
> {
    tokio_tungstenite::connect_async(format!(
        "ws://{addr}/ws/rooms/{room_id}?ticket={ticket}&format=protobuf"
    ))
    .await
}

async fn ws_connect(addr: &str, room_id: &str, token: &str) -> TestWebSocketStream {
    let url = format!("ws://{addr}/ws/rooms/{room_id}?format=protobuf");
    let request = tungstenite::http::Request::builder()
        .uri(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .header("Host", addr)
        .body(())
        .expect("build websocket request");
    let (ws_stream, response) = tokio_tungstenite::connect_async(request)
        .await
        .expect("websocket connect should succeed");
    assert_eq!(
        response.status(),
        tungstenite::http::StatusCode::SWITCHING_PROTOCOLS,
        "websocket handshake should return 101"
    );
    ws_stream
}

async fn recv_server_message(ws: &mut TestWebSocketStream) -> Option<ServerMessage> {
    while let Some(message) = ws.next().await {
        match message {
            Ok(tungstenite::Message::Binary(bytes)) => {
                return Some(ServerMessage::decode(bytes.as_ref()).expect("decode server message"));
            }
            Ok(tungstenite::Message::Close(_)) => return None,
            Ok(_) => {}
            Err(error) => panic!("websocket read failed: {error}"),
        }
    }

    None
}

async fn drain_until_quiet(ws: &mut TestWebSocketStream, quiet_ms: u64) -> Vec<ServerMessage> {
    let mut collected = Vec::new();
    while let Ok(Some(message)) =
        tokio::time::timeout(Duration::from_millis(quiet_ms), recv_server_message(ws)).await
    {
        collected.push(message);
    }
    collected
}

async fn recv_matching_server_message<F>(
    ws: &mut TestWebSocketStream,
    timeout: Duration,
    mut predicate: F,
    context: &str,
) -> ServerMessage
where
    F: FnMut(&ServerMessage) -> bool,
{
    let deadline = Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let message = tokio::time::timeout(remaining, recv_server_message(ws))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {context}"))
            .unwrap_or_else(|| panic!("websocket closed while waiting for {context}"));
        if predicate(&message) {
            return message;
        }

        assert!(
            Instant::now() < deadline,
            "did not receive {context} before timeout; last message: {message:?}"
        );
    }
}

async fn send_client_message(
    ws: &mut TestWebSocketStream,
    message: synctv_proto::client::ClientMessage,
) {
    ws.send(tungstenite::Message::Binary(message.encode_to_vec().into()))
        .await
        .expect("send websocket client message");
}

fn observe_chat_events_message(observe_id: &str) -> synctv_proto::client::ClientMessage {
    use synctv_proto::client::{
        client_message, observe_resource, ObserveChatEvents, ObserveResource, ResourceDeliveryMode,
    };

    synctv_proto::client::ClientMessage {
        message: Some(client_message::Message::ObserveResource(ObserveResource {
            observe_id: observe_id.to_string(),
            delivery_mode: ResourceDeliveryMode::NotifyOnly as i32,
            resource: Some(observe_resource::Resource::ChatEvents(ObserveChatEvents {
                after_event_sequence: None,
                include_message_types: Vec::new(),
            })),
        })),
    }
}

fn resource_chat_event(message: &ServerMessage) -> Option<&synctv_proto::client::ChatMessageEvent> {
    match &message.message {
        Some(server_message::Message::ResourceEvent(resource_event)) => {
            match resource_event.payload.as_ref() {
                Some(synctv_proto::client::resource_event::Payload::ChatEvent(event)) => {
                    Some(event)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

struct RoomRealtimeFixture {
    server: TestServer,
    api_addr: String,
    room_id: String,
    owner_username: String,
    owner_token: String,
    member_username: String,
    member_user_id: String,
    member_token: String,
}

async fn start_room_realtime_fixture(label: &str) -> RoomRealtimeFixture {
    let server = start_test_server().await;
    let suffix = unique_test_suffix();
    let owner_username = bounded_test_username(label, "owner", &suffix);
    let owner_email = format!("{label}-owner-{suffix}@example.com");
    let owner_password = "RealtimeOwnerPass12345!";
    let member_username = bounded_test_username(label, "member", &suffix);
    let member_email = format!("{label}-member-{suffix}@example.com");
    let member_password = "RealtimeMemberPass12345!";

    create_cli_user(
        &server,
        &owner_username,
        &owner_email,
        owner_password,
        Some("active"),
        "create realtime owner",
    )
    .await;
    let created_member = create_cli_user(
        &server,
        &member_username,
        &member_email,
        member_password,
        Some("active"),
        "create realtime member",
    )
    .await;
    let member_user_id = created_member["id"]
        .as_str()
        .expect("member create should return user id")
        .to_string();

    let owner_token = login_http_ok_token(&server, &owner_username, owner_password).await;
    let member_token = login_http_ok_token(&server, &member_username, member_password).await;

    let created_room = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "create",
            &format!("{label} room {suffix}"),
            "--username",
            &owner_username,
        ],
        "create realtime room",
    )
    .await;
    let room_id = created_room["id"]
        .as_str()
        .expect("room create should return room id")
        .to_string();

    let join_response = join_room_http(&server, &room_id, "", &member_token).await;
    assert_eq!(
        join_response.status(),
        StatusCode::OK,
        "member should join realtime room"
    );

    let api_addr = server
        .api_base_url
        .strip_prefix("http://")
        .or_else(|| server.api_base_url.strip_prefix("https://"))
        .unwrap_or_else(|| panic!("unexpected api base url: {}", server.api_base_url))
        .to_string();

    RoomRealtimeFixture {
        server,
        api_addr,
        room_id,
        owner_username,
        owner_token,
        member_username,
        member_user_id,
        member_token,
    }
}

fn error_message(body: &Value) -> &str {
    body["message"].as_str().unwrap_or("<missing error>")
}

fn bearer_metadata(token: &str) -> MetadataValue<tonic::metadata::Ascii> {
    format!("Bearer {token}")
        .parse()
        .expect("valid bearer metadata")
}

fn room_id_metadata(room_id: &str) -> MetadataValue<tonic::metadata::Ascii> {
    room_id.parse().expect("valid x-room-id metadata")
}

fn management_request<T>(message: T) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    request
        .metadata_mut()
        .insert("authorization", bearer_metadata(MANAGEMENT_E2E_AUTH_TOKEN));
    request
}

async fn recv_grpc_server_message(
    stream: &mut tonic::codec::Streaming<synctv_proto::client::ServerMessage>,
) -> Option<synctv_proto::client::ServerMessage> {
    match stream.message().await {
        Ok(message) => message,
        Err(error) => panic!("gRPC stream read failed: {error}"),
    }
}

async fn recv_matching_grpc_server_message<F>(
    stream: &mut tonic::codec::Streaming<synctv_proto::client::ServerMessage>,
    timeout: Duration,
    mut predicate: F,
    context: &str,
) -> synctv_proto::client::ServerMessage
where
    F: FnMut(&synctv_proto::client::ServerMessage) -> bool,
{
    let deadline = Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let message = tokio::time::timeout(remaining, recv_grpc_server_message(stream))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {context}"))
            .unwrap_or_else(|| panic!("gRPC stream closed while waiting for {context}"));
        if predicate(&message) {
            return message;
        }

        assert!(
            Instant::now() < deadline,
            "did not receive {context} before timeout; last message: {message:?}"
        );
    }
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_health_endpoints_report_live_and_ready() {
    let server = start_test_server().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("HTTP client");

    let live = client
        .get(format!("{}/health/live", server.api_base_url))
        .send()
        .await
        .expect("liveness request");
    assert_eq!(live.status(), StatusCode::OK);
    let live_body = response_json(live).await;
    assert_eq!(live_body["status"], "ok");

    let ready = client
        .get(format!("{}/health/ready", server.api_base_url))
        .send()
        .await
        .expect("readiness request");
    assert_eq!(ready.status(), StatusCode::OK);
    let ready_body = response_json(ready).await;
    assert_eq!(ready_body["status"], "healthy");
    assert_eq!(ready_body["details"]["database"], "healthy");
    assert_eq!(ready_body["details"]["redis"], "healthy");
    assert_eq!(
        ready_body["details"]["wsTicket"],
        "healthy (cross-node capable ticket storage)"
    );

    // per-test isolated server
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_management_exposes_only_remote_admin_surface() {
    use synctv_api::{AdminServiceImpl, ClientServiceImpl};
    use synctv_management::proto::management_service_server::ManagementServiceServer;
    use synctv_management::ManagementServiceImpl;
    use synctv_proto::admin::admin_service_server::AdminServiceServer;
    use synctv_proto::client::{
        auth_service_server::AuthServiceServer, public_service_server::PublicServiceServer,
        room_service_server::RoomServiceServer, user_service_server::UserServiceServer,
    };

    let server = start_test_server().await;

    assert_eq!(
        grpc_health_status(&server.management_base_url, "").await,
        Ok(ServingStatus::Serving),
    );
    assert_eq!(
        grpc_health_status(
            &server.management_base_url,
            <ManagementServiceServer<ManagementServiceImpl> as tonic::server::NamedService>::NAME,
        )
        .await,
        Ok(ServingStatus::Serving),
    );
    assert_eq!(
        grpc_health_status(
            &server.management_base_url,
            <AuthServiceServer<ClientServiceImpl> as tonic::server::NamedService>::NAME,
        )
        .await,
        Err(tonic::Code::NotFound),
    );
    assert_eq!(
        grpc_health_status(
            &server.management_base_url,
            <RoomServiceServer<ClientServiceImpl> as tonic::server::NamedService>::NAME,
        )
        .await,
        Err(tonic::Code::NotFound),
    );
    assert_eq!(
        grpc_health_status(
            &server.management_base_url,
            <AdminServiceServer<AdminServiceImpl> as tonic::server::NamedService>::NAME,
        )
        .await,
        Err(tonic::Code::NotFound),
    );
    assert_eq!(
        grpc_health_status(
            &server.management_base_url,
            <UserServiceServer<ClientServiceImpl> as tonic::server::NamedService>::NAME,
        )
        .await,
        Err(tonic::Code::NotFound),
    );
    assert_eq!(
        grpc_health_status(
            &server.management_base_url,
            <PublicServiceServer<ClientServiceImpl> as tonic::server::NamedService>::NAME,
        )
        .await,
        Err(tonic::Code::NotFound),
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_room_lifecycle_commands_use_remote_management_endpoint() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use synctv_proto::client::{ListPlaylistItemsRequest, ListPlaylistsRequest};

    let server = start_test_server().await;
    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect auth gRPC client");
    let admin_login =
        opaque_grpc_login(&mut auth_client, BOOTSTRAP_ROOT_USERNAME, "StrongPwd12345!").await;

    let room_create = run_synctv_remote_cli(
        &server,
        &[
            "room",
            "create",
            "CLI managed room",
            "--username",
            BOOTSTRAP_ROOT_USERNAME,
            "--description",
            "cli remote room lifecycle e2e",
        ],
    )
    .await;
    assert!(
        room_create.status.success(),
        "room create via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&room_create.stdout),
        String::from_utf8_lossy(&room_create.stderr),
    );
    let room_create_body: Value =
        serde_json::from_slice(&room_create.stdout).expect("CLI room create output should be JSON");
    let room_id = room_create_body["id"]
        .as_str()
        .expect("CLI room create output should contain id")
        .to_string();
    assert_eq!(room_create_body["name"], "CLI managed room");

    let playlist_list =
        run_synctv_remote_cli(&server, &["playlist", "list", "--room-id", &room_id]).await;
    assert!(
        playlist_list.status.success(),
        "playlist list via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&playlist_list.stdout),
        String::from_utf8_lossy(&playlist_list.stderr),
    );

    let media_add = run_synctv_remote_cli(
        &server,
        &[
            "media",
            "add",
            "--room-id",
            &room_id,
            "--username",
            BOOTSTRAP_ROOT_USERNAME,
            "--source-provider",
            "direct-url",
            "--source-config-json",
            "{\"medias\":[{\"url\":\"https://cdn.example.com/cli-e2e.mp4\"}]}",
            "--name",
            "CLI E2E Media",
        ],
    )
    .await;
    assert!(
        media_add.status.success(),
        "media add via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&media_add.stdout),
        String::from_utf8_lossy(&media_add.stderr),
    );
    let media_add_body: Value =
        serde_json::from_slice(&media_add.stdout).expect("CLI media add output should be JSON");
    let media_one_id = media_add_body["id"]
        .as_str()
        .expect("CLI media add output should contain id")
        .to_string();

    let media_add_second = run_synctv_remote_cli(
        &server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/cli-e2e-second.mp4",
            "--room-id",
            &room_id,
            "--username",
            BOOTSTRAP_ROOT_USERNAME,
            "--name",
            "CLI E2E Media Second",
        ],
    )
    .await;
    assert!(
        media_add_second.status.success(),
        "second media add-url via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&media_add_second.stdout),
        String::from_utf8_lossy(&media_add_second.stderr),
    );
    let media_add_second_body: Value = serde_json::from_slice(&media_add_second.stdout)
        .expect("CLI second media add output should be JSON");
    let media_two_id = media_add_second_body["id"]
        .as_str()
        .expect("CLI second media add output should contain id")
        .to_string();

    let media_reorder = run_synctv_remote_cli(
        &server,
        &[
            "media",
            "move",
            "--room-id",
            &room_id,
            "--before-media-id",
            &media_one_id,
            "--media-id",
            &media_two_id,
        ],
    )
    .await;
    assert!(
        media_reorder.status.success(),
        "media move via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&media_reorder.stdout),
        String::from_utf8_lossy(&media_reorder.stderr),
    );

    let mut room_client = RoomServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect room gRPC client");

    let mut list_playlists = tonic::Request::new(ListPlaylistsRequest {
        parent_id: String::new(),
        page: 1,
        page_size: 50,
        search: String::new(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        dynamic_only: None,
        sort_by: synctv_proto::client::PlaylistListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
    });
    list_playlists
        .metadata_mut()
        .insert("authorization", bearer_metadata(&admin_login.access_token));
    list_playlists
        .metadata_mut()
        .insert("x-room-id", room_id_metadata(&room_id));
    let playlists = room_client
        .list_playlists(list_playlists)
        .await
        .expect("list_playlists should succeed after CLI call")
        .into_inner();
    assert!(
        playlists.playlists.is_empty(),
        "new room should still have no child playlists after CLI playlist list: {playlists:?}"
    );

    let mut list_items = tonic::Request::new(ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: None,
        pagination: page_pagination(1),
        page_size: 50,
        search: String::new(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    });
    list_items
        .metadata_mut()
        .insert("authorization", bearer_metadata(&admin_login.access_token));
    list_items
        .metadata_mut()
        .insert("x-room-id", room_id_metadata(&room_id));
    let items = room_client
        .list_playlist_items(list_items)
        .await
        .expect("list_playlist_items should succeed after CLI room/media lifecycle commands")
        .into_inner();
    assert_eq!(
        items.media.len(),
        2,
        "CLI media commands should create two media items"
    );
    let first_media = &items.media[0];
    let second_media = &items.media[1];
    assert_eq!(first_media.room_id, room_id);
    assert_eq!(first_media.id, media_two_id);
    assert_eq!(first_media.name, "CLI E2E Media Second");
    assert_eq!(
        first_media.source_provider,
        synctv_proto::source_config::SourceProvider::DirectUrl as i32
    );
    assert_eq!(first_media.provider_instance_name, "");
    assert_eq!(second_media.id, media_one_id);
    assert_eq!(second_media.name, "CLI E2E Media");
    assert_eq!(
        second_media.source_provider,
        synctv_proto::source_config::SourceProvider::DirectUrl as i32
    );
    assert_eq!(second_media.provider_instance_name, "");
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_user_and_room_commands_use_remote_management_endpoint() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::CreateRoomRequest;

    let server = start_test_server().await;
    let suffix = unique_test_suffix();
    let username = format!("cli_user_{suffix}");
    let email = format!("cli-user-{suffix}@example.com");
    let room_name = format!("CLI Room {suffix}");

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect public auth gRPC client");
    let admin_login =
        opaque_grpc_login(&mut auth_client, BOOTSTRAP_ROOT_USERNAME, "StrongPwd12345!").await;

    let create_user = run_synctv_remote_cli(
        &server,
        &[
            "user",
            "create",
            &username,
            "--email",
            &email,
            "--password",
            "CliUserPass12345!",
        ],
    )
    .await;
    assert!(
        create_user.status.success(),
        "user create via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&create_user.stdout),
        String::from_utf8_lossy(&create_user.stderr),
    );
    let create_user_body: Value =
        serde_json::from_slice(&create_user.stdout).expect("CLI user create output should be JSON");
    let created_user_id = create_user_body["id"]
        .as_str()
        .expect("CLI user create output should contain id")
        .to_string();
    assert_eq!(create_user_body["username"], username);
    assert_eq!(create_user_body["email"], email);

    let mut management_client =
        management_proto::management_service_client::ManagementServiceClient::connect(
            server.management_base_url.clone(),
        )
        .await
        .expect("connect management gRPC client");
    let fetched_user = management_client
        .get_user(management_request(management_proto::GetUserRequest {
            user_id: created_user_id.clone(),
            username: String::new(),
        }))
        .await
        .expect("management get_user should succeed for CLI-created user")
        .into_inner();
    assert_eq!(fetched_user.id, created_user_id);
    assert_eq!(fetched_user.username, username);
    assert_eq!(fetched_user.email, email);

    let mut user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect public user gRPC client");
    let mut create_room = tonic::Request::new(CreateRoomRequest {
        name: room_name.clone(),
        settings: None,
        description: "cli room get e2e".to_string(),
        password: String::new(),
        category_id: String::new(),
        label_ids: Vec::new(),
    });
    create_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&admin_login.access_token));
    let room_id = user_client
        .create_room(create_room)
        .await
        .expect("admin should create room for room get CLI test")
        .into_inner()
        .id;

    let room_get = run_synctv_remote_cli(&server, &["room", "get", &room_id]).await;
    assert!(
        room_get.status.success(),
        "room get via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&room_get.stdout),
        String::from_utf8_lossy(&room_get.stderr),
    );
    let room_get_body: Value =
        serde_json::from_slice(&room_get.stdout).expect("CLI room get output should be JSON");
    assert_eq!(room_get_body["id"], room_id);
    assert_eq!(room_get_body["name"], room_name);
    assert_eq!(room_get_body["description"], "cli room get e2e");
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_room_ban_and_unban_commands_manage_room_lifecycle() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::CreateRoomRequest;

    let server = start_test_server().await;
    let suffix = unique_test_suffix();
    let room_name = format!("CLI Ban Room {suffix}");

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect public auth gRPC client");
    let admin_login =
        opaque_grpc_login(&mut auth_client, BOOTSTRAP_ROOT_USERNAME, "StrongPwd12345!").await;

    let mut user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect public user gRPC client");
    let mut create_room = tonic::Request::new(CreateRoomRequest {
        name: room_name,
        settings: None,
        description: "cli room ban lifecycle e2e".to_string(),
        password: String::new(),
        category_id: String::new(),
        label_ids: Vec::new(),
    });
    create_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&admin_login.access_token));
    let room_id = user_client
        .create_room(create_room)
        .await
        .expect("admin should create room for room ban CLI test")
        .into_inner()
        .id;

    let room_ban = run_synctv_remote_cli(
        &server,
        &["room", "ban", &room_id, "--reason", "CLI moderation test"],
    )
    .await;
    assert!(
        room_ban.status.success(),
        "room ban via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&room_ban.stdout),
        String::from_utf8_lossy(&room_ban.stderr),
    );
    let room_ban_body: Value =
        serde_json::from_slice(&room_ban.stdout).expect("CLI room ban output should be JSON");
    assert_eq!(room_ban_body["id"], room_id);
    assert_eq!(room_ban_body["isBanned"], true);
    assert_eq!(room_ban_body["creatorUsername"], BOOTSTRAP_ROOT_USERNAME);

    let room_unban = run_synctv_remote_cli(&server, &["room", "unban", &room_id]).await;
    assert!(
        room_unban.status.success(),
        "room unban via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&room_unban.stdout),
        String::from_utf8_lossy(&room_unban.stderr),
    );
    let room_unban_body: Value =
        serde_json::from_slice(&room_unban.stdout).expect("CLI room unban output should be JSON");
    assert_eq!(room_unban_body["id"], room_id);
    assert!(!room_unban_body["isBanned"].as_bool().unwrap_or(false));
    assert_eq!(room_unban_body["creatorUsername"], BOOTSTRAP_ROOT_USERNAME);

    let mut management_client =
        management_proto::management_service_client::ManagementServiceClient::connect(
            server.management_base_url.clone(),
        )
        .await
        .expect("connect management gRPC client");
    let fetched_room = management_client
        .get_room(management_request(management_proto::GetRoomRequest {
            room_id: room_id.clone(),
        }))
        .await
        .expect("management get_room should succeed after CLI unban")
        .into_inner();
    assert_eq!(fetched_room.id, room_id);
    assert!(!fetched_room.is_banned);
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_room_settings_commands_manage_room_settings_lifecycle() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::CreateRoomRequest;

    let server = start_test_server().await;
    let suffix = unique_test_suffix();

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect public auth gRPC client");
    let admin_login =
        opaque_grpc_login(&mut auth_client, BOOTSTRAP_ROOT_USERNAME, "StrongPwd12345!").await;

    let mut user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect public user gRPC client");
    let mut create_room = tonic::Request::new(CreateRoomRequest {
        name: format!("CLI Settings Room {suffix}"),
        settings: None,
        description: "cli room settings lifecycle e2e".to_string(),
        password: String::new(),
        category_id: String::new(),
        label_ids: Vec::new(),
    });
    create_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&admin_login.access_token));
    let room_id = user_client
        .create_room(create_room)
        .await
        .expect("admin should create room for room settings CLI test")
        .into_inner()
        .id;

    let settings_get = run_synctv_remote_cli(&server, &["room", "settings", "get", &room_id]).await;
    assert!(
        settings_get.status.success(),
        "room settings get via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&settings_get.stdout),
        String::from_utf8_lossy(&settings_get.stderr),
    );
    let settings_get_body: Value = serde_json::from_slice(&settings_get.stdout)
        .expect("CLI room settings get output should be JSON");
    let initial_settings_version = json_i64(&settings_get_body["version"])
        .expect("CLI room settings get output should contain version");
    assert!(
        initial_settings_version > 0,
        "CLI room settings get should return a persisted initial version"
    );
    let settings_update = run_synctv_remote_cli(
        &server,
        &[
            "room",
            "settings",
            "update",
            &room_id,
            "--set",
            "chatEnabled=false",
            "--set",
            "allowGuestJoin=true",
        ],
    )
    .await;
    assert!(
        settings_update.status.success(),
        "room settings update via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&settings_update.stdout),
        String::from_utf8_lossy(&settings_update.stderr),
    );

    let mut management_client =
        management_proto::management_service_client::ManagementServiceClient::connect(
            server.management_base_url.clone(),
        )
        .await
        .expect("connect management gRPC client");
    let updated_settings_response = management_client
        .get_room_settings(management_request(
            management_proto::GetRoomSettingsRequest {
                room_id: room_id.clone(),
            },
        ))
        .await
        .expect("management get_room_settings should succeed after CLI update")
        .into_inner();
    let updated_settings = updated_settings_response
        .settings
        .as_ref()
        .expect("updated room settings should be present");
    assert!(!updated_settings.chat_enabled);
    assert!(updated_settings.allow_guest_join);
    assert!(
        updated_settings_response.version > initial_settings_version,
        "updated settings version should increase"
    );

    let settings_reset =
        run_synctv_remote_cli(&server, &["room", "settings", "reset", &room_id]).await;
    assert!(
        settings_reset.status.success(),
        "room settings reset via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&settings_reset.stdout),
        String::from_utf8_lossy(&settings_reset.stderr),
    );

    let reset_settings_response = management_client
        .get_room_settings(management_request(
            management_proto::GetRoomSettingsRequest {
                room_id: room_id.clone(),
            },
        ))
        .await
        .expect("management get_room_settings should succeed after CLI reset")
        .into_inner();
    let reset_settings = reset_settings_response
        .settings
        .as_ref()
        .expect("reset room settings should be present");
    assert!(reset_settings.chat_enabled);
    assert!(!reset_settings.allow_guest_join);
    assert!(
        reset_settings_response.version > updated_settings_response.version,
        "reset settings version should increase"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_user_admin_commands_manage_global_admin_lifecycle() {
    let server = start_test_server().await;
    let suffix = unique_test_suffix();
    let username = format!("cli_admin_{suffix}");
    let email = format!("cli-admin-{suffix}@example.com");

    let create_user = run_synctv_remote_cli(
        &server,
        &[
            "user",
            "create",
            &username,
            "--email",
            &email,
            "--password",
            "CliAdminPass12345!",
        ],
    )
    .await;
    assert!(
        create_user.status.success(),
        "user create via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&create_user.stdout),
        String::from_utf8_lossy(&create_user.stderr),
    );
    let create_user_body: Value =
        serde_json::from_slice(&create_user.stdout).expect("CLI user create output should be JSON");
    let created_user_id = create_user_body["id"]
        .as_str()
        .expect("CLI user create output should contain id")
        .to_string();

    let add_admin = run_synctv_remote_cli(
        &server,
        &["user", "admin", "grant", "--user-id", &created_user_id],
    )
    .await;
    assert!(
        add_admin.status.success(),
        "user admin add via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&add_admin.stdout),
        String::from_utf8_lossy(&add_admin.stderr),
    );
    let add_admin_body: Value = serde_json::from_slice(&add_admin.stdout)
        .expect("CLI user admin add output should be JSON");
    assert_eq!(add_admin_body["id"], created_user_id);
    assert_eq!(
        add_admin_body["role"].as_i64(),
        Some(synctv_proto::common::UserRole::Admin as i64)
    );

    let list_admins = run_synctv_remote_cli(&server, &["user", "admin", "list"]).await;
    assert!(
        list_admins.status.success(),
        "user admin list via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&list_admins.stdout),
        String::from_utf8_lossy(&list_admins.stderr),
    );
    let list_admins_body: Value = serde_json::from_slice(&list_admins.stdout)
        .expect("CLI user admin list output should be JSON");
    assert!(
        list_admins_body["admins"]
            .as_array()
            .expect("admins should be an array")
            .iter()
            .any(|admin| admin["id"] == created_user_id),
        "CLI user admin list output should include the promoted admin: {list_admins_body}"
    );

    let remove_admin = run_synctv_remote_cli(
        &server,
        &["user", "admin", "revoke", "--user-id", &created_user_id],
    )
    .await;
    assert!(
        remove_admin.status.success(),
        "user admin remove via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&remove_admin.stdout),
        String::from_utf8_lossy(&remove_admin.stderr),
    );
    let remove_admin_body: Value = serde_json::from_slice(&remove_admin.stdout)
        .expect("CLI user admin remove output should be JSON");
    assert_eq!(remove_admin_body["success"], true);

    let mut management_client =
        management_proto::management_service_client::ManagementServiceClient::connect(
            server.management_base_url.clone(),
        )
        .await
        .expect("connect management gRPC client");
    let fetched_user = management_client
        .get_user(management_request(management_proto::GetUserRequest {
            user_id: created_user_id.clone(),
            username: String::new(),
        }))
        .await
        .expect("management get_user should succeed after remove-admin")
        .into_inner();
    assert_eq!(fetched_user.id, created_user_id);
    assert_eq!(
        fetched_user.role,
        synctv_proto::common::UserRole::User as i32
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_user_unban_succeeds_even_when_target_is_the_only_bootstrap_root() {
    let server = start_test_server().await;

    let ban_root = run_synctv_remote_cli(
        &server,
        &["user", "ban", BOOTSTRAP_ROOT_USERNAME, "--reason", "e2e"],
    )
    .await;
    assert!(
        ban_root.status.success(),
        "user ban via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&ban_root.stdout),
        String::from_utf8_lossy(&ban_root.stderr),
    );
    let ban_root_body: Value =
        serde_json::from_slice(&ban_root.stdout).expect("CLI user ban output should be JSON");
    assert_eq!(
        ban_root_body["status"].as_i64(),
        Some(synctv_proto::common::UserStatus::Banned as i64)
    );

    let unban_root =
        run_synctv_remote_cli(&server, &["user", "unban", BOOTSTRAP_ROOT_USERNAME]).await;
    assert!(
        unban_root.status.success(),
        "user unban via CLI should succeed even when the target is the only bootstrap root\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&unban_root.stdout),
        String::from_utf8_lossy(&unban_root.stderr),
    );
    let unban_root_body: Value =
        serde_json::from_slice(&unban_root.stdout).expect("CLI user unban output should be JSON");
    assert_eq!(
        unban_root_body["status"].as_i64(),
        Some(synctv_proto::common::UserStatus::Active as i64)
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_user_batch_and_settings_commands_cover_remaining_management_paths() {
    let server = start_test_server().await;
    let suffix = unique_test_suffix();

    let batch_user_a = create_cli_user(
        &server,
        &format!("cli_batch_user_a_{suffix}"),
        &format!("cli-batch-a-{suffix}@example.com"),
        "CliBatchPass12345!",
        Some("active"),
        "create batch user a",
    )
    .await;
    let batch_user_b = create_cli_user(
        &server,
        &format!("cli_batch_user_b_{suffix}"),
        &format!("cli-batch-b-{suffix}@example.com"),
        "CliBatchPass12345!",
        Some("active"),
        "create batch user b",
    )
    .await;
    let batch_user_c = create_cli_user(
        &server,
        &format!("cli_batch_user_c_{suffix}"),
        &format!("cli-batch-c-{suffix}@example.com"),
        "CliBatchPass12345!",
        Some("active"),
        "create batch user c",
    )
    .await;

    let batch_user_a_id = batch_user_a["id"]
        .as_str()
        .expect("batch user a should include id")
        .to_string();
    let batch_user_b_id = batch_user_b["id"]
        .as_str()
        .expect("batch user b should include id")
        .to_string();
    let batch_user_c_id = batch_user_c["id"]
        .as_str()
        .expect("batch user c should include id")
        .to_string();

    let batch_ban = run_synctv_remote_cli_json(
        &server,
        &[
            "user",
            "batch",
            "ban",
            "--user-id",
            &batch_user_a_id,
            "--user-id",
            &batch_user_b_id,
            "--reason",
            "cli-batch-ban",
        ],
        "batch ban users",
    )
    .await;
    assert!(
        batch_ban["results"].is_array(),
        "batch ban should return per-item results: {batch_ban}"
    );

    let banned_users = run_synctv_remote_cli_json(
        &server,
        &[
            "user",
            "list",
            "--status",
            "banned",
            "--search",
            "cli_batch_user_",
        ],
        "list batch-banned users",
    )
    .await;
    assert!(
        banned_users["users"]
            .as_array()
            .expect("user list should return users array")
            .iter()
            .any(|user| user["id"] == batch_user_a_id)
            && banned_users["users"]
                .as_array()
                .expect("user list should return users array")
                .iter()
                .any(|user| user["id"] == batch_user_b_id),
        "batch-banned users should appear in banned list: {banned_users}"
    );

    let batch_delete = run_synctv_remote_cli_json(
        &server,
        &[
            "user",
            "batch",
            "delete",
            "--user-id",
            &batch_user_b_id,
            "--user-id",
            &batch_user_c_id,
        ],
        "batch delete users",
    )
    .await;
    assert!(
        batch_delete["results"].is_array(),
        "batch delete should return per-item results: {batch_delete}"
    );

    for deleted_user_id in [&batch_user_b_id, &batch_user_c_id] {
        let error = run_synctv_remote_cli_failure(
            &server,
            &["user", "get", "--user-id", deleted_user_id],
            "get deleted batch user",
        )
        .await;
        assert!(
            error.contains("not found")
                || error.contains("Not found")
                || error.contains("NotFound"),
            "deleted batch user lookup should return not found, got: {error}"
        );
    }

    let room_settings_group = run_synctv_remote_cli_json(
        &server,
        &["settings", "get", "roomCreation"],
        "get room creation settings group",
    )
    .await;
    assert!(
        !room_settings_group["approvalRequired"]
            .as_bool()
            .unwrap_or(false),
        "settings get roomCreation should default approvalRequired to false: {room_settings_group}"
    );

    let test_email_result = run_synctv_remote_cli_json(
        &server,
        &["settings", "test-email", "ops@example.com"],
        "test email without SMTP configuration",
    )
    .await;
    assert!(
        !test_email_result["success"].as_bool().unwrap_or(false),
        "settings test-email should report failure when SMTP is not configured: {test_email_result}"
    );
    assert!(
        test_email_result["message"]
            .as_str()
            .is_some_and(|message| {
                message.contains("Failed to send test email")
                    || message.contains("verify the email configuration")
            }),
        "settings test-email should surface a sanitized configuration error: {test_email_result}"
    );

    let room = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "create",
            &format!("CLI Settings Reset Room {suffix}"),
            "--username",
            BOOTSTRAP_ROOT_USERNAME,
        ],
        "create room for room settings reset",
    )
    .await;
    let room_id = room["id"]
        .as_str()
        .expect("room create should include id")
        .to_string();

    let updated_room_settings = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "settings",
            "update",
            &room_id,
            "--set",
            "chatEnabled=false",
            "--set",
            "allowGuestJoin=true",
        ],
        "update room settings for reset coverage",
    )
    .await;
    assert!(
        updated_room_settings["id"].is_string(),
        "room settings update should return room payload: {updated_room_settings}"
    );

    let fetched_updated_room_settings = run_synctv_remote_cli_json(
        &server,
        &["room", "settings", "get", &room_id],
        "get room settings after full update",
    )
    .await;
    assert!(!fetched_updated_room_settings["settings"]["chatEnabled"]
        .as_bool()
        .unwrap_or(false));
    assert_eq!(
        fetched_updated_room_settings["settings"]["allowGuestJoin"],
        true
    );

    let reset_room_settings = run_synctv_remote_cli_json(
        &server,
        &["room", "settings", "reset", &room_id],
        "reset room settings",
    )
    .await;
    assert!(
        reset_room_settings["id"].is_string(),
        "room settings reset should return room payload: {reset_room_settings}"
    );

    let fetched_room_settings = run_synctv_remote_cli_json(
        &server,
        &["room", "settings", "get", &room_id],
        "get room settings after reset",
    )
    .await;
    assert_eq!(fetched_room_settings["settings"]["chatEnabled"], true);
    assert!(!fetched_room_settings["settings"]["allowGuestJoin"]
        .as_bool()
        .unwrap_or(false));
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_user_lifecycle_commands_cover_identity_state_role_and_batch_flows() {
    let server = start_test_server().await;
    let suffix = unique_test_suffix();

    let lifecycle_username = format!("cli_lifecycle_user_{suffix}");
    let lifecycle_email = format!("cli-lifecycle-{suffix}@example.com");
    let lifecycle_password = "CliLifecyclePass12345!";
    let rotated_password = "CliPendingPass67890!";
    let renamed_username = format!("cli_renamed_user_{suffix}");
    let room_name = format!("CLI User Lifecycle Room {suffix}");

    let created_lifecycle_user = create_cli_user(
        &server,
        &lifecycle_username,
        &lifecycle_email,
        lifecycle_password,
        Some("active"),
        "user create active",
    )
    .await;
    let lifecycle_user_id = created_lifecycle_user["id"]
        .as_str()
        .expect("user create should include id")
        .to_string();
    assert_eq!(created_lifecycle_user["username"], lifecycle_username);
    assert_eq!(
        created_lifecycle_user["status"].as_i64(),
        Some(synctv_proto::common::UserStatus::Active as i64)
    );

    let active_list = run_synctv_remote_cli_json(
        &server,
        &[
            "user",
            "list",
            "--status",
            "active",
            "--search",
            &lifecycle_username,
        ],
        "user list active",
    )
    .await;
    assert!(
        active_list["users"]
            .as_array()
            .expect("user list should return users array")
            .iter()
            .any(|user| user["id"] == lifecycle_user_id),
        "active user should appear in filtered list: {active_list}"
    );

    let fetched_user = run_synctv_remote_cli_json(
        &server,
        &["user", "get", &lifecycle_username],
        "user get by username",
    )
    .await;
    assert_eq!(fetched_user["id"], lifecycle_user_id);
    assert_eq!(fetched_user["email"], lifecycle_email);

    let active_login_token =
        login_http_ok_token(&server, &lifecycle_username, lifecycle_password).await;
    assert!(
        !active_login_token.is_empty(),
        "approved user should receive an access token"
    );

    let password_rotated = run_synctv_remote_cli_json(
        &server,
        &[
            "user",
            "set-password",
            "--user-id",
            &lifecycle_user_id,
            "--password",
            rotated_password,
            "--reason",
            "cli-user-lifecycle",
        ],
        "user set-password by id",
    )
    .await;
    assert_eq!(password_rotated["success"], true);

    assert!(
        opaque_http_login_token(&server, &lifecycle_username, lifecycle_password)
            .await
            .is_err(),
        "old password should stop working after CLI password rotation"
    );

    let renamed_user = run_synctv_remote_cli_json(
        &server,
        &[
            "user",
            "set-username",
            "--user-id",
            &lifecycle_user_id,
            "--username",
            &renamed_username,
        ],
        "user set-username by id",
    )
    .await;
    assert_eq!(renamed_user["user"]["id"], lifecycle_user_id);
    assert_eq!(renamed_user["user"]["username"], renamed_username);

    assert!(
        opaque_http_login_token(&server, &lifecycle_username, rotated_password)
            .await
            .is_err(),
        "old username should stop working after CLI username rotation"
    );
    let renamed_login_token =
        login_http_ok_token(&server, &renamed_username, rotated_password).await;
    assert!(
        !renamed_login_token.is_empty(),
        "renamed user should still be able to log in"
    );

    let promoted_user = run_synctv_remote_cli_json(
        &server,
        &["user", "set-role", &renamed_username, "--role", "admin"],
        "user set-role by username",
    )
    .await;
    assert_eq!(
        promoted_user["user"]["role"].as_i64(),
        Some(synctv_proto::common::UserRole::Admin as i64)
    );

    let created_room = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "create",
            &room_name,
            "--username",
            &renamed_username,
            "--description",
            "room created during CLI user lifecycle coverage",
        ],
        "room create for user rooms",
    )
    .await;
    let room_id = created_room["id"]
        .as_str()
        .expect("room create should return id")
        .to_string();

    let related_rooms = run_synctv_remote_cli_json(
        &server,
        &["user", "rooms", "--user-id", &lifecycle_user_id],
        "user rooms by id",
    )
    .await;
    assert!(
        related_rooms["rooms"]
            .as_array()
            .expect("user rooms should return rooms array")
            .iter()
            .any(|room| room["id"] == room_id && room["creatorUsername"] == renamed_username),
        "user rooms should include the created room: {related_rooms}"
    );

    let banned_user = run_synctv_remote_cli_json(
        &server,
        &["user", "ban", &renamed_username, "--reason", "cli-user-ban"],
        "user ban",
    )
    .await;
    assert_eq!(
        banned_user["status"].as_i64(),
        Some(synctv_proto::common::UserStatus::Banned as i64)
    );

    assert!(
        opaque_http_login_token(&server, &renamed_username, rotated_password)
            .await
            .is_err(),
        "banned users must not be able to authenticate"
    );

    let unbanned_user =
        run_synctv_remote_cli_json(&server, &["user", "unban", &renamed_username], "user unban")
            .await;
    assert_eq!(
        unbanned_user["status"].as_i64(),
        Some(synctv_proto::common::UserStatus::Active as i64)
    );
    let restored_login_token =
        login_http_ok_token(&server, &renamed_username, rotated_password).await;
    assert!(
        !restored_login_token.is_empty(),
        "unbanned user should be able to authenticate again"
    );

    let batch_ban_one = format!("cli_batch_ban_one_{suffix}");
    let batch_ban_two = format!("cli_batch_ban_two_{suffix}");
    let delete_one = format!("cli_delete_one_{suffix}");
    let delete_two = format!("cli_delete_two_{suffix}");
    let batch_ban_one_body = create_cli_user(
        &server,
        &batch_ban_one,
        &format!("cli-batch-ban-one-{suffix}@example.com"),
        "CliExtraPass12345!",
        None,
        "user create extra target",
    )
    .await;
    let batch_ban_two_body = create_cli_user(
        &server,
        &batch_ban_two,
        &format!("cli-batch-ban-two-{suffix}@example.com"),
        "CliExtraPass12345!",
        None,
        "user create extra target",
    )
    .await;
    let delete_one_body = create_cli_user(
        &server,
        &delete_one,
        &format!("cli-delete-one-{suffix}@example.com"),
        "CliExtraPass12345!",
        None,
        "user create extra target",
    )
    .await;
    let delete_two_body = create_cli_user(
        &server,
        &delete_two,
        &format!("cli-delete-two-{suffix}@example.com"),
        "CliExtraPass12345!",
        None,
        "user create extra target",
    )
    .await;

    let delete_one_id = delete_one_body["id"]
        .as_str()
        .expect("delete target one should have id")
        .to_string();
    let delete_two_id = delete_two_body["id"]
        .as_str()
        .expect("delete target two should have id")
        .to_string();

    let batch_ban_result = run_synctv_remote_cli_json(
        &server,
        &[
            "user",
            "batch",
            "ban",
            &batch_ban_one,
            &batch_ban_two,
            "--reason",
            "cli-batch-ban",
        ],
        "user batch ban",
    )
    .await;
    assert!(
        batch_ban_result["results"].is_array(),
        "batch ban should return per-item results: {batch_ban_result}"
    );

    let banned_users = run_synctv_remote_cli_json(
        &server,
        &[
            "user",
            "list",
            "--status",
            "banned",
            "--search",
            "cli_batch_ban_",
        ],
        "user list banned after batch ban",
    )
    .await;
    assert!(
        banned_users["users"]
            .as_array()
            .expect("banned user list should return users array")
            .iter()
            .any(|user| user["username"] == batch_ban_one)
            && banned_users["users"]
                .as_array()
                .expect("banned user list should return users array")
                .iter()
                .any(|user| user["username"] == batch_ban_two),
        "batch-banned users should appear in banned list: {banned_users}"
    );

    let deleted_single = run_synctv_remote_cli_json(
        &server,
        &["user", "delete", "--user-id", &delete_one_id],
        "user delete by id",
    )
    .await;
    assert_eq!(deleted_single["success"], true);

    let deleted_batch = run_synctv_remote_cli_json(
        &server,
        &[
            "user",
            "batch",
            "delete",
            "--user-id",
            &delete_two_id,
            "--user-id",
            batch_ban_two_body["id"]
                .as_str()
                .expect("batch ban two should have id"),
        ],
        "user batch delete by id",
    )
    .await;
    assert!(
        deleted_batch["results"].is_array(),
        "batch delete should return per-item results: {deleted_batch}"
    );

    let mut management_client =
        management_proto::management_service_client::ManagementServiceClient::connect(
            server.management_base_url.clone(),
        )
        .await
        .expect("connect management gRPC client");

    for deleted_user_id in [&delete_one_id, &delete_two_id] {
        let deleted_lookup = management_client
            .get_user(management_request(management_proto::GetUserRequest {
                user_id: (*deleted_user_id).clone(),
                username: String::new(),
            }))
            .await
            .expect_err("deleted user should not be retrievable via management");
        assert_eq!(deleted_lookup.code(), tonic::Code::NotFound);
    }

    let still_banned = management_client
        .get_user(management_request(management_proto::GetUserRequest {
            user_id: batch_ban_one_body["id"]
                .as_str()
                .expect("batch ban one should have id")
                .to_string(),
            username: String::new(),
        }))
        .await
        .expect("management get_user should still work for batch-banned survivor")
        .into_inner();
    assert_eq!(
        still_banned.status,
        synctv_proto::common::UserStatus::Banned as i32
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_media_resource_and_member_commands_cover_status_permissions_playback_and_batches(
) {
    let server = start_test_server().await;
    let suffix = unique_test_suffix();

    let owner_username = format!("cli_room_owner_{suffix}");
    let member_username = format!("cli_room_member_{suffix}");
    let subject_username = format!("cli_room_subject_{suffix}");
    let owner_password = "CliOwnerPass12345!";
    let member_password = "CliMemberPass12345!";
    let subject_password = "CliSubjectPass12345!";
    let room_password = "CliRoomPass12345!";
    let pending_room_name = format!("CLI Pending Room {suffix}");

    create_cli_user(
        &server,
        &owner_username,
        &format!("cli-room-owner-{suffix}@example.com"),
        owner_password,
        Some("active"),
        "create room owner",
    )
    .await;
    let member_user = create_cli_user(
        &server,
        &member_username,
        &format!("cli-room-member-{suffix}@example.com"),
        member_password,
        Some("active"),
        "create room member",
    )
    .await;
    let subject_user = create_cli_user(
        &server,
        &subject_username,
        &format!("cli-room-subject-{suffix}@example.com"),
        subject_password,
        Some("active"),
        "create room subject",
    )
    .await;

    let member_user_id = member_user["id"]
        .as_str()
        .expect("member user should have id")
        .to_string();
    let _subject_user_id = subject_user["id"]
        .as_str()
        .expect("subject user should have id")
        .to_string();

    let member_token = login_http_ok_token(&server, &member_username, member_password).await;
    let subject_token = login_http_ok_token(&server, &subject_username, subject_password).await;

    let updated_global_room_settings = run_synctv_remote_cli_json(
        &server,
        &[
            "settings",
            "update",
            "--request-json",
            r#"{"settings":{"roomCreation":{"approvalRequired":true}},"updateMask":"roomCreation.approvalRequired"}"#,
        ],
        "enable room creation review",
    )
    .await;
    assert_eq!(
        updated_global_room_settings["roomCreation"]["approvalRequired"],
        true
    );

    let review_room = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "create",
            &pending_room_name,
            "--username",
            &owner_username,
            "--description",
            "room created during CLI room lifecycle coverage",
        ],
        "create room requiring review",
    )
    .await;
    let room_id = review_room["id"]
        .as_str()
        .expect("review room response should include id")
        .to_string();
    assert_eq!(
        review_room["status"].as_i64(),
        Some(synctv_proto::common::RoomStatus::Active as i64)
    );

    let approved_room = run_synctv_remote_cli_json(
        &server,
        &["review", "room-creation", "approve", &room_id],
        "approve room creation request",
    )
    .await;
    assert_eq!(
        approved_room["room"]["status"].as_i64(),
        Some(synctv_proto::common::RoomStatus::Active as i64)
    );
    assert_eq!(approved_room["room"]["creatorUsername"], owner_username);

    let reset_global_room_review = run_synctv_remote_cli_json(
        &server,
        &[
            "settings",
            "update",
            "--request-json",
            r#"{"settings":{"roomCreation":{"approvalRequired":false}},"updateMask":"roomCreation.approvalRequired"}"#,
        ],
        "disable room creation review",
    )
    .await;
    assert!(
        !reset_global_room_review["roomCreation"]["approvalRequired"]
            .as_bool()
            .unwrap_or(false)
    );

    let room_password_set = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "set-password",
            &room_id,
            "--password",
            room_password,
        ],
        "set room password",
    )
    .await;
    assert_eq!(room_password_set["success"], true);
    let member_join = join_room_http(&server, &room_id, room_password, &member_token).await;
    assert_eq!(
        member_join.status(),
        StatusCode::OK,
        "member should be able to join password-protected room"
    );
    let subject_join = join_room_http(&server, &room_id, room_password, &subject_token).await;
    assert_eq!(
        subject_join.status(),
        StatusCode::OK,
        "subject should be able to join password-protected room"
    );

    let initial_members = run_synctv_remote_cli_json(
        &server,
        &["room", "member", "list", &room_id],
        "list room members",
    )
    .await;
    assert_eq!(initial_members["total"].as_i64(), Some(3));
    let room_after_joins =
        run_synctv_remote_cli_json(&server, &["room", "get", &room_id], "get room after joins")
            .await;
    assert_eq!(room_after_joins["memberCount"].as_i64(), Some(3));
    let joined_subject_user_id = initial_members["members"]
        .as_array()
        .expect("room member list should return members array")
        .iter()
        .find(|member| member["username"] == subject_username)
        .and_then(|member| member["userId"].as_str())
        .expect("subject should appear in room member list")
        .to_string();

    let member_permissions = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "member",
            "set-permissions",
            "--room-id",
            &room_id,
            &member_username,
            "--role",
            "admin",
            "--admin-added-permissions",
            &(synctv_core::models::RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE
                | synctv_core::models::RoomAdminPermissionBits::LIVE_CONTROL)
                .to_string(),
            "--admin-removed-permissions",
            &synctv_core::models::RoomAdminPermissionBits::CHAT.to_string(),
        ],
        "set member permissions",
    )
    .await;
    assert_eq!(
        member_permissions["role"].as_i64(),
        Some(synctv_proto::common::RoomMemberRole::Admin as i64)
    );

    let admin_members = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "member",
            "list",
            &room_id,
            "--role",
            "admin",
            "--search",
            &member_username,
        ],
        "list admin room members",
    )
    .await;
    assert!(
        admin_members["members"]
            .as_array()
            .expect("admin member list should contain members array")
            .iter()
            .any(|member| member["userId"] == member_user_id),
        "promoted admin should appear in filtered member list: {admin_members}"
    );

    let kicked_subject = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "member",
            "kick",
            "--room-id",
            &room_id,
            "--user-id",
            &joined_subject_user_id,
            "--kick-cooldown-seconds",
            "3",
        ],
        "kick room member",
    )
    .await;
    assert_eq!(kicked_subject["success"], true);

    let kicked_subject_rejoin =
        join_room_http(&server, &room_id, room_password, &subject_token).await;
    assert_eq!(
        kicked_subject_rejoin.status(),
        StatusCode::FORBIDDEN,
        "kicked user must not be able to rejoin during kick cooldown"
    );
    let kicked_subject_rejoin_body = response_json(kicked_subject_rejoin).await;
    assert!(
        kicked_subject_rejoin_body
            .to_string()
            .contains("recently kicked"),
        "kick cooldown rejection should be explicit: {kicked_subject_rejoin_body}"
    );

    let kicked_subject_ticket = post_json(
        &test_http_client(),
        &format!("{}/api/tickets", server.api_base_url),
        create_ticket_request(&room_id),
        Some(&subject_token),
    )
    .await;
    assert_eq!(
        kicked_subject_ticket.status(),
        StatusCode::FORBIDDEN,
        "kicked user must no longer be able to request room tickets"
    );

    let transferred_room = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "transfer-owner",
            &room_id,
            "--username",
            &owner_username,
            "--new-owner-id",
            &member_user_id,
        ],
        "transfer room ownership",
    )
    .await;
    assert_eq!(transferred_room["createdBy"], member_user_id);

    let room_password_cleared = run_synctv_remote_cli_json(
        &server,
        &["room", "set-password", &room_id, "--clear"],
        "clear room password",
    )
    .await;
    assert_eq!(room_password_cleared["success"], true);
    tokio::time::sleep(Duration::from_secs(4)).await;
    let rejoined_without_password = join_room_http(&server, &room_id, "", &subject_token).await;
    assert_eq!(
        rejoined_without_password.status(),
        StatusCode::OK,
        "subject should be able to rejoin after password clear"
    );

    let room_after_transfer = run_synctv_remote_cli_json(
        &server,
        &["room", "get", &room_id],
        "get room after transfer",
    )
    .await;
    assert_eq!(room_after_transfer["creatorUsername"], member_username);

    let playlist_alpha = run_synctv_remote_cli_json(
        &server,
        &[
            "playlist",
            "create",
            "--room-id",
            &room_id,
            "--username",
            &member_username,
            "Alpha Playlist",
        ],
        "create first playlist",
    )
    .await;
    let playlist_alpha_id = playlist_alpha["id"]
        .as_str()
        .expect("playlist create should return id")
        .to_string();

    let playlist_beta = run_synctv_remote_cli_json(
        &server,
        &[
            "playlist",
            "create",
            "--room-id",
            &room_id,
            "--username",
            &member_username,
            "Beta Playlist",
        ],
        "create second playlist",
    )
    .await;
    let playlist_beta_id = playlist_beta["id"]
        .as_str()
        .expect("playlist create should return id")
        .to_string();

    let renamed_playlist = run_synctv_remote_cli_json(
        &server,
        &[
            "playlist",
            "update",
            "--room-id",
            &room_id,
            &playlist_alpha_id,
            "--name",
            "Alpha Playlist Renamed",
        ],
        "rename playlist",
    )
    .await;
    assert_eq!(renamed_playlist["name"], "Alpha Playlist Renamed");

    let moved_playlist = run_synctv_remote_cli_json(
        &server,
        &[
            "playlist",
            "move",
            "--room-id",
            &room_id,
            &playlist_beta_id,
            "--before-playlist-id",
            &playlist_alpha_id,
        ],
        "move playlist",
    )
    .await;
    assert_eq!(moved_playlist["id"], playlist_beta_id);

    let listed_playlists = run_synctv_remote_cli_json(
        &server,
        &["playlist", "list", "--room-id", &room_id],
        "list playlists",
    )
    .await;
    let listed_playlists_array = listed_playlists["playlists"]
        .as_array()
        .expect("playlist list should return playlists array");
    assert_eq!(listed_playlists_array.len(), 2);
    assert_eq!(listed_playlists_array[0]["id"], playlist_beta_id);
    assert_eq!(listed_playlists_array[1]["id"], playlist_alpha_id);

    let fetched_playlist = run_synctv_remote_cli_json(
        &server,
        &["playlist", "get", "--room-id", &room_id, &playlist_alpha_id],
        "get playlist",
    )
    .await;
    assert_eq!(
        fetched_playlist["playlist"]["name"],
        "Alpha Playlist Renamed"
    );

    let first_media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/cli-room-one.mp4",
            "--room-id",
            &room_id,
            "--username",
            &member_username,
            "--playlist-id",
            &playlist_alpha_id,
            "--name",
            "CLI Room Media One",
        ],
        "add first media",
    )
    .await;
    let media_one_id = first_media["id"]
        .as_str()
        .expect("media add should return id")
        .to_string();

    let second_media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/cli-room-two.mp4",
            "--room-id",
            &room_id,
            "--username",
            &member_username,
            "--name",
            "CLI Room Media Two",
        ],
        "add second media",
    )
    .await;
    let media_two_id = second_media["id"]
        .as_str()
        .expect("second media add should return id")
        .to_string();

    let root_media = run_synctv_remote_cli_json(
        &server,
        &["media", "list", "--room-id", &room_id],
        "list root media",
    )
    .await;
    assert!(
        root_media["media"]
            .as_array()
            .expect("root media list should contain media array")
            .iter()
            .any(|media| media["id"] == media_two_id),
        "root media list should contain the room-root media: {root_media}"
    );

    let renamed_media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "update",
            "--room-id",
            &room_id,
            &media_two_id,
            "--name",
            "CLI Room Media Two Renamed",
        ],
        "rename media",
    )
    .await;
    assert_eq!(renamed_media["name"], "CLI Room Media Two Renamed");

    let moved_media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "move",
            "--room-id",
            &room_id,
            "--media-id",
            &media_two_id,
            "--to-playlist-id",
            &playlist_alpha_id,
            "--before-media-id",
            &media_one_id,
        ],
        "move media into playlist",
    )
    .await;
    assert_eq!(moved_media["movedCount"].as_i64(), Some(1));

    let playlist_media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "list",
            "--room-id",
            &room_id,
            "--playlist-id",
            &playlist_alpha_id,
        ],
        "list playlist media",
    )
    .await;
    let playlist_media_array = playlist_media["media"]
        .as_array()
        .expect("playlist media list should return media array");
    assert_eq!(playlist_media_array.len(), 2);
    assert_eq!(playlist_media_array[0]["id"], media_two_id);
    assert_eq!(playlist_media_array[1]["id"], media_one_id);

    let started_playback = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "playback",
            "start",
            "--room-id",
            &room_id,
            "--media-id",
            &media_one_id,
        ],
        "start room playback",
    )
    .await;
    assert!(
        started_playback.as_object().is_some(),
        "start playback should emit a JSON object"
    );

    let playback_state = run_synctv_remote_cli_json(
        &server,
        &["room", "playback", "get", "--room-id", &room_id],
        "get room playback",
    )
    .await;
    assert_eq!(
        playback_state["playbackState"]["playingMediaId"],
        media_one_id
    );
    assert_eq!(playback_state["playbackState"]["isPlaying"], true);
    assert_eq!(playback_state["playback"]["mediaId"], media_one_id);

    let stopped_playback = run_synctv_remote_cli_json(
        &server,
        &["room", "playback", "stop", "--room-id", &room_id],
        "stop room playback",
    )
    .await;
    assert!(
        stopped_playback.as_object().is_some(),
        "stop playback should emit a JSON object"
    );

    let playback_after_stop = run_synctv_remote_cli_json(
        &server,
        &["room", "playback", "get", "--room-id", &room_id],
        "get room playback after stop",
    )
    .await;
    assert!(!playback_after_stop["playbackState"]["isPlaying"]
        .as_bool()
        .unwrap_or(false));

    let deleted_media = run_synctv_remote_cli_json(
        &server,
        &["media", "delete", "--room-id", &room_id, &media_two_id],
        "delete media",
    )
    .await;
    assert_eq!(deleted_media["success"], true);

    let deleted_playlist = run_synctv_remote_cli_json(
        &server,
        &[
            "playlist",
            "delete",
            "--room-id",
            &room_id,
            &playlist_beta_id,
        ],
        "delete playlist",
    )
    .await;
    assert_eq!(deleted_playlist["success"], true);

    let batch_room_one = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "create",
            &format!("CLI Batch Room One {suffix}"),
            "--username",
            &member_username,
        ],
        "create batch room one",
    )
    .await;
    let batch_room_one_id = batch_room_one["id"]
        .as_str()
        .expect("batch room one should have id")
        .to_string();

    let batch_room_two = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "create",
            &format!("CLI Batch Room Two {suffix}"),
            "--username",
            &member_username,
        ],
        "create batch room two",
    )
    .await;
    let batch_room_two_id = batch_room_two["id"]
        .as_str()
        .expect("batch room two should have id")
        .to_string();

    let batch_banned_rooms = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "batch",
            "ban",
            &batch_room_one_id,
            &batch_room_two_id,
            "--reason",
            "cli-room-batch-ban",
        ],
        "batch ban rooms",
    )
    .await;
    assert!(
        batch_banned_rooms["results"].is_array(),
        "batch room ban should return per-item results: {batch_banned_rooms}"
    );

    let banned_room_list = run_synctv_remote_cli_json(
        &server,
        &["room", "list", "--is-banned", "--search", "CLI Batch Room"],
        "list banned batch rooms",
    )
    .await;
    assert!(
        banned_room_list["rooms"]
            .as_array()
            .expect("banned room list should return rooms array")
            .iter()
            .any(|room| room["id"] == batch_room_one_id)
            && banned_room_list["rooms"]
                .as_array()
                .expect("banned room list should return rooms array")
                .iter()
                .any(|room| room["id"] == batch_room_two_id),
        "batch-banned rooms should appear in banned room list: {banned_room_list}"
    );

    let batch_deleted_rooms = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "batch",
            "delete",
            &batch_room_one_id,
            &batch_room_two_id,
        ],
        "batch delete rooms",
    )
    .await;
    assert!(
        batch_deleted_rooms["results"].is_array(),
        "batch room delete should return per-item results: {batch_deleted_rooms}"
    );

    let deleted_room =
        run_synctv_remote_cli_json(&server, &["room", "delete", &room_id], "delete main room")
            .await;
    assert_eq!(deleted_room["success"], true);

    let deleted_room_get_error =
        run_synctv_remote_cli_failure(&server, &["room", "get", &room_id], "get deleted room")
            .await;
    assert!(
        deleted_room_get_error.contains("not found")
            || deleted_room_get_error.contains("Not found")
            || deleted_room_get_error.contains("NotFound"),
        "deleted room get should surface a not-found error, got: {deleted_room_get_error}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_stream_commands_cover_publish_list_get_and_kick_with_real_rtmp_session() {
    let server = start_test_server().await;
    let suffix = unique_test_suffix();

    let owner_username = format!("cli_stream_owner_{suffix}");
    let owner_email = format!("cli-stream-owner-{suffix}@example.com");
    let owner_password = "CliStreamOwnerPass12345!";

    let owner_user = create_cli_user(
        &server,
        &owner_username,
        &owner_email,
        owner_password,
        Some("active"),
        "create stream owner",
    )
    .await;
    let owner_user_id = owner_user["id"]
        .as_str()
        .expect("stream owner should have id")
        .to_string();

    let room = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "create",
            &format!("CLI Stream Room {suffix}"),
            "--username",
            &owner_username,
        ],
        "create stream room",
    )
    .await;
    let room_id = room["id"]
        .as_str()
        .expect("stream room should include id")
        .to_string();

    let media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/cli-stream-source.mp4",
            "--room-id",
            &room_id,
            "--username",
            &owner_username,
            "--name",
            "CLI Stream Source",
        ],
        "create stream media",
    )
    .await;
    let media_id = media["id"]
        .as_str()
        .expect("stream media should include id")
        .to_string();

    let publish_key = run_synctv_remote_cli_json(
        &server,
        &[
            "provider",
            "rtmp",
            "create-publish-key",
            "--room-id",
            &room_id,
            "--username",
            &owner_username,
            &media_id,
        ],
        "create rtmp publish key",
    )
    .await;
    let rtmp_url = publish_key["rtmpUrl"]
        .as_str()
        .expect("publish key response should include rtmpUrl")
        .to_string();
    let stream_key = publish_key["streamKey"]
        .as_str()
        .expect("publish key response should include streamKey")
        .to_string();
    assert!(
        publish_key["publishKey"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "publish key should include a non-empty token"
    );
    assert!(
        rtmp_url.contains(&room_id),
        "publish key response should target the room RTMP path: {publish_key}"
    );
    assert!(
        stream_key.contains(&media_id),
        "publish key response should target the media stream key: {publish_key}"
    );

    let mut publisher = spawn_idle_rtmp_publisher(&rtmp_url, &stream_key).await;

    let room_streams = wait_for_remote_cli_json(
        &server,
        &[
            "room",
            "stream",
            "list",
            "--room-id",
            &room_id,
            "--search",
            &media_id,
            "--sort-by",
            "media-id",
            "--sort-dir",
            "asc",
        ],
        "wait for active room stream",
        |response| {
            response["total"].as_i64() == Some(1)
                && response["streams"].as_array().is_some_and(|streams| {
                    streams.iter().any(|stream| stream["mediaId"] == media_id)
                })
        },
    )
    .await;
    assert_eq!(room_streams["total"].as_i64(), Some(1));

    let room_stream_info = run_synctv_remote_cli_json(
        &server,
        &[
            "provider",
            "rtmp",
            "get-stream-info",
            "--room-id",
            &room_id,
            &media_id,
        ],
        "get rtmp stream info",
    )
    .await;
    assert_eq!(room_stream_info["active"], true);
    assert_eq!(room_stream_info["publisher"]["userId"], owner_user_id);

    let system_streams = wait_for_remote_cli_json(
        &server,
        &[
            "system",
            "stream",
            "list",
            "--room-id",
            &room_id,
            "--user-id",
            &owner_user_id,
            "--search",
            &media_id,
            "--sort-by",
            "media-id",
            "--sort-dir",
            "asc",
        ],
        "wait for active system stream",
        |response| {
            response["streams"].as_array().is_some_and(|streams| {
                streams.len() == 1
                    && streams[0]["roomId"] == room_id
                    && streams[0]["mediaId"] == media_id
                    && streams[0]["userId"] == owner_user_id
            })
        },
    )
    .await;
    assert_eq!(
        system_streams["streams"]
            .as_array()
            .expect("system stream list should return streams array")
            .len(),
        1
    );

    let kicked_stream = run_synctv_remote_cli_json(
        &server,
        &[
            "system",
            "stream",
            "kick",
            "--room-id",
            &room_id,
            "--media-id",
            &media_id,
            "--reason",
            "cli-stream-kick",
        ],
        "kick active stream",
    )
    .await;
    assert!(
        kicked_stream.as_object().is_some(),
        "kick stream should emit a JSON object"
    );

    publisher.shutdown("stream test cleanup after kick").await;

    let room_streams_after_kick = wait_for_room_stream_total(&server, &room_id, 0).await;
    assert_eq!(room_streams_after_kick["total"].as_i64().unwrap_or(0), 0);

    let system_streams_after_kick = wait_for_system_stream_count(&server, &room_id, 0).await;
    assert_eq!(
        system_streams_after_kick["streams"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .len(),
        0
    );

    let room_stream_info_after_kick = run_synctv_remote_cli_json(
        &server,
        &[
            "provider",
            "rtmp",
            "get-stream-info",
            "--room-id",
            &room_id,
            &media_id,
        ],
        "get rtmp stream info after kick",
    )
    .await;
    assert!(!room_stream_info_after_kick["active"]
        .as_bool()
        .unwrap_or(false));
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_management_actor_state_constraints_reject_invalid_room_operations() {
    let server = start_test_server().await;
    let suffix = unique_test_suffix();

    let owner_username = format!("cli_actor_owner_{suffix}");
    let banned_username = format!("cli_actor_banned_{suffix}");
    let outsider_username = format!("cli_actor_outsider_{suffix}");

    create_cli_user(
        &server,
        &owner_username,
        &format!("cli-actor-owner-{suffix}@example.com"),
        "CliActorOwnerPass12345!",
        Some("active"),
        "create active owner actor",
    )
    .await;
    let outsider_user = create_cli_user(
        &server,
        &outsider_username,
        &format!("cli-actor-outsider-{suffix}@example.com"),
        "CliActorOutsiderPass12345!",
        Some("active"),
        "create outsider actor",
    )
    .await;
    let banned_user = create_cli_user(
        &server,
        &banned_username,
        &format!("cli-actor-banned-{suffix}@example.com"),
        "CliActorBannedPass12345!",
        Some("banned"),
        "create banned actor",
    )
    .await;
    let banned_create_error = run_synctv_remote_cli_failure(
        &server,
        &[
            "room",
            "create",
            &format!("CLI Banned Actor Room {suffix}"),
            "--username",
            &banned_username,
        ],
        "banned actor create room",
    )
    .await;
    assert!(
        banned_create_error.contains("is banned"),
        "banned actor create room should fail with explicit state message, got: {banned_create_error}"
    );

    let room = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "create",
            &format!("CLI Actor Constraint Room {suffix}"),
            "--username",
            &owner_username,
        ],
        "create room for actor constraint coverage",
    )
    .await;
    let room_id = room["id"]
        .as_str()
        .expect("actor constraint room should include id")
        .to_string();

    let outsider_user_id = outsider_user["id"]
        .as_str()
        .expect("outsider actor should include id")
        .to_string();
    let banned_user_id = banned_user["id"]
        .as_str()
        .expect("banned actor should include id")
        .to_string();

    let transfer_error = run_synctv_remote_cli_failure(
        &server,
        &[
            "room",
            "transfer-owner",
            &room_id,
            "--username",
            &owner_username,
            "--new-owner-id",
            &outsider_user_id,
        ],
        "transfer room ownership to non-member",
    )
    .await;
    assert!(
        transfer_error.contains("active member of this room"),
        "transferring room ownership to a non-member should fail with membership guidance, got: {transfer_error}"
    );

    let media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/cli-actor-constraint.mp4",
            "--room-id",
            &room_id,
            "--username",
            &owner_username,
            "--name",
            "CLI Actor Constraint Media",
        ],
        "create media for actor publish-key coverage",
    )
    .await;
    let media_id = media["id"]
        .as_str()
        .expect("actor constraint media should include id")
        .to_string();

    let banned_lookup = run_synctv_remote_cli_json(
        &server,
        &["user", "get", "--user-id", &banned_user_id],
        "get banned actor state",
    )
    .await;
    assert_eq!(
        banned_lookup["status"].as_i64(),
        Some(synctv_proto::common::UserStatus::Banned as i64)
    );

    let banned_publish_key_error = run_synctv_remote_cli_failure(
        &server,
        &[
            "provider",
            "rtmp",
            "create-publish-key",
            "--room-id",
            &room_id,
            "--username",
            &banned_username,
            &media_id,
        ],
        "banned actor publish key",
    )
    .await;
    assert!(
        banned_publish_key_error.contains("is banned"),
        "banned actor publish-key should fail with explicit state message, got: {banned_publish_key_error}"
    );

    let banned_room = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "ban",
            &room_id,
            "--reason",
            "publish-key room guard",
        ],
        "ban room before creator publish key",
    )
    .await;
    assert_eq!(banned_room["isBanned"], true);

    let creator_banned_room_publish_key_error = run_synctv_remote_cli_failure(
        &server,
        &[
            "provider",
            "rtmp",
            "create-publish-key",
            "--room-id",
            &room_id,
            "--username",
            &owner_username,
            &media_id,
        ],
        "media creator publish key in banned room",
    )
    .await;
    assert!(
        creator_banned_room_publish_key_error.contains("Room is banned"),
        "media creator publish-key in banned room should fail with room state message, got: {creator_banned_room_publish_key_error}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_management_actor_membership_constraints_gate_playlist_and_media_mutations()
{
    let server = start_test_server().await;
    let suffix = unique_test_suffix();

    let owner_username = format!("cli_membership_owner_{suffix}");
    let outsider_username = format!("cli_membership_outsider_{suffix}");
    let outsider_password = "CliMembershipOutsiderPass12345!";

    create_cli_user(
        &server,
        &owner_username,
        &format!("cli-membership-owner-{suffix}@example.com"),
        "CliMembershipOwnerPass12345!",
        Some("active"),
        "create membership room owner",
    )
    .await;
    create_cli_user(
        &server,
        &outsider_username,
        &format!("cli-membership-outsider-{suffix}@example.com"),
        outsider_password,
        Some("active"),
        "create membership outsider",
    )
    .await;

    let room = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "create",
            &format!("CLI Membership Constraint Room {suffix}"),
            "--username",
            &owner_username,
        ],
        "create room for membership constraint coverage",
    )
    .await;
    let room_id = room["id"]
        .as_str()
        .expect("membership constraint room should include id")
        .to_string();

    let outsider_playlist_error = run_synctv_remote_cli_failure(
        &server,
        &[
            "playlist",
            "create",
            "--room-id",
            &room_id,
            "--username",
            &outsider_username,
            "Outsider Playlist",
        ],
        "outsider playlist create before joining room",
    )
    .await;
    assert!(
        outsider_playlist_error.contains("Not a member of this room"),
        "outsider playlist create should fail with explicit membership error, got: {outsider_playlist_error}"
    );

    let outsider_media_error = run_synctv_remote_cli_failure(
        &server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/cli-membership-outsider.mp4",
            "--room-id",
            &room_id,
            "--username",
            &outsider_username,
            "--name",
            "Outsider Media",
        ],
        "outsider media add before joining room",
    )
    .await;
    assert!(
        outsider_media_error.contains("Not a member of this room"),
        "outsider media add should fail with explicit membership error, got: {outsider_media_error}"
    );

    let outsider_token = login_http_ok_token(&server, &outsider_username, outsider_password).await;
    let outsider_join = join_room_http(&server, &room_id, "", &outsider_token).await;
    assert_eq!(
        outsider_join.status(),
        StatusCode::OK,
        "outsider should be able to join the room before retrying resource mutations"
    );

    let outsider_playlist = run_synctv_remote_cli_json(
        &server,
        &[
            "playlist",
            "create",
            "--room-id",
            &room_id,
            "--username",
            &outsider_username,
            "Joined Playlist",
        ],
        "joined outsider playlist create",
    )
    .await;
    let playlist_id = outsider_playlist["id"]
        .as_str()
        .expect("joined outsider playlist create should include id")
        .to_string();
    assert_eq!(outsider_playlist["name"], "Joined Playlist");

    let outsider_media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/cli-membership-joined.mp4",
            "--room-id",
            &room_id,
            "--username",
            &outsider_username,
            "--playlist-id",
            &playlist_id,
            "--name",
            "Joined Outsider Media",
        ],
        "joined outsider media add",
    )
    .await;
    assert_eq!(outsider_media["name"], "Joined Outsider Media");
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_settings_and_system_commands_manage_remote_runtime_state() {
    let server = start_test_server().await;

    let settings_list = run_synctv_remote_cli(&server, &["settings", "list"]).await;
    assert!(
        settings_list.status.success(),
        "settings list via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&settings_list.stdout),
        String::from_utf8_lossy(&settings_list.stderr),
    );
    let settings_list_body: Value = serde_json::from_slice(&settings_list.stdout)
        .expect("CLI settings list output should be JSON");
    assert_eq!(
        settings_list_body["roomCreation"]["enabled"], true,
        "CLI settings list output should include structured room creation settings: {settings_list_body}"
    );

    let settings_get = run_synctv_remote_cli(&server, &["settings", "get", "roomCreation"]).await;
    assert!(
        settings_get.status.success(),
        "settings get via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&settings_get.stdout),
        String::from_utf8_lossy(&settings_get.stderr),
    );
    let settings_get_body: Value = serde_json::from_slice(&settings_get.stdout)
        .expect("CLI settings get output should be JSON");
    assert_eq!(settings_get_body["enabled"], true);

    let settings_update = run_synctv_remote_cli(
        &server,
        &[
            "settings",
            "update",
            "--request-json",
            r#"{"settings":{"roomCreation":{"maxRoomsPerUser":"42"}},"updateMask":"roomCreation.maxRoomsPerUser"}"#,
        ],
    )
    .await;
    assert!(
        settings_update.status.success(),
        "settings update via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&settings_update.stdout),
        String::from_utf8_lossy(&settings_update.stderr),
    );
    let settings_update_body: Value = serde_json::from_slice(&settings_update.stdout)
        .expect("CLI settings update output should be JSON");
    assert_eq!(
        settings_update_body["roomCreation"]["maxRoomsPerUser"],
        "42"
    );

    let mut management_client =
        management_proto::management_service_client::ManagementServiceClient::connect(
            server.management_base_url.clone(),
        )
        .await
        .expect("connect management gRPC client");
    let settings = management_client
        .get_settings(management_request(management_proto::GetSettingsRequest {}))
        .await
        .expect("management get_settings should succeed after CLI update")
        .into_inner();
    assert_eq!(
        settings
            .room_creation
            .expect("room_creation runtime settings")
            .max_rooms_per_user,
        42
    );

    let system_stats = run_synctv_remote_cli(&server, &["system", "stats"]).await;
    assert!(
        system_stats.status.success(),
        "system stats via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&system_stats.stdout),
        String::from_utf8_lossy(&system_stats.stderr),
    );
    let system_stats_body: Value = serde_json::from_slice(&system_stats.stdout)
        .expect("CLI system stats output should be JSON");
    assert!(
        json_i64(&system_stats_body["totalUsers"]).expect("totalUsers should be numeric") >= 1,
        "system stats should report at least the bootstrap root user: {system_stats_body}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_system_stats_uses_explicit_management_endpoint_flag_without_config() {
    let server = start_test_server().await;

    let system_stats = run_synctv_cli_with_env(
        &[
            "system",
            "stats",
            "--endpoint",
            &server.management_base_url,
            "--output",
            "json",
        ],
        &[("SYNCTV_MANAGEMENT_AUTH_TOKEN", MANAGEMENT_E2E_AUTH_TOKEN)],
    )
    .await;
    assert!(
        system_stats.status.success(),
        "system stats via CLI explicit endpoint should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&system_stats.stdout),
        String::from_utf8_lossy(&system_stats.stderr),
    );
    let system_stats_body: Value = serde_json::from_slice(&system_stats.stdout)
        .expect("CLI explicit endpoint system stats output should be JSON");
    assert!(
        json_i64(&system_stats_body["totalUsers"]).expect("totalUsers should be numeric") >= 1,
        "system stats should report at least the bootstrap root user: {system_stats_body}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_system_stats_works_when_bootstrap_root_username_differs() {
    let server = start_test_server().await;

    let system_stats = run_synctv_remote_cli(&server, &["system", "stats"]).await;
    assert!(
        system_stats.status.success(),
        "system stats via CLI should succeed when bootstrap root username differs\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&system_stats.stdout),
        String::from_utf8_lossy(&system_stats.stderr),
    );
    let system_stats_body: Value = serde_json::from_slice(&system_stats.stdout)
        .expect("CLI system stats output should be JSON when bootstrap root username differs");
    assert!(
        json_i64(&system_stats_body["totalUsers"]).expect("totalUsers should be numeric") >= 1,
        "system stats should report at least the bootstrap root user: {system_stats_body}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_provider_create_rejects_unsupported_provider_type() {
    let server = start_test_server().await;
    let suffix = unique_test_suffix();
    let provider_name = format!("unsupported-provider-{suffix}");

    let provider_add = run_synctv_remote_cli(
        &server,
        &[
            "provider",
            "create",
            &provider_name,
            "http://127.0.0.1:59999",
            "--provider",
            "custom_local",
        ],
    )
    .await;
    assert!(
        !provider_add.status.success(),
        "provider add with unsupported provider type should fail\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_add.stdout),
        String::from_utf8_lossy(&provider_add.stderr),
    );
    assert!(
        String::from_utf8_lossy(&provider_add.stderr).contains("invalid value 'custom_local'"),
        "stderr should explain unsupported provider type: {}",
        String::from_utf8_lossy(&provider_add.stderr)
    );

    server.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_provider_commands_manage_remote_provider_lifecycle() {
    let server = start_test_server().await;
    let suffix = unique_test_suffix();
    let provider_name = format!("remote-provider-{suffix}");

    let provider_add = run_synctv_remote_cli(
        &server,
        &[
            "provider",
            "create",
            &provider_name,
            &server.provider_probe_endpoint,
            "--provider",
            "alist",
            "--comment",
            "remote provider lifecycle e2e",
            "--jwt-secret",
            &server.provider_probe_secret,
        ],
    )
    .await;
    assert!(
        provider_add.status.success(),
        "remote provider add via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_add.stdout),
        String::from_utf8_lossy(&provider_add.stderr),
    );
    let provider_add_body: Value = serde_json::from_slice(&provider_add.stdout)
        .expect("CLI remote provider add output should be JSON");
    assert_eq!(provider_add_body["instance"]["name"], provider_name);
    assert_eq!(provider_add_body["instance"]["enabled"], true);
    assert_eq!(
        provider_add_body["instance"]["providers"],
        serde_json::to_value(vec![
            synctv_proto::source_config::SourceProvider::Alist as i32
        ])
        .expect("provider list should serialize")
    );

    let provider_list_filtered =
        run_synctv_remote_cli(&server, &["provider", "list", "--provider-type", "alist"]).await;
    assert!(
        provider_list_filtered.status.success(),
        "remote provider list via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_list_filtered.stdout),
        String::from_utf8_lossy(&provider_list_filtered.stderr),
    );
    let provider_list_filtered_body: Value = serde_json::from_slice(&provider_list_filtered.stdout)
        .expect("CLI remote provider list output should be JSON");
    assert!(
        provider_list_filtered_body["instances"]
            .as_array()
            .expect("instances should be an array")
            .iter()
            .any(|instance| instance["name"] == provider_name),
        "filtered remote provider list should include the created instance: {provider_list_filtered_body}"
    );

    let provider_reconnect =
        run_synctv_remote_cli(&server, &["provider", "reconnect", &provider_name]).await;
    assert!(
        provider_reconnect.status.success(),
        "remote provider reconnect via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_reconnect.stdout),
        String::from_utf8_lossy(&provider_reconnect.stderr),
    );

    let provider_disable =
        run_synctv_remote_cli(&server, &["provider", "disable", &provider_name]).await;
    assert!(
        provider_disable.status.success(),
        "remote provider disable via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_disable.stdout),
        String::from_utf8_lossy(&provider_disable.stderr),
    );
    let provider_disable_body: Value = serde_json::from_slice(&provider_disable.stdout)
        .expect("CLI remote provider disable output should be JSON");
    assert!(!provider_disable_body["instance"]["enabled"]
        .as_bool()
        .unwrap_or(false));

    let provider_enable =
        run_synctv_remote_cli(&server, &["provider", "enable", &provider_name]).await;
    assert!(
        provider_enable.status.success(),
        "remote provider enable via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_enable.stdout),
        String::from_utf8_lossy(&provider_enable.stderr),
    );
    let provider_enable_body: Value = serde_json::from_slice(&provider_enable.stdout)
        .expect("CLI remote provider enable output should be JSON");
    assert_eq!(provider_enable_body["instance"]["enabled"], true);

    let mut management_client =
        management_proto::management_service_client::ManagementServiceClient::connect(
            server.management_base_url.clone(),
        )
        .await
        .expect("connect management gRPC client");
    let provider_list_after_enable = management_client
        .list_provider_instances(management_request(
            synctv_proto::providers::common::ListProviderInstancesRequest {
                page: 1,
                page_size: 50,
                provider_type: synctv_proto::source_config::SourceProvider::Alist as i32,
                search: String::new(),
                enabled: None,
                tls: None,
                sort_by: synctv_proto::providers::common::ProviderInstanceListSortBy::CreatedAt
                    as i32,
                sort_direction: management_proto::SortDirection::Desc as i32,
            },
        ))
        .await
        .expect("management list_provider_instances should succeed after remote provider lifecycle")
        .into_inner();
    assert!(
        provider_list_after_enable
            .instances
            .into_iter()
            .any(|instance| instance.name == provider_name && instance.enabled),
        "remote provider should stay persisted and enabled after reconnect/disable/enable lifecycle"
    );

    let provider_delete =
        run_synctv_remote_cli(&server, &["provider", "delete", &provider_name]).await;
    assert!(
        provider_delete.status.success(),
        "remote provider delete via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_delete.stdout),
        String::from_utf8_lossy(&provider_delete.stderr),
    );
    let provider_delete_body: Value = serde_json::from_slice(&provider_delete.stdout)
        .expect("CLI remote provider delete output should be JSON");
    assert_eq!(provider_delete_body["success"], true);
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_system_stats_uses_management_unix_socket_via_env_without_config() {
    let (postgres, database_url) =
        create_test_database_url_with_label("synctv_e2e_unix", "full-stack-management-unix").await;
    let (redis, redis_url) = start_redis_url_with_label("full-stack-management-unix").await;
    let api_port = reserve_local_port();
    let management_port = reserve_local_port();
    let rtmp_port = reserve_local_port();
    let socket_dir = tempfile::tempdir().expect("temp dir should be created");
    let socket_path = socket_dir.path().join("management.sock");

    let mut config = test_config(
        database_url,
        redis_url,
        api_port,
        management_port,
        rtmp_port,
    );
    configure_management_unix_socket(&mut config, &socket_path);

    let app = Box::pin(Application::build_with_options(
        config,
        ApplicationBuildOptions {
            credential_encryption_key_override: Some(TEST_CREDENTIAL_ENCRYPTION_KEY.to_string()),
            ..ApplicationBuildOptions::default()
        },
    ))
    .await
    .expect("unix management application should build");

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (server_handle, startup_exit_rx) = spawn_server_task(async move {
        Box::pin(app.run_with_shutdown_signal(async move {
            let _ = shutdown_rx.await;
        }))
        .await
    });

    let api_base_url = format!("http://127.0.0.1:{api_port}");
    wait_until_live_or_server_exit(&api_base_url, &startup_exit_rx)
        .await
        .expect("unix management server should become live");
    wait_until_unix_grpc_ready_or_server_exit(&socket_path, &startup_exit_rx)
        .await
        .expect("unix management server should become ready over unix socket");

    let management_endpoint = format!("unix://{}", socket_path.display());
    let system_stats = run_synctv_cli_with_env_async(
        &["system", "stats", "--output", "json"],
        &[("SYNCTV_MANAGEMENT_ENDPOINT", management_endpoint.as_str())],
    )
    .await;
    assert!(
        system_stats.status.success(),
        "system stats via CLI over unix socket should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&system_stats.stdout),
        String::from_utf8_lossy(&system_stats.stderr),
    );
    let system_stats_body: Value = serde_json::from_slice(&system_stats.stdout)
        .expect("CLI unix socket system stats output should be JSON");
    assert!(
        json_i64(&system_stats_body["totalUsers"]).expect("totalUsers should be numeric") >= 1,
        "system stats over unix socket should report at least the bootstrap root user: {system_stats_body}"
    );

    let _ = shutdown_tx.send(());
    let server_result = server_handle
        .await
        .expect("unix management server task should join");
    server_result.expect("unix management server should shut down cleanly");

    drop(postgres);
    drop(redis);
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_system_stats_uses_default_management_unix_socket_without_overrides() {
    #[cfg(target_os = "macos")]
    let socket_root = tempfile::Builder::new()
        .prefix("stv")
        .tempdir_in("/tmp")
        .expect("isolated default socket root should be created under /tmp");
    #[cfg(all(unix, not(target_os = "macos")))]
    let socket_root = tempfile::tempdir().expect("isolated default socket root should be created");
    let (default_socket_path, cli_envs) = isolated_default_management_socket(socket_root.path());
    let suffix = unique_test_suffix();
    let database_name = format!("synctv_e2e_default_unix_{suffix}");
    let container_label = format!("full-stack-management-default-unix-{suffix}");
    let (postgres, database_url) =
        create_test_database_url_with_label(&database_name, &container_label).await;
    let (redis, redis_url) = start_redis_url_with_label(&container_label).await;
    let api_port = reserve_local_port();
    let management_port = reserve_local_port();
    let rtmp_port = reserve_local_port();

    let mut config = test_config(
        database_url,
        redis_url,
        api_port,
        management_port,
        rtmp_port,
    );
    configure_management_unix_socket(&mut config, &default_socket_path);
    config.management.enable_reflection = false;

    let app = Box::pin(Application::build_with_options(
        config,
        ApplicationBuildOptions {
            credential_encryption_key_override: Some(TEST_CREDENTIAL_ENCRYPTION_KEY.to_string()),
            ..ApplicationBuildOptions::default()
        },
    ))
    .await
    .expect("default unix management application should build");

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (server_handle, startup_exit_rx) = spawn_server_task(async move {
        Box::pin(app.run_with_shutdown_signal(async move {
            let _ = shutdown_rx.await;
        }))
        .await
    });

    let api_base_url = format!("http://127.0.0.1:{api_port}");
    wait_until_live_or_server_exit(&api_base_url, &startup_exit_rx)
        .await
        .expect("default unix management server should become live");
    wait_until_unix_grpc_ready_or_server_exit(&default_socket_path, &startup_exit_rx)
        .await
        .expect("default unix management server should become ready over unix socket");

    let cli_env_refs = cli_envs
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let system_stats = run_synctv_cli_with_env_async(
        &["system", "stats", "--output", "json", "--no-dotenv"],
        &cli_env_refs,
    )
    .await;
    assert!(
        system_stats.status.success(),
        "system stats via CLI default unix socket should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&system_stats.stdout),
        String::from_utf8_lossy(&system_stats.stderr),
    );
    let system_stats_body: Value = serde_json::from_slice(&system_stats.stdout)
        .expect("CLI default unix socket system stats output should be JSON");
    assert!(
        json_i64(&system_stats_body["totalUsers"]).expect("totalUsers should be numeric") >= 1,
        "system stats over default unix socket should report at least the bootstrap root user: {system_stats_body}"
    );

    let _ = shutdown_tx.send(());
    let server_result = server_handle
        .await
        .expect("default unix management server task should join");
    server_result.expect("default unix management server should shut down cleanly");

    drop(socket_root);
    drop(postgres);
    drop(redis);
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_system_stats_reads_management_unix_socket_auth_token_from_config() {
    let (postgres, database_url) = create_test_database_url_with_label(
        "synctv_e2e_unix_auth",
        "full-stack-management-unix-auth",
    )
    .await;
    let (redis, redis_url) = start_redis_url_with_label("full-stack-management-unix-auth").await;
    let api_port = reserve_local_port();
    let management_port = reserve_local_port();
    let rtmp_port = reserve_local_port();
    let socket_dir = tempfile::tempdir().expect("temp dir should be created");
    let socket_path = socket_dir.path().join("management.sock");
    let config_path = socket_dir.path().join("synctv.yaml");
    let management_auth_token = "unix-management-config-token";

    let mut config = test_config(
        database_url,
        redis_url,
        api_port,
        management_port,
        rtmp_port,
    );
    configure_management_unix_socket_with_auth_token(
        &mut config,
        &socket_path,
        management_auth_token,
    );
    let config_yaml = serde_yaml::to_string(&config).expect("config should serialize to yaml");
    std::fs::write(&config_path, config_yaml).expect("config file should be written");

    let app = Box::pin(Application::build_with_options(
        config,
        ApplicationBuildOptions {
            credential_encryption_key_override: Some(TEST_CREDENTIAL_ENCRYPTION_KEY.to_string()),
            ..ApplicationBuildOptions::default()
        },
    ))
    .await
    .expect("unix management auth application should build");

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (server_handle, startup_exit_rx) = spawn_server_task(async move {
        Box::pin(app.run_with_shutdown_signal(async move {
            let _ = shutdown_rx.await;
        }))
        .await
    });

    let api_base_url = format!("http://127.0.0.1:{api_port}");
    wait_until_live_or_server_exit(&api_base_url, &startup_exit_rx)
        .await
        .expect("unix management auth server should become live");
    wait_until_unix_grpc_ready_or_server_exit(&socket_path, &startup_exit_rx)
        .await
        .expect("unix management auth server should become ready over unix socket");

    let system_stats = run_synctv_cli_with_env_async(
        &[
            "system",
            "stats",
            "--output",
            "json",
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--no-dotenv",
        ],
        &[],
    )
    .await;
    assert!(
        system_stats.status.success(),
        "system stats via CLI over unix socket with config-managed auth token should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&system_stats.stdout),
        String::from_utf8_lossy(&system_stats.stderr),
    );
    let system_stats_body: Value = serde_json::from_slice(&system_stats.stdout)
        .expect("CLI unix socket system stats output should be JSON");
    assert!(
        json_i64(&system_stats_body["totalUsers"]).expect("totalUsers should be numeric") >= 1,
        "system stats over authenticated unix socket should report at least the bootstrap root user: {system_stats_body}"
    );

    let _ = shutdown_tx.send(());
    let server_result = server_handle
        .await
        .expect("unix management auth server task should join");
    server_result.expect("unix management auth server should shut down cleanly");

    drop(postgres);
    drop(redis);
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_explicit_management_endpoint_does_not_implicitly_use_config_auth_token() {
    let (postgres, database_url) = create_test_database_url_with_label(
        "synctv_e2e_unix_auth_explicit",
        "full-stack-management-unix-auth-explicit",
    )
    .await;
    let (redis, redis_url) =
        start_redis_url_with_label("full-stack-management-unix-auth-explicit").await;
    let api_port = reserve_local_port();
    let management_port = reserve_local_port();
    let rtmp_port = reserve_local_port();
    let socket_dir = tempfile::tempdir().expect("temp dir should be created");
    let socket_path = socket_dir.path().join("management.sock");
    let config_path = socket_dir.path().join("synctv.yaml");
    let management_auth_token = "unix-management-explicit-endpoint-token";

    let mut config = test_config(
        database_url,
        redis_url,
        api_port,
        management_port,
        rtmp_port,
    );
    configure_management_unix_socket_with_auth_token(
        &mut config,
        &socket_path,
        management_auth_token,
    );
    write_cli_test_config(&config_path, &config);

    let app = Box::pin(Application::build_with_options(
        config,
        ApplicationBuildOptions {
            credential_encryption_key_override: Some(TEST_CREDENTIAL_ENCRYPTION_KEY.to_string()),
            ..ApplicationBuildOptions::default()
        },
    ))
    .await
    .expect("unix management auth application should build");

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (server_handle, startup_exit_rx) = spawn_server_task(async move {
        Box::pin(app.run_with_shutdown_signal(async move {
            let _ = shutdown_rx.await;
        }))
        .await
    });

    let api_base_url = format!("http://127.0.0.1:{api_port}");
    wait_until_live_or_server_exit(&api_base_url, &startup_exit_rx)
        .await
        .expect("unix management explicit-endpoint server should become live");
    wait_until_unix_grpc_ready_or_server_exit(&socket_path, &startup_exit_rx)
        .await
        .expect("unix management explicit-endpoint server should become ready over unix socket");

    let management_endpoint = format!("unix://{}", socket_path.display());
    let system_stats = run_synctv_cli_with_env_async(
        &[
            "system",
            "stats",
            "--output",
            "json",
            "--endpoint",
            &management_endpoint,
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--no-dotenv",
        ],
        &[],
    )
    .await;
    assert!(
        !system_stats.status.success(),
        "explicit endpoint should not implicitly use the config auth token\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&system_stats),
        cli_stderr(&system_stats),
    );

    let stderr = cli_stderr(&system_stats);
    assert!(
        stderr.contains("authentication failed"),
        "explicit endpoint without env auth token should fail authentication\nstderr:\n{stderr}"
    );

    let _ = shutdown_tx.send(());
    let server_result = server_handle
        .await
        .expect("unix management auth server task should join");
    server_result.expect("unix management auth server should shut down cleanly");

    drop(postgres);
    drop(redis);
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_stop_gracefully_shuts_down_server_via_management_api() {
    let (postgres, database_url) =
        create_test_database_url_with_label("synctv_e2e_stop_graceful", "full-stack-stop-graceful")
            .await;
    let (redis, redis_url) = start_redis_url_with_label("full-stack-stop-graceful").await;
    let api_port = reserve_local_port();
    let management_port = reserve_local_port();
    let rtmp_port = reserve_local_port();
    let socket_dir = tempfile::tempdir().expect("temp dir should be created");
    let socket_path = socket_dir.path().join("management.sock");

    let mut config = test_config(
        database_url,
        redis_url,
        api_port,
        management_port,
        rtmp_port,
    );
    configure_management_unix_socket(&mut config, &socket_path);

    let app = Box::pin(Application::build_with_options(
        config,
        ApplicationBuildOptions {
            credential_encryption_key_override: Some(TEST_CREDENTIAL_ENCRYPTION_KEY.to_string()),
            ..ApplicationBuildOptions::default()
        },
    ))
    .await
    .expect("stop test application should build");

    let (server_handle, startup_exit_rx) =
        spawn_server_task(async move { Box::pin(app.run()).await });

    let api_base_url = format!("http://127.0.0.1:{api_port}");
    wait_until_live_or_server_exit(&api_base_url, &startup_exit_rx)
        .await
        .expect("graceful stop test server should become live");
    wait_until_unix_grpc_ready_or_server_exit(&socket_path, &startup_exit_rx)
        .await
        .expect("graceful stop test server should become ready over unix socket");

    let management_endpoint = format!("unix://{}", socket_path.display());
    let stop_output = run_synctv_cli_with_env_async(
        &["stop"],
        &[("SYNCTV_MANAGEMENT_ENDPOINT", management_endpoint.as_str())],
    )
    .await;

    assert!(
        stop_output.status.success(),
        "stop CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&stop_output.stdout),
        String::from_utf8_lossy(&stop_output.stderr),
    );

    let stop_stdout = String::from_utf8_lossy(&stop_output.stdout);
    assert!(
        stop_stdout.contains("shutdown requested"),
        "stop CLI should stream shutdown request status\nstdout:\n{stop_stdout}"
    );
    assert!(
        stop_stdout.contains("shutdown complete"),
        "stop CLI should stream shutdown completion status\nstdout:\n{stop_stdout}"
    );

    let server_result = tokio::time::timeout(Duration::from_secs(30), server_handle)
        .await
        .expect("server should stop after CLI stop request")
        .expect("server task should join");
    server_result.expect("server should shut down cleanly after CLI stop");

    drop(postgres);
    drop(redis);
}

#[tokio::test]
async fn full_stack_cli_config_validate_and_show_use_explicit_config_file() {
    let api_port = reserve_local_port();
    let management_port = reserve_local_port();
    let rtmp_port = reserve_local_port();
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let config_path = temp_dir.path().join("synctv.yaml");

    let mut config = test_config(
        "postgresql://postgres:super-secret-db@db.internal:5432/synctv_cli_config".to_string(),
        "redis://:redis-secret@redis.internal:6379/0".to_string(),
        api_port,
        management_port,
        rtmp_port,
    );
    config.management.auth_token = "management-config-secret".to_string();
    write_cli_test_config(&config_path, &config);

    let validate_output = run_synctv_cli_with_env_async(
        &[
            "config",
            "validate",
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--no-dotenv",
        ],
        &[],
    )
    .await;
    assert!(
        validate_output.status.success(),
        "config validate should succeed\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&validate_output),
        cli_stderr(&validate_output),
    );
    let validate_stdout = cli_stdout(&validate_output);
    assert!(
        validate_stdout.contains("Configuration is valid"),
        "config validate should confirm success\nstdout:\n{validate_stdout}"
    );
    assert!(
        validate_stdout.contains(&format!("127.0.0.1:{api_port}")),
        "config validate should print the resolved API address\nstdout:\n{validate_stdout}"
    );

    let show_output = run_synctv_cli_with_env_async(
        &[
            "config",
            "show",
            "--output",
            "json",
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--no-dotenv",
        ],
        &[],
    )
    .await;
    let show_body = cli_json_output(&show_output, "config show");
    let show_text = cli_stdout(&show_output);
    assert_eq!(show_body["server"]["port"], api_port);
    assert_eq!(show_body["management"]["authToken"], "<redacted>");
    assert!(
        show_body["database"]["url"]
            .as_str()
            .expect("rendered database url should be a string")
            .contains("***"),
        "config show should mask database credentials: {show_text}"
    );
    assert!(
        !show_text.contains("super-secret-db")
            && !show_text.contains("redis-secret")
            && !show_text.contains("management-config-secret"),
        "config show should redact embedded secrets: {show_text}"
    );

    let show_toml_output = run_synctv_cli_with_env_async(
        &[
            "config",
            "show",
            "--output",
            "toml",
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--no-dotenv",
        ],
        &[],
    )
    .await;
    assert!(
        show_toml_output.status.success(),
        "config show --output toml should succeed\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&show_toml_output),
        cli_stderr(&show_toml_output),
    );
    let show_toml = cli_stdout(&show_toml_output);
    assert!(
        show_toml.contains("[server]") && show_toml.contains("port ="),
        "config show --output toml should render TOML tables\nstdout:\n{show_toml}"
    );
    assert!(
        !show_toml.contains("= null"),
        "config show --output toml should prune null values before rendering\nstdout:\n{show_toml}"
    );
    assert!(
        !show_toml.contains("super-secret-db")
            && !show_toml.contains("redis-secret")
            && !show_toml.contains("management-config-secret"),
        "config show --output toml should redact embedded secrets\nstdout:\n{show_toml}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_db_status_reports_migration_readiness_and_db_migrate_is_idempotent() {
    let (postgres, database_url) =
        create_test_database_url_with_label("synctv_e2e_db_cli", "full-stack-cli-db").await;
    let (redis, redis_url) = start_redis_url_with_label("full-stack-cli-db").await;
    let api_port = reserve_local_port();
    let management_port = reserve_local_port();
    let rtmp_port = reserve_local_port();
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let config_path = temp_dir.path().join("synctv-db-cli.yaml");

    let config = test_config(
        database_url.clone(),
        redis_url,
        api_port,
        management_port,
        rtmp_port,
    );
    write_cli_test_config(&config_path, &config);
    postgres.recreate_as_empty_database().await;

    let pending_status = run_synctv_cli_with_env_async(
        &[
            "db",
            "status",
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--no-dotenv",
        ],
        &[],
    )
    .await;
    assert!(
        pending_status.status.success(),
        "db status before migrations should succeed\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&pending_status),
        cli_stderr(&pending_status),
    );
    let pending_stdout = cli_stdout(&pending_status);
    assert!(
        pending_stdout.contains("Database connection: OK"),
        "db status should verify connectivity\nstdout:\n{pending_stdout}"
    );
    assert!(
        pending_stdout.contains("Migration status: pending"),
        "db status should report unapplied embedded migrations\nstdout:\n{pending_stdout}"
    );
    assert!(
        pending_stdout.contains("postgresql://***:***@"),
        "db status should mask database credentials\nstdout:\n{pending_stdout}"
    );

    let migrate_output = run_synctv_cli_with_env_async(
        &[
            "db",
            "migrate",
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--no-dotenv",
        ],
        &[],
    )
    .await;
    assert!(
        migrate_output.status.success(),
        "db migrate should succeed\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&migrate_output),
        cli_stderr(&migrate_output),
    );

    let pool = connect_test_pool_url(&database_url).await;
    let applied_migrations: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM _sqlx_migrations WHERE success = true"#
    )
    .fetch_one(&pool)
    .await
    .expect("_sqlx_migrations should exist after db migrate");
    assert!(
        applied_migrations > 0,
        "db migrate should apply embedded migrations"
    );
    pool.close().await;

    let second_migrate_output = run_synctv_cli_with_env_async(
        &[
            "db",
            "migrate",
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--no-dotenv",
        ],
        &[],
    )
    .await;
    assert!(
        second_migrate_output.status.success(),
        "db migrate should be idempotent\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&second_migrate_output),
        cli_stderr(&second_migrate_output),
    );

    let ready_status = run_synctv_cli_with_env_async(
        &[
            "db",
            "status",
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--no-dotenv",
        ],
        &[],
    )
    .await;
    assert!(
        ready_status.status.success(),
        "db status after migrations should succeed\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&ready_status),
        cli_stderr(&ready_status),
    );
    let ready_stdout = cli_stdout(&ready_status);
    assert!(
        ready_stdout.contains("Migration status: ready"),
        "db status should report ready after migrations are applied\nstdout:\n{ready_stdout}"
    );

    let pool = connect_test_pool_url(&database_url).await;
    sqlx::query!(
        "UPDATE _sqlx_migrations \
         SET checksum = decode('00', 'hex') \
         WHERE version = (SELECT MIN(version) FROM _sqlx_migrations WHERE success = true)"
    )
    .execute(&pool)
    .await
    .expect("test should tamper with migration checksum");
    pool.close().await;

    let broken_status = run_synctv_cli_with_env_async(
        &[
            "db",
            "status",
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--no-dotenv",
        ],
        &[],
    )
    .await;
    assert!(
        broken_status.status.success(),
        "db status with drifted history should still report status\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&broken_status),
        cli_stderr(&broken_status),
    );
    let broken_stdout = cli_stdout(&broken_status);
    assert!(
        broken_stdout.contains("Migration status: broken"),
        "db status should distinguish drifted history from pending\nstdout:\n{broken_stdout}"
    );
    assert!(
        broken_stdout.contains(
            "Migration detail: applied migration history does not match the embedded migration set"
        ),
        "db status should explain drifted migration history\nstdout:\n{broken_stdout}"
    );

    drop(postgres);
    drop(redis);
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_db_status_fails_when_migration_metadata_is_unreadable() {
    let (postgres, database_url) =
        create_test_database_url_with_label("synctv_e2e_db_cli_acl", "full-stack-cli-db-acl").await;
    let (redis, redis_url) = start_redis_url_with_label("full-stack-cli-db-acl").await;
    let api_port = reserve_local_port();
    let management_port = reserve_local_port();
    let rtmp_port = reserve_local_port();
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let config_path = temp_dir.path().join("synctv-db-cli-acl.yaml");

    let config = test_config(
        database_url.clone(),
        redis_url.clone(),
        api_port,
        management_port,
        rtmp_port,
    );
    write_cli_test_config(&config_path, &config);

    let migrate_output = run_synctv_cli_with_env_async(
        &[
            "db",
            "migrate",
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--no-dotenv",
        ],
        &[],
    )
    .await;
    assert!(
        migrate_output.status.success(),
        "db migrate should succeed before permission test\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&migrate_output),
        cli_stderr(&migrate_output),
    );

    let limited_role = format!("status_reader_{}", unique_test_suffix());
    let limited_password = "StatusPwd12345!";
    let limited_role_ident = quote_pg_ident(&limited_role);
    let database_ident = quote_pg_ident(postgres.database_name());
    let limited_password_literal = quote_pg_literal(limited_password);
    let admin_pool = connect_test_pool_url(&database_url).await;
    sqlx::query(trusted_dynamic_sql(format!(
        "CREATE ROLE {limited_role_ident} LOGIN PASSWORD {limited_password_literal}"
    )))
    .execute(&admin_pool)
    .await
    .expect("test should create a limited role");
    sqlx::query(trusted_dynamic_sql(format!(
        "GRANT CONNECT ON DATABASE {database_ident} TO {limited_role_ident}"
    )))
    .execute(&admin_pool)
    .await
    .expect("test should grant database connect");
    sqlx::query(trusted_dynamic_sql(format!(
        "GRANT USAGE ON SCHEMA public TO {limited_role_ident}"
    )))
    .execute(&admin_pool)
    .await
    .expect("test should grant schema usage without table reads");
    admin_pool.close().await;

    let limited_database_url = postgres_connection_url_with_credentials(
        &postgres.host(),
        postgres.port_ipv4(5432),
        postgres.database_name(),
        &limited_role,
        limited_password,
    );
    let limited_config = test_config(
        limited_database_url,
        redis_url,
        reserve_local_port(),
        reserve_local_port(),
        reserve_local_port(),
    );
    let limited_config_path = temp_dir.path().join("synctv-db-cli-acl-limited.yaml");
    write_cli_test_config(&limited_config_path, &limited_config);

    let status_output = run_synctv_cli_with_env_async(
        &[
            "db",
            "status",
            "--config",
            limited_config_path
                .to_str()
                .expect("config path should be utf-8"),
            "--no-dotenv",
        ],
        &[],
    )
    .await;
    assert!(
        !status_output.status.success(),
        "db status should fail when migration metadata cannot be inspected\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&status_output),
        cli_stderr(&status_output),
    );
    let stderr = cli_stderr(&status_output);
    assert!(
        stderr.contains("Failed to inspect") && stderr.contains("_sqlx_migrations"),
        "db status should surface migration inspection failures\nstderr:\n{stderr}"
    );

    drop(postgres);
    drop(redis);
}

#[tokio::test]
async fn full_stack_cli_serve_dry_run_validates_config_without_starting_listeners() {
    let api_port = reserve_local_port();
    let management_port = reserve_local_port();
    let rtmp_port = reserve_local_port();
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let config_path = temp_dir.path().join("synctv-dry-run.yaml");

    let config = test_config(
        "postgresql://postgres:password@db.invalid/synctv_dry_run".to_string(),
        "redis://redis.invalid/0".to_string(),
        api_port,
        management_port,
        rtmp_port,
    );
    write_cli_test_config(&config_path, &config);

    let dry_run_output = run_synctv_cli_with_env_async(
        &[
            "serve",
            "--dry-run",
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--no-dotenv",
        ],
        &[],
    )
    .await;
    assert!(
        dry_run_output.status.success(),
        "serve --dry-run should succeed\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&dry_run_output),
        cli_stderr(&dry_run_output),
    );

    let combined_output = format!(
        "{}\n{}",
        cli_stdout(&dry_run_output),
        cli_stderr(&dry_run_output)
    );
    assert!(
        combined_output.contains("Dry run requested"),
        "serve --dry-run should report that startup was skipped\noutput:\n{combined_output}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_server_binary_starts_and_handles_management_commands() {
    let (postgres, database_url) = create_test_database_url_with_label(
        "synctv_e2e_server_binary_cli",
        "full-stack-server-binary-cli",
    )
    .await;
    let (redis, redis_url) = start_redis_url_with_label("full-stack-server-binary-cli").await;
    let api_port = reserve_local_port();
    let management_port = reserve_local_port();
    let rtmp_port = reserve_local_port();
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let config_path = temp_dir.path().join("synctv-server-binary.yaml");

    let mut config = test_config(
        database_url,
        redis_url,
        api_port,
        management_port,
        rtmp_port,
    );
    config.management.auth_token = MANAGEMENT_E2E_AUTH_TOKEN.to_string();
    write_cli_test_config(&config_path, &config);

    let config_path_string = config_path
        .to_str()
        .expect("config path should be valid utf-8")
        .to_string();
    let mut server_process = spawn_synctv_process(
        &["serve", "--config", &config_path_string, "--no-dotenv"],
        &[(
            "SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY",
            TEST_CREDENTIAL_ENCRYPTION_KEY,
        )],
        temp_dir.path(),
    );

    let api_base_url = format!("http://127.0.0.1:{api_port}");
    let management_base_url = format!("http://127.0.0.1:{management_port}");
    wait_until_http_live_or_process_exit(
        &api_base_url,
        &mut server_process.child,
        &server_process.stdout_log_path,
        &server_process.stderr_log_path,
    )
    .await
    .expect("server binary should become live");
    wait_until_grpc_ready_or_process_exit(
        &management_base_url,
        &mut server_process.child,
        &server_process.stdout_log_path,
        &server_process.stderr_log_path,
    )
    .await
    .expect("server binary management endpoint should become ready");

    let envs = [
        ("SYNCTV_MANAGEMENT_ENDPOINT", management_base_url.as_str()),
        ("SYNCTV_MANAGEMENT_AUTH_TOKEN", MANAGEMENT_E2E_AUTH_TOKEN),
    ];

    let system_stats =
        run_synctv_cli_with_env_async(&["system", "stats", "--output", "json"], &envs).await;
    let system_stats_body = cli_json_output(&system_stats, "system stats against server binary");
    assert!(
        json_i64(&system_stats_body["totalUsers"]).unwrap_or_default() >= 1,
        "system stats should report the bootstrap root user: {system_stats_body}"
    );

    let created_user = run_synctv_cli_with_env_async(
        &[
            "user",
            "create",
            "binary_cli_owner",
            "--email",
            "binary-cli-owner@example.com",
            "--password",
            "BinaryCliOwnerPass12345!",
            "--output",
            "json",
        ],
        &envs,
    )
    .await;
    let created_user_body = cli_json_output(&created_user, "user create against server binary");
    let created_user_id = created_user_body["id"]
        .as_str()
        .expect("user create should return id");

    let listed_users =
        run_synctv_cli_with_env_async(&["user", "list", "--output", "json"], &envs).await;
    let listed_users_body = cli_json_output(&listed_users, "user list against server binary");
    assert!(
        listed_users_body["users"]
            .as_array()
            .expect("user list should return users array")
            .iter()
            .any(|user| user["id"] == created_user_id),
        "freshly created user should appear in user list: {listed_users_body}"
    );

    let created_room = run_synctv_cli_with_env_async(
        &[
            "room",
            "create",
            "Binary Server Room",
            "--username",
            "binary_cli_owner",
            "--description",
            "room created through the binary-started server",
            "--output",
            "json",
        ],
        &envs,
    )
    .await;
    let created_room_body = cli_json_output(&created_room, "room create against server binary");
    let room_id = created_room_body["id"]
        .as_str()
        .expect("room create should return id");

    let fetched_room =
        run_synctv_cli_with_env_async(&["room", "get", room_id, "--output", "json"], &envs).await;
    let fetched_room_body = cli_json_output(&fetched_room, "room get against server binary");
    assert_eq!(fetched_room_body["id"], room_id);
    assert_eq!(fetched_room_body["creatorUsername"], "binary_cli_owner");

    let stop_output = run_synctv_cli_with_env_async(&["stop"], &envs).await;
    assert!(
        stop_output.status.success(),
        "stop CLI should succeed against the binary-started server\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&stop_output),
        cli_stderr(&stop_output),
    );

    let server_status = tokio::time::timeout(Duration::from_secs(30), server_process.child.wait())
        .await
        .expect("binary-started server should stop after stop CLI")
        .expect("binary-started server process should join");
    assert!(
        server_status.success(),
        "binary-started server should exit successfully after stop\n{}",
        cli_process_failure_details(
            &server_process.stdout_log_path,
            &server_process.stderr_log_path
        )
    );

    drop(postgres);
    drop(redis);
}

#[tokio::test]
async fn full_stack_cli_version_prints_package_name_and_version() {
    let version_output = run_synctv_cli_with_env_async(&["version"], &[]).await;
    assert!(
        version_output.status.success(),
        "version CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&version_output),
        cli_stderr(&version_output),
    );

    assert_eq!(
        cli_stdout(&version_output).trim(),
        format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
        "version CLI should print the package name and version"
    );
}

#[tokio::test]
async fn full_stack_cli_completion_bash_emits_shell_script() {
    let completion_output = run_synctv_cli_with_env_async(&["completion", "bash"], &[]).await;
    assert!(
        completion_output.status.success(),
        "completion bash should succeed\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&completion_output),
        cli_stderr(&completion_output),
    );

    let completion_stdout = cli_stdout(&completion_output);
    assert!(
        completion_stdout.contains("_synctv()"),
        "bash completion should define the synctv completion function\nstdout:\n{completion_stdout}"
    );
    assert!(
        completion_stdout.contains("complete -F _synctv") && completion_stdout.contains(" synctv"),
        "bash completion should register completion for synctv\nstdout:\n{completion_stdout}"
    );
}

#[tokio::test]
async fn full_stack_cli_completion_zsh_emits_compdef_registration() {
    let completion_output = run_synctv_cli_with_env_async(&["completion", "zsh"], &[]).await;
    assert!(
        completion_output.status.success(),
        "completion zsh should succeed\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&completion_output),
        cli_stderr(&completion_output),
    );

    let completion_stdout = cli_stdout(&completion_output);
    assert!(
        completion_stdout.contains("#compdef synctv"),
        "zsh completion should declare the target command\nstdout:\n{completion_stdout}"
    );
    assert!(
        completion_stdout.contains("compdef _synctv synctv"),
        "zsh completion should register completion for synctv\nstdout:\n{completion_stdout}"
    );
}

#[tokio::test]
async fn full_stack_cli_completion_fish_emits_completion_directives() {
    let completion_output = run_synctv_cli_with_env_async(&["completion", "fish"], &[]).await;
    assert!(
        completion_output.status.success(),
        "completion fish should succeed\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&completion_output),
        cli_stderr(&completion_output),
    );

    let completion_stdout = cli_stdout(&completion_output);
    assert!(
        completion_stdout.contains("complete -c synctv"),
        "fish completion should emit complete directives for synctv\nstdout:\n{completion_stdout}"
    );
    assert!(
        completion_stdout.contains("__fish_synctv_global_optspecs"),
        "fish completion should define the global option spec helper\nstdout:\n{completion_stdout}"
    );
}

#[tokio::test]
async fn full_stack_cli_system_stats_reports_unreachable_management_endpoint() {
    let output = run_synctv_cli_with_env_async(
        &[
            "system",
            "stats",
            "--endpoint",
            "http://127.0.0.1:9",
            "--output",
            "json",
            "--no-dotenv",
        ],
        &[],
    )
    .await;
    assert!(
        !output.status.success(),
        "system stats should fail when the explicit management endpoint is unreachable\nstdout:\n{}\nstderr:\n{}",
        cli_stdout(&output),
        cli_stderr(&output),
    );

    let stderr = cli_stderr(&output);
    assert!(
        stderr.contains("failed to connect to any management endpoint"),
        "connection failure should mention that no management endpoint could be reached\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("127.0.0.1:9"),
        "connection failure should include the unreachable endpoint for debugging\nstderr:\n{stderr}"
    );
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_force_stop_shuts_down_server_via_management_api() {
    let (postgres, database_url) =
        create_test_database_url_with_label("synctv_e2e_stop_force", "full-stack-stop-force").await;
    let (redis, redis_url) = start_redis_url_with_label("full-stack-stop-force").await;
    let api_port = reserve_local_port();
    let management_port = reserve_local_port();
    let rtmp_port = reserve_local_port();
    let socket_dir = tempfile::tempdir().expect("temp dir should be created");
    let socket_path = socket_dir.path().join("management.sock");

    let mut config = test_config(
        database_url,
        redis_url,
        api_port,
        management_port,
        rtmp_port,
    );
    configure_management_unix_socket(&mut config, &socket_path);

    let app = Box::pin(Application::build_with_options(
        config,
        ApplicationBuildOptions {
            credential_encryption_key_override: Some(TEST_CREDENTIAL_ENCRYPTION_KEY.to_string()),
            ..ApplicationBuildOptions::default()
        },
    ))
    .await
    .expect("force stop test application should build");

    let (server_handle, startup_exit_rx) =
        spawn_server_task(async move { Box::pin(app.run()).await });

    let api_base_url = format!("http://127.0.0.1:{api_port}");
    wait_until_live_or_server_exit(&api_base_url, &startup_exit_rx)
        .await
        .expect("force stop test server should become live");
    wait_until_unix_grpc_ready_or_server_exit(&socket_path, &startup_exit_rx)
        .await
        .expect("force stop test server should become ready over unix socket");

    let management_endpoint = format!("unix://{}", socket_path.display());
    let stop_output = run_synctv_cli_with_env_async(
        &["stop", "--force"],
        &[("SYNCTV_MANAGEMENT_ENDPOINT", management_endpoint.as_str())],
    )
    .await;

    assert!(
        stop_output.status.success(),
        "force stop CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&stop_output.stdout),
        String::from_utf8_lossy(&stop_output.stderr),
    );

    let stop_stdout = String::from_utf8_lossy(&stop_output.stdout);
    assert!(
        stop_stdout.contains("force shutdown requested"),
        "force stop CLI should stream force shutdown request status\nstdout:\n{stop_stdout}"
    );
    assert!(
        stop_stdout.contains("shutdown complete"),
        "force stop CLI should stream shutdown completion status\nstdout:\n{stop_stdout}"
    );

    let server_result = tokio::time::timeout(Duration::from_secs(30), server_handle)
        .await
        .expect("server should stop after CLI force stop request")
        .expect("server task should join");
    server_result.expect("server should shut down cleanly after CLI force stop");

    drop(postgres);
    drop(redis);
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_http_auth_room_and_ticket_flow_enforces_membership() {
    let server = start_test_server().await;
    let client = test_http_client();

    let owner_register = opaque_http_register(
        &server,
        "owner_user",
        "owner@example.com",
        "OwnerPass12345!",
    )
    .await;
    assert_eq!(owner_register.status(), StatusCode::OK);

    let owner_token = login_http_ok_token(&server, "owner_user", "OwnerPass12345!").await;

    let owner_profile = get_with_bearer(
        &client,
        &format!("{}/api/user", server.api_base_url),
        &owner_token,
    )
    .await;
    assert_eq!(owner_profile.status(), StatusCode::OK);
    let owner_profile_body = response_json(owner_profile).await;
    assert_eq!(owner_profile_body["username"], "owner_user");

    let create_room = post_json(
        &client,
        &format!("{}/api/rooms", server.api_base_url),
        create_room_request("Full Stack Room", "end-to-end room", "RoomPass12345!"),
        Some(&owner_token),
    )
    .await;
    assert_eq!(create_room.status(), StatusCode::OK);
    let create_room_body = response_json(create_room).await;
    let room_id = create_room_body["id"]
        .as_str()
        .expect("created room id")
        .to_string();

    let member_register = opaque_http_register(
        &server,
        "member_user",
        "member@example.com",
        "MemberPass12345!",
    )
    .await;
    assert_eq!(member_register.status(), StatusCode::OK);

    let member_token = login_http_ok_token(&server, "member_user", "MemberPass12345!").await;

    let forbidden_ticket = post_json(
        &client,
        &format!("{}/api/tickets", server.api_base_url),
        create_ticket_request(&room_id),
        Some(&member_token),
    )
    .await;
    assert_eq!(forbidden_ticket.status(), StatusCode::FORBIDDEN);
    let forbidden_ticket_body = response_json(forbidden_ticket).await;
    assert!(
        error_message(&forbidden_ticket_body).contains("Not a member of this room"),
        "unexpected ticket denial body: {forbidden_ticket_body}"
    );

    let join_room = put_json(
        &client,
        &format!("{}/api/rooms/{room_id}/members/@me", server.api_base_url),
        synctv_proto::client::JoinRoomRequest {
            room_id: room_id.clone(),
            password: "RoomPass12345!".to_string(),
            remark_name: String::new(),
            display_tag: String::new(),
        },
        &member_token,
    )
    .await;
    assert_eq!(join_room.status(), StatusCode::OK);
    let join_room_body = response_json(join_room).await;
    assert_eq!(join_room_body["room"]["id"], room_id);
    let members = join_room_body["members"]
        .as_array()
        .expect("join room should return members");
    assert_eq!(members.len(), 2);

    let member_ticket = post_json(
        &client,
        &format!("{}/api/tickets", server.api_base_url),
        create_ticket_request(&room_id),
        Some(&member_token),
    )
    .await;
    assert_eq!(member_ticket.status(), StatusCode::OK);
    let member_ticket_body = response_json(member_ticket).await;
    assert_eq!(member_ticket_body["roomId"], room_id);
    assert!(
        member_ticket_body["ticket"]
            .as_str()
            .expect("ticket string")
            .len()
            > 10
    );
    assert!(member_ticket_body["usage"]
        .as_str()
        .expect("usage string")
        .contains(&format!("/ws/rooms/{room_id}?ticket=")));

    // per-test isolated server
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_http_ticket_upgrades_websocket_once_and_then_expires() {
    use synctv_proto::client::{client_message, HeartbeatMessage};

    let server = start_test_server().await;
    let client = test_http_client();

    let register = opaque_http_register(
        &server,
        "ws_ticket_user",
        "ws-ticket@example.com",
        "WsTicketPass12345!",
    )
    .await;
    assert_eq!(register.status(), StatusCode::OK);

    let token = login_http_ok_token(&server, "ws_ticket_user", "WsTicketPass12345!").await;

    let create_room = post_json(
        &client,
        &format!("{}/api/rooms", server.api_base_url),
        create_room_request("WS Ticket Room", "ticket to websocket e2e", ""),
        Some(&token),
    )
    .await;
    assert_eq!(create_room.status(), StatusCode::OK);
    let create_room_body = response_json(create_room).await;
    let room_id = create_room_body["id"]
        .as_str()
        .expect("room id")
        .to_string();

    let create_ticket = post_json(
        &client,
        &format!("{}/api/tickets", server.api_base_url),
        create_ticket_request(&room_id),
        Some(&token),
    )
    .await;
    assert_eq!(create_ticket.status(), StatusCode::OK);
    let create_ticket_body = response_json(create_ticket).await;
    let ticket = create_ticket_body["ticket"]
        .as_str()
        .expect("ticket")
        .to_string();

    let addr = server
        .api_base_url
        .strip_prefix("http://")
        .expect("http base url should use http");

    let (mut ws, response) = ws_connect_with_ticket(addr, &room_id, &ticket)
        .await
        .expect("websocket connect with fresh ticket should succeed");
    assert_eq!(
        response.status(),
        tungstenite::http::StatusCode::SWITCHING_PROTOCOLS
    );

    let heartbeat_timestamp = chrono::Utc::now().timestamp_millis();
    send_client_message(
        &mut ws,
        synctv_proto::client::ClientMessage {
            message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
                timestamp: heartbeat_timestamp,
            })),
        },
    )
    .await;
    let ack = recv_matching_server_message(
        &mut ws,
        Duration::from_secs(5),
        |message| {
            matches!(
                &message.message,
                Some(synctv_proto::client::server_message::Message::HeartbeatAck(heartbeat_ack))
                    if heartbeat_ack.timestamp >= heartbeat_timestamp
            )
        },
        "websocket heartbeat ack after ticket-authenticated upgrade",
    )
    .await;
    assert!(
        matches!(
            ack.message,
            Some(synctv_proto::client::server_message::Message::HeartbeatAck(
                _
            ))
        ),
        "ticket-authenticated websocket should acknowledge heartbeat"
    );

    ws.close(None).await.expect("close websocket");

    let reused = ws_connect_with_ticket(addr, &room_id, &ticket).await;
    match reused {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(
                response.status(),
                tungstenite::http::StatusCode::UNAUTHORIZED
            );
        }
        other => panic!("expected reused ticket to be rejected with HTTP 401, got: {other:?}"),
    }

    // per-test isolated server
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_auth_register_login_and_get_profile() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::GetProfileRequest;

    let server = start_test_server().await;

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect auth gRPC client");
    let register = opaque_grpc_register(
        &mut auth_client,
        "grpc_user",
        "grpc-user@example.com",
        "GrpcPass12345!",
    )
    .await;
    let registered_user = register.user.expect("registered user");
    assert_eq!(registered_user.username, "grpc_user");
    assert_eq!(registered_user.email, "grpc-user@example.com");
    assert!(!register.access_token.is_empty());
    assert!(!register.refresh_token.is_empty());

    let login = opaque_grpc_login(&mut auth_client, "grpc_user", "GrpcPass12345!").await;
    assert_eq!(login.user.expect("logged in user").username, "grpc_user",);
    assert!(!login.access_token.is_empty());
    assert!(!login.refresh_token.is_empty());

    let mut profile_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect user gRPC client");
    let unauthenticated = profile_client
        .get_profile(GetProfileRequest {})
        .await
        .expect_err("missing auth should be rejected");
    assert_eq!(unauthenticated.code(), tonic::Code::Unauthenticated);

    let mut request = tonic::Request::new(GetProfileRequest {});
    request
        .metadata_mut()
        .insert("authorization", bearer_metadata(&login.access_token));
    let profile = profile_client
        .get_profile(request)
        .await
        .expect("authenticated get_profile should succeed")
        .into_inner();
    let user = profile;
    assert_eq!(user.username, "grpc_user");
    assert_eq!(user.email, "grpc-user@example.com");

    // per-test isolated server
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_create_room_requires_auth_and_returns_created_room() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::CreateRoomRequest;

    let server = start_test_server().await;

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect auth gRPC client");
    opaque_grpc_register(
        &mut auth_client,
        "grpc_room_owner",
        "grpc-room-owner@example.com",
        "GrpcRoomPass12345!",
    )
    .await;
    let login = opaque_grpc_login(&mut auth_client, "grpc_room_owner", "GrpcRoomPass12345!").await;

    let mut user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect user gRPC client");
    let unauthenticated = user_client
        .create_room(CreateRoomRequest {
            name: "gRPC room".to_string(),
            settings: None,
            description: "created through full-stack gRPC e2e".to_string(),
            password: String::new(),
            category_id: String::new(),
            label_ids: Vec::new(),
        })
        .await
        .expect_err("missing auth should be rejected");
    assert_eq!(unauthenticated.code(), tonic::Code::Unauthenticated);

    let mut request = tonic::Request::new(CreateRoomRequest {
        name: "gRPC room".to_string(),
        settings: None,
        description: "created through full-stack gRPC e2e".to_string(),
        password: String::new(),
        category_id: String::new(),
        label_ids: Vec::new(),
    });
    request
        .metadata_mut()
        .insert("authorization", bearer_metadata(&login.access_token));
    let response = user_client
        .create_room(request)
        .await
        .expect("authenticated create_room should succeed")
        .into_inner();
    let room = response;
    assert_eq!(room.name, "gRPC room");
    assert_eq!(room.description, "created through full-stack gRPC e2e");
    assert_eq!(room.created_by, login.user.expect("login user").id);
    assert_eq!(room.member_count, 1);
    let decoded_room_id = synctv_api::PublicIdCodec::plain()
        .decode_room_id(&room.id)
        .expect("created room id should be a typed public room id");
    assert!(decoded_room_id.as_i64() > 0);

    // per-test isolated server
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_room_context_flow_requires_membership_and_room_metadata() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::{CreateRoomRequest, GetRoomRequest, GetRoomSettingsRequest};

    let server = start_test_server().await;

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect auth gRPC client");

    opaque_grpc_register(
        &mut auth_client,
        "grpc_owner",
        "grpc-owner@example.com",
        "GrpcOwnerPass12345!",
    )
    .await;
    let owner_login =
        opaque_grpc_login(&mut auth_client, "grpc_owner", "GrpcOwnerPass12345!").await;

    opaque_grpc_register(
        &mut auth_client,
        "grpc_member",
        "grpc-member@example.com",
        "GrpcMemberPass12345!",
    )
    .await;
    let member_login =
        opaque_grpc_login(&mut auth_client, "grpc_member", "GrpcMemberPass12345!").await;

    let mut owner_user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect owner user gRPC client");
    let mut create_room = tonic::Request::new(CreateRoomRequest {
        name: "gRPC metadata room".to_string(),
        settings: None,
        description: "room-scoped grpc e2e".to_string(),
        password: String::new(),
        category_id: String::new(),
        label_ids: Vec::new(),
    });
    create_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&owner_login.access_token));
    let created = owner_user_client
        .create_room(create_room)
        .await
        .expect("owner should create room")
        .into_inner();
    let room = created;
    let room_id = room.id;

    let mut member_room_client = RoomServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect member room gRPC client");
    let mut owner_room_client = RoomServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect owner room gRPC client");
    opaque_grpc_set_room_password(
        &mut owner_room_client,
        &room_id,
        &owner_login.access_token,
        "GrpcRoomSecret123!",
    )
    .await;

    let mut non_member_get_room = tonic::Request::new(GetRoomSettingsRequest {});
    non_member_get_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&member_login.access_token));
    let missing_room_id = member_room_client
        .get_room_settings(non_member_get_room)
        .await
        .expect_err("room-scoped RPC without x-room-id metadata should fail");
    assert_eq!(missing_room_id.code(), tonic::Code::InvalidArgument);
    assert!(
        missing_room_id.message().contains("Missing x-room-id"),
        "unexpected missing room metadata error: {missing_room_id}"
    );

    let mut forbidden_get_room = tonic::Request::new(GetRoomRequest {
        room_id: room_id.clone(),
    });
    forbidden_get_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&member_login.access_token));
    let forbidden = owner_user_client
        .get_room(forbidden_get_room)
        .await
        .expect_err("non-member should not access room");
    assert_eq!(forbidden.code(), tonic::Code::PermissionDenied);
    assert!(
        forbidden.message().contains("Forbidden") || forbidden.message().contains("Not a member"),
        "unexpected membership denial: {forbidden}"
    );

    let joined = opaque_grpc_join_room(
        &mut owner_user_client,
        &room_id,
        &member_login.access_token,
        "GrpcRoomSecret123!",
    )
    .await;
    assert_eq!(
        joined.room.expect("joined room").id,
        room_id,
        "join_room should return the joined room"
    );
    assert_eq!(joined.members.len(), 2);

    let mut get_room = tonic::Request::new(GetRoomRequest {
        room_id: room_id.clone(),
    });
    get_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&member_login.access_token));
    let fetched = owner_user_client
        .get_room(get_room)
        .await
        .expect("joined member should access room")
        .into_inner();
    let fetched_room = fetched.room.expect("fetched room");
    assert_eq!(fetched_room.id, room_id);
    assert_eq!(fetched_room.name, "gRPC metadata room");
    assert_eq!(fetched_room.description, "room-scoped grpc e2e");

    // per-test isolated server
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_message_stream_establishes_and_acks_heartbeat() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::client_message;
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use synctv_proto::client::server_message;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::{ClientMessage, CreateRoomRequest, HeartbeatMessage};

    let server = start_test_server().await;

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect auth gRPC client");
    opaque_grpc_register(
        &mut auth_client,
        "grpc_stream_owner",
        "grpc-stream-owner@example.com",
        "GrpcStreamOwnerPass12345!",
    )
    .await;
    let owner_login = opaque_grpc_login(
        &mut auth_client,
        "grpc_stream_owner",
        "GrpcStreamOwnerPass12345!",
    )
    .await;

    opaque_grpc_register(
        &mut auth_client,
        "grpc_stream_member",
        "grpc-stream-member@example.com",
        "GrpcStreamMemberPass12345!",
    )
    .await;
    let member_login = opaque_grpc_login(
        &mut auth_client,
        "grpc_stream_member",
        "GrpcStreamMemberPass12345!",
    )
    .await;

    let mut owner_user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect owner user gRPC client");
    let mut create_room = tonic::Request::new(CreateRoomRequest {
        name: "gRPC stream room".to_string(),
        settings: None,
        description: "grpc stream e2e".to_string(),
        password: String::new(),
        category_id: String::new(),
        label_ids: Vec::new(),
    });
    create_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&owner_login.access_token));
    let room_id = owner_user_client
        .create_room(create_room)
        .await
        .expect("owner create_room should succeed")
        .into_inner()
        .id;

    let mut member_room_client = RoomServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect member room gRPC client");
    let mut owner_room_client = RoomServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect owner room gRPC client");
    opaque_grpc_set_room_password(
        &mut owner_room_client,
        &room_id,
        &owner_login.access_token,
        "GrpcStreamRoomSecret123!",
    )
    .await;
    let mut member_user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect member user gRPC client");
    opaque_grpc_join_room(
        &mut member_user_client,
        &room_id,
        &member_login.access_token,
        "GrpcStreamRoomSecret123!",
    )
    .await;

    let heartbeat_timestamp = chrono::Utc::now().timestamp_millis();
    let outbound = stream::iter(vec![ClientMessage {
        message: Some(client_message::Message::Heartbeat(HeartbeatMessage {
            timestamp: heartbeat_timestamp,
        })),
    }]);
    let mut request = tonic::Request::new(outbound);
    request
        .metadata_mut()
        .insert("authorization", bearer_metadata(&member_login.access_token));
    request
        .metadata_mut()
        .insert("x-room-id", room_id_metadata(&room_id));
    let mut inbound = member_room_client
        .message_stream(request)
        .await
        .expect("message_stream should establish")
        .into_inner();

    let ack = recv_matching_grpc_server_message(
        &mut inbound,
        Duration::from_secs(5),
        |message| {
            matches!(
                &message.message,
                Some(server_message::Message::HeartbeatAck(heartbeat_ack))
                    if heartbeat_ack.timestamp >= heartbeat_timestamp
            )
        },
        "grpc heartbeat ack",
    )
    .await;
    match ack.message {
        Some(server_message::Message::HeartbeatAck(heartbeat_ack)) => {
            assert!(
                heartbeat_ack.timestamp >= heartbeat_timestamp,
                "heartbeat ack timestamp {} should be >= sent timestamp {}",
                heartbeat_ack.timestamp,
                heartbeat_timestamp
            );
        }
        other => panic!("expected HeartbeatAck, got: {other:?}"),
    }

    // per-test isolated server
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_message_stream_watch_playback_receives_initial_and_future_updates() {
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use tokio_stream::wrappers::ReceiverStream;

    let fixture = start_room_realtime_fixture("grpc-watch-playback").await;
    let RoomRealtimeFixture {
        server,
        room_id,
        owner_username,
        member_token,
        ..
    } = fixture;

    let first_media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/grpc-watch-playback-one.mp4",
            "--room-id",
            &room_id,
            "--username",
            &owner_username,
            "--name",
            "gRPC Watch Playback One",
        ],
        "add first room media for grpc playback watch",
    )
    .await;
    let media_one_id = first_media["id"]
        .as_str()
        .expect("first media add should return id")
        .to_string();

    let second_media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/grpc-watch-playback-two.mp4",
            "--room-id",
            &room_id,
            "--username",
            &owner_username,
            "--name",
            "gRPC Watch Playback Two",
        ],
        "add second room media for grpc playback watch",
    )
    .await;
    let media_two_id = second_media["id"]
        .as_str()
        .expect("second media add should return id")
        .to_string();

    let _ = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "playback",
            "start",
            "--room-id",
            &room_id,
            "--media-id",
            &media_one_id,
        ],
        "start first playback for grpc watch",
    )
    .await;

    let mut member_room_client = RoomServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect member room gRPC client");
    let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(8);
    outbound_tx
        .send(observe_playback_message("grpc-playback"))
        .await
        .expect("queue initial playback observe request");
    let outbound = ReceiverStream::new(outbound_rx);
    let mut request = tonic::Request::new(outbound);
    request
        .metadata_mut()
        .insert("authorization", bearer_metadata(&member_token));
    request
        .metadata_mut()
        .insert("x-room-id", room_id_metadata(&room_id));
    let mut inbound = member_room_client
        .message_stream(request)
        .await
        .expect("message_stream should establish")
        .into_inner();

    let _initial_playback = recv_matching_grpc_server_message(
        &mut inbound,
        Duration::from_secs(10),
        |message| {
            resource_playback(message).is_some_and(|playback| playback.media_id == media_one_id)
        },
        "initial grpc playback",
    )
    .await;

    let _ = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "playback",
            "start",
            "--room-id",
            &room_id,
            "--media-id",
            &media_two_id,
        ],
        "start second playback for grpc watch",
    )
    .await;

    let updated_playback = recv_matching_grpc_server_message(
        &mut inbound,
        Duration::from_secs(10),
        |message| {
            resource_playback(message).is_some_and(|playback| playback.media_id == media_two_id)
        },
        "updated grpc playback",
    )
    .await;

    let playback =
        resource_playback(&updated_playback).expect("updated playback should be present");
    assert_eq!(playback.media_id, media_two_id);
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_message_stream_watch_room_settings_receives_initial_and_future_updates() {
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use tokio_stream::wrappers::ReceiverStream;

    let fixture = start_room_realtime_fixture("grpc-watch-room-settings").await;
    let RoomRealtimeFixture {
        server,
        room_id,
        member_token,
        ..
    } = fixture;

    let mut member_room_client = RoomServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect member room gRPC client");
    let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(8);
    outbound_tx
        .send(observe_room_settings_message(
            "grpc-room-settings",
            String::new(),
        ))
        .await
        .expect("queue initial room settings observe request");
    let outbound = ReceiverStream::new(outbound_rx);
    let mut request = tonic::Request::new(outbound);
    request
        .metadata_mut()
        .insert("authorization", bearer_metadata(&member_token));
    request
        .metadata_mut()
        .insert("x-room-id", room_id_metadata(&room_id));
    let mut inbound = member_room_client
        .message_stream(request)
        .await
        .expect("message_stream should establish")
        .into_inner();

    let current_settings_response = run_synctv_remote_cli_json(
        &server,
        &["room", "settings", "get", &room_id],
        "get room settings before grpc watch",
    )
    .await;
    let expected_initial_version = json_i64(&current_settings_response["version"])
        .expect("room settings get should return version");
    let expected_initial_settings = current_settings_response["settings"].clone();

    let initial_settings = recv_matching_grpc_server_message(
        &mut inbound,
        Duration::from_secs(10),
        |message| resource_room_settings(message).is_some(),
        "initial grpc room settings snapshot",
    )
    .await;
    let settings = resource_room_settings(&initial_settings)
        .expect("room settings snapshot should be present");
    let decoded = room_settings_value(settings.settings.as_ref(), "initial room settings");
    assert_eq!(decoded, expected_initial_settings);
    assert_eq!(settings.version, expected_initial_version);
    let initial_version = settings.version;

    let _ = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "settings",
            "update",
            &room_id,
            "--set",
            "chatEnabled=false",
            "--set",
            "allowGuestJoin=true",
        ],
        "update room settings for grpc watch",
    )
    .await;

    let updated_settings = recv_matching_grpc_server_message(
        &mut inbound,
        Duration::from_secs(10),
        |message| {
            resource_room_settings(message)
                .is_some_and(|settings| settings.version > initial_version)
        },
        "updated grpc room settings snapshot",
    )
    .await;

    let settings =
        resource_room_settings(&updated_settings).expect("updated settings should be present");
    let decoded = settings
        .settings
        .as_ref()
        .expect("updated room settings should be present");
    assert!(!decoded.chat_enabled);
    assert!(decoded.allow_guest_join);
    assert!(settings.version > initial_version);
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_message_stream_watch_playlist_items_receives_initial_and_future_updates() {
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use synctv_proto::client::ListPlaylistItemsRequest;
    use tokio_stream::wrappers::ReceiverStream;

    let fixture = start_room_realtime_fixture("grpc-watch-playlist-items").await;
    let RoomRealtimeFixture {
        server,
        room_id,
        owner_username,
        member_token,
        ..
    } = fixture;

    let mut member_room_client = RoomServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect member room gRPC client");

    let mut initial_list_request = tonic::Request::new(ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: None,
        pagination: page_pagination(1),
        page_size: 50,
        search: String::new(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    });
    initial_list_request
        .metadata_mut()
        .insert("authorization", bearer_metadata(&member_token));
    initial_list_request
        .metadata_mut()
        .insert("x-room-id", room_id_metadata(&room_id));
    let initial_api_snapshot = member_room_client
        .list_playlist_items(initial_list_request)
        .await
        .expect("initial playlist items request should succeed")
        .into_inner();
    assert_eq!(initial_api_snapshot.total, Some(0));
    assert!(!initial_api_snapshot.version.is_empty());

    let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(8);
    outbound_tx
        .send(observe_playlist_items_message(
            "grpc-playlist-items",
            String::new(),
            ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: None,
                pagination: page_pagination(1),
                page_size: 50,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
            },
        ))
        .await
        .expect("queue initial playlist items observe request");
    let outbound = ReceiverStream::new(outbound_rx);
    let mut request = tonic::Request::new(outbound);
    request
        .metadata_mut()
        .insert("authorization", bearer_metadata(&member_token));
    request
        .metadata_mut()
        .insert("x-room-id", room_id_metadata(&room_id));
    let mut inbound = member_room_client
        .message_stream(request)
        .await
        .expect("message_stream should establish")
        .into_inner();

    let initial_snapshot = recv_matching_grpc_server_message(
        &mut inbound,
        Duration::from_secs(10),
        |message| {
            resource_playlist_items(message)
                .is_some_and(|snapshot| snapshot.total == Some(0) && !snapshot.version.is_empty())
        },
        "initial grpc playlist items snapshot",
    )
    .await;
    let snapshot = resource_playlist_items(&initial_snapshot)
        .expect("playlist items snapshot should be present");
    assert_eq!(snapshot.version, initial_api_snapshot.version);
    assert_eq!(snapshot.total, Some(0));
    let initial_version = snapshot.version.clone();

    let added_media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/grpc-watch-playlist-items-one.mp4",
            "--room-id",
            &room_id,
            "--username",
            &owner_username,
            "--name",
            "gRPC Watch Playlist Items One",
        ],
        "add first room media for grpc playlist items watch",
    )
    .await;
    let media_id = added_media["id"]
        .as_str()
        .expect("media add should return id")
        .to_string();

    let updated_snapshot = recv_matching_grpc_server_message(
        &mut inbound,
        Duration::from_secs(10),
        |message| {
            resource_playlist_items(message).is_some_and(|snapshot| {
                snapshot.media.iter().any(|media| media.id == media_id)
                    && snapshot.total == Some(1)
                    && !snapshot.version.is_empty()
            })
        },
        "updated grpc playlist items snapshot",
    )
    .await;

    let snapshot =
        resource_playlist_items(&updated_snapshot).expect("updated snapshot should be present");
    assert_eq!(snapshot.total, Some(1));
    assert!(snapshot.media.iter().any(|media| media.id == media_id));
    assert_ne!(
        snapshot.version, initial_version,
        "playlist-items version should change after media is added"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_message_stream_watch_room_members_receives_initial_and_future_updates() {
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use synctv_proto::client::GetRoomMembersRequest;
    use tokio_stream::wrappers::ReceiverStream;

    let fixture = start_room_realtime_fixture("grpc-watch-room-members").await;
    let RoomRealtimeFixture {
        server,
        room_id,
        member_user_id,
        member_token,
        ..
    } = fixture;

    let mut member_room_client = RoomServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect member room gRPC client");
    let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(8);
    outbound_tx
        .send(observe_room_members_message(
            "grpc-room-members",
            String::new(),
            synctv_proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
                sort_by: synctv_proto::client::RoomMemberListSortBy::JoinedAt as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
            },
        ))
        .await
        .expect("queue initial room members observe request");
    let outbound = ReceiverStream::new(outbound_rx);
    let mut request = tonic::Request::new(outbound);
    request
        .metadata_mut()
        .insert("authorization", bearer_metadata(&member_token));
    request
        .metadata_mut()
        .insert("x-room-id", room_id_metadata(&room_id));
    let mut inbound = member_room_client
        .message_stream(request)
        .await
        .expect("message_stream should establish")
        .into_inner();

    let _initial_observed = recv_matching_grpc_server_message(
        &mut inbound,
        Duration::from_secs(10),
        |message| {
            matches!(
                &message.message,
                Some(synctv_proto::client::server_message::Message::ResourceObserved(_))
            )
        },
        "initial grpc room member events observation",
    )
    .await;
    let mut get_members_request = tonic::Request::new(GetRoomMembersRequest {
        page: 1,
        page_size: 20,
        search: String::new(),
        role: None,
        sort_by: synctv_proto::client::RoomMemberListSortBy::JoinedAt as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
    });
    get_members_request
        .metadata_mut()
        .insert("authorization", bearer_metadata(&member_token));
    get_members_request
        .metadata_mut()
        .insert("x-room-id", room_id_metadata(&room_id));
    let member_snapshot = member_room_client
        .get_room_members(get_members_request)
        .await
        .expect("get room members snapshot should succeed")
        .into_inner();
    assert!(
        member_snapshot
            .members
            .iter()
            .any(|member| member.user_id == member_user_id),
        "room members snapshot should include the joined member"
    );

    let suffix = unique_test_suffix();
    let joiner_username = bounded_test_username("grpc-watch-room-members", "joiner", &suffix);
    let joiner_email = format!("grpc-watch-room-members-joiner-{suffix}@example.com");
    let joiner_password = "GrpcWatchMembersJoin123!";
    let created_joiner = create_cli_user(
        &server,
        &joiner_username,
        &joiner_email,
        joiner_password,
        Some("active"),
        "create grpc room-members joiner",
    )
    .await;
    let joiner_id = created_joiner["id"]
        .as_str()
        .expect("joiner create should return user id")
        .to_string();
    let joiner_token = login_http_ok_token(&server, &joiner_username, joiner_password).await;
    let join_response = join_room_http(&server, &room_id, "", &joiner_token).await;
    assert_eq!(
        join_response.status(),
        StatusCode::OK,
        "joiner should join room for grpc room-members watch"
    );

    let joined_event = recv_matching_grpc_server_message(
        &mut inbound,
        Duration::from_secs(10),
        |message| {
            resource_room_member_joined(message).is_some_and(|event| {
                event
                    .member
                    .as_ref()
                    .is_some_and(|member| member.user_id == joiner_id)
            })
        },
        "grpc room member joined event",
    )
    .await;

    let joined =
        resource_room_member_joined(&joined_event).expect("joined event should be present");
    assert!(
        joined
            .member
            .as_ref()
            .is_some_and(|member| member.user_id == joiner_id),
        "room member joined event should include the new joiner"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_message_stream_requires_join_room_first() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use synctv_proto::client::ClientMessage;

    let server = start_test_server().await;

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect auth gRPC client");
    opaque_grpc_register(
        &mut auth_client,
        "grpc_stream_missing_join",
        "grpc-stream-missing-join@example.com",
        "GrpcStreamMissingJoinPass12345!",
    )
    .await;
    let login = opaque_grpc_login(
        &mut auth_client,
        "grpc_stream_missing_join",
        "GrpcStreamMissingJoinPass12345!",
    )
    .await;

    let mut room_client = RoomServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect room gRPC client");
    let outbound = stream::iter(vec![ClientMessage::default()]);
    let mut request = tonic::Request::new(outbound);
    request
        .metadata_mut()
        .insert("authorization", bearer_metadata(&login.access_token));

    let error = room_client
        .message_stream(request)
        .await
        .expect_err("message_stream without room metadata should fail");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(
        error.message().contains("Missing x-room-id"),
        "unexpected error message: {error}"
    );

    // per-test isolated server
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_websocket_room_messages_cover_chat_playback_media_settings_and_permissions() {
    use synctv_proto::client::client_message;
    use synctv_proto::client::server_message;
    use synctv_proto::client::{
        ChatMessageSend, ClientMessage, PlaybackUpdateType, UpdatePlaybackStateRequest,
    };

    let fixture = start_room_realtime_fixture("ws-room-events").await;
    let RoomRealtimeFixture {
        server,
        api_addr,
        room_id,
        owner_username,
        owner_token,
        member_username,
        member_user_id,
        member_token,
    } = fixture;
    let mut owner_ws = ws_connect(&api_addr, &room_id, &owner_token).await;
    let mut member_ws = ws_connect(&api_addr, &room_id, &member_token).await;
    let _ = drain_until_quiet(&mut owner_ws, 250).await;
    let _ = drain_until_quiet(&mut member_ws, 250).await;
    send_client_message(&mut member_ws, observe_chat_events_message("chat-events")).await;
    recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            matches!(
                &message.message,
                Some(server_message::Message::ResourceObserved(_))
            )
        },
        "chat_events observation acknowledgement",
    )
    .await;
    send_client_message(
        &mut member_ws,
        observe_playlist_items_message(
            "ws-room-playlist-items",
            String::new(),
            synctv_proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                pagination: page_pagination(1),
                page_size: 100,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
                target: None,
            },
        ),
    )
    .await;
    send_client_message(
        &mut member_ws,
        observe_room_settings_message("ws-room-settings", String::new()),
    )
    .await;
    send_client_message(
        &mut member_ws,
        observe_room_members_message(
            "ws-room-member-events",
            String::new(),
            synctv_proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
                sort_by: synctv_proto::client::RoomMemberListSortBy::JoinedAt as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
            },
        ),
    )
    .await;
    send_client_message(
        &mut owner_ws,
        observe_room_members_message(
            "ws-owner-room-member-events",
            String::new(),
            synctv_proto::client::GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
                sort_by: synctv_proto::client::RoomMemberListSortBy::JoinedAt as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
            },
        ),
    )
    .await;
    send_client_message(
        &mut member_ws,
        observe_playback_state_message("ws-playback-state"),
    )
    .await;
    let _ = drain_until_quiet(&mut member_ws, 250).await;
    let _ = drain_until_quiet(&mut owner_ws, 250).await;

    let observer_suffix = unique_test_suffix();
    let observer_username = format!("ws_room_observer_{observer_suffix}");
    let observer_email = format!("ws-room-observer-{observer_suffix}@example.com");
    let observer_password = "RealtimeObserverPass12345!";
    let created_observer = create_cli_user(
        &server,
        &observer_username,
        &observer_email,
        observer_password,
        Some("active"),
        "create realtime observer",
    )
    .await;
    let observer_user_id = created_observer["id"]
        .as_str()
        .expect("observer create should return user id")
        .to_string();
    let observer_token = login_http_ok_token(&server, &observer_username, observer_password).await;
    let observer_join = join_room_http(&server, &room_id, "", &observer_token).await;
    assert_eq!(
        observer_join.status(),
        StatusCode::OK,
        "observer should join room"
    );
    let mut observer_ws = ws_connect(&api_addr, &room_id, &observer_token).await;
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_room_member_joined(message).is_some_and(|joined| {
                joined.room_id == room_id
                    && joined.member.as_ref().is_some_and(|member| {
                        member.user_id == observer_user_id && member.username == observer_username
                    })
            })
        },
        "user joined broadcast",
    )
    .await;
    observer_ws
        .close(None)
        .await
        .expect("observer websocket close should succeed");
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_room_member_left(message)
                .is_some_and(|left| left.room_id == room_id && left.user_id == observer_user_id)
        },
        "user left broadcast",
    )
    .await;

    let chat_content = "full stack websocket chat";
    send_client_message(
        &mut owner_ws,
        ClientMessage {
            message: Some(client_message::Message::Chat(ChatMessageSend {
                content: chat_content.to_string(),
                display_position: String::new(),
                display_color: String::new(),
                client_message_id: String::new(),
                attachments: Vec::new(),
                reply_to_message_id: String::new(),
                metadata: None,
                mentions: Vec::new(),
            })),
        },
    )
    .await;
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_chat_event(message).is_some_and(|event| {
                event
                    .message
                    .as_ref()
                    .is_some_and(|chat| chat.content == chat_content)
            })
        },
        "chat_events websocket resource update",
    )
    .await;

    let second_chat_content = "full stack websocket chat follow-up";
    send_client_message(
        &mut owner_ws,
        ClientMessage {
            message: Some(client_message::Message::Chat(ChatMessageSend {
                content: second_chat_content.to_string(),
                display_position: String::new(),
                display_color: String::new(),
                client_message_id: String::new(),
                attachments: Vec::new(),
                reply_to_message_id: String::new(),
                metadata: None,
                mentions: Vec::new(),
            })),
        },
    )
    .await;
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_chat_event(message).is_some_and(|event| {
                event
                    .message
                    .as_ref()
                    .is_some_and(|chat| chat.content == second_chat_content)
            })
        },
        "second chat_events websocket resource update",
    )
    .await;

    let first_media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/ws-room-media-one.mp4",
            "--room-id",
            &room_id,
            "--username",
            &owner_username,
            "--name",
            "WS Room Media One",
        ],
        "add first room media for websocket test",
    )
    .await;
    let media_one_id = first_media["id"]
        .as_str()
        .expect("first media add should return id")
        .to_string();
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playlist_items(message).is_some_and(|snapshot| {
                snapshot
                    .media
                    .iter()
                    .any(|media| media.id == media_one_id && media.name == "WS Room Media One")
            })
        },
        "first media added broadcast",
    )
    .await;

    let second_media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/ws-room-media-two.mp4",
            "--room-id",
            &room_id,
            "--username",
            &owner_username,
            "--name",
            "WS Room Media Two",
        ],
        "add second room media for websocket test",
    )
    .await;
    let media_two_id = second_media["id"]
        .as_str()
        .expect("second media add should return id")
        .to_string();
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playlist_items(message).is_some_and(|snapshot| {
                snapshot
                    .media
                    .iter()
                    .any(|media| media.id == media_two_id && media.name == "WS Room Media Two")
            })
        },
        "second media added broadcast",
    )
    .await;

    let _ = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "update",
            "--room-id",
            &room_id,
            &media_two_id,
            "--name",
            "WS Room Media Two Renamed",
        ],
        "rename room media for websocket test",
    )
    .await;
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playlist_items(message).is_some_and(|snapshot| {
                snapshot.media.iter().any(|media| {
                    media.id == media_two_id && media.name == "WS Room Media Two Renamed"
                })
            })
        },
        "media updated broadcast",
    )
    .await;

    let _ = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "move",
            "--room-id",
            &room_id,
            "--media-id",
            &media_two_id,
            "--media-id",
            &media_one_id,
        ],
        "reorder room media for websocket test",
    )
    .await;
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playlist_items(message).is_some_and(|snapshot| {
                snapshot
                    .media
                    .iter()
                    .map(|media| media.id.clone())
                    .collect::<Vec<_>>()
                    == vec![media_two_id.clone(), media_one_id.clone()]
            })
        },
        "playlist reordered broadcast",
    )
    .await;

    let _ = run_synctv_remote_cli_json(
        &server,
        &["media", "delete", "--room-id", &room_id, &media_two_id],
        "delete room media for websocket test",
    )
    .await;
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playlist_items(message)
                .is_some_and(|snapshot| snapshot.media.iter().all(|media| media.id != media_two_id))
        },
        "media removed broadcast",
    )
    .await;

    let _ = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "settings",
            "update",
            &room_id,
            "--set",
            "chatEnabled=false",
            "--set",
            "allowGuestJoin=true",
        ],
        "update room settings for websocket test",
    )
    .await;
    let room_settings_message = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_room_settings(message).is_some_and(|settings| {
                settings
                    .settings
                    .as_ref()
                    .is_some_and(|settings| !settings.chat_enabled && settings.allow_guest_join)
            })
        },
        "room settings broadcast",
    )
    .await;
    let settings =
        resource_room_settings(&room_settings_message).expect("room settings payload expected");
    let decoded = settings
        .settings
        .as_ref()
        .expect("room settings payload should be present");
    assert!(!decoded.chat_enabled);
    assert!(decoded.allow_guest_join);
    assert!(
        settings.version > 0,
        "room settings change should carry version"
    );

    let permission_bits = synctv_core::models::RoomAdminPermissionBits::LIVE_CONTROL
        | synctv_core::models::RoomAdminPermissionBits::PLAY_CONTROL;
    let _ = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "member",
            "set-permissions",
            "--room-id",
            &room_id,
            &member_username,
            "--role",
            "admin",
            "--admin-added-permissions",
            &permission_bits.to_string(),
        ],
        "update member permissions for websocket test",
    )
    .await;
    let _ = recv_matching_server_message(
        &mut owner_ws,
        Duration::from_secs(10),
        |message| {
            resource_room_member_event(message).is_some_and(|event| {
                event.kind == synctv_proto::client::RoomMemberEventKind::PermissionChanged as i32
                    && event.member.as_ref().is_some_and(|member| {
                        member.user_id == member_user_id
                            && member.role == synctv_proto::common::RoomMemberRole::Admin as i32
                            && member.admin_added_permissions == permission_bits
                    })
            })
        },
        "permission changed broadcast",
    )
    .await;

    let _ = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "playback",
            "start",
            "--room-id",
            &room_id,
            "--media-id",
            &media_one_id,
        ],
        "start playback for websocket test",
    )
    .await;
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playback_state(message)
                .is_some_and(|state| state.is_playing && state.playing_media_id == media_one_id)
        },
        "playback start broadcast",
    )
    .await;

    send_client_message(
        &mut owner_ws,
        ClientMessage {
            message: Some(client_message::Message::PlaybackStateUpdate(
                UpdatePlaybackStateRequest {
                    r#type: PlaybackUpdateType::Pause as i32,
                    playing: None,
                    position: None,
                    speed: None,
                    version: None,
                    expected_media_id: None,
                    expected_playlist_id: None,
                    expected_target_hash: None,
                },
            )),
        },
    )
    .await;
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playback_state(message)
                .is_some_and(|state| !state.is_playing && state.playing_media_id == media_one_id)
        },
        "pause playback broadcast",
    )
    .await;

    send_client_message(
        &mut owner_ws,
        ClientMessage {
            message: Some(client_message::Message::PlaybackStateUpdate(
                UpdatePlaybackStateRequest {
                    r#type: PlaybackUpdateType::Seek as i32,
                    playing: Some(false),
                    position: Some(17.5),
                    speed: None,
                    version: None,
                    expected_media_id: Some(media_one_id.clone()),
                    expected_playlist_id: Some(String::new()),
                    expected_target_hash: Some(
                        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                            .to_string(),
                    ),
                },
            )),
        },
    )
    .await;
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playback_state(message).is_some_and(|state| {
                !state.is_playing
                    && state.playing_media_id == media_one_id
                    && (state.position - 17.5).abs() < 0.01
            })
        },
        "seek playback broadcast",
    )
    .await;

    send_client_message(
        &mut owner_ws,
        ClientMessage {
            message: Some(client_message::Message::PlaybackStateUpdate(
                UpdatePlaybackStateRequest {
                    r#type: PlaybackUpdateType::Speed as i32,
                    playing: Some(false),
                    position: None,
                    speed: Some(1.5),
                    version: None,
                    expected_media_id: None,
                    expected_playlist_id: None,
                    expected_target_hash: None,
                },
            )),
        },
    )
    .await;
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playback_state(message).is_some_and(|state| {
                !state.is_playing
                    && state.playing_media_id == media_one_id
                    && (state.speed - 1.5).abs() < f64::EPSILON
            })
        },
        "set playback speed broadcast",
    )
    .await;

    send_client_message(
        &mut owner_ws,
        ClientMessage {
            message: Some(client_message::Message::PlaybackStateUpdate(
                UpdatePlaybackStateRequest {
                    r#type: PlaybackUpdateType::Play as i32,
                    playing: None,
                    position: None,
                    speed: None,
                    version: None,
                    expected_media_id: None,
                    expected_playlist_id: None,
                    expected_target_hash: None,
                },
            )),
        },
    )
    .await;
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playback_state(message).is_some_and(|state| {
                state.is_playing
                    && state.playing_media_id == media_one_id
                    && (state.speed - 1.5).abs() < f64::EPSILON
            })
        },
        "resume playback broadcast",
    )
    .await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_websocket_room_messages_include_playlist_lifecycle_events() {
    let fixture = start_room_realtime_fixture("ws-playlist-events").await;
    let RoomRealtimeFixture {
        server,
        api_addr,
        room_id,
        owner_username,
        owner_token: _,
        member_username: _,
        member_user_id: _,
        member_token,
    } = fixture;

    let mut member_ws = ws_connect(&api_addr, &room_id, &member_token).await;
    let _ = drain_until_quiet(&mut member_ws, 250).await;
    send_client_message(
        &mut member_ws,
        observe_playlist_items_message(
            "ws-playlist-lifecycle-items",
            String::new(),
            synctv_proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                pagination: page_pagination(1),
                page_size: 100,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
                target: None,
            },
        ),
    )
    .await;
    let _ = drain_until_quiet(&mut member_ws, 250).await;

    let created_playlist = run_synctv_remote_cli_json(
        &server,
        &[
            "playlist",
            "create",
            "--room-id",
            &room_id,
            "--username",
            &owner_username,
            "Realtime Playlist",
        ],
        "create realtime playlist",
    )
    .await;
    let playlist_id = created_playlist["id"]
        .as_str()
        .expect("playlist create should return playlist id")
        .to_string();

    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playlist_items(message).is_some_and(|snapshot| {
                snapshot
                    .playlists
                    .iter()
                    .any(|entry| entry.id == playlist_id && entry.name == "Realtime Playlist")
            })
        },
        "playlist created broadcast",
    )
    .await;

    let _ = run_synctv_remote_cli_json(
        &server,
        &[
            "playlist",
            "update",
            "--room-id",
            &room_id,
            &playlist_id,
            "--name",
            "Realtime Playlist Renamed",
        ],
        "rename realtime playlist",
    )
    .await;
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playlist_items(message).is_some_and(|snapshot| {
                snapshot.playlists.iter().any(|entry| {
                    entry.id == playlist_id && entry.name == "Realtime Playlist Renamed"
                })
            })
        },
        "playlist updated broadcast",
    )
    .await;

    let _ = run_synctv_remote_cli_json(
        &server,
        &["playlist", "delete", "--room-id", &room_id, &playlist_id],
        "delete realtime playlist",
    )
    .await;
    let _ = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playlist_items(message).is_some_and(|snapshot| {
                snapshot
                    .playlists
                    .iter()
                    .all(|entry| entry.id != playlist_id)
            })
        },
        "playlist deleted broadcast",
    )
    .await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_websocket_watch_playback_receives_initial_and_future_updates() {
    let fixture = start_room_realtime_fixture("ws-watch-playback").await;
    let RoomRealtimeFixture {
        server,
        api_addr,
        room_id,
        owner_username,
        member_token,
        ..
    } = fixture;

    let first_media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/ws-watch-playback-one.mp4",
            "--room-id",
            &room_id,
            "--username",
            &owner_username,
            "--name",
            "WS Watch Playback One",
        ],
        "add first room media for websocket playback watch",
    )
    .await;
    let media_one_id = first_media["id"]
        .as_str()
        .expect("first media add should return id")
        .to_string();

    let second_media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/ws-watch-playback-two.mp4",
            "--room-id",
            &room_id,
            "--username",
            &owner_username,
            "--name",
            "WS Watch Playback Two",
        ],
        "add second room media for websocket playback watch",
    )
    .await;
    let media_two_id = second_media["id"]
        .as_str()
        .expect("second media add should return id")
        .to_string();

    let _ = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "playback",
            "start",
            "--room-id",
            &room_id,
            "--media-id",
            &media_one_id,
        ],
        "start first playback for websocket watch",
    )
    .await;

    let mut member_ws = ws_connect(&api_addr, &room_id, &member_token).await;
    send_client_message(&mut member_ws, observe_playback_message("ws-playback")).await;

    let _initial_playback = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playback(message).is_some_and(|playback| playback.media_id == media_one_id)
        },
        "initial playback",
    )
    .await;

    let _ = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "playback",
            "start",
            "--room-id",
            &room_id,
            "--media-id",
            &media_two_id,
        ],
        "start second playback for websocket watch",
    )
    .await;

    let updated_playback = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playback(message).is_some_and(|playback| playback.media_id == media_two_id)
        },
        "updated playback",
    )
    .await;

    let playback =
        resource_playback(&updated_playback).expect("updated playback should be present");
    assert_eq!(playback.media_id, media_two_id);
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_websocket_watch_room_settings_receives_initial_and_future_updates() {
    let fixture = start_room_realtime_fixture("ws-watch-room-settings").await;
    let RoomRealtimeFixture {
        server,
        api_addr,
        room_id,
        member_token,
        ..
    } = fixture;

    let mut member_ws = ws_connect(&api_addr, &room_id, &member_token).await;

    send_client_message(
        &mut member_ws,
        observe_room_settings_message("ws-room-settings", String::new()),
    )
    .await;

    let current_settings_response = run_synctv_remote_cli_json(
        &server,
        &["room", "settings", "get", &room_id],
        "get room settings before websocket watch",
    )
    .await;
    let expected_initial_version = json_i64(&current_settings_response["version"])
        .expect("room settings get should return version");
    let expected_initial_settings = current_settings_response["settings"].clone();

    let initial_settings = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| resource_room_settings(message).is_some(),
        "initial websocket room settings snapshot",
    )
    .await;
    let settings = resource_room_settings(&initial_settings)
        .expect("room settings snapshot should be present");
    let decoded = room_settings_value(settings.settings.as_ref(), "initial room settings");
    assert_eq!(decoded, expected_initial_settings);
    assert_eq!(settings.version, expected_initial_version);
    let initial_version = settings.version;

    let _ = run_synctv_remote_cli_json(
        &server,
        &[
            "room",
            "settings",
            "update",
            &room_id,
            "--set",
            "chatEnabled=false",
            "--set",
            "allowGuestJoin=true",
        ],
        "update room settings for websocket watch",
    )
    .await;

    let updated_settings = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_room_settings(message)
                .is_some_and(|settings| settings.version > initial_version)
        },
        "updated websocket room settings snapshot",
    )
    .await;

    let settings =
        resource_room_settings(&updated_settings).expect("updated settings should be present");
    let decoded = settings
        .settings
        .as_ref()
        .expect("updated room settings should be present");
    assert!(!decoded.chat_enabled);
    assert!(decoded.allow_guest_join);
    assert!(settings.version > initial_version);
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_websocket_watch_playlist_items_receives_initial_and_future_updates() {
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use synctv_proto::client::ListPlaylistItemsRequest;

    let fixture = start_room_realtime_fixture("ws-watch-playlist-items").await;
    let RoomRealtimeFixture {
        server,
        api_addr,
        room_id,
        owner_username,
        member_token,
        ..
    } = fixture;

    let mut member_ws = ws_connect(&api_addr, &room_id, &member_token).await;

    let mut member_room_client = RoomServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect member room gRPC client");
    let mut initial_list_request = tonic::Request::new(ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: None,
        pagination: page_pagination(1),
        page_size: 50,
        search: String::new(),
        source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
        refresh: false,
    });
    initial_list_request
        .metadata_mut()
        .insert("authorization", bearer_metadata(&member_token));
    initial_list_request
        .metadata_mut()
        .insert("x-room-id", room_id_metadata(&room_id));
    let initial_api_snapshot = member_room_client
        .list_playlist_items(initial_list_request)
        .await
        .expect("initial playlist items request should succeed")
        .into_inner();
    let expected_initial_version = initial_api_snapshot.version.clone();
    assert_eq!(
        initial_api_snapshot.total,
        Some(0),
        "new room should start with an empty playlist-items snapshot"
    );

    send_client_message(
        &mut member_ws,
        observe_playlist_items_message(
            "ws-playlist-items",
            String::new(),
            ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: None,
                pagination: page_pagination(1),
                page_size: 50,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
            },
        ),
    )
    .await;

    let initial_snapshot = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playlist_items(message)
                .is_some_and(|snapshot| snapshot.total == Some(0) && !snapshot.version.is_empty())
        },
        "initial websocket playlist items snapshot",
    )
    .await;
    let snapshot = resource_playlist_items(&initial_snapshot)
        .expect("playlist items snapshot should be present");
    assert_eq!(snapshot.version, expected_initial_version);
    assert_eq!(snapshot.total, Some(0));
    let initial_version = snapshot.version.clone();

    let added_media = run_synctv_remote_cli_json(
        &server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/ws-watch-playlist-items-one.mp4",
            "--room-id",
            &room_id,
            "--username",
            &owner_username,
            "--name",
            "WS Watch Playlist Items One",
        ],
        "add first room media for websocket playlist items watch",
    )
    .await;
    let media_id = added_media["id"]
        .as_str()
        .expect("media add should return id")
        .to_string();

    let updated_snapshot = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_playlist_items(message).is_some_and(|snapshot| {
                snapshot.media.iter().any(|media| media.id == media_id)
                    && snapshot.total == Some(1)
                    && !snapshot.version.is_empty()
            })
        },
        "updated websocket playlist items snapshot",
    )
    .await;

    let snapshot =
        resource_playlist_items(&updated_snapshot).expect("updated snapshot should be present");
    assert_eq!(snapshot.total, Some(1));
    assert!(snapshot.media.iter().any(|media| media.id == media_id));
    assert_ne!(
        snapshot.version, initial_version,
        "playlist-items version should change after media is added"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_websocket_watch_room_members_receives_initial_and_future_updates() {
    use synctv_proto::client::GetRoomMembersRequest;

    let fixture = start_room_realtime_fixture("ws-watch-room-members").await;
    let RoomRealtimeFixture {
        server,
        api_addr,
        room_id,
        member_user_id,
        member_token,
        ..
    } = fixture;

    let client = reqwest::Client::new();
    let mut member_ws = ws_connect(&api_addr, &room_id, &member_token).await;
    send_client_message(
        &mut member_ws,
        observe_room_members_message(
            "ws-room-members",
            String::new(),
            GetRoomMembersRequest {
                page: 1,
                page_size: 20,
                search: String::new(),
                role: None,
                sort_by: synctv_proto::client::RoomMemberListSortBy::JoinedAt as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
            },
        ),
    )
    .await;

    let _initial_observed = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            matches!(
                &message.message,
                Some(synctv_proto::client::server_message::Message::ResourceObserved(_))
            )
        },
        "initial websocket room member events observation",
    )
    .await;
    let members_response = client
        .get(format!(
            "{}/api/rooms/{}/members",
            server.api_base_url, room_id
        ))
        .bearer_auth(&member_token)
        .send()
        .await
        .expect("get room members snapshot should succeed");
    assert_eq!(members_response.status(), StatusCode::OK);
    let members_body = response_json(members_response).await;
    assert!(
        members_body["members"]
            .as_array()
            .expect("members should be an array")
            .iter()
            .any(|member| member["userId"].as_str() == Some(member_user_id.as_str())),
        "room members snapshot should include the joined member"
    );

    let suffix = unique_test_suffix();
    let joiner_username = bounded_test_username("ws-watch-room-members", "joiner", &suffix);
    let joiner_email = format!("ws-watch-room-members-joiner-{suffix}@example.com");
    let joiner_password = "WsWatchMembersJoin123!";
    let created_joiner = create_cli_user(
        &server,
        &joiner_username,
        &joiner_email,
        joiner_password,
        Some("active"),
        "create websocket room-members joiner",
    )
    .await;
    let joiner_id = created_joiner["id"]
        .as_str()
        .expect("joiner create should return user id")
        .to_string();
    let joiner_token = login_http_ok_token(&server, &joiner_username, joiner_password).await;
    let join_response = join_room_http(&server, &room_id, "", &joiner_token).await;
    assert_eq!(
        join_response.status(),
        StatusCode::OK,
        "joiner should join room for websocket room-members watch"
    );

    let joined_event = recv_matching_server_message(
        &mut member_ws,
        Duration::from_secs(10),
        |message| {
            resource_room_member_joined(message).is_some_and(|event| {
                event
                    .member
                    .as_ref()
                    .is_some_and(|member| member.user_id == joiner_id)
            })
        },
        "websocket room member joined event",
    )
    .await;

    let joined =
        resource_room_member_joined(&joined_event).expect("joined event should be present");
    assert!(
        joined
            .member
            .as_ref()
            .is_some_and(|member| member.user_id == joiner_id),
        "room member joined event should include the new joiner"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_message_stream_requires_membership() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::{ClientMessage, CreateRoomRequest};

    let server = start_test_server().await;

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect auth gRPC client");

    opaque_grpc_register(
        &mut auth_client,
        "grpc_stream_owner_only",
        "grpc-stream-owner-only@example.com",
        "GrpcStreamOwnerOnlyPass12345!",
    )
    .await;
    let owner_login = opaque_grpc_login(
        &mut auth_client,
        "grpc_stream_owner_only",
        "GrpcStreamOwnerOnlyPass12345!",
    )
    .await;

    opaque_grpc_register(
        &mut auth_client,
        "grpc_stream_outsider",
        "grpc-stream-outsider@example.com",
        "GrpcStreamOutsiderPass12345!",
    )
    .await;
    let outsider_login = opaque_grpc_login(
        &mut auth_client,
        "grpc_stream_outsider",
        "GrpcStreamOutsiderPass12345!",
    )
    .await;

    let mut owner_user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect owner user gRPC client");
    let mut create_room = tonic::Request::new(CreateRoomRequest {
        name: "gRPC outsider stream room".to_string(),
        settings: None,
        description: "membership denial for grpc stream".to_string(),
        password: String::new(),
        category_id: String::new(),
        label_ids: Vec::new(),
    });
    create_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&owner_login.access_token));
    let room_id = owner_user_client
        .create_room(create_room)
        .await
        .expect("owner create_room should succeed")
        .into_inner()
        .id;

    let mut outsider_room_client = RoomServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect outsider room gRPC client");

    // Try to establish message_stream with room metadata without being a member
    let outbound = stream::iter(vec![ClientMessage::default()]);
    let mut request = tonic::Request::new(outbound);
    request.metadata_mut().insert(
        "authorization",
        bearer_metadata(&outsider_login.access_token),
    );
    request
        .metadata_mut()
        .insert("x-room-id", room_id_metadata(&room_id));

    let error = outsider_room_client
        .message_stream(request)
        .await
        .expect_err("non-member message_stream should fail");
    assert_eq!(error.code(), tonic::Code::PermissionDenied);
    assert!(
        error.message().contains("Not a member") || error.message().contains("Forbidden"),
        "unexpected membership denial: {error}"
    );

    // per-test isolated server
}
