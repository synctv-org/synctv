use std::{collections::HashMap, net::ToSocketAddrs as _, sync::Arc, time::Duration};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use url::Url;

use super::{media::BackendPlaybackRequest, LeaderCheck, PlaybackService};
use crate::{
    models::{PlaybackDurationStatus, PlaybackSourceIdentity},
    provider::PlaybackResult,
    Error, Result,
};

const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const MANIFEST_MAX_BYTES: u64 = 2 * 1024 * 1024;
const MP4_SCAN_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone)]
pub struct PlaybackDurationProbeService {
    playback_service: PlaybackService,
    leader_check: Arc<dyn LeaderCheck>,
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
    concurrency: usize,
}

#[derive(Debug, Clone)]
struct ProbeTarget {
    url: String,
    format: String,
    headers: HashMap<String, String>,
}

impl PlaybackDurationProbeService {
    const DEFAULT_SCAN_LIMIT: i64 = 32;
    const DEFAULT_CONCURRENCY: usize = 4;
    const RETRY_AFTER_TRANSIENT: chrono::Duration = chrono::Duration::minutes(10);
    const RETRY_AFTER_UNAVAILABLE: chrono::Duration = chrono::Duration::hours(6);

    #[must_use]
    pub const fn new(
        playback_service: PlaybackService,
        leader_check: Arc<dyn LeaderCheck>,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
    ) -> Self {
        Self {
            playback_service,
            leader_check,
            ssrf_guard,
            concurrency: Self::DEFAULT_CONCURRENCY,
        }
    }

    #[must_use]
    pub const fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency;
        self
    }

    #[must_use]
    pub fn spawn(
        &self,
        interval: Duration,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let service = self.clone();

        crate::spawn::spawn_monitored("playback_duration_probe", async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await;

            loop {
                tokio::select! {
                    () = cancel.cancelled() => {
                        info!("Playback duration probe task cancelled");
                        return;
                    }
                    _ = ticker.tick() => {
                        if !service.leader_check.is_leader() {
                            continue;
                        }

                        match service.run_once().await {
                            Ok(probed) if probed > 0 => {
                                info!(probed, "Playback duration probe completed");
                            }
                            Ok(_) => {}
                            Err(error) => {
                                error!(error = %error, "Playback duration probe failed");
                            }
                        }
                    }
                }
            }
        })
    }

    pub async fn run_once(&self) -> Result<usize> {
        let claims = self
            .playback_service
            .source_metadata_repository()
            .claim_duration_probe_batch(Self::DEFAULT_SCAN_LIMIT)
            .await?;
        if claims.is_empty() {
            return Ok(0);
        }

        let semaphore = Arc::new(Semaphore::new(self.concurrency.max(1)));
        let mut tasks = tokio::task::JoinSet::new();

        for claim in claims {
            let service = self.clone();
            let semaphore = semaphore.clone();
            tasks.spawn(async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .map_err(|_| Error::ServiceUnavailable("duration probe stopped".to_string()))?;
                service.probe_claim(claim).await
            });
        }

        let mut completed = 0_usize;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => completed += 1,
                Ok(Err(error)) => warn!(error = %error, "Playback duration probe claim failed"),
                Err(error) => warn!(error = %error, "Playback duration probe task failed"),
            }
        }

        Ok(completed)
    }

    async fn probe_claim(&self, claim: crate::models::ClaimedPlaybackDurationProbe) -> Result<()> {
        let Some(identity) = PlaybackSourceIdentity::from_state(&claim.state) else {
            return Ok(());
        };
        if identity != playback_identity_from_metadata(&claim.metadata) {
            return Ok(());
        }

        let playback = self
            .playback_service
            .generate_backend_playback_for_source(BackendPlaybackRequest {
                room_id: claim.metadata.room_id,
                media_id: claim.metadata.media_id,
                playlist_id: claim.metadata.playlist_id,
                target: &claim.state.target,
            })
            .await?;
        let Some(playback) = playback else {
            self.mark_failed(
                &identity,
                claim.metadata.version,
                PlaybackDurationStatus::Unavailable,
                "playback source disappeared",
                Self::RETRY_AFTER_UNAVAILABLE,
            )
            .await?;
            return Ok(());
        };

        if let Some(duration_seconds) = playback
            .duration_seconds
            .filter(|duration| duration.is_finite() && *duration > 0.0)
        {
            self.playback_service
                .source_metadata_repository()
                .complete_probe_duration(&identity, claim.metadata.version, duration_seconds)
                .await?;
            return Ok(());
        }

        let Some(target) = select_probe_target(&playback) else {
            self.mark_failed(
                &identity,
                claim.metadata.version,
                PlaybackDurationStatus::Unavailable,
                "playback has no probeable URL",
                Self::RETRY_AFTER_UNAVAILABLE,
            )
            .await?;
            return Ok(());
        };

        match probe_duration(&target, &self.ssrf_guard).await {
            Ok(duration_seconds) => {
                self.playback_service
                    .source_metadata_repository()
                    .complete_probe_duration(&identity, claim.metadata.version, duration_seconds)
                    .await?;
            }
            Err(error) => {
                self.mark_failed(
                    &identity,
                    claim.metadata.version,
                    PlaybackDurationStatus::Failed,
                    &error.to_string(),
                    Self::RETRY_AFTER_TRANSIENT,
                )
                .await?;
            }
        }

        Ok(())
    }

    async fn mark_failed(
        &self,
        identity: &PlaybackSourceIdentity,
        expected_version: i64,
        status: PlaybackDurationStatus,
        error: &str,
        retry_after: chrono::Duration,
    ) -> Result<()> {
        self.playback_service
            .source_metadata_repository()
            .mark_probe_failed(identity, expected_version, status, error, retry_after)
            .await?;
        Ok(())
    }
}

