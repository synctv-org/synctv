//! Filter entry points: proxy_with_cache, stream_through_with_status,
//! head_content_length.
//!
//! These correspond to nginx's header/body filter chain -- the top-level
//! request handling that decides whether to use slice caching or passthrough.

use std::collections::HashMap;
use std::io;
use std::time::Duration;

use anyhow::Context as _;
use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use bytes::Bytes;
use synctv_common::ExecutionControl;

use crate::{
    apply_provider_headers, run_with_proxy_cancellation,
    send_with_redirect_validation_with_control_and_timeout, ProxyError,
};

use super::etag::CachedResourceMeta;
use super::head;
use super::passthrough::{
    stream_existing_response_with_status, stream_head_through_with_status,
    stream_through_with_status, StreamThroughRequest,
};
use super::range::{
    parse_client_range_plan, range_bounds_for_total, slice_index_for_byte, ClientRangeError,
    ClientRangePlan,
};
use super::status::CacheStatus;
use super::store::SliceCache;
use super::types::{CachedSlice, HeadResourceResult, SliceFetchResult};

fn proxy_error_from_client_range_error(error: ClientRangeError) -> ProxyError {
    match error {
        ClientRangeError::InvalidRequest(message) => ProxyError::InvalidRequest(message),
        ClientRangeError::Unsatisfiable {
            message,
            total_size,
        } => ProxyError::RangeNotSatisfiable {
            message,
            total_size,
        },
    }
}

fn check_stream_active(request_control: Option<&ExecutionControl>) -> Result<(), io::Error> {
    if let Some(control) = request_control {
        control
            .check_cancelled()
            .map_err(|error| io::Error::other(format!("Proxy stream stopped: {error}")))?;
    }
    Ok(())
}

struct FullResourceStream {
    cache: SliceCache,
    url: String,
    provider_headers: HashMap<String, String>,
    total_size: u64,
    total_slices: u64,
    first_chunk: Bytes,
    request_control: Option<ExecutionControl>,
    upstream_header_timeout: Option<Duration>,
}

fn full_resource_slice_stream(
    cfg: FullResourceStream,
) -> impl futures::Stream<Item = Result<Bytes, io::Error>> {
    let FullResourceStream {
        cache,
        url,
        provider_headers,
        total_size,
        total_slices,
        first_chunk,
        request_control,
        upstream_header_timeout,
    } = cfg;

    futures::stream::try_unfold(
        (0_u64, Some(first_chunk)),
        move |(slice_index, mut first_chunk)| {
            let cache = cache.clone();
            let url = url.clone();
            let provider_headers = provider_headers.clone();
            let request_control = request_control.clone();
            async move {
                if slice_index >= total_slices {
                    return Ok::<Option<(Bytes, _)>, io::Error>(None);
                }
                check_stream_active(request_control.as_ref())?;
                let next_index = slice_index.saturating_add(1);
                let chunk = if let Some(chunk) = first_chunk.take() {
                    Ok(chunk)
                } else {
                    cache
                        .get_or_fetch_slice_with_control(
                            &url,
                            &provider_headers,
                            slice_index,
                            total_size,
                            request_control.as_ref(),
                            upstream_header_timeout,
                        )
                        .await
                        .map(|(data, _status)| data)
                        .map_err(|error| {
                            io::Error::other(format!("Slice cache fetch failed: {error}"))
                        })
                }?;
                Ok(Some((chunk, (next_index, first_chunk))))
            }
        },
    )
}

fn crop_slice_for_range(
    data: &Bytes,
    slice_start: u64,
    range_start: u64,
    range_end: u64,
) -> Result<Bytes, io::Error> {
    #[allow(clippy::cast_possible_truncation)]
    let offset_start = if range_start > slice_start {
        (range_start - slice_start) as usize
    } else {
        0
    };

    let slice_len = data.len();
    let slice_end = slice_start
        .checked_add(slice_len as u64)
        .ok_or_else(|| io::Error::other("Slice end overflow"))?;
    #[allow(clippy::cast_possible_truncation)]
    let offset_end = if range_end < slice_end {
        (range_end - slice_start) as usize + 1
    } else {
        slice_len
    };

    if offset_start > offset_end || offset_end > slice_len {
        return Err(io::Error::other("Invalid slice crop range"));
    }

    Ok(data.slice(offset_start..offset_end))
}

