//! Shared media proxy utilities
//!
//! Provides reusable functions for proxying media streams and rewriting M3U8
//! playlists.  Used by per-provider proxy routes in `synctv-api`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use synctv_media_providers::ssrf;

/// Maximum response body size for proxied media (256 MB).
const MAX_PROXY_BODY_SIZE: usize = 256 * 1024 * 1024;

/// Maximum response body size for M3U8/MPD manifests (10 MB).
const MAX_MANIFEST_SIZE: usize = 10 * 1024 * 1024;

/// Connection timeout for outbound proxy requests.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Overall request timeout for outbound proxy requests.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);

/// Timeout for reading the response body after headers are received.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Shared HTTP client for proxy requests.
///
/// Reuses TCP connections and TLS sessions across requests for performance.
///
/// # Panics
///
/// Maximum number of redirects to follow manually.
const MAX_REDIRECTS: usize = 10;

/// Custom DNS resolver that checks resolved IPs against SSRF blocklists
/// at connection time, preventing DNS rebinding TOCTOU attacks.
///
/// By injecting this into the reqwest client, every TCP connection attempt
/// validates the resolved IP before connecting -- not just at request-build
/// time. This closes the window where a DNS name could resolve to a public
/// IP during validation but rebind to a private IP by the time the TCP
/// handshake occurs.
#[derive(Clone)]
struct SsrfSafeDnsResolver;

impl reqwest::dns::Resolve for SsrfSafeDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let host = name.as_str();
            // Resolve via the system DNS
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, 0))
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                    Box::new(std::io::Error::other(
                        format!("DNS lookup failed for {host}: {e}"),
                    ))
                })?
                .collect();

            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::other(
                    format!("DNS lookup for {host} returned no addresses"),
                )) as Box<dyn std::error::Error + Send + Sync>);
            }

            // Filter out blocked IPs; if all are blocked, return an error.
            let safe_addrs: Vec<SocketAddr> = addrs
                .into_iter()
                .filter(|addr| !ssrf::is_blocked_ip(addr.ip()))
                .collect();

            if safe_addrs.is_empty() {
                return Err(Box::new(std::io::Error::other(
                    format!("All resolved IPs for {host} are private/reserved (SSRF blocked)"),
                )) as Box<dyn std::error::Error + Send + Sync>);
            }

            Ok(Box::new(safe_addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Panics during initialization if the HTTP client cannot be built (e.g., TLS backend unavailable).
/// This is intentional as the proxy cannot function without an HTTP client.
///
/// **SSRF Protection**: Uses a custom DNS resolver (`SsrfSafeDnsResolver`) that
/// checks every resolved IP at TCP-connect time against the SSRF blocklist.
/// This prevents DNS rebinding attacks where a hostname resolves to a public IP
/// during pre-request validation but rebinds to a private IP by connection time.
///
/// **Performance Enhancement**: Increased connection pool from 20 to 100 connections per host
/// to better support high-traffic scenarios where multiple media sources may be accessed
/// simultaneously (e.g., multi-room streaming, provider API calls).
static PROXY_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .dns_resolver(Arc::new(SsrfSafeDnsResolver))
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .read_timeout(BODY_READ_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(100)  // Increased from 20 to support high concurrency
        .pool_idle_timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|e| {
            // Log the error before panicking for better debugging
            tracing::error!("Failed to build shared proxy HTTP client: {}", e);
            panic!("Failed to build shared proxy HTTP client: {e}")
        })
});

/// Configuration for a single proxy fetch.
pub struct ProxyConfig<'a> {
    /// The remote URL to fetch.
    pub url: &'a str,
    /// Extra headers the provider requires (e.g. Referer, cookies).
    pub provider_headers: &'a HashMap<String, String>,
    /// Original client request headers to forward.
    pub client_headers: &'a HeaderMap,
}

