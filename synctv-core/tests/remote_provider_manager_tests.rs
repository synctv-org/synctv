//! `RemoteProviderManager` integration tests
//!
//! Tests: channel creation, cache TTL, Redis invalidation, health checks,
//!        TLS configuration, fallback behavior.
//!
//! Run with: cargo test -p synctv-core --test `remote_provider_manager_tests`
//!
//! NOTE: These tests require Docker for testcontainers (`PostgreSQL` + Redis).
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use sqlx::PgPool;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use synctv_core::{
    cache::{CacheInvalidationRuntime, CacheInvalidationService, InvalidationMessage},
    models::ProviderInstance,
    repository::ProviderInstanceRepository,
    service::{remote_provider_manager::RemoteProviderManager, CredentialEncryption},
    Error,
};
use synctv_core_testing::{create_test_pool_with_options_and_label, start_redis_with_client};
use synctv_media_providers::grpc::{
    alist::alist_server::AlistServer, alist_server::AlistService as AlistGrpcService,
    emby::emby_server::EmbyServer, emby_server::EmbyService as EmbyGrpcService,
};
use tokio::sync::{broadcast, Barrier, RwLock};
use tonic::transport::Server;
use tonic_health::ServingStatus;

// Test utilities

/// Test infrastructure with `PostgreSQL` and Redis
struct TestInfra {
    pool: PgPool,
    redis_client: redis::Client,
    #[allow(dead_code)]
    postgres: synctv_core_testing::TestContainer,
    #[allow(dead_code)]
    redis: synctv_core_testing::RedisContainer,
}

impl TestInfra {
    async fn new() -> Self {
        let (postgres, pool) = create_test_pool_with_options_and_label(
            "synctv_test",
            "remote-provider-manager",
            20,
            std::time::Duration::from_secs(30),
        )
        .await;
        let (redis, redis_client) = start_redis_with_client().await;

        Self {
            pool,
            redis_client,
            postgres,
            redis,
        }
    }

    async fn redis_connection_manager(&self) -> redis::aio::ConnectionManager {
        redis::aio::ConnectionManager::new(self.redis_client.clone())
            .await
            .expect("Failed to create Redis ConnectionManager")
    }
}

#[derive(Default)]
struct FailingInvalidationRuntime;

#[async_trait::async_trait]
impl CacheInvalidationRuntime for FailingInvalidationRuntime {
    fn subscribe(&self) -> broadcast::Receiver<InvalidationMessage> {
        let (_sender, receiver) = broadcast::channel(1);
        receiver
    }

    async fn start(&self) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn stop(&self) {}

    async fn broadcast_remote(&self, _message: InvalidationMessage) -> synctv_core::Result<()> {
        Err(Error::ServiceUnavailable(
            "simulated invalidation publish failure".to_string(),
        ))
    }

    fn broadcast_local(&self, _message: InvalidationMessage) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn broadcast_all(&self, _message: InvalidationMessage) -> synctv_core::Result<()> {
        Err(Error::ServiceUnavailable(
            "simulated invalidation publish failure".to_string(),
        ))
    }

    async fn invalidate_user_permission(
        &self,
        _room_id: &synctv_core::models::RoomId,
        _user_id: &synctv_core::models::UserId,
    ) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn invalidate_room_permission(
        &self,
        _room_id: &synctv_core::models::RoomId,
    ) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn invalidate_user(
        &self,
        _user_id: &synctv_core::models::UserId,
    ) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn invalidate_username(
        &self,
        _user_id: &synctv_core::models::UserId,
    ) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn invalidate_room(
        &self,
        _room_id: &synctv_core::models::RoomId,
    ) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn invalidate_provider_instance(&self, _instance_name: &str) -> synctv_core::Result<()> {
        Err(Error::ServiceUnavailable(
            "simulated invalidation publish failure".to_string(),
        ))
    }

    async fn invalidate_playback_state(
        &self,
        _room_id: &synctv_core::models::RoomId,
    ) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn update_playback_state(
        &self,
        _room_id: &synctv_core::models::RoomId,
        _state: &synctv_core::models::RoomPlaybackState,
    ) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn invalidate_room_settings(
        &self,
        _room_id: &synctv_core::models::RoomId,
    ) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn invalidate_all(&self) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn invalidate_and_broadcast_user(
        &self,
        _user_id: &synctv_core::models::UserId,
    ) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn invalidate_and_broadcast_room(
        &self,
        _room_id: &synctv_core::models::RoomId,
    ) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn invalidate_and_broadcast_room_settings(
        &self,
        _room_id: &synctv_core::models::RoomId,
    ) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn invalidate_and_broadcast_username(
        &self,
        _user_id: &synctv_core::models::UserId,
    ) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn invalidate_and_broadcast_user_permission(
        &self,
        _room_id: &synctv_core::models::RoomId,
        _user_id: &synctv_core::models::UserId,
    ) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn invalidate_and_broadcast_room_permission(
        &self,
        _room_id: &synctv_core::models::RoomId,
    ) -> synctv_core::Result<()> {
        Ok(())
    }
}

fn unavailable_invalidation_service(
    _stream_key: impl Into<String>,
) -> Arc<dyn synctv_core::cache::CacheInvalidationRuntime> {
    Arc::new(FailingInvalidationRuntime)
}

async fn distributed_invalidation_service(
    infra: &TestInfra,
    node_id: &str,
    stream_key: String,
) -> Arc<dyn synctv_core::cache::CacheInvalidationRuntime> {
    Arc::new(CacheInvalidationService::from_runtime(
        synctv_core::direct_runtime(infra.redis_connection_manager().await),
        node_id.to_string(),
        stream_key,
    ))
}

async fn wait_until<F, Fut>(timeout: Duration, interval: Duration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if check().await {
            return;
        }
        let now = tokio::time::Instant::now();
        assert!(now < deadline, "condition not satisfied within {timeout:?}");
        tokio::time::sleep(interval.min(deadline - now)).await;
    }
}

async fn flush_provider_instances(infra: &TestInfra) {
    sqlx::query("TRUNCATE TABLE media_provider_instances RESTART IDENTITY CASCADE")
        .execute(&infra.pool)
        .await
        .expect("Failed to truncate media_provider_instances");
}

fn test_encryption() -> CredentialEncryption {
    CredentialEncryption::new(&[0x42; 32]).expect("test encryption key should be valid")
}

fn provider_repo(pool: &PgPool) -> ProviderInstanceRepository {
    ProviderInstanceRepository::new_with_encryption(pool.clone(), test_encryption())
}

const REDIS_INVALIDATION_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const REDIS_INVALIDATION_WAIT_INTERVAL: Duration = Duration::from_millis(50);

/// Create a test provider instance
fn make_test_instance(name: &str) -> ProviderInstance {
    let now = Utc::now();
    ProviderInstance {
        name: name.to_string(),
        endpoint: "http://example.com:50051".to_string(), // Use external domain to pass SSRF
        comment: Some("test instance".to_string()),
        jwt_secret: Some("remote-provider-test-secret".to_string()),
        custom_ca: None,
        timeout: "1s".to_string(),
        tls: false,
        insecure_tls: false,
        providers: vec!["bilibili".to_string()],
        enabled: true,
        created_at: now,
        updated_at: now,
    }
}

fn make_reachable_remote_instance(name: &str, host: &str, port: u16) -> ProviderInstance {
    let mut instance = make_test_instance(name);
    instance.endpoint = format!("http://{host}:{port}");
    instance.providers = vec!["alist".to_string()];
    instance
}

fn make_test_address_overrides(host: &str, port: u16) -> HashMap<String, SocketAddr> {
    HashMap::from([(
        host.to_string(),
        SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)),
    )])
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

    let (reporter, health_service) = tonic_health::server::health_reporter();
    reporter
        .set_service_status("", ServingStatus::Serving)
        .await;
    reporter
        .set_serving::<AlistServer<AlistGrpcService>>()
        .await;

    let auth_secret = auth_secret.to_string();
    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(health_service)
            .add_service(AlistServer::new(GrpcAuthProbeAlistService::new(
                auth_secret,
            )))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .expect("provider auth test server should run");
    });

    (addr, handle)
}

#[derive(Clone)]
struct GrpcAuthProbeAlistService {
    expected_secret: Arc<str>,
    fail_after_auth: bool,
    require_real_upstream_token: bool,
}

impl GrpcAuthProbeAlistService {
    fn new(expected_secret: String) -> Self {
        Self {
            expected_secret: Arc::<str>::from(expected_secret),
            fail_after_auth: false,
            require_real_upstream_token: false,
        }
    }

    fn failing_after_auth(expected_secret: String) -> Self {
        Self {
            expected_secret: Arc::<str>::from(expected_secret),
            fail_after_auth: true,
            require_real_upstream_token: false,
        }
    }

