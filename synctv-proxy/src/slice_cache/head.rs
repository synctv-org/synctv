use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use synctv_common::ExecutionControl;
use tokio::sync::Mutex;

use crate::{
    apply_provider_headers, send_head_with_redirect_validation_with_control_and_timeout,
    send_with_redirect_validation_with_control_and_timeout,
};

use super::etag::CachedResourceMeta;
use super::range::{parse_content_range, parse_range_header};
use super::status::CacheStatus;
use super::types::HeadResourceResult;

type SliceLock = Arc<Mutex<()>>;

pub(super) fn parse_total_size_from_content_range(
    resp: &reqwest::Response,
) -> Result<Option<u64>, anyhow::Error> {
    let Some(value) = resp.headers().get(reqwest::header::CONTENT_RANGE) else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|e| anyhow::anyhow!("Invalid Content-Range header in fallback response: {e}"))?;
    let parsed = parse_content_range(value)?;
    Ok(parsed.complete_length)
}

fn parse_content_length_header(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
}

async fn discover_content_length_via_range_get(
    client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    url: &str,
    provider_headers: &HashMap<String, String>,
    request_control: Option<&ExecutionControl>,
    upstream_header_timeout: Option<Duration>,
) -> Result<u64, anyhow::Error> {
    let mut request = client.get(url);
    request = apply_provider_headers(request, url, provider_headers)?;
    request = request.header(reqwest::header::RANGE, "bytes=0-0");
    let resp = send_with_redirect_validation_with_control_and_timeout(
        client,
        request,
        ssrf_guard,
        request_control,
        upstream_header_timeout,
    )
    .await
    .context("Range GET fallback failed")?
    .response;

    match resp.status() {
        reqwest::StatusCode::PARTIAL_CONTENT => parse_total_size_from_content_range(&resp)?
            .ok_or_else(|| {
                anyhow::anyhow!("Missing complete length in Content-Range fallback response")
            }),
        reqwest::StatusCode::OK => parse_content_length_header(&resp).ok_or_else(|| {
            anyhow::anyhow!("Missing or invalid Content-Length in fallback GET response")
        }),
        status => Err(anyhow::anyhow!(
            "Range GET fallback returned status {status}"
        )),
    }
}

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
    head_content_length_with_control_and_timeout(
        client,
        ssrf_guard,
        url,
        provider_headers,
        request_control,
        None,
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
    let mut request = client.head(url);
    request = apply_provider_headers(request, url, provider_headers)?;
    let resp = send_head_with_redirect_validation_with_control_and_timeout(
        client,
        request,
        ssrf_guard,
        request_control,
        upstream_header_timeout,
    )
    .await
    .context("HEAD request failed")?
    .response;

    if !resp.status().is_success() {
        return discover_content_length_via_range_get(
            client,
            ssrf_guard,
            url,
            provider_headers,
            request_control,
            upstream_header_timeout,
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
        upstream_header_timeout,
    )
    .await
}

pub(super) struct HeadFetchContext<'a> {
    pub(super) client: &'a reqwest::Client,
    pub(super) ssrf_guard: &'a synctv_common::ssrf::SsrfGuard,
    pub(super) segment_ttl: Duration,
    pub(super) meta: &'a Arc<dashmap::DashMap<String, CachedResourceMeta>>,
    pub(super) meta_key: String,
    pub(super) meta_lock: SliceLock,
}

pub(super) fn cached_head_headers(
    meta: &CachedResourceMeta,
    range_header: Option<&str>,
    metadata_ttl: Duration,
) -> Option<(reqwest::StatusCode, reqwest::header::HeaderMap)> {
    if std::time::SystemTime::now()
        .duration_since(meta.validated_at)
        .unwrap_or(Duration::ZERO)
        > metadata_ttl
    {
        return None;
    }

    let total_size = meta.total_size?;
    let mut headers = reqwest::header::HeaderMap::new();
    if meta.supports_ranges {
        headers.insert(
            reqwest::header::ACCEPT_RANGES,
            reqwest::header::HeaderValue::from_static("bytes"),
        );
    }
    insert_header_from_string(
        &mut headers,
        reqwest::header::CONTENT_TYPE,
        meta.content_type.as_deref(),
    );
    insert_header_from_string(&mut headers, reqwest::header::ETAG, meta.etag.as_deref());
    insert_header_from_string(
        &mut headers,
        reqwest::header::LAST_MODIFIED,
        meta.last_modified.as_deref(),
    );

    if let Some(range) = range_header {
        if !meta.supports_ranges {
            return None;
        }
        let Ok((start, end)) = parse_range_header(range, total_size) else {
            return None;
        };
        let content_range = format!("bytes {start}-{end}/{total_size}");
        insert_header_from_str(&mut headers, reqwest::header::CONTENT_RANGE, &content_range);
        let content_length = end.checked_sub(start)?.checked_add(1)?;
        insert_header_from_str(
            &mut headers,
            reqwest::header::CONTENT_LENGTH,
            &content_length.to_string(),
        );
        Some((reqwest::StatusCode::PARTIAL_CONTENT, headers))
    } else {
        insert_header_from_str(
            &mut headers,
            reqwest::header::CONTENT_LENGTH,
            &total_size.to_string(),
        );
        Some((reqwest::StatusCode::OK, headers))
    }
}

