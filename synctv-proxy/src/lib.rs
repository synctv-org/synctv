#![cfg_attr(test, allow(clippy::unwrap_used))]
//! Shared media proxy utilities
//!
//! Provides reusable functions for proxying media streams and rewriting M3U8
//! playlists.  Used by per-provider proxy routes in `synctv-api`.

pub mod slice_cache;

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::time::Duration;

use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use futures::StreamExt;
use reqwest::header::{HeaderName, HeaderValue, REFERER, USER_AGENT};

/// Maximum response body size for proxied media (256 MB).
const MAX_PROXY_BODY_SIZE: usize = 256 * 1024 * 1024;

/// Maximum response body size for M3U8/MPD manifests (10 MB).
const MAX_MANIFEST_SIZE: usize = 10 * 1024 * 1024;

/// Check if an HTTP status code is retryable.
///
/// Only specific 5xx errors that indicate transient issues should be retried:
/// - 500 Internal Server Error: Generic server error, often transient
/// - 502 Bad Gateway: Upstream server issue, may resolve quickly
/// - 503 Service Unavailable: Temporary overload, likely to recover
/// - 504 Gateway Timeout: Upstream timeout, may succeed on retry
///
/// Status codes like 501 (Not Implemented) and 505 (HTTP Version Not Supported)
/// are permanent errors and should NOT be retried.
#[must_use]
pub fn is_retryable_status(status: StatusCode) -> bool {
    RETRYABLE_STATUS_CODES.contains(&status.as_u16())
}

/// Calculate a random delay for retry attempts.
///
/// Returns a duration between `RETRY_DELAY_MIN_MS` and `RETRY_DELAY_MAX_MS`.
/// Using random delay helps prevent thundering herd when multiple clients
/// retry simultaneously.
fn calculate_retry_delay() -> Duration {
    use rand::RngExt;
    // Use proper random jitter to prevent thundering herd
    let mut rng = rand::rng();
    let jitter = rng.random_range(0..=(RETRY_DELAY_MAX_MS - RETRY_DELAY_MIN_MS));
    Duration::from_millis(RETRY_DELAY_MIN_MS + jitter)
}

/// Timeout for reading the response body after headers are received.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Minimum delay before retrying a failed request (100ms).
const RETRY_DELAY_MIN_MS: u64 = 100;

/// Maximum delay before retrying a failed request (500ms).
const RETRY_DELAY_MAX_MS: u64 = 500;

/// HTTP status codes that are retryable (transient server errors).
/// These indicate temporary issues that may resolve on retry.
const RETRYABLE_STATUS_CODES: &[u16] = &[500, 502, 503, 504];

/// Maximum number of redirects to follow manually.
const MAX_REDIRECTS: usize = 10;

/// Build a proxy HTTP client for outbound media requests.
///
/// Callers are expected to build this once during startup and inject it into
/// the proxy/cache layers rather than relying on hidden process-global state.
pub fn build_proxy_http_client() -> Result<reqwest::Client, anyhow::Error> {
    synctv_common::http::build_proxy_client()
        .map_err(|e| anyhow::anyhow!("failed to build proxy HTTP client: {e}"))
}

/// Configuration for a single proxy fetch.
pub struct ProxyConfig<'a> {
    /// Shared outbound HTTP client.
    pub client: &'a reqwest::Client,
    /// The remote URL to fetch.
    pub url: &'a str,
    /// Extra headers the provider requires (e.g. Referer, cookies).
    pub provider_headers: &'a HashMap<String, String>,
    /// Original client request headers to forward.
    pub client_headers: &'a HeaderMap,
}

/// Apply provider headers and defaults (User-Agent, Referer) to a request builder.
pub fn apply_provider_headers<S: BuildHasher>(
    mut request: reqwest::RequestBuilder,
    url: &str,
    provider_headers: &HashMap<String, String, S>,
) -> Result<reqwest::RequestBuilder, anyhow::Error> {
    let mut has_user_agent = false;
    let mut has_referer = false;

    for (name, value) in provider_headers {
        let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
            ProxyError::InvalidRequest(format!("invalid provider header name `{name}`: {e}"))
        })?;
        let header_value = HeaderValue::from_str(value).map_err(|e| {
            ProxyError::InvalidRequest(format!("invalid provider header value for `{name}`: {e}"))
        })?;

        if header_name == USER_AGENT {
            has_user_agent = true;
        }
        if header_name == REFERER {
            has_referer = true;
        }

        request = request.header(header_name, header_value);
    }

    if !has_user_agent {
        request = request.header(
            USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        );
    }

    if !has_referer {
        if let Ok(parsed) = url::Url::parse(url) {
            let referer = format!(
                "{}://{}{}",
                parsed.scheme(),
                parsed.host_str().unwrap_or(""),
                parsed.path()
            );
            request = request.header(REFERER, referer);
        }
    }

    Ok(request)
}

/// Callback for proxy metrics reporting.
///
/// Called after each proxy fetch completes. Implementations can record
/// latency, error counts, etc. to their preferred metrics backend.
pub trait ProxyMetrics: Send + Sync {
    /// Called when a proxy fetch completes (success or failure).
    fn on_proxy_complete(&self, protocol: &str, duration: Duration, error: Option<&str>);
}