    fn requiring_real_upstream_token(expected_secret: String) -> Self {
        Self {
            expected_secret: Arc::<str>::from(expected_secret),
            fail_after_auth: false,
            require_real_upstream_token: true,
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

    async fn me(
        &self,
        request: tonic::Request<synctv_media_providers::grpc::alist::MeReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::alist::MeResp>, tonic::Status> {
        self.validate_secret(&request)?;
        if self.require_real_upstream_token && request.get_ref().token == "health-check-token" {
            return Err(tonic::Status::unauthenticated(
                "upstream provider rejected placeholder token",
            ));
        }
        if self.fail_after_auth {
            return Err(tonic::Status::internal(
                "authenticated provider handler failure",
            ));
        }
        Ok(tonic::Response::new(
            synctv_media_providers::grpc::alist::MeResp {
                id: 1,
                username: "health-check".to_string(),
                base_path: String::new(),
                role: 0,
                disabled: false,
                permission: 0,
                sso_id: String::new(),
                otp: false,
            },
        ))
    }
}

#[derive(Clone)]
struct GrpcAuthProbeEmbyService {
    expected_secret: Arc<str>,
    fail_after_auth: bool,
}

impl GrpcAuthProbeEmbyService {
    fn failing_after_auth(expected_secret: String) -> Self {
        Self {
            expected_secret: Arc::<str>::from(expected_secret),
            fail_after_auth: true,
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
impl synctv_media_providers::grpc::emby::emby_server::Emby for GrpcAuthProbeEmbyService {
    async fn login(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::emby::LoginReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::emby::LoginResp>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "login not needed for health probe",
        ))
    }

    async fn me(
        &self,
        request: tonic::Request<synctv_media_providers::grpc::emby::MeReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::emby::MeResp>, tonic::Status> {
        self.validate_secret(&request)?;
        if self.fail_after_auth {
            return Err(tonic::Status::internal(
                "authenticated emby provider handler failure",
            ));
        }
        Ok(tonic::Response::new(
            synctv_media_providers::grpc::emby::MeResp {
                id: "health-check-user".to_string(),
                name: "health-check".to_string(),
                server_id: "health-check-server".to_string(),
                policy: None,
            },
        ))
    }

    async fn get_items(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::emby::GetItemsReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::emby::GetItemsResp>, tonic::Status>
    {
        Err(tonic::Status::unimplemented(
            "get_items not needed for health probe",
        ))
    }

    async fn get_item(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::emby::GetItemReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::emby::Item>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "get_item not needed for health probe",
        ))
    }

    async fn get_system_info(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::emby::SystemInfoReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::emby::SystemInfoResp>, tonic::Status>
    {
        Err(tonic::Status::unimplemented(
            "get_system_info not needed for health probe",
        ))
    }

    async fn fs_list(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::emby::FsListReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::emby::FsListResp>, tonic::Status>
    {
        Err(tonic::Status::unimplemented(
            "fs_list not needed for health probe",
        ))
    }

    async fn logout(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::emby::LogoutReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::emby::Empty>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "logout not needed for health probe",
        ))
    }

    async fn playback_info(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::emby::PlaybackInfoReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::emby::PlaybackInfoResp>, tonic::Status>
    {
        Err(tonic::Status::unimplemented(
            "playback_info not needed for health probe",
        ))
    }

    async fn delete_active_encodings(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::emby::DeleteActiveEncodingsReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::emby::Empty>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "delete_active_encodings not needed for health probe",
        ))
    }

    async fn report_playback_start(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::emby::ReportPlaybackStartReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::emby::Empty>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "report_playback_start not needed for health probe",
        ))
    }

    async fn report_playback_stop(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::emby::ReportPlaybackStopReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::emby::Empty>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "report_playback_stop not needed for health probe",
        ))
    }

    async fn report_playback_progress(
        &self,
        _request: tonic::Request<synctv_media_providers::grpc::emby::ReportPlaybackProgressReq>,
    ) -> Result<tonic::Response<synctv_media_providers::grpc::emby::Empty>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "report_playback_progress not needed for health probe",
        ))
    }
}

async fn spawn_authenticated_provider_server_with_handler_failure(
    auth_secret: &str,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("provider auth failure test server should bind to an ephemeral port");
    let addr = listener
        .local_addr()
        .expect("provider auth failure test server should expose a local address");

    let (reporter, health_service) = tonic_health::server::health_reporter();
    reporter
        .set_service_status("", ServingStatus::Serving)
        .await;
    reporter
        .set_serving::<AlistServer<AlistGrpcService>>()
        .await;

    let auth_secret = auth_secret.to_string();
    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(health_service)
            .add_service(AlistServer::new(
                GrpcAuthProbeAlistService::failing_after_auth(auth_secret),
            ))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .expect("provider auth failure test server should run");
    });

    (addr, handle)
}

async fn spawn_authenticated_provider_server_rejecting_placeholder_upstream_auth(
    auth_secret: &str,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("provider auth placeholder rejection server should bind");
    let addr = listener
        .local_addr()
        .expect("provider auth placeholder rejection server should expose local address");

    let (reporter, health_service) = tonic_health::server::health_reporter();
    reporter
        .set_service_status("", ServingStatus::Serving)
        .await;
    reporter
        .set_serving::<AlistServer<AlistGrpcService>>()
        .await;

    let auth_secret = auth_secret.to_string();
    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(health_service)
            .add_service(AlistServer::new(
                GrpcAuthProbeAlistService::requiring_real_upstream_token(auth_secret),
            ))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .expect("provider auth placeholder rejection server should run");
    });

    (addr, handle)
}

async fn spawn_authenticated_emby_provider_server_with_handler_failure(
    auth_secret: &str,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("emby auth failure test server should bind to an ephemeral port");
    let addr = listener
        .local_addr()
        .expect("emby auth failure test server should expose a local address");

    let (reporter, health_service) = tonic_health::server::health_reporter();
    reporter
        .set_service_status("", ServingStatus::Serving)
        .await;
    reporter.set_serving::<EmbyServer<EmbyGrpcService>>().await;

    let auth_secret = auth_secret.to_string();
    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(health_service)
            .add_service(EmbyServer::new(
                GrpcAuthProbeEmbyService::failing_after_auth(auth_secret),
            ))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .expect("emby auth failure test server should run");
    });

    (addr, handle)
}

async fn spawn_stalling_tcp_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stalling test server should bind to an ephemeral port");
    let addr = listener
        .local_addr()
        .expect("stalling test server should expose a local address");

    let handle = tokio::spawn(async move {
        loop {
            let (stream, _) = listener
                .accept()
                .await
                .expect("stalling test server should accept connections");
            tokio::spawn(async move {
                let _stream = stream;
                std::future::pending::<()>().await;
            });
        }
    });

    (addr, handle)
}

/// Create a test provider instance with TLS
fn make_test_instance_tls(name: &str, insecure: bool) -> ProviderInstance {
    let now = Utc::now();
    ProviderInstance {
        name: name.to_string(),
        endpoint: "https://example.com:50052".to_string(),
        comment: Some("test TLS instance".to_string()),
        jwt_secret: Some("remote-provider-test-secret".to_string()),
        custom_ca: None,
        timeout: "1s".to_string(),
        tls: true,
        insecure_tls: insecure,
        providers: vec!["emby".to_string()],
        enabled: true,
        created_at: now,
        updated_at: now,
    }
}

// ─── Test 1: Channel creation from DB config ────────────────────────────

async fn scenario_channel_creation_from_db_config() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "channel-create.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );

    let instance = make_reachable_remote_instance("test-instance-1", host, health_addr.port());
    manager.add(instance.clone()).await.unwrap();

    // Get channel - should load the validated/cached remote connection.
    let channel = manager.get("test-instance-1").await;

    assert!(
        channel.is_some(),
        "validated remote channel should be available"
    );

    // Verify instance exists in DB
    let repo = provider_repo(&infra.pool);
    let fetched = repo.get_by_name("test-instance-1").await.unwrap();
    assert!(fetched.is_some());
    assert_eq!(fetched.unwrap().name, "test-instance-1");

    health_handle.abort();
    let _ = health_handle.await;
}

// ─── Test 2: Channel cache hit (cached channel returned) ─────────────────────

async fn scenario_channel_cache_hit() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "cache-hit.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );

    let instance = make_reachable_remote_instance("test-instance-2", host, health_addr.port());
    manager.add(instance.clone()).await.unwrap();

    // First get - cache miss, attempts DB lookup
    let _ = manager.get("test-instance-2").await;

    // Second get - should hit cache (though still returns None since no server)
    let _ = manager.get("test-instance-2").await;

    // Verify DB was only queried once (cache working)
    // This is implicit - if cache wasn't working, we'd see multiple DB queries
    // in logs. For now, we just verify no panics occur.

    health_handle.abort();
    let _ = health_handle.await;
}

// ─── Test 3: Channel cache TTL expiration ───────────────────────────────────

async fn scenario_channel_cache_ttl_expiration() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "cache-ttl.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );

    let instance = make_reachable_remote_instance("test-instance-3", host, health_addr.port());
    manager.add(instance.clone()).await.unwrap();

    // First get - populates cache
    let _ = manager.get("test-instance-3").await;

    // without modifying the manager or using a custom build)
    // For now, we just verify the cache is being used
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Get again - should still be in cache (300s TTL)
    let _ = manager.get("test-instance-3").await;

    // Note: Testing actual TTL expiration would require:
    // This is left as an exercise for future enhancement

    health_handle.abort();
    let _ = health_handle.await;
}

// ─── Test 4: Redis invalidation on delete ───────────────────────────────────

