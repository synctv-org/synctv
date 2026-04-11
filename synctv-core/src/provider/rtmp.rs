//! RTMP `MediaProvider`
//!
//! Provides playback URLs for RTMP live streams.
//! URLs point to synctv's own HTTP-FLV and HLS endpoints.

use super::{
    proxy::{ProviderProxy, ProxyAction, ProxyRequestContext},
    store::VersionedPlayback,
    MediaProvider, PlaybackResult, ProviderContext, ProviderError,
};
use async_trait::async_trait;
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
pub struct RtmpProvider {}

impl RtmpProvider {
    pub const NAME: &'static str = "rtmp";

    pub const fn new() -> Self {
        Self {}
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

impl Default for RtmpProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MediaProvider for RtmpProvider {
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

        let result = super::build_live_playback(media_id, room_id);

        let cache_key = format!("playback:{room_id}:{media_id}");
        let cache_ttl = Duration::from_mins(5); // 5 minutes for live
        super::finalize_versioned_playback(result, Self::NAME, &cache_key, cache_ttl, _ctx).await
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

    fn as_provider_proxy(&self) -> Option<&dyn ProviderProxy> {
        Some(self)
    }
}

#[async_trait]
impl ProviderProxy for RtmpProvider {
    async fn resolve_proxy(
        &self,
        ctx: &ProxyRequestContext<'_>,
    ) -> Result<ProxyAction, ProviderError> {
        let (version, rest) = ctx
            .sub_path
            .split_once('/')
            .ok_or(ProviderError::NotFound)?;
        let versioned = super::proxy::lookup_versioned(ctx.store, version).await?;
        Self::build_live_proxy_action(rest, &versioned, ctx.verified_claims)
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
        let provider = RtmpProvider::new();
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
        let provider = RtmpProvider::new();
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

    #[tokio::test]
    async fn generate_playback_signs_urls_with_provider_proxy_prefix() {
        use crate::provider::store::InMemoryProviderStore;
        use crate::service::ProxySigningKey;
        use std::sync::Arc;

        let provider = RtmpProvider::new();
        let signing_key = ProxySigningKey::derive_from(b"test-jwt-secret-that-is-long-enough");
        let ctx = ProviderContext::new("synctv")
            .with_user_id("user1")
            .with_room_id("room1")
            .with_signing_key(&signing_key)
            .with_store(Arc::new(InMemoryProviderStore::new(16)));
        let result = provider
            .generate_playback(&ctx, &json!({"room_id": "room1", "media_id": "media1"}))
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
        assert!(hls.starts_with("/api/providers/proxy/rtmp/"));
        assert!(hls.contains("/m3u8?"));
        assert!(flv.starts_with("/api/providers/proxy/rtmp/"));
        assert!(flv.contains("/stream?"));
    }

    #[tokio::test]
    async fn cached_playback_is_resigned_for_current_identity() {
        use crate::provider::store::InMemoryProviderStore;
        use crate::service::ProxySigningKey;
        use std::sync::Arc;

        let provider = RtmpProvider::new();
        let store = Arc::new(InMemoryProviderStore::new(16));
        let signing_key = ProxySigningKey::derive_from(b"test-jwt-secret-that-is-long-enough");

        let ctx1 = ProviderContext::new("synctv")
            .with_user_id("user1")
            .with_room_id("room1")
            .with_signing_key(&signing_key)
            .with_store(store.clone());
        let first = provider
            .generate_playback(&ctx1, &json!({"room_id": "room1", "media_id": "media1"}))
            .await
            .unwrap();

        let ctx2 = ProviderContext::new("synctv")
            .with_user_id("user2")
            .with_room_id("room1")
            .with_signing_key(&signing_key)
            .with_store(store);
        let second = provider
            .generate_playback(&ctx2, &json!({"room_id": "room1", "media_id": "media1"}))
            .await
            .unwrap();

        let first_hls = first
            .playback_infos
            .get("hls")
            .unwrap()
            .urls
            .first()
            .unwrap();
        let second_hls = second
            .playback_infos
            .get("hls")
            .unwrap()
            .urls
            .first()
            .unwrap();
        assert_ne!(
            first_hls, second_hls,
            "cached playback must be re-signed per user"
        );
        assert!(
            second_hls.contains("uid=user2")
                || second_hls.contains("user_id=user2")
                || second_hls.contains("sig=")
        );
    }

    #[tokio::test]
    async fn resolve_proxy_flv_includes_verified_identity_and_expiry() {
        use crate::provider::store::VersionedPlayback;
        use crate::service::proxy_signature::ProxyUrlClaims;
        use std::collections::HashMap;

        let versioned = VersionedPlayback {
            version: "v1".to_string(),
            result: PlaybackResult {
                playback_infos: HashMap::new(),
                default_mode: "hls".to_string(),
                metadata: HashMap::from([
                    ("room_id".to_string(), json!("room1")),
                    ("media_id".to_string(), json!("media1")),
                ]),
            },
            expires_at: chrono::Utc::now().timestamp() + 60,
        };
        let claims = ProxyUrlClaims {
            provider: "rtmp".to_string(),
            version: "v1".to_string(),
            room_id: "room1".to_string(),
            user_id: "user1".to_string(),
            expires_at: chrono::Utc::now().timestamp() + 30,
        };
        let action =
            RtmpProvider::build_live_proxy_action("stream", &versioned, Some(&claims)).unwrap();
        match action {
            ProxyAction::LiveFlv {
                user_id,
                expires_at,
                ..
            } => {
                assert_eq!(user_id, "user1");
                assert_eq!(expires_at, claims.expires_at);
            }
            other => panic!("expected LiveFlv action, got {other:?}"),
        }
    }

    #[test]
    fn resolve_proxy_flv_requires_verified_claims() {
        use crate::provider::store::VersionedPlayback;
        use std::collections::HashMap;

        let versioned = VersionedPlayback {
            version: "v1".to_string(),
            result: PlaybackResult {
                playback_infos: HashMap::new(),
                default_mode: "hls".to_string(),
                metadata: HashMap::from([
                    ("room_id".to_string(), json!("room1")),
                    ("media_id".to_string(), json!("media1")),
                ]),
            },
            expires_at: chrono::Utc::now().timestamp() + 60,
        };

        let err = RtmpProvider::build_live_proxy_action("stream", &versioned, None).unwrap_err();
        assert!(matches!(err, ProviderError::ApiError(_)));
    }
}
