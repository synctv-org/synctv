#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::net::{SocketAddr, TcpListener};
use std::process::Command as StdCommand;
use std::sync::Arc;
use std::sync::{LazyLock, Once};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{stream, StreamExt};
use prost::Message;
use reqwest::StatusCode;
use serde_json::{json, Value};
use synctv::app::{Application, ApplicationBuildOptions};
use synctv_core::config::{default_management_unix_socket_path, Config};
use synctv_core_testing::{
    create_test_database_url_with_label, start_redis_url_with_label, test_redis_key_prefix,
    RedisContainer, TestContainer,
};
use synctv_management::proto as management_proto;
use synctv_media_providers::grpc::alist::{alist_server::AlistServer, MeResp as AlistMeResp};
use synctv_proto::client::{server_message, ServerMessage};
use tokio::runtime::Runtime;
use tokio::sync::OnceCell;
use tokio_tungstenite::tungstenite;
use tonic::metadata::MetadataValue;
use tonic::transport::Server;
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::health_client::HealthClient;
use tonic_health::pb::HealthCheckRequest;

const PROVIDER_PROBE_HOST: &str = "provider-test.example.com";
const PROVIDER_PROBE_SECRET: &str = "provider-remote-e2e-secret";
const TEST_CREDENTIAL_ENCRYPTION_KEY: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

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
    config.management.transport = synctv_core::config::ManagementTransport::Tcp;
    config.management.port = management_port;
    config.management.enable_reflection = false;
    config.database.url = database_url;
    config.redis.url = redis_url;
    config.redis.key_prefix = test_redis_key_prefix("full-stack");
    config.jwt.secret = "test-jwt-secret-key-for-full-stack-e2e-123456".to_string();
    config.bootstrap.create_root_user = true;
    config.bootstrap.root_username = "admin".to_string();
    config.bootstrap.root_password = "StrongPwd12345!".to_string();
    config.livestream.rtmp_port = rtmp_port;
    config.webrtc.enable_builtin_stun = false;

    // Raise rate limits well above defaults for avoid cross-test interference
    // when 20 tests share the same Redis + server.
    config.grpc_rate_limits.auth_max_requests = 10_000;
    config.grpc_rate_limits.auth_window_seconds = 1;
    config.grpc_rate_limits.email_max_requests = 5_000;
    config.grpc_rate_limits.email_window_seconds = 1;
    config.grpc_rate_limits.media_max_requests = 5_000;
    config.grpc_rate_limits.media_window_seconds = 1;
    config.grpc_rate_limits.write_max_requests = 5_000;
    config.grpc_rate_limits.write_window_seconds = 1;
    config.grpc_rate_limits.admin_max_requests = 5_000;
    config.grpc_rate_limits.admin_window_seconds = 1;
    config.grpc_rate_limits.read_max_requests = 5_000;
    config.grpc_rate_limits.read_window_seconds = 1;
    config.http_rate_limits.auth_max_requests = 5_000;
    config.http_rate_limits.auth_window_seconds = 1;
    config.http_rate_limits.write_max_requests = 5_000;
    config.http_rate_limits.write_window_seconds = 1;
    config.http_rate_limits.read_max_requests = 5_000;
    config.http_rate_limits.read_window_seconds = 1;
    config.http_rate_limits.media_max_requests = 5_000;
    config.http_rate_limits.media_window_seconds = 1;
    config.http_rate_limits.admin_max_requests = 5_000;
    config.http_rate_limits.admin_window_seconds = 1;
    config.http_rate_limits.streaming_max_requests = 5_000;
    config.http_rate_limits.streaming_window_seconds = 1;
    config.http_rate_limits.websocket_max_requests = 5_000;
    config.http_rate_limits.websocket_window_seconds = 1;
    config
}

// ---------------------------------------------------------------------------
// Shared server: all full_stack tests reuse ONE Application instance.
// The server starts on first access and runs until the process exits.
// ---------------------------------------------------------------------------

/// Dedicated tokio runtime for the shared server.
/// Created synchronously via `LazyLock`, lives for the process lifetime.
/// The server runs here independently of individual `#[tokio::test]` runtimes.
static DEDICATED_RT: LazyLock<Runtime> =
    LazyLock::new(|| Runtime::new().expect("shared test server runtime"));
static TEST_LOGGING: Once = Once::new();

fn ensure_test_logging() {
    TEST_LOGGING.call_once(|| {
        let mut logging = synctv_core::config::LoggingConfig::default();
        logging.level = "debug".to_string();
        logging.filter = Some("debug,synctv=debug,synctv_core=debug".to_string());
        synctv_core::logging::init_logging(&logging)
            .expect("test tracing subscriber should initialize");
    });
}

struct SharedServer {
    api_base_url: String,
    management_base_url: String,
    provider_probe_endpoint: String,
    provider_probe_secret: String,
    // ManuallyDrop: prevent Drop at process exit.
    // The OS cleans up when the process exits; avoids hanging on server task join.
    _postgres: ManuallyDrop<TestContainer>,
    _redis: ManuallyDrop<RedisContainer>,
}

static SHARED_SERVER: OnceCell<SharedServer> = OnceCell::const_new();

/// Returns a reference to the lazily-initialized shared server.
/// The first call builds the Application and waits for health checks.
/// Subsequent calls return immediately.
async fn shared_server() -> &'static SharedServer {
    ensure_test_logging();
    SHARED_SERVER
        .get_or_init(|| async {
            let (postgres, database_url) =
                create_test_database_url_with_label("synctv_e2e_shared", "full-stack-shared").await;
            let (redis, redis_url) = start_redis_url_with_label("full-stack-shared").await;
            let api_port = reserve_local_port();
            let management_port = reserve_local_port();
            let rtmp_port = reserve_local_port();
            let provider_probe_addr =
                spawn_authenticated_provider_server(PROVIDER_PROBE_SECRET).await;
            let config = test_config(
                database_url,
                redis_url,
                api_port,
                management_port,
                rtmp_port,
            );

            // Spawn build + run on the dedicated runtime.
            // The JoinHandle is intentionally discarded — the server runs
            // independently until the process exits.
            let provider_probe_host = PROVIDER_PROBE_HOST.to_string();
            DEDICATED_RT.spawn(async move {
                let app = Application::build_with_options(
                    config,
                    ApplicationBuildOptions {
                        provider_test_address_overrides: HashMap::from([(
                            provider_probe_host,
                            provider_probe_addr,
                        )]),
                        credential_encryption_hex_key_override: Some(
                            TEST_CREDENTIAL_ENCRYPTION_KEY.to_string(),
                        ),
                    },
                )
                .await
                .expect("shared application build");
                // `pending()` means the server never receives a shutdown signal.
                app.run_with_shutdown_signal(std::future::pending()).await
            });

            let api_base_url = format!("http://127.0.0.1:{api_port}");
            let management_base_url = format!("http://127.0.0.1:{management_port}");
            let provider_probe_endpoint = format!(
                "http://{PROVIDER_PROBE_HOST}:{}",
                provider_probe_addr.port()
            );

            wait_until_live(&api_base_url).await;
            wait_until_grpc_ready(&api_base_url).await;
            wait_until_grpc_ready(&management_base_url).await;

            SharedServer {
                api_base_url,
                management_base_url,
                provider_probe_endpoint,
                provider_probe_secret: PROVIDER_PROBE_SECRET.to_string(),
                _postgres: ManuallyDrop::new(postgres),
                _redis: ManuallyDrop::new(redis),
            }
        })
        .await
}

