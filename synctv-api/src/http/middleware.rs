// HTTP middleware

use axum::{
    extract::{FromRef, FromRequestParts, OptionalFromRequestParts, Request, State},
    http::request::Parts,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::LazyLock;
use synctv_core::{
    models::{id::UserId, RoomId},
    service::{auth::JwtValidator, rate_limit::RateLimitError},
};

use super::{AppError, AppState};

tokio::task_local! {
    pub static CURRENT_REQUEST_ID: String;
}

/// Request ID extracted from request extensions
///
/// This is set by the `request_id_middleware` and can be used in handlers
/// to correlate errors with the request ID.
#[derive(Debug, Clone)]
pub struct RequestId(pub String);

// ------------------------------------------------------------------
// Request ID middleware (Issue #22)
// ------------------------------------------------------------------

/// HTTP header name for request/trace ID propagation.
static X_REQUEST_ID: LazyLock<axum::http::HeaderName> =
    LazyLock::new(|| axum::http::HeaderName::from_static("x-request-id"));

/// Middleware that generates a unique request ID per request.
///
/// - If the client sends an `X-Request-ID` header whose value is a non-empty
///   alphanumeric ASCII string of at most 64 characters, that value is reused
///   (allows end-to-end trace correlation from trusted clients).
/// - Otherwise a fresh 12-character nanoid is generated.
///
/// The request ID is:
/// 1. Recorded in the current tracing span as `request_id` for log correlation.
/// 2. Echoed back in the `X-Request-ID` response header so callers can correlate
///    logs with their own request tracking.
/// 3. Exposed via a task-local so `AppError` responses can include it without
///    buffering and rewriting response bodies.
pub async fn request_id_middleware(mut request: Request, next: Next) -> Response {
    // Honour an incoming X-Request-ID header when safe to do so.
    let request_id = request
        .headers()
        .get(X_REQUEST_ID.clone())
        .and_then(|v| v.to_str().ok())
        .filter(|s| {
            // Validate: non-empty, max 64 chars, alphanumeric + hyphens/underscores only.
            let len = s.len();
            len > 0
                && len <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .map_or_else(|| nanoid::nanoid!(12), str::to_owned);

    // Record in current tracing span for log correlation.
    tracing::Span::current().record("request_id", request_id.as_str());
    tracing::debug!(request_id = %request_id, "Request received");

    // Store in request extensions so error responses can include it
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let mut response = CURRENT_REQUEST_ID
        .scope(request_id.clone(), async move { next.run(request).await })
        .await;

    // Echo back in response header so callers can correlate.
    if let Ok(value) = axum::http::HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(X_REQUEST_ID.clone(), value);
    }

    response
}

/// Pre-validated security header names (validated once at startup via Lazy)
static X_FRAME_OPTIONS: LazyLock<axum::http::HeaderName> =
    LazyLock::new(|| axum::http::HeaderName::from_static("x-frame-options"));
static X_CONTENT_TYPE_OPTIONS: LazyLock<axum::http::HeaderName> =
    LazyLock::new(|| axum::http::HeaderName::from_static("x-content-type-options"));
static X_XSS_PROTECTION: LazyLock<axum::http::HeaderName> =
    LazyLock::new(|| axum::http::HeaderName::from_static("x-xss-protection"));
static CONTENT_SECURITY_POLICY: LazyLock<axum::http::HeaderName> =
    LazyLock::new(|| axum::http::HeaderName::from_static("content-security-policy"));
static REFERRER_POLICY: LazyLock<axum::http::HeaderName> =
    LazyLock::new(|| axum::http::HeaderName::from_static("referrer-policy"));
static PERMISSIONS_POLICY: LazyLock<axum::http::HeaderName> =
    LazyLock::new(|| axum::http::HeaderName::from_static("permissions-policy"));
static PRAGMA: LazyLock<axum::http::HeaderName> =
    LazyLock::new(|| axum::http::HeaderName::from_static("pragma"));

/// Authenticated user extracted from JWT token
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: UserId,
    /// Password version from JWT claims.
    /// Used when creating WebSocket tickets to ensure tickets are invalidated
    /// when the user changes their password.
    pub password_version: i32,
}

impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        // Get AppState from state
        let app_state = AppState::from_ref(state);

        // Use shared JWT validator from AppState (created once at startup)
        let validator = app_state.jwt_validator.clone();

        // Extract Authorization header
        let auth_header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .ok_or_else(|| AppError::unauthorized("Missing Authorization header"))?;

        // Parse Bearer token and validate using unified validator.
        let auth_str = auth_header
            .to_str()
            .map_err(|e| AppError::unauthorized(format!("Invalid Authorization header: {e}")))?;

        // Step 1: JWT verification
        let claims = validator
            .validate_http(auth_str)
            .map_err(|e| AppError::unauthorized(format!("{e}")))?;

        // Steps 2-3: Shared security pipeline (password invalidation, user status)
        let authenticated = app_state
            .security_pipeline
            .check(&claims)
            .await
            .map_err(|e| AppError::unauthorized(format!("{e}")))?;

        // Extract password version from claims, defaulting to 0 for legacy tokens
        let password_version = authenticated.claims.pv;

        Ok(Self {
            user_id: authenticated.user_id,
            password_version,
        })
    }
}

