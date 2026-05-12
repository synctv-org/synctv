// Media Provider System
// Three-tier architecture:
// Tier 1: synctv-media-providers (Pure provider HTTP clients)
//   - alist::AlistClient, bilibili::BilibiliClient, emby::EmbyClient
//   - Independent libraries with no MediaProvider dependency
//   - Used by local adapters and remote provider servers as transport clients
// Tier 2: synctv-core/provider (MediaProvider adapters)
//   - AlistProvider, BilibiliProvider, EmbyProvider
//   - Implement MediaProvider and resolve optional top-level provider_instance_name bindings
// Tier 3: synctv-core/service/providers_manager
//   - ProvidersManager - resolves provider type + optional provider instance name
//   - Factory pattern for local providers and integration with RemoteProviderManager

// Core traits and types
pub mod access;
pub mod context;
pub mod credential_resolver;
pub mod error;
pub mod playback_profile;
pub mod provider_client;
pub mod proxy;
pub mod registry;
pub mod store;
pub mod traits;

// MediaProvider implementations (adapters)
pub mod alist;
pub mod bilibili;
pub mod direct_url;
pub mod emby;
pub mod live_proxy;
pub mod rtmp;

pub use access::*;
pub use context::*;
pub use credential_resolver::*;
pub use error::*;
pub use playback_profile::*;
pub use provider_client::ProviderClientManager;
pub use proxy::*;
pub use registry::*;
pub use store::*;
pub use synctv_common::{ExecutionControl, ExecutionControlError};
pub use traits::*;

use crate::models::{normalize_provider_instance_name, MediaId, RoomId};

pub(crate) fn subtitle_headers_for_proxy(
    playback_headers: &std::collections::HashMap<String, String>,
    subtitle: &SubtitleTrack,
) -> std::collections::HashMap<String, String> {
    let mut merged = playback_headers.clone();
    merged.extend(subtitle.headers.clone());
    merged
}

// Re-export providers
pub use alist::AlistProvider;
pub use bilibili::BilibiliProvider;
pub use direct_url::DirectUrlProvider;
pub use emby::EmbyProvider;
pub use live_proxy::LiveProxyProvider;
pub use rtmp::RtmpProvider;

/// Bundle of all media provider instances.
///
/// Consolidates per-provider fields into a single struct. Pass this through
/// the application instead of 4 separate `Arc<XxxProvider>` fields.
/// Adding a new provider only requires updating this struct and its
/// `build_proxy_registry()` method.
#[derive(Clone)]
pub struct ProviderSet {
    pub alist: std::sync::Arc<AlistProvider>,
    pub bilibili: std::sync::Arc<BilibiliProvider>,
    pub emby: std::sync::Arc<EmbyProvider>,
    pub direct_url: std::sync::Arc<DirectUrlProvider>,
    pub rtmp: std::sync::Arc<RtmpProvider>,
    pub live_proxy: std::sync::Arc<LiveProxyProvider>,
}

impl ProviderSet {
    /// Build a `ProxyProviderRegistry` from this provider set.
    ///
    /// Registers all proxy-capable providers under their canonical names.
    /// Called once at startup; both HTTP and gRPC share the resulting registry.
    #[must_use]
    pub fn build_proxy_registry(&self) -> ProxyProviderRegistry {
        let registry = ProxyProviderRegistry::new();
        registry.register(AlistProvider::NAME, self.alist.clone());
        registry.register(BilibiliProvider::NAME, self.bilibili.clone());
        registry.register(EmbyProvider::NAME, self.emby.clone());
        registry.register(DirectUrlProvider::NAME, self.direct_url.clone());
        registry.register(RtmpProvider::NAME, self.rtmp.clone());
        registry.register(LiveProxyProvider::NAME, self.live_proxy.clone());
        registry
    }
}

/// Parse a `serde_json::Value` into a typed source config.
///
/// Common helper for provider `TryFrom<&Value>` implementations.
pub fn parse_source_config<T: serde::de::DeserializeOwned>(
    value: &serde_json::Value,
    provider_name: &str,
) -> std::result::Result<T, ProviderError> {
    serde_json::from_value(value.clone()).map_err(|e| {
        ProviderError::InvalidConfig(format!(
            "Failed to parse {provider_name} source config: {e}"
        ))
    })
}

