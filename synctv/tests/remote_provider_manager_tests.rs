//! `RemoteProviderManager` integration tests
//!
//! Tests: channel creation, cache TTL, Redis invalidation, health checks,
//!        TLS configuration, fallback behavior.
//!
//!
//! NOTE: These tests require Docker for testcontainers (`PostgreSQL` + Redis).
use chrono::Utc;
use sqlx::PgPool;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use synctv_core::{
    cache::{CacheInvalidationRuntime, CacheInvalidationService, InvalidationMessage},
    credential_encryption::CredentialEncryption,
    models::{ProviderInstance, SourceProvider},
    repository::ProviderInstanceRepository,
    service::RemoteProviderManager,
    Error,
};
use synctv_core_testing::{
    create_test_pool_with_options_and_label, redis_connection_manager, start_redis_with_client,
    TestOptionExt, TestResultExt,
};
use synctv_media_providers::grpc::{
    alist::alist_server::AlistServer, emby::emby_server::EmbyServer,
    AlistService as AlistRemoteService, EmbyService as EmbyRemoteService,
};
use tokio::sync::{broadcast, Barrier};
use tonic::metadata::MetadataMap;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Server;
use tonic_health::ServingStatus;

struct TestInfra {
    pool: PgPool,
    redis_client: redis::Client,
    _postgres: synctv_core_testing::TestContainer,
    _redis: synctv_core_testing::RedisContainer,
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
            _postgres: postgres,
            _redis: redis,
        }
    }

    async fn redis_connection_manager(&self) -> redis::aio::ConnectionManager {
        redis_connection_manager(&self.redis_client).await
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

async fn abort_test_task(handle: tokio::task::JoinHandle<()>) {
    handle.abort();
    let error = handle
        .await
        .expect_err("aborted test task should finish with JoinError");
    assert!(
        error.is_cancelled(),
        "aborted test task should be cancelled, got: {error}"
    );
}

async fn flush_provider_instances(infra: &TestInfra) {
    sqlx::query!("TRUNCATE TABLE media_provider_instances RESTART IDENTITY CASCADE")
        .execute(&infra.pool)
        .await
        .checked("Failed to truncate media_provider_instances");
}

fn test_encryption() -> CredentialEncryption {
    CredentialEncryption::new(&[0x42; 32]).checked("test encryption key should be valid")
}

fn provider_repo(pool: &PgPool) -> ProviderInstanceRepository {
    ProviderInstanceRepository::new_with_encryption(pool.clone(), test_encryption())
}

const REDIS_INVALIDATION_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const REDIS_INVALIDATION_WAIT_INTERVAL: Duration = Duration::from_millis(50);

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
        providers: vec![SourceProvider::Bilibili],
        enabled: true,
        created_at: now,
        updated_at: now,
    }
}

fn make_reachable_remote_instance(name: &str, host: &str, port: u16) -> ProviderInstance {
    let mut instance = make_test_instance(name);
    instance.endpoint = format!("http://{host}:{port}");
    instance.providers = vec![SourceProvider::Alist];
    instance
}

fn make_test_address_overrides(host: &str, port: u16) -> HashMap<String, SocketAddr> {
    HashMap::from([(
        host.to_string(),
        SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)),
    )])
}

fn validate_test_provider_secret(
    metadata: &MetadataMap,
    expected_secret: &str,
) -> Result<(), tonic::Status> {
    let value = metadata
        .get("x-provider-secret")
        .ok_or_else(|| tonic::Status::unauthenticated("Missing x-provider-secret header"))?;
    let provided = value
        .to_str()
        .map_err(|_| tonic::Status::unauthenticated("Invalid x-provider-secret header"))?;
    if provided != expected_secret {
        return Err(tonic::Status::unauthenticated("Invalid provider secret"));
    }
    Ok(())
}

fn validate_test_provider_request_secret<T>(
    request: &tonic::Request<T>,
    expected_secret: &str,
) -> Result<(), tonic::Status> {
    validate_test_provider_secret(request.metadata(), expected_secret)
}

fn authenticated_health_service<I>(
    health_service: I,
    auth_secret: String,
) -> InterceptedService<
    I,
    impl FnMut(tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> + Clone,
>
where
    I: Clone,
{
    let auth_secret = Arc::<str>::from(auth_secret);
    InterceptedService::new(health_service, move |request: tonic::Request<()>| {
        validate_test_provider_secret(request.metadata(), auth_secret.as_ref())?;
        Ok(request)
    })
}

async fn spawn_authenticated_provider_server(
    auth_secret: &str,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .checked("provider auth test server should bind to an ephemeral port");
    let addr = listener
        .local_addr()
        .checked("provider auth test server should expose a local address");

    let (reporter, health_service) = tonic_health::server::health_reporter();
    reporter
        .set_service_status("", ServingStatus::Serving)
        .await;
    reporter
        .set_serving::<AlistServer<AlistRemoteService>>()
        .await;

    let auth_secret = auth_secret.to_string();
    let authenticated_health = authenticated_health_service(health_service, auth_secret.clone());
    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(authenticated_health)
            .add_service(AlistServer::new(RemoteAuthProbeAlistService::new(
                auth_secret,
            )))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .checked("provider auth test server should run");
    });

    (addr, handle)
}

#[derive(Clone)]
struct RemoteAuthProbeAlistService {
    expected_secret: Arc<str>,
    fail_after_auth: bool,
    require_real_upstream_token: bool,
}

impl RemoteAuthProbeAlistService {
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
        validate_test_provider_request_secret(request, self.expected_secret.as_ref())
    }
}

#[tonic::async_trait]
impl synctv_media_providers::grpc::alist::alist_server::Alist for RemoteAuthProbeAlistService {
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
struct RemoteAuthProbeEmbyService {
    expected_secret: Arc<str>,
    fail_after_auth: bool,
}

impl RemoteAuthProbeEmbyService {
    fn failing_after_auth(expected_secret: String) -> Self {
        Self {
            expected_secret: Arc::<str>::from(expected_secret),
            fail_after_auth: true,
        }
    }

