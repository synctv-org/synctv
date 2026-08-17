#![allow(clippy::missing_errors_doc, clippy::too_many_lines)]

//! Filter entry points: [`SliceCache::proxy`], stream-through, and HEAD
//! content-length lookup.
//!
//! These correspond to nginx's header/body filter chain -- the top-level
//! request handling that decides whether to use slice caching or passthrough.

use std::io;
use std::time::Duration;

use anyhow::Context as _;
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use synctv_common::ExecutionControl;

use crate::{
    apply_provider_headers, send_with_redirect_validation_with_control_and_timeout,
    ProviderHeaders, ProxyError,
};

use super::etag::CachedResourceMeta;
use super::head;
use super::passthrough::{
    stream_existing_response_with_status, stream_head_through_with_status,
    stream_through_with_status, StreamThroughRequest,
};
use super::range::{
    format_content_range, parse_client_range_plan, range_bounds_for_total, slice_index_for_byte,
    ClientRangeError, ClientRangePlan,
};
use super::status::CacheStatus;
use super::store::SliceCache;
use super::types::{
    CachedFullResponse, CachedSlice, FullResponseFetchResult, HeadResourceResult, SliceFetchResult,
};

/// Decides whether a failed range probe may be retried as an ordinary GET.
///
/// A `200 OK` response to a range probe already provides a valid complete
/// response and is streamed directly. Error responses have no interoperable
/// meaning for range support, so callers opt into a second request only when
/// they recognize an origin-specific response shape.
pub trait SliceRangeProbeFallback: Send + Sync {
    fn retry_without_range(&self, status: StatusCode, headers: &HeaderMap) -> bool;
}

impl<F> SliceRangeProbeFallback for F
where
    F: Fn(StatusCode, &HeaderMap) -> bool + Send + Sync,
{
    fn retry_without_range(&self, status: StatusCode, headers: &HeaderMap) -> bool {
        self(status, headers)
    }
}

/// HTTP method executed through the shared proxy cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceCacheProxyMethod {
    Get,
    Head,
}

/// Cache behavior selected for a proxied resource.
pub enum SliceCacheProxyStrategy<'a> {
    /// Fixed-size byte-range cache for seekable media.
    Slice {
        /// Optional origin-specific handling for failed range probes.
        range_probe_fallback: Option<&'a dyn SliceRangeProbeFallback>,
    },
    /// Bounded complete-response cache for documents that use ordinary GET.
    FullResponse,
    /// Forward an upstream response without cache storage.
    Stream,
}

/// Complete execution parameters for a request served through [`SliceCache`].
pub struct SliceCacheProxyRequest<'a> {
    pub method: SliceCacheProxyMethod,
    pub strategy: SliceCacheProxyStrategy<'a>,
    pub cache_enabled: bool,
    pub url: &'a str,
    pub provider_headers: &'a ProviderHeaders,
    pub range_header: Option<&'a str>,
    pub request_control: Option<&'a ExecutionControl>,
    pub upstream_header_timeout: Option<Duration>,
}

#[derive(Clone, Copy)]
struct ProxyRequestContext<'a> {
    cache_enabled: bool,
    url: &'a str,
    provider_headers: &'a ProviderHeaders,
    range_header: Option<&'a str>,
    request_control: Option<&'a ExecutionControl>,
    upstream_header_timeout: Option<Duration>,
}

