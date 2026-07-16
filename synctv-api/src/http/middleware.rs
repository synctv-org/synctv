// HTTP middleware

use axum::{
    extract::{FromRef, FromRequestParts, Request},
    http::request::Parts,
    middleware::Next,
    response::Response,
};
use std::future::{ready, Future};
use std::sync::LazyLock;

use super::{optional_header_str, AppError, AppState};

tokio::task_local! {
    pub static CURRENT_REQUEST_ID: String;
}

/// Transport metadata extracted from the HTTP request without performing
/// authentication, blacklist, rate-limit, or timeout decisions.
#[derive(Debug, Clone)]
pub struct RequestMetadata(pub crate::impls::RequestMetadata);

impl<S> FromRequestParts<S> for RequestMetadata
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = AppError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let result = (|| {
            let app_state = AppState::from_ref(state);
            let peer_ip = parts
                .extensions
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .map(|info| info.0.ip());
            let client_ip = peer_ip
                .map(|peer_ip| {
                    synctv_adapter::client_ip::extract_client_ip_from_headers(
                        |ip| app_state.runtime_settings.server.is_trusted_proxy(ip),
                        peer_ip,
                        &parts.headers,
                    )
                    .map_err(|error| AppError::bad_request(error.to_string()))
                })
                .transpose()?;
            super::reject_duplicate_header(&parts.headers, &axum::http::header::AUTHORIZATION)?;
            let authorization = parts
                .headers
                .get(axum::http::header::AUTHORIZATION)
                .map(|value| {
                    value
                        .to_str()
                        .map(str::to_owned)
                        .map_err(|_| AppError::invalid_authorization_header_non_utf8())
                })
                .transpose()?;
            let user_agent = optional_header_str(&parts.headers, &axum::http::header::USER_AGENT)?
                .map(str::to_owned);

            Ok(Self(
                crate::impls::RequestMetadata::new(crate::impls::TransportProtocol::Http)
                    .with_authorization(authorization)
                    .with_client_ip(client_ip)
                    .with_socket_ip(peer_ip)
                    .with_user_agent(user_agent)
                    .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT)),
            ))
        })();

        ready(result)
    }
}

/// HTTP header name for request/trace ID propagation.
static X_REQUEST_ID: LazyLock<axum::http::HeaderName> =
    LazyLock::new(|| axum::http::HeaderName::from_static("x-request-id"));

