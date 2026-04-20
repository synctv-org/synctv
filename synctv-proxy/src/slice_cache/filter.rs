//! Filter entry points: proxy_with_cache, full_body_cache_path,
//! stream_through_with_status, head_content_length.
//!
//! These correspond to nginx's header/body filter chain -- the top-level
//! request handling that decides whether to use slice caching, full-body
//! caching, or passthrough.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use bytes::Bytes;
use futures::StreamExt;
use synctv_common::ExecutionControl;

use crate::{
    apply_provider_headers, run_with_proxy_cancellation,
    send_head_with_redirect_validation_with_control, send_with_redirect_validation,
    send_with_redirect_validation_with_control,
};

use super::config::is_manifest_content_type;
use super::etag::CachedResourceMeta;
use super::range::{
    aligned_range_for_slice, compute_needed_slices, parse_content_range, parse_range_header,
};
use super::status::CacheStatus;
use super::store::{FullBodyWrite, SliceCache};

const MAX_BUFFERED_SLICES: usize = 8;

// HEAD helper

fn parse_content_length_header(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
}

struct FullBodyUpdateGuard<'a> {
    cache: &'a SliceCache,
    key: String,
}

impl<'a> FullBodyUpdateGuard<'a> {
    fn new(cache: &'a SliceCache, url: &str, provider_headers: &HashMap<String, String>) -> Self {
        Self {
            cache,
            key: SliceCache::full_body_key(url, provider_headers),
        }
    }
}

impl Drop for FullBodyUpdateGuard<'_> {
    fn drop(&mut self) {
        self.cache.finish_full_body_update(&self.key);
    }
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

