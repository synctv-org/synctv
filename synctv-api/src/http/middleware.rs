// HTTP middleware

use axum::{
    extract::{FromRef, FromRequestParts, OptionalFromRequestParts, Request, State},
    http::{request::Parts, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::LazyLock;
use synctv_core::{
    models::{id::UserId, UserStatus},
    service::{auth::JwtValidator, rate_limit::RateLimitError},
};

use super::{AppError, AppState};

/// Pre-validated security header names (validated once at startup via Lazy)
static X_FRAME_OPTIONS: LazyLock<axum::http::HeaderName> = LazyLock::new(|| {
    axum::http::HeaderName::from_static("x-frame-options")
});
static X_CONTENT_TYPE_OPTIONS: LazyLock<axum::http::HeaderName> = LazyLock::new(|| {
    axum::http::HeaderName::from_static("x-content-type-options")
});
static X_XSS_PROTECTION: LazyLock<axum::http::HeaderName> = LazyLock::new(|| {
    axum::http::HeaderName::from_static("x-xss-protection")
});
static CONTENT_SECURITY_POLICY: LazyLock<axum::http::HeaderName> = LazyLock::new(|| {
    axum::http::HeaderName::from_static("content-security-policy")
});
static REFERRER_POLICY: LazyLock<axum::http::HeaderName> = LazyLock::new(|| {
    axum::http::HeaderName::from_static("referrer-policy")
});
static PERMISSIONS_POLICY: LazyLock<axum::http::HeaderName> = LazyLock::new(|| {
    axum::http::HeaderName::from_static("permissions-policy")
});
static PRAGMA: LazyLock<axum::http::HeaderName> = LazyLock::new(|| {
    axum::http::HeaderName::from_static("pragma")
});

/// Authenticated user extracted from JWT token
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: UserId,
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
        // We extract full claims (not just user_id) so we can check the
        // issued-at timestamp against password-change invalidation.
        let auth_str = auth_header
            .to_str()
            .map_err(|e| AppError::unauthorized(format!("Invalid Authorization header: {e}")))?;

        let claims = validator
            .validate_http(auth_str)
            .map_err(|e| AppError::unauthorized(format!("{e}")))?;

        // Check if the token has been revoked (e.g. after logout).
        // Extract the raw bearer token for blacklist lookup.
        let raw_token = JwtValidator::extract_bearer_token(auth_str)
            .map_err(|e| AppError::unauthorized(format!("{e}")))?;
        if app_state
            .token_blacklist_service
            .is_blacklisted(&raw_token)
            .await
            .unwrap_or(true) // Fail closed: deny if blacklist check errors
        {
            return Err(AppError::unauthorized("Token has been revoked"));
        }

        let user_id = UserId::from_string(claims.sub);

        // Check if user is banned or deleted (defense-in-depth: catches banned
        // users even if they hold a valid JWT issued before the ban)
        let user = app_state.user_service.get_user(&user_id).await
            .map_err(|_| AppError::unauthorized("User not found"))?;
        if user.is_deleted() || user.status == UserStatus::Banned {
            return Err(AppError::unauthorized("Authentication failed"));
        }

        // Reject tokens issued before the user's last password change.
        // This ensures that stolen tokens become useless after a password reset.
        if app_state
            .user_service
            .is_token_invalidated_by_password_change(&user_id, claims.iat)
            .await
            .unwrap_or(false)
        {
            return Err(AppError::unauthorized(
                "Token invalidated due to password change. Please log in again.",
            ));
        }

        Ok(Self { user_id })
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
        if parts.headers.get(axum::http::header::AUTHORIZATION).is_none() {
            return Ok(None);
        }
        // Header IS present: authenticate and propagate errors for invalid tokens
        // (don't silently downgrade to anonymous when the token is malformed/expired)
        let user = <Self as FromRequestParts<S>>::from_request_parts(parts, state).await?;
        Ok(Some(user))
    }
}

