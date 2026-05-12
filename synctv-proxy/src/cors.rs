use std::sync::Arc;

use axum::{body::Body, http::StatusCode, response::Response};

/// Standard CORS headers for preflight requests.
const CORS_ALLOW_METHODS: &str = "GET, HEAD, OPTIONS";
const CORS_ALLOW_HEADERS: &str = "Authorization, Content-Type, Accept, Range";
const CORS_MAX_AGE: &str = "86400";

/// Headers that frontend JavaScript can read from proxy responses.
///
/// By default, browsers only expose "simple" response headers to JavaScript.
/// This header allows frontend code to read custom headers like Content-Range
/// (needed for video seeking), cache status, and other useful metadata.
const CORS_EXPOSE_HEADERS: &str = "Content-Range, Accept-Ranges, Content-Length, Content-Type, Cache-Control, ETag, Last-Modified, X-Content-Type-Options";

/// Build a rate-limit response (429 Too Many Requests).
#[cfg(test)]
fn build_rate_limit_response() -> Response {
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("Content-Type", "text/plain")
        .header("Retry-After", "60")
        .body(Body::from("Too Many Requests"))
        .expect("Failed to build rate limit response")
}

/// Build a CORS preflight response for wildcard mode.
///
/// Returns 204 No Content with `Access-Control-Allow-Origin: *`.
fn build_wildcard_cors_response() -> Response {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Allow-Methods", CORS_ALLOW_METHODS)
        .header("Access-Control-Allow-Headers", CORS_ALLOW_HEADERS)
        .header("Access-Control-Expose-Headers", CORS_EXPOSE_HEADERS)
        .header("Access-Control-Max-Age", CORS_MAX_AGE)
        .body(Body::empty())
        .expect("Failed to build wildcard CORS response")
}

/// Build a CORS preflight response when no Origin header is present.
///
/// Returns 204 No Content without Access-Control-Allow-Origin header.
fn build_no_origin_cors_response() -> Response {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("Access-Control-Allow-Methods", CORS_ALLOW_METHODS)
        .header("Access-Control-Allow-Headers", CORS_ALLOW_HEADERS)
        .header("Access-Control-Expose-Headers", CORS_EXPOSE_HEADERS)
        .header("Access-Control-Max-Age", CORS_MAX_AGE)
        .body(Body::empty())
        .expect("Failed to build no-origin CORS response")
}

/// Build a CORS preflight response for a forbidden origin.
///
/// Returns 403 Forbidden with plain text error message.
fn build_forbidden_cors_response() -> Response {
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header("Content-Type", "text/plain")
        .body(Body::from("Origin not allowed"))
        .expect("Failed to build forbidden CORS response")
}

/// Build a CORS preflight response for an allowed origin.
///
/// Returns 204 No Content with the origin echoed back and credentials allowed.
fn build_allowed_cors_response(origin: &str) -> Response {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("Access-Control-Allow-Origin", origin)
        .header("Access-Control-Allow-Methods", CORS_ALLOW_METHODS)
        .header("Access-Control-Allow-Headers", CORS_ALLOW_HEADERS)
        .header("Access-Control-Expose-Headers", CORS_EXPOSE_HEADERS)
        .header("Access-Control-Allow-Credentials", "true")
        .header("Access-Control-Max-Age", CORS_MAX_AGE)
        .header("Vary", "Origin")
        .body(Body::empty())
        .expect("Failed to build allowed CORS response")
}

/// Core CORS preflight logic shared between all preflight handlers.
///
/// Returns the appropriate response based on the CORS configuration and origin.
fn handle_cors_preflight(origin: Option<&str>, config: &CorsConfig) -> Response {
    if config.wildcard {
        return build_wildcard_cors_response();
    }

    let Some(origin) = origin else {
        return build_no_origin_cors_response();
    };

    if !config.is_allowed(origin) {
        return build_forbidden_cors_response();
    }

    build_allowed_cors_response(origin)
}

/// CORS configuration for the proxy.
///
/// Controls which origins are allowed to access the proxy endpoints.
/// By default, no origins are allowed (secure by default).
#[derive(Clone, Default)]
pub struct CorsConfig {
    /// List of allowed origins. Empty means no origins allowed.
    allowed_origins: Vec<String>,
    /// If true, allow all origins (wildcard mode).
    wildcard: bool,
}

impl CorsConfig {
    /// Create a new CORS config with the given allowed origins.
    ///
    /// # Arguments
    ///
    /// * `allowed_origins` - List of origin URLs that are allowed to access the proxy.
    ///
    /// # Example
    ///
    /// ```
    /// use synctv_proxy::CorsConfig;
    ///
    /// let config = CorsConfig::new(vec![
    ///     "https://example.com".to_string(),
    ///     "https://app.example.com".to_string(),
    /// ]);
    /// ```
    #[must_use]
    pub const fn new(allowed_origins: Vec<String>) -> Self {
        Self {
            allowed_origins,
            wildcard: false,
        }
    }

    /// Create a CORS config that allows all origins (wildcard mode).
    ///
    /// **Warning**: This is less secure than explicit origin lists.
    /// Use only in development or when you intentionally want to allow all origins.
    #[must_use]
    pub const fn new_wildcard() -> Self {
        Self {
            allowed_origins: vec![],
            wildcard: true,
        }
    }