impl SliceCache {
    /// Execute one proxy request with its cache strategy and request controls.
    pub async fn proxy(
        &self,
        request: SliceCacheProxyRequest<'_>,
    ) -> Result<Response, anyhow::Error> {
        let SliceCacheProxyRequest {
            method,
            strategy,
            cache_enabled,
            url,
            provider_headers,
            range_header,
            request_control,
            upstream_header_timeout,
        } = request;
        let context = ProxyRequestContext {
            cache_enabled,
            url,
            provider_headers,
            range_header,
            request_control,
            upstream_header_timeout,
        };

        match (method, strategy) {
            (
                SliceCacheProxyMethod::Get,
                SliceCacheProxyStrategy::Slice {
                    range_probe_fallback,
                },
            ) => proxy_slice_cache(self, context, range_probe_fallback).await,
            (SliceCacheProxyMethod::Get, SliceCacheProxyStrategy::FullResponse) => {
                proxy_full_response_cache(self, context).await
            }
            (SliceCacheProxyMethod::Head, SliceCacheProxyStrategy::Slice { .. }) => {
                proxy_head_slice_cache(self, context).await
            }
            (
                SliceCacheProxyMethod::Head,
                SliceCacheProxyStrategy::FullResponse | SliceCacheProxyStrategy::Stream,
            ) => {
                stream_head_through_with_status(StreamThroughRequest {
                    client: self.client(),
                    ssrf_guard: self.ssrf_guard(),
                    url: context.url,
                    provider_headers: context.provider_headers,
                    range_header: context.range_header,
                    cache_status: CacheStatus::Bypass,
                    request_control: context.request_control,
                    upstream_header_timeout: context.upstream_header_timeout,
                })
                .await
            }
            (SliceCacheProxyMethod::Get, SliceCacheProxyStrategy::Stream) => {
                stream_through_with_status(StreamThroughRequest {
                    client: self.client(),
                    ssrf_guard: self.ssrf_guard(),
                    url: context.url,
                    provider_headers: context.provider_headers,
                    range_header: context.range_header,
                    cache_status: CacheStatus::Bypass,
                    request_control: context.request_control,
                    upstream_header_timeout: context.upstream_header_timeout,
                })
                .await
            }
        }
    }
}

