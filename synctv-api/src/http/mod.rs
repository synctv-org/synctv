// HTTP/JSON REST API

pub mod admin;
pub(crate) mod admin_execute;
pub mod auth;
pub mod email;
pub mod error;
pub mod health;
pub mod metrics_auth;
pub mod middleware;
pub mod notifications;
pub mod oauth2;
pub mod public;
pub mod room;
pub mod room_extra;
pub mod ticket;
pub mod user;
pub mod validation;
pub mod webrtc;
pub mod websocket;

// Provider HTTP routes
// Provider-specific HTTP endpoints are registered from provider instances
pub mod providers;

use crate::realtime_fanout::RealtimeFanoutService;
use crate::runtime::{RealtimeConnectionService, RealtimeEventService};
use axum::{
    http::{HeaderMap, HeaderName, HeaderValue, Method},
    middleware as axum_middleware,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use std::sync::{Arc, LazyLock};
use synctv_core::provider::proxy::ProxyServices;
use synctv_core::provider::ProviderSet;
use synctv_core::proxy_signature::ProxySigningKey;
use synctv_core::repository::UserProviderCredentialRepository;
use synctv_core::service::{RemoteProviderManager, RoomService, UserService};
use synctv_livestream::api::LiveStreamingInfrastructure;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;
use tower_http::on_early_drop::{EarlyDropsAsFailures, OnEarlyDropLayer};
use tower_http::trace::{DefaultOnFailure, TraceLayer};

pub use error::{AppError, AppResult};

pub(crate) fn required_header_str<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
    missing_message: &'static str,
) -> AppResult<&'a str> {
    let value = headers
        .get(name)
        .ok_or_else(|| AppError::bad_request(missing_message))
        .and_then(|value| {
            value
                .to_str()
                .map_err(|_| AppError::bad_request(format!("Invalid {name} header")))
        })?;
    if value.trim().is_empty() {
        return Err(AppError::bad_request(missing_message));
    }
    Ok(value)
}

pub(crate) fn optional_header_str<'a>(
    headers: &'a HeaderMap,
    name: &'static HeaderName,
) -> AppResult<Option<&'a str>> {
    headers
        .get(name)
        .map(|value| {
            value
                .to_str()
                .map_err(|_| AppError::bad_request(format!("Invalid {name} header")))
        })
        .transpose()
}

static X_FORWARDED_PROTO: LazyLock<HeaderName> =
    LazyLock::new(|| HeaderName::from_static("x-forwarded-proto"));

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
    pub event_service: Arc<dyn RealtimeEventService>,
    pub connection_manager: Arc<dyn RealtimeConnectionService>,
    pub jwt_service: synctv_core::service::JwtService,
    pub realtime_fanout_service: Arc<dyn RealtimeFanoutService>,
    pub oauth2_service: Option<Arc<synctv_core::service::OAuth2Service>>,
    pub passkey_service: Option<Arc<synctv_core::service::PasskeyService>>,
    pub settings_service: Option<Arc<synctv_core::service::SettingsService>>,
    pub settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
    pub email_service: Option<Arc<synctv_core::service::EmailService>>,
    pub email_token_service: Option<Arc<synctv_core::service::EmailTokenService>>,
    pub publish_key_service: Option<Arc<dyn synctv_core::service::StreamingPublishKeyService>>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub chat_service: Option<Arc<synctv_core::service::ChatService>>,
    pub audit_service: Arc<synctv_core::service::AuditService>,
    pub live_streaming_infrastructure: Option<Arc<LiveStreamingInfrastructure>>,
    pub rate_limiter: Arc<dyn synctv_core::service::RequestRateLimiterService>,
    /// WebSocket ticket service for secure WebSocket authentication (HTTP only)
    pub ws_ticket_service: Arc<dyn synctv_core::service::WebSocketTicketService>,
    /// Shared runtime for playback caching and other shared-state lookups.
    pub redis_runtime: Option<Arc<dyn synctv_core::RedisConnectionRuntime>>,
    /// Shared provider playback store registry reused across transports when available.
    pub shared_provider_stores:
        Option<Arc<dyn synctv_core::provider::store::ProviderStoreResolver>>,
    /// Shared proxy signing key reused across transports when available.
    pub shared_proxy_signing_key: Option<Arc<ProxySigningKey>>,
    /// Resolved built-in STUN URL (e.g. "stun:203.0.113.1:3478") from a successfully started
    /// STUN server. When `None`, the built-in STUN entry is omitted from ICE server lists.
    pub builtin_stun_url: Option<String>,
    /// Structured WebRTC/STUN runtime state exposed through health and ICE bootstrap responses.
    pub webrtc_status: synctv_core::service::WebRtcRuntimeStatus,
    /// Credential encryption for provider credential resolution
    pub credential_encryption: Option<synctv_core::credential_encryption::CredentialEncryption>,
    /// Shared proxy slice cache instance managed by the runtime.
    pub proxy_slice_cache: Arc<synctv_proxy::slice_cache::SliceCache>,
    /// Global SSRF policy used by proxy handlers.
    pub ssrf_guard: synctv_common::ssrf::SsrfGuard,
    /// Shared outbound HTTP client used by proxy handlers and cache fills.
    pub proxy_http_client: reqwest::Client,
    /// Rate limit configuration for WebSocket chat messaging.
    /// This is separate from the HTTP request rate limit config used by the
    /// shared request execution path.
    pub messaging_rate_limit_config: synctv_core::service::RateLimitConfig,
    /// Heartbeat/cache timing for real-time messaging. Production defaults are
    /// conservative; tests may inject a shorter schedule.
    pub heartbeat_schedule: crate::impls::HeartbeatSchedule,
    /// Providers manager for playback generation (media provider lookup)
    pub providers_manager: Option<Arc<synctv_core::service::ProvidersManager>>,
}

/// Shared transport-agnostic API runtime derived from `RouterConfig`.
///
/// HTTP, gRPC, and management transports reuse these instances instead of
/// constructing parallel API impls, validators, caches, or provider stores.
#[derive(Clone)]
pub struct SharedApiRuntime {
    /// Redis runtime abstraction derived from the shared connection when available.
    pub redis_runtime: Option<Arc<dyn synctv_core::RedisConnectionRuntime>>,
    /// Shared rate limit config (created once at startup, not per-request)
    pub rate_limit_config: Arc<middleware::RateLimitConfig>,
    /// Shared messaging rate limit config for WebSocket chat messages.
    pub messaging_rate_limit_config: Arc<synctv_core::service::RateLimitConfig>,
    /// Shared content filter configured at startup.
    pub content_filter: Arc<synctv_core::service::ContentFilter>,
    pub heartbeat_schedule: crate::impls::HeartbeatSchedule,
    /// Shared JWT validator (created once at startup, not per-request)
    pub jwt_validator: Arc<synctv_core::service::auth::JwtValidator>,
    /// Shared security pipeline for post-JWT checks (password, user status)
    pub security_pipeline: Arc<synctv_core::service::SecurityPipeline>,
    /// Shared sqids codec for API-facing resource identifiers.
    pub public_id_codec: Arc<crate::PublicIdCodec>,
    /// Shared impl-level request executor for auth, rate limiting, and timeout.
    pub request_executor: Arc<crate::impls::RequestExecutor>,
    // Unified API implementation layer
    pub client_api: Arc<crate::impls::ClientApiImpl>,
    pub admin_api: Option<Arc<crate::impls::AdminApiImpl>>,
    pub email_api: Option<Arc<crate::impls::EmailApiImpl>>,
    pub notification_api: Option<Arc<crate::impls::NotificationApiImpl>>,
    pub oauth2_api: Option<Arc<crate::impls::OAuth2ApiImpl>>,
    pub provider_common_api: Arc<crate::impls::ProviderCommonApiImpl>,
    // Provider API implementations are stored once in shared runtime.
    pub bilibili_api: Arc<crate::impls::BilibiliApiImpl>,
    pub alist_api: Arc<crate::impls::AlistApiImpl>,
    pub emby_api: Arc<crate::impls::EmbyApiImpl>,
    /// Repository shared by provider transports for bind lookups.
    pub user_provider_credential_repository: Arc<UserProviderCredentialRepository>,
    /// Typed provider credential/session access cache.
    pub provider_access_service: Arc<dyn synctv_core::provider::ProviderAccessService>,
    /// Per-provider stores for caching and distributed locking (lazy creation)
    pub provider_stores: Arc<dyn synctv_core::provider::store::ProviderStoreResolver>,
    /// Registry of proxy-capable providers (looked up by type name in unified proxy handler)
    pub proxy_provider_registry: Arc<synctv_core::provider::proxy::ProxyProviderRegistry>,
    /// Services available to providers during proxy resolution (DB access)
    pub proxy_services: Arc<ProxyServices>,
    /// HMAC signing key for proxy URL authentication
    pub proxy_signing_key: Arc<ProxySigningKey>,
    /// Structured WebRTC/STUN runtime state shared across transports.
    pub webrtc_status: synctv_core::service::WebRtcRuntimeStatus,
}

#[derive(Clone)]
pub struct AppState {
    /// Common service configuration (shared cheaply via `Arc`).
    pub router_config: Arc<RouterConfig>,
    /// Shared transport-agnostic runtime reused across HTTP, gRPC, and management.
    pub shared_api_runtime: Arc<SharedApiRuntime>,
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
        match &self.shared_api_runtime.redis_runtime {
            Some(runtime) => match runtime.snapshot().await {
                Ok(conn) => Some(conn),
                Err(error) => {
                    tracing::warn!(error = %error, "Redis connection snapshot failed");
                    None
                }
            },
            None => None,
        }
    }
}

/// Create the HTTP router from configuration struct
pub fn create_router_from_config(config: RouterConfig) -> anyhow::Result<axum::Router> {
    let state = create_app_state_from_config(config)?;
    let router = create_router_from_shared_state(&state)?;
    Ok(router)
}

/// Create shared `AppState` once so multiple transports can reuse the same impl instances.
pub fn create_app_state_from_config(config: RouterConfig) -> anyhow::Result<AppState> {
    build_app_state(config)
}

/// Create the HTTP router from an already constructed shared `AppState`.
pub fn create_router_from_shared_state(state: &AppState) -> anyhow::Result<axum::Router> {
    let state = state.clone();
    let router = register_all_routes(&state);
    apply_global_layers(router, &state)
}

/// Create the HTTP router and the shared application state from configuration.
pub fn create_router_with_state_from_config(
    config: RouterConfig,
) -> anyhow::Result<(axum::Router, AppState)> {
    let state = create_app_state_from_config(config)?;
    let router = create_router_from_shared_state(&state)?;
    Ok((router, state))
}

