//! RTMP `MediaProvider`
//!
//! Provides HTTP-FLV and HLS playback URLs for SyncTV live streams published over RTMP.
//! URLs point to synctv's own HTTP-FLV and HLS endpoints.

use super::{
    proxy::{ProviderProxy, ProxyAction, ProxyRequestContext},
    store::VersionedPlayback,
    MediaProvider, PlaybackResult, ProviderContext, ProviderError, SourceConfig,
};
use crate::models::{MediaId, RoomId, TypedId};
use crate::proxy_signature::ProxySigningKey;
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

    fn metadata_typed_id<T>(
        versioned: &VersionedPlayback,
        field: &'static str,
        parse_public_id: impl FnOnce(&str) -> Result<T, ProviderError>,
    ) -> Result<T, ProviderError>
    where
        T: TypedId,
    {
        let value = versioned
            .result
            .metadata
            .get(field)
            .ok_or_else(|| ProviderError::ApiError(format!("Live playback missing {field}")))?;

        if let Some(id) = value.as_i64() {
            return T::try_from(id).map_err(|error| {
                ProviderError::InvalidConfig(format!(
                    "Invalid {field} in live playback metadata: {error}"
                ))
            });
        }

        if let Some(id) = value.as_u64() {
            let id = i64::try_from(id).map_err(|_| {
                ProviderError::InvalidConfig(format!(
                    "Invalid {field} in live playback metadata: exceeds i64"
                ))
            })?;
            return T::try_from(id).map_err(|error| {
                ProviderError::InvalidConfig(format!(
                    "Invalid {field} in live playback metadata: {error}"
                ))
            });
        }

        let value = value.as_str().ok_or_else(|| {
            ProviderError::InvalidConfig(format!(
                "Invalid {field} in live playback metadata: expected public ID string or numeric ID"
            ))
        })?;

        parse_public_id(value)
    }

    fn build_proxy_action(
        rest: &str,
        versioned: &VersionedPlayback,
        ctx: &ProxyRequestContext<'_>,
    ) -> Result<ProxyAction, ProviderError> {
        let room_id = Self::metadata_typed_id(versioned, "room_id", |room_id| {
            super::proxy::parse_proxy_room_id(
                ctx.public_id_codec()?,
                room_id,
                "live stream playback metadata",
            )
        })?;
        let media_id = Self::metadata_typed_id(versioned, "media_id", |media_id| {
            super::proxy::parse_proxy_media_id(
                ctx.public_id_codec()?,
                media_id,
                "live stream playback metadata",
            )
        })?;

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
                        ctx.public_id_codec()?,
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
                    provider_name: Self::NAME.to_string(),
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

fn sign_rtmp_playback_urls(
    result: &mut PlaybackResult,
    version: &str,
    signing_key: &ProxySigningKey,
    room_id: &str,
    user_id: &str,
    expires_at: i64,
) {
    // RTMP playback is a SyncTV-managed live source. HLS and FLV use distinct
    // signed proxy actions so clients can request either delivery format while
    // the provider preserves room/media/user binding in the proxy claim. Any
    // new live mode must be backed by livestream lifecycle tracking and idle
    // cleanup before it is returned to clients.
    for (mode_name, info) in &mut result.playback_infos {
        if info.urls.is_empty() {
            continue;
        }

        info.urls = if super::playback_info_is_hls(mode_name, info) {
            vec![super::signed_provider_proxy_url(
                RtmpProvider::NAME,
                version,
                "m3u8",
                signing_key,
                room_id,
                user_id,
                expires_at,
            )]
        } else {
            info.urls
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    super::signed_provider_proxy_url(
                        RtmpProvider::NAME,
                        version,
                        &format!("stream/{mode_name}/{index}"),
                        signing_key,
                        room_id,
                        user_id,
                        expires_at,
                    )
                })
                .collect()
        };
        info.headers.clear();
        info.cors_proxy_required = false;
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
        super::cache_versioned_playback_and_build_response(
            result,
            Self::NAME,
            &cache_key,
            cache_ttl,
            ctx,
            sign_rtmp_playback_urls,
        )
        .await
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
        let (version, rest) = super::proxy::split_versioned_proxy_path(ctx.sub_path)?;
        let versioned =
            super::proxy::lookup_versioned(ctx.store, version, ctx.request_context).await?;
        Self::build_proxy_action(rest, &versioned, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MediaId, RoomId, UserId};
    use crate::test_helpers::{TestOptionExt, TestResultExt};
    use serde_json::json;

    fn create_context() -> ProviderContext<'static> {
        ProviderContext::new("synctv")
            .with_user_id(UserId::expect_positive(1))
            .with_room_id(RoomId::expect_positive(10))
            .with_media_id(MediaId::expect_positive(100))
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

        let result = provider
            .generate_playback(&ctx, &json!({}))
            .await
            .checked("operation should succeed");

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
        let signing_key = ProxySigningKey::try_derive_from(b"test-jwt-secret-that-is-long-enough")
            .checked("test proxy signing key should derive");
        let ctx = ProviderContext::new("synctv")
            .with_user_id(UserId::expect_positive(1))
            .with_room_id(RoomId::expect_positive(10))
            .with_media_id(MediaId::expect_positive(100))
            .with_signing_key(&signing_key)
            .with_store(Arc::new(InMemoryProviderStore::new(16)));
        let result = provider
            .generate_playback(&ctx, &json!({}))
            .await
            .checked("operation should succeed");

        let hls = result
            .playback_infos
            .get("hls")
            .checked("operation should succeed")
            .urls
            .first()
            .checked("operation should succeed");
        let flv = result
            .playback_infos
            .get("flv")
            .checked("operation should succeed")
            .urls
            .first()
            .checked("operation should succeed");
        assert!(hls.starts_with("/api/providers/proxy/rtmp/"));
        assert!(hls.contains("/m3u8?"));
        assert!(flv.starts_with("/api/providers/proxy/rtmp/"));
        let flv_url = url::Url::parse(&format!("http://synctv.local{flv}"))
            .checked("operation should succeed");
        assert!(flv_url
            .path_segments()
            .checked("operation should succeed")
            .nth(5)
            .is_some_and(|action| action == "stream" || action.starts_with("stream/")));
        assert!(flv_url.query_pairs().any(|(key, _)| key == "sig"));
    }

    #[tokio::test]
    async fn resolve_proxy_rejects_empty_action() {
        let provider = RtmpProvider::new();
        let ctx = ProxyRequestContext {
            sub_path: "v1/",
            query_string: None,
            store: None,
            proxy_base: "/api/providers/proxy/rtmp",
            services: None,
            public_id_codec: None,
            verified_claims: None,
            request_context: None,
            request_headers: &http::HeaderMap::new(),
        };

        let err = provider
            .resolve_proxy(&ctx)
            .await
            .failed("empty proxy action should fail before store lookup");
        assert!(matches!(err, ProviderError::NotFound));
    }

    #[tokio::test]
    async fn cached_playback_is_resigned_for_current_identity() {
        use crate::provider::store::InMemoryProviderStore;
        use crate::proxy_signature::ProxySigningKey;
        use std::sync::Arc;

        let provider = RtmpProvider::new();
        let store = Arc::new(InMemoryProviderStore::new(16));
        let signing_key = ProxySigningKey::try_derive_from(b"test-jwt-secret-that-is-long-enough")
            .checked("test proxy signing key should derive");

        let ctx1 = ProviderContext::new("synctv")
            .with_user_id(UserId::expect_positive(1))
            .with_room_id(RoomId::expect_positive(10))
            .with_media_id(MediaId::expect_positive(100))
            .with_signing_key(&signing_key)
            .with_store(store.clone());
        let first = provider
            .generate_playback(&ctx1, &json!({}))
            .await
            .checked("operation should succeed");

        let ctx2 = ProviderContext::new("synctv")
            .with_user_id(UserId::expect_positive(2))
            .with_room_id(RoomId::expect_positive(10))
            .with_media_id(MediaId::expect_positive(100))
            .with_signing_key(&signing_key)
            .with_store(store);
        let second = provider
            .generate_playback(&ctx2, &json!({}))
            .await
            .checked("operation should succeed");

        let first_hls = first
            .playback_infos
            .get("hls")
            .checked("operation should succeed")
            .urls
            .first()
            .checked("operation should succeed");
        let second_hls = second
            .playback_infos
            .get("hls")
            .checked("operation should succeed")
            .urls
            .first()
            .checked("operation should succeed");
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

        let public_id_codec = crate::PublicIdCodec::plain();
        let versioned = VersionedPlayback {
            version: "v1".to_string(),
            result: PlaybackResult {
                playback_infos: HashMap::new(),
                default_mode: "hls".to_string(),
                duration_seconds: None,
                metadata: HashMap::from([
                    (
                        "room_id".to_string(),
                        json!(public_id_codec
                            .encode_room_id(RoomId::expect_positive(10))
                            .checked("operation should succeed")),
                    ),
                    (
                        "media_id".to_string(),
                        json!(public_id_codec
                            .encode_media_id(MediaId::expect_positive(100))
                            .checked("operation should succeed")),
                    ),
                ]),
            },
            expires_at: chrono::Utc::now().timestamp() + 60,
        };
        let claims = ProxyUrlClaims {
            provider: "rtmp".to_string(),
            version: "v1".to_string(),
            room_id: public_id_codec
                .encode_room_id(RoomId::expect_positive(10))
                .checked("room id should encode"),
            user_id: public_id_codec
                .encode_user_id(UserId::expect_positive(1))
                .checked("user id should encode"),
            expires_at: chrono::Utc::now().timestamp() + 30,
            target_url: None,
        };
        let ctx = ProxyRequestContext {
            sub_path: "v1/stream",
            query_string: None,
            store: None,
            proxy_base: "/api/providers/proxy/rtmp",
            services: None,
            public_id_codec: Some(&public_id_codec),
            verified_claims: Some(&claims),
            request_context: None,
            request_headers: &http::HeaderMap::new(),
        };
        let action = RtmpProvider::build_proxy_action("stream", &versioned, &ctx)
            .checked("operation should succeed");
        match action {
            ProxyAction::LiveFlv {
                user_id,
                expires_at,
                ..
            } => {
                assert_eq!(user_id, UserId::expect_positive(1));
                assert_eq!(expires_at, claims.expires_at);
            }
            other => std::panic::panic_any(format!("expected LiveFlv action, got {other:?}")),
        }
    }

    #[tokio::test]
    async fn resolve_proxy_accepts_generated_numeric_live_metadata() {
        use crate::provider::store::VersionedPlayback;
        use crate::proxy_signature::ProxyUrlClaims;

        let public_id_codec = crate::PublicIdCodec::plain();
        let room_id = RoomId::expect_positive(10);
        let media_id = MediaId::expect_positive(100);
        let versioned = VersionedPlayback {
            version: "v1".to_string(),
            result: crate::provider::build_live_playback(media_id, room_id),
            expires_at: chrono::Utc::now().timestamp() + 60,
        };
        let claims = ProxyUrlClaims {
            provider: "rtmp".to_string(),
            version: "v1".to_string(),
            room_id: public_id_codec
                .encode_room_id(room_id)
                .checked("room id should encode"),
            user_id: public_id_codec
                .encode_user_id(UserId::expect_positive(1))
                .checked("user id should encode"),
            expires_at: chrono::Utc::now().timestamp() + 30,
            target_url: None,
        };
        let ctx = ProxyRequestContext {
            sub_path: "v1/stream",
            query_string: None,
            store: None,
            proxy_base: "/api/providers/proxy/rtmp",
            services: None,
            public_id_codec: Some(&public_id_codec),
            verified_claims: Some(&claims),
            request_context: None,
            request_headers: &http::HeaderMap::new(),
        };

        let hls = RtmpProvider::build_proxy_action("m3u8", &versioned, &ctx)
            .checked("operation should succeed");
        match hls {
            ProxyAction::LiveHlsPlaylist {
                room_id: resolved_room_id,
                media_id: resolved_media_id,
                ..
            } => {
                assert_eq!(resolved_room_id, room_id);
                assert_eq!(resolved_media_id, media_id);
            }
            other => {
                std::panic::panic_any(format!("expected LiveHlsPlaylist action, got {other:?}"))
            }
        }

        let flv = RtmpProvider::build_proxy_action("stream", &versioned, &ctx)
            .checked("operation should succeed");
        match flv {
            ProxyAction::LiveFlv {
                room_id: resolved_room_id,
                media_id: resolved_media_id,
                user_id,
                ..
            } => {
                assert_eq!(resolved_room_id, room_id);
                assert_eq!(resolved_media_id, media_id);
                assert_eq!(user_id, UserId::expect_positive(1));
            }
            other => std::panic::panic_any(format!("expected LiveFlv action, got {other:?}")),
        }
    }

    #[tokio::test]
    async fn resolve_proxy_flv_requires_verified_claims() {
        use crate::provider::store::VersionedPlayback;
        use std::collections::HashMap;

        let versioned = VersionedPlayback {
            version: "v1".to_string(),
            result: PlaybackResult {
                playback_infos: HashMap::new(),
                default_mode: "hls".to_string(),
                duration_seconds: None,
                metadata: HashMap::from([
                    ("room_id".to_string(), json!(10)),
                    ("media_id".to_string(), json!(100)),
                ]),
            },
            expires_at: chrono::Utc::now().timestamp() + 60,
        };

        let ctx = ProxyRequestContext {
            sub_path: "v1/stream",
            query_string: None,
            store: None,
            proxy_base: "/api/providers/proxy/rtmp",
            services: None,
            public_id_codec: None,
            verified_claims: None,
            request_context: None,
            request_headers: &http::HeaderMap::new(),
        };
        let err = RtmpProvider::build_proxy_action("stream", &versioned, &ctx)
            .failed("operation should fail");
        assert!(matches!(err, ProviderError::ApiError(_)));
    }
}
