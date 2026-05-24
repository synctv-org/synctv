#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use opaque_ke::argon2::Argon2;
use opaque_ke::ciphersuite::CipherSuite;
use opaque_ke::rand::rngs::OsRng;
use opaque_ke::{
    ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialResponse, RegistrationResponse,
};
use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Value};
use sha2_010::Sha512;
use synctv_core::models::UserId;
use synctv_core::service::auth::{JwtService, TokenType};
use synctv_media_providers::grpc::alist::{alist_server::AlistServer, MeResp as AlistMeResp};
use synctv_proto::client::ServerMessage;
use tokio::process::Command;
use tokio_tungstenite::tungstenite;
use tonic::transport::Server;
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::health_client::HealthClient;
use tonic_health::pb::HealthCheckRequest;

const LIVE_JWT_SECRET: &str = "test-jwt-secret-value-with-high-entropy-1234567890";
const MANAGEMENT_BASE_URL: &str = "http://127.0.0.1:15052";
const MANAGEMENT_AUTH_TOKEN: &str = "mgmt-test-token";
const PROVIDER_PROBE_SECRET: &str = "provider-remote-e2e-secret";
const ROOT_INTERNAL_USER_ID: i64 = 1;
const ALICE_INTERNAL_USER_ID: i64 = 2;
const ROOM_ID: &str = "room_1";

static PASSWORD_SIGNUP_SETTINGS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static AUTH_RATE_LIMIT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct TestOpaqueCipherSuite;

impl CipherSuite for TestOpaqueCipherSuite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeyExchange = opaque_ke::TripleDh<opaque_ke::Ristretto255, Sha512>;
    type Ksf = Argon2<'static>;
}

#[derive(Clone)]
struct GrpcAuthProbeAlistService {
    expected_secret: std::sync::Arc<str>,
}

impl GrpcAuthProbeAlistService {
    fn new(expected_secret: &str) -> Self {
        Self {
            expected_secret: std::sync::Arc::<str>::from(expected_secret),
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
        Err(tonic::Status::unimplemented("fs_get not needed"))
    }

    async fn fs_list(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::alist::FsListReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::alist::FsListResp>, tonic::Status>
    {
        Err(tonic::Status::unimplemented("fs_list not needed"))
    }

    async fn fs_other(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::alist::FsOtherReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::alist::FsOtherResp>, tonic::Status>
    {
        Err(tonic::Status::unimplemented("fs_other not needed"))
    }

    async fn fs_search(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::alist::FsSearchReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::alist::FsSearchResp>, tonic::Status>
    {
        Err(tonic::Status::unimplemented("fs_search not needed"))
    }
}

fn live_base_url() -> String {
    std::env::var("SYNCTV_LIVE_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:18080".to_string())
        .trim_end_matches('/')
        .to_string()
}

fn test_http_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("HTTP client should build")
}

fn unique_suffix() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos()
        .to_string()
}

fn live_access_token(internal_user_id: i64, password_version: i32) -> String {
    JwtService::new(LIVE_JWT_SECRET)
        .expect("live test JWT service should initialize")
        .sign_token(
            &UserId::expect_positive(internal_user_id),
            TokenType::Access,
            password_version,
        )
        .expect("test access token should sign")
}

async fn response_json(response: Response) -> Value {
    let url = response.url().clone();
    let status = response.status();
    response.json::<Value>().await.unwrap_or_else(|error| {
        panic!("failed to parse JSON from {url} (status {status}): {error}");
    })
}

async fn post_json(client: &Client, url: &str, body: Value, bearer: Option<&str>) -> Response {
    let request = client.post(url).json(&body);
    let request = if let Some(token) = bearer {
        request.bearer_auth(token)
    } else {
        request
    };
    request
        .send()
        .await
        .unwrap_or_else(|error| panic!("HTTP POST to {url} failed: {error}"))
}

async fn put_json(client: &Client, url: &str, body: Value, bearer: Option<&str>) -> Response {
    let request = client.put(url).json(&body);
    let request = if let Some(token) = bearer {
        request.bearer_auth(token)
    } else {
        request
    };
    request
        .send()
        .await
        .unwrap_or_else(|error| panic!("HTTP PUT to {url} failed: {error}"))
}

async fn patch_json(client: &Client, url: &str, body: Value, bearer: Option<&str>) -> Response {
    let request = client.patch(url).json(&body);
    let request = if let Some(token) = bearer {
        request.bearer_auth(token)
    } else {
        request
    };
    request
        .send()
        .await
        .unwrap_or_else(|error| panic!("HTTP PATCH to {url} failed: {error}"))
}

async fn opaque_http_register(
    client: &Client,
    base_url: &str,
    username: &str,
    email: &str,
    password: &str,
) -> Response {
    let mut rng = OsRng;
    let client_start =
        ClientRegistration::<TestOpaqueCipherSuite>::start(&mut rng, password.as_bytes())
            .expect("client OPAQUE registration start should succeed");

    let start = post_json_with_rate_limit_retry(
        client,
        &format!("{base_url}/api/auth/opaque/registration/start"),
        json!({
            "username": username,
            "email": email,
            "registration_request": client_start.message.serialize()
        }),
        None,
    )
    .await;
    if start.status() != StatusCode::OK {
        return start;
    }

    let challenge = response_json(start).await;
    let session_id = challenge["session_id"]
        .as_str()
        .expect("OPAQUE registration start should return session_id")
        .to_string();
    let registration_response: Vec<u8> = serde_json::from_value(
        challenge
            .get("registration_response")
            .cloned()
            .expect("OPAQUE registration start should return registration_response"),
    )
    .expect("registration_response should decode as bytes");
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

    post_json_with_rate_limit_retry(
        client,
        &format!("{base_url}/api/auth/opaque/registration/finish"),
        json!({
            "session_id": session_id,
            "registration_upload": client_finish.message.serialize()
        }),
        None,
    )
    .await
}

async fn opaque_http_login(
    client: &Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<Value, String> {
    let mut rng = OsRng;
    let client_start = ClientLogin::<TestOpaqueCipherSuite>::start(&mut rng, password.as_bytes())
        .expect("client OPAQUE login start should succeed");
    let start = post_json_with_rate_limit_retry(
        client,
        &format!("{base_url}/api/auth/opaque/login/start"),
        json!({
            "username": username,
            "email": "",
            "credential_request": client_start.message.serialize()
        }),
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
    let session_id = challenge["session_id"]
        .as_str()
        .expect("OPAQUE login start should return session_id")
        .to_string();
    let credential_response: Vec<u8> = serde_json::from_value(
        challenge
            .get("credential_response")
            .cloned()
            .expect("OPAQUE login start should return credential_response"),
    )
    .expect("credential_response should decode as bytes");
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
        .map_err(|error| format!("OPAQUE login finalization failed for {username}: {error}"))?;

    let finish = post_json_with_rate_limit_retry(
        client,
        &format!("{base_url}/api/auth/opaque/login/finish"),
        json!({
            "session_id": session_id,
            "credential_finalization": client_finish.message.serialize()
        }),
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

    Ok(body)
}

async fn opaque_http_update_password(
    client: &Client,
    base_url: &str,
    access_token: &str,
    current_password: &str,
    new_password: &str,
) -> (StatusCode, Value) {
    let mut rng = OsRng;
    let login_start =
        ClientLogin::<TestOpaqueCipherSuite>::start(&mut rng, current_password.as_bytes())
            .expect("client OPAQUE password-update login start should succeed");
    let registration_start =
        ClientRegistration::<TestOpaqueCipherSuite>::start(&mut rng, new_password.as_bytes())
            .expect("client OPAQUE password-update registration start should succeed");

    let start = post_json(
        client,
        &format!("{base_url}/api/user/opaque-password/update/start"),
        json!({
            "credential_request": login_start.message.serialize(),
            "registration_request": registration_start.message.serialize(),
            "verification_method": 1,
            "email_token": ""
        }),
        Some(access_token),
    )
    .await;
    let start_status = start.status();
    let start_body = response_json(start).await;
    if start_status != StatusCode::OK {
        return (start_status, start_body);
    }

    let session_id = start_body["session_id"]
        .as_str()
        .expect("OPAQUE password-update start should return session_id")
        .to_string();
    let credential_response: Vec<u8> = serde_json::from_value(
        start_body
            .get("credential_response")
            .cloned()
            .expect("OPAQUE password-update start should return credential_response"),
    )
    .expect("credential_response should decode as bytes");
    let credential_response =
        CredentialResponse::<TestOpaqueCipherSuite>::deserialize(&credential_response)
            .expect("server password-update credential response should deserialize");
    let registration_response: Vec<u8> = serde_json::from_value(
        start_body
            .get("registration_response")
            .cloned()
            .expect("OPAQUE password-update start should return registration_response"),
    )
    .expect("registration_response should decode as bytes");
    let registration_response =
        RegistrationResponse::<TestOpaqueCipherSuite>::deserialize(&registration_response)
            .expect("server password-update registration response should deserialize");

    let login_finish = match login_start.state.finish(
        &mut rng,
        current_password.as_bytes(),
        credential_response,
        ClientLoginFinishParameters::default(),
    ) {
        Ok(finish) => finish,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                json!({ "error": format!("client credential finalization failed: {error}") }),
            );
        }
    };
    let registration_finish = registration_start
        .state
        .finish(
            &mut rng,
            new_password.as_bytes(),
            registration_response,
            ClientRegistrationFinishParameters::default(),
        )
        .expect("client OPAQUE password-update registration finish should succeed");

    let finish = post_json(
        client,
        &format!("{base_url}/api/user/opaque-password/update/finish"),
        json!({
            "session_id": session_id,
            "credential_finalization": login_finish.message.serialize(),
            "registration_upload": registration_finish.message.serialize(),
            "passkey_session_id": "",
            "passkey_credential": []
        }),
        Some(access_token),
    )
    .await;
    let finish_status = finish.status();
    let finish_body = response_json(finish).await;
    (finish_status, finish_body)
}

async fn get_json(client: &Client, url: &str, bearer: Option<&str>) -> Response {
    let request = client.get(url);
    let request = if let Some(token) = bearer {
        request.bearer_auth(token)
    } else {
        request
    };
    request
        .send()
        .await
        .unwrap_or_else(|error| panic!("HTTP GET to {url} failed: {error}"))
}

async fn delete_json(client: &Client, url: &str, body: Value, bearer: Option<&str>) -> Response {
    let request = client.delete(url).json(&body);
    let request = if let Some(token) = bearer {
        request.bearer_auth(token)
    } else {
        request
    };
    request
        .send()
        .await
        .unwrap_or_else(|error| panic!("HTTP DELETE to {url} failed: {error}"))
}

async fn delete_no_body(client: &Client, url: &str, bearer: Option<&str>) -> Response {
    let request = client.delete(url);
    let request = if let Some(token) = bearer {
        request.bearer_auth(token)
    } else {
        request
    };
    request
        .send()
        .await
        .unwrap_or_else(|error| panic!("HTTP DELETE to {url} failed: {error}"))
}

async fn read_sse_event(client: &Client, url: &str, bearer: &str) -> (String, Value) {
    let response = client
        .get(url)
        .bearer_auth(bearer)
        .send()
        .await
        .unwrap_or_else(|error| panic!("HTTP GET to {url} failed: {error}"));
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "SSE watch endpoint should accept {url}"
    );

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let deadline = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => panic!("timed out waiting for SSE event from {url}; buffer={buffer:?}"),
            chunk = stream.next() => {
                let chunk = chunk
                    .unwrap_or_else(|| panic!("SSE stream from {url} ended before first event"))
                    .unwrap_or_else(|error| panic!("SSE stream from {url} failed: {error}"));
                buffer.push_str(
                    std::str::from_utf8(&chunk)
                        .unwrap_or_else(|error| panic!("SSE stream from {url} returned non-UTF8 data: {error}")),
                );
                if let Some((event_name, data)) = parse_first_sse_event(&buffer) {
                    let data = serde_json::from_str(&data)
                        .unwrap_or_else(|error| panic!("SSE data from {url} was not JSON: {data}; {error}"));
                    return (event_name, data);
                }
            }
        }
    }
}