async fn spawn_authenticated_provider_server(auth_secret: &str) -> SocketAddr {
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
    let auth_secret = auth_secret.to_string();
    DEDICATED_RT.spawn(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("provider auth test server should bind to an ephemeral port");
        let addr = listener
            .local_addr()
            .expect("provider auth test server should expose a local address");
        addr_tx
            .send(addr)
            .expect("provider auth test server address receiver should be alive");

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

    let addr = addr_rx
        .await
        .expect("provider auth test server should publish its local address");
    wait_until_grpc_ready(&format!("http://127.0.0.1:{}", addr.port())).await;
    addr
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

async fn wait_until_live(http_base_url: &str) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("HTTP client");
    let deadline = Instant::now() + Duration::from_secs(30);
    let url = format!("{}/health/live", http_base_url);

    loop {
        match client.get(&url).send().await {
            Ok(response) if response.status() == StatusCode::OK => return,
            Ok(_) | Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok(response) => {
                panic!(
                    "health endpoint never became live, last status: {}",
                    response.status()
                )
            }
            Err(error) => panic!("health endpoint never became live: {error}"),
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

#[cfg(unix)]
async fn wait_until_unix_grpc_ready(socket_path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        let path = socket_path.to_path_buf();
        let status = match tonic::transport::Endpoint::try_from("http://[::]:50052")
            .expect("valid synthetic unix gRPC endpoint")
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
            Ok(status) if status == ServingStatus::Serving as i32 => return,
            Ok(_) | Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok(status) => panic!("unix gRPC health never became serving, last status: {status}"),
            Err(error) => panic!("unix gRPC health never became ready: {error}"),
        }
    }
}

#[cfg(unix)]
struct DefaultManagementSocketGuard {
    path: std::path::PathBuf,
    backup_dir: Option<tempfile::TempDir>,
    backup_path: Option<std::path::PathBuf>,
}

#[cfg(unix)]
impl DefaultManagementSocketGuard {
    async fn acquire() -> anyhow::Result<Option<Self>> {
        use std::os::unix::fs::FileTypeExt;

        let path = default_management_unix_socket_path();

        if tokio::net::UnixStream::connect(&path).await.is_ok() {
            eprintln!(
                "skipping default management socket e2e because {} is already in use",
                path.display()
            );
            return Ok(None);
        }

        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                let backup_dir = tempfile::tempdir().map_err(|error| {
                    anyhow::anyhow!(
                        "failed to create backup dir for default management socket test: {error}"
                    )
                })?;
                let backup_path = backup_dir.path().join("synctv.sock.backup");
                std::fs::rename(&path, &backup_path).map_err(|error| {
                    anyhow::anyhow!(
                        "failed to move stale default management socket {} aside: {error}",
                        path.display()
                    )
                })?;

                Ok(Some(Self {
                    path,
                    backup_dir: Some(backup_dir),
                    backup_path: Some(backup_path),
                }))
            }
            Ok(_) => {
                eprintln!(
                    "skipping default management socket e2e because {} exists and is not a socket",
                    path.display()
                );
                Ok(None)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = path.parent() {
                    if let Err(mkdir_error) = std::fs::create_dir_all(parent) {
                        if mkdir_error.kind() == std::io::ErrorKind::PermissionDenied
                            || mkdir_error.raw_os_error() == Some(30)
                        {
                            eprintln!(
                                "skipping default management socket e2e because {} is not writable: {}",
                                parent.display(),
                                mkdir_error
                            );
                            return Ok(None);
                        }

                        return Err(anyhow::anyhow!(
                            "failed to create default management socket directory {}: {mkdir_error}",
                            parent.display()
                        ));
                    }
                }

                Ok(Some(Self {
                    path,
                    backup_dir: None,
                    backup_path: None,
                }))
            }
            Err(error) => Err(anyhow::anyhow!(
                "failed to inspect default management socket path {}: {error}",
                path.display()
            )),
        }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(unix)]
impl Drop for DefaultManagementSocketGuard {
    fn drop(&mut self) {
        if let Some(backup_path) = self.backup_path.as_ref() {
            if let Err(error) = std::fs::remove_file(&self.path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "failed to remove test-created default management socket {} during restore: {error}",
                        self.path.display()
                    );
                }
            }

            if let Err(error) = std::fs::rename(backup_path, &self.path) {
                eprintln!(
                    "failed to restore original default management socket {} from {}: {error}",
                    self.path.display(),
                    backup_path.display()
                );
            }
        }

        let _ = self.backup_dir.take();
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

fn run_synctv_remote_cli(server: &SharedServer, args: &[&str]) -> std::process::Output {
    let mut structured_args = args.to_vec();
    structured_args.extend(["--output", "json"]);
    run_synctv_cli_with_env(
        &structured_args,
        &[(
            "SYNCTV_MANAGEMENT_ENDPOINT",
            server.management_base_url.as_str(),
        )],
    )
}

fn run_synctv_cli_with_env(args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
    let binary = synctv_binary_path();
    let mut command = StdCommand::new(binary);
    command.args(args);
    for (name, value) in envs {
        command.env(name, value);
    }
    command.output().expect("synctv CLI process should start")
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

#[cfg(unix)]
fn write_daemon_test_config(
    path: &std::path::Path,
    database_url: &str,
    redis_url: &str,
    api_port: u16,
    management_socket_path: &std::path::Path,
    rtmp_port: u16,
) {
    let config = format!(
        r#"
server:
  host: "127.0.0.1"
  port: {api_port}
  enable_reflection: false
  advertise_host: "127.0.0.1"
  shutdown_drain_timeout_seconds: 3
metrics:
  enabled: false
  host: "127.0.0.1"
  port: 9090
  auth:
    mode: "bearer_token"
    bearer_token: ""
management:
  enabled: true
  transport: "unix"
  unix_socket_path: "{management_socket_path}"
  enable_reflection: false
database:
  url: "{database_url}"
redis:
  url: "{redis_url}"
  key_prefix: "synctv:test:daemon"
jwt:
  secret: "test-jwt-secret-key-for-daemon-e2e-123456"
bootstrap:
  create_root_user: true
  root_username: "admin"
  root_password: "StrongPwd12345!"
livestream:
  rtmp_port: {rtmp_port}
webrtc:
  enable_builtin_stun: false
"#,
        management_socket_path = management_socket_path.display(),
    );

    std::fs::write(path, config).expect("daemon test config should be written");
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
    body: Value,
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

async fn put_json(
    client: &reqwest::Client,
    url: &str,
    body: Value,
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

async fn ws_connect_with_ticket(
    addr: &str,
    room_id: &str,
    ticket: &str,
) -> Result<
    (
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tungstenite::handshake::client::Response,
    ),
    tokio_tungstenite::tungstenite::Error,
> {
    tokio_tungstenite::connect_async(format!("ws://{addr}/ws/rooms/{room_id}?ticket={ticket}"))
        .await
}

async fn recv_server_message(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Option<ServerMessage> {
    while let Some(message) = ws.next().await {
        match message {
            Ok(tungstenite::Message::Binary(bytes)) => {
                return Some(ServerMessage::decode(bytes.as_ref()).expect("decode server message"));
            }
            Ok(tungstenite::Message::Close(_)) => return None,
            Ok(_) => continue,
            Err(error) => panic!("websocket read failed: {error}"),
        }
    }

    None
}

fn error_message(body: &Value) -> &str {
    body["error"].as_str().unwrap_or("<missing error>")
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
    tonic::Request::new(message)
}

async fn recv_grpc_server_message(
    stream: &mut tonic::codec::Streaming<synctv_proto::client::ServerMessage>,
) -> Option<synctv_proto::client::ServerMessage> {
    match stream.message().await {
        Ok(message) => message,
        Err(error) => panic!("gRPC stream read failed: {error}"),
    }
}

async fn recv_grpc_server_message_skip_membership(
    stream: &mut tonic::codec::Streaming<synctv_proto::client::ServerMessage>,
) -> Option<synctv_proto::client::ServerMessage> {
    loop {
        let message = recv_grpc_server_message(stream).await?;
        match &message.message {
            Some(
                synctv_proto::client::server_message::Message::UserJoined(_)
                | synctv_proto::client::server_message::Message::UserLeft(_),
            ) => continue,
            _ => return Some(message),
        }
    }
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_health_endpoints_report_live_and_ready() {
    let server = shared_server().await;
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
    assert_eq!(ready_body["details"]["ws_ticket"], "healthy (redis)");

    // shared server — no per-test shutdown
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_management_exposes_only_remote_admin_surface() {
    use synctv_api::grpc::{AdminServiceImpl, ClientServiceImpl};
    use synctv_api::proto::admin_service_server::AdminServiceServer;
    use synctv_api::proto::client::{
        auth_service_server::AuthServiceServer, public_service_server::PublicServiceServer,
        room_service_server::RoomServiceServer, user_service_server::UserServiceServer,
    };
    use synctv_management::proto::management_service_server::ManagementServiceServer;
    use synctv_management::service::ManagementServiceImpl;

    let server = shared_server().await;

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
    use synctv_proto::client::{ListPlaylistItemsRequest, ListPlaylistsRequest, LoginRequest};

    let server = shared_server().await;
    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect auth gRPC client");
    let admin_login = auth_client
        .login(LoginRequest {
            username: "admin".to_string(),
            password: "StrongPwd12345!".to_string(),
        })
        .await
        .expect("bootstrap root login should succeed")
        .into_inner();

    let room_create = run_synctv_remote_cli(
        server,
        &[
            "room",
            "create",
            "CLI managed room",
            "--username",
            "admin",
            "--description",
            "cli remote room lifecycle e2e",
        ],
    );
    assert!(
        room_create.status.success(),
        "room create via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&room_create.stdout),
        String::from_utf8_lossy(&room_create.stderr),
    );
    let room_create_body: Value =
        serde_json::from_slice(&room_create.stdout).expect("CLI room create output should be JSON");
    let room_id = room_create_body["room"]["id"]
        .as_str()
        .expect("CLI room create output should contain room.id")
        .to_string();
    assert_eq!(room_create_body["room"]["name"], "CLI managed room");

    let playlist_list = run_synctv_remote_cli(server, &["playlist", "list", "--room-id", &room_id]);
    assert!(
        playlist_list.status.success(),
        "playlist list via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&playlist_list.stdout),
        String::from_utf8_lossy(&playlist_list.stderr),
    );

    let media_add = run_synctv_remote_cli(
        server,
        &[
            "media",
            "add",
            "--room-id",
            &room_id,
            "--username",
            "admin",
            "--provider",
            "direct_url",
            "--source-config-json",
            "{\"url\":\"https://cdn.example.com/cli-e2e.mp4\"}",
            "--title",
            "CLI E2E Media",
        ],
    );
    assert!(
        media_add.status.success(),
        "media add via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&media_add.stdout),
        String::from_utf8_lossy(&media_add.stderr),
    );
    let media_add_body: Value =
        serde_json::from_slice(&media_add.stdout).expect("CLI media add output should be JSON");
    let media_one_id = media_add_body["media"]["id"]
        .as_str()
        .expect("CLI media add output should contain media.id")
        .to_string();

    let media_add_second = run_synctv_remote_cli(
        server,
        &[
            "media",
            "add-url",
            "https://cdn.example.com/cli-e2e-second.mp4",
            "--room-id",
            &room_id,
            "--username",
            "admin",
            "--title",
            "CLI E2E Media Second",
        ],
    );
    assert!(
        media_add_second.status.success(),
        "second media add-url via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&media_add_second.stdout),
        String::from_utf8_lossy(&media_add_second.stderr),
    );
    let media_add_second_body: Value = serde_json::from_slice(&media_add_second.stdout)
        .expect("CLI second media add output should be JSON");
    let media_two_id = media_add_second_body["media"]["id"]
        .as_str()
        .expect("CLI second media add output should contain media.id")
        .to_string();

    let media_reorder = run_synctv_remote_cli(
        server,
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
    );
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
        source_provider: String::new(),
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
        target: Vec::new(),
        page: 1,
        page_size: 50,
        search: String::new(),
        source_provider: String::new(),
        provider_instance_name: String::new(),
        sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
        sort_direction: synctv_proto::client::SortDirection::Asc as i32,
        availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
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
    assert_eq!(first_media.title, "CLI E2E Media Second");
    assert_eq!(first_media.provider, "direct_url");
    assert_eq!(first_media.provider_instance_name, "");
    assert_eq!(second_media.id, media_one_id);
    assert_eq!(second_media.title, "CLI E2E Media");
    assert_eq!(second_media.provider, "direct_url");
    assert_eq!(second_media.provider_instance_name, "");
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_user_and_room_commands_use_remote_management_endpoint() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::{CreateRoomRequest, LoginRequest};

    let server = shared_server().await;
    let suffix = unique_test_suffix();
    let username = format!("cli_user_{suffix}");
    let email = format!("cli-user-{suffix}@example.com");
    let room_name = format!("CLI Room {suffix}");

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect public auth gRPC client");
    let admin_login = auth_client
        .login(LoginRequest {
            username: "admin".to_string(),
            password: "StrongPwd12345!".to_string(),
        })
        .await
        .expect("public auth login for bootstrap root should succeed")
        .into_inner();

    let create_user = run_synctv_remote_cli(
        server,
        &[
            "user",
            "create",
            &username,
            "--email",
            &email,
            "--password",
            "CliUserPass12345!",
        ],
    );
    assert!(
        create_user.status.success(),
        "user create via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&create_user.stdout),
        String::from_utf8_lossy(&create_user.stderr),
    );
    let create_user_body: Value =
        serde_json::from_slice(&create_user.stdout).expect("CLI user create output should be JSON");
    let created_user_id = create_user_body["user"]["id"]
        .as_str()
        .expect("CLI user create output should contain user.id")
        .to_string();
    assert_eq!(create_user_body["user"]["username"], username);
    assert_eq!(create_user_body["user"]["email"], email);

    let mut management_client =
        management_proto::management_service_client::ManagementServiceClient::connect(
            server.management_base_url.clone(),
        )
        .await
        .expect("connect management gRPC client");
    let fetched_user = management_client
        .get_user(management_request(management_proto::GetUserRequest {
            user_id: created_user_id.clone(),
        }))
        .await
        .expect("management get_user should succeed for CLI-created user")
        .into_inner()
        .user
        .expect("fetched admin user");
    assert_eq!(fetched_user.id, created_user_id);
    assert_eq!(fetched_user.username, username);
    assert_eq!(fetched_user.email, email);

    let mut user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect public user gRPC client");
    let mut create_room = tonic::Request::new(CreateRoomRequest {
        name: room_name.clone(),
        password: String::new(),
        settings: Vec::new(),
        description: "cli room get e2e".to_string(),
    });
    create_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&admin_login.access_token));
    let room_id = user_client
        .create_room(create_room)
        .await
        .expect("admin should create room for room get CLI test")
        .into_inner()
        .room
        .expect("created room")
        .id;

    let room_get = run_synctv_remote_cli(server, &["room", "get", &room_id]);
    assert!(
        room_get.status.success(),
        "room get via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&room_get.stdout),
        String::from_utf8_lossy(&room_get.stderr),
    );
    let room_get_body: Value =
        serde_json::from_slice(&room_get.stdout).expect("CLI room get output should be JSON");
    assert_eq!(room_get_body["room"]["id"], room_id);
    assert_eq!(room_get_body["room"]["name"], room_name);
    assert_eq!(room_get_body["room"]["description"], "cli room get e2e");
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_room_ban_and_unban_commands_manage_room_lifecycle() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::{CreateRoomRequest, LoginRequest};

    let server = shared_server().await;
    let suffix = unique_test_suffix();
    let room_name = format!("CLI Ban Room {suffix}");

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect public auth gRPC client");
    let admin_login = auth_client
        .login(LoginRequest {
            username: "admin".to_string(),
            password: "StrongPwd12345!".to_string(),
        })
        .await
        .expect("public auth login for bootstrap root should succeed")
        .into_inner();

    let mut user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect public user gRPC client");
    let mut create_room = tonic::Request::new(CreateRoomRequest {
        name: room_name,
        password: String::new(),
        settings: Vec::new(),
        description: "cli room ban lifecycle e2e".to_string(),
    });
    create_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&admin_login.access_token));
    let room_id = user_client
        .create_room(create_room)
        .await
        .expect("admin should create room for room ban CLI test")
        .into_inner()
        .room
        .expect("created room")
        .id;

    let room_ban = run_synctv_remote_cli(
        server,
        &["room", "ban", &room_id, "--reason", "CLI moderation test"],
    );
    assert!(
        room_ban.status.success(),
        "room ban via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&room_ban.stdout),
        String::from_utf8_lossy(&room_ban.stderr),
    );
    let room_ban_body: Value =
        serde_json::from_slice(&room_ban.stdout).expect("CLI room ban output should be JSON");
    assert_eq!(room_ban_body["room"]["id"], room_id);
    assert_eq!(room_ban_body["room"]["is_banned"], true);

    let room_unban = run_synctv_remote_cli(server, &["room", "unban", &room_id]);
    assert!(
        room_unban.status.success(),
        "room unban via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&room_unban.stdout),
        String::from_utf8_lossy(&room_unban.stderr),
    );
    let room_unban_body: Value =
        serde_json::from_slice(&room_unban.stdout).expect("CLI room unban output should be JSON");
    assert_eq!(room_unban_body["room"]["id"], room_id);
    assert_eq!(room_unban_body["room"]["is_banned"], false);

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
        .into_inner()
        .room
        .expect("fetched room");
    assert_eq!(fetched_room.id, room_id);
    assert!(!fetched_room.is_banned);
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_room_settings_commands_manage_room_settings_lifecycle() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::{CreateRoomRequest, LoginRequest};

    let server = shared_server().await;
    let suffix = unique_test_suffix();

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect public auth gRPC client");
    let admin_login = auth_client
        .login(LoginRequest {
            username: "admin".to_string(),
            password: "StrongPwd12345!".to_string(),
        })
        .await
        .expect("public auth login for bootstrap root should succeed")
        .into_inner();

    let mut user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect public user gRPC client");
    let mut create_room = tonic::Request::new(CreateRoomRequest {
        name: format!("CLI Settings Room {suffix}"),
        password: String::new(),
        settings: Vec::new(),
        description: "cli room settings lifecycle e2e".to_string(),
    });
    create_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&admin_login.access_token));
    let room_id = user_client
        .create_room(create_room)
        .await
        .expect("admin should create room for room settings CLI test")
        .into_inner()
        .room
        .expect("created room")
        .id;

    let settings_get = run_synctv_remote_cli(server, &["room", "settings", "get", &room_id]);
    assert!(
        settings_get.status.success(),
        "room settings get via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&settings_get.stdout),
        String::from_utf8_lossy(&settings_get.stderr),
    );
    let settings_get_body: Value = serde_json::from_slice(&settings_get.stdout)
        .expect("CLI room settings get output should be JSON");
    let mut updated_settings = settings_get_body["settings"]
        .as_object()
        .expect("CLI room settings get output should contain settings object")
        .clone();
    updated_settings.insert("chat_enabled".to_string(), Value::Bool(false));
    updated_settings.insert("allow_guest_join".to_string(), Value::Bool(true));
    let updated_settings_json = serde_json::to_string(&Value::Object(updated_settings))
        .expect("settings JSON should encode");

    let settings_update = run_synctv_remote_cli(
        server,
        &[
            "room",
            "settings",
            "update",
            &room_id,
            "--settings-json",
            &updated_settings_json,
        ],
    );
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
    let updated_settings_json: Value = serde_json::from_slice(&updated_settings_response.settings)
        .expect("updated room settings JSON should decode");
    assert_eq!(updated_settings_json["chat_enabled"], false);
    assert_eq!(updated_settings_json["allow_guest_join"], true);

    let settings_reset = run_synctv_remote_cli(server, &["room", "settings", "reset", &room_id]);
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
    let reset_settings_json: Value = serde_json::from_slice(&reset_settings_response.settings)
        .expect("reset room settings JSON should decode");
    assert_eq!(reset_settings_json["chat_enabled"], true);
    assert_eq!(reset_settings_json["allow_guest_join"], false);
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_user_admin_commands_manage_global_admin_lifecycle() {
    let server = shared_server().await;
    let suffix = unique_test_suffix();
    let username = format!("cli_admin_{suffix}");
    let email = format!("cli-admin-{suffix}@example.com");

    let create_user = run_synctv_remote_cli(
        server,
        &[
            "user",
            "create",
            &username,
            "--email",
            &email,
            "--password",
            "CliAdminPass12345!",
        ],
    );
    assert!(
        create_user.status.success(),
        "user create via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&create_user.stdout),
        String::from_utf8_lossy(&create_user.stderr),
    );
    let create_user_body: Value =
        serde_json::from_slice(&create_user.stdout).expect("CLI user create output should be JSON");
    let created_user_id = create_user_body["user"]["id"]
        .as_str()
        .expect("CLI user create output should contain user.id")
        .to_string();

    let add_admin = run_synctv_remote_cli(
        server,
        &["user", "admin", "grant", "--user-id", &created_user_id],
    );
    assert!(
        add_admin.status.success(),
        "user admin add via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&add_admin.stdout),
        String::from_utf8_lossy(&add_admin.stderr),
    );
    let add_admin_body: Value = serde_json::from_slice(&add_admin.stdout)
        .expect("CLI user admin add output should be JSON");
    assert_eq!(add_admin_body["user"]["id"], created_user_id);
    assert_eq!(
        add_admin_body["user"]["role"].as_i64(),
        Some(synctv_proto::common::UserRole::Admin as i64)
    );

    let list_admins = run_synctv_remote_cli(server, &["user", "admin", "list"]);
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
        server,
        &["user", "admin", "revoke", "--user-id", &created_user_id],
    );
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
        }))
        .await
        .expect("management get_user should succeed after remove-admin")
        .into_inner()
        .user
        .expect("fetched admin user");
    assert_eq!(fetched_user.id, created_user_id);
    assert_eq!(
        fetched_user.role,
        synctv_proto::common::UserRole::User as i32
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_user_unban_succeeds_even_when_target_is_the_only_bootstrap_root() {
    let server = shared_server().await;

    let ban_root = run_synctv_remote_cli(server, &["user", "ban", "admin", "--reason", "e2e"]);
    assert!(
        ban_root.status.success(),
        "user ban via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&ban_root.stdout),
        String::from_utf8_lossy(&ban_root.stderr),
    );
    let ban_root_body: Value =
        serde_json::from_slice(&ban_root.stdout).expect("CLI user ban output should be JSON");
    assert_eq!(
        ban_root_body["user"]["status"].as_i64(),
        Some(synctv_proto::common::UserStatus::Banned as i64)
    );

    let unban_root = run_synctv_remote_cli(server, &["user", "unban", "admin"]);
    assert!(
        unban_root.status.success(),
        "user unban via CLI should succeed even when the target is the only bootstrap root\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&unban_root.stdout),
        String::from_utf8_lossy(&unban_root.stderr),
    );
    let unban_root_body: Value =
        serde_json::from_slice(&unban_root.stdout).expect("CLI user unban output should be JSON");
    assert_eq!(
        unban_root_body["user"]["status"].as_i64(),
        Some(synctv_proto::common::UserStatus::Active as i64)
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_settings_and_system_commands_manage_remote_runtime_state() {
    let server = shared_server().await;

    let settings_list = run_synctv_remote_cli(server, &["settings", "list"]);
    assert!(
        settings_list.status.success(),
        "settings list via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&settings_list.stdout),
        String::from_utf8_lossy(&settings_list.stderr),
    );
    let settings_list_body: Value = serde_json::from_slice(&settings_list.stdout)
        .expect("CLI settings list output should be JSON");
    assert!(
        settings_list_body["groups"]
            .as_array()
            .expect("groups should be an array")
            .iter()
            .any(|group| group["name"] == "server"),
        "CLI settings list output should include server group: {settings_list_body}"
    );

    let settings_get = run_synctv_remote_cli(server, &["settings", "get", "server"]);
    assert!(
        settings_get.status.success(),
        "settings get via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&settings_get.stdout),
        String::from_utf8_lossy(&settings_get.stderr),
    );
    let settings_get_body: Value = serde_json::from_slice(&settings_get.stdout)
        .expect("CLI settings get output should be JSON");
    assert_eq!(settings_get_body["name"], "server");
    assert_eq!(settings_get_body["settings"]["signup_enabled"], true);

    let settings_update = run_synctv_remote_cli(
        server,
        &[
            "settings",
            "update",
            "server",
            "--set",
            "signup_enabled=false",
            "--set",
            "max_rooms_per_user=42",
        ],
    );
    assert!(
        settings_update.status.success(),
        "settings update via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&settings_update.stdout),
        String::from_utf8_lossy(&settings_update.stderr),
    );
    let settings_update_body: Value = serde_json::from_slice(&settings_update.stdout)
        .expect("CLI settings update output should be JSON");
    assert_eq!(settings_update_body["name"], "server");
    assert_eq!(settings_update_body["settings"]["signup_enabled"], false);
    assert_eq!(settings_update_body["settings"]["max_rooms_per_user"], 42);

    let mut management_client =
        management_proto::management_service_client::ManagementServiceClient::connect(
            server.management_base_url.clone(),
        )
        .await
        .expect("connect management gRPC client");
    let server_group = management_client
        .get_settings_group(management_request(
            management_proto::GetSettingsGroupRequest {
                group: "server".to_string(),
            },
        ))
        .await
        .expect("management get_settings_group should succeed after CLI update")
        .into_inner()
        .group
        .expect("server settings group");
    let server_group_settings: Value = serde_json::from_slice(&server_group.settings)
        .expect("server settings group payload should decode");
    assert_eq!(server_group_settings["signup_enabled"], false);
    assert_eq!(server_group_settings["max_rooms_per_user"], 42);

    let system_stats = run_synctv_remote_cli(server, &["system", "stats"]);
    assert!(
        system_stats.status.success(),
        "system stats via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&system_stats.stdout),
        String::from_utf8_lossy(&system_stats.stderr),
    );
    let system_stats_body: Value = serde_json::from_slice(&system_stats.stdout)
        .expect("CLI system stats output should be JSON");
    assert!(
        system_stats_body["total_users"]
            .as_i64()
            .expect("total_users should be an integer")
            >= 1,
        "system stats should report at least the bootstrap root user: {system_stats_body}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_system_stats_uses_explicit_management_endpoint_flag_without_config() {
    let server = shared_server().await;

    let system_stats = run_synctv_cli_with_env(
        &[
            "system",
            "stats",
            "--endpoint",
            &server.management_base_url,
            "--output",
            "json",
        ],
        &[],
    );
    assert!(
        system_stats.status.success(),
        "system stats via CLI explicit endpoint should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&system_stats.stdout),
        String::from_utf8_lossy(&system_stats.stderr),
    );
    let system_stats_body: Value = serde_json::from_slice(&system_stats.stdout)
        .expect("CLI explicit endpoint system stats output should be JSON");
    assert!(
        system_stats_body["total_users"]
            .as_i64()
            .expect("total_users should be an integer")
            >= 1,
        "system stats should report at least the bootstrap root user: {system_stats_body}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_system_stats_works_when_bootstrap_root_username_differs() {
    let server = shared_server().await;

    let system_stats = run_synctv_remote_cli(server, &["system", "stats"]);
    assert!(
        system_stats.status.success(),
        "system stats via CLI should succeed when bootstrap root username differs\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&system_stats.stdout),
        String::from_utf8_lossy(&system_stats.stderr),
    );
    let system_stats_body: Value = serde_json::from_slice(&system_stats.stdout)
        .expect("CLI system stats output should be JSON when bootstrap root username differs");
    assert!(
        system_stats_body["total_users"]
            .as_i64()
            .expect("total_users should be an integer")
            >= 1,
        "system stats should report at least the bootstrap root user: {system_stats_body}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_provider_commands_manage_local_only_provider_lifecycle() {
    let server = shared_server().await;
    let suffix = unique_test_suffix();
    let provider_name = format!("local-provider-{suffix}");

    let provider_add = run_synctv_remote_cli(
        server,
        &[
            "provider",
            "create",
            &provider_name,
            "http://127.0.0.1:59999",
            "--provider",
            "custom_local",
            "--comment",
            "local-only provider lifecycle e2e",
        ],
    );
    assert!(
        provider_add.status.success(),
        "provider add via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_add.stdout),
        String::from_utf8_lossy(&provider_add.stderr),
    );
    let provider_add_body: Value = serde_json::from_slice(&provider_add.stdout)
        .expect("CLI provider add output should be JSON");
    assert_eq!(provider_add_body["instance"]["name"], provider_name);
    assert_eq!(provider_add_body["instance"]["enabled"], true);
    assert_eq!(
        provider_add_body["instance"]["providers"],
        json!(["custom_local"])
    );

    let provider_list_filtered = run_synctv_remote_cli(
        server,
        &["provider", "list", "--provider-type", "custom_local"],
    );
    assert!(
        provider_list_filtered.status.success(),
        "provider list via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_list_filtered.stdout),
        String::from_utf8_lossy(&provider_list_filtered.stderr),
    );
    let provider_list_filtered_body: Value = serde_json::from_slice(&provider_list_filtered.stdout)
        .expect("CLI provider list output should be JSON");
    assert!(
        provider_list_filtered_body["instances"]
            .as_array()
            .expect("instances should be an array")
            .iter()
            .any(|instance| instance["name"] == provider_name),
        "filtered provider list should include the created instance: {provider_list_filtered_body}"
    );

    let provider_update = run_synctv_remote_cli(
        server,
        &[
            "provider",
            "update",
            &provider_name,
            "--comment",
            "updated provider lifecycle e2e",
            "--provider",
            "custom_local",
            "--provider",
            "custom_archive",
            "--timeout-seconds",
            "25",
        ],
    );
    assert!(
        provider_update.status.success(),
        "provider update via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_update.stdout),
        String::from_utf8_lossy(&provider_update.stderr),
    );
    let provider_update_body: Value = serde_json::from_slice(&provider_update.stdout)
        .expect("CLI provider update output should be JSON");
    assert_eq!(
        provider_update_body["instance"]["comment"],
        "updated provider lifecycle e2e"
    );
    assert_eq!(
        provider_update_body["instance"]["providers"],
        json!(["custom_local", "custom_archive"])
    );
    assert_eq!(provider_update_body["instance"]["timeout_seconds"], 25);

    let provider_disable = run_synctv_remote_cli(server, &["provider", "disable", &provider_name]);
    assert!(
        provider_disable.status.success(),
        "provider disable via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_disable.stdout),
        String::from_utf8_lossy(&provider_disable.stderr),
    );
    let provider_disable_body: Value = serde_json::from_slice(&provider_disable.stdout)
        .expect("CLI provider disable output should be JSON");
    assert_eq!(provider_disable_body["instance"]["enabled"], false);

    let provider_enable = run_synctv_remote_cli(server, &["provider", "enable", &provider_name]);
    assert!(
        provider_enable.status.success(),
        "provider enable via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_enable.stdout),
        String::from_utf8_lossy(&provider_enable.stderr),
    );
    let provider_enable_body: Value = serde_json::from_slice(&provider_enable.stdout)
        .expect("CLI provider enable output should be JSON");
    assert_eq!(provider_enable_body["instance"]["enabled"], true);

    let mut management_client =
        management_proto::management_service_client::ManagementServiceClient::connect(
            server.management_base_url.clone(),
        )
        .await
        .expect("connect management gRPC client");
    let provider_list_after_enable = management_client
        .list_provider_instances(management_request(
            management_proto::ListProviderInstancesRequest {
                page: 1,
                page_size: 50,
                provider_type: "custom_archive".to_string(),
                search: String::new(),
                enabled: None,
                tls: None,
                sort_by: management_proto::ProviderInstanceListSortBy::CreatedAt as i32,
                sort_direction: management_proto::SortDirection::Desc as i32,
            },
        ))
        .await
        .expect("management list_provider_instances should succeed after CLI lifecycle updates")
        .into_inner();
    let persisted_provider = provider_list_after_enable
        .instances
        .into_iter()
        .find(|instance| instance.name == provider_name)
        .expect("provider should be persisted after CLI lifecycle updates");
    assert_eq!(persisted_provider.comment, "updated provider lifecycle e2e");
    assert!(persisted_provider.enabled);
    assert_eq!(persisted_provider.timeout_seconds, 25);

    let provider_delete = run_synctv_remote_cli(server, &["provider", "delete", &provider_name]);
    assert!(
        provider_delete.status.success(),
        "provider delete via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_delete.stdout),
        String::from_utf8_lossy(&provider_delete.stderr),
    );
    let provider_delete_body: Value = serde_json::from_slice(&provider_delete.stdout)
        .expect("CLI provider delete output should be JSON");
    assert_eq!(provider_delete_body["success"], true);

    let provider_list_after_delete = run_synctv_remote_cli(
        server,
        &["provider", "list", "--provider-type", "custom_archive"],
    );
    assert!(
        provider_list_after_delete.status.success(),
        "provider list after delete via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_list_after_delete.stdout),
        String::from_utf8_lossy(&provider_list_after_delete.stderr),
    );
    let provider_list_after_delete_body: Value =
        serde_json::from_slice(&provider_list_after_delete.stdout)
            .expect("CLI provider list after delete output should be JSON");
    assert!(
        provider_list_after_delete_body["instances"]
            .as_array()
            .expect("instances should be an array")
            .iter()
            .all(|instance| instance["name"] != provider_name),
        "provider should be absent after delete: {provider_list_after_delete_body}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_provider_commands_manage_remote_provider_lifecycle() {
    let server = shared_server().await;
    let suffix = unique_test_suffix();
    let provider_name = format!("remote-provider-{suffix}");
    let provider_config_json = serde_json::to_string(&json!({
        "jwt_secret": server.provider_probe_secret,
    }))
    .expect("provider config JSON should encode");

    let provider_add = run_synctv_remote_cli(
        server,
        &[
            "provider",
            "create",
            &provider_name,
            &server.provider_probe_endpoint,
            "--provider",
            "alist",
            "--comment",
            "remote provider lifecycle e2e",
            "--config-json",
            &provider_config_json,
        ],
    );
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
    assert_eq!(provider_add_body["instance"]["providers"], json!(["alist"]));

    let provider_list_filtered =
        run_synctv_remote_cli(server, &["provider", "list", "--provider-type", "alist"]);
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
        run_synctv_remote_cli(server, &["provider", "reconnect", &provider_name]);
    assert!(
        provider_reconnect.status.success(),
        "remote provider reconnect via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_reconnect.stdout),
        String::from_utf8_lossy(&provider_reconnect.stderr),
    );

    let provider_disable = run_synctv_remote_cli(server, &["provider", "disable", &provider_name]);
    assert!(
        provider_disable.status.success(),
        "remote provider disable via CLI should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&provider_disable.stdout),
        String::from_utf8_lossy(&provider_disable.stderr),
    );
    let provider_disable_body: Value = serde_json::from_slice(&provider_disable.stdout)
        .expect("CLI remote provider disable output should be JSON");
    assert_eq!(provider_disable_body["instance"]["enabled"], false);

    let provider_enable = run_synctv_remote_cli(server, &["provider", "enable", &provider_name]);
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
            management_proto::ListProviderInstancesRequest {
                page: 1,
                page_size: 50,
                provider_type: "alist".to_string(),
                search: String::new(),
                enabled: None,
                tls: None,
                sort_by: management_proto::ProviderInstanceListSortBy::CreatedAt as i32,
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

    let provider_delete = run_synctv_remote_cli(server, &["provider", "delete", &provider_name]);
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
    config.management.transport = synctv_core::config::ManagementTransport::Unix;
    config.management.unix_socket_path = socket_path.display().to_string();

    let app = Application::build_with_options(
        config,
        ApplicationBuildOptions {
            credential_encryption_hex_key_override: Some(
                TEST_CREDENTIAL_ENCRYPTION_KEY.to_string(),
            ),
            ..ApplicationBuildOptions::default()
        },
    )
    .await
    .expect("unix management application should build");

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server_handle = tokio::spawn(async move {
        app.run_with_shutdown_signal(async move {
            let _ = shutdown_rx.await;
        })
        .await
    });

    let api_base_url = format!("http://127.0.0.1:{api_port}");
    wait_until_live(&api_base_url).await;
    wait_until_unix_grpc_ready(&socket_path).await;

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
        system_stats_body["total_users"]
            .as_i64()
            .expect("total_users should be an integer")
            >= 1,
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
    let socket_guard = match DefaultManagementSocketGuard::acquire()
        .await
        .expect("default management socket guard should inspect the default path")
    {
        Some(guard) => guard,
        None => return,
    };

    let (postgres, database_url) = create_test_database_url_with_label(
        "synctv_e2e_default_unix",
        "full-stack-management-default-unix",
    )
    .await;
    let (redis, redis_url) = start_redis_url_with_label("full-stack-management-default-unix").await;
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
    config.management = synctv_core::config::ManagementConfig::default();
    config.management.enable_reflection = false;

    let app = Application::build_with_options(
        config,
        ApplicationBuildOptions {
            credential_encryption_hex_key_override: Some(
                TEST_CREDENTIAL_ENCRYPTION_KEY.to_string(),
            ),
            ..ApplicationBuildOptions::default()
        },
    )
    .await
    .expect("default unix management application should build");

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server_handle = tokio::spawn(async move {
        app.run_with_shutdown_signal(async move {
            let _ = shutdown_rx.await;
        })
        .await
    });

    let api_base_url = format!("http://127.0.0.1:{api_port}");
    wait_until_live(&api_base_url).await;
    wait_until_unix_grpc_ready(socket_guard.path()).await;

    let system_stats =
        run_synctv_cli_with_env_async(&["system", "stats", "--output", "json"], &[]).await;
    assert!(
        system_stats.status.success(),
        "system stats via CLI default unix socket should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&system_stats.stdout),
        String::from_utf8_lossy(&system_stats.stderr),
    );
    let system_stats_body: Value = serde_json::from_slice(&system_stats.stdout)
        .expect("CLI default unix socket system stats output should be JSON");
    assert!(
        system_stats_body["total_users"]
            .as_i64()
            .expect("total_users should be an integer")
            >= 1,
        "system stats over default unix socket should report at least the bootstrap root user: {system_stats_body}"
    );

    let _ = shutdown_tx.send(());
    let server_result = server_handle
        .await
        .expect("default unix management server task should join");
    server_result.expect("default unix management server should shut down cleanly");

    drop(socket_guard);
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
    config.management.transport = synctv_core::config::ManagementTransport::Unix;
    config.management.unix_socket_path = socket_path.display().to_string();

    let app = Application::build_with_options(
        config,
        ApplicationBuildOptions {
            credential_encryption_hex_key_override: Some(
                TEST_CREDENTIAL_ENCRYPTION_KEY.to_string(),
            ),
            ..ApplicationBuildOptions::default()
        },
    )
    .await
    .expect("stop test application should build");

    let server_handle = tokio::spawn(async move { app.run().await });

    let api_base_url = format!("http://127.0.0.1:{api_port}");
    wait_until_live(&api_base_url).await;
    wait_until_unix_grpc_ready(&socket_path).await;

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
    config.management.transport = synctv_core::config::ManagementTransport::Unix;
    config.management.unix_socket_path = socket_path.display().to_string();

    let app = Application::build_with_options(
        config,
        ApplicationBuildOptions {
            credential_encryption_hex_key_override: Some(
                TEST_CREDENTIAL_ENCRYPTION_KEY.to_string(),
            ),
            ..ApplicationBuildOptions::default()
        },
    )
    .await
    .expect("force stop test application should build");

    let server_handle = tokio::spawn(async move { app.run().await });

    let api_base_url = format!("http://127.0.0.1:{api_port}");
    wait_until_live(&api_base_url).await;
    wait_until_unix_grpc_ready(&socket_path).await;

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

#[cfg(unix)]
#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_cli_serve_daemon_starts_background_server_and_stop_shuts_it_down() {
    let (postgres, database_url) =
        create_test_database_url_with_label("synctv_e2e_serve_daemon", "full-stack-serve-daemon")
            .await;
    let (redis, redis_url) = start_redis_url_with_label("full-stack-serve-daemon").await;
    let api_port = reserve_local_port();
    let rtmp_port = reserve_local_port();
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let socket_path = temp_dir.path().join("management.sock");
    let config_path = temp_dir.path().join("synctv.yaml");

    write_daemon_test_config(
        &config_path,
        &database_url,
        &redis_url,
        api_port,
        &socket_path,
        rtmp_port,
    );

    let serve_output = run_synctv_cli_with_env_async(
        &[
            "serve",
            "--daemon",
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--no-dotenv",
        ],
        &[],
    )
    .await;

    assert!(
        serve_output.status.success(),
        "serve --daemon should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&serve_output.stdout),
        String::from_utf8_lossy(&serve_output.stderr),
    );

    wait_until_unix_grpc_ready(&socket_path).await;

    let management_endpoint = format!("unix://{}", socket_path.display());
    let stop_output = run_synctv_cli_with_env_async(
        &["stop"],
        &[("SYNCTV_MANAGEMENT_ENDPOINT", management_endpoint.as_str())],
    )
    .await;
    assert!(
        stop_output.status.success(),
        "stop should shut down daemonized server\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&stop_output.stdout),
        String::from_utf8_lossy(&stop_output.stderr),
    );

    drop(postgres);
    drop(redis);
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_http_auth_room_and_ticket_flow_enforces_membership() {
    let server = shared_server().await;
    let client = test_http_client();

    let owner_register = post_json(
        &client,
        &format!("{}/api/auth/register", server.api_base_url),
        json!({
            "username": "owner_user",
            "email": "owner@example.com",
            "password": "OwnerPass12345!"
        }),
        None,
    )
    .await;
    assert_eq!(owner_register.status(), StatusCode::OK);

    let owner_login = post_json(
        &client,
        &format!("{}/api/auth/login", server.api_base_url),
        json!({
            "username": "owner_user",
            "password": "OwnerPass12345!"
        }),
        None,
    )
    .await;
    assert_eq!(owner_login.status(), StatusCode::OK);
    let owner_login_body = response_json(owner_login).await;
    let owner_token = owner_login_body["access_token"]
        .as_str()
        .expect("owner access token")
        .to_string();

    let owner_profile = get_with_bearer(
        &client,
        &format!("{}/api/user", server.api_base_url),
        &owner_token,
    )
    .await;
    assert_eq!(owner_profile.status(), StatusCode::OK);
    let owner_profile_body = response_json(owner_profile).await;
    assert_eq!(owner_profile_body["user"]["username"], "owner_user");

    let create_room = post_json(
        &client,
        &format!("{}/api/rooms", server.api_base_url),
        json!({
            "name": "Full Stack Room",
            "password": "RoomPass12345!",
            "settings": [],
            "description": "end-to-end room"
        }),
        Some(&owner_token),
    )
    .await;
    assert_eq!(create_room.status(), StatusCode::OK);
    let create_room_body = response_json(create_room).await;
    let room_id = create_room_body["room"]["id"]
        .as_str()
        .expect("created room id")
        .to_string();

    let member_register = post_json(
        &client,
        &format!("{}/api/auth/register", server.api_base_url),
        json!({
            "username": "member_user",
            "email": "member@example.com",
            "password": "MemberPass12345!"
        }),
        None,
    )
    .await;
    assert_eq!(member_register.status(), StatusCode::OK);

    let member_login = post_json(
        &client,
        &format!("{}/api/auth/login", server.api_base_url),
        json!({
            "username": "member_user",
            "password": "MemberPass12345!"
        }),
        None,
    )
    .await;
    assert_eq!(member_login.status(), StatusCode::OK);
    let member_login_body = response_json(member_login).await;
    let member_token = member_login_body["access_token"]
        .as_str()
        .expect("member access token")
        .to_string();

    let forbidden_ticket = post_json(
        &client,
        &format!("{}/api/tickets", server.api_base_url),
        json!({ "room_id": room_id }),
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
        json!({ "password": "RoomPass12345!" }),
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
        json!({ "room_id": room_id }),
        Some(&member_token),
    )
    .await;
    assert_eq!(member_ticket.status(), StatusCode::OK);
    let member_ticket_body = response_json(member_ticket).await;
    assert_eq!(member_ticket_body["room_id"], room_id);
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

    // shared server — no per-test shutdown
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_http_ticket_upgrades_websocket_once_and_then_expires() {
    let server = shared_server().await;
    let client = test_http_client();

    let register = post_json(
        &client,
        &format!("{}/api/auth/register", server.api_base_url),
        json!({
            "username": "ws_ticket_user",
            "email": "ws-ticket@example.com",
            "password": "WsTicketPass12345!"
        }),
        None,
    )
    .await;
    assert_eq!(register.status(), StatusCode::OK);

    let login = post_json(
        &client,
        &format!("{}/api/auth/login", server.api_base_url),
        json!({
            "username": "ws_ticket_user",
            "password": "WsTicketPass12345!"
        }),
        None,
    )
    .await;
    assert_eq!(login.status(), StatusCode::OK);
    let login_body = response_json(login).await;
    let token = login_body["access_token"]
        .as_str()
        .expect("access token")
        .to_string();

    let create_room = post_json(
        &client,
        &format!("{}/api/rooms", server.api_base_url),
        json!({
            "name": "WS Ticket Room",
            "password": "",
            "settings": [],
            "description": "ticket to websocket e2e"
        }),
        Some(&token),
    )
    .await;
    assert_eq!(create_room.status(), StatusCode::OK);
    let create_room_body = response_json(create_room).await;
    let room_id = create_room_body["room"]["id"]
        .as_str()
        .expect("room id")
        .to_string();

    let create_ticket = post_json(
        &client,
        &format!("{}/api/tickets", server.api_base_url),
        json!({ "room_id": room_id }),
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

    let initial = tokio::time::timeout(Duration::from_secs(5), recv_server_message(&mut ws))
        .await
        .expect("timed out waiting for websocket welcome message")
        .expect("websocket closed before first message");
    assert!(
        matches!(
            initial.message,
            Some(server_message::Message::UserJoined(_))
        ),
        "expected initial UserJoined after ticket-authenticated upgrade, got: {initial:?}"
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

    // shared server — no per-test shutdown
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_auth_register_login_and_get_profile() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::{GetProfileRequest, LoginRequest, RegisterRequest};

    let server = shared_server().await;

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect auth gRPC client");
    let register = auth_client
        .register(RegisterRequest {
            username: "grpc_user".to_string(),
            email: "grpc-user@example.com".to_string(),
            password: "GrpcPass12345!".to_string(),
        })
        .await
        .expect("grpc register should succeed")
        .into_inner();
    let registered_user = register.user.expect("registered user");
    assert_eq!(registered_user.username, "grpc_user");
    assert_eq!(registered_user.email, "grpc-user@example.com");
    assert!(!register.access_token.is_empty());
    assert!(!register.refresh_token.is_empty());

    let login = auth_client
        .login(LoginRequest {
            username: "grpc_user".to_string(),
            password: "GrpcPass12345!".to_string(),
        })
        .await
        .expect("grpc login should succeed")
        .into_inner();
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
    let user = profile.user.expect("profile user");
    assert_eq!(user.username, "grpc_user");
    assert_eq!(user.email, "grpc-user@example.com");

    // shared server — no per-test shutdown
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_create_room_requires_auth_and_returns_created_room() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::{CreateRoomRequest, LoginRequest, RegisterRequest};

    let server = shared_server().await;

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect auth gRPC client");
    auth_client
        .register(RegisterRequest {
            username: "grpc_room_owner".to_string(),
            email: "grpc-room-owner@example.com".to_string(),
            password: "GrpcRoomPass12345!".to_string(),
        })
        .await
        .expect("grpc register should succeed");
    let login = auth_client
        .login(LoginRequest {
            username: "grpc_room_owner".to_string(),
            password: "GrpcRoomPass12345!".to_string(),
        })
        .await
        .expect("grpc login should succeed")
        .into_inner();

    let mut user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect user gRPC client");
    let unauthenticated = user_client
        .create_room(CreateRoomRequest {
            name: "gRPC room".to_string(),
            password: String::new(),
            settings: Vec::new(),
            description: "created through full-stack gRPC e2e".to_string(),
        })
        .await
        .expect_err("missing auth should be rejected");
    assert_eq!(unauthenticated.code(), tonic::Code::Unauthenticated);

    let mut request = tonic::Request::new(CreateRoomRequest {
        name: "gRPC room".to_string(),
        password: "RoomPass12345!".to_string(),
        settings: Vec::new(),
        description: "created through full-stack gRPC e2e".to_string(),
    });
    request
        .metadata_mut()
        .insert("authorization", bearer_metadata(&login.access_token));
    let response = user_client
        .create_room(request)
        .await
        .expect("authenticated create_room should succeed")
        .into_inner();
    let room = response.room.expect("created room");
    assert_eq!(room.name, "gRPC room");
    assert_eq!(room.description, "created through full-stack gRPC e2e");
    assert_eq!(room.created_by, login.user.expect("login user").id);
    assert_eq!(room.member_count, 0);
    assert_eq!(room.id.len(), 12);

    // shared server — no per-test shutdown
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_room_context_flow_requires_membership_and_room_metadata() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::{
        CreateRoomRequest, GetRoomRequest, GetRoomSettingsRequest, JoinRoomRequest, LoginRequest,
        RegisterRequest,
    };

    let server = shared_server().await;

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect auth gRPC client");

    auth_client
        .register(RegisterRequest {
            username: "grpc_owner".to_string(),
            email: "grpc-owner@example.com".to_string(),
            password: "GrpcOwnerPass12345!".to_string(),
        })
        .await
        .expect("owner register should succeed");
    let owner_login = auth_client
        .login(LoginRequest {
            username: "grpc_owner".to_string(),
            password: "GrpcOwnerPass12345!".to_string(),
        })
        .await
        .expect("owner login should succeed")
        .into_inner();

    auth_client
        .register(RegisterRequest {
            username: "grpc_member".to_string(),
            email: "grpc-member@example.com".to_string(),
            password: "GrpcMemberPass12345!".to_string(),
        })
        .await
        .expect("member register should succeed");
    let member_login = auth_client
        .login(LoginRequest {
            username: "grpc_member".to_string(),
            password: "GrpcMemberPass12345!".to_string(),
        })
        .await
        .expect("member login should succeed")
        .into_inner();

    let mut owner_user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect owner user gRPC client");
    let mut create_room = tonic::Request::new(CreateRoomRequest {
        name: "gRPC metadata room".to_string(),
        password: "GrpcRoomSecret123!".to_string(),
        settings: Vec::new(),
        description: "room-scoped grpc e2e".to_string(),
    });
    create_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&owner_login.access_token));
    let created = owner_user_client
        .create_room(create_room)
        .await
        .expect("owner should create room")
        .into_inner();
    let room = created.room.expect("created room");
    let room_id = room.id;

    let mut member_room_client = RoomServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect member room gRPC client");

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

    let mut join_room = tonic::Request::new(JoinRoomRequest {
        room_id: room_id.clone(),
        password: "GrpcRoomSecret123!".to_string(),
    });
    join_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&member_login.access_token));
    let joined = owner_user_client
        .join_room(join_room)
        .await
        .expect("member should join room")
        .into_inner();
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

    // shared server — no per-test shutdown
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_message_stream_establishes_and_acks_heartbeat() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::client_message;
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use synctv_proto::client::server_message;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::{
        ClientMessage, CreateRoomRequest, HeartbeatMessage, JoinRoomRequest, LoginRequest,
        RegisterRequest,
    };

    let server = shared_server().await;

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect auth gRPC client");
    auth_client
        .register(RegisterRequest {
            username: "grpc_stream_owner".to_string(),
            email: "grpc-stream-owner@example.com".to_string(),
            password: "GrpcStreamOwnerPass12345!".to_string(),
        })
        .await
        .expect("owner register should succeed");
    let owner_login = auth_client
        .login(LoginRequest {
            username: "grpc_stream_owner".to_string(),
            password: "GrpcStreamOwnerPass12345!".to_string(),
        })
        .await
        .expect("owner login should succeed")
        .into_inner();

    auth_client
        .register(RegisterRequest {
            username: "grpc_stream_member".to_string(),
            email: "grpc-stream-member@example.com".to_string(),
            password: "GrpcStreamMemberPass12345!".to_string(),
        })
        .await
        .expect("member register should succeed");
    let member_login = auth_client
        .login(LoginRequest {
            username: "grpc_stream_member".to_string(),
            password: "GrpcStreamMemberPass12345!".to_string(),
        })
        .await
        .expect("member login should succeed")
        .into_inner();

    let mut owner_user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect owner user gRPC client");
    let mut create_room = tonic::Request::new(CreateRoomRequest {
        name: "gRPC stream room".to_string(),
        password: "GrpcStreamRoomSecret123!".to_string(),
        settings: Vec::new(),
        description: "grpc stream e2e".to_string(),
    });
    create_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&owner_login.access_token));
    let room_id = owner_user_client
        .create_room(create_room)
        .await
        .expect("owner create_room should succeed")
        .into_inner()
        .room
        .expect("created room")
        .id;

    let mut member_room_client = RoomServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect member room gRPC client");
    let mut join_room = tonic::Request::new(JoinRoomRequest {
        room_id: room_id.clone(),
        password: "GrpcStreamRoomSecret123!".to_string(),
    });
    join_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&member_login.access_token));
    let mut member_user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect member user gRPC client");
    member_user_client
        .join_room(join_room)
        .await
        .expect("member join_room should succeed");

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

    let initial = tokio::time::timeout(
        Duration::from_secs(5),
        recv_grpc_server_message(&mut inbound),
    )
    .await
    .expect("timed out waiting for initial grpc stream message")
    .expect("grpc stream ended before initial message");
    assert!(
        matches!(
            initial.message,
            Some(server_message::Message::UserJoined(_))
        ),
        "expected initial UserJoined message, got: {initial:?}"
    );

    let ack = tokio::time::timeout(
        Duration::from_secs(5),
        recv_grpc_server_message_skip_membership(&mut inbound),
    )
    .await
    .expect("timed out waiting for grpc heartbeat ack")
    .expect("grpc stream ended before heartbeat ack");
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

    // shared server — no per-test shutdown
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_message_stream_requires_join_room_first() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use synctv_proto::client::{ClientMessage, LoginRequest, RegisterRequest};

    let server = shared_server().await;

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect auth gRPC client");
    auth_client
        .register(RegisterRequest {
            username: "grpc_stream_missing_join".to_string(),
            email: "grpc-stream-missing-join@example.com".to_string(),
            password: "GrpcStreamMissingJoinPass12345!".to_string(),
        })
        .await
        .expect("register should succeed");
    let login = auth_client
        .login(LoginRequest {
            username: "grpc_stream_missing_join".to_string(),
            password: "GrpcStreamMissingJoinPass12345!".to_string(),
        })
        .await
        .expect("login should succeed")
        .into_inner();

    let mut room_client = RoomServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect room gRPC client");
    // Send stream without x-room-id metadata - should fail before stream establishment
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

    // shared server — no per-test shutdown
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_message_stream_requires_membership() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::{ClientMessage, CreateRoomRequest, LoginRequest, RegisterRequest};

    let server = shared_server().await;

    let mut auth_client = AuthServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect auth gRPC client");

    auth_client
        .register(RegisterRequest {
            username: "grpc_stream_owner_only".to_string(),
            email: "grpc-stream-owner-only@example.com".to_string(),
            password: "GrpcStreamOwnerOnlyPass12345!".to_string(),
        })
        .await
        .expect("owner register should succeed");
    let owner_login = auth_client
        .login(LoginRequest {
            username: "grpc_stream_owner_only".to_string(),
            password: "GrpcStreamOwnerOnlyPass12345!".to_string(),
        })
        .await
        .expect("owner login should succeed")
        .into_inner();

    auth_client
        .register(RegisterRequest {
            username: "grpc_stream_outsider".to_string(),
            email: "grpc-stream-outsider@example.com".to_string(),
            password: "GrpcStreamOutsiderPass12345!".to_string(),
        })
        .await
        .expect("outsider register should succeed");
    let outsider_login = auth_client
        .login(LoginRequest {
            username: "grpc_stream_outsider".to_string(),
            password: "GrpcStreamOutsiderPass12345!".to_string(),
        })
        .await
        .expect("outsider login should succeed")
        .into_inner();

    let mut owner_user_client = UserServiceClient::connect(server.api_base_url.clone())
        .await
        .expect("connect owner user gRPC client");
    let mut create_room = tonic::Request::new(CreateRoomRequest {
        name: "gRPC outsider stream room".to_string(),
        password: String::new(),
        settings: Vec::new(),
        description: "membership denial for grpc stream".to_string(),
    });
    create_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&owner_login.access_token));
    let room_id = owner_user_client
        .create_room(create_room)
        .await
        .expect("owner create_room should succeed")
        .into_inner()
        .room
        .expect("created room")
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

    // shared server — no per-test shutdown
}
