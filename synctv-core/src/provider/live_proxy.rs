//! `LiveProxy` `MediaProvider`
//!
//! Provides playback URLs for live streams sourced from external URLs.
//! The external source URL is stored in `source_config`, while the internal
//! room/media binding comes from the runtime provider context. Playback URLs
//! point to synctv's own HTTP-FLV and HLS endpoints (same as `RtmpProvider`).
//!
//! The `PullStreamManager` handles the actual pulling from the external source.

use super::{
    proxy::{ProviderProxy, ProxyAction, ProxyRequestContext},
    store::VersionedPlayback,
    MediaProvider, PlaybackResult, ProviderContext, ProviderError,
};
use crate::models::{MediaId, RoomId};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

/// `LiveProxy` `MediaProvider`
///
/// Generates playback URLs for live streams from external sources.
/// The external URL is stored in `source_config.url` and validated on creation.
/// Playback URLs point to synctv's own HLS/FLV endpoints. Internal room/media
/// identity is injected at playback time through `ProviderContext`.
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

    fn resolve_live_binding<'a>(
        ctx: &'a ProviderContext<'a>,
    ) -> Result<(&'a RoomId, &'a MediaId), ProviderError> {
        let room_id = ctx.room_id().ok_or_else(|| {
            ProviderError::InvalidConfig(
                "Missing room_id in provider context for live proxy playback".to_string(),
            )
        })?;

        let media_id = ctx.media_id().ok_or_else(|| {
            ProviderError::InvalidConfig(
                "Missing media_id in provider context for live proxy playback".to_string(),
            )
        })?;

        Ok((room_id, media_id))
    }

    fn validate_config_shape(source_config: &Value) -> Result<(), ProviderError> {
        super::reject_source_config_provider_instance_name(source_config, "LiveProxy")?;

        for field in [
            "room_id",
            "media_id",
            "rtmp_url",
            "source_url",
            "stream_url",
        ] {
            if source_config.get(field).is_some() {
                return Err(ProviderError::InvalidConfig(format!(
                    "Field '{field}' is not supported. Live proxy source_config only accepts 'url'; internal room/media identity comes from runtime context."
                )));
            }
        }

        Ok(())
    }

    fn build_proxy_action(
        rest: &str,
        versioned: &VersionedPlayback,
        ctx: &ProxyRequestContext<'_>,
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
        let room_id = super::proxy::parse_proxy_room_id(
            &ctx.services.public_id_codec,
            room_id,
            "live playback metadata",
        )?;
        let media_id = super::proxy::parse_proxy_media_id(
            &ctx.services.public_id_codec,
            media_id,
            "live playback metadata",
        )?;

        match rest {
            stream if stream == "stream" || stream.starts_with("stream/") => {
                let claims = ctx.verified_claims.ok_or_else(|| {
                    ProviderError::ApiError("Missing verified proxy claims".into())
                })?;
                Ok(ProxyAction::LiveFlv {
                    provider_name: Self::NAME.to_string(),
                    room_id,
                    media_id,
                    user_id: super::proxy::parse_proxy_user_id(
                        &ctx.services.public_id_codec,
                        &claims.user_id,
                        "live proxy claims",
                    )?,
                    expires_at: claims.expires_at,
                })
            }
            "m3u8" => Ok(ProxyAction::LiveHlsPlaylist {
                provider_name: Self::NAME.to_string(),
                room_id,
                media_id,
                version: versioned.version.clone(),
            }),
            segment if segment.starts_with("segment/") => {
                let segment_name = segment.trim_start_matches("segment/");
                let disguised_as_png = segment_name.ends_with(".png");
                Ok(ProxyAction::LiveHlsSegment {
                    room_id,
                    media_id,
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
        ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        Self::validate_config_shape(source_config)?;
        let (room_id, media_id) = Self::resolve_live_binding(ctx)?;

        let source_url = source_config
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing url".to_string()))?;
        Self::validate_live_source_url(source_url)?;

        let mut result = super::build_live_playback(*media_id, *room_id);
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
        super::finalize_versioned_playback(result, Self::NAME, &cache_key, cache_ttl, ctx).await
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<(), ProviderError> {
        Self::validate_config_shape(source_config)?;

        // Validate required fields
        let url = source_config
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing url".to_string()))?;

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
        Self::build_proxy_action(rest, &versioned, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MediaId, RoomId, UserId};
    use serde_json::json;

    #[tokio::test]
    async fn test_live_proxy_metadata_does_not_expose_source_url() {
        let provider = LiveProxyProvider::new();
        let ctx = ProviderContext::new("test")
            .with_room_id(RoomId::from(10))
            .with_media_id(MediaId::from(100));

        let source_config = json!({
            "url": "rtmp://secret-internal-server.local/live/stream-key"
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
        let ctx = ProviderContext::new("test")
            .with_room_id(RoomId::from(10))
            .with_media_id(MediaId::from(100));

        let source_config = json!({
            "url": "rtmp://example.com/live/stream"
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
            .with_user_id(UserId::from(1))
            .with_room_id(RoomId::from(10))
            .with_media_id(MediaId::from(100))
            .with_signing_key(&signing_key)
            .with_store(Arc::new(InMemoryProviderStore::new(16)));
        let result = provider
            .generate_playback(&ctx, &json!({"url": "rtmp://example.com/live/stream"}))
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
        let flv_url = url::Url::parse(&format!("http://synctv.local{flv}")).unwrap();
        assert!(flv_url
            .path_segments()
            .unwrap()
            .nth(5)
            .is_some_and(|action| action == "stream" || action.starts_with("stream%2F")));
        assert!(flv_url.query_pairs().any(|(key, _)| key == "sig"));
    }

    #[tokio::test]
    async fn test_live_proxy_validate_source_config_allows_blocked_hosts_when_default_ssrf_is_disabled(
    ) {
        let provider = LiveProxyProvider::new();
        let ctx = ProviderContext::new("test");

        for config in [
            json!({
                "url": "rtmp://localhost/live/stream"
            }),
            json!({
                "url": "http://127.0.0.1/live/stream.flv"
            }),
        ] {
            provider
                .validate_source_config(&ctx, &config)
                .await
                .expect("default SSRF policy should allow blocked live source URLs");
        }
    }

    #[tokio::test]
    async fn test_live_proxy_validate_source_config_rejects_internal_identity_fields() {
        let provider = LiveProxyProvider::new();
        let ctx = ProviderContext::new("test");

        let err = provider
            .validate_source_config(
                &ctx,
                &json!({"url": "rtmp://example.com/live/stream", "room_id": "room-123"}),
            )
            .await
            .expect_err("live_proxy source_config must not persist internal identity");
        assert!(matches!(
            err,
            ProviderError::InvalidConfig(ref msg) if msg.contains("runtime context")
        ));
    }

    #[tokio::test]
    async fn test_live_proxy_validate_source_config_rejects_provider_instance_name() {
        let provider = LiveProxyProvider::new();
        let ctx = ProviderContext::new("test");

        let err = provider
            .validate_source_config(
                &ctx,
                &json!({
                    "url": "rtmp://example.com/live/stream",
                    "provider_instance_name": "remote-live"
                }),
            )
            .await
            .expect_err("live_proxy source_config must not contain provider_instance_name");
        assert!(matches!(
            err,
            ProviderError::InvalidConfig(ref msg)
                if msg.contains("top-level provider_instance_name")
        ));
    }

    #[tokio::test]
    async fn test_live_proxy_generate_playback_requires_runtime_binding() {
        let provider = LiveProxyProvider::new();
        let ctx = ProviderContext::new("test");

        let err = provider
            .generate_playback(&ctx, &json!({"url": "rtmp://example.com/live/stream"}))
            .await
            .expect_err("live proxy playback must fail closed without room/media binding");
        assert!(matches!(
            err,
            ProviderError::InvalidConfig(ref msg) if msg.contains("provider context")
        ));
    }
}