/// No-op metrics implementation used when callers don't need metrics.
pub struct NoopMetrics;

impl ProxyMetrics for NoopMetrics {
    fn on_proxy_complete(&self, _protocol: &str, _duration: Duration, _error: Option<&str>) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyErrorKind {
    Timeout,
    Connection,
    Ssrf,
    InvalidRequest,
    Upstream,
    Other,
}

impl ProxyErrorKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Connection => "connection",
            Self::Ssrf => "ssrf",
            Self::InvalidRequest => "invalid_request",
            Self::Upstream => "upstream",
            Self::Other => "other",
        }
    }
}

#[derive(Debug)]
enum ProxyError {
    Timeout(String),
    Connection(String),
    Ssrf(String),
    InvalidRequest(String),
    Upstream(String),
    Other(String),
}

impl ProxyError {
    const fn kind(&self) -> ProxyErrorKind {
        match self {
            Self::Timeout(_) => ProxyErrorKind::Timeout,
            Self::Connection(_) => ProxyErrorKind::Connection,
            Self::Ssrf(_) => ProxyErrorKind::Ssrf,
            Self::InvalidRequest(_) => ProxyErrorKind::InvalidRequest,
            Self::Upstream(_) => ProxyErrorKind::Upstream,
            Self::Other(_) => ProxyErrorKind::Other,
        }
    }
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout(message) => write!(f, "Request timed out: {message}"),
            Self::Connection(message) => write!(f, "Connection failed: {message}"),
            Self::Ssrf(message) => write!(f, "SSRF protection blocked request: {message}"),
            Self::InvalidRequest(message) => write!(f, "Invalid proxy request: {message}"),
            Self::Upstream(message) => write!(f, "Upstream rejected request: {message}"),
            Self::Other(message) => write!(f, "Proxy request failed: {message}"),
        }
    }
}

impl std::error::Error for ProxyError {}

/// Fetch a remote URL and return the response.
///
/// The `metrics` parameter allows callers to inject their own metrics
/// recording without this crate depending on any specific metrics library.
pub async fn proxy_fetch_and_forward(
    cfg: ProxyConfig<'_>,
    metrics: &dyn ProxyMetrics,
) -> Result<Response, anyhow::Error> {
    let start = std::time::Instant::now();

    let result = proxy_fetch_and_forward_inner(cfg).await;

    let elapsed = start.elapsed();
    let error_type = match &result {
        Ok(_) => None,
        Err(e) => e
            .downcast_ref::<ProxyError>()
            .map(|err| err.kind().as_str()),
    };

    // Derive the media type label from the Content-Type header of the proxied
    // response rather than hard-coding "hls".
    let media_type = match &result {
        Ok(resp) => {
            let ct = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if ct.contains("mpegurl") || ct.contains("m3u8") {
                "hls"
            } else if ct.contains("dash") || ct.contains("mpd") {
                "dash"
            } else if ct.contains("video/") {
                "video"
            } else if ct.contains("audio/") {
                "audio"
            } else if ct.contains("octet-stream") {
                "binary"
            } else {
                "other"
            }
        }
        Err(_) => "unknown",
    };
    metrics.on_proxy_complete(media_type, elapsed, error_type);

    result
}

/// Allowlisted client headers forwarded to the upstream origin.
///
/// Only these headers are passed through to avoid leaking auth tokens, cookies,
/// or other sensitive data from the original client request.
const CLIENT_HEADER_ALLOWLIST: &[&str] = &[
    "range",
    "if-none-match",
    "if-modified-since",
    "accept",
    "accept-language",
    "user-agent",
];

/// Build a proxy request for the given URL, forwarding allowlisted client
/// headers and applying provider-specific headers.
///
/// This is the single point of request construction used by both the initial
/// fetch and retry attempts.
fn build_proxy_request(cfg: &ProxyConfig<'_>) -> Result<reqwest::RequestBuilder, anyhow::Error> {
    let mut request = cfg.client.get(cfg.url);

    // Forward only allowlisted client headers to avoid leaking auth tokens / cookies
    for (name, value) in cfg.client_headers {
        if !CLIENT_HEADER_ALLOWLIST.contains(&name.as_str()) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            request = request.header(name.as_str(), v);
        }
    }

    apply_provider_headers(request, cfg.url, cfg.provider_headers)
}

fn validate_target_url_against_ssrf(url: &url::Url) -> Result<(), ProxyError> {
    let host = url
        .host_str()
        .ok_or_else(|| ProxyError::InvalidRequest("URL host is required".to_string()))?;

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if synctv_common::ssrf::SsrfGuard::shared_default().is_ip_blocked(&ip) {
            return Err(ProxyError::Ssrf(format!(
                "target host `{host}` is blocked by SSRF policy"
            )));
        }
    } else if synctv_common::ssrf::SsrfGuard::shared_default().is_host_blocked(host) {
        return Err(ProxyError::Ssrf(format!(
            "target host `{host}` is blocked by SSRF policy"
        )));
    }

    Ok(())
}

