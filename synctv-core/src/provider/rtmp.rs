//! RTMP `MediaProvider`
//!
//! Provides playback URLs for RTMP live streams.
//! URLs point to synctv's own HTTP-FLV and HLS endpoints.

use super::{
    MediaProvider, PlaybackResult, ProviderContext, ProviderError,
};
use async_trait::async_trait;
use serde_json::Value;

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

        Ok(super::build_live_playback(&self.base_url, media_id, room_id))
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

        Ok(())
    }

    fn cache_key(&self, ctx: &ProviderContext<'_>, source_config: &Value) -> String {
        let room_id = source_config.get("room_id").and_then(|v| v.as_str());
        let media_id = source_config.get("media_id").and_then(|v| v.as_str());

        match (room_id, media_id) {
            (Some(room_id), Some(media_id)) => {
                // Normal case: both fields present
                format!("{}:playback:rtmp:{room_id}:{media_id}", ctx.key_prefix)
            }
            _ => {
                // Fallback: include hash of source_config to ensure uniqueness
                // This prevents cache collisions when room_id or media_id are missing
                use sha2::{Digest, Sha256};
                let config_hash = hex::encode(Sha256::digest(source_config.to_string().as_bytes()));
                format!("{}:playback:rtmp:fallback:{config_hash}", ctx.key_prefix)
            }
        }
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

    #[test]
    fn test_cache_key_with_all_fields() {
        let provider = RtmpProvider::default();
        let ctx = create_context();
        let source_config = json!({
            "room_id": "room123",
            "media_id": "media456"
        });

        let key = provider.cache_key(&ctx, &source_config);
        assert_eq!(key, "synctv:playback:rtmp:room123:media456");
    }

    #[test]
    fn test_cache_key_missing_room_id() {
        let provider = RtmpProvider::default();
        let ctx = create_context();
        let source_config = json!({
            "media_id": "media456"
        });

        let key = provider.cache_key(&ctx, &source_config);
        // Should use fallback with hash
        assert!(key.starts_with("synctv:playback:rtmp:fallback:"));
        // Hash should be deterministic
        let key2 = provider.cache_key(&ctx, &source_config);
        assert_eq!(key, key2);
    }

    #[test]
    fn test_cache_key_missing_media_id() {
        let provider = RtmpProvider::default();
        let ctx = create_context();
        let source_config = json!({
            "room_id": "room123"
        });

        let key = provider.cache_key(&ctx, &source_config);
        // Should use fallback with hash
        assert!(key.starts_with("synctv:playback:rtmp:fallback:"));
    }

    #[test]
    fn test_cache_key_missing_both_fields() {
        let provider = RtmpProvider::default();
        let ctx = create_context();
        let source_config = json!({});

        let key = provider.cache_key(&ctx, &source_config);
        // Should use fallback with hash
        assert!(key.starts_with("synctv:playback:rtmp:fallback:"));
    }

    #[test]
    fn test_cache_key_no_collision_for_different_missing_configs() {
        let provider = RtmpProvider::default();
        let ctx = create_context();

        // Two configs with different missing fields should produce different keys
        let config1 = json!({"room_id": "room1"});
        let config2 = json!({"room_id": "room2"});

        let key1 = provider.cache_key(&ctx, &config1);
        let key2 = provider.cache_key(&ctx, &config2);

        // Both should use fallback, but with different hashes
        assert_ne!(key1, key2, "Different configs should produce different cache keys");
    }

    #[test]
    fn test_cache_key_complete_vs_missing_produces_different_keys() {
        let provider = RtmpProvider::default();
        let ctx = create_context();

        // Complete config should produce a direct key
        let complete_config = json!({
            "room_id": "room123",
            "media_id": "media456"
        });

        // Incomplete config should produce a fallback key
        let incomplete_config = json!({
            "room_id": "room123"
        });

        let complete_key = provider.cache_key(&ctx, &complete_config);
        let incomplete_key = provider.cache_key(&ctx, &incomplete_config);

        // Keys should have different formats
        assert!(complete_key.starts_with("synctv:playback:rtmp:room123:"));
        assert!(incomplete_key.starts_with("synctv:playback:rtmp:fallback:"));
        assert_ne!(complete_key, incomplete_key);
    }

    #[test]
    fn test_cache_key_is_deterministic() {
        let provider = RtmpProvider::default();
        let ctx = create_context();
        let source_config = json!({
            "room_id": "room123",
            "media_id": "media456",
            "extra_field": "ignored"
        });

        let key1 = provider.cache_key(&ctx, &source_config);
        let key2 = provider.cache_key(&ctx, &source_config);
        assert_eq!(key1, key2, "Cache key should be deterministic");
    }
}
