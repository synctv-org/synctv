//! `LiveProxy` `MediaProvider`
//!
//! Provides playback URLs for live streams sourced from external URLs.
//! The external source URL is stored in `source_config`, and playback URLs
//! point to synctv's own HTTP-FLV and HLS endpoints (same as `RtmpProvider`).
//!
//! The `PullStreamManager` handles the actual pulling from the external source.

use super::{
    MediaProvider, PlaybackResult, ProviderContext, ProviderError,
};
use crate::validation::{validate_url_for_ssrf, ValidationError};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::net::IpAddr;

/// `LiveProxy` `MediaProvider`
///
/// Generates playback URLs for live streams from external sources.
/// The external URL is stored in `source_config.url` and validated on creation.
/// Playback URLs point to synctv's own HLS/FLV endpoints.
pub struct LiveProxyProvider {
    base_url: String,
}

impl LiveProxyProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

impl Default for LiveProxyProvider {
    fn default() -> Self {
        Self::new("https://localhost:8080")
    }
}

#[async_trait]
impl MediaProvider for LiveProxyProvider {
    fn name(&self) -> &'static str {
        "live_proxy"
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        let media_id = source_config
            .get("media_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing media_id".to_string()))?;

        let room_id = source_config
            .get("room_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing room_id".to_string()))?;

        let source_url = source_config
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing url".to_string()))?;

        let mut result = super::build_live_playback(&self.base_url, media_id, room_id);
        result.metadata.insert("source_url".to_string(), json!(source_url));
        result.metadata.insert("provider".to_string(), json!("live_proxy"));

        Ok(result)
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<(), ProviderError> {
        // Validate required fields
        let url = source_config
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing url".to_string()))?;

        source_config
            .get("room_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing room_id".to_string()))?;

        source_config
            .get("media_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing media_id".to_string()))?;

        // Validate URL format (only RTMP and HTTP-FLV are supported for pulling).
        // Use URL path parsing to avoid false positives from `.flv` appearing
        // in query parameters or other URL parts.
        let is_rtmp = url.starts_with("rtmp://");
        let is_flv = url::Url::parse(url).map_or_else(|_| url.ends_with(".flv"), |u| u.path().ends_with(".flv"));
        if !is_rtmp && !is_flv {
            return Err(ProviderError::InvalidConfig(format!(
                "Unsupported source URL format: {url}. Expected rtmp:// or *.flv"
            )));
        }

        // SSRF protection: validate the host is not a private/internal address
        validate_source_url_host(url).await?;

        Ok(())
    }

    fn cache_key(&self, ctx: &ProviderContext<'_>, source_config: &Value) -> String {
        let room_id = source_config
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let media_id = source_config
            .get("media_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        format!("{}:playback:live_proxy:{room_id}:{media_id}", ctx.key_prefix)
    }
}

/// Validate that a source URL's host is not a private/internal address (SSRF protection).
///
/// Supports `rtmp://`, `http://`, and `https://` schemes.
/// For HTTP(S) URLs, delegates to the shared `validate_url_for_ssrf` which covers
/// hostname blocklists, IP range checks, and cloud metadata endpoints.
/// For RTMP URLs (not parseable by `url::Url`), extracts the host manually and
/// performs static + DNS resolution checks.
async fn validate_source_url_host(raw: &str) -> Result<(), ProviderError> {
    // For HTTP(S) URLs, use the shared comprehensive SSRF validator
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return validate_url_for_ssrf(raw).map_err(|e| match e {
            ValidationError::SSRF(msg) => {
                ProviderError::InvalidConfig(format!("SSRF protection: {msg}"))
            }
            _ => ProviderError::InvalidConfig(e.to_string()),
        });
    }

    // For RTMP URLs, extract host and port from rtmp://host:port/app/stream format
    let (host_str, port) = if let Some(rest) = raw.strip_prefix("rtmp://") {
        let authority = rest.split('/').next().unwrap_or(rest);
        if let Some((host, port_str)) = authority.rsplit_once(':') {
            (host, port_str.parse::<u16>().unwrap_or(1935))
        } else {
            (authority, 1935u16)
        }
    } else {
        return Err(ProviderError::InvalidConfig(format!("Cannot parse URL: {raw}")));
    };

    check_host_not_internal(host_str)?;
    resolve_and_check_dns(host_str, port).await
}

/// Check a hostname/IP against the shared SSRF blocklists.
///
/// Uses `synctv_media_providers::ssrf` (via `crate::validation`) for comprehensive
/// hostname and IP range checks including cloud metadata endpoints.
fn check_host_not_internal(host: &str) -> Result<(), ProviderError> {
    // Check IP addresses against private ranges using the authoritative SSRF validator
    if let Ok(ip) = host.parse::<IpAddr>() {
        if crate::validation::is_private_ip(&ip) {
            return Err(ProviderError::InvalidConfig(
                "Source URL targets a private IP address".to_string(),
            ));
        }
        return Ok(());
    }

    // Check hostnames against shared blocklist (covers localhost, metadata endpoints, etc.)
    use synctv_media_providers::ssrf::{check_hostname, SsrfCheckResult};
    match check_hostname(host) {
        SsrfCheckResult::Ok => Ok(()),
        SsrfCheckResult::Blocked(reason) => Err(ProviderError::InvalidConfig(
            format!("Source URL targets a blocked host: {reason}"),
        )),
    }
}

/// Perform async DNS resolution and reject any address that resolves to a private IP.
///
/// This prevents DNS rebinding attacks where a domain passes static hostname
/// checks but resolves to a private/internal IP address at query time.
async fn resolve_and_check_dns(host: &str, port: u16) -> Result<(), ProviderError> {
    // Skip DNS resolution for literal IP addresses (already validated above)
    if host.parse::<IpAddr>().is_ok() {
        return Ok(());
    }

    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| ProviderError::InvalidConfig(format!("DNS lookup failed for {host}: {e}")))?;

    let mut found = false;
    for addr in addrs {
        if crate::validation::is_private_ip(&addr.ip()) {
            return Err(ProviderError::InvalidConfig(format!(
                "Hostname {host} resolves to private/reserved IP {}",
                addr.ip()
            )));
        }
        found = true;
    }

    if !found {
        return Err(ProviderError::InvalidConfig(format!(
            "Hostname {host} resolved to no addresses"
        )));
    }

    Ok(())
}
