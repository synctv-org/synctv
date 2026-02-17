// Module: http
// HTTP/JSON REST API for backward compatibility and easier integration

pub mod admin;
pub mod auth;
pub mod email_verification;
pub mod error;
pub mod health;
pub mod live;
pub mod media;
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
    http::{Method, HeaderName, HeaderValue},
    middleware as axum_middleware,
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use synctv_cluster::sync::PublishRequest;
use synctv_core::provider::{AlistProvider, BilibiliProvider, EmbyProvider};
use synctv_core::repository::UserProviderCredentialRepository;
use synctv_core::service::{RemoteProviderManager, RoomService, TokenBlacklistService, UserService};
use synctv_livestream::api::LiveStreamingInfrastructure;
use tokio::sync::mpsc;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

pub use error::{AppError, AppResult};

/// Configuration for creating the HTTP router
#[derive(Clone)]
pub struct RouterConfig {
    pub config: Arc<synctv_core::Config>,
    pub user_service: Arc<UserService>,
    pub room_service: Arc<RoomService>,
    pub provider_instance_manager: Arc<RemoteProviderManager>,
    pub user_provider_credential_repository: Arc<UserProviderCredentialRepository>,
    pub alist_provider: Arc<AlistProvider>,
    pub bilibili_provider: Arc<BilibiliProvider>,
    pub emby_provider: Arc<EmbyProvider>,
    pub cluster_manager: Option<Arc<synctv_cluster::sync::ClusterManager>>,
    pub connection_manager: Arc<synctv_cluster::sync::ConnectionManager>,
    pub jwt_service: synctv_core::service::JwtService,
    pub redis_publish_tx: Option<mpsc::Sender<PublishRequest>>,
    pub oauth2_service: Option<Arc<synctv_core::service::OAuth2Service>>,
    pub settings_service: Option<Arc<synctv_core::service::SettingsService>>,
    pub settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
    pub email_service: Option<Arc<synctv_core::service::EmailService>>,
    pub publish_key_service: Option<Arc<synctv_core::service::PublishKeyService>>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub audit_service: Arc<synctv_core::service::AuditService>,
    pub live_streaming_infrastructure: Option<Arc<LiveStreamingInfrastructure>>,
    pub sfu_manager: Option<Arc<synctv_sfu::SfuManager>>,
    /// SFU session manager for session affinity queries (multi-replica routing).
    pub sfu_session_manager: Option<Arc<synctv_sfu::SfuSessionManager>>,
    pub rate_limiter: synctv_core::service::rate_limit::RateLimiter,
    /// Token blacklist service for checking revoked tokens
    pub token_blacklist_service: TokenBlacklistService,
    /// WebSocket ticket service for secure WebSocket authentication (HTTP only)
    pub ws_ticket_service: Option<Arc<synctv_core::service::WsTicketService>>,
    /// Shared Redis connection for playback caching
    pub redis_conn: Option<redis::aio::ConnectionManager>,
}