async fn scenario_redis_invalidation_on_delete() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "redis-delete.test.localhost";
    let repo = provider_repo(&infra.pool);
    let stream_key = format!("test:provider:invalidate:{}", synctv_common::snanoid!(8));
    let invalidation1: Arc<dyn synctv_core::cache::CacheInvalidationRuntime> =
        distributed_invalidation_service(&infra, "node1", stream_key.clone()).await;
    let invalidation2: Arc<dyn synctv_core::cache::CacheInvalidationRuntime> =
        distributed_invalidation_service(&infra, "node2", stream_key).await;
    invalidation1.start().await.unwrap();
    invalidation2.start().await.unwrap();

    let address_overrides = make_test_address_overrides(host, health_addr.port());
    let manager1 = RemoteProviderManager::new_with_address_overrides(
        Arc::new(provider_repo(&infra.pool)),
        Some(invalidation1.clone()),
        address_overrides.clone(),
    );
    let manager2 = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        Some(invalidation2.clone()),
        address_overrides,
    );

    manager2.start_invalidation_listener().await.unwrap();

    let instance = make_reachable_remote_instance("test-instance-5", host, health_addr.port());
    manager1.add(instance.clone()).await.unwrap();

    // Pre-warm manager2's cache
    let _ = manager2.get("test-instance-5").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Delete via manager1
    manager1.delete("test-instance-5").await.unwrap();

    wait_until(
        REDIS_INVALIDATION_WAIT_TIMEOUT,
        REDIS_INVALIDATION_WAIT_INTERVAL,
        || {
            let manager2 = &manager2;
            async move { manager2.get("test-instance-5").await.is_none() }
        },
    )
    .await;

    // Verify manager2 no longer lists the instance
    let instances2 = manager2.list().await.unwrap();
    assert!(
        !instances2.contains(&"test-instance-5".to_string()),
        "Manager2 should not list deleted instance"
    );

    // Verify get returns None
    let channel = manager2.get("test-instance-5").await;
    assert!(channel.is_none(), "Deleted instance should return None");

    manager1.shutdown().await;
    manager2.shutdown().await;
    invalidation1.stop().await;
    invalidation2.stop().await;
    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_add_rolls_back_when_provider_invalidation_publish_fails() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "provider-add-rollback.test.localhost";
    let instance =
        make_reachable_remote_instance("provider-add-rollback", host, health_addr.port());
    let repo = Arc::new(provider_repo(&infra.pool));
    let manager = RemoteProviderManager::new_with_address_overrides(
        repo.clone(),
        Some(unavailable_invalidation_service(format!(
            "test:provider:add-rollback:{}",
            synctv_common::snanoid!(8)
        ))),
        make_test_address_overrides(host, health_addr.port()),
    );

    let err = manager
        .add(instance.clone())
        .await
        .expect_err("provider add must fail closed when invalidation publish fails");

    assert!(
        matches!(
            err,
            synctv_core::Error::ServiceUnavailable(_) | synctv_core::Error::Internal(_)
        ),
        "unexpected error: {err:?}"
    );
    assert!(
        repo.get_by_name(&instance.name)
            .await
            .expect("repository lookup should succeed")
            .is_none(),
        "provider row must be rolled back when invalidation publish fails"
    );
    assert!(
        manager.get(&instance.name).await.is_none(),
        "local channel cache must be cleared when add rollback occurs"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_delete_rolls_back_when_provider_invalidation_publish_fails() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "provider-delete-rollback.test.localhost";
    let instance =
        make_reachable_remote_instance("provider-delete-rollback", host, health_addr.port());
    let repo = Arc::new(provider_repo(&infra.pool));
    let seed_manager = RemoteProviderManager::new_with_address_overrides(
        repo.clone(),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );
    seed_manager
        .add(instance.clone())
        .await
        .expect("seed provider instance should be created");

    let failing_manager = RemoteProviderManager::new_with_address_overrides(
        repo.clone(),
        Some(unavailable_invalidation_service(format!(
            "test:provider:delete-rollback:{}",
            synctv_common::snanoid!(8)
        ))),
        make_test_address_overrides(host, health_addr.port()),
    );

    let cached_before_delete = failing_manager
        .get(&instance.name)
        .await
        .expect("cached channel should exist before delete");
    let err = failing_manager
        .delete(&instance.name)
        .await
        .expect_err("provider delete must fail closed when invalidation publish fails");

    assert!(
        matches!(
            err,
            synctv_core::Error::ServiceUnavailable(_) | synctv_core::Error::Internal(_)
        ),
        "unexpected error: {err:?}"
    );
    let restored = repo
        .get_by_name(&instance.name)
        .await
        .expect("repository lookup should succeed")
        .expect("provider row must be restored after delete rollback");
    assert_eq!(
        restored.endpoint, instance.endpoint,
        "delete rollback must restore the original provider configuration"
    );
    let cached_after_delete = failing_manager
        .get(&instance.name)
        .await
        .expect("cached channel should be restored after delete rollback");
    assert_eq!(
        cached_after_delete.auth_secret(),
        cached_before_delete.auth_secret(),
        "delete rollback must restore the previous local cache entry"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

// ─── Test 5: Health check integration ───────────────────────────────────────

async fn scenario_health_check_integration() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        HashMap::from([(
            "health-check.test.localhost".to_string(),
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, health_addr.port())),
        )]),
    );

    let mut instance = make_test_instance("test-instance-6");
    instance.endpoint = format!("http://health-check.test.localhost:{}", health_addr.port());
    instance.providers = vec!["alist".to_string()];
    manager.add(instance.clone()).await.unwrap();

    // Run health check
    let health_results = manager.health_check().await;

    // The in-process provider server should be reported as healthy only when
    // both the health service and authenticated provider traffic succeed.
    assert!(
        health_results.contains_key("test-instance-6"),
        "Health check should include the instance"
    );
    assert!(
        health_results["test-instance-6"],
        "Instance should be healthy when both the gRPC health service and authenticated provider probe succeed"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

// ─── Test 7: Health check with enabled/disabled instances ──────────────────

async fn scenario_health_check_respects_enabled_flag() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "health-enabled.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );

    let instance_enabled =
        make_reachable_remote_instance("test-instance-7a", host, health_addr.port());
    manager.add(instance_enabled).await.unwrap();

    let instance_disabled = make_test_instance("test-instance-7b");
    let mut disabled = instance_disabled.clone();
    disabled.enabled = false;
    manager.add(disabled).await.unwrap();

    // Run health check
    let health_results = manager.health_check().await;

    // Enabled instance should be in results
    assert!(
        health_results.contains_key("test-instance-7a"),
        "Health check should include enabled instance"
    );

    // Disabled instance should NOT be in results
    assert!(
        !health_results.contains_key("test-instance-7b"),
        "Health check should skip disabled instance"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_health_check_reports_enabled_instance_with_invalid_secret_as_unhealthy() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut invalid = make_test_instance("test-instance-7c-invalid-secret");
    invalid.jwt_secret = Some("shared\nsecret".to_string());
    provider_repo(&infra.pool)
        .create(&invalid)
        .await
        .expect("invalid remote row should persist for health-check coverage");

    let health_results = manager.health_check().await;
    assert!(
        health_results.contains_key(&invalid.name),
        "enabled remote instances with invalid secrets should still appear in health results"
    );
    assert!(
        !health_results[&invalid.name],
        "enabled remote instances with invalid secrets should be reported unhealthy"
    );
}

async fn scenario_health_check_reports_enabled_instance_with_missing_secret_as_unhealthy() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        HashMap::from([(
            "invalid-health.test.localhost".to_string(),
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, health_addr.port())),
        )]),
    );

    let mut invalid = make_test_instance("test-instance-7d-missing-secret");
    invalid.endpoint = format!(
        "http://invalid-health.test.localhost:{}",
        health_addr.port()
    );
    invalid.jwt_secret = None;
    provider_repo(&infra.pool)
        .create(&invalid)
        .await
        .expect("invalid missing-secret row should persist for health-check coverage");

    let health_results = manager.health_check().await;
    assert!(
        health_results.contains_key(&invalid.name),
        "enabled remote instances with missing secrets should still appear in health results"
    );
    assert!(
        !health_results[&invalid.name],
        "enabled remote instances without secrets must be reported unhealthy"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_health_check_reports_enabled_instance_with_wrong_secret_as_unhealthy() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        HashMap::from([(
            "wrong-secret-health.test.localhost".to_string(),
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, health_addr.port())),
        )]),
    );

    let mut wrong = make_test_instance("test-instance-7e-wrong-secret");
    wrong.endpoint = format!(
        "http://wrong-secret-health.test.localhost:{}",
        health_addr.port()
    );
    wrong.providers = vec!["alist".to_string()];
    wrong.jwt_secret = Some("wrong-secret".to_string());

    provider_repo(&infra.pool)
        .create(&wrong)
        .await
        .expect("wrong-secret row should persist for health-check coverage");

    let health_results = manager.health_check().await;
    assert!(
        health_results.contains_key(&wrong.name),
        "enabled remote instances with wrong secrets should still appear in health results"
    );
    assert!(
        !health_results[&wrong.name],
        "enabled remote instances with wrong but well-formed secrets must be reported unhealthy"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_health_check_reports_authenticated_provider_failure_as_unhealthy() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server_with_handler_failure("remote-provider-test-secret")
            .await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        HashMap::from([(
            "handler-failure-health.test.localhost".to_string(),
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, health_addr.port())),
        )]),
    );

    let mut broken = make_test_instance("test-instance-7f-handler-failure");
    broken.endpoint = format!(
        "http://handler-failure-health.test.localhost:{}",
        health_addr.port()
    );
    broken.providers = vec!["alist".to_string()];

    provider_repo(&infra.pool)
        .create(&broken)
        .await
        .expect("handler-failure row should persist for health-check coverage");

    let health_results = manager.health_check().await;
    assert!(
        health_results.contains_key(&broken.name),
        "instances with authenticated provider failures should still appear in health results"
    );
    assert!(
        !health_results[&broken.name],
        "authenticated provider handler failures must be reported unhealthy"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_add_alist_instance_does_not_require_fake_upstream_auth_for_management_validation()
{
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server_rejecting_placeholder_upstream_auth(
            "remote-provider-test-secret",
        )
        .await;
    let host = "alist-management-validation.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );

    let instance = make_reachable_remote_instance(
        "test-instance-alist-management-validation",
        host,
        health_addr.port(),
    );

    manager
        .add(instance)
        .await
        .expect("management validation should not depend on fake upstream Alist credentials");

    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_health_check_reports_emby_authenticated_provider_failure_as_unhealthy() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_emby_provider_server_with_handler_failure(
            "remote-provider-test-secret",
        )
        .await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        HashMap::from([(
            "emby-handler-failure-health.test.localhost".to_string(),
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, health_addr.port())),
        )]),
    );

    let mut broken = make_test_instance("test-instance-7h-emby-handler-failure");
    broken.endpoint = format!(
        "http://emby-handler-failure-health.test.localhost:{}",
        health_addr.port()
    );
    broken.providers = vec!["emby".to_string()];

    provider_repo(&infra.pool)
        .create(&broken)
        .await
        .expect("emby handler-failure row should persist for health-check coverage");

    let health_results = manager.health_check().await;
    assert!(
        health_results.contains_key(&broken.name),
        "emby instances with authenticated provider failures should appear in health results"
    );
    assert!(
        !health_results[&broken.name],
        "authenticated emby provider handler failures must be reported unhealthy"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_add_emby_instance_does_not_require_authenticated_handler_success_for_management_validation(
) {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_emby_provider_server_with_handler_failure(
            "remote-provider-test-secret",
        )
        .await;
    let host = "emby-management-validation.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );

    let mut instance = make_reachable_remote_instance(
        "test-instance-emby-management-validation",
        host,
        health_addr.port(),
    );
    instance.providers = vec!["emby".to_string()];

    manager
        .add(instance.clone())
        .await
        .expect("management validation should only require transport health, not authenticated Emby handler success");
    assert!(
        provider_repo(&infra.pool)
            .get_by_name(&instance.name)
            .await
            .expect("lookup should succeed")
            .is_some(),
        "successful management validation must persist the Emby instance even when authenticated handlers are unhealthy"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

// ─── Test 8: TLS configuration (non-insecure) ───────────────────────────────

async fn scenario_tls_configuration_secure() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _repo = provider_repo(&infra.pool);

    let instance = make_test_instance_tls("test-instance-8", false);

    // This avoids the rustls crypto provider issue in test environment
    let repo_instance = provider_repo(&infra.pool);
    repo_instance.create(&instance).await.unwrap();

    // Verify instance was saved with correct TLS settings
    let fetched = repo_instance.get_by_name("test-instance-8").await.unwrap();
    assert!(fetched.is_some());
    let fetched = fetched.unwrap();
    assert!(fetched.tls, "Instance should have TLS enabled");
    assert!(
        !fetched.insecure_tls,
        "Instance should not have insecure TLS"
    );

    // Now create the manager and verify it can list the instance
    let manager = RemoteProviderManager::new(Arc::new(provider_repo(&infra.pool)));

    // Verify the instance can be listed
    let instances = manager.list().await.unwrap();
    assert!(
        instances.contains(&"test-instance-8".to_string()),
        "Should list the TLS instance"
    );
}

// ─── Test 9: TLS configuration (insecure) ───────────────────────────────────

async fn scenario_tls_configuration_insecure() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    // insecure TLS (connect_with_connector), so use a short timeout to avoid
    // waiting for the remote to respond. The test exercises the TLS code path.
    let instance = {
        let mut inst = make_test_instance_tls("test-instance-9", true);
        inst.timeout = "2s".to_string();
        inst
    };

    // Wrap with timeout to avoid 270s+ waits on DNS/connect to example.com
    let result = tokio::time::timeout(Duration::from_secs(5), manager.add(instance.clone())).await;

    // If it succeeds (unlikely with port 1), verify the stored config
    if matches!(&result, Ok(Ok(()))) {
        let repo = provider_repo(&infra.pool);
        let fetched = repo.get_by_name("test-instance-9").await.unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert!(fetched.tls, "Instance should have TLS enabled");
        assert!(
            fetched.insecure_tls,
            "Instance should have insecure TLS enabled"
        );
    }
    // Timeout or connection error is expected and acceptable
}

// ─── Test 10: Fallback to local provider (no remote instance) ───────────────

async fn scenario_fallback_to_local_provider() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    // Try to get a non-existent instance
    let channel = manager.get("non-existent-instance").await;

    // Should return None, allowing caller to fallback to local provider
    assert!(
        channel.is_none(),
        "Non-existent instance should return None for fallback"
    );

    // Test resolve_client with fallback
    let result = manager
        .resolve_client(
            Some("non-existent-instance"),
            |_channel| "remote",
            || "local",
        )
        .await;

    assert_eq!(
        result, "local",
        "resolve_client should fallback to local when remote not found"
    );
}