/// Rate limiting configuration for different endpoint categories
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Authentication endpoints (login, register) - stricter limits
    pub auth_max_requests: u32,
    pub auth_window_seconds: u64,

    /// Write operations (create, update, delete) - moderate limits
    pub write_max_requests: u32,
    pub write_window_seconds: u64,

    /// Read operations (get, list) - relaxed limits
    pub read_max_requests: u32,
    pub read_window_seconds: u64,

    /// Media operations (add, remove media) - moderate limits
    pub media_max_requests: u32,
    pub media_window_seconds: u64,

    /// Admin operations - moderate limits to prevent brute force
    pub admin_max_requests: u32,
    pub admin_window_seconds: u64,

    /// Streaming operations (FLV/HLS) - per-user concurrency limits
    pub streaming_max_requests: u32,
    pub streaming_window_seconds: u64,

    /// WebSocket connection attempts
    pub websocket_max_requests: u32,
    pub websocket_window_seconds: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            // Auth: 5 requests per minute
            auth_max_requests: 5,
            auth_window_seconds: 60,

            // Write: 30 requests per minute
            write_max_requests: 30,
            write_window_seconds: 60,

            // Read: 100 requests per minute
            read_max_requests: 100,
            read_window_seconds: 60,

            // Media: 20 requests per minute
            media_max_requests: 20,
            media_window_seconds: 60,

            // Admin: 30 requests per minute
            admin_max_requests: 30,
            admin_window_seconds: 60,

            // Streaming: 50 requests per minute (playlist + segment fetches)
            streaming_max_requests: 50,
            streaming_window_seconds: 60,

            // WebSocket: 10 connection attempts per minute
            websocket_max_requests: 10,
            websocket_window_seconds: 60,
        }
    }
}

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
    // Extract user ID from authorization header if present
    let user_id = extract_user_id_from_header(&request, &state);

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
        let should_trust_headers = state.config.server.development_mode
            || remote_addr.is_some_and(|ip| state.config.server.is_trusted_proxy(&ip));

        if should_trust_headers {
            // Trust X-Forwarded-For from trusted proxies (or in dev mode)
            let forwarded = request
                .headers()
                .get("X-Forwarded-For")
                .and_then(|h| h.to_str().ok())
                .and_then(|v| v.split(',').next())
                .map(str::trim);

            if let Some(ip) = forwarded {
                ip.to_string()
            } else if let Some(ip) = request
                .headers()
                .get("X-Real-IP")
                .and_then(|h| h.to_str().ok())
            {
                ip.to_string()
            } else if let Some(ip) = remote_addr {
                ip.to_string()
            } else {
                "unknown".to_string()
            }
        } else {
            // Don't trust headers - use socket address directly
            remote_addr.map_or_else(|| "unknown".to_string(), |ip| ip.to_string())
        }
    });

    // Get rate limiter and config from app state (config is shared, not per-request)
    let rate_limiter = state.rate_limiter.clone();
    let config = &state.rate_limit_config;

    // Determine rate limit parameters based on category
    let (max_requests, window_seconds, category_name) = match category {
        RateLimitCategory::Auth => (config.auth_max_requests, config.auth_window_seconds, "auth"),
        RateLimitCategory::Write => (config.write_max_requests, config.write_window_seconds, "write"),
        RateLimitCategory::Read => (config.read_max_requests, config.read_window_seconds, "read"),
        RateLimitCategory::Media => (config.media_max_requests, config.media_window_seconds, "media"),
        RateLimitCategory::Admin => (config.admin_max_requests, config.admin_window_seconds, "admin"),
        RateLimitCategory::Streaming => (config.streaming_max_requests, config.streaming_window_seconds, "streaming"),
        RateLimitCategory::WebSocket => (config.websocket_max_requests, config.websocket_window_seconds, "websocket"),
    };

    // Check rate limit
    // FIXED: P0.13 - Removed path from key to enforce category-wide limit
    // Previously: format!("{}:{}:{}", category_name, rate_limit_key, path)
    // This caused each endpoint to have its own counter, effectively multiplying the limit
    // Now: All endpoints in same category share the limit (e.g., 30 req/min for ALL write operations)
    let key = format!("ratelimit:{category_name}:{rate_limit_key}");
    match rate_limiter.check_rate_limit(&key, max_requests, window_seconds).await {
        Ok(()) => {
            // Rate limit check passed, proceed with request
            Ok(next.run(request).await)
        }
        Err(RateLimitError::RateLimitExceeded { retry_after_seconds }) => {
            // Rate limit exceeded, return 429 Too Many Requests
            let response = (
                StatusCode::TOO_MANY_REQUESTS,
                [
                    ("Retry-After", retry_after_seconds.to_string()),
                    ("X-RateLimit-Limit", max_requests.to_string()),
                    ("X-RateLimit-Reset", retry_after_seconds.to_string()),
                ],
                format!("Rate limit exceeded. Try again in {retry_after_seconds} seconds"),
            )
                .into_response();

            Ok(response)
        }
        Err(e) => {
            // This branch should not be reached for check_rate_limit (which
            // degrades to in-memory on Redis errors), but handle defensively.
            tracing::error!("Rate limit check unexpected error: {}. Denying request (fail closed).", e);
            let response = (
                StatusCode::TOO_MANY_REQUESTS,
                "Rate limiting temporarily degraded. Please try again shortly.",
            )
                .into_response();
            Ok(response)
        }
    }
}

