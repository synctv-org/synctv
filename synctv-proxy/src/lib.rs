#![cfg_attr(test, allow(clippy::unwrap_used))]

//! Shared media proxy utilities
//!
//! Provides reusable functions for proxying media streams and rewriting M3U8
//! playlists.  Used by per-provider proxy routes in `synctv-api`.

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

mod cors;
mod error;
pub mod grpc;
mod manifest;
mod redirect;
pub mod slice_cache;

use std::collections::HashMap;
use std::future::Future;
use std::hash::BuildHasher;
use std::time::Duration;

use axum::{body::Body, http::StatusCode, response::Response};
use bytes::Bytes;
use futures::StreamExt;
use reqwest::header::{HeaderName, HeaderValue, REFERER, USER_AGENT};
use synctv_common::ExecutionControl;

pub use cors::{proxy_options_preflight_with_cors, CorsConfig};
pub(crate) use error::{classify_reqwest_body_error, ProxyError};
pub use error::{
    proxy_error_kind, proxy_error_kind_from_std_error, proxy_range_not_satisfiable_total_size,
    ProxyErrorKind,
};
pub use manifest::{
    default_proxy_url, make_absolute, percent_encode, rewrite_m3u8, rewrite_m3u8_with_limit,
    rewrite_m3u8_with_url_mapper, rewrite_uri_attribute_with_count, MAX_M3U8_URLS,
};
#[cfg(test)]
pub(crate) use redirect::REDIRECT_PRESERVE_HEADERS;
pub(crate) use redirect::{
    send_head_with_redirect_validation_with_control, send_with_redirect_validation,
    send_with_redirect_validation_with_control,
    send_with_redirect_validation_with_control_and_timeout, validate_target_url_against_ssrf,
};

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

/// Minimum delay before retrying a failed request (100ms).
const RETRY_DELAY_MIN_MS: u64 = 100;

/// Maximum delay before retrying a failed request (500ms).
const RETRY_DELAY_MAX_MS: u64 = 500;

/// HTTP status codes that are retryable (transient server errors).
/// These indicate temporary issues that may resolve on retry.
const RETRYABLE_STATUS_CODES: &[u16] = &[500, 502, 503, 504];

/// Build a proxy HTTP client for outbound media requests.
///
/// Callers are expected to build this once during startup and inject it into
/// the proxy/cache layers rather than relying on hidden process-global state.
pub fn build_proxy_http_client(
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
) -> Result<reqwest::Client, anyhow::Error> {
    synctv_common::http::SsrfSafeClientBuilder::new()
        .ssrf_guard(ssrf_guard)
        .connect_timeout(Duration::from_secs(10))
        .disable_request_timeout()
        .disable_read_timeout()
        .pool_max_idle_per_host(100)
        .pool_idle_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build proxy HTTP client: {e}"))
}

/// Configuration for a single proxy fetch.
pub struct ProxyConfig<'a> {
    /// SSRF policy used for static URL checks and redirect validation.
    pub ssrf_guard: &'a synctv_common::ssrf::SsrfGuard,
    /// Shared outbound HTTP client.
    pub client: &'a reqwest::Client,
    /// The remote URL to fetch.
    pub url: &'a str,
    /// Extra headers the provider requires (e.g. Referer, cookies).
    pub provider_headers: &'a HashMap<String, String>,
    /// Provider-selected Range header for this fetch.
    pub range_header: Option<&'a str>,
    /// Cooperative execution control propagated by the caller.
    ///
    /// Proxy flows only consume the cancellation signal from this control.
    /// They must not inherit an end-to-end deadline for the entire proxy
    /// lifecycle because upstream body size and transfer rate are unbounded.
    pub request_control: Option<&'a ExecutionControl>,
    /// Optional timeout for a single upstream HTTP hop.
    ///
    /// This timeout applies only to "send request + receive response headers".
    /// Once headers are received, body reading/streaming is cancellation-only.
    ///
    /// Slice-cache flows may perform multiple upstream requests; each request
    /// gets an independent timeout from this field and there is no shared
    /// whole-lifecycle proxy deadline.
    pub upstream_header_timeout: Option<Duration>,
}