fn parse_first_sse_event(buffer: &str) -> Option<(String, String)> {
    let event_end = buffer.find("\n\n").or_else(|| buffer.find("\r\n\r\n"))?;
    let event = &buffer[..event_end];
    let mut event_name = None;
    let mut data_lines = Vec::new();
    for line in event.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim_start().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_string());
        }
    }
    Some((event_name.unwrap_or_default(), data_lines.join("\n")))
}

async fn ws_connect_with_ticket(
    base_url: &str,
    room_id: &str,
    ticket: &str,
) -> Result<
    (
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tungstenite::handshake::client::Response,
    ),
    tungstenite::Error,
> {
    let addr = base_url
        .strip_prefix("http://")
        .expect("live test only supports ws over http base URL");
    tokio_tungstenite::connect_async(format!(
        "ws://{addr}/ws/rooms/{room_id}?ticket={ticket}&format=protobuf"
    ))
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
                return Some(
                    prost::Message::decode(bytes.as_ref())
                        .expect("websocket binary frame should decode as ServerMessage"),
                );
            }
            Ok(tungstenite::Message::Close(_)) => return None,
            Ok(_) => {}
            Err(error) => panic!("websocket read failed: {error}"),
        }
    }

    None
}

fn retry_after_seconds(body: &Value) -> Option<u64> {
    body.get("error")?
        .as_str()?
        .split_once("Try again in ")?
        .1
        .split_once('s')?
        .0
        .parse()
        .ok()
}

async fn post_json_with_rate_limit_retry(
    client: &Client,
    url: &str,
    body: Value,
    bearer: Option<&str>,
) -> Response {
    let mut attempts = 0;
    loop {
        let response = post_json(client, url, body.clone(), bearer).await;
        if response.status() != StatusCode::TOO_MANY_REQUESTS {
            return response;
        }

        let rate_limit_body = response_json(response).await;
        attempts += 1;
        if attempts > 8 {
            panic!("HTTP POST to {url} remained rate-limited after retries: {rate_limit_body}");
        }

        let wait = retry_after_seconds(&rate_limit_body).unwrap_or(60).min(65) + 1;
        tokio::time::sleep(Duration::from_secs(wait)).await;
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
            _ if Instant::now() < deadline => tokio::time::sleep(Duration::from_millis(50)).await,
            Ok(status) => panic!("provider gRPC health never became SERVING, last status={status}"),
            Err(error) => panic!("provider gRPC health never became ready: {error}"),
        }
    }
}

async fn spawn_authenticated_provider_server(
    auth_secret: &str,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("provider auth test server should bind to an ephemeral port");
    let addr = listener
        .local_addr()
        .expect("provider auth test server should expose a local address");
    let service = GrpcAuthProbeAlistService::new(auth_secret);
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
            .add_service(AlistServer::new(service))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .expect("provider auth test server should run");
    });

    wait_until_grpc_ready(&format!("http://127.0.0.1:{}", addr.port())).await;
    (addr, handle)
}