    fn validate_secret<T>(&self, request: &tonic::Request<T>) -> Result<(), tonic::Status> {
        validate_test_provider_request_secret(request, self.expected_secret.as_ref())
    }
}

#[tonic::async_trait]
impl synctv_media_providers::grpc::emby::emby_server::Emby for RemoteAuthProbeEmbyService {
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
        .checked("provider auth failure test server should bind to an ephemeral port");
    let addr = listener
        .local_addr()
        .checked("provider auth failure test server should expose a local address");

    let (reporter, health_service) = tonic_health::server::health_reporter();
    reporter
        .set_service_status("", ServingStatus::Serving)
        .await;
    reporter
        .set_serving::<AlistServer<AlistRemoteService>>()
        .await;

    let auth_secret = auth_secret.to_string();
    let authenticated_health = authenticated_health_service(health_service, auth_secret.clone());
    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(authenticated_health)
            .add_service(AlistServer::new(
                RemoteAuthProbeAlistService::failing_after_auth(auth_secret),
            ))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .checked("provider auth failure test server should run");
    });

    (addr, handle)
}

async fn spawn_authenticated_provider_server_rejecting_placeholder_upstream_auth(
    auth_secret: &str,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .checked("provider auth placeholder rejection server should bind");
    let addr = listener
        .local_addr()
        .checked("provider auth placeholder rejection server should expose local address");

    let (reporter, health_service) = tonic_health::server::health_reporter();
    reporter
        .set_service_status("", ServingStatus::Serving)
        .await;
    reporter
        .set_serving::<AlistServer<AlistRemoteService>>()
        .await;

    let auth_secret = auth_secret.to_string();
    let authenticated_health = authenticated_health_service(health_service, auth_secret.clone());
    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(authenticated_health)
            .add_service(AlistServer::new(
                RemoteAuthProbeAlistService::requiring_real_upstream_token(auth_secret),
            ))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .checked("provider auth placeholder rejection server should run");
    });

    (addr, handle)
}

async fn spawn_authenticated_emby_provider_server_with_handler_failure(
    auth_secret: &str,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .checked("emby auth failure test server should bind to an ephemeral port");
    let addr = listener
        .local_addr()
        .checked("emby auth failure test server should expose a local address");

    let (reporter, health_service) = tonic_health::server::health_reporter();
    reporter
        .set_service_status("", ServingStatus::Serving)
        .await;
    reporter
        .set_serving::<EmbyServer<EmbyRemoteService>>()
        .await;

    let auth_secret = auth_secret.to_string();
    let authenticated_health = authenticated_health_service(health_service, auth_secret.clone());
    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(authenticated_health)
            .add_service(EmbyServer::new(
                RemoteAuthProbeEmbyService::failing_after_auth(auth_secret),
            ))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .checked("emby auth failure test server should run");
    });

    (addr, handle)
}

async fn spawn_stalling_tcp_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .checked("stalling test server should bind to an ephemeral port");
    let addr = listener
        .local_addr()
        .checked("stalling test server should expose a local address");

    let handle = tokio::spawn(async move {
        loop {
            let (stream, _) = listener
                .accept()
                .await
                .checked("stalling test server should accept connections");
            tokio::spawn(async move {
                let _stream = stream;
                std::future::pending::<()>().await;
            });
        }
    });

    (addr, handle)
}

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
        providers: vec![SourceProvider::Emby],
        enabled: true,
        created_at: now,
        updated_at: now,
    }
}

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
    manager
        .add(instance.clone())
        .await
        .checked("test operation should succeed");
    let status = manager.runtime_status("test-instance-1").await;

    assert!(
        status.available,
        "validated remote runtime should be available"
    );
    let repo = provider_repo(&infra.pool);
    let fetched = repo
        .get_by_name("test-instance-1")
        .await
        .checked("test operation should succeed");
    assert!(fetched.is_some());
    assert_eq!(
        fetched.checked("test operation should succeed").name,
        "test-instance-1"
    );

    abort_test_task(health_handle).await;
}

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
    invalidation1
        .start()
        .await
        .checked("test operation should succeed");
    invalidation2
        .start()
        .await
        .checked("test operation should succeed");

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

    manager2
        .start_invalidation_listener()
        .await
        .checked("test operation should succeed");

    let instance = make_reachable_remote_instance("test-instance-5", host, health_addr.port());
    manager1
        .add(instance.clone())
        .await
        .checked("test operation should succeed");
    assert!(
        manager2.runtime_status("test-instance-5").await.available,
        "manager2 should warm provider cache before delete invalidation"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager1
        .delete("test-instance-5")
        .await
        .checked("test operation should succeed");

    wait_until(
        REDIS_INVALIDATION_WAIT_TIMEOUT,
        REDIS_INVALIDATION_WAIT_INTERVAL,
        || {
            let manager2 = &manager2;
            async move { !manager2.runtime_status("test-instance-5").await.available }
        },
    )
    .await;
    let instances2 = manager2
        .list()
        .await
        .checked("test operation should succeed");
    assert!(
        !instances2.contains(&"test-instance-5".to_string()),
        "Manager2 should not list deleted instance"
    );
    let status = manager2.runtime_status("test-instance-5").await;
    assert!(!status.available, "Deleted instance should be unavailable");

    manager1.shutdown().await;
    manager2.shutdown().await;
    invalidation1.stop().await;
    invalidation2.stop().await;
    abort_test_task(health_handle).await;
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
        .failed("provider add must fail closed when invalidation publish fails");

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
            .checked("repository lookup should succeed")
            .is_none(),
        "provider row must be rolled back when invalidation publish fails"
    );
    assert!(
        !manager.runtime_status(&instance.name).await.available,
        "local channel cache must be cleared when add rollback occurs"
    );

    abort_test_task(health_handle).await;
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
        .checked("seed provider instance should be created");

    let failing_manager = RemoteProviderManager::new_with_address_overrides(
        repo.clone(),
        Some(unavailable_invalidation_service(format!(
            "test:provider:delete-rollback:{}",
            synctv_common::snanoid!(8)
        ))),
        make_test_address_overrides(host, health_addr.port()),
    );

    let status_before_delete = failing_manager.runtime_status(&instance.name).await;
    assert!(
        status_before_delete.available,
        "runtime should be available before delete"
    );
    let err = failing_manager
        .delete(&instance.name)
        .await
        .failed("provider delete must fail closed when invalidation publish fails");

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
        .checked("repository lookup should succeed")
        .checked("provider row must be restored after delete rollback");
    assert_eq!(
        restored.endpoint, instance.endpoint,
        "delete rollback must restore the original provider configuration"
    );
    let status_after_delete = failing_manager.runtime_status(&instance.name).await;
    assert!(
        status_after_delete.available,
        "delete rollback must restore runtime availability"
    );
    assert_eq!(
        status_after_delete.has_auth_secret, status_before_delete.has_auth_secret,
        "delete rollback must restore auth-secret readiness"
    );

    abort_test_task(health_handle).await;
}

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
    instance.providers = vec![SourceProvider::Alist];
    manager
        .add(instance.clone())
        .await
        .checked("test operation should succeed");
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

    abort_test_task(health_handle).await;
}

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
    manager
        .add(instance_enabled)
        .await
        .checked("test operation should succeed");

    let instance_disabled = make_test_instance("test-instance-7b");
    let mut disabled = instance_disabled.clone();
    disabled.enabled = false;
    manager
        .add(disabled)
        .await
        .checked("test operation should succeed");
    let health_results = manager.health_check().await;
    assert!(
        health_results.contains_key("test-instance-7a"),
        "Health check should include enabled instance"
    );
    assert!(
        !health_results.contains_key("test-instance-7b"),
        "Health check should skip disabled instance"
    );

    abort_test_task(health_handle).await;
}

