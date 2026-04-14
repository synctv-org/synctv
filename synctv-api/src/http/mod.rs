// Module: http
// HTTP/JSON REST API

pub mod admin;
pub mod auth;
pub mod email_verification;
pub mod error;
pub mod health;
pub mod metrics_auth;
pub mod middleware;
pub mod notifications;
pub mod oauth2;
pub mod public;
pub mod publish_key;
pub mod room;
pub mod room_extra;
pub mod ticket;
pub mod user;
pub mod validation;
pub mod webrtc;
pub mod websocket;

// Provider HTTP routes
// Provider-specific HTTP endpoints are registered from provider instances
pub mod provider_common;
pub mod providers;

use axum::{
    http::{HeaderValue, Method},
    middleware as axum_middleware,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use synctv_core::provider::proxy::ProxyServices;
use synctv_core::provider::ProviderSet;
use synctv_core::repository::UserProviderCredentialRepository;
use synctv_core::service::ProxySigningKey;
use synctv_core::service::{RemoteProviderManager, RoomService, UserService};
use synctv_livestream::api::LiveStreamingInfrastructure;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use crate::cluster_fanout::ClusterFanoutService;
use crate::runtime::{RealtimeConnectionService, RealtimeEventService};

pub use error::{AppError, AppResult};

const HTTP_REQUEST_TIMEOUT: std::time::Duration =
    synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT;

pub(crate) trait WithUserId: Sized {
    fn with_user_id(self, user_id: String) -> Self;
}

pub(crate) trait WithRoomId: Sized {
    fn with_room_id(self, room_id: String) -> Self;
}

pub(crate) trait WithMediaId: Sized {
    fn with_media_id(self, media_id: String) -> Self;
}

pub(crate) trait WithPlaylistId: Sized {
    fn with_playlist_id(self, playlist_id: String) -> Self;
}

pub(crate) trait WithProviderInstanceName: Sized {
    fn with_provider_instance_name(self, name: String) -> Self;
}

pub(crate) trait WithId: Sized {
    fn with_id(self, id: String) -> Self;
}

macro_rules! impl_with_string_field {
    ($trait_name:ident, $method_name:ident, $field:ident, [$($ty:path),+ $(,)?]) => {
        $(
            impl $trait_name for $ty {
                fn $method_name(mut self, value: String) -> Self {
                    self.$field = value;
                    self
                }
            }
        )+
    };
}

impl_with_string_field!(
    WithUserId,
    with_user_id,
    user_id,
    [
        crate::proto::client::UpdateMemberPermissionsRequest,
        crate::proto::admin::UpdateUserRoleRequest,
        crate::proto::admin::UpdateUserPasswordRequest,
        crate::proto::admin::UpdateUserUsernameRequest,
        crate::proto::admin::BanUserRequest,
        crate::proto::admin::GetUserRoomsRequest
    ]
);

impl_with_string_field!(
    WithRoomId,
    with_room_id,
    room_id,
    [
        crate::proto::admin::UpdateRoomPasswordRequest,
        crate::proto::admin::GetRoomMembersRequest,
        crate::proto::admin::BanRoomRequest,
        crate::proto::admin::UpdateRoomSettingsRequest
    ]
);

impl_with_string_field!(
    WithMediaId,
    with_media_id,
    media_id,
    [crate::proto::client::EditMediaRequest]
);

impl_with_string_field!(
    WithPlaylistId,
    with_playlist_id,
    playlist_id,
    [
        crate::proto::client::UpdatePlaylistRequest,
        crate::proto::client::MovePlaylistRequest
    ]
);

impl_with_string_field!(
    WithProviderInstanceName,
    with_provider_instance_name,
    name,
    [crate::proto::admin::UpdateProviderInstanceRequest]
);

impl_with_string_field!(
    WithId,
    with_id,
    id,
    [crate::proto::client::CreatePublishKeyRequest]
);

impl_with_string_field!(
    WithUserId,
    with_user_id,
    user_id,
    [
        crate::proto::client::KickMemberRequest,
        crate::proto::client::UnbanMemberRequest,
        crate::proto::client::ApproveMemberRequest,
        crate::proto::client::RejectMemberRequest
    ]
);

/// Configuration for creating the HTTP router
#[derive(Clone)]
pub struct RouterConfig {
    pub config: Arc<synctv_core::Config>,
    pub user_service: Arc<UserService>,
    pub user_cache: Arc<synctv_core::cache::UserCache>,
    pub room_service: Arc<RoomService>,
    pub content_filter: synctv_core::service::ContentFilter,
    pub provider_instance_manager: Arc<RemoteProviderManager>,
    pub user_provider_credential_repository: Arc<UserProviderCredentialRepository>,
    pub providers: ProviderSet,
    pub event_service: Option<Arc<dyn RealtimeEventService>>,
    pub connection_manager: Arc<dyn RealtimeConnectionService>,
    pub jwt_service: synctv_core::service::JwtService,
    pub cluster_fanout_service: Arc<dyn ClusterFanoutService>,
    pub oauth2_service: Option<Arc<synctv_core::service::OAuth2Service>>,
    pub settings_service: Option<Arc<synctv_core::service::SettingsService>>,
    pub settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
    pub email_service: Option<Arc<synctv_core::service::EmailService>>,
    pub email_token_service: Option<Arc<synctv_core::service::EmailTokenService>>,
    pub publish_key_service: Option<Arc<synctv_core::service::PublishKeyService>>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub chat_service: Option<Arc<synctv_core::service::ChatService>>,
    pub audit_service: Arc<synctv_core::service::AuditService>,
    pub live_streaming_infrastructure: Option<Arc<LiveStreamingInfrastructure>>,
    pub rate_limiter: synctv_core::service::rate_limit::RateLimiter,
    /// WebSocket ticket service for secure WebSocket authentication (HTTP only)
    pub ws_ticket_service: Arc<synctv_core::service::WsTicketService>,
    /// Shared runtime for playback caching and other shared-state lookups.
    pub redis_runtime: Option<Arc<dyn synctv_core::RedisConnectionRuntime>>,
    /// Shared provider playback store registry reused across transports when available.
    pub shared_provider_stores: Option<Arc<dyn synctv_core::provider::store::ProviderStoreResolver>>,
    /// Shared proxy signing key reused across transports when available.
    pub shared_proxy_signing_key: Option<Arc<ProxySigningKey>>,
    /// Resolved built-in STUN URL (e.g. "stun:203.0.113.1:3478") from a successfully started
    /// STUN server. When `None`, the built-in STUN entry is omitted from ICE server lists.
    pub builtin_stun_url: Option<String>,
    /// TURN health checker for filtering unhealthy TURN servers
    pub turn_health_checker: Option<Arc<synctv_core::service::TurnHealthChecker>>,
    /// Credential encryption for protecting sensitive data in `source_config`
    pub credential_encryption: Option<synctv_core::service::CredentialEncryption>,
    /// Shared proxy slice cache instance managed by the runtime.
    pub proxy_slice_cache: Arc<synctv_proxy::slice_cache::SliceCache>,
    /// Shared outbound HTTP client used by proxy handlers and cache fills.
    pub proxy_http_client: reqwest::Client,
    /// Rate limit configuration for WebSocket messaging (chat/danmaku).
    /// This is separate from the HTTP rate limit config used by middleware.
    pub messaging_rate_limit_config: synctv_core::service::RateLimitConfig,
    /// Heartbeat/cache timing for real-time messaging. Production defaults are
    /// conservative; tests may inject a shorter schedule.
    pub heartbeat_schedule: crate::impls::HeartbeatSchedule,
    /// Providers manager for playback generation (media provider lookup)
    pub providers_manager: Option<Arc<synctv_core::service::ProvidersManager>>,
}

/// Shared application state.
///
/// Common service fields live in `RouterConfig` (shared via `Arc`). Derived
/// fields that are computed at startup (API impls, validators, etc.) live
/// directly on `AppState`. Thanks to the `Deref` impl, all `RouterConfig`
/// fields are accessible transparently (e.g. `state.user_service`).
#[derive(Clone)]
pub struct SharedApiRuntime {
    /// Redis runtime abstraction derived from the shared connection when available.
    pub redis_runtime: Option<Arc<dyn synctv_core::RedisConnectionRuntime>>,
    /// Shared rate limit config (created once at startup, not per-request)
    pub rate_limit_config: Arc<middleware::RateLimitConfig>,
    /// Shared messaging rate limit config for WebSocket (chat/danmaku rate limits)
    pub messaging_rate_limit_config: Arc<synctv_core::service::RateLimitConfig>,
    /// Shared content filter configured at startup.
    pub content_filter: Arc<synctv_core::service::ContentFilter>,
    pub heartbeat_schedule: crate::impls::HeartbeatSchedule,
    /// Shared JWT validator (created once at startup, not per-request)
    pub jwt_validator: Arc<synctv_core::service::auth::JwtValidator>,
    /// Shared security pipeline for post-JWT checks (password, user status)
    pub security_pipeline: Arc<synctv_core::service::SecurityPipeline>,
    /// Shared guest token validator (JWT + blacklist check)
    pub guest_token_validator: Arc<synctv_core::service::auth::GuestTokenValidator>,
    // Unified API implementation layer
    pub client_api: Arc<crate::impls::ClientApiImpl>,
    pub admin_api: Option<Arc<crate::impls::AdminApiImpl>>,
    pub email_api: Option<Arc<crate::impls::EmailApiImpl>>,
    pub notification_api: Option<Arc<crate::impls::NotificationApiImpl>>,
    pub oauth2_api: Option<Arc<crate::impls::OAuth2ApiImpl>>,
    // H-2: Provider ApiImpls stored once in shared runtime (not created per-request)
    pub bilibili_api: Arc<crate::impls::BilibiliApiImpl>,
    pub alist_api: Arc<crate::impls::AlistApiImpl>,
    pub emby_api: Arc<crate::impls::EmbyApiImpl>,
    /// Repository shared by provider transports for bind lookups.
    pub user_provider_credential_repository: Arc<UserProviderCredentialRepository>,
    /// Per-provider stores for caching and distributed locking (lazy creation)
    pub provider_stores: Arc<dyn synctv_core::provider::store::ProviderStoreResolver>,
    /// Registry of proxy-capable providers (looked up by type name in unified proxy handler)
    pub proxy_provider_registry: Arc<synctv_core::provider::proxy::ProxyProviderRegistry>,
    /// Services available to providers during proxy resolution (DB access)
    pub proxy_services: Arc<ProxyServices>,
    /// HMAC signing key for proxy URL authentication
    pub proxy_signing_key: Arc<ProxySigningKey>,
}

#[derive(Clone)]
pub struct AppState {
    /// Common service configuration (shared cheaply via `Arc`).
    pub router_config: Arc<RouterConfig>,
    /// Shared transport-agnostic runtime reused across HTTP, gRPC, and management.
    pub shared_api_runtime: Arc<SharedApiRuntime>,
    /// Redis runtime abstraction derived from the shared connection when available.
    pub redis_runtime: Option<Arc<dyn synctv_core::RedisConnectionRuntime>>,
    /// Shared rate limit config (created once at startup, not per-request)
    pub rate_limit_config: Arc<middleware::RateLimitConfig>,
    /// Shared messaging rate limit config for WebSocket (chat/danmaku rate limits)
    pub messaging_rate_limit_config: Arc<synctv_core::service::RateLimitConfig>,
    /// Shared content filter configured at startup.
    pub content_filter: Arc<synctv_core::service::ContentFilter>,
    pub heartbeat_schedule: crate::impls::HeartbeatSchedule,
    /// Shared JWT validator (created once at startup, not per-request)
    pub jwt_validator: Arc<synctv_core::service::auth::JwtValidator>,
    /// Shared security pipeline for post-JWT checks (password, user status)
    pub security_pipeline: Arc<synctv_core::service::SecurityPipeline>,
    /// Shared guest token validator (JWT + blacklist check)
    pub guest_token_validator: Arc<synctv_core::service::auth::GuestTokenValidator>,
    // Unified API implementation layer
    pub client_api: Arc<crate::impls::ClientApiImpl>,
    pub admin_api: Option<Arc<crate::impls::AdminApiImpl>>,
    pub email_api: Option<Arc<crate::impls::EmailApiImpl>>,
    pub notification_api: Option<Arc<crate::impls::NotificationApiImpl>>,
    pub oauth2_api: Option<Arc<crate::impls::OAuth2ApiImpl>>,
    // H-2: Provider ApiImpls stored once in AppState (not created per-request)
    pub bilibili_api: Arc<crate::impls::BilibiliApiImpl>,
    pub alist_api: Arc<crate::impls::AlistApiImpl>,
    pub emby_api: Arc<crate::impls::EmbyApiImpl>,
    /// Per-provider stores for caching and distributed locking (lazy creation)
    pub provider_stores: Arc<dyn synctv_core::provider::store::ProviderStoreResolver>,
    /// Registry of proxy-capable providers (looked up by type name in unified proxy handler)
    pub proxy_provider_registry: Arc<synctv_core::provider::proxy::ProxyProviderRegistry>,
    /// Services available to providers during proxy resolution (DB access)
    pub proxy_services: Arc<ProxyServices>,
    /// HMAC signing key for proxy URL authentication
    pub proxy_signing_key: Arc<ProxySigningKey>,
    /// Shared proxy slice cache used by unified provider proxy routes.
    pub proxy_slice_cache: Arc<synctv_proxy::slice_cache::SliceCache>,
    /// Shared outbound HTTP client used by proxy handlers.
    pub proxy_http_client: reqwest::Client,
    pub metrics_access_controller: Arc<metrics_auth::MetricsAccessController>,
}

pub struct ProxyCacheLifecycleRuntime {
    pub cancel: CancellationToken,
    pub handle: JoinHandle<()>,
}

impl std::ops::Deref for AppState {
    type Target = RouterConfig;
    fn deref(&self) -> &RouterConfig {
        &self.router_config
    }
}

impl AppState {
    /// Resolve a fresh Redis `ConnectionManager` clone from the shared `RwLock`.
    ///
    /// Returns `None` when Redis is not configured.
    pub async fn resolve_redis_conn(&self) -> Option<redis::aio::ConnectionManager> {
        match &self.redis_runtime {
            Some(runtime) => Some(runtime.snapshot().await),
            None => None,
        }
    }
}

/// Create the HTTP router from configuration struct
pub fn create_router_from_config(config: RouterConfig) -> anyhow::Result<axum::Router> {
    let state = create_app_state_from_config(config);
    let router = create_router_from_shared_state(&state)?;
    Ok(router)
}

/// Create shared `AppState` once so multiple transports can reuse the same impl instances.
#[must_use]
pub fn create_app_state_from_config(config: RouterConfig) -> AppState {
    build_app_state(config)
}

/// Create the HTTP router from an already constructed shared `AppState`.
pub fn create_router_from_shared_state(state: &AppState) -> anyhow::Result<axum::Router> {
    let state = state.clone();
    let (timeout_router, no_timeout_router, upgrade_router) = register_all_routes(state.clone());
    apply_global_layers(timeout_router, no_timeout_router, upgrade_router, &state)
}

/// Create the HTTP router and the shared application state from configuration.
pub fn create_router_with_state_from_config(
    config: RouterConfig,
) -> anyhow::Result<(axum::Router, AppState)> {
    let state = create_app_state_from_config(config);
    let router = create_router_from_shared_state(&state)?;
    Ok((router, state))
}

/// Build `AppState` from `RouterConfig`, creating the shared API implementation layers.
fn build_app_state(config: RouterConfig) -> AppState {
    let shared_api_runtime = Arc::new(build_shared_api_runtime(&config));
    let proxy_slice_cache = config.proxy_slice_cache.clone();
    let proxy_http_client = config.proxy_http_client.clone();

    AppState {
        router_config: Arc::new(config),
        shared_api_runtime: shared_api_runtime.clone(),
        redis_runtime: shared_api_runtime.redis_runtime.clone(),
        rate_limit_config: shared_api_runtime.rate_limit_config.clone(),
        messaging_rate_limit_config: shared_api_runtime.messaging_rate_limit_config.clone(),
        content_filter: shared_api_runtime.content_filter.clone(),
        heartbeat_schedule: shared_api_runtime.heartbeat_schedule,
        jwt_validator: shared_api_runtime.jwt_validator.clone(),
        security_pipeline: shared_api_runtime.security_pipeline.clone(),
        guest_token_validator: shared_api_runtime.guest_token_validator.clone(),
        client_api: shared_api_runtime.client_api.clone(),
        admin_api: shared_api_runtime.admin_api.clone(),
        email_api: shared_api_runtime.email_api.clone(),
        notification_api: shared_api_runtime.notification_api.clone(),
        oauth2_api: shared_api_runtime.oauth2_api.clone(),
        bilibili_api: shared_api_runtime.bilibili_api.clone(),
        alist_api: shared_api_runtime.alist_api.clone(),
        emby_api: shared_api_runtime.emby_api.clone(),
        provider_stores: shared_api_runtime.provider_stores.clone(),
        proxy_provider_registry: shared_api_runtime.proxy_provider_registry.clone(),
        proxy_services: shared_api_runtime.proxy_services.clone(),
        proxy_signing_key: shared_api_runtime.proxy_signing_key.clone(),
        proxy_slice_cache,
        proxy_http_client,
        metrics_access_controller: Arc::new(metrics_auth::MetricsAccessController::new()),
    }
}

pub(crate) fn build_shared_api_runtime(config: &RouterConfig) -> SharedApiRuntime {
    let redis_runtime = config.redis_runtime.clone();
    let proxy_signing_key = config.shared_proxy_signing_key.clone().unwrap_or_else(|| {
        Arc::new(ProxySigningKey::derive_from(
            config.config.jwt.secret.as_bytes(),
        ))
    });
    let provider_stores = config.shared_provider_stores.clone().unwrap_or_else(|| {
        synctv_core::provider::store::build_provider_store_resolver_from_profile(
            &synctv_core::SharedStateProfile::from_runtime(
                redis_runtime.clone(),
                config.config.redis.key_prefix.clone(),
                false,
            ),
        )
    });

    // Build the shared security pipeline through the builder so startup fails
    // early if blacklist wiring becomes partial during future refactors.
    let security_pipeline = Arc::new(
        synctv_core::service::auth::SecurityPipelineBuilder::new(config.user_service.clone())
            .with_user_cache(config.user_cache.clone())
            .with_token_blacklist(
                config.user_service.token_blacklist_store(),
                config.user_service.key_builder().clone(),
            )
            .build()
            .expect("HTTP security pipeline wiring must be complete at startup"),
    );

    let client_api = Arc::new(
        crate::impls::ClientApiImpl::new(
            config.user_service.clone(),
            config.room_service.clone(),
            config.connection_manager.clone(),
            config.config.clone(),
            config.publish_key_service.clone(),
            config.jwt_service.clone(),
            config.live_streaming_infrastructure.clone(),
            config.providers_manager.clone(),
            config.settings_registry.clone(),
        )
        .with_cluster_fanout_service(config.cluster_fanout_service.clone())
        .with_shared_runtime(redis_runtime.clone())
        .with_rate_limiter(config.rate_limiter.clone())
        .with_credential_encryption(config.credential_encryption.clone())
        .with_credential_repo(config.user_provider_credential_repository.clone())
        .with_signing_key(proxy_signing_key.clone())
        .with_provider_stores(provider_stores.clone()),
    );

    // Wire in the resolved STUN URL if the built-in STUN server started successfully
    let client_api = if let Some(ref stun_url) = config.builtin_stun_url {
        let inner = Arc::try_unwrap(client_api).unwrap_or_else(|arc| (*arc).clone());
        Arc::new(inner.with_builtin_stun_url(stun_url.clone()))
    } else {
        client_api
    };

    // Wire in the TURN health checker
    let client_api = if config.turn_health_checker.is_some() {
        let inner = Arc::try_unwrap(client_api).unwrap_or_else(|arc| (*arc).clone());
        Arc::new(inner.with_turn_health_checker(config.turn_health_checker.clone()))
    } else {
        client_api
    };

    let admin_api = config.settings_service.as_ref().map(|settings_svc| {
        let email_svc = config.email_service.clone().unwrap_or_else(|| {
            Arc::new(
                synctv_core::service::EmailService::new(None)
                    .expect("EmailService::new(None) should not fail"),
            )
        });
        Arc::new(
            crate::impls::AdminApiImpl::new(
                config.room_service.clone(),
                config.user_service.clone(),
                settings_svc.clone(),
                config.settings_registry.clone(),
                email_svc,
                config.connection_manager.clone(),
                config.provider_instance_manager.clone(),
                config.live_streaming_infrastructure.clone(),
                config.publish_key_service.clone(),
                config.config.clone(),
                config.audit_service.clone(),
            )
            .with_cluster_fanout_service(config.cluster_fanout_service.clone())
            .with_provider_stores(provider_stores.clone()),
        )
    });
    let email_api = crate::impls::email::build_shared_email_api(
        config.user_service.clone(),
        config.email_service.clone(),
        config.email_token_service.clone(),
        config.rate_limiter.clone(),
    );

    // C-1: Create shared NotificationApiImpl (matches HTTP and gRPC)
    let notification_api = config
        .notification_service
        .as_ref()
        .map(|notif_svc| Arc::new(crate::impls::NotificationApiImpl::new(notif_svc.clone())));

    // Create shared OAuth2ApiImpl
    let oauth2_api = config.oauth2_service.as_ref().map(|oauth2_svc| {
        Arc::new(crate::impls::OAuth2ApiImpl::new(
            oauth2_svc.clone(),
            config.user_service.clone(),
        ))
    });

    // H-3: Create shared RateLimitConfig from the config file (not hardcoded defaults)
    let rate_limit_config = Arc::new(config.config.http_rate_limits.clone());

    // Create shared messaging rate limit config for WebSocket (chat/danmaku)
    let messaging_rate_limit_config = Arc::new(config.messaging_rate_limit_config.clone());

    // H-5: Create shared JwtValidator once at startup (not per-request)
    let jwt_validator = Arc::new(synctv_core::service::auth::JwtValidator::new(Arc::new(
        config.jwt_service.clone(),
    )));

    // H-2: Create shared provider ApiImpls once at startup (not per-request)
    let credential_repo = config.user_provider_credential_repository.clone();
    let bilibili_api = Arc::new(crate::impls::BilibiliApiImpl::new(
        config.providers.bilibili.clone(),
        credential_repo.clone(),
    ));
    let alist_api = Arc::new(crate::impls::AlistApiImpl::new(
        config.providers.alist.clone(),
        credential_repo.clone(),
    ));
    let emby_api = Arc::new(crate::impls::EmbyApiImpl::new(
        config.providers.emby.clone(),
        credential_repo.clone(),
    ));

    // B1: Create shared GuestTokenValidator with blacklist support
    // This ensures guest tokens are checked against the blacklist (for kicked guests)
    // instead of only verifying the JWT signature.
    let guest_token_validator = Arc::new(
        synctv_core::service::auth::GuestTokenValidator::new(Arc::new(config.jwt_service.clone()))
            .with_blacklist(
                config.user_service.token_blacklist_store(),
                config.user_service.key_builder().clone(),
            ),
    );

    // Build proxy provider registry from ProviderSet (single source of truth)
    let proxy_provider_registry = Arc::new(config.providers.build_proxy_registry());

    // Create ProxyServices for unified proxy handler (gives providers DB access)
    let proxy_services = Arc::new(ProxyServices {
        room_service: config.room_service.clone(),
        credential_encryption: config.credential_encryption.clone(),
        credential_repo: credential_repo.clone(),
        signing_key: proxy_signing_key.clone(),
    });

    SharedApiRuntime {
        redis_runtime,
        rate_limit_config,
        messaging_rate_limit_config,
        content_filter: Arc::new(config.content_filter.clone()),
        heartbeat_schedule: config.heartbeat_schedule,
        jwt_validator,
        security_pipeline,
        guest_token_validator,
        client_api,
        admin_api,
        email_api,
        notification_api,
        oauth2_api,
        bilibili_api,
        alist_api,
        emby_api,
        user_provider_credential_repository: credential_repo,
        provider_stores,
        proxy_provider_registry,
        proxy_services,
        proxy_signing_key,
    }
}

pub fn start_proxy_cache_lifecycle(
    cache: &Arc<synctv_proxy::slice_cache::SliceCache>,
) -> Option<ProxyCacheLifecycleRuntime> {
    let manager = synctv_proxy::slice_cache::CacheLifecycleManager::new(
        cache.backend().clone(),
        cache.config().clone(),
    );
    let cancel = manager.cancellation_token();
    let handle = manager.start();
    Some(ProxyCacheLifecycleRuntime { cancel, handle })
}

/// Body size limits for specific endpoint categories (Issue #23).
///
/// These are applied as `route_layer`s INSIDE the rate-limit route groups so that
/// the limit is enforced before the handler reads the body, and the global 10 MB
/// safety net remains as a fallback for routes not explicitly limited here.
mod body_limits {
    /// Auth endpoints (login, register, refresh, OAuth2 exchange): 64 KB.
    /// A typical login JSON body is under 512 bytes; 64 KB is generous.
    pub const AUTH: usize = 64 * 1024;

    /// Room create / update / settings: 64 KB.
    pub const ROOM: usize = 64 * 1024;

    /// Media add / edit requests: 512 KB (media metadata may include longer URLs or
    /// subtitles, but should never be megabyte-scale).
    pub const MEDIA: usize = 512 * 1024;
}

/// Authentication routes (register, login, refresh, `OAuth2` exchange).
/// Strict rate limiting: 5 req/min. Body limit: 64 KB (Issue #23).
fn register_auth_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/email/request", post(auth::request_email_login))
        .route("/api/auth/refresh", post(auth::refresh_token))
        .route(
            "/api/oauth2/{provider}/exchange",
            post(oauth2::exchange_authorization_code),
        )
        // Tighter body limit for authentication endpoints (64 KB)
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::AUTH))
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_rate_limit,
        ))
}

