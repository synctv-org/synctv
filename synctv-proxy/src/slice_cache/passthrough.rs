use std::collections::HashMap;
use std::io;
use std::time::Duration;

use anyhow::Context as _;
use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use futures::StreamExt;
use synctv_common::ExecutionControl;

use crate::{
    apply_provider_headers, send_head_with_redirect_validation_with_control_and_timeout,
    send_with_redirect_validation_with_control_and_timeout,
};

use super::filter::build_head_cache_response;
use super::status::CacheStatus;
use super::types::HeadResourceResult;

pub(super) struct StreamThroughRequest<'a> {
    pub(super) client: &'a reqwest::Client,
    pub(super) ssrf_guard: &'a synctv_common::ssrf::SsrfGuard,
    pub(super) url: &'a str,
    pub(super) provider_headers: &'a HashMap<String, String>,
    pub(super) range_header: Option<&'a str>,
    pub(super) cache_status: CacheStatus,
    pub(super) request_control: Option<&'a ExecutionControl>,
    pub(super) upstream_header_timeout: Option<Duration>,
}

const PASSTHROUGH_RESPONSE_HEADERS: &[&str] = &[
    "content-length",
    "content-type",
    "content-encoding",
    "content-range",
    "accept-ranges",
    "cache-control",
    "etag",
    "last-modified",
];

pub(super) fn stream_existing_response_with_status(
    resp: reqwest::Response,
    cache_status: CacheStatus,
) -> Result<Response, anyhow::Error> {
    let status = if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    };

    let mut builder = Response::builder()
        .status(status)
        .header("X-Cache-Status", cache_status.as_str());

    for name in PASSTHROUGH_RESPONSE_HEADERS {
        if let Some(val) = resp.headers().get(*name) {
            if let Ok(v) = val.to_str() {
                builder = builder.header(*name, v);
            }
        }
    }

    let stream = resp
        .bytes_stream()
        .map(|result| result.map_err(|e| io::Error::other(format!("Stream error: {e}"))));

    builder
        .body(Body::from_stream(stream))
        .map_err(|e| anyhow::anyhow!("Failed to build stream-through response: {e}"))
}

pub(super) async fn stream_through_with_status(
    cfg: StreamThroughRequest<'_>,
) -> Result<Response, anyhow::Error> {
    let mut request = cfg.client.get(cfg.url);
    request = apply_provider_headers(request, cfg.url, cfg.provider_headers)?;

    if let Some(range) = cfg.range_header {
        request = request.header(reqwest::header::RANGE, range);
    }

    let resp = send_with_redirect_validation_with_control_and_timeout(
        cfg.client,
        request,
        cfg.ssrf_guard,
        cfg.request_control,
        cfg.upstream_header_timeout,
    )
    .await
    .context("Upstream request failed")?
    .response;

    stream_existing_response_with_status(resp, cfg.cache_status)
}

pub(super) async fn stream_head_through_with_status(
    cfg: StreamThroughRequest<'_>,
) -> Result<Response, anyhow::Error> {
    let mut request = cfg.client.head(cfg.url);
    request = apply_provider_headers(request, cfg.url, cfg.provider_headers)?;

    if let Some(range) = cfg.range_header {
        request = request.header(reqwest::header::RANGE, range);
    }

    let resp = send_head_with_redirect_validation_with_control_and_timeout(
        cfg.client,
        request,
        cfg.ssrf_guard,
        cfg.request_control,
        cfg.upstream_header_timeout,
    )
    .await
    .context("Upstream HEAD request failed")?
    .response;

    build_head_cache_response(&HeadResourceResult {
        status: resp.status(),
        headers: resp.headers().clone(),
        cache_status: cfg.cache_status,
    })
}