/// Helper function to extract user ID from authorization header
fn extract_user_id_from_header(request: &Request, state: &AppState) -> Option<String> {
    let auth_header = request.headers().get(axum::http::header::AUTHORIZATION)?;
    let auth_str = auth_header.to_str().ok()?;

    // Use shared JwtValidator from AppState (not per-request creation)
    let user_id = state.jwt_validator.validate_http_extract_user_id(auth_str).ok()?;

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
pub async fn security_headers_middleware(
    request: Request,
    next: Next,
) -> Response {
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
                 base-uri 'none'"
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
                 magnetometer=(), microphone=(), payment=(), usb=()"
            ),
        );
    }

    // Cache Control for API responses
    // Prevents caching of sensitive API responses
    if !headers.contains_key("Cache-Control") {
        headers.insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static(
                "no-store, no-cache, must-revalidate, proxy-revalidate"
            ),
        );
    }

    // Pragma: no-cache (for HTTP/1.0 compatibility)
    if !headers.contains_key("Pragma") {
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
        assert_eq!(config.streaming_max_requests, 50);
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
            .route(
                "/test",
                axum::routing::get(|| async { "ok" }),
            )
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let request = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();

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
                        [(axum::http::header::HeaderName::from_static("x-frame-options"), "SAMEORIGIN")],
                        "ok",
                    )
                }),
            )
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let request = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();

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
            .route(
                "/test",
                axum::routing::get(|| async { "ok" }),
            )
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let request = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();

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
            .route(
                "/test",
                axum::routing::get(|| async { "ok" }),
            )
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let request = Request::builder()
            .uri("/test")
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
        assert!(cache_control.contains("no-cache"));
        assert!(cache_control.contains("must-revalidate"));
    }

    // === AuthUser Tests (extracting behavior) ===

    #[test]
    fn test_auth_user_debug() {
        let auth_user = AuthUser {
            user_id: synctv_core::models::id::UserId::from_string("test123".to_string()),
        };
        let debug_str = format!("{auth_user:?}");
        assert!(debug_str.contains("test123"));
    }

    #[test]
    fn test_auth_user_clone() {
        let auth_user = AuthUser {
            user_id: synctv_core::models::id::UserId::from_string("test123".to_string()),
        };
        let cloned = auth_user.clone();
        assert_eq!(cloned.user_id.as_str(), "test123");
    }

    // === RateLimitCategory Tests ===

    #[test]
    fn test_rate_limit_category_debug() {
        // Ensure all variants are Debug-printable (compile-time check mostly)
        let categories = [
            RateLimitCategory::Auth,
            RateLimitCategory::Write,
            RateLimitCategory::Read,
            RateLimitCategory::Media,
            RateLimitCategory::Admin,
            RateLimitCategory::Streaming,
            RateLimitCategory::WebSocket,
        ];
        for cat in categories {
            let s = format!("{cat:?}");
            assert!(!s.is_empty());
        }
    }

    #[test]
    fn test_rate_limit_category_clone() {
        let cat = RateLimitCategory::Auth;
        let cloned = cat;
        assert!(matches!(cloned, RateLimitCategory::Auth));
    }
}