impl<S> OptionalFromRequestParts<S> for AuthUser
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
        // If there's no Authorization header, return None (anonymous access)
        if parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .is_none()
        {
            return Ok(None);
        }
        // Header IS present: authenticate and propagate errors for invalid tokens
        // (don't silently downgrade to anonymous when the token is malformed/expired)
        let user = <Self as FromRequestParts<S>>::from_request_parts(parts, state).await?;
        Ok(Some(user))
    }
}

/// Authenticated guest extracted from a guest JWT token.
///
/// Guest tokens are scoped to a single room and only permit read/view
/// operations. Write endpoints must use [`AuthUser`] which rejects guest
/// tokens. This type-safe separation ensures guest tokens cannot be used
/// to create, modify, or manage resources.
#[derive(Debug, Clone)]
pub struct GuestUser {
    pub room_id: RoomId,
    pub session_id: String,
}

impl<S> FromRequestParts<S> for GuestUser
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);

        let auth_header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .ok_or_else(|| AppError::unauthorized("Missing Authorization header"))?;

        let auth_str = auth_header
            .to_str()
            .map_err(|e| AppError::unauthorized(format!("Invalid Authorization header: {e}")))?;

        let token = JwtValidator::extract_bearer_token(auth_str)
            .map_err(|e| AppError::unauthorized(format!("{e}")))?;

        // B1 FIX: Use GuestTokenValidator::validate_async() which checks the
        // token blacklist (for kicked guests) in addition to JWT signature/expiry.
        // Previously this only called jwt_service.verify_guest_token() which
        // allowed kicked guests to continue accessing the room until token expiry.
        let claims = app_state
            .guest_token_validator
            .validate_async(&token)
            .await
            .map_err(|e| AppError::unauthorized(format!("{e}")))?;

        if !claims.is_guest() {
            return Err(AppError::unauthorized("Not a guest token"));
        }

        let room_id = claims.room_id();

        // Verify the room still exists. A guest token is only valid for the specific room
        // it was issued for. If the room has been deleted, the token must be rejected so
        // that stale guest tokens cannot be replayed against newly-created rooms that
        // happen to reuse the same ID.
        //
        // Uses lightweight existence check (SELECT EXISTS) instead of fetching the full
        // room row, since we only need to confirm the room hasn't been deleted.
        let exists = app_state
            .room_service
            .room_exists(&room_id)
            .await
            .map_err(|_| AppError::unauthorized("Room not found or has been deleted"))?;
        if !exists {
            return Err(AppError::unauthorized("Room not found or has been deleted"));
        }

        Ok(Self {
            room_id,
            session_id: claims.session_id,
        })
    }
}

