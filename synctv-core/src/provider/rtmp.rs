//! RTMP `MediaProvider`
//!
//! Provides HTTP-FLV and HLS playback URLs for SyncTV live streams published over RTMP.
//! URLs point to synctv's own HTTP-FLV and HLS endpoints.

use super::{
    proxy::{ProviderProxy, ProxyAction, ProxyRequestContext},
    store::VersionedPlayback,
    MediaProvider, PlaybackResult, ProviderContext, ProviderError, SourceConfig,
};
use crate::models::{MediaId, RoomId};
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

/// Fields that should not be allowed in `source_config`.
/// `RtmpProvider` only serves the current SyncTV media from runtime context.
/// Any external URL field could be abused.
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

    fn resolve_live_binding<'a>(
        ctx: &'a ProviderContext<'a>,
    ) -> Result<(&'a RoomId, &'a MediaId), ProviderError> {
        let room_id = ctx.room_id().ok_or_else(|| {
            ProviderError::InvalidConfig(
                "Missing room_id in provider context for live stream playback".to_string(),
            )
        })?;

        let media_id = ctx.media_id().ok_or_else(|| {
            ProviderError::InvalidConfig(
                "Missing media_id in provider context for live stream playback".to_string(),
            )
        })?;

        Ok((room_id, media_id))
    }

    fn validate_config_fields(source_config: &Value) -> Result<(), ProviderError> {
        // SSRF protection: reject any URL fields in source_config.
        // RtmpProvider only accepts synctv-managed live stream bindings.
        for field in FORBIDDEN_URL_FIELDS {
            if source_config.get(field).is_some() {
                return Err(ProviderError::InvalidConfig(format!(
                    "Field '{field}' is not supported. RtmpProvider does not accept external URLs."
                )));
            }
        }

        Ok(())
    }

    fn validate_config_shape(source_config: &Value) -> Result<(), ProviderError> {
        for field in ["room_id", "media_id"] {
            if source_config.get(field).is_some() {
                return Err(ProviderError::InvalidConfig(format!(
                    "Field '{field}' is not supported. Internal RTMP media identity comes from runtime context."
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
            "live stream playback metadata",
        )?;
        let media_id = super::proxy::parse_proxy_media_id(
            &ctx.services.public_id_codec,
            media_id,
            "live stream playback metadata",
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
                        "RTMP proxy claims",
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
        ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        Self::validate_config_fields(source_config)?;
        Self::validate_config_shape(source_config)?;
        let (room_id, media_id) = Self::resolve_live_binding(ctx)?;

        let result = super::build_live_playback(*media_id, *room_id);

        let cache_key = format!("playback:{room_id}:{media_id}");
        let cache_ttl = Duration::from_mins(5); // 5 minutes for live
        super::finalize_versioned_playback(result, Self::NAME, &cache_key, cache_ttl, ctx).await
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        let source_config = source_config.value();
        Self::validate_config_fields(source_config)?;
        Self::validate_config_shape(source_config)?;
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
        let versioned =
            super::proxy::lookup_versioned(ctx.store, version, ctx.request_context).await?;
        Self::build_proxy_action(rest, &versioned, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MediaId, RoomId, UserId};
    use crate::provider::proxy::ProxyServices;
    use serde_json::json;
    use std::sync::Arc;

    fn create_context() -> ProviderContext<'static> {
        ProviderContext::new("synctv")
            .with_user_id(UserId::expect_positive(1))
            .with_room_id(RoomId::expect_positive(10))
            .with_media_id(MediaId::expect_positive(100))
    }

    fn fake_proxy_services() -> ProxyServices {
        let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
        let jwt = crate::service::auth::JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!")
            .expect("jwt");
        let username_cache =
            crate::cache::UsernameCache::local_only("test:username:".to_string(), 100, 60);
        let token_blacklist = Arc::new(
            crate::service::auth::token_blacklist::InMemoryTokenBlacklistStore::new(
                1000, 3600, 86400,
            ),
        );
        let key_builder = crate::cache::KeyBuilder::new("test");
        let brute_force = crate::service::auth::BruteForceProtection::in_memory("test".to_string());
        let user_service = crate::service::UserService::new(
            &pool,
            jwt,
            username_cache,
            crate::config::PasswordComplexityConfig::default(),
            token_blacklist,
            key_builder,
            brute_force,
        );
        let credential_repo = Arc::new(crate::repository::UserProviderCredentialRepository::new(
            pool.clone(),
        ));
        let room_service = crate::service::RoomService::new(pool, user_service);
        ProxyServices {
            room_service: Arc::new(room_service),
            credential_encryption: None,
            credential_repo,
            provider_access_service: None,
            signing_key: Arc::new(crate::proxy_signature::ProxySigningKey::derive_from(
                b"Test_Secret_Key_For_JWT_Tokens_32Bytes!!",
            )),
            public_id_codec: Arc::new(crate::PublicIdCodec::default_for_tests()),
        }
    }

    #[tokio::test]
    async fn test_validate_source_config_rejects_url_fields() {
        let provider = RtmpProvider::new();
        let ctx = create_context();

        let configs_with_urls = vec![
            json!({"url": "http://evil.com"}),
            json!({"rtmp_url": "rtmp://evil.com"}),
            json!({"source_url": "http://evil.com"}),
        ];

        for config in configs_with_urls {
            let result = provider
                .validate_source_config(&ctx, SourceConfig::media(&config))
                .await;
            assert!(
                result.is_err(),
                "validate_source_config should reject URL fields: {config}"
            );
        }
    }

    #[tokio::test]
    async fn test_validate_source_config_accepts_empty_config() {
        let provider = RtmpProvider::new();
        let ctx = create_context();

        let result = provider
            .validate_source_config(&ctx, SourceConfig::media(&json!({})))
            .await;
        assert!(
            result.is_ok(),
            "validate_source_config should accept empty config: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_validate_source_config_rejects_identity_fields() {
        let provider = RtmpProvider::new();
        let ctx = create_context();

        let result = provider
            .validate_source_config(&ctx, SourceConfig::media(&json!({"room_id": "room123"})))
            .await;
        assert!(
            matches!(result, Err(ProviderError::InvalidConfig(_))),
            "identity fields must be rejected for internal RTMP media"
        );
    }

    #[tokio::test]
    async fn test_validate_source_config_accepts_empty_config_without_context_binding() {
        let provider = RtmpProvider::new();
        let ctx = ProviderContext::new("synctv");

        let result = provider
            .validate_source_config(&ctx, SourceConfig::media(&json!({})))
            .await;
        assert!(
            result.is_ok(),
            "creation-time validation should allow deferred room/media binding: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_generate_playback_requires_provider_context_identity() {
        let provider = RtmpProvider::new();
        let ctx = ProviderContext::new("synctv");

        let result = provider.generate_playback(&ctx, &json!({})).await;
        assert!(
            matches!(result, Err(ProviderError::InvalidConfig(_))),
            "generate_playback must fail closed when room/media identity is missing from context"
        );
    }

    #[tokio::test]
    async fn test_generate_playback_uses_context_binding_when_source_config_is_empty() {
        let provider = RtmpProvider::new();
        let ctx = create_context();

        let result = provider.generate_playback(&ctx, &json!({})).await.unwrap();

        assert_eq!(result.metadata.get("room_id"), Some(&json!(10)));
        assert_eq!(result.metadata.get("media_id"), Some(&json!(100)));
        assert!(result.playback_infos.contains_key("hls"));
        assert!(result.playback_infos.contains_key("flv"));
    }

    #[tokio::test]
    async fn generate_playback_signs_urls_with_provider_proxy_prefix() {
        use crate::provider::store::InMemoryProviderStore;
        use crate::proxy_signature::ProxySigningKey;
        use std::sync::Arc;

        let provider = RtmpProvider::new();
        let signing_key = ProxySigningKey::derive_from(b"test-jwt-secret-that-is-long-enough");
        let ctx = ProviderContext::new("synctv")
            .with_user_id(UserId::expect_positive(1))
            .with_room_id(RoomId::expect_positive(10))
            .with_media_id(MediaId::expect_positive(100))
            .with_signing_key(&signing_key)
            .with_store(Arc::new(InMemoryProviderStore::new(16)));
        let result = provider.generate_playback(&ctx, &json!({})).await.unwrap();

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
        let flv_url = url::Url::parse(&format!("http://synctv.local{flv}")).unwrap();
        assert!(flv_url
            .path_segments()
            .unwrap()
            .nth(5)
            .is_some_and(|action| action == "stream" || action.starts_with("stream%2F")));
        assert!(flv_url.query_pairs().any(|(key, _)| key == "sig"));
    }

    #[tokio::test]
    async fn cached_playback_is_resigned_for_current_identity() {
        use crate::provider::store::InMemoryProviderStore;
        use crate::proxy_signature::ProxySigningKey;
        use std::sync::Arc;

        let provider = RtmpProvider::new();
        let store = Arc::new(InMemoryProviderStore::new(16));
        let signing_key = ProxySigningKey::derive_from(b"test-jwt-secret-that-is-long-enough");

        let ctx1 = ProviderContext::new("synctv")
            .with_user_id(UserId::expect_positive(1))
            .with_room_id(RoomId::expect_positive(10))
            .with_media_id(MediaId::expect_positive(100))
            .with_signing_key(&signing_key)
            .with_store(store.clone());
        let first = provider.generate_playback(&ctx1, &json!({})).await.unwrap();

        let ctx2 = ProviderContext::new("synctv")
            .with_user_id(UserId::expect_positive(2))
            .with_room_id(RoomId::expect_positive(10))
            .with_media_id(MediaId::expect_positive(100))
            .with_signing_key(&signing_key)
            .with_store(store);
        let second = provider.generate_playback(&ctx2, &json!({})).await.unwrap();

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
            second_hls.contains("uid=2")
                || second_hls.contains("user_id=2")
                || second_hls.contains("sig=")
        );
    }

    #[tokio::test]
    async fn resolve_proxy_flv_includes_verified_identity_and_expiry() {
        use crate::provider::store::VersionedPlayback;
        use crate::proxy_signature::ProxyUrlClaims;
        use std::collections::HashMap;

        let services = fake_proxy_services();
        let versioned = VersionedPlayback {
            version: "v1".to_string(),
            result: PlaybackResult {
                playback_infos: HashMap::new(),
                default_mode: "hls".to_string(),
                metadata: HashMap::from([
                    (
                        "room_id".to_string(),
                        json!(services
                            .public_id_codec
                            .encode_room_id(RoomId::expect_positive(10))
                            .expect("room id should encode")),
                    ),
                    (
                        "media_id".to_string(),
                        json!(services
                            .public_id_codec
                            .encode_media_id(MediaId::expect_positive(100))
                            .expect("media id should encode")),
                    ),
                ]),
            },
            expires_at: chrono::Utc::now().timestamp() + 60,
        };
        let claims = ProxyUrlClaims {
            provider: "rtmp".to_string(),
            version: "v1".to_string(),
            room_id: services
                .public_id_codec
                .encode_room_id(RoomId::expect_positive(10))
                .expect("room id should encode"),
            user_id: services
                .public_id_codec
                .encode_user_id(UserId::expect_positive(1))
                .expect("user id should encode"),
            expires_at: chrono::Utc::now().timestamp() + 30,
            target_url: None,
        };
        let ctx = ProxyRequestContext {
            sub_path: "v1/stream",
            query_string: None,
            store: None,
            proxy_base: "/api/providers/proxy/rtmp",
            services: &services,
            verified_claims: Some(&claims),
            request_context: None,
            request_headers: &http::HeaderMap::new(),
        };
        let action = RtmpProvider::build_proxy_action("stream", &versioned, &ctx).unwrap();
        match action {
            ProxyAction::LiveFlv {
                user_id,
                expires_at,
                ..
            } => {
                assert_eq!(user_id, UserId::expect_positive(1));
                assert_eq!(expires_at, claims.expires_at);
            }
            other => panic!("expected LiveFlv action, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_proxy_flv_requires_verified_claims() {
        use crate::provider::store::VersionedPlayback;
        use std::collections::HashMap;

        let services = fake_proxy_services();
        let versioned = VersionedPlayback {
            version: "v1".to_string(),
            result: PlaybackResult {
                playback_infos: HashMap::new(),
                default_mode: "hls".to_string(),
                metadata: HashMap::from([
                    (
                        "room_id".to_string(),
                        json!(services
                            .public_id_codec
                            .encode_room_id(RoomId::expect_positive(10))
                            .expect("room id should encode")),
                    ),
                    (
                        "media_id".to_string(),
                        json!(services
                            .public_id_codec
                            .encode_media_id(MediaId::expect_positive(100))
                            .expect("media id should encode")),
                    ),
                ]),
            },
            expires_at: chrono::Utc::now().timestamp() + 60,
        };

        let ctx = ProxyRequestContext {
            sub_path: "v1/stream",
            query_string: None,
            store: None,
            proxy_base: "/api/providers/proxy/rtmp",
            services: &services,
            verified_claims: None,
            request_context: None,
            request_headers: &http::HeaderMap::new(),
        };
        let err = RtmpProvider::build_proxy_action("stream", &versioned, &ctx).unwrap_err();
        assert!(matches!(err, ProviderError::ApiError(_)));
    }
}