/// Build `AppState` from `RouterConfig`, creating the shared API implementation layers.
fn build_app_state(config: RouterConfig) -> anyhow::Result<AppState> {
    let shared_api_runtime = Arc::new(build_shared_api_runtime(&config)?);

    Ok(AppState {
        router_config: Arc::new(config),
        shared_api_runtime: shared_api_runtime.clone(),
        metrics_access_controller: Arc::new(metrics_auth::MetricsAccessController::new()),
    })
}

pub(crate) fn build_shared_api_runtime(config: &RouterConfig) -> anyhow::Result<SharedApiRuntime> {
    let redis_runtime = config.redis_runtime.clone();
    let proxy_signing_key = match config.shared_proxy_signing_key.clone() {
        Some(key) => key,
        None => Arc::new(
            ProxySigningKey::try_derive_from(config.config.jwt.secret.as_bytes())
                .map_err(|error| anyhow::anyhow!("Failed to derive proxy signing key: {error}"))?,
        ),
    };
    let provider_stores = config.shared_provider_stores.clone().unwrap_or_else(|| {
        synctv_core::provider::store::build_provider_store_resolver_from_profile(
            &synctv_core::SharedStateProfile::best_effort(
                redis_runtime.clone(),
                config.config.redis.key_prefix.clone(),
            ),
        )
    });
    let credential_repo = config.user_provider_credential_repository.clone();
    let provider_access_service: Arc<dyn synctv_core::provider::ProviderAccessService> = Arc::new(
        synctv_core::provider::CachedProviderAccessService::new(
            credential_repo.clone(),
            config.providers.alist.clone(),
        )
        .with_store(provider_stores.load("credentials"))
        .with_credential_encryption(config.credential_encryption.clone()),
    );

    let security_pipeline = Arc::new(synctv_core::service::SecurityPipeline::new_with_runtime(
        config.user_service.clone(),
        synctv_core::service::SecurityPipelineRuntime {
            user_cache: Some(config.user_cache.clone()),
            token_blacklist: Some(config.user_service.token_blacklist_store()),
            key_builder: Some(config.user_service.key_builder().clone()),
        },
    ));

    let jwt_validator = Arc::new(synctv_core::service::auth::JwtValidator::new(Arc::new(
        config.jwt_service.clone(),
    )));
    let public_id_codec = Arc::new(
        crate::PublicIdCodec::from_config(&config.config.public_ids)
            .map_err(|error| anyhow::anyhow!("Invalid public ID configuration: {error}"))?,
    );
    let request_executor = Arc::new(crate::impls::RequestExecutor::new(
        config.config.clone(),
        jwt_validator.clone(),
        security_pipeline.clone(),
        config.rate_limiter.clone(),
    ));

    let email_api = crate::impls::email::build_shared_email_api(
        config.user_service.clone(),
        config.email_service.clone(),
        config.email_token_service.clone(),
        config.rate_limiter.clone(),
        public_id_codec.clone(),
    );
    let realtime_event_service = config.event_service.clone();
    let chat_event_dispatcher =
        crate::chat_event_dispatcher::default_chat_event_dispatcher(realtime_event_service.clone());

    let client_api = Arc::new(crate::impls::ClientApiImpl::new_with_runtime(
        crate::impls::ClientApiConfig {
            user_service: config.user_service.clone(),
            room_service: config.room_service.clone(),
            connection_service: config.connection_manager.clone(),
            config: config.config.clone(),
            publish_key_service: config.publish_key_service.clone(),
            jwt_service: config.jwt_service.clone(),
            live_streaming_infrastructure: config.live_streaming_infrastructure.clone(),
            providers_manager: config.providers_manager.clone(),
            settings_registry: config.settings_registry.clone(),
            public_id_codec: public_id_codec.clone(),
            chat_service: config.chat_service.clone(),
            credential_encryption: config.credential_encryption.clone(),
            provider_stores: Some(provider_stores.clone()),
            email_api: email_api.clone(),
            passkey_service: config.passkey_service.clone(),
        },
        crate::impls::ClientApiRuntime {
            realtime_fanout: config.realtime_fanout_service.clone(),
            realtime_event_service: realtime_event_service.clone(),
            chat_event_dispatcher,
            redis_runtime: redis_runtime.clone(),
            rate_limiter: Some(config.rate_limiter.clone()),
            builtin_stun_url: config.builtin_stun_url.clone(),
            webrtc_status: Some(config.webrtc_status.clone()),
            credential_repo: Some(config.user_provider_credential_repository.clone()),
            provider_access_service: Some(provider_access_service.clone()),
            signing_key: Some(proxy_signing_key.clone()),
            request_executor: Some(request_executor.clone()),
            ws_ticket_service: Some(config.ws_ticket_service.clone()),
        },
    ));

    let admin_api = if let Some(settings_svc) = config.settings_service.as_ref() {
        let email_svc = if let Some(email_service) = config.email_service.clone() {
            email_service
        } else {
            let settings_registry = config.settings_registry.clone().ok_or_else(|| {
                anyhow::anyhow!("settings_registry is required to build the admin email service")
            })?;
            Arc::new(
                synctv_core::service::EmailService::new(Arc::new(
                    synctv_core::service::RuntimeEmailConfigProvider::new(settings_registry),
                ))
                .map_err(|error| {
                    anyhow::anyhow!("Failed to build runtime admin email service: {error}")
                })?,
            )
        };
        let admin_api = crate::impls::AdminApiImpl::new_with_runtime(
            crate::impls::AdminApiConfig {
                room_service: config.room_service.clone(),
                user_service: config.user_service.clone(),
                settings_service: settings_svc.clone(),
                settings_registry: config.settings_registry.clone(),
                email_service: email_svc,
                connection_service: config.connection_manager.clone(),
                provider_instance_manager: config.provider_instance_manager.clone(),
                live_streaming_infrastructure: config.live_streaming_infrastructure.clone(),
                publish_key_service: config.publish_key_service.clone(),
                config: config.config.clone(),
                audit_service: config.audit_service.clone(),
                public_id_codec: public_id_codec.clone(),
            },
            crate::impls::AdminApiRuntime {
                realtime_fanout: config.realtime_fanout_service.clone(),
                realtime_event_service: realtime_event_service.clone(),
                provider_stores: Some(provider_stores.clone()),
                provider_access_service: Some(provider_access_service.clone()),
                request_executor: Some(request_executor.clone()),
            },
        );
        Some(Arc::new(admin_api))
    } else {
        None
    };
    // Create shared NotificationApiImpl for HTTP and gRPC.
    let notification_api = config.notification_service.as_ref().map(|notif_svc| {
        Arc::new(crate::impls::NotificationApiImpl::new(
            notif_svc.clone(),
            public_id_codec.clone(),
        ))
    });

    // Create shared OAuth2ApiImpl
    let oauth2_api = config.oauth2_service.as_ref().map(|oauth2_svc| {
        Arc::new(crate::impls::OAuth2ApiImpl::new(
            oauth2_svc.clone(),
            config.user_service.clone(),
            public_id_codec.clone(),
        ))
    });

    let provider_common_api = Arc::new(crate::impls::ProviderCommonApiImpl::new_with_runtime(
        config.provider_instance_manager.clone(),
        config.user_service.clone(),
        config.audit_service.clone(),
        crate::impls::ProviderCommonApiRuntime {
            providers_manager: config.providers_manager.clone(),
            request_executor: Some(request_executor.clone()),
        },
    ));

    let provider_api_runtime = crate::impls::ProviderApiRuntime {
        access_service: Some(provider_access_service.clone()),
        event_service: config.event_service.clone(),
    };

    // Create shared provider API implementations once at startup.
    let bilibili_api = Arc::new(
        crate::impls::BilibiliApiImpl::new_with_runtime(
            config.providers.bilibili.clone(),
            credential_repo.clone(),
            config.config.jwt.secret.as_bytes(),
            provider_api_runtime.clone(),
        )
        .map_err(|error| anyhow::anyhow!("Failed to initialize Bilibili API: {error}"))?,
    );
    let alist_api = Arc::new(crate::impls::AlistApiImpl::new_with_runtime(
        config.providers.alist.clone(),
        credential_repo.clone(),
        provider_api_runtime.clone(),
    ));
    let emby_api = Arc::new(crate::impls::EmbyApiImpl::new_with_runtime(
        config.providers.emby.clone(),
        credential_repo.clone(),
        provider_api_runtime,
    ));

    // Create shared RateLimitConfig from the config file.
    let rate_limit_config = Arc::new(config.config.request_rate_limits.clone());

    // Create shared messaging rate limit config for WebSocket chat messages.
    let messaging_rate_limit_config = Arc::new(config.messaging_rate_limit_config.clone());

    // Prefer the provider graph built by ProvidersManager so playback and proxy
    // resolution share the same provider instances. Tests and fallback
    // transports without a manager still use the explicitly supplied ProviderSet.
    let proxy_provider_registry = config.providers_manager.as_ref().map_or_else(
        || Arc::new(config.providers.build_proxy_registry()),
        |manager| manager.proxy_registry(),
    );

    // Create ProxyServices for unified proxy handler (gives providers DB access)
    let proxy_services = Arc::new(ProxyServices {
        room_service: config.room_service.clone(),
        credential_encryption: config.credential_encryption.clone(),
        credential_repo: credential_repo.clone(),
        provider_access_service: Some(provider_access_service.clone()),
        signing_key: proxy_signing_key.clone(),
        public_id_codec: public_id_codec.clone(),
    });

    Ok(SharedApiRuntime {
        redis_runtime,
        rate_limit_config,
        messaging_rate_limit_config,
        content_filter: Arc::new(config.content_filter.clone()),
        heartbeat_schedule: config.heartbeat_schedule,
        jwt_validator,
        security_pipeline,
        public_id_codec,
        request_executor,
        client_api,
        admin_api,
        email_api,
        notification_api,
        oauth2_api,
        provider_common_api,
        bilibili_api,
        alist_api,
        emby_api,
        user_provider_credential_repository: credential_repo,
        provider_access_service,
        provider_stores,
        proxy_provider_registry,
        proxy_services,
        proxy_signing_key,
        webrtc_status: config.webrtc_status.clone(),
    })
}

pub fn start_proxy_cache_lifecycle(
    cache: &Arc<synctv_proxy::slice_cache::SliceCache>,
) -> ProxyCacheLifecycleRuntime {
    let manager = synctv_proxy::slice_cache::CacheLifecycleManager::new(
        cache.backend().clone(),
        cache.config().clone(),
    );
    let cancel = manager.cancellation_token();
    let handle = manager.start();
    ProxyCacheLifecycleRuntime { cancel, handle }
}

/// Body size limits for specific endpoint categories.
///
/// These are applied as `route_layer`s INSIDE the rate-limit route groups so that
/// the limit is enforced before the handler reads the body, and the global 10 MB
/// safety net remains as a fallback for routes not explicitly limited here.
pub(crate) mod body_limits {
    /// Auth endpoints: 64 KB.
    /// Typical auth JSON bodies are under 1 KB; 64 KB leaves room for OPAQUE
    /// and passkey payloads.
    pub const AUTH: usize = 64 * 1024;