/// Shared application state
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<synctv_core::Config>,
    pub user_service: Arc<UserService>,
    pub room_service: Arc<RoomService>,
    pub provider_instance_manager: Arc<RemoteProviderManager>,
    pub user_provider_credential_repository: Arc<UserProviderCredentialRepository>,
    pub alist_provider: Arc<AlistProvider>,
    pub bilibili_provider: Arc<BilibiliProvider>,
    pub emby_provider: Arc<EmbyProvider>,
    pub cluster_manager: Option<Arc<synctv_cluster::sync::ClusterManager>>,
    pub connection_manager: Arc<synctv_cluster::sync::ConnectionManager>,
    pub jwt_service: synctv_core::service::JwtService,
    pub redis_publish_tx: Option<mpsc::Sender<PublishRequest>>,
    pub oauth2_service: Option<Arc<synctv_core::service::OAuth2Service>>,
    pub settings_service: Option<Arc<synctv_core::service::SettingsService>>,
    pub settings_registry: Option<Arc<synctv_core::service::SettingsRegistry>>,
    pub email_service: Option<Arc<synctv_core::service::EmailService>>,
    pub publish_key_service: Option<Arc<synctv_core::service::PublishKeyService>>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub live_streaming_infrastructure: Option<Arc<LiveStreamingInfrastructure>>,
    /// SFU session manager for session affinity queries (multi-replica routing).
    pub sfu_session_manager: Option<Arc<synctv_sfu::SfuSessionManager>>,
    pub rate_limiter: synctv_core::service::rate_limit::RateLimiter,
    /// Shared rate limit config (created once at startup, not per-request)
    pub rate_limit_config: Arc<middleware::RateLimitConfig>,
    /// Shared JWT validator (created once at startup, not per-request)
    pub jwt_validator: Arc<synctv_core::service::auth::JwtValidator>,
    /// Shared security pipeline for post-JWT checks (blacklist, password, user status)
    pub security_pipeline: Arc<synctv_core::service::SecurityPipeline>,
    /// Token blacklist service for checking revoked tokens
    pub token_blacklist_service: TokenBlacklistService,
    /// WebSocket ticket service for secure WebSocket authentication (HTTP only)
    pub ws_ticket_service: Option<Arc<synctv_core::service::WsTicketService>>,
    // Unified API implementation layer
    pub client_api: Arc<crate::impls::ClientApiImpl>,
    pub admin_api: Option<Arc<crate::impls::AdminApiImpl>>,
    pub notification_api: Option<Arc<crate::impls::NotificationApiImpl>>,
    pub oauth2_api: Option<Arc<crate::impls::OAuth2ApiImpl>>,
    // H-2: Provider ApiImpls stored once in AppState (not created per-request)
    pub bilibili_api: Arc<crate::impls::BilibiliApiImpl>,
    pub alist_api: Arc<crate::impls::AlistApiImpl>,
    pub emby_api: Arc<crate::impls::EmbyApiImpl>,
    /// Shared Redis connection for playback caching
    pub redis_conn: Option<redis::aio::ConnectionManager>,
}

/// Create the HTTP router from configuration struct
pub fn create_router_from_config(config: RouterConfig) -> axum::Router {
    let state = build_app_state(config);
    let router = register_all_routes(state.clone());
    apply_global_layers(router, &state)
}

/// Build `AppState` from `RouterConfig`, creating the shared API implementation layers.
fn build_app_state(config: RouterConfig) -> AppState {
    let client_api = Arc::new(crate::impls::ClientApiImpl::new(
        config.user_service.clone(),
        config.room_service.clone(),
        config.connection_manager.clone(),
        config.config.clone(),
        config.sfu_manager,
        config.publish_key_service.clone(),
        config.jwt_service.clone(),
        config.live_streaming_infrastructure.clone(),
        None,
        config.settings_registry.clone(),
    ).with_redis_publish_tx(config.redis_publish_tx.clone())
     .with_redis_conn(config.redis_conn.clone())
     .with_rate_limiter(config.rate_limiter.clone()));

    let admin_api = config.settings_service.as_ref().map(|settings_svc| {
        let email_svc = config.email_service.clone().unwrap_or_else(|| {
            Arc::new(synctv_core::service::EmailService::new(None)
                .expect("EmailService::new(None) should not fail"))
        });
        Arc::new(crate::impls::AdminApiImpl::new(
            config.room_service.clone(),
            config.user_service.clone(),
            settings_svc.clone(),
            config.settings_registry.clone(),
            email_svc,
            config.connection_manager.clone(),
            config.provider_instance_manager.clone(),
            config.live_streaming_infrastructure.clone(),
            config.redis_publish_tx.clone(),
            config.audit_service.clone(),
        ))
    });

    // C-1: Create shared NotificationApiImpl (matches HTTP and gRPC)
    let notification_api = config.notification_service.as_ref().map(|notif_svc| {
        Arc::new(crate::impls::NotificationApiImpl::new(notif_svc.clone()))
    });

    // Create shared OAuth2ApiImpl
    let oauth2_api = config.oauth2_service.as_ref().map(|oauth2_svc| {
        Arc::new(crate::impls::OAuth2ApiImpl::new(
            oauth2_svc.clone(),
            config.user_service.clone(),
        ))
    });

    // H-3: Create shared RateLimitConfig once at startup (not per-request)
    let rate_limit_config = Arc::new(middleware::RateLimitConfig::default());

    // H-5: Create shared JwtValidator once at startup (not per-request)
    let jwt_validator = Arc::new(synctv_core::service::auth::JwtValidator::new(
        Arc::new(config.jwt_service.clone()),
    ));

    // H-2: Create shared provider ApiImpls once at startup (not per-request)
    let bilibili_api = Arc::new(crate::impls::BilibiliApiImpl::new(config.bilibili_provider.clone()));
    let alist_api = Arc::new(crate::impls::AlistApiImpl::new(config.alist_provider.clone()));
    let emby_api = Arc::new(crate::impls::EmbyApiImpl::new(config.emby_provider.clone()));

    // Create shared security pipeline for post-JWT checks (steps 2-3)
    let security_pipeline = Arc::new(synctv_core::service::SecurityPipeline::new(
        config.user_service.clone(),
    ));

    AppState {
        config: config.config,
        user_service: config.user_service,
        room_service: config.room_service,
        provider_instance_manager: config.provider_instance_manager,
        user_provider_credential_repository: config.user_provider_credential_repository,
        alist_provider: config.alist_provider,
        bilibili_provider: config.bilibili_provider,
        emby_provider: config.emby_provider,
        cluster_manager: config.cluster_manager,
        connection_manager: config.connection_manager,
        jwt_service: config.jwt_service,
        redis_publish_tx: config.redis_publish_tx,
        oauth2_service: config.oauth2_service,
        settings_service: config.settings_service,
        settings_registry: config.settings_registry,
        email_service: config.email_service,
        publish_key_service: config.publish_key_service,
        notification_service: config.notification_service,
        live_streaming_infrastructure: config.live_streaming_infrastructure,
        sfu_session_manager: config.sfu_session_manager,
        rate_limiter: config.rate_limiter,
        rate_limit_config,
        jwt_validator,
        security_pipeline,
        token_blacklist_service: config.token_blacklist_service,
        ws_ticket_service: config.ws_ticket_service,
        client_api,
        admin_api,
        notification_api,
        oauth2_api,
        bilibili_api,
        alist_api,
        emby_api,
        redis_conn: config.redis_conn,
    }
}

