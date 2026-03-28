#![allow(clippy::unwrap_used)]

use std::net::TcpListener;
use std::time::{Duration, Instant};

use futures_util::{stream, StreamExt};
use prost::Message;
use reqwest::StatusCode;
use serde_json::{json, Value};
use synctv::app::Application;
use synctv_core::config::Config;
use synctv_core_testing::{
    create_test_database_url_with_label, start_redis_url_with_label, RedisContainer, TestContainer,
};
use synctv_proto::client::{server_message, ServerMessage};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite;
use tonic::metadata::MetadataValue;
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::health_client::HealthClient;
use tonic_health::pb::HealthCheckRequest;

fn reserve_local_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("ephemeral local_addr")
        .port()
}

fn test_config(
    database_url: String,
    redis_url: String,
    http_port: u16,
    grpc_port: u16,
    rtmp_port: u16,
) -> Config {
    let mut config = Config::default();
    config.server.host = "127.0.0.1".to_string();
    config.server.http_port = http_port;
    config.server.grpc_port = grpc_port;
    config.server.enable_reflection = false;
    config.server.metrics_enabled = false;
    config.server.advertise_host = "127.0.0.1".to_string();
    config.server.shutdown_drain_timeout_seconds = 3;
    config.database.url = database_url;
    config.redis.url = redis_url;
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

struct FullStackHarness {
    http_base_url: String,
    grpc_base_url: String,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    server_task: Option<JoinHandle<anyhow::Result<()>>>,
    postgres: Option<TestContainer>,
    redis: Option<RedisContainer>,
}

impl FullStackHarness {
    async fn start(db_name: &str, label: &str) -> Self {
        let (postgres, database_url) = create_test_database_url_with_label(db_name, label).await;
        let (redis, redis_url) = start_redis_url_with_label(label).await;
        let http_port = reserve_local_port();
        let grpc_port = reserve_local_port();
        let rtmp_port = reserve_local_port();
        let config = test_config(database_url, redis_url, http_port, grpc_port, rtmp_port);

        let app = Application::build(config)
            .await
            .expect("full-stack application should build");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server_task = tokio::spawn(async move {
            app.run_with_shutdown_signal(async move {
                let _ = shutdown_rx.await;
            })
            .await
        });

        let harness = Self {
            http_base_url: format!("http://127.0.0.1:{http_port}"),
            grpc_base_url: format!("http://127.0.0.1:{grpc_port}"),
            shutdown_tx: Some(shutdown_tx),
            server_task: Some(server_task),
            postgres: Some(postgres),
            redis: Some(redis),
        };

        harness.wait_until_live().await;
        harness.wait_until_grpc_ready().await;
        harness
    }

    async fn wait_until_live(&self) {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("HTTP client");
        let deadline = Instant::now() + Duration::from_secs(10);
        let url = format!("{}/health/live", self.http_base_url);

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

    async fn wait_until_grpc_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);

        loop {
            let status = match tonic::transport::Endpoint::from_shared(self.grpc_base_url.clone())
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

    async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }

        if let Some(task) = self.server_task.take() {
            match tokio::time::timeout(Duration::from_secs(15), task).await {
                Ok(joined) => match joined {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => panic!("server exited with error: {error}"),
                    Err(error) => panic!("server task join failed: {error}"),
                },
                Err(_) => panic!("server did not shut down within 30s"),
            }
        }

        if let Some(redis) = self.redis.take() {
            redis.cleanup().await;
        }

        if let Some(postgres) = self.postgres.take() {
            postgres.cleanup().await;
        }
    }
}