async fn run_synctv_management_cli(args: &[&str]) -> (std::process::ExitStatus, Value, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_synctv"))
        .args([
            "--no-dotenv",
            "--endpoint",
            MANAGEMENT_BASE_URL,
            "--auth-token",
            MANAGEMENT_AUTH_TOKEN,
        ])
        .args(args)
        .args(["--output", "json"])
        .output()
        .await
        .expect("synctv provider CLI should run");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let body = if stdout.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&stdout).unwrap_or_else(|error| {
            panic!("CLI stdout should be JSON: {stdout}; stderr={stderr}; {error}")
        })
    };
    (output.status, body, stderr)
}

async fn set_password_signup_enabled(enabled: bool) {
    let enable_value = if enabled {
        "enable_password_signup=true"
    } else {
        "enable_password_signup=false"
    };
    let (status, body, stderr) = run_synctv_management_cli(&[
        "settings",
        "update",
        "user",
        "--set",
        enable_value,
        "--set",
        "password_signup_need_review=false",
    ])
    .await;
    assert!(
        status.success(),
        "settings update for password signup should succeed; body={body} stderr={stderr}"
    );
}

async fn opaque_http_register_with_password_signup_enabled(
    client: &Client,
    base_url: &str,
    username: &str,
    email: &str,
    password: &str,
) -> Response {
    let _guard = PASSWORD_SIGNUP_SETTINGS_LOCK.lock().await;
    set_password_signup_enabled(true).await;
    let register = opaque_http_register(client, base_url, username, email, password).await;
    set_password_signup_enabled(false).await;
    register
}

async fn create_live_opaque_user(
    client: &Client,
    base_url: &str,
    username_prefix: &str,
) -> (String, String, String) {
    let username = format!("{username_prefix}_{}", unique_suffix());
    let email = format!("{username}@example.test");
    let password = "OpaqueLiveUserPass12345";
    let register = opaque_http_register_with_password_signup_enabled(
        client, base_url, &username, &email, password,
    )
    .await;
    let register_status = register.status();
    let register_body = response_json(register).await;
    assert_eq!(
        register_status,
        StatusCode::OK,
        "test OPAQUE user registration should succeed: {register_body}"
    );
    let access_token = register_body["access_token"]
        .as_str()
        .expect("test OPAQUE registration should return access_token")
        .to_string();
    (username, email, access_token)
}

#[tokio::test]
#[ignore = "requires a live synctv serve instance; set SYNCTV_LIVE_BASE_URL if not using 127.0.0.1:18080"]
async fn live_opaque_registration_login_refresh_and_logout_round_trip() {
    let _auth_guard = AUTH_RATE_LIMIT_LOCK.lock().await;
    let base_url = live_base_url();
    let client = test_http_client();
    let suffix = unique_suffix();
    let username = format!("live_opaque_{suffix}");
    let email = format!("{username}@example.test");
    let password = "OpaqueLivePass12345";

    let register = opaque_http_register_with_password_signup_enabled(
        &client, &base_url, &username, &email, password,
    )
    .await;
    let register_status = register.status();
    let register_body = response_json(register).await;
    assert_eq!(
        register_status,
        StatusCode::OK,
        "OPAQUE registration should succeed; enable user.enable_password_signup first if this fails: {register_body}"
    );
    assert!(
        register_body["access_token"]
            .as_str()
            .is_some_and(|token| !token.is_empty()),
        "registration response should include an access token: {register_body}"
    );
    assert!(
        register_body["refresh_token"]
            .as_str()
            .is_some_and(|token| !token.is_empty()),
        "registration response should include a refresh token: {register_body}"
    );
    let registration_access_token = register_body["access_token"]
        .as_str()
        .expect("registration should return access_token")
        .to_string();
    let registration_refresh_token = register_body["refresh_token"]
        .as_str()
        .expect("registration should return refresh_token")
        .to_string();

    let profile = get_json(
        &client,
        &format!("{base_url}/api/user"),
        Some(&registration_access_token),
    )
    .await;
    let profile_status = profile.status();
    let profile_body = response_json(profile).await;
    assert_eq!(
        profile_status,
        StatusCode::OK,
        "fresh OPAQUE access token should authenticate /api/user: {profile_body}"
    );
    assert_eq!(profile_body["user"]["username"], username);

    let refresh = post_json_with_rate_limit_retry(
        &client,
        &format!("{base_url}/api/auth/refresh"),
        json!({ "refresh_token": registration_refresh_token }),
        None,
    )
    .await;
    let refresh_status = refresh.status();
    let refresh_body = response_json(refresh).await;
    assert_eq!(
        refresh_status,
        StatusCode::OK,
        "refresh token should mint a new access token: {refresh_body}"
    );
    let refreshed_access_token = refresh_body["access_token"]
        .as_str()
        .expect("refresh response should return access_token")
        .to_string();

    let logout = post_json(
        &client,
        &format!("{base_url}/api/user/logout"),
        json!({}),
        Some(&refreshed_access_token),
    )
    .await;
    let logout_status = logout.status();
    let logout_body = response_json(logout).await;
    assert_eq!(
        logout_status,
        StatusCode::OK,
        "logout should accept refreshed access token: {logout_body}"
    );
    assert_eq!(logout_body["success"], true);

    let old_profile = get_json(
        &client,
        &format!("{base_url}/api/user"),
        Some(&refreshed_access_token),
    )
    .await;
    assert_eq!(
        old_profile.status(),
        StatusCode::UNAUTHORIZED,
        "logout should immediately blacklist the access token"
    );

    let login = opaque_http_login(&client, &base_url, &username, password)
        .await
        .expect("OPAQUE login should succeed");
    assert!(
        login["access_token"]
            .as_str()
            .is_some_and(|token| !token.is_empty()),
        "OPAQUE login should return an access token: {login}"
    );

    let wrong_password = opaque_http_login(&client, &base_url, &username, "WrongOpaquePass12345")
        .await
        .expect_err("OPAQUE login should fail before token issuance for a wrong password");
    assert!(
        wrong_password.contains("finalization failed") || wrong_password.contains("finish"),
        "wrong-password OPAQUE login should fail clearly, got: {wrong_password}"
    );
}