/// Media mutation routes (add, delete, reorder, edit, batch operations).
/// Moderate rate limiting: 20 req/min. Body limit: 512 KB (Issue #23).
fn register_media_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/api/rooms/{room_id}/media", post(room::add_media))
        .route(
            "/api/rooms/{room_id}/media",
            axum::routing::delete(room::clear_playlist),
        )
        .route(
            "/api/rooms/{room_id}/media/batch",
            post(room::push_media_batch),
        )
        .route("/api/rooms/{room_id}/media/move", post(room::move_media))
        .route(
            "/api/rooms/{room_id}/media/{media_id}",
            axum::routing::delete(room::delete_media),
        )
        .route(
            "/api/rooms/{room_id}/media/{media_id}",
            axum::routing::patch(room::edit_media),
        )
        // Media metadata bodies are small (URLs, titles, subtitles)
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::MEDIA))
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::media_rate_limit,
        ))
}

/// Write routes (room CRUD, membership, playback control, playlists, user updates).
/// Moderate rate limiting: 30 req/min. Room create/update body limit: 64 KB (Issue #23).
fn register_write_routes(state: &AppState) -> Router<AppState> {
    let mut router = Router::new()
        .route("/api/rooms", post(room::create_room))
        .route(
            "/api/rooms/{room_id}",
            axum::routing::delete(room::delete_room),
        )
        .route(
            "/api/rooms/{room_id}/members/@me",
            axum::routing::put(room::join_room),
        )
        .route(
            "/api/rooms/{room_id}/members/@me",
            axum::routing::delete(room::leave_room),
        )
        .route("/api/rooms/{room_id}/members", post(room_extra::add_member))
        .route(
            "/api/rooms/{room_id}/members/{user_id}/approve",
            post(room_extra::approve_member),
        )
        .route(
            "/api/rooms/{room_id}/members/{user_id}/reject",
            post(room_extra::reject_member),
        )
        .route(
            "/api/rooms/{room_id}/settings",
            axum::routing::patch(room::update_room_settings),
        )
        .route(
            "/api/rooms/{room_id}/owner",
            post(room::transfer_room_ownership),
        )
        .route(
            "/api/rooms/{room_id}/password",
            axum::routing::patch(room::set_room_password),
        )
        .route(
            "/api/rooms/{room_id}/playback/start",
            post(room::start_playback),
        )
        .route(
            "/api/rooms/{room_id}/playback/stop",
            post(room::stop_playback),
        )
        .route(
            "/api/rooms/{room_id}/playback",
            axum::routing::patch(room::update_playback),
        )
        .route("/api/user/logout", post(auth::logout))
        .route("/api/user", axum::routing::patch(user::update_user))
        .route("/api/user/me", axum::routing::delete(user::delete_me))
        .route(
            "/api/rooms/{room_id}/members/{user_id}",
            axum::routing::delete(room_extra::kick_member),
        )
        .route(
            "/api/rooms/{room_id}/members/{user_id}",
            axum::routing::patch(room_extra::set_member_permissions),
        )
        .route("/api/rooms/{room_id}/bans", post(room_extra::ban_member))
        .route(
            "/api/rooms/{room_id}/bans/{user_id}",
            axum::routing::delete(room_extra::unban_member),
        )
        .route(
            "/api/rooms/{room_id}/playlists",
            post(room::create_playlist),
        )
        .route(
            "/api/rooms/{room_id}/playlists/{playlist_id}",
            axum::routing::patch(room::update_playlist),
        )
        .route(
            "/api/rooms/{room_id}/playlists/{playlist_id}/move",
            post(room::move_playlist),
        )
        .route(
            "/api/rooms/{room_id}/playlists/{playlist_id}",
            axum::routing::delete(room::delete_playlist),
        )
        .route(
            "/api/rooms/{room_id}/entries",
            axum::routing::delete(room::delete_entries),
        )
        .route(
            "/api/rooms/{room_id}/settings/reset",
            post(room::reset_room_settings),
        );

    router = router.merge(
        Router::new()
            .route("/api/tickets", post(ticket::create_ticket))
            .route_layer(axum_middleware::from_fn_with_state(
                state.clone(),
                middleware::websocket_runtime_required,
            )),
    );

    router = router
        .route(
            "/api/oauth2/{provider}/bind",
            get(oauth2::get_bind_authorize_url),
        )
        .route(
            "/api/oauth2/type/{provider}/unlink",
            axum::routing::delete(oauth2::unlink_provider),
        )
        .route("/api/oauth2/linked", get(oauth2::get_linked_providers));

    router
        // Room/user write bodies should be small (room metadata, settings, passwords)
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::ROOM))
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::write_rate_limit,
        ))
}