pub(crate) fn reject_source_config_provider_instance_name(
    source_config: &serde_json::Value,
    provider_name: &str,
) -> std::result::Result<(), ProviderError> {
    if source_config
        .as_object()
        .is_some_and(|object| object.contains_key("provider_instance_name"))
    {
        return Err(ProviderError::InvalidConfig(format!(
            "{provider_name} source_config must not contain provider_instance_name; use the media/playlist top-level provider_instance_name field instead"
        )));
    }

    Ok(())
}

pub(crate) fn reject_source_config_credential_ref(
    source_config: &serde_json::Value,
    provider_name: &str,
) -> std::result::Result<(), ProviderError> {
    if source_config
        .as_object()
        .is_some_and(|object| object.contains_key("credential_ref"))
    {
        return Err(ProviderError::InvalidConfig(format!(
            "{provider_name} source_config must not contain credential_ref; provider credentials are resolved from the media/playlist creator at runtime"
        )));
    }

    Ok(())
}

pub(crate) fn bound_provider_instance_name<'a>(ctx: &'a ProviderContext<'a>) -> Option<&'a str> {
    normalize_provider_instance_name(ctx.provider_instance_name())
}

#[must_use]
pub fn provider_requires_credential_repo(provider_name: &str) -> bool {
    matches!(provider_name, AlistProvider::NAME | EmbyProvider::NAME)
}

#[must_use]
pub fn build_live_playback(media_id: MediaId, room_id: RoomId) -> PlaybackResult {
    use serde_json::json;
    use std::collections::HashMap;

    let live_expires_at = Some(chrono::Utc::now().timestamp() + 30);

    let mut playback_infos = HashMap::new();

    playback_infos.insert(
        "hls".to_string(),
        PlaybackInfo {
            urls: vec![format!("live-hls://{room_id}/{media_id}")],
            format: "m3u8".to_string(),
            headers: HashMap::new(),
            subtitles: Vec::new(),
            expires_at: live_expires_at,
            cors_proxy_required: false,
        },
    );

    playback_infos.insert(
        "flv".to_string(),
        PlaybackInfo {
            urls: vec![format!("live-flv://{room_id}/{media_id}")],
            format: "flv".to_string(),
            headers: HashMap::new(),
            subtitles: Vec::new(),
            expires_at: live_expires_at,
            cors_proxy_required: false,
        },
    );

    let mut metadata = HashMap::new();
    metadata.insert("is_live".to_string(), json!(true));
    metadata.insert("media_id".to_string(), json!(media_id));
    metadata.insert("room_id".to_string(), json!(room_id));

    PlaybackResult {
        playback_infos,
        default_mode: "hls".to_string(),
        metadata,
    }
}

/// Standard Bilibili HTTP headers required for CDN requests.
///
/// These headers must be sent with all Bilibili media requests (video, audio, subtitles)
/// to avoid being blocked by Bilibili's CDN. Shared between the provider layer
/// (playback result headers) and the API proxy layer.
#[must_use]
pub fn bilibili_headers() -> std::collections::HashMap<String, String> {
    let mut headers = std::collections::HashMap::new();
    headers.insert(
        "Referer".to_string(),
        "https://www.bilibili.com".to_string(),
    );
    headers.insert(
        "User-Agent".to_string(),
        synctv_media_providers::error::PROVIDER_USER_AGENT.to_string(),
    );
    headers
}