/// Inner implementation of proxy fetch, separated for metrics wrapping.
async fn proxy_fetch_and_forward_inner(cfg: ProxyConfig<'_>) -> Result<Response, anyhow::Error> {
    // Validate URL scheme to prevent SSRF via non-HTTP schemes (e.g., file://)
    if !cfg.url.starts_with("http://") && !cfg.url.starts_with("https://") {
        return Err(
            ProxyError::InvalidRequest("only http and https are supported".to_string()).into(),
        );
    }

    let parsed_url = url::Url::parse(cfg.url)
        .map_err(|e| ProxyError::InvalidRequest(format!("invalid URL: {e}")))?;
    validate_target_url_against_ssrf(&parsed_url)?;

    let request = build_proxy_request(&cfg)?;
    let proxy_result = send_with_redirect_validation(cfg.client, request).await?;

    // Retry only on specific retryable 5xx server errors (500, 502, 503, 504).
    // We only retry once to avoid excessive latency for the client.
    // A delay is added before retry to avoid hammering struggling upstream servers.
    let (proxy_response, followed_redirects) =
        if is_retryable_status(proxy_result.response.status()) {
            let retry_delay = calculate_retry_delay();
            tracing::warn!(
                status = %proxy_result.response.status(),
                url = %cfg.url,
                retry_delay_ms = retry_delay.as_millis(),
                "Upstream returned retryable server error, retrying once after delay"
            );
            tokio::time::sleep(retry_delay).await;

            let retry_req = build_proxy_request(&cfg)?;
            let retry_result = send_with_redirect_validation(cfg.client, retry_req).await?;
            (retry_result.response, retry_result.followed_redirects)
        } else {
            (proxy_result.response, proxy_result.followed_redirects)
        };

    let status = proxy_response.status();
    let response_headers = proxy_response.headers().clone();

    // Check Content-Length hint before streaming (not authoritative, but catches obvious cases).
    // Use `try_from` instead of `as usize` to avoid silent truncation on 32-bit targets
    // where u64 > usize::MAX would wrap around and pass the size check.
    if let Some(cl) = proxy_response.content_length() {
        if usize::try_from(cl).map_or(true, |s| s > MAX_PROXY_BODY_SIZE) {
            return Err(ProxyError::Upstream(format!(
                "response too large ({cl} bytes, max {MAX_PROXY_BODY_SIZE})"
            ))
            .into());
        }
    }

    // Determine if reqwest auto-decompressed the response body.
    // reqwest transparently decodes gzip, deflate, and brotli by default.
    // If the upstream used one of these encodings, reqwest has already decoded
    // the body, so we must strip the content-encoding header (otherwise the
    // client would try to decompress already-decoded data).
    // For other encodings (e.g. zstd) that reqwest does NOT handle, we must
    // preserve the header so the client knows to decode it.
    // Additionally, when redirects were followed the body has been fully
    // consumed and re-requested at the final URL; in that case we strip
    // content-encoding unconditionally because the body is already decoded.
    // Use contains() to handle multiple encodings like "gzip, deflate" or "br, gzip".
    // This correctly handles cases where servers return multiple encodings.
    let reqwest_auto_decompressed = followed_redirects
        || response_headers
            .get("content-encoding")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ce| {
                let ce_lower = ce.to_lowercase();
                ce_lower.contains("gzip") || ce_lower.contains("deflate") || ce_lower.contains("br")
            });

    let mut builder = Response::builder().status(status);

    // For 206 Partial Content responses the client needs Content-Length to
    // determine the size of the range so that video players can seek correctly.
    let is_range_response = status == StatusCode::PARTIAL_CONTENT;

    for (name, value) in &response_headers {
        // Filter hop-by-hop headers per RFC 2616 Section 13.5.1.
        // Content-Length is normally stripped (axum sets it from the body),
        // but for range (206) responses we preserve it so players can seek.
        if name.as_str() == "content-length" && is_range_response {
            // Fall through to forward the header.
        } else if matches!(
            name.as_str(),
            "connection"
                | "transfer-encoding"
                | "content-length"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "upgrade"
        ) {
            continue;
        }
        // Only strip content-encoding if reqwest auto-decompressed the body
        if name.as_str() == "content-encoding" && reqwest_auto_decompressed {
            continue;
        }
        if let Ok(v) = value.to_str() {
            builder = builder.header(name.as_str(), v);
        }
    }

    // Set Cache-Control based on content type:
    // - Segment files (.m4s, .ts) are immutable and can be cached aggressively
    // - Manifests (.m3u8, .mpd) and unknown types must not be cached
    let cache_control = match response_headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
    {
        Some(ct)
            if ct.contains("video/") || ct.contains("audio/") || ct.contains("octet-stream") =>
        {
            "public, max-age=86400, immutable"
        }
        _ => "no-cache",
    };
    builder = builder.header("Cache-Control", cache_control);
    if cache_control == "no-cache" {
        builder = builder.header("Pragma", "no-cache");
    }
    builder = builder.header("X-Content-Type-Options", "nosniff");

    // Stream the body with cumulative size enforcement to prevent upstream servers
    // from sending unlimited data (e.g. with chunked transfer encoding or lying Content-Length).
    // Returns `None` after the first size-exceeded error to terminate the stream immediately.
    let body_stream =
        proxy_response
            .bytes_stream()
            .scan((0usize, false), |(total, exceeded), chunk| {
                if *exceeded {
                    return futures::future::ready(None);
                }
                match chunk {
                    Ok(data) => {
                        *total += data.len();
                        if *total > MAX_PROXY_BODY_SIZE {
                            *exceeded = true;
                            futures::future::ready(Some(Err(std::io::Error::other(
                                format!(
                                    "Response body exceeded size limit ({} bytes, max {MAX_PROXY_BODY_SIZE})",
                                    *total
                                ),
                            ))))
                        } else {
                            futures::future::ready(Some(Ok(data)))
                        }
                    }
                    Err(e) => futures::future::ready(Some(Err(std::io::Error::other(e)))),
                }
            });
    let body = Body::from_stream(body_stream);

    builder
        .body(body)
        .map_err(|e| anyhow::anyhow!("Failed to build response: {e}"))
}