async fn stream_original_range_with_learned_meta(
    cache: &SliceCache,
    url: &str,
    provider_headers: &HashMap<String, String>,
    range_str: &str,
    request_control: Option<&ExecutionControl>,
    upstream_header_timeout: Option<Duration>,
) -> Result<Response, anyhow::Error> {
    let mut request = cache.client().get(url);
    request = apply_provider_headers(request, url, provider_headers)?;
    request = request.header("Range", range_str);

    let resp = send_with_redirect_validation_with_control_and_timeout(
        cache.client(),
        request,
        cache.ssrf_guard(),
        request_control,
        upstream_header_timeout,
    )
    .await
    .context("Upstream range request failed")?
    .response;

    if resp.status() == StatusCode::PARTIAL_CONTENT {
        if let Some(total_size) = head::parse_total_size_from_content_range(&resp)? {
            cache.put_resource_meta(
                url,
                provider_headers,
                CachedResourceMeta {
                    etag: resp
                        .headers()
                        .get("etag")
                        .and_then(|v| v.to_str().ok())
                        .map(ToString::to_string),
                    last_modified: resp
                        .headers()
                        .get("last-modified")
                        .and_then(|v| v.to_str().ok())
                        .map(ToString::to_string),
                    total_size: Some(total_size),
                    supports_ranges: true,
                    content_type: resp
                        .headers()
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .map(ToString::to_string),
                    validated_at: std::time::SystemTime::now(),
                    last_accessed: std::time::SystemTime::now(),
                },
            );
        }
    }

    stream_existing_response_with_status(resp, CacheStatus::Bypass)
}

/// Send a HEAD request to discover the upstream `Content-Length`.
///
/// Falls back to a constrained `GET Range: bytes=0-0` request when the origin
/// rejects HEAD or omits `Content-Length`, while still reusing the proxy's
/// SSRF-safe redirect validation path.
#[allow(clippy::implicit_hasher)]
pub async fn head_content_length(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &HashMap<String, String>,
) -> Result<u64, anyhow::Error> {
    head::head_content_length(client, ssrf_guard, url, provider_headers).await
}

#[allow(clippy::implicit_hasher)]
pub async fn head_content_length_with_control(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
) -> Result<u64, anyhow::Error> {
    head::head_content_length_with_control(
        client,
        ssrf_guard,
        url,
        provider_headers,
        request_control,
    )
    .await
}

#[allow(clippy::implicit_hasher)]
pub async fn head_content_length_with_control_and_timeout(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
    upstream_header_timeout: Option<Duration>,
) -> Result<u64, anyhow::Error> {
    head::head_content_length_with_control_and_timeout(
        client,
        ssrf_guard,
        url,
        provider_headers,
        request_control,
        upstream_header_timeout,
    )
    .await
}

// proxy_with_cache  --  main entry point

/// Serve a request through the slice cache.
///
/// Behaviour:
/// - **Disabled cache**: streams through with `X-Cache-Status: BYPASS`.
/// - **No Range header**: requests the first full slice from the origin; if
///   the origin supports byte ranges, streams the complete resource as cached
///   slices with a `200 OK` response; otherwise streams through with `BYPASS`.
/// - **Single Range**: slice-cache path with `HIT` / `MISS` / `EXPIRED`
///   / `STALE` / `UPDATING` / `REVALIDATED`.
/// - **Multi-Range**: bypasses the slice cache and streams the original
///   upstream response so standards-compliant multipart range requests remain
///   valid without implementing multipart assembly in the cache.
#[allow(clippy::implicit_hasher)]
pub async fn proxy_with_cache(
    cache: &SliceCache,
    range_header: Option<&str>,
    url: &str,
    provider_headers: &HashMap<String, String>,
) -> Result<Response, anyhow::Error> {
    proxy_with_cache_with_control(cache, range_header, url, provider_headers, None).await
}

#[allow(clippy::implicit_hasher)]
pub async fn proxy_with_cache_with_control(
    cache: &SliceCache,
    range_header: Option<&str>,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
) -> Result<Response, anyhow::Error> {
    proxy_with_cache_with_control_and_timeout(
        cache,
        range_header,
        url,
        provider_headers,
        request_control,
        None,
    )
    .await
}

#[allow(clippy::implicit_hasher)]
pub async fn proxy_with_cache_with_control_and_timeout(
    cache: &SliceCache,
    range_header: Option<&str>,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
    upstream_header_timeout: Option<Duration>,
) -> Result<Response, anyhow::Error> {
    proxy_with_cache_enabled_with_control_and_timeout(
        cache,
        cache.config().enabled,
        range_header,
        url,
        provider_headers,
        request_control,
        upstream_header_timeout,
    )
    .await
}