/// Rewrite playback URLs in a `PlaybackResult` to use signed proxy URLs.
///
/// Called by providers after `generate_playback` when `signing_key`, `room_id`,
/// and `user_id` are available in the context. Each playback mode gets a signed
/// proxy URL based on whether `cors_proxy_required` is set or the mode suggests proxying.
///
/// The version and provider name are needed to construct the proxy path.
pub fn sign_playback_urls(
    result: &mut PlaybackResult,
    provider_name: &str,
    version: &str,
    signing_key: &crate::service::proxy_signature::ProxySigningKey,
    room_id: &str,
    user_id: &str,
    expires_at: i64,
) {
    if provider_name == AlistProvider::NAME
        && result
            .metadata
            .get("thumbnail")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|thumbnail| !thumbnail.trim().is_empty())
    {
        result.metadata.insert(
            "thumbnail".to_string(),
            serde_json::json!(crate::service::proxy_signature::build_signed_proxy_url(
                provider_name,
                version,
                "thumbnail",
                signing_key,
                room_id,
                user_id,
                expires_at,
            )),
        );
    }

    let default_mode = result.default_mode.clone();
    for (mode_name, info) in &mut result.playback_infos {
        if info.urls.is_empty() {
            continue;
        }

        if info.format == "mpd" {
            info.urls = info
                .urls
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    crate::service::proxy_signature::build_signed_proxy_url(
                        provider_name,
                        version,
                        &format!("stream/{mode_name}/{index}"),
                        signing_key,
                        room_id,
                        user_id,
                        expires_at,
                    )
                })
                .collect();
            info.headers.clear();
            info.cors_proxy_required = false;
        } else if info.format == "m3u8" || info.format == "hls" || mode_name.contains("hls") {
            if mode_name == &default_mode && info.urls.len() == 1 {
                info.urls = vec![crate::service::proxy_signature::build_signed_proxy_url(
                    provider_name,
                    version,
                    "m3u8",
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
                        crate::service::proxy_signature::build_signed_proxy_url(
                            provider_name,
                            version,
                            &format!("m3u8/{mode_name}/{index}"),
                            signing_key,
                            room_id,
                            user_id,
                            expires_at,
                        )
                    })
                    .collect();
            }
            // Proxy handles headers — client doesn't need them
            info.headers.clear();
            info.cors_proxy_required = false;
        } else if mode_name == &default_mode && info.urls.len() == 1 {
            info.urls = vec![crate::service::proxy_signature::build_signed_proxy_url(
                provider_name,
                version,
                "stream",
                signing_key,
                room_id,
                user_id,
                expires_at,
            )];
            // Proxy handles headers — client doesn't need them
            info.headers.clear();
            info.cors_proxy_required = false;
        } else {
            info.urls = info
                .urls
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    crate::service::proxy_signature::build_signed_proxy_url(
                        provider_name,
                        version,
                        &format!("stream/{mode_name}/{index}"),
                        signing_key,
                        room_id,
                        user_id,
                        expires_at,
                    )
                })
                .collect();
            // Proxy handles headers — client doesn't need them
            info.headers.clear();
            info.cors_proxy_required = false;
        }

        // Also sign subtitle URLs
        for (idx, subtitle) in info.subtitles.iter_mut().enumerate() {
            subtitle.url = crate::service::proxy_signature::build_signed_proxy_url(
                provider_name,
                version,
                &format!("subtitle/{mode_name}/{idx}"),
                signing_key,
                room_id,
                user_id,
                expires_at,
            );
            subtitle.headers.clear();
        }
    }
}

#[must_use]
pub fn maybe_sign_versioned_playback(
    mut result: PlaybackResult,
    provider_name: &str,
    version: &str,
    expires_at: i64,
    ctx: &ProviderContext<'_>,
) -> PlaybackResult {
    if let (Some(signing_key), Some(room_id), Some(user_id)) =
        (ctx.signing_key, ctx.proxy_room_id(), ctx.proxy_user_id())
    {
        sign_playback_urls(
            &mut result,
            provider_name,
            version,
            signing_key,
            &room_id,
            &user_id,
            expires_at,
        );
    }
    result
}

const fn signed_proxy_playback_requested(ctx: &ProviderContext<'_>) -> bool {
    ctx.signing_key.is_some() && ctx.room_id.is_some() && ctx.user_id.is_some()
}

fn remaining_versioned_ttl(expires_at: i64) -> std::time::Duration {
    let remaining_secs = (expires_at - chrono::Utc::now().timestamp())
        .max(1)
        .cast_unsigned();
    std::time::Duration::from_secs(remaining_secs)
}

async fn persist_versioned_mapping(
    store: &dyn ProviderStore,
    versioned: &VersionedPlayback,
    ttl: std::time::Duration,
    provider_name: &str,
) -> Result<()> {
    store
        .set(&format!("v:{}", versioned.version), versioned, ttl)
        .await
        .map_err(|e| {
            ProviderError::Internal(format!(
                "Provider '{provider_name}' failed to persist signed proxy version mapping: {e}"
            ))
        })
}