struct RangeSliceRequest<'a> {
    cache: &'a SliceCache,
    range_str: &'a str,
    url: &'a str,
    provider_headers: &'a ProviderHeaders,
    request_control: Option<&'a ExecutionControl>,
    upstream_header_timeout: Option<Duration>,
    plan: ClientRangePlan,
    known_total_size: Option<u64>,
}

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
    provider_headers: ProviderHeaders,
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
    let offset_start = if range_start > slice_start {
        usize::try_from(range_start - slice_start)
            .map_err(|_| io::Error::other("Slice crop start exceeds platform usize"))?
    } else {
        0
    };

    let slice_len = data.len();
    let slice_end = slice_start
        .checked_add(slice_len as u64)
        .ok_or_else(|| io::Error::other("Slice end overflow"))?;
    let offset_end = if range_end < slice_end {
        usize::try_from(range_end - slice_start)
            .map_err(|_| io::Error::other("Slice crop end exceeds platform usize"))?
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Slice crop end overflow"))?
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
    provider_headers: &ProviderHeaders,
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
                    status: Some(resp.status().as_u16()),
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
                    content_encoding: resp
                        .headers()
                        .get("content-encoding")
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
pub async fn head_content_length(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &ProviderHeaders,
) -> Result<u64, anyhow::Error> {
    head::head_content_length(client, ssrf_guard, url, provider_headers).await
}

pub async fn head_content_length_with_control(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &ProviderHeaders,
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

pub async fn head_content_length_with_control_and_timeout(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &ProviderHeaders,
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

/// Serve a request through the slice cache with an optional origin-specific
/// fallback from a failed range probe to an ordinary GET.
///
/// The default API has no fallback because status codes such as `416` and
/// `503` do not identify range support. A caller that owns a known upstream
/// may provide a callback after inspecting the status and response headers.
async fn proxy_slice_cache(
    cache: &SliceCache,
    request: ProxyRequestContext<'_>,
    range_probe_fallback: Option<&dyn SliceRangeProbeFallback>,
) -> Result<Response, anyhow::Error> {
    let ProxyRequestContext {
        cache_enabled,
        url,
        provider_headers,
        range_header,
        request_control,
        upstream_header_timeout,
    } = request;
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
            range_probe_fallback,
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

    // A suffix range (`bytes=-N`) cannot be mapped onto slice indices until the
    // total size is known, so stream it through while learning the metadata.
    // Explicit/open-ended ranges proceed straight to the slice path. MultiRange
    // was already handled by the bypass above.
    if matches!(plan, ClientRangePlan::Suffix { .. }) && known_total_size.is_none() {
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

    range_slice_cache_path(RangeSliceRequest {
        cache,
        range_str,
        url,
        provider_headers,
        request_control,
        upstream_header_timeout,
        plan,
        known_total_size,
    })
    .await
}

/// Serve an ordinary GET through the shared full-response cache.
///
/// This mode never sends a Range request upstream. It is intended for small,
/// complete documents such as subtitles and static danmaku. A cache fill holds
/// one per-resource lock; oversized responses bypass the cache and continue as
/// a normal stream.
async fn proxy_full_response_cache(
    cache: &SliceCache,
    request: ProxyRequestContext<'_>,
) -> Result<Response, anyhow::Error> {
    let ProxyRequestContext {
        cache_enabled,
        url,
        provider_headers,
        range_header,
        request_control,
        upstream_header_timeout,
    } = request;
    if !cache_enabled || range_header.is_some() {
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

    match cache
        .get_or_fetch_full_response_with_control(
            url,
            provider_headers,
            request_control,
            upstream_header_timeout,
        )
        .await?
    {
        FullResponseFetchResult::Cached(response) => cached_full_response(response),
        FullResponseFetchResult::Bypass(response) => {
            stream_existing_response_with_status(response, CacheStatus::Bypass)
        }
    }
}

fn cached_full_response(response: CachedFullResponse) -> Result<Response, anyhow::Error> {
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Length", response.data.len().to_string())
        .header("X-Cache-Status", response.status.as_str())
        .header("Cache-Control", "no-cache")
        .header("Pragma", "no-cache")
        .header("X-Content-Type-Options", "nosniff");
    if let Some(content_type) = response.content_type {
        builder = builder.header("Content-Type", content_type);
    }
    if let Some(content_encoding) = response.content_encoding {
        builder = builder.header("Content-Encoding", content_encoding);
    }
    if let Some(etag) = response.etag {
        builder = builder.header("ETag", etag);
    }
    if let Some(last_modified) = response.last_modified {
        builder = builder.header("Last-Modified", last_modified);
    }
    builder
        .body(Body::from(response.data))
        .map_err(|error| anyhow::anyhow!("Failed to build cached full response: {error}"))
}

async fn proxy_head_slice_cache(
    cache: &SliceCache,
    request: ProxyRequestContext<'_>,
) -> Result<Response, anyhow::Error> {
    let ProxyRequestContext {
        cache_enabled,
        url,
        provider_headers,
        range_header,
        request_control,
        upstream_header_timeout,
    } = request;
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
        if crate::is_hop_by_hop_header(name.as_str()) {
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

async fn range_slice_cache_path(request: RangeSliceRequest<'_>) -> Result<Response, anyhow::Error> {
    let RangeSliceRequest {
        cache,
        range_str,
        url,
        provider_headers,
        request_control,
        upstream_header_timeout,
        plan,
        known_total_size,
    } = request;

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
        // MultiRange is bypassed before this function
        // is ever reached, so it can never appear here.
        ClientRangePlan::MultiRange => {
            unreachable!("MultiRange is bypassed before reaching the slice path")
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
        content_encoding: first_slice.slice.content_encoding.clone(),
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
            format_content_range(range_start, range_end, total_size),
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
    provider_headers: &ProviderHeaders,
    request_control: Option<&ExecutionControl>,
    upstream_header_timeout: Option<Duration>,
    range_probe_fallback: Option<&dyn SliceRangeProbeFallback>,
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
            if !resp.status().is_success()
                && range_probe_fallback.is_some_and(|fallback| {
                    fallback.retry_without_range(resp.status(), resp.headers())
                })
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
    if let Some(ref encoding) = slice.content_encoding {
        builder = builder.header("Content-Encoding", encoding.as_str());
    }
    if let Some(ref etag) = slice.etag {
        builder = builder.header("ETag", etag.as_str());
    }
    if let Some(ref last_modified) = slice.last_modified {
        builder = builder.header("Last-Modified", last_modified.as_str());
    }
    builder
}
