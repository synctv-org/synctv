//! Shared media proxy utilities
//!
//! Provides reusable functions for proxying media streams and rewriting M3U8
//! playlists.  Used by per-provider proxy routes in `synctv-api`.

pub mod mpd;

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::LazyLock;
use std::time::Duration;

use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

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

/// Panics during initialization if the HTTP client cannot be built (e.g., TLS backend unavailable).
/// This is intentional as the proxy cannot function without an HTTP client.
static PROXY_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .read_timeout(BODY_READ_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(20)
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
fn apply_provider_headers(
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

/// Fetch a remote URL and return the response.
pub async fn proxy_fetch_and_forward(cfg: ProxyConfig<'_>) -> Result<Response, anyhow::Error> {
    let timer = synctv_core::metrics::stream::STREAM_RELAY_DURATION
        .with_label_values(&["hls"])
        .start_timer();

    let result = proxy_fetch_and_forward_inner(cfg).await;

    timer.observe_duration();
    if let Err(ref e) = result {
        let error_type = if e.to_string().contains("timeout") {
            "timeout"
        } else if e.to_string().contains("connection") {
            "connection"
        } else {
            "other"
        };
        synctv_core::metrics::stream::STREAM_ERRORS
            .with_label_values(&["hls", error_type])
            .inc();
    }

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

    let proxy_response = send_with_redirect_validation(request).await?;

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

    let mut builder = Response::builder().status(status);

    for (name, value) in &response_headers {
        // Filter hop-by-hop headers per RFC 2616 Section 13.5.1
        if matches!(
            name.as_str(),
            "connection"
                | "transfer-encoding"
                | "content-encoding"
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

    builder = builder.header("Cache-Control", "no-cache");
    builder = builder.header("Pragma", "no-cache");
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
            Ok(ref data) => {
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
                    futures::future::ready(Some(Ok(data.clone())))
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

    let proxy_response = send_with_redirect_validation(request).await?;

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

    let m3u8_text = String::from_utf8_lossy(&m3u8_bytes).to_string();

    let rewritten = rewrite_m3u8(&m3u8_text, url, proxy_base);

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/vnd.apple.mpegurl")
        .header("Cache-Control", "no-cache")
        .body(Body::from(rewritten))
        .map_err(|e| anyhow::anyhow!("Failed to build M3U8 response: {e}"))
}

/// Preflight handler suitable for `OPTIONS` routes.
#[allow(clippy::unused_async)]
pub async fn proxy_options_preflight() -> impl IntoResponse {
    StatusCode::NO_CONTENT
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
fn rewrite_m3u8(m3u8: &str, source_url: &str, proxy_base: &str) -> String {
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
                        "M3U8 playlist exceeded maximum URL limit, truncating"
                    );
                    // Still output the line but don't proxy it
                    output.push_str("# ERROR: Too many URLs in playlist\n");
                    continue;
                }
                let absolute = make_absolute(trimmed, base.as_ref());
                let proxied = format!("{}?url={}", proxy_base, percent_encode(&absolute));
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
fn make_absolute(raw: &str, base: Option<&url::Url>) -> String {
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
fn rewrite_uri_attribute_with_count(line: &str, base: Option<&url::Url>, proxy_base: &str) -> (String, usize) {
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
            let proxied = format!("{}?url={}", proxy_base, percent_encode(&absolute));
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

/// Send a request via the proxy client, manually following redirects with
/// full async DNS validation on every hop.
///
/// Automatic redirects are disabled on `PROXY_CLIENT`, so 3xx responses
/// are handled here. Each redirect target gets both static URL validation
/// and async DNS resolution checks to prevent DNS-rebinding SSRF.
async fn send_with_redirect_validation(
    request: reqwest::RequestBuilder,
) -> Result<reqwest::Response, anyhow::Error> {
    let mut response = request
        .send()
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

        response = PROXY_CLIENT
            .get(&location)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Redirect request failed: {e}"))?;
    }

    Ok(response)
}

// ------------------------------------------------------------------
// SSRF protection
// ------------------------------------------------------------------

/// Validate that a URL is safe to proxy (not targeting internal services).
///
/// Performs DNS resolution to guard against DNS rebinding attacks where a
/// hostname passes string-level checks but resolves to a private IP.
pub async fn validate_proxy_url(raw: &str) -> Result<(), anyhow::Error> {
    validate_proxy_url_static(raw)?;

    // Resolve hostname and check all resolved IPs to prevent DNS rebinding
    let parsed = url::Url::parse(raw)?;
    let host = parsed.host_str().unwrap_or("");
    // Only resolve if the host is NOT already a literal IP (already checked above)
    if host.parse::<IpAddr>().is_err() {
        let port = parsed.port().unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });
        let addrs = tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| anyhow::anyhow!("DNS lookup failed for {host}: {e}"))?;

        let mut found = false;
        for addr in addrs {
            if is_private_ip(addr.ip()) {
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
/// Used by redirect policy where async is not available.
fn validate_proxy_url_static(raw: &str) -> Result<(), anyhow::Error> {
    let parsed = url::Url::parse(raw)
        .map_err(|_| anyhow::anyhow!("Invalid proxy URL"))?;

    // Only allow HTTP(S) schemes
    match parsed.scheme() {
        "http" | "https" => {}
        s => return Err(anyhow::anyhow!("Disallowed URL scheme: {s}")),
    }

    let host = parsed.host_str()
        .ok_or_else(|| anyhow::anyhow!("URL has no host"))?;

    // Block well-known internal hostnames (defense-in-depth; IP checks cover most cases)
    if matches!(
        host,
        "localhost"
            | "metadata.google.internal"
            | "instance-data"
            | "metadata"
            | "kubernetes.default"
            | "kubernetes.default.svc"
    ) {
        return Err(anyhow::anyhow!("Proxy to internal hosts is not allowed"));
    }

    // Parse and check IP address
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(ip) {
            return Err(anyhow::anyhow!("Proxy to private IP addresses is not allowed"));
        }
    }

    // Also check hostnames that are raw IPv6 in brackets (url crate strips brackets)
    if let Some(url::Host::Ipv4(ip)) = parsed.host() {
        if is_private_ip(IpAddr::V4(ip)) {
            return Err(anyhow::anyhow!("Proxy to private IP addresses is not allowed"));
        }
    }
    if let Some(url::Host::Ipv6(ip)) = parsed.host() {
        if is_private_ip(IpAddr::V6(ip)) {
            return Err(anyhow::anyhow!("Proxy to private IP addresses is not allowed"));
        }
    }

    Ok(())
}

/// Check if an IP address is in a private/reserved range.
///
/// Delegates to `synctv_core::validation::is_private_ip` which is the
/// authoritative SSRF IP validator covering IPv4 private, loopback,
/// link-local, CGNAT, multicast, broadcast, IPv4-mapped IPv6, and
/// IPv6 unique-local addresses.
fn is_private_ip(ip: IpAddr) -> bool {
    synctv_core::validation::is_private_ip(&ip)
}
