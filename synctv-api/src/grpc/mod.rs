// gRPC transport adapter.
//
// Keep business behavior in synctv-api/src/impls and synctv-core. gRPC code
// owns protobuf transport, metadata extraction, status mapping, and streaming
// adapters. Shared contracts are documented in
// docs/src/content/docs/en/develop/implementation-contracts.mdx.

use synctv_proto::client::o_auth2_service_server::OAuth2ServiceServer;
use synctv_proto::playback_provider::alist::alist_playback_provider_service_server::AlistPlaybackProviderServiceServer;
use synctv_proto::playback_provider::bilibili::bilibili_playback_provider_service_server::BilibiliPlaybackProviderServiceServer;
use synctv_proto::playback_provider::direct_url::direct_url_playback_provider_service_server::DirectUrlPlaybackProviderServiceServer;
use synctv_proto::playback_provider::emby::emby_playback_provider_service_server::EmbyPlaybackProviderServiceServer;
use synctv_proto::playback_provider::live_proxy::live_proxy_playback_provider_service_server::LiveProxyPlaybackProviderServiceServer;
use synctv_proto::playback_provider::rtmp::rtmp_playback_provider_service_server::RtmpPlaybackProviderServiceServer;
use synctv_proto::providers::alist::alist_provider_service_server::AlistProviderServiceServer;
use synctv_proto::providers::bilibili::bilibili_provider_service_server::BilibiliProviderServiceServer;
use synctv_proto::providers::common::provider_common_service_server::ProviderCommonServiceServer;
use synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderServiceServer;
use synctv_proto::providers::rtmp::rtmp_provider_service_server::RtmpProviderServiceServer;
use synctv_realtime::grpc::RealtimePresenceServiceServer;
use tonic::codec::CompressionEncoding;

pub(crate) mod admin_service;
pub(crate) mod client_service;
pub(crate) mod notification_service;
pub(crate) mod oauth2_service;
pub(crate) mod playback_provider;

// Provider gRPC services (local implementations)
// Provider-specific gRPC services are registered from provider instances
pub(crate) mod providers;

pub use admin_service::AdminServiceImpl;
pub use client_service::{ClientServiceImpl, ClientServiceOptions};
pub(crate) use notification_service::NotificationServiceImpl;
pub use synctv_cluster::grpc::ClusterAuthInterceptor;

pub(crate) use crate::grpc_support::{
    extract_client_ip, grpc_unary_request_timeout, map_api_error, request_metadata,
    request_user_agent,
};

/// Trait to apply gRPC message size limits to tonic service servers.
///
/// This trait provides a unified interface for setting max decoding/encoding
/// message sizes on tonic-generated service servers, protecting against OOM
/// attacks from oversized messages.
pub(crate) trait GrpcServiceExt: Sized {
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
impl_grpc_service_ext!(<T> synctv_proto::client::auth_service_server::AuthServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::client::user_service_server::UserServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::client::room_service_server::RoomServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::client::public_service_server::PublicServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::client::email_service_server::EmailServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::client::notification_service_server::NotificationServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::client::o_auth2_service_server::OAuth2ServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::admin::admin_service_server::AdminServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::providers::alist::alist_provider_service_server::AlistProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::providers::bilibili::bilibili_provider_service_server::BilibiliProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::providers::common::provider_common_service_server::ProviderCommonServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::providers::rtmp::rtmp_provider_service_server::RtmpProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::playback_provider::direct_url::direct_url_playback_provider_service_server::DirectUrlPlaybackProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::playback_provider::alist::alist_playback_provider_service_server::AlistPlaybackProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::playback_provider::emby::emby_playback_provider_service_server::EmbyPlaybackProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::playback_provider::bilibili::bilibili_playback_provider_service_server::BilibiliPlaybackProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::playback_provider::rtmp::rtmp_playback_provider_service_server::RtmpPlaybackProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proto::playback_provider::live_proxy::live_proxy_playback_provider_service_server::LiveProxyPlaybackProviderServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_livestream::StreamRelayServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_cluster::grpc::ClusterServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_cluster::grpc::ServerStateServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_realtime::grpc::RealtimePresenceServiceServer<T>);
impl_grpc_service_ext!(<T> synctv_proxy::grpc::ProxySliceCacheServiceServer<T>);

const fn should_register_cluster_grpc_service(
    runtime_settings: &crate::ApiRuntimeSettings,
    node_registry_available: bool,
) -> bool {
    runtime_settings.cluster_runtime_enabled()
        && !runtime_settings.cluster.secret.is_empty()
        && node_registry_available
}

const fn should_register_livestream_relay_service(
    runtime_settings: &crate::ApiRuntimeSettings,
    live_streaming_infrastructure_available: bool,
) -> bool {
    runtime_settings.cluster_runtime_enabled()
        && !runtime_settings.cluster.secret.is_empty()
        && live_streaming_infrastructure_available
}

const fn should_register_proxy_slice_cache_service(
    runtime_settings: &crate::ApiRuntimeSettings,
) -> bool {
    runtime_settings.cluster_runtime_enabled() && !runtime_settings.cluster.secret.is_empty()
}

const fn should_register_server_state_service(
    runtime_settings: &crate::ApiRuntimeSettings,
) -> bool {
    runtime_settings.cluster_runtime_enabled() && !runtime_settings.cluster.secret.is_empty()
}

const fn should_register_realtime_presence_service(
    runtime_settings: &crate::ApiRuntimeSettings,
) -> bool {
    runtime_settings.cluster_runtime_enabled() && !runtime_settings.cluster.secret.is_empty()
}

const fn should_register_email_service(email_available: bool, email_token_available: bool) -> bool {
    email_available && email_token_available
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
    server_state_registered: bool,
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
    server_state_registered: bool,
    realtime_presence_registered: bool,
    proxy_slice_cache_registered: bool,
    livestream_relay_registered: bool,
}

const fn grpc_service_registration_plan(
    runtime_settings: &crate::ApiRuntimeSettings,
    optional: GrpcOptionalRegistrations,
) -> GrpcServiceRegistrationPlan {
    GrpcServiceRegistrationPlan {
        reflection_enabled: runtime_settings.server.enable_reflection,
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
            server_state_registered: optional.server_state_registered,
            realtime_presence_registered: optional.realtime_presence_registered,
            proxy_slice_cache_registered: optional.proxy_slice_cache_registered,
            livestream_relay_registered: optional.livestream_relay_registered,
        },
    }
}

fn request_targets_grpc_transport(
    headers: &axum::http::HeaderMap,
) -> Result<bool, axum::http::header::ToStrError> {
    let Some(value) = headers.get(axum::http::header::CONTENT_TYPE) else {
        return Ok(false);
    };
    let media_type = value
        .to_str()?
        .trim()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    Ok(media_type.starts_with("application/grpc"))
}

async fn grpc_transport_only_middleware(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    match request_targets_grpc_transport(request.headers()) {
        Ok(true) => next.run(request).await,
        Ok(false) => axum::response::IntoResponse::into_response(axum::http::StatusCode::NOT_FOUND),
        Err(_) => axum::response::IntoResponse::into_response(axum::http::StatusCode::BAD_REQUEST),
    }
}

async fn set_registered_grpc_services_with_status(
    health_reporter: &tonic_health::server::HealthReporter,
    state: GrpcHealthRegistrationState,
    serving: bool,
) {
    use synctv_proto::client::o_auth2_service_server::OAuth2ServiceServer;

    let status = if serving {
        tonic_health::ServingStatus::Serving
    } else {
        tonic_health::ServingStatus::NotServing
    };
    health_reporter.set_service_status("", status).await;

    macro_rules! set_service_status {
        ($health_reporter:expr, $condition:expr, $service_type:ty) => {
            if $condition {
                if serving {
                    $health_reporter.set_serving::<$service_type>().await;
                } else {
                    $health_reporter.set_not_serving::<$service_type>().await;
                }
            }
        };
    }

    set_service_status!(
        health_reporter,
        state.auth_registered,
        AuthServiceServer<ClientServiceImpl>
    );
    set_service_status!(
        health_reporter,
        state.user_registered,
        UserServiceServer<ClientServiceImpl>
    );
    set_service_status!(
        health_reporter,
        state.room_registered,
        RoomServiceServer<ClientServiceImpl>
    );
    set_service_status!(
        health_reporter,
        state.public_registered,
        PublicServiceServer<ClientServiceImpl>
    );
    set_service_status!(
        health_reporter,
        state.email_registered,
        EmailServiceServer<ClientServiceImpl>
    );
    set_service_status!(
        health_reporter,
        state.admin_registered,
        AdminServiceServer<AdminServiceImpl>
    );
    set_service_status!(
        health_reporter,
        state.notification_registered,
        NotificationServiceServer<NotificationServiceImpl>
    );
    set_service_status!(
        health_reporter,
        state.oauth2_registered,
        OAuth2ServiceServer<oauth2_service::OAuth2GrpcService>
    );

    if state.provider_services_registered {
        use synctv_proto::providers::alist::alist_provider_service_server::AlistProviderServiceServer;
        use synctv_proto::providers::bilibili::bilibili_provider_service_server::BilibiliProviderServiceServer;
        use synctv_proto::providers::common::provider_common_service_server::ProviderCommonServiceServer;
        use synctv_proto::providers::emby::emby_provider_service_server::EmbyProviderServiceServer;
        use synctv_proto::providers::rtmp::rtmp_provider_service_server::RtmpProviderServiceServer;

        set_service_status!(
            health_reporter,
            true,
            ProviderCommonServiceServer<providers::common::ProviderCommonGrpcService>
        );
        set_service_status!(
            health_reporter,
            true,
            AlistProviderServiceServer<providers::alist::AlistProviderGrpcService>
        );
        set_service_status!(
            health_reporter,
            true,
            BilibiliProviderServiceServer<providers::bilibili::BilibiliProviderGrpcService>
        );
        set_service_status!(
            health_reporter,
            true,
            EmbyProviderServiceServer<providers::emby::EmbyProviderGrpcService>
        );
        set_service_status!(
            health_reporter,
            true,
            RtmpProviderServiceServer<providers::rtmp::RtmpProviderGrpcService>
        );
        set_service_status!(
            health_reporter,
            true,
            DirectUrlPlaybackProviderServiceServer<
                playback_provider::direct_url::DirectUrlPlaybackProviderGrpcService,
            >
        );
        set_service_status!(
            health_reporter,
            true,
            AlistPlaybackProviderServiceServer<
                playback_provider::alist::AlistPlaybackProviderGrpcService,
            >
        );
        set_service_status!(
            health_reporter,
            true,
            EmbyPlaybackProviderServiceServer<
                playback_provider::emby::EmbyPlaybackProviderGrpcService,
            >
        );
        set_service_status!(
            health_reporter,
            true,
            BilibiliPlaybackProviderServiceServer<
                playback_provider::bilibili::BilibiliPlaybackProviderGrpcService,
            >
        );
        set_service_status!(
            health_reporter,
            true,
            RtmpPlaybackProviderServiceServer<
                playback_provider::rtmp::RtmpPlaybackProviderGrpcService,
            >
        );
        set_service_status!(
            health_reporter,
            true,
            LiveProxyPlaybackProviderServiceServer<
                playback_provider::live_proxy::LiveProxyPlaybackProviderGrpcService,
            >
        );
    }

    set_service_status!(
        health_reporter,
        state.cluster_service_registered,
        synctv_cluster::grpc::ClusterServiceServer<synctv_cluster::grpc::ClusterServer>
    );
    set_service_status!(
        health_reporter,
        state.server_state_registered,
        synctv_cluster::grpc::ServerStateServiceServer<crate::status::ServerStateGrpcService>
    );
    set_service_status!(
        health_reporter,
        state.realtime_presence_registered,
        synctv_realtime::grpc::RealtimePresenceServiceServer<
            synctv_realtime::grpc::RealtimePresenceServiceImpl,
        >
    );
    set_service_status!(
        health_reporter,
        state.proxy_slice_cache_registered,
        synctv_proxy::grpc::ProxySliceCacheServiceServer<
            synctv_proxy::grpc::ProxySliceCacheServiceImpl,
        >
    );
    set_service_status!(
        health_reporter,
        state.livestream_relay_registered,
        synctv_livestream::StreamRelayServiceServer<synctv_livestream::StreamRelayServiceImpl>
    );
}