// ─── Test 11: Fallback when instance_name is None ───────────────────────────

async fn scenario_fallback_when_instance_name_none() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    // Test resolve_client with None instance_name (should use local)
    let result = manager
        .resolve_client(None as Option<&str>, |_channel| "remote", || "local")
        .await;

    assert_eq!(
        result, "local",
        "resolve_client should use local when instance_name is None"
    );
}

// ─── Test 11b: Explicit instance must not silently fallback locally ─────────

async fn scenario_resolve_client_required_rejects_missing_remote_instance() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let result = manager
        .resolve_client_required(Some("missing-instance"), |_channel| "remote", || "local")
        .await;

    assert!(
        result.is_err(),
        "explicit remote instance must not fallback to local"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err, synctv_core::provider::ProviderError::InstanceNotFound(ref name) if name == "missing-instance"),
        "unexpected error: {err:?}"
    );
}

async fn scenario_resolve_client_required_surfaces_existing_remote_instance_config_errors() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut invalid = make_test_instance("misconfigured-instance");
    invalid.jwt_secret = None;
    provider_repo(&infra.pool)
        .create(&invalid)
        .await
        .expect("invalid remote row should persist for resolution coverage");

    let result = manager
        .resolve_client_required(Some(&invalid.name), |_channel| "remote", || "local")
        .await;

    assert!(
        result.is_err(),
        "explicit remote instance with invalid stored config must fail"
    );
    let err = result.unwrap_err();
    assert!(
        !matches!(
            err,
            synctv_core::provider::ProviderError::InstanceNotFound(_)
        ),
        "existing remote instance with bad config must not be reported as missing: {err:?}"
    );
}

async fn scenario_resolve_client_required_preserves_retryable_repository_failures() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    infra.pool.close().await;

    let err = manager
        .resolve_client_required(
            Some("retryable-store-failure"),
            |_channel| "remote",
            || "local",
        )
        .await
        .expect_err("repository outages should surface as retryable remote resolution failures");

    assert!(
        matches!(err, synctv_core::provider::ProviderError::ApiError(_)),
        "repository outages must remain retryable instead of becoming internal errors: {err:?}"
    );
}

// ─── Test 12: Fallback when remote instance exists but channel fails ─────────

async fn scenario_fallback_when_channel_creation_fails() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut instance = make_test_instance("test-instance-12");
    instance.endpoint = "http://invalid-host-with-invalid-port:99999".to_string();
    let result = manager.add(instance.clone()).await;

    // Should fail due to invalid port/SSRF validation
    assert!(
        result.is_err(),
        "Adding instance with invalid endpoint should fail validation"
    );
}

// ─── Test 13: Enable/disable instance ───────────────────────────────────────

async fn scenario_enable_disable_instance() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "enable-disable.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );

    let instance = make_reachable_remote_instance("test-instance-13", host, health_addr.port());
    manager.add(instance.clone()).await.unwrap();

    // Verify it's enabled and gettable
    let repo = provider_repo(&infra.pool);
    let fetched = repo.get_by_name("test-instance-13").await.unwrap();
    assert!(fetched.is_some());
    assert!(fetched.unwrap().enabled);

    // Disable the instance
    manager.disable("test-instance-13").await.unwrap();

    // Verify it's disabled
    let fetched = repo.get_by_name("test-instance-13").await.unwrap();
    assert!(fetched.is_some());
    assert!(!fetched.unwrap().enabled);

    // get() should return None for disabled instance
    let channel = manager.get("test-instance-13").await;
    assert!(channel.is_none(), "Disabled instance should return None");

    // Re-enable the instance
    manager.enable("test-instance-13").await.unwrap();

    // Verify it's enabled again
    let fetched = repo.get_by_name("test-instance-13").await.unwrap();
    assert!(fetched.is_some());
    assert!(fetched.unwrap().enabled);

    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_enable_with_invalid_endpoint_preserves_disabled_state() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut invalid_disabled = make_test_instance("test-instance-13-invalid-enable");
    invalid_disabled.enabled = false;
    invalid_disabled.endpoint = "http://127.0.0.1:50051".to_string();

    let repo = provider_repo(&infra.pool);
    repo.create(&invalid_disabled).await.unwrap();

    let result = manager.enable("test-instance-13-invalid-enable").await;
    assert!(result.is_err(), "enabling invalid config should fail");

    let persisted = repo
        .get_by_name("test-instance-13-invalid-enable")
        .await
        .unwrap()
        .expect("instance should still exist");
    assert!(
        !persisted.enabled,
        "failed enable must not leave the DB row enabled"
    );

    let channel = manager.get("test-instance-13-invalid-enable").await;
    assert!(
        channel.is_none(),
        "failed enable must not leave a cached channel behind"
    );
}

async fn scenario_enable_remote_instance_requires_jwt_secret() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut instance = make_test_instance("test-instance-13-missing-secret-enable");
    instance.enabled = false;
    instance.jwt_secret = None;

    let repo = provider_repo(&infra.pool);
    repo.create(&instance).await.unwrap();

    let result = manager
        .enable("test-instance-13-missing-secret-enable")
        .await;
    assert!(
        result.is_err(),
        "enabling a remote instance without jwt_secret must fail"
    );

    let persisted = repo
        .get_by_name("test-instance-13-missing-secret-enable")
        .await
        .unwrap()
        .expect("instance should still exist");
    assert!(
        !persisted.enabled,
        "failed enable must not leave the DB row enabled"
    );
}

async fn scenario_enable_already_enabled_invalid_remote_instance_without_jwt_secret_fails() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut invalid = make_test_instance("test-instance-13-invalid-already-enabled");
    invalid.jwt_secret = None;
    invalid.enabled = true;
    invalid.comment = Some("invalid enabled row".to_string());

    provider_repo(&infra.pool)
        .create(&invalid)
        .await
        .expect("invalid enabled row should persist");

    let result = manager
        .enable("test-instance-13-invalid-already-enabled")
        .await;
    assert!(
        result.is_err(),
        "re-enabling an already-enabled invalid row without jwt_secret must fail"
    );

    let error_message = result.expect_err("missing secret should fail").to_string();
    assert!(
        error_message.contains("jwt_secret"),
        "error should explain missing jwt_secret: {error_message}"
    );

    let persisted = provider_repo(&infra.pool)
        .get_by_name("test-instance-13-invalid-already-enabled")
        .await
        .expect("lookup should succeed")
        .expect("invalid row should still exist");
    assert!(persisted.enabled, "invalid row should remain enabled");
    assert_eq!(persisted.jwt_secret, None);

    let connection = manager
        .get("test-instance-13-invalid-already-enabled")
        .await;
    assert!(
        connection.is_none(),
        "invalid row without jwt_secret must not resolve to an unusable remote connection"
    );
}

// ─── Test 14: Reconnect instance ────────────────────────────────────────────

