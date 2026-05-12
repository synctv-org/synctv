//! Filter entry points: proxy_with_cache, stream_through_with_status,
//! head_content_length.
//!
//! These correspond to nginx's header/body filter chain -- the top-level
//! request handling that decides whether to use slice caching or passthrough.

use std::collections::HashMap;
use std::io;

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use bytes::Bytes;
use futures::StreamExt;
use synctv_common::ExecutionControl;

use crate::{
    apply_provider_headers, run_with_proxy_cancellation,
    send_head_with_redirect_validation_with_control, send_with_redirect_validation_with_control,
    ProxyError,
};

use super::etag::CachedResourceMeta;
use super::range::parse_content_range;
use super::status::CacheStatus;
use super::store::{HeadResourceResult, SliceCache, SliceFetchResult};

// HEAD helper

fn parse_content_length_header(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
}

#[derive(Clone, Copy)]
enum ClientRangePlan {
    Explicit { start: u64, end: u64 },
    OpenEnded { start: u64 },
    Suffix { suffix_len: u64 },
}

fn parse_client_range_plan(range: &str) -> Result<ClientRangePlan, ProxyError> {
    let range = range.trim();
    if !range.starts_with("bytes=") {
        return Err(ProxyError::InvalidRequest(
            "Invalid range format: must start with 'bytes='".to_string(),
        ));
    }

    let spec = &range["bytes=".len()..];
    if spec.contains(',') {
        return Err(ProxyError::InvalidRequest(
            "Multi-range requests are not supported".to_string(),
        ));
    }

    let Some((start_text, end_text)) = spec.split_once('-') else {
        return Err(ProxyError::InvalidRequest(
            "Invalid range format".to_string(),
        ));
    };

    if start_text.is_empty() {
        if let Ok(suffix_len) = end_text.parse::<u64>() {
            if suffix_len == 0 {
                return Err(ProxyError::InvalidRequest(
                    "Invalid suffix range".to_string(),
                ));
            }
            return Ok(ClientRangePlan::Suffix { suffix_len });
        }
        return Err(ProxyError::InvalidRequest(
            "Invalid suffix range".to_string(),
        ));
    }

    let start = start_text
        .parse::<u64>()
        .map_err(|_| ProxyError::InvalidRequest("Invalid range start".to_string()))?;

    if end_text.is_empty() {
        return Ok(ClientRangePlan::OpenEnded { start });
    }

    let end = end_text
        .parse::<u64>()
        .map_err(|_| ProxyError::InvalidRequest("Invalid range end".to_string()))?;
    if start > end {
        return Err(ProxyError::InvalidRequest(
            "Range start must not exceed range end".to_string(),
        ));
    }

    Ok(ClientRangePlan::Explicit { start, end })
}

fn range_bounds_for_total(
    plan: ClientRangePlan,
    total_size: u64,
) -> Result<(u64, u64), ProxyError> {
    match plan {
        ClientRangePlan::Explicit { start, mut end } => {
            if start >= total_size {
                return Err(ProxyError::InvalidRequest(
                    "Range start beyond total size".to_string(),
                ));
            }
            if end >= total_size {
                end = total_size - 1;
            }
            Ok((start, end))
        }
        ClientRangePlan::OpenEnded { start } => {
            if start >= total_size {
                return Err(ProxyError::InvalidRequest(
                    "Range start beyond total size".to_string(),
                ));
            }
            Ok((start, total_size - 1))
        }
        ClientRangePlan::Suffix { suffix_len } => {
            if suffix_len == 0 || suffix_len > total_size {
                return Err(ProxyError::InvalidRequest(
                    "Suffix range out of bounds".to_string(),
                ));
            }
            Ok((total_size - suffix_len, total_size - 1))
        }
    }
}

fn slice_index_for_byte(byte: u64, slice_size: usize) -> u64 {
    byte / slice_size as u64
}

fn parse_total_size_from_content_range(
    resp: &reqwest::Response,
) -> Result<Option<u64>, anyhow::Error> {
    let Some(value) = resp.headers().get("content-range") else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|e| anyhow::anyhow!("Invalid Content-Range header in fallback response: {e}"))?;
    let parsed = parse_content_range(value)?;
    Ok(parsed.complete_length)
}