#[tokio::test]
#[ignore = "requires the live cluster server seeded by the management/HTTP live run"]
async fn live_user_opaque_password_update_invalidates_old_password_and_tokens() {
    let _auth_guard = AUTH_RATE_LIMIT_LOCK.lock().await;
    let base_url = live_base_url();
    let client = test_http_client();
    let suffix = unique_suffix();
    let username = format!("live_pwd_update_{suffix}");
    let email = format!("{username}@example.test");
    let old_password = "OpaqueOldPass12345";
    let new_password = "OpaqueNewPass12345";

    let mut rng = OsRng;
    let no_auth_login_start =
        ClientLogin::<TestOpaqueCipherSuite>::start(&mut rng, old_password.as_bytes())
            .expect("client OPAQUE login start should succeed");
    let no_auth_registration_start =
        ClientRegistration::<TestOpaqueCipherSuite>::start(&mut rng, new_password.as_bytes())
            .expect("client OPAQUE registration start should succeed");
    let invalid_no_auth = post_json(
        &client,
        &format!("{base_url}/api/user/opaque-password/update/start"),
        json!({
            "credential_request": no_auth_login_start.message.serialize(),
            "registration_request": no_auth_registration_start.message.serialize(),
            "verification_method": 1,
            "email_token": ""
        }),
        None,
    )
    .await;
    let invalid_no_auth_status = invalid_no_auth.status();
    let invalid_no_auth_body = response_json(invalid_no_auth).await;
    assert_eq!(
        invalid_no_auth_status,
        StatusCode::UNAUTHORIZED,
        "password update start must require bearer auth: {invalid_no_auth_body}"
    );

    let register = opaque_http_register_with_password_signup_enabled(
        &client,
        &base_url,
        &username,
        &email,
        old_password,
    )
    .await;
    let register_status = register.status();
    let register_body = response_json(register).await;
    assert_eq!(
        register_status,
        StatusCode::OK,
        "OPAQUE registration for password-update test should succeed: {register_body}"
    );
    let old_access_token = register_body["access_token"]
        .as_str()
        .expect("registration should return access_token")
        .to_string();

    let bogus_registration_start =
        ClientRegistration::<TestOpaqueCipherSuite>::start(&mut rng, new_password.as_bytes())
            .expect("client OPAQUE registration start should succeed");
    let unsupported = post_json(
        &client,
        &format!("{base_url}/api/user/opaque-password/update/start"),
        json!({
            "credential_request": [],
            "registration_request": bogus_registration_start.message.serialize(),
            "verification_method": 0,
            "email_token": ""
        }),
        Some(&old_access_token),
    )
    .await;
    assert_eq!(
        unsupported.status(),
        StatusCode::BAD_REQUEST,
        "verification_method=0 should be rejected before creating an update session"
    );

    let (update_status, update_body) = opaque_http_update_password(
        &client,
        &base_url,
        &old_access_token,
        old_password,
        new_password,
    )
    .await;
    assert_eq!(
        update_status,
        StatusCode::OK,
        "OPAQUE password update with current password should succeed: {update_body}"
    );
    assert_eq!(update_body["user"]["username"], username);

    let old_token_profile = get_json(
        &client,
        &format!("{base_url}/api/user"),
        Some(&old_access_token),
    )
    .await;
    assert_eq!(
        old_token_profile.status(),
        StatusCode::UNAUTHORIZED,
        "password update should invalidate pre-update access tokens via password_version"
    );

    let old_password_login = opaque_http_login(&client, &base_url, &username, old_password)
        .await
        .expect_err("old OPAQUE password should no longer authenticate after password update");
    assert!(
        old_password_login.contains("finalization failed") || old_password_login.contains("finish"),
        "old password failure should be an OPAQUE authentication failure, got: {old_password_login}"
    );

    let new_password_login = opaque_http_login(&client, &base_url, &username, new_password)
        .await
        .expect("new OPAQUE password should authenticate after password update");
    let new_access_token = new_password_login["access_token"]
        .as_str()
        .expect("new-password login should return access_token")
        .to_string();
    let new_profile = get_json(
        &client,
        &format!("{base_url}/api/user"),
        Some(&new_access_token),
    )
    .await;
    let new_profile_status = new_profile.status();
    let new_profile_body = response_json(new_profile).await;
    assert_eq!(
        new_profile_status,
        StatusCode::OK,
        "new password access token should authenticate /api/user: {new_profile_body}"
    );
    assert_eq!(new_profile_body["user"]["username"], username);
}