async fn scenario_reconnect_instance() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "reconnect.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );

    let instance = make_reachable_remote_instance("test-instance-14", host, health_addr.port());
    manager.add(instance.clone()).await.unwrap();

    let result = manager.reconnect("test-instance-14").await;
    assert!(
        result.is_ok(),
        "Reconnect should succeed for a reachable remote instance"
    );

    // Disable the instance
    manager.disable("test-instance-14").await.unwrap();

    // Try to reconnect disabled instance - should fail
    let result = manager.reconnect("test-instance-14").await;
    assert!(
        result.is_err(),
        "Reconnect should fail for disabled instance"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

// ─── Test 15: Add duplicate instance fails ───────────────────────────────────

async fn scenario_add_duplicate_instance_fails() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "duplicate-add.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );

    let instance = make_reachable_remote_instance("test-instance-15", host, health_addr.port());
    manager.add(instance.clone()).await.unwrap();

    // Try to add duplicate - should fail
    let result = manager.add(instance).await;
    assert!(result.is_err(), "Adding duplicate instance should fail");

    if let Err(e) = result {
        assert!(
            format!("{e:?}").contains("AlreadyExists"),
            "Error should be AlreadyExists variant"
        );
    }

    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_add_disabled_instance_is_not_retrievable_via_get() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut disabled = make_test_instance("test-instance-15-disabled");
    disabled.enabled = false;
    manager.add(disabled).await.unwrap();

    let fetched = provider_repo(&infra.pool)
        .get_by_name("test-instance-15-disabled")
        .await
        .unwrap()
        .expect("instance should exist");
    assert!(!fetched.enabled, "instance should remain disabled in DB");

    let channel = manager.get("test-instance-15-disabled").await;
    assert!(
        channel.is_none(),
        "disabled instance must not be returned from same-node cache"
    );
}

async fn scenario_update_to_disabled_invalidates_cached_channel() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "update-disable.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );

    let instance = make_reachable_remote_instance(
        "test-instance-15-update-disabled",
        host,
        health_addr.port(),
    );
    manager.add(instance.clone()).await.unwrap();

    let initial = manager.get("test-instance-15-update-disabled").await;
    assert!(initial.is_some(), "enabled instance should be retrievable");

    let mut disabled = instance;
    disabled.enabled = false;
    disabled.comment = Some("now disabled".to_string());
    manager.update(disabled).await.unwrap();

    let fetched = provider_repo(&infra.pool)
        .get_by_name("test-instance-15-update-disabled")
        .await
        .unwrap()
        .expect("instance should exist");
    assert!(!fetched.enabled, "instance should be disabled in DB");

    let channel = manager.get("test-instance-15-update-disabled").await;
    assert!(
        channel.is_none(),
        "update(enabled=false) must evict any cached channel"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_concurrent_duplicate_add_returns_one_success_and_one_already_exists() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "concurrent-dup.test.localhost";

    let manager = Arc::new(RemoteProviderManager::new_with_address_overrides(
        Arc::new(provider_repo(&infra.pool)),
        None,
        make_test_address_overrides(host, health_addr.port()),
    ));
    let barrier = Arc::new(Barrier::new(3));
    let instance =
        make_reachable_remote_instance("test-instance-15-concurrent-dup", host, health_addr.port());

    let task1 = {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        let instance = instance.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            manager.add(instance).await
        })
    };
    let task2 = {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            manager.add(instance).await
        })
    };

    barrier.wait().await;

    let result1 = task1.await.unwrap();
    let result2 = task2.await.unwrap();
    let results = [result1, result2];

    let success_count = results.iter().filter(|result| result.is_ok()).count();
    let duplicate_errors = results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .collect::<Vec<_>>();

    assert_eq!(success_count, 1, "exactly one create must succeed");
    assert_eq!(
        duplicate_errors.len(),
        1,
        "exactly one create must fail with AlreadyExists"
    );
    assert!(
        matches!(
            duplicate_errors[0],
            synctv_core::Error::AlreadyExists(message)
                if message.contains("test-instance-15-concurrent-dup")
        ),
        "duplicate add should normalize to a stable AlreadyExists error, got {:?}",
        duplicate_errors[0]
    );

    let stored_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_provider_instances WHERE name = $1")
            .bind("test-instance-15-concurrent-dup")
            .fetch_one(&infra.pool)
            .await
            .unwrap();
    assert_eq!(stored_count, 1, "only one DB row should be persisted");

    health_handle.abort();
    let _ = health_handle.await;
}

// ─── Test 16: Update non-existent instance fails ─────────────────────────────

async fn scenario_update_nonexistent_instance_fails() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    // Try to update non-existent instance
    let instance = make_test_instance("test-instance-16");
    let result = manager.update(instance).await;

    // Should fail (database will return 0 rows affected)
    assert!(
        result.is_err(),
        "Updating non-existent instance should fail"
    );
}

async fn scenario_update_invalid_remote_instance_without_jwt_secret_fails() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut invalid = make_test_instance("test-instance-invalid-update");
    invalid.jwt_secret = None;
    invalid.comment = Some("invalid row".to_string());

    provider_repo(&infra.pool)
        .create(&invalid)
        .await
        .expect("invalid row should persist");

    invalid.comment = Some("updated comment".to_string());
    invalid.timeout = "2s".to_string();

    let result = manager.update(invalid.clone()).await;
    assert!(
        result.is_err(),
        "updating an invalid remote row without jwt_secret must fail"
    );

    let error_message = result.expect_err("missing secret should fail").to_string();
    assert!(
        error_message.contains("jwt_secret"),
        "error should explain missing jwt_secret: {error_message}"
    );

    let fetched = provider_repo(&infra.pool)
        .get_by_name(&invalid.name)
        .await
        .expect("lookup should succeed")
        .expect("updated instance should exist");
    assert_ne!(fetched.comment, invalid.comment);
    assert_ne!(fetched.timeout, "2s");
    assert_eq!(fetched.jwt_secret, None);
}

async fn scenario_update_local_only_instance_to_remote_requires_jwt_secret() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut instance = make_test_instance("test-instance-local-to-remote-update");
    instance.providers = vec!["live_proxy".to_string()];
    instance.jwt_secret = None;

    provider_repo(&infra.pool)
        .create(&instance)
        .await
        .expect("local-only row should persist without jwt_secret");

    instance.providers = vec!["bilibili".to_string()];

    let result = manager.update(instance.clone()).await;
    assert!(
        result.is_err(),
        "updating a local-only instance into a remote-capable one without jwt_secret must fail"
    );

    let persisted = provider_repo(&infra.pool)
        .get_by_name(&instance.name)
        .await
        .expect("lookup should succeed")
        .expect("instance should still exist");
    assert_eq!(
        persisted.providers,
        vec!["live_proxy".to_string()],
        "failed update must not persist the remote-capable provider set"
    );
    assert_eq!(persisted.jwt_secret, None);
}

async fn scenario_update_existing_remote_instance_requires_jwt_secret() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut instance = make_test_instance("test-instance-remote-update-missing-secret");
    provider_repo(&infra.pool)
        .create(&instance)
        .await
        .expect("remote row with jwt_secret should persist");

    instance.comment = Some("updated comment".to_string());
    instance.jwt_secret = None;

    let result = manager.update(instance.clone()).await;
    assert!(
        result.is_err(),
        "updating an existing remote instance with missing jwt_secret must fail"
    );

    let error = result.expect_err("missing secret should fail");
    let error_message = error.to_string();
    assert!(
        error_message.contains("jwt_secret"),
        "error should explain missing jwt_secret: {error_message}"
    );

    let persisted = provider_repo(&infra.pool)
        .get_by_name(&instance.name)
        .await
        .expect("lookup should succeed")
        .expect("instance should still exist");
    assert_ne!(
        persisted.comment, instance.comment,
        "failed update must not persist other field changes"
    );
    assert_eq!(
        persisted.jwt_secret.as_deref(),
        Some("remote-provider-test-secret"),
        "failed update must preserve the existing valid jwt_secret"
    );
}

async fn scenario_update_local_only_instance_to_remote_rejects_invalid_jwt_secret() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut instance = make_test_instance("test-instance-local-to-remote-invalid-secret");
    instance.providers = vec!["live_proxy".to_string()];
    instance.jwt_secret = None;
    instance.enabled = false;

    provider_repo(&infra.pool)
        .create(&instance)
        .await
        .expect("local-only row should persist without jwt_secret");

    instance.providers = vec!["bilibili".to_string()];
    instance.jwt_secret = Some("shared\nsecret".to_string());

    let result = manager.update(instance.clone()).await;
    assert!(
        result.is_err(),
        "updating a local-only instance into a remote-capable one with invalid jwt_secret must fail"
    );

    let error = result.expect_err("invalid secret should fail");
    let error_message = error.to_string();
    assert!(
        error_message.contains("secret"),
        "error should explain invalid jwt_secret syntax: {error_message}"
    );

    let persisted = provider_repo(&infra.pool)
        .get_by_name(&instance.name)
        .await
        .expect("lookup should succeed")
        .expect("instance should still exist");
    assert_eq!(
        persisted.providers,
        vec!["live_proxy".to_string()],
        "failed update must not persist the remote-capable provider set"
    );
    assert_eq!(persisted.jwt_secret, None);
}

// ─── Test 17: Delete non-existent instance fails ─────────────────────────────

async fn scenario_delete_nonexistent_instance_fails() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    // Try to delete non-existent instance
    let result = manager.delete("test-instance-17").await;

    // Should fail (database will return 0 rows affected)
    assert!(
        result.is_err(),
        "Deleting non-existent instance should fail"
    );
}

// ─── Test 18: Get all instances ─────────────────────────────────────────────