/// Authentication routes (register, login, refresh, OAuth2 exchange, password verify).
/// Strict rate limiting: 5 req/min.
fn register_auth_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/refresh", post(auth::refresh_token))
        .route("/api/oauth2/{provider}/exchange", post(oauth2::exchange_authorization_code))
        .route("/api/rooms/{room_id}/password/verify", post(room::check_password))
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_rate_limit,
        ))
}

/// Media mutation routes (add, delete, reorder, edit, batch operations).
/// Moderate rate limiting: 20 req/min.
fn register_media_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/api/rooms/{room_id}/media", post(room::add_media))
        .route("/api/rooms/{room_id}/media", axum::routing::delete(room::clear_playlist))
        .route("/api/rooms/{room_id}/media", axum::routing::patch(room::update_media_batch))
        .route("/api/rooms/{room_id}/media/batch", post(room::push_media_batch))
        .route("/api/rooms/{room_id}/media/batch", axum::routing::delete(room::delete_media_batch))
        .route("/api/rooms/{room_id}/media/reorder", post(room::reorder_media_batch))
        .route("/api/rooms/{room_id}/media/swap", post(room::swap_media_items))
        .route("/api/rooms/{room_id}/media/{media_id}", axum::routing::delete(room::delete_media))
        .route("/api/rooms/{room_id}/media/{media_id}", axum::routing::patch(room::edit_media))
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::media_rate_limit,
        ))
}