#[tokio::test]
#[ignore = "requires the live cluster server seeded by the management/HTTP live run"]
async fn live_user_auxiliary_http_endpoints_cover_auth_validation_and_disabled_services() {
    let base_url = live_base_url();
    let client = test_http_client();
    let root_token = live_access_token(ROOT_INTERNAL_USER_ID, 0);

    let user_rooms_no_auth = get_json(&client, &format!("{base_url}/api/user/rooms"), None).await;
    assert_eq!(
        user_rooms_no_auth.status(),
        StatusCode::UNAUTHORIZED,
        "list-my-rooms must require bearer auth"
    );

    let user_rooms = get_json(
        &client,
        &format!(
            "{base_url}/api/user/rooms?include_owned=true&include_joined=true&page=1&page_size=10"
        ),
        Some(&root_token),
    )
    .await;
    let user_rooms_status = user_rooms.status();
    let user_rooms_body = response_json(user_rooms).await;
    assert_eq!(
        user_rooms_status,
        StatusCode::OK,
        "authenticated user should list related rooms: {user_rooms_body}"
    );
    assert!(
        user_rooms_body.get("rooms").is_some() && user_rooms_body.get("total").is_some(),
        "list-my-rooms response should contain rooms and total: {user_rooms_body}"
    );

    let passkeys_no_auth = get_json(&client, &format!("{base_url}/api/user/passkeys"), None).await;
    assert_eq!(
        passkeys_no_auth.status(),
        StatusCode::UNAUTHORIZED,
        "passkey list must require bearer auth"
    );

    let passkeys = get_json(
        &client,
        &format!("{base_url}/api/user/passkeys"),
        Some(&root_token),
    )
    .await;
    let passkeys_status = passkeys.status();
    let passkeys_body = response_json(passkeys).await;
    if passkeys_status == StatusCode::SERVICE_UNAVAILABLE {
        assert_eq!(
            passkeys_body["status"], 503,
            "passkey-disabled response should use the normalized 503 error shape: {passkeys_body}"
        );
    } else {
        assert_eq!(
            passkeys_status,
            StatusCode::OK,
            "passkey list should either succeed or report disabled service: {passkeys_body}"
        );
        assert!(
            passkeys_body.get("credentials").is_some(),
            "passkey list response should include credentials: {passkeys_body}"
        );
    }

    let passkey_bind_no_auth = post_json(
        &client,
        &format!("{base_url}/api/user/passkeys/bind/start"),
        json!({ "name": "No Auth Device" }),
        None,
    )
    .await;
    assert_eq!(
        passkey_bind_no_auth.status(),
        StatusCode::UNAUTHORIZED,
        "passkey bind start must require bearer auth"
    );

    let passkey_bind = post_json(
        &client,
        &format!("{base_url}/api/user/passkeys/bind/start"),
        json!({ "name": "Live Test Device" }),
        Some(&root_token),
    )
    .await;
    let passkey_bind_status = passkey_bind.status();
    let passkey_bind_body = response_json(passkey_bind).await;
    assert!(
        passkey_bind_status == StatusCode::OK
            || passkey_bind_status == StatusCode::SERVICE_UNAVAILABLE,
        "passkey bind start should succeed when configured or fail closed when disabled: {passkey_bind_body}"
    );

    let passkey_finish_bad = post_json(
        &client,
        &format!("{base_url}/api/user/passkeys/bind/finish"),
        json!({ "session_id": "missing-session", "credential": [] }),
        Some(&root_token),
    )
    .await;
    assert_eq!(
        passkey_finish_bad.status(),
        StatusCode::BAD_REQUEST,
        "empty passkey credential should fail proto validation"
    );

    let passkey_delete_no_auth = delete_no_body(
        &client,
        &format!("{base_url}/api/user/passkeys/Y3JlZGVudGlhbA"),
        None,
    )
    .await;
    assert_eq!(
        passkey_delete_no_auth.status(),
        StatusCode::UNAUTHORIZED,
        "passkey delete must require bearer auth"
    );

    let oauth_providers =
        get_json(&client, &format!("{base_url}/api/oauth2/providers"), None).await;
    let oauth_providers_status = oauth_providers.status();
    let oauth_providers_body = response_json(oauth_providers).await;
    assert!(
        oauth_providers_status == StatusCode::OK
            || oauth_providers_status == StatusCode::SERVICE_UNAVAILABLE,
        "OAuth2 provider list should either report providers or disabled service: {oauth_providers_body}"
    );
    if oauth_providers_status == StatusCode::OK {
        assert!(
            oauth_providers_body.get("providers").is_some(),
            "OAuth2 providers response should include providers: {oauth_providers_body}"
        );
    }

    let oauth_bad_redirect = get_json(
        &client,
        &format!("{base_url}/api/oauth2/example/authorize?redirect_url=javascript:alert(1)"),
        None,
    )
    .await;
    assert_eq!(
        oauth_bad_redirect.status(),
        StatusCode::BAD_REQUEST,
        "OAuth2 authorize should reject unsafe redirect_url at query validation"
    );

    let oauth_bind_no_auth = get_json(
        &client,
        &format!("{base_url}/api/oauth2/example/bind?redirect_url=https://client.example/callback"),
        None,
    )
    .await;
    assert_eq!(
        oauth_bind_no_auth.status(),
        StatusCode::UNAUTHORIZED,
        "OAuth2 bind authorize URL must require bearer auth when request validates"
    );

    let oauth_exchange_bad = {
        let _auth_guard = AUTH_RATE_LIMIT_LOCK.lock().await;
        post_json_with_rate_limit_retry(
            &client,
            &format!("{base_url}/api/oauth2/example/exchange"),
            json!({ "code": "", "state": "short" }),
            None,
        )
        .await
    };
    assert_eq!(
        oauth_exchange_bad.status(),
        StatusCode::BAD_REQUEST,
        "OAuth2 exchange should reject malformed code/state before provider lookup"
    );

    let linked_no_auth = get_json(&client, &format!("{base_url}/api/oauth2/linked"), None).await;
    assert_eq!(
        linked_no_auth.status(),
        StatusCode::UNAUTHORIZED,
        "linked OAuth2 providers must require bearer auth"
    );

    let linked = get_json(
        &client,
        &format!("{base_url}/api/oauth2/linked"),
        Some(&root_token),
    )
    .await;
    let linked_status = linked.status();
    let linked_body = response_json(linked).await;
    assert!(
        linked_status == StatusCode::OK || linked_status == StatusCode::SERVICE_UNAVAILABLE,
        "linked OAuth2 providers should either list bindings or report disabled service: {linked_body}"
    );
    if linked_status == StatusCode::OK {
        assert!(
            linked_body.get("providers").is_some(),
            "linked OAuth2 providers response should include providers: {linked_body}"
        );
    }

    let unlink_no_auth = delete_no_body(
        &client,
        &format!("{base_url}/api/oauth2/type/github/unlink"),
        None,
    )
    .await;
    assert_eq!(
        unlink_no_auth.status(),
        StatusCode::UNAUTHORIZED,
        "OAuth2 unlink must require bearer auth"
    );

    let unlink_bad_query = delete_no_body(
        &client,
        &format!("{base_url}/api/oauth2/type/github/unlink?provider_user_id=external-user"),
        Some(&root_token),
    )
    .await;
    assert_eq!(
        unlink_bad_query.status(),
        StatusCode::BAD_REQUEST,
        "OAuth2 unlink should require provider_instance_name with provider_user_id"
    );

    let notifications = get_json(
        &client,
        &format!("{base_url}/api/notifications?page=1&page_size=10&sort_by=1&sort_direction=1"),
        Some(&root_token),
    )
    .await;
    let notifications_status = notifications.status();
    let notifications_body = response_json(notifications).await;
    assert_eq!(
        notifications_status,
        StatusCode::OK,
        "authenticated user should list notifications: {notifications_body}"
    );
    assert!(
        notifications_body.get("notifications").is_some()
            && notifications_body.get("total").is_some()
            && notifications_body.get("unread_count").is_some(),
        "notification list response should include notifications/total/unread_count: {notifications_body}"
    );

    let notifications_bad_query = get_json(
        &client,
        &format!("{base_url}/api/notifications?page=0&page_size=101"),
        Some(&root_token),
    )
    .await;
    assert_eq!(
        notifications_bad_query.status(),
        StatusCode::BAD_REQUEST,
        "notification list should reject page_size > 100"
    );

    let missing_notification = get_json(
        &client,
        &format!("{base_url}/api/notifications/999999999"),
        Some(&root_token),
    )
    .await;
    assert_eq!(
        missing_notification.status(),
        StatusCode::NOT_FOUND,
        "missing notification get should return 404"
    );

    let bad_notification_path = get_json(
        &client,
        &format!("{base_url}/api/notifications/0"),
        Some(&root_token),
    )
    .await;
    assert_eq!(
        bad_notification_path.status(),
        StatusCode::BAD_REQUEST,
        "notification id=0 should fail path validation"
    );

    let mark_bad = post_json(
        &client,
        &format!("{base_url}/api/notifications/actions/mark-read"),
        json!({ "notification_ids": [0] }),
        Some(&root_token),
    )
    .await;
    assert_eq!(
        mark_bad.status(),
        StatusCode::BAD_REQUEST,
        "mark-read should reject non-positive notification ids"
    );

    let mark_all = post_json(
        &client,
        &format!("{base_url}/api/notifications/read-all"),
        json!({}),
        Some(&root_token),
    )
    .await;
    assert_eq!(
        mark_all.status(),
        StatusCode::NO_CONTENT,
        "mark-all notifications read should be idempotent"
    );

    let delete_missing = delete_no_body(
        &client,
        &format!("{base_url}/api/notifications/999999999"),
        Some(&root_token),
    )
    .await;
    assert_eq!(
        delete_missing.status(),
        StatusCode::NOT_FOUND,
        "missing notification delete should return 404"
    );

    let delete_all_read = delete_no_body(
        &client,
        &format!("{base_url}/api/notifications/read"),
        Some(&root_token),
    )
    .await;
    assert_eq!(
        delete_all_read.status(),
        StatusCode::NO_CONTENT,
        "delete-all-read notifications should be idempotent"
    );
}

