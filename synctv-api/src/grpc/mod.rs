// Re-export proto types from synctv-proto
pub use synctv_proto::{admin, client};

// Re-export cluster proto from synctv-cluster (internal)
pub use synctv_cluster::grpc::synctv::cluster;
use synctv_proto::client::o_auth2_service_server::OAuth2ServiceServer;
use synctv_proto::providers::alist::alist_provider_service_server::AlistProviderServiceServer;
use synctv_proto::providers::bilibili::bilibili_provider_service_server::BilibiliProviderServiceServer;
use synctv_proto::providers::common::provider_common_service_server::ProviderCommonServiceServer;
use synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderServiceServer;
use synctv_proto::providers::rtmp::rtmp_provider_service_server::RtmpProviderServiceServer;
use synctv_realtime::grpc::RealtimePresenceServiceServer;
use tonic::codec::CompressionEncoding;

pub mod admin_service;
pub mod client_service;
pub mod notification_service;
pub mod oauth2_service;

// Provider gRPC services (local implementations)
// Provider-specific gRPC services are registered from provider instances
pub mod providers;

pub use admin_service::AdminServiceImpl;
pub use client_service::{ClientServiceConfig, ClientServiceImpl};
pub use notification_service::NotificationServiceImpl;
pub use synctv_cluster::grpc::ClusterAuthInterceptor;

/// Trait to apply gRPC message size limits to tonic service servers.
///
/// This trait provides a unified interface for setting max decoding/encoding
/// message sizes on tonic-generated service servers, protecting against OOM
/// attacks from oversized messages.
pub trait GrpcServiceExt: Sized {
    /// Apply message size limits (both decoding and encoding) to the service.
    /// Returns the service with limits configured.
    #[must_use]
    fn with_message_size_limit(self, max_size: usize) -> Self {
        self.with_decoding_limit(max_size)
            .with_encoding_limit(max_size)
    }

    /// Apply message size limits and optional gRPC gzip compression negotiation.
    #[must_use]
    fn with_transport_settings(self, max_size: usize, compression_enabled: bool) -> Self {
        let service = self.with_message_size_limit(max_size);
        if compression_enabled {
            service
                .with_accept_compressed(CompressionEncoding::Gzip)
                .with_send_compressed(CompressionEncoding::Gzip)
        } else {
            service
        }
    }

    /// Apply maximum decoding (incoming) message size limit.
    #[must_use]
    fn with_decoding_limit(self, limit: usize) -> Self;

    /// Apply maximum encoding (outgoing) message size limit.
    #[must_use]
    fn with_encoding_limit(self, limit: usize) -> Self;

    #[must_use]
    fn with_accept_compressed(self, encoding: CompressionEncoding) -> Self;