    /// Check if an origin is allowed.
    fn is_allowed(&self, origin: &str) -> bool {
        if self.wildcard {
            return true;
        }
        self.allowed_origins.iter().any(|o| o == origin)
    }

    /// Check if wildcard mode is enabled.
    #[cfg(test)]
    #[must_use]
    pub const fn is_wildcard(&self) -> bool {
        self.wildcard
    }
}

/// Preflight handler with explicit CORS origin validation.
///
/// Returns 403 Forbidden if the origin is not in the allowed list,
/// otherwise returns proper CORS headers echoing the origin back.
///
/// # Arguments
///
/// * `origin` - The Origin header value from the request.
/// * `config` - The CORS configuration.
///
/// # Security
///
/// - Origins not in the allowed list receive 403 Forbidden.
/// - When the allowed list is empty, all origins are rejected (secure default).
/// - The `Vary: Origin` header is included for proper caching.
#[allow(clippy::unused_async)]
pub async fn proxy_options_preflight_with_cors(
    origin: Option<&str>,
    config: Arc<CorsConfig>,
) -> Response {
    handle_cors_preflight(origin, &config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_rate_limit_response() {
        let response = build_rate_limit_response();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get("Content-Type")
                .map(|v| v.to_str().unwrap()),
            Some("text/plain")
        );
        assert_eq!(
            response
                .headers()
                .get("Retry-After")
                .map(|v| v.to_str().unwrap()),
            Some("60")
        );
    }

    #[test]
    fn test_build_wildcard_cors_response() {
        let response = build_wildcard_cors_response();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Allow-Origin")
                .map(|v| v.to_str().unwrap()),
            Some("*")
        );
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Allow-Methods")
                .map(|v| v.to_str().unwrap()),
            Some("GET, HEAD, OPTIONS")
        );
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Allow-Headers")
                .map(|v| v.to_str().unwrap()),
            Some("Authorization, Content-Type, Accept, Range")
        );
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Max-Age")
                .map(|v| v.to_str().unwrap()),
            Some("86400")
        );
        assert!(response
            .headers()
            .get("Access-Control-Allow-Credentials")
            .is_none());
        assert!(response.headers().get("Vary").is_none());
    }

    #[test]
    fn test_build_no_origin_cors_response() {
        let response = build_no_origin_cors_response();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(response
            .headers()
            .get("Access-Control-Allow-Origin")
            .is_none());
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Allow-Methods")
                .map(|v| v.to_str().unwrap()),
            Some("GET, HEAD, OPTIONS")
        );
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Allow-Headers")
                .map(|v| v.to_str().unwrap()),
            Some("Authorization, Content-Type, Accept, Range")
        );
    }

    #[test]
    fn test_build_forbidden_cors_response() {
        let response = build_forbidden_cors_response();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response
                .headers()
                .get("Content-Type")
                .map(|v| v.to_str().unwrap()),
            Some("text/plain")
        );
    }

    #[test]
    fn test_build_allowed_cors_response() {
        let origin = "https://example.com";
        let response = build_allowed_cors_response(origin);

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Allow-Origin")
                .map(|v| v.to_str().unwrap()),
            Some(origin)
        );
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Allow-Credentials")
                .map(|v| v.to_str().unwrap()),
            Some("true")
        );
        assert_eq!(
            response.headers().get("Vary").map(|v| v.to_str().unwrap()),
            Some("Origin")
        );
    }

    #[test]
    fn test_handle_cors_preflight_wildcard_mode() {
        let config = CorsConfig::new_wildcard();
        let response = handle_cors_preflight(Some("https://example.com"), &config);

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Allow-Origin")
                .map(|v| v.to_str().unwrap()),
            Some("*")
        );
    }

    #[test]
    fn test_handle_cors_preflight_no_origin_header() {
        let config = CorsConfig::new(vec!["https://example.com".to_string()]);
        let response = handle_cors_preflight(None, &config);

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(response
            .headers()
            .get("Access-Control-Allow-Origin")
            .is_none());
    }

    #[test]
    fn test_handle_cors_preflight_origin_not_allowed() {
        let config = CorsConfig::new(vec!["https://allowed.com".to_string()]);
        let response = handle_cors_preflight(Some("https://evil.com"), &config);

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_handle_cors_preflight_origin_allowed() {
        let allowed_origin = "https://allowed.com";
        let config = CorsConfig::new(vec![allowed_origin.to_string()]);
        let response = handle_cors_preflight(Some(allowed_origin), &config);

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Allow-Origin")
                .map(|v| v.to_str().unwrap()),
            Some(allowed_origin)
        );
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Allow-Credentials")
                .map(|v| v.to_str().unwrap()),
            Some("true")
        );
        assert_eq!(
            response.headers().get("Vary").map(|v| v.to_str().unwrap()),
            Some("Origin")
        );
    }

    #[test]
    fn test_handle_cors_preflight_empty_allowed_list_rejects_all() {
        let config = CorsConfig::new(vec![]);
        let response = handle_cors_preflight(Some("https://example.com"), &config);

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