async fn scenario_health_check_reports_enabled_instance_with_invalid_secret_as_unhealthy() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides_and_ssrf_guard(
        Arc::new(repo),
        None,
        HashMap::new(),
        synctv_common::ssrf::SsrfGuard::disabled(),
    );

    let mut invalid = make_test_instance("test-instance-7c-invalid-secret");
    invalid.jwt_secret = Some("shared\nsecret".to_string());
    provider_repo(&infra.pool)
        .create(&invalid)
        .await
        .checked("invalid remote row should persist for health-check coverage");

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
        .checked("invalid missing-secret row should persist for health-check coverage");

    let health_results = manager.health_check().await;
    assert!(
        health_results.contains_key(&invalid.name),
        "enabled remote instances with missing secrets should still appear in health results"
    );
    assert!(
        !health_results[&invalid.name],
        "enabled remote instances without secrets must be reported unhealthy"
    );

    abort_test_task(health_handle).await;
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
    wrong.providers = vec![SourceProvider::Alist];
    wrong.jwt_secret = Some("wrong-secret".to_string());

    provider_repo(&infra.pool)
        .create(&wrong)
        .await
        .checked("wrong-secret row should persist for health-check coverage");

    let health_results = manager.health_check().await;
    assert!(
        health_results.contains_key(&wrong.name),
        "enabled remote instances with wrong secrets should still appear in health results"
    );
    assert!(
        !health_results[&wrong.name],
        "enabled remote instances with wrong but well-formed secrets must be reported unhealthy"
    );

    abort_test_task(health_handle).await;
}

async fn scenario_health_check_ignores_authenticated_provider_handler_failure() {
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
    broken.providers = vec![SourceProvider::Alist];

    provider_repo(&infra.pool)
        .create(&broken)
        .await
        .checked("handler-failure row should persist for health-check coverage");

    let health_results = manager.health_check().await;
    assert!(
        health_results.contains_key(&broken.name),
        "instances with authenticated provider failures should still appear in health results"
    );
    assert!(
        health_results[&broken.name],
        "management health checks must not depend on authenticated Alist business handlers"
    );

    abort_test_task(health_handle).await;
}

async fn scenario_add_alist_instance_does_not_require_external_upstream_auth_for_management_validation(
) {
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
        .checked("management validation should not depend on fake upstream Alist credentials");

    abort_test_task(health_handle).await;
}

async fn scenario_health_check_ignores_emby_authenticated_provider_handler_failure() {
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
    broken.providers = vec![SourceProvider::Emby];

    provider_repo(&infra.pool)
        .create(&broken)
        .await
        .checked("emby handler-failure row should persist for health-check coverage");

    let health_results = manager.health_check().await;
    assert!(
        health_results.contains_key(&broken.name),
        "emby instances with authenticated provider failures should appear in health results"
    );
    assert!(
        health_results[&broken.name],
        "management health checks must not depend on authenticated Emby business handlers"
    );

    abort_test_task(health_handle).await;
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
    instance.providers = vec![SourceProvider::Emby];

    manager
        .add(instance.clone())
        .await
        .checked("management validation should only require transport health, not authenticated Emby handler success");
    assert!(
        provider_repo(&infra.pool)
            .get_by_name(&instance.name)
            .await
            .checked("lookup should succeed")
            .is_some(),
        "successful management validation must persist the Emby instance even when authenticated handlers are unhealthy"
    );

    abort_test_task(health_handle).await;
}

async fn scenario_tls_configuration_secure() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let _repo = provider_repo(&infra.pool);

    let instance = make_test_instance_tls("test-instance-8", false);

    // This avoids the rustls crypto provider issue in test environment
    let repo_instance = provider_repo(&infra.pool);
    repo_instance
        .create(&instance)
        .await
        .checked("test operation should succeed");
    let fetched = repo_instance
        .get_by_name("test-instance-8")
        .await
        .checked("test operation should succeed");
    assert!(fetched.is_some());
    let fetched = fetched.checked("test operation should succeed");
    assert!(fetched.tls, "Instance should have TLS enabled");
    assert!(
        !fetched.insecure_tls,
        "Instance should not have insecure TLS"
    );
    let manager = RemoteProviderManager::new(Arc::new(provider_repo(&infra.pool)));
    let instances = manager
        .list()
        .await
        .checked("test operation should succeed");
    assert!(
        instances.contains(&"test-instance-8".to_string()),
        "Should list the TLS instance"
    );
}

async fn scenario_get_returns_none_for_absent_remote_instance() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let status = manager.runtime_status("non-existent-instance").await;

    assert!(
        !status.available,
        "Non-existent instance should be unavailable for best-effort probes"
    );
}

async fn scenario_runtime_status_reports_missing_remote_instance_unavailable() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let status = manager.runtime_status("missing-instance").await;

    assert!(
        !status.available,
        "missing remote instance should be unavailable"
    );
}

