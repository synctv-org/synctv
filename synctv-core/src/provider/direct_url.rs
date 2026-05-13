//! Direct URL `MediaProvider`
//!
//! Provides direct playback for HTTP(S) URLs

use super::{
    proxy::{ProviderProxy, ProxyAction, ProxyRequestContext},
    store::{ProviderStoreExt, VersionedPlayback},
    MediaProvider, PlaybackInfo, PlaybackResult, ProviderContext, ProviderError, SourceConfig,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use url::{Host, Url};

/// Direct URL `MediaProvider`
pub struct DirectUrlProvider {
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
}

impl DirectUrlProvider {
    /// Provider type name constant.
    pub const NAME: &'static str = "direct_url";

    #[must_use]
    pub fn new() -> Self {
        Self::new_with_ssrf_guard(synctv_common::ssrf::SsrfGuard::strict_policy())
    }

    #[must_use]
    pub const fn new_with_ssrf_guard(ssrf_guard: synctv_common::ssrf::SsrfGuard) -> Self {
        Self { ssrf_guard }
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
        let path = Url::parse(url).map_or_else(|_| url.to_string(), |u| u.path().to_string());

        let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
        match ext.as_str() {
            "m3u8" => "m3u8",
            "flv" => "flv",
            "mp4" | "m4v" | "mov" => "mp4",
            "mkv" => "mkv",
            "webm" => "webm",
            "avi" => "avi",
            _ => "video",
        }
        .to_string()
    }

    fn validate_source_url(
        url: &str,
        guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Url, ProviderError> {
        let parsed = Url::parse(url).map_err(|error| {
            ProviderError::InvalidConfig(format!("DirectUrl URL is invalid: {error}"))
        })?;

        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(ProviderError::InvalidConfig(
                "DirectUrl only supports http:// and https:// schemes".to_string(),
            ));
        }

        match parsed.host() {
            Some(Host::Domain(host)) if guard.is_host_blocked(host) => {
                return Err(ProviderError::InvalidConfig(format!(
                    "DirectUrl host '{host}' is blocked by SSRF policy"
                )));
            }
            Some(Host::Ipv4(ip)) if guard.is_ip_blocked(&ip.into()) => {
                return Err(ProviderError::InvalidConfig(format!(
                    "DirectUrl IP '{ip}' is blocked by SSRF policy"
                )));
            }
            Some(Host::Ipv6(ip)) if guard.is_ip_blocked(&ip.into()) => {
                return Err(ProviderError::InvalidConfig(format!(
                    "DirectUrl IP '{ip}' is blocked by SSRF policy"
                )));
            }
            Some(_) => {}
            None => {
                return Err(ProviderError::InvalidConfig(
                    "DirectUrl URL must include a host".to_string(),
                ));
            }
        }

        if let Some(port) = parsed.port_or_known_default() {
            if let Some(acl) = guard.acl() {
                if acl.is_port_allowed(port).is_denied() {
                    return Err(ProviderError::InvalidConfig(format!(
                        "DirectUrl port '{port}' is not allowed"
                    )));
                }
            }
        }

        Ok(parsed)
    }

    fn playback_cache_key(config: &DirectUrlSourceConfig) -> String {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(config.url.as_bytes());
        hasher.update(b"\0");
        hasher.update(if config.proxy { b"1" } else { b"0" });
        hasher.update(b"\0");

        let mut header_entries: Vec<_> = config.headers.iter().collect();
        header_entries.sort_unstable_by(|(left_name, left_value), (right_name, right_value)| {
            left_name
                .cmp(right_name)
                .then_with(|| left_value.cmp(right_value))
        });

        for (name, value) in header_entries {
            hasher.update(name.as_bytes());
            hasher.update(b"=");
            hasher.update(value.as_bytes());
            hasher.update(b"\0");
        }

        let cache_key_suffix: String = hex::encode(hasher.finalize()).chars().take(16).collect();
        format!("playback:{cache_key_suffix}")
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
// Supported sub_paths (same pattern as other providers):
// - `{version}/stream` — proxy the video stream
// - `{version}/m3u8` — proxy M3U8 with URL rewriting
// - `{version}/subtitle/{mode}/{index}` — proxy a subtitle track for a mode
#[async_trait]
impl ProviderProxy for DirectUrlProvider {
    async fn resolve_proxy(
        &self,
        ctx: &ProxyRequestContext<'_>,
    ) -> Result<ProxyAction, ProviderError> {
        let sub_path = ctx.sub_path;

        if let Some((version, rest)) = sub_path.split_once('/') {
            let versioned =
                super::proxy::lookup_versioned(ctx.store, version, ctx.request_context).await?;

            if let Some(subtitle_path) = rest.strip_prefix("subtitle/") {
                let (playback_info, index_str) =
                    if let Some((mode_name, index_str)) = subtitle_path.split_once('/') {
                        (
                            versioned
                                .result
                                .playback_infos
                                .get(mode_name)
                                .ok_or(ProviderError::NotFound)?,
                            index_str,
                        )
                    } else {
                        (
                            versioned
                                .result
                                .playback_infos
                                .get(&versioned.result.default_mode)
                                .ok_or(ProviderError::NotFound)?,
                            subtitle_path,
                        )
                    };
                let Ok(index) = index_str.parse::<usize>() else {
                    return Err(ProviderError::NotFound);
                };
                let subtitle = playback_info
                    .subtitles
                    .get(index)
                    .ok_or(ProviderError::NotFound)?;

                return Ok(ProxyAction::FetchAndForward {
                    url: subtitle.url.clone(),
                    headers: super::subtitle_headers_for_proxy(&playback_info.headers, subtitle),
                    range_header: None,
                });
            }

            if let Some(stream_path) = rest.strip_prefix("stream/") {
                let (playback_info, index_str) =
                    if let Some((mode_name, index_str)) = stream_path.split_once('/') {
                        (
                            versioned
                                .result
                                .playback_infos
                                .get(mode_name)
                                .ok_or(ProviderError::NotFound)?,
                            index_str,
                        )
                    } else {
                        (
                            versioned
                                .result
                                .playback_infos
                                .get(&versioned.result.default_mode)
                                .ok_or(ProviderError::NotFound)?,
                            stream_path,
                        )
                    };
                let Ok(index) = index_str.parse::<usize>() else {
                    return Err(ProviderError::NotFound);
                };
                let url = playback_info
                    .urls
                    .get(index)
                    .ok_or(ProviderError::NotFound)?;

                return Ok(ProxyAction::FetchAndForward {
                    url: url.clone(),
                    headers: playback_info.headers.clone(),
                    range_header: super::proxy::selected_range_header(ctx),
                });
            }

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
                        range_header: super::proxy::selected_range_header(ctx),
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
                        proxy_url_claims: ctx.verified_claims.cloned(),
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
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        super::reject_source_config_provider_instance_name(source_config.value(), "DirectUrl")?;

        let config = DirectUrlSourceConfig::try_from(source_config.value())?;

        Self::validate_source_url(&config.url, &self.ssrf_guard)?;

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
        super::reject_source_config_provider_instance_name(source_config, "DirectUrl")?;

        let config = DirectUrlSourceConfig::try_from(source_config)?;
        Self::validate_source_url(&config.url, &self.ssrf_guard)?;

        let cache_key = Self::playback_cache_key(&config);
        let cache_ttl = Duration::from_hours(1); // 1 hour for direct URLs

        let store = _ctx.store.as_ref();

        // Check cache
        if let Some(store) = store {
            if let Ok(Some(cached)) = store.get::<VersionedPlayback>(&cache_key).await {
                if !cached.is_expired() {
                    return super::maybe_sign_cached_versioned_playback(cached, Self::NAME, _ctx)
                        .await;
                }
            }
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

        super::finalize_versioned_playback(result, Self::NAME, &cache_key, cache_ttl, _ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::store::InMemoryProviderStore;
    use std::sync::Arc;

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

    #[tokio::test]
    async fn test_generate_playback_rejects_embedded_playback_result_source_config() {
        let provider = DirectUrlProvider::new();
        let source_config = serde_json::json!({
            "playback_infos": {
                "direct": {
                    "urls": [{
                        "name": "primary",
                        "url": "https://example.com/video-primary.mp4",
                        "headers": {
                            "Authorization": "Bearer token"
                        }
                    }],
                    "default_url_index": 0,
                    "subtitles": [],
                    "default_subtitle_index": null,
                    "danmakus": [],
                    "format": "mp4"
                },
            },
            "default_mode": "direct",
            "metadata": {}
        });

        let err = provider
            .generate_playback(&ProviderContext::new("synctv"), &source_config)
            .await
            .expect_err("embedded playback result source_config should be rejected");

        assert!(
            err.to_string().contains("DirectUrl"),
            "error should come from normal DirectUrl source_config parsing, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_generate_playback_rejects_rtmp_urls() {
        let provider = DirectUrlProvider::new();
        let source_config = serde_json::json!({
            "url": "rtmp://live.example.com/app/stream-key"
        });

        let err = provider
            .generate_playback(&ProviderContext::new("synctv"), &source_config)
            .await
            .expect_err("DirectUrl must reject RTMP URLs");

        assert!(matches!(
            err,
            ProviderError::InvalidConfig(ref msg)
                if msg.contains("DirectUrl only supports http:// and https:// schemes")
        ));
    }

    #[tokio::test]
    async fn test_validate_source_config_allows_blocked_hosts_and_ips_when_ssrf_is_explicitly_disabled(
    ) {
        let provider =
            DirectUrlProvider::new_with_ssrf_guard(synctv_common::ssrf::SsrfGuard::disabled());
        let ctx = ProviderContext::new("synctv");

        for url in [
            "http://localhost/video.mp4",
            "http://127.0.0.1/video.mp4",
            "http://[::1]/video.mp4",
        ] {
            provider
                .validate_source_config(&ctx, SourceConfig::media(&json!({ "url": url })))
                .await
                .expect("disabled SSRF policy should allow blocked hosts and IP literals");
        }
    }

    #[tokio::test]
    async fn test_validate_source_config_allows_non_default_ports_when_ssrf_is_explicitly_disabled()
    {
        let provider =
            DirectUrlProvider::new_with_ssrf_guard(synctv_common::ssrf::SsrfGuard::disabled());
        provider
            .validate_source_config(
                &ProviderContext::new("synctv"),
                SourceConfig::media(&json!({ "url": "http://example.com:8080/video.mp4" })),
            )
            .await
            .expect("disabled SSRF policy should allow non-default ports");
    }

    #[tokio::test]
    async fn test_validate_source_config_rejects_provider_instance_name() {
        let provider = DirectUrlProvider::new();
        let err = provider
            .validate_source_config(
                &ProviderContext::new("synctv"),
                SourceConfig::media(&json!({
                    "url": "https://example.com/video.mp4",
                    "provider_instance_name": "remote-direct"
                })),
            )
            .await
            .expect_err("DirectUrl source_config must not contain provider_instance_name");

        assert!(matches!(
            err,
            ProviderError::InvalidConfig(ref msg)
                if msg.contains("top-level provider_instance_name")
        ));
    }

    #[tokio::test]
    async fn test_generate_playback_rejects_provider_instance_name() {
        let provider = DirectUrlProvider::new();
        let err = provider
            .generate_playback(
                &ProviderContext::new("synctv"),
                &json!({
                    "url": "https://example.com/video.mp4",
                    "provider_instance_name": "remote-direct"
                }),
            )
            .await
            .expect_err(
                "DirectUrl playback must not accept provider_instance_name in source_config",
            );

        assert!(matches!(
            err,
            ProviderError::InvalidConfig(ref msg)
                if msg.contains("top-level provider_instance_name")
        ));
    }

    #[tokio::test]
    async fn test_generate_playback_allows_blocked_hosts_when_ssrf_is_explicitly_disabled() {
        let provider =
            DirectUrlProvider::new_with_ssrf_guard(synctv_common::ssrf::SsrfGuard::disabled());
        let result = provider
            .generate_playback(
                &ProviderContext::new("synctv"),
                &json!({ "url": "http://localhost/video.mp4" }),
            )
            .await
            .expect("disabled SSRF policy should allow blocked hosts");
        assert_eq!(
            result.playback_infos["direct"].urls,
            vec!["http://localhost/video.mp4"]
        );
    }

    #[tokio::test]
    async fn test_generate_playback_cache_key_includes_headers() {
        let provider = DirectUrlProvider::new();
        let store = Arc::new(InMemoryProviderStore::new(128));
        let ctx = ProviderContext::new("synctv").with_store(store);

        let first = provider
            .generate_playback(
                &ctx,
                &json!({
                    "url": "https://cdn.example.com/video.mp4",
                    "headers": {
                        "Referer": "https://site-a.example"
                    }
                }),
            )
            .await
            .expect("first playback should be cached");

        let second = provider
            .generate_playback(
                &ctx,
                &json!({
                    "url": "https://cdn.example.com/video.mp4",
                    "headers": {
                        "Referer": "https://site-b.example"
                    }
                }),
            )
            .await
            .expect("second playback should not reuse mismatched cached headers");

        assert_eq!(
            first.playback_infos["direct"].headers.get("Referer"),
            Some(&"https://site-a.example".to_string())
        );
        assert_eq!(
            second.playback_infos["direct"].headers.get("Referer"),
            Some(&"https://site-b.example".to_string())
        );
    }
}