    /// Room create / update / settings: 64 KB.
    pub const ROOM: usize = 64 * 1024;

    /// Media add / edit requests: 512 KB (media metadata may include longer URLs or
    /// subtitles, but should never be megabyte-scale).
    pub const MEDIA: usize = 512 * 1024;

    /// Chat image database uploads: match the service-level image cap.
    pub const CHAT_IMAGE: usize = 20 * 1024 * 1024;

    /// User avatar database uploads: match the service-level avatar cap.
    pub const USER_AVATAR: usize = 5 * 1024 * 1024;

    /// Video cover database uploads: match the service-level cover cap.
    pub const VIDEO_COVER: usize = 10 * 1024 * 1024;
}

/// Authentication routes that are mounted inside the strict rate-limit group.
/// Strict rate limiting: 5 req/min. Body limit: 64 KB.
fn register_auth_routes(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/api/oauth2/{provider}/exchange",
            post(oauth2::exchange_authorization_code),
        )
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::AUTH))
}

fn register_extracted_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/api/auth/email/confirm", post(auth::confirm_email_login))
        .route("/api/auth/guest-token", post(auth::create_guest_token))
        .route(
            "/api/auth/direct-password/register",
            post(auth::register_with_direct_password),
        )
        .route(
            "/api/auth/direct-password/login",
            post(auth::login_with_direct_password),
        )
        .route(
            "/api/auth/email/registration/request",
            post(auth::request_email_registration),
        )
        .route(
            "/api/auth/email/registration/confirm",
            post(auth::confirm_email_registration),
        )
        .route(
            "/api/auth/passkeys/registration/start",
            post(auth::start_passkey_registration),
        )
        .route(
            "/api/auth/passkeys/registration/finish",
            post(auth::finish_passkey_registration),
        )
        .route(
            "/api/auth/passkeys/login/start",
            post(auth::start_passkey_login),
        )
        .route(
            "/api/auth/passkeys/login/finish",
            post(auth::finish_passkey_login),
        )
        .route(
            "/api/auth/opaque/login/start",
            post(auth::start_opaque_login),
        )
        .route(
            "/api/auth/opaque/login/finish",
            post(auth::finish_opaque_login),
        )
        .route(
            "/api/auth/opaque/registration/start",
            post(auth::start_opaque_registration),
        )
        .route(
            "/api/auth/opaque/registration/finish",
            post(auth::finish_opaque_registration),
        )
        .route("/api/auth/email/request", post(auth::request_email_login))
        .route(
            "/api/auth/mfa/email/request",
            post(auth::request_mfa_email_code),
        )
        .route(
            "/api/auth/mfa/email/verify",
            post(auth::verify_mfa_email_code),
        )
        .route(
            "/api/auth/mfa/passkeys/start",
            post(auth::start_mfa_passkey),
        )
        .route(
            "/api/auth/mfa/passkeys/finish",
            post(auth::finish_mfa_passkey),
        )
        .route("/api/auth/refresh", post(auth::refresh_token))
        // Tighter body limit for authentication endpoints (64 KB)
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::AUTH))
}

/// Media mutation routes (add, delete, reorder, edit, batch operations).
/// Moderate rate limiting: 20 req/min. Body limit: 512 KB.
fn register_media_routes(_state: &AppState) -> Router<AppState> {
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
        .route(
            "/api/rooms/{room_id}/media/{media_id}/cover/upload-session",
            post(room::create_video_cover_upload_session),
        )
        .route(
            "/api/rooms/{room_id}/media/{media_id}/cover",
            axum::routing::put(room::update_video_cover).delete(room::clear_video_cover),
        )
        .route(
            "/api/rooms/{room_id}/cover/upload-session",
            post(room::create_room_cover_upload_session),
        )
        .route(
            "/api/rooms/{room_id}/cover",
            axum::routing::put(room::update_room_cover).delete(room::clear_room_cover),
        )
        .route(
            "/api/rooms/{room_id}/playlists/{playlist_id}/cover/upload-session",
            post(room::create_playlist_cover_upload_session),
        )
        .route(
            "/api/rooms/{room_id}/playlists/{playlist_id}/cover",
            axum::routing::put(room::update_playlist_cover).delete(room::clear_playlist_cover),
        )
        // Media metadata bodies are small (URLs, titles, subtitles)
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::MEDIA))
}

/// Write routes (room CRUD, membership, playback control, playlists, user updates).
/// Moderate rate limiting: 30 req/min. Room create/update body limit: 64 KB.
fn register_write_routes(_state: &AppState) -> Router<AppState> {
    let router = Router::new()
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
            "/api/rooms/{room_id}/password/opaque/login/start",
            post(room::start_room_password_login),
        )
        .route(
            "/api/rooms/{room_id}/password/opaque/login/finish",
            post(room::finish_room_password_login),
        )
        .route(
            "/api/rooms/{room_id}/members/@me",
            axum::routing::delete(room::leave_room),
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
            "/api/rooms/{room_id}/password/opaque/registration/start",
            axum::routing::patch(room::start_room_password_registration),
        )
        .route(
            "/api/rooms/{room_id}/password/opaque/registration/finish",
            axum::routing::patch(room::finish_room_password_registration),
        )
        .route(
            "/api/rooms/{room_id}/password",
            axum::routing::delete(room::clear_room_password),
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
            "/api/rooms/{room_id}/streams/{media_id}/kick",
            post(room::kick_room_stream),
        )
        .route(
            "/api/rooms/{room_id}/settings/reset",
            post(room::reset_room_settings),
        )
        .route(
            "/api/rooms/{room_id}/chat/messages",
            post(room::send_chat_message),
        )
        .route(
            "/api/rooms/{room_id}/chat/images/upload-session",
            post(room::create_chat_image_upload_session),
        )
        .route(
            "/api/rooms/{room_id}/chat/messages/{message_id}",
            axum::routing::patch(room::edit_chat_message),
        )
        .route(
            "/api/rooms/{room_id}/chat/messages/{message_id}",
            axum::routing::delete(room::delete_chat_message),
        )
        .route(
            "/api/rooms/{room_id}/chat/messages/{message_id}/reactions/{reaction_key}",
            axum::routing::put(room::set_chat_reaction).delete(room::clear_chat_reaction),
        )
        .route(
            "/api/rooms/{room_id}/chat/read-state",
            post(room::mark_chat_read),
        );

    router
        // Room/user write bodies should be small (room metadata, settings, passwords)
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::ROOM))
}

/// Read routes (user info, room discovery, room details, playlists, chat, media, playback).
/// Rate limited: 100 req/min.
fn register_read_routes(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/api/rooms", get(room::list_or_get_rooms))
        .route("/api/rooms/hot", get(room::get_hot_rooms))
        .route("/api/rooms/{room_id}/check", get(room::check_room))
        .route("/api/rooms/{room_id}", get(room::get_room))
        .route(
            "/api/rooms/{room_id}/settings",
            get(room::get_room_settings),
        )
        .route("/api/rooms/{room_id}/members", get(room::get_room_members))
        .route("/api/rooms/{room_id}/streams", get(room::list_room_streams))
        .route(
            "/api/rooms/{room_id}/streams/{media_id}",
            get(room::get_room_stream_info),
        )
        .route(
            "/api/rooms/{room_id}/chat/history",
            get(room::get_chat_history),
        )
        .route(
            "/api/rooms/{room_id}/chat/playback-messages",
            get(room::get_chat_playback_messages),
        )
        .route(
            "/api/rooms/{room_id}/chat/messages/{message_id}",
            get(room::get_chat_message),
        )
        .route(
            "/api/rooms/{room_id}/chat/messages/{message_id}/context",
            get(room::get_chat_message_context),
        )
        .route(
            "/api/rooms/{room_id}/chat/messages/{message_id}/reactions/{reaction_key}/users",
            get(room::list_chat_reaction_users),
        )
        .route(
            "/api/rooms/{room_id}/chat/read-state",
            get(room::get_chat_read_state),
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
        .route(
            "/api/rooms/{room_id}/watch/playback-state",
            get(room::watch_playback_state),
        )
        .route(
            "/api/rooms/{room_id}/watch/playback",
            get(room::watch_playback),
        )
        .route(
            "/api/rooms/{room_id}/watch/room-settings",
            get(room::watch_room_settings),
        )
        .route(
            "/api/rooms/{room_id}/watch/playlist-items",
            get(room::watch_playlist_items),
        )
        .route(
            "/api/rooms/{room_id}/watch/room-members",
            get(room::watch_room_members),
        )
        .route(
            "/api/rooms/{room_id}/watch/chat-events",
            get(room::watch_chat_events),
        )
}

fn register_chat_image_object_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/chat/image-objects/{encoded_object_key}",
            axum::routing::put(room::upload_chat_image_object).get(room::get_chat_image_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            body_limits::CHAT_IMAGE,
        ))
}

fn register_video_cover_object_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/video/cover-objects/{encoded_object_key}",
            axum::routing::put(room::upload_video_cover_object).get(room::get_video_cover_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            body_limits::VIDEO_COVER,
        ))
}

fn register_room_cover_object_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/room/cover-objects/{encoded_object_key}",
            axum::routing::put(room::upload_room_cover_object).get(room::get_room_cover_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            body_limits::VIDEO_COVER,
        ))
}

fn register_playlist_cover_object_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/playlist/cover-objects/{encoded_object_key}",
            axum::routing::put(room::upload_playlist_cover_object)
                .get(room::get_playlist_cover_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            body_limits::VIDEO_COVER,
        ))
}

fn register_extracted_user_routes() -> Router<AppState> {
    Router::new()
        .route("/api/user", get(user::get_me))
        .route("/api/user/rooms", get(user::list_my_rooms))
        .route("/api/user", axum::routing::patch(user::update_user))
        .route(
            "/api/user/avatar/upload-session",
            post(user::create_user_avatar_upload_session),
        )
        .route(
            "/api/user/avatar",
            axum::routing::put(user::update_user_avatar).delete(user::clear_user_avatar),
        )
        .route("/api/user/email/bind/start", post(user::start_email_bind))
        .route(
            "/api/user/email/bind/confirm",
            post(user::confirm_email_bind),
        )
        .route("/api/user/email/unbind", post(user::unbind_email))
        .route(
            "/api/user/sensitive-verification/start",
            post(user::start_sensitive_operation_verification),
        )
        .route(
            "/api/user/sensitive-verification/passkey/start",
            post(user::start_sensitive_operation_passkey),
        )
        .route(
            "/api/user/sensitive-verification/email/request",
            post(user::request_sensitive_operation_email_code),
        )
        .route(
            "/api/user/sensitive-verification/finish",
            post(user::finish_sensitive_operation_verification),
        )
        .route(
            "/api/user/preferences",
            get(user::get_user_preferences).patch(user::update_user_preferences),
        )
        .route("/api/user/passkeys", get(user::list_passkeys))
        .route(
            "/api/user/passkeys/bind/start",
            post(user::start_passkey_bind),
        )
        .route(
            "/api/user/passkeys/bind/finish",
            post(user::finish_passkey_bind),
        )
        .route(
            "/api/user/opaque-password/update/start",
            post(user::start_opaque_password_update),
        )
        .route(
            "/api/user/opaque-password/update/finish",
            post(user::finish_opaque_password_update),
        )
        .route(
            "/api/user/passkeys/{credential_id}",
            axum::routing::delete(user::delete_passkey),
        )
        .route("/api/user/account-closure", post(user::close_account))
        .route("/api/user/logout", post(auth::logout))
}