fn check_stream_active(request_control: Option<&ExecutionControl>) -> Result<(), io::Error> {
    if let Some(control) = request_control {
        control
            .check_cancelled()
            .map_err(|error| io::Error::other(format!("Proxy stream stopped: {error}")))?;
    }
    Ok(())
}

async fn discover_content_length_via_range_get(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
) -> Result<u64, anyhow::Error> {
    let mut request = client.get(url);
    request = apply_provider_headers(request, url, provider_headers)?;
    request = request.header("Range", "bytes=0-0");
    let resp =
        send_with_redirect_validation_with_control(client, request, ssrf_guard, request_control)
            .await
            .map_err(|e| anyhow::anyhow!("Range GET fallback failed: {e}"))?
            .response;

    match resp.status() {
        StatusCode::PARTIAL_CONTENT => {
            parse_total_size_from_content_range(&resp)?.ok_or_else(|| {
                anyhow::anyhow!("Missing complete length in Content-Range fallback response")
            })
        }
        StatusCode::OK => parse_content_length_header(&resp).ok_or_else(|| {
            anyhow::anyhow!("Missing or invalid Content-Length in fallback GET response")
        }),
        status => Err(anyhow::anyhow!(
            "Range GET fallback returned status {status}"
        )),
    }
}

async fn stream_original_range_with_learned_meta(
    cache: &SliceCache,
    url: &str,
    provider_headers: &HashMap<String, String>,
    range_str: &str,
    request_control: Option<&ExecutionControl>,
) -> Result<Response, anyhow::Error> {
    let mut request = cache.client().get(url);
    request = apply_provider_headers(request, url, provider_headers)?;
    request = request.header("Range", range_str);

    let resp = send_with_redirect_validation_with_control(
        cache.client(),
        request,
        cache.ssrf_guard(),
        request_control,
    )
    .await
    .map_err(|e| anyhow::anyhow!("Upstream range request failed: {e}"))?
    .response;

    if resp.status() == StatusCode::PARTIAL_CONTENT {
        if let Some(total_size) = parse_total_size_from_content_range(&resp)? {
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
    head_content_length_with_control(client, ssrf_guard, url, provider_headers, None).await
}

#[allow(clippy::implicit_hasher)]
pub async fn head_content_length_with_control(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
) -> Result<u64, anyhow::Error> {
    let mut request = client.head(url);
    request = apply_provider_headers(request, url, provider_headers)?;
    let resp = send_head_with_redirect_validation_with_control(
        client,
        request,
        ssrf_guard,
        request_control,
    )
    .await
    .map_err(|e| anyhow::anyhow!("HEAD request failed: {e}"))?
    .response;

    if !resp.status().is_success() {
        return discover_content_length_via_range_get(
            client,
            ssrf_guard,
            url,
            provider_headers,
            request_control,
        )
        .await;
    }

    if let Some(content_length) = parse_content_length_header(&resp) {
        return Ok(content_length);
    }

    discover_content_length_via_range_get(
        client,
        ssrf_guard,
        url,
        provider_headers,
        request_control,
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
/// - **Multi-Range**: rejected with an error.
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
    proxy_with_cache_enabled_with_control(
        cache,
        cache.config().enabled,
        range_header,
        url,
        provider_headers,
        request_control,
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
    if !cache_enabled {
        return stream_through_with_status(
            cache.client(),
            cache.ssrf_guard(),
            url,
            provider_headers,
            range_header,
            CacheStatus::Bypass,
            request_control,
        )
        .await;
    }

    if range_header.is_none() {
        return no_range_slice_cache_path(cache, url, provider_headers, request_control).await;
    }

    let Some(range_str) = range_header else {
        unreachable!("range_header.is_none() was checked above");
    };

    let plan = parse_client_range_plan(range_str)?;
    let known_total_size = cache
        .get_resource_meta(url, provider_headers)
        .await
        .and_then(|meta| meta.total_size);

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
                )
                .await;
            }
        }
    }

    range_slice_cache_path(
        cache,
        range_str,
        url,
        provider_headers,
        request_control,
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
    if !cache_enabled {
        return stream_head_through_with_status(
            cache.client(),
            cache.ssrf_guard(),
            url,
            provider_headers,
            range_header,
            CacheStatus::Bypass,
            request_control,
        )
        .await;
    }

    let result = cache
        .get_or_fetch_head_resource_with_control(
            url,
            provider_headers,
            range_header,
            request_control,
        )
        .await?;
    build_head_cache_response(&result)
}

fn build_head_cache_response(result: &HeadResourceResult) -> Result<Response, anyhow::Error> {
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
                )
                .await;
            };
            let (start, _) = range_bounds_for_total(plan, total_size)?;
            start
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
    let (range_start, range_end) = range_bounds_for_total(plan, total_size)?;
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

                    #[allow(clippy::cast_possible_truncation)]
                    let offset_start = if range_start > slice_start {
                        (range_start - slice_start) as usize
                    } else {
                        0
                    };

                    let slice_len = fetched.slice.data.len();
                    #[allow(clippy::cast_possible_truncation)]
                    let offset_end = if range_end < slice_start + slice_len as u64 {
                        (range_end - slice_start) as usize + 1
                    } else {
                        slice_len
                    };

                    if offset_start > offset_end || offset_end > slice_len {
                        return Err(io::Error::other("Invalid slice crop range"));
                    }

                    Ok(fetched.slice.data.slice(offset_start..offset_end))
                });

                chunk.map(|chunk| Some((chunk, (next_idx, first_slice))))
            }
        },
    );

    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(
            "Content-Range",
            format!("bytes {range_start}-{range_end}/{total_size}"),
        )
        .header("Content-Length", content_length.to_string())
        .header("Accept-Ranges", "bytes")
        .header("X-Cache-Status", first_status.as_str())
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
) -> Result<Response, anyhow::Error> {
    let first_slice = match cache
        .get_or_fetch_slice_or_bypass_with_control(url, provider_headers, 0, None, request_control)
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
                return stream_through_with_status(
                    cache.client(),
                    cache.ssrf_guard(),
                    url,
                    provider_headers,
                    None,
                    CacheStatus::Bypass,
                    request_control,
                )
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
    if let Some(ref ct) = first_slice.slice.content_type {
        builder = builder.header("Content-Type", ct.as_str());
    }
    if let Some(ref etag) = first_slice.slice.etag {
        builder = builder.header("ETag", etag.as_str());
    }
    if let Some(ref last_modified) = first_slice.slice.last_modified {
        builder = builder.header("Last-Modified", last_modified.as_str());
    }

    let cache = cache.clone();
    let url = url.to_string();
    let provider_headers = provider_headers.clone();
    let request_control = request_control.cloned();
    let first_chunk = Some(first_slice.slice.data);
    let stream = futures::stream::try_unfold(
        (0_u64, first_chunk),
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
    );

    builder
        .body(Body::from_stream(stream))
        .map_err(|e| anyhow::anyhow!("Failed to build no-range slice response: {e}"))
}