/// Read routes (user info, room discovery, room details, playlists, chat, media, playback).
/// Rate limited: 100 req/min.
fn register_read_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/api/user", get(user::get_me))
        .route("/api/user/rooms", get(user::list_my_rooms))
        .route("/api/rooms", get(room::list_or_get_rooms))
        .route("/api/rooms/hot", get(room::get_hot_rooms))
        .route("/api/rooms/{room_id}/check", get(room::check_room))
        .route("/api/rooms/{room_id}", get(room::get_room))
        .route(
            "/api/rooms/{room_id}/settings",
            get(room::get_room_settings),
        )
        .route("/api/rooms/{room_id}/members", get(room::get_room_members))
        .route(
            "/api/rooms/{room_id}/chat/history",
            get(room::get_chat_history),
        )
        // Playlist and Media APIs
        .route("/api/rooms/{room_id}/playlists", get(room::list_playlists))
        .route(
            "/api/rooms/{room_id}/playlists/{playlist_id}",
            get(room::get_playlist),
        )
        .route(
            "/api/rooms/{room_id}/media/list",
            post(room::list_playlist_items),
        )
        .route(
            "/api/rooms/{room_id}/media/{media_id}",
            get(room::get_media),
        )
        .route("/api/rooms/{room_id}/playback", get(room::get_playback))
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::read_rate_limit,
        ))
}

