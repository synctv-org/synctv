// Re-export proto types from synctv-proto
pub use synctv_proto::{admin, client};

// Re-export cluster proto from synctv-cluster (internal)
pub use synctv_cluster::grpc::synctv::cluster;

pub mod admin_service;
pub mod blacklist_layer;
pub mod client_service;
pub mod interceptors;
pub mod notification_service;
pub mod oauth2_service;
pub mod rate_limit_layer;
pub mod timeout_layer;

// Provider gRPC services (local implementations)
// Provider-specific gRPC services are registered from provider instances
pub mod providers;

pub use admin_service::AdminServiceImpl;
pub use client_service::{ClientServiceConfig, ClientServiceImpl};
pub use interceptors::{AuthInterceptor, ClusterAuthInterceptor, LoggingInterceptor};
pub use notification_service::NotificationServiceImpl;

/// Trait to apply gRPC message size limits to tonic service servers.
///
/// This trait provides a unified interface for setting max decoding/encoding
/// message sizes on tonic-generated service servers, protecting against OOM
/// attacks from oversized messages.
pub trait GrpcServiceExt: Sized {
    /// Apply message size limits (both decoding and encoding) to the service.
    /// Returns the service with limits configured.
    fn with_message_size_limit(self, max_size: usize) -> Self {
        self.with_decoding_limit(max_size)
            .with_encoding_limit(max_size)
    }

    /// Apply maximum decoding (incoming) message size limit.
    fn with_decoding_limit(self, limit: usize) -> Self;

    /// Apply maximum encoding (outgoing) message size limit.
    fn with_encoding_limit(self, limit: usize) -> Self;
}

// Implement GrpcServiceExt for all tonic-generated server types that support
// max_decoding_message_size and max_encoding_message_size methods.
// These implementations use the generated methods directly.

macro_rules! impl_grpc_service_ext {
    (<$T:ident> $server_type:ty) => {
        impl<$T> GrpcServiceExt for $server_type {
            fn with_decoding_limit(self, limit: usize) -> Self {
                self.max_decoding_message_size(limit)
            }
            fn with_encoding_limit(self, limit: usize) -> Self {
                self.max_encoding_message_size(limit)
            }
        }
    };
}

// Apply the macro to all gRPC service server types used in this crate
impl_grpc_service_ext!(<T> crate::proto::client::auth_service_server::AuthServiceServer<T>);
impl_grpc_service_ext!(<T> crate::proto::client::user_service_server::UserServiceServer<T>);
impl_grpc_service_ext!(<T> crate::proto::client::room_service_server::RoomServiceServer<T>);
impl_grpc_service_ext!(<T> crate::proto::client::public_service_server::PublicServiceServer<T>);
impl_grpc_service_ext!(<T> crate::proto::client::email_service_server::EmailServiceServer<T>);
impl_grpc_service_ext!(<T> crate::proto::client::notification_service_server::NotificationServiceServer<T>);
impl_grpc_service_ext!(<T> crate::proto::client::o_auth2_service_server::OAuth2ServiceServer<T>);
impl_grpc_service_ext!(<T> crate::proto::admin_service_server::AdminServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::providers::alist::alist_provider_service_server::AlistProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::providers::bilibili::bilibili_provider_service_server::BilibiliProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_livestream::grpc::StreamRelayServiceServer<T>);

/// Map a typed [`ApiError`](crate::impls::ApiError) to a gRPC `Status`.
///
/// Shared across all gRPC service implementations to avoid duplicating the
/// identical match block in every service file.
///
/// For internal errors, the details are logged server-side and a generic
/// message is returned to the client to avoid leaking sensitive information.
pub(crate) fn map_api_error(err: crate::impls::ApiError) -> tonic::Status {
    use crate::impls::ErrorKind;
    let msg = err.message().to_string();
    match err.classify() {
        ErrorKind::NotFound => tonic::Status::not_found(msg),
        ErrorKind::Unauthenticated => tonic::Status::unauthenticated(msg),
        ErrorKind::PermissionDenied => tonic::Status::permission_denied(msg),
        ErrorKind::AlreadyExists => tonic::Status::already_exists(msg),
        ErrorKind::InvalidArgument => tonic::Status::invalid_argument(msg),
        ErrorKind::RateLimited => tonic::Status::resource_exhausted(msg),
        ErrorKind::ServiceUnavailable => tonic::Status::unavailable(msg),
        ErrorKind::Internal => {
            tracing::error!("API internal error: {msg}");
            tonic::Status::internal("Internal error")
        }
    }
}

/// Map a `ProviderError` to an appropriate gRPC status code.
///
/// Uses typed matching on the `ProviderError` enum instead of
/// keyword-based string heuristics.
pub(crate) fn map_provider_error(err: synctv_core::provider::ProviderError) -> tonic::Status {
    use synctv_core::provider::ProviderError;
    let msg = err.to_string();
    match err {
        ProviderError::NetworkError(_) | ProviderError::ApiError(_) => {
            tonic::Status::unavailable(msg)
        }
        ProviderError::UpstreamHttp { status, .. } => {
            if (400..500).contains(&status) {
                tonic::Status::failed_precondition(msg)
            } else {
                tonic::Status::unavailable(msg)
            }
        }
        ProviderError::ParseError(_)
        | ProviderError::InvalidConfig(_)
        | ProviderError::InvalidUrl(_)
        | ProviderError::MissingField(_)
        | ProviderError::InvalidCredentialType
        | ProviderError::UnsupportedFormat(_) => tonic::Status::invalid_argument(msg),
        ProviderError::NotFound
        | ProviderError::InstanceNotFound(_)
        | ProviderError::MissingInstance => tonic::Status::not_found(msg),
        ProviderError::AuthRequired | ProviderError::CredentialRequired => {
            tonic::Status::unauthenticated(msg)
        }
        ProviderError::CredentialNotFound(_) => tonic::Status::not_found(msg),
        ProviderError::CredentialExpired(_) => tonic::Status::unauthenticated(msg),
        ProviderError::RouteRegistrationFailed(_)
        | ProviderError::IoError(_)
        | ProviderError::JsonError(_)
        | ProviderError::EncryptionRequired(_)
        | ProviderError::Internal(_) => {
            tracing::error!("Provider internal error: {msg}");
            tonic::Status::internal("Internal error")
        }
    }
}

/// Extract the effective client IP for gRPC requests.
///
/// Matches HTTP semantics: only trust forwarded headers when the direct peer is
/// a configured trusted proxy. Otherwise fall back to the socket peer address.
#[must_use]
pub(crate) fn extract_client_ip<T>(
    request: &tonic::Request<T>,
    config: &synctv_core::Config,
) -> Option<std::net::IpAddr> {
    let remote_addr = request
        .extensions()
        .get::<tonic::transport::server::TcpConnectInfo>()
        .and_then(tonic::transport::server::TcpConnectInfo::remote_addr)
        .map(|addr| addr.ip());

    if let Some(peer_ip) = remote_addr {
        let mut headers = axum::http::HeaderMap::new();
        for header_name in ["x-forwarded-for", "x-real-ip"] {
            if let Some(value) = request.metadata().get(header_name).and_then(|v| {
                v.to_str()
                    .ok()
                    .and_then(|s| s.parse::<axum::http::HeaderValue>().ok())
            }) {
                headers.insert(header_name, value);
            }
        }
        return Some(crate::client_ip::extract_client_ip_from_headers(
            config, peer_ip, &headers,
        ));
    }

    remote_addr
}