    #[must_use]
    fn with_send_compressed(self, encoding: CompressionEncoding) -> Self;
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
            fn with_accept_compressed(self, encoding: CompressionEncoding) -> Self {
                self.accept_compressed(encoding)
            }
            fn with_send_compressed(self, encoding: CompressionEncoding) -> Self {
                self.send_compressed(encoding)
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
impl_grpc_service_ext!(<T> synctv_proto::providers::common::provider_common_service_server::ProviderCommonServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::providers::rtmp::rtmp_provider_service_server::RtmpProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_livestream::grpc::StreamRelayServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_cluster::grpc::ClusterServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_realtime::grpc::RealtimePresenceServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proxy::grpc::ProxySliceCacheServiceServer<T>);

/// Map a typed [`ApiError`](crate::impls::ApiError) to a gRPC `Status`.
///
/// Shared across all gRPC service implementations to avoid duplicating the
/// identical match block in every service file.
///
/// For internal errors, the details are logged server-side and a generic
/// message is returned to the client to avoid leaking sensitive information.
#[must_use]
pub fn map_api_error_ref(err: &crate::impls::ApiError) -> tonic::Status {
    use crate::impls::ErrorKind;
    let msg = err.message().to_string();
    match err.classify() {
        ErrorKind::NotFound => tonic::Status::not_found(msg),
        ErrorKind::Unauthenticated => tonic::Status::unauthenticated(msg),
        ErrorKind::PermissionDenied => tonic::Status::permission_denied(msg),
        ErrorKind::AlreadyExists => tonic::Status::already_exists(msg),
        ErrorKind::Conflict => tonic::Status::aborted(msg),
        ErrorKind::InvalidArgument => tonic::Status::invalid_argument(msg),
        ErrorKind::RateLimited => tonic::Status::resource_exhausted(msg),
        ErrorKind::ServiceUnavailable => tonic::Status::unavailable(msg),
        ErrorKind::Timeout => tonic::Status::deadline_exceeded(msg),
        ErrorKind::Internal => {
            tracing::error!("API internal error: {msg}");
            tonic::Status::internal("Internal error")
        }
    }
}

pub fn map_api_error(err: crate::impls::ApiError) -> tonic::Status {
    use crate::impls::ApiError;

    match err {
        ApiError::NotFound(msg) => tonic::Status::not_found(msg),
        ApiError::Authentication(msg) => tonic::Status::unauthenticated(msg),
        ApiError::Authorization(msg) => tonic::Status::permission_denied(msg),
        ApiError::AlreadyExists(msg) => tonic::Status::already_exists(msg),
        ApiError::Conflict(msg) => tonic::Status::aborted(msg),
        ApiError::InvalidInput(msg) => tonic::Status::invalid_argument(msg),
        ApiError::RateLimited(msg) | ApiError::RateLimitedWithRetry { message: msg, .. } => {
            tonic::Status::resource_exhausted(msg)
        }
        ApiError::ServiceUnavailable(msg) => tonic::Status::unavailable(msg),
        ApiError::Timeout(msg) => tonic::Status::deadline_exceeded(msg),
        ApiError::Internal(msg) => {
            tracing::error!("API internal error: {msg}");
            tonic::Status::internal("Internal error")
        }
    }
}

#[must_use]
pub fn map_auth_authorization_error(err: &synctv_core::Error) -> tonic::Status {
    match err {
        synctv_core::Error::Authorization(message) => {
            tonic::Status::permission_denied(message.clone())
        }
        synctv_core::Error::EmailNotVerified => tonic::Status::permission_denied(
            "Email not verified. Please verify your email to continue.",
        ),
        other => {
            tracing::error!(error = %other, "Unexpected authorization-classified auth error");
            tonic::Status::permission_denied("You do not have permission to perform this action")
        }
    }
}

/// Extract the effective client IP for gRPC requests.
///
/// Matches HTTP semantics: only trust forwarded headers when the direct peer is
/// a configured trusted proxy. Otherwise fall back to the socket peer address.
#[must_use]
pub fn extract_client_ip<T>(
    request: &tonic::Request<T>,
    config: &synctv_core::Config,
) -> Option<std::net::IpAddr> {
    let remote_addr = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip())
        .or_else(|| {
            request
                .extensions()
                .get::<tonic::transport::server::TcpConnectInfo>()
                .and_then(tonic::transport::server::TcpConnectInfo::remote_addr)
                .map(|addr| addr.ip())
        });

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

#[must_use]
pub fn request_metadata<T>(
    request: &tonic::Request<T>,
    config: &synctv_core::Config,
    timeout: Option<std::time::Duration>,
) -> crate::impls::RequestMetadata {
    let authorization = request
        .metadata()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let user_agent = request
        .metadata()
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    crate::impls::RequestMetadata::new(crate::impls::TransportProtocol::Grpc)
        .with_authorization(authorization)
        .with_client_ip(extract_client_ip(request, config))
        .with_user_agent(user_agent)
        .with_timeout(timeout)
}

const fn should_register_cluster_grpc_service(
    config: &synctv_core::Config,
    node_registry_available: bool,
) -> bool {
    config.cluster_runtime_enabled() && !config.cluster.secret.is_empty() && node_registry_available
}

const fn should_register_livestream_relay_service(
    config: &synctv_core::Config,
    live_streaming_infrastructure_available: bool,
) -> bool {
    config.cluster_runtime_enabled()
        && !config.cluster.secret.is_empty()
        && live_streaming_infrastructure_available
}

const fn should_register_proxy_slice_cache_service(config: &synctv_core::Config) -> bool {
    config.cluster_runtime_enabled() && !config.cluster.secret.is_empty()
}

const fn should_register_realtime_presence_service(config: &synctv_core::Config) -> bool {
    config.cluster_runtime_enabled() && !config.cluster.secret.is_empty()
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
    auth_registered: bool,
    user_registered: bool,
    room_registered: bool,
    public_registered: bool,
    admin_registered: bool,
    email_registered: bool,
    notification_registered: bool,
    oauth2_registered: bool,
    provider_services_registered: bool,
    cluster_service_registered: bool,
    realtime_presence_registered: bool,
    proxy_slice_cache_registered: bool,
    livestream_relay_registered: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GrpcServiceRegistrationPlan {
    reflection_enabled: bool,
    health_state: GrpcHealthRegistrationState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GrpcOptionalRegistrations {
    email_registered: bool,
    notification_registered: bool,
    oauth2_registered: bool,
    provider_services_registered: bool,
    cluster_service_registered: bool,
    realtime_presence_registered: bool,
    proxy_slice_cache_registered: bool,
    livestream_relay_registered: bool,
}

const fn grpc_service_registration_plan(
    config: &synctv_core::Config,
    optional: GrpcOptionalRegistrations,
) -> GrpcServiceRegistrationPlan {
    GrpcServiceRegistrationPlan {
        reflection_enabled: config.server.enable_reflection,
        health_state: GrpcHealthRegistrationState {
            auth_registered: true,
            user_registered: true,
            room_registered: true,
            public_registered: true,
            admin_registered: true,
            email_registered: optional.email_registered,
            notification_registered: optional.notification_registered,
            oauth2_registered: optional.oauth2_registered,
            provider_services_registered: optional.provider_services_registered,
            cluster_service_registered: optional.cluster_service_registered,
            realtime_presence_registered: optional.realtime_presence_registered,
            proxy_slice_cache_registered: optional.proxy_slice_cache_registered,
            livestream_relay_registered: optional.livestream_relay_registered,
        },
    }
}

const fn grpc_unary_request_timeout() -> std::time::Duration {
    synctv_core::resilience::timeout::GRPC_CALL_TIMEOUT
}

fn request_targets_grpc_transport(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/grpc"))
}

async fn grpc_transport_only_middleware(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if request_targets_grpc_transport(request.headers()) {
        next.run(request).await
    } else {
        axum::response::IntoResponse::into_response(axum::http::StatusCode::NOT_FOUND)
    }
}

async fn set_registered_grpc_services_serving(
    health_reporter: &tonic_health::server::HealthReporter,
    state: GrpcHealthRegistrationState,
) {
    use synctv_proto::client::o_auth2_service_server::OAuth2ServiceServer;

    health_reporter
        .set_service_status("", tonic_health::ServingStatus::Serving)
        .await;
    if state.auth_registered {
        health_reporter
            .set_serving::<AuthServiceServer<ClientServiceImpl>>()
            .await;
    }
    if state.user_registered {
        health_reporter
            .set_serving::<UserServiceServer<ClientServiceImpl>>()
            .await;
    }
    if state.room_registered {
        health_reporter
            .set_serving::<RoomServiceServer<ClientServiceImpl>>()
            .await;
    }
    if state.public_registered {
        health_reporter
            .set_serving::<PublicServiceServer<ClientServiceImpl>>()
            .await;
    }
    if state.email_registered {
        health_reporter
            .set_serving::<EmailServiceServer<ClientServiceImpl>>()
            .await;
    }
    if state.admin_registered {
        health_reporter
            .set_serving::<AdminServiceServer<AdminServiceImpl>>()
            .await;
    }
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
        use synctv_proto::providers::common::provider_common_service_server::ProviderCommonServiceServer;
        use synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderServiceServer;
        use synctv_proto::providers::rtmp::rtmp_provider_service_server::RtmpProviderServiceServer;

        health_reporter
            .set_serving::<ProviderCommonServiceServer<providers::common::ProviderCommonGrpcService>>()
            .await;
        health_reporter
            .set_serving::<AlistProviderServiceServer<providers::alist::AlistProviderGrpcService>>()
            .await;
        health_reporter
            .set_serving::<BilibiliProviderServiceServer<providers::bilibili::BilibiliProviderGrpcService>>()
            .await;
        health_reporter
            .set_serving::<EmbyProviderServiceServer<providers::emby::EmbyProviderGrpcService>>()
            .await;
        health_reporter
            .set_serving::<RtmpProviderServiceServer<providers::rtmp::RtmpProviderGrpcService>>()
            .await;
    }
    if state.cluster_service_registered {
        health_reporter
            .set_serving::<synctv_cluster::grpc::ClusterServiceServer<
                synctv_cluster::grpc::ClusterServer,
            >>()
            .await;
    }
    if state.realtime_presence_registered {
        health_reporter
            .set_serving::<synctv_realtime::grpc::RealtimePresenceServiceServer<
                synctv_realtime::grpc::RealtimePresenceServiceImpl,
            >>()
            .await;
    }
    if state.proxy_slice_cache_registered {
        health_reporter
            .set_serving::<synctv_proxy::grpc::ProxySliceCacheServiceServer<
                synctv_proxy::grpc::ProxySliceCacheServiceImpl,
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

#[cfg(test)]
async fn set_registered_grpc_services_not_serving(
    health_reporter: &tonic_health::server::HealthReporter,
    state: GrpcHealthRegistrationState,
) {
    use synctv_proto::client::o_auth2_service_server::OAuth2ServiceServer;

    health_reporter
        .set_service_status("", tonic_health::ServingStatus::NotServing)
        .await;
    if state.auth_registered {
        health_reporter
            .set_not_serving::<AuthServiceServer<ClientServiceImpl>>()
            .await;
    }
    if state.user_registered {
        health_reporter
            .set_not_serving::<UserServiceServer<ClientServiceImpl>>()
            .await;
    }
    if state.room_registered {
        health_reporter
            .set_not_serving::<RoomServiceServer<ClientServiceImpl>>()
            .await;
    }
    if state.public_registered {
        health_reporter
            .set_not_serving::<PublicServiceServer<ClientServiceImpl>>()
            .await;
    }
    if state.email_registered {
        health_reporter
            .set_not_serving::<EmailServiceServer<ClientServiceImpl>>()
            .await;
    }
    if state.admin_registered {
        health_reporter
            .set_not_serving::<AdminServiceServer<AdminServiceImpl>>()
            .await;
    }
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
        use synctv_proto::providers::common::provider_common_service_server::ProviderCommonServiceServer;
        use synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderServiceServer;
        use synctv_proto::providers::rtmp::rtmp_provider_service_server::RtmpProviderServiceServer;

        health_reporter
            .set_not_serving::<ProviderCommonServiceServer<providers::common::ProviderCommonGrpcService>>()
            .await;
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
        health_reporter
            .set_not_serving::<RtmpProviderServiceServer<providers::rtmp::RtmpProviderGrpcService>>(
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
    if state.realtime_presence_registered {
        health_reporter
            .set_not_serving::<synctv_realtime::grpc::RealtimePresenceServiceServer<
                synctv_realtime::grpc::RealtimePresenceServiceImpl,
            >>()
            .await;
    }
    if state.proxy_slice_cache_registered {
        health_reporter
            .set_not_serving::<synctv_proxy::grpc::ProxySliceCacheServiceServer<
                synctv_proxy::grpc::ProxySliceCacheServiceImpl,
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
    if config.cluster_runtime_enabled() && config.cluster.secret.is_empty() {
        return Err(anyhow::anyhow!(
            "cluster.enabled=true requires cluster.secret before starting the gRPC server; refusing to start with unauthenticated cluster endpoints"
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
use crate::realtime_fanout::RealtimeFanoutService;
use crate::runtime::{
    RealtimeConnectionService, RealtimeDeliveryRequirement, RealtimeEventService,
};
use std::sync::Arc;
use synctv_core::provider::{AlistProvider, BilibiliProvider, DirectUrlProvider, EmbyProvider};
use synctv_core::service::auth::JwtService;
use synctv_core::service::{
    ContentFilter, EmailService, EmailTokenService, ProvidersManager, RateLimitConfig,
    RemoteProviderManager, RequestRateLimiterService, RoomService as CoreRoomService,
    SettingsRegistry, SettingsService, UserService as CoreUserService,
};
use synctv_core::Config;

/// Configuration for the gRPC server
pub struct GrpcServerConfig<'a> {
    pub config: &'a Config,
    pub jwt_service: JwtService,
    pub user_service: Arc<CoreUserService>,
    pub user_cache: Arc<synctv_core::cache::UserCache>,
    pub room_service: Arc<CoreRoomService>,
    pub event_service: Option<Arc<dyn RealtimeEventService>>,
    pub realtime_fanout_service: Arc<dyn RealtimeFanoutService>,
    pub rate_limiter: Arc<dyn RequestRateLimiterService>,
    pub rate_limit_config: RateLimitConfig,
    pub content_filter: ContentFilter,
    pub connection_service: Arc<dyn RealtimeConnectionService>,
    pub providers_manager: Option<Arc<ProvidersManager>>,
    pub provider_instance_manager: Arc<RemoteProviderManager>,
    pub user_provider_credential_repository:
        Arc<synctv_core::repository::UserProviderCredentialRepository>,
    pub settings_service: Arc<SettingsService>,
    pub settings_registry: Option<Arc<SettingsRegistry>>,
    pub email_service: Option<Arc<EmailService>>,
    pub email_token_service: Option<Arc<EmailTokenService>>,
    pub ws_ticket_service: Arc<dyn synctv_core::service::WebSocketTicketService>,
    pub live_streaming_infrastructure:
        Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    pub publish_key_service: Option<Arc<dyn synctv_core::service::StreamingPublishKeyService>>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub chat_service: Option<Arc<synctv_core::service::ChatService>>,
    pub oauth2_service: Option<Arc<synctv_core::service::OAuth2Service>>,
    pub passkey_service: Option<Arc<synctv_core::service::PasskeyService>>,
    pub audit_service: Arc<synctv_core::service::AuditService>,
    pub node_registry: Option<Arc<dyn synctv_cluster::discovery::ClusterNodeDirectory>>,
    /// Shared runtime for playback caching and other shared-state lookups.
    pub redis_runtime: Option<Arc<dyn synctv_core::RedisConnectionRuntime>>,
    /// Shared HTTP app state from the unified API server.
    ///
    /// When present, gRPC reuses the HTTP proxy/signing infrastructure instead
    /// of constructing a transport-local copy.
    pub shared_http_app_state: Option<Arc<crate::http::AppState>>,
    pub shutdown_rx: Option<tokio::sync::watch::Receiver<bool>>,
    /// Resolved built-in STUN URL (e.g. "stun:203.0.113.1:3478") from a successfully started
    /// STUN server. When `None`, the built-in STUN entry is omitted from ICE server lists.
    pub builtin_stun_url: Option<String>,
    /// Credential encryption for provider credential resolution
    pub credential_encryption: Option<synctv_core::service::CredentialEncryption>,
    /// Pre-bound TCP listener for the gRPC server.
    /// When provided, the server will use this listener instead of binding internally.
    /// This allows the caller to detect port-in-use errors before spawning the server task.
    pub grpc_listener: Option<tokio::net::TcpListener>,
}

fn resolve_provider_proxy_runtime(
    config: &Config,
    redis_runtime: Option<Arc<dyn synctv_core::RedisConnectionRuntime>>,
    shared_api_runtime: Option<&Arc<crate::http::SharedApiRuntime>>,
) -> (
    Arc<synctv_core::service::ProxySigningKey>,
    Arc<dyn synctv_core::provider::store::ProviderStoreResolver>,
) {
    if let Some(shared_api_runtime) = shared_api_runtime {
        return (
            shared_api_runtime.proxy_signing_key.clone(),
            shared_api_runtime.provider_stores.clone(),
        );
    }

    (
        Arc::new(synctv_core::service::ProxySigningKey::derive_from(
            config.jwt.secret.as_bytes(),
        )),
        synctv_core::provider::store::build_provider_store_resolver_from_profile(
            &synctv_core::SharedStateProfile::from_runtime(
                redis_runtime,
                config.redis.key_prefix.clone(),
                false,
            ),
        ),
    )
}

struct FallbackHttpAppStateDeps {
    user_service: Arc<CoreUserService>,
    user_cache: Arc<synctv_core::cache::UserCache>,
    room_service: Arc<CoreRoomService>,
    event_service: Option<Arc<dyn RealtimeEventService>>,
    connection_service: Arc<dyn RealtimeConnectionService>,
    config: Arc<Config>,
    content_filter: ContentFilter,
    publish_key_service: Option<Arc<dyn synctv_core::service::StreamingPublishKeyService>>,
    jwt_service: JwtService,
    live_streaming_infrastructure: Option<Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    providers_manager: Option<Arc<ProvidersManager>>,
    provider_instance_manager: Arc<RemoteProviderManager>,
    notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    chat_service: Option<Arc<synctv_core::service::ChatService>>,
    oauth2_service: Option<Arc<synctv_core::service::OAuth2Service>>,
    passkey_service: Option<Arc<synctv_core::service::PasskeyService>>,
    settings_service: Arc<SettingsService>,
    settings_registry: Option<Arc<SettingsRegistry>>,
    email_service: Option<Arc<EmailService>>,
    email_token_service: Option<Arc<EmailTokenService>>,
    ws_ticket_service: Arc<dyn synctv_core::service::WebSocketTicketService>,
    realtime_fanout_service: Arc<dyn RealtimeFanoutService>,
    redis_runtime: Option<Arc<dyn synctv_core::RedisConnectionRuntime>>,
    rate_limiter: Arc<dyn RequestRateLimiterService>,
    messaging_rate_limit_config: RateLimitConfig,
    credential_encryption: Option<synctv_core::service::CredentialEncryption>,
    credential_repo: Arc<synctv_core::repository::UserProviderCredentialRepository>,
    proxy_signing_key: Arc<synctv_core::service::ProxySigningKey>,
    provider_stores: Arc<dyn synctv_core::provider::store::ProviderStoreResolver>,
    builtin_stun_url: Option<String>,
    audit_service: Arc<synctv_core::service::AuditService>,
}

fn cluster_node_id(event_service: Option<&Arc<dyn RealtimeEventService>>) -> String {
    event_service.map_or_else(
        || "single-node".to_string(),
        |service| service.node_id().to_string(),
    )
}

fn build_fallback_http_app_state(deps: FallbackHttpAppStateDeps) -> Arc<crate::http::AppState> {
    let providers = synctv_core::provider::ProviderSet {
        alist: Arc::new(AlistProvider::new(deps.provider_instance_manager.clone())),
        bilibili: Arc::new(BilibiliProvider::new(
            deps.provider_instance_manager.clone(),
        )),
        emby: Arc::new(EmbyProvider::new(deps.provider_instance_manager.clone())),
        direct_url: Arc::new(DirectUrlProvider::new()),
        rtmp: Arc::new(synctv_core::provider::RtmpProvider::new()),
        live_proxy: Arc::new(synctv_core::provider::LiveProxyProvider::new()),
    };
    let ssrf_guard = deps.config.security.ssrf_guard();
    let proxy_http_client = synctv_proxy::build_proxy_http_client(ssrf_guard.clone())
        .expect("gRPC proxy HTTP client should build");
    let proxy_slice_cache_config =
        crate::config_adapters::proxy_slice_cache_config_from_app_config(deps.config.as_ref());

    Arc::new(crate::http::create_app_state_from_config(
        crate::http::RouterConfig {
            config: deps.config,
            user_service: deps.user_service,
            user_cache: deps.user_cache,
            room_service: deps.room_service,
            content_filter: deps.content_filter,
            provider_instance_manager: deps.provider_instance_manager,
            user_provider_credential_repository: deps.credential_repo,
            providers,
            event_service: deps.event_service,
            connection_manager: deps.connection_service,
            jwt_service: deps.jwt_service,
            realtime_fanout_service: deps.realtime_fanout_service,
            oauth2_service: deps.oauth2_service,
            passkey_service: deps.passkey_service,
            settings_service: Some(deps.settings_service),
            settings_registry: deps.settings_registry,
            email_service: deps.email_service,
            email_token_service: deps.email_token_service,
            publish_key_service: deps.publish_key_service,
            notification_service: deps.notification_service,
            chat_service: deps.chat_service,
            audit_service: deps.audit_service,
            live_streaming_infrastructure: deps.live_streaming_infrastructure,
            rate_limiter: deps.rate_limiter,
            ws_ticket_service: deps.ws_ticket_service,
            redis_runtime: deps.redis_runtime,
            shared_provider_stores: Some(deps.provider_stores),
            shared_proxy_signing_key: Some(deps.proxy_signing_key),
            builtin_stun_url: deps.builtin_stun_url,
            credential_encryption: deps.credential_encryption,
            proxy_slice_cache: Arc::new(
                synctv_proxy::slice_cache::SliceCache::new_with_client_and_ssrf_guard(
                    proxy_slice_cache_config,
                    proxy_http_client.clone(),
                    ssrf_guard.clone(),
                ),
            ),
            ssrf_guard,
            proxy_http_client,
            messaging_rate_limit_config: deps.messaging_rate_limit_config,
            heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
            providers_manager: deps.providers_manager,
        },
    ))
}

pub async fn build_axum_router(grpc_config: GrpcServerConfig<'_>) -> anyhow::Result<axum::Router> {
    let GrpcServerConfig {
        config,
        jwt_service,
        user_service,
        user_cache,
        room_service,
        event_service,
        realtime_fanout_service,
        rate_limiter,
        rate_limit_config,
        content_filter,
        connection_service,
        providers_manager,
        provider_instance_manager,
        user_provider_credential_repository,
        settings_service,
        settings_registry,
        email_service,
        email_token_service,
        ws_ticket_service,
        live_streaming_infrastructure,
        publish_key_service,
        notification_service,
        chat_service,
        oauth2_service,
        passkey_service,
        audit_service,
        node_registry,
        redis_runtime,
        shared_http_app_state,
        shutdown_rx,
        builtin_stun_url,
        credential_encryption,
        grpc_listener: _,
    } = grpc_config;
    validate_cluster_grpc_runtime_requirements(config, node_registry.is_some())?;

    let (proxy_signing_key, provider_stores) = resolve_provider_proxy_runtime(
        config,
        redis_runtime.clone(),
        shared_http_app_state
            .as_ref()
            .map(|state| &state.shared_api_runtime),
    );
    let shared_http_app_state = shared_http_app_state.or_else(|| {
        Some(build_fallback_http_app_state(FallbackHttpAppStateDeps {
            user_service: user_service.clone(),
            user_cache: user_cache.clone(),
            room_service: room_service.clone(),
            event_service: event_service.clone(),
            connection_service: connection_service.clone(),
            config: Arc::new(config.clone()),
            content_filter: content_filter.clone(),
            publish_key_service: publish_key_service.clone(),
            jwt_service: jwt_service.clone(),
            live_streaming_infrastructure: live_streaming_infrastructure.clone(),
            providers_manager: providers_manager.clone(),
            provider_instance_manager: provider_instance_manager.clone(),
            notification_service: notification_service.clone(),
            chat_service: chat_service.clone(),
            oauth2_service: oauth2_service.clone(),
            passkey_service: passkey_service.clone(),
            settings_service: settings_service.clone(),
            settings_registry: settings_registry.clone(),
            email_service: email_service.clone(),
            email_token_service: email_token_service.clone(),
            ws_ticket_service: ws_ticket_service.clone(),
            realtime_fanout_service: realtime_fanout_service.clone(),
            redis_runtime: redis_runtime.clone(),
            rate_limiter: rate_limiter.clone(),
            messaging_rate_limit_config: rate_limit_config.clone(),
            credential_encryption: credential_encryption.clone(),
            credential_repo: user_provider_credential_repository.clone(),
            proxy_signing_key: proxy_signing_key.clone(),
            provider_stores: provider_stores.clone(),
            builtin_stun_url,
            audit_service: audit_service.clone(),
        }))
    });

    tracing::info!("Building gRPC router for {}", config.api_address());

    // Clone services for all uses before unwrapping
    let user_service_for_client = user_service.clone();

    let room_service_for_client = room_service.clone();

    // Create service instances
    let user_service_clone =
        Arc::try_unwrap(user_service_for_client).unwrap_or_else(|arc| (*arc).clone());
    let room_service_clone =
        Arc::try_unwrap(room_service_for_client).unwrap_or_else(|arc| (*arc).clone());

    // Resolve node identity from the injected realtime event service.
    let cluster_node_id = cluster_node_id(event_service.as_ref());

    let email_service_registered =
        should_register_email_service(email_service.is_some(), email_token_service.is_some());
    let providers_manager_for_client = providers_manager.clone();
    let shared_http_app_state =
        shared_http_app_state.expect("shared or fallback HTTP state must be available for gRPC");
    let shared_api_runtime = shared_http_app_state.shared_api_runtime.clone();
    let client_api = shared_api_runtime.client_api.clone();
    let email_api = shared_api_runtime.email_api.clone().or_else(|| {
        crate::impls::email::build_shared_email_api(
            user_service.clone(),
            email_service.clone(),
            email_token_service.clone(),
            rate_limiter.clone(),
            shared_api_runtime.public_id_codec.clone(),
        )
    });

    let event_service_for_rt = event_service.clone();
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
        event_service,
        rate_limiter,
        rate_limit_config: rate_limit_config.clone(),
        content_filter,
        connection_service: connection_service.clone(),
        email_api,
        settings_registry: settings_registry.clone(),
        providers_manager: providers_manager_for_client,
        config: Arc::new(config.clone()),
        client_api: client_api.clone(),
        notification_service: notification_service.clone(),
        heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
    });

    let admin_api = shared_api_runtime
        .admin_api
        .clone()
        .expect("shared or fallback API runtime must provide admin API wiring");

    let admin_service = AdminServiceImpl::new(admin_api.clone(), Arc::new(config.clone()));

    let grpc_unary_request_timeout = grpc_unary_request_timeout();
    tracing::info!(
        grpc_unary_request_timeout_secs = grpc_unary_request_timeout.as_secs(),
        "gRPC impl-level unary request timeout configured"
    );

    // Get the configured max message size (prevents OOM from oversized messages)
    let max_message_size = config.server.grpc_max_message_size_bytes;
    tracing::info!(
        max_message_size_bytes = max_message_size,
        max_message_size_mb = max_message_size / (1024 * 1024),
        "gRPC message size limit configured"
    );

    // Build router - all services have message size limits applied to prevent OOM attacks
    let client_service_clone1 = client_service.clone();
    let client_service_clone2 = client_service.clone();
    let client_service_clone3 = client_service.clone();
    let client_service_clone4 = client_service.clone();

    let notification_service_registered = notification_service.is_some();
    let oauth2_service_registered = oauth2_service.is_some();
    let provider_services_registered = providers_manager.is_some();
    let cluster_service_registered =
        should_mark_cluster_service_serving(config, node_registry.is_some());
    let grpc_registration_plan = grpc_service_registration_plan(
        config,
        GrpcOptionalRegistrations {
            email_registered: email_service_registered,
            notification_registered: notification_service_registered,
            oauth2_registered: oauth2_service_registered,
            provider_services_registered,
            cluster_service_registered,
            realtime_presence_registered: should_register_realtime_presence_service(config),
            proxy_slice_cache_registered: should_register_proxy_slice_cache_service(config),
            livestream_relay_registered: should_mark_livestream_relay_serving(
                config,
                live_streaming_infrastructure.is_some(),
            ),
        },
    );

    let mut routes = tonic::service::Routes::builder();
    if grpc_registration_plan.health_state.auth_registered {
        routes.add_service(
            AuthServiceServer::new(client_service)
                .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
    }

    if grpc_registration_plan.health_state.user_registered {
        routes.add_service(
            UserServiceServer::new(client_service_clone1)
                .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
    }

    if grpc_registration_plan.health_state.room_registered {
        routes.add_service(
            RoomServiceServer::new(client_service_clone2)
                .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
    }

    if grpc_registration_plan.health_state.public_registered {
        routes.add_service(
            PublicServiceServer::new(client_service_clone3)
                .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
    }

    if grpc_registration_plan.health_state.admin_registered {
        routes.add_service(
            AdminServiceServer::new(admin_service)
                .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
    }

    if grpc_registration_plan.health_state.email_registered {
        routes.add_service(
            EmailServiceServer::new(client_service_clone4)
                .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
    }

    // Register NotificationService if notification_service is configured
    if grpc_registration_plan.health_state.notification_registered {
        let notif_svc = notification_service
            .expect("notification service presence must match registration plan");
        let notification_api = shared_api_runtime
            .notification_api
            .clone()
            .unwrap_or_else(|| {
                Arc::new(crate::impls::NotificationApiImpl::new(
                    notif_svc.clone(),
                    shared_api_runtime.public_id_codec.clone(),
                ))
            });
        let notif_impl = NotificationServiceImpl::new(
            notification_api,
            shared_api_runtime.request_executor.clone(),
            Arc::new(config.clone()),
        );
        routes.add_service(
            NotificationServiceServer::new(notif_impl)
                .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
        tracing::info!("NotificationService gRPC registered");

        // Spawn a background task that bridges notification creation events to
        // the realtime event system, enabling real-time WebSocket push for
        // persistent user notifications. Without this, clients must poll.
        // The task listens for the server shutdown signal so it does not leak
        // when the gRPC server stops.
        if let Some(ref event_service) = event_service_for_rt {
            let event_service = Arc::clone(event_service);
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
                            tracing::info!("Notification-to-realtime bridge task stopping (shutdown signal)");
                            break;
                        }
                        result = notification_rx.recv() => {
                            match result {
                                Ok(event) => {
                                    let realtime_event = synctv_realtime::sync::RealtimeEvent::UserNotification {
                                        event_id: synctv_common::snanoid!(16),
                                        user_id: event.user_id,
                                        notification_id: event.notification.id.to_string(),
                                        title: event.notification.title,
                                        content: event.notification.content,
                                        notification_type: event.notification.notification_type.to_string(),
                                        timestamp: chrono::Utc::now(),
                                    };
                                    let outcome = event_service.publish_only_outcome(realtime_event);
                                    if !outcome.satisfies(
                                        RealtimeDeliveryRequirement::DistributedIfAvailable,
                                    ) {
                                        tracing::error!(
                                            "Notification-to-realtime bridge failed to reach the distributed fan-out path"
                                        );
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    tracing::warn!(
                                        lagged = n,
                                        "Notification-to-realtime bridge lagged, some notifications may not have been pushed in real time"
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
                "Notification-to-realtime bridge task spawned for real-time WebSocket push"
            );
        }
    }

    // Register OAuth2Service if oauth2_service is configured.
    // Uses a single service with no transport-level auth middleware. Public
    // endpoints (GetAuthorizationUrl, ExchangeAuthorizationCode,
    // ListAvailableProviders) require no authentication. Private endpoints
    // (GetAuthorizationUrlForBind, UnlinkProvider, GetLinkedProviders) execute
    // the shared impl-level auth pipeline inline through RequestExecutor.
    if grpc_registration_plan.health_state.oauth2_registered {
        let oauth2_svc =
            oauth2_service.expect("oauth2 service presence must match registration plan");
        let oauth2_api = shared_api_runtime.oauth2_api.clone().unwrap_or_else(|| {
            Arc::new(crate::impls::OAuth2ApiImpl::new(
                oauth2_svc,
                user_service.clone(),
                shared_api_runtime.public_id_codec.clone(),
            ))
        });
        let oauth2_impl = oauth2_service::OAuth2GrpcService::new(
            oauth2_api,
            Arc::new(config.clone()),
            shared_api_runtime.request_executor.clone(),
        );
        // Public endpoints are unauthenticated; private endpoints invoke the
        // shared impl-level auth pipeline inline.
        routes.add_service(
            OAuth2ServiceServer::new(oauth2_impl)
                .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
        tracing::info!("OAuth2Service gRPC registered (public + authenticated split)");
    }

    // Register provider gRPC services
    if grpc_registration_plan
        .health_state
        .provider_services_registered
    {
        let _ = providers_manager.expect("provider services presence must match registration plan");
        tracing::info!("Registering provider gRPC services");

        let shared_api_runtime = shared_api_runtime.clone();

        // Register provider gRPC services. Auth, blacklist, rate limiting, and
        // timeouts are enforced explicitly inside the shared impl layer.
        routes.add_service(
            ProviderCommonServiceServer::new(providers::common::ProviderCommonGrpcService::new(
                &shared_api_runtime,
                Arc::new(config.clone()),
            ))
            .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
        routes.add_service(
            AlistProviderServiceServer::new(providers::alist::AlistProviderGrpcService::new(
                &shared_api_runtime,
                shared_api_runtime.request_executor.clone(),
                Arc::new(config.clone()),
            ))
            .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
        routes.add_service(
            BilibiliProviderServiceServer::new(
                providers::bilibili::BilibiliProviderGrpcService::new(
                    &shared_api_runtime,
                    shared_api_runtime.request_executor.clone(),
                    Arc::new(config.clone()),
                ),
            )
            .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
        routes.add_service(
            EmbyProviderServiceServer::new(providers::emby::EmbyProviderGrpcService::new(
                &shared_api_runtime,
                shared_api_runtime.request_executor.clone(),
                Arc::new(config.clone()),
            ))
            .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
        routes.add_service(
            RtmpProviderServiceServer::new(providers::rtmp::RtmpProviderGrpcService::new(
                &shared_api_runtime,
                shared_api_runtime.request_executor.clone(),
                Arc::new(config.clone()),
            ))
            .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
    }

    // Register cluster gRPC service only in distributed mode.
    if !grpc_registration_plan
        .health_state
        .cluster_service_registered
    {
        if config.cluster_runtime_enabled() {
            tracing::info!("Cluster gRPC service hidden by gRPC exposure profile");
        } else {
            tracing::info!("Cluster mode disabled — cluster gRPC service will not be registered");
        }
    } else if !config.cluster_runtime_enabled() {
        tracing::info!("Cluster mode disabled — cluster gRPC service will not be registered");
    } else if config.cluster.secret.is_empty() {
        tracing::error!(
            "cluster.secret is empty — cluster gRPC service will NOT be registered. \
             Cluster coordination will be disabled. Set cluster.secret or SYNCTV_CLUSTER_SECRET to enable."
        );
    } else if should_register_cluster_grpc_service(config, node_registry.is_some()) {
        let nr = node_registry
            .as_ref()
            .expect("node_registry presence checked by should_register_cluster_grpc_service");
        let cluster_server = synctv_cluster::grpc::ClusterServer::from_runtime(nr.clone())
            .with_cluster_secret(config.cluster.secret.clone());
        routes.add_service(
            synctv_cluster::grpc::ClusterServiceServer::new(cluster_server)
                .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
        tracing::info!("Cluster node-discovery gRPC service registered with shared-secret auth");
    } else {
        unreachable!(
            "cluster.enabled=true without NodeRegistry must be rejected before gRPC service assembly"
        );
    }

    if grpc_registration_plan
        .health_state
        .realtime_presence_registered
    {
        let service = synctv_realtime::grpc::RealtimePresenceServiceImpl::new(
            connection_service.clone(),
            cluster_node_id.clone(),
        )
        .with_cluster_secret(config.cluster.secret.clone());
        routes.add_service(
            RealtimePresenceServiceServer::new(service)
                .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
        tracing::info!("Realtime presence gRPC service registered with shared-secret auth");
    }

    if grpc_registration_plan
        .health_state
        .proxy_slice_cache_registered
    {
        let service = synctv_proxy::grpc::ProxySliceCacheServiceImpl::new(
            shared_http_app_state.proxy_slice_cache.clone(),
            cluster_node_id.clone(),
        )
        .with_cluster_secret(config.cluster.secret.clone());
        routes.add_service(
            synctv_proxy::grpc::ProxySliceCacheServiceServer::new(service)
                .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
        );
        tracing::info!("Proxy slice-cache gRPC service registered with shared-secret auth");
    }

    if grpc_registration_plan
        .health_state
        .livestream_relay_registered
    {
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
        .with_cluster_secret(config.cluster.secret.clone());

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

        let relay_interceptor = ClusterAuthInterceptor::new(config.cluster.secret.clone());
        routes.add_service(tonic::codegen::InterceptedService::new(
            synctv_livestream::grpc::StreamRelayServiceServer::new(relay_service)
                .with_transport_settings(max_message_size, config.server.grpc_compression_enabled),
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
    set_registered_grpc_services_serving(&health_reporter, grpc_registration_plan.health_state)
        .await;
    routes.add_service(health_service);
    tracing::info!("gRPC health check service registered");

    // Register gRPC reflection service if enabled in config
    if grpc_registration_plan.reflection_enabled {
        let reflection_service = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(synctv_proto::FILE_DESCRIPTOR_SET)
            .register_encoded_file_descriptor_set(synctv_proto::PROVIDERS_FILE_DESCRIPTOR_SET)
            .build_v1()
            .map_err(|e| anyhow::anyhow!("Failed to build gRPC reflection service: {e}"))?;
        routes.add_service(reflection_service);
        tracing::info!("gRPC reflection service registered");
    }

    let router = routes
        .routes()
        .into_axum_router()
        .layer(axum::middleware::from_fn(grpc_transport_only_middleware));

    let _ = health_reporter;
    Ok(router)
}

/// Build and start the gRPC server
pub async fn serve(mut grpc_config: GrpcServerConfig<'_>) -> anyhow::Result<()> {
    let shutdown_rx = grpc_config.shutdown_rx.clone();
    let grpc_listener = grpc_config.grpc_listener.take();
    let addr: std::net::SocketAddr = grpc_config.config.api_address().parse()?;
    let router = build_axum_router(grpc_config).await?;

    if let Some(listener) = grpc_listener {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            if let Some(mut rx) = shutdown_rx {
                let _ = rx.changed().await;
            } else {
                tokio::signal::ctrl_c().await.ok();
            }
        })
        .await
        .map_err(|e| anyhow::anyhow!("gRPC server error: {e}"))?;
    } else {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind API address {addr}: {e}"))?;
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            if let Some(mut rx) = shutdown_rx {
                let _ = rx.changed().await;
            } else {
                tokio::signal::ctrl_c().await.ok();
            }
        })
        .await
        .map_err(|e| anyhow::anyhow!("gRPC server error: {e}"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_fallback_http_app_state, cluster_node_id, extract_client_ip,
        grpc_service_registration_plan, grpc_unary_request_timeout, map_api_error,
        resolve_provider_proxy_runtime, set_registered_grpc_services_not_serving,
        set_registered_grpc_services_serving, should_mark_cluster_service_serving,
        should_mark_email_service_serving, should_mark_livestream_relay_serving,
        should_mark_notification_service_serving, should_mark_oauth2_service_serving,
        should_mark_provider_services_serving, should_register_cluster_grpc_service,
        should_register_email_service, should_register_livestream_relay_service,
        validate_cluster_grpc_runtime_requirements, FallbackHttpAppStateDeps,
        GrpcHealthRegistrationState,
    };
    use crate::runtime::{
        RealtimeConnectionService, RealtimeDeliveryOutcome, RealtimeDeliveryRequirement,
        RealtimeEventService, RealtimeMetrics,
    };
    use async_trait::async_trait;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use synctv_core::cache::UsernameCache;
    use synctv_core::models::{RoomId, UserId};
    use synctv_core::provider::{
        AlistProvider, BilibiliProvider, DirectUrlProvider, EmbyProvider, LiveProxyProvider,
        ProviderSet, RtmpProvider,
    };
    use synctv_core::repository::{SettingsRepository, UserProviderCredentialRepository};
    use synctv_core::service::{
        auth::JwtService, AuditService, ContentFilter, RateLimitConfig, RateLimiter,
        RemoteProviderManager, RoomService, SettingsRegistry, SettingsService, UserService,
    };
    use synctv_core_testing::{
        create_test_brute_force_protection_service, create_test_token_blacklist_store_service,
    };
    use synctv_realtime::sync::{BroadcastResult, RealtimeEvent};
    use tokio::sync::{broadcast, mpsc};

    #[test]
    fn test_map_api_error_timeout_maps_to_deadline_exceeded() {
        let status = map_api_error(crate::impls::ApiError::Timeout(
            "request budget exceeded".to_string(),
        ));
        assert_eq!(status.code(), tonic::Code::DeadlineExceeded);
        assert_eq!(status.message(), "request budget exceeded");
    }
    use tonic::metadata::{MetadataKey, MetadataValue};
    use tonic_health::pb::health_check_response::ServingStatus;
    use tonic_health::pb::health_server::Health;
    use tonic_health::pb::HealthCheckRequest;
    use tonic_health::server::HealthService;
    use tower::ServiceExt;

    struct FallbackGrpcTestContext {
        config: Arc<synctv_core::Config>,
        jwt_service: JwtService,
        user_service: Arc<UserService>,
        room_service: Arc<RoomService>,
        settings_service: Arc<SettingsService>,
        settings_registry: Arc<SettingsRegistry>,
        provider_instance_manager: Arc<RemoteProviderManager>,
        credential_repo: Arc<UserProviderCredentialRepository>,
        audit_service: Arc<AuditService>,
    }

    struct FakeRealtimeEventService {
        node_id: String,
        distributed_enabled: bool,
    }

    #[async_trait]
    impl RealtimeEventService for FakeRealtimeEventService {
        async fn subscribe_with_id(
            &self,
            _room_id: RoomId,
            _user_id: UserId,
            _connection_id: String,
        ) -> synctv_realtime::Result<(
            mpsc::Receiver<RealtimeEvent>,
            synctv_realtime::sync::ConnectionId,
        )> {
            panic!("subscribe_with_id should not be called in this test");
        }

        fn unsubscribe(&self, _connection_id: &str) {
            panic!("unsubscribe should not be called in this test");
        }

        fn broadcast(&self, _event: RealtimeEvent) -> BroadcastResult {
            panic!("broadcast should not be called in this test");
        }

        fn publish_only(&self, _event: RealtimeEvent) -> bool {
            panic!("publish_only should not be called in this test");
        }

        fn broadcast_local(&self, _room_id: &RoomId, _event: &RealtimeEvent) -> usize {
            panic!("broadcast_local should not be called in this test");
        }

        fn subscribe_admin_events(&self) -> broadcast::Receiver<RealtimeEvent> {
            panic!("subscribe_admin_events should not be called in this test");
        }

        fn metrics(&self) -> RealtimeMetrics {
            RealtimeMetrics {
                distributed_enabled: self.distributed_enabled,
            }
        }

        fn node_id(&self) -> &str {
            &self.node_id
        }

        async fn shutdown(&self) {}
    }

    fn fallback_grpc_test_context() -> FallbackGrpcTestContext {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
            .expect("lazy pool");
        let config = Arc::new(synctv_core::Config::default());
        let jwt_service =
            JwtService::new("test-secret-key-for-grpc-router-tests-minimum-32-chars").expect("jwt");
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 128, 60);
        let user_service = Arc::new(UserService::new(
            &pool,
            jwt_service.clone(),
            username_cache,
            synctv_core::config::PasswordComplexityConfig::default(),
            create_test_token_blacklist_store_service(),
            synctv_core::cache::KeyBuilder::new("test"),
            create_test_brute_force_protection_service(),
        ));
        let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
        let settings_service = Arc::new(SettingsService::new(
            SettingsRepository::new(pool.clone()),
            pool.clone(),
        ));
        let settings_registry = Arc::new(SettingsRegistry::new(settings_service.clone()));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
            synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
        )));
        let credential_repo = Arc::new(UserProviderCredentialRepository::new(pool.clone()));
        let audit_service = AuditService::new_unbuffered(pool);

        FallbackGrpcTestContext {
            config,
            jwt_service,
            user_service,
            room_service,
            settings_service,
            settings_registry,
            provider_instance_manager,
            credential_repo,
            audit_service: Arc::new(audit_service),
        }
    }

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

    fn shared_http_app_state() -> Arc<crate::http::AppState> {
        let context = fallback_grpc_test_context();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
            .expect("lazy pool");
        let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
            synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
        )));
        let providers = ProviderSet {
            alist: Arc::new(AlistProvider::new(provider_instance_manager.clone())),
            bilibili: Arc::new(BilibiliProvider::new(provider_instance_manager.clone())),
            emby: Arc::new(EmbyProvider::new(provider_instance_manager.clone())),
            direct_url: Arc::new(DirectUrlProvider::new()),
            rtmp: Arc::new(RtmpProvider::new()),
            live_proxy: Arc::new(LiveProxyProvider::new()),
        };
        let (audit_service, _audit_handle) = AuditService::new(pool.clone());

        let (_, state) =
            crate::http::create_router_with_state_from_config(crate::http::RouterConfig {
                config: Arc::new(synctv_core::Config::default()),
                user_service: context.user_service,
                user_cache: Arc::new(
                    synctv_core::cache::UserCache::local_only(
                        128,
                        60,
                        300,
                        "test:user:".to_string(),
                    )
                    .expect("user cache"),
                ),
                room_service: context.room_service,
                content_filter: ContentFilter::new(),
                provider_instance_manager,
                user_provider_credential_repository: Arc::new(
                    synctv_core::repository::UserProviderCredentialRepository::new(pool),
                ),
                providers,
                event_service: None,
                connection_manager: Arc::new(synctv_realtime::sync::ConnectionManager::new(
                    synctv_realtime::sync::ConnectionLimits::default(),
                )),
                jwt_service: context.jwt_service,
                realtime_fanout_service: crate::realtime_fanout::default_realtime_fanout_service(
                    None, false,
                ),
                oauth2_service: None,
                passkey_service: None,
                settings_service: None,
                settings_registry: None,
                email_service: None,
                email_token_service: None,
                publish_key_service: None,
                notification_service: None,
                chat_service: None,
                audit_service: Arc::new(audit_service),
                live_streaming_infrastructure: None,
                rate_limiter: Arc::new(RateLimiter::local_only("test:".to_string())),
                ws_ticket_service: Arc::new(synctv_core::service::WsTicketService::local_only(
                    None,
                )),
                redis_runtime: None,
                shared_provider_stores: None,
                shared_proxy_signing_key: None,
                builtin_stun_url: None,
                credential_encryption: None,
                proxy_slice_cache: Arc::new(synctv_proxy::slice_cache::SliceCache::new(
                    synctv_proxy::slice_cache::SliceCacheConfig::default(),
                )),
                ssrf_guard: synctv_common::ssrf::SsrfGuard::strict_policy(),
                proxy_http_client: synctv_proxy::build_proxy_http_client(
                    synctv_common::ssrf::SsrfGuard::strict_policy(),
                )
                .expect("proxy HTTP client should build for tests"),
                messaging_rate_limit_config: RateLimitConfig::default(),
                heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
                providers_manager: None,
            })
            .expect("HTTP app state should build for tests");

        Arc::new(state)
    }

    #[test]
    fn test_request_targets_grpc_transport_requires_grpc_content_type() {
        let mut headers = axum::http::HeaderMap::new();
        assert!(
            !super::request_targets_grpc_transport(&headers),
            "requests without Content-Type must not be treated as gRPC"
        );

        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        assert!(
            !super::request_targets_grpc_transport(&headers),
            "plain HTTP JSON requests must not be treated as gRPC"
        );

        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/grpc"),
        );
        assert!(
            super::request_targets_grpc_transport(&headers),
            "canonical gRPC requests must be routed to tonic"
        );

        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/grpc+proto; charset=utf-8"),
        );
        assert!(
            super::request_targets_grpc_transport(&headers),
            "gRPC content-type variants must still be routed to tonic"
        );
    }

    #[tokio::test]
    async fn test_grpc_transport_gate_returns_not_found_for_non_grpc_requests() {
        let app = axum::Router::new()
            .route("/", axum::routing::get(|| async { "grpc route" }))
            .layer(axum::middleware::from_fn(
                super::grpc_transport_only_middleware,
            ));

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/")
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(
            response.status(),
            axum::http::StatusCode::NOT_FOUND,
            "non-gRPC requests must fall through instead of being handled as tonic responses"
        );
        assert!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .is_none_or(|value| value != "application/grpc"),
            "non-gRPC responses must not advertise a gRPC content-type"
        );
    }

    #[tokio::test]
    async fn test_build_fallback_http_app_state_reuses_shared_runtime_instances() {
        let context = fallback_grpc_test_context();
        let connection_service: Arc<dyn RealtimeConnectionService> =
            Arc::new(synctv_realtime::sync::ConnectionManager::new(
                synctv_realtime::sync::ConnectionLimits::default(),
            ));
        let event_service: Arc<dyn RealtimeEventService> = Arc::new(FakeRealtimeEventService {
            node_id: "fallback-http-node".to_string(),
            distributed_enabled: true,
        });
        let provider_stores: Arc<dyn synctv_core::provider::store::ProviderStoreResolver> =
            Arc::new(
                synctv_core::provider::store::ProviderStoreRegistry::local_only(
                    context.config.redis.key_prefix.clone(),
                ),
            );
        let ws_ticket_service: Arc<dyn synctv_core::service::WebSocketTicketService> =
            Arc::new(synctv_core::service::WsTicketService::local_only(None));
        let proxy_signing_key = Arc::new(synctv_core::service::ProxySigningKey::derive_from(
            b"test-secret-key-for-grpc-router-tests-minimum-32-chars",
        ));
        let content_filter =
            ContentFilter::with_config(17, 9, Some(vec!["blocked".to_string()]), false);
        let messaging_rate_limit_config = RateLimitConfig {
            chat_per_second: 23,
            danmaku_per_second: 7,
            window_seconds: 11,
        };
        let mut fallback_config = context.config.as_ref().clone();
        fallback_config.proxy_slice_cache.enabled = false;
        fallback_config.proxy_slice_cache.slice_size_bytes = 4 * 1024 * 1024;
        fallback_config.proxy_slice_cache.max_cache_size_bytes = 1024 * 1024 * 1024;
        fallback_config.proxy_slice_cache.segment_ttl_seconds = 600;
        fallback_config.proxy_slice_cache.stale_max_age_seconds = 120;
        fallback_config.proxy_slice_cache.stale_while_revalidate = false;
        fallback_config.proxy_slice_cache.eviction_interval_seconds = 30;
        fallback_config.proxy_slice_cache.watermark_ratio = 0.75;
        let fallback_config = Arc::new(fallback_config);
        let http_state = build_fallback_http_app_state(FallbackHttpAppStateDeps {
            user_service: context.user_service,
            user_cache: Arc::new(
                synctv_core::cache::UserCache::local_only(128, 60, 300, "test:user:".to_string())
                    .expect("user cache"),
            ),
            room_service: context.room_service,
            event_service: Some(event_service.clone()),
            connection_service: connection_service.clone(),
            config: fallback_config.clone(),
            content_filter: content_filter.clone(),
            publish_key_service: None,
            jwt_service: context.jwt_service,
            live_streaming_infrastructure: None,
            providers_manager: None,
            provider_instance_manager: context.provider_instance_manager,
            notification_service: None,
            chat_service: None,
            oauth2_service: None,
            passkey_service: None,
            settings_service: context.settings_service,
            settings_registry: Some(context.settings_registry),
            email_service: None,
            email_token_service: None,
            ws_ticket_service: ws_ticket_service.clone(),
            realtime_fanout_service: crate::realtime_fanout::default_realtime_fanout_service(
                None, false,
            ),
            redis_runtime: None,
            rate_limiter: Arc::new(RateLimiter::local_only("test:".to_string())),
            messaging_rate_limit_config: messaging_rate_limit_config.clone(),
            credential_encryption: None,
            credential_repo: context.credential_repo,
            proxy_signing_key: proxy_signing_key.clone(),
            provider_stores: provider_stores.clone(),
            builtin_stun_url: None,
            audit_service: context.audit_service,
        });

        assert!(
            Arc::ptr_eq(
                &http_state.shared_api_runtime.client_api.connection_service,
                &connection_service
            ),
            "standalone gRPC fallback HTTP state must reuse the injected connection service for client APIs"
        );
        assert!(
            Arc::ptr_eq(
                &http_state
                    .shared_api_runtime
                    .admin_api
                    .as_ref()
                    .expect("fallback HTTP state should wire admin API")
                    .connection_service,
                &connection_service
            ),
            "standalone gRPC fallback HTTP state must reuse the injected connection service for admin APIs"
        );
        assert!(
            Arc::ptr_eq(
                &http_state.shared_api_runtime.proxy_signing_key,
                &proxy_signing_key
            ),
            "fallback HTTP state must reuse the shared proxy signing key"
        );
        assert!(
            Arc::ptr_eq(&http_state.ws_ticket_service, &ws_ticket_service),
            "fallback HTTP state must reuse the injected WebSocket ticket service"
        );
        assert!(
            Arc::ptr_eq(
                &http_state.shared_api_runtime.provider_stores,
                &provider_stores
            ),
            "fallback HTTP state must reuse the shared provider store registry"
        );
        assert!(
            Arc::ptr_eq(
                &http_state.shared_api_runtime.provider_access_service,
                &http_state.shared_api_runtime.provider_access_service
            ),
            "fallback HTTP state must expose the same provider access cache across transports"
        );
        assert!(
            Arc::ptr_eq(
                http_state
                    .event_service
                    .as_ref()
                    .expect("fallback HTTP state must preserve realtime events"),
                &event_service,
            ),
            "fallback HTTP state must preserve the injected realtime event service"
        );
        assert_eq!(
            http_state.shared_api_runtime.content_filter.max_chat_length,
            content_filter.max_chat_length,
            "fallback HTTP state must preserve custom chat filtering limits"
        );
        assert_eq!(
            http_state
                .shared_api_runtime
                .content_filter
                .max_danmaku_length,
            content_filter.max_danmaku_length,
            "fallback HTTP state must preserve custom danmaku filtering limits"
        );
        assert_eq!(
            http_state
                .shared_api_runtime
                .messaging_rate_limit_config
                .chat_per_second,
            messaging_rate_limit_config.chat_per_second,
            "fallback HTTP state must preserve configured chat rate limits"
        );
        assert_eq!(
            http_state
                .shared_api_runtime
                .messaging_rate_limit_config
                .danmaku_per_second,
            messaging_rate_limit_config.danmaku_per_second,
            "fallback HTTP state must preserve configured danmaku rate limits"
        );
        assert_eq!(
            http_state
                .shared_api_runtime
                .messaging_rate_limit_config
                .window_seconds,
            messaging_rate_limit_config.window_seconds,
            "fallback HTTP state must preserve configured rate-limit windows"
        );
        assert_eq!(
            http_state.proxy_slice_cache.config().enabled,
            fallback_config.proxy_slice_cache.enabled,
            "fallback HTTP state must preserve proxy slice cache enablement"
        );
        assert_eq!(
            http_state.proxy_slice_cache.config().slice_size,
            fallback_config.proxy_slice_cache.slice_size_bytes,
            "fallback HTTP state must preserve proxy slice size"
        );
        assert_eq!(
            http_state.proxy_slice_cache.config().max_cache_size,
            fallback_config.proxy_slice_cache.max_cache_size_bytes,
            "fallback HTTP state must preserve proxy cache size"
        );
        assert_eq!(
            http_state.proxy_slice_cache.config().segment_ttl,
            std::time::Duration::from_secs(fallback_config.proxy_slice_cache.segment_ttl_seconds),
            "fallback HTTP state must preserve proxy cache TTL"
        );
        assert_eq!(
            http_state.proxy_slice_cache.config().stale_max_age,
            std::time::Duration::from_secs(fallback_config.proxy_slice_cache.stale_max_age_seconds),
            "fallback HTTP state must preserve proxy stale max age"
        );
        assert_eq!(
            http_state.proxy_slice_cache.config().stale_while_revalidate,
            fallback_config.proxy_slice_cache.stale_while_revalidate,
            "fallback HTTP state must preserve stale-while-revalidate"
        );
        assert_eq!(
            http_state.proxy_slice_cache.config().eviction_interval,
            std::time::Duration::from_secs(
                fallback_config.proxy_slice_cache.eviction_interval_seconds,
            ),
            "fallback HTTP state must preserve eviction interval"
        );
        assert!(
            (http_state.proxy_slice_cache.config().watermark_ratio
                - fallback_config.proxy_slice_cache.watermark_ratio)
                .abs()
                < f64::EPSILON,
            "fallback HTTP state must preserve cache watermark"
        );
    }

    #[test]
    fn test_cluster_node_id_uses_injected_event_service() {
        let event_service: Arc<dyn RealtimeEventService> = Arc::new(FakeRealtimeEventService {
            node_id: "fake-node".to_string(),
            distributed_enabled: true,
        });

        assert_eq!(
            cluster_node_id(Some(&event_service)),
            "fake-node",
            "gRPC transport must derive cluster node identity from the injected realtime event service"
        );
    }

    #[tokio::test]
    async fn test_resolve_provider_proxy_runtime_reuses_shared_http_app_state() {
        let shared_http_app_state = shared_http_app_state();
        let config = synctv_core::Config::default();

        let (signing_key, provider_stores) = resolve_provider_proxy_runtime(
            &config,
            None,
            Some(&shared_http_app_state.shared_api_runtime),
        );

        assert!(
            Arc::ptr_eq(
                &signing_key,
                &shared_http_app_state.shared_api_runtime.proxy_signing_key
            ),
            "gRPC must reuse the shared proxy signing key when unified app state is provided"
        );
        assert!(
            Arc::ptr_eq(
                &provider_stores,
                &shared_http_app_state.shared_api_runtime.provider_stores
            ),
            "gRPC must reuse the shared provider store registry when unified app state is provided"
        );
    }

    #[test]
    fn test_cluster_grpc_service_requires_cluster_mode() {
        let mut config = synctv_core::Config::default();
        config.cluster.secret = "shared-secret".to_string();

        assert!(
            !should_register_cluster_grpc_service(&config, true),
            "cluster.secret alone must not enable cluster gRPC"
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
        config.cluster.secret = "shared-secret".to_string();

        assert!(
            !should_register_cluster_grpc_service(&config, false),
            "cluster gRPC must not be registered before NodeRegistry is ready"
        );
    }

    #[test]
    fn test_cluster_grpc_runtime_requires_node_registry() {
        let mut config = synctv_core::Config::default();
        config.cluster.enabled = true;
        config.cluster.secret = "shared-secret".to_string();

        let err = validate_cluster_grpc_runtime_requirements(&config, false)
            .expect_err("realtime runtime must fail closed without NodeRegistry");

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
            .expect_err("realtime runtime must fail closed without cluster.secret");

        assert!(
            err.to_string().contains("cluster.secret"),
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
            "distributed mode without a secret must fail closed"
        );

        config.cluster.secret = "shared-secret".to_string();
        assert!(
            !should_register_livestream_relay_service(&config, false),
            "relay service must not be registered before livestream infra is ready"
        );

        assert!(
            should_register_livestream_relay_service(&config, true),
            "distributed mode with secret and livestream infra should register relay service"
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
    fn test_public_grpc_registration_plan_preserves_optional_service_registration() {
        let config = synctv_core::Config::default();

        let plan = grpc_service_registration_plan(
            &config,
            super::GrpcOptionalRegistrations {
                email_registered: true,
                notification_registered: false,
                oauth2_registered: true,
                provider_services_registered: false,
                cluster_service_registered: true,
                realtime_presence_registered: true,
                proxy_slice_cache_registered: true,
                livestream_relay_registered: false,
            },
        );

        assert!(plan.health_state.auth_registered);
        assert!(plan.health_state.user_registered);
        assert!(plan.health_state.room_registered);
        assert!(plan.health_state.public_registered);
        assert!(plan.health_state.admin_registered);
        assert!(plan.health_state.email_registered);
        assert!(!plan.health_state.notification_registered);
        assert!(plan.health_state.oauth2_registered);
        assert!(!plan.health_state.provider_services_registered);
        assert!(plan.health_state.cluster_service_registered);
        assert!(plan.health_state.realtime_presence_registered);
        assert!(plan.health_state.proxy_slice_cache_registered);
        assert!(!plan.health_state.livestream_relay_registered);
    }

    #[test]
    fn test_user_notification_fanout_requires_distributed_delivery_only_when_available() {
        let requirement = RealtimeDeliveryRequirement::DistributedIfAvailable;

        assert!(!RealtimeDeliveryOutcome::from_publish_only(
            false,
            RealtimeMetrics {
                distributed_enabled: true,
            },
        )
        .satisfies(requirement));
        assert!(RealtimeDeliveryOutcome::from_publish_only(
            true,
            RealtimeMetrics {
                distributed_enabled: true,
            },
        )
        .satisfies(requirement));
        assert!(RealtimeDeliveryOutcome::from_publish_only(
            false,
            RealtimeMetrics {
                distributed_enabled: false,
            },
        )
        .satisfies(requirement));
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
        config.cluster.secret = "shared-secret".to_string();

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
            auth_registered: true,
            user_registered: true,
            room_registered: true,
            public_registered: true,
            admin_registered: true,
            email_registered: true,
            notification_registered: true,
            oauth2_registered: false,
            provider_services_registered: false,
            cluster_service_registered: false,
            realtime_presence_registered: false,
            proxy_slice_cache_registered: false,
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
