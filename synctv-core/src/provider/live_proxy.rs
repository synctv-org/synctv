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

        // Validate URL format (only RTMP and HTTP-FLV are supported for pulling)
        if !url.starts_with("rtmp://")
            && !url.ends_with(".flv")
            && !url.contains(".flv?")
        {
            return Err(ProviderError::InvalidConfig(format!(
                "Unsupported source URL format: {url}. Expected rtmp:// or *.flv"
            )));
        }

        // SSRF protection: validate the host is not a private/internal address
        validate_source_url_host(url).await?;

        Ok(())
    }

    fn cache_key(&self, _ctx: &ProviderContext<'_>, source_config: &Value) -> String {
        let room_id = source_config
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let media_id = source_config
            .get("media_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        format!("live_proxy:{room_id}:{media_id}")
    }
}

/// Validate that a source URL's host is not a private/internal address (SSRF protection).
///
/// Supports `rtmp://`, `http://`, and `https://` schemes. Strips `rtmp://` prefix and
/// parses the host portion to check against private IP ranges and well-known internal hostnames.
///
/// In addition to static hostname/IP checks, performs **async DNS resolution** to guard
/// against DNS rebinding attacks where a public-looking domain resolves to a private IP.
async fn validate_source_url_host(raw: &str) -> Result<(), ProviderError> {
    // For RTMP URLs, extract host and port from rtmp://host:port/app/stream format
    let (host_str, port) = if let Some(rest) = raw.strip_prefix("rtmp://") {
        let authority = rest.split('/').next().unwrap_or(rest);
        if let Some((host, port_str)) = authority.rsplit_once(':') {
            (host, port_str.parse::<u16>().unwrap_or(1935))
        } else {
            (authority, 1935u16)
        }
    } else if let Ok(parsed) = url::Url::parse(raw) {
        match parsed.host_str() {
            Some(host) => {
                let port = parsed.port().unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });
                // check_host_not_internal borrows the parsed URL's host_str, so
                // run the static + DNS check inline here to avoid lifetime issues.
                check_host_not_internal(host)?;
                return resolve_and_check_dns(host, port).await;
            }
            None => return Err(ProviderError::InvalidConfig("URL has no host".to_string())),
        };
    } else {
        return Err(ProviderError::InvalidConfig(format!("Cannot parse URL: {raw}")));
    };

    check_host_not_internal(host_str)?;
    resolve_and_check_dns(host_str, port).await
}

fn check_host_not_internal(host: &str) -> Result<(), ProviderError> {
    // Block well-known internal hostnames
    if matches!(
        host,
        "localhost"
            | "metadata.google.internal"
            | "instance-data"
            | "metadata"
            | "kubernetes.default"
            | "kubernetes.default.svc"
    ) {
        return Err(ProviderError::InvalidConfig(
            "Source URL targets an internal host".to_string(),
        ));
    }

    // Check IP addresses against private ranges using the authoritative SSRF validator
    if let Ok(ip) = host.parse::<IpAddr>() {
        if crate::validation::is_private_ip(&ip) {
            return Err(ProviderError::InvalidConfig(
                "Source URL targets a private IP address".to_string(),
            ));
        }
    }

    Ok(())
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