/// Apply provider headers and defaults (User-Agent, Referer) to a request builder.
pub fn apply_provider_headers(
    mut request: reqwest::RequestBuilder,
    url: &str,
    provider_headers: &HashMap<String, String>,
) -> reqwest::RequestBuilder {
    for (name, value) in provider_headers {
        request = request.header(name.as_str(), value.as_str());
    }

    if !provider_headers.contains_key("User-Agent") {
        request = request.header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        );
    }

    if !provider_headers.contains_key("Referer") {
        if let Ok(parsed) = url::Url::parse(url) {
            let referer = format!(
                "{}://{}{}",
                parsed.scheme(),
                parsed.host_str().unwrap_or(""),
                parsed.path()
            );
            request = request.header("Referer", referer);
        }
    }

    request
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
        Err(e) => {
            let msg = e.to_string();
            Some(if msg.contains("timeout") {
                "timeout"
            } else if msg.contains("connection") {
                "connection"
            } else {
                "other"
            })
        }
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

/// Inner implementation of proxy fetch, separated for metrics wrapping.
async fn proxy_fetch_and_forward_inner(cfg: ProxyConfig<'_>) -> Result<Response, anyhow::Error> {
    validate_proxy_url(cfg.url).await?;

    let mut request = PROXY_CLIENT.get(cfg.url);

    // Forward only allowlisted client headers to avoid leaking auth tokens / cookies
    for (name, value) in cfg.client_headers {
        if !matches!(
            name.as_str(),
            "range"
                | "if-none-match"
                | "if-modified-since"
                | "accept"
                | "accept-language"
                | "user-agent"
        ) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            request = request.header(name.as_str(), v);
        }
    }

    request = apply_provider_headers(request, cfg.url, cfg.provider_headers);

    let proxy_result = send_with_redirect_validation(request).await?;

    // Retry on 5xx server errors: build a fresh request and retry once.
    // We only retry once to avoid excessive latency for the client.
    let (proxy_response, followed_redirects) = if proxy_result.response.status().is_server_error() {
        tracing::warn!(
            status = %proxy_result.response.status(),
            url = %cfg.url,
            "Upstream returned server error, retrying once"
        );
        let mut retry_req = PROXY_CLIENT.get(cfg.url);
        for (name, value) in cfg.client_headers {
            if matches!(
                name.as_str(),
                "range" | "if-none-match" | "if-modified-since" | "accept" | "accept-language" | "user-agent"
            ) {
                if let Ok(v) = value.to_str() {
                    retry_req = retry_req.header(name.as_str(), v);
                }
            }
        }
        retry_req = apply_provider_headers(retry_req, cfg.url, cfg.provider_headers);
        let retry_result = send_with_redirect_validation(retry_req).await?;
        (retry_result.response, retry_result.followed_redirects)
    } else {
        (proxy_result.response, proxy_result.followed_redirects)
    };

    let status = proxy_response.status();
    let response_headers = proxy_response.headers().clone();

    // Check Content-Length hint before streaming (not authoritative, but catches obvious cases)
    if let Some(cl) = proxy_response.content_length() {
        if cl as usize > MAX_PROXY_BODY_SIZE {
            return Err(anyhow::anyhow!(
                "Response too large ({cl} bytes, max {MAX_PROXY_BODY_SIZE})"
            ));
        }
    }

    // Determine if reqwest auto-decompressed the response body.
    // reqwest transparently decodes gzip, deflate, and brotli by default.
    // If the upstream used one of these encodings, reqwest has already decoded
    // the body, so we must strip the content-encoding header (otherwise the
    // client would try to decompress already-decoded data).
    // For other encodings (e.g. zstd) that reqwest does NOT handle, we must
    // preserve the header so the client knows to decode it.
    //
    // Additionally, when redirects were followed the body has been fully
    // consumed and re-requested at the final URL; in that case we strip
    // content-encoding unconditionally because the body is already decoded.
    let reqwest_auto_decompressed = followed_redirects || response_headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ce| {
            let ce_lower = ce.to_lowercase();
            ce_lower == "gzip" || ce_lower == "deflate" || ce_lower == "br"
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
        Some(ct) if ct.contains("video/") || ct.contains("audio/") || ct.contains("octet-stream") => {
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
    use futures::StreamExt;
    let body_stream = proxy_response.bytes_stream().scan((0usize, false), |(total, exceeded), chunk| {
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
                    // `data` is already a `Bytes` which is cheaply cloneable (Arc-backed),
                    // but we own it here so no clone is needed at all.
                    futures::future::ready(Some(Ok(data)))
                }
            }
            Err(e) => futures::future::ready(Some(Err(std::io::Error::other(
                e,
            )))),
        }
    });
    let body = Body::from_stream(body_stream);

    builder
        .body(body)
        .map_err(|e| anyhow::anyhow!("Failed to build response: {e}"))
}

