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
use crate::runtime::RealtimeEventService;
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
use synctv_livestream::LiveStreamingInfrastructure;
use synctv_realtime::sync::ConnectionRuntime;
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
    pub connection_manager: Arc<dyn ConnectionRuntime>,
    pub presence_service: Arc<synctv_core::service::OnlinePresenceService>,
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
    /// Shared provider playback store registry reused across transports.
    pub shared_provider_stores: Arc<dyn synctv_core::provider::store::ProviderStoreResolver>,
    /// Shared proxy signing key reused across transports.
    pub shared_proxy_signing_key: Arc<ProxySigningKey>,
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
    /// Providers manager for playback generation and provider HTTP APIs.
    pub providers_manager: Arc<synctv_core::service::ProvidersManager>,
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
    pub rate_limit_config: Arc<synctv_core::RequestRateLimitConfig>,
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
    pub public_id_codec: Arc<synctv_core::PublicIdCodec>,
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
    #[cfg(test)]
    test_database_leases: Arc<std::sync::Mutex<Vec<synctv_core_testing::TestDatabase>>>,
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
    #[cfg(test)]
    pub(crate) fn with_test_database_leases(
        mut self,
        leases: Vec<synctv_core_testing::TestDatabase>,
    ) -> Self {
        self.test_database_leases = Arc::new(std::sync::Mutex::new(leases));
        self
    }

    #[cfg(test)]
    pub(crate) fn with_shared_test_database_leases(
        mut self,
        leases: Arc<std::sync::Mutex<Vec<synctv_core_testing::TestDatabase>>>,
    ) -> Self {
        self.test_database_leases = leases;
        self
    }

    #[cfg(test)]
    pub(crate) fn test_database_leases(
        &self,
    ) -> Arc<std::sync::Mutex<Vec<synctv_core_testing::TestDatabase>>> {
        Arc::clone(&self.test_database_leases)
    }

    #[cfg(test)]
    pub(crate) fn with_added_test_database_lease(
        self,
        lease: synctv_core_testing::TestDatabase,
    ) -> Self {
        match self.test_database_leases.lock() {
            Ok(mut leases) => leases.push(lease),
            Err(error) => {
                tracing::warn!(error = %error, "test database lease lock was poisoned");
            }
        }
        self
    }

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
    let router = register_all_routes();
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
        #[cfg(test)]
        test_database_leases: Arc::new(std::sync::Mutex::new(Vec::new())),
    })
}