#[tokio::test]
#[ignore = "requires the live cluster server seeded by the management/HTTP live run"]
async fn live_websocket_ticket_is_single_use_and_watch_sse_returns_snapshots() {
    let base_url = live_base_url();
    let client = test_http_client();
    let alice_token = live_access_token(ALICE_INTERNAL_USER_ID, 0);
    let root_token = live_access_token(ROOT_INTERNAL_USER_ID, 0);

    let no_auth_ticket = post_json(
        &client,
        &format!("{base_url}/api/tickets"),
        json!({ "room_id": ROOM_ID }),
        None,
    )
    .await;
    assert_eq!(
        no_auth_ticket.status(),
        StatusCode::UNAUTHORIZED,
        "ticket creation must require bearer auth"
    );

    let create_ticket = post_json(
        &client,
        &format!("{base_url}/api/tickets"),
        json!({ "room_id": ROOM_ID }),
        Some(&alice_token),
    )
    .await;
    let create_ticket_status = create_ticket.status();
    let create_ticket_body = response_json(create_ticket).await;
    assert_eq!(
        create_ticket_status,
        StatusCode::OK,
        "room member should be able to create websocket ticket: {create_ticket_body}"
    );
    let ticket = create_ticket_body["ticket"]
        .as_str()
        .expect("ticket response should include ticket")
        .to_string();
    assert_eq!(create_ticket_body["room_id"], ROOM_ID);
    assert_eq!(create_ticket_body["expires_in_secs"], 30);

    let (mut ws, response) = ws_connect_with_ticket(&base_url, ROOM_ID, &ticket)
        .await
        .expect("fresh websocket ticket should upgrade");
    assert_eq!(
        response.status(),
        tungstenite::http::StatusCode::SWITCHING_PROTOCOLS
    );
    let initial_message =
        tokio::time::timeout(Duration::from_secs(5), recv_server_message(&mut ws))
            .await
            .expect("timed out waiting for websocket initial message")
            .expect("websocket should send an initial server message");
    assert!(
        initial_message.message.is_some(),
        "websocket initial message should have a protobuf payload"
    );
    drop(ws);

    let reused = ws_connect_with_ticket(&base_url, ROOM_ID, &ticket)
        .await
        .expect_err("websocket ticket must be single-use");
    assert!(
        reused.to_string().contains("401") || reused.to_string().contains("HTTP error"),
        "reused websocket ticket should be rejected as unauthorized, got: {reused}"
    );

    let bad_room_ticket = post_json(
        &client,
        &format!("{base_url}/api/tickets"),
        json!({ "room_id": "room_notreal" }),
        Some(&alice_token),
    )
    .await;
    assert_eq!(
        bad_room_ticket.status(),
        StatusCode::BAD_REQUEST,
        "ticket creation for an invalid public room id should be 400"
    );

    for path in [
        "watch/playback-state?format=json",
        "watch/playback-snapshot?format=json&delivery_preference=auto&video_codecs=h264,hevc&containers=mp4",
        "watch/room-settings?format=json",
        "watch/playlist-items?format=json&page=1&page_size=20",
        "watch/room-members?format=json&page=1&page_size=20",
    ] {
        let (event_name, data) = read_sse_event(
            &client,
            &format!("{base_url}/api/rooms/{ROOM_ID}/{path}"),
            &alice_token,
        )
        .await;
        assert!(
            event_name == "observed" || event_name == "changed",
            "watch endpoint {path} should return observed/changed, got event={event_name} data={data}"
        );
        assert!(
            data.get("observe_id").is_some(),
            "watch endpoint {path} should include observe_id: {data}"
        );
    }

    let invalid_delivery = get_json(
        &client,
        &format!("{base_url}/api/rooms/{ROOM_ID}/watch/playback-state?delivery_mode=bogus"),
        Some(&alice_token),
    )
    .await;
    assert_eq!(
        invalid_delivery.status(),
        StatusCode::BAD_REQUEST,
        "invalid watch delivery_mode should be rejected"
    );

    let invalid_snapshot_profile = get_json(
        &client,
        &format!("{base_url}/api/rooms/{ROOM_ID}/watch/playback-snapshot?video_codecs=bogus"),
        Some(&alice_token),
    )
    .await;
    assert_eq!(
        invalid_snapshot_profile.status(),
        StatusCode::BAD_REQUEST,
        "invalid playback snapshot watch profile should be rejected"
    );

    let root_forbidden = get_json(
        &client,
        &format!("{base_url}/api/rooms/{ROOM_ID}/watch/room-members?format=json"),
        Some(&root_token),
    )
    .await;
    assert!(
        root_forbidden.status() == StatusCode::FORBIDDEN
            || root_forbidden.status() == StatusCode::NOT_FOUND,
        "non-member root token should not be able to watch room member resource directly"
    );
}

#[tokio::test]
#[ignore = "requires the live cluster server seeded by the management/HTTP live run"]
async fn live_admin_reviews_and_batch_endpoints_cover_permissions_and_partial_failures() {
    let base_url = live_base_url();
    let client = test_http_client();
    let root_token = live_access_token(ROOT_INTERNAL_USER_ID, 0);
    let alice_token = live_access_token(ALICE_INTERNAL_USER_ID, 0);
    let suffix = unique_suffix();

    for review_path in ["user-registrations", "room-creations", "room-joins"] {
        let no_auth = get_json(
            &client,
            &format!("{base_url}/api/admin/reviews/{review_path}"),
            None,
        )
        .await;
        assert_eq!(
            no_auth.status(),
            StatusCode::UNAUTHORIZED,
            "admin review list should require auth for {review_path}"
        );

        let non_admin = get_json(
            &client,
            &format!("{base_url}/api/admin/reviews/{review_path}"),
            Some(&alice_token),
        )
        .await;
        assert_eq!(
            non_admin.status(),
            StatusCode::FORBIDDEN,
            "admin review list should reject non-admin users for {review_path}"
        );

        let root = get_json(
            &client,
            &format!("{base_url}/api/admin/reviews/{review_path}?page=1&page_size=10"),
            Some(&root_token),
        )
        .await;
        let root_status = root.status();
        let root_body = response_json(root).await;
        assert_eq!(
            root_status,
            StatusCode::OK,
            "root should list review queue {review_path}: {root_body}"
        );
        assert!(
            root_body.get("reviews").is_some() && root_body.get("total").is_some(),
            "review list response should contain reviews and total: {root_body}"
        );
    }

    let user_a = post_json(
        &client,
        &format!("{base_url}/api/admin/users"),
        json!({
            "username": format!("http_batch_a_{suffix}"),
            "password": "HttpBatchPass12345",
            "email": format!("http-batch-a-{suffix}@example.test"),
            "role": 3,
            "status": 1
        }),
        Some(&root_token),
    )
    .await;
    let user_a_status = user_a.status();
    let user_a_body = response_json(user_a).await;
    assert_eq!(
        user_a_status,
        StatusCode::OK,
        "create user A: {user_a_body}"
    );
    let user_a_id = user_a_body["user"]["id"].as_str().expect("user A id");

    let user_b = post_json(
        &client,
        &format!("{base_url}/api/admin/users"),
        json!({
            "username": format!("http_batch_b_{suffix}"),
            "password": "HttpBatchPass12345",
            "email": format!("http-batch-b-{suffix}@example.test"),
            "role": 3,
            "status": 1
        }),
        Some(&root_token),
    )
    .await;
    let user_b_status = user_b.status();
    let user_b_body = response_json(user_b).await;
    assert_eq!(
        user_b_status,
        StatusCode::OK,
        "create user B: {user_b_body}"
    );
    let user_b_id = user_b_body["user"]["id"].as_str().expect("user B id");

    let user_forbidden = post_json(
        &client,
        &format!("{base_url}/api/admin/users/batch/ban"),
        json!({ "user_ids": [user_a_id], "reason": "non-admin should fail" }),
        Some(&alice_token),
    )
    .await;
    assert_eq!(
        user_forbidden.status(),
        StatusCode::FORBIDDEN,
        "non-admin user must not batch ban users"
    );

    let ban_users = post_json(
        &client,
        &format!("{base_url}/api/admin/users/batch/ban"),
        json!({
            "user_ids": [user_a_id, user_b_id, "usr_999999"],
            "reason": "http batch live test"
        }),
        Some(&root_token),
    )
    .await;
    let ban_users_status = ban_users.status();
    let ban_users_body = response_json(ban_users).await;
    assert_eq!(
        ban_users_status,
        StatusCode::OK,
        "batch ban users should return per-item results: {ban_users_body}"
    );
    assert_eq!(ban_users_body["succeeded"], 2);
    assert_eq!(ban_users_body["failed"], 1);

    let delete_users_forbidden = post_json(
        &client,
        &format!("{base_url}/api/admin/users/batch/delete"),
        json!({ "user_ids": [user_a_id] }),
        Some(&alice_token),
    )
    .await;
    assert_eq!(
        delete_users_forbidden.status(),
        StatusCode::FORBIDDEN,
        "batch delete users is root-only"
    );

    let delete_users = post_json(
        &client,
        &format!("{base_url}/api/admin/users/batch/delete"),
        json!({ "user_ids": [user_a_id, user_b_id] }),
        Some(&root_token),
    )
    .await;
    let delete_users_status = delete_users.status();
    let delete_users_body = response_json(delete_users).await;
    assert_eq!(
        delete_users_status,
        StatusCode::OK,
        "batch delete users should return per-item results: {delete_users_body}"
    );
    assert_eq!(delete_users_body["succeeded"], 2);
    assert_eq!(delete_users_body["failed"], 0);

    let room_a = post_json(
        &client,
        &format!("{base_url}/api/rooms"),
        json!({
            "name": format!("HTTP Batch Room A {suffix}"),
            "password": "",
            "settings": [],
            "description": "admin batch room live test"
        }),
        Some(&alice_token),
    )
    .await;
    let room_a_status = room_a.status();
    let room_a_body = response_json(room_a).await;
    assert_eq!(
        room_a_status,
        StatusCode::OK,
        "create room A: {room_a_body}"
    );
    let room_a_id = room_a_body["room"]["id"].as_str().expect("room A id");

    let room_b = post_json(
        &client,
        &format!("{base_url}/api/rooms"),
        json!({
            "name": format!("HTTP Batch Room B {suffix}"),
            "password": "",
            "settings": [],
            "description": "admin batch room live test"
        }),
        Some(&alice_token),
    )
    .await;
    let room_b_status = room_b.status();
    let room_b_body = response_json(room_b).await;
    assert_eq!(
        room_b_status,
        StatusCode::OK,
        "create room B: {room_b_body}"
    );
    let room_b_id = room_b_body["room"]["id"].as_str().expect("room B id");

    let ban_rooms = post_json(
        &client,
        &format!("{base_url}/api/admin/rooms/batch/ban"),
        json!({
            "room_ids": [room_a_id, room_b_id, "room_999999"],
            "reason": "http batch live test"
        }),
        Some(&root_token),
    )
    .await;
    let ban_rooms_status = ban_rooms.status();
    let ban_rooms_body = response_json(ban_rooms).await;
    assert_eq!(
        ban_rooms_status,
        StatusCode::OK,
        "batch ban rooms should return per-item results: {ban_rooms_body}"
    );
    assert_eq!(ban_rooms_body["succeeded"], 2);
    assert_eq!(ban_rooms_body["failed"], 1);

    let delete_rooms = post_json(
        &client,
        &format!("{base_url}/api/admin/rooms/batch/delete"),
        json!({ "room_ids": [room_a_id, room_b_id] }),
        Some(&root_token),
    )
    .await;
    let delete_rooms_status = delete_rooms.status();
    let delete_rooms_body = response_json(delete_rooms).await;
    assert_eq!(
        delete_rooms_status,
        StatusCode::OK,
        "batch delete rooms should return per-item results: {delete_rooms_body}"
    );
    assert_eq!(delete_rooms_body["succeeded"], 2);
    assert_eq!(delete_rooms_body["failed"], 0);

    let bad_batch = post_json(
        &client,
        &format!("{base_url}/api/admin/users/batch/ban"),
        json!({ "user_ids": [], "reason": "empty should fail validation" }),
        Some(&root_token),
    )
    .await;
    assert_eq!(
        bad_batch.status(),
        StatusCode::BAD_REQUEST,
        "empty batch user list should fail proto validation"
    );

    let _ = delete_json(
        &client,
        &format!("{base_url}/api/admin/users/{user_a_id}"),
        json!({}),
        Some(&root_token),
    )
    .await;
}