fn register_user_avatar_object_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/user/avatar-objects/{encoded_object_key}",
            axum::routing::put(user::upload_user_avatar_object).get(user::get_user_avatar_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            body_limits::USER_AVATAR,
        ))
}

/// Assemble all route groups into a single router.
fn register_websocket_routes(_state: &AppState) -> Router<AppState> {
    Router::new().route(
        "/ws/rooms/{room_id}",
        axum::routing::get(websocket::websocket_handler),
    )
}

fn register_all_routes(state: &AppState) -> Router<AppState> {
    let mut router = Router::new()
        .merge(health::create_health_router())
        .merge(openapi_router())
        .merge(public::create_public_router())
        .merge(register_extracted_auth_routes())
        .merge(register_auth_routes(state))
        .merge(register_extracted_user_routes())
        .merge(register_user_avatar_object_routes())
        .merge(
            Router::new()
                .route("/api/rooms/{room_id}/members", post(room_extra::add_member))
                .route(
                    "/api/rooms/{room_id}/reviews/joins",
                    get(room_extra::list_room_join_reviews),
                )
                .route(
                    "/api/rooms/{room_id}/reviews/joins/{request_id}/approve",
                    post(room_extra::approve_room_join_review),
                )
                .route(
                    "/api/rooms/{room_id}/reviews/joins/{request_id}/reject",
                    post(room_extra::reject_room_join_review),
                )
                .route(
                    "/api/rooms/{room_id}/members/{user_id}",
                    axum::routing::delete(room_extra::kick_member),
                )
                .route(
                    "/api/rooms/{room_id}/members/{user_id}",
                    axum::routing::patch(room_extra::set_member_permissions),
                ),
        )
        .merge(Router::new().route("/api/tickets", post(ticket::create_ticket)))
        .merge(
            Router::new()
                .route(
                    "/api/oauth2/{provider}/bind",
                    get(oauth2::get_bind_authorize_url),
                )
                .route(
                    "/api/oauth2/type/{provider}/unlink",
                    axum::routing::delete(oauth2::unlink_provider),
                )
                .route("/api/oauth2/linked", get(oauth2::get_linked_providers)),
        )
        .merge(register_media_routes(state))
        .merge(register_write_routes(state))
        .merge(register_read_routes(state))
        .merge(register_chat_image_object_routes())
        .merge(register_video_cover_object_routes())
        .merge(register_room_cover_object_routes())
        .merge(register_playlist_cover_object_routes())
        // WebRTC configuration endpoints
        .merge(Router::new().route(
            "/api/rooms/{room_id}/webrtc/ice-servers",
            get(webrtc::get_ice_servers),
        ))
        // Admin routes
        .merge(Router::new().nest("/api/admin", admin::create_admin_router()))
        // Provider routes
        .merge(
            Router::new()
                .merge(register_provider_management_routes(state))
                .merge(Router::new().nest(
                    "/api/providers",
                    providers::common::register_common_routes(),
                )),
        )
        .merge(register_provider_proxy_routes(state))
        .merge(register_websocket_routes(state));

    router = router
        .merge(notifications::create_notification_read_router())
        .merge(notifications::create_notification_write_router());

    let email_routes = email::create_email_router();
    router = router.merge(email_routes);

    router = router.merge(
        Router::new()
            .route(
                "/api/oauth2/{provider}/authorize",
                get(oauth2::get_authorize_url),
            )
            .route(
                "/api/oauth2/providers",
                get(oauth2::list_available_providers),
            ),
    );

    router =
        router.merge(Router::new().nest("/api/providers/rtmp", providers::rtmp::rtmp_routes()));

    router
}

#[cfg(feature = "openapi")]
fn openapi_router() -> Router<AppState> {
    crate::openapi::router()
}

#[cfg(not(feature = "openapi"))]
fn openapi_router() -> Router<AppState> {
    Router::new()
}

fn register_provider_management_routes(_state: &AppState) -> Router<AppState> {
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
                .nest("/api/providers/emby", providers::emby::emby_auth_routes()),
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
                .nest("/api/providers/emby", providers::emby::emby_read_routes()),
        )
}

fn register_provider_proxy_routes(state: &AppState) -> Router<AppState> {
    let _ = state;
    Router::new().route(
        "/api/providers/proxy/{provider_name}/{*sub_path}",
        get(providers::unified_proxy_handler)
            .head(providers::unified_proxy_head_handler)
            .options(providers::proxy_options_preflight),
    )
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
                axum::http::HeaderName::from_static("x-request-id"),
                axum::http::HeaderName::from_static("traceparent"),
                axum::http::HeaderName::from_static("tracestate"),
            ])
            .expose_headers([axum::http::HeaderName::from_static("x-request-id")])
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

fn forwarded_proto_is_https(
    server: &synctv_core::config::ServerConfig,
    headers: &HeaderMap,
    remote_addr: Option<std::net::IpAddr>,
) -> AppResult<bool> {
    let Some(remote_addr) = remote_addr else {
        return Ok(false);
    };
    if !server.is_trusted_proxy(&remote_addr) {
        return Ok(false);
    }

    let Some(value) = optional_header_str(headers, &X_FORWARDED_PROTO)? else {
        return Ok(false);
    };

    Ok(value.eq_ignore_ascii_case("https"))
}