/// Serve a request through the slice cache with an explicit runtime enable flag.
///
/// This allows callers that support dynamic runtime settings to bypass the
/// startup-time `SliceCacheConfig.enabled` snapshot and decide cache usage from
/// their live configuration source.
#[allow(clippy::implicit_hasher)]
pub async fn proxy_with_cache_enabled(
    cache: &SliceCache,
    cache_enabled: bool,
    range_header: Option<&str>,
    url: &str,
    provider_headers: &HashMap<String, String>,
) -> Result<Response, anyhow::Error> {
    proxy_with_cache_enabled_with_control(
        cache,
        cache_enabled,
        range_header,
        url,
        provider_headers,
        None,
    )
    .await
}

#[allow(clippy::implicit_hasher)]
pub async fn proxy_with_cache_enabled_with_control(
    cache: &SliceCache,
    cache_enabled: bool,
    range_header: Option<&str>,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
) -> Result<Response, anyhow::Error> {
    proxy_with_cache_enabled_with_control_and_timeout(
        cache,
        cache_enabled,
        range_header,
        url,
        provider_headers,
        request_control,
        None,
    )
    .await
}

#[allow(clippy::implicit_hasher)]
pub async fn proxy_with_cache_enabled_with_control_and_timeout(
    cache: &SliceCache,
    cache_enabled: bool,
    range_header: Option<&str>,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
    upstream_header_timeout: Option<Duration>,
) -> Result<Response, anyhow::Error> {
    if !cache_enabled {
        return stream_through_with_status(StreamThroughRequest {
            client: cache.client(),
            ssrf_guard: cache.ssrf_guard(),
            url,
            provider_headers,
            range_header,
            cache_status: CacheStatus::Bypass,
            request_control,
            upstream_header_timeout,
        })
        .await;
    }

    let Some(range_str) = range_header else {
        return no_range_slice_cache_path(
            cache,
            url,
            provider_headers,
            request_control,
            upstream_header_timeout,
        )
        .await;
    };

    let plan = parse_client_range_plan(range_str).map_err(proxy_error_from_client_range_error)?;
    match plan {
        ClientRangePlan::MultiRange => {
            return stream_through_with_status(StreamThroughRequest {
                client: cache.client(),
                ssrf_guard: cache.ssrf_guard(),
                url,
                provider_headers,
                range_header: Some(range_str),
                cache_status: CacheStatus::Bypass,
                request_control,
                upstream_header_timeout,
            })
            .await;
        }
        ClientRangePlan::Explicit { .. }
        | ClientRangePlan::OpenEnded { .. }
        | ClientRangePlan::Suffix { .. } => {}
    }

    let cached_meta = cache.get_resource_meta(url, provider_headers);
    if cached_meta
        .as_ref()
        .is_some_and(|meta| !meta.supports_ranges)
    {
        return stream_through_with_status(StreamThroughRequest {
            client: cache.client(),
            ssrf_guard: cache.ssrf_guard(),
            url,
            provider_headers,
            range_header: None,
            cache_status: CacheStatus::Bypass,
            request_control,
            upstream_header_timeout,
        })
        .await;
    }
    let known_total_size = cached_meta.and_then(|meta| meta.total_size);
    if let Some(total_size) = known_total_size {
        range_bounds_for_total(plan, total_size).map_err(proxy_error_from_client_range_error)?;
    }

    match plan {
        ClientRangePlan::Explicit { .. } | ClientRangePlan::OpenEnded { .. } => {}
        ClientRangePlan::Suffix { .. } => {
            if known_total_size.is_none() {
                return stream_original_range_with_learned_meta(
                    cache,
                    url,
                    provider_headers,
                    range_str,
                    request_control,
                    upstream_header_timeout,
                )
                .await;
            }
        }
        ClientRangePlan::MultiRange => {
            return stream_through_with_status(StreamThroughRequest {
                client: cache.client(),
                ssrf_guard: cache.ssrf_guard(),
                url,
                provider_headers,
                range_header: Some(range_str),
                cache_status: CacheStatus::Bypass,
                request_control,
                upstream_header_timeout,
            })
            .await;
        }
    }

    range_slice_cache_path(
        cache,
        range_str,
        url,
        provider_headers,
        request_control,
        upstream_header_timeout,
        plan,
        known_total_size,
    )
    .await
}

#[allow(clippy::implicit_hasher)]
pub async fn proxy_head_with_cache_enabled_with_control(
    cache: &SliceCache,
    cache_enabled: bool,
    range_header: Option<&str>,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
) -> Result<Response, anyhow::Error> {
    proxy_head_with_cache_enabled_with_control_and_timeout(
        cache,
        cache_enabled,
        range_header,
        url,
        provider_headers,
        request_control,
        None,
    )
    .await
}