async fn discover_content_length_via_range_get(
    client: &reqwest::Client,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
) -> Result<u64, anyhow::Error> {
    let mut request = client.get(url);
    request = apply_provider_headers(request, url, provider_headers)?;
    request = request.header("Range", "bytes=0-0");
    let resp = send_with_redirect_validation_with_control(client, request, request_control)
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

/// Send a HEAD request to discover the upstream `Content-Length`.
///
/// Falls back to a constrained `GET Range: bytes=0-0` request when the origin
/// rejects HEAD or omits `Content-Length`, while still reusing the proxy's
/// SSRF-safe redirect validation path.
#[allow(clippy::implicit_hasher)]
pub async fn head_content_length(
    client: &reqwest::Client,
    url: &str,
    provider_headers: &HashMap<String, String>,
) -> Result<u64, anyhow::Error> {
    head_content_length_with_control(client, url, provider_headers, None).await
}

#[allow(clippy::implicit_hasher)]
pub async fn head_content_length_with_control(
    client: &reqwest::Client,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
) -> Result<u64, anyhow::Error> {
    let mut request = client.head(url);
    request = apply_provider_headers(request, url, provider_headers)?;
    let resp = send_head_with_redirect_validation_with_control(client, request, request_control)
        .await
        .map_err(|e| anyhow::anyhow!("HEAD request failed: {e}"))?
        .response;

    if !resp.status().is_success() {
        return discover_content_length_via_range_get(
            client,
            url,
            provider_headers,
            request_control,
        )
        .await;
    }

    if let Some(content_length) = parse_content_length_header(&resp) {
        return Ok(content_length);
    }

    discover_content_length_via_range_get(client, url, provider_headers, request_control).await
}

// proxy_with_cache  --  main entry point

/// Serve a request through the slice cache.
///
/// Behaviour:
/// - **Disabled cache**: streams through with `X-Cache-Status: BYPASS`.
/// - **No Range header**: full-body cache path.  Bodies up to
///   `max_cacheable_body` are cached; oversized ones are streamed with
///   `BYPASS`.
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
            url,
            provider_headers,
            range_header,
            CacheStatus::Bypass,
            request_control,
        )
        .await;
    }

    if range_header.is_none() {
        return full_body_cache_path(cache, url, provider_headers, request_control).await;
    }

    let Some(range_str) = range_header else {
        unreachable!("range_header.is_none() was checked above");
    };

    // Total size needed for range parsing.
    // Reuse cached metadata when available to avoid a HEAD request on every
    // range request, even when the slice data is already cached (L4 fix).
    let total_size = match cache.get_resource_meta(url, provider_headers).await {
        Some(meta) if meta.total_size.is_some() => {
            let Some(size) = meta.total_size else {
                unreachable!("meta.total_size.is_some() was checked above");
            };
            size
        }
        _ => {
            head_content_length_with_control(cache.client(), url, provider_headers, request_control)
                .await?
        }
    };

    let (range_start, range_end) = parse_range_header(range_str, total_size)?;

    let needed = compute_needed_slices(range_start, range_end, cache.config().slice_size);

    // Determine cache status *before* fetching.
    let pre_status = cache
        .determine_slice_cache_status(url, provider_headers, &needed)
        .await;

    // For large range requests spanning many slices, stream directly from
    // upstream to avoid buffering all slice data in memory.
    if needed.len() > MAX_BUFFERED_SLICES {
        return stream_through_with_status(
            cache.client(),
            url,
            provider_headers,
            range_header,
            CacheStatus::Bypass,
            request_control,
        )
        .await;
    }

    // Fetch all needed slices, tracking the aggregate cache status.
    // For typical requests spanning 1-8 slices (up to 16 MiB with default
    // 2 MiB slice size), buffering is acceptable.
    let mut combined = Vec::new();
    let mut worst_status = CacheStatus::Hit; // Start optimistic.

    for &idx in &needed {
        let (slice_data, slice_status) = cache
            .get_or_fetch_slice_with_control(
                url,
                provider_headers,
                idx,
                total_size,
                request_control,
            )
            .await?;

        // Merge slice status: the "worst" status wins.
        worst_status = merge_cache_status(worst_status, slice_status);

        let (slice_start, _) = aligned_range_for_slice(idx, cache.config().slice_size, total_size)?;

        #[allow(clippy::cast_possible_truncation)]
        let offset_start = if range_start > slice_start {
            (range_start - slice_start) as usize
        } else {
            0
        };

        let slice_len = slice_data.len();
        #[allow(clippy::cast_possible_truncation)]
        let offset_end = if range_end < slice_start + slice_len as u64 {
            (range_end - slice_start) as usize + 1
        } else {
            slice_len
        };

        combined.extend_from_slice(&slice_data[offset_start..offset_end]);
    }

    // Use the pre-status if all slices were already cached (HIT), otherwise
    // use the merged status from actual fetches.  The pre_status captures
    // the EXPIRED distinction that get_or_fetch_slice won't return (it
    // returns MISS after re-fetching an expired entry).
    let final_status = if worst_status == CacheStatus::Hit {
        // All slices hit -- trust the pre-check.
        pre_status
    } else if worst_status == CacheStatus::Miss
        && (pre_status == CacheStatus::Expired || pre_status == CacheStatus::Stale)
    {
        // Pre-check saw EXPIRED or STALE (was-seen), slices re-fetched ->
        // report the pre-check status since it captures the "was cached
        // before" distinction.
        pre_status
    } else {
        worst_status
    };

    let content_length = combined.len();

    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(
            "Content-Range",
            format!("bytes {range_start}-{range_end}/{total_size}"),
        )
        .header("Content-Length", content_length.to_string())
        .header("Accept-Ranges", "bytes")
        .header("X-Cache-Status", final_status.as_str())
        .body(Body::from(Bytes::from(combined)))
        .map_err(|e| anyhow::anyhow!("Failed to build cached response: {e}"))
}

/// Merge two cache statuses, returning the "worse" one.
///
/// Priority (worst to best): Miss > Expired > Stale > Updating >
/// Revalidated > Hit.  Bypass is kept if either operand is Bypass.
const fn merge_cache_status(a: CacheStatus, b: CacheStatus) -> CacheStatus {
    const fn priority(s: CacheStatus) -> u8 {
        match s {
            CacheStatus::Hit => 0,
            CacheStatus::Revalidated => 1,
            CacheStatus::Updating => 2,
            CacheStatus::Stale => 3,
            CacheStatus::Expired => 4,
            CacheStatus::Miss => 5,
            CacheStatus::Bypass => 6,
        }
    }
    if priority(a) >= priority(b) {
        a
    } else {
        b
    }
}

// Full-body cache path