/// Assemble all route groups into a single router.
fn register_websocket_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/ws/rooms/{room_id}",
            axum::routing::get(websocket::websocket_handler),
        )
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::websocket_runtime_required,
        ))
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::websocket_rate_limit,
        ))
}

#[cfg(test)]
fn register_all_routes_for_test(state: AppState) -> Router<AppState> {
    let (timeout_router, no_timeout_router, upgrade_router) = register_all_routes(state);
    timeout_router
        .merge(no_timeout_router)
        .merge(upgrade_router)
}

fn register_all_routes(state: AppState) -> (Router<AppState>, Router<AppState>, Router<AppState>) {
    let mut timeout_router = Router::new()
        .merge(health::create_health_router())
        .merge(openapi_router())
        .merge(public::create_public_router())
        .merge(publish_key::create_publish_key_router().route_layer(
            axum_middleware::from_fn_with_state(state.clone(), middleware::auth_rate_limit),
        ))
        .merge(register_auth_routes(&state))
        .merge(register_media_routes(&state))
        .merge(register_write_routes(&state))
        .merge(register_read_routes(&state))
        // WebRTC configuration endpoints
        .merge(
            Router::new()
                .route(
                    "/api/rooms/{room_id}/webrtc/ice-servers",
                    get(webrtc::get_ice_servers),
                )
                .route_layer(axum_middleware::from_fn_with_state(
                    state.clone(),
                    middleware::read_rate_limit,
                )),
        )
        // Admin routes
        .merge(
            Router::new()
                .nest("/api/admin", admin::create_admin_router())
                .route_layer(axum_middleware::from_fn_with_state(
                    state.clone(),
                    middleware::admin_rate_limit,
                )),
        )
        // Provider routes
        .merge(
            Router::new()
                .merge(register_provider_management_routes(&state))
                .merge(
                    Router::new()
                        .nest("/api/provider", provider_common::register_common_routes())
                        .route_layer(axum_middleware::from_fn_with_state(
                            state.clone(),
                            middleware::read_rate_limit,
                        )),
                ),
        );

    timeout_router = timeout_router.merge(register_provider_proxy_routes(&state));

    let no_timeout_router = register_provider_streaming_routes(&state);

    timeout_router = timeout_router
        .merge(
            notifications::create_notification_read_router().route_layer(
                axum_middleware::from_fn_with_state(state.clone(), middleware::read_rate_limit),
            ),
        )
        .merge(
            notifications::create_notification_write_router().route_layer(
                axum_middleware::from_fn_with_state(state.clone(), middleware::write_rate_limit),
            ),
        );

    let email_routes = email_verification::create_email_router().route_layer(
        axum_middleware::from_fn_with_state(state.clone(), middleware::auth_rate_limit),
    );
    timeout_router = timeout_router.merge(email_routes);

    timeout_router = timeout_router.merge(
        Router::new()
            .route(
                "/api/oauth2/{provider}/authorize",
                get(oauth2::get_authorize_url),
            )
            .route(
                "/api/oauth2/providers",
                get(oauth2::list_available_providers),
            )
            .route_layer(axum_middleware::from_fn_with_state(
                state.clone(),
                middleware::read_rate_limit,
            )),
    );

    let upgrade_router = register_websocket_routes(&state);

    if state.live_streaming_infrastructure.is_some() {
        timeout_router = timeout_router.merge(
            Router::new()
                .nest("/api/providers/rtmp", providers::live::rtmp_routes())
                .nest(
                    "/api/providers/live_proxy",
                    providers::live::live_proxy_routes(),
                )
                .route_layer(axum_middleware::from_fn_with_state(
                    state,
                    middleware::read_rate_limit,
                )),
        );
    }

    (timeout_router, no_timeout_router, upgrade_router)
}

#[cfg(feature = "openapi")]
fn openapi_router() -> Router<AppState> {
    crate::openapi::router()
}

#[cfg(not(feature = "openapi"))]
fn openapi_router() -> Router<AppState> {
    Router::new()
}

fn register_provider_management_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .merge(
            Router::new()
                .nest(
                    "/api/providers/bilibili",
                    providers::bilibili::bilibili_auth_routes(),
                )
                .nest(
                    "/api/providers/alist",
                    providers::alist::alist_auth_routes(),
                )
                .nest("/api/providers/emby", providers::emby::emby_auth_routes())
                .route_layer(axum_middleware::from_fn_with_state(
                    state.clone(),
                    middleware::auth_rate_limit,
                )),
        )
        .merge(
            Router::new()
                .nest(
                    "/api/providers/bilibili",
                    providers::bilibili::bilibili_read_routes(),
                )
                .nest(
                    "/api/providers/alist",
                    providers::alist::alist_read_routes(),
                )
                .nest("/api/providers/emby", providers::emby::emby_read_routes())
                .route_layer(axum_middleware::from_fn_with_state(
                    state.clone(),
                    middleware::read_rate_limit,
                )),
        )
}

fn register_provider_proxy_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/api/providers/proxy/{provider_name}/{*sub_path}",
            get(providers::unified_proxy_handler).options(providers::proxy_options_preflight),
        )
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::streaming_rate_limit,
        ))
}

fn register_provider_streaming_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/api/providers/proxy/rtmp/{*sub_path}",
            get(providers::unified_proxy_handler).options(providers::proxy_options_preflight),
        )
        .route(
            "/api/providers/proxy/live_proxy/{*sub_path}",
            get(providers::unified_proxy_handler).options(providers::proxy_options_preflight),
        )
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::streaming_rate_limit,
        ))
}

/// Build CORS layer based on configuration.
fn build_cors_layer(config: &synctv_core::Config) -> anyhow::Result<CorsLayer> {
    if config.server.cors_allowed_origins.is_empty() {
        tracing::warn!(
            "CORS policy: DENY ALL cross-origin requests (no origins configured). \
             Web frontends on different origins will fail to connect. \
             To fix, set server.cors_allowed_origins to your frontend URL(s): \
             SYNCTV_SERVER_CORS_ALLOWED_ORIGINS='[\"https://app.example.com\"]'"
        );
        Ok(CorsLayer::new())
    } else {
        let origins: Vec<HeaderValue> = config
            .server
            .cors_allowed_origins
            .iter()
            .map(|origin| parse_configured_cors_origin(origin))
            .collect::<anyhow::Result<Vec<_>>>()?;
        tracing::info!(
            origins = ?origins,
            "CORS: Configured with {} allowed origin(s)",
            origins.len()
        );
        Ok(CorsLayer::new()
            .allow_origin(origins)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                axum::http::header::AUTHORIZATION,
                axum::http::header::CONTENT_TYPE,
                axum::http::header::ACCEPT,
            ])
            .vary([
                axum::http::header::ORIGIN,
                axum::http::header::ACCESS_CONTROL_REQUEST_METHOD,
                axum::http::header::ACCESS_CONTROL_REQUEST_HEADERS,
            ]))
    }
}

fn parse_configured_cors_origin(origin: &str) -> anyhow::Result<HeaderValue> {
    synctv_core::config::validate_cors_origin(origin)
        .map_err(|error| anyhow::anyhow!("invalid CORS origin configured: {error}"))?;

    HeaderValue::from_str(origin)
        .map_err(|_| anyhow::anyhow!("invalid CORS origin configured: `{origin}`"))
}