async fn scenario_get_all_instances() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "get-all.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );

    for i in 1..=3 {
        let instance = make_reachable_remote_instance(
            &format!("test-instance-18a-{i}"),
            host,
            health_addr.port(),
        );
        manager.add(instance).await.unwrap();
    }

    // Also create a disabled instance
    let mut disabled = make_test_instance("test-instance-18b-disabled");
    disabled.enabled = false;
    manager.add(disabled).await.unwrap();

    // Get all instances (should include both enabled and disabled)
    let all_instances = manager.get_all_instances().await.unwrap();

    assert!(
        all_instances.len() >= 4,
        "Should have at least 4 instances (3 enabled + 1 disabled)"
    );

    // Verify we have both enabled and disabled
    let enabled_count = all_instances.iter().filter(|i| i.enabled).count();
    let disabled_count = all_instances.iter().filter(|i| !i.enabled).count();

    assert!(
        enabled_count >= 3,
        "Should have at least 3 enabled instances"
    );
    assert!(
        disabled_count >= 1,
        "Should have at least 1 disabled instance"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

// ─── Test 19: Manager without Redis (local-only invalidation) ───────────────

async fn scenario_manager_without_redis() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "manager-no-redis.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );

    let result = manager.start_invalidation_listener().await;
    assert!(
        result.is_ok(),
        "Starting invalidation listener without Redis should succeed"
    );

    let instance = make_reachable_remote_instance("test-instance-19", host, health_addr.port());
    manager.add(instance.clone()).await.unwrap();

    // Get should still work
    let _ = manager.get("test-instance-19").await;

    // List should work
    let instances = manager.list().await.unwrap();
    assert!(
        instances.contains(&"test-instance-19".to_string()),
        "Should list the instance even without Redis"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

// ─── Test 20: Init pre-warms cache ──────────────────────────────────────────

async fn scenario_init_pre_warms_cache() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "init-prewarm.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );

    for i in 1..=3 {
        let instance = make_reachable_remote_instance(
            &format!("test-instance-20-{i}"),
            host,
            health_addr.port(),
        );
        manager.add(instance).await.unwrap();
    }

    // Call init - should pre-warm cache
    let result = manager.init().await;
    assert!(result.is_ok(), "Init should succeed");

    // Verify instances are listed
    let instances = manager.list().await.unwrap();
    assert!(
        instances.len() >= 3,
        "Should list at least 3 instances after init"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_init_rejects_invalid_secret_and_aborts_prewarming() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "init-invalid-secret.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );

    let mut invalid = make_test_instance("test-instance-20-invalid-secret");
    invalid.jwt_secret = Some("shared\nsecret".to_string());
    provider_repo(&infra.pool)
        .create(&invalid)
        .await
        .expect("invalid remote row should persist for fail-closed coverage");

    let healthy =
        make_reachable_remote_instance("test-instance-20-healthy", host, health_addr.port());
    manager
        .add(healthy.clone())
        .await
        .expect("healthy remote instance should be added");

    manager
        .init()
        .await
        .expect_err("init must fail closed when persisted remote config is invalid");

    let healthy_connection = manager
        .get(&healthy.name)
        .await
        .expect("healthy instance should still be available from its add-time cache");
    assert_eq!(
        healthy_connection.auth_secret(),
        Some("remote-provider-test-secret")
    );

    let invalid_connection = manager.get(&invalid.name).await;
    assert!(
        invalid_connection.is_none(),
        "invalid remote instance must not resolve after init failure"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

// ─── Test 21: SSRF validation prevents internal endpoints ───────────────────

async fn scenario_ssrf_validation_blocks_internal_ips() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    // Try to create instance with internal IP (should fail SSRF validation)
    let mut instance = make_test_instance("test-instance-21");
    instance.endpoint = "http://127.0.0.1:50051".to_string();

    let result = manager.add(instance).await;

    // Should fail due to SSRF validation
    assert!(
        result.is_err(),
        "Adding instance with internal IP should fail SSRF validation"
    );

    if let Err(e) = result {
        let error_msg = format!("{e:?}");
        assert!(
            error_msg.contains("SSRF")
                || error_msg.contains("ssrf")
                || error_msg.contains("internal"),
            "Error should mention SSRF validation: {error_msg}"
        );
    }
}

// ─── Test 22: SSRF validation allows public endpoints ───────────────────────

async fn scenario_ssrf_validation_allows_public_endpoints() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut instance = make_test_instance("test-instance-22");
    instance.endpoint = "http://example.com:50051".to_string();

    let result = manager.add(instance.clone()).await;

    // Public endpoints should pass static SSRF validation, but the management
    // path now also requires real connectivity before persisting the instance.
    assert!(
        result.is_err(),
        "Adding an unreachable public endpoint must fail connectivity validation"
    );

    let error_message = result
        .expect_err("unreachable public endpoint should fail")
        .to_string();
    assert!(
        !error_message.contains("SSRF validation: host"),
        "public host should not be rejected by static SSRF policy: {error_message}"
    );
    assert!(
        provider_repo(&infra.pool)
            .get_by_name("test-instance-22")
            .await
            .expect("lookup should succeed")
            .is_none(),
        "failed connectivity validation must not persist the instance"
    );
}

// ─── Test 23: resolve_client with remote instance ───────────────────────────

async fn scenario_resolve_client_uses_remote_when_available() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "resolve-remote.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = Arc::new(RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    ));

    let instance = make_reachable_remote_instance("test-instance-23", host, health_addr.port());
    manager.add(instance).await.unwrap();

    let result = manager
        .resolve_client(Some("test-instance-23"), |_channel| "remote", || "local")
        .await;

    assert_eq!(result, "remote");

    health_handle.abort();
    let _ = health_handle.await;
}

// ─── Test 24: Cache respects max capacity ───────────────────────────────────

async fn scenario_cache_respects_max_capacity() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "cache-capacity.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, health_addr.port()),
    );

    // This is more of a sanity check that the cache doesn't panic
    for i in 1..=10 {
        let instance = make_reachable_remote_instance(
            &format!("test-instance-24-{i}"),
            host,
            health_addr.port(),
        );
        manager.add(instance).await.unwrap();
    }

    // All should be listable
    let instances = manager.list().await.unwrap();
    assert!(instances.len() >= 10, "Should list at least 10 instances");

    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_redis_invalidation_respects_key_prefix() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "redis-prefix.test.localhost";
    let stream_key = format!(
        "tenant-a:test:provider:invalidate:{}",
        synctv_common::snanoid!(8)
    );
    let invalidation1: Arc<dyn synctv_core::cache::CacheInvalidationRuntime> =
        distributed_invalidation_service(&infra, "tenant-a-node1", stream_key.clone()).await;
    let invalidation2: Arc<dyn synctv_core::cache::CacheInvalidationRuntime> =
        distributed_invalidation_service(&infra, "tenant-a-node2", stream_key).await;
    invalidation1.start().await.unwrap();
    invalidation2.start().await.unwrap();

    let address_overrides = make_test_address_overrides(host, health_addr.port());
    let manager1 = RemoteProviderManager::new_with_address_overrides(
        Arc::new(provider_repo(&infra.pool)),
        Some(invalidation1.clone()),
        address_overrides.clone(),
    );
    let manager2 = RemoteProviderManager::new_with_address_overrides(
        Arc::new(provider_repo(&infra.pool)),
        Some(invalidation2.clone()),
        address_overrides,
    );

    manager2.start_invalidation_listener().await.unwrap();

    let instance = make_reachable_remote_instance("test-instance-prefix", host, health_addr.port());
    manager1.add(instance).await.unwrap();
    let _ = manager2.get("test-instance-prefix").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    manager1.delete("test-instance-prefix").await.unwrap();

    wait_until(
        REDIS_INVALIDATION_WAIT_TIMEOUT,
        REDIS_INVALIDATION_WAIT_INTERVAL,
        || {
            let manager2 = &manager2;
            async move { manager2.get("test-instance-prefix").await.is_none() }
        },
    )
    .await;

    manager1.shutdown().await;
    manager2.shutdown().await;
    invalidation1.stop().await;
    invalidation2.stop().await;
    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_invalidation_listener_shutdown_is_idempotent() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let stream_key = format!(
        "tenant-shutdown:test:provider:invalidate:{}",
        synctv_common::snanoid!(8)
    );
    let invalidation: Arc<dyn synctv_core::cache::CacheInvalidationRuntime> =
        distributed_invalidation_service(&infra, "tenant-shutdown-node", stream_key).await;
    invalidation.start().await.unwrap();

    let manager = RemoteProviderManager::new_with_invalidation(
        Arc::new(provider_repo(&infra.pool)),
        Some(invalidation.clone()),
    );

    manager.start_invalidation_listener().await.unwrap();
    manager
        .start_invalidation_listener()
        .await
        .expect("second start should be idempotent");

    manager.shutdown().await;
    manager.shutdown().await;
    invalidation.stop().await;
}

async fn scenario_durable_invalidation_catches_up_after_listener_starts_late() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;
    let host = "durable-invalidation.test.localhost";

    let stream_key = format!("test:provider:durable:{}", synctv_common::snanoid!(8));
    let invalidation1: Arc<dyn synctv_core::cache::CacheInvalidationRuntime> =
        distributed_invalidation_service(&infra, "provider-node1", stream_key.clone()).await;
    let invalidation2: Arc<dyn synctv_core::cache::CacheInvalidationRuntime> =
        distributed_invalidation_service(&infra, "provider-node2", stream_key).await;
    invalidation1.start().await.unwrap();
    invalidation2.start().await.unwrap();

    let address_overrides = make_test_address_overrides(host, health_addr.port());
    let manager1 = RemoteProviderManager::new_with_address_overrides(
        Arc::new(provider_repo(&infra.pool)),
        Some(invalidation1.clone()),
        address_overrides.clone(),
    );
    let manager2 = RemoteProviderManager::new_with_address_overrides(
        Arc::new(provider_repo(&infra.pool)),
        Some(invalidation2.clone()),
        address_overrides,
    );

    let instance =
        make_reachable_remote_instance("durable-provider-instance", host, health_addr.port());
    manager1.add(instance).await.unwrap();

    let channel = manager2.get("durable-provider-instance").await;
    assert!(
        channel.is_some(),
        "manager2 should cache the instance before delete"
    );

    manager1.delete("durable-provider-instance").await.unwrap();

    manager2
        .start_invalidation_listener()
        .await
        .expect("late-started listener should catch up through durable invalidation stream");

    wait_until(
        REDIS_INVALIDATION_WAIT_TIMEOUT,
        REDIS_INVALIDATION_WAIT_INTERVAL,
        || {
            let manager2 = &manager2;
            async move { manager2.get("durable-provider-instance").await.is_none() }
        },
    )
    .await;

    manager1.shutdown().await;
    manager2.shutdown().await;
    invalidation1.stop().await;
    invalidation2.stop().await;
    health_handle.abort();
    let _ = health_handle.await;
}

