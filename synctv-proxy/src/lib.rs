//! Shared media proxy utilities
//!
//! Provides reusable functions for proxying media streams and rewriting M3U8
//! playlists.  Used by per-provider proxy routes in `synctv-api`.
#![allow(clippy::missing_errors_doc)]

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

mod cors;
mod error;
pub mod grpc;
mod manifest;
mod mpd;
mod redirect;
pub mod slice_cache;

use std::collections::HashMap;
use std::future::Future;
use std::hash::BuildHasher;
use std::time::Duration;

use axum::{body::Body, http::StatusCode, response::Response};
use reqwest::header::{HeaderName, HeaderValue, USER_AGENT};
use synctv_common::ExecutionControl;

pub use cors::{handle_cors_preflight, CorsConfig};
pub use error::{
    proxy_error_kind, proxy_error_kind_from_std_error, proxy_range_not_satisfiable_total_size,
    ProxyErrorKind,
};
pub(crate) use error::{reqwest_error_indicates_connection_failure, ProxyError};
pub use manifest::{
    classify_hls_playlist, percent_encode, rewrite_m3u8, rewrite_m3u8_with_typed_url_mapper,
    rewrite_m3u8_with_url_mapper, HlsPlaylistKind, HlsResourceKind, MAX_M3U8_URLS,
};
pub use mpd::{rewrite_mpd_with_url_mapper, MpdResourceKind};
pub(crate) use redirect::{
    send_head_with_redirect_validation_with_control_and_timeout, validate_target_url_against_ssrf,
};
pub use redirect::{send_with_redirect_validation_with_control_and_timeout, ProxyResponse};

/// Maximum response body size for M3U8/MPD manifests (10 MB).
const MAX_MANIFEST_SIZE: usize = 10 * 1024 * 1024;

/// Provider-selected HTTP headers forwarded to upstream media origins.
pub type ProviderHeaders = HashMap<String, String>;

/// Default timeout for sending an upstream proxy request and receiving response headers.
///
/// Media bodies are intentionally streamed without a transfer timeout after
/// headers arrive, but the request/header phase must stay bounded so slow or
/// stalled origins cannot hold proxy tasks forever.
pub const DEFAULT_UPSTREAM_HEADER_TIMEOUT: Duration = Duration::from_secs(30);

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
        .preserve_content_encoding()
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build proxy HTTP client: {e}"))
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

/// Apply provider headers and a default User-Agent to a request builder.
pub fn apply_provider_headers<S: BuildHasher>(
    mut request: reqwest::RequestBuilder,
    _url: &str,
    provider_headers: &HashMap<String, String, S>,
) -> Result<reqwest::RequestBuilder, anyhow::Error> {
    let mut has_user_agent = false;

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

        request = request.header(header_name, header_value);
    }

    if !has_user_agent {
        request = request.header(
            USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        );
    }

    Ok(request)
}

pub struct M3u8RewriteConfig<'a, S: BuildHasher> {
    pub client: &'a reqwest::Client,
    pub ssrf_guard: &'a synctv_common::ssrf::SsrfGuard,
    pub url: &'a str,
    pub provider_headers: &'a HashMap<String, String, S>,
    pub proxy_base: &'a str,
    pub request_control: Option<&'a ExecutionControl>,
    pub upstream_header_timeout: Option<Duration>,
}

/// Returns `true` for hop-by-hop headers that must not be forwarded to the
/// client per RFC 2616 Section 13.5.1.
///
/// Note: `content-length` is intentionally *not* listed here. The forwarding
/// proxy path strips it conditionally (axum normally recomputes it from the
/// body, but 206 range responses preserve it so players can seek), so callers
/// that need that behavior handle `content-length` separately.
pub(crate) fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "transfer-encoding"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "upgrade"
    )
}

pub async fn proxy_m3u8_and_rewrite_with_control<S: BuildHasher>(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &HashMap<String, String, S>,
    proxy_base: &str,
    request_control: Option<&ExecutionControl>,
) -> Result<Response, anyhow::Error> {
    proxy_m3u8_and_rewrite_with_control_and_timeout(
        client,
        ssrf_guard,
        url,
        provider_headers,
        proxy_base,
        request_control,
        Some(DEFAULT_UPSTREAM_HEADER_TIMEOUT),
    )
    .await
}

pub async fn proxy_m3u8_and_rewrite_with_control_and_timeout<S: BuildHasher>(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &HashMap<String, String, S>,
    proxy_base: &str,
    request_control: Option<&ExecutionControl>,
    upstream_header_timeout: Option<Duration>,
) -> Result<Response, anyhow::Error> {
    proxy_m3u8_and_rewrite_with_control_and_mapper(
        M3u8RewriteConfig {
            client,
            ssrf_guard,
            url,
            provider_headers,
            proxy_base,
            request_control,
            upstream_header_timeout,
        },
        manifest::default_proxy_url,
    )
    .await
}

pub async fn proxy_m3u8_and_rewrite_with_control_and_mapper<S, F>(
    cfg: M3u8RewriteConfig<'_, S>,
    proxy_url_for_target: F,
) -> Result<Response, anyhow::Error>
where
    S: BuildHasher,
    F: Fn(&str, &str) -> String,
{
    let parsed =
        url::Url::parse(cfg.url).map_err(|e| anyhow::anyhow!("M3U8 URL is invalid: {e}"))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(anyhow::anyhow!("M3U8 URL has disallowed scheme: {scheme}"));
    }
    validate_target_url_against_ssrf(&parsed, cfg.ssrf_guard)
        .map_err(|e| anyhow::anyhow!("M3U8 SSRF validation failed: {e}"))?;

    let request = apply_provider_headers(cfg.client.get(cfg.url), cfg.url, cfg.provider_headers)?;

    let proxy_result = send_with_redirect_validation_with_control_and_timeout(
        cfg.client,
        request,
        cfg.ssrf_guard,
        cfg.request_control,
        cfg.upstream_header_timeout,
    )
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
        cfg.request_control,
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

    let m3u8_text = std::str::from_utf8(&m3u8_bytes)
        .map_err(|e| anyhow::anyhow!("M3U8 response is not valid UTF-8: {e}"))?;

    let rewritten = manifest::rewrite_m3u8_with_url_mapper(
        m3u8_text,
        cfg.url,
        cfg.proxy_base,
        proxy_url_for_target,
    )?;

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/vnd.apple.mpegurl")
        .header("Cache-Control", "no-cache")
        .body(Body::from(rewritten))
        .map_err(|e| anyhow::anyhow!("Failed to build M3U8 response: {e}"))
}

#[cfg(test)]
mod lib_tests;