/// Handle a non-range request through the full-body cache.
pub(super) async fn full_body_cache_path(
    cache: &SliceCache,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
) -> Result<Response, anyhow::Error> {
    // Check cache first.
    if let Some((data, content_type, status)) = cache.get_full_body(url, provider_headers).await {
        // Spawn background revalidation for stale entries so the next
        // request gets a fresh copy without waiting.
        if status == CacheStatus::Stale {
            let bg_cache = cache.clone();
            let bg_url = url.to_string();
            let bg_headers = provider_headers.clone();
            let bg_meta = cache.get_resource_meta(url, provider_headers).await;
            tokio::spawn(async move {
                if let Err(error) =
                    revalidate_stale_full_body_entry(&bg_cache, &bg_url, &bg_headers, bg_meta).await
                {
                    tracing::debug!(
                        url = %bg_url,
                        error = %error,
                        "Background full-body revalidation failed to update cache"
                    );
                }
            });
        }

        let mut builder = Response::builder()
            .status(StatusCode::OK)
            .header("Content-Length", data.len().to_string())
            .header("X-Cache-Status", status.as_str());
        if let Some(ct) = content_type {
            builder = builder.header("Content-Type", ct);
        }
        return builder
            .body(Body::from(data))
            .map_err(|e| anyhow::anyhow!("Failed to build cached full-body response: {e}"));
    }

    // Determine pre-fetch status (MISS vs EXPIRED).
    let pre_status = cache.full_body_pre_status(url, provider_headers).await;

    // Fetch from upstream, with conditional headers if we have metadata.
    let mut request = cache.client().get(url);
    request = apply_provider_headers(request, url, provider_headers)?;

    // Add conditional request headers from stored metadata to enable 304
    // responses and avoid re-downloading unchanged resources.
    let existing_meta = cache.get_resource_meta(url, provider_headers).await;
    if let Some(meta) = existing_meta.as_ref() {
        if let Some(ref etag) = meta.etag {
            request = request.header("If-None-Match", etag.as_str());
        }
        if let Some(ref lm) = meta.last_modified {
            request = request.header("If-Modified-Since", lm.as_str());
        }
    }

    let resp = send_with_redirect_validation_with_control(cache.client(), request, request_control)
        .await
        .map_err(|e| anyhow::anyhow!("Upstream request failed: {e}"))?
        .response;

    // Handle 304 Not Modified: upstream confirmed the cached copy is still
    // valid.  Re-insert the full body with a fresh TTL and serve it.
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        // Consume the empty body to release the connection.
        let _ =
            run_with_proxy_cancellation("full-body cache 304 drain", request_control, resp.bytes())
                .await;

        // Re-fetch from our cache (it may have been evicted in the meantime).
        if let Some((data, content_type)) = cache
            .get_full_body_cached_entry(url, provider_headers)
            .await
        {
            // Refresh the TTL by re-inserting.
            let (etag, last_modified) = existing_meta.as_ref().map_or((None, None), |meta| {
                (meta.etag.as_deref(), meta.last_modified.as_deref())
            });
            let ttl = match content_type.as_deref() {
                Some(ct) if is_manifest_content_type(ct) => cache.config().manifest_ttl,
                _ => cache.config().segment_ttl,
            };
            cache
                .put_full_body(FullBodyWrite {
                    url,
                    provider_headers,
                    data: data.clone(),
                    etag,
                    last_modified,
                    content_type: content_type.as_deref(),
                    ttl,
                })
                .await;

            let mut builder = Response::builder()
                .status(StatusCode::OK)
                .header("Content-Length", data.len().to_string())
                .header("X-Cache-Status", CacheStatus::Revalidated.as_str());
            if let Some(ct) = content_type {
                builder = builder.header("Content-Type", ct);
            }
            return builder
                .body(Body::from(data))
                .map_err(|e| anyhow::anyhow!("Failed to build revalidated response: {e}"));
        }
        // Cache entry was evicted between conditional request and now --
        // fall through to a full re-fetch without conditional headers.
        let mut request2 = cache.client().get(url);
        request2 = apply_provider_headers(request2, url, provider_headers)?;
        let resp2 =
            send_with_redirect_validation_with_control(cache.client(), request2, request_control)
                .await
                .map_err(|e| anyhow::anyhow!("Full-body re-fetch after 304 failed: {e}"))?
                .response;
        return handle_full_body_response(
            cache,
            url,
            provider_headers,
            resp2,
            pre_status,
            request_control,
        )
        .await;
    }

    handle_full_body_response(
        cache,
        url,
        provider_headers,
        resp,
        pre_status,
        request_control,
    )
    .await
}