/// Write routes (room CRUD, membership, playback control, playlists, user updates).
/// Moderate rate limiting: 30 req/min.
fn register_write_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/api/rooms", post(room::create_room))
        .route("/api/rooms/{room_id}", axum::routing::delete(room::delete_room))
        .route("/api/rooms/{room_id}/members/@me", axum::routing::put(room::join_room))
        .route("/api/rooms/{room_id}/members/@me", axum::routing::delete(room::leave_room))
        .route("/api/rooms/{room_id}/settings", axum::routing::patch(room::update_room_settings))
        .route("/api/rooms/{room_id}/password", axum::routing::patch(room::set_room_password))
        .route("/api/rooms/{room_id}/playback/start", post(room::start_playback))
        .route("/api/rooms/{room_id}/playback/stop", post(room::stop_playback))
        .route("/api/user", axum::routing::patch(user::update_user))
        .route("/api/auth/session", axum::routing::delete(user::logout))
        .route("/api/user/rooms/{room_id}", axum::routing::delete(user::delete_my_room))
        .route("/api/oauth2/{provider}/bind", get(oauth2::get_bind_authorize_url))
        .route("/api/oauth2/{provider}/unlink", axum::routing::delete(oauth2::unlink_provider))
        .route("/api/oauth2/linked", get(oauth2::get_linked_providers))
        .route("/api/rooms/{room_id}/members/{user_id}", axum::routing::delete(room_extra::kick_member))
        .route("/api/rooms/{room_id}/members/{user_id}", axum::routing::patch(room_extra::set_member_permissions))
        .route("/api/rooms/{room_id}/bans", post(room_extra::ban_member))
        .route("/api/rooms/{room_id}/bans/{user_id}", axum::routing::delete(room_extra::unban_member))
        .route("/api/rooms/{room_id}/playlists", post(room::create_playlist))
        .route("/api/rooms/{room_id}/playlists/{playlist_id}", axum::routing::patch(room::update_playlist))
        .route("/api/rooms/{room_id}/playlists/{playlist_id}", axum::routing::delete(room::delete_playlist))
        .route("/api/rooms/{room_id}/settings/reset", post(room::reset_room_settings))
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
        .route("/api/user/rooms", get(user::get_joined_rooms))
        .route("/api/user/rooms/created", get(user::list_created_rooms))
        .route("/api/tickets", post(ticket::create_ticket))
        .route("/api/rooms", get(room::list_or_get_rooms))
        .route("/api/rooms/hot", get(room::get_hot_rooms))
        .route("/api/rooms/{room_id}/check", get(room::check_room))
        .route("/api/rooms/{room_id}", get(room::get_room))
        .route("/api/rooms/{room_id}/settings", get(room::get_room_settings))
        .route("/api/rooms/{room_id}/members", get(room::get_room_members))
        .route("/api/rooms/{room_id}/chat/history", get(room::get_chat_history))
        // Playlist and Media APIs
        .route("/api/rooms/{room_id}/playlists", get(room::list_playlists))
        .route("/api/rooms/{room_id}/playlists/{playlist_id}", get(room::get_playlist))
        .route("/api/rooms/{room_id}/playlists/{playlist_id}/items", get(media::list_playlist_items))
        .route("/api/rooms/{room_id}/media", get(room::list_media))
        .route("/api/rooms/{room_id}/media/{media_id}", get(room::get_media))
        .route("/api/rooms/{room_id}/playback", get(room::get_playback))
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::read_rate_limit,
        ))
}

/// Assemble all route groups into a single router.
fn register_all_routes(state: AppState) -> Router<AppState> {
    let health_router = if state.config.server.metrics_enabled || state.config.server.development_mode {
        health::create_health_router_with_metrics()
    } else {
        health::create_health_router()
    };

    let email_routes = email_verification::create_email_router()
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_rate_limit,
        ));

    let router = Router::new()
        .merge(health_router)
        .merge(public::create_public_router())
        .merge(email_routes)
        .merge(
            publish_key::create_publish_key_router()
                .route_layer(axum_middleware::from_fn_with_state(
                    state.clone(),
                    middleware::auth_rate_limit,
                ))
        )
        .merge(
            notifications::create_notification_read_router()
                .route_layer(axum_middleware::from_fn_with_state(
                    state.clone(),
                    middleware::read_rate_limit,
                ))
        )
        .merge(
            notifications::create_notification_write_router()
                .route_layer(axum_middleware::from_fn_with_state(
                    state.clone(),
                    middleware::write_rate_limit,
                ))
        )
        .merge(register_auth_routes(&state))
        .merge(register_media_routes(&state))
        .merge(register_write_routes(&state))
        // OAuth2 read-only routes
        .merge(
            Router::new()
                .route("/api/oauth2/{provider}/authorize", get(oauth2::get_authorize_url))
                .route("/api/oauth2/providers", get(oauth2::list_available_providers))
                .route_layer(axum_middleware::from_fn_with_state(
                    state.clone(),
                    middleware::read_rate_limit,
                ))
        )
        .merge(register_read_routes(&state))
        // Live streaming routes
        .merge(
            Router::new()
                .nest("/api/room/movie/live", live::create_live_router())
                .route_layer(axum_middleware::from_fn_with_state(
                    state.clone(),
                    middleware::streaming_rate_limit,
                ))
        )
        // WebSocket endpoint
        .merge(
            Router::new()
                .route("/ws/rooms/{room_id}", axum::routing::get(websocket::websocket_handler))
                .route_layer(axum_middleware::from_fn_with_state(
                    state.clone(),
                    middleware::websocket_rate_limit,
                ))
        )
        // WebRTC configuration endpoints
        .merge(
            Router::new()
                .route("/api/rooms/{room_id}/webrtc/ice-servers", get(webrtc::get_ice_servers))
                .route("/api/rooms/{room_id}/webrtc/network-quality", get(webrtc::get_network_quality))
                .route("/api/webrtc/session/{conn_id}/affinity", get(webrtc::session_affinity_lookup))
                .route_layer(axum_middleware::from_fn_with_state(
                    state.clone(),
                    middleware::read_rate_limit,
                ))
        )
        // Admin routes
        .merge(
            Router::new()
                .nest("/api/admin", admin::create_admin_router())
                .route_layer(axum_middleware::from_fn_with_state(
                    state.clone(),
                    middleware::admin_rate_limit,
                ))
        )
        // Provider routes
        .merge(
            Router::new()
                .nest("/api/provider", provider_common::register_common_routes())
                .nest("/api/providers/bilibili", providers::bilibili::bilibili_routes())
                .nest("/api/providers/alist", providers::alist::alist_routes())
                .nest("/api/providers/emby", providers::emby::emby_routes())
                .nest("/api/providers/direct_url", providers::direct_url::direct_url_routes())
                .route_layer(axum_middleware::from_fn_with_state(
                    state.clone(),
                    middleware::read_rate_limit,
                ))
        );

    router
}