/// HTTP rate limiting configuration for different endpoint categories.
///
/// Type alias to the canonical config struct in `synctv_core::config`.
/// Previously this was a duplicate struct with hardcoded defaults;
/// now it comes from the config file via `Config.http_rate_limits`.
pub type RateLimitConfig = synctv_core::HttpRateLimitConfig;

/// Rate limit category for different types of operations
#[derive(Debug, Clone, Copy)]
pub enum RateLimitCategory {
    Auth,
    Write,
    Read,
    Media,
    Admin,
    Streaming,
    WebSocket,
}

/// Middleware for rate limiting based on user ID and endpoint category
pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    category: RateLimitCategory,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    // Extract user ID from authorization header if present.
    // Prefix with "user:" so the key format matches the gRPC rate limit layer
    // (which uses "user:{token_hash}"), ensuring the same user shares one bucket
    // regardless of whether they connect via HTTP or gRPC.
    let user_id = extract_user_id_from_header(&request, &state).map(|id| format!("user:{id}"));

    // Use IP address as fallback if no user ID (for public endpoints).
    // We only trust X-Forwarded-For/X-Real-IP headers when:
    // 1. The request comes from a configured trusted proxy, OR
    // 2. Development mode is enabled (for local testing)
    //
    // This prevents header spoofing attacks that could bypass rate limiting.
    let rate_limit_key = user_id.unwrap_or_else(|| {
        // Try to get the remote/socket address from ConnectInfo extension
        let remote_addr = request
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0.ip());

        // Check if we should trust proxy headers
        let should_trust_headers =
            remote_addr.is_some_and(|ip| state.config.server.is_trusted_proxy(&ip));

        if should_trust_headers {
            // Trust X-Forwarded-For from trusted proxies (or in dev mode).
            // Parse as IpAddr to prevent attackers from injecting arbitrary
            // strings (e.g. "127.0.0.1, evil") as rate limit keys.
            let forwarded_ip = request
                .headers()
                .get("X-Forwarded-For")
                .and_then(|h| h.to_str().ok())
                .and_then(|v| v.split(',').next())
                .map(str::trim)
                .and_then(|s| s.parse::<std::net::IpAddr>().ok());

            let real_ip = request
                .headers()
                .get("X-Real-IP")
                .and_then(|h| h.to_str().ok())
                .map(str::trim)
                .and_then(|s| s.parse::<std::net::IpAddr>().ok());

            if let Some(ip) = forwarded_ip {
                format!("anon:{ip}")
            } else if let Some(ip) = real_ip {
                format!("anon:{ip}")
            } else if let Some(ip) = remote_addr {
                format!("anon:{ip}")
            } else {
                "anon:unknown".to_string()
            }
        } else {
            // Don't trust headers - use socket address directly
            remote_addr.map_or_else(|| "anon:unknown".to_string(), |ip| format!("anon:{ip}"))
        }
    });

    // Get rate limiter and config from app state (config is shared, not per-request)
    let rate_limiter = state.rate_limiter.clone();
    let config = &state.rate_limit_config;

    // Determine rate limit parameters based on category
    let (max_requests, window_seconds, category_name) = match category {
        RateLimitCategory::Auth => (config.auth_max_requests, config.auth_window_seconds, "auth"),
        RateLimitCategory::Write => (
            config.write_max_requests,
            config.write_window_seconds,
            "write",
        ),
        RateLimitCategory::Read => (config.read_max_requests, config.read_window_seconds, "read"),
        RateLimitCategory::Media => (
            config.media_max_requests,
            config.media_window_seconds,
            "media",
        ),
        RateLimitCategory::Admin => (
            config.admin_max_requests,
            config.admin_window_seconds,
            "admin",
        ),
        RateLimitCategory::Streaming => (
            config.streaming_max_requests,
            config.streaming_window_seconds,
            "streaming",
        ),
        RateLimitCategory::WebSocket => (
            config.websocket_max_requests,
            config.websocket_window_seconds,
            "websocket",
        ),
    };

    // Check rate limit
    // FIXED: P0.13 - Removed path from key to enforce category-wide limit
    // Previously: format!("{}:{}:{}", category_name, rate_limit_key, path)
    // This caused each endpoint to have its own counter, effectively multiplying the limit
    // Now: All endpoints in same category share the limit (e.g., 30 req/min for ALL write operations)
    let key = format!("ratelimit:{category_name}:{rate_limit_key}");
    match rate_limiter
        .check_rate_limit(&key, max_requests, window_seconds)
        .await
    {
        Ok(()) => {
            // Rate limit check passed, proceed with request
            Ok(next.run(request).await)
        }
        Err(RateLimitError::RateLimitExceeded {
            retry_after_seconds,
        }) => {
            // Rate limit exceeded, return 429 Too Many Requests with standard JSON error format
            let error = AppError::rate_limited(retry_after_seconds);
            let mut response = error.into_response();
            // Add rate-limit headers for client consumption
            let headers = response.headers_mut();
            if let Ok(v) = axum::http::HeaderValue::from_str(&retry_after_seconds.to_string()) {
                headers.insert("Retry-After", v);
            }
            if let Ok(v) = axum::http::HeaderValue::from_str(&max_requests.to_string()) {
                headers.insert("X-RateLimit-Limit", v);
            }
            if let Ok(v) = axum::http::HeaderValue::from_str(&retry_after_seconds.to_string()) {
                headers.insert("X-RateLimit-Reset", v);
            }

            Ok(response)
        }
        Err(e) => {
            // This branch should not be reached for check_rate_limit (which
            // degrades to in-memory on Redis errors), but handle defensively.
            tracing::error!(
                "Rate limit check unexpected error: {}. Denying request (fail closed).",
                e
            );
            let error = AppError::rate_limited(1);
            Ok(error.into_response())
        }
    }
}