/// Middleware that generates a unique request ID per request.
///
/// - If the client sends an `X-Request-ID` header whose value is a non-empty
///   alphanumeric ASCII string of at most 64 characters, that value is reused
///   (allows end-to-end trace correlation from trusted clients).
/// - Otherwise a fresh 12-character shared base62 request ID is generated.
///
/// The request ID is:
/// 1. Recorded in the current tracing span as `request_id` for log correlation.
/// 2. Echoed back in the `X-Request-ID` response header so callers can correlate
///    logs with their own request tracking.
/// 3. Exposed via a task-local so `AppError` responses can include it without
///    buffering and rewriting response bodies.
pub async fn request_id_middleware(request: Request, next: Next) -> Response {
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
        .map_or_else(|| synctv_common::snanoid!(12), str::to_owned);

    // Record in current tracing span for log correlation.
    tracing::Span::current().record("requestId", request_id.as_str());
    tracing::debug!(request_id = %request_id, "Request received");

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
static CONTENT_SECURITY_POLICY: LazyLock<axum::http::HeaderName> =
    LazyLock::new(|| axum::http::HeaderName::from_static("content-security-policy"));
static REFERRER_POLICY: LazyLock<axum::http::HeaderName> =
    LazyLock::new(|| axum::http::HeaderName::from_static("referrer-policy"));
static PERMISSIONS_POLICY: LazyLock<axum::http::HeaderName> =
    LazyLock::new(|| axum::http::HeaderName::from_static("permissions-policy"));

/// Security headers middleware
///
/// Adds security-related HTTP headers to all responses to protect against
/// common web vulnerabilities:
/// - X-Frame-Options: Prevents clickjacking
/// - X-Content-Type-Options: Prevents MIME type sniffing
/// - Content-Security-Policy: Restricts resource loading
/// - Strict-Transport-Security: Enforces HTTPS (only if configured)
/// - Referrer-Policy: Controls referrer information
/// - Permissions-Policy: Restricts browser features
pub async fn security_headers_middleware(request: Request, next: Next) -> Response {
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

    // Content Security Policy
    // Default API responses should not grant broad media or framing privileges.
    // Routes that intentionally serve embeddable frontend/media content can set
    // their own CSP; this middleware preserves existing endpoint-specific
    // headers.
    if !headers.contains_key("Content-Security-Policy") {
        headers.insert(
            CONTENT_SECURITY_POLICY.clone(),
            axum::http::HeaderValue::from_static(
                "default-src 'self'; \
                 media-src 'none'; \
                 frame-src 'none'; \
                 connect-src 'self' wss: ws:; \
                 img-src 'self' data: https:; \
                 style-src 'self'; \
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

    // Cache Control for API responses.
    // Middleware only provides the safe default. Endpoints that intentionally
    // allow caching must set an explicit Cache-Control header themselves.
    if !headers.contains_key("Cache-Control") {
        headers.insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static(
                "no-store, no-cache, must-revalidate, proxy-revalidate",
            ),
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
    use synctv_core::service::AuthErrorCategory;
    use synctv_core::service::SecurityPipeline;
    use tower::ServiceExt;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    fn request(uri: &str) -> TestResult<Request<Body>> {
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .map_err(|err| test_error(format!("request should build: {err}")))
    }

    fn header_str<'a>(
        headers: &'a axum::http::HeaderMap,
        name: &'static str,
    ) -> TestResult<&'a str> {
        headers
            .get(name)
            .ok_or_else(|| test_error(format!("{name} header should exist")))?
            .to_str()
            .map_err(|err| test_error(format!("{name} header should be valid ascii: {err}")))
    }

    #[tokio::test]
    async fn test_security_headers_adds_all_headers() -> TestResult {
        let app = axum::Router::new()
            .route("/test", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let response = app.oneshot(request("/test")?).await?;

        assert_eq!(response.status(), StatusCode::OK);

        let headers = response.headers();
        assert_eq!(header_str(headers, "X-Frame-Options")?, "DENY");
        assert_eq!(header_str(headers, "X-Content-Type-Options")?, "nosniff");
        assert!(headers.contains_key("Content-Security-Policy"));
        assert!(headers.contains_key("Referrer-Policy"));
        assert!(headers.contains_key("Permissions-Policy"));
        assert!(headers.contains_key("Cache-Control"));
        Ok(())
    }

    #[tokio::test]
    async fn test_security_headers_does_not_overwrite_existing() -> TestResult {
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

        let response = app.oneshot(request("/test")?).await?;

        assert_eq!(
            header_str(response.headers(), "X-Frame-Options")?,
            "SAMEORIGIN"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_security_headers_csp_policy() -> TestResult {
        let app = axum::Router::new()
            .route("/test", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let response = app.oneshot(request("/test")?).await?;

        let csp = header_str(response.headers(), "Content-Security-Policy")?;

        assert!(csp.contains("default-src 'self'"));
        assert!(csp.contains("media-src 'none'"));
        assert!(csp.contains("frame-src 'none'"));
        assert!(csp.contains("style-src 'self'"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(csp.contains("base-uri 'none'"));
        Ok(())
    }

    #[tokio::test]
    async fn test_security_headers_cache_control() -> TestResult {
        let app = axum::Router::new()
            .route("/test", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let response = app.oneshot(request("/test")?).await?;

        let cache_control = header_str(response.headers(), "Cache-Control")?;

        assert!(cache_control.contains("no-store"));
        assert!(cache_control.contains("no-cache"));
        assert!(cache_control.contains("must-revalidate"));
        Ok(())
    }

    #[test]
    fn test_guest_user_maps_service_unavailable_to_503() {
        let err =
            match SecurityPipeline::classify_auth_error(&synctv_core::Error::ServiceUnavailable(
                "Authentication service temporarily unavailable".to_string(),
            )) {
                AuthErrorCategory::Authentication => {
                    AppError::unauthorized("unexpected authentication classification")
                }
                AuthErrorCategory::Authorization => {
                    AppError::forbidden("unexpected authorization classification")
                }
                AuthErrorCategory::Unavailable | AuthErrorCategory::Internal => {
                    AppError::from(synctv_core::Error::ServiceUnavailable(
                        "Authentication service temporarily unavailable".to_string(),
                    ))
                }
            };

        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_guest_user_room_existence_backend_failure_maps_to_503() {
        let room_lookup_error = synctv_core::Error::ServiceUnavailable(
            "room lookup temporarily unavailable".to_string(),
        );
        let err = match SecurityPipeline::classify_auth_error(&room_lookup_error) {
            AuthErrorCategory::Authentication => {
                AppError::unauthorized("unexpected authentication classification")
            }
            AuthErrorCategory::Authorization => {
                AppError::forbidden("unexpected authorization classification")
            }
            AuthErrorCategory::Unavailable | AuthErrorCategory::Internal => {
                AppError::from(room_lookup_error)
            }
        };

        assert_eq!(
            err.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "guest room existence backend failures must not be misreported as unauthorized"
        );
    }

    #[tokio::test]
    async fn test_security_headers_no_store_cache_control() -> TestResult {
        let app = axum::Router::new()
            .route(
                "/api/test",
                axum::routing::get(|| async { "sensitive data" }),
            )
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let response = app.oneshot(request("/api/test")?).await?;
        let cache_control = header_str(response.headers(), "Cache-Control")?;

        assert!(cache_control.contains("no-store"));
        assert!(cache_control.contains("proxy-revalidate"));
        Ok(())
    }

    #[test]
    fn test_hsts_header_large_max_age() {
        // 2 years is a common production value
        let header = hsts_header(63_072_000, true, true);
        assert!(header.starts_with("max-age=63072000"));
        assert!(header.contains("includeSubDomains"));
        assert!(header.contains("preload"));
    }

    #[test]
    fn test_hsts_header_min_max_age_for_preload() {
        // HSTS preload list requires max-age >= 31536000 (1 year)
        let header = hsts_header(31_536_000, true, true);
        assert_eq!(header, "max-age=31536000; includeSubDomains; preload");
    }

    #[tokio::test]
    async fn test_nested_routes_receive_security_headers() -> TestResult {
        let inner_router = axum::Router::new().route(
            "/api/providers/test",
            axum::routing::get(|| async { "provider response" }),
        );

        let outer_router = axum::Router::new()
            .merge(inner_router)
            .layer(axum::middleware::from_fn(security_headers_middleware));

        let response = outer_router
            .oneshot(request("/api/providers/test")?)
            .await?;

        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(
            header_str(headers, "X-Frame-Options")?,
            "DENY",
            "Provider routes must have X-Frame-Options header"
        );
        assert_eq!(
            header_str(headers, "X-Content-Type-Options")?,
            "nosniff",
            "Provider routes must have X-Content-Type-Options header"
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
        Ok(())
    }

    #[tokio::test]
    async fn test_nested_routes_with_additional_route_layers_still_get_security_headers(
    ) -> TestResult {
        async fn mock_rate_limit(
            request: axum::extract::Request,
            next: axum::middleware::Next,
        ) -> Result<Response, std::convert::Infallible> {
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
            .map_err(|err| test_error(format!("request should build: {err}")))?;

        let response = app.oneshot(request).await?;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers().contains_key("X-Frame-Options"),
            "Provider routes with extra route layers must still have X-Frame-Options"
        );
        assert!(
            response.headers().contains_key("X-Content-Type-Options"),
            "Provider routes with extra route layers must still have X-Content-Type-Options"
        );
        assert!(
            response.headers().contains_key("Content-Security-Policy"),
            "Provider routes with extra route layers must still have Content-Security-Policy"
        );
        Ok(())
    }
}