/// Apply global middleware layers (CORS, body limit, timeout, security headers, HSTS,
/// request ID propagation, and tracing) and bind state.
fn apply_shared_http_layers(
    router: Router<AppState>,
    cors: CorsLayer,
    trusted_proxies: Vec<String>,
    hsts_value: String,
) -> Router<AppState> {
    router
        .layer(cors)
        .layer(axum::extract::DefaultBodyLimit::max(10 * 1024 * 1024))
        .layer(axum_middleware::from_fn(middleware::request_id_middleware))
        .layer(axum_middleware::from_fn(
            middleware::security_headers_middleware,
        ))
        .layer(axum_middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let hsts = hsts_value.clone();
                let trusted_proxies = trusted_proxies.clone();
                async move {
                    let remote_addr = request
                        .extensions()
                        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                        .map(|ci| ci.0.ip());
                    let forwarded_proto_https = remote_addr.is_some_and(|ip| {
                        let trusted = trusted_proxies.iter().any(|proxy| {
                            proxy
                                .parse::<ipnet::IpNet>()
                                .map(|network| network.contains(&ip))
                                .or_else(|_| {
                                    proxy
                                        .parse::<std::net::IpAddr>()
                                        .map(|proxy_ip| proxy_ip == ip)
                                })
                                .unwrap_or(false)
                        });
                        trusted
                            && request
                                .headers()
                                .get("x-forwarded-proto")
                                .and_then(|v| v.to_str().ok())
                                .is_some_and(|value| value.eq_ignore_ascii_case("https"))
                    });

                    let mut response = next.run(request).await;
                    if forwarded_proto_https {
                        if let Ok(value) = axum::http::HeaderValue::from_str(&hsts) {
                            response
                                .headers_mut()
                                .insert(axum::http::header::STRICT_TRANSPORT_SECURITY, value);
                        }
                    } else {
                        response
                            .headers_mut()
                            .remove(axum::http::header::STRICT_TRANSPORT_SECURITY);
                    }
                    response
                }
            },
        ))
}

fn apply_global_layers(
    timeout_router: Router<AppState>,
    no_timeout_router: Router<AppState>,
    upgrade_router: Router<AppState>,
    state: &AppState,
) -> anyhow::Result<axum::Router> {
    apply_global_layers_with_timeout(
        timeout_router,
        no_timeout_router,
        upgrade_router,
        state,
        HTTP_REQUEST_TIMEOUT,
    )
}

fn apply_global_layers_with_timeout(
    timeout_router: Router<AppState>,
    no_timeout_router: Router<AppState>,
    upgrade_router: Router<AppState>,
    state: &AppState,
    request_timeout: std::time::Duration,
) -> anyhow::Result<axum::Router> {
    let cors = build_cors_layer(&state.config)?;
    let trusted_proxies = state.config.server.trusted_proxies.clone();
    let hsts_value = middleware::hsts_header(63_072_000, true, false);
    let timeout_router = apply_shared_http_layers(
        timeout_router.layer(axum_middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| async move {
                match tokio::time::timeout(request_timeout, next.run(request)).await {
                    Ok(response) => response,
                    Err(_) => {
                        AppError::new(axum::http::StatusCode::REQUEST_TIMEOUT, "Request timed out")
                            .into_response()
                    }
                }
            },
        )),
        cors.clone(),
        trusted_proxies.clone(),
        hsts_value.clone(),
    );

    let no_timeout_router = apply_shared_http_layers(
        no_timeout_router,
        cors.clone(),
        trusted_proxies.clone(),
        hsts_value.clone(),
    );

    let upgrade_router =
        apply_shared_http_layers(upgrade_router, cors, trusted_proxies, hsts_value);

    Ok(timeout_router
        .merge(no_timeout_router)
        .merge(upgrade_router)
        .layer(axum_middleware::from_fn(
            crate::observability::metrics_middleware::metrics_layer,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone()))
}

#[cfg(test)]
mod tests {
    use super::{
        apply_global_layers_with_timeout, build_app_state, build_cors_layer,
        register_all_routes_for_test, start_proxy_cache_lifecycle, RouterConfig, WithId,
        WithMediaId, WithPlaylistId, WithProviderInstanceName, WithRoomId, WithUserId,
        HTTP_REQUEST_TIMEOUT,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::{routing::get, Router};
    use bytes::Bytes;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};
    use synctv_core::cache::{KeyBuilder, UsernameCache};
    use synctv_core::provider::{
        AlistProvider, BilibiliProvider, DirectUrlProvider, EmbyProvider, LiveProxyProvider,
        ProviderSet, RtmpProvider,
    };
    use synctv_core::service::{
        AuditService, ContentFilter, InMemoryTokenBlacklistStore, ProxySigningKey, RateLimitConfig,
        RateLimiter, RemoteProviderManager, RoomService, UserService,
    };
    use synctv_proxy::slice_cache::{SliceCache, SliceCacheBackend, SliceCacheConfig, StoredEntry};
    use tower::ServiceExt;

    #[test]
    fn test_with_user_id_injects_proto_user_scoped_requests() {
        let req = crate::proto::admin::BanUserRequest {
            user_id: String::new(),
            reason: "spam".to_string(),
        }
        .with_user_id("AbC123xYz890".to_string());

        assert_eq!(req.user_id, "AbC123xYz890");
        assert_eq!(req.reason, "spam");
    }

    #[test]
    fn test_with_user_id_injects_client_single_field_requests() {
        let req = crate::proto::client::KickMemberRequest::default()
            .with_user_id("AbC123xYz890".to_string());

        assert_eq!(req.user_id, "AbC123xYz890");
    }

    #[test]
    fn test_with_room_id_injects_proto_room_scoped_requests() {
        let req = crate::proto::admin::GetRoomMembersRequest {
            room_id: String::new(),
            page: 2,
            page_size: 25,
            search: "alice".to_string(),
            role: 0,
            status: 0,
            sort_by: 0,
            sort_direction: 0,
        }
        .with_room_id("ZyX098wVu765".to_string());

        assert_eq!(req.room_id, "ZyX098wVu765");
        assert_eq!(req.page, 2);
        assert_eq!(req.page_size, 25);
        assert_eq!(req.search, "alice");
    }

    #[test]
    fn test_proto_http_injection_traits_cover_media_playlist_and_provider_name() {
        let edit_media = crate::proto::client::EditMediaRequest {
            media_id: String::new(),
            title: "Episode 1".to_string(),
        }
        .with_media_id("AbC123xYz890".to_string());

        let move_playlist = crate::proto::client::MovePlaylistRequest {
            playlist_id: String::new(),
            anchor: Some(
                crate::proto::client::move_playlist_request::Anchor::AfterPlaylistId(
                    "QwErTy123456".to_string(),
                ),
            ),
        }
        .with_playlist_id("ZyX098wVu765".to_string());

        let update_provider = crate::proto::admin::UpdateProviderInstanceRequest {
            name: String::new(),
            endpoint: Some("https://provider.internal".to_string()),
            comment: None,
            timeout_seconds: None,
            tls: None,
            insecure_tls: None,
            providers: Vec::new(),
            config: Vec::new(),
        }
        .with_provider_instance_name("alist_main".to_string());

        assert_eq!(edit_media.media_id, "AbC123xYz890");
        assert_eq!(edit_media.title, "Episode 1");
        assert_eq!(move_playlist.playlist_id, "ZyX098wVu765");
        assert_eq!(update_provider.name, "alist_main");
        assert_eq!(
            update_provider.endpoint.as_deref(),
            Some("https://provider.internal")
        );
    }

    #[test]
    fn test_with_id_injects_single_id_requests() {
        let req =
            crate::proto::client::CreatePublishKeyRequest::default().with_id("ZyX098wVu765".into());

        assert_eq!(req.id, "ZyX098wVu765");
    }

    pub(crate) fn test_app_state() -> super::AppState {
        test_app_state_with_rate_limits(
            synctv_core::HttpRateLimitConfig::default(),
            synctv_core::GrpcRateLimitConfig::default(),
        )
    }