pub(crate) async fn run_with_proxy_cancellation<T, F>(
    context: &str,
    request_control: Option<&ExecutionControl>,
    future: F,
) -> Result<T, anyhow::Error>
where
    F: Future<Output = T>,
{
    match request_control {
        Some(request_control) => request_control
            .run_cancellable_only(future)
            .await
            .map_err(|_| ProxyError::Cancelled(context.to_string()).into()),
        None => Ok(future.await),
    }
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

fn proxy_body_stream<S>(
    stream: S,
    max_body_size: usize,
) -> impl futures::Stream<Item = Result<Bytes, ProxyError>>
where
    S: futures::Stream<Item = Result<Bytes, reqwest::Error>>,
{
    stream.scan((0usize, false), move |(total, exceeded), chunk| {
        if *exceeded {
            return futures::future::ready(None);
        }
        match chunk {
            Ok(data) => {
                *total += data.len();
                if *total > max_body_size {
                    *exceeded = true;
                    futures::future::ready(Some(Err(ProxyError::BodyTooLarge(format!(
                        "response exceeded size limit ({} bytes, max {max_body_size})",
                        *total
                    )))))
                } else {
                    futures::future::ready(Some(Ok(data)))
                }
            }
            Err(e) => futures::future::ready(Some(Err(classify_reqwest_body_error(&e)))),
        }
    })
}

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

/// Forward a client HEAD request to the upstream origin as HEAD.
///
/// This avoids Axum's implicit GET-to-HEAD fallback from triggering an
/// upstream body fetch or slice-cache fill for metadata-only client requests.
pub async fn proxy_head_and_forward(cfg: ProxyConfig<'_>) -> Result<Response, anyhow::Error> {
    if !cfg.url.starts_with("http://") && !cfg.url.starts_with("https://") {
        return Err(
            ProxyError::InvalidRequest("only http and https are supported".to_string()).into(),
        );
    }

    let parsed_url = url::Url::parse(cfg.url)
        .map_err(|e| ProxyError::InvalidRequest(format!("invalid URL: {e}")))?;
    validate_target_url_against_ssrf(&parsed_url, cfg.ssrf_guard)?;

    let request = build_proxy_request_with_method(&cfg, reqwest::Method::HEAD)?;
    let proxy_result = send_head_with_redirect_validation_with_control(
        cfg.client,
        request,
        cfg.ssrf_guard,
        cfg.request_control,
    )
    .await?;

    build_head_response(&proxy_result.response)
}

/// Build a proxy request for the given URL, applying provider-specific headers.
///
/// This is the single point of request construction used by both the initial
/// fetch and retry attempts.
fn build_proxy_request_with_method(
    cfg: &ProxyConfig<'_>,
    method: reqwest::Method,
) -> Result<reqwest::RequestBuilder, anyhow::Error> {
    let request = cfg.client.request(method, cfg.url);
    let mut request = apply_provider_headers(request, cfg.url, cfg.provider_headers)?;
    if let Some(range) = cfg.range_header {
        request = request.header(reqwest::header::RANGE, range);
    }
    Ok(request)
}

fn build_proxy_request(cfg: &ProxyConfig<'_>) -> Result<reqwest::RequestBuilder, anyhow::Error> {
    build_proxy_request_with_method(cfg, reqwest::Method::GET)
}

fn build_head_response(proxy_response: &reqwest::Response) -> Result<Response, anyhow::Error> {
    let status = proxy_response.status();
    let response_headers = proxy_response.headers().clone();

    let mut builder = Response::builder().status(status);
    for (name, value) in &response_headers {
        if matches!(
            name.as_str(),
            "connection"
                | "transfer-encoding"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "upgrade"
        ) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            builder = builder.header(name.as_str(), v);
        }
    }

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

    builder
        .body(Body::empty())
        .map_err(|e| anyhow::anyhow!("Failed to build HEAD response: {e}"))
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
    validate_target_url_against_ssrf(&parsed_url, cfg.ssrf_guard)?;

    let request = build_proxy_request(&cfg)?;
    let proxy_result = send_with_redirect_validation_with_control_and_timeout(
        cfg.client,
        request,
        cfg.ssrf_guard,
        cfg.request_control,
        cfg.upstream_header_timeout,
    )
    .await?;

    // Retry only on specific retryable 5xx server errors (500, 502, 503, 504).
    // We only retry once to avoid excessive latency for the client.
    // A delay is added before retry to avoid hammering struggling upstream servers.
    let (proxy_response, _followed_redirects) =
        if is_retryable_status(proxy_result.response.status()) {
            let retry_delay = calculate_retry_delay();
            tracing::warn!(
                status = %proxy_result.response.status(),
                url = %cfg.url,
                retry_delay_ms = retry_delay.as_millis(),
                "Upstream returned retryable server error, retrying once after delay"
            );
            run_with_proxy_cancellation("proxy retry delay", cfg.request_control, async move {
                tokio::time::sleep(retry_delay).await;
            })
            .await?;

            let retry_req = build_proxy_request(&cfg)?;
            let retry_result = send_with_redirect_validation_with_control_and_timeout(
                cfg.client,
                retry_req,
                cfg.ssrf_guard,
                cfg.request_control,
                cfg.upstream_header_timeout,
            )
            .await?;
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
            return Err(ProxyError::BodyTooLarge(format!(
                "response too large ({cl} bytes, max {MAX_PROXY_BODY_SIZE})"
            ))
            .into());
        }
    }

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

    // After headers arrive the body is intentionally cancellation-only. We do
    // not apply a timeout to the remainder of the proxy lifecycle because
    // upstream media responses can be arbitrarily large or slow by design.
    let body_stream = proxy_body_stream(proxy_response.bytes_stream(), MAX_PROXY_BODY_SIZE);
    let body = Body::from_stream(body_stream);

    builder
        .body(body)
        .map_err(|e| anyhow::anyhow!("Failed to build response: {e}"))
}