pub async fn maybe_sign_cached_versioned_playback(
    versioned: VersionedPlayback,
    provider_name: &str,
    ctx: &ProviderContext<'_>,
) -> Result<PlaybackResult> {
    if signed_proxy_playback_requested(ctx) {
        let store = ctx.store.as_ref().ok_or_else(|| {
            ProviderError::Internal(format!(
                "Provider '{provider_name}' cannot generate signed proxy playback without a provider store"
            ))
        })?;
        persist_versioned_mapping(
            store.as_ref(),
            &versioned,
            remaining_versioned_ttl(versioned.expires_at),
            provider_name,
        )
        .await?;
    }

    Ok(maybe_sign_versioned_playback(
        versioned.result,
        provider_name,
        &versioned.version,
        versioned.expires_at,
        ctx,
    ))
}

pub async fn finalize_versioned_playback(
    result: PlaybackResult,
    provider_name: &str,
    cache_key: &str,
    cache_ttl: std::time::Duration,
    ctx: &ProviderContext<'_>,
) -> Result<PlaybackResult> {
    let versioned = VersionedPlayback {
        version: synctv_common::snanoid!(16),
        result: result.clone(),
        expires_at: chrono::Utc::now().timestamp() + cache_ttl.as_secs().cast_signed(),
    };

    if let Some(store) = ctx.store.as_ref() {
        store.set(cache_key, &versioned, cache_ttl).await.map_err(|e| {
            ProviderError::Internal(format!(
                "Provider '{provider_name}' failed to persist playback cache entry '{cache_key}': {e}"
            ))
        })?;

        if signed_proxy_playback_requested(ctx) {
            persist_versioned_mapping(store.as_ref(), &versioned, cache_ttl, provider_name).await?;
        } else if let Err(e) = store
            .set(&format!("v:{}", versioned.version), &versioned, cache_ttl)
            .await
        {
            tracing::warn!(
                provider = provider_name,
                version = %versioned.version,
                error = %e,
                "Failed to persist unsigned versioned playback mapping"
            );
        }
    } else if signed_proxy_playback_requested(ctx) {
        return Err(ProviderError::Internal(format!(
            "Provider '{provider_name}' cannot generate signed proxy playback without a provider store"
        )));
    }

    Ok(maybe_sign_versioned_playback(
        result,
        provider_name,
        &versioned.version,
        versioned.expires_at,
        ctx,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{RoomId, UserId};
    use crate::provider::store::{InMemoryProviderStore, StoreError, StoreLockGuard};
    use crate::service::ProxySigningKey;
    use std::sync::Arc;

    struct FailVersionMappingStore {
        inner: InMemoryProviderStore,
    }

    #[async_trait::async_trait]
    impl ProviderStore for FailVersionMappingStore {
        async fn get_raw(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, StoreError> {
            self.inner.get_raw(key).await
        }

        async fn set_raw(
            &self,
            key: &str,
            value: &[u8],
            ttl: std::time::Duration,
        ) -> std::result::Result<(), StoreError> {
            if key.starts_with("v:") {
                return Err(StoreError::Backend(
                    "forced version mapping failure".to_string(),
                ));
            }
            self.inner.set_raw(key, value, ttl).await
        }

        async fn delete(&self, key: &str) -> std::result::Result<(), StoreError> {
            self.inner.delete(key).await
        }

        async fn lock(
            &self,
            _key: &str,
            _ttl: std::time::Duration,
        ) -> std::result::Result<StoreLockGuard, StoreError> {
            Ok(StoreLockGuard::noop())
        }
    }

    fn playback_result() -> PlaybackResult {
        let mut playback_infos = std::collections::HashMap::new();
        playback_infos.insert(
            "direct".to_string(),
            PlaybackInfo {
                urls: vec!["http://example.com/video.mp4".to_string()],
                format: "mp4".to_string(),
                headers: std::collections::HashMap::new(),
                subtitles: Vec::new(),
                expires_at: None,
                cors_proxy_required: false,
            },
        );
        PlaybackResult {
            playback_infos,
            default_mode: "direct".to_string(),
            metadata: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn provider_requires_credential_repo_matches_provider_capabilities() {
        assert!(provider_requires_credential_repo(AlistProvider::NAME));
        assert!(provider_requires_credential_repo(EmbyProvider::NAME));
        assert!(!provider_requires_credential_repo(BilibiliProvider::NAME));
        assert!(!provider_requires_credential_repo(DirectUrlProvider::NAME));
    }

    #[test]
    fn test_sign_playback_urls_signs_mpd_streams_with_indexed_proxy_paths() {
        let signing_key = ProxySigningKey::derive_from(b"test-jwt-secret-that-is-long-enough");
        let mut result = PlaybackResult {
            playback_infos: std::collections::HashMap::from([(
                "dash".to_string(),
                PlaybackInfo {
                    urls: vec![
                        "https://cdn.example.com/video-1080.m4s".to_string(),
                        "https://cdn.example.com/video-720.m4s".to_string(),
                    ],
                    format: "mpd".to_string(),
                    headers: std::collections::HashMap::from([(
                        "Referer".to_string(),
                        "https://www.bilibili.com".to_string(),
                    )]),
                    subtitles: vec![SubtitleTrack {
                        language: "zh-CN".to_string(),
                        name: "Chinese".to_string(),
                        url: "https://cdn.example.com/subtitle.json".to_string(),
                        headers: std::collections::HashMap::from([(
                            "Authorization".to_string(),
                            "Bearer subtitle-token".to_string(),
                        )]),
                        format: "json".to_string(),
                    }],
                    expires_at: None,
                    cors_proxy_required: true,
                },
            )]),
            default_mode: "dash".to_string(),
            metadata: std::collections::HashMap::new(),
        };

        sign_playback_urls(
            &mut result,
            "bilibili",
            "ver-1",
            &signing_key,
            "room-1",
            "user-1",
            chrono::Utc::now().timestamp() + 3600,
        );

        let dash = &result.playback_infos["dash"];
        assert_eq!(dash.urls.len(), 2);
        assert!(
            dash.urls[0].starts_with("/api/providers/proxy/bilibili/ver-1/stream%2Fdash%2F0?"),
            "first DASH stream should use an indexed signed proxy URL"
        );
        assert!(
            dash.urls[1].starts_with("/api/providers/proxy/bilibili/ver-1/stream%2Fdash%2F1?"),
            "second DASH stream should use an indexed signed proxy URL"
        );
        assert!(dash.headers.is_empty(), "proxy should own DASH headers");
        assert!(
            !dash.cors_proxy_required,
            "signed DASH proxy should clear the client-side CORS proxy requirement"
        );
        assert!(
            dash.subtitles[0]
                .url
                .starts_with("/api/providers/proxy/bilibili/ver-1/subtitle%2Fdash%2F0?"),
            "subtitle URLs may still use the signed proxy contract"
        );
        assert!(
            dash.subtitles[0].headers.is_empty(),
            "signed subtitle proxy should not expose upstream headers to clients"
        );
    }

    #[test]
    fn test_sign_playback_urls_preserves_multiple_plain_stream_urls() {
        let signing_key = ProxySigningKey::derive_from(b"test-jwt-secret-that-is-long-enough");
        let mut result = PlaybackResult {
            playback_infos: std::collections::HashMap::from([(
                "direct".to_string(),
                PlaybackInfo {
                    urls: vec![
                        "https://cdn.example.com/video-primary.mp4".to_string(),
                        "https://cdn.example.com/video-backup.mp4".to_string(),
                    ],
                    format: "mp4".to_string(),
                    headers: std::collections::HashMap::from([(
                        "Authorization".to_string(),
                        "Bearer hidden".to_string(),
                    )]),
                    subtitles: Vec::new(),
                    expires_at: None,
                    cors_proxy_required: true,
                },
            )]),
            default_mode: "direct".to_string(),
            metadata: std::collections::HashMap::new(),
        };

        sign_playback_urls(
            &mut result,
            "alist",
            "ver-1",
            &signing_key,
            "room-1",
            "user-1",
            chrono::Utc::now().timestamp() + 3600,
        );

        let direct = &result.playback_infos["direct"];
        assert_eq!(direct.urls.len(), 2);
        assert!(
            direct.urls[0].starts_with("/api/providers/proxy/alist/ver-1/stream%2Fdirect%2F0?"),
            "first direct stream should use an indexed signed proxy URL"
        );
        assert!(
            direct.urls[1].starts_with("/api/providers/proxy/alist/ver-1/stream%2Fdirect%2F1?"),
            "second direct stream should use an indexed signed proxy URL"
        );
        assert!(direct.headers.is_empty(), "proxy should own stream headers");
        assert!(!direct.cors_proxy_required);
    }

    #[tokio::test]
    async fn test_finalize_versioned_playback_requires_store_for_signed_proxy() {
        let signing_key = ProxySigningKey::derive_from(b"test-jwt-secret-that-is-long-enough");
        let ctx = ProviderContext::new("test")
            .with_user_id(UserId::expect_positive(1))
            .with_room_id(RoomId::expect_positive(10))
            .with_signing_key(&signing_key);

        let err = finalize_versioned_playback(
            playback_result(),
            "direct_url",
            "playback:test",
            std::time::Duration::from_mins(1),
            &ctx,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ProviderError::Internal(_)));
        assert!(
            err.to_string().contains("provider store"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_finalize_versioned_playback_fails_closed_when_mapping_persist_fails() {
        let signing_key = ProxySigningKey::derive_from(b"test-jwt-secret-that-is-long-enough");
        let ctx = ProviderContext::new("test")
            .with_user_id(UserId::expect_positive(1))
            .with_room_id(RoomId::expect_positive(10))
            .with_signing_key(&signing_key);
        let ctx = ctx.with_store(Arc::new(FailVersionMappingStore {
            inner: InMemoryProviderStore::new(16),
        }));

        let err = finalize_versioned_playback(
            playback_result(),
            "direct_url",
            "playback:test",
            std::time::Duration::from_mins(1),
            &ctx,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ProviderError::Internal(_)));
        assert!(
            err.to_string().contains("version mapping"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_cached_signed_playback_repairs_missing_version_mapping() {
        let signing_key = ProxySigningKey::derive_from(b"test-jwt-secret-that-is-long-enough");
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(16));
        let ctx = ProviderContext::new("test")
            .with_user_id(UserId::expect_positive(1))
            .with_room_id(RoomId::expect_positive(10))
            .with_signing_key(&signing_key)
            .with_store(store.clone());
        let versioned = VersionedPlayback {
            version: "cached-version".to_string(),
            result: playback_result(),
            expires_at: chrono::Utc::now().timestamp() + 60,
        };

        let signed =
            maybe_sign_cached_versioned_playback(versioned.clone(), "direct_url", &ctx).await;

        assert!(signed.is_ok(), "cached signing should succeed: {signed:?}");
        let stored: Option<VersionedPlayback> = store.get("v:cached-version").await.unwrap();
        assert!(
            stored.is_some(),
            "signed cache hit must restore version mapping"
        );
        let url = &signed.unwrap().playback_infos["direct"].urls[0];
        assert!(url.contains("/direct_url/cached-version/stream"));

        let query = url.split('?').nth(1).expect("signed proxy URL query");
        let claims = signing_key
            .parse_and_verify_query(query, "direct_url", "cached-version")
            .expect("valid signed query");
        assert_eq!(claims.user_id, "1");
        assert_eq!(claims.room_id, "10");
    }

    #[test]
    fn test_subtitle_headers_for_proxy_merges_playback_and_subtitle_headers() {
        let playback_headers = std::collections::HashMap::from([
            (
                "Authorization".to_string(),
                "Bearer playback-token".to_string(),
            ),
            ("Referer".to_string(), "https://player.example".to_string()),
        ]);
        let subtitle = SubtitleTrack {
            language: "en".to_string(),
            name: "English".to_string(),
            url: "https://cdn.example.com/subtitle.vtt".to_string(),
            headers: std::collections::HashMap::from([
                (
                    "X-Subtitle-Token".to_string(),
                    "subtitle-secret".to_string(),
                ),
                (
                    "Referer".to_string(),
                    "https://subtitle.example".to_string(),
                ),
            ]),
            format: "vtt".to_string(),
        };

        let headers = subtitle_headers_for_proxy(&playback_headers, &subtitle);

        assert_eq!(
            headers.get("Authorization"),
            Some(&"Bearer playback-token".to_string())
        );
        assert_eq!(
            headers.get("X-Subtitle-Token"),
            Some(&"subtitle-secret".to_string())
        );
        assert_eq!(
            headers.get("Referer"),
            Some(&"https://subtitle.example".to_string()),
            "subtitle-specific headers should override playback defaults"
        );
    }
}