/// Fetch a remote M3U8, rewrite its URLs so segments proxy through
/// `proxy_base`, and return the rewritten content.
pub async fn proxy_m3u8_and_rewrite<S: BuildHasher>(
    client: &reqwest::Client,
    url: &str,
    provider_headers: &HashMap<String, String, S>,
    proxy_base: &str,
) -> Result<Response, anyhow::Error> {
    let parsed = url::Url::parse(url).map_err(|e| anyhow::anyhow!("M3U8 URL is invalid: {e}"))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(anyhow::anyhow!("M3U8 URL has disallowed scheme: {scheme}"));
    }
    validate_target_url_against_ssrf(&parsed)
        .map_err(|e| anyhow::anyhow!("M3U8 SSRF validation failed: {e}"))?;

    let request = apply_provider_headers(client.get(url), url, provider_headers)?;

    let proxy_result = send_with_redirect_validation(client, request).await?;
    let proxy_response = proxy_result.response;

    if !proxy_response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Remote M3U8 returned status {}",
            proxy_response.status()
        ));
    }

    if let Some(cl) = proxy_response.content_length() {
        if usize::try_from(cl).map_or(true, |s| s > MAX_MANIFEST_SIZE) {
            return Err(anyhow::anyhow!(
                "M3U8 too large ({cl} bytes, max {MAX_MANIFEST_SIZE})"
            ));
        }
    }

    let m3u8_bytes = tokio::time::timeout(BODY_READ_TIMEOUT, proxy_response.bytes())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "M3U8 body read timed out after {}s",
                BODY_READ_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| anyhow::anyhow!("Failed to read M3U8 body: {e}"))?;

    if m3u8_bytes.len() > MAX_MANIFEST_SIZE {
        return Err(anyhow::anyhow!(
            "M3U8 too large ({} bytes, max {MAX_MANIFEST_SIZE})",
            m3u8_bytes.len()
        ));
    }

    let m3u8_text = String::from_utf8(m3u8_bytes.to_vec())
        .map_err(|e| anyhow::anyhow!("M3U8 response is not valid UTF-8: {e}"))?;

    let rewritten = rewrite_m3u8(&m3u8_text, url, proxy_base)?;

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/vnd.apple.mpegurl")
        .header("Cache-Control", "no-cache")
        .body(Body::from(rewritten))
        .map_err(|e| anyhow::anyhow!("Failed to build M3U8 response: {e}"))
}

// CORS preflight helper functions

/// Standard CORS headers for preflight requests.
const CORS_ALLOW_METHODS: &str = "GET, OPTIONS";
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
    // Handle wildcard mode
    if config.wildcard {
        return build_wildcard_cors_response();
    }

    // Check if origin is provided
    let Some(origin) = origin else {
        // No origin header - return minimal response without Allow-Origin header
        return build_no_origin_cors_response();
    };

    // Check if origin is allowed
    if !config.is_allowed(origin) {
        return build_forbidden_cors_response();
    }

    // Origin is allowed - return proper CORS headers
    build_allowed_cors_response(origin)
}

// CORS configuration

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
    config: std::sync::Arc<CorsConfig>,
) -> Response {
    handle_cors_preflight(origin, &config)
}

// M3U8 rewriting helpers

/// Default maximum number of URLs that can be rewritten in a single M3U8 playlist.
/// This prevents abuse via extremely large playlists that could cause memory
/// exhaustion or excessive proxy traffic.
pub const MAX_M3U8_URLS: usize = 1000;

/// Rewrite URLs inside an M3U8 playlist so they proxy through the server.
///
/// # Limits
/// - Maximum 1000 URLs per playlist by default (prevents abuse)
/// - Pass `max_urls` to override the default limit
///
/// # Security
/// - Returns an error if proxy_base contains line breaks (prevents response injection)
pub fn rewrite_m3u8(
    m3u8: &str,
    source_url: &str,
    proxy_base: &str,
) -> Result<String, anyhow::Error> {
    rewrite_m3u8_with_limit(m3u8, source_url, proxy_base, None)
}