async fn scenario_runtime_status_reports_misconfigured_remote_instance_unavailable() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut invalid = make_test_instance("misconfigured-instance");
    invalid.jwt_secret = None;
    provider_repo(&infra.pool)
        .create(&invalid)
        .await
        .checked("invalid remote row should persist for resolution coverage");

    let status = manager.runtime_status(&invalid.name).await;

    assert!(
        !status.available,
        "misconfigured remote instance should be unavailable"
    );
}

async fn scenario_fallback_when_channel_creation_fails() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

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
    manager
        .add(instance.clone())
        .await
        .checked("test operation should succeed");
    let repo = provider_repo(&infra.pool);
    let fetched = repo
        .get_by_name("test-instance-13")
        .await
        .checked("test operation should succeed");
    assert!(fetched.is_some());
    assert!(fetched.checked("test operation should succeed").enabled);
    manager
        .disable("test-instance-13")
        .await
        .checked("test operation should succeed");
    let fetched = repo
        .get_by_name("test-instance-13")
        .await
        .checked("test operation should succeed");
    assert!(fetched.is_some());
    assert!(!fetched.checked("test operation should succeed").enabled);
    let status = manager.runtime_status("test-instance-13").await;
    assert!(!status.available, "Disabled instance should be unavailable");
    manager
        .enable("test-instance-13")
        .await
        .checked("test operation should succeed");
    let fetched = repo
        .get_by_name("test-instance-13")
        .await
        .checked("test operation should succeed");
    assert!(fetched.is_some());
    assert!(fetched.checked("test operation should succeed").enabled);

    abort_test_task(health_handle).await;
}

async fn scenario_enable_with_invalid_endpoint_preserves_disabled_state() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut invalid_disabled = make_test_instance("test-instance-13-invalid-enable");
    invalid_disabled.enabled = false;
    invalid_disabled.endpoint = "http://127.0.0.1:50051".to_string();

    let repo = provider_repo(&infra.pool);
    repo.create(&invalid_disabled)
        .await
        .checked("test operation should succeed");

    let result = manager.enable("test-instance-13-invalid-enable").await;
    assert!(result.is_err(), "enabling invalid config should fail");

    let persisted = repo
        .get_by_name("test-instance-13-invalid-enable")
        .await
        .checked("test operation should succeed")
        .checked("instance should still exist");
    assert!(
        !persisted.enabled,
        "failed enable must not leave the DB row enabled"
    );

    let status = manager
        .runtime_status("test-instance-13-invalid-enable")
        .await;
    assert!(
        !status.available,
        "failed enable must not leave a cached channel behind"
    );
}

async fn scenario_enable_remote_instances_validate_jwt_secret() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut disabled_missing_secret = make_test_instance("test-instance-13-missing-secret-enable");
    disabled_missing_secret.enabled = false;
    disabled_missing_secret.jwt_secret = None;

    let repo = provider_repo(&infra.pool);
    repo.create(&disabled_missing_secret)
        .await
        .checked("test operation should succeed");

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
        .checked("test operation should succeed")
        .checked("instance should still exist");
    assert!(
        !persisted.enabled,
        "failed enable must not leave the DB row enabled"
    );

    let status = manager
        .runtime_status("test-instance-13-missing-secret-enable")
        .await;
    assert!(
        !status.available,
        "invalid row without jwt_secret must not resolve to an unusable remote connection"
    );

    let mut invalid = make_test_instance("test-instance-13-invalid-already-enabled");
    invalid.jwt_secret = None;
    invalid.enabled = true;
    invalid.comment = Some("invalid enabled row".to_string());

    repo.create(&invalid)
        .await
        .checked("invalid enabled row should persist");

    let result = manager
        .enable("test-instance-13-invalid-already-enabled")
        .await;
    assert!(
        result.is_err(),
        "re-enabling an already-enabled invalid row without jwt_secret must fail"
    );

    let error_message = result.failed("missing secret should fail").to_string();
    assert!(
        error_message.contains("jwt_secret"),
        "error should explain missing jwt_secret: {error_message}"
    );

    let persisted = repo
        .get_by_name("test-instance-13-invalid-already-enabled")
        .await
        .checked("lookup should succeed")
        .checked("invalid row should still exist");
    assert!(persisted.enabled, "invalid row should remain enabled");
    assert_eq!(persisted.jwt_secret, None);

    let status = manager
        .runtime_status("test-instance-13-invalid-already-enabled")
        .await;
    assert!(
        !status.available,
        "invalid row without jwt_secret must not resolve to an unusable remote connection"
    );
}

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
    manager
        .add(instance.clone())
        .await
        .checked("test operation should succeed");

    let result = manager.reconnect("test-instance-14").await;
    assert!(
        result.is_ok(),
        "Reconnect should succeed for a reachable remote instance"
    );
    manager
        .disable("test-instance-14")
        .await
        .checked("test operation should succeed");
    let result = manager.reconnect("test-instance-14").await;
    assert!(
        result.is_err(),
        "Reconnect should fail for disabled instance"
    );

    abort_test_task(health_handle).await;
}

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
    manager
        .add(instance.clone())
        .await
        .checked("test operation should succeed");
    let result = manager.add(instance).await;
    assert!(result.is_err(), "Adding duplicate instance should fail");

    if let Err(e) = result {
        assert!(
            format!("{e:?}").contains("AlreadyExists"),
            "Error should be AlreadyExists variant"
        );
    }

    abort_test_task(health_handle).await;
}

