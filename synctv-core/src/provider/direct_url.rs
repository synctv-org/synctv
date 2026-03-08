//! Direct URL `MediaProvider`
//!
//! Provides direct playback for HTTP(S) URLs

use super::{
    proxy::{ProviderProxy, ProxyAction, ProxyRequestContext},
    store::{ProviderStoreExt, VersionedPlayback},
    MediaProvider, PlaybackInfo, PlaybackResult, ProviderContext, ProviderError,
};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

/// Direct URL `MediaProvider`
pub struct DirectUrlProvider {}

impl DirectUrlProvider {
    /// Provider type name constant.
    pub const NAME: &'static str = "direct_url";

    #[must_use]
    pub const fn new() -> Self {
        Self {}
    }

    /// Forbidden header names that must not be set via user-supplied config.
    ///
    /// These headers can be exploited for request smuggling, credential injection,
    /// or SSRF amplification if user-controlled.
    const FORBIDDEN_HEADERS: &[&str] = &[
        "host",
        "authorization",
        "cookie",
        "transfer-encoding",
        "content-length",
        "connection",
        "upgrade",
        "proxy-authorization",
        "proxy-connection",
        "te",
        "trailer",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-real-ip",
        "x-original-url",
        "x-rewrite-url",
    ];

    /// Validate that custom headers do not include forbidden header names.
    fn validate_headers(headers: &HashMap<String, String>) -> Result<(), ProviderError> {
        for key in headers.keys() {
            let lower = key.to_lowercase();

            // Check exact forbidden headers list
            if Self::FORBIDDEN_HEADERS.contains(&lower.as_str()) {
                return Err(ProviderError::InvalidConfig(format!(
                    "DirectUrl header '{key}' is forbidden for security reasons"
                )));
            }

            // Block all Sec-* prefix headers (HTTP/3 security headers, Client Hints, WebSocket headers)
            if lower.starts_with("sec-") {
                return Err(ProviderError::InvalidConfig(format!(
                    "DirectUrl header '{key}' is forbidden (Sec- prefix blocked for security)"
                )));
            }

            // Block Priority header (HTTP/3 prioritization)
            if lower == "priority" {
                return Err(ProviderError::InvalidConfig(format!(
                    "DirectUrl header '{key}' is forbidden for security reasons"
                )));
            }
        }
        Ok(())
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

// ProviderProxy implementation for DirectUrl
//
// Supported sub_paths (same pattern as other providers):
// - `{version}/stream` — proxy the video stream
// - `{version}/m3u8` — proxy M3U8 with URL rewriting
#[async_trait]
impl ProviderProxy for DirectUrlProvider {
    async fn resolve_proxy(
        &self,
        ctx: &ProxyRequestContext<'_>,
    ) -> Result<ProxyAction, ProviderError> {
        let sub_path = ctx.sub_path;

        if let Some((version, rest)) = sub_path.split_once('/') {
            let versioned = super::proxy::lookup_versioned(ctx.store, version).await?;
            let default_info = versioned
                .result
                .playback_infos
                .get(&versioned.result.default_mode)
                .ok_or(ProviderError::NotFound)?;
            let url = default_info.urls.first().ok_or(ProviderError::NotFound)?;

            match rest {
                "stream" => {
                    return Ok(ProxyAction::FetchAndForward {
                        url: url.clone(),
                        headers: default_info.headers.clone(),
                    });
                }
                "m3u8" => {
                    // Propagate HMAC signature into M3U8 segment URLs
                    let proxy_base = if let Some(claims) = ctx.verified_claims {
                        let signed_query = ctx.services.signing_key.build_signed_query(claims);
                        format!("{}/{version}?{signed_query}", ctx.proxy_base)
                    } else {
                        format!("{}/{version}", ctx.proxy_base)
                    };
                    return Ok(ProxyAction::M3u8Rewrite {
                        url: url.clone(),
                        headers: default_info.headers.clone(),
                        proxy_base,
                    });
                }
                _ => {}
            }
        }

        Err(ProviderError::NotFound)
    }
}

#[async_trait]
impl MediaProvider for DirectUrlProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn as_provider_proxy(&self) -> Option<&dyn ProviderProxy> {
        Some(self)
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<(), ProviderError> {
        let config = DirectUrlSourceConfig::try_from(source_config)?;

        // Validate URL scheme: only allow http(s)
        if !config.url.starts_with("http://") && !config.url.starts_with("https://") {
            return Err(ProviderError::InvalidConfig(
                "DirectUrl only supports http:// and https:// schemes".to_string(),
            ));
        }

        // Validate custom headers: reject forbidden header names that could be
        // used for request smuggling or credential injection.
        Self::validate_headers(&config.headers)?;

        Ok(())
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        let config = DirectUrlSourceConfig::try_from(source_config)?;

        // Build cache key from URL hash
        let cache_key = {
            use sha2::{Digest, Sha256};
            let url_hash: String = format!("{:x}", Sha256::digest(config.url.as_bytes()))
                .chars()
                .take(16)
                .collect();
            format!("playback:{url_hash}")
        };
        let cache_ttl = Duration::from_hours(1); // 1 hour for direct URLs

        let store = _ctx.store.as_ref();

        // Check cache
        if let Some(store) = store {
            if let Ok(Some(cached)) = store.get::<VersionedPlayback>(&cache_key).await {
                if !cached.is_expired() {
                    return Ok(super::maybe_sign_versioned_playback(
                        cached.result,
                        Self::NAME,
                        &cached.version,
                        cached.expires_at,
                        _ctx,
                    ));
                }
            }
        }

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

        if let Some(filename) = config.url.split('/').next_back() {
            metadata.insert("filename".to_string(), json!(filename));
        }

        let result = PlaybackResult {
            playback_infos,
            default_mode: "direct".to_string(),
            metadata,
        };

        // Store with version
        let version = nanoid::nanoid!(16);
        let expires_at = Utc::now().timestamp() + cache_ttl.as_secs() as i64;
        let versioned = VersionedPlayback {
            version: version.clone(),
            result: result.clone(),
            expires_at,
        };
        if let Some(store) = store {
            let _ = store.set(&cache_key, &versioned, cache_ttl).await;
            let _ = store
                .set(&format!("v:{version}"), &versioned, cache_ttl)
                .await;
        }

        // Sign playback URLs when signing_key and identity are available
        Ok(super::maybe_sign_versioned_playback(
            result,
            Self::NAME,
            &version,
            expires_at,
            _ctx,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_forbidden_headers_rejected() {
        let mut headers = HashMap::new();
        headers.insert("Host".to_string(), "evil.com".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());

        let mut headers = HashMap::new();
        headers.insert("authorization".to_string(), "Bearer token".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());

        let mut headers = HashMap::new();
        headers.insert("Cookie".to_string(), "session=abc".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());

        let mut headers = HashMap::new();
        headers.insert("Transfer-Encoding".to_string(), "chunked".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());
    }

    #[test]
    fn test_allowed_headers_accepted() {
        let mut headers = HashMap::new();
        headers.insert("Referer".to_string(), "https://example.com".to_string());
        headers.insert("User-Agent".to_string(), "MyPlayer/1.0".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_ok());
    }

    #[test]
    fn test_forbidden_headers_x_forwarded_proto() {
        let mut headers = HashMap::new();
        headers.insert("X-Forwarded-Proto".to_string(), "https".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());
    }

    #[test]
    fn test_forbidden_headers_x_original_url() {
        let mut headers = HashMap::new();
        headers.insert(
            "X-Original-URL".to_string(),
            "http://internal.host/secret".to_string(),
        );
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());
    }

    #[test]
    fn test_forbidden_headers_x_rewrite_url() {
        let mut headers = HashMap::new();
        headers.insert("X-Rewrite-URL".to_string(), "/admin".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());
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

    #[test]
    fn test_forbidden_sec_prefix_headers() {
        // Sec-CH-UA (Client Hints)
        let mut headers = HashMap::new();
        headers.insert("Sec-CH-UA".to_string(), "\"Chrome\";v=\"93\"".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());

        // Sec-CH-UA-Mobile
        let mut headers = HashMap::new();
        headers.insert("Sec-CH-UA-Mobile".to_string(), "?0".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());

        // Sec-CH-UA-Platform
        let mut headers = HashMap::new();
        headers.insert("Sec-CH-UA-Platform".to_string(), "\"Windows\"".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());

        // Sec-Fetch-Site
        let mut headers = HashMap::new();
        headers.insert("Sec-Fetch-Site".to_string(), "cross-site".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());

        // Sec-Fetch-Mode
        let mut headers = HashMap::new();
        headers.insert("Sec-Fetch-Mode".to_string(), "cors".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());

        // Sec-Fetch-User
        let mut headers = HashMap::new();
        headers.insert("Sec-Fetch-User".to_string(), "?1".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());

        // Sec-Fetch-Dest
        let mut headers = HashMap::new();
        headers.insert("Sec-Fetch-Dest".to_string(), "video".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());

        // Sec-WebSocket-Key (HTTP/3 WebSockets)
        let mut headers = HashMap::new();
        headers.insert(
            "Sec-WebSocket-Key".to_string(),
            "dGhlIHNhbXBsZSBub25jZQ==".to_string(),
        );
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());

        // Sec-WebSocket-Version
        let mut headers = HashMap::new();
        headers.insert("Sec-WebSocket-Version".to_string(), "13".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());

        // Sec-WebSocket-Protocol
        let mut headers = HashMap::new();
        headers.insert("Sec-WebSocket-Protocol".to_string(), "chat".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());
    }

    #[test]
    fn test_forbidden_priority_header() {
        // Priority header (HTTP/3)
        let mut headers = HashMap::new();
        headers.insert("Priority".to_string(), "u=5, i".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());
    }

    #[test]
    fn test_forbidden_headers_case_insensitive() {
        // Mixed case Sec- headers should still be blocked
        let mut headers = HashMap::new();
        headers.insert("sec-ch-ua".to_string(), "\"Chrome\"".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());

        // Weird case Priority header
        let mut headers = HashMap::new();
        headers.insert("PRIORITY".to_string(), "u=5".to_string());
        assert!(DirectUrlProvider::validate_headers(&headers).is_err());
    }
}