/// Creates a test HTTP client with reasonable timeouts for E2E tests.
fn test_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
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
    let harness = FullStackHarness::start("synctv_e2e_health", "full-stack-health").await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("HTTP client");

    let live = client
        .get(format!("{}/health/live", harness.http_base_url))
        .send()
        .await
        .expect("liveness request");
    assert_eq!(live.status(), StatusCode::OK);
    let live_body = response_json(live).await;
    assert_eq!(live_body["status"], "ok");

    let ready = client
        .get(format!("{}/health/ready", harness.http_base_url))
        .send()
        .await
        .expect("readiness request");
    assert_eq!(ready.status(), StatusCode::OK);
    let ready_body = response_json(ready).await;
    assert_eq!(ready_body["status"], "healthy");
    assert_eq!(ready_body["details"]["database"], "healthy");
    assert_eq!(ready_body["details"]["redis"], "healthy");
    assert_eq!(ready_body["details"]["ws_ticket"], "healthy (redis)");

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_http_auth_room_and_ticket_flow_enforces_membership() {
    let harness = FullStackHarness::start("synctv_e2e_http_flow", "full-stack-http-flow").await;
    let client = test_http_client();

    let owner_register = post_json(
        &client,
        &format!("{}/api/auth/register", harness.http_base_url),
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
        &format!("{}/api/auth/login", harness.http_base_url),
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
        &format!("{}/api/user", harness.http_base_url),
        &owner_token,
    )
    .await;
    assert_eq!(owner_profile.status(), StatusCode::OK);
    let owner_profile_body = response_json(owner_profile).await;
    assert_eq!(owner_profile_body["user"]["username"], "owner_user");

    let create_room = post_json(
        &client,
        &format!("{}/api/rooms", harness.http_base_url),
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
        &format!("{}/api/auth/register", harness.http_base_url),
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
        &format!("{}/api/auth/login", harness.http_base_url),
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
        &format!("{}/api/tickets", harness.http_base_url),
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
        &format!("{}/api/rooms/{room_id}/members/@me", harness.http_base_url),
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
        &format!("{}/api/tickets", harness.http_base_url),
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

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_http_ticket_upgrades_websocket_once_and_then_expires() {
    let harness =
        FullStackHarness::start("synctv_e2e_http_ws_ticket", "full-stack-http-ws-ticket").await;
    let client = test_http_client();

    let register = post_json(
        &client,
        &format!("{}/api/auth/register", harness.http_base_url),
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
        &format!("{}/api/auth/login", harness.http_base_url),
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
        &format!("{}/api/rooms", harness.http_base_url),
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
        &format!("{}/api/tickets", harness.http_base_url),
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

    let addr = harness
        .http_base_url
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

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_auth_register_login_and_get_profile() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::{GetProfileRequest, LoginRequest, RegisterRequest};

    let harness = FullStackHarness::start("synctv_e2e_grpc_auth", "full-stack-grpc-auth").await;

    let mut auth_client = AuthServiceClient::connect(harness.grpc_base_url.clone())
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

    let mut profile_client = UserServiceClient::connect(harness.grpc_base_url.clone())
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

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_create_room_requires_auth_and_returns_created_room() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::{CreateRoomRequest, LoginRequest, RegisterRequest};

    let harness = FullStackHarness::start("synctv_e2e_grpc_room", "full-stack-grpc-room").await;

    let mut auth_client = AuthServiceClient::connect(harness.grpc_base_url.clone())
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

    let mut user_client = UserServiceClient::connect(harness.grpc_base_url.clone())
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

    harness.shutdown().await;
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

    let harness = FullStackHarness::start(
        "synctv_e2e_grpc_room_context",
        "full-stack-grpc-room-context",
    )
    .await;

    let mut auth_client = AuthServiceClient::connect(harness.grpc_base_url.clone())
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

    let mut owner_user_client = UserServiceClient::connect(harness.grpc_base_url.clone())
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

    let mut member_room_client = RoomServiceClient::connect(harness.grpc_base_url.clone())
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

    harness.shutdown().await;
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

    let harness = FullStackHarness::start(
        "synctv_e2e_grpc_message_stream",
        "full-stack-grpc-message-stream",
    )
    .await;

    let mut auth_client = AuthServiceClient::connect(harness.grpc_base_url.clone())
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

    let mut owner_user_client = UserServiceClient::connect(harness.grpc_base_url.clone())
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

    let mut member_room_client = RoomServiceClient::connect(harness.grpc_base_url.clone())
        .await
        .expect("connect member room gRPC client");
    let mut join_room = tonic::Request::new(JoinRoomRequest {
        room_id: room_id.clone(),
        password: "GrpcStreamRoomSecret123!".to_string(),
    });
    join_room
        .metadata_mut()
        .insert("authorization", bearer_metadata(&member_login.access_token));
    let mut member_user_client = UserServiceClient::connect(harness.grpc_base_url.clone())
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

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_message_stream_requires_join_room_first() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use synctv_proto::client::{ClientMessage, LoginRequest, RegisterRequest};

    let harness = FullStackHarness::start(
        "synctv_e2e_grpc_message_stream_missing_join_room",
        "full-stack-grpc-message-stream-missing-join-room",
    )
    .await;

    let mut auth_client = AuthServiceClient::connect(harness.grpc_base_url.clone())
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

    let mut room_client = RoomServiceClient::connect(harness.grpc_base_url.clone())
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

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers)"]
async fn full_stack_grpc_message_stream_requires_membership() {
    use synctv_proto::client::auth_service_client::AuthServiceClient;
    use synctv_proto::client::room_service_client::RoomServiceClient;
    use synctv_proto::client::user_service_client::UserServiceClient;
    use synctv_proto::client::{ClientMessage, CreateRoomRequest, LoginRequest, RegisterRequest};

    let harness = FullStackHarness::start(
        "synctv_e2e_grpc_message_stream_membership",
        "full-stack-grpc-message-stream-membership",
    )
    .await;

    let mut auth_client = AuthServiceClient::connect(harness.grpc_base_url.clone())
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

    let mut owner_user_client = UserServiceClient::connect(harness.grpc_base_url.clone())
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

    let mut outsider_room_client = RoomServiceClient::connect(harness.grpc_base_url.clone())
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

    harness.shutdown().await;
}
