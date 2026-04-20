//! `LiveProxy` `MediaProvider`
//!
//! Provides playback URLs for live streams sourced from external URLs.
//! The external source URL is stored in `source_config`, and playback URLs
//! point to synctv's own HTTP-FLV and HLS endpoints (same as `RtmpProvider`).
//!
//! The `PullStreamManager` handles the actual pulling from the external source.

use super::{
    proxy::{ProviderProxy, ProxyAction, ProxyRequestContext},
    store::VersionedPlayback,
    MediaProvider, PlaybackResult, ProviderContext, ProviderError,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

/// `LiveProxy` `MediaProvider`
///
/// Generates playback URLs for live streams from external sources.
/// The external URL is stored in `source_config.url` and validated on creation.
/// Playback URLs point to synctv's own HLS/FLV endpoints.
pub struct LiveProxyProvider {}

impl Default for LiveProxyProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveProxyProvider {
    pub const NAME: &'static str = "live_proxy";

    pub const fn new() -> Self {
        Self {}
    }

    fn validate_live_source_url(url: &str) -> Result<(), ProviderError> {
        // Validate URL format (only RTMP and HTTP-FLV are supported for pulling).
        // Use URL path parsing to avoid false positives from `.flv` appearing
        // in query parameters or other URL parts.
        let is_rtmp = url.starts_with("rtmp://");
        let parsed_url = url::Url::parse(url).ok();
        let is_flv = parsed_url
            .as_ref()
            .map_or_else(|| url.ends_with(".flv"), |u| u.path().ends_with(".flv"));
        if !is_rtmp && !is_flv {
            return Err(ProviderError::InvalidConfig(format!(
                "Unsupported source URL format: {url}. Expected rtmp:// or *.flv"
            )));
        }

        if let Some(parsed_url) = parsed_url {
            let host = parsed_url.host_str().ok_or_else(|| {
                ProviderError::InvalidConfig("LiveProxy source URL is missing a host".to_string())
            })?;
            let guard = synctv_common::ssrf::SsrfGuard::shared_default();

            if guard.is_host_blocked(host) {
                return Err(ProviderError::InvalidConfig(format!(
                    "LiveProxy source host '{host}' is blocked by SSRF policy"
                )));
            }

            if let Ok(ip) = host.parse::<std::net::IpAddr>() {
                if guard.is_ip_blocked(&ip) {
                    return Err(ProviderError::InvalidConfig(format!(
                        "LiveProxy source IP '{ip}' is blocked by SSRF policy"
                    )));
                }
            }
        }

        Ok(())
    }

    fn build_live_proxy_action(
        rest: &str,
        versioned: &VersionedPlayback,
        verified_claims: Option<&crate::service::proxy_signature::ProxyUrlClaims>,
    ) -> Result<ProxyAction, ProviderError> {
        let room_id = versioned
            .result
            .metadata
            .get("room_id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| ProviderError::ApiError("Live playback missing room_id".into()))?;
        let media_id = versioned
            .result
            .metadata
            .get("media_id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| ProviderError::ApiError("Live playback missing media_id".into()))?;

        match rest {
            "stream" => {
                let claims = verified_claims.ok_or_else(|| {
                    ProviderError::ApiError("Missing verified proxy claims".into())
                })?;
                Ok(ProxyAction::LiveFlv {
                    provider_name: Self::NAME.to_string(),
                    room_id: room_id.to_string(),
                    media_id: media_id.to_string(),
                    user_id: claims.user_id.clone(),
                    expires_at: claims.expires_at,
                })
            }
            "m3u8" => Ok(ProxyAction::LiveHlsPlaylist {
                provider_name: Self::NAME.to_string(),
                room_id: room_id.to_string(),
                media_id: media_id.to_string(),
                version: versioned.version.clone(),
            }),
            segment if segment.starts_with("segment/") => {
                let segment_name = segment.trim_start_matches("segment/");
                let disguised_as_png = segment_name.ends_with(".png");
                Ok(ProxyAction::LiveHlsSegment {
                    room_id: room_id.to_string(),
                    media_id: media_id.to_string(),
                    segment_name: segment_name.to_string(),
                    disguised_as_png,
                })
            }
            _ => Err(ProviderError::NotFound),
        }
    }
}

#[async_trait]
impl MediaProvider for LiveProxyProvider {
    fn name(&self) -> &'static str {
        Self::NAME
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
        Self::validate_live_source_url(source_url)?;

        let mut result = super::build_live_playback(media_id, room_id);
        let redacted_host = url::Url::parse(source_url)
            .ok()
            .and_then(|u| u.host_str().map(String::from))
            .unwrap_or_else(|| "unknown".to_string());
        result
            .metadata
            .insert("source_host".to_string(), json!(redacted_host));
        result
            .metadata
            .insert("provider".to_string(), json!("live_proxy"));

        let cache_key = format!("playback:{room_id}:{media_id}");
        let cache_ttl = Duration::from_mins(5);
        super::finalize_versioned_playback(result, Self::NAME, &cache_key, cache_ttl, _ctx).await
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

        Self::validate_live_source_url(url)
    }

    fn as_provider_proxy(&self) -> Option<&dyn ProviderProxy> {
        Some(self)
    }
}

#[async_trait]
impl ProviderProxy for LiveProxyProvider {
    async fn resolve_proxy(
        &self,
        ctx: &ProxyRequestContext<'_>,
    ) -> Result<ProxyAction, ProviderError> {
        let (version, rest) = ctx
            .sub_path
            .split_once('/')
            .ok_or(ProviderError::NotFound)?;
        let versioned =
            super::proxy::lookup_versioned(ctx.store, version, ctx.request_context).await?;
        Self::build_live_proxy_action(rest, &versioned, ctx.verified_claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_live_proxy_metadata_does_not_expose_source_url() {
        let provider = LiveProxyProvider::new();
        let ctx = ProviderContext::new("test");

        let source_config = json!({
            "url": "rtmp://secret-internal-server.local/live/stream-key",
            "room_id": "room-123",
            "media_id": "media-456"
        });

        let result = provider
            .generate_playback(&ctx, &source_config)
            .await
            .unwrap();

        // The metadata must NOT contain the full source URL.
        // Exposing it would leak internal infrastructure URLs to clients.
        assert!(
            !result.metadata.contains_key("source_url"),
            "Metadata must not contain 'source_url' key (leaks internal URL)"
        );

        // If there is any URL-like info, it should be at most the hostname
        for (key, value) in &result.metadata {
            if let Some(s) = value.as_str() {
                assert!(
                    !s.contains("secret-internal-server.local/live/stream-key"),
                    "Metadata key '{key}' contains full source URL path, which leaks internal info"
                );
            }
        }
    }

    #[test]
    fn test_live_proxy_provider_can_be_constructed_without_base_url() {
        let provider = LiveProxyProvider::new();
        let _ = provider;
    }

    #[tokio::test]
    async fn test_live_proxy_metadata_contains_provider_tag() {
        let provider = LiveProxyProvider::new();
        let ctx = ProviderContext::new("test");

        let source_config = json!({
            "url": "rtmp://example.com/live/stream",
            "room_id": "room-1",
            "media_id": "media-1"
        });

        let result = provider
            .generate_playback(&ctx, &source_config)
            .await
            .unwrap();
        assert_eq!(
            result.metadata.get("provider").and_then(|v| v.as_str()),
            Some("live_proxy"),
            "Metadata should still contain provider tag"
        );
    }

    #[tokio::test]
    async fn generate_playback_signs_urls_with_provider_proxy_prefix() {
        use crate::provider::store::InMemoryProviderStore;
        use crate::service::ProxySigningKey;
        use std::sync::Arc;

        let provider = LiveProxyProvider::new();
        let signing_key = ProxySigningKey::derive_from(b"test-jwt-secret-that-is-long-enough");
        let ctx = ProviderContext::new("synctv")
            .with_user_id("user1")
            .with_room_id("room1")
            .with_signing_key(&signing_key)
            .with_store(Arc::new(InMemoryProviderStore::new(16)));
        let result = provider
            .generate_playback(
                &ctx,
                &json!({"room_id": "room1", "media_id": "media1", "url": "rtmp://example.com/live/stream"}),
            )
            .await
            .unwrap();

        let hls = result
            .playback_infos
            .get("hls")
            .unwrap()
            .urls
            .first()
            .unwrap();
        let flv = result
            .playback_infos
            .get("flv")
            .unwrap()
            .urls
            .first()
            .unwrap();
        assert!(hls.starts_with("/api/providers/proxy/live_proxy/"));
        assert!(hls.contains("/m3u8?"));
        assert!(flv.starts_with("/api/providers/proxy/live_proxy/"));
        assert!(flv.contains("/stream?"));
    }

    #[tokio::test]
    async fn test_live_proxy_validate_source_config_rejects_blocked_hosts() {
        let provider = LiveProxyProvider::new();
        let ctx = ProviderContext::new("test");

        for config in [
            json!({
                "url": "rtmp://localhost/live/stream",
                "room_id": "room-123",
                "media_id": "media-456"
            }),
            json!({
                "url": "http://127.0.0.1/live/stream.flv",
                "room_id": "room-123",
                "media_id": "media-456"
            }),
        ] {
            let err = provider
                .validate_source_config(&ctx, &config)
                .await
                .expect_err("blocked live source URLs must be rejected at validation time");
            assert!(matches!(
                err,
                ProviderError::InvalidConfig(ref msg) if msg.contains("blocked by SSRF policy")
            ));
        }
    }
}
