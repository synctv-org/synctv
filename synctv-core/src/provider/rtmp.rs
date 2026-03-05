//! RTMP `MediaProvider`
//!
//! Provides playback URLs for RTMP live streams.
//! URLs point to synctv's own HTTP-FLV and HLS endpoints.

use super::{
    store::{ProviderStoreExt, VersionedPlayback},
    MediaProvider, PlaybackResult, ProviderContext, ProviderError,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use std::time::Duration;

/// Fields that should not be allowed in `source_config`.
/// `RtmpProvider` only uses `room_id` and `media_id`; any URL field could be abused.
const FORBIDDEN_URL_FIELDS: &[&str] = &[
    "url",
    "rtmp_url",
    "rtmps_url",
    "source_url",
    "stream_url",
    "external_url",
];

/// RTMP `MediaProvider`
pub struct RtmpProvider {
    base_url: String,
}

impl RtmpProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

impl Default for RtmpProvider {
    fn default() -> Self {
        Self::new("https://localhost:8080")
    }
}

#[async_trait]
impl MediaProvider for RtmpProvider {
    fn name(&self) -> &'static str {
        "rtmp"
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

        let result = super::build_live_playback(&self.base_url, media_id, room_id);

        // Store with version for proxy URL identity
        let store = _ctx.store.as_ref();
        let cache_key = format!("playback:{room_id}:{media_id}");
        let cache_ttl = Duration::from_mins(5); // 5 minutes for live
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
        source_config
            .get("room_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing room_id".to_string()))?;

        source_config
            .get("media_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing media_id".to_string()))?;

        // SSRF protection: reject any URL fields in source_config
        // RtmpProvider only uses room_id and media_id; URL fields are not supported
        // and could indicate an attempt to inject external URLs for SSRF attacks.
        for field in FORBIDDEN_URL_FIELDS {
            if source_config.get(field).is_some() {
                return Err(ProviderError::InvalidConfig(format!(
                    "Field '{field}' is not supported. RtmpProvider does not accept external URLs."
                )));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_context() -> ProviderContext<'static> {
        ProviderContext::new("synctv")
            .with_user_id("test_user")
            .with_room_id("test_room")
    }

    #[tokio::test]
    async fn test_validate_source_config_rejects_url_fields() {
        let provider = RtmpProvider::new("https://example.com");
        let ctx = create_context();

        let configs_with_urls = vec![
            json!({"room_id": "r1", "media_id": "m1", "url": "http://evil.com"}),
            json!({"room_id": "r1", "media_id": "m1", "rtmp_url": "rtmp://evil.com"}),
            json!({"room_id": "r1", "media_id": "m1", "source_url": "http://evil.com"}),
        ];

        for config in configs_with_urls {
            let result = provider.validate_source_config(&ctx, &config).await;
            assert!(
                result.is_err(),
                "validate_source_config should reject URL fields: {config}"
            );
        }
    }

    #[tokio::test]
    async fn test_validate_source_config_accepts_valid_config() {
        let provider = RtmpProvider::new("https://example.com");
        let ctx = create_context();

        let valid_config = json!({
            "room_id": "room123",
            "media_id": "media456"
        });

        let result = provider.validate_source_config(&ctx, &valid_config).await;
        assert!(
            result.is_ok(),
            "validate_source_config should accept valid config: {:?}",
            result.err()
        );
    }
}