/// Apply shared transport layers (CORS, body limit, security headers, HSTS,
/// request ID propagation, and tracing) and bind state.
fn apply_shared_http_layers(
    router: Router<AppState>,
    cors: CorsLayer,
    server_config: synctv_core::config::ServerConfig,
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
                let server_config = server_config.clone();
                async move {
                    let remote_addr = request
                        .extensions()
                        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                        .map(|ci| ci.0.ip());
                    let forwarded_proto_https = match forwarded_proto_is_https(
                        &server_config,
                        request.headers(),
                        remote_addr,
                    ) {
                        Ok(value) => value,
                        Err(error) => return error.into_response(),
                    };

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

fn apply_global_layers(router: Router<AppState>, state: &AppState) -> anyhow::Result<axum::Router> {
    let cors = build_cors_layer(&state.config)?;
    let server_config = state.config.server.clone();
    let hsts_value = middleware::hsts_header(63_072_000, true, false);
    Ok(
        apply_shared_http_layers(router, cors, server_config, hsts_value)
            .layer(axum_middleware::from_fn(
                crate::observability::metrics_middleware::metrics_layer,
            ))
            .layer(OnEarlyDropLayer::new(EarlyDropsAsFailures::new(
                DefaultOnFailure::default(),
            )))
            .layer(TraceLayer::new_for_http())
            .with_state(state.clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        apply_global_layers, build_app_state, build_cors_layer, optional_header_str,
        register_all_routes, required_header_str, start_proxy_cache_lifecycle, RouterConfig,
    };
    use axum::body::Body;
    use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
    use axum::{routing::get, Router};
    use bytes::Bytes;
    use http_body_util::BodyExt as _;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};
    use synctv_core::cache::{KeyBuilder, UsernameCache};
    use synctv_core::provider::ProviderSet;
    use synctv_core::proxy_signature::ProxySigningKey;
    use synctv_core::service::{
        AuditService, ContentFilter, InMemoryTokenBlacklistStore, RateLimitConfig, RateLimiter,
        RemoteProviderManager, RoomService, UserService,
    };
    use synctv_proxy::slice_cache::{SliceCache, SliceCacheBackend, SliceCacheConfig, StoredEntry};
    use tower::ServiceExt;

    #[test]
    fn required_header_str_rejects_missing_header() {
        let headers = HeaderMap::new();

        let error = required_header_str(&headers, "x-upload-token", "missing token")
            .expect_err("missing required header should fail");

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.message, "missing token");
    }

    #[test]
    fn required_header_str_rejects_blank_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-upload-token", HeaderValue::from_static("   "));

        let error = required_header_str(&headers, "x-upload-token", "missing token")
            .expect_err("blank required header should fail");

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.message, "missing token");
    }

    #[test]
    fn required_header_str_rejects_non_utf8_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-upload-token",
            HeaderValue::from_bytes(&[0xff]).expect("raw header should build"),
        );

        let error = required_header_str(&headers, "x-upload-token", "missing token")
            .expect_err("invalid required header should fail");

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("x-upload-token"));
    }

    #[test]
    fn optional_header_str_rejects_non_utf8_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_bytes(&[0xff]).expect("raw header should build"),
        );

        let error = optional_header_str(&headers, &axum::http::header::CONTENT_TYPE)
            .expect_err("invalid optional header should fail");

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("content-type"));
    }

    #[test]
    fn forwarded_proto_is_https_accepts_trusted_proxy_https() {
        let mut server = synctv_core::config::ServerConfig::default();
        server.trusted_proxies = vec!["10.0.0.0/8".to_string()];
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));

        let result = super::forwarded_proto_is_https(
            &server,
            &headers,
            Some("10.1.2.3".parse().expect("ip")),
        )
        .expect("forwarded proto should parse");

        assert!(result);
    }

    #[test]
    fn forwarded_proto_is_https_ignores_untrusted_peer() {
        let mut server = synctv_core::config::ServerConfig::default();
        server.trusted_proxies = vec!["10.0.0.0/8".to_string()];
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));

        let result = super::forwarded_proto_is_https(
            &server,
            &headers,
            Some("192.168.1.10".parse().expect("ip")),
        )
        .expect("untrusted peer should ignore forwarded proto");

        assert!(!result);
    }

    #[test]
    fn forwarded_proto_is_https_rejects_non_utf8_from_trusted_proxy() {
        let mut server = synctv_core::config::ServerConfig::default();
        server.trusted_proxies = vec!["10.0.0.0/8".to_string()];
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-proto",
            HeaderValue::from_bytes(&[0xff]).expect("raw header should build"),
        );

        let error = super::forwarded_proto_is_https(
            &server,
            &headers,
            Some("10.1.2.3".parse().expect("ip")),
        )
        .expect_err("invalid forwarded proto should fail");

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("x-forwarded-proto"));
    }

    #[test]
    fn test_path_injected_json_proto_requests_deserialize_without_injected_fields() {
        let join_room: crate::proto::client::JoinRoomRequest =
            serde_json::from_str(r"{}").expect("join room body");
        assert!(join_room.room_id.is_empty());

        let room_password_login: crate::proto::client::StartRoomPasswordLoginRequest =
            serde_json::from_str(r#"{"credential_request":"AQID"}"#)
                .expect("room password login body");
        assert!(room_password_login.room_id.is_empty());
        assert_eq!(room_password_login.credential_request, vec![1, 2, 3]);

        let edit_media: crate::proto::client::EditMediaRequest =
            serde_json::from_str(r#"{"name":"Episode 1"}"#).expect("edit media body");
        assert_eq!(edit_media.name, "Episode 1");
        assert!(edit_media.media_id.is_empty());

        let update_playlist: crate::proto::client::UpdatePlaylistRequest =
            serde_json::from_str(r#"{"name":"Season 1"}"#).expect("update playlist body");
        assert_eq!(update_playlist.name, "Season 1");
        assert!(update_playlist.playlist_id.is_empty());

        let move_playlist: crate::proto::client::MovePlaylistRequest =
            serde_json::from_str(r#"{"after_playlist_id":"pl_anchor123"}"#)
                .expect("move playlist body");
        assert!(move_playlist.playlist_id.is_empty());
        assert!(matches!(
            move_playlist.anchor,
            Some(crate::proto::client::move_playlist_request::Anchor::AfterPlaylistId(
                ref id
            )) if id == "pl_anchor123"
        ));

        let member_permissions: crate::proto::client::UpdateMemberPermissionsRequest =
            serde_json::from_str(r#"{"role":2,"added_permissions":1}"#)
                .expect("member permissions body");
        assert!(member_permissions.user_id.is_empty());
        assert_eq!(member_permissions.role, 2);
        assert_eq!(member_permissions.added_permissions, 1);

        let delete_passkey: crate::proto::client::DeletePasskeyRequest =
            serde_json::from_str(r#"{"verification_id":"verify_123"}"#)
                .expect("delete passkey body");
        assert!(delete_passkey.credential_id.is_empty());
        assert_eq!(delete_passkey.verification_id, "verify_123");
    }

    #[test]
    fn test_admin_path_injected_json_proto_requests_deserialize_without_injected_fields() {
        let user_preferences: crate::proto::admin::UpdateUserPreferencesRequest =
            serde_json::from_str(r#"{"two_factor_enabled":true}"#)
                .expect("admin user preferences body");
        assert!(user_preferences.user_id.is_empty());
        assert_eq!(user_preferences.two_factor_enabled, Some(true));

        let user_role: crate::proto::admin::UpdateUserRoleRequest =
            serde_json::from_str(r#"{"role":1}"#).expect("admin user role body");
        assert!(user_role.user_id.is_empty());
        assert_eq!(user_role.role, 1);

        let user_password: crate::proto::admin::SetUserPasswordRequest =
            serde_json::from_str(r#"{"password":"NewPassword123!","reason":"support reset"}"#)
                .expect("admin user password body");
        assert!(user_password.user_id.is_empty());
        assert_eq!(user_password.password, "NewPassword123!");
        assert_eq!(user_password.reason, "support reset");

        let user_username: crate::proto::admin::UpdateUserUsernameRequest =
            serde_json::from_str(r#"{"new_username":"new_admin_name"}"#)
                .expect("admin user username body");
        assert!(user_username.user_id.is_empty());
        assert_eq!(user_username.new_username, "new_admin_name");

        let ban_user: crate::proto::admin::BanUserRequest =
            serde_json::from_str(r#"{"reason":"spam"}"#).expect("admin ban user body");
        assert!(ban_user.user_id.is_empty());
        assert_eq!(ban_user.reason, "spam");

        let room_password: crate::proto::admin::UpdateRoomPasswordRequest =
            serde_json::from_str(r#"{"new_password":""}"#).expect("admin room password body");
        assert!(room_password.room_id.is_empty());
        assert!(room_password.new_password.is_empty());

        let ban_room: crate::proto::admin::BanRoomRequest =
            serde_json::from_str(r#"{"reason":"abuse"}"#).expect("admin ban room body");
        assert!(ban_room.room_id.is_empty());
        assert_eq!(ban_room.reason, "abuse");

        let room_settings: crate::proto::admin::UpdateRoomSettingsRequest =
            serde_json::from_str(r#"{"settings":{"room":"settings"}}"#)
                .expect("admin room settings body");
        let settings: serde_json::Value =
            serde_json::from_slice(&room_settings.settings).expect("settings json");
        assert_eq!(settings, serde_json::json!({"room":"settings"}));
    }

    #[test]
    fn test_provider_path_injected_json_proto_requests_deserialize_without_injected_fields() {
        let update_provider: crate::proto::providers::common::UpdateProviderInstanceRequest =
            serde_json::from_str(
                r#"{"endpoint":"https://provider.internal","providers":["alist"]}"#,
            )
            .expect("update provider instance body");

        assert_eq!(
            update_provider.endpoint.as_deref(),
            Some("https://provider.internal")
        );
        assert_eq!(update_provider.providers, vec!["alist".to_string()]);
    }

    pub(crate) fn test_app_state() -> super::AppState {
        test_app_state_with_rate_limits(synctv_core::RequestRateLimitConfig::default())
    }

    fn test_app_state_with_rate_limits(
        request_rate_limits: synctv_core::RequestRateLimitConfig,
    ) -> super::AppState {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
            .expect("lazy pool");
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 128, 60);
        let user_service = Arc::new(UserService::new_for_tests(
            &pool,
            synctv_core::service::JwtService::new(
                "test-secret-key-for-http-router-tests-minimum-32-chars",
            )
            .expect("jwt"),
            username_cache,
            Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
            KeyBuilder::new("test"),
            synctv_core::service::BruteForceProtection::in_memory("test".to_string()),
        ));
        let room_service = Arc::new(
            RoomService::new_for_tests(pool.clone(), (*user_service).clone())
                .expect("room service should build"),
        );
        let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
            synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
        )));
        let providers = ProviderSet::new_with_ssrf_guard(
            provider_instance_manager.clone(),
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .expect("provider set should build");
        let jwt_service = synctv_core::service::JwtService::new(
            "test-secret-key-for-http-router-tests-minimum-32-chars",
        )
        .expect("jwt");
        let (audit_service, _audit_handle) = AuditService::new(pool.clone());
        let config = synctv_core::Config {
            request_rate_limits,
            ..synctv_core::Config::default()
        };
        let router_config = RouterConfig {
            config: Arc::new(config),
            user_cache: Arc::new(synctv_core::cache::UserCache::local_only(
                128,
                60,
                300,
                "test:user:".to_string(),
            )),
            user_service,
            room_service,
            content_filter: ContentFilter::new(),
            provider_instance_manager,
            user_provider_credential_repository: Arc::new(
                synctv_core::repository::UserProviderCredentialRepository::new(pool),
            ),
            providers,
            event_service: Arc::new(crate::runtime::LocalNoopRealtimeEventService::new()),
            connection_manager: Arc::new(synctv_realtime::sync::ConnectionManager::new(
                synctv_realtime::sync::ConnectionLimits::default(),
            )),
            jwt_service,
            realtime_fanout_service: crate::realtime_fanout::disabled_realtime_fanout_service(),
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
            ws_ticket_service: Arc::new(synctv_core::service::WsTicketService::local_only(None)),
            redis_runtime: None,
            shared_provider_stores: None,
            shared_proxy_signing_key: None,
            builtin_stun_url: None,
            webrtc_status: synctv_core::service::WebRtcRuntimeStatus::peer_to_peer_stun_disabled(),
            credential_encryption: None,
            ssrf_guard: synctv_common::ssrf::SsrfGuard::strict_policy(),
            proxy_slice_cache: Arc::new(SliceCache::new(SliceCacheConfig::default())),
            proxy_http_client: synctv_proxy::build_proxy_http_client(
                synctv_common::ssrf::SsrfGuard::strict_policy(),
            )
            .expect("proxy HTTP client should build for tests"),
            messaging_rate_limit_config: RateLimitConfig::default(),
            heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
            providers_manager: None,
        };
        build_app_state(router_config).expect("test HTTP app state should build")
    }

    async fn test_app_state_with_websocket_runtime(
        request_rate_limits: synctv_core::RequestRateLimitConfig,
    ) -> super::AppState {
        let state = test_app_state_with_rate_limits(request_rate_limits);
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
            Arc::new(synctv_core::repository::ChatRepository::new(pool.clone())),
            synctv_core::service::chat::ChatRuntime {
                rate_limiter: router_config.rate_limiter.clone(),
                rate_limit_config: state
                    .shared_api_runtime
                    .messaging_rate_limit_config
                    .as_ref()
                    .clone(),
                content_filter: state.shared_api_runtime.content_filter.as_ref().clone(),
            },
            synctv_core::service::chat::ChatDependencies {
                permission_service: router_config.room_service.permission_service().clone(),
                room_settings_service,
                user_service: router_config.user_service.clone(),
                file_storage_service: Arc::new(synctv_core::service::DisabledFileStorageService),
                audit_service: None,
                notification_service: synctv_core::service::NotificationService::default(),
            },
        );
        router_config.chat_service = Some(Arc::new(chat_service));
        let realtime_manager = Arc::new(
            synctv_realtime::sync::RealtimeManager::new(synctv_realtime::sync::RealtimeConfig {
                distributed_transport_factory: None,
                message_runtime: Arc::new(synctv_realtime::sync::RoomMessageHub::new()),
                distributed_enabled: false,
                node_id: "test-node".to_string(),
                dedup_window: Duration::from_secs(30),
                critical_channel_capacity: 8,
                publish_channel_capacity: 8,
                key_prefix: "test:".to_string(),
                catchup_window_secs: 60,
                stream_max_length: 100,
                event_handler: None,
                parent_cancel_token: None,
            })
            .await
            .expect("realtime manager"),
        );
        router_config.event_service = realtime_manager;
        build_app_state(router_config).expect("test websocket HTTP app state should build")
    }

    async fn test_app_state_with_real_chat_runtime(pool: sqlx::PgPool) -> super::AppState {
        let state = test_app_state_with_rate_limits(synctv_core::RequestRateLimitConfig::default());
        let mut router_config = state.router_config.as_ref().clone();

        let username_cache =
            UsernameCache::local_only("test:http-chat:username:".to_string(), 128, 60);
        let user_service = UserService::new_for_tests(
            &pool,
            router_config.jwt_service.clone(),
            username_cache,
            Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
            KeyBuilder::new("test:http-chat"),
            synctv_core::service::BruteForceProtection::in_memory(
                "test:http-chat:auth".to_string(),
            ),
        );
        let user_service = Arc::new(user_service);

        let room_service = RoomService::new_for_tests(pool.clone(), (*user_service).clone())
            .expect("room service should build");
        let room_service = Arc::new(room_service);

        let room_settings_repo = synctv_core::repository::RoomSettingsRepository::new(pool.clone());
        let permission_service = synctv_core::service::PermissionService::new_with_runtime(
            synctv_core::repository::RoomMemberRepository::new(pool.clone()),
            synctv_core::repository::RoomRepository::new(pool.clone()),
            synctv_core::service::permission::PermissionServiceRuntime {
                room_settings_repo: Some(room_settings_repo.clone()),
                ..synctv_core::service::permission::PermissionServiceRuntime::default()
            },
        )
        .expect("permission service should build");
        let notification_service = synctv_core::service::NotificationService::default();
        let room_settings_service = synctv_core::service::RoomSettingsService::new(
            room_settings_repo,
            None,
            Arc::new(notification_service.clone()),
            None,
            None,
        );
        let chat_service = synctv_core::service::ChatService::new(
            Arc::new(synctv_core::repository::ChatRepository::new(pool.clone())),
            synctv_core::service::chat::ChatRuntime {
                rate_limiter: Arc::new(RateLimiter::local_only("test:http-chat:".to_string())),
                rate_limit_config: RateLimitConfig::default(),
                content_filter: ContentFilter::new(),
            },
            synctv_core::service::chat::ChatDependencies {
                permission_service,
                room_settings_service,
                user_service: Arc::clone(&user_service),
                file_storage_service: Arc::new(synctv_core::service::DisabledFileStorageService),
                audit_service: None,
                notification_service,
            },
        );

        let realtime_manager = Arc::new(
            synctv_realtime::sync::RealtimeManager::new(synctv_realtime::sync::RealtimeConfig {
                distributed_transport_factory: None,
                message_runtime: Arc::new(synctv_realtime::sync::RoomMessageHub::new()),
                distributed_enabled: false,
                node_id: "test-http-chat-node".to_string(),
                dedup_window: Duration::from_secs(30),
                critical_channel_capacity: 8,
                publish_channel_capacity: 8,
                key_prefix: "test:http-chat:".to_string(),
                catchup_window_secs: 60,
                stream_max_length: 100,
                event_handler: None,
                parent_cancel_token: None,
            })
            .await
            .expect("realtime manager"),
        );

        router_config.user_service = user_service;
        router_config.room_service = room_service;
        router_config.chat_service = Some(Arc::new(chat_service));
        router_config.event_service = realtime_manager;
        router_config.connection_manager = Arc::new(synctv_realtime::sync::ConnectionManager::new(
            synctv_realtime::sync::ConnectionLimits::default(),
        ));
        router_config.audit_service = Arc::new(AuditService::new_unbuffered(pool.clone()));
        router_config.user_provider_credential_repository =
            Arc::new(synctv_core::repository::UserProviderCredentialRepository::new(pool));

        build_app_state(router_config).expect("test chat HTTP app state should build")
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

        let lifecycle = start_proxy_cache_lifecycle(&cache);

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

        let lifecycle = start_proxy_cache_lifecycle(&cache);
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
        let user_service = Arc::new(UserService::new_for_tests(
            &pool,
            synctv_core::service::JwtService::new(
                "test-secret-key-for-http-router-tests-minimum-32-chars",
            )
            .expect("jwt"),
            username_cache,
            Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
            KeyBuilder::new("test"),
            synctv_core::service::BruteForceProtection::in_memory("test".to_string()),
        ));
        let room_service = Arc::new(
            RoomService::new_for_tests(pool.clone(), (*user_service).clone())
                .expect("room service should build"),
        );
        let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
            synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
        )));
        let providers = ProviderSet::new_with_ssrf_guard(
            provider_instance_manager.clone(),
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .expect("provider set should build");
        let jwt_service = synctv_core::service::JwtService::new(
            "test-secret-key-for-http-router-tests-minimum-32-chars",
        )
        .expect("jwt");
        let (audit_service, _audit_handle) = AuditService::new(pool);
        let injected_cache = Arc::new(SliceCache::new(SliceCacheConfig {
            enabled: false,
            ..SliceCacheConfig::default()
        }));
        let injected_provider_stores: Arc<dyn synctv_core::provider::store::ProviderStoreResolver> =
            Arc::new(
                synctv_core::provider::store::ProviderStoreRegistry::local_only("shared:test:"),
            );
        let injected_proxy_signing_key = Arc::new(
            ProxySigningKey::try_derive_from(
                b"test-secret-key-for-http-router-tests-minimum-32-chars",
            )
            .expect("test proxy signing key should derive"),
        );
        let injected_proxy_http_client =
            synctv_proxy::build_proxy_http_client(synctv_common::ssrf::SsrfGuard::strict_policy())
                .expect("proxy HTTP client should build for tests");

        let state = build_app_state(RouterConfig {
            config: Arc::new(synctv_core::Config::default()),
            user_service,
            user_cache: Arc::new(synctv_core::cache::UserCache::local_only(
                128,
                60,
                300,
                "test:user:".to_string(),
            )),
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
            event_service: Arc::new(crate::runtime::LocalNoopRealtimeEventService::new()),
            connection_manager: Arc::new(synctv_realtime::sync::ConnectionManager::new(
                synctv_realtime::sync::ConnectionLimits::default(),
            )),
            jwt_service,
            realtime_fanout_service: crate::realtime_fanout::disabled_realtime_fanout_service(),
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
            ws_ticket_service: Arc::new(synctv_core::service::WsTicketService::local_only(None)),
            redis_runtime: None,
            shared_provider_stores: Some(injected_provider_stores.clone()),
            shared_proxy_signing_key: Some(injected_proxy_signing_key.clone()),
            builtin_stun_url: None,
            webrtc_status: synctv_core::service::WebRtcRuntimeStatus::peer_to_peer_stun_disabled(),
            credential_encryption: None,
            proxy_slice_cache: injected_cache.clone(),
            ssrf_guard: synctv_common::ssrf::SsrfGuard::strict_policy(),
            proxy_http_client: injected_proxy_http_client,
            messaging_rate_limit_config: RateLimitConfig::default(),
            heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
            providers_manager: None,
        })
        .expect("test HTTP app state should build");

        assert!(
            Arc::ptr_eq(&state.proxy_slice_cache, &injected_cache),
            "AppState must reuse the injected proxy slice cache instead of creating a hidden default instance"
        );
        assert!(
            !state.proxy_slice_cache.config().enabled,
            "The injected cache configuration must be preserved"
        );
        assert!(
            Arc::ptr_eq(
                &state.shared_api_runtime.provider_stores,
                &injected_provider_stores
            ),
            "AppState must reuse the injected provider store registry"
        );
        assert!(
            Arc::ptr_eq(
                &state.shared_api_runtime.proxy_signing_key,
                &injected_proxy_signing_key
            ),
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
            state.shared_api_runtime.security_pipeline.has_user_cache(),
            "AppState security pipeline should carry the shared user cache"
        );
    }

    #[tokio::test]
    async fn test_build_app_state_wires_user_cache_into_security_pipeline() {
        let state = test_app_state();
        assert!(
            state.shared_api_runtime.security_pipeline.has_user_cache(),
            "build_app_state should wire the shared user cache into the auth security pipeline"
        );
    }

    #[tokio::test]
    async fn test_build_app_state_wires_blacklist_into_security_pipeline() {
        let state = test_app_state();
        assert!(
            state
                .shared_api_runtime
                .security_pipeline
                .has_blacklist_store(),
            "build_app_state should wire token blacklist configuration through the builder"
        );
    }

    #[tokio::test]
    async fn test_playback_patch_route_is_reachable_via_project_router() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/rooms/room_123/playback")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"state":1}"#))
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
    async fn test_chat_message_patch_route_is_reachable_via_project_router() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/rooms/room_123/chat/messages/msg_456")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"content":"edited","expected_version":"1"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "chat message PATCH route must be registered in the project router"
        );
        assert_ne!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "chat message PATCH route must accept PATCH requests"
        );
        assert!(
            matches!(
                response.status(),
                StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED
            ),
            "request should reach the registered route and be handled by the normal request pipeline"
        );
    }

    #[tokio::test]
    async fn test_chat_message_delete_route_is_reachable_via_project_router() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/rooms/room_123/chat/messages/msg_456")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"expected_version":"1","reason":"cleanup"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "chat message DELETE route must be registered in the project router"
        );
        assert_ne!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "chat message DELETE route must accept DELETE requests"
        );
        assert!(
            matches!(
                response.status(),
                StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED
            ),
            "request should reach the registered route and be handled by the normal request pipeline"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_chat_events_sse_receives_live_send_event() {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let state = test_app_state_with_real_chat_runtime(pool.clone()).await;
        let now = chrono::Utc::now();
        let owner = synctv_core::repository::UserRepository::new(pool)
            .create(&synctv_core::models::User {
                id: synctv_core::models::UserId::new(),
                username: "http_sse_chat_live_owner".to_string(),
                role: synctv_core::models::UserRole::User,
                avatar_file_reference_id: None,
                status: synctv_core::models::UserStatus::Active,
                signup_method: synctv_core::models::SignupMethod::Email,
                created_at: now,
                updated_at: now,
                version: 0,
                deleted_at: None,
                is_banned: false,
                banned_at: None,
                banned_by: None,
                banned_reason: None,
            })
            .await
            .expect("owner should be created");
        let access_token = state
            .jwt_service
            .sign_access_token(&owner.id, 0)
            .expect("access token should sign");
        let (room, _) = state
            .room_service
            .create_room(
                "HTTP SSE Chat Live Room".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room should be created");
        let public_room_id = state
            .shared_api_runtime
            .public_id_codec
            .encode_room_id(room.id)
            .expect("room id should encode");
        let app = register_all_routes(&state).with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/api/rooms/{public_room_id}/watch/chat-events?format=json"
                    ))
                    .header(
                        axum::http::header::AUTHORIZATION,
                        format!("Bearer {access_token}"),
                    )
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        let first_frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
            .await
            .expect("initial SSE frame should arrive")
            .expect("SSE stream should remain open")
            .expect("SSE frame should be readable");
        let mut rendered = String::new();
        if let Some(data) = first_frame.data_ref() {
            rendered.push_str(std::str::from_utf8(data).expect("SSE frame should be utf-8"));
        }
        assert!(rendered.contains("event: observed\n"));

        let sent = state
            .shared_api_runtime
            .client_api
            .send_chat_message_for_actor(
                &crate::impls::client::RoomActor::User {
                    room_id: room.id,
                    user_id: owner.id,
                },
                crate::proto::client::SendChatMessageRequest {
                    client_message_id: "http-sse-live-send-1".to_string(),
                    content: "live push event".to_string(),
                    metadata: br"{}".to_vec(),
                    ..Default::default()
                },
            )
            .await
            .expect("chat send should succeed")
            .event
            .expect("chat send should return event");

        for _ in 0..8 {
            let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
                .await
                .expect("SSE frame should arrive")
                .expect("SSE stream should remain open")
                .expect("SSE frame should be readable");
            if let Some(data) = frame.data_ref() {
                rendered.push_str(std::str::from_utf8(data).expect("SSE frame should be utf-8"));
            }
            if rendered.contains("live push event") {
                break;
            }
        }

        assert!(rendered.contains("event: changed\n"));
        assert!(rendered.contains(&format!("id: {}\n", sent.sequence)));
        assert!(rendered.contains("live push event"));
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_chat_events_sse_replays_after_last_event_id_header() {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let state = test_app_state_with_real_chat_runtime(pool.clone()).await;
        let now = chrono::Utc::now();
        let owner = synctv_core::repository::UserRepository::new(pool)
            .create(&synctv_core::models::User {
                id: synctv_core::models::UserId::new(),
                username: "http_sse_chat_owner".to_string(),
                role: synctv_core::models::UserRole::User,
                avatar_file_reference_id: None,
                status: synctv_core::models::UserStatus::Active,
                signup_method: synctv_core::models::SignupMethod::Email,
                created_at: now,
                updated_at: now,
                version: 0,
                deleted_at: None,
                is_banned: false,
                banned_at: None,
                banned_by: None,
                banned_reason: None,
            })
            .await
            .expect("owner should be created");
        let access_token = state
            .jwt_service
            .sign_access_token(&owner.id, 0)
            .expect("access token should sign");
        let (room, _) = state
            .room_service
            .create_room(
                "HTTP SSE Chat Replay Room".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room should be created");
        let chat_service = state.chat_service.as_ref().expect("chat service").clone();

        let first = chat_service
            .send_message_event(synctv_core::models::SendChatMessage {
                room_id: room.id,
                user_id: owner.id,
                client_message_id: Some("http-sse-chat-1".to_string()),
                content: "first replay".to_string(),
                message_type: synctv_core::models::ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await
            .expect("first message should be stored");
        chat_service
            .send_message_event(synctv_core::models::SendChatMessage {
                room_id: room.id,
                user_id: owner.id,
                client_message_id: Some("http-sse-chat-2".to_string()),
                content: "second replay".to_string(),
                message_type: synctv_core::models::ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await
            .expect("second message should be stored");
        chat_service
            .send_message_event(synctv_core::models::SendChatMessage {
                room_id: room.id,
                user_id: owner.id,
                client_message_id: Some("http-sse-chat-3".to_string()),
                content: "third replay".to_string(),
                message_type: synctv_core::models::ChatMessageType::Text,
                reply_to_message_id: None,
                metadata: serde_json::Value::Object(Default::default()),
                images: Vec::new(),
            })
            .await
            .expect("third message should be stored");

        let public_room_id = state
            .shared_api_runtime
            .public_id_codec
            .encode_room_id(room.id)
            .expect("room id should encode");
        let app = register_all_routes(&state).with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/api/rooms/{public_room_id}/watch/chat-events?format=json"
                    ))
                    .header(
                        axum::http::header::AUTHORIZATION,
                        format!("Bearer {access_token}"),
                    )
                    .header("last-event-id", first.sequence.to_string())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        let mut rendered = String::new();
        for _ in 0..8 {
            let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
                .await
                .expect("SSE frame should arrive")
                .expect("SSE stream should remain open")
                .expect("SSE frame should be readable");
            if let Some(data) = frame.data_ref() {
                rendered.push_str(std::str::from_utf8(data).expect("SSE frame should be utf-8"));
            }
            if rendered.contains("second replay") && rendered.contains("third replay") {
                break;
            }
        }

        assert!(rendered.contains("event: observed\n"));
        assert!(rendered.contains("event: changed\n"));
        assert!(rendered.contains("id: "));
        assert!(rendered.contains("second replay"));
        assert!(rendered.contains("third replay"));
        assert!(
            !rendered.contains("first replay"),
            "Last-Event-ID should replay events strictly after the supplied sequence"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_chat_events_sse_unknown_last_event_id_returns_bad_request() {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let state = test_app_state_with_real_chat_runtime(pool.clone()).await;
        let now = chrono::Utc::now();
        let owner = synctv_core::repository::UserRepository::new(pool)
            .create(&synctv_core::models::User {
                id: synctv_core::models::UserId::new(),
                username: "http_sse_chat_bad_cursor_owner".to_string(),
                role: synctv_core::models::UserRole::User,
                avatar_file_reference_id: None,
                status: synctv_core::models::UserStatus::Active,
                signup_method: synctv_core::models::SignupMethod::Email,
                created_at: now,
                updated_at: now,
                version: 0,
                deleted_at: None,
                is_banned: false,
                banned_at: None,
                banned_by: None,
                banned_reason: None,
            })
            .await
            .expect("owner should be created");
        let access_token = state
            .jwt_service
            .sign_access_token(&owner.id, 0)
            .expect("access token should sign");
        let (room, _) = state
            .room_service
            .create_room(
                "HTTP SSE Chat Bad Cursor Room".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room should be created");
        let public_room_id = state
            .shared_api_runtime
            .public_id_codec
            .encode_room_id(room.id)
            .expect("room id should encode");
        let app = register_all_routes(&state).with_state(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/api/rooms/{public_room_id}/watch/chat-events?format=json"
                    ))
                    .header(
                        axum::http::header::AUTHORIZATION,
                        format!("Bearer {access_token}"),
                    )
                    .header("last-event-id", "missing-chat-sequence")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_public_rooms_route_is_reachable_without_auth() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

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
    async fn test_opaque_login_routes_are_registered() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

        for uri in [
            "/api/auth/opaque/login/start",
            "/api/auth/opaque/login/finish",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header(axum::http::header::CONTENT_TYPE, "application/json")
                        .body(Body::from("{"))
                        .expect("request"),
                )
                .await
                .expect("response");

            assert_ne!(response.status(), StatusCode::NOT_FOUND, "{uri} is missing");
            assert_ne!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{uri} must accept POST"
            );
        }
    }

    #[tokio::test]
    async fn test_direct_password_and_email_registration_routes_are_registered() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

        for uri in [
            "/api/auth/direct-password/register",
            "/api/auth/direct-password/login",
            "/api/auth/email/registration/request",
            "/api/auth/email/registration/confirm",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header(axum::http::header::CONTENT_TYPE, "application/json")
                        .body(Body::from("{"))
                        .expect("request"),
                )
                .await
                .expect("response");

            assert_ne!(response.status(), StatusCode::NOT_FOUND, "{uri} is missing");
            assert_ne!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{uri} must accept POST"
            );
        }
    }

    #[tokio::test]
    async fn test_passkey_login_routes_fail_closed_when_service_missing() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

        let start_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/passkeys/login/start")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r"{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(start_response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let finish_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/passkeys/login/finish")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"session_id":"session","credential":{"id":"cred","type":"public-key"}}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(finish_response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_passkey_user_routes_are_registered_and_require_authentication() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

        for (method, uri, body) in [
            ("GET", "/api/user/preferences", None),
            (
                "PATCH",
                "/api/user/preferences",
                Some(r#"{"two_factor_enabled":true}"#),
            ),
            ("GET", "/api/user/passkeys", None),
            (
                "POST",
                "/api/user/passkeys/bind/start",
                Some(r#"{"name":"Laptop"}"#),
            ),
            (
                "POST",
                "/api/user/passkeys/bind/finish",
                Some(
                    r#"{"session_id":"session","credential":{"id":"cred","type":"public-key"},"verification_id":"verification-id"}"#,
                ),
            ),
            (
                "DELETE",
                "/api/user/passkeys/Y3JlZGVudGlhbA",
                Some(r#"{"verification_id":"verification-id"}"#),
            ),
            (
                "PUT",
                "/api/rooms/room_123/chat/messages/42/reactions/like",
                None,
            ),
            (
                "DELETE",
                "/api/rooms/room_123/chat/messages/42/reactions/like",
                None,
            ),
            (
                "GET",
                "/api/rooms/room_123/chat/messages/42/reactions/like/users",
                None,
            ),
        ] {
            let mut builder = Request::builder().method(method).uri(uri);
            if body.is_some() {
                builder = builder.header(axum::http::header::CONTENT_TYPE, "application/json");
            }
            let response = app
                .clone()
                .oneshot(
                    builder
                        .body(body.map_or_else(Body::empty, Body::from))
                        .expect("request"),
                )
                .await
                .expect("response");

            assert_ne!(response.status(), StatusCode::NOT_FOUND, "{uri} is missing");
            assert_ne!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{uri} must accept {method}"
            );
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{uri} should follow the normal authenticated user route path"
            );
        }
    }

    #[tokio::test]
    async fn test_member_approval_routes_are_reachable_via_project_router() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

        for (method, uri, body) in [
            (
                "POST",
                "/api/rooms/room1234_abx/members",
                Some(r#"{"user_id":"usr_1","role":1,"notify":true}"#),
            ),
            ("GET", "/api/rooms/room1234_abx/reviews/joins", None),
            (
                "POST",
                "/api/rooms/room1234_abx/reviews/joins/AbC123xYz890/approve",
                None,
            ),
            (
                "POST",
                "/api/rooms/room1234_abx/reviews/joins/AbC123xYz890/reject",
                Some(r#"{"request_id":"usr_1","reason":"no longer eligible"}"#),
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
    async fn test_oauth2_unlink_route_is_reachable() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

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
    }

    #[tokio::test]
    async fn test_delete_all_read_notifications_route_is_reachable() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

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
        let app = register_all_routes(&state).with_state(state);

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
    async fn test_provider_login_routes_reject_invalid_tokens_before_rate_limiting() {
        let state = test_app_state_with_rate_limits(synctv_core::RequestRateLimitConfig {
            auth_max_requests: 1,
            auth_window_seconds: 60,
            read_max_requests: 100,
            read_window_seconds: 60,
            ..synctv_core::RequestRateLimitConfig::default()
        });
        let app = register_all_routes(&state).with_state(state);

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/alist/login")
                    .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"host":"https://alist.example.com","username":"demo","password":"demo"}"#,
                    ))
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
                    .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"host":"https://alist.example.com","username":"demo","password":"demo"}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            second.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "provider login routes should consume the auth rate-limit bucket before invalid-token authentication fails"
        );
    }

    #[tokio::test]
    async fn test_auth_login_malformed_json_still_consumes_auth_rate_limit_bucket() {
        let state = test_app_state_with_rate_limits(synctv_core::RequestRateLimitConfig {
            auth_max_requests: 1,
            auth_window_seconds: 60,
            read_max_requests: 100,
            read_window_seconds: 60,
            ..synctv_core::RequestRateLimitConfig::default()
        });
        let app = register_all_routes(&state).with_state(state);

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/email/confirm")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{invalid json"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(first.status(), StatusCode::BAD_REQUEST);

        let second = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/email/confirm")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{invalid json"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            second.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "malformed auth payloads should still consume the auth rate-limit bucket"
        );
    }

    #[tokio::test]
    async fn test_bilibili_me_route_requires_post() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

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
            "Bilibili /me must require POST so provider requests stay consistently structured"
        );
    }

    #[tokio::test]
    async fn test_ticket_route_uses_write_rate_limit_tier() {
        let state = test_app_state_with_websocket_runtime(synctv_core::RequestRateLimitConfig {
            write_max_requests: 1,
            write_window_seconds: 60,
            read_max_requests: 100,
            read_window_seconds: 60,
            ..synctv_core::RequestRateLimitConfig::default()
        })
        .await;
        let app = register_all_routes(&state).with_state(state);

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tickets")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"room_id":"room_123"}"#))
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
                    .body(Body::from(r#"{"room_id":"room_123"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            second.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "ticket issuance should consume the write rate-limit bucket before unauthenticated requests reach impl authentication"
        );
    }

    #[tokio::test]
    async fn test_provider_proxy_routes_use_streaming_rate_limit_tier() {
        let state = test_app_state_with_rate_limits(synctv_core::RequestRateLimitConfig {
            streaming_max_requests: 1,
            streaming_window_seconds: 60,
            read_max_requests: 100,
            read_window_seconds: 60,
            ..synctv_core::RequestRateLimitConfig::default()
        });
        let app = register_all_routes(&state).with_state(state);

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
    async fn test_transport_layers_preserve_shared_http_metadata_without_global_timeout() {
        let state = test_app_state();
        let app = apply_global_layers(
            Router::new().route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    "completed"
                }),
            ),
            &state,
        )
        .expect("valid test router should build");

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/slow")
                    .header("x-request-id", "transport-no-timeout-123")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "transport layers should no longer enforce a path-selected unary timeout"
        );
        assert_eq!(
            response
                .headers()
                .get("x-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("transport-no-timeout-123"),
            "request IDs must still be propagated without transport timeout wrapping"
        );
        assert_eq!(
            response.headers().get("X-Frame-Options").unwrap(),
            "DENY",
            "shared security headers must still be applied after removing transport timeout routing"
        );
    }

    #[tokio::test]
    async fn test_streaming_proxy_routes_preserve_options_preflight() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

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

    #[tokio::test]
    async fn test_cors_preflight_allows_request_correlation_headers() {
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
                    .header(
                        axum::http::header::ACCESS_CONTROL_REQUEST_HEADERS,
                        "x-request-id, traceparent, tracestate",
                    )
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);

        let allowed_headers = response
            .headers()
            .get(axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS)
            .expect("preflight should advertise allowed headers")
            .to_str()
            .expect("allowed headers should be valid ascii")
            .to_ascii_lowercase();
        assert!(allowed_headers.contains("x-request-id"));
        assert!(allowed_headers.contains("traceparent"));
        assert!(allowed_headers.contains("tracestate"));
    }

    #[tokio::test]
    async fn test_cors_actual_response_exposes_request_id_header() {
        let mut config = synctv_core::Config::default();
        config.server.cors_allowed_origins = vec!["https://example.com".to_string()];

        let app = Router::new()
            .route("/test", get(|| async { "ok" }))
            .layer(build_cors_layer(&config).expect("valid CORS config should build"));

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/test")
                    .header(axum::http::header::ORIGIN, "https://example.com")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let exposed_headers = response
            .headers()
            .get(axum::http::header::ACCESS_CONTROL_EXPOSE_HEADERS)
            .expect("CORS response should expose request correlation response headers")
            .to_str()
            .expect("exposed headers should be valid ascii")
            .to_ascii_lowercase();
        assert!(exposed_headers.contains("x-request-id"));
    }

    #[cfg(feature = "openapi")]
    #[tokio::test]
    async fn test_openapi_json_route_is_available() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

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
        assert!(json["paths"]["/api/auth/email/confirm"].is_object());
        assert!(json["paths"]["/api/auth/direct-password/register"].is_object());
        assert!(json["paths"]["/api/auth/direct-password/login"].is_object());
        assert!(json["paths"]["/api/auth/email/registration/request"].is_object());
        assert!(json["paths"]["/api/auth/email/registration/confirm"].is_object());
        assert!(json["paths"]["/api/tickets"].is_object());
        assert!(json["paths"]["/api/user"].is_object());
        assert!(json["paths"]["/api/rooms/{room_id}/media"].is_object());
        assert!(json["paths"]["/api/admin/users"].is_object());
        assert!(json["paths"]["/api/rooms/{room_id}/webrtc/ice-servers"].is_object());
        assert!(json["paths"]["/api/oauth2/{provider}/exchange"].is_object());
        assert!(json["paths"]["/api/oauth2/providers"].is_object());
        assert!(json["paths"]["/api/oauth2/{provider}/authorize"].is_object());
        assert!(json["paths"]["/api/notifications"].is_object());
        assert!(json["paths"]["/api/providers/bilibili/parse"].is_object());
        assert!(json["paths"]["/api/providers/alist/login"].is_object());
        assert!(json["paths"]["/api/providers/instances"].is_object());
        assert!(json["paths"]["/api/rooms/{room_id}/streams"].is_object());
        assert!(
            json["paths"]["/api/providers/rtmp/rooms/{room_id}/publish-key/{media_id}"].is_object()
        );
        assert!(json["paths"]["/api/providers/rtmp/rooms/{room_id}/info/{media_id}"].is_object());
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
            "#/components/schemas/synctv_client_SetUsernameResponse"
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
        let auth_login_ref = json["paths"]["/api/auth/email/confirm"]["post"]["responses"]["200"]
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
        let app = register_all_routes(&state).with_state(state);

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
    async fn test_provider_common_routes_rate_limit_invalid_tokens_before_authentication() {
        let state = test_app_state_with_rate_limits(synctv_core::RequestRateLimitConfig {
            admin_max_requests: 1,
            admin_window_seconds: 60,
            auth_max_requests: 100,
            auth_window_seconds: 60,
            ..synctv_core::RequestRateLimitConfig::default()
        });
        let app = register_all_routes(&state).with_state(state);

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/instances")
                    .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            first.status(),
            StatusCode::UNAUTHORIZED,
            "first provider common request should still reach authentication while the admin bucket has capacity"
        );

        let second = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/instances")
                    .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            second.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "provider common routes should consume the admin rate-limit bucket before invalid-token authentication fails"
        );
    }

    #[tokio::test]
    async fn test_provider_management_routes_do_not_consume_outer_read_bucket() {
        let state = test_app_state_with_rate_limits(synctv_core::RequestRateLimitConfig {
            read_max_requests: 1,
            read_window_seconds: 60,
            auth_max_requests: 100,
            auth_window_seconds: 60,
            ..synctv_core::RequestRateLimitConfig::default()
        });
        let app = register_all_routes(&state).with_state(state);

        let management = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/alist/login")
                    .header(axum::http::header::AUTHORIZATION, "Bearer malformed-token")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"host":"https://alist.example.com","username":"demo","password":"demo"}"#,
                    ))
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
                    .uri("/api/providers/instances")
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
        let state = test_app_state_with_websocket_runtime(synctv_core::RequestRateLimitConfig {
            write_max_requests: 1,
            write_window_seconds: 60,
            read_max_requests: 100,
            read_window_seconds: 60,
            ..synctv_core::RequestRateLimitConfig::default()
        })
        .await;
        let app = register_all_routes(&state).with_state(state);

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tickets")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"room_id":"room_123"}"#))
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
                    .body(Body::from(r#"{"room_id":"room_123"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            second.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "ticket creation should consume the write rate-limit bucket before unauthenticated requests reach impl authentication"
        );
    }

    #[tokio::test]
    async fn test_ticket_route_fails_closed_when_websocket_runtime_is_unavailable() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

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
    async fn test_websocket_route_fails_closed_when_runtime_is_unavailable_before_upgrade_checks() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/ws/rooms/AbC123xYz890")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "websocket runtime checks must fail closed before WebSocketUpgrade extraction would otherwise return 400"
        );
    }

    #[tokio::test]
    async fn test_websocket_ticket_runtime_gate_does_not_leak_to_other_write_routes() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/user")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"new_username":"patched-name"}"#))
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
    async fn test_rtmp_publish_key_routes_are_reachable_under_api() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

        let api_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/providers/rtmp/rooms/AbC123xYz890/publish-key/ZyX098wVu765")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(api_response.status(), StatusCode::UNAUTHORIZED);

        let info_api_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/providers/rtmp/rooms/AbC123xYz890/info/ZyX098wVu765")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(info_api_response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_oauth2_routes_fail_closed_when_service_missing() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

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
    async fn test_optional_user_execution_rejects_invalid_authorization_header() {
        let state = test_app_state();
        let request_meta =
            crate::impls::RequestMetadata::new(crate::impls::TransportProtocol::Http)
                .with_authorization(Some("Bearer malformed-token".to_string()))
                .with_client_ip(Some("127.0.0.1".parse().expect("ip")));

        let err = state
            .shared_api_runtime
            .request_executor
            .execute_optional_user_with_control(
                &request_meta,
                crate::impls::EndpointRateLimitCategory::Auth,
                |_control, _authenticated| async move { Ok::<_, crate::impls::ApiError>(()) },
            )
            .await
            .expect_err("invalid bearer token must be rejected on the strict optional-auth path");

        assert!(
            matches!(err.classify(), crate::impls::ErrorKind::Unauthenticated),
            "strict optional-auth execution must reject invalid bearer headers instead of downgrading to anonymous",
        );
    }

    #[tokio::test]
    async fn test_http_request_metadata_rejects_non_utf8_authorization_header() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/rooms/hot")
                    .header(
                        axum::http::header::AUTHORIZATION,
                        axum::http::HeaderValue::from_bytes(&[0xff])
                            .expect("raw header should build"),
                    )
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_http_request_metadata_rejects_non_utf8_user_agent_header() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/rooms/hot")
                    .header(
                        axum::http::header::USER_AGENT,
                        axum::http::HeaderValue::from_bytes(&[0xff])
                            .expect("raw header should build"),
                    )
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_notification_routes_fail_closed_when_service_missing() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

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
        assert_eq!(read_response.status(), StatusCode::SERVICE_UNAVAILABLE);

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
        assert_eq!(write_response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_live_provider_routes_remain_registered_when_infrastructure_missing() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/rooms/room_123/streams")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_websocket_routes_fail_closed_when_dependencies_missing() {
        let state = test_app_state();
        let app = register_all_routes(&state).with_state(state);

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