/// Helper function to extract user ID from authorization header
fn extract_user_id_from_header(request: &Request, state: &AppState) -> Option<String> {
    let auth_header = request.headers().get(axum::http::header::AUTHORIZATION)?;
    let auth_str = auth_header.to_str().ok()?;

    // Use shared JwtValidator from AppState (not per-request creation)
    let user_id = state
        .jwt_validator
        .validate_http_extract_user_id(auth_str)
        .ok()?;

    Some(user_id.to_string())
}

/// Middleware factory for authentication endpoints
pub async fn auth_rate_limit(
    state: State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    rate_limit_middleware(state, RateLimitCategory::Auth, request, next).await
}

/// Middleware factory for write operations
pub async fn write_rate_limit(
    state: State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    rate_limit_middleware(state, RateLimitCategory::Write, request, next).await
}

/// Middleware factory for read operations
pub async fn read_rate_limit(
    state: State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    rate_limit_middleware(state, RateLimitCategory::Read, request, next).await
}

/// Middleware factory for media operations
pub async fn media_rate_limit(
    state: State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    rate_limit_middleware(state, RateLimitCategory::Media, request, next).await
}

/// Middleware factory for admin operations
pub async fn admin_rate_limit(
    state: State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    rate_limit_middleware(state, RateLimitCategory::Admin, request, next).await
}

/// Middleware factory for streaming operations (FLV/HLS)
pub async fn streaming_rate_limit(
    state: State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    rate_limit_middleware(state, RateLimitCategory::Streaming, request, next).await
}

/// Middleware factory for WebSocket connection attempts
pub async fn websocket_rate_limit(
    state: State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    rate_limit_middleware(state, RateLimitCategory::WebSocket, request, next).await
}

/// Middleware for routes that require the WebSocket runtime to be fully wired.
///
/// This runs before request extractors in the handler, ensuring these endpoints
/// fail closed with 503 instead of leaking auth/validation-specific status codes
/// when the runtime is unavailable.
pub async fn websocket_runtime_required(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    crate::http::websocket::validate_websocket_runtime_dependencies(&state)?;
    Ok(next.run(request).await)
}