/// Fetch a remote M3U8, rewrite its URLs so segments proxy through
/// `proxy_base`, and return the rewritten content.
pub async fn proxy_m3u8_and_rewrite<S: BuildHasher>(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &HashMap<String, String, S>,
    proxy_base: &str,
) -> Result<Response, anyhow::Error> {
    proxy_m3u8_and_rewrite_with_control(client, ssrf_guard, url, provider_headers, proxy_base, None)
        .await
}

pub async fn proxy_m3u8_and_rewrite_with_control<S: BuildHasher>(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &HashMap<String, String, S>,
    proxy_base: &str,
    request_control: Option<&ExecutionControl>,
) -> Result<Response, anyhow::Error> {
    proxy_m3u8_and_rewrite_with_control_and_mapper(
        client,
        ssrf_guard,
        url,
        provider_headers,
        proxy_base,
        request_control,
        manifest::default_proxy_url,
    )
    .await
}

pub async fn proxy_m3u8_and_rewrite_with_control_and_mapper<S, F>(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &HashMap<String, String, S>,
    proxy_base: &str,
    request_control: Option<&ExecutionControl>,
    proxy_url_for_target: F,
) -> Result<Response, anyhow::Error>
where
    S: BuildHasher,
    F: Fn(&str, &str) -> String,
{
    let parsed = url::Url::parse(url).map_err(|e| anyhow::anyhow!("M3U8 URL is invalid: {e}"))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(anyhow::anyhow!("M3U8 URL has disallowed scheme: {scheme}"));
    }
    validate_target_url_against_ssrf(&parsed, ssrf_guard)
        .map_err(|e| anyhow::anyhow!("M3U8 SSRF validation failed: {e}"))?;

    let request = apply_provider_headers(client.get(url), url, provider_headers)?;

    let proxy_result =
        send_with_redirect_validation_with_control(client, request, ssrf_guard, request_control)
            .await?;
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

    let m3u8_bytes = run_with_proxy_cancellation(
        "manifest proxy body read",
        request_control,
        proxy_response.bytes(),
    )
    .await?
    .map_err(|e| anyhow::anyhow!("Failed to read M3U8 body: {e}"))?;

    if m3u8_bytes.len() > MAX_MANIFEST_SIZE {
        return Err(anyhow::anyhow!(
            "M3U8 too large ({} bytes, max {MAX_MANIFEST_SIZE})",
            m3u8_bytes.len()
        ));
    }

    let m3u8_text = String::from_utf8(m3u8_bytes.to_vec())
        .map_err(|e| anyhow::anyhow!("M3U8 response is not valid UTF-8: {e}"))?;

    let rewritten =
        manifest::rewrite_m3u8_with_url_mapper(&m3u8_text, url, proxy_base, proxy_url_for_target)?;

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/vnd.apple.mpegurl")
        .header("Cache-Control", "no-cache")
        .body(Body::from(rewritten))
        .map_err(|e| anyhow::anyhow!("Failed to build M3U8 response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use http_body_util::BodyExt;

    fn test_proxy_client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("test proxy client should build")
    }

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
                synctv_common::ssrf::SsrfGuard::strict_policy().is_ip_blocked(&ip),
                "strict SSRF policy should block {ip}"
            );
        }
    }

    #[test]
    fn test_ssrf_acl_allows_public_ips() {
        use std::net::IpAddr;
        let guard = synctv_common::ssrf::SsrfGuard::strict_policy();
        let allowed: &[&str] = &["1.1.1.1", "8.8.8.8"];
        for ip_str in allowed {
            let ip: IpAddr = ip_str.parse().unwrap();
            assert!(!guard.is_ip_blocked(&ip), "IP {ip} should be allowed");
        }
    }

    // URL scheme validation tests

    #[tokio::test]
    async fn test_proxy_fetch_rejects_file_scheme() {
        let provider_headers = HashMap::new();
        let client = test_proxy_client();
        let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
        let cfg = ProxyConfig {
            ssrf_guard: &ssrf_guard,
            client: &client,
            url: "file:///etc/passwd",
            provider_headers: &provider_headers,
            range_header: None,
            request_control: None,
            upstream_header_timeout: None,
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
        let client = test_proxy_client();
        let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
        let cfg = ProxyConfig {
            ssrf_guard: &ssrf_guard,
            client: &client,
            url: "ftp://example.com/file.txt",
            provider_headers: &provider_headers,
            range_header: None,
            request_control: None,
            upstream_header_timeout: None,
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
        let client = test_proxy_client();
        let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
        let cfg = ProxyConfig {
            ssrf_guard: &ssrf_guard,
            client: &client,
            url: "javascript:alert(1)",
            provider_headers: &provider_headers,
            range_header: None,
            request_control: None,
            upstream_header_timeout: None,
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
        let client = test_proxy_client();
        let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
        let cfg = ProxyConfig {
            ssrf_guard: &ssrf_guard,
            client: &client,
            url: "data:text/plain,hello",
            provider_headers: &provider_headers,
            range_header: None,
            request_control: None,
            upstream_header_timeout: None,
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
    async fn test_proxy_fetch_ignores_outer_deadline_for_body_lifetime() {
        let server = wiremock::MockServer::start().await;
        let public_origin = format!("http://cdn.example.com:{}", server.address().port());

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/slow.mp4"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(200))
                    .set_body_string("slow body"),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("cdn.example.com", *server.address())
            .build()
            .expect("client should build");
        let provider_headers = HashMap::new();
        let request_control = ExecutionControl::from_timeout(Some(Duration::from_millis(50)));
        let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
        let cfg = ProxyConfig {
            ssrf_guard: &ssrf_guard,
            client: &client,
            url: &format!("{public_origin}/slow.mp4"),
            provider_headers: &provider_headers,
            range_header: None,
            request_control: Some(&request_control),
            upstream_header_timeout: None,
        };

        let response = proxy_fetch_and_forward(cfg, &NoopMetrics)
            .await
            .expect("proxy fetch should not inherit the outer request deadline");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_proxy_fetch_preserves_content_encoding_for_byte_transparent_body() {
        let server = wiremock::MockServer::start().await;
        let public_origin = format!("http://cdn.example.com:{}", server.address().port());

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/encoded.bin"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_bytes(b"gzip-bytes".to_vec())
                    .insert_header("content-encoding", "gzip")
                    .insert_header("content-type", "application/octet-stream"),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("cdn.example.com", *server.address())
            .build()
            .expect("client should build");
        let provider_headers = HashMap::new();
        let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
        let cfg = ProxyConfig {
            ssrf_guard: &ssrf_guard,
            client: &client,
            url: &format!("{public_origin}/encoded.bin"),
            provider_headers: &provider_headers,
            range_header: None,
            request_control: None,
            upstream_header_timeout: None,
        };

        let response = proxy_fetch_and_forward(cfg, &NoopMetrics)
            .await
            .expect("proxy fetch should succeed");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(reqwest::header::CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("gzip")
        );
    }

    #[test]
    fn test_proxy_error_kind_mapping() {
        assert_eq!(
            ProxyError::Cancelled("x".into()).kind(),
            ProxyErrorKind::Cancelled
        );
        assert_eq!(
            ProxyError::Timeout("x".into()).kind(),
            ProxyErrorKind::Timeout
        );
        assert_eq!(
            ProxyError::Connection("x".into()).kind(),
            ProxyErrorKind::Connection
        );
        assert_eq!(
            ProxyError::BodyTooLarge("x".into()).kind(),
            ProxyErrorKind::BodyTooLarge
        );
        assert_eq!(ProxyError::Ssrf("x".into()).kind(), ProxyErrorKind::Ssrf);
        assert_eq!(
            ProxyError::InvalidRequest("x".into()).kind(),
            ProxyErrorKind::InvalidRequest
        );
        assert_eq!(
            ProxyError::RangeNotSatisfiable {
                message: "x".into(),
                total_size: 42,
            }
            .kind(),
            ProxyErrorKind::RangeNotSatisfiable
        );
        assert_eq!(
            ProxyError::Upstream("x".into()).kind(),
            ProxyErrorKind::Upstream
        );
        assert_eq!(ProxyError::Other("x".into()).kind(), ProxyErrorKind::Other);
    }

    #[test]
    fn test_proxy_error_kind_from_error_chain() {
        let err = anyhow::Error::from(ProxyError::BodyTooLarge(
            "stream exceeded limit".to_string(),
        ))
        .context("outer context");

        assert_eq!(proxy_error_kind(&err), Some(ProxyErrorKind::BodyTooLarge));
    }

    #[test]
    fn test_proxy_range_not_satisfiable_total_size_from_error_chain() {
        let err = anyhow::Error::from(ProxyError::RangeNotSatisfiable {
            message: "range start beyond total size".to_string(),
            total_size: 4096,
        })
        .context("outer context");

        assert_eq!(
            proxy_error_kind(&err),
            Some(ProxyErrorKind::RangeNotSatisfiable)
        );
        assert_eq!(proxy_range_not_satisfiable_total_size(&err), Some(4096));
    }

    #[tokio::test]
    async fn test_proxy_body_stream_preserves_typed_oversize_error() {
        let stream = futures::stream::iter([
            Ok(Bytes::from_static(b"1234")),
            Ok(Bytes::from_static(b"567")),
        ]);
        let body = Body::from_stream(proxy_body_stream(stream, 6));
        let err = body
            .collect()
            .await
            .expect_err("oversized streaming body should fail");

        assert_eq!(
            proxy_error_kind_from_std_error(&err),
            Some(ProxyErrorKind::BodyTooLarge)
        );
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

        let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
        let result = send_with_redirect_validation(&client, request, &ssrf_guard).await;
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
    async fn test_send_with_redirect_validation_dns_rebind_error_is_typed_ssrf() {
        let client = build_proxy_http_client(synctv_common::ssrf::SsrfGuard::strict_policy())
            .expect("strict proxy client should build");
        let request = client.get("http://localhost/private");
        let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();

        let Err(err) = send_with_redirect_validation(&client, request, &ssrf_guard).await else {
            panic!("DNS-level SSRF denial should fail");
        };

        assert_eq!(proxy_error_kind(&err), Some(ProxyErrorKind::Ssrf));
    }

    #[tokio::test]
    async fn test_send_with_redirect_validation_redirect_to_loopback_without_listener_fails_with_disabled_ssrf(
    ) {
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

        let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
        let result = send_with_redirect_validation(&client, request, &ssrf_guard).await;
        let Err(err) = result else {
            panic!("redirect to loopback without a listener must fail");
        };
        let proxy_err = err
            .downcast_ref::<ProxyError>()
            .expect("error should downcast to ProxyError");
        assert!(matches!(proxy_err, ProxyError::Connection(_)));
        assert!(
            proxy_err.to_string().contains("Connection failed"),
            "unexpected error: {proxy_err}"
        );
    }

    #[tokio::test]
    async fn test_send_with_redirect_validation_initial_loopback_fails_by_connection_with_disabled_ssrf(
    ) {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client should build");
        let request = client.get("http://127.0.0.1:12345/private");

        let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();
        let Err(err) = send_with_redirect_validation(&client, request, &ssrf_guard).await else {
            panic!("initial loopback target without a listener must fail");
        };
        let proxy_err = err
            .downcast_ref::<ProxyError>()
            .expect("error should downcast to ProxyError");
        assert!(matches!(proxy_err, ProxyError::Connection(_)));
        assert!(
            proxy_err.to_string().contains("Connection failed"),
            "unexpected error: {proxy_err}"
        );
    }

    #[tokio::test]
    async fn test_proxy_m3u8_and_rewrite_initial_loopback_fails_by_connection_with_disabled_ssrf() {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client should build");
        let ssrf_guard = synctv_common::ssrf::SsrfGuard::disabled();

        let err = proxy_m3u8_and_rewrite(
            &client,
            &ssrf_guard,
            "http://127.0.0.1:12345/private.m3u8",
            &HashMap::new(),
            "/proxy",
        )
        .await
        .expect_err("loopback manifest without a listener must fail");

        assert!(
            err.to_string().contains("Connection failed"),
            "unexpected error: {err}"
        );
    }
}