async fn revalidate_stale_full_body_entry(
    cache: &SliceCache,
    url: &str,
    provider_headers: &HashMap<String, String>,
    existing_meta: Option<CachedResourceMeta>,
) -> Result<(), anyhow::Error> {
    let _update_guard = FullBodyUpdateGuard::new(cache, url, provider_headers);

    let mut req = cache.client().get(url);
    req = apply_provider_headers(req, url, provider_headers).map_err(|error| {
        anyhow::anyhow!(
            "Skipping background cache revalidation due to invalid provider headers: {error}"
        )
    })?;

    if let Some(ref meta) = existing_meta {
        if let Some(ref etag) = meta.etag {
            req = req.header("If-None-Match", etag.as_str());
        }
        if let Some(ref lm) = meta.last_modified {
            req = req.header("If-Modified-Since", lm.as_str());
        }
    }

    let resp = send_with_redirect_validation(cache.client(), req)
        .await
        .map_err(|e| anyhow::anyhow!("Background full-body revalidation failed: {e}"))?
        .response;

    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        let _ = resp.bytes().await;
        if let Some((data, content_type)) = cache
            .get_full_body_cached_entry(url, provider_headers)
            .await
        {
            let (etag, last_modified) = existing_meta.as_ref().map_or((None, None), |meta| {
                (meta.etag.as_deref(), meta.last_modified.as_deref())
            });
            let ttl = match content_type.as_deref() {
                Some(ct) if is_manifest_content_type(ct) => cache.config().manifest_ttl,
                _ => cache.config().segment_ttl,
            };
            cache
                .put_full_body(FullBodyWrite {
                    url,
                    provider_headers,
                    data,
                    etag,
                    last_modified,
                    content_type: content_type.as_deref(),
                    ttl,
                })
                .await;
        }
        return Ok(());
    }

    refresh_full_body_cache_entry(cache, url, provider_headers, resp).await
}

async fn refresh_full_body_cache_entry(
    cache: &SliceCache,
    url: &str,
    provider_headers: &HashMap<String, String>,
    resp: reqwest::Response,
) -> Result<(), anyhow::Error> {
    let _update_guard = FullBodyUpdateGuard::new(cache, url, provider_headers);

    if !resp.status().is_success() {
        return Ok(());
    }
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let last_modified = resp
        .headers()
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);

    let max_body = cache.config().max_cacheable_body;
    let mut buf = Vec::with_capacity(std::cmp::min(max_body, 4 * 1024 * 1024));
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| anyhow::anyhow!("Failed to read upstream body: {e}"))?;
        if buf.len().saturating_add(chunk.len()) > max_body {
            return Ok(());
        }
        buf.extend_from_slice(&chunk);
    }

    let ttl = match content_type.as_deref() {
        Some(ct) if is_manifest_content_type(ct) => cache.config().manifest_ttl,
        _ => cache.config().segment_ttl,
    };

    cache
        .put_full_body(FullBodyWrite {
            url,
            provider_headers,
            data: Bytes::from(buf),
            etag: etag.as_deref(),
            last_modified: last_modified.as_deref(),
            content_type: content_type.as_deref(),
            ttl,
        })
        .await;

    Ok(())
}

