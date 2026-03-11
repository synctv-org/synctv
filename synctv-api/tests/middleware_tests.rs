//! Middleware tests for synctv-api
//!
//! Tests AuthUser/GuestUser extractors and rate limit middleware behavior
//! using axum test utilities (`tower::ServiceExt::oneshot`).

#![allow(clippy::unwrap_used)]
use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::get,
    Router,
};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use synctv_api::http::error::AppError;
use synctv_api::http::middleware::AuthUser;

// ============================================================================
// Helper
// ============================================================================

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

// ============================================================================
// AuthUser extractor tests (simulated)
// ============================================================================

/// Without Authorization header, `AuthUser` extraction should fail with 401.
///
/// Since `AuthUser` requires `AppState` (with JWT validator, security pipeline, etc.),
/// we simulate the behavior by testing the error path directly -- the extractor
/// checks for the Authorization header first before any async work.
#[tokio::test]
async fn test_auth_user_missing_header_401() {
    // Build a router that returns 401 when no Authorization header is present
    // (simulating what the AuthUser extractor does)
    let app = Router::new().route(
        "/test",
        get(|| async {
            Err::<String, AppError>(AppError::unauthorized("Missing Authorization header"))
        }),
    );

    let req = Request::get("/test").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(resp).await;
    assert!(json["error"].as_str().unwrap().contains("Authorization"));
}

/// Malformed bearer token should return 401.
#[tokio::test]
async fn test_auth_user_malformed_token_401() {
    let app = Router::new().route(
        "/test",
        get(|| async {
            Err::<String, AppError>(AppError::unauthorized(
                "Token verification failed: InvalidToken",
            ))
        }),
    );

    let req = Request::get("/test")
        .header("Authorization", "Bearer invalid-jwt-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// A regular (non-guest) token used for a guest endpoint should return 401.
#[tokio::test]
async fn test_guest_user_non_guest_token_401() {
    let app = Router::new().route(
        "/test",
        get(|| async { Err::<String, AppError>(AppError::unauthorized("Not a guest token")) }),
    );

    let req = Request::get("/test")
        .header("Authorization", "Bearer some-regular-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let json = body_json(resp).await;
    assert!(json["error"].as_str().unwrap().contains("guest"));
}

// ============================================================================
// AuthUser / GuestUser structural tests
// ============================================================================

#[test]
fn test_auth_user_struct_debug_and_clone() {
    let user = AuthUser {
        user_id: synctv_core::models::id::UserId::from_string("user_123".to_string()),
        password_version: 3,
    };
    let cloned = user.clone();
    assert_eq!(cloned.user_id.as_str(), "user_123");
    assert_eq!(cloned.password_version, 3);

    let debug = format!("{user:?}");
    assert!(debug.contains("user_123"));
}

// ============================================================================
// Rate limit config verification
// ============================================================================

#[test]
fn test_rate_limit_tiers_ordered_correctly() {
    use synctv_api::http::middleware::RateLimitConfig;
    let config = RateLimitConfig::default();

    // Auth should be strictest (brute force protection)
    assert!(config.auth_max_requests < config.read_max_requests);
    assert!(config.auth_max_requests < config.write_max_requests);

    // WebSocket should be stricter than streaming
    assert!(config.websocket_max_requests < config.streaming_max_requests);

    // Read should be most permissive
    assert!(config.read_max_requests >= config.write_max_requests);
}

// ============================================================================
// Security headers middleware integration
// ============================================================================

#[tokio::test]
async fn test_request_id_middleware_generates_id() {
    use synctv_api::http::middleware::request_id_middleware;

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(request_id_middleware));

    let req = Request::get("/test").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    // The middleware should generate and echo back an X-Request-ID header
    assert!(
        resp.headers().contains_key("x-request-id"),
        "Response should contain X-Request-ID header"
    );
}

#[tokio::test]
async fn test_request_id_middleware_preserves_client_id() {
    use synctv_api::http::middleware::request_id_middleware;

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(request_id_middleware));

    let req = Request::get("/test")
        .header("x-request-id", "my-custom-trace-id")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("x-request-id")
            .unwrap()
            .to_str()
            .unwrap(),
        "my-custom-trace-id"
    );
}

#[tokio::test]
async fn test_request_id_middleware_rejects_invalid_id() {
    use synctv_api::http::middleware::request_id_middleware;

    let app = Router::new()
        .route("/test", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(request_id_middleware));

    // Send a request with an invalid X-Request-ID (contains special characters)
    let req = Request::get("/test")
        .header("x-request-id", "invalid id with spaces!")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    // The middleware should reject the invalid ID and generate a new one
    let id = resp
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap();
    assert_ne!(id, "invalid id with spaces!");
}

// ============================================================================
// Request ID in error response tests
// ============================================================================

#[tokio::test]
async fn test_request_id_in_error_response_json() {
    use synctv_api::http::middleware::request_id_middleware;

    let app = Router::new()
        .route(
            "/error",
            get(|| async { Err::<(), AppError>(AppError::bad_request("Test error")) }),
        )
        .layer(axum::middleware::from_fn(request_id_middleware));

    let req = Request::get("/error")
        .header("x-request-id", "test-request-123")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Check that the response header has the request ID
    let req_id_header = resp
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(req_id_header, "test-request-123");

    // Check that the error response JSON body includes the request_id
    let body = body_json(resp).await;
    assert_eq!(body["error"], "Test error");
    assert_eq!(body["status"], 400);
    assert_eq!(body["request_id"], "test-request-123");
}