/// Fetch a remote M3U8, rewrite its URLs so segments proxy through
/// `proxy_base`, and return the rewritten content.
pub async fn proxy_m3u8_and_rewrite(
    url: &str,
    provider_headers: &HashMap<String, String>,
    proxy_base: &str,
) -> Result<Response, anyhow::Error> {
    validate_proxy_url(url).await?;

    let request = apply_provider_headers(PROXY_CLIENT.get(url), url, provider_headers);

    let proxy_result = send_with_redirect_validation(request).await?;
    let proxy_response = proxy_result.response;

    if !proxy_response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Remote M3U8 returned status {}",
            proxy_response.status()
        ));
    }

    if let Some(cl) = proxy_response.content_length() {
        if cl as usize > MAX_MANIFEST_SIZE {
            return Err(anyhow::anyhow!(
                "M3U8 too large ({cl} bytes, max {MAX_MANIFEST_SIZE})"
            ));
        }
    }

    let m3u8_bytes = tokio::time::timeout(BODY_READ_TIMEOUT, proxy_response.bytes())
        .await
        .map_err(|_| anyhow::anyhow!("M3U8 body read timed out after {}s", BODY_READ_TIMEOUT.as_secs()))?
        .map_err(|e| anyhow::anyhow!("Failed to read M3U8 body: {e}"))?;

    if m3u8_bytes.len() > MAX_MANIFEST_SIZE {
        return Err(anyhow::anyhow!(
            "M3U8 too large ({} bytes, max {MAX_MANIFEST_SIZE})",
            m3u8_bytes.len()
        ));
    }

    let m3u8_text = String::from_utf8(m3u8_bytes.to_vec())
        .map_err(|e| anyhow::anyhow!("M3U8 response is not valid UTF-8: {e}"))?;

    let rewritten = rewrite_m3u8(&m3u8_text, url, proxy_base);

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/vnd.apple.mpegurl")
        .header("Cache-Control", "no-cache")
        .body(Body::from(rewritten))
        .map_err(|e| anyhow::anyhow!("Failed to build M3U8 response: {e}"))
}

/// Preflight handler suitable for `OPTIONS` routes.
///
/// Returns CORS headers as defense-in-depth. The global `CorsLayer` middleware
/// handles standard preflight requests before they reach routes, but this
/// ensures correct headers if the middleware is bypassed or misconfigured.
#[allow(clippy::unused_async)]
pub async fn proxy_options_preflight() -> impl IntoResponse {
    (
        StatusCode::NO_CONTENT,
        [
            ("Access-Control-Allow-Origin", "*"),
            ("Access-Control-Allow-Methods", "GET, OPTIONS"),
            ("Access-Control-Allow-Headers", "Authorization, Content-Type, Accept, Range"),
            ("Access-Control-Max-Age", "86400"),
        ],
    )
}

// ------------------------------------------------------------------
// M3U8 rewriting helpers
// ------------------------------------------------------------------

/// Maximum number of URLs that can be rewritten in a single M3U8 playlist.
/// This prevents abuse via extremely large playlists that could cause memory
/// exhaustion or excessive proxy traffic.
const MAX_M3U8_URLS: usize = 1000;