async fn set_registered_grpc_services_serving(
    health_reporter: &tonic_health::server::HealthReporter,
    state: GrpcHealthRegistrationState,
) {
    set_registered_grpc_services_with_status(health_reporter, state, true).await;
}

async fn set_registered_grpc_services_not_serving(
    health_reporter: &tonic_health::server::HealthReporter,
    state: GrpcHealthRegistrationState,
) {
    set_registered_grpc_services_with_status(health_reporter, state, false).await;
}

struct BuiltGrpcRouter {
    router: axum::Router,
    health_reporter: tonic_health::server::HealthReporter,
    health_state: GrpcHealthRegistrationState,
}

fn validate_cluster_grpc_runtime_requirements(
    runtime_settings: &crate::ApiRuntimeSettings,
    node_registry_available: bool,
) -> anyhow::Result<()> {
    if runtime_settings.cluster_runtime_enabled() && runtime_settings.cluster.secret.is_empty() {
        return Err(anyhow::anyhow!(
            "cluster.enabled=true requires cluster.secret before starting the gRPC server; refusing to start with unauthenticated cluster endpoints"
        ));
    }

    if runtime_settings.cluster_runtime_enabled() && !node_registry_available {
        return Err(anyhow::anyhow!(
            "cluster.enabled=true requires NodeRegistry before starting the gRPC server; refusing to start with cluster gRPC disabled"
        ));
    }

    Ok(())
}

// Use synctv_proto for all server traits and message types (single source of truth)
use std::sync::Arc;
use synctv_core::service::JwtService;
use synctv_core::service::{
    ContentFilter, EmailService, EmailTokenService, ProvidersManager, RateLimitConfig,
    RequestRateLimiterService, RoomService as CoreRoomService, RuntimeSettingsStore,
    SettingsService, UserService as CoreUserService,
};
use synctv_proto::admin::admin_service_server::AdminServiceServer;
use synctv_proto::client::{
    auth_service_server::AuthServiceServer, email_service_server::EmailServiceServer,
    notification_service_server::NotificationServiceServer,
    public_service_server::PublicServiceServer, room_service_server::RoomServiceServer,
    user_service_server::UserServiceServer,
};
use synctv_realtime::fanout::{
    RealtimeDeliveryRequirement, RealtimeEventService, RealtimeFanoutService,
};
use synctv_realtime::sync::ConnectionRuntime;

/// Options for gRPC server construction
pub struct GrpcServerOptions<'a> {
    pub runtime_settings: &'a crate::ApiRuntimeSettings,
    pub jwt_service: JwtService,
    pub user_service: Arc<CoreUserService>,
    pub read_pool: Option<sqlx::PgPool>,
    pub user_cache: Arc<synctv_core::cache::UserCache>,
    pub room_service: Arc<CoreRoomService>,
    pub event_service: Arc<dyn RealtimeEventService>,
    pub realtime_fanout_service: Arc<dyn RealtimeFanoutService>,
    pub rate_limiter: Arc<dyn RequestRateLimiterService>,
    pub rate_limit_config: RateLimitConfig,
    pub content_filter: ContentFilter,
    pub connection_service: Arc<dyn ConnectionRuntime>,
    pub presence_service: Arc<synctv_core::service::OnlinePresenceService>,
    pub jwt_validator: Arc<synctv_core::service::JwtValidator>,
    pub security_pipeline: Arc<synctv_core::service::SecurityPipeline>,
    pub public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
    pub request_executor: Arc<crate::impls::RequestExecutor>,
    pub metrics_access_controller: Arc<crate::metrics_auth::MetricsAccessController>,
    pub client_api: Arc<crate::impls::ClientApiImpl>,
    pub admin_api: Option<Arc<crate::impls::AdminApiImpl>>,
    pub email_api: Option<Arc<crate::impls::EmailApiImpl>>,
    pub notification_api: Option<Arc<crate::impls::NotificationApiImpl>>,
    pub oauth2_api: Option<Arc<crate::impls::OAuth2ApiImpl>>,
    pub providers_manager: Option<Arc<ProvidersManager>>,
    pub provider_access_service: Arc<dyn synctv_core::provider::ProviderAccessService>,
    pub settings_service: Arc<SettingsService>,
    pub runtime_settings_store: Option<Arc<RuntimeSettingsStore>>,
    pub email_service: Option<Arc<EmailService>>,
    pub email_token_service: Option<Arc<EmailTokenService>>,
    pub ws_ticket_service: Arc<dyn synctv_core::service::WebSocketTicketService>,
    pub live_streaming_infrastructure: Option<Arc<synctv_livestream::LiveStreamingInfrastructure>>,
    pub publish_key_service: Option<Arc<dyn synctv_core::service::StreamingPublishKeyService>>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub chat_service: Option<Arc<synctv_core::service::ChatService>>,
    pub oauth2_service: Option<Arc<synctv_core::service::OAuth2Service>>,
    pub passkey_service: Option<Arc<synctv_core::service::PasskeyService>>,
    pub audit_service: Arc<synctv_core::service::AuditService>,
    pub node_registry: Option<Arc<dyn synctv_cluster::discovery::ClusterNodeDirectory>>,
    /// Shared runtime for playback caching and other shared-state lookups.
    pub redis_runtime: Option<Arc<dyn synctv_core::RedisConnectionRuntime>>,
    pub proxy_signing_key: Arc<crate::proxy_signature::ProxySigningKey>,
    pub provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver>,
    pub playback_transport_services: Arc<synctv_core::provider::PlaybackTransportServices>,
    pub alist_playback_provider_service: Arc<synctv_core::service::AlistPlaybackProviderService>,
    pub bilibili_playback_provider_service:
        Arc<synctv_core::service::BilibiliPlaybackProviderService>,
    pub direct_url_playback_provider_service:
        Arc<synctv_core::service::DirectUrlPlaybackProviderService>,
    pub emby_playback_provider_service: Arc<synctv_core::service::EmbyPlaybackProviderService>,
    pub rtmp_playback_provider_service: Arc<synctv_core::service::RtmpPlaybackProviderService>,
    pub live_proxy_playback_provider_service:
        Arc<synctv_core::service::LiveProxyPlaybackProviderService>,
    pub provider_common_api: Arc<crate::impls::ProviderCommonApiImpl>,
    pub bilibili_api: Arc<crate::impls::BilibiliApiImpl>,
    pub alist_api: Arc<crate::impls::AlistApiImpl>,
    pub emby_api: Arc<crate::impls::EmbyApiImpl>,
    pub proxy_slice_cache: Arc<synctv_proxy::slice_cache::SliceCache>,
    pub ssrf_guard: synctv_common::ssrf::SsrfGuard,
    pub proxy_http_client: reqwest::Client,
    /// Shared HTTP app state from the unified API server.
    ///
    /// When present, gRPC reuses the HTTP proxy/signing infrastructure instead
    /// of constructing a transport-local copy. gRPC handlers should translate
    /// protobuf requests into impl calls and share business behavior with HTTP;
    /// transport-local code owns framing, metadata, and status conversion.
    pub shared_http_app_state: Option<Arc<crate::http::AppState>>,
    pub shutdown_rx: Option<tokio::sync::watch::Receiver<bool>>,
    /// Resolved built-in STUN URL (e.g. "stun:203.0.113.1:3478") from a successfully started
    /// STUN server. When `None`, the built-in STUN entry is omitted from ICE server lists.
    pub builtin_stun_url: Option<String>,
    /// Structured WebRTC/STUN runtime state exposed through shared API runtime.
    pub webrtc_status: synctv_core::service::WebRtcRuntimeStatus,
    /// Credential encryption for provider credential resolution
    pub credential_encryption: Option<synctv_core::credential_encryption::CredentialEncryption>,
    /// Pre-bound TCP listener for the gRPC server.
    /// When provided, the server will use this listener instead of binding internally.
    /// This allows the caller to detect port-in-use errors before spawning the server task.
    pub grpc_listener: Option<tokio::net::TcpListener>,
}

struct FallbackHttpAppStateDeps {
    user_service: Arc<CoreUserService>,
    read_pool: Option<sqlx::PgPool>,
    user_cache: Arc<synctv_core::cache::UserCache>,
    room_service: Arc<CoreRoomService>,
    event_service: Arc<dyn RealtimeEventService>,
    connection_service: Arc<dyn ConnectionRuntime>,
    presence_service: Arc<synctv_core::service::OnlinePresenceService>,
    runtime_settings: Arc<crate::ApiRuntimeSettings>,
    content_filter: ContentFilter,
    publish_key_service: Option<Arc<dyn synctv_core::service::StreamingPublishKeyService>>,
    jwt_service: JwtService,
    jwt_validator: Arc<synctv_core::service::JwtValidator>,
    security_pipeline: Arc<synctv_core::service::SecurityPipeline>,
    public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
    request_executor: Arc<crate::impls::RequestExecutor>,
    metrics_access_controller: Arc<crate::metrics_auth::MetricsAccessController>,
    client_api: Arc<crate::impls::ClientApiImpl>,
    admin_api: Option<Arc<crate::impls::AdminApiImpl>>,
    email_api: Option<Arc<crate::impls::EmailApiImpl>>,
    notification_api: Option<Arc<crate::impls::NotificationApiImpl>>,
    oauth2_api: Option<Arc<crate::impls::OAuth2ApiImpl>>,
    live_streaming_infrastructure: Option<Arc<synctv_livestream::LiveStreamingInfrastructure>>,
    providers_manager: Arc<ProvidersManager>,
    notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    chat_service: Option<Arc<synctv_core::service::ChatService>>,
    oauth2_service: Option<Arc<synctv_core::service::OAuth2Service>>,
    passkey_service: Option<Arc<synctv_core::service::PasskeyService>>,
    settings_service: Arc<SettingsService>,
    runtime_settings_store: Option<Arc<RuntimeSettingsStore>>,
    email_service: Option<Arc<EmailService>>,
    email_token_service: Option<Arc<EmailTokenService>>,
    ws_ticket_service: Arc<dyn synctv_core::service::WebSocketTicketService>,
    realtime_fanout_service: Arc<dyn RealtimeFanoutService>,
    redis_runtime: Option<Arc<dyn synctv_core::RedisConnectionRuntime>>,
    rate_limiter: Arc<dyn RequestRateLimiterService>,
    messaging_rate_limit_config: RateLimitConfig,
    credential_encryption: Option<synctv_core::credential_encryption::CredentialEncryption>,
    provider_access_service: Arc<dyn synctv_core::provider::ProviderAccessService>,
    proxy_signing_key: Arc<crate::proxy_signature::ProxySigningKey>,
    provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver>,
    playback_transport_services: Arc<synctv_core::provider::PlaybackTransportServices>,
    alist_playback_provider_service: Arc<synctv_core::service::AlistPlaybackProviderService>,
    bilibili_playback_provider_service: Arc<synctv_core::service::BilibiliPlaybackProviderService>,
    direct_url_playback_provider_service:
        Arc<synctv_core::service::DirectUrlPlaybackProviderService>,
    emby_playback_provider_service: Arc<synctv_core::service::EmbyPlaybackProviderService>,
    rtmp_playback_provider_service: Arc<synctv_core::service::RtmpPlaybackProviderService>,
    live_proxy_playback_provider_service:
        Arc<synctv_core::service::LiveProxyPlaybackProviderService>,
    provider_common_api: Arc<crate::impls::ProviderCommonApiImpl>,
    bilibili_api: Arc<crate::impls::BilibiliApiImpl>,
    alist_api: Arc<crate::impls::AlistApiImpl>,
    emby_api: Arc<crate::impls::EmbyApiImpl>,
    proxy_slice_cache: Arc<synctv_proxy::slice_cache::SliceCache>,
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
    proxy_http_client: reqwest::Client,
    builtin_stun_url: Option<String>,
    webrtc_status: synctv_core::service::WebRtcRuntimeStatus,
    audit_service: Arc<synctv_core::service::AuditService>,
}

