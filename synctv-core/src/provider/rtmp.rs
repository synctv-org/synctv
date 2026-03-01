//! RTMP `MediaProvider`
//!
//! Provides playback URLs for RTMP live streams.
//! URLs point to synctv's own HTTP-FLV and HLS endpoints.
//!
//! # SSRF Protection
//!
//! The `new_validated()` constructor validates the `base_url` against private/internal
//! IP ranges to prevent Server-Side Request Forgery attacks. Use this constructor
//! when the `base_url` comes from an untrusted source (e.g., user configuration).

use super::{MediaProvider, PlaybackResult, ProviderContext, ProviderError};
use crate::validation::{validate_url_for_ssrf, ValidationError};
use async_trait::async_trait;
use serde_json::Value;

/// Fields that should not be allowed in `source_config` to prevent SSRF.
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
    /// Create a new `RtmpProvider` without SSRF validation.
    ///
    /// Use this when the `base_url` is trusted (e.g., from server configuration).
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    /// Create a new `RtmpProvider` with SSRF validation.
    ///
    /// Validates that the `base_url` does not point to a private/internal IP address
    /// or blocked hostname. Use this when the `base_url` may come from an untrusted source.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The URL cannot be parsed
    /// - The URL points to a private IP address (192.168.x.x, 10.x.x.x, 172.16-31.x.x, etc.)
    /// - The URL points to localhost or loopback
    /// - The URL points to a link-local address (169.254.x.x)
    /// - The URL points to a blocked hostname (metadata endpoints, etc.)
    pub fn new_validated(base_url: &str) -> Result<Self, ProviderError> {
        validate_url_for_ssrf(base_url).map_err(|e| match e {
            ValidationError::SSRF(msg) => {
                ProviderError::InvalidConfig(format!("SSRF protection: {msg}"))
            }
            _ => ProviderError::InvalidConfig(e.to_string()),
        })?;

        Ok(Self {
            base_url: base_url.to_string(),
        })
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

        Ok(super::build_live_playback(
            &self.base_url,
            media_id,
            room_id,
        ))
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

    fn cache_key(&self, ctx: &ProviderContext<'_>, source_config: &Value) -> String {
        let room_id = source_config.get("room_id").and_then(|v| v.as_str());
        let media_id = source_config.get("media_id").and_then(|v| v.as_str());

        if let (Some(room_id), Some(media_id)) = (room_id, media_id) {
            // Normal case: both fields present
            format!("{}:playback:rtmp:{room_id}:{media_id}", ctx.key_prefix)
        } else {
            // Fallback: include hash of source_config to ensure uniqueness
            // This prevents cache collisions when room_id or media_id are missing
            use sha2::{Digest, Sha256};
            let config_hash = hex::encode(Sha256::digest(source_config.to_string().as_bytes()));
            format!("{}:playback:rtmp:fallback:{config_hash}", ctx.key_prefix)
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
        assert_ne!(
            key1, key2,
            "Different configs should produce different cache keys"
        );
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

    // ========== SSRF Protection Tests ==========

    #[test]
    fn test_new_validated_rejects_private_ipv4() {
        let private_ips = vec![
            "http://192.168.1.1:8080",
            "http://10.0.0.1:8080",
            "http://172.16.0.1:8080",
            "http://127.0.0.1:8080",
        ];

        for url in private_ips {
            let result = RtmpProvider::new_validated(url);
            assert!(
                result.is_err(),
                "new_validated should reject private IP: {url}"
            );
        }
    }

    #[test]
    fn test_new_validated_rejects_localhost() {
        let localhost_urls = vec!["http://localhost:8080", "https://localhost:8080"];

        for url in localhost_urls {
            let result = RtmpProvider::new_validated(url);
            assert!(
                result.is_err(),
                "new_validated should reject localhost: {url}"
            );
        }
    }

    #[test]
    fn test_new_validated_accepts_public_urls() {
        let public_urls = vec!["https://example.com", "http://93.184.216.34:8080"];

        for url in public_urls {
            let result = RtmpProvider::new_validated(url);
            assert!(
                result.is_ok(),
                "new_validated should accept public URL: {url}, error: {:?}",
                result.err()
            );
        }
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