pub(crate) fn build_shared_api_runtime(config: &RouterConfig) -> anyhow::Result<SharedApiRuntime> {
    let redis_runtime = config.redis_runtime.clone();
    let proxy_signing_key = config.shared_proxy_signing_key.clone();
    let provider_stores = config.shared_provider_stores.clone();
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
            token_blacklist: config.user_service.token_blacklist_store(),
            key_builder: config.user_service.key_builder().clone(),
        },
    ));

    let jwt_validator = Arc::new(synctv_core::service::auth::JwtValidator::new(Arc::new(
        config.jwt_service.clone(),
    )));
    let public_id_codec = Arc::new(
        synctv_core::PublicIdCodec::from_config(&config.config.public_ids)
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
            settings_registry: config.settings_registry.clone(),
            public_id_codec: public_id_codec.clone(),
            chat_service: config.chat_service.clone(),
            provider_stores: provider_stores.clone(),
            email_api: email_api.clone(),
            passkey_service: config.passkey_service.clone(),
        },
        crate::impls::ClientApiRuntime {
            realtime_fanout: config.realtime_fanout_service.clone(),
            realtime_event_service: realtime_event_service.clone(),
            chat_event_dispatcher,
            redis_runtime: redis_runtime.clone(),
            builtin_stun_url: config.builtin_stun_url.clone(),
            webrtc_status: config.webrtc_status.clone(),
            provider_access_service: provider_access_service.clone(),
            signing_key: proxy_signing_key.clone(),
            presence_service: config.presence_service.clone(),
            request_executor: request_executor.clone(),
            ws_ticket_service: config.ws_ticket_service.clone(),
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
                    synctv_core::service::RuntimeEmailConfigProvider::new(&settings_registry),
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
                provider_stores: provider_stores.clone(),
                provider_access_service: provider_access_service.clone(),
                signing_key: proxy_signing_key.clone(),
                presence_service: config.presence_service.clone(),
                request_executor: request_executor.clone(),
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
            request_executor: request_executor.clone(),
        },
    ));

    let provider_api_runtime = crate::impls::ProviderApiRuntime {
        access_service: provider_access_service.clone(),
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
    let proxy_provider_registry = config.providers_manager.proxy_registry();

    // Create ProxyServices for unified proxy handler (gives providers DB access)
    let proxy_services = Arc::new(ProxyServices {
        room_service: config.room_service.clone(),
        credential_encryption: config.credential_encryption.clone(),
        credential_repo: credential_repo.clone(),
        provider_access_service: provider_access_service.clone(),
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

    /// Chat attachment database uploads: match the service-level attachment cap.
    pub const CHAT_ATTACHMENT: usize = 50 * 1024 * 1024;

    /// User avatar database uploads: match the service-level avatar cap.
    pub const USER_AVATAR: usize = 5 * 1024 * 1024;

    /// Cover database uploads: match the service-level cover cap.
    pub const COVER: usize = 10 * 1024 * 1024;
}

/// Authentication routes that are mounted inside the strict rate-limit group.
/// Strict rate limiting: 5 req/min. Body limit: 64 KB.
fn register_auth_routes() -> Router<AppState> {
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
fn register_media_routes() -> Router<AppState> {
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
            post(room::create_media_cover_upload_session),
        )
        .route(
            "/api/rooms/{room_id}/media/{media_id}/cover",
            axum::routing::put(room::update_media_cover).delete(room::clear_media_cover),
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
fn register_write_routes() -> Router<AppState> {
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
            axum::routing::patch(room::update_playback_state),
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
            "/api/rooms/{room_id}/chat/attachments/upload-session",
            post(room::create_chat_attachment_upload_session),
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
fn register_read_routes() -> Router<AppState> {
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
            "/api/rooms/{room_id}/chat/messages/{message_id}/read-receipts",
            get(room::get_chat_message_read_receipts),
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

fn register_chat_attachment_object_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/chat/attachment-objects/{encoded_object_key}",
            axum::routing::put(room::upload_chat_attachment_object)
                .get(room::get_chat_attachment_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            body_limits::CHAT_ATTACHMENT,
        ))
}

fn register_media_cover_object_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/media/cover-objects/{encoded_object_key}",
            axum::routing::put(room::upload_media_cover_object).get(room::get_media_cover_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::COVER))
}

fn register_room_cover_object_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/room/cover-objects/{encoded_object_key}",
            axum::routing::put(room::upload_room_cover_object).get(room::get_room_cover_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::COVER))
}

fn register_playlist_cover_object_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/playlist/cover-objects/{encoded_object_key}",
            axum::routing::put(room::upload_playlist_cover_object)
                .get(room::get_playlist_cover_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::COVER))
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
fn register_websocket_routes() -> Router<AppState> {
    Router::new().route(
        "/ws/rooms/{room_id}",
        axum::routing::get(websocket::websocket_handler),
    )
}

fn register_all_routes() -> Router<AppState> {
    let mut router = Router::new()
        .merge(health::create_health_router())
        .merge(openapi_router())
        .merge(public::create_public_router())
        .merge(register_extracted_auth_routes())
        .merge(register_auth_routes())
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
                )
                .route(
                    "/api/rooms/{room_id}/reports",
                    get(room::list_room_content_reports).post(room::report_content),
                )
                .route(
                    "/api/rooms/{room_id}/reports/{report_id}",
                    get(room::get_room_content_report),
                )
                .route(
                    "/api/rooms/{room_id}/reports/{report_id}/status",
                    post(room::update_room_content_report_status),
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
        .merge(register_media_routes())
        .merge(register_write_routes())
        .merge(register_read_routes())
        .merge(register_chat_attachment_object_routes())
        .merge(register_media_cover_object_routes())
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
                .merge(register_provider_management_routes())
                .merge(Router::new().nest(
                    "/api/providers",
                    providers::common::register_common_routes(),
                )),
        )
        .route(
            "/api/providers/proxy/{provider_name}/{*sub_path}",
            get(providers::unified_proxy_handler)
                .head(providers::unified_proxy_head_handler)
                .options(providers::proxy_options_preflight),
        )
        .merge(register_websocket_routes());

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

fn register_provider_management_routes() -> Router<AppState> {
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
mod tests;