/// Rewrite URLs inside an M3U8 playlist with a custom URL limit.
///
/// # Arguments
/// * `m3u8` - The M3U8 playlist content
/// * `source_url` - The original URL of the playlist (for resolving relative URLs)
/// * `proxy_base` - The base URL for proxying
/// * `max_urls` - Optional maximum number of URLs to rewrite (defaults to MAX_M3U8_URLS)
///
/// # Security
/// - Returns an error if proxy_base contains line breaks (prevents response injection)
pub fn rewrite_m3u8_with_limit(
    m3u8: &str,
    source_url: &str,
    proxy_base: &str,
    max_urls: Option<usize>,
) -> Result<String, anyhow::Error> {
    // Security: Reject proxy_base with line breaks to prevent response splitting/injection
    if proxy_base.contains('\n') || proxy_base.contains('\r') {
        return Err(anyhow::anyhow!(
            "proxy_base contains line break characters, refusing to rewrite M3U8"
        ));
    }

    let max_urls = max_urls.unwrap_or(MAX_M3U8_URLS);
    let base = url::Url::parse(source_url).ok();
    let mut output = String::with_capacity(m3u8.len());
    let mut url_count = 0usize;

    // Detect if this is a VOD playlist (has #EXT-X-ENDLIST) vs live stream.
    // For VOD, we can safely add #EXT-X-ENDLIST when truncating.
    // For live streams, adding #EXT-X-ENDLIST would incorrectly signal stream end.
    let is_vod = m3u8.contains("#EXT-X-ENDLIST");

    for line in m3u8.lines() {
        if line.starts_with('#') {
            let (rewritten_line, count) =
                rewrite_uri_attribute_with_count(line, base.as_ref(), proxy_base);
            url_count += count;
            output.push_str(&rewritten_line);
        } else {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                output.push_str(line);
            } else {
                url_count += 1;
                if url_count > max_urls {
                    tracing::warn!(
                        source_url = %source_url,
                        url_count = url_count,
                        max = max_urls,
                        is_vod = is_vod,
                        "M3U8 playlist exceeded maximum URL limit, truncating"
                    );
                    // Only add #EXT-X-ENDLIST for VOD playlists.
                    // For live streams, just truncate - clients will request more segments.
                    if is_vod {
                        output.push_str("#EXT-X-ENDLIST\n");
                    }
                    break;
                }
                let absolute = make_absolute(trimmed, base.as_ref());
                let separator = if proxy_base.contains('?') { '&' } else { '?' };
                let proxied = format!("{}{separator}url={}", proxy_base, percent_encode(&absolute));
                output.push_str(&proxied);
            }
        }
        output.push('\n');
    }

    if url_count > max_urls / 2 {
        tracing::info!(
            source_url = %source_url,
            url_count = url_count,
            "M3U8 playlist has many URLs"
        );
    }

    Ok(output)
}

/// Resolve a possibly-relative URL to absolute using the given base URL.
#[must_use]
pub fn make_absolute(raw: &str, base: Option<&url::Url>) -> String {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return raw.to_string();
    }
    if let Some(base) = base {
        if let Ok(joined) = base.join(raw) {
            return joined.to_string();
        }
    }
    raw.to_string()
}

/// Rewrite any `URI="..."` values found in an M3U8 tag line.
/// Returns the rewritten line and the count of URLs rewritten.
#[must_use]
pub fn rewrite_uri_attribute_with_count(
    line: &str,
    base: Option<&url::Url>,
    proxy_base: &str,
) -> (String, usize) {
    let pattern = "URI=\"";
    let mut result = String::with_capacity(line.len());
    let mut remaining = line;
    let mut count = 0usize;

    while let Some(start) = remaining.find(pattern) {
        result.push_str(&remaining[..start + pattern.len()]);
        remaining = &remaining[start + pattern.len()..];

        if let Some(end) = remaining.find('"') {
            let uri = &remaining[..end];
            let absolute = make_absolute(uri, base);
            let separator = if proxy_base.contains('?') { '&' } else { '?' };
            let proxied = format!("{}{separator}url={}", proxy_base, percent_encode(&absolute));
            result.push_str(&proxied);
            result.push('"');
            remaining = &remaining[end + 1..];
            count += 1;
        } else {
            result.push_str(remaining);
            remaining = "";
        }
    }

    result.push_str(remaining);
    (result, count)
}

/// Percent-encode a string for use in URL query parameter values.
///
/// This function first decodes any existing percent-encoded sequences, then
/// re-encodes the result. This prevents double-encoding bugs where `%20`
/// would become `%2520`.
///
/// Uses the `NON_ALPHANUMERIC` encode set, which encodes everything except
/// `A-Z a-z 0-9` (the RFC 3986 "unreserved" alphanumeric characters).
/// Note: Unlike strict RFC 3986, this also encodes `-`, `_`, `.`, and `~`
/// to ensure consistent encoding for query parameter values.
#[must_use]
pub fn percent_encode(input: &str) -> String {
    // First, decode any existing percent-encoded sequences to get raw bytes.
    // This normalizes the input so we don't double-encode.
    let decoded = percent_encoding::percent_decode_str(input).collect::<Vec<u8>>();
    // Then, re-encode using NON_ALPHANUMERIC for safe use in query params.
    percent_encoding::percent_encode(&decoded, percent_encoding::NON_ALPHANUMERIC).to_string()
}

// Manual redirect following with DNS validation