/// Build CORS layer based on configuration.
fn build_cors_layer(config: &synctv_core::Config) -> CorsLayer {
    if config.server.development_mode {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
    } else if config.server.cors_allowed_origins.is_empty() {
        tracing::warn!(
            "CORS policy: DENY ALL cross-origin requests (no origins configured). \
             Web frontends on different origins will fail to connect. \
             To fix, set server.cors_allowed_origins to your frontend URL(s): \
             SYNCTV_SERVER_CORS_ALLOWED_ORIGINS='[\"https://app.example.com\"]'"
        );
        CorsLayer::new()
    } else {
        let origins: Vec<HeaderValue> = config
            .server
            .cors_allowed_origins
            .iter()
            .filter_map(|origin| origin.parse().ok())
            .collect();
        tracing::info!(
            origins = ?origins,
            "CORS: Configured with {} allowed origin(s)",
            origins.len()
        );
        let x_room_id: HeaderName = "x-room-id".parse().unwrap_or_else(|_| HeaderName::from_static("x-room-id"));
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods([Method::GET, Method::POST, Method::PUT, Method::PATCH, Method::DELETE, Method::OPTIONS])
            .allow_headers([
                axum::http::header::AUTHORIZATION,
                axum::http::header::CONTENT_TYPE,
                axum::http::header::ACCEPT,
                x_room_id,
            ])
            .allow_credentials(true)
            .vary([
                axum::http::header::ORIGIN,
                axum::http::header::ACCESS_CONTROL_REQUEST_METHOD,
                axum::http::header::ACCESS_CONTROL_REQUEST_HEADERS,
            ])
    }
}

/// Apply global middleware layers (CORS, body limit, timeout, security headers, HSTS, tracing)
/// and bind state.
fn apply_global_layers(router: Router<AppState>, state: &AppState) -> axum::Router {
    let cors = build_cors_layer(&state.config);

    let router = router
        .layer(cors)
        .layer(axum::extract::DefaultBodyLimit::max(10 * 1024 * 1024))
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            std::time::Duration::from_secs(30),
        ))
        .layer(axum_middleware::from_fn(middleware::security_headers_middleware));

    // Apply HSTS in production (not in development mode)
    let router = if !state.config.server.development_mode {
        let hsts_value = middleware::hsts_header(63_072_000, true, false);
        router.layer(axum_middleware::from_fn(move |request: axum::extract::Request, next: axum::middleware::Next| {
            let hsts = hsts_value.clone();
            async move {
                let mut response = next.run(request).await;
                if let Ok(value) = axum::http::HeaderValue::from_str(&hsts) {
                    response.headers_mut().insert(
                        axum::http::header::STRICT_TRANSPORT_SECURITY,
                        value,
                    );
                }
                response
            }
        }))
    } else {
        router
    };

    router
        .layer(axum_middleware::from_fn(
            crate::observability::metrics_middleware::metrics_layer,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone())
}