const fn should_register_cluster_grpc_service(
    config: &synctv_core::Config,
    node_registry_available: bool,
) -> bool {
    config.cluster_runtime_enabled()
        && !config.server.cluster_secret.is_empty()
        && node_registry_available
}

const fn should_register_livestream_relay_service(
    config: &synctv_core::Config,
    live_streaming_infrastructure_available: bool,
) -> bool {
    config.cluster_runtime_enabled()
        && !config.server.cluster_secret.is_empty()
        && live_streaming_infrastructure_available
}

const fn should_mark_livestream_relay_serving(
    config: &synctv_core::Config,
    live_streaming_infrastructure_available: bool,
) -> bool {
    should_register_livestream_relay_service(config, live_streaming_infrastructure_available)
}

#[cfg(test)]
const fn should_mark_notification_service_serving(notification_service_available: bool) -> bool {
    notification_service_available
}

const fn should_fail_user_notification_fanout(
    redis_publish_succeeded: bool,
    cluster_redis_enabled: bool,
) -> bool {
    cluster_redis_enabled && !redis_publish_succeeded
}

const fn should_register_email_service(email_available: bool, email_token_available: bool) -> bool {
    email_available && email_token_available
}

#[cfg(test)]
const fn should_mark_email_service_serving(
    email_available: bool,
    email_token_available: bool,
) -> bool {
    should_register_email_service(email_available, email_token_available)
}

#[cfg(test)]
const fn should_mark_oauth2_service_serving(oauth2_service_available: bool) -> bool {
    oauth2_service_available
}

#[cfg(test)]
const fn should_mark_provider_services_serving(providers_available: bool) -> bool {
    providers_available
}

const fn should_mark_cluster_service_serving(
    config: &synctv_core::Config,
    node_registry_available: bool,
) -> bool {
    should_register_cluster_grpc_service(config, node_registry_available)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GrpcHealthRegistrationState {
    email_registered: bool,
    notification_registered: bool,
    oauth2_registered: bool,
    provider_services_registered: bool,
    cluster_service_registered: bool,
    livestream_relay_registered: bool,
}

const fn effective_grpc_request_timeout() -> Option<std::time::Duration> {
    // Tonic's server-wide timeout applies to the entire RPC lifetime, which
    // breaks long-lived streaming calls such as MessageStream. Keep it disabled
    // at the transport level and enforce timeouts in unary business paths.
    None
}

const fn grpc_unary_request_timeout() -> std::time::Duration {
    synctv_core::resilience::timeout::GRPC_CALL_TIMEOUT
}

async fn set_registered_grpc_services_serving(
    health_reporter: &tonic_health::server::HealthReporter,
    state: GrpcHealthRegistrationState,
) {
    use synctv_proto::client::o_auth2_service_server::OAuth2ServiceServer;

    health_reporter
        .set_service_status("", tonic_health::ServingStatus::Serving)
        .await;
    health_reporter
        .set_serving::<AuthServiceServer<ClientServiceImpl>>()
        .await;
    health_reporter
        .set_serving::<UserServiceServer<ClientServiceImpl>>()
        .await;
    health_reporter
        .set_serving::<RoomServiceServer<ClientServiceImpl>>()
        .await;
    health_reporter
        .set_serving::<PublicServiceServer<ClientServiceImpl>>()
        .await;
    if state.email_registered {
        health_reporter
            .set_serving::<EmailServiceServer<ClientServiceImpl>>()
            .await;
    }
    health_reporter
        .set_serving::<AdminServiceServer<AdminServiceImpl>>()
        .await;
    if state.notification_registered {
        health_reporter
            .set_serving::<NotificationServiceServer<NotificationServiceImpl>>()
            .await;
    }
    if state.oauth2_registered {
        health_reporter
            .set_serving::<OAuth2ServiceServer<oauth2_service::OAuth2GrpcService>>()
            .await;
    }
    if state.provider_services_registered {
        use synctv_proto::providers::alist::alist_provider_service_server::AlistProviderServiceServer;
        use synctv_proto::providers::bilibili::bilibili_provider_service_server::BilibiliProviderServiceServer;
        use synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderServiceServer;

        health_reporter
            .set_serving::<AlistProviderServiceServer<providers::alist::AlistProviderGrpcService>>()
            .await;
        health_reporter
            .set_serving::<BilibiliProviderServiceServer<providers::bilibili::BilibiliProviderGrpcService>>()
            .await;
        health_reporter
            .set_serving::<EmbyProviderServiceServer<providers::emby::EmbyProviderGrpcService>>()
            .await;
    }
    if state.cluster_service_registered {
        health_reporter
            .set_serving::<synctv_cluster::grpc::ClusterServiceServer<
                synctv_cluster::grpc::ClusterServer,
            >>()
            .await;
    }
    if state.livestream_relay_registered {
        health_reporter
            .set_serving::<synctv_livestream::grpc::StreamRelayServiceServer<
                synctv_livestream::grpc::StreamRelayServiceImpl,
            >>()
            .await;
    }
}

async fn set_registered_grpc_services_not_serving(
    health_reporter: &tonic_health::server::HealthReporter,
    state: GrpcHealthRegistrationState,
) {
    use synctv_proto::client::o_auth2_service_server::OAuth2ServiceServer;

    health_reporter
        .set_service_status("", tonic_health::ServingStatus::NotServing)
        .await;
    health_reporter
        .set_not_serving::<AuthServiceServer<ClientServiceImpl>>()
        .await;
    health_reporter
        .set_not_serving::<UserServiceServer<ClientServiceImpl>>()
        .await;
    health_reporter
        .set_not_serving::<RoomServiceServer<ClientServiceImpl>>()
        .await;
    health_reporter
        .set_not_serving::<PublicServiceServer<ClientServiceImpl>>()
        .await;
    if state.email_registered {
        health_reporter
            .set_not_serving::<EmailServiceServer<ClientServiceImpl>>()
            .await;
    }
    health_reporter
        .set_not_serving::<AdminServiceServer<AdminServiceImpl>>()
        .await;
    if state.notification_registered {
        health_reporter
            .set_not_serving::<NotificationServiceServer<NotificationServiceImpl>>()
            .await;
    }
    if state.oauth2_registered {
        health_reporter
            .set_not_serving::<OAuth2ServiceServer<oauth2_service::OAuth2GrpcService>>()
            .await;
    }
    if state.provider_services_registered {
        use synctv_proto::providers::alist::alist_provider_service_server::AlistProviderServiceServer;
        use synctv_proto::providers::bilibili::bilibili_provider_service_server::BilibiliProviderServiceServer;
        use synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderServiceServer;

        health_reporter
            .set_not_serving::<AlistProviderServiceServer<
                providers::alist::AlistProviderGrpcService,
            >>()
            .await;
        health_reporter
            .set_not_serving::<BilibiliProviderServiceServer<
                providers::bilibili::BilibiliProviderGrpcService,
            >>()
            .await;
        health_reporter
            .set_not_serving::<EmbyProviderServiceServer<providers::emby::EmbyProviderGrpcService>>(
            )
            .await;
    }
    if state.cluster_service_registered {
        health_reporter
            .set_not_serving::<synctv_cluster::grpc::ClusterServiceServer<
                synctv_cluster::grpc::ClusterServer,
            >>()
            .await;
    }
    if state.livestream_relay_registered {
        health_reporter
            .set_not_serving::<synctv_livestream::grpc::StreamRelayServiceServer<
                synctv_livestream::grpc::StreamRelayServiceImpl,
            >>()
            .await;
    }
}

fn validate_cluster_grpc_runtime_requirements(
    config: &synctv_core::Config,
    node_registry_available: bool,
) -> anyhow::Result<()> {
    if config.cluster_runtime_enabled() && config.server.cluster_secret.is_empty() {
        return Err(anyhow::anyhow!(
            "cluster.enabled=true requires server.cluster_secret before starting the gRPC server; refusing to start with unauthenticated cluster endpoints"
        ));
    }

    if config.cluster_runtime_enabled() && !node_registry_available {
        return Err(anyhow::anyhow!(
            "cluster.enabled=true requires NodeRegistry before starting the gRPC server; refusing to start with cluster gRPC disabled"
        ));
    }

    Ok(())
}

// Use synctv_proto for all server traits and message types (single source of truth)
use crate::proto::admin_service_server::AdminServiceServer;
use crate::proto::client::{
    auth_service_server::AuthServiceServer, email_service_server::EmailServiceServer,
    notification_service_server::NotificationServiceServer,
    public_service_server::PublicServiceServer, room_service_server::RoomServiceServer,
    user_service_server::UserServiceServer,
};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

use std::sync::Arc;
use synctv_cluster::sync::{ClusterManager, ConnectionManager, PublishRequest};
use synctv_core::provider::{AlistProvider, BilibiliProvider, DirectUrlProvider, EmbyProvider};
use synctv_core::service::auth::JwtService;
use synctv_core::service::{
    ContentFilter, EmailService, EmailTokenService, ProvidersManager, RateLimitConfig, RateLimiter,
    RemoteProviderManager, RoomService as CoreRoomService, SettingsRegistry, SettingsService,
    UserService as CoreUserService,
};
use synctv_core::Config;

/// Configuration for the gRPC server
pub struct GrpcServerConfig<'a> {
    pub config: &'a Config,
    pub jwt_service: JwtService,
    pub user_service: Arc<CoreUserService>,
    pub user_cache: Arc<synctv_core::cache::UserCache>,
    pub room_service: Arc<CoreRoomService>,
    pub cluster_manager: Option<Arc<ClusterManager>>,
    pub redis_publish_tx: Option<tokio::sync::mpsc::Sender<PublishRequest>>,
    pub rate_limiter: RateLimiter,
    pub rate_limit_config: RateLimitConfig,
    pub content_filter: ContentFilter,
    pub connection_manager: ConnectionManager,
    pub providers_manager: Option<Arc<ProvidersManager>>,
    pub provider_instance_manager: Arc<RemoteProviderManager>,
    pub user_provider_credential_repository:
        Arc<synctv_core::repository::UserProviderCredentialRepository>,
    pub settings_service: Arc<SettingsService>,
    pub settings_registry: Option<Arc<SettingsRegistry>>,
    pub email_service: Option<Arc<EmailService>>,
    pub email_token_service: Option<Arc<EmailTokenService>>,
    pub live_streaming_infrastructure:
        Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    pub publish_key_service: Option<Arc<synctv_core::service::PublishKeyService>>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub chat_service: Option<Arc<synctv_core::service::ChatService>>,
    pub oauth2_service: Option<Arc<synctv_core::service::OAuth2Service>>,
    pub audit_service: Arc<synctv_core::service::AuditService>,
    pub node_registry: Option<Arc<synctv_cluster::discovery::NodeRegistry>>,
    /// Pre-built Redis client (from the single `init_redis()` call).
    /// Used by the fallback `NodeRegistry` creation to avoid duplicate `redis::Client::open()`.
    /// `None` in standalone mode without Redis.
    pub redis_client: Option<redis::Client>,
    /// Shared Redis connection for playback caching (Sentinel-failover safe)
    pub redis_conn: Option<crate::SharedRedisConn>,
    pub shutdown_rx: Option<tokio::sync::watch::Receiver<bool>>,
    /// Resolved built-in STUN URL (e.g. "stun:203.0.113.1:3478") from a successfully started
    /// STUN server. When `None`, the built-in STUN entry is omitted from ICE server lists.
    pub builtin_stun_url: Option<String>,
    /// TURN health checker for filtering unhealthy TURN servers
    pub turn_health_checker: Option<Arc<synctv_core::service::TurnHealthChecker>>,
    /// Credential encryption for protecting sensitive data in `source_config`
    pub credential_encryption: Option<synctv_core::service::CredentialEncryption>,
    /// Pre-bound TCP listener for the gRPC server.
    /// When provided, the server will use this listener instead of binding internally.
    /// This allows the caller to detect port-in-use errors before spawning the server task.
    pub grpc_listener: Option<tokio::net::TcpListener>,
}