/// Headers that should be preserved across redirect hops.
///
/// Provider headers (Referer, User-Agent) and client passthrough headers
/// (Range, Accept) are re-applied on each redirect to avoid breaking
/// providers that require them on the final CDN request.
const REDIRECT_PRESERVE_HEADERS: &[&str] = &[
    "referer",
    "user-agent",
    "range",
    "accept",
    "accept-language",
    "if-none-match",
    "if-modified-since",
];

/// Headers to drop when a redirect crosses origin boundaries.
///
/// The `Referer` header can leak the original request URL (including signed
/// query parameters) to a third-party host. Dropping it on cross-origin
/// redirects follows browser `strict-origin-when-cross-origin` behaviour.
const CROSS_ORIGIN_DROP_HEADERS: &[&str] = &["referer"];

/// Result of `send_with_redirect_validation`.
struct ProxyResponse {
    /// The final HTTP response after following any redirects.
    response: reqwest::Response,
    /// `true` if at least one redirect was followed.
    ///
    /// When redirects occurred the response body has been fully consumed and
    /// re-requested at the final URL, so `Content-Encoding` must be stripped
    /// from the forwarded headers regardless of the encoding value (the body
    /// is already decoded by reqwest).
    followed_redirects: bool,
}

pub(crate) async fn send_head_with_redirect_validation(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
) -> Result<ProxyResponse, anyhow::Error> {
    send_with_redirect_validation_inner(client, request, reqwest::Method::HEAD).await
}

/// Send a request via the proxy client, manually following redirects with
/// full async DNS validation on every hop.
///
/// Automatic redirects are disabled on the injected proxy client, so 3xx responses
/// are handled here. Each redirect target gets both static URL validation
/// and async DNS resolution checks to prevent DNS-rebinding SSRF.
///
/// Headers matching [`REDIRECT_PRESERVE_HEADERS`] are captured from the
/// initial request and re-applied on every redirect hop so that provider
/// and client headers are not lost.
async fn send_with_redirect_validation(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
) -> Result<ProxyResponse, anyhow::Error> {
    send_with_redirect_validation_inner(client, request, reqwest::Method::GET).await
}

async fn send_with_redirect_validation_inner(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    redirect_method: reqwest::Method,
) -> Result<ProxyResponse, anyhow::Error> {
    // Build the request to capture headers before sending.
    let built = request
        .build()
        .map_err(|e| ProxyError::InvalidRequest(format!("failed to build proxy request: {e}")))?;
    validate_target_url_against_ssrf(built.url())?;

    // Capture the original request's origin for cross-origin detection.
    let original_origin = built.url().origin().ascii_serialization();

    // Snapshot headers to preserve across redirects.
    let preserved: Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)> = built
        .headers()
        .iter()
        .filter(|(name, _)| REDIRECT_PRESERVE_HEADERS.contains(&name.as_str()))
        .map(|(n, v)| (n.clone(), v.clone()))
        .collect();

    let mut response = client
        .execute(built)
        .await
        .map_err(|error| classify_reqwest_error(&error))?;

    let mut hops = 0usize;
    while response.status().is_redirection()
        && response.status() != reqwest::StatusCode::NOT_MODIFIED
    {
        hops += 1;
        if hops > MAX_REDIRECTS {
            return Err(
                ProxyError::Upstream(format!("too many redirects ({MAX_REDIRECTS} max)")).into(),
            );
        }

        let current_url = response.url().clone();
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .ok_or_else(|| ProxyError::Upstream("redirect without Location header".to_string()))?
            .to_str()
            .map_err(|_| ProxyError::Upstream("invalid Location header".to_string()))?
            .to_string();

        let location = current_url.join(&location).map_err(|e| {
            ProxyError::Upstream(format!("invalid redirect target `{location}`: {e}"))
        })?;

        // Validate redirect URL scheme to prevent protocol downgrade attacks
        // (e.g. redirecting to file://, ftp://, data://, etc.)
        let scheme = location.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(
                ProxyError::Ssrf(format!("redirect to disallowed scheme: {scheme}")).into(),
            );
        }

        // Determine if this redirect crosses origin boundaries.
        let is_cross_origin = location.origin().ascii_serialization() != original_origin;
        if is_cross_origin {
            validate_target_url_against_ssrf(&location)?;
        }

        // SSRF protection is handled by the DNS resolver at connection time
        let mut redirect_req = client.request(redirect_method.clone(), location.clone());
        for (name, value) in &preserved {
            // Drop sensitive headers (e.g. Referer) on cross-origin redirects
            // to avoid leaking signed URLs to third-party hosts.
            if is_cross_origin && CROSS_ORIGIN_DROP_HEADERS.contains(&name.as_str()) {
                continue;
            }
            redirect_req = redirect_req.header(name.clone(), value.clone());
        }

        drop(response);
        response = redirect_req
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?;
    }

    Ok(ProxyResponse {
        response,
        followed_redirects: hops > 0,
    })
}