#[tokio::test]
#[ignore = "requires live server restarted with SYNCTV_SECURITY_SSRF_ALLOW_PRIVATE_NETWORK_TARGETS=true"]
async fn live_remote_provider_cli_lifecycle_succeeds_with_allowed_local_provider() {
    let (provider_addr, provider_handle) =
        spawn_authenticated_provider_server(PROVIDER_PROBE_SECRET).await;
    let provider_name = format!("live_provider_{}", unique_suffix());
    let provider_endpoint = format!("http://127.0.0.1:{}", provider_addr.port());

    let (create_status, create_body, create_stderr) = run_synctv_management_cli(&[
        "provider",
        "create",
        &provider_name,
        &provider_endpoint,
        "--provider",
        "alist",
        "--comment",
        "live remote provider lifecycle",
        "--jwt-secret",
        PROVIDER_PROBE_SECRET,
    ])
    .await;
    assert!(
        create_status.success(),
        "provider create should succeed when SSRF allows local provider; body={create_body} stderr={create_stderr}"
    );
    assert_eq!(create_body["instance"]["name"], provider_name);
    assert_eq!(create_body["instance"]["enabled"], true);
    assert_eq!(create_body["instance"]["providers"], json!(["alist"]));

    let (list_status, list_body, list_stderr) =
        run_synctv_management_cli(&["provider", "list", "--provider-type", "alist"]).await;
    assert!(
        list_status.success(),
        "provider list should succeed; stderr={list_stderr}"
    );
    assert!(
        list_body["instances"]
            .as_array()
            .expect("provider list instances should be array")
            .iter()
            .any(|instance| instance["name"] == provider_name),
        "provider list should include created instance: {list_body}"
    );

    let (reconnect_status, reconnect_body, reconnect_stderr) =
        run_synctv_management_cli(&["provider", "reconnect", &provider_name]).await;
    assert!(
        reconnect_status.success(),
        "provider reconnect should succeed; body={reconnect_body} stderr={reconnect_stderr}"
    );

    let (disable_status, disable_body, disable_stderr) =
        run_synctv_management_cli(&["provider", "disable", &provider_name]).await;
    assert!(
        disable_status.success(),
        "provider disable should succeed; body={disable_body} stderr={disable_stderr}"
    );
    assert_eq!(disable_body["instance"]["enabled"], false);

    let (enable_status, enable_body, enable_stderr) =
        run_synctv_management_cli(&["provider", "enable", &provider_name]).await;
    assert!(
        enable_status.success(),
        "provider enable should succeed; body={enable_body} stderr={enable_stderr}"
    );
    assert_eq!(enable_body["instance"]["enabled"], true);

    let (delete_status, delete_body, delete_stderr) =
        run_synctv_management_cli(&["provider", "delete", &provider_name]).await;
    assert!(
        delete_status.success(),
        "provider delete should succeed; body={delete_body} stderr={delete_stderr}"
    );
    assert_eq!(delete_body["success"], true);

    provider_handle.abort();
}