pub(super) async fn get_or_fetch_head_resource_with_control(
    ctx: HeadFetchContext<'_>,
    url: &str,
    provider_headers: &HashMap<String, String>,
    range_header: Option<&str>,
    request_control: Option<&ExecutionControl>,
    upstream_header_timeout: Option<Duration>,
) -> Result<HeadResourceResult, anyhow::Error> {
    if let Some(meta) = get_resource_meta(ctx.meta, &ctx.meta_key) {
        if let Some((status, headers)) = cached_head_headers(&meta, range_header, ctx.segment_ttl) {
            return Ok(HeadResourceResult {
                status,
                headers,
                cache_status: CacheStatus::Hit,
            });
        }
    }

    let _guard = if let Some(control) = request_control {
        let cancellation = control.cancellation_token();
        tokio::select! {
            () = cancellation.cancelled() => {
                return Err(anyhow::anyhow!(
                    "Request cancelled while waiting for HEAD metadata cache lock",
                ));
            }
            guard = ctx.meta_lock.lock() => guard,
        }
    } else {
        ctx.meta_lock.lock().await
    };

    if let Some(meta) = get_resource_meta(ctx.meta, &ctx.meta_key) {
        if let Some((status, headers)) = cached_head_headers(&meta, range_header, ctx.segment_ttl) {
            return Ok(HeadResourceResult {
                status,
                headers,
                cache_status: CacheStatus::Hit,
            });
        }
    }

    let mut request = ctx.client.head(url);
    request = apply_provider_headers(request, url, provider_headers)?;
    if let Some(range) = range_header {
        request = request.header(reqwest::header::RANGE, range);
    }

    let resp = send_head_with_redirect_validation_with_control_and_timeout(
        ctx.client,
        request,
        ctx.ssrf_guard,
        request_control,
        upstream_header_timeout,
    )
    .await
    .context("HEAD metadata request failed")?
    .response;
    let status = resp.status();
    let headers = resp.headers().clone();

    if status.is_success() {
        ctx.meta
            .insert(ctx.meta_key, resource_meta_from_head_headers(&headers));
    }

    Ok(HeadResourceResult {
        status,
        headers,
        cache_status: CacheStatus::Miss,
    })
}

fn get_resource_meta(
    meta: &dashmap::DashMap<String, CachedResourceMeta>,
    meta_key: &str,
) -> Option<CachedResourceMeta> {
    if let Some(mut entry) = meta.get_mut(meta_key) {
        entry.last_accessed = std::time::SystemTime::now();
        Some(entry.clone())
    } else {
        None
    }
}

fn resource_meta_from_head_headers(headers: &reqwest::header::HeaderMap) -> CachedResourceMeta {
    let content_range_total_size = headers
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_content_range(value).ok())
        .and_then(|parsed| parsed.complete_length);
    let accepts_ranges = headers
        .get(reqwest::header::ACCEPT_RANGES)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("bytes"));
    let supports_ranges = content_range_total_size.is_some() || accepts_ranges;
    let total_size = content_range_total_size.or_else(|| {
        headers
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
    });
    let now = std::time::SystemTime::now();

    CachedResourceMeta {
        etag: header_to_string(headers, reqwest::header::ETAG),
        last_modified: header_to_string(headers, reqwest::header::LAST_MODIFIED),
        total_size,
        supports_ranges,
        content_type: header_to_string(headers, reqwest::header::CONTENT_TYPE),
        validated_at: now,
        last_accessed: now,
    }
}

fn header_to_string(
    headers: &reqwest::header::HeaderMap,
    name: reqwest::header::HeaderName,
) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

fn insert_header_from_string(
    headers: &mut reqwest::header::HeaderMap,
    name: reqwest::header::HeaderName,
    value: Option<&str>,
) {
    let Some(value) = value else {
        return;
    };
    if let Ok(value) = reqwest::header::HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
}

fn insert_header_from_str(
    headers: &mut reqwest::header::HeaderMap,
    name: reqwest::header::HeaderName,
    value: &str,
) {
    if let Ok(value) = reqwest::header::HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
}