/// Rewrite URLs inside an M3U8 playlist so they proxy through the server.
///
/// # Limits
/// - Maximum 1000 URLs per playlist (prevents abuse)
pub fn rewrite_m3u8(m3u8: &str, source_url: &str, proxy_base: &str) -> String {
    let base = url::Url::parse(source_url).ok();
    let mut output = String::with_capacity(m3u8.len());
    let mut url_count = 0usize;

    for line in m3u8.lines() {
        if line.starts_with('#') {
            let (rewritten_line, count) = rewrite_uri_attribute_with_count(line, base.as_ref(), proxy_base);
            url_count += count;
            output.push_str(&rewritten_line);
        } else {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                output.push_str(line);
            } else {
                url_count += 1;
                if url_count > MAX_M3U8_URLS {
                    tracing::warn!(
                        source_url = %source_url,
                        url_count = url_count,
                        max = MAX_M3U8_URLS,
                        "M3U8 playlist exceeded maximum URL limit, truncating with EXT-X-ENDLIST"
                    );
                    // Terminate the playlist cleanly instead of including raw segments
                    output.push_str("#EXT-X-ENDLIST\n");
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

    if url_count > MAX_M3U8_URLS / 2 {
        tracing::info!(
            source_url = %source_url,
            url_count = url_count,
            "M3U8 playlist has many URLs"
        );
    }

    output
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
pub fn rewrite_uri_attribute_with_count(line: &str, base: Option<&url::Url>, proxy_base: &str) -> (String, usize) {
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
/// Uses the `NON_ALPHANUMERIC` encode set, which encodes everything except
/// `A-Z a-z 0-9 - _ . ~` (the RFC 3986 "unreserved" characters).
#[must_use]
pub fn percent_encode(input: &str) -> String {
    percent_encoding::utf8_percent_encode(input, percent_encoding::NON_ALPHANUMERIC).to_string()
}

// ------------------------------------------------------------------
// Manual redirect following with DNS validation
// ------------------------------------------------------------------

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

/// Send a request via the proxy client, manually following redirects with
/// full async DNS validation on every hop.
///
/// Automatic redirects are disabled on `PROXY_CLIENT`, so 3xx responses
/// are handled here. Each redirect target gets both static URL validation
/// and async DNS resolution checks to prevent DNS-rebinding SSRF.
///
/// Headers matching [`REDIRECT_PRESERVE_HEADERS`] are captured from the
/// initial request and re-applied on every redirect hop so that provider
/// and client headers are not lost.
async fn send_with_redirect_validation(
    request: reqwest::RequestBuilder,
) -> Result<ProxyResponse, anyhow::Error> {
    // Build the request to capture headers before sending.
    let built = request
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build proxy request: {e}"))?;

    // Validate the INITIAL URL before sending (SSRF protection).
    // Previously only redirect targets were validated here, relying on callers
    // to validate the initial URL. This was fragile and left a gap if any
    // caller forgot to validate. Now we validate both initial and redirect URLs.
    validate_proxy_url(built.url().as_str()).await?;

    // Snapshot headers to preserve across redirects.
    let preserved: Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)> = built
        .headers()
        .iter()
        .filter(|(name, _)| REDIRECT_PRESERVE_HEADERS.contains(&name.as_str()))
        .map(|(n, v)| (n.clone(), v.clone()))
        .collect();

    let mut response = PROXY_CLIENT
        .execute(built)
        .await
        .map_err(|e| anyhow::anyhow!("Proxy request failed: {e}"))?;

    let mut hops = 0usize;
    while response.status().is_redirection() {
        hops += 1;
        if hops > MAX_REDIRECTS {
            return Err(anyhow::anyhow!("Too many redirects ({MAX_REDIRECTS} max)"));
        }

        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .ok_or_else(|| anyhow::anyhow!("Redirect without Location header"))?
            .to_str()
            .map_err(|_| anyhow::anyhow!("Invalid Location header"))?
            .to_string();

        // Full validation: static checks + async DNS resolution
        validate_proxy_url(&location).await?;

        let mut redirect_req = PROXY_CLIENT.get(&location);
        for (name, value) in &preserved {
            redirect_req = redirect_req.header(name.clone(), value.clone());
        }

        response = redirect_req
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Redirect request failed: {e}"))?;
    }

    Ok(ProxyResponse {
        response,
        followed_redirects: hops > 0,
    })
}

// ------------------------------------------------------------------
// SSRF protection
// ------------------------------------------------------------------

/// Validate that a URL is safe to proxy (not targeting internal services).
///
/// Performs DNS resolution to guard against DNS rebinding attacks where a
/// hostname passes string-level checks but resolves to a private IP.
///
/// Delegates to `synctv_media_providers::ssrf` as the single source of truth
/// for SSRF validation logic.
pub async fn validate_proxy_url(raw: &str) -> Result<(), anyhow::Error> {
    // Static string-level checks (scheme, hostname blocklist, literal IP)
    validate_proxy_url_static(raw)?;

    // Resolve hostname and check all resolved IPs to prevent DNS rebinding
    let parsed = url::Url::parse(raw)?;
    let host = parsed.host_str().unwrap_or("");
    // Only resolve if the host is NOT already a literal IP (already checked above)
    if host.parse::<std::net::IpAddr>().is_err() {
        let port = parsed.port().unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });
        let addrs = tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| anyhow::anyhow!("DNS lookup failed for {host}: {e}"))?;

        let mut found = false;
        for addr in addrs {
            if ssrf::is_blocked_ip(addr.ip()) {
                return Err(anyhow::anyhow!(
                    "Hostname {host} resolves to private/reserved IP {}",
                    addr.ip()
                ));
            }
            found = true;
        }
        if !found {
            return Err(anyhow::anyhow!("Hostname {host} resolved to no addresses"));
        }
    }

    Ok(())
}

/// Synchronous URL string validation (scheme, hostname blocklist, literal IP checks).
///
/// Delegates to `synctv_media_providers::ssrf::check_url` as the single source
/// of truth for SSRF URL validation.
pub fn validate_proxy_url_static(raw: &str) -> Result<(), anyhow::Error> {
    match ssrf::check_url(raw) {
        ssrf::SsrfCheckResult::Ok => Ok(()),
        ssrf::SsrfCheckResult::Blocked(reason) => Err(anyhow::anyhow!(reason)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    // ------------------------------------------------------------------
    // P-01: CORS preflight returns proper headers
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_options_preflight_returns_cors_headers() {
        let response = proxy_options_preflight().await.into_response();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let headers = response.headers();
        assert_eq!(
            headers.get("Access-Control-Allow-Origin").map(|v| v.to_str().unwrap_or("")),
            Some("*"),
            "OPTIONS preflight must include Access-Control-Allow-Origin"
        );
        assert!(
            headers.get("Access-Control-Allow-Methods").is_some(),
            "OPTIONS preflight must include Access-Control-Allow-Methods"
        );
        assert!(
            headers.get("Access-Control-Allow-Headers").is_some(),
            "OPTIONS preflight must include Access-Control-Allow-Headers"
        );
        assert!(
            headers.get("Access-Control-Max-Age").is_some(),
            "OPTIONS preflight should include Access-Control-Max-Age for caching"
        );
    }

    // ------------------------------------------------------------------
    // P-06: Redirect header preservation list
    // ------------------------------------------------------------------

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

    // ------------------------------------------------------------------
    // Existing helpers
    // ------------------------------------------------------------------

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
        );
        assert!(rewritten.contains("/proxy/stream?url="));
        assert!(rewritten.contains("cdn%2Eexample%2Ecom"));
    }

    #[test]
    fn test_validate_proxy_url_static_blocks_localhost() {
        assert!(validate_proxy_url_static("http://localhost/foo").is_err());
    }

    #[test]
    fn test_validate_proxy_url_static_blocks_private_ip() {
        assert!(validate_proxy_url_static("http://192.168.1.1/foo").is_err());
        assert!(validate_proxy_url_static("http://10.0.0.1/foo").is_err());
        assert!(validate_proxy_url_static("http://127.0.0.1/foo").is_err());
    }

    #[test]
    fn test_validate_proxy_url_static_allows_public() {
        assert!(validate_proxy_url_static("https://cdn.bilibili.com/v1.m4s").is_ok());
    }

    #[test]
    fn test_validate_proxy_url_static_blocks_non_http() {
        assert!(validate_proxy_url_static("ftp://example.com/file").is_err());
        assert!(validate_proxy_url_static("file:///etc/passwd").is_err());
    }
}