async fn scenario_add_disabled_instance_is_not_retrievable_via_get() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut disabled = make_test_instance("test-instance-15-disabled");
    disabled.enabled = false;
    manager
        .add(disabled)
        .await
        .checked("test operation should succeed");

    let fetched = provider_repo(&infra.pool)
        .get_by_name("test-instance-15-disabled")
        .await
        .checked("test operation should succeed")
        .checked("instance should exist");
    assert!(!fetched.enabled, "instance should remain disabled in DB");

    let status = manager.runtime_status("test-instance-15-disabled").await;
    assert!(
        !status.available,
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
    manager
        .add(instance.clone())
        .await
        .checked("test operation should succeed");

    let initial = manager
        .runtime_status("test-instance-15-update-disabled")
        .await;
    assert!(initial.available, "enabled instance should be retrievable");

    let mut disabled = instance;
    disabled.enabled = false;
    disabled.comment = Some("now disabled".to_string());
    manager
        .update(disabled)
        .await
        .checked("test operation should succeed");

    let fetched = provider_repo(&infra.pool)
        .get_by_name("test-instance-15-update-disabled")
        .await
        .checked("test operation should succeed")
        .checked("instance should exist");
    assert!(!fetched.enabled, "instance should be disabled in DB");

    let status = manager
        .runtime_status("test-instance-15-update-disabled")
        .await;
    assert!(
        !status.available,
        "update(enabled=false) must evict any cached channel"
    );

    abort_test_task(health_handle).await;
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

    let result1 = task1.await.checked("test operation should succeed");
    let result2 = task2.await.checked("test operation should succeed");
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

    let stored_count: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM media_provider_instances WHERE name = $1"#,
        "test-instance-15-concurrent-dup"
    )
    .fetch_one(&infra.pool)
    .await
    .checked("test operation should succeed");
    assert_eq!(stored_count, 1, "only one DB row should be persisted");

    abort_test_task(health_handle).await;
}

async fn scenario_update_nonexistent_instance_fails() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));
    let instance = make_test_instance("test-instance-16");
    let result = manager.update(instance).await;
    assert!(
        result.is_err(),
        "Updating non-existent instance should fail"
    );
}

async fn scenario_update_remote_instances_validate_jwt_secret() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));
    let repo = provider_repo(&infra.pool);

    let mut invalid_existing = make_test_instance("test-instance-invalid-update");
    invalid_existing.jwt_secret = None;
    invalid_existing.comment = Some("invalid row".to_string());

    repo.create(&invalid_existing)
        .await
        .checked("invalid row should persist");

    let mut invalid_existing_update = invalid_existing.clone();
    invalid_existing_update.comment = Some("updated comment".to_string());
    invalid_existing_update.timeout = "2s".to_string();

    let result = manager.update(invalid_existing_update.clone()).await;
    assert!(
        result.is_err(),
        "updating an invalid remote row without jwt_secret must fail"
    );

    let error_message = result.failed("missing secret should fail").to_string();
    assert!(
        error_message.contains("jwt_secret"),
        "error should explain missing jwt_secret: {error_message}"
    );

    let fetched = repo
        .get_by_name(&invalid_existing_update.name)
        .await
        .checked("lookup should succeed")
        .checked("updated instance should exist");
    assert_ne!(fetched.comment, invalid_existing_update.comment);
    assert_ne!(fetched.timeout, "2s");
    assert_eq!(fetched.jwt_secret, None);

    let mut provider_change_missing_secret =
        make_test_instance("test-instance-provider-change-missing-secret");
    provider_change_missing_secret.providers = vec![SourceProvider::LiveProxy];
    provider_change_missing_secret.jwt_secret = None;

    repo.create(&provider_change_missing_secret)
        .await
        .checked("legacy invalid row should persist without jwt_secret");

    let mut changed_provider_missing_secret = provider_change_missing_secret.clone();
    changed_provider_missing_secret.providers = vec![SourceProvider::Bilibili];

    let result = manager
        .update(changed_provider_missing_secret.clone())
        .await;
    assert!(
        result.is_err(),
        "changing the declared provider must still reject a missing jwt_secret"
    );

    let error_message = result.failed("missing secret should fail").to_string();
    assert!(
        error_message.contains("jwt_secret"),
        "error should explain missing jwt_secret: {error_message}"
    );

    let persisted = repo
        .get_by_name(&changed_provider_missing_secret.name)
        .await
        .checked("lookup should succeed")
        .checked("instance should still exist");
    assert_eq!(
        persisted.providers,
        vec![SourceProvider::LiveProxy],
        "failed update must not persist the remote-capable provider set"
    );
    assert_eq!(persisted.jwt_secret, None);

    let existing_remote = make_test_instance("test-instance-remote-update-missing-secret");
    repo.create(&existing_remote)
        .await
        .checked("remote row with jwt_secret should persist");

    let mut existing_remote_missing_secret = existing_remote.clone();
    existing_remote_missing_secret.comment = Some("updated comment".to_string());
    existing_remote_missing_secret.jwt_secret = None;

    let result = manager.update(existing_remote_missing_secret.clone()).await;
    assert!(
        result.is_err(),
        "updating an existing remote instance with missing jwt_secret must fail"
    );

    let error = result.failed("missing secret should fail");
    let error_message = error.to_string();
    assert!(
        error_message.contains("jwt_secret"),
        "error should explain missing jwt_secret: {error_message}"
    );

    let persisted = repo
        .get_by_name(&existing_remote_missing_secret.name)
        .await
        .checked("lookup should succeed")
        .checked("instance should still exist");
    assert_ne!(
        persisted.comment, existing_remote_missing_secret.comment,
        "failed update must not persist other field changes"
    );
    assert_eq!(
        persisted.jwt_secret.as_deref(),
        Some("remote-provider-test-secret"),
        "failed update must preserve the existing valid jwt_secret"
    );

    let mut provider_change_invalid_secret =
        make_test_instance("test-instance-provider-change-invalid-secret");
    provider_change_invalid_secret.providers = vec![SourceProvider::LiveProxy];
    provider_change_invalid_secret.jwt_secret = None;
    provider_change_invalid_secret.enabled = false;

    repo.create(&provider_change_invalid_secret)
        .await
        .checked("legacy invalid row should persist without jwt_secret");

    let mut changed_provider_invalid_secret = provider_change_invalid_secret.clone();
    changed_provider_invalid_secret.providers = vec![SourceProvider::Bilibili];
    changed_provider_invalid_secret.jwt_secret = Some("shared\nsecret".to_string());

    let result = manager
        .update(changed_provider_invalid_secret.clone())
        .await;
    assert!(
        result.is_err(),
        "changing the declared provider must still reject an invalid jwt_secret"
    );

    let error = result.failed("invalid secret should fail");
    let error_message = error.to_string();
    assert!(
        error_message.contains("secret"),
        "error should explain invalid jwt_secret syntax: {error_message}"
    );

    let persisted = repo
        .get_by_name(&changed_provider_invalid_secret.name)
        .await
        .checked("lookup should succeed")
        .checked("instance should still exist");
    assert_eq!(
        persisted.providers,
        vec![SourceProvider::LiveProxy],
        "failed update must not persist the remote-capable provider set"
    );
    assert_eq!(persisted.jwt_secret, None);
}

