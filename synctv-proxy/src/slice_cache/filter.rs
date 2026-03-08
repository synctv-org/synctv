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

use crate::{apply_provider_headers, proxy_client};

use super::config::is_manifest_content_type;
use super::range::{aligned_range_for_slice, compute_needed_slices, parse_range_header};
use super::status::CacheStatus;
use super::store::SliceCache;

// ------------------------------------------------------------------
// HEAD helper
// ------------------------------------------------------------------

/// Send a HEAD request to discover the upstream `Content-Length`.
#[allow(clippy::implicit_hasher)]
pub async fn head_content_length(
    url: &str,
    provider_headers: &HashMap<String, String>,
) -> Result<u64, anyhow::Error> {
    let mut request = proxy_client()?.head(url);
    request = apply_provider_headers(request, url, provider_headers)?;

    let resp = request
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("HEAD request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(anyhow::anyhow!(
            "HEAD request returned status {}",
            resp.status()
        ));
    }

    let content_length = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| anyhow::anyhow!("Missing or invalid Content-Length in HEAD response"))?;

    Ok(content_length)
}

// ------------------------------------------------------------------
// proxy_with_cache  --  main entry point
// ------------------------------------------------------------------

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
    // ------ BYPASS: cache disabled ------
    if !cache.config().enabled {
        return stream_through_with_status(
            url,
            provider_headers,
            range_header,
            CacheStatus::Bypass,
        )
        .await;
    }

    // ------ No Range header: full-body cache path ------
    if range_header.is_none() {
        return full_body_cache_path(cache, url, provider_headers).await;
    }

    // ------ Range request: slice-cache path ------
    // SAFETY: range_header.is_none() was checked above.
    let range_str = range_header.expect("range_header checked above");

    // Total size needed for range parsing.
    // Reuse cached metadata when available to avoid a HEAD request on every
    // range request, even when the slice data is already cached (L4 fix).
    let total_size = match cache.get_resource_meta(url, provider_headers).await {
        Some(meta) if meta.total_size.is_some() => meta.total_size.expect("checked above"),
        _ => head_content_length(url, provider_headers).await?,
    };

    let (range_start, range_end) = parse_range_header(range_str, total_size)?;

    let needed = compute_needed_slices(range_start, range_end, cache.config().slice_size);

    // Determine cache status *before* fetching.
    let pre_status = cache
        .determine_slice_cache_status(url, provider_headers, &needed)
        .await;

    // For large range requests spanning many slices, stream directly from
    // upstream to avoid buffering all slice data in memory.
    const MAX_BUFFERED_SLICES: usize = 8;
    if needed.len() > MAX_BUFFERED_SLICES {
        return stream_through_with_status(
            url,
            provider_headers,
            range_header,
            CacheStatus::Bypass,
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
            .get_or_fetch_slice(url, provider_headers, idx, total_size)
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

// ------------------------------------------------------------------
// Full-body cache path
// ------------------------------------------------------------------

/// Handle a non-range request through the full-body cache.
pub(super) async fn full_body_cache_path(
    cache: &SliceCache,
    url: &str,
    provider_headers: &HashMap<String, String>,
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
                let client = match proxy_client() {
                    Ok(client) => client,
                    Err(error) => {
                        tracing::warn!(
                            url = %bg_url,
                            error = %error,
                            "Skipping background cache revalidation due to proxy client init failure"
                        );
                        return;
                    }
                };
                let mut req = client.get(&bg_url);
                req = match apply_provider_headers(req, &bg_url, &bg_headers) {
                    Ok(req) => req,
                    Err(error) => {
                        tracing::warn!(
                            url = %bg_url,
                            error = %error,
                            "Skipping background cache revalidation due to invalid provider headers"
                        );
                        return;
                    }
                };
                if let Some(ref meta) = bg_meta {
                    if let Some(ref etag) = meta.etag {
                        req = req.header("If-None-Match", etag.as_str());
                    }
                    if let Some(ref lm) = meta.last_modified {
                        req = req.header("If-Modified-Since", lm.as_str());
                    }
                }
                match req.send().await {
                    Ok(resp) if resp.status() == reqwest::StatusCode::NOT_MODIFIED => {
                        // Still valid -- refresh TTL so the next request is a HIT instead of
                        // repeatedly re-entering the stale path.
                        let _ = resp.bytes().await;
                        if let Some((data, content_type, _)) =
                            bg_cache.get_full_body(&bg_url, &bg_headers).await
                        {
                            let ttl = match content_type.as_deref() {
                                Some(ct) if is_manifest_content_type(ct) => {
                                    bg_cache.config().manifest_ttl
                                }
                                _ => bg_cache.config().segment_ttl,
                            };
                            bg_cache
                                .put_full_body(
                                    &bg_url,
                                    &bg_headers,
                                    data,
                                    content_type.as_deref(),
                                    ttl,
                                )
                                .await;
                        }
                    }
                    Ok(resp) => {
                        if let Err(error) =
                            refresh_full_body_cache_entry(&bg_cache, &bg_url, &bg_headers, resp)
                                .await
                        {
                            tracing::debug!(
                                url = %bg_url,
                                error = %error,
                                "Background full-body revalidation failed to update cache"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::debug!(
                            url = %bg_url,
                            error = %e,
                            "Background full-body revalidation failed"
                        );
                    }
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
    let mut request = proxy_client()?.get(url);
    request = apply_provider_headers(request, url, provider_headers)?;

    // Add conditional request headers from stored metadata to enable 304
    // responses and avoid re-downloading unchanged resources.
    if let Some(meta) = cache.get_resource_meta(url, provider_headers).await {
        if let Some(ref etag) = meta.etag {
            request = request.header("If-None-Match", etag.as_str());
        }
        if let Some(ref lm) = meta.last_modified {
            request = request.header("If-Modified-Since", lm.as_str());
        }
    }

    let resp = request
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Upstream request failed: {e}"))?;

    // Handle 304 Not Modified: upstream confirmed the cached copy is still
    // valid.  Re-insert the full body with a fresh TTL and serve it.
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        // Consume the empty body to release the connection.
        let _ = resp.bytes().await;

        // Re-fetch from our cache (it may have been evicted in the meantime).
        if let Some((data, content_type, _)) = cache.get_full_body(url, provider_headers).await {
            // Refresh the TTL by re-inserting.
            let ttl = match content_type.as_deref() {
                Some(ct) if is_manifest_content_type(ct) => cache.config().manifest_ttl,
                _ => cache.config().segment_ttl,
            };
            cache
                .put_full_body(
                    url,
                    provider_headers,
                    data.clone(),
                    content_type.as_deref(),
                    ttl,
                )
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
        let mut request2 = proxy_client()?.get(url);
        request2 = apply_provider_headers(request2, url, provider_headers)?;
        let resp2 = request2
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Full-body re-fetch after 304 failed: {e}"))?;
        return handle_full_body_response(cache, url, provider_headers, resp2, pre_status).await;
    }

    handle_full_body_response(cache, url, provider_headers, resp, pre_status).await
}

async fn refresh_full_body_cache_entry(
    cache: &SliceCache,
    url: &str,
    provider_headers: &HashMap<String, String>,
    resp: reqwest::Response,
) -> Result<(), anyhow::Error> {
    let content_type = resp
        .headers()
        .get("content-type")
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
        .put_full_body(
            url,
            provider_headers,
            Bytes::from(buf),
            content_type.as_deref(),
            ttl,
        )
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
) -> Result<Response, anyhow::Error> {
    let status =
        StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let content_type = resp
        .headers()
        .get("content-type")
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
    while let Some(chunk) = stream.next().await {
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

    // Cache the body.
    let ttl = match content_type.as_deref() {
        Some(ct) if is_manifest_content_type(ct) => cache.config().manifest_ttl,
        _ => cache.config().segment_ttl,
    };

    cache
        .put_full_body(
            url,
            provider_headers,
            body_bytes.clone(),
            content_type.as_deref(),
            ttl,
        )
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

// ------------------------------------------------------------------
// Stream-through helper
// ------------------------------------------------------------------

/// Stream an upstream response through without caching, attaching the given
/// `X-Cache-Status` header.
pub(super) async fn stream_through_with_status(
    url: &str,
    provider_headers: &HashMap<String, String>,
    range_header: Option<&str>,
    cache_status: CacheStatus,
) -> Result<Response, anyhow::Error> {
    let mut request = proxy_client()?.get(url);
    request = apply_provider_headers(request, url, provider_headers)?;

    if let Some(range) = range_header {
        request = request.header("Range", range);
    }

    let resp = request
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Upstream request failed: {e}"))?;

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

    use futures::StreamExt;
    let stream = resp
        .bytes_stream()
        .map(|result| result.map_err(|e| std::io::Error::other(format!("Stream error: {e}"))));

    builder
        .body(Body::from_stream(stream))
        .map_err(|e| anyhow::anyhow!("Failed to build stream-through response: {e}"))
}