/// Process a non-304 full-body upstream response: cache if small enough,
/// stream through with BYPASS if too large.
async fn handle_full_body_response(
    cache: &SliceCache,
    url: &str,
    provider_headers: &HashMap<String, String>,
    resp: reqwest::Response,
    pre_status: CacheStatus,
    request_control: Option<&ExecutionControl>,
) -> Result<Response, anyhow::Error> {
    let status =
        StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let last_modified = resp
        .headers()
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);

    // Check Content-Length to decide if we should cache or stream through.
    let content_length_hint = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());

    let too_large_hint =
        content_length_hint.is_some_and(|cl| cl > cache.config().max_cacheable_body);

    if too_large_hint {
        // Too large to cache -- stream through with BYPASS.
        let mut builder = Response::builder()
            .status(status)
            .header("X-Cache-Status", CacheStatus::Bypass.as_str());
        if let Some(ref ct) = content_type {
            builder = builder.header("Content-Type", ct.as_str());
        }
        if let Some(cl) = content_length_hint {
            builder = builder.header("Content-Length", cl.to_string());
        }
        let stream = resp
            .bytes_stream()
            .map(|r| r.map_err(|e| std::io::Error::other(format!("Stream error: {e}"))));

        return builder
            .body(Body::from_stream(stream))
            .map_err(|e| anyhow::anyhow!("Failed to build bypass response: {e}"));
    }

    // Read body into memory with a size cap to prevent OOM from unbounded
    // chunked responses that lack a Content-Length header.
    let max_body = cache.config().max_cacheable_body;
    let mut buf = Vec::with_capacity(std::cmp::min(max_body + 1, 4 * 1024 * 1024));
    let mut stream = resp.bytes_stream();
    let mut exceeded = false;
    while let Some(chunk) =
        run_with_proxy_cancellation("full-body proxy cache read", request_control, stream.next())
            .await?
    {
        let chunk = chunk.map_err(|e| anyhow::anyhow!("Failed to read upstream body: {e}"))?;
        buf.extend_from_slice(&chunk);
        if buf.len() > max_body {
            exceeded = true;
            break;
        }
    }

    if exceeded {
        // Body exceeds max_cacheable_body (chunked transfer without
        // Content-Length). Stream the remainder through without caching.
        // First yield what we've already buffered, then stream the rest.
        let buffered = Bytes::from(buf);
        let remainder =
            stream.map(|r| r.map_err(|e| std::io::Error::other(format!("Stream error: {e}"))));
        let first_chunk: Result<Bytes, std::io::Error> = Ok(buffered);
        let combined_stream = futures::stream::once(async move { first_chunk }).chain(remainder);

        let mut builder = Response::builder()
            .status(status)
            .header("X-Cache-Status", CacheStatus::Bypass.as_str());
        if let Some(ref ct) = content_type {
            builder = builder.header("Content-Type", ct.as_str());
        }
        // Do NOT set Content-Length -- full size is unknown.
        return builder
            .body(Body::from_stream(combined_stream))
            .map_err(|e| anyhow::anyhow!("Failed to build bypass response: {e}"));
    }
    let body_bytes = Bytes::from(buf);

    if !status.is_success() {
        let mut builder = Response::builder()
            .status(status)
            .header("Content-Length", body_bytes.len().to_string())
            .header("X-Cache-Status", pre_status.as_str());
        if let Some(ref ct) = content_type {
            builder = builder.header("Content-Type", ct.as_str());
        }
        return builder
            .body(Body::from(body_bytes))
            .map_err(|e| anyhow::anyhow!("Failed to build full-body error response: {e}"));
    }

    // Cache the body.
    let ttl = match content_type.as_deref() {
        Some(ct) if is_manifest_content_type(ct) => cache.config().manifest_ttl,
        _ => cache.config().segment_ttl,
    };

    cache
        .put_full_body(FullBodyWrite {
            url,
            provider_headers,
            data: body_bytes.clone(),
            etag: etag.as_deref(),
            last_modified: last_modified.as_deref(),
            content_type: content_type.as_deref(),
            ttl,
        })
        .await;

    let mut builder = Response::builder()
        .status(status)
        .header("Content-Length", body_bytes.len().to_string())
        .header("X-Cache-Status", pre_status.as_str());
    if let Some(ref ct) = content_type {
        builder = builder.header("Content-Type", ct.as_str());
    }

    builder
        .body(Body::from(body_bytes))
        .map_err(|e| anyhow::anyhow!("Failed to build full-body response: {e}"))
}

// Stream-through helper

/// Stream an upstream response through without caching, attaching the given
/// `X-Cache-Status` header.
pub(super) async fn stream_through_with_status(
    client: &reqwest::Client,
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

    let resp = send_with_redirect_validation_with_control(client, request, request_control)
        .await
        .map_err(|e| anyhow::anyhow!("Upstream request failed: {e}"))?
        .response;

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

    // Once upstream headers are received, the body is intentionally
    // cancellation-only. We do not add a timeout around long or slow media
    // bodies here.
    let stream = resp
        .bytes_stream()
        .map(|result| result.map_err(|e| std::io::Error::other(format!("Stream error: {e}"))));

    builder
        .body(Body::from_stream(stream))
        .map_err(|e| anyhow::anyhow!("Failed to build stream-through response: {e}"))
}