/// Security headers middleware
///
/// Adds security-related HTTP headers to all responses to protect against
/// common web vulnerabilities:
/// - X-Frame-Options: Prevents clickjacking
/// - X-Content-Type-Options: Prevents MIME type sniffing
/// - X-XSS-Protection: Enables browser XSS filter (legacy, but still useful)
/// - Content-Security-Policy: Restricts resource loading
/// - Strict-Transport-Security: Enforces HTTPS (only if configured)
/// - Referrer-Policy: Controls referrer information
/// - Permissions-Policy: Restricts browser features
pub async fn security_headers_middleware(request: Request, next: Next) -> Response {
    let request_path = request.uri().path().to_string();
    let mut response = next.run(request).await;

    let headers = response.headers_mut();

    // Prevent clickjacking attacks
    // DENY: The page cannot be displayed in a frame, regardless of the site attempting to do so.
    if !headers.contains_key("X-Frame-Options") {
        headers.insert(
            // Static header name - validated at compile time via Lazy
            X_FRAME_OPTIONS.clone(),
            axum::http::HeaderValue::from_static("DENY"),
        );
    }

    // Prevent MIME type sniffing
    // nosniff: Blocks a request if the requested type is "style" or "script"
    // and the MIME type is not a valid MIME type for the requested type.
    if !headers.contains_key("X-Content-Type-Options") {
        headers.insert(
            X_CONTENT_TYPE_OPTIONS.clone(),
            axum::http::HeaderValue::from_static("nosniff"),
        );
    }

    // Enable XSS filtering in browsers (legacy but still useful for older browsers)
    // 1; mode=block: Enables XSS filtering. Rather than sanitizing the page,
    // the browser will prevent rendering of the page entirely if an attack is detected.
    if !headers.contains_key("X-XSS-Protection") {
        headers.insert(
            X_XSS_PROTECTION.clone(),
            axum::http::HeaderValue::from_static("1; mode=block"),
        );
    }

    // Content Security Policy
    // Relaxed for a video platform: allows media from any source (needed for
    // provider instances like Alist, Emby, Bilibili), WebSocket connections,
    // data URIs for thumbnails, and inline styles for the player UI.
    if !headers.contains_key("Content-Security-Policy") {
        headers.insert(
            CONTENT_SECURITY_POLICY.clone(),
            axum::http::HeaderValue::from_static(
                "default-src 'self'; \
                 media-src * blob:; \
                 frame-src * blob:; \
                 connect-src 'self' wss: ws:; \
                 img-src 'self' data: https:; \
                 style-src 'self' 'unsafe-inline'; \
                 script-src 'self'; \
                 frame-ancestors 'none'; \
                 base-uri 'none'",
            ),
        );
    }

    // Referrer Policy
    // strict-origin-when-cross-origin: Send origin only for cross-origin requests,
    // full URL for same-origin requests
    if !headers.contains_key("Referrer-Policy") {
        headers.insert(
            REFERRER_POLICY.clone(),
            axum::http::HeaderValue::from_static("strict-origin-when-cross-origin"),
        );
    }

    // Permissions Policy (formerly Feature Policy)
    // Disables various browser features that are typically not needed by APIs
    if !headers.contains_key("Permissions-Policy") {
        headers.insert(
            PERMISSIONS_POLICY.clone(),
            axum::http::HeaderValue::from_static(
                "accelerometer=(), camera=(), geolocation=(), gyroscope=(), \
                 magnetometer=(), microphone=(), payment=(), usb=()",
            ),
        );
    }

    // Cache Control for API responses
    // Use short cache for HLS/streaming paths to avoid breaking live playback;
    // apply strict no-store for all other (sensitive) API responses.
    let is_streaming_path = request_path.contains("/live/hls/")
        || request_path.contains("/live/flv/")
        || request_path.ends_with(".m3u8")
        || request_path.ends_with(".ts")
        || request_path.ends_with(".flv");

    if !headers.contains_key("Cache-Control") {
        if is_streaming_path {
            // HLS segments and playlists need short caching for smooth playback
            headers.insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("public, max-age=2"),
            );
        } else {
            headers.insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static(
                    "no-store, no-cache, must-revalidate, proxy-revalidate",
                ),
            );
        }
    }

    // Pragma: no-cache (for HTTP/1.0 compatibility) -- skip for streaming paths
    if !is_streaming_path && !headers.contains_key("Pragma") {
        headers.insert(
            PRAGMA.clone(),
            axum::http::HeaderValue::from_static("no-cache"),
        );
    }

    response
}