// Stream-through helper

fn stream_existing_response_with_status(
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

    for name in &[
        "content-length",
        "content-type",
        "content-range",
        "accept-ranges",
    ] {
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

/// Stream an upstream response through without caching, attaching the given
/// `X-Cache-Status` header.
pub(super) async fn stream_through_with_status(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &HashMap<String, String>,
    range_header: Option<&str>,
    cache_status: CacheStatus,
    request_control: Option<&ExecutionControl>,
) -> Result<Response, anyhow::Error> {
    let mut request = client.get(url);
    request = apply_provider_headers(request, url, provider_headers)?;

    if let Some(range) = range_header {
        request = request.header("Range", range);
    }

    let resp =
        send_with_redirect_validation_with_control(client, request, ssrf_guard, request_control)
            .await
            .map_err(|e| anyhow::anyhow!("Upstream request failed: {e}"))?
            .response;

    stream_existing_response_with_status(resp, cache_status)
}

async fn stream_head_through_with_status(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &HashMap<String, String>,
    range_header: Option<&str>,
    cache_status: CacheStatus,
    request_control: Option<&ExecutionControl>,
) -> Result<Response, anyhow::Error> {
    let mut request = client.head(url);
    request = apply_provider_headers(request, url, provider_headers)?;

    if let Some(range) = range_header {
        request = request.header("Range", range);
    }

    let resp = send_head_with_redirect_validation_with_control(
        client,
        request,
        ssrf_guard,
        request_control,
    )
    .await
    .map_err(|e| anyhow::anyhow!("Upstream HEAD request failed: {e}"))?
    .response;

    build_head_cache_response(&HeadResourceResult {
        status: resp.status(),
        headers: resp.headers().clone(),
        cache_status,
    })
}