    fn test_app_state_with_rate_limits(
        http_rate_limits: synctv_core::HttpRateLimitConfig,
        grpc_rate_limits: synctv_core::GrpcRateLimitConfig,
    ) -> super::AppState {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
            .expect("lazy pool");
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 128, 60);
        let user_service = Arc::new(UserService::new(
            pool.clone(),
            synctv_core::service::JwtService::new(
                "test-secret-key-for-http-router-tests-minimum-32-chars",
            )
            .expect("jwt"),
            username_cache,
            synctv_core::config::PasswordComplexityConfig::default(),
            Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
            KeyBuilder::new("test"),
            synctv_core::service::BruteForceProtection::in_memory("test".to_string()),
        ));
        let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
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
        let jwt_service = synctv_core::service::JwtService::new(
            "test-secret-key-for-http-router-tests-minimum-32-chars",
        )
        .expect("jwt");
        let (audit_service, _audit_handle) = AuditService::new(pool.clone());
        let config = synctv_core::Config {
            http_rate_limits,
            grpc_rate_limits,
            ..synctv_core::Config::default()
        };
        let router_config = RouterConfig {
            config: Arc::new(config),
            user_cache: Arc::new(
                synctv_core::cache::UserCache::local_only(
                    128,
                    60,
                    300,
                    "test:user:".to_string(),
                )
                .expect("user cache"),
            ),
            user_service,
            room_service,
            content_filter: ContentFilter::new(),
            provider_instance_manager,
            user_provider_credential_repository: Arc::new(
                synctv_core::repository::UserProviderCredentialRepository::new(pool),
            ),
            providers,
            event_service: None,
            connection_manager: Arc::new(synctv_cluster::sync::ConnectionManager::new(
                synctv_cluster::sync::ConnectionLimits::default(),
            )),
            jwt_service,
            cluster_fanout_service: crate::cluster_fanout::default_cluster_fanout_service(
                None, false,
            ),
            oauth2_service: None,
            settings_service: None,
            settings_registry: None,
            email_service: None,
            email_token_service: None,
            publish_key_service: None,
            notification_service: None,
            chat_service: None,
            audit_service: Arc::new(audit_service),
            live_streaming_infrastructure: None,
            rate_limiter: RateLimiter::local_only("test:".to_string()),
            ws_ticket_service: Arc::new(synctv_core::service::WsTicketService::local_only(None)),
            redis_runtime: None,
            shared_provider_stores: None,
            shared_proxy_signing_key: None,
            builtin_stun_url: None,
            turn_health_checker: None,
            credential_encryption: None,
            proxy_slice_cache: Arc::new(SliceCache::new(SliceCacheConfig::default())),
            proxy_http_client: synctv_proxy::build_proxy_http_client()
                .expect("proxy HTTP client should build for tests"),
            messaging_rate_limit_config: RateLimitConfig::default(),
            heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
            providers_manager: None,
        };
        build_app_state(router_config)
    }

    async fn test_app_state_with_websocket_runtime(
        http_rate_limits: synctv_core::HttpRateLimitConfig,
        grpc_rate_limits: synctv_core::GrpcRateLimitConfig,
    ) -> super::AppState {
        let state = test_app_state_with_rate_limits(http_rate_limits, grpc_rate_limits);
        let mut router_config = state.router_config.as_ref().clone();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
            .expect("lazy pool");

        let room_settings_service = synctv_core::service::RoomSettingsService::new(
            synctv_core::repository::RoomSettingsRepository::new(pool.clone()),
            None,
            Arc::new(synctv_core::service::NotificationService::default()),
            None,
            None,
        );
        let chat_service = synctv_core::service::ChatService::new(
            Arc::new(synctv_core::repository::ChatRepository::new(pool)),
            synctv_core::service::chat::ChatRuntime {
                rate_limiter: router_config.rate_limiter.clone(),
                rate_limit_config: state.messaging_rate_limit_config.as_ref().clone(),
                content_filter: state.content_filter.as_ref().clone(),
                username_cache: router_config.user_service.username_cache().clone(),
            },
            synctv_core::service::chat::ChatDependencies {
                permission_service: router_config.room_service.permission_service().clone(),
                room_settings_service,
                notification_service: synctv_core::service::NotificationService::default(),
            },
        );
        router_config.chat_service = Some(Arc::new(chat_service));
        let cluster_manager = Arc::new(
            synctv_cluster::sync::ClusterManager::new(
                synctv_cluster::sync::ClusterConfig {
                    distributed_transport_factory: None,
                    message_runtime: Arc::new(synctv_cluster::sync::RoomMessageHub::new()),
                    cluster_enabled: false,
                    node_id: "test-node".to_string(),
                    dedup_window: Duration::from_secs(30),
                    critical_channel_capacity: 8,
                    publish_channel_capacity: 8,
                    key_prefix: "test:".to_string(),
                    catchup_window_secs: 60,
                    stream_max_length: 100,
                    parent_cancel_token: None,
                },
                None,
                None,
            )
            .await
            .expect("cluster manager"),
        );
        router_config.event_service = Some(Arc::new(
            crate::runtime::ClusterRealtimeEventService::new(cluster_manager),
        ));
        build_app_state(router_config)
    }

    #[tokio::test]
    async fn test_start_proxy_cache_lifecycle_evicts_expired_entries_and_stops_on_cancel() {
        let cache = Arc::new(SliceCache::new(SliceCacheConfig {
            eviction_interval: Duration::from_millis(20),
            max_cache_size: 1024,
            ..SliceCacheConfig::default()
        }));
        let key = "expired-slice".to_string();
        cache
            .backend()
            .put(
                &key,
                StoredEntry {
                    data: Bytes::from_static(b"stale"),
                    inserted_at: SystemTime::now() - Duration::from_secs(2),
                    ttl: Duration::from_millis(5),
                    last_accessed: SystemTime::now() - Duration::from_secs(2),
                },
            )
            .await
            .expect("seed expired slice");

        let lifecycle = start_proxy_cache_lifecycle(&cache)
            .expect("enabled proxy cache must start lifecycle task");

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if cache.backend().get(&key).await.is_none() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("lifecycle task should evict expired slices");

        lifecycle.cancel.cancel();

        tokio::time::timeout(Duration::from_secs(1), lifecycle.handle)
            .await
            .expect("lifecycle task should stop after cancellation")
            .expect("lifecycle join should succeed");
    }

    #[tokio::test]
    async fn test_start_proxy_cache_lifecycle_starts_even_when_runtime_toggle_is_off() {
        let cache = Arc::new(SliceCache::new(SliceCacheConfig {
            enabled: false,
            ..SliceCacheConfig::default()
        }));

        let lifecycle = start_proxy_cache_lifecycle(&cache)
            .expect("cache lifecycle should start so runtime settings can enable caching later");
        lifecycle.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), lifecycle.handle)
            .await
            .expect("lifecycle task should stop after cancellation")
            .expect("lifecycle join should succeed");
    }

    #[tokio::test]
    async fn test_build_app_state_reuses_injected_proxy_cache() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
            .expect("lazy pool");
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 128, 60);
        let user_service = Arc::new(UserService::new(
            pool.clone(),
            synctv_core::service::JwtService::new(
                "test-secret-key-for-http-router-tests-minimum-32-chars",
            )
            .expect("jwt"),
            username_cache,
            synctv_core::config::PasswordComplexityConfig::default(),
            Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
            KeyBuilder::new("test"),
            synctv_core::service::BruteForceProtection::in_memory("test".to_string()),
        ));
        let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
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
        let jwt_service = synctv_core::service::JwtService::new(
            "test-secret-key-for-http-router-tests-minimum-32-chars",
        )
        .expect("jwt");
        let (audit_service, _audit_handle) = AuditService::new(pool);
        let injected_cache = Arc::new(SliceCache::new(SliceCacheConfig {
            enabled: false,
            ..SliceCacheConfig::default()
        }));
        let injected_provider_stores: Arc<
            dyn synctv_core::provider::store::ProviderStoreResolver,
        > = Arc::new(synctv_core::provider::store::ProviderStoreRegistry::local_only(
            "shared:test:",
        ));
        let injected_proxy_signing_key = Arc::new(ProxySigningKey::derive_from(
            b"test-secret-key-for-http-router-tests-minimum-32-chars",
        ));
        let injected_proxy_http_client = synctv_proxy::build_proxy_http_client()
            .expect("proxy HTTP client should build for tests");

        let state = build_app_state(RouterConfig {
            config: Arc::new(synctv_core::Config::default()),
            user_service,
            user_cache: Arc::new(
                synctv_core::cache::UserCache::local_only(
                    128,
                    60,
                    300,
                    "test:user:".to_string(),
                )
                .expect("user cache"),
            ),
            room_service,
            content_filter: ContentFilter::new(),
            provider_instance_manager,
            user_provider_credential_repository: Arc::new(
                synctv_core::repository::UserProviderCredentialRepository::new(
                    sqlx::postgres::PgPoolOptions::new()
                        .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
                        .expect("lazy pool"),
                ),
            ),
            providers,
            event_service: None,
            connection_manager: Arc::new(synctv_cluster::sync::ConnectionManager::new(
                synctv_cluster::sync::ConnectionLimits::default(),
            )),
            jwt_service,
            cluster_fanout_service: crate::cluster_fanout::default_cluster_fanout_service(
                None, false,
            ),
            oauth2_service: None,
            settings_service: None,
            settings_registry: None,
            email_service: None,
            email_token_service: None,
            publish_key_service: None,
            notification_service: None,
            chat_service: None,
            audit_service: Arc::new(audit_service),
            live_streaming_infrastructure: None,
            rate_limiter: RateLimiter::local_only("test:".to_string()),
            ws_ticket_service: Arc::new(synctv_core::service::WsTicketService::local_only(None)),
            redis_runtime: None,
            shared_provider_stores: Some(injected_provider_stores.clone()),
            shared_proxy_signing_key: Some(injected_proxy_signing_key.clone()),
            builtin_stun_url: None,
            turn_health_checker: None,
            credential_encryption: None,
            proxy_slice_cache: injected_cache.clone(),
            proxy_http_client: injected_proxy_http_client,
            messaging_rate_limit_config: RateLimitConfig::default(),
            heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
            providers_manager: None,
        });

        assert!(
            Arc::ptr_eq(&state.proxy_slice_cache, &injected_cache),
            "AppState must reuse the injected proxy slice cache instead of creating a hidden default instance"
        );
        assert!(
            !state.proxy_slice_cache.config().enabled,
            "The injected cache configuration must be preserved"
        );
        assert!(
            Arc::ptr_eq(&state.provider_stores, &injected_provider_stores),
            "AppState must reuse the injected provider store registry"
        );
        assert!(
            Arc::ptr_eq(&state.proxy_signing_key, &injected_proxy_signing_key),
            "AppState must reuse the injected proxy signing key"
        );
        assert!(
            state
                .proxy_http_client
                .get("https://example.com")
                .build()
                .is_ok(),
            "The injected proxy HTTP client must remain usable in AppState"
        );
        assert!(
            state.security_pipeline.has_user_cache(),
            "AppState security pipeline should carry the shared user cache"
        );
    }

    #[tokio::test]
    async fn test_build_app_state_wires_user_cache_into_security_pipeline() {
        let state = test_app_state();
        assert!(
            state.security_pipeline.has_user_cache(),
            "build_app_state should wire the shared user cache into the auth security pipeline"
        );
    }

    #[tokio::test]
    async fn test_build_app_state_wires_blacklist_into_security_pipeline() {
        let state = test_app_state();
        assert!(
            state.security_pipeline.has_blacklist_store(),
            "build_app_state should wire token blacklist configuration through the builder"
        );
    }

    #[tokio::test]
    async fn test_playback_patch_route_is_reachable_via_project_router() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/rooms/room123/playback")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"state":"playing"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "PATCH playback route must be registered in the project router"
        );
        assert_ne!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "PATCH playback route must accept PATCH requests"
        );
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "request should reach the registered route and follow the normal auth path; playback PATCH is not gated by websocket-runtime-only middleware"
        );
    }

    #[tokio::test]
    async fn test_public_rooms_route_is_reachable_without_auth() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/rooms?page=1&page_size=10")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_ne!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "public room listing must not require auth"
        );
        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "public room listing route must be registered"
        );
    }

    #[tokio::test]
    async fn test_removed_room_password_verify_route_returns_not_found() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/rooms/room1234_abx/password/verify")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"password":"secret"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "legacy room password verify route must stay removed from the latest API surface"
        );
    }

    #[tokio::test]
    async fn test_member_approval_routes_are_reachable_via_project_router() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        for (method, uri, body) in [
            (
                "POST",
                "/api/rooms/room1234_abx/members",
                Some(r#"{"user_id":"AbC123xYz890","role":1,"notify":true}"#),
            ),
            (
                "POST",
                "/api/rooms/room1234_abx/members/AbC123xYz890/approve",
                None,
            ),
            (
                "POST",
                "/api/rooms/room1234_abx/members/AbC123xYz890/reject",
                Some(r#"{"reason":"no longer eligible"}"#),
            ),
        ] {
            let builder = Request::builder().method(method).uri(uri);
            let builder = if body.is_some() {
                builder.header(axum::http::header::CONTENT_TYPE, "application/json")
            } else {
                builder
            };
            let response = app
                .clone()
                .oneshot(
                    builder
                        .body(Body::from(body.unwrap_or_default().to_string()))
                        .expect("request"),
                )
                .await
                .expect("response");

            assert_ne!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{uri} must be registered in the project router"
            );
            assert_ne!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{uri} must accept its documented HTTP method"
            );
        }
    }

    #[tokio::test]
    async fn test_oauth2_unlink_route_uses_provider_type_namespace() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let new_route_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/oauth2/type/github/unlink")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_ne!(new_route_response.status(), StatusCode::NOT_FOUND);

        let legacy_route_response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/oauth2/github/unlink")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(legacy_route_response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_delete_all_read_notifications_uses_read_namespace() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let new_route_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/notifications/read")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_ne!(new_route_response.status(), StatusCode::NOT_FOUND);

        let legacy_route_response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/notifications/actions/mark-read")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            legacy_route_response.status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[tokio::test]
    async fn test_main_router_does_not_expose_metrics_endpoint() {
        let mut state = test_app_state();
        Arc::make_mut(&mut state.router_config).config = Arc::new({
            let mut config = (*state.config).clone();
            config.metrics.enabled = true;
            config.metrics.auth.mode = synctv_core::config::MetricsAuthMode::BearerToken;
            config.metrics.auth.bearer_token = "metrics-secret".to_string();
            config
        });
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_provider_login_routes_use_auth_rate_limit_tier() {
        let state = test_app_state_with_rate_limits(
            synctv_core::HttpRateLimitConfig {
                auth_max_requests: 1,
                auth_window_seconds: 60,
                read_max_requests: 100,
                read_window_seconds: 60,
                ..synctv_core::HttpRateLimitConfig::default()
            },
            synctv_core::GrpcRateLimitConfig::default(),
        );
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/alist/login")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(first.status(), StatusCode::UNAUTHORIZED);

        let second = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/alist/login")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            second.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "provider login endpoints must share the stricter auth rate-limit bucket"
        );
    }

    #[tokio::test]
    async fn test_bilibili_me_route_requires_post() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/bilibili/me")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "Bilibili /me must require an explicit request body carrying server_id"
        );
    }

    #[tokio::test]
    async fn test_ticket_route_uses_write_rate_limit_tier() {
        let state = test_app_state_with_websocket_runtime(
            synctv_core::HttpRateLimitConfig {
                write_max_requests: 1,
                write_window_seconds: 60,
                read_max_requests: 100,
                read_window_seconds: 60,
                ..synctv_core::HttpRateLimitConfig::default()
            },
            synctv_core::GrpcRateLimitConfig::default(),
        )
        .await;
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tickets")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"room_id":"room1234_abx"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            first.status(),
            StatusCode::UNAUTHORIZED,
            "first unauthenticated ticket request should reach auth before exhausting the write bucket"
        );

        let second = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tickets")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"room_id":"room1234_abx"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            second.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "ticket issuance must share the write rate-limit bucket"
        );
    }

    #[tokio::test]
    async fn test_provider_proxy_routes_use_streaming_rate_limit_tier() {
        let state = test_app_state_with_rate_limits(
            synctv_core::HttpRateLimitConfig {
                streaming_max_requests: 1,
                streaming_window_seconds: 60,
                read_max_requests: 100,
                read_window_seconds: 60,
                ..synctv_core::HttpRateLimitConfig::default()
            },
            synctv_core::GrpcRateLimitConfig::default(),
        );
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/proxy/bilibili/v1/test.m3u8")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(first.status(), StatusCode::UNAUTHORIZED);

        let second = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/proxy/bilibili/v1/test.m3u8")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            second.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "provider proxy endpoints must use the streaming rate-limit bucket"
        );
    }

    #[tokio::test]
    async fn test_global_timeout_applies_only_to_timeout_router() {
        let state = test_app_state();

        let timeout_router = Router::new().route(
            "/timeout",
            get(|| async {
                tokio::time::sleep(HTTP_REQUEST_TIMEOUT + Duration::from_millis(50)).await;
                "too slow"
            }),
        );
        let no_timeout_router = Router::new().route(
            "/streaming",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                "stream survives"
            }),
        );

        let app = apply_global_layers_with_timeout(
            timeout_router,
            no_timeout_router,
            Router::new(),
            &state,
            Duration::from_millis(10),
        )
        .expect("valid test router should build");

        let timeout_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/timeout")
                    .header("x-request-id", "timeout-test-123")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            timeout_response.status(),
            StatusCode::REQUEST_TIMEOUT,
            "ordinary HTTP routes must still respect the global timeout budget"
        );
        assert_eq!(
            timeout_response
                .headers()
                .get("x-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("timeout-test-123"),
            "timed out responses must still propagate request IDs"
        );
        assert_eq!(
            timeout_response.headers().get("X-Frame-Options").unwrap(),
            "DENY",
            "timed out responses must still include security headers"
        );
        let timeout_body = axum::body::to_bytes(timeout_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let timeout_json: serde_json::Value =
            serde_json::from_slice(&timeout_body).expect("timeout response should be JSON");
        assert_eq!(timeout_json["status"], 408);
        assert_eq!(timeout_json["error"], "Request timed out");
        assert_eq!(timeout_json["request_id"], "timeout-test-123");

        let streaming_response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/streaming")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            streaming_response.status(),
            StatusCode::OK,
            "streaming/provider proxy routes must not be cut off by the unary HTTP timeout layer"
        );
    }

    #[tokio::test]
    async fn test_live_metadata_routes_stay_on_timeout_router() {
        let state = test_app_state();

        let timeout_router = Router::new().route(
            "/api/providers/rtmp/rooms/{room_id}/info/{media_id}",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                "metadata too slow"
            }),
        );
        let no_timeout_router = Router::new().route(
            "/api/providers/proxy/{provider_name}/{*sub_path}",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                "stream survives"
            }),
        );

        let app = apply_global_layers_with_timeout(
            timeout_router,
            no_timeout_router,
            Router::new(),
            &state,
            Duration::from_millis(10),
        )
        .expect("valid test router should build");

        let metadata_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/rtmp/rooms/room123/info/media123")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            metadata_response.status(),
            StatusCode::REQUEST_TIMEOUT,
            "live metadata routes must remain protected by the unary HTTP timeout"
        );

        let streaming_response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/proxy/rtmp/ver1/playlist.m3u8")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            streaming_response.status(),
            StatusCode::OK,
            "actual streaming proxy routes must remain outside the unary HTTP timeout"
        );
    }

    #[tokio::test]
    async fn test_streaming_proxy_routes_preserve_options_preflight() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let rtmp_preflight = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/api/providers/proxy/rtmp/ver1/playlist.m3u8")
                    .header(axum::http::header::ORIGIN, "https://example.com")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_ne!(
            rtmp_preflight.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "RTMP proxy routes must continue handling browser preflight through the generic proxy route"
        );

        let live_proxy_preflight = app
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/api/providers/proxy/live_proxy/ver1/playlist.m3u8")
                    .header(axum::http::header::ORIGIN, "https://example.com")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_ne!(
            live_proxy_preflight.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "live_proxy proxy routes must continue handling browser preflight through the generic proxy route"
        );
    }

    #[tokio::test]
    async fn test_cors_preflight_does_not_advertise_credentials() {
        let mut config = synctv_core::Config::default();
        config.server.cors_allowed_origins = vec!["https://example.com".to_string()];

        let app = Router::new()
            .route("/test", get(|| async { "ok" }))
            .layer(build_cors_layer(&config).expect("valid CORS config should build"));

        let response = app
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/test")
                    .header(axum::http::header::ORIGIN, "https://example.com")
                    .header(axum::http::header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert!(
            response
                .headers()
                .get(axum::http::header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
                .is_none(),
            "native-client-oriented CORS policy should not advertise credentialed browser requests by default"
        );
    }

    #[cfg(feature = "openapi")]
    #[tokio::test]
    async fn test_openapi_json_route_is_available() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api-docs/openapi.json")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid openapi json");
        assert_eq!(json["openapi"], "3.1.0");
        assert!(json["paths"]["/api/auth/login"].is_object());
        assert!(json["paths"]["/api/tickets"].is_object());
        assert!(json["paths"]["/api/user"].is_object());
        assert!(json["paths"]["/api/rooms/{room_id}/media"].is_object());
        assert!(json["paths"]["/api/admin/users"].is_object());
        assert!(json["paths"]["/api/rooms/{room_id}/webrtc/ice-servers"].is_object());
        assert!(json["paths"]["/api/oauth2/{provider}/exchange"].is_object());
        assert!(json["paths"]["/api/oauth2/providers"].is_object());
        assert!(json["paths"]["/api/oauth2/{provider}/authorize"].is_object());
        assert!(json["paths"]["/api/notifications"].is_object());
        assert!(json["paths"]["/api/email/verify/send"].is_object());
        assert!(json["paths"]["/api/providers/bilibili/parse"].is_object());
        assert!(json["paths"]["/api/providers/alist/login"].is_object());
        assert!(json["paths"]["/api/providers/rtmp/rooms/{room_id}/streams"].is_object());
        assert!(json["paths"]["/api/providers/live_proxy/rooms/{room_id}/streams"].is_object());
        assert!(
            json["paths"]["/api/providers/live_proxy/rooms/{room_id}/info/{media_id}"].is_object()
        );
        assert_eq!(
            json["paths"]["/api/providers/alist/login"]["post"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["$ref"],
            "#/components/schemas/synctv_provider_alist_LoginResponse"
        );
        assert_eq!(
            json["paths"]["/api/providers/emby/list"]["post"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["$ref"],
            "#/components/schemas/synctv_provider_emby_ListResponse"
        );
        assert_eq!(
            json["paths"]["/api/providers/bilibili/login/qr/check"]["post"]["responses"]["200"]
                ["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/synctv_provider_bilibili_QRStatusResponse"
        );
        assert_eq!(
            json["paths"]["/api/user"]["patch"]["responses"]["200"]["content"]["application/json"]
                ["schema"]["$ref"],
            "#/components/schemas/synctv_client_UpdateUserResponse"
        );
        assert_eq!(
            json["paths"]["/api/tickets"]["post"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["$ref"],
            "#/components/schemas/synctv_client_CreateWebSocketTicketResponse"
        );
        assert_eq!(
            json["paths"]["/api/rooms/{room_id}/webrtc/ice-servers"]["get"]["responses"]["200"]
                ["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/synctv_client_GetIceServersResponse"
        );

        let alist_login_ref = json["paths"]["/api/providers/alist/login"]["post"]["responses"]
            ["200"]["content"]["application/json"]["schema"]["$ref"]
            .as_str()
            .expect("alist login schema ref");
        let auth_login_ref = json["paths"]["/api/auth/login"]["post"]["responses"]["200"]
            ["content"]["application/json"]["schema"]["$ref"]
            .as_str()
            .expect("auth login schema ref");
        let emby_login_ref = json["paths"]["/api/providers/emby/login"]["post"]["responses"]["200"]
            ["content"]["application/json"]["schema"]["$ref"]
            .as_str()
            .expect("emby login schema ref");
        assert_eq!(
            auth_login_ref,
            "#/components/schemas/synctv_client_LoginResponse"
        );
        assert_ne!(
            auth_login_ref, alist_login_ref,
            "client login and provider login must use distinct OpenAPI components"
        );
        assert_ne!(
            alist_login_ref, emby_login_ref,
            "distinct provider response types must not collapse onto the same OpenAPI component"
        );

        let alist_login_schema_name = alist_login_ref
            .rsplit('/')
            .next()
            .expect("alist login schema name");
        let emby_login_schema_name = emby_login_ref
            .rsplit('/')
            .next()
            .expect("emby login schema name");

        let alist_login_properties =
            &json["components"]["schemas"][alist_login_schema_name]["properties"];
        assert!(
            alist_login_properties["token"].is_object(),
            "alist login schema should expose token"
        );
        assert!(
            alist_login_properties["server_id"].is_object(),
            "alist login schema should expose server_id"
        );
        assert!(
            alist_login_properties["user_id"].is_null(),
            "alist login schema must not be overwritten by emby login response"
        );

        let emby_login_properties =
            &json["components"]["schemas"][emby_login_schema_name]["properties"];
        assert!(
            emby_login_properties["user_id"].is_object(),
            "emby login schema should expose user_id"
        );
        assert!(
            emby_login_properties["username"].is_object(),
            "emby login schema should expose username"
        );
        assert!(
            emby_login_properties["is_admin"].is_object(),
            "emby login schema should expose is_admin"
        );
        assert!(
            emby_login_properties["token"].is_null(),
            "emby login schema must not be overwritten by alist login response"
        );
    }

    #[cfg(feature = "openapi")]
    #[tokio::test]
    async fn test_swagger_ui_route_is_available() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/swagger-ui/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn test_build_cors_layer_rejects_invalid_configured_origin() {
        let mut config = synctv_core::Config::default();
        config.server.cors_allowed_origins = vec![
            "https://example.com".to_string(),
            "not a valid origin".to_string(),
        ];

        let result = build_cors_layer(&config);

        assert!(
            result.is_err(),
            "invalid configured CORS origins must fail fast instead of being silently ignored"
        );
    }

    #[test]
    fn test_build_cors_layer_rejects_configured_origin_with_path() {
        let mut config = synctv_core::Config::default();
        config.server.cors_allowed_origins = vec!["https://example.com/app".to_string()];

        let result = build_cors_layer(&config);

        assert!(
            result.is_err(),
            "configured CORS origins with paths must fail fast during router construction"
        );
    }

    #[tokio::test]
    async fn test_finite_provider_proxy_routes_stay_on_timeout_router() {
        let state = test_app_state();

        let timeout_router = Router::new().route(
            "/api/providers/proxy/{provider_name}/{*sub_path}",
            get(|| async {
                tokio::time::sleep(HTTP_REQUEST_TIMEOUT + Duration::from_millis(50)).await;
                "finite proxy too slow"
            }),
        );
        let no_timeout_router = Router::new().route(
            "/api/providers/proxy/rtmp/{*sub_path}",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                "stream survives"
            }),
        );

        let app = apply_global_layers_with_timeout(
            timeout_router,
            no_timeout_router,
            Router::new(),
            &state,
            Duration::from_millis(10),
        )
        .expect("valid test router should build");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/proxy/bilibili/ver1/playlist.m3u8")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            response.status(),
            StatusCode::REQUEST_TIMEOUT,
            "finite provider proxy routes must remain protected by the unary HTTP timeout"
        );

        let streaming_response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/proxy/rtmp/ver1/playlist.m3u8")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            streaming_response.status(),
            StatusCode::OK,
            "live provider proxy routes must remain outside the unary HTTP timeout"
        );
    }

    #[tokio::test]
    async fn test_provider_common_routes_use_read_rate_limit_tier() {
        let state = test_app_state_with_rate_limits(
            synctv_core::HttpRateLimitConfig {
                read_max_requests: 1,
                read_window_seconds: 60,
                auth_max_requests: 100,
                auth_window_seconds: 60,
                ..synctv_core::HttpRateLimitConfig::default()
            },
            synctv_core::GrpcRateLimitConfig::default(),
        );
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/provider/instances")
                    .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            first.status(),
            StatusCode::UNAUTHORIZED,
            "first provider common request should reach auth before exhausting the read bucket"
        );

        let second = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/provider/instances")
                    .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            second.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "provider common routes must share the read rate-limit bucket"
        );
    }

    #[tokio::test]
    async fn test_provider_management_routes_do_not_consume_outer_read_bucket() {
        let state = test_app_state_with_rate_limits(
            synctv_core::HttpRateLimitConfig {
                read_max_requests: 1,
                read_window_seconds: 60,
                auth_max_requests: 100,
                auth_window_seconds: 60,
                ..synctv_core::HttpRateLimitConfig::default()
            },
            synctv_core::GrpcRateLimitConfig::default(),
        );
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let management = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/alist/login")
                    .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"username":"demo","password":"demo"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            management.status(),
            StatusCode::UNAUTHORIZED,
            "provider auth routes should hit their own auth limiter without consuming the outer read bucket"
        );

        let common = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/provider/instances")
                    .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            common.status(),
            StatusCode::UNAUTHORIZED,
            "provider management traffic must not drain the provider-common read bucket"
        );
    }

    #[tokio::test]
    async fn test_ticket_routes_use_write_rate_limit_tier() {
        let state = test_app_state_with_websocket_runtime(
            synctv_core::HttpRateLimitConfig {
                write_max_requests: 1,
                write_window_seconds: 60,
                read_max_requests: 100,
                read_window_seconds: 60,
                ..synctv_core::HttpRateLimitConfig::default()
            },
            synctv_core::GrpcRateLimitConfig::default(),
        )
        .await;
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tickets")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"room_id":"room1234_abx"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(first.status(), StatusCode::UNAUTHORIZED);

        let second = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tickets")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"room_id":"room1234_abx"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            second.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "ticket creation must use the write rate-limit bucket"
        );
    }

    #[tokio::test]
    async fn test_ticket_route_fails_closed_when_websocket_runtime_is_unavailable() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tickets")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"room_id":"room1234_abx"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "ticket issuance must fail closed with service unavailable when websocket runtime dependencies are unavailable"
        );
    }

    #[tokio::test]
    async fn test_websocket_ticket_runtime_middleware_does_not_leak_to_other_write_routes() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/user")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"username":"patched-name"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "write routes unrelated to ticket issuance must keep their normal auth path when websocket runtime dependencies are unavailable"
        );
    }

    #[tokio::test]
    async fn test_publish_key_route_is_namespaced_under_api() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state).with_state(test_app_state());

        let api_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/rooms/room1234_abx/movies/media123/live/publish-key")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(api_response.status(), StatusCode::UNAUTHORIZED);

        let removed_route_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/rooms/room1234_abx/movies/media123/live/publish-key")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(removed_route_response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_oauth2_routes_fail_closed_when_service_missing() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/oauth2/providers")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_email_routes_fail_closed_when_services_missing() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/email/verify/send")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"email":"test@example.com"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_notification_routes_fail_closed_when_service_missing() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let read_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/notifications")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(read_response.status(), StatusCode::UNAUTHORIZED);

        let write_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/notifications/read-all")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(write_response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_live_provider_routes_are_not_registered_when_infrastructure_missing() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/rtmp/rooms/room1234_abx/streams")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_websocket_routes_fail_closed_when_dependencies_missing() {
        let state = test_app_state();
        let app = register_all_routes_for_test(state.clone()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/ws/rooms/room1234_abx")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "websocket route must fail closed before auth/query validation when runtime dependencies are unavailable"
        );
    }
}
