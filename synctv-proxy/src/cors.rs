use std::sync::Arc;

use axum::{
    body::Body,
    http::{header, HeaderName, HeaderValue, StatusCode},
    response::Response,
};

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

fn empty_response(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

fn text_response(status: StatusCode, body: &'static str) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response
}

fn insert_static_header(response: &mut Response, name: &'static str, value: &'static str) {
    response.headers_mut().insert(
        HeaderName::from_static(name),
        HeaderValue::from_static(value),
    );
}

fn insert_preflight_headers(response: &mut Response) {
    insert_static_header(response, "access-control-allow-methods", CORS_ALLOW_METHODS);
    insert_static_header(response, "access-control-allow-headers", CORS_ALLOW_HEADERS);
    insert_static_header(
        response,
        "access-control-expose-headers",
        CORS_EXPOSE_HEADERS,
    );
    insert_static_header(response, "access-control-max-age", CORS_MAX_AGE);
}

/// Build a rate-limit response (429 Too Many Requests).
#[cfg(test)]
fn build_rate_limit_response() -> Response {
    let mut response = text_response(StatusCode::TOO_MANY_REQUESTS, "Too Many Requests");
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    response
}

/// Build a CORS preflight response for wildcard mode.
///
/// Returns 204 No Content with `Access-Control-Allow-Origin: *`.
fn build_wildcard_cors_response() -> Response {
    let mut response = empty_response(StatusCode::NO_CONTENT);
    insert_static_header(&mut response, "access-control-allow-origin", "*");
    insert_preflight_headers(&mut response);
    response
}

/// Build a CORS preflight response when no Origin header is present.
///
/// Returns 204 No Content without Access-Control-Allow-Origin header.
fn build_no_origin_cors_response() -> Response {
    let mut response = empty_response(StatusCode::NO_CONTENT);
    insert_preflight_headers(&mut response);
    response
}

/// Build a CORS preflight response for a forbidden origin.
///
/// Returns 403 Forbidden with plain text error message.
fn build_forbidden_cors_response() -> Response {
    let mut response = text_response(StatusCode::FORBIDDEN, "Origin not allowed");
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    response
}

/// Build a CORS preflight response for an allowed origin.
///
/// Returns 204 No Content with the origin echoed back.
fn build_allowed_cors_response(origin: &str) -> Response {
    let Ok(origin) = HeaderValue::from_str(origin) else {
        return build_forbidden_cors_response();
    };

    let mut response = empty_response(StatusCode::NO_CONTENT);
    response.headers_mut().insert(
        HeaderName::from_static("access-control-allow-origin"),
        origin,
    );
    insert_preflight_headers(&mut response);
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Origin"));
    response
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
#[path = "cors_tests.rs"]
mod tests;