fn classify_reqwest_error(error: &reqwest::Error) -> anyhow::Error {
    let message = error.to_string();
    let proxy_error = if error.is_timeout() {
        ProxyError::Timeout(message)
    } else if error.is_connect() {
        ProxyError::Connection(message)
    } else {
        let lower = message.to_ascii_lowercase();
        if lower.contains("private")
            || lower.contains("loopback")
            || lower.contains("disallowed")
            || lower.contains("blocked")
        {
            ProxyError::Ssrf(message)
        } else {
            ProxyError::Other(message)
        }
    };
    proxy_error.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    fn test_proxy_client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("test proxy client should build")
    }

    // CORS preflight helper function tests

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
            Some("GET, OPTIONS")
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
        // Wildcard response should NOT have Allow-Credentials or Vary
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
        // No origin header when origin is not provided
        assert!(response
            .headers()
            .get("Access-Control-Allow-Origin")
            .is_none());
        assert_eq!(
            response
                .headers()
                .get("Access-Control-Allow-Methods")
                .map(|v| v.to_str().unwrap()),
            Some("GET, OPTIONS")
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
        let config = CorsConfig::new(vec![]); // Empty allowed list
        let response = handle_cors_preflight(Some("https://example.com"), &config);

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    // P-06: Redirect header preservation list

    #[test]
    fn test_redirect_preserve_headers_includes_critical_headers() {
        // Verify that provider-critical and client-critical headers are preserved
        assert!(
            REDIRECT_PRESERVE_HEADERS.contains(&"referer"),
            "Referer must be preserved across redirects for provider auth"
        );
        assert!(
            REDIRECT_PRESERVE_HEADERS.contains(&"user-agent"),
            "User-Agent must be preserved across redirects for provider auth"
        );
        assert!(
            REDIRECT_PRESERVE_HEADERS.contains(&"range"),
            "Range must be preserved across redirects for partial content"
        );
    }

    // Existing helpers

    #[test]
    fn test_make_absolute_already_absolute() {
        assert_eq!(
            make_absolute("https://cdn.example.com/seg1.ts", None),
            "https://cdn.example.com/seg1.ts"
        );
    }

    #[test]
    fn test_make_absolute_relative() {
        let base = url::Url::parse("https://cdn.example.com/path/master.m3u8").unwrap();
        assert_eq!(
            make_absolute("seg1.ts", Some(&base)),
            "https://cdn.example.com/path/seg1.ts"
        );
    }

    #[test]
    fn test_rewrite_m3u8_basic() {
        let m3u8 = "#EXTM3U\n#EXT-X-VERSION:3\nseg1.ts\nseg2.ts\n";
        let rewritten = rewrite_m3u8(
            m3u8,
            "https://cdn.example.com/path/master.m3u8",
            "/proxy/stream",
        )
        .unwrap();
        assert!(rewritten.contains("/proxy/stream?url="));
        assert!(rewritten.contains("cdn%2Eexample%2Ecom"));
    }

    #[test]
    fn test_rewrite_m3u8_rejects_newline_in_proxy_base() {
        let m3u8 = "#EXTM3U\n#EXT-X-VERSION:3\nseg1.ts\n";
        // proxy_base with LF should return an error
        let result = rewrite_m3u8(
            m3u8,
            "https://cdn.example.com/path/master.m3u8",
            "/proxy/stream\nSet-Cookie: malicious=value",
        );
        assert!(result.is_err());

        // proxy_base with CR should also return an error
        let result = rewrite_m3u8(
            m3u8,
            "https://cdn.example.com/path/master.m3u8",
            "/proxy/stream\rSet-Cookie: malicious=value",
        );
        assert!(result.is_err());

        // proxy_base with CRLF should also return an error
        let result = rewrite_m3u8(
            m3u8,
            "https://cdn.example.com/path/master.m3u8",
            "/proxy/stream\r\nSet-Cookie: malicious=value",
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_ssrf_acl_blocks_private_ips() {
        use std::net::IpAddr;
        let blocked: &[&str] = &["127.0.0.1", "192.168.1.1", "10.0.0.1"];
        for ip_str in blocked {
            let ip: IpAddr = ip_str.parse().unwrap();
            assert!(
                synctv_common::ssrf::SsrfGuard::shared_default().is_ip_blocked(&ip),
                "IP {ip} should be blocked"
            );
        }
    }

    #[test]
    fn test_ssrf_acl_allows_public_ips() {
        use std::net::IpAddr;
        let allowed: &[&str] = &["1.1.1.1", "8.8.8.8"];
        for ip_str in allowed {
            let ip: IpAddr = ip_str.parse().unwrap();
            assert!(
                !synctv_common::ssrf::SsrfGuard::shared_default().is_ip_blocked(&ip),
                "IP {ip} should be allowed"
            );
        }
    }

    // URL scheme validation tests

    #[tokio::test]
    async fn test_proxy_fetch_rejects_file_scheme() {
        let provider_headers = HashMap::new();
        let client_headers = HeaderMap::new();
        let client = test_proxy_client();
        let cfg = ProxyConfig {
            client: &client,
            url: "file:///etc/passwd",
            provider_headers: &provider_headers,
            client_headers: &client_headers,
        };

        let result = proxy_fetch_and_forward_inner(cfg).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("only http and https are supported"),
            "Expected invalid-request scheme rejection, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_proxy_fetch_rejects_ftp_scheme() {
        let provider_headers = HashMap::new();
        let client_headers = HeaderMap::new();
        let client = test_proxy_client();
        let cfg = ProxyConfig {
            client: &client,
            url: "ftp://example.com/file.txt",
            provider_headers: &provider_headers,
            client_headers: &client_headers,
        };

        let result = proxy_fetch_and_forward_inner(cfg).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("only http and https are supported"),
            "Expected invalid-request scheme rejection, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_proxy_fetch_rejects_javascript_scheme() {
        let provider_headers = HashMap::new();
        let client_headers = HeaderMap::new();
        let client = test_proxy_client();
        let cfg = ProxyConfig {
            client: &client,
            url: "javascript:alert(1)",
            provider_headers: &provider_headers,
            client_headers: &client_headers,
        };

        let result = proxy_fetch_and_forward_inner(cfg).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("only http and https are supported"),
            "Expected invalid-request scheme rejection, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_proxy_fetch_rejects_data_scheme() {
        let provider_headers = HashMap::new();
        let client_headers = HeaderMap::new();
        let client = test_proxy_client();
        let cfg = ProxyConfig {
            client: &client,
            url: "data:text/plain,hello",
            provider_headers: &provider_headers,
            client_headers: &client_headers,
        };

        let result = proxy_fetch_and_forward_inner(cfg).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("only http and https are supported"),
            "Expected invalid-request scheme rejection, got: {err}"
        );
    }

    #[test]
    fn test_proxy_error_kind_mapping() {
        assert_eq!(
            ProxyError::Timeout("x".into()).kind(),
            ProxyErrorKind::Timeout
        );
        assert_eq!(
            ProxyError::Connection("x".into()).kind(),
            ProxyErrorKind::Connection
        );
        assert_eq!(ProxyError::Ssrf("x".into()).kind(), ProxyErrorKind::Ssrf);
        assert_eq!(
            ProxyError::InvalidRequest("x".into()).kind(),
            ProxyErrorKind::InvalidRequest
        );
        assert_eq!(
            ProxyError::Upstream("x".into()).kind(),
            ProxyErrorKind::Upstream
        );
        assert_eq!(ProxyError::Other("x".into()).kind(), ProxyErrorKind::Other);
    }

    #[tokio::test]
    async fn test_send_with_redirect_validation_resolves_relative_location() {
        let server = wiremock::MockServer::start().await;
        let public_origin = format!("http://cdn.example.com:{}", server.address().port());

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/start"))
            .respond_with(wiremock::ResponseTemplate::new(302).insert_header("location", "/final"))
            .mount(&server)
            .await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/final"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("cdn.example.com", *server.address())
            .build()
            .expect("client should build");
        let request = client.get(format!("{public_origin}/start"));

        let result = send_with_redirect_validation(&client, request).await;
        assert!(
            result.is_ok(),
            "relative redirects should resolve against original URL"
        );

        let proxy_response = result.expect("redirect should succeed");
        assert_eq!(proxy_response.response.status(), reqwest::StatusCode::OK);
        let body = proxy_response
            .response
            .bytes()
            .await
            .expect("body should be readable");
        assert_eq!(body.as_ref(), b"ok");
        assert!(proxy_response.followed_redirects);
    }

    #[tokio::test]
    async fn test_send_with_redirect_validation_rejects_redirect_to_blocked_ip() {
        let server = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/start"))
            .respond_with(
                wiremock::ResponseTemplate::new(302)
                    .insert_header("location", "http://127.0.0.1:12345/private"),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client should build");
        let request = client.get(format!("{}/start", server.uri()));

        let result = send_with_redirect_validation(&client, request).await;
        let Err(err) = result else {
            panic!("redirect to blocked loopback must fail");
        };
        let proxy_err = err
            .downcast_ref::<ProxyError>()
            .expect("error should downcast to ProxyError");
        assert!(matches!(proxy_err, ProxyError::Ssrf(_)));
        assert!(
            proxy_err.to_string().contains("blocked by SSRF policy"),
            "unexpected error: {proxy_err}"
        );
    }

    #[tokio::test]
    async fn test_send_with_redirect_validation_rejects_initial_blocked_ip() {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client should build");
        let request = client.get("http://127.0.0.1:12345/private");

        let Err(err) = send_with_redirect_validation(&client, request).await else {
            panic!("initial loopback target must fail before network IO");
        };
        let proxy_err = err
            .downcast_ref::<ProxyError>()
            .expect("error should downcast to ProxyError");
        assert!(matches!(proxy_err, ProxyError::Ssrf(_)));
        assert!(
            proxy_err.to_string().contains("blocked by SSRF policy"),
            "unexpected error: {proxy_err}"
        );
    }

    #[tokio::test]
    async fn test_proxy_m3u8_and_rewrite_rejects_initial_blocked_ip_before_io() {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client should build");

        let err = proxy_m3u8_and_rewrite(
            &client,
            "http://127.0.0.1:12345/private.m3u8",
            &HashMap::new(),
            "/proxy",
        )
        .await
        .expect_err("loopback manifest must fail before network IO");

        assert!(
            err.to_string().contains("blocked by SSRF policy"),
            "unexpected error: {err}"
        );
    }
}