/// Build and start the gRPC server
pub async fn serve(grpc_config: GrpcServerConfig<'_>) -> anyhow::Result<()> {
    let GrpcServerConfig {
        config,
        jwt_service,
        user_service,
        user_cache,
        room_service,
        cluster_manager,
        redis_publish_tx,
        rate_limiter,
        rate_limit_config,
        content_filter,
        connection_manager,
        providers_manager,
        provider_instance_manager,
        user_provider_credential_repository,
        settings_service,
        settings_registry,
        email_service,
        email_token_service,
        live_streaming_infrastructure,
        publish_key_service,
        notification_service,
        chat_service,
        oauth2_service,
        audit_service,
        node_registry,
        redis_client: _,
        redis_conn,
        shutdown_rx,
        builtin_stun_url,
        turn_health_checker,
        credential_encryption,
        // grpc_listener is reserved for future use to support pre-bound listeners
        grpc_listener,
    } = grpc_config;
    let addr = config.grpc_address().parse()?;

    validate_cluster_grpc_runtime_requirements(config, node_registry.is_some())?;

    // Derive HMAC signing key for proxy URLs from JWT secret
    let proxy_signing_key = std::sync::Arc::new(
        synctv_core::service::ProxySigningKey::derive_from(config.jwt.secret.as_bytes()),
    );

    tracing::info!("Starting gRPC server on {}", addr);

    // Clone services for all uses before unwrapping
    let user_service_for_client = user_service.clone();
    let user_service_for_admin = user_service.clone();
    let user_service_for_provider = user_service.clone();

    let room_service_for_client = room_service.clone();
    let room_service_for_provider = room_service.clone();

    let jwt_service_for_provider = jwt_service.clone();

    // Create service instances
    let user_service_clone =
        Arc::try_unwrap(user_service_for_client).unwrap_or_else(|arc| (*arc).clone());
    let room_service_clone =
        Arc::try_unwrap(room_service_for_client).unwrap_or_else(|arc| (*arc).clone());

    // Extract node_id reference before moving cluster_manager
    let cluster_node_id = cluster_manager
        .as_ref()
        .map_or_else(|| "single-node".to_string(), |cm| cm.node_id().to_string());

    // Clone connection_manager for later use
    let connection_manager_for_provider = connection_manager.clone();

    let email_service_for_admin = email_service.clone();
    let email_service_registered =
        should_register_email_service(email_service.is_some(), email_token_service.is_some());
    let providers_manager_for_client = providers_manager.clone();
    let rate_limiter_for_provider = rate_limiter.clone();
    let shared_content_filter_for_provider = Arc::new(content_filter.clone());

    // Build the shared ClientApiImpl for gRPC handlers
    let client_api = Arc::new(
        crate::impls::ClientApiImpl::new(
            user_service.clone(),
            room_service.clone(),
            Arc::new(connection_manager.clone()),
            Arc::new(config.clone()),
            publish_key_service,
            jwt_service.clone(),
            live_streaming_infrastructure.clone(),
            providers_manager_for_client.clone(),
            settings_registry.clone(),
        )
        .with_redis_publish_tx(redis_publish_tx.clone())
        .with_redis_conn(redis_conn.clone())
        .with_rate_limiter(rate_limiter.clone())
        .with_credential_encryption(credential_encryption.clone())
        .with_credential_repo(user_provider_credential_repository.clone())
        .with_signing_key(proxy_signing_key.clone()),
    );

    // Wire in the resolved STUN URL if the built-in STUN server started successfully
    let client_api = if let Some(stun_url) = builtin_stun_url {
        let inner = Arc::try_unwrap(client_api).unwrap_or_else(|arc| (*arc).clone());
        Arc::new(inner.with_builtin_stun_url(stun_url))
    } else {
        client_api
    };

    // Wire in the TURN health checker
    let client_api = if turn_health_checker.is_some() {
        let inner = Arc::try_unwrap(client_api).unwrap_or_else(|arc| (*arc).clone());
        Arc::new(inner.with_turn_health_checker(turn_health_checker.clone()))
    } else {
        client_api
    };

    let rate_limiter_for_layer = rate_limiter.clone();
    let cluster_manager_for_rt = cluster_manager.clone();
    let chat_service = chat_service.ok_or_else(|| {
        anyhow::anyhow!(
            "chat_service is required for gRPC ClientService but was not provided. \
             Ensure chat_service is initialized before starting the gRPC server."
        )
    })?;
    let client_service = ClientServiceImpl::from_config(ClientServiceConfig {
        user_service: user_service_clone,
        room_service: room_service_clone,
        chat_service,
        cluster_manager,
        rate_limiter,
        rate_limit_config: rate_limit_config.clone(),
        content_filter,
        connection_manager,
        email_service,
        email_token_service,
        settings_registry: settings_registry.clone(),
        providers_manager: providers_manager_for_client,
        config: Arc::new(config.clone()),
        client_api: client_api.clone(),
        notification_service: notification_service.clone(),
        heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
    });

    // Build the shared AdminApiImpl for gRPC handlers (same impls layer used by HTTP)
    // AdminApiImpl requires EmailService; if not configured, create with None config
    // so send_test_email fails gracefully.
    let email_svc_for_admin_api = email_service_for_admin.unwrap_or_else(|| {
        Arc::new(EmailService::new(None).expect("EmailService::new(None) should not fail"))
    });
    let live_streaming_infrastructure_for_admin = live_streaming_infrastructure.clone();

    let admin_api = Arc::new(crate::impls::AdminApiImpl::new(
        room_service.clone(),
        user_service_for_admin.clone(),
        settings_service.clone(),
        settings_registry.clone(),
        email_svc_for_admin_api,
        Arc::new(connection_manager_for_provider.clone()),
        provider_instance_manager,
        live_streaming_infrastructure_for_admin,
        redis_publish_tx.clone(),
        audit_service.clone(),
    ));

    let admin_service =
        AdminServiceImpl::new(user_service_for_admin, admin_api, Arc::new(config.clone()));

    // Create auth interceptor for authenticated services (clone jwt_service for blacklist layer)
    let auth_interceptor = AuthInterceptor::new(jwt_service.clone());

    // Create JwtValidator for rate limiting layer (needs to verify JWT to extract user_id)
    let jwt_validator_for_rate_limit =
        synctv_core::service::auth::JwtValidator::new(std::sync::Arc::new(jwt_service.clone()));

    // Create server builder with the security checking tower layer.
    // This layer extracts the raw JWT bearer token from the HTTP Authorization
    // header and performs async security checks via the shared SecurityPipeline:
    // 1. JWT verification (validate signature, expiration, access token type)
    // 2. Password invalidation check (tokens issued before password change)
    // 3. User status check (banned/pending/deleted)
    // It runs before tonic routes and interceptors, so public endpoints (no Authorization header)
    // pass through without security checks.
    let security_pipeline =
        synctv_core::service::auth::SecurityPipelineBuilder::new(user_service.clone())
            .with_user_cache(user_cache.clone())
            .with_token_blacklist(
                user_service.token_blacklist_store(),
                user_service.key_builder().clone(),
            )
            .build()
            .expect("gRPC security pipeline wiring must be complete at startup");
    let blacklist_layer =
        blacklist_layer::BlacklistCheckLayer::new(jwt_service.clone(), security_pipeline);
    // Distributed rate limiting layer: uses Redis when available (shared across
    // replicas), falls back to in-memory governor when Redis is unavailable.
    // Determines tier per-request from the gRPC service path.
    // Uses verified user_id from JWT claims as rate limit key to ensure all tokens
    // belonging to the same user share a single quota.
    let distributed_rate_limit_layer = rate_limit_layer::GrpcRateLimitLayer::new(
        rate_limiter_for_layer,
        Arc::new(config.clone()),
        jwt_validator_for_rate_limit,
    );
    let grpc_request_timeout = effective_grpc_request_timeout();
    let grpc_unary_request_timeout = grpc_unary_request_timeout();
    let unary_timeout_layer =
        timeout_layer::GrpcRequestTimeoutLayer::new(grpc_unary_request_timeout);
    let mut server_builder = Server::builder()
        .layer(unary_timeout_layer)
        .layer(distributed_rate_limit_layer)
        .layer(blacklist_layer);
    if let Some(timeout) = grpc_request_timeout {
        server_builder = server_builder.timeout(timeout);
        tracing::info!(
            grpc_request_timeout_secs = timeout.as_secs(),
            "gRPC request timeout configured"
        );
    } else {
        tracing::info!("gRPC server-wide request timeout disabled");
    }
    tracing::info!(
        grpc_unary_request_timeout_secs = grpc_unary_request_timeout.as_secs(),
        "gRPC unary request timeout configured"
    );

    // Get the configured max message size (prevents OOM from oversized messages)
    let max_message_size = config.server.grpc_max_message_size_bytes;
    tracing::info!(
        max_message_size_bytes = max_message_size,
        max_message_size_mb = max_message_size / (1024 * 1024),
        "gRPC message size limit configured"
    );

    // Clone interceptors for different services
    let user_interceptor = auth_interceptor.clone();
    let admin_interceptor = auth_interceptor.clone();
    let room_interceptor1 = auth_interceptor.clone();

    // Rate limiting is handled by the distributed_rate_limit_layer applied at the
    // server level (above). Per-service interceptors only handle auth concerns.

    // Build router - register all client services with auth interceptors
    // All services have message size limits applied to prevent OOM attacks
    let client_service_clone1 = client_service.clone();
    let client_service_clone2 = client_service.clone();
    let client_service_clone3 = client_service.clone();
    let client_service_clone4 = client_service.clone();

    let notification_service_registered = notification_service.is_some();
    let oauth2_service_registered = oauth2_service.is_some();
    let provider_services_registered = providers_manager.is_some();
    let cluster_service_registered =
        should_mark_cluster_service_serving(config, node_registry.is_some());
    let grpc_health_state = GrpcHealthRegistrationState {
        email_registered: email_service_registered,
        notification_registered: notification_service_registered,
        oauth2_registered: oauth2_service_registered,
        provider_services_registered,
        cluster_service_registered,
        livestream_relay_registered: should_mark_livestream_relay_serving(
            config,
            live_streaming_infrastructure.is_some(),
        ),
    };

    let mut router = server_builder
        // AuthService (public: register, login, refresh_token)
        .add_service(
            AuthServiceServer::new(client_service).with_message_size_limit(max_message_size),
        )
        // UserService - JWT authentication (inject UserContext)
        // Use tonic::codegen::InterceptedService::new to preserve message size limits set on the service
        .add_service(tonic::codegen::InterceptedService::new(
            UserServiceServer::new(client_service_clone1).with_message_size_limit(max_message_size),
            move |req| user_interceptor.inject_user(req),
        ))
        // RoomService - JWT + room_id metadata (inject RoomContext)
        .add_service(tonic::codegen::InterceptedService::new(
            RoomServiceServer::new(client_service_clone2)
                .with_message_size_limit(max_message_size),
            move |req| room_interceptor1.inject_room(req),
        ))
        // PublicService (public room discovery)
        .add_service(
            PublicServiceServer::new(client_service_clone3)
                .with_message_size_limit(max_message_size),
        )
        // EmailService (send codes, confirm with token)
        // AdminService - JWT authentication (inject UserContext)
        .add_service(tonic::codegen::InterceptedService::new(
            AdminServiceServer::new(admin_service).with_message_size_limit(max_message_size),
            move |req| admin_interceptor.inject_user(req),
        ));

    if email_service_registered {
        router = router.add_service(
            EmailServiceServer::new(client_service_clone4)
                .with_message_size_limit(max_message_size),
        );
    }

    // Register NotificationService if notification_service is configured
    if let Some(notif_svc) = notification_service {
        let notification_interceptor = auth_interceptor.clone();
        let notification_api = Arc::new(crate::impls::NotificationApiImpl::new(notif_svc.clone()));
        let notif_impl = NotificationServiceImpl::new(notification_api);
        router = router.add_service(tonic::codegen::InterceptedService::new(
            NotificationServiceServer::new(notif_impl).with_message_size_limit(max_message_size),
            move |req| notification_interceptor.inject_user(req),
        ));
        tracing::info!("NotificationService gRPC registered");

        // RT-1: Spawn a background task that bridges notification creation events
        // to the cluster event system, enabling real-time WebSocket push for
        // persistent user notifications. Without this, clients must poll.
        // The task listens for the server shutdown signal so it does not leak
        // when the gRPC server stops.
        if let Some(ref cm) = cluster_manager_for_rt {
            let cm = Arc::clone(cm);
            let cluster_redis_enabled = cm.metrics().redis_enabled;
            let mut notification_rx = notif_svc.subscribe_events();
            // Clone the optional shutdown watch receiver so the bridge task
            // can stop cleanly when the server receives a shutdown signal.
            // When no shutdown receiver is configured (e.g., test environments),
            // the bridge runs until the notification channel closes.
            let mut bridge_shutdown_rx: Option<tokio::sync::watch::Receiver<bool>> =
                shutdown_rx.clone();
            tokio::spawn(async move {
                loop {
                    // Build a future that resolves when the shutdown signal fires.
                    // When no receiver is available, use a pending future so the
                    // select falls through to the notification arm.
                    let shutdown_future: std::pin::Pin<
                        Box<dyn std::future::Future<Output = ()> + Send>,
                    > = match bridge_shutdown_rx.as_mut() {
                        Some(rx) => Box::pin(async move {
                            let _ = rx.changed().await;
                        }),
                        None => Box::pin(std::future::pending()),
                    };

                    tokio::select! {
                        // Honour the server-wide shutdown signal.
                        () = shutdown_future => {
                            tracing::info!("Notification-to-cluster bridge task stopping (shutdown signal)");
                            break;
                        }
                        result = notification_rx.recv() => {
                            match result {
                                Ok(event) => {
                                    let cluster_event = synctv_cluster::sync::ClusterEvent::UserNotification {
                                        event_id: nanoid::nanoid!(16),
                                        user_id: event.user_id,
                                        notification_id: event.notification.id.to_string(),
                                        title: event.notification.title,
                                        content: event.notification.content,
                                        notification_type: event.notification.notification_type.to_string(),
                                        timestamp: chrono::Utc::now(),
                                    };
                                    let redis_sent = cm.publish_only(cluster_event);
                                    if should_fail_user_notification_fanout(redis_sent, cluster_redis_enabled) {
                                        tracing::error!(
                                            "Notification-to-cluster bridge failed to publish user notification to Redis"
                                        );
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    tracing::warn!(
                                        lagged = n,
                                        "Notification-to-cluster bridge lagged, some notifications may not have been pushed in real time"
                                    );
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                    tracing::info!("Notification event channel closed, stopping bridge task");
                                    break;
                                }
                            }
                        }
                    }
                }
            });
            tracing::info!(
                "Notification-to-cluster bridge task spawned for real-time WebSocket push"
            );
        }
    }

    // Register OAuth2Service if oauth2_service is configured.
    // Uses a single service with NO global auth interceptor. Public endpoints
    // (GetAuthorizationUrl, ExchangeAuthorizationCode, ListAvailableProviders)
    // require no authentication. Private endpoints (GetAuthorizationUrlForBind,
    // UnlinkProvider, GetLinkedProviders) perform inline JWT validation using
    // the auth interceptor passed to the service constructor.
    if let Some(oauth2_svc) = oauth2_service {
        use synctv_proto::client::o_auth2_service_server::OAuth2ServiceServer;
        let oauth2_auth_interceptor = auth_interceptor.clone();
        let oauth2_api = Arc::new(crate::impls::OAuth2ApiImpl::new(
            oauth2_svc,
            user_service.clone(),
        ));
        let oauth2_impl = oauth2_service::OAuth2GrpcService::new(
            oauth2_api,
            Arc::new(config.clone()),
            oauth2_auth_interceptor,
        );
        // No global interceptor: public endpoints are unauthenticated,
        // private endpoints call require_auth() inline.
        router = router.add_service(
            OAuth2ServiceServer::new(oauth2_impl).with_message_size_limit(max_message_size),
        );
        tracing::info!("OAuth2Service gRPC registered (public + authenticated split)");
    }

    // Register provider gRPC services
    if let Some(_providers_mgr) = providers_manager {
        tracing::info!("Registering provider gRPC services");

        // Create provider set for the gRPC services
        let provider_instance_manager_for_provider = _providers_mgr.instance_manager().clone();
        let providers = synctv_core::provider::ProviderSet {
            alist: Arc::new(AlistProvider::new(
                provider_instance_manager_for_provider.clone(),
            )),
            bilibili: Arc::new(BilibiliProvider::new(
                provider_instance_manager_for_provider.clone(),
            )),
            emby: Arc::new(EmbyProvider::new(provider_instance_manager_for_provider)),
            direct_url: Arc::new(DirectUrlProvider::new()),
            rtmp: Arc::new(synctv_core::provider::RtmpProvider::new()),
            live_proxy: Arc::new(synctv_core::provider::LiveProxyProvider::new()),
        };

        let provider_jwt_validator = Arc::new(synctv_core::service::auth::JwtValidator::new(
            Arc::new(jwt_service_for_provider.clone()),
        ));
        let provider_proxy_http_client = synctv_proxy::build_proxy_http_client()
            .expect("provider proxy HTTP client should build");

        // Build a RouterConfig for provider gRPC services, sharing common fields
        let provider_router_config = Arc::new(crate::http::RouterConfig {
            config: Arc::new(config.clone()),
            user_service: user_service_for_provider,
            user_cache: user_cache.clone(),
            room_service: room_service_for_provider,
            content_filter: (*shared_content_filter_for_provider).clone(),
            provider_instance_manager: _providers_mgr.instance_manager().clone(),
            user_provider_credential_repository: user_provider_credential_repository.clone(),
            providers: providers.clone(),
            cluster_manager: None,
            connection_manager: Arc::new(connection_manager_for_provider.clone()),
            jwt_service: jwt_service_for_provider.clone(),
            redis_publish_tx: redis_publish_tx.clone(),
            oauth2_service: None,
            settings_service: Some(settings_service.clone()),
            settings_registry: None,
            email_service: None,
            email_token_service: None,
            publish_key_service: None,
            notification_service: None,
            chat_service: None,
            audit_service: audit_service.clone(),
            live_streaming_infrastructure: None,
            rate_limiter: rate_limiter_for_provider,
            ws_ticket_service: Arc::new(synctv_core::service::WsTicketService::with_memory(None)),
            redis_conn: redis_conn.clone(),
            builtin_stun_url: None,
            turn_health_checker: None,
            credential_encryption: credential_encryption.clone(),
            proxy_slice_cache: Arc::new(synctv_proxy::slice_cache::SliceCache::new_with_client(
                synctv_proxy::slice_cache::SliceCacheConfig::default(),
                provider_proxy_http_client.clone(),
            )),
            proxy_http_client: provider_proxy_http_client.clone(),
            messaging_rate_limit_config: synctv_core::service::RateLimitConfig::default(),
            heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
            providers_manager: Some(Arc::clone(&_providers_mgr)),
        });
        let provider_proxy_slice_cache = provider_router_config.proxy_slice_cache.clone();

        // Reuse the already-constructed client_api and use actual rate limit config
        let app_state = Arc::new(crate::http::AppState {
            router_config: provider_router_config,
            rate_limit_config: Arc::new(config.http_rate_limits.clone()),
            messaging_rate_limit_config: Arc::new(synctv_core::service::RateLimitConfig::default()),
            content_filter: shared_content_filter_for_provider.clone(),
            heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
            jwt_validator: provider_jwt_validator,
            security_pipeline: Arc::new(
                synctv_core::service::SecurityPipeline::new(user_service.clone())
                    .with_user_cache(user_cache.clone())
                    .with_token_blacklist(
                        user_service.token_blacklist_store(),
                        user_service.key_builder().clone(),
                    ),
            ),
            guest_token_validator: Arc::new(
                synctv_core::service::auth::GuestTokenValidator::new(Arc::new(jwt_service.clone()))
                    .with_blacklist(
                        user_service.token_blacklist_store(),
                        user_service.key_builder().clone(),
                    ),
            ),
            client_api: client_api.clone(),
            admin_api: None,
            notification_api: None,
            oauth2_api: None,
            bilibili_api: Arc::new(crate::impls::BilibiliApiImpl::new(
                providers.bilibili.clone(),
                user_provider_credential_repository.clone(),
            )),
            alist_api: Arc::new(crate::impls::AlistApiImpl::new(
                providers.alist.clone(),
                user_provider_credential_repository.clone(),
            )),
            emby_api: Arc::new(crate::impls::EmbyApiImpl::new(
                providers.emby.clone(),
                user_provider_credential_repository.clone(),
            )),
            provider_stores: Arc::new(synctv_core::provider::store::ProviderStoreRegistry::new(
                redis_conn.clone(),
                config.redis.key_prefix.clone(),
            )),
            proxy_provider_registry: Arc::new(providers.build_proxy_registry()),
            proxy_services: std::sync::Arc::new(synctv_core::provider::proxy::ProxyServices {
                room_service: room_service.clone(),
                credential_encryption: credential_encryption.clone(),
                credential_repo: user_provider_credential_repository.clone(),
                signing_key: proxy_signing_key.clone(),
            }),
            proxy_signing_key: proxy_signing_key.clone(),
            proxy_slice_cache: provider_proxy_slice_cache,
            proxy_http_client: provider_proxy_http_client,
        });

        // Register provider gRPC services with auth interceptor
        use synctv_proto::providers::alist::alist_provider_service_server::AlistProviderServiceServer;
        use synctv_proto::providers::bilibili::bilibili_provider_service_server::BilibiliProviderServiceServer;
        use synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderServiceServer;

        let provider_interceptor1 = auth_interceptor.clone();
        let provider_interceptor2 = auth_interceptor.clone();
        let provider_interceptor3 = auth_interceptor.clone();

        // Register provider services with interceptors and message size limits
        // Using InterceptedService::new() to apply message size limits before the interceptor
        router = router.add_service(tonic::codegen::InterceptedService::new(
            AlistProviderServiceServer::new(providers::alist::AlistProviderGrpcService::new(
                app_state.clone(),
            ))
            .with_message_size_limit(max_message_size),
            move |req| provider_interceptor1.inject_user(req),
        ));
        router = router.add_service(tonic::codegen::InterceptedService::new(
            BilibiliProviderServiceServer::new(
                providers::bilibili::BilibiliProviderGrpcService::new(app_state.clone()),
            )
            .with_message_size_limit(max_message_size),
            move |req| provider_interceptor2.inject_user(req),
        ));
        router = router.add_service(tonic::codegen::InterceptedService::new(
            EmbyProviderServiceServer::new(providers::emby::EmbyProviderGrpcService::new(
                app_state,
            ))
            .with_message_size_limit(max_message_size),
            move |req| provider_interceptor3.inject_user(req),
        ));
    }

    // Register cluster gRPC service only in cluster mode.
    if !config.cluster_runtime_enabled() {
        tracing::info!("Cluster mode disabled — cluster gRPC service will not be registered");
    } else if config.server.cluster_secret.is_empty() {
        tracing::error!(
            "cluster_secret is empty — cluster gRPC service will NOT be registered. \
             Cluster coordination will be disabled. Set cluster_secret in config to enable."
        );
    } else if should_register_cluster_grpc_service(config, node_registry.is_some()) {
        let nr = node_registry
            .as_ref()
            .expect("node_registry presence checked by should_register_cluster_grpc_service");
        let cluster_server =
            synctv_cluster::grpc::ClusterServer::new(nr.clone(), cluster_node_id.clone())
                .with_cluster_secret(config.server.cluster_secret.clone())
                .with_connection_manager(std::sync::Arc::new(
                    connection_manager_for_provider.clone(),
                ));
        router = router.add_service(synctv_cluster::grpc::ClusterServiceServer::new(
            cluster_server,
        ));
        tracing::info!(
            "Cluster gRPC service registered with shared-secret auth (using shared NodeRegistry)"
        );
    } else {
        unreachable!(
            "cluster.enabled=true without NodeRegistry must be rejected before gRPC service assembly"
        );
    }

    if should_register_livestream_relay_service(config, live_streaming_infrastructure.is_some()) {
        let live_infra = live_streaming_infrastructure.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "cluster.enabled=true requires livestream infrastructure before registering the relay gRPC service"
            )
        })?;
        let relay_service = synctv_livestream::grpc::StreamRelayServiceImpl::new(
            live_infra.registry.clone(),
            cluster_node_id.clone(),
            live_infra.stream_hub_event_sender.clone(),
            tokio_util::sync::CancellationToken::new(),
        )
        .with_cluster_secret(config.server.cluster_secret.clone());

        let relay_service = if let Some(segment_manager) = live_infra.segment_manager.clone() {
            relay_service.with_segment_manager(segment_manager)
        } else {
            relay_service
        };

        let relay_service =
            if let Some(hls_stream_registry) = live_infra.hls_stream_registry.clone() {
                relay_service.with_hls_stream_registry(hls_stream_registry)
            } else {
                relay_service
            };

        let relay_interceptor = ClusterAuthInterceptor::new(config.server.cluster_secret.clone());
        router = router.add_service(tonic::codegen::InterceptedService::new(
            synctv_livestream::grpc::StreamRelayServiceServer::new(relay_service)
                .with_message_size_limit(max_message_size),
            move |req| relay_interceptor.validate(req),
        ));
        tracing::info!("Livestream relay gRPC service registered with shared-secret auth");
    } else if config.cluster_runtime_enabled() {
        tracing::warn!(
            "Cluster mode enabled but livestream relay gRPC service not registered because livestream infrastructure is unavailable"
        );
    }

    // Register gRPC health check service (standard grpc.health.v1.Health).
    // All registered services are marked as SERVING so gRPC health probes
    // return the correct status rather than UNKNOWN.
    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    set_registered_grpc_services_serving(&health_reporter, grpc_health_state).await;
    router = router.add_service(health_service);
    tracing::info!("gRPC health check service registered");

    // Register gRPC reflection service if enabled in config
    if config.server.enable_reflection {
        let reflection_service = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(synctv_proto::FILE_DESCRIPTOR_SET)
            .register_encoded_file_descriptor_set(synctv_proto::PROVIDERS_FILE_DESCRIPTOR_SET)
            .build_v1()
            .map_err(|e| anyhow::anyhow!("Failed to build gRPC reflection service: {e}"))?;
        router = router.add_service(reflection_service);
        tracing::info!("gRPC reflection service registered");
    }

    // Start server with graceful shutdown support
    // Use pre-bound listener if provided (for proper error propagation), otherwise bind internally
    if let Some(listener) = grpc_listener {
        let incoming = TcpListenerStream::new(listener);
        let shutdown_health_reporter = health_reporter.clone();
        router
            .serve_with_incoming_shutdown(incoming, async move {
                if let Some(mut rx) = shutdown_rx {
                    // Use centralized shutdown signal from the server
                    let _ = rx.changed().await;
                } else {
                    // Fallback: listen for Ctrl+C
                    tokio::signal::ctrl_c().await.ok();
                }
                set_registered_grpc_services_not_serving(
                    &shutdown_health_reporter,
                    grpc_health_state,
                )
                .await;
            })
            .await
            .map_err(|e| anyhow::anyhow!("gRPC server error: {e}"))?;
    } else {
        let shutdown_health_reporter = health_reporter.clone();
        router
            .serve_with_shutdown(addr, async move {
                if let Some(mut rx) = shutdown_rx {
                    // Use centralized shutdown signal from the server
                    let _ = rx.changed().await;
                } else {
                    // Fallback: listen for Ctrl+C
                    tokio::signal::ctrl_c().await.ok();
                }
                set_registered_grpc_services_not_serving(
                    &shutdown_health_reporter,
                    grpc_health_state,
                )
                .await;
            })
            .await
            .map_err(|e| anyhow::anyhow!("gRPC server error: {e}"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        effective_grpc_request_timeout, extract_client_ip, grpc_unary_request_timeout,
        set_registered_grpc_services_not_serving, set_registered_grpc_services_serving,
        should_fail_user_notification_fanout, should_mark_cluster_service_serving,
        should_mark_email_service_serving, should_mark_livestream_relay_serving,
        should_mark_notification_service_serving, should_mark_oauth2_service_serving,
        should_mark_provider_services_serving, should_register_cluster_grpc_service,
        should_register_email_service, should_register_livestream_relay_service,
        validate_cluster_grpc_runtime_requirements, GrpcHealthRegistrationState,
    };
    use std::net::SocketAddr;
    use tonic::metadata::{MetadataKey, MetadataValue};
    use tonic_health::pb::health_check_response::ServingStatus;
    use tonic_health::pb::health_server::Health;
    use tonic_health::pb::HealthCheckRequest;
    use tonic_health::server::HealthService;

    async fn health_status_for_service(
        health_service: &impl Health,
        service_name: &str,
    ) -> Result<ServingStatus, tonic::Code> {
        match health_service
            .check(tonic::Request::new(HealthCheckRequest {
                service: service_name.to_string(),
            }))
            .await
        {
            Ok(response) => {
                Ok(ServingStatus::try_from(response.into_inner().status)
                    .expect("valid health status"))
            }
            Err(status) => Err(status.code()),
        }
    }

    fn request_with_peer_and_headers(
        peer: SocketAddr,
        headers: &[(&str, &str)],
    ) -> tonic::Request<()> {
        let mut request = tonic::Request::new(());
        request
            .extensions_mut()
            .insert(tonic::transport::server::TcpConnectInfo {
                local_addr: None,
                remote_addr: Some(peer),
            });
        for (key, value) in headers {
            request.metadata_mut().insert(
                MetadataKey::from_bytes(key.as_bytes()).expect("valid metadata key"),
                MetadataValue::try_from(*value).expect("valid metadata value"),
            );
        }
        request
    }

    #[test]
    fn test_cluster_grpc_service_requires_cluster_mode() {
        let mut config = synctv_core::Config::default();
        config.server.cluster_secret = "shared-secret".to_string();

        assert!(
            !should_register_cluster_grpc_service(&config, true),
            "cluster_secret alone must not enable cluster gRPC"
        );

        config.cluster.enabled = true;
        assert!(
            should_register_cluster_grpc_service(&config, true),
            "cluster-enabled deployments with a secret and registry should expose cluster gRPC"
        );
    }

    #[test]
    fn test_cluster_grpc_service_requires_node_registry() {
        let mut config = synctv_core::Config::default();
        config.cluster.enabled = true;
        config.server.cluster_secret = "shared-secret".to_string();

        assert!(
            !should_register_cluster_grpc_service(&config, false),
            "cluster gRPC must not be registered before NodeRegistry is ready"
        );
    }

    #[test]
    fn test_cluster_grpc_runtime_requires_node_registry() {
        let mut config = synctv_core::Config::default();
        config.cluster.enabled = true;
        config.server.cluster_secret = "shared-secret".to_string();

        let err = validate_cluster_grpc_runtime_requirements(&config, false)
            .expect_err("cluster runtime must fail closed without NodeRegistry");

        assert!(
            err.to_string().contains("requires NodeRegistry"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_cluster_grpc_runtime_requires_cluster_secret() {
        let mut config = synctv_core::Config::default();
        config.cluster.enabled = true;

        let err = validate_cluster_grpc_runtime_requirements(&config, true)
            .expect_err("cluster runtime must fail closed without cluster_secret");

        assert!(
            err.to_string().contains("server.cluster_secret"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_standalone_grpc_runtime_allows_missing_node_registry() {
        let config = synctv_core::Config::default();

        validate_cluster_grpc_runtime_requirements(&config, false)
            .expect("standalone gRPC runtime should allow missing NodeRegistry");
    }

    #[test]
    fn test_email_service_requires_both_dependencies() {
        assert!(
            !should_register_email_service(false, false),
            "email gRPC must stay hidden when no email infrastructure exists"
        );
        assert!(
            !should_register_email_service(true, false),
            "email gRPC must stay hidden when token service is missing"
        );
        assert!(
            !should_register_email_service(false, true),
            "email gRPC must stay hidden when email sender is missing"
        );
        assert!(
            should_register_email_service(true, true),
            "email gRPC may only be exposed when both dependencies are configured"
        );
    }

    #[test]
    fn test_email_service_serving_state_matches_registration() {
        assert!(!should_mark_email_service_serving(true, false));
        assert!(!should_mark_email_service_serving(false, true));
        assert!(should_mark_email_service_serving(true, true));
    }

    #[test]
    fn test_extract_client_ip_uses_x_forwarded_for_from_trusted_proxy() {
        let mut config = synctv_core::Config::default();
        config.server.trusted_proxies = vec!["127.0.0.1".to_string()];

        let request = request_with_peer_and_headers(
            "127.0.0.1:50051".parse().unwrap(),
            &[("x-forwarded-for", "203.0.113.50, 70.41.3.18")],
        );

        assert_eq!(
            extract_client_ip(&request, &config),
            Some("203.0.113.50".parse().unwrap())
        );
    }

    #[test]
    fn test_extract_client_ip_ignores_headers_from_untrusted_peer() {
        let mut config = synctv_core::Config::default();
        config.server.trusted_proxies = vec!["127.0.0.1".to_string()];

        let request = request_with_peer_and_headers(
            "192.168.1.100:50051".parse().unwrap(),
            &[
                ("x-forwarded-for", "203.0.113.50"),
                ("x-real-ip", "198.51.100.42"),
            ],
        );

        assert_eq!(
            extract_client_ip(&request, &config),
            Some("192.168.1.100".parse().unwrap())
        );
    }

    #[test]
    fn test_extract_client_ip_falls_back_to_x_real_ip_for_trusted_proxy() {
        let mut config = synctv_core::Config::default();
        config.server.trusted_proxies = vec!["10.0.0.0/8".to_string()];

        let request = request_with_peer_and_headers(
            "10.1.2.3:50051".parse().unwrap(),
            &[
                ("x-forwarded-for", "not-an-ip"),
                ("x-real-ip", "198.51.100.42"),
            ],
        );

        assert_eq!(
            extract_client_ip(&request, &config),
            Some("198.51.100.42".parse().unwrap())
        );
    }

    #[test]
    fn test_livestream_relay_service_requires_cluster_mode_secret_and_infra() {
        let mut config = synctv_core::Config::default();

        assert!(
            !should_register_livestream_relay_service(&config, true),
            "standalone mode must not expose livestream relay gRPC service"
        );

        config.cluster.enabled = true;
        assert!(
            !should_register_livestream_relay_service(&config, true),
            "cluster mode without a secret must fail closed"
        );

        config.server.cluster_secret = "shared-secret".to_string();
        assert!(
            !should_register_livestream_relay_service(&config, false),
            "relay service must not be registered before livestream infra is ready"
        );

        assert!(
            should_register_livestream_relay_service(&config, true),
            "cluster mode with secret and livestream infra should register relay service"
        );
        assert!(
            should_mark_livestream_relay_serving(&config, true),
            "health status must only be marked serving when relay service is actually registered"
        );
        assert!(
            !should_mark_livestream_relay_serving(&config, false),
            "health status must not report relay serving when infra is unavailable"
        );
    }

    #[test]
    fn test_optional_grpc_services_only_mark_serving_when_registered() {
        assert!(should_mark_notification_service_serving(true));
        assert!(!should_mark_notification_service_serving(false));
        assert!(should_mark_oauth2_service_serving(true));
        assert!(!should_mark_oauth2_service_serving(false));
        assert!(should_mark_provider_services_serving(true));
        assert!(!should_mark_provider_services_serving(false));
    }

    #[test]
    fn test_user_notification_fanout_requires_redis_when_cluster_enabled() {
        assert!(should_fail_user_notification_fanout(false, true));
        assert!(!should_fail_user_notification_fanout(true, true));
        assert!(!should_fail_user_notification_fanout(false, false));
    }

    #[test]
    fn test_effective_grpc_request_timeout_is_disabled_for_streaming_rpcs() {
        assert_eq!(
            effective_grpc_request_timeout(),
            None,
            "server-wide tonic timeout must stay disabled because it aborts long-lived streaming RPCs"
        );
    }

    #[test]
    fn test_grpc_unary_request_timeout_matches_resilience_budget() {
        assert_eq!(
            grpc_unary_request_timeout(),
            synctv_core::resilience::timeout::GRPC_CALL_TIMEOUT
        );
    }

    #[test]
    fn test_cluster_grpc_service_mark_serving_matches_registration() {
        let mut config = synctv_core::Config::default();
        config.cluster.enabled = true;
        config.server.cluster_secret = "shared-secret".to_string();

        assert!(
            should_mark_cluster_service_serving(&config, true),
            "registered cluster service must be marked serving"
        );
        assert!(
            !should_mark_cluster_service_serving(&config, false),
            "missing node registry must not report cluster service serving"
        );
    }

    #[tokio::test]
    async fn test_grpc_health_registered_services_transition_to_not_serving_on_shutdown() {
        let health_reporter = tonic_health::server::HealthReporter::new();
        let health_service = HealthService::from_health_reporter(health_reporter.clone());
        let state = GrpcHealthRegistrationState {
            email_registered: true,
            notification_registered: true,
            oauth2_registered: false,
            provider_services_registered: false,
            cluster_service_registered: false,
            livestream_relay_registered: false,
        };

        set_registered_grpc_services_serving(&health_reporter, state).await;

        assert_eq!(
            health_status_for_service(&health_service, "").await,
            Ok(ServingStatus::Serving),
        );
        assert_eq!(
            health_status_for_service(
                &health_service,
                <crate::proto::client::auth_service_server::AuthServiceServer<
                    crate::grpc::ClientServiceImpl,
                > as tonic::server::NamedService>::NAME,
            )
            .await,
            Ok(ServingStatus::Serving),
        );
        assert_eq!(
            health_status_for_service(
                &health_service,
                <crate::proto::client::email_service_server::EmailServiceServer<
                    crate::grpc::ClientServiceImpl,
                > as tonic::server::NamedService>::NAME,
            )
            .await,
            Ok(ServingStatus::Serving),
        );
        assert_eq!(
            health_status_for_service(
                &health_service,
                <crate::proto::client::notification_service_server::NotificationServiceServer<
                    crate::grpc::NotificationServiceImpl,
                > as tonic::server::NamedService>::NAME,
            )
            .await,
            Ok(ServingStatus::Serving),
        );

        set_registered_grpc_services_not_serving(&health_reporter, state).await;

        assert_eq!(
            health_status_for_service(&health_service, "").await,
            Ok(ServingStatus::NotServing),
        );
        assert_eq!(
            health_status_for_service(
                &health_service,
                <crate::proto::client::auth_service_server::AuthServiceServer<
                    crate::grpc::ClientServiceImpl,
                > as tonic::server::NamedService>::NAME,
            )
            .await,
            Ok(ServingStatus::NotServing),
        );
        assert_eq!(
            health_status_for_service(
                &health_service,
                <crate::proto::client::email_service_server::EmailServiceServer<
                    crate::grpc::ClientServiceImpl,
                > as tonic::server::NamedService>::NAME,
            )
            .await,
            Ok(ServingStatus::NotServing),
        );
        assert_eq!(
            health_status_for_service(
                &health_service,
                <crate::proto::client::notification_service_server::NotificationServiceServer<
                    crate::grpc::NotificationServiceImpl,
                > as tonic::server::NamedService>::NAME,
            )
            .await,
            Ok(ServingStatus::NotServing),
        );
    }
}