async fn scenario_delete_nonexistent_instance_fails() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));
    let result = manager.delete("test-instance-17").await;
    assert!(
        result.is_err(),
        "Deleting non-existent instance should fail"
    );
}

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
        manager
            .add(instance)
            .await
            .checked("test operation should succeed");
    }
    let mut disabled = make_test_instance("test-instance-18b-disabled");
    disabled.enabled = false;
    manager
        .add(disabled)
        .await
        .checked("test operation should succeed");
    let all_instances = manager
        .get_all_instances()
        .await
        .checked("test operation should succeed");

    assert!(
        all_instances.len() >= 4,
        "Should have at least 4 instances (3 enabled + 1 disabled)"
    );
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

    abort_test_task(health_handle).await;
}

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
    manager
        .add(instance.clone())
        .await
        .checked("test operation should succeed");
    assert!(
        manager.runtime_status("test-instance-19").await.available,
        "provider cache should warm before list"
    );
    let instances = manager
        .list()
        .await
        .checked("test operation should succeed");
    assert!(
        instances.contains(&"test-instance-19".to_string()),
        "Should list the instance even without Redis"
    );

    abort_test_task(health_handle).await;
}

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
        manager
            .add(instance)
            .await
            .checked("test operation should succeed");
    }
    let result = manager.init().await;
    assert!(result.is_ok(), "Init should succeed");
    let instances = manager
        .list()
        .await
        .checked("test operation should succeed");
    assert!(
        instances.len() >= 3,
        "Should list at least 3 instances after init"
    );

    abort_test_task(health_handle).await;
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
        .checked("invalid remote row should persist for fail-closed coverage");

    let healthy =
        make_reachable_remote_instance("test-instance-20-healthy", host, health_addr.port());
    manager
        .add(healthy.clone())
        .await
        .checked("healthy remote instance should be added");

    manager
        .init()
        .await
        .failed("init must fail closed when persisted remote config is invalid");

    let healthy_status = manager.runtime_status(&healthy.name).await;
    assert!(
        healthy_status.available,
        "healthy instance should still be available from its add-time cache"
    );
    assert!(healthy_status.has_auth_secret);

    let invalid_status = manager.runtime_status(&invalid.name).await;
    assert!(
        !invalid_status.available,
        "invalid remote instance must not resolve after init failure"
    );

    abort_test_task(health_handle).await;
}

async fn scenario_internal_ips_fail_connectivity_validation_when_ssrf_is_explicitly_disabled() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides_and_ssrf_guard(
        Arc::new(repo),
        None,
        HashMap::new(),
        synctv_common::ssrf::SsrfGuard::disabled(),
    );

    // Try to create instance with an internal IP. With SSRF explicitly disabled,
    // static validation no longer rejects it, but the live health check still
    // fails because nothing is listening on that endpoint.
    let mut instance = make_test_instance("test-instance-21");
    instance.endpoint = "http://127.0.0.1:50051".to_string();

    let result = manager.add(instance).await;

    // Should still fail because connectivity validation runs before persisting.
    assert!(
        result.is_err(),
        "Adding instance with an unreachable internal IP should fail connectivity validation"
    );

    let error_msg = result
        .failed("internal IP endpoint should fail")
        .to_string();
    assert!(
        error_msg.contains("health check failed")
            || error_msg.contains("Connection refused")
            || error_msg.contains("service is currently unavailable"),
        "Error should reflect connectivity validation, got: {error_msg}"
    );
    assert!(
        provider_repo(&infra.pool)
            .get_by_name("test-instance-21")
            .await
            .checked("lookup should succeed")
            .is_none(),
        "failed connectivity validation must not persist the instance"
    );
}

async fn scenario_ssrf_validation_allows_public_endpoints() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

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
        .failed("unreachable public endpoint should fail")
        .to_string();
    assert!(
        !error_message.contains("SSRF validation: host"),
        "public host should not be rejected by static SSRF policy: {error_message}"
    );
    assert!(
        provider_repo(&infra.pool)
            .get_by_name("test-instance-22")
            .await
            .checked("lookup should succeed")
            .is_none(),
        "failed connectivity validation must not persist the instance"
    );
}

