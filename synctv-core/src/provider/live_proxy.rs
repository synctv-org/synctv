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
    MediaProvider, PlaybackResult, ProviderContext, ProviderError, SourceConfig,
};
use crate::models::{MediaId, RoomId, TypedId};
use crate::proxy_signature::ProxySigningKey;
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{json, Value};
use std::time::Duration;

/// `LiveProxy` `MediaProvider`
///
/// Generates playback URLs for live streams from external sources.
/// The external URL is stored in `source_config.url` and validated on creation.
/// Playback URLs point to synctv's own HLS/FLV endpoints. Internal room/media
/// identity is injected at playback time through `ProviderContext`.
pub struct LiveProxyProvider {
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
}

impl Default for LiveProxyProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveProxyProvider {
    pub const NAME: &'static str = "live_proxy";

    pub fn new() -> Self {
        Self::new_with_ssrf_guard(synctv_common::ssrf::SsrfGuard::strict_policy())
    }

    #[must_use]
    pub const fn new_with_ssrf_guard(ssrf_guard: synctv_common::ssrf::SsrfGuard) -> Self {
        Self { ssrf_guard }
    }

    async fn validate_live_source_url(
        url: &str,
        guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<(), ProviderError> {
        // Validate URL format (only RTMP and HTTP-FLV are supported for pulling).
        // Use URL path parsing to avoid false positives from `.flv` appearing
        // in query parameters or other URL parts.
        let parsed_url = url::Url::parse(url).map_err(|error| {
            ProviderError::InvalidConfig(format!("Invalid LiveProxy source URL '{url}': {error}"))
        })?;
        let is_rtmp = parsed_url.scheme().eq_ignore_ascii_case("rtmp");
        let is_flv =
            matches!(parsed_url.scheme(), "http" | "https") && parsed_url.path().ends_with(".flv");
        if !is_rtmp && !is_flv {
            return Err(ProviderError::InvalidConfig(format!(
                "Unsupported source URL format: {url}. Expected rtmp:// or *.flv"
            )));
        }
        Self::reject_synctv_publish_url(&parsed_url)?;

        let host = parsed_url.host_str().ok_or_else(|| {
            ProviderError::InvalidConfig("LiveProxy source URL is missing a host".to_string())
        })?;
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
        } else if is_rtmp && guard.dns_resolver().is_some() {
            let port = parsed_url.port().unwrap_or(1935);
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
                .await
                .map_err(|error| {
                    ProviderError::InvalidConfig(format!(
                        "LiveProxy RTMP source host '{host}' could not be resolved: {error}"
                    ))
                })?
                .collect();

            if addrs.is_empty() {
                return Err(ProviderError::InvalidConfig(format!(
                    "LiveProxy RTMP source host '{host}' did not resolve to any addresses"
                )));
            }

            if let Some(blocked_addr) = addrs.iter().find(|addr| guard.is_ip_blocked(&addr.ip())) {
                return Err(ProviderError::InvalidConfig(format!(
                    "LiveProxy RTMP source host '{host}' resolved to blocked IP '{}'",
                    blocked_addr.ip()
                )));
            }
        }

        Ok(())
    }

    fn reject_synctv_publish_url(parsed_url: &url::Url) -> Result<(), ProviderError> {
        if !parsed_url.scheme().eq_ignore_ascii_case("rtmp") {
            return Ok(());
        }

        let Some(token) = parsed_url
            .query_pairs()
            .find_map(|(key, value)| (key == "token").then_some(value.into_owned()))
        else {
            return Ok(());
        };

        if !Self::looks_like_synctv_publish_key(&token) {
            return Ok(());
        }

        Err(ProviderError::InvalidConfig(
            "LiveProxy source URL points at a SyncTV RTMP publish endpoint. Use the original upstream RTMP/HTTP-FLV source URL, or use the RTMP provider for SyncTV-managed live media.".to_string(),
        ))
    }