fn cluster_node_id(event_service: &Arc<dyn RealtimeEventService>) -> String {
    event_service.node_id().to_string()
}

fn build_fallback_http_app_state(
    deps: FallbackHttpAppStateDeps,
) -> anyhow::Result<Arc<crate::http::AppState>> {
    Ok(Arc::new(crate::http::create_app_state_from_options(
        crate::http::RouterOptions {
            runtime_settings: deps.runtime_settings,
            user_service: deps.user_service,
            read_pool: deps.read_pool,
            user_cache: deps.user_cache,
            room_service: deps.room_service,
            content_filter: deps.content_filter,
            provider_access_service: deps.provider_access_service,
            event_service: deps.event_service,
            connection_manager: deps.connection_service,
            presence_service: deps.presence_service,
            jwt_service: deps.jwt_service,
            jwt_validator: deps.jwt_validator,
            security_pipeline: deps.security_pipeline,
            public_id_codec: deps.public_id_codec,
            request_executor: deps.request_executor,
            metrics_access_controller: deps.metrics_access_controller,
            client_api: deps.client_api,
            admin_api: deps.admin_api,
            email_api: deps.email_api,
            notification_api: deps.notification_api,
            oauth2_api: deps.oauth2_api,
            realtime_fanout_service: deps.realtime_fanout_service,
            oauth2_service: deps.oauth2_service,
            passkey_service: deps.passkey_service,
            settings_service: Some(deps.settings_service),
            runtime_settings_store: deps.runtime_settings_store,
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
            shared_provider_stores: deps.provider_stores,
            playback_transport_services: deps.playback_transport_services,
            alist_playback_provider_service: deps.alist_playback_provider_service,
            bilibili_playback_provider_service: deps.bilibili_playback_provider_service,
            direct_url_playback_provider_service: deps.direct_url_playback_provider_service,
            emby_playback_provider_service: deps.emby_playback_provider_service,
            rtmp_playback_provider_service: deps.rtmp_playback_provider_service,
            live_proxy_playback_provider_service: deps.live_proxy_playback_provider_service,
            provider_common_api: deps.provider_common_api,
            bilibili_api: deps.bilibili_api,
            alist_api: deps.alist_api,
            emby_api: deps.emby_api,
            shared_proxy_signing_key: deps.proxy_signing_key,
            builtin_stun_url: deps.builtin_stun_url,
            webrtc_status: deps.webrtc_status,
            credential_encryption: deps.credential_encryption,
            proxy_slice_cache: deps.proxy_slice_cache,
            ssrf_guard: deps.ssrf_guard,
            proxy_http_client: deps.proxy_http_client,
            cluster_client: None,
            messaging_rate_limit_config: deps.messaging_rate_limit_config,
            heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
            providers_manager: deps.providers_manager,
            playback_duration_probe: None,
        },
    )?))
}