async fn scenario_runtime_status_reports_remote_instance_available() {
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
    manager
        .add(instance)
        .await
        .checked("test operation should succeed");

    let status = manager.runtime_status("test-instance-23").await;

    assert!(status.available);
    assert!(status.has_auth_secret);

    abort_test_task(health_handle).await;
}

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
    for i in 1..=10 {
        let instance = make_reachable_remote_instance(
            &format!("test-instance-24-{i}"),
            host,
            health_addr.port(),
        );
        manager
            .add(instance)
            .await
            .checked("test operation should succeed");
    }
    let instances = manager
        .list()
        .await
        .checked("test operation should succeed");
    assert!(instances.len() >= 10, "Should list at least 10 instances");

    abort_test_task(health_handle).await;
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
    invalidation1
        .start()
        .await
        .checked("test operation should succeed");
    invalidation2
        .start()
        .await
        .checked("test operation should succeed");

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

    manager2
        .start_invalidation_listener()
        .await
        .checked("test operation should succeed");

    let instance = make_reachable_remote_instance("test-instance-prefix", host, health_addr.port());
    manager1
        .add(instance)
        .await
        .checked("test operation should succeed");
    assert!(
        manager2
            .runtime_status("test-instance-prefix")
            .await
            .available,
        "manager2 should warm provider cache before prefix invalidation"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;

    manager1
        .delete("test-instance-prefix")
        .await
        .checked("test operation should succeed");

    wait_until(
        REDIS_INVALIDATION_WAIT_TIMEOUT,
        REDIS_INVALIDATION_WAIT_INTERVAL,
        || {
            let manager2 = &manager2;
            async move {
                !manager2
                    .runtime_status("test-instance-prefix")
                    .await
                    .available
            }
        },
    )
    .await;

    manager1.shutdown().await;
    manager2.shutdown().await;
    invalidation1.stop().await;
    invalidation2.stop().await;
    abort_test_task(health_handle).await;
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
    invalidation
        .start()
        .await
        .checked("test operation should succeed");

    let manager = RemoteProviderManager::new_with_invalidation(
        Arc::new(provider_repo(&infra.pool)),
        Some(invalidation.clone()),
    );

    manager
        .start_invalidation_listener()
        .await
        .checked("test operation should succeed");
    manager
        .start_invalidation_listener()
        .await
        .checked("second start should be idempotent");

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
    invalidation1
        .start()
        .await
        .checked("test operation should succeed");
    invalidation2
        .start()
        .await
        .checked("test operation should succeed");

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
    manager1
        .add(instance)
        .await
        .checked("test operation should succeed");

    let status = manager2.runtime_status("durable-provider-instance").await;
    assert!(
        status.available,
        "manager2 should cache the instance before delete"
    );

    manager1
        .delete("durable-provider-instance")
        .await
        .checked("test operation should succeed");

    manager2
        .start_invalidation_listener()
        .await
        .checked("late-started listener should catch up through durable invalidation stream");

    wait_until(
        REDIS_INVALIDATION_WAIT_TIMEOUT,
        REDIS_INVALIDATION_WAIT_INTERVAL,
        || {
            let manager2 = &manager2;
            async move {
                !manager2
                    .runtime_status("durable-provider-instance")
                    .await
                    .available
            }
        },
    )
    .await;

    manager1.shutdown().await;
    manager2.shutdown().await;
    invalidation1.stop().await;
    invalidation2.stop().await;
    abort_test_task(health_handle).await;
}

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
            SourceProvider::Bilibili,
            SourceProvider::Alist,
            SourceProvider::Emby,
        ],
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    assert!(instance.supports_provider("bilibili"));
    assert!(instance.supports_provider("alist"));
    assert!(instance.supports_provider("emby"));
    assert!(!instance.supports_provider("direct_url"));
    assert!(!instance.supports_provider("rtmp"));
}

async fn scenario_add_remote_instances_validate_jwt_secret() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut missing_secret = make_test_instance("test-instance-missing-secret");
    missing_secret.jwt_secret = None;

    let result = manager.add(missing_secret).await;
    assert!(
        result.is_err(),
        "remote instance without jwt_secret must be rejected"
    );

    let error = result.failed("missing secret should fail");
    let error_message = error.to_string();
    assert!(
        error_message.contains("jwt_secret"),
        "error should explain missing jwt_secret: {error_message}"
    );

    let mut invalid_secret = make_test_instance("test-instance-invalid-secret-disabled");
    invalid_secret.enabled = false;
    invalid_secret.jwt_secret = Some("shared\nsecret".to_string());

    let result = manager.add(invalid_secret.clone()).await;
    assert!(
        result.is_err(),
        "disabled remote instance with invalid jwt_secret must be rejected"
    );

    let error = result.failed("invalid secret should fail");
    let error_message = error.to_string();
    assert!(
        error_message.contains("secret"),
        "error should explain invalid jwt_secret syntax: {error_message}"
    );

    assert!(
        provider_repo(&infra.pool)
            .get_by_name(&invalid_secret.name)
            .await
            .checked("lookup should succeed")
            .is_none(),
        "failed add must not persist the invalid remote instance"
    );
}

async fn scenario_add_instance_accepts_all_provider_types() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let (health_addr, health_handle) =
        spawn_authenticated_provider_server("remote-provider-test-secret").await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new_with_address_overrides(
        Arc::new(repo),
        None,
        HashMap::from([(
            "all-providers.test.localhost".to_string(),
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, health_addr.port())),
        )]),
    );

    let now = Utc::now();
    let instance = ProviderInstance {
        name: "test-all-provider-types-instance".to_string(),
        endpoint: format!("http://all-providers.test.localhost:{}", health_addr.port()),
        comment: Some("all provider types".to_string()),
        jwt_secret: Some("remote-provider-test-secret".to_string()),
        custom_ca: None,
        timeout: "1s".to_string(),
        tls: false,
        insecure_tls: false,
        providers: SourceProvider::ALL.to_vec(),
        enabled: true,
        created_at: now,
        updated_at: now,
    };

    manager
        .add(instance.clone())
        .await
        .checked("every source provider type should be accepted by remote instances");

    let fetched = provider_repo(&infra.pool)
        .get_by_name(&instance.name)
        .await
        .checked("lookup should succeed")
        .checked("accepted provider instance should be persisted");
    assert_eq!(fetched.providers, SourceProvider::ALL);

    abort_test_task(health_handle).await;
}

async fn scenario_add_unreachable_remote_instance_fails_connectivity_validation() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut instance = make_test_instance("test-instance-unreachable-add");
    instance.endpoint = "http://unreachable-provider.example.invalid:50051".to_string();
    instance.providers = vec![SourceProvider::Alist];
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
            .checked("lookup should succeed")
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
    instance.providers = vec![SourceProvider::Alist];

    manager
        .add(instance.clone())
        .await
        .checked("reachable remote instance should pass connectivity validation");

    let status = manager.runtime_status(&instance.name).await;
    assert!(
        status.available,
        "reachable remote instance should be cached"
    );
    assert!(
        status.has_auth_secret,
        "validated remote instance should retain auth-secret readiness"
    );

    let stored = provider_repo(&infra.pool)
        .get_by_name(&instance.name)
        .await
        .checked("lookup should succeed")
        .checked("reachable instance should be persisted");
    assert_eq!(stored.endpoint, instance.endpoint);

    abort_test_task(health_handle).await;
}

async fn scenario_enable_unreachable_remote_instance_preserves_disabled_state() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut instance = make_test_instance("test-instance-unreachable-enable");
    instance.enabled = false;
    instance.endpoint = "http://unreachable-provider.example.invalid:50051".to_string();
    instance.providers = vec![SourceProvider::Alist];
    instance.timeout = "1s".to_string();

    provider_repo(&infra.pool)
        .create(&instance)
        .await
        .checked("disabled instance should persist");

    let result = manager.enable(&instance.name).await;
    assert!(
        result.is_err(),
        "enable must fail when the remote endpoint is unreachable"
    );

    let stored = provider_repo(&infra.pool)
        .get_by_name(&instance.name)
        .await
        .checked("lookup should succeed")
        .checked("instance should still exist");
    assert!(
        !stored.enabled,
        "failed enable must leave the instance disabled in the database"
    );
}

