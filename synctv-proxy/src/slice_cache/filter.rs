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

use crate::{apply_provider_headers, PROXY_CLIENT};

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
    let mut request = PROXY_CLIENT.head(url);
    request = apply_provider_headers(request, url, provider_headers);

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
        return stream_through_with_status(url, provider_headers, range_header, CacheStatus::Bypass)
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

    // Fetch all needed slices, tracking the aggregate cache status.
    let mut combined = Vec::new();
    let mut worst_status = CacheStatus::Hit; // Start optimistic.

    for &idx in &needed {
        let (slice_data, slice_status) = cache
            .get_or_fetch_slice(url, provider_headers, idx, total_size)
            .await?;

        // Merge slice status: the "worst" status wins.
        worst_status = merge_cache_status(worst_status, slice_status);

        let (slice_start, _) =
            aligned_range_for_slice(idx, cache.config().slice_size, total_size);

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
fn merge_cache_status(a: CacheStatus, b: CacheStatus) -> CacheStatus {
    fn priority(s: CacheStatus) -> u8 {
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
    if priority(a) >= priority(b) { a } else { b }
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

    // Fetch from upstream.
    let mut request = PROXY_CLIENT.get(url);
    request = apply_provider_headers(request, url, provider_headers);

    let resp = request
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Upstream request failed: {e}"))?;

    let status = StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

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

    let too_large_hint = content_length_hint
        .is_some_and(|cl| cl > cache.config().max_cacheable_body);

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

    // Read body into memory (up to max_cacheable_body).
    let body_bytes = resp
        .bytes()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read upstream body: {e}"))?;

    if body_bytes.len() > cache.config().max_cacheable_body {
        // Body turned out larger than expected (chunked transfer).
        // Return it but don't cache.
        let mut builder = Response::builder()
            .status(status)
            .header("Content-Length", body_bytes.len().to_string())
            .header("X-Cache-Status", CacheStatus::Bypass.as_str());
        if let Some(ref ct) = content_type {
            builder = builder.header("Content-Type", ct.as_str());
        }
        return builder
            .body(Body::from(body_bytes))
            .map_err(|e| anyhow::anyhow!("Failed to build bypass response: {e}"));
    }

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
    let mut request = PROXY_CLIENT.get(url);
    request = apply_provider_headers(request, url, provider_headers);

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