    fn looks_like_synctv_publish_key(token: &str) -> bool {
        let mut parts = token.split('.');
        let (Some(_header), Some(payload), Some(_signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return false;
        };

        let Ok(payload) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload) else {
            return false;
        };
        let Ok(payload) = serde_json::from_slice::<Value>(&payload) else {
            return false;
        };

        payload
            .get("perm_live_control")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && payload.get("room_id").is_some()
            && payload.get("media_id").is_some()
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
        let services = ctx.services()?;
        let room_id = Self::metadata_typed_id(versioned, "room_id", |room_id| {
            super::proxy::parse_proxy_room_id(
                &services.public_id_codec,
                room_id,
                "live playback metadata",
            )
        })?;
        let media_id = Self::metadata_typed_id(versioned, "media_id", |media_id| {
            super::proxy::parse_proxy_media_id(
                &services.public_id_codec,
                media_id,
                "live playback metadata",
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
                        &services.public_id_codec,
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
}

fn sign_live_proxy_playback_urls(
    result: &mut PlaybackResult,
    version: &str,
    signing_key: &ProxySigningKey,
    room_id: &str,
    user_id: &str,
    expires_at: i64,
) {
    // Live proxy delivery is owned by SyncTV, so generated modes point at
    // signed provider-proxy actions directly. HLS and FLV actions stay
    // provider-specific because they start and track different live resources.
    // New live modes must attach to the external publish lifecycle and idle
    // cleanup path before they are exposed in playback results.
    let default_mode = result.default_mode.clone();
    for (mode_name, info) in &mut result.playback_infos {
        if info.urls.is_empty() {
            continue;
        }

        if super::playback_info_is_hls(mode_name, info) {
            info.urls = vec![super::signed_provider_proxy_url(
                LiveProxyProvider::NAME,
                version,
                "m3u8",
                signing_key,
                room_id,
                user_id,
                expires_at,
            )];
        } else if mode_name == &default_mode && info.urls.len() == 1 {
            info.urls = vec![super::signed_provider_proxy_url(
                LiveProxyProvider::NAME,
                version,
                "stream",
                signing_key,
                room_id,
                user_id,
                expires_at,
            )];
        } else {
            info.urls = info
                .urls
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    super::signed_provider_proxy_url(
                        LiveProxyProvider::NAME,
                        version,
                        &format!("stream/{mode_name}/{index}"),
                        signing_key,
                        room_id,
                        user_id,
                        expires_at,
                    )
                })
                .collect();
        }
        info.headers.clear();
        info.cors_proxy_required = false;
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
        Self::validate_live_source_url(source_url, &self.ssrf_guard).await?;

        let mut result = super::build_live_playback(*media_id, *room_id);
        let parsed_source_url = url::Url::parse(source_url).map_err(|error| {
            ProviderError::InvalidConfig(format!(
                "Invalid LiveProxy source URL '{source_url}': {error}"
            ))
        })?;
        let redacted_host = parsed_source_url
            .host_str()
            .ok_or_else(|| {
                ProviderError::InvalidConfig("LiveProxy source URL is missing a host".to_string())
            })?
            .to_string();
        result
            .metadata
            .insert("source_host".to_string(), json!(redacted_host));
        result
            .metadata
            .insert("provider".to_string(), json!("live_proxy"));

        let cache_key = format!("playback:{room_id}:{media_id}");
        let cache_ttl = Duration::from_mins(5);
        super::cache_versioned_playback_and_build_response(
            result,
            Self::NAME,
            &cache_key,
            cache_ttl,
            ctx,
            sign_live_proxy_playback_urls,
        )
        .await
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        let source_config = source_config.value();
        Self::validate_config_shape(source_config)?;

        // Validate required fields
        let url = source_config
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing url".to_string()))?;

        Self::validate_live_source_url(url, &self.ssrf_guard).await
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

    #[tokio::test]
    async fn test_live_proxy_metadata_does_not_expose_source_url() {
        let provider =
            LiveProxyProvider::new_with_ssrf_guard(synctv_common::ssrf::SsrfGuard::disabled());
        let ctx = ProviderContext::new("test")
            .with_room_id(RoomId::expect_positive(10))
            .with_media_id(MediaId::expect_positive(100));

        let source_config = json!({
            "url": "rtmp://secret-internal-server.local/live/stream-key"
        });

        let result = provider
            .generate_playback(&ctx, &source_config)
            .await
            .checked("operation should succeed");

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
        assert_eq!(provider.name(), LiveProxyProvider::NAME);
    }

    #[tokio::test]
    async fn resolve_proxy_rejects_empty_action() {
        let provider = LiveProxyProvider::new();
        let ctx = ProxyRequestContext {
            sub_path: "v1/",
            query_string: None,
            store: None,
            proxy_base: "/api/providers/proxy/live_proxy",
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
    async fn test_live_proxy_metadata_contains_provider_tag() {
        let provider =
            LiveProxyProvider::new_with_ssrf_guard(synctv_common::ssrf::SsrfGuard::disabled());
        let ctx = ProviderContext::new("test")
            .with_room_id(RoomId::expect_positive(10))
            .with_media_id(MediaId::expect_positive(100));

        let source_config = json!({
            "url": "rtmp://example.com/live/stream"
        });

        let result = provider
            .generate_playback(&ctx, &source_config)
            .await
            .checked("operation should succeed");
        assert_eq!(
            result.metadata.get("provider").and_then(|v| v.as_str()),
            Some("live_proxy"),
            "Metadata should still contain provider tag"
        );
    }

    #[tokio::test]
    async fn generate_playback_signs_urls_with_provider_proxy_prefix() {
        use crate::provider::store::InMemoryProviderStore;
        use crate::proxy_signature::ProxySigningKey;
        use std::sync::Arc;

        let provider =
            LiveProxyProvider::new_with_ssrf_guard(synctv_common::ssrf::SsrfGuard::disabled());
        let signing_key = ProxySigningKey::try_derive_from(b"test-jwt-secret-that-is-long-enough")
            .checked("test proxy signing key should derive");
        let ctx = ProviderContext::new("synctv")
            .with_user_id(UserId::expect_positive(1))
            .with_room_id(RoomId::expect_positive(10))
            .with_media_id(MediaId::expect_positive(100))
            .with_signing_key(&signing_key)
            .with_store(Arc::new(InMemoryProviderStore::new(16)));
        let result = provider
            .generate_playback(&ctx, &json!({"url": "rtmp://example.com/live/stream"}))
            .await
            .checked("operation should succeed");

        let flv = result
            .playback_infos
            .get("flv")
            .checked("operation should succeed")
            .urls
            .first()
            .checked("operation should succeed");
        let hls = result
            .playback_infos
            .get("hls")
            .checked("operation should succeed")
            .urls
            .first()
            .checked("operation should succeed");
        assert!(hls.starts_with("/api/providers/proxy/live_proxy/"));
        assert!(hls.contains("/m3u8?"));
        assert!(flv.starts_with("/api/providers/proxy/live_proxy/"));
        assert_eq!(result.default_mode, "hls");
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
    async fn test_live_proxy_validate_source_config_allows_blocked_hosts_when_ssrf_is_explicitly_disabled(
    ) {
        let provider =
            LiveProxyProvider::new_with_ssrf_guard(synctv_common::ssrf::SsrfGuard::disabled());
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
                .validate_source_config(&ctx, SourceConfig::media(&config))
                .await
                .checked("disabled SSRF policy should allow blocked live source URLs");
        }
    }

    #[tokio::test]
    async fn test_live_proxy_validate_source_config_rejects_allowlisted_rtmp_hostname_resolving_private(
    ) {
        let guard = synctv_common::ssrf::SsrfGuard::builder()
            .extra_allowed_host("localhost".to_string())
            .build();
        let provider = LiveProxyProvider::new_with_ssrf_guard(guard);
        let ctx = ProviderContext::new("test");

        let err = provider
            .validate_source_config(
                &ctx,
                SourceConfig::media(&json!({
                    "url": "rtmp://localhost/live/stream"
                })),
            )
            .await
            .failed("RTMP hostnames must resolve to public-safe addresses at config validation");

        assert!(matches!(
            err,
            ProviderError::InvalidConfig(ref msg)
                if msg.contains("resolved to blocked IP")
        ));
    }

    #[tokio::test]
    async fn test_live_proxy_validate_source_config_rejects_internal_identity_fields() {
        let provider = LiveProxyProvider::new();
        let ctx = ProviderContext::new("test");

        let err = provider
            .validate_source_config(
                &ctx,
                SourceConfig::media(
                    &json!({"url": "rtmp://example.com/live/stream", "room_id": "room-123"}),
                ),
            )
            .await
            .failed("live_proxy source_config must not persist internal identity");
        assert!(matches!(
            err,
            ProviderError::InvalidConfig(ref msg) if msg.contains("runtime context")
        ));
    }

    #[tokio::test]
    async fn test_live_proxy_validate_source_config_rejects_synctv_publish_url() {
        let provider =
            LiveProxyProvider::new_with_ssrf_guard(synctv_common::ssrf::SsrfGuard::disabled());
        let ctx = ProviderContext::new("test");
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "room_id": "1",
                "media_id": "10",
                "perm_live_control": true
            }))
            .checked("payload should serialize"),
        );
        let token = format!("header.{payload}.signature");
        let source_config = json!({
            "url": format!("rtmp://127.0.0.1:53008/room_1/med_10?token={token}")
        });

        let err = provider
            .validate_source_config(&ctx, SourceConfig::media(&source_config))
            .await
            .failed("SyncTV publish endpoints are not valid live_proxy pull sources");

        assert!(matches!(
            err,
            ProviderError::InvalidConfig(ref msg) if msg.contains("RTMP publish endpoint")
        ));
    }

    #[tokio::test]
    async fn test_live_proxy_validate_source_config_allows_external_rtmp_with_unrelated_token() {
        let provider =
            LiveProxyProvider::new_with_ssrf_guard(synctv_common::ssrf::SsrfGuard::disabled());
        let ctx = ProviderContext::new("test");

        provider
            .validate_source_config(
                &ctx,
                SourceConfig::media(&json!({
                    "url": "rtmp://127.0.0.1:19350/live/source?token=external"
                })),
            )
            .await
            .checked("external RTMP sources with unrelated query tokens should remain valid");
    }

    #[tokio::test]
    async fn test_live_proxy_validate_source_config_rejects_provider_instance_name() {
        let provider = LiveProxyProvider::new();
        let ctx = ProviderContext::new("test");

        let err = provider
            .validate_source_config(
                &ctx,
                SourceConfig::media(&json!({
                    "url": "rtmp://example.com/live/stream",
                    "provider_instance_name": "remote-live"
                })),
            )
            .await
            .failed("live_proxy source_config must not contain provider_instance_name");
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
            .failed("live proxy playback must fail closed without room/media binding");
        assert!(matches!(
            err,
            ProviderError::InvalidConfig(ref msg) if msg.contains("provider context")
        ));
    }
}