fn playback_identity_from_metadata(
    metadata: &crate::models::PlaybackSourceMetadata,
) -> PlaybackSourceIdentity {
    PlaybackSourceIdentity {
        room_id: metadata.room_id,
        media_id: metadata.media_id,
        playlist_id: metadata.playlist_id,
        target_hash: metadata.target_hash.clone(),
    }
}

fn select_probe_target(playback: &PlaybackResult) -> Option<ProbeTarget> {
    let info = playback
        .playback_infos
        .get(&playback.default_mode)
        .or_else(|| playback.playback_infos.values().next())?;
    let url = info.urls.iter().find(|url| is_http_url(url))?.clone();
    Some(ProbeTarget {
        url,
        format: info.format.clone(),
        headers: info.headers.clone(),
    })
}

fn is_http_url(url: &str) -> bool {
    Url::parse(url)
        .ok()
        .is_some_and(|parsed| matches!(parsed.scheme(), "http" | "https"))
}

async fn probe_duration(
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
        return probe_dash_duration(&client, target, ssrf_guard).await;
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

async fn probe_dash_duration(
    client: &reqwest::Client,
    target: &ProbeTarget,
    _ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> Result<f64> {
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

fn parse_hls_media_duration(manifest: &str) -> Result<Option<f64>> {
    let mut total = 0.0;
    let mut found = false;
    for line in manifest.lines().map(str::trim) {
        let Some(rest) = line.strip_prefix("#EXTINF:") else {
            continue;
        };
        let value = rest.split(',').next().unwrap_or_default().trim();
        let duration = value.parse::<f64>().map_err(|error| {
            Error::InvalidInput(format!("invalid HLS EXTINF duration: {error}"))
        })?;
        if !duration.is_finite() || duration < 0.0 {
            return Err(Error::InvalidInput(
                "invalid HLS EXTINF duration".to_string(),
            ));
        }
        found = true;
        total += duration;
    }

    Ok((found && total > 0.0).then_some(total))
}

fn first_hls_variant_url(base_url: &str, manifest: &str) -> Result<String> {
    let mut next_uri_is_variant = false;
    for line in manifest.lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        if line.starts_with("#EXT-X-STREAM-INF") {
            next_uri_is_variant = true;
            continue;
        }
        if next_uri_is_variant && !line.starts_with('#') {
            return resolve_relative_url(base_url, line);
        }
    }
    Err(Error::InvalidInput(
        "HLS master manifest does not contain a variant URI".to_string(),
    ))
}

fn resolve_relative_url(base_url: &str, uri: &str) -> Result<String> {
    Url::parse(base_url)
        .and_then(|base| base.join(uri))
        .map(|url| url.to_string())
        .map_err(|error| Error::InvalidInput(format!("invalid manifest URI: {error}")))
}

fn parse_dash_duration(manifest: &str) -> Option<f64> {
    let attr = find_xml_attr(manifest, "mediaPresentationDuration")?;
    parse_iso8601_duration_seconds(attr)
}

fn find_xml_attr<'a>(text: &'a str, attr: &str) -> Option<&'a str> {
    let needle = format!("{attr}=\"");
    let start = text.find(&needle)? + needle.len();
    let rest = &text[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn parse_iso8601_duration_seconds(value: &str) -> Option<f64> {
    let value = value.strip_prefix('P')?;
    let mut number = String::new();
    let mut seconds = 0.0;
    let mut in_time = false;

    for ch in value.chars() {
        match ch {
            'T' => in_time = true,
            '0'..='9' | '.' => number.push(ch),
            'D' => {
                seconds += take_duration_number(&mut number)? * 86_400.0;
            }
            'H' if in_time => {
                seconds += take_duration_number(&mut number)? * 3_600.0;
            }
            'M' if in_time => {
                seconds += take_duration_number(&mut number)? * 60.0;
            }
            'S' if in_time => {
                seconds += take_duration_number(&mut number)?;
            }
            _ => return None,
        }
    }

    (number.is_empty() && seconds > 0.0 && seconds.is_finite()).then_some(seconds)
}

fn take_duration_number(number: &mut String) -> Option<f64> {
    let value = number.parse().ok()?;
    number.clear();
    Some(value)
}

fn parse_mp4_duration(bytes: &[u8]) -> Option<f64> {
    const MOOV: [u8; 4] = *b"moov";
    const MVHD: [u8; 4] = *b"mvhd";

    let mut offset = 0_usize;
    while offset + 8 <= bytes.len() {
        let header = Mp4BoxHeader::parse(bytes, offset)?;
        let body_start = header.header_end;
        let body_end = header.end.min(bytes.len());
        match header.name {
            MOOV => {
                if let Some(duration) = parse_mp4_duration(&bytes[body_start..body_end]) {
                    return Some(duration);
                }
            }
            MVHD => {
                return parse_mvhd_duration(&bytes[body_start..body_end]);
            }
            _ => {}
        }
        if header.end <= offset {
            return None;
        }
        offset = header.end;
    }
    None
}

struct Mp4BoxHeader {
    name: [u8; 4],
    header_end: usize,
    end: usize,
}

impl Mp4BoxHeader {
    fn parse(bytes: &[u8], offset: usize) -> Option<Self> {
        let size32 = u64::from(read_u32(bytes, offset)?);
        let name = bytes.get(offset + 4..offset + 8)?.try_into().ok()?;
        let (size, header_len) = if size32 == 1 {
            (read_u64(bytes, offset + 8)?, 16_usize)
        } else if size32 == 0 {
            ((bytes.len() - offset) as u64, 8_usize)
        } else {
            (size32, 8_usize)
        };
        if size < header_len as u64 {
            return None;
        }
        let end = offset.checked_add(usize::try_from(size).ok()?)?;
        Some(Self {
            name,
            header_end: offset + header_len,
            end,
        })
    }
}

fn parse_mvhd_duration(bytes: &[u8]) -> Option<f64> {
    let version = *bytes.first()?;
    match version {
        0 => {
            let timescale = f64::from(read_u32(bytes, 12)?);
            let duration = f64::from(read_u32(bytes, 16)?);
            valid_scaled_duration(duration, timescale)
        }
        1 => {
            let timescale = f64::from(read_u32(bytes, 20)?);
            let duration = u64_to_f64(read_u64(bytes, 24)?);
            valid_scaled_duration(duration, timescale)
        }
        _ => None,
    }
}

#[allow(clippy::cast_precision_loss)]
fn u64_to_f64(value: u64) -> f64 {
    value as f64
}

fn valid_scaled_duration(duration: f64, timescale: f64) -> Option<f64> {
    if duration.is_finite() && timescale.is_finite() && duration > 0.0 && timescale > 0.0 {
        Some(duration / timescale)
    } else {
        None
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_be_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}