async fn build_axum_router_with_health(
    grpc_options: GrpcServerOptions<'_>,
) -> anyhow::Result<BuiltGrpcRouter> {
    let GrpcServerOptions {
        runtime_settings,
        jwt_service,
        user_service,
        read_pool,
        user_cache,
        room_service,
        event_service,
        realtime_fanout_service,
        rate_limiter,
        rate_limit_config,
        content_filter,
        connection_service,
        presence_service,
        jwt_validator,
        security_pipeline,
        public_id_codec,
        request_executor,
        metrics_access_controller,
        client_api,
        admin_api,
        email_api,
        notification_api,
        oauth2_api,
        providers_manager,
        provider_access_service,
        settings_service,
        runtime_settings_store,
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
        proxy_signing_key,
        provider_stores,
        playback_transport_services,
        alist_playback_provider_service,
        bilibili_playback_provider_service,
        direct_url_playback_provider_service,
        emby_playback_provider_service,
        rtmp_playback_provider_service,
        live_proxy_playback_provider_service,
        provider_common_api,
        bilibili_api,
        alist_api,
        emby_api,
        proxy_slice_cache,
        ssrf_guard,
        proxy_http_client,
        shared_http_app_state,
        shutdown_rx,
        builtin_stun_url,
        webrtc_status,
        credential_encryption,
        grpc_listener: _,
    } = grpc_options;
    validate_cluster_grpc_runtime_requirements(runtime_settings, node_registry.is_some())?;

    let shared_http_app_state = if let Some(state) = shared_http_app_state {
        state
    } else {
        let fallback_providers_manager = providers_manager
            .clone()
            .ok_or_else(|| anyhow::anyhow!("gRPC fallback HTTP state requires ProvidersManager"))?;
        build_fallback_http_app_state(FallbackHttpAppStateDeps {
            user_service: user_service.clone(),
            read_pool: read_pool.clone(),
            user_cache: user_cache.clone(),
            room_service: room_service.clone(),
            event_service: event_service.clone(),
            connection_service: connection_service.clone(),
            presence_service: presence_service.clone(),
            runtime_settings: Arc::new(runtime_settings.clone()),
            content_filter: content_filter.clone(),
            publish_key_service: publish_key_service.clone(),
            jwt_service: jwt_service.clone(),
            jwt_validator: jwt_validator.clone(),
            security_pipeline: security_pipeline.clone(),
            public_id_codec: public_id_codec.clone(),
            request_executor: request_executor.clone(),
            metrics_access_controller: metrics_access_controller.clone(),
            client_api: client_api.clone(),
            admin_api: admin_api.clone(),
            email_api: email_api.clone(),
            notification_api: notification_api.clone(),
            oauth2_api: oauth2_api.clone(),
            live_streaming_infrastructure: live_streaming_infrastructure.clone(),
            providers_manager: fallback_providers_manager,
            notification_service: notification_service.clone(),
            chat_service: chat_service.clone(),
            oauth2_service: oauth2_service.clone(),
            passkey_service: passkey_service.clone(),
            settings_service: settings_service.clone(),
            runtime_settings_store: runtime_settings_store.clone(),
            email_service: email_service.clone(),
            email_token_service: email_token_service.clone(),
            ws_ticket_service: ws_ticket_service.clone(),
            realtime_fanout_service: realtime_fanout_service.clone(),
            redis_runtime: redis_runtime.clone(),
            rate_limiter: rate_limiter.clone(),
            messaging_rate_limit_config: rate_limit_config.clone(),
            credential_encryption: credential_encryption.clone(),
            provider_access_service: provider_access_service.clone(),
            proxy_signing_key: proxy_signing_key.clone(),
            provider_stores: provider_stores.clone(),
            playback_transport_services: playback_transport_services.clone(),
            alist_playback_provider_service: alist_playback_provider_service.clone(),
            bilibili_playback_provider_service: bilibili_playback_provider_service.clone(),
            direct_url_playback_provider_service: direct_url_playback_provider_service.clone(),
            emby_playback_provider_service: emby_playback_provider_service.clone(),
            rtmp_playback_provider_service: rtmp_playback_provider_service.clone(),
            live_proxy_playback_provider_service: live_proxy_playback_provider_service.clone(),
            provider_common_api: provider_common_api.clone(),
            bilibili_api: bilibili_api.clone(),
            alist_api: alist_api.clone(),
            emby_api: emby_api.clone(),
            proxy_slice_cache: proxy_slice_cache.clone(),
            ssrf_guard: ssrf_guard.clone(),
            proxy_http_client: proxy_http_client.clone(),
            builtin_stun_url,
            webrtc_status,
            audit_service: audit_service.clone(),
        })?
    };

    tracing::info!(
        "Building gRPC router for {}",
        runtime_settings.api_address()
    );

    let user_service_clone = user_service.as_ref().clone();
    let room_service_clone = room_service.as_ref().clone();

    // Resolve node identity from the injected realtime event service.
    let cluster_node_id = cluster_node_id(&event_service);

    let email_service_registered =
        should_register_email_service(email_service.is_some(), email_token_service.is_some());
    let shared_api_runtime = shared_http_app_state.shared_api_runtime.clone();
    let playback_provider_state = Arc::new(playback_provider::PlaybackProviderGrpcState {
        shared_api_runtime: shared_api_runtime.clone(),
        runtime_settings: shared_http_app_state.runtime_settings.clone(),
        connection_manager: shared_http_app_state.connection_manager.clone(),
        runtime_settings_store: shared_http_app_state.runtime_settings_store.clone(),
        live_streaming_infrastructure: shared_http_app_state.live_streaming_infrastructure.clone(),
        proxy_slice_cache: shared_http_app_state.proxy_slice_cache.clone(),
        ssrf_guard: shared_http_app_state.ssrf_guard.clone(),
        proxy_http_client: shared_http_app_state.proxy_http_client.clone(),
    });
    let client_api = shared_api_runtime.client_api.clone();
    let email_api = shared_api_runtime.email_api.clone();

    let chat_service = chat_service.ok_or_else(|| {
        anyhow::anyhow!(
            "chat_service is required for gRPC ClientService but was not provided. \
             Ensure chat_service is initialized before starting the gRPC server."
        )
    })?;
    let client_service = ClientServiceImpl::new(ClientServiceOptions {
        user_service: user_service_clone,
        room_service: room_service_clone,
        chat_service,
        event_service: event_service.clone(),
        rate_limiter,
        rate_limit_config: rate_limit_config.clone(),
        content_filter,
        connection_service: connection_service.clone(),
        presence_service: presence_service.clone(),
        email_api,
        runtime_settings: Arc::new(runtime_settings.clone()),
        client_api: client_api.clone(),
        notification_service: notification_service.clone(),
        heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
    });

    let admin_api = shared_api_runtime
        .admin_api
        .clone()
        .ok_or_else(|| anyhow::anyhow!("gRPC API runtime is missing admin API wiring"))?;

    let admin_service = AdminServiceImpl::new(
        admin_api.clone(),
        Arc::new(runtime_settings.clone()),
        shared_api_runtime.slice_cache_management_runtime.clone(),
    );

    let grpc_unary_request_timeout = grpc_unary_request_timeout();
    tracing::info!(
        grpc_unary_request_timeout_secs = grpc_unary_request_timeout.as_secs(),
        "gRPC impl-level unary request timeout configured"
    );

    // Get the configured max message size (prevents OOM from oversized messages)
    let max_message_size = runtime_settings.server.grpc_max_message_size_bytes;
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
        should_register_cluster_grpc_service(runtime_settings, node_registry.is_some());
    let grpc_registration_plan = grpc_service_registration_plan(
        runtime_settings,
        GrpcOptionalRegistrations {
            email_registered: email_service_registered,
            notification_registered: notification_service_registered,
            oauth2_registered: oauth2_service_registered,
            provider_services_registered,
            cluster_service_registered,
            server_state_registered: should_register_server_state_service(runtime_settings),
            realtime_presence_registered: should_register_realtime_presence_service(
                runtime_settings,
            ),
            proxy_slice_cache_registered: should_register_proxy_slice_cache_service(
                runtime_settings,
            ),
            livestream_relay_registered: should_register_livestream_relay_service(
                runtime_settings,
                live_streaming_infrastructure.is_some(),
            ),
        },
    );

    let mut routes = tonic::service::Routes::builder();
    if grpc_registration_plan.health_state.auth_registered {
        routes.add_service(
            AuthServiceServer::new(client_service).with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
    }

    if grpc_registration_plan.health_state.user_registered {
        routes.add_service(
            UserServiceServer::new(client_service_clone1).with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
    }

    if grpc_registration_plan.health_state.room_registered {
        routes.add_service(
            RoomServiceServer::new(client_service_clone2).with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
    }

    if grpc_registration_plan.health_state.public_registered {
        routes.add_service(
            PublicServiceServer::new(client_service_clone3).with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
    }

    if grpc_registration_plan.health_state.admin_registered {
        routes.add_service(
            AdminServiceServer::new(admin_service).with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
    }

    if grpc_registration_plan.health_state.email_registered {
        routes.add_service(
            EmailServiceServer::new(client_service_clone4).with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
    }

    // Register NotificationService if notification_service is configured
    if grpc_registration_plan.health_state.notification_registered {
        let notif_svc = notification_service.ok_or_else(|| {
            anyhow::anyhow!("NotificationService gRPC registration requires notification_service")
        })?;
        let notification_api = shared_api_runtime.notification_api.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "NotificationService gRPC registration requires shared notification API wiring"
            )
        })?;
        let notif_impl = NotificationServiceImpl::new(
            notification_api,
            shared_api_runtime.request_executor.clone(),
            Arc::new(runtime_settings.clone()),
        );
        routes.add_service(
            NotificationServiceServer::new(notif_impl).with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
        tracing::info!("NotificationService gRPC registered");

        // Spawn a background task that bridges notification creation events to
        // the realtime event system, enabling real-time WebSocket push for
        // persistent user notifications. Without this, clients must poll.
        // The task listens for the server shutdown signal so it does not leak
        // when the gRPC server stops.
        {
            let event_service = Arc::clone(&event_service);
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
                            if rx.changed().await.is_err() {
                                tracing::debug!(
                                    "Notification-to-realtime bridge shutdown signal channel closed"
                                );
                            }
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
                                        notification_type: event.notification.notification_type,
                                        data: event.notification.data,
                                        timestamp: synctv_core::SystemClock.now(),
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
        let _oauth2_service = oauth2_service.ok_or_else(|| {
            anyhow::anyhow!("OAuth2Service gRPC registration requires oauth2_service")
        })?;
        let oauth2_api = shared_api_runtime.oauth2_api.clone().ok_or_else(|| {
            anyhow::anyhow!("OAuth2Service gRPC registration requires shared OAuth2 API wiring")
        })?;
        let oauth2_impl = oauth2_service::OAuth2GrpcService::new(
            oauth2_api,
            Arc::new(runtime_settings.clone()),
            shared_api_runtime.request_executor.clone(),
        );
        // Public endpoints are unauthenticated; private endpoints invoke the
        // shared impl-level auth pipeline inline.
        routes.add_service(
            OAuth2ServiceServer::new(oauth2_impl).with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
        tracing::info!("OAuth2Service gRPC registered (public + authenticated split)");
    }

    // Register provider gRPC services
    if grpc_registration_plan
        .health_state
        .provider_services_registered
    {
        if providers_manager.is_none() {
            return Err(anyhow::anyhow!(
                "provider gRPC registration requires providers_manager"
            ));
        }
        tracing::info!("Registering provider gRPC services");

        let shared_api_runtime = shared_api_runtime.clone();

        // Register provider gRPC services. Auth, blacklist, rate limiting, and
        // timeouts are enforced explicitly inside the shared impl layer.
        routes.add_service(
            ProviderCommonServiceServer::new(providers::common::ProviderCommonGrpcService::new(
                &shared_api_runtime,
                Arc::new(runtime_settings.clone()),
            ))
            .with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
        routes.add_service(
            AlistProviderServiceServer::new(providers::alist::AlistProviderGrpcService::new(
                &shared_api_runtime,
                shared_api_runtime.request_executor.clone(),
                Arc::new(runtime_settings.clone()),
            ))
            .with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
        routes.add_service(
            BilibiliProviderServiceServer::new(
                providers::bilibili::BilibiliProviderGrpcService::new(
                    &shared_api_runtime,
                    shared_api_runtime.request_executor.clone(),
                    Arc::new(runtime_settings.clone()),
                ),
            )
            .with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
        routes.add_service(
            EmbyProviderServiceServer::new(providers::emby::EmbyProviderGrpcService::new(
                &shared_api_runtime,
                shared_api_runtime.request_executor.clone(),
                Arc::new(runtime_settings.clone()),
            ))
            .with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
        routes.add_service(
            RtmpProviderServiceServer::new(providers::rtmp::RtmpProviderGrpcService::new(
                &shared_api_runtime,
                shared_api_runtime.request_executor.clone(),
                Arc::new(runtime_settings.clone()),
            ))
            .with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
        routes.add_service(
            DirectUrlPlaybackProviderServiceServer::new(
                playback_provider::direct_url::DirectUrlPlaybackProviderGrpcService::new(
                    playback_provider_state.clone(),
                    Arc::new(runtime_settings.clone()),
                ),
            )
            .with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
        routes.add_service(
            AlistPlaybackProviderServiceServer::new(
                playback_provider::alist::AlistPlaybackProviderGrpcService::new(
                    playback_provider_state.clone(),
                    Arc::new(runtime_settings.clone()),
                ),
            )
            .with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
        routes.add_service(
            EmbyPlaybackProviderServiceServer::new(
                playback_provider::emby::EmbyPlaybackProviderGrpcService::new(
                    playback_provider_state.clone(),
                    Arc::new(runtime_settings.clone()),
                ),
            )
            .with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
        routes.add_service(
            BilibiliPlaybackProviderServiceServer::new(
                playback_provider::bilibili::BilibiliPlaybackProviderGrpcService::new(
                    playback_provider_state.clone(),
                    Arc::new(runtime_settings.clone()),
                ),
            )
            .with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
        routes.add_service(
            RtmpPlaybackProviderServiceServer::new(
                playback_provider::rtmp::RtmpPlaybackProviderGrpcService::new(
                    playback_provider_state.clone(),
                    Arc::new(runtime_settings.clone()),
                ),
            )
            .with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
        routes.add_service(
            LiveProxyPlaybackProviderServiceServer::new(
                playback_provider::live_proxy::LiveProxyPlaybackProviderGrpcService::new(
                    playback_provider_state.clone(),
                    Arc::new(runtime_settings.clone()),
                ),
            )
            .with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
    }

    // Register cluster gRPC service only in distributed mode.
    if !grpc_registration_plan
        .health_state
        .cluster_service_registered
    {
        if runtime_settings.cluster_runtime_enabled() {
            tracing::info!("Cluster gRPC service hidden by gRPC exposure profile");
        } else {
            tracing::info!("Cluster mode disabled — cluster gRPC service will not be registered");
        }
    } else if !runtime_settings.cluster_runtime_enabled() {
        tracing::info!("Cluster mode disabled — cluster gRPC service will not be registered");
    } else if runtime_settings.cluster.secret.is_empty() {
        tracing::error!(
            "cluster.secret is empty — cluster gRPC service will NOT be registered. \
             Cluster coordination will be disabled. Set cluster.secret or SYNCTV_CLUSTER_SECRET to enable."
        );
    } else if should_register_cluster_grpc_service(runtime_settings, node_registry.is_some()) {
        let nr = node_registry
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("cluster gRPC registration requires node_registry"))?;
        let cluster_server = synctv_cluster::grpc::ClusterServer::from_runtime(nr.clone())
            .with_cluster_secret(runtime_settings.cluster.secret.clone());
        routes.add_service(
            synctv_cluster::grpc::ClusterServiceServer::new(cluster_server)
                .with_transport_settings(
                    max_message_size,
                    runtime_settings.server.grpc_compression_enabled,
                ),
        );
        tracing::info!("Cluster node-discovery gRPC service registered with shared-secret auth");
    } else {
        anyhow::bail!("cluster gRPC registration requirements were not satisfied");
    }

    if grpc_registration_plan.health_state.server_state_registered {
        let service = crate::status::ServerStateGrpcService::new(
            shared_http_app_state
                .shared_api_runtime
                .server_state_runtime
                .clone(),
            runtime_settings.cluster.secret.clone(),
        );
        routes.add_service(
            synctv_cluster::grpc::ServerStateServiceServer::new(service).with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
        );
        tracing::info!("Server-state gRPC service registered with shared-secret auth");
    }

    if grpc_registration_plan
        .health_state
        .realtime_presence_registered
    {
        let service = synctv_realtime::grpc::RealtimePresenceServiceImpl::new(
            connection_service.clone(),
            cluster_node_id.clone(),
        )
        .with_cluster_secret(runtime_settings.cluster.secret.clone());
        routes.add_service(
            RealtimePresenceServiceServer::new(service).with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
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
        .with_cluster_secret(runtime_settings.cluster.secret.clone());
        routes.add_service(
            synctv_proxy::grpc::ProxySliceCacheServiceServer::new(service).with_transport_settings(
                max_message_size,
                runtime_settings.server.grpc_compression_enabled,
            ),
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
        let relay_service = live_infra.relay_service(
            cluster_node_id.clone(),
            runtime_settings.cluster.secret.clone(),
            tokio_util::sync::CancellationToken::new(),
        );

        let relay_interceptor =
            ClusterAuthInterceptor::new(runtime_settings.cluster.secret.clone());
        routes.add_service(tonic::codegen::InterceptedService::new(
            synctv_livestream::StreamRelayServiceServer::new(relay_service)
                .with_transport_settings(
                    max_message_size,
                    runtime_settings.server.grpc_compression_enabled,
                ),
            move |req| relay_interceptor.validate(req),
        ));
        tracing::info!("Livestream relay gRPC service registered with shared-secret auth");
    } else if runtime_settings.cluster_runtime_enabled() {
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
            .register_encoded_file_descriptor_set(
                synctv_proto::PLAYBACK_PROVIDER_FILE_DESCRIPTOR_SET,
            )
            .build_v1()
            .map_err(|e| anyhow::anyhow!("Failed to build gRPC reflection service: {e}"))?;
        routes.add_service(reflection_service);
        tracing::info!("gRPC reflection service registered");
    }

    let router = routes
        .routes()
        .into_axum_router()
        .layer(axum::middleware::from_fn(grpc_transport_only_middleware));

    Ok(BuiltGrpcRouter {
        router,
        health_reporter,
        health_state: grpc_registration_plan.health_state,
    })
}

pub async fn build_axum_router(
    grpc_options: GrpcServerOptions<'_>,
) -> anyhow::Result<axum::Router> {
    Ok(build_axum_router_with_health(grpc_options).await?.router)
}

async fn wait_for_grpc_shutdown(
    mut shutdown_rx: Option<tokio::sync::watch::Receiver<bool>>,
    health_reporter: tonic_health::server::HealthReporter,
    health_state: GrpcHealthRegistrationState,
) {
    if let Some(rx) = shutdown_rx.as_mut() {
        if rx.changed().await.is_err() {
            tracing::debug!("gRPC shutdown signal channel closed");
        }
    } else if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::warn!(
            error = %error,
            "Failed to listen for Ctrl-C shutdown signal"
        );
    }
    set_registered_grpc_services_not_serving(&health_reporter, health_state).await;
}

/// Build and start the gRPC server
pub async fn serve(mut grpc_options: GrpcServerOptions<'_>) -> anyhow::Result<()> {
    let shutdown_rx = grpc_options.shutdown_rx.clone();
    let grpc_listener = grpc_options.grpc_listener.take();
    let addr = if grpc_listener.is_some() {
        None
    } else {
        Some(
            grpc_options
                .runtime_settings
                .api_address()
                .parse::<std::net::SocketAddr>()?,
        )
    };
    let built = build_axum_router_with_health(grpc_options).await?;

    if let Some(listener) = grpc_listener {
        let shutdown = wait_for_grpc_shutdown(
            shutdown_rx,
            built.health_reporter.clone(),
            built.health_state,
        );
        axum::serve(
            listener,
            built
                .router
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|e| anyhow::anyhow!("gRPC server error: {e}"))?;
    } else {
        let addr = addr.expect("gRPC bind address is parsed when no listener is supplied");
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind API address {addr}: {e}"))?;
        let shutdown = wait_for_grpc_shutdown(
            shutdown_rx,
            built.health_reporter.clone(),
            built.health_state,
        );
        axum::serve(
            listener,
            built
                .router
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown)
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
        set_registered_grpc_services_not_serving, set_registered_grpc_services_serving,
        should_register_cluster_grpc_service, should_register_email_service,
        should_register_livestream_relay_service, validate_cluster_grpc_runtime_requirements,
        wait_for_grpc_shutdown, FallbackHttpAppStateDeps, GrpcHealthRegistrationState,
    };
    use crate::grpc::{ClientServiceImpl, ClientServiceOptions};
    use crate::impls::{
        client::RoomActor, AdminApiImpl, AdminApiOptions, AdminApiRuntime, AlistApiImpl,
        BilibiliApiImpl, ClientApiImpl, ClientApiOptions, ClientApiRuntime,
        ClientApiRuntimeServices, EmbyApiImpl, ProviderApiRuntime, ProviderCommonApiImpl,
        ProviderCommonApiRuntime, RequestExecutor,
    };
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;
    use synctv_core::cache::UsernameCache;
    use synctv_core::models::{SignupMethod, User, UserId, UserRole, UserStatus};
    use synctv_core::repository::{
        ChatRepository, RoomMemberRepository, RoomRepository, RoomSettingsRepository,
        SettingsRepository, UserProviderCredentialRepository, UserRepository,
    };
    use synctv_core::service::{
        AuditService, BruteForceProtection, ContentFilter, InMemoryTokenBlacklistStore, JwtService,
        JwtValidator, NotificationService, PermissionService, RateLimitConfig, RateLimiter,
        RemoteProviderManager, RequestRateLimiterService, RoomService, RoomSettingsService,
        RuntimeEmailConfigProvider, RuntimeSettingsStore, SettingsService, UserService,
        UserServiceDependencies, UserServiceRuntimeOptions,
    };
    use synctv_core_testing::{
        create_test_brute_force_protection_service, create_test_token_blacklist_store_service,
    };
    use synctv_proto::client::room_service_server::RoomService as GrpcRoomService;
    use synctv_realtime::fanout::{
        RealtimeDeliveryOutcome, RealtimeDeliveryRequirement, RealtimeEventService, RealtimeMetrics,
    };
    use synctv_realtime::sync::ConnectionRuntime;
    use synctv_realtime::sync::{ConnectionLimits, ConnectionManager, RealtimeManager};
    use tokio_stream::StreamExt;
    use tonic::Request;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn make_chat_watch_user(username: &str) -> User {
        let now = synctv_core::SystemClock.now();
        User {
            id: UserId::new(),
            username: username.to_string(),
            role: UserRole::User,
            avatar_file_reference_id: None,
            status: UserStatus::Active,
            signup_method: SignupMethod::Email,
            created_at: now,
            updated_at: now,
            version: 0,
            deleted_at: None,
            is_banned: false,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
        }
    }

    fn make_chat_watch_user_service(pool: &sqlx::PgPool) -> TestResult<UserService> {
        let jwt_service = JwtService::new("test-secret-key-for-grpc-chat-watch-minimum-32-chars")?;
        let username_cache =
            UsernameCache::local_only("test:grpc-chat:username:".to_string(), 128, 60);
        let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400));

        Ok(UserService::new_for_tests(
            pool,
            jwt_service,
            username_cache,
            token_blacklist,
            synctv_core::cache::KeyBuilder::new("test:grpc-chat"),
            BruteForceProtection::in_memory("test:grpc-chat:auth".to_string()),
        ))
    }

    fn make_chat_watch_chat_service(
        pool: &sqlx::PgPool,
        user_service: Arc<UserService>,
        audit_service: Option<Arc<AuditService>>,
    ) -> TestResult<Arc<synctv_core::service::ChatService>> {
        let room_settings_repo = RoomSettingsRepository::new(pool.clone());
        let permission_service = PermissionService::new_with_runtime(
            RoomMemberRepository::new(pool.clone()),
            RoomRepository::new(pool.clone()),
            synctv_core::service::PermissionServiceRuntime {
                cache_size: 1000,
                cache_ttl_secs: 300,
                room_settings_repo: Some(room_settings_repo.clone()),
                ..synctv_core::service::PermissionServiceRuntime::local_only()
            },
        )
        .map_err(|error| test_error(error.to_string()))?;

        Ok(Arc::new(synctv_core::service::ChatService::new(
            Arc::new(ChatRepository::new(pool.clone())),
            synctv_core::service::ChatRuntime {
                clock: Arc::new(synctv_core::SystemClock),
                rate_limiter: Arc::new(RateLimiter::local_only("test:grpc-chat:".to_string()))
                    as Arc<dyn RequestRateLimiterService>,
                rate_limit_config: RateLimitConfig::default(),
                content_filter: ContentFilter::new(),
            },
            synctv_core::service::ChatDependencies {
                permission_service,
                room_settings_service: RoomSettingsService::new(
                    room_settings_repo,
                    None,
                    Arc::new(NotificationService::default()),
                    None,
                    None,
                ),
                user_service,
                file_storage_service: Arc::new(synctv_core::service::DisabledFileStorageService),
                audit_service,
                notification_service: NotificationService::default(),
                runtime_settings_store: None,
            },
        )))
    }

    fn make_chat_watch_client_api(
        user_service: Arc<UserService>,
        room_service: Arc<RoomService>,
        chat_service: Arc<synctv_core::service::ChatService>,
        event_service: Arc<dyn RealtimeEventService>,
    ) -> TestResult<ClientApiImpl> {
        let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        let security_pipeline = synctv_core::service::SecurityPipeline::new_with_runtime(
            user_service.clone(),
            synctv_core::service::SecurityPipelineRuntime {
                user_cache: None,
                token_blacklist: user_service.token_blacklist_store(),
                key_builder: user_service.key_builder().clone(),
            },
        );
        let request_executor = Arc::new(RequestExecutor::new(
            Arc::new(crate::ApiRuntimeSettings::default()),
            Arc::new(JwtValidator::new(Arc::new(JwtService::new(
                "test-secret-key-for-grpc-chat-watch-minimum-32-chars",
            )?))),
            Arc::new(security_pipeline),
            Arc::new(RateLimiter::local_only("test:grpc-chat:".to_string())),
        ));
        Ok(ClientApiImpl::new_with_runtime(
            crate::impls::ClientApiOptions {
                read_pool: None,
                user_service,
                room_service,
                connection_service: connection_manager,
                runtime_settings: Arc::new(crate::ApiRuntimeSettings::default()),
                publish_key_service: None,
                jwt_service: JwtService::new(
                    "test-secret-key-for-grpc-chat-watch-minimum-32-chars",
                )?,
                live_streaming_infrastructure: None,
                runtime_settings_store: None,
                public_id_codec: Arc::new(synctv_adapter::PublicIdCodec::plain()),
                chat_service: Some(chat_service),
                provider_stores: Arc::new(
                    synctv_core::provider::ProviderStoreRegistry::local_only("test:provider:"),
                ),
                email_api: None,
                passkey_service: None,
            },
            crate::impls::ClientApiRuntime {
                realtime_event_service: event_service.clone(),
                chat_event_dispatcher: crate::chat_event_dispatcher::default_chat_event_dispatcher(
                    event_service,
                ),
                jwt_validator: request_executor.jwt_validator().clone(),
                request_executor,
                ..crate::test_support::client_api_runtime()
            },
        ))
    }

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
        _postgres: synctv_core_testing::TestContainer,
        runtime_settings: Arc<crate::ApiRuntimeSettings>,
        jwt_service: JwtService,
        user_service: Arc<UserService>,
        room_service: Arc<RoomService>,
        settings_service: Arc<SettingsService>,
        runtime_settings_store: Arc<RuntimeSettingsStore>,
        email_service: Arc<synctv_core::service::EmailService>,
        provider_instance_manager: Arc<RemoteProviderManager>,
        credential_repo: Arc<UserProviderCredentialRepository>,
        audit_service: Arc<AuditService>,
    }

    async fn fallback_grpc_test_context() -> TestResult<FallbackGrpcTestContext> {
        let (postgres, pool) = synctv_core_testing::create_test_pool().await;
        let config = Arc::new(crate::ApiRuntimeSettings::default());
        let jwt_service =
            JwtService::new("test-secret-key-for-grpc-router-tests-minimum-32-chars")?;
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 128, 60);
        let user_service = Arc::new(UserService::new_with_brute_force_service_and_runtime(
            &pool,
            UserServiceDependencies {
                jwt_service: jwt_service.clone(),
                username_cache,
                token_blacklist: create_test_token_blacklist_store_service(),
                key_builder: synctv_core::cache::KeyBuilder::new("test"),
                brute_force: create_test_brute_force_protection_service(),
                password_complexity: synctv_core::validation::PasswordComplexityOptions::default(),
            },
            UserServiceRuntimeOptions::test_defaults(),
        ));
        let room_service = Arc::new(
            RoomService::new_for_tests(pool.clone(), (*user_service).clone())
                .map_err(|error| test_error(error.to_string()))?,
        );
        let settings_service = Arc::new(SettingsService::new(
            SettingsRepository::new(pool.clone()),
            pool.clone(),
        ));
        let runtime_settings_store = Arc::new(RuntimeSettingsStore::new(settings_service.clone()));
        let email_service = Arc::new(
            synctv_core::service::EmailService::new(Arc::new(RuntimeEmailConfigProvider::new(
                &runtime_settings_store,
            )))
            .map_err(|error| test_error(error.to_string()))?,
        );
        let provider_instance_manager =
            synctv_core_testing::create_empty_provider_instance_manager();
        let credential_repo = Arc::new(UserProviderCredentialRepository::new(pool.clone()));
        let audit_service = AuditService::new_unbuffered(pool.clone());

        Ok(FallbackGrpcTestContext {
            _postgres: postgres,
            runtime_settings: config,
            jwt_service,
            user_service,
            room_service,
            settings_service,
            runtime_settings_store,
            email_service,
            provider_instance_manager,
            credential_repo,
            audit_service: Arc::new(audit_service),
        })
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
            Ok(response) => ServingStatus::try_from(response.into_inner().status)
                .map_err(|_| tonic::Code::Internal),
            Err(status) => Err(status.code()),
        }
    }

    fn request_with_peer_and_headers(
        peer: SocketAddr,
        headers: &[(&str, &str)],
    ) -> TestResult<tonic::Request<()>> {
        let mut request = tonic::Request::new(());
        request
            .extensions_mut()
            .insert(tonic::transport::server::TcpConnectInfo {
                local_addr: None,
                remote_addr: Some(peer),
            });
        for (key, value) in headers {
            request.metadata_mut().insert(
                MetadataKey::from_bytes(key.as_bytes())?,
                MetadataValue::try_from(*value)?,
            );
        }
        Ok(request)
    }

    #[test]
    fn test_request_targets_grpc_transport_requires_grpc_content_type() -> TestResult {
        let mut headers = axum::http::HeaderMap::new();
        assert!(
            !super::request_targets_grpc_transport(&headers)?,
            "requests without Content-Type must not be treated as gRPC"
        );

        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        assert!(
            !super::request_targets_grpc_transport(&headers)?,
            "plain HTTP JSON requests must not be treated as gRPC"
        );

        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/grpc"),
        );
        assert!(
            super::request_targets_grpc_transport(&headers)?,
            "canonical gRPC requests must be routed to tonic"
        );

        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/grpc+proto; charset=utf-8"),
        );
        assert!(
            super::request_targets_grpc_transport(&headers)?,
            "gRPC content-type variants must still be routed to tonic"
        );
        Ok(())
    }

    #[test]
    fn test_request_targets_grpc_transport_rejects_non_ascii_content_type() -> TestResult {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_bytes(b"application/grpc\xff")?,
        );

        assert!(
            super::request_targets_grpc_transport(&headers).is_err(),
            "invalid Content-Type bytes must not be silently treated as a non-gRPC request"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_grpc_transport_gate_returns_not_found_for_non_grpc_requests() -> TestResult {
        let app = axum::Router::new()
            .route("/", axum::routing::get(|| async { "grpc route" }))
            .layer(axum::middleware::from_fn(
                super::grpc_transport_only_middleware,
            ));

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/")
                    .body(axum::body::Body::empty())?,
            )
            .await?;

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
        Ok(())
    }

    #[tokio::test]
    async fn test_grpc_transport_gate_returns_bad_request_for_invalid_content_type() -> TestResult {
        let app = axum::Router::new()
            .route("/", axum::routing::get(|| async { "grpc route" }))
            .layer(axum::middleware::from_fn(
                super::grpc_transport_only_middleware,
            ));

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/")
                    .header(
                        axum::http::header::CONTENT_TYPE,
                        axum::http::HeaderValue::from_bytes(b"application/grpc\xff")?,
                    )
                    .body(axum::body::Body::empty())?,
            )
            .await?;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_build_fallback_http_app_state_reuses_shared_runtime_instances() -> TestResult {
        let context = fallback_grpc_test_context().await?;
        let connection_service: Arc<dyn ConnectionRuntime> =
            Arc::new(synctv_realtime::sync::ConnectionManager::new(
                synctv_realtime::sync::ConnectionLimits::default(),
            ));
        let event_service: Arc<dyn RealtimeEventService> = Arc::new(
            crate::test_support::RecordingRealtimeEventService::with_node(
                "fallback-http-node",
                true,
            ),
        );
        let provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver> =
            Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                context.runtime_settings.redis.key_prefix.clone(),
            ));
        let providers_for_access = synctv_core::provider::ProviderSet::new_with_ssrf_guard(
            context.provider_instance_manager.clone(),
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )?;
        let provider_access_service: Arc<dyn synctv_core::provider::ProviderAccessService> =
            Arc::new(
                synctv_core::provider::CachedProviderAccessService::new(
                    context.credential_repo.clone(),
                    providers_for_access.alist.clone(),
                )
                .with_store(provider_stores.load("credentials")),
            );
        let playback_transport_services =
            Arc::new(synctv_core::provider::PlaybackTransportServices {
                room_service: context.room_service.clone(),
                permission_service: context.room_service.permission_service().clone(),
                credential_encryption: None,
                credential_repo: context.credential_repo.clone(),
                provider_access_service: provider_access_service.clone(),
            });
        let playback_provider_deps = synctv_core::service::PlaybackProviderServiceDeps {
            providers: providers_for_access.clone(),
            provider_stores: provider_stores.clone(),
            playback_transport_services: playback_transport_services.clone(),
            provider_access_service: provider_access_service.clone(),
        };
        let alist_playback_provider_service = Arc::new(
            synctv_core::service::AlistPlaybackProviderService::new(playback_provider_deps.clone()),
        );
        let bilibili_playback_provider_service =
            Arc::new(synctv_core::service::BilibiliPlaybackProviderService::new(
                playback_provider_deps.clone(),
            ));
        let direct_url_playback_provider_service =
            Arc::new(synctv_core::service::DirectUrlPlaybackProviderService::new(
                playback_provider_deps.clone(),
            ));
        let emby_playback_provider_service = Arc::new(
            synctv_core::service::EmbyPlaybackProviderService::new(playback_provider_deps.clone()),
        );
        let rtmp_playback_provider_service = Arc::new(
            synctv_core::service::RtmpPlaybackProviderService::new(playback_provider_deps.clone()),
        );
        let live_proxy_playback_provider_service = Arc::new(
            synctv_core::service::LiveProxyPlaybackProviderService::new(playback_provider_deps),
        );
        let ws_ticket_service: Arc<dyn synctv_core::service::WebSocketTicketService> =
            Arc::new(synctv_core::service::WsTicketService::local_only(None));
        let proxy_signing_key = Arc::new(
            crate::proxy_signature::ProxySigningKey::try_derive_from(
                b"test-secret-key-for-grpc-router-tests-minimum-32-chars",
            )
            .map_err(|error| test_error(error.to_string()))?,
        );
        let content_filter =
            ContentFilter::new_with_config(17, Some(vec!["blocked".to_string()]), false);
        let messaging_rate_limit_config = RateLimitConfig {
            chat_per_second: 23,
            window_seconds: 11,
        };
        let user_cache = Arc::new(synctv_core::cache::UserCache::local_only(
            128,
            60,
            300,
            "test:user:".to_string(),
        ));
        let rate_limiter: Arc<dyn RequestRateLimiterService> =
            Arc::new(RateLimiter::local_only("test:".to_string()));
        let mut fallback_config = context.runtime_settings.as_ref().clone();
        fallback_config.proxy_slice_cache.enabled = false;
        fallback_config.proxy_slice_cache.slice_size_bytes = 4 * 1024 * 1024;
        fallback_config.proxy_slice_cache.max_cache_size_bytes = 1024 * 1024 * 1024;
        fallback_config.proxy_slice_cache.segment_ttl_seconds = 600;
        fallback_config.proxy_slice_cache.stale_max_age_seconds = 120;
        fallback_config.proxy_slice_cache.stale_while_revalidate = false;
        fallback_config.proxy_slice_cache.eviction_interval_seconds = 30;
        fallback_config.proxy_slice_cache.watermark_ratio = 0.75;
        let fallback_config = Arc::new(fallback_config);
        let ssrf_guard = synctv_common::ssrf::SsrfGuard::strict_policy();
        let proxy_http_client = synctv_proxy::build_proxy_http_client(ssrf_guard.clone())
            .map_err(|error| test_error(error.to_string()))?;
        let proxy_slice_cache_config =
            crate::runtime_adapters::proxy_slice_cache_options_from_runtime_settings(
                fallback_config.as_ref(),
            );
        let proxy_slice_cache = Arc::new(
            synctv_proxy::slice_cache::SliceCache::try_new_with_client_and_ssrf_guard(
                proxy_slice_cache_config,
                proxy_http_client.clone(),
                ssrf_guard.clone(),
            )
            .await
            .map_err(|error| test_error(error.to_string()))?,
        );
        let security_pipeline = Arc::new(synctv_core::service::SecurityPipeline::new_with_runtime(
            context.user_service.clone(),
            synctv_core::service::SecurityPipelineRuntime {
                user_cache: Some(user_cache.clone()),
                token_blacklist: context.user_service.token_blacklist_store(),
                key_builder: context.user_service.key_builder().clone(),
            },
        ));
        let jwt_validator = Arc::new(JwtValidator::new(Arc::new(context.jwt_service.clone())));
        let public_id_codec = Arc::new(
            synctv_adapter::PublicIdCodec::from_config(&synctv_adapter::PublicIdConfig::default())
                .map_err(|error| {
                    test_error(format!("invalid fallback public id config: {error}"))
                })?,
        );
        let request_executor = Arc::new(RequestExecutor::new(
            fallback_config.clone(),
            jwt_validator.clone(),
            security_pipeline.clone(),
            rate_limiter.clone(),
        ));
        let providers_manager = Arc::new(synctv_core::service::ProvidersManager::new(
            context.provider_instance_manager.clone(),
        )?);
        let provider_common_api = Arc::new(ProviderCommonApiImpl::new_with_runtime(
            context.provider_instance_manager.clone(),
            context.user_service.clone(),
            context.audit_service.clone(),
            ProviderCommonApiRuntime {
                providers_manager: providers_manager.clone(),
                request_executor: request_executor.clone(),
            },
        ));
        let provider_api_runtime = ProviderApiRuntime {
            access_service: provider_access_service.clone(),
            event_service: event_service.clone(),
        };
        let credential_backed_providers =
            providers_for_access.with_credential_repo(context.credential_repo.clone());
        let bilibili_api = Arc::new(
            BilibiliApiImpl::new_with_runtime(
                credential_backed_providers.bilibili.clone(),
                b"test-secret-key-for-grpc-router-tests-minimum-32-chars",
                provider_api_runtime.clone(),
            )
            .map_err(|error| test_error(error.to_string()))?,
        );
        let alist_api = Arc::new(AlistApiImpl::new_with_runtime(
            credential_backed_providers.alist.clone(),
            provider_api_runtime.clone(),
        ));
        let emby_api = Arc::new(EmbyApiImpl::new_with_runtime(
            credential_backed_providers.emby.clone(),
            provider_api_runtime,
        ));
        let presence_service = Arc::new(synctv_core::service::OnlinePresenceService::local());
        let client_api = Arc::new(ClientApiImpl::new_with_runtime(
            ClientApiOptions {
                user_service: context.user_service.clone(),
                read_pool: None,
                room_service: context.room_service.clone(),
                chat_service: None,
                connection_service: connection_service.clone(),
                runtime_settings: fallback_config.clone(),
                publish_key_service: None,
                jwt_service: context.jwt_service.clone(),
                live_streaming_infrastructure: None,
                runtime_settings_store: Some(context.runtime_settings_store.clone()),
                provider_stores: provider_stores.clone(),
                public_id_codec: public_id_codec.clone(),
                email_api: None,
                passkey_service: None,
            },
            ClientApiRuntime::new_with_services(ClientApiRuntimeServices {
                clock: Arc::new(synctv_core::SystemClock),
                realtime_fanout: crate::realtime_fanout::disabled_realtime_fanout_service(),
                realtime_event_service: event_service.clone(),
                redis_runtime: None,
                builtin_stun_url: None,
                webrtc_status:
                    synctv_core::service::WebRtcRuntimeStatus::peer_to_peer_stun_disabled(),
                provider_access_service: provider_access_service.clone(),
                signing_key: proxy_signing_key.clone(),
                presence_service: presence_service.clone(),
                jwt_validator: jwt_validator.clone(),
                request_executor: request_executor.clone(),
                ws_ticket_service: ws_ticket_service.clone(),
                playback_duration_probe: None,
            }),
        ));
        let admin_api = Arc::new(AdminApiImpl::new_with_runtime(
            AdminApiOptions {
                room_service: context.room_service.clone(),
                user_service: context.user_service.clone(),
                read_services: crate::test_support::admin_read_services(
                    context.user_service.as_ref(),
                ),
                settings_service: context.settings_service.clone(),
                runtime_settings_store: Some(context.runtime_settings_store.clone()),
                email_service: context.email_service.clone(),
                connection_service: connection_service.clone(),
                provider_instance_manager: context.provider_instance_manager.clone(),
                live_streaming_infrastructure: None,
                publish_key_service: None,
                runtime_settings: fallback_config.clone(),
                audit_service: context.audit_service.clone(),
                public_id_codec: public_id_codec.clone(),
            },
            AdminApiRuntime {
                clock: Arc::new(synctv_core::SystemClock),
                realtime_fanout: crate::realtime_fanout::disabled_realtime_fanout_service(),
                realtime_event_service: event_service.clone(),
                provider_stores: provider_stores.clone(),
                provider_access_service: provider_access_service.clone(),
                signing_key: proxy_signing_key.clone(),
                presence_service: presence_service.clone(),
                request_executor: request_executor.clone(),
            },
        ));
        let http_state =
            build_fallback_http_app_state(FallbackHttpAppStateDeps {
                user_service: context.user_service,
                read_pool: None,
                user_cache,
                room_service: context.room_service,
                providers_manager,
                event_service: event_service.clone(),
                connection_service: connection_service.clone(),
                presence_service,
                runtime_settings: fallback_config.clone(),
                content_filter: content_filter.clone(),
                publish_key_service: None,
                jwt_service: context.jwt_service,
                jwt_validator: jwt_validator.clone(),
                security_pipeline: security_pipeline.clone(),
                public_id_codec: public_id_codec.clone(),
                request_executor: request_executor.clone(),
                metrics_access_controller: Arc::new(
                    crate::metrics_auth::MetricsAccessController::new(),
                ),
                client_api: client_api.clone(),
                admin_api: Some(admin_api.clone()),
                email_api: None,
                notification_api: None,
                oauth2_api: None,
                live_streaming_infrastructure: None,
                notification_service: None,
                chat_service: None,
                oauth2_service: None,
                passkey_service: None,
                settings_service: context.settings_service,
                runtime_settings_store: Some(context.runtime_settings_store),
                email_service: Some(context.email_service),
                email_token_service: None,
                ws_ticket_service: ws_ticket_service.clone(),
                realtime_fanout_service: crate::realtime_fanout::disabled_realtime_fanout_service(),
                redis_runtime: None,
                rate_limiter,
                messaging_rate_limit_config: messaging_rate_limit_config.clone(),
                credential_encryption: None,
                provider_access_service: provider_access_service.clone(),
                proxy_signing_key: proxy_signing_key.clone(),
                provider_stores: provider_stores.clone(),
                playback_transport_services: playback_transport_services.clone(),
                alist_playback_provider_service: alist_playback_provider_service.clone(),
                bilibili_playback_provider_service: bilibili_playback_provider_service.clone(),
                direct_url_playback_provider_service: direct_url_playback_provider_service.clone(),
                emby_playback_provider_service: emby_playback_provider_service.clone(),
                rtmp_playback_provider_service: rtmp_playback_provider_service.clone(),
                live_proxy_playback_provider_service: live_proxy_playback_provider_service.clone(),
                provider_common_api: provider_common_api.clone(),
                bilibili_api: bilibili_api.clone(),
                alist_api: alist_api.clone(),
                emby_api: emby_api.clone(),
                proxy_slice_cache: proxy_slice_cache.clone(),
                ssrf_guard: ssrf_guard.clone(),
                proxy_http_client: proxy_http_client.clone(),
                builtin_stun_url: None,
                webrtc_status:
                    synctv_core::service::WebRtcRuntimeStatus::peer_to_peer_stun_disabled(),
                audit_service: context.audit_service,
            })
            .map_err(|error| test_error(format!("{error:?}")))?;
        let admin_api = http_state
            .shared_api_runtime
            .admin_api
            .as_ref()
            .ok_or_else(|| test_error("fallback HTTP state should wire admin API"))?;

        assert!(
            Arc::ptr_eq(
                &http_state.shared_api_runtime.client_api.connection_service,
                &connection_service
            ),
            "standalone gRPC fallback HTTP state must reuse the injected connection service for client APIs"
        );
        assert!(
            Arc::ptr_eq(
                &admin_api.connection_service,
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
            Arc::ptr_eq(&http_state.proxy_slice_cache, &proxy_slice_cache),
            "fallback HTTP state must reuse the injected proxy slice cache"
        );
        assert!(
            http_state
                .proxy_http_client
                .get("https://example.com")
                .build()
                .is_ok(),
            "fallback HTTP state must retain the injected proxy HTTP client"
        );
        assert!(
            Arc::ptr_eq(
                &http_state.shared_api_runtime.provider_access_service,
                &provider_access_service
            ),
            "fallback HTTP state must reuse the injected provider access service"
        );
        assert!(
            Arc::ptr_eq(
                &http_state.shared_api_runtime.public_id_codec,
                &public_id_codec
            ),
            "fallback HTTP state must reuse the injected public ID codec"
        );
        assert!(
            Arc::ptr_eq(
                &http_state.shared_api_runtime.request_executor,
                &request_executor
            ),
            "fallback HTTP state must reuse the injected request executor"
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
                &http_state
                    .shared_api_runtime
                    .client_api
                    .provider_access_service
            ),
            "fallback HTTP state must share provider access cache with client API"
        );
        assert!(
            Arc::ptr_eq(
                &http_state.shared_api_runtime.provider_access_service,
                &http_state
                    .shared_api_runtime
                    .admin_api
                    .as_ref()
                    .ok_or_else(|| test_error("fallback HTTP state should wire admin API"))?
                    .provider_access_service
            ),
            "fallback HTTP state must share provider access cache with admin API"
        );
        assert!(
            Arc::ptr_eq(
                &http_state.shared_api_runtime.provider_access_service,
                &http_state
                    .shared_api_runtime
                    .playback_transport_services
                    .provider_access_service
            ),
            "fallback HTTP state must share provider access cache with proxy services"
        );
        assert!(
            Arc::ptr_eq(&http_state.event_service, &event_service,),
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
                .messaging_rate_limit_config
                .chat_per_second,
            messaging_rate_limit_config.chat_per_second,
            "fallback HTTP state must preserve configured chat rate limits"
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
        Ok(())
    }

    #[tokio::test]
    async fn test_grpc_watch_chat_events_receives_live_send_event() -> TestResult {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let user_service = Arc::new(make_chat_watch_user_service(&pool)?);
        let room_service = Arc::new(
            RoomService::new_for_tests(pool.clone(), (*user_service).clone())
                .map_err(|error| test_error(error.to_string()))?,
        );
        let audit_service = Arc::new(AuditService::new_unbuffered(pool.clone()));
        let chat_service =
            make_chat_watch_chat_service(&pool, user_service.clone(), Some(audit_service))?;
        let event_service: Arc<dyn RealtimeEventService> = Arc::new(
            RealtimeManager::new(synctv_realtime::sync::RealtimeConfig {
                distributed_transport_factory: None,
                message_runtime: Arc::new(synctv_realtime::sync::RoomMessageHub::new()),
                distributed_enabled: false,
                node_id: "grpc-chat-watch".to_string(),
                dedup_window: Duration::from_secs(30),
                critical_channel_capacity: 64,
                publish_channel_capacity: 256,
                key_prefix: "test:grpc-chat:".to_string(),
                catchup_window_secs: 60,
                stream_max_length: 256,
                event_handler: None,
                parent_cancel_token: None,
            })
            .await
            .map_err(|error| test_error(error.to_string()))?,
        );
        let client_api = Arc::new(make_chat_watch_client_api(
            user_service.clone(),
            room_service.clone(),
            chat_service.clone(),
            event_service.clone(),
        )?);
        let client_service = ClientServiceImpl::new(ClientServiceOptions {
            user_service: (*user_service).clone(),
            room_service: (*room_service).clone(),
            chat_service: chat_service.clone(),
            event_service,
            rate_limiter: Arc::new(RateLimiter::local_only("test:grpc-chat:".to_string())),
            rate_limit_config: RateLimitConfig::default(),
            content_filter: ContentFilter::new(),
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            presence_service: Arc::new(synctv_core::service::OnlinePresenceService::local()),
            email_api: None,
            runtime_settings: Arc::new(crate::ApiRuntimeSettings::default()),
            client_api: client_api.clone(),
            notification_service: None,
            heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
        });

        let owner = user_repo
            .create(&make_chat_watch_user("grpc_watch_owner"))
            .await?;
        let token = client_api
            .jwt_service
            .sign_access_token(&owner.id, 0)
            .map_err(|error| test_error(error.to_string()))?;
        let (room, _) = room_service
            .create_room(
                "gRPC Chat Watch Room".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .map_err(|error| test_error(error.to_string()))?;

        let public_room_id = client_api
            .public_id_codec
            .encode_room_id(room.id)
            .map_err(test_error)?;
        let mut request = Request::new(synctv_proto::client::WatchChatEventsRequest::default());
        request.metadata_mut().insert(
            "x-room-id",
            MetadataValue::try_from(public_room_id.as_str())?,
        );
        request.metadata_mut().insert(
            "authorization",
            MetadataValue::try_from(format!("Bearer {token}"))?,
        );

        let response = client_service
            .watch_chat_events(request)
            .await
            .map_err(|status| test_error(format!("{status:?}")))?;
        let mut stream = response.into_inner();
        let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .map_err(|_| test_error("initial observed event timed out"))?
            .ok_or_else(|| test_error("watch stream ended before initial event"))?
            .map_err(|status| test_error(format!("{status:?}")))?;
        assert!(matches!(
            first.event,
            Some(synctv_proto::client::watch_chat_events_event::Event::Observed(_))
        ));

        let sent = client_api
            .send_chat_message_for_actor(
                &RoomActor::User {
                    room_id: room.id,
                    user_id: owner.id,
                },
                synctv_proto::client::SendChatMessageRequest {
                    client_message_id: "grpc-chat-live-send-1".to_string(),
                    content: "grpc live push".to_string(),
                    metadata: None,
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| test_error(format!("{error:?}")))?
            .event
            .ok_or_else(|| test_error("chat send should return event"))?;

        let mut rendered = String::new();
        for _ in 0..8 {
            let item = tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .map_err(|_| test_error("grpc watch event timed out"))?
                .ok_or_else(|| test_error("watch stream ended before changed event"))?
                .map_err(|status| test_error(format!("{status:?}")))?;
            if let Some(event) = item.event.as_ref() {
                match event {
                    synctv_proto::client::watch_chat_events_event::Event::ResourceEvent(
                        changed,
                    ) => {
                        if let Some(synctv_proto::client::resource_event::Payload::ChatEvent(
                            chat,
                        )) = changed.payload.as_ref()
                        {
                            if let Some(message) = chat.message.as_ref() {
                                rendered.push_str(&message.content);
                            }
                        }
                    }
                    synctv_proto::client::watch_chat_events_event::Event::Observed(_) => {}
                    synctv_proto::client::watch_chat_events_event::Event::Error(error) => {
                        return Err(test_error(format!("watch returned error: {error:?}")));
                    }
                }
            }
            if rendered.contains("grpc live push") {
                break;
            }
        }

        assert!(rendered.contains("grpc live push"));
        assert_eq!(sent.event_id.trim(), sent.event_id.as_str());
        Ok(())
    }

    #[test]
    fn test_cluster_node_id_uses_injected_event_service() {
        let event_service: Arc<dyn RealtimeEventService> = Arc::new(
            crate::test_support::RecordingRealtimeEventService::with_node("test-node", true),
        );

        assert_eq!(
            cluster_node_id(&event_service),
            "test-node",
            "gRPC transport must derive cluster node identity from the injected realtime event service"
        );
    }

    #[test]
    fn test_cluster_grpc_service_requires_cluster_mode() {
        let mut runtime_settings = crate::ApiRuntimeSettings::default();
        runtime_settings.cluster.secret = "shared-secret".to_string();

        assert!(
            !should_register_cluster_grpc_service(&runtime_settings, true),
            "cluster.secret alone must not enable cluster gRPC"
        );

        runtime_settings.cluster_enabled = true;
        assert!(
            should_register_cluster_grpc_service(&runtime_settings, true),
            "cluster-enabled deployments with a secret and registry should expose cluster gRPC"
        );
    }

    #[test]
    fn test_cluster_grpc_service_requires_node_registry() {
        let mut runtime_settings = crate::ApiRuntimeSettings::default();
        runtime_settings.cluster_enabled = true;
        runtime_settings.cluster.secret = "shared-secret".to_string();

        assert!(
            !should_register_cluster_grpc_service(&runtime_settings, false),
            "cluster gRPC must not be registered before NodeRegistry is ready"
        );
    }

    #[test]
    fn test_cluster_grpc_runtime_requires_node_registry() {
        let mut runtime_settings = crate::ApiRuntimeSettings::default();
        runtime_settings.cluster_enabled = true;
        runtime_settings.cluster.secret = "shared-secret".to_string();

        let err = validate_cluster_grpc_runtime_requirements(&runtime_settings, false)
            .expect_err("realtime runtime must fail closed without NodeRegistry");

        assert!(
            err.to_string().contains("requires NodeRegistry"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_cluster_grpc_runtime_requires_cluster_secret() {
        let mut config = crate::ApiRuntimeSettings::default();
        config.cluster_enabled = true;

        let err = validate_cluster_grpc_runtime_requirements(&config, true)
            .expect_err("realtime runtime must fail closed without cluster.secret");

        assert!(
            err.to_string().contains("cluster.secret"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_standalone_grpc_runtime_allows_missing_node_registry() -> TestResult {
        let config = crate::ApiRuntimeSettings::default();

        validate_cluster_grpc_runtime_requirements(&config, false)?;
        Ok(())
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
        assert!(!should_register_email_service(true, false));
        assert!(!should_register_email_service(false, true));
        assert!(should_register_email_service(true, true));
    }

    #[test]
    fn test_extract_client_ip_uses_x_forwarded_for_from_trusted_proxy() -> TestResult {
        let mut config = crate::ApiRuntimeSettings::default();
        config.server.trusted_proxies = vec!["127.0.0.1".to_string()];

        let request = request_with_peer_and_headers(
            "127.0.0.1:50051".parse()?,
            &[("x-forwarded-for", "203.0.113.50, 70.41.3.18")],
        )?;

        assert_eq!(
            extract_client_ip(&request, &config).expect("extract client ip"),
            Some("203.0.113.50".parse()?)
        );
        Ok(())
    }

    #[test]
    fn test_extract_client_ip_ignores_headers_from_untrusted_peer() -> TestResult {
        let mut config = crate::ApiRuntimeSettings::default();
        config.server.trusted_proxies = vec!["127.0.0.1".to_string()];

        let request = request_with_peer_and_headers(
            "192.168.1.100:50051".parse()?,
            &[
                ("x-forwarded-for", "203.0.113.50"),
                ("x-real-ip", "198.51.100.42"),
            ],
        )?;

        assert_eq!(
            extract_client_ip(&request, &config).expect("extract client ip"),
            Some("192.168.1.100".parse()?)
        );
        Ok(())
    }

    #[test]
    fn test_extract_client_ip_rejects_invalid_forwarded_for() -> TestResult {
        let mut config = crate::ApiRuntimeSettings::default();
        config.server.trusted_proxies = vec!["10.0.0.0/8".to_string()];

        let request = request_with_peer_and_headers(
            "10.1.2.3:50051".parse()?,
            &[
                ("x-forwarded-for", "not-an-ip"),
                ("x-real-ip", "198.51.100.42"),
            ],
        )?;

        let status =
            extract_client_ip(&request, &config).expect_err("invalid forwarded-for should fail");

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        Ok(())
    }

    #[test]
    fn test_extract_client_ip_uses_x_real_ip_when_forwarded_for_absent() -> TestResult {
        let mut config = crate::ApiRuntimeSettings::default();
        config.server.trusted_proxies = vec!["10.0.0.0/8".to_string()];

        let request = request_with_peer_and_headers(
            "10.1.2.3:50051".parse()?,
            &[("x-real-ip", "198.51.100.42")],
        )?;

        assert_eq!(
            extract_client_ip(&request, &config).expect("extract client ip"),
            Some("198.51.100.42".parse()?)
        );
        Ok(())
    }

    #[test]
    fn test_livestream_relay_service_requires_cluster_mode_secret_and_infra() {
        let mut runtime_settings = crate::ApiRuntimeSettings::default();

        assert!(
            !should_register_livestream_relay_service(&runtime_settings, true),
            "standalone mode must not expose livestream relay gRPC service"
        );

        runtime_settings.cluster_enabled = true;
        assert!(
            !should_register_livestream_relay_service(&runtime_settings, true),
            "distributed mode without a secret must fail closed"
        );

        runtime_settings.cluster.secret = "shared-secret".to_string();
        assert!(
            !should_register_livestream_relay_service(&runtime_settings, false),
            "relay service must not be registered before livestream infra is ready"
        );

        assert!(
            should_register_livestream_relay_service(&runtime_settings, true),
            "distributed mode with secret and livestream infra should register relay service"
        );
    }

    #[test]
    fn test_public_grpc_registration_plan_preserves_optional_service_registration() {
        let config = crate::ApiRuntimeSettings::default();

        let plan = grpc_service_registration_plan(
            &config,
            super::GrpcOptionalRegistrations {
                email_registered: true,
                notification_registered: false,
                oauth2_registered: true,
                provider_services_registered: false,
                cluster_service_registered: true,
                server_state_registered: true,
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
        assert!(plan.health_state.server_state_registered);
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
            synctv_core::resilience::timeout::REMOTE_TRANSPORT_CALL_TIMEOUT
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
            server_state_registered: false,
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
                <synctv_proto::client::auth_service_server::AuthServiceServer<
                    crate::grpc::ClientServiceImpl,
                > as tonic::server::NamedService>::NAME,
            )
            .await,
            Ok(ServingStatus::Serving),
        );
        assert_eq!(
            health_status_for_service(
                &health_service,
                <synctv_proto::client::email_service_server::EmailServiceServer<
                    crate::grpc::ClientServiceImpl,
                > as tonic::server::NamedService>::NAME,
            )
            .await,
            Ok(ServingStatus::Serving),
        );
        assert_eq!(
            health_status_for_service(
                &health_service,
                <synctv_proto::client::notification_service_server::NotificationServiceServer<
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
                <synctv_proto::client::auth_service_server::AuthServiceServer<
                    crate::grpc::ClientServiceImpl,
                > as tonic::server::NamedService>::NAME,
            )
            .await,
            Ok(ServingStatus::NotServing),
        );
        assert_eq!(
            health_status_for_service(
                &health_service,
                <synctv_proto::client::email_service_server::EmailServiceServer<
                    crate::grpc::ClientServiceImpl,
                > as tonic::server::NamedService>::NAME,
            )
            .await,
            Ok(ServingStatus::NotServing),
        );
        assert_eq!(
            health_status_for_service(
                &health_service,
                <synctv_proto::client::notification_service_server::NotificationServiceServer<
                    crate::grpc::NotificationServiceImpl,
                > as tonic::server::NamedService>::NAME,
            )
            .await,
            Ok(ServingStatus::NotServing),
        );
    }

    #[tokio::test]
    async fn test_grpc_shutdown_signal_marks_health_not_serving() -> TestResult {
        let health_reporter = tonic_health::server::HealthReporter::new();
        let health_service = HealthService::from_health_reporter(health_reporter.clone());
        let state = GrpcHealthRegistrationState {
            auth_registered: true,
            user_registered: false,
            room_registered: false,
            public_registered: false,
            admin_registered: false,
            email_registered: false,
            notification_registered: false,
            oauth2_registered: false,
            provider_services_registered: false,
            cluster_service_registered: false,
            server_state_registered: false,
            realtime_presence_registered: false,
            proxy_slice_cache_registered: false,
            livestream_relay_registered: false,
        };
        set_registered_grpc_services_serving(&health_reporter, state).await;

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let shutdown_task = tokio::spawn(wait_for_grpc_shutdown(
            Some(shutdown_rx),
            health_reporter,
            state,
        ));

        shutdown_tx
            .send(true)
            .map_err(|_| test_error("send shutdown signal"))?;
        shutdown_task.await?;

        assert_eq!(
            health_status_for_service(&health_service, "").await,
            Ok(ServingStatus::NotServing),
        );
        assert_eq!(
            health_status_for_service(
                &health_service,
                <synctv_proto::client::auth_service_server::AuthServiceServer<
                    crate::grpc::ClientServiceImpl,
                > as tonic::server::NamedService>::NAME,
            )
            .await,
            Ok(ServingStatus::NotServing),
        );
        Ok(())
    }
}
