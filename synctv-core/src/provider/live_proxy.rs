//! `LiveProxy` `MediaProvider`
//!
//! Provides playback URLs for live streams sourced from external URLs.
//! The external source URL is stored in `source_config`, and playback URLs
//! point to synctv's own HTTP-FLV and HLS endpoints (same as `RtmpProvider`).
//!
//! The `PullStreamManager` handles the actual pulling from the external source.

use super::{
    store::{ProviderStoreExt, VersionedPlayback},
    MediaProvider, PlaybackResult, ProviderContext, ProviderError,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use std::time::Duration;

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

        // Store with version for proxy URL identity
        let store = _ctx.store.as_ref();
        let cache_key = format!("playback:{room_id}:{media_id}");
        let cache_ttl = Duration::from_secs(300);
        let version = nanoid::nanoid!(16);
        let versioned = VersionedPlayback {
            version: version.clone(),
            result: result.clone(),
            expires_at: Utc::now().timestamp() + cache_ttl.as_secs() as i64,
        };
        if let Some(store) = store {
            let _ = store.set(&cache_key, &versioned, cache_ttl).await;
            let _ = store
                .set(&format!("v:{version}"), &versioned, cache_ttl)
                .await;
        }

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
        let is_flv = url::Url::parse(url)
            .map_or_else(|_| url.ends_with(".flv"), |u| u.path().ends_with(".flv"));
        if !is_rtmp && !is_flv {
            return Err(ProviderError::InvalidConfig(format!(
                "Unsupported source URL format: {url}. Expected rtmp:// or *.flv"
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ========== B9: LiveProxy metadata must not expose source URL ==========

    #[tokio::test]
    async fn test_live_proxy_metadata_does_not_expose_source_url() {
        let provider = LiveProxyProvider::new("https://localhost:8080");
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
    fn test_live_proxy_provider_requires_explicit_base_url() {
        // L14: LiveProxyProvider must NOT implement Default.
        // It must always be constructed with an explicit base URL via new().
        let provider = LiveProxyProvider::new("https://my-server.example.com:9090");
        assert_eq!(provider.base_url, "https://my-server.example.com:9090");
    }

    #[tokio::test]
    async fn test_live_proxy_metadata_contains_provider_tag() {
        let provider = LiveProxyProvider::new("https://localhost:8080");
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
}