#[tokio::test]
#[ignore = "requires the live cluster server seeded by the management/HTTP live run"]
async fn live_guest_join_review_email_and_rtmp_edges_cover_complex_http_paths() {
    let _auth_guard = AUTH_RATE_LIMIT_LOCK.lock().await;
    let base_url = live_base_url();
    let client = test_http_client();
    let (_owner_username, _owner_email, owner_token) =
        create_live_opaque_user(&client, &base_url, "live_owner").await;
    let (_joiner_username, _joiner_email, joiner_token) =
        create_live_opaque_user(&client, &base_url, "live_joiner").await;
    let suffix = unique_suffix();

    let guest_bad_room = post_json_with_rate_limit_retry(
        &client,
        &format!("{base_url}/api/auth/guest-token"),
        json!({ "room_id": "not-a-room-id" }),
        None,
    )
    .await;
    assert_eq!(
        guest_bad_room.status(),
        StatusCode::BAD_REQUEST,
        "guest-token should validate public room id format before lookup"
    );

    let create_room = post_json(
        &client,
        &format!("{base_url}/api/rooms"),
        json!({
            "name": format!("Live Guest Review Room {suffix}"),
            "password": "",
            "settings": [],
            "description": "guest/review live http coverage"
        }),
        Some(&owner_token),
    )
    .await;
    let create_room_status = create_room.status();
    let create_room_body = response_json(create_room).await;
    assert_eq!(
        create_room_status,
        StatusCode::OK,
        "owner should create isolated room for guest/review coverage: {create_room_body}"
    );
    let room_id = create_room_body["room"]["id"]
        .as_str()
        .expect("create room should return room id")
        .to_string();

    let guest_disabled = post_json_with_rate_limit_retry(
        &client,
        &format!("{base_url}/api/auth/guest-token"),
        json!({ "room_id": room_id }),
        None,
    )
    .await;
    assert_eq!(
        guest_disabled.status(),
        StatusCode::FORBIDDEN,
        "guest-token should respect default room allow_guest_join=false"
    );

    let enable_guest_and_review = patch_json(
        &client,
        &format!("{base_url}/api/rooms/{room_id}/settings"),
        json!({
            "allow_guest_join": true,
            "require_approval": true
        }),
        Some(&owner_token),
    )
    .await;
    let enable_guest_status = enable_guest_and_review.status();
    let enable_guest_body = response_json(enable_guest_and_review).await;
    assert_eq!(
        enable_guest_status,
        StatusCode::OK,
        "owner should update room guest/review settings: {enable_guest_body}"
    );

    let guest_token = post_json_with_rate_limit_retry(
        &client,
        &format!("{base_url}/api/auth/guest-token"),
        json!({ "room_id": room_id }),
        None,
    )
    .await;
    let guest_token_status = guest_token.status();
    let guest_token_body = response_json(guest_token).await;
    assert_eq!(
        guest_token_status,
        StatusCode::OK,
        "guest-token should be issued after global and room policy allow guests: {guest_token_body}"
    );
    let guest_access_token = guest_token_body["token"]
        .as_str()
        .expect("guest-token response should include token")
        .to_string();
    assert_eq!(guest_token_body["room_id"], room_id);
    assert!(
        guest_token_body["guest_id"]
            .as_str()
            .is_some_and(|value| value.starts_with("gst_")),
        "guest-token response should expose public guest id: {guest_token_body}"
    );

    let guest_room = get_json(
        &client,
        &format!("{base_url}/api/rooms/{room_id}"),
        Some(&guest_access_token),
    )
    .await;
    let guest_room_status = guest_room.status();
    let guest_room_body = response_json(guest_room).await;
    assert_eq!(
        guest_room_status,
        StatusCode::OK,
        "guest bearer token should read the room it is scoped to: {guest_room_body}"
    );

    let join_request = put_json(
        &client,
        &format!("{base_url}/api/rooms/{room_id}/members/@me"),
        json!({ "password": "" }),
        Some(&joiner_token),
    )
    .await;
    let join_request_status = join_request.status();
    let join_request_body = response_json(join_request).await;
    assert_eq!(
        join_request_status,
        StatusCode::OK,
        "joiner should be able to request joining a require_approval room: {join_request_body}"
    );
    assert_eq!(
        join_request_body["requires_approval"], true,
        "join response should flag pending approval: {join_request_body}"
    );
    assert!(
        join_request_body["members"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "pending join should not expose active room members: {join_request_body}"
    );

    let join_reviews_forbidden = get_json(
        &client,
        &format!("{base_url}/api/rooms/{room_id}/reviews/joins?page=1&page_size=10"),
        Some(&joiner_token),
    )
    .await;
    assert_eq!(
        join_reviews_forbidden.status(),
        StatusCode::FORBIDDEN,
        "pending joiner must not list room join reviews"
    );

    let join_reviews = get_json(
        &client,
        &format!("{base_url}/api/rooms/{room_id}/reviews/joins?page=1&page_size=10&status=1"),
        Some(&owner_token),
    )
    .await;
    let join_reviews_status = join_reviews.status();
    let join_reviews_body = response_json(join_reviews).await;
    assert_eq!(
        join_reviews_status,
        StatusCode::OK,
        "room owner should list pending join reviews: {join_reviews_body}"
    );
    let review_id = join_reviews_body["reviews"]
        .as_array()
        .and_then(|reviews| reviews.first())
        .and_then(|review| review["id"].as_str())
        .expect("pending join review should exist")
        .to_string();

    let reject_review_no_auth = post_json(
        &client,
        &format!("{base_url}/api/rooms/{room_id}/reviews/joins/{review_id}/reject"),
        json!({ "reason": "missing auth" }),
        None,
    )
    .await;
    assert_eq!(
        reject_review_no_auth.status(),
        StatusCode::UNAUTHORIZED,
        "join-review rejection must require bearer auth"
    );

    let reject_review = post_json(
        &client,
        &format!("{base_url}/api/rooms/{room_id}/reviews/joins/{review_id}/reject"),
        json!({ "reason": "live test rejection" }),
        Some(&owner_token),
    )
    .await;
    let reject_review_status = reject_review.status();
    let reject_review_body = response_json(reject_review).await;
    assert_eq!(
        reject_review_status,
        StatusCode::OK,
        "room owner should reject pending join review: {reject_review_body}"
    );
    assert_eq!(reject_review_body["success"], true);
    assert_eq!(reject_review_body["review"]["id"], review_id);

    let rtmp_no_auth = post_json(
        &client,
        &format!("{base_url}/api/providers/rtmp/rooms/{room_id}/publish-key/med_999999"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        rtmp_no_auth.status(),
        StatusCode::UNAUTHORIZED,
        "RTMP publish-key endpoint must require bearer auth"
    );

    let rtmp_missing_media = post_json(
        &client,
        &format!("{base_url}/api/providers/rtmp/rooms/{room_id}/publish-key/med_999999"),
        json!({}),
        Some(&owner_token),
    )
    .await;
    assert_eq!(
        rtmp_missing_media.status(),
        StatusCode::NOT_FOUND,
        "RTMP publish-key should return 404 for missing room media"
    );

    let rtmp_info_missing = get_json(
        &client,
        &format!("{base_url}/api/providers/rtmp/rooms/{room_id}/info/med_999999"),
        Some(&owner_token),
    )
    .await;
    let rtmp_info_status = rtmp_info_missing.status();
    let rtmp_info_body = response_json(rtmp_info_missing).await;
    assert_eq!(
        rtmp_info_status,
        StatusCode::OK,
        "RTMP stream info should be readable for room member even when inactive: {rtmp_info_body}"
    );
    assert_eq!(rtmp_info_body["active"], false);

    let email_send_missing_service = post_json(
        &client,
        &format!("{base_url}/api/email/verify/send"),
        json!({ "email": format!("verify-{suffix}@example.test") }),
        None,
    )
    .await;
    assert_eq!(
        email_send_missing_service.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "email verification send should fail closed when email services are not configured"
    );

    let email_confirm_missing_service = post_json(
        &client,
        &format!("{base_url}/api/email/verify/confirm"),
        json!({ "email": format!("verify-{suffix}@example.test"), "token": "123456" }),
        None,
    )
    .await;
    assert_eq!(
        email_confirm_missing_service.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "email confirmation should fail closed when email services are not configured"
    );

    let reset_missing_service = post_json(
        &client,
        &format!("{base_url}/api/email/password/reset"),
        json!({ "email": format!("reset-{suffix}@example.test") }),
        None,
    )
    .await;
    assert_eq!(
        reset_missing_service.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "password reset request should fail closed when email services are not configured"
    );

    let reset_start_bad = post_json(
        &client,
        &format!("{base_url}/api/email/password/opaque/start"),
        json!({
            "email": "",
            "token": "",
            "registration_request": []
        }),
        None,
    )
    .await;
    assert_eq!(
        reset_start_bad.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "OPAQUE password-reset start should fail closed when email services are not configured"
    );

    let reset_finish_bad = post_json(
        &client,
        &format!("{base_url}/api/email/password/opaque/finish"),
        json!({
            "session_id": "",
            "registration_upload": []
        }),
        None,
    )
    .await;
    assert_eq!(
        reset_finish_bad.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "OPAQUE password-reset finish should fail closed when email services are not configured"
    );

    let delete_room = delete_json(
        &client,
        &format!("{base_url}/api/rooms/{room_id}"),
        json!({}),
        Some(&owner_token),
    )
    .await;
    assert!(
        delete_room.status().is_success(),
        "cleanup room delete should succeed or be idempotent enough for live test"
    );
}