// ─── Test 25: Provider instance supports_provider ───────────────────────────

async fn scenario_provider_instance_supports_provider() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

    let instance = ProviderInstance {
        name: "test-instance-25".to_string(),
        endpoint: "http://localhost:50051".to_string(),
        comment: None,
        jwt_secret: None,
        custom_ca: None,
        timeout: "10s".to_string(),
        tls: false,
        insecure_tls: false,
        providers: vec![
            "bilibili".to_string(),
            "alist".to_string(),
            "emby".to_string(),
        ],
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    // Test supports_provider
    assert!(instance.supports_provider("bilibili"));
    assert!(instance.supports_provider("alist"));
    assert!(instance.supports_provider("emby"));
    assert!(!instance.supports_provider("direct_url"));
    assert!(!instance.supports_provider("rtmp"));
}

async fn scenario_add_remote_instance_requires_jwt_secret() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut instance = make_test_instance("test-instance-missing-secret");
    instance.jwt_secret = None;

    let result = manager.add(instance).await;
    assert!(
        result.is_err(),
        "remote instance without jwt_secret must be rejected"
    );

    let error = result.expect_err("missing secret should fail");
    let error_message = error.to_string();
    assert!(
        error_message.contains("jwt_secret"),
        "error should explain missing jwt_secret: {error_message}"
    );
}

async fn scenario_add_remote_instance_rejects_invalid_jwt_secret_even_when_disabled() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut instance = make_test_instance("test-instance-invalid-secret-disabled");
    instance.enabled = false;
    instance.jwt_secret = Some("shared\nsecret".to_string());

    let result = manager.add(instance.clone()).await;
    assert!(
        result.is_err(),
        "disabled remote instance with invalid jwt_secret must be rejected"
    );

    let error = result.expect_err("invalid secret should fail");
    let error_message = error.to_string();
    assert!(
        error_message.contains("secret"),
        "error should explain invalid jwt_secret syntax: {error_message}"
    );

    assert!(
        provider_repo(&infra.pool)
            .get_by_name(&instance.name)
            .await
            .expect("lookup should succeed")
            .is_none(),
        "failed add must not persist the invalid remote instance"
    );
}

async fn scenario_add_local_only_instance_allows_empty_jwt_secret() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let now = Utc::now();
    let instance = ProviderInstance {
        name: "test-local-only-instance".to_string(),
        endpoint: "http://localhost:50051".to_string(),
        comment: Some("local-only provider instance".to_string()),
        jwt_secret: None,
        custom_ca: None,
        timeout: "1s".to_string(),
        tls: false,
        insecure_tls: false,
        providers: vec!["direct_url".to_string(), "rtmp".to_string()],
        enabled: true,
        created_at: now,
        updated_at: now,
    };

    manager
        .add(instance.clone())
        .await
        .expect("local-only provider instance should be accepted without jwt_secret");

    assert!(
        manager.get(&instance.name).await.is_none(),
        "local-only provider instance should not create a remote connection"
    );

    let fetched = provider_repo(&infra.pool)
        .get_by_name(&instance.name)
        .await
        .expect("lookup should succeed")
        .expect("instance should exist");
    assert_eq!(fetched.providers, instance.providers);
    assert_eq!(fetched.jwt_secret, None);
}

async fn scenario_add_unreachable_remote_instance_fails_connectivity_validation() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut instance = make_test_instance("test-instance-unreachable-add");
    instance.endpoint = "http://unreachable-provider.example.invalid:50051".to_string();
    instance.providers = vec!["alist".to_string()];
    instance.timeout = "1s".to_string();

    let result = manager.add(instance.clone()).await;
    assert!(
        result.is_err(),
        "remote instance add must fail when the configured endpoint is unreachable"
    );

    assert!(
        provider_repo(&infra.pool)
            .get_by_name(&instance.name)
            .await
            .expect("lookup should succeed")
            .is_none(),
        "failed add must not persist an unreachable remote instance"
    );
}

async fn scenario_add_reachable_remote_instance_succeeds_with_connectivity_validation() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        HashMap::from([(
            "reachable-provider.test.localhost".to_string(),
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, health_addr.port())),
        )]),
    );

    let mut instance = make_test_instance("test-instance-reachable-add");
    instance.endpoint = format!(
        "http://reachable-provider.test.localhost:{}",
        health_addr.port()
    );
    instance.providers = vec!["alist".to_string()];

    manager
        .add(instance.clone())
        .await
        .expect("reachable remote instance should pass connectivity validation");

    let connection = manager
        .get(&instance.name)
        .await
        .expect("reachable remote instance should be cached");
    assert_eq!(
        connection.auth_secret(),
        Some("remote-provider-test-secret"),
        "validated remote instance should retain its auth secret"
    );

    let stored = provider_repo(&infra.pool)
        .get_by_name(&instance.name)
        .await
        .expect("lookup should succeed")
        .expect("reachable instance should be persisted");
    assert_eq!(stored.endpoint, instance.endpoint);

    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_enable_unreachable_remote_instance_preserves_disabled_state() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut instance = make_test_instance("test-instance-unreachable-enable");
    instance.enabled = false;
    instance.endpoint = "http://unreachable-provider.example.invalid:50051".to_string();
    instance.providers = vec!["alist".to_string()];
    instance.timeout = "1s".to_string();

    provider_repo(&infra.pool)
        .create(&instance)
        .await
        .expect("disabled instance should persist");

    let result = manager.enable(&instance.name).await;
    assert!(
        result.is_err(),
        "enable must fail when the remote endpoint is unreachable"
    );

    let stored = provider_repo(&infra.pool)
        .get_by_name(&instance.name)
        .await
        .expect("lookup should succeed")
        .expect("instance should still exist");
    assert!(
        !stored.enabled,
        "failed enable must leave the instance disabled in the database"
    );
}

async fn scenario_reconnect_unreachable_remote_instance_fails_connectivity_validation() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let _redis_client = Some(infra.redis_client.clone());

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut instance = make_test_instance("test-instance-unreachable-reconnect");
    instance.endpoint = "http://unreachable-provider.example.invalid:50051".to_string();
    instance.providers = vec!["alist".to_string()];
    instance.timeout = "1s".to_string();

    provider_repo(&infra.pool)
        .create(&instance)
        .await
        .expect("enabled instance should persist for reconnect coverage");

    let result = manager.reconnect(&instance.name).await;
    assert!(
        result.is_err(),
        "reconnect must fail when the remote endpoint is unreachable"
    );
}

async fn scenario_update_unreachable_remote_instance_preserves_existing_configuration() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        HashMap::from([(
            "update-provider.test.localhost".to_string(),
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, health_addr.port())),
        )]),
    );

    let mut instance = make_test_instance("test-instance-unreachable-update");
    instance.endpoint = format!(
        "http://update-provider.test.localhost:{}",
        health_addr.port()
    );
    instance.providers = vec!["alist".to_string()];
    manager
        .add(instance.clone())
        .await
        .expect("reachable instance should be added before update");

    let mut updated = instance.clone();
    updated.endpoint = "http://unreachable-provider.example.invalid:50051".to_string();
    updated.timeout = "1s".to_string();

    let result = manager.update(updated.clone()).await;
    assert!(
        result.is_err(),
        "update must fail when the new remote endpoint is unreachable"
    );

    let stored = provider_repo(&infra.pool)
        .get_by_name(&instance.name)
        .await
        .expect("lookup should succeed")
        .expect("instance should still exist");
    assert_eq!(
        stored.endpoint, instance.endpoint,
        "failed update must preserve the last known-good endpoint"
    );

    health_handle.abort();
    let _ = health_handle.await;
}

async fn scenario_add_stalling_remote_instance_honors_connect_timeout() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (stall_addr, stall_handle) = spawn_stalling_tcp_server().await;
    let host = "connect-timeout.test.localhost";

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        make_test_address_overrides(host, stall_addr.port()),
    );

    let mut instance =
        make_reachable_remote_instance("test-instance-connect-timeout", host, stall_addr.port());
    instance.timeout = "500ms".to_string();

    let start = tokio::time::Instant::now();
    let result = manager.add(instance.clone()).await;
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "stalling remote endpoint must fail connectivity validation"
    );
    assert!(
        elapsed < Duration::from_millis(1500),
        "configured timeout should bound management-path validation latency, elapsed: {elapsed:?}"
    );

    stall_handle.abort();
    let _ = stall_handle.await;
}

async fn scenario_invalid_remote_instance_without_jwt_secret_is_rejected_at_runtime() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut instance = make_test_instance("test-invalid-remote-instance");
    instance.jwt_secret = None;

    provider_repo(&infra.pool)
        .create(&instance)
        .await
        .expect("invalid row without jwt_secret should still persist");

    let connection = manager.get(&instance.name).await;
    assert!(
        connection.is_none(),
        "invalid remote instance without jwt_secret must not build a runtime connection"
    );

    manager
        .init()
        .await
        .expect_err("init must fail closed for invalid persisted remote rows");

    let cached = manager.get(&instance.name).await;
    assert!(
        cached.is_none(),
        "pre-warm must not cache an invalid remote instance without jwt_secret"
    );
}

// ─── Test 26: Provider instance parse_timeout ───────────────────────────────

async fn scenario_provider_instance_parse_timeout() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

    // Test valid timeout formats
    let mut instance1 = make_test_instance("test-26a");
    instance1.timeout = "10s".to_string();
    assert_eq!(instance1.parse_timeout().unwrap(), Duration::from_secs(10));

    let mut instance2 = make_test_instance("test-26b");
    instance2.timeout = "30s".to_string();
    assert_eq!(instance2.parse_timeout().unwrap(), Duration::from_secs(30));

    let mut instance3 = make_test_instance("test-26c");
    instance3.timeout = "5m".to_string();
    assert_eq!(instance3.parse_timeout().unwrap(), Duration::from_mins(5));

    // Test invalid timeout format
    let mut instance4 = make_test_instance("test-26d");
    instance4.timeout = "invalid".to_string();
    assert!(
        instance4.parse_timeout().is_err(),
        "Invalid timeout should parse error"
    );
}