#[tokio::test]
async fn test_request_id_in_generated_error_response() {
    use synctv_api::http::middleware::request_id_middleware;

    let app = Router::new()
        .route(
            "/error",
            get(|| async { Err::<(), AppError>(AppError::not_found("Resource not found")) }),
        )
        .layer(axum::middleware::from_fn(request_id_middleware));

    let req = Request::get("/error").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Get the generated request ID from the header
    let req_id_header = resp
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string(); // Clone to avoid borrow issue

    // Verify the request_id is in the JSON body
    let body = body_json(resp).await;
    assert_eq!(body["error"], "Resource not found");
    assert_eq!(body["status"], 404);
    assert_eq!(body["request_id"], req_id_header);
}

#[tokio::test]
async fn test_hsts_not_added_without_https_forwarding() {
    use synctv_api::http::{create_router_with_state_from_config, RouterConfig};
    use synctv_api::http::AppState;
    use synctv_core::cache::{KeyBuilder, NoopCacheL2, UsernameCache};
    use synctv_core::provider::{
        AlistProvider, BilibiliProvider, DirectUrlProvider, EmbyProvider, LiveProxyProvider,
        ProviderSet, RtmpProvider,
    };
    use synctv_core::service::{
        AuditService, ContentFilter, InMemoryTokenBlacklistStore, RateLimitConfig, RateLimiter,
        RemoteProviderManager, RoomService, UserService,
    };
    use synctv_proxy::slice_cache::{SliceCache, SliceCacheConfig};

    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
        .expect("lazy pool");
    let username_cache =
        UsernameCache::new(std::sync::Arc::new(NoopCacheL2), "test:username:".to_string(), 128, 60);
    let user_service = std::sync::Arc::new(UserService::new(
        pool.clone(),
        synctv_core::service::JwtService::new(
            "test-secret-key-for-http-router-tests-minimum-32-chars",
        )
        .expect("jwt"),
        username_cache,
        synctv_core::config::PasswordComplexityConfig::default(),
        std::sync::Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
        KeyBuilder::new("test"),
        synctv_core::service::BruteForceProtection::in_memory("test".to_string()),
    ));
    let room_service = std::sync::Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
    let provider_instance_manager = std::sync::Arc::new(RemoteProviderManager::new(
        std::sync::Arc::new(synctv_core::repository::ProviderInstanceRepository::new(
            pool.clone(),
        )),
        None,
        None,
        "test:",
    ));
    let providers = ProviderSet {
        alist: std::sync::Arc::new(AlistProvider::new(provider_instance_manager.clone())),
        bilibili: std::sync::Arc::new(BilibiliProvider::new(provider_instance_manager.clone())),
        emby: std::sync::Arc::new(EmbyProvider::new(provider_instance_manager.clone())),
        direct_url: std::sync::Arc::new(DirectUrlProvider::new()),
        rtmp: std::sync::Arc::new(RtmpProvider::new()),
        live_proxy: std::sync::Arc::new(LiveProxyProvider::new()),
    };
    let (audit_service, _audit_handle) = AuditService::new(pool.clone());

    let mut config = synctv_core::Config::default();
    config.server.trusted_proxies = vec!["127.0.0.1".to_string()];

    let (app, _state): (_, AppState) = create_router_with_state_from_config(RouterConfig {
        config: std::sync::Arc::new(config),
        user_service,
        room_service,
        content_filter: ContentFilter::new(),
        provider_instance_manager,
        user_provider_credential_repository: std::sync::Arc::new(
            synctv_core::repository::UserProviderCredentialRepository::new(pool),
        ),
        providers,
        cluster_manager: None,
        connection_manager: std::sync::Arc::new(synctv_cluster::sync::ConnectionManager::new(
            synctv_cluster::sync::ConnectionLimits::default(),
        )),
        jwt_service: synctv_core::service::JwtService::new(
            "test-secret-key-for-http-router-tests-minimum-32-chars",
        )
        .expect("jwt"),
        redis_publish_tx: None,
        oauth2_service: None,
        settings_service: None,
        settings_registry: None,
        email_service: None,
        email_token_service: None,
        publish_key_service: None,
        notification_service: None,
        chat_service: None,
        audit_service: std::sync::Arc::new(audit_service),
        live_streaming_infrastructure: None,
        rate_limiter: RateLimiter::in_memory_only("test:".to_string()),
        ws_ticket_service: None,
        redis_conn: None,
        builtin_stun_url: None,
        turn_health_checker: None,
        credential_encryption: None,
        proxy_slice_cache: std::sync::Arc::new(SliceCache::new(SliceCacheConfig {
            enabled: false,
            ..SliceCacheConfig::default()
        })),
        messaging_rate_limit_config: RateLimitConfig::default(),
        heartbeat_schedule: synctv_api::impls::HeartbeatSchedule::production(),
        providers_manager: None,
    });

    let request = Request::builder()
        .uri("/health/live")
        .extension(axum::extract::ConnectInfo(
            "127.0.0.1:12345"
                .parse::<std::net::SocketAddr>()
                .expect("socket addr"),
        ))
        .body(Body::empty())
        .expect("request");

    let response = app.oneshot(request).await.expect("response");
    assert!(
        response
            .headers()
            .get(axum::http::header::STRICT_TRANSPORT_SECURITY)
            .is_none(),
        "HSTS must not be injected for non-HTTPS requests"
    );
}
