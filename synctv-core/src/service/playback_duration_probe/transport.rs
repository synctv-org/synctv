use std::{collections::HashMap, net::ToSocketAddrs as _, time::Duration};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use tracing::debug;
use url::Url;

use super::parsing::{
    first_hls_variant_url, parse_dash_duration, parse_hls_media_duration, parse_mp4_duration,
};
use crate::{Error, Result};

const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const MANIFEST_MAX_BYTES: u64 = 2 * 1024 * 1024;
const MP4_SCAN_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(super) struct ProbeTarget {
    pub(super) url: String,
    pub(super) format: String,
    pub(super) headers: HashMap<String, String>,
}

pub(super) fn is_http_url(url: &str) -> bool {
    Url::parse(url).is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https"))
}

pub(super) async fn probe_duration(
    target: &ProbeTarget,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<f64> {
    validate_probe_url(&target.url, ssrf_guard)?;
    let client = synctv_common::http::SsrfSafeClientBuilder::new()
        .ssrf_guard(ssrf_guard.clone())
        .request_timeout(PROBE_TIMEOUT)
        .read_timeout(PROBE_TIMEOUT)
        .user_agent("SyncTV duration probe")
        .build()
        .map_err(|error| Error::Internal(format!("duration probe client build failed: {error}")))?;

    if target.looks_like_hls() {
        return probe_hls_duration(&client, target, ssrf_guard).await;
    }
    if target.looks_like_dash() {
        return probe_dash_duration(&client, target).await;
    }

    match probe_mp4_duration(&client, target).await {
        Ok(duration) => Ok(duration),
        Err(mp4_error) if target.looks_like_manifest() => Err(mp4_error),
        Err(mp4_error) => {
            debug!(error = %mp4_error, "MP4 duration probe failed");
            Err(mp4_error)
        }
    }
}

impl ProbeTarget {
    fn format_lower(&self) -> String {
        self.format.trim().to_ascii_lowercase()
    }

    fn path_lower(&self) -> String {
        Url::parse(&self.url).map_or_else(
            |_| self.url.to_ascii_lowercase(),
            |url| url.path().to_ascii_lowercase(),
        )
    }

    fn looks_like_hls(&self) -> bool {
        let format = self.format_lower();
        format == "hls" || format == "m3u8" || self.path_lower().ends_with(".m3u8")
    }

    fn looks_like_dash(&self) -> bool {
        let format = self.format_lower();
        format == "dash" || format == "mpd" || self.path_lower().ends_with(".mpd")
    }

    fn looks_like_manifest(&self) -> bool {
        self.looks_like_hls() || self.looks_like_dash()
    }
}

async fn probe_hls_duration(
    client: &reqwest::Client,
    target: &ProbeTarget,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<f64> {
    let manifest = fetch_text(client, &target.url, &target.headers).await?;
    if let Some(duration) = parse_hls_media_duration(&manifest)? {
        return Ok(duration);
    }

    let variant_url = first_hls_variant_url(&target.url, &manifest)?;
    validate_probe_url(&variant_url, ssrf_guard)?;
    let variant = fetch_text(client, &variant_url, &target.headers).await?;
    parse_hls_media_duration(&variant)?.ok_or_else(|| {
        Error::InvalidInput("HLS manifest does not contain media segment durations".to_string())
    })
}

async fn probe_dash_duration(client: &reqwest::Client, target: &ProbeTarget) -> Result<f64> {
    let manifest = fetch_text(client, &target.url, &target.headers).await?;
    parse_dash_duration(&manifest).ok_or_else(|| {
        Error::InvalidInput("DASH manifest does not contain a presentation duration".to_string())
    })
}

async fn probe_mp4_duration(client: &reqwest::Client, target: &ProbeTarget) -> Result<f64> {
    let head = fetch_range(client, target, 0, MP4_SCAN_BYTES - 1).await?;
    if let Some(duration) = parse_mp4_duration(&head.bytes) {
        return Ok(duration);
    }

    let total_len = head
        .total_len
        .ok_or_else(|| Error::InvalidInput("MP4 duration box was not found".to_string()))?;
    if total_len <= MP4_SCAN_BYTES {
        return Err(Error::InvalidInput(
            "MP4 duration box was not found".to_string(),
        ));
    }

    let start = total_len.saturating_sub(MP4_SCAN_BYTES);
    let tail = fetch_range(client, target, start, total_len - 1).await?;
    parse_mp4_duration(&tail.bytes).ok_or_else(|| {
        Error::InvalidInput("MP4 duration box was not found in scanned ranges".to_string())
    })
}

fn validate_probe_url(url: &str, guard: &synctv_common::ssrf::SsrfGuard) -> Result<()> {
    let parsed = Url::parse(url).map_err(|error| Error::InvalidInput(error.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::InvalidInput(
            "duration probe requires http or https URL".to_string(),
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| Error::InvalidInput("duration probe URL is missing host".to_string()))?;
    if guard.is_host_blocked(host) {
        return Err(Error::Authorization(
            "duration probe URL host is blocked".to_string(),
        ));
    }

    let port = parsed.port_or_known_default().ok_or_else(|| {
        Error::InvalidInput("duration probe URL is missing effective port".to_string())
    })?;
    let addrs = (host, port).to_socket_addrs().map_err(|error| {
        Error::ServiceUnavailable(format!("duration probe DNS failed: {error}"))
    })?;
    let mut resolved = false;
    for addr in addrs {
        resolved = true;
        if guard.is_ip_blocked_for_host(host, &addr.ip())
            || guard.is_port_blocked_for_ip(port, &addr.ip())
        {
            return Err(Error::Authorization(
                "duration probe URL address is blocked".to_string(),
            ));
        }
    }
    if !resolved {
        return Err(Error::ServiceUnavailable(
            "duration probe URL did not resolve".to_string(),
        ));
    }

    Ok(())
}

async fn fetch_text(
    client: &reqwest::Client,
    url: &str,
    headers: &HashMap<String, String>,
) -> Result<String> {
    let response = apply_provider_headers(client.get(url), headers)
        .send()
        .await
        .map_err(|error| {
            Error::ServiceUnavailable(format!("duration probe GET failed: {error}"))
        })?;
    if !response.status().is_success() {
        return Err(Error::ServiceUnavailable(format!(
            "duration probe GET returned status {}",
            response.status()
        )));
    }
    if let Some(len) = response.content_length() {
        if len > MANIFEST_MAX_BYTES {
            return Err(Error::InvalidInput(
                "manifest is too large to probe".to_string(),
            ));
        }
    }
    let bytes = response.bytes().await.map_err(|error| {
        Error::ServiceUnavailable(format!("duration probe body failed: {error}"))
    })?;
    if bytes.len() as u64 > MANIFEST_MAX_BYTES {
        return Err(Error::InvalidInput(
            "manifest is too large to probe".to_string(),
        ));
    }
    String::from_utf8(bytes.to_vec())
        .map_err(|error| Error::InvalidInput(format!("manifest is not utf-8: {error}")))
}

struct RangeFetch {
    bytes: bytes::Bytes,
    total_len: Option<u64>,
}

async fn fetch_range(
    client: &reqwest::Client,
    target: &ProbeTarget,
    start: u64,
    end: u64,
) -> Result<RangeFetch> {
    let response = apply_range_probe_headers(client.get(&target.url), &target.headers)
        .header(RANGE, format!("bytes={start}-{end}"))
        .send()
        .await
        .map_err(|error| {
            Error::ServiceUnavailable(format!("duration probe range GET failed: {error}"))
        })?;
    let status = response.status();
    if status != reqwest::StatusCode::PARTIAL_CONTENT && status != reqwest::StatusCode::OK {
        return Err(Error::ServiceUnavailable(format!(
            "duration probe range GET returned status {status}"
        )));
    }

    let headers = response.headers().clone();
    if status == reqwest::StatusCode::OK {
        let Some(content_len) = parse_content_length(&headers) else {
            return Err(Error::InvalidInput(
                "range probe response is missing content length".to_string(),
            ));
        };
        if content_len > MP4_SCAN_BYTES {
            return Err(Error::InvalidInput(
                "server ignored range request for large media".to_string(),
            ));
        }
    }

    let total_len = parse_total_len(&headers).or_else(|| parse_content_length(&headers));
    let bytes = response.bytes().await.map_err(|error| {
        Error::ServiceUnavailable(format!("duration probe range body failed: {error}"))
    })?;
    if bytes.len() as u64 > MP4_SCAN_BYTES {
        return Err(Error::InvalidInput(
            "range response is too large".to_string(),
        ));
    }

    Ok(RangeFetch { bytes, total_len })
}

fn apply_provider_headers(
    mut builder: reqwest::RequestBuilder,
    headers: &HashMap<String, String>,
) -> reqwest::RequestBuilder {
    for (name, value) in headers {
        if let Some((name, value)) = parse_probe_header(name, value) {
            builder = builder.header(name, value);
        }
    }
    builder
}

fn apply_range_probe_headers(
    mut builder: reqwest::RequestBuilder,
    headers: &HashMap<String, String>,
) -> reqwest::RequestBuilder {
    for (name, value) in headers {
        let Some((name, value)) = parse_probe_header(name, value) else {
            continue;
        };
        if name == RANGE {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder
}

fn parse_probe_header(name: &str, value: &str) -> Option<(HeaderName, HeaderValue)> {
    let name = HeaderName::from_bytes(name.as_bytes()).ok()?;
    let value = HeaderValue::from_str(value).ok()?;
    Some((name, value))
}

fn parse_content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn parse_total_len(headers: &HeaderMap) -> Option<u64> {
    let content_range = headers.get(CONTENT_RANGE)?.to_str().ok()?;
    let (_, total) = content_range.rsplit_once('/')?;
    total.trim().parse().ok()
}