async fn scenario_reconnect_unreachable_remote_instance_fails_connectivity_validation() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;

    let repo = provider_repo(&infra.pool);
    let manager = RemoteProviderManager::new(Arc::new(repo));

    let mut instance = make_test_instance("test-instance-unreachable-reconnect");
    instance.endpoint = "http://unreachable-provider.example.invalid:50051".to_string();
    instance.providers = vec![SourceProvider::Alist];
    instance.timeout = "1s".to_string();

    provider_repo(&infra.pool)
        .create(&instance)
        .await
        .checked("enabled instance should persist for reconnect coverage");

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
    instance.providers = vec![SourceProvider::Alist];
    manager
        .add(instance.clone())
        .await
        .checked("reachable instance should be added before update");

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
        .checked("lookup should succeed")
        .checked("instance should still exist");
    assert_eq!(
        stored.endpoint, instance.endpoint,
        "failed update must preserve the last known-good endpoint"
    );

    abort_test_task(health_handle).await;
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

    abort_test_task(stall_handle).await;
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
        .checked("invalid row without jwt_secret should still persist");

    let status = manager.runtime_status(&instance.name).await;
    assert!(
        !status.available,
        "invalid remote instance without jwt_secret must not build a runtime connection"
    );

    manager
        .init()
        .await
        .failed("init must fail closed for invalid persisted remote rows");

    let cached = manager.runtime_status(&instance.name).await;
    assert!(
        !cached.available,
        "pre-warm must not cache an invalid remote instance without jwt_secret"
    );
}

async fn scenario_provider_instance_parse_timeout() {
    let infra = TestInfra::new().await;
    flush_provider_instances(&infra).await;
    let mut instance1 = make_test_instance("test-26a");
    instance1.timeout = "10s".to_string();
    assert_eq!(
        instance1
            .parse_timeout()
            .checked("test operation should succeed"),
        Duration::from_secs(10)
    );

    let mut instance2 = make_test_instance("test-26b");
    instance2.timeout = "30s".to_string();
    assert_eq!(
        instance2
            .parse_timeout()
            .checked("test operation should succeed"),
        Duration::from_secs(30)
    );

    let mut instance3 = make_test_instance("test-26c");
    instance3.timeout = "5m".to_string();
    assert_eq!(
        instance3
            .parse_timeout()
            .checked("test operation should succeed"),
        Duration::from_mins(5)
    );
    let mut instance4 = make_test_instance("test-26d");
    instance4.timeout = "invalid".to_string();
    assert!(
        instance4.parse_timeout().is_err(),
        "Invalid timeout should parse error"
    );
}

fn install_rustls_provider_once() {
    install_rustls_provider_once_with_selected_backend();
}

#[cfg(feature = "tls-aws-lc")]
fn install_rustls_provider_once_with_selected_backend() {
    let _ = rustls::crypto::CryptoProvider::install_default(
        rustls::crypto::aws_lc_rs::default_provider(),
    );
}

#[cfg(all(
    not(feature = "tls-aws-lc"),
    any(
        feature = "tls-ring",
        feature = "tls-webpki-roots",
        feature = "tls-native-roots"
    )
))]
fn install_rustls_provider_once_with_selected_backend() {
    let _ =
        rustls::crypto::CryptoProvider::install_default(rustls::crypto::ring::default_provider());
}

#[cfg(not(any(
    feature = "tls-aws-lc",
    feature = "tls-ring",
    feature = "tls-webpki-roots",
    feature = "tls-native-roots"
)))]
fn install_rustls_provider_once_with_selected_backend() {}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_channel_creation_from_db_config() {
    install_rustls_provider_once();
    scenario_channel_creation_from_db_config().await;
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
async fn test_health_check_ignores_authenticated_provider_handler_failure() {
    install_rustls_provider_once();
    scenario_health_check_ignores_authenticated_provider_handler_failure().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_health_check_ignores_emby_authenticated_provider_handler_failure() {
    install_rustls_provider_once();
    scenario_health_check_ignores_emby_authenticated_provider_handler_failure().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_add_alist_instance_does_not_require_external_upstream_auth_for_management_validation()
{
    install_rustls_provider_once();
    scenario_add_alist_instance_does_not_require_external_upstream_auth_for_management_validation()
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
async fn test_get_returns_none_for_absent_remote_instance() {
    install_rustls_provider_once();
    scenario_get_returns_none_for_absent_remote_instance().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_runtime_status_reports_missing_remote_instance_unavailable() {
    install_rustls_provider_once();
    scenario_runtime_status_reports_missing_remote_instance_unavailable().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_runtime_status_reports_misconfigured_remote_instance_unavailable() {
    install_rustls_provider_once();
    scenario_runtime_status_reports_misconfigured_remote_instance_unavailable().await;
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
async fn test_internal_ips_fail_connectivity_validation_when_ssrf_is_explicitly_disabled() {
    install_rustls_provider_once();
    scenario_internal_ips_fail_connectivity_validation_when_ssrf_is_explicitly_disabled().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_ssrf_validation_allows_public_endpoints() {
    install_rustls_provider_once();
    scenario_ssrf_validation_allows_public_endpoints().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_runtime_status_reports_remote_instance_available() {
    install_rustls_provider_once();
    scenario_runtime_status_reports_remote_instance_available().await;
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
async fn test_add_remote_instances_validate_jwt_secret() {
    install_rustls_provider_once();
    scenario_add_remote_instances_validate_jwt_secret().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_add_instance_accepts_all_provider_types() {
    install_rustls_provider_once();
    scenario_add_instance_accepts_all_provider_types().await;
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
async fn test_enable_remote_instances_validate_jwt_secret() {
    install_rustls_provider_once();
    scenario_enable_remote_instances_validate_jwt_secret().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_update_remote_instances_validate_jwt_secret() {
    install_rustls_provider_once();
    scenario_update_remote_instances_validate_jwt_secret().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_init_rejects_invalid_secret_and_aborts_prewarming() {
    install_rustls_provider_once();
    scenario_init_rejects_invalid_secret_and_aborts_prewarming().await;
}