#[allow(clippy::implicit_hasher)]
pub async fn proxy_head_with_cache_enabled_with_control_and_timeout(
    cache: &SliceCache,
    cache_enabled: bool,
    range_header: Option<&str>,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
    upstream_header_timeout: Option<Duration>,
) -> Result<Response, anyhow::Error> {
    if !cache_enabled {
        return stream_head_through_with_status(StreamThroughRequest {
            client: cache.client(),
            ssrf_guard: cache.ssrf_guard(),
            url,
            provider_headers,
            range_header,
            cache_status: CacheStatus::Bypass,
            request_control,
            upstream_header_timeout,
        })
        .await;
    }

    let result = cache
        .get_or_fetch_head_resource_with_control(
            url,
            provider_headers,
            range_header,
            request_control,
            upstream_header_timeout,
        )
        .await?;
    build_head_cache_response(&result)
}

pub(super) fn build_head_cache_response(
    result: &HeadResourceResult,
) -> Result<Response, anyhow::Error> {
    let mut builder = Response::builder()
        .status(
            StatusCode::from_u16(result.status.as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        )
        .header("X-Cache-Status", result.cache_status.as_str());

    for (name, value) in &result.headers {
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

    builder
        .body(Body::empty())
        .map_err(|e| anyhow::anyhow!("Failed to build HEAD cache response: {e}"))
}

#[allow(clippy::too_many_arguments)]
async fn range_slice_cache_path(
    cache: &SliceCache,
    range_str: &str,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
    upstream_header_timeout: Option<Duration>,
    plan: ClientRangePlan,
    known_total_size: Option<u64>,
) -> Result<Response, anyhow::Error> {
    let first_byte = match plan {
        ClientRangePlan::Explicit { start, .. } | ClientRangePlan::OpenEnded { start } => start,
        ClientRangePlan::Suffix { .. } => {
            let Some(total_size) = known_total_size else {
                return stream_original_range_with_learned_meta(
                    cache,
                    url,
                    provider_headers,
                    range_str,
                    request_control,
                    upstream_header_timeout,
                )
                .await;
            };
            let (start, _) = range_bounds_for_total(plan, total_size)
                .map_err(proxy_error_from_client_range_error)?;
            start
        }
        ClientRangePlan::MultiRange => {
            return stream_through_with_status(StreamThroughRequest {
                client: cache.client(),
                ssrf_guard: cache.ssrf_guard(),
                url,
                provider_headers,
                range_header: Some(range_str),
                cache_status: CacheStatus::Bypass,
                request_control,
                upstream_header_timeout,
            })
            .await;
        }
    };
    let first_slice_index = slice_index_for_byte(first_byte, cache.config().slice_size);
    let first_slice = match cache
        .get_or_fetch_slice_or_bypass_with_control(
            url,
            provider_headers,
            first_slice_index,
            known_total_size,
            request_control,
            upstream_header_timeout,
        )
        .await?
    {
        SliceFetchResult::Slice(slice) => slice,
        SliceFetchResult::Bypass(resp) => {
            return stream_existing_response_with_status(resp, CacheStatus::Bypass);
        }
    };

    let first_status = first_slice.status;
    let total_size = first_slice.slice.total_size;
    let response_header_slice = CachedSlice {
        total_size,
        content_type: first_slice.slice.content_type.clone(),
        etag: first_slice.slice.etag.clone(),
        last_modified: first_slice.slice.last_modified.clone(),
        data: Bytes::new(),
    };
    let (range_start, range_end) =
        range_bounds_for_total(plan, total_size).map_err(proxy_error_from_client_range_error)?;
    if range_start > range_end {
        return Err(ProxyError::InvalidRequest("Invalid range".to_string()).into());
    }

    let last_slice_index = slice_index_for_byte(range_end, cache.config().slice_size);
    let content_length = range_end
        .checked_sub(range_start)
        .and_then(|len| len.checked_add(1))
        .ok_or_else(|| anyhow::anyhow!("Range content length overflow"))?;

    let cache = cache.clone();
    let url = url.to_string();
    let provider_headers = provider_headers.clone();
    let request_control = request_control.cloned();
    let first_slice = Some(first_slice);
    let stream = futures::stream::try_unfold(
        (first_slice_index, first_slice),
        move |(idx, mut first_slice)| {
            let cache = cache.clone();
            let url = url.clone();
            let provider_headers = provider_headers.clone();
            let request_control = request_control.clone();
            async move {
                if idx > last_slice_index {
                    return Ok::<Option<(Bytes, _)>, io::Error>(None);
                }
                check_stream_active(request_control.as_ref())?;
                let next_idx = idx.saturating_add(1);
                let slice_result = if let Some(slice) = first_slice.take() {
                    Ok(slice)
                } else {
                    match cache
                        .get_or_fetch_slice_or_bypass_with_control(
                            &url,
                            &provider_headers,
                            idx,
                            Some(total_size),
                            request_control.as_ref(),
                            upstream_header_timeout,
                        )
                        .await
                    {
                        Ok(SliceFetchResult::Slice(slice)) => Ok(slice),
                        Ok(SliceFetchResult::Bypass(resp)) => Err(io::Error::other(format!(
                            "Upstream returned {} while streaming cached range",
                            resp.status()
                        ))),
                        Err(error) => Err(io::Error::other(format!(
                            "Slice cache fetch failed: {error}"
                        ))),
                    }
                };

                let chunk = slice_result.and_then(|fetched| {
                    let slice_start = idx
                        .checked_mul(cache.config().slice_size as u64)
                        .ok_or_else(|| io::Error::other("Slice start overflow"))?;
                    crop_slice_for_range(&fetched.slice.data, slice_start, range_start, range_end)
                });

                chunk.map(|chunk| Some((chunk, (next_idx, first_slice))))
            }
        },
    );

    let mut builder = Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(
            "Content-Range",
            format!("bytes {range_start}-{range_end}/{total_size}"),
        )
        .header("Content-Length", content_length.to_string())
        .header("Accept-Ranges", "bytes")
        .header("X-Cache-Status", first_status.as_str());
    builder = apply_cached_slice_response_headers(builder, &response_header_slice);

    builder
        .body(Body::from_stream(stream))
        .map_err(|e| anyhow::anyhow!("Failed to build cached response: {e}"))
}

// No-Range slice path

/// Handle a non-range client request by actively using range requests against
/// the origin. If the origin does not support byte ranges, the request is
/// streamed through without caching.
async fn no_range_slice_cache_path(
    cache: &SliceCache,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
    upstream_header_timeout: Option<Duration>,
) -> Result<Response, anyhow::Error> {
    let first_slice = match cache
        .get_or_fetch_slice_or_bypass_with_control(
            url,
            provider_headers,
            0,
            None,
            request_control,
            upstream_header_timeout,
        )
        .await?
    {
        SliceFetchResult::Slice(slice) => slice,
        SliceFetchResult::Bypass(resp) => {
            if !resp.status().is_success() {
                let _ = run_with_proxy_cancellation(
                    "no-range slice probe rejection drain",
                    request_control,
                    resp.bytes(),
                )
                .await;
                return stream_through_with_status(StreamThroughRequest {
                    client: cache.client(),
                    ssrf_guard: cache.ssrf_guard(),
                    url,
                    provider_headers,
                    range_header: None,
                    cache_status: CacheStatus::Bypass,
                    request_control,
                    upstream_header_timeout,
                })
                .await;
            }
            return stream_existing_response_with_status(resp, CacheStatus::Bypass);
        }
    };

    let total_size = first_slice.slice.total_size;
    let slice_size = cache.config().slice_size as u64;
    let total_slices = total_size.div_ceil(slice_size);

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Length", total_size.to_string())
        .header("Accept-Ranges", "bytes")
        .header("X-Cache-Status", first_slice.status.as_str());
    builder = apply_cached_slice_response_headers(builder, &first_slice.slice);

    let cache = cache.clone();
    let url = url.to_string();
    let provider_headers = provider_headers.clone();
    let request_control = request_control.cloned();
    let stream = full_resource_slice_stream(FullResourceStream {
        cache,
        url,
        provider_headers,
        total_size,
        total_slices,
        first_chunk: first_slice.slice.data,
        request_control,
        upstream_header_timeout,
    });

    builder
        .body(Body::from_stream(stream))
        .map_err(|e| anyhow::anyhow!("Failed to build no-range slice response: {e}"))
}

fn apply_cached_slice_response_headers(
    mut builder: axum::http::response::Builder,
    slice: &CachedSlice,
) -> axum::http::response::Builder {
    if let Some(ref ct) = slice.content_type {
        builder = builder.header("Content-Type", ct.as_str());
    }
    if let Some(ref etag) = slice.etag {
        builder = builder.header("ETag", etag.as_str());
    }
    if let Some(ref last_modified) = slice.last_modified {
        builder = builder.header("Last-Modified", last_modified.as_str());
    }
    builder
}