fn install_rustls_provider_once() {
    if let Some(provider) = default_rustls_provider() {
        let _ = rustls::crypto::CryptoProvider::install_default(provider);
    }
}

fn default_rustls_provider() -> Option<rustls::crypto::CryptoProvider> {
    #[cfg(feature = "tls-aws-lc")]
    {
        return Some(rustls::crypto::aws_lc_rs::default_provider());
    }

    #[cfg(all(
        not(feature = "tls-aws-lc"),
        any(
            feature = "tls-ring",
            feature = "tls-webpki-roots",
            feature = "tls-native-roots"
        )
    ))]
    {
        return Some(rustls::crypto::ring::default_provider());
    }

    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_channel_creation_from_db_config() {
    install_rustls_provider_once();
    scenario_channel_creation_from_db_config().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_channel_cache_hit() {
    install_rustls_provider_once();
    scenario_channel_cache_hit().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_channel_cache_ttl_expiration() {
    install_rustls_provider_once();
    scenario_channel_cache_ttl_expiration().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_redis_invalidation_on_delete() {
    install_rustls_provider_once();
    scenario_redis_invalidation_on_delete().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_durable_invalidation_catches_up_after_listener_starts_late() {
    install_rustls_provider_once();
    scenario_durable_invalidation_catches_up_after_listener_starts_late().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_health_check_integration() {
    install_rustls_provider_once();
    scenario_health_check_integration().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_health_check_respects_enabled_flag() {
    install_rustls_provider_once();
    scenario_health_check_respects_enabled_flag().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_health_check_reports_enabled_instance_with_invalid_secret_as_unhealthy() {
    install_rustls_provider_once();
    scenario_health_check_reports_enabled_instance_with_invalid_secret_as_unhealthy().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_health_check_reports_enabled_instance_with_missing_secret_as_unhealthy() {
    install_rustls_provider_once();
    scenario_health_check_reports_enabled_instance_with_missing_secret_as_unhealthy().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_health_check_reports_enabled_instance_with_wrong_secret_as_unhealthy() {
    install_rustls_provider_once();
    scenario_health_check_reports_enabled_instance_with_wrong_secret_as_unhealthy().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_health_check_reports_authenticated_provider_failure_as_unhealthy() {
    install_rustls_provider_once();
    scenario_health_check_reports_authenticated_provider_failure_as_unhealthy().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_health_check_reports_emby_authenticated_provider_failure_as_unhealthy() {
    install_rustls_provider_once();
    scenario_health_check_reports_emby_authenticated_provider_failure_as_unhealthy().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_add_alist_instance_does_not_require_fake_upstream_auth_for_management_validation() {
    install_rustls_provider_once();
    scenario_add_alist_instance_does_not_require_fake_upstream_auth_for_management_validation()
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_add_emby_instance_does_not_require_authenticated_handler_success_for_management_validation(
) {
    install_rustls_provider_once();
    scenario_add_emby_instance_does_not_require_authenticated_handler_success_for_management_validation()
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_tls_configuration_secure() {
    install_rustls_provider_once();
    scenario_tls_configuration_secure().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_tls_configuration_insecure() {
    install_rustls_provider_once();
    scenario_tls_configuration_insecure().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_fallback_to_local_provider() {
    install_rustls_provider_once();
    scenario_fallback_to_local_provider().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_fallback_when_instance_name_none() {
    install_rustls_provider_once();
    scenario_fallback_when_instance_name_none().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_resolve_client_required_rejects_missing_remote_instance() {
    install_rustls_provider_once();
    scenario_resolve_client_required_rejects_missing_remote_instance().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_resolve_client_required_surfaces_existing_remote_instance_config_errors() {
    install_rustls_provider_once();
    scenario_resolve_client_required_surfaces_existing_remote_instance_config_errors().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_resolve_client_required_preserves_retryable_repository_failures() {
    install_rustls_provider_once();
    scenario_resolve_client_required_preserves_retryable_repository_failures().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_fallback_when_channel_creation_fails() {
    install_rustls_provider_once();
    scenario_fallback_when_channel_creation_fails().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_enable_disable_instance() {
    install_rustls_provider_once();
    scenario_enable_disable_instance().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_enable_with_invalid_endpoint_preserves_disabled_state() {
    install_rustls_provider_once();
    scenario_enable_with_invalid_endpoint_preserves_disabled_state().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_reconnect_instance() {
    install_rustls_provider_once();
    scenario_reconnect_instance().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_add_duplicate_instance_fails() {
    install_rustls_provider_once();
    scenario_add_duplicate_instance_fails().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_add_disabled_instance_is_not_retrievable_via_get() {
    install_rustls_provider_once();
    scenario_add_disabled_instance_is_not_retrievable_via_get().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_update_to_disabled_invalidates_cached_channel() {
    install_rustls_provider_once();
    scenario_update_to_disabled_invalidates_cached_channel().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_concurrent_duplicate_add_returns_one_success_and_one_already_exists() {
    install_rustls_provider_once();
    scenario_concurrent_duplicate_add_returns_one_success_and_one_already_exists().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_update_nonexistent_instance_fails() {
    install_rustls_provider_once();
    scenario_update_nonexistent_instance_fails().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_delete_nonexistent_instance_fails() {
    install_rustls_provider_once();
    scenario_delete_nonexistent_instance_fails().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_get_all_instances() {
    install_rustls_provider_once();
    scenario_get_all_instances().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_manager_without_redis() {
    install_rustls_provider_once();
    scenario_manager_without_redis().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_init_pre_warms_cache() {
    install_rustls_provider_once();
    scenario_init_pre_warms_cache().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_ssrf_validation_blocks_internal_ips() {
    install_rustls_provider_once();
    scenario_ssrf_validation_blocks_internal_ips().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_ssrf_validation_allows_public_endpoints() {
    install_rustls_provider_once();
    scenario_ssrf_validation_allows_public_endpoints().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_resolve_client_uses_remote_when_available() {
    install_rustls_provider_once();
    scenario_resolve_client_uses_remote_when_available().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_cache_respects_max_capacity() {
    install_rustls_provider_once();
    scenario_cache_respects_max_capacity().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_redis_invalidation_respects_key_prefix() {
    install_rustls_provider_once();
    scenario_redis_invalidation_respects_key_prefix().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_invalidation_listener_shutdown_is_idempotent() {
    install_rustls_provider_once();
    scenario_invalidation_listener_shutdown_is_idempotent().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_provider_instance_supports_provider() {
    install_rustls_provider_once();
    scenario_provider_instance_supports_provider().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_add_remote_instance_requires_jwt_secret() {
    install_rustls_provider_once();
    scenario_add_remote_instance_requires_jwt_secret().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_add_remote_instance_rejects_invalid_jwt_secret_even_when_disabled() {
    install_rustls_provider_once();
    scenario_add_remote_instance_rejects_invalid_jwt_secret_even_when_disabled().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_add_local_only_instance_allows_empty_jwt_secret() {
    install_rustls_provider_once();
    scenario_add_local_only_instance_allows_empty_jwt_secret().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_add_unreachable_remote_instance_fails_connectivity_validation() {
    install_rustls_provider_once();
    scenario_add_unreachable_remote_instance_fails_connectivity_validation().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_add_reachable_remote_instance_succeeds_with_connectivity_validation() {
    install_rustls_provider_once();
    scenario_add_reachable_remote_instance_succeeds_with_connectivity_validation().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_add_rolls_back_when_provider_invalidation_publish_fails() {
    install_rustls_provider_once();
    scenario_add_rolls_back_when_provider_invalidation_publish_fails().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_delete_rolls_back_when_provider_invalidation_publish_fails() {
    install_rustls_provider_once();
    scenario_delete_rolls_back_when_provider_invalidation_publish_fails().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_enable_unreachable_remote_instance_preserves_disabled_state() {
    install_rustls_provider_once();
    scenario_enable_unreachable_remote_instance_preserves_disabled_state().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_reconnect_unreachable_remote_instance_fails_connectivity_validation() {
    install_rustls_provider_once();
    scenario_reconnect_unreachable_remote_instance_fails_connectivity_validation().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_update_unreachable_remote_instance_preserves_existing_configuration() {
    install_rustls_provider_once();
    scenario_update_unreachable_remote_instance_preserves_existing_configuration().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_add_stalling_remote_instance_honors_connect_timeout() {
    install_rustls_provider_once();
    scenario_add_stalling_remote_instance_honors_connect_timeout().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_invalid_remote_instance_without_jwt_secret_is_rejected_at_runtime() {
    install_rustls_provider_once();
    scenario_invalid_remote_instance_without_jwt_secret_is_rejected_at_runtime().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_provider_instance_parse_timeout() {
    install_rustls_provider_once();
    scenario_provider_instance_parse_timeout().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_enable_remote_instance_requires_jwt_secret() {
    install_rustls_provider_once();
    scenario_enable_remote_instance_requires_jwt_secret().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_enable_already_enabled_invalid_remote_instance_without_jwt_secret_fails() {
    install_rustls_provider_once();
    scenario_enable_already_enabled_invalid_remote_instance_without_jwt_secret_fails().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_update_invalid_remote_instance_without_jwt_secret_fails() {
    install_rustls_provider_once();
    scenario_update_invalid_remote_instance_without_jwt_secret_fails().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_update_local_only_instance_to_remote_requires_jwt_secret() {
    install_rustls_provider_once();
    scenario_update_local_only_instance_to_remote_requires_jwt_secret().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_update_existing_remote_instance_requires_jwt_secret() {
    install_rustls_provider_once();
    scenario_update_existing_remote_instance_requires_jwt_secret().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_update_local_only_instance_to_remote_rejects_invalid_jwt_secret() {
    install_rustls_provider_once();
    scenario_update_local_only_instance_to_remote_rejects_invalid_jwt_secret().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_init_rejects_invalid_secret_and_aborts_prewarming() {
    install_rustls_provider_once();
    scenario_init_rejects_invalid_secret_and_aborts_prewarming().await;
}