/// HSTS (HTTP Strict Transport Security) middleware
///
/// Should be used alongside `security_headers_middleware` when HTTPS is enabled.
/// This tells browsers to always use HTTPS for this site.
///
/// # Arguments
/// * `max_age` - The time, in seconds, that the browser should remember
///   that a site is only to be accessed using HTTPS.
/// * `include_subdomains` - If true, this rule applies to all subdomains as well.
/// * `preload` - If true, the site can be included in browser HSTS preload lists.
#[must_use]
pub fn hsts_header(max_age: u64, include_subdomains: bool, preload: bool) -> String {
    let mut value = format!("max-age={max_age}");

    if include_subdomains {
        value.push_str("; includeSubDomains");
    }

    if preload {
        value.push_str("; preload");
    }

    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    // === RateLimitConfig Tests ===

    #[test]
    fn test_rate_limit_config_default() {
        let config = RateLimitConfig::default();
        assert_eq!(config.auth_max_requests, 5);
        assert_eq!(config.auth_window_seconds, 60);
        assert_eq!(config.write_max_requests, 30);
        assert_eq!(config.write_window_seconds, 60);
        assert_eq!(config.read_max_requests, 100);
        assert_eq!(config.read_window_seconds, 60);
        assert_eq!(config.media_max_requests, 20);
        assert_eq!(config.media_window_seconds, 60);
        assert_eq!(config.admin_max_requests, 30);
        assert_eq!(config.admin_window_seconds, 60);
        assert_eq!(config.streaming_max_requests, 200);
        assert_eq!(config.streaming_window_seconds, 60);
        assert_eq!(config.websocket_max_requests, 10);
        assert_eq!(config.websocket_window_seconds, 60);
    }

    #[test]
    fn test_rate_limit_config_auth_stricter_than_read() {
        let config = RateLimitConfig::default();
        assert!(config.auth_max_requests < config.read_max_requests);
    }

    #[test]
    fn test_rate_limit_config_websocket_stricter_than_streaming() {
        let config = RateLimitConfig::default();
        assert!(config.websocket_max_requests < config.streaming_max_requests);
    }

    // === HSTS Header Tests ===

    #[test]
    fn test_hsts_header_basic() {
        let header = hsts_header(31536000, false, false);
        assert_eq!(header, "max-age=31536000");
    }

    #[test]
    fn test_hsts_header_with_subdomains() {
        let header = hsts_header(31536000, true, false);
        assert_eq!(header, "max-age=31536000; includeSubDomains");
    }

    #[test]
    fn test_hsts_header_with_preload() {
        let header = hsts_header(31536000, false, true);
        assert_eq!(header, "max-age=31536000; preload");
    }

    #[test]
    fn test_hsts_header_full() {
        let header = hsts_header(63072000, true, true);
        assert_eq!(header, "max-age=63072000; includeSubDomains; preload");
    }

    #[test]
    fn test_hsts_header_zero_max_age() {
        let header = hsts_header(0, false, false);
        assert_eq!(header, "max-age=0");
    }

    // === Security Headers Middleware Tests ===

    #[tokio::test]
    async fn test_security_headers_adds_all_headers() {
        let app = axum::Router::new()
            .route("/test", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let headers = response.headers();
        assert_eq!(headers.get("X-Frame-Options").unwrap(), "DENY");
        assert_eq!(headers.get("X-Content-Type-Options").unwrap(), "nosniff");
        assert_eq!(headers.get("X-XSS-Protection").unwrap(), "1; mode=block");
        assert!(headers.contains_key("Content-Security-Policy"));
        assert!(headers.contains_key("Referrer-Policy"));
        assert!(headers.contains_key("Permissions-Policy"));
        assert!(headers.contains_key("Cache-Control"));
        assert_eq!(headers.get("Pragma").unwrap(), "no-cache");
    }

    #[tokio::test]
    async fn test_security_headers_does_not_overwrite_existing() {
        let app = axum::Router::new()
            .route(
                "/test",
                axum::routing::get(|| async {
                    (
                        [(
                            axum::http::header::HeaderName::from_static("x-frame-options"),
                            "SAMEORIGIN",
                        )],
                        "ok",
                    )
                }),
            )
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Should not overwrite the existing X-Frame-Options header
        assert_eq!(
            response.headers().get("X-Frame-Options").unwrap(),
            "SAMEORIGIN"
        );
    }

    #[tokio::test]
    async fn test_security_headers_csp_policy() {
        let app = axum::Router::new()
            .route("/test", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();

        let csp = response
            .headers()
            .get("Content-Security-Policy")
            .unwrap()
            .to_str()
            .unwrap();

        assert!(csp.contains("default-src 'self'"));
        assert!(csp.contains("media-src * blob:"));
        assert!(csp.contains("frame-ancestors 'none'"));
    }

    #[tokio::test]
    async fn test_security_headers_cache_control() {
        let app = axum::Router::new()
            .route("/test", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();

        let cache_control = response
            .headers()
            .get("Cache-Control")
            .unwrap()
            .to_str()
            .unwrap();

        assert!(cache_control.contains("no-store"));
        assert!(cache_control.contains("no-cache"));
        assert!(cache_control.contains("must-revalidate"));
    }

    // === AuthUser Tests (extracting behavior) ===

    #[test]
    fn test_auth_user_debug() {
        let auth_user = AuthUser {
            user_id: synctv_core::models::id::UserId::from_string("test123".to_string()),
            password_version: 0,
        };
        let debug_str = format!("{auth_user:?}");
        assert!(debug_str.contains("test123"));
    }

    #[test]
    fn test_auth_user_clone() {
        let auth_user = AuthUser {
            user_id: synctv_core::models::id::UserId::from_string("test123".to_string()),
            password_version: 0,
        };
        let cloned = auth_user;
        assert_eq!(cloned.user_id.as_str(), "test123");
    }

    // === RateLimitCategory Tests ===

    #[test]
    fn test_rate_limit_category_clone() {
        let cat = RateLimitCategory::Auth;
        let cloned = cat;
        assert!(matches!(cloned, RateLimitCategory::Auth));
    }

    // === Security Parity: HTTP middleware checks ===
    //
    // The HTTP AuthUser extractor performs security checks in order:
    // 1. JWT verification (validate signature, expiration, and access token type)
    // 2. Password invalidation check (reject tokens issued before password change)
    // 3. Banned/deleted user check (reject banned or soft-deleted users)
    //
    // These checks mirror the gRPC BlacklistCheckLayer to ensure consistent
    // security enforcement across both transport layers.
    //
    // Full integration tests require AppState with real services; the tests
    // below verify the structural aspects that can be tested in isolation.

    #[tokio::test]
    async fn test_security_headers_frame_ancestors_none() {
        // Verify CSP includes frame-ancestors 'none' to prevent clickjacking
        // (parity with X-Frame-Options: DENY)
        let app = axum::Router::new()
            .route("/test", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let request = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        let csp = response
            .headers()
            .get("Content-Security-Policy")
            .unwrap()
            .to_str()
            .unwrap();

        assert!(
            csp.contains("frame-ancestors 'none'"),
            "CSP must include frame-ancestors 'none' for clickjacking protection"
        );
        assert!(
            csp.contains("base-uri 'none'"),
            "CSP must include base-uri 'none' to prevent base tag injection"
        );
    }

    #[tokio::test]
    async fn test_security_headers_no_store_cache_control() {
        // Verify that API responses include no-store to prevent caching of
        // sensitive authentication data
        let app = axum::Router::new()
            .route(
                "/api/test",
                axum::routing::get(|| async { "sensitive data" }),
            )
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let request = Request::builder()
            .uri("/api/test")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let cache_control = response
            .headers()
            .get("Cache-Control")
            .unwrap()
            .to_str()
            .unwrap();

        assert!(cache_control.contains("no-store"));
        assert!(cache_control.contains("proxy-revalidate"));
    }

    // === HSTS Edge Cases ===

    #[test]
    fn test_hsts_header_large_max_age() {
        // 2 years is a common production value
        let header = hsts_header(63072000, true, true);
        assert!(header.starts_with("max-age=63072000"));
        assert!(header.contains("includeSubDomains"));
        assert!(header.contains("preload"));
    }

    #[test]
    fn test_hsts_header_min_max_age_for_preload() {
        // HSTS preload list requires max-age >= 31536000 (1 year)
        let header = hsts_header(31536000, true, true);
        assert_eq!(header, "max-age=31536000; includeSubDomains; preload");
    }

    // === Provider Routes Security Headers Tests ===
    //
    // Provider routes are registered with only rate-limit middleware, but the
    // global security_headers_middleware is applied to the entire router in
    // apply_global_layers(). These tests verify that nested routes still
    // receive security headers.

    #[tokio::test]
    async fn test_nested_routes_receive_security_headers() {
        // Simulate the Provider routes architecture:
        // 1. Inner router with a simple handler (no security layer)
        // 2. Outer router that nests the inner router
        // 3. Global security layer applied to the outer router

        let inner_router = axum::Router::new().route(
            "/api/providers/test",
            axum::routing::get(|| async { "provider response" }),
        );

        let outer_router = axum::Router::new()
            .merge(inner_router)
            // Global security layer applied AFTER route registration
            // (in Tower/Axum, layers are applied in reverse order)
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let request = Request::builder()
            .uri("/api/providers/test")
            .body(Body::empty())
            .unwrap();

        let response = outer_router.oneshot(request).await.unwrap();

        // Verify the nested route receives all security headers
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(
            headers.get("X-Frame-Options").unwrap(),
            "DENY",
            "Provider routes must have X-Frame-Options header"
        );
        assert_eq!(
            headers.get("X-Content-Type-Options").unwrap(),
            "nosniff",
            "Provider routes must have X-Content-Type-Options header"
        );
        assert_eq!(
            headers.get("X-XSS-Protection").unwrap(),
            "1; mode=block",
            "Provider routes must have X-XSS-Protection header"
        );
        assert!(
            headers.contains_key("Content-Security-Policy"),
            "Provider routes must have Content-Security-Policy header"
        );
        assert!(
            headers.contains_key("Referrer-Policy"),
            "Provider routes must have Referrer-Policy header"
        );
        assert!(
            headers.contains_key("Permissions-Policy"),
            "Provider routes must have Permissions-Policy header"
        );
    }

    #[tokio::test]
    async fn test_nested_routes_with_rate_limit_still_get_security_headers() {
        // Simulate the exact Provider routes architecture:
        // 1. Inner router with provider handlers
        // 2. Route layer with rate limiting
        // 3. Merged into outer router
        // 4. Global security layer applied

        async fn mock_rate_limit(
            request: axum::extract::Request,
            next: axum::middleware::Next,
        ) -> Result<Response, std::convert::Infallible> {
            // Mock rate limiter that always passes
            Ok(next.run(request).await)
        }

        let provider_router = axum::Router::new()
            .route(
                "/api/providers/bilibili/parse",
                axum::routing::post(|| async { "parsed" }),
            )
            .route_layer(axum::middleware::from_fn(mock_rate_limit));

        let app = axum::Router::new()
            .merge(provider_router)
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let request = Request::builder()
            .method("POST")
            .uri("/api/providers/bilibili/parse")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Verify security headers are present even with rate limit layer
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers().contains_key("X-Frame-Options"),
            "Provider routes with rate limit must still have X-Frame-Options"
        );
        assert!(
            response.headers().contains_key("X-Content-Type-Options"),
            "Provider routes with rate limit must still have X-Content-Type-Options"
        );
        assert!(
            response.headers().contains_key("Content-Security-Policy"),
            "Provider routes with rate limit must still have Content-Security-Policy"
        );
    }
}
