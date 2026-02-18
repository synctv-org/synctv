//! Direct URL `MediaProvider`
//!
//! Provides direct playback for HTTP(S) URLs

use super::{MediaProvider, PlaybackInfo, PlaybackResult, ProviderContext, ProviderError};
use crate::validation::{validate_url_for_ssrf, ValidationError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Direct URL `MediaProvider`
pub struct DirectUrlProvider {}

impl DirectUrlProvider {
    #[must_use]
    pub const fn new() -> Self {
        Self {}
    }

    /// Validate that a URL does not target internal/private network addresses (SSRF protection).
    fn validate_url_not_internal(raw: &str) -> Result<(), ProviderError> {
        validate_url_for_ssrf(raw).map_err(|e| {
            match e {
                ValidationError::SSRF(msg) => {
                    ProviderError::InvalidUrl(format!("SSRF protection: {msg}"))
                }
                _ => ProviderError::InvalidUrl(e.to_string()),
            }
        })
    }

    /// Detect format from URL path extension.
    ///
    /// Parses the URL to extract the path component, then checks the file
    /// extension. This avoids false positives from `contains()` matching
    /// against query parameters or hostnames (e.g., "cdn.flv.com/video").
    fn detect_format(url: &str) -> String {
        let path = url::Url::parse(url).map_or_else(|_| url.to_string(), |u| u.path().to_string());

        let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
        match ext.as_str() {
            "m3u8" => "m3u8",
            "flv" => "flv",
            "mp4" | "m4v" => "mp4",
            "mkv" => "mkv",
            "webm" => "webm",
            "avi" => "avi",
            "mov" => "mp4",
            _ => "video",
        }
        .to_string()
    }
}

impl Default for DirectUrlProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// `DirectUrl` source configuration
#[derive(Debug, Deserialize, Serialize)]
struct DirectUrlSourceConfig {
    url: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    proxy: bool,
}

impl TryFrom<&Value> for DirectUrlSourceConfig {
    type Error = ProviderError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        super::parse_source_config(value, "DirectUrl")
    }
}

#[async_trait]
impl MediaProvider for DirectUrlProvider {
    fn name(&self) -> &'static str {
        "direct_url"
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        let config = DirectUrlSourceConfig::try_from(source_config)?;

        // Validate URL scheme: only allow http(s) and rtmp(s)
        if !config.url.starts_with("http://")
            && !config.url.starts_with("https://")
            && !config.url.starts_with("rtmp://")
            && !config.url.starts_with("rtmps://")
        {
            return Err(ProviderError::InvalidConfig(
                "URL must use http, https, rtmp, or rtmps scheme".to_string(),
            ));
        }

        // SSRF protection: reject URLs targeting private/internal networks
        if config.url.starts_with("http://") || config.url.starts_with("https://") {
            Self::validate_url_not_internal(&config.url)?;
        }

        let format = Self::detect_format(&config.url);

        let mut playback_infos = HashMap::new();
        playback_infos.insert(
            "direct".to_string(),
            PlaybackInfo {
                urls: vec![config.url.clone()],
                format: format.clone(),
                headers: config.headers,
                subtitles: Vec::new(),
                expires_at: None,
                cors_proxy_required: false,
            },
        );

        let mut metadata = HashMap::new();
        metadata.insert("format".to_string(), json!(format));
        metadata.insert("is_live".to_string(), json!(false));
        metadata.insert("proxy".to_string(), json!(config.proxy));

        // Extract filename from URL
        if let Some(filename) = config.url.split('/').next_back() {
            metadata.insert("filename".to_string(), json!(filename));
        }

        Ok(PlaybackResult {
            playback_infos,
            default_mode: "direct".to_string(),
            metadata,
        })
    }

    fn cache_key(&self, _ctx: &ProviderContext<'_>, source_config: &Value) -> String {
        if let Ok(config) = DirectUrlSourceConfig::try_from(source_config) {
            {
                use sha2::{Sha256, Digest};
                format!("direct_url:{:x}", Sha256::digest(config.url.as_bytes()))
            }
        } else {
            "direct_url:unknown".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssrf_blocks_localhost() {
        let result = DirectUrlProvider::validate_url_not_internal("http://localhost/secret");
        assert!(result.is_err());
    }

    #[test]
    fn test_ssrf_blocks_private_ipv4() {
        // 10.x.x.x
        assert!(DirectUrlProvider::validate_url_not_internal("http://10.0.0.1/path").is_err());
        // 172.16.x.x
        assert!(DirectUrlProvider::validate_url_not_internal("http://172.16.0.1/path").is_err());
        // 192.168.x.x
        assert!(DirectUrlProvider::validate_url_not_internal("http://192.168.1.1/path").is_err());
        // 127.x.x.x
        assert!(DirectUrlProvider::validate_url_not_internal("http://127.0.0.1/path").is_err());
        // 0.0.0.0
        assert!(DirectUrlProvider::validate_url_not_internal("http://0.0.0.0/path").is_err());
        // link-local
        assert!(
            DirectUrlProvider::validate_url_not_internal("http://169.254.169.254/latest/meta-data")
                .is_err()
        );
        // CGNAT
        assert!(DirectUrlProvider::validate_url_not_internal("http://100.64.0.1/path").is_err());
    }

    #[test]
    fn test_ssrf_blocks_metadata_endpoints() {
        assert!(
            DirectUrlProvider::validate_url_not_internal("http://metadata.google.internal/v1")
                .is_err()
        );
        assert!(
            DirectUrlProvider::validate_url_not_internal("http://instance-data/latest").is_err()
        );
    }

    #[test]
    fn test_ssrf_blocks_ipv6_loopback() {
        assert!(DirectUrlProvider::validate_url_not_internal("http://[::1]/path").is_err());
    }

    #[test]
    fn test_ssrf_allows_public_urls() {
        assert!(DirectUrlProvider::validate_url_not_internal("https://example.com/video.mp4").is_ok());
        assert!(
            DirectUrlProvider::validate_url_not_internal("https://cdn.example.com/stream.m3u8")
                .is_ok()
        );
        assert!(
            DirectUrlProvider::validate_url_not_internal("http://93.184.216.34/video.mp4").is_ok()
        );
    }

    #[test]
    fn test_detect_format() {
        assert_eq!(
            DirectUrlProvider::detect_format("http://example.com/video.mp4"),
            "mp4"
        );
        assert_eq!(
            DirectUrlProvider::detect_format("http://example.com/stream.m3u8"),
            "m3u8"
        );
        assert_eq!(
            DirectUrlProvider::detect_format("http://example.com/stream.flv"),
            "flv"
        );
        assert_eq!(
            DirectUrlProvider::detect_format("http://example.com/video"),
            "video"
        );
    }

    #[test]
    fn test_detect_format_ignores_query_params() {
        // Previously contains(".flv") would false-positive on query params or hostnames
        assert_eq!(
            DirectUrlProvider::detect_format("http://cdn.flv.com/video"),
            "video"
        );
        assert_eq!(
            DirectUrlProvider::detect_format("http://example.com/stream?file=test.mp4&token=abc"),
            "video" // extension is on the query param, not the path
        );
        assert_eq!(
            DirectUrlProvider::detect_format("http://example.com/video.mp4?quality=high"),
            "mp4"
        );
    }
}
