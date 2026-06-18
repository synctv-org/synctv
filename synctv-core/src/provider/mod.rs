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
pub mod store;
pub mod traits;

// MediaProvider implementations (adapters)
pub mod alist;
pub mod bilibili;
pub mod direct_url;
pub mod emby;
pub mod live_proxy;
pub mod rtmp;

pub use access::{
    AlistAccess, AlistBinding, BilibiliAccess, CachedProviderAccessService, EmbyAccess,
    ProviderAccessService, ProviderCredentialReader,
};
pub use context::ProviderContext;
pub use error::ProviderError;
pub use playback_profile::{
    PlaybackAudioCapability, PlaybackClientProfile, PlaybackContainer, PlaybackDeliveryPreference,
    PlaybackSubtitlePreference, PlaybackVideoCodec,
};
pub use provider_client::ProviderClientManager;
pub use proxy::{
    ProviderProxy, ProxyAction, ProxyProviderRegistry, ProxyRequestContext, ProxyServices,
};
pub use store::{
    InMemoryProviderStore, PrefixedProviderStore, ProviderStore, ProviderStoreExt,
    ProviderStoreRegistry, ProviderStoreResolver, RedisProviderStore, StoreError, StoreLockGuard,
    VersionedPlayback,
};
pub use synctv_common::{ExecutionControl, ExecutionControlError};
pub use traits::{
    DirectoryItem, DynamicBrowsePathSegment, DynamicFolder, DynamicListQuery, ItemType,
    MediaProvider, NextPlayItem, PlaybackInfo, PlaybackResult, ProviderCredentialDependency,
    SourceConfig, SourceConfigKind, SubtitleTrack,
};

use crate::models::{normalize_provider_instance_name, MediaId, RoomId};
use crate::proxy_signature::{build_signed_proxy_url, ProxySigningKey, SignedProxyUrlRequest};

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

fn playback_info_is_hls(mode_name: &str, info: &PlaybackInfo) -> bool {
    info.format == "m3u8" || info.format == "hls" || mode_name.contains("hls")
}

fn playback_info_has_transport_headers(info: &PlaybackInfo) -> bool {
    !info.headers.is_empty()
        || info
            .subtitles
            .iter()
            .any(|subtitle| !subtitle.headers.is_empty())
}

pub(crate) struct PlaybackProxySigning<'a> {
    provider_name: &'a str,
    version: &'a str,
    signing_key: &'a ProxySigningKey,
    room_id: &'a str,
    user_id: &'a str,
    expires_at: i64,
}

impl<'a> PlaybackProxySigning<'a> {
    #[must_use]
    pub(crate) const fn new(
        provider_name: &'a str,
        version: &'a str,
        signing_key: &'a ProxySigningKey,
        room_id: &'a str,
        user_id: &'a str,
        expires_at: i64,
    ) -> Self {
        Self {
            provider_name,
            version,
            signing_key,
            room_id,
            user_id,
            expires_at,
        }
    }

    #[must_use]
    pub(crate) fn signed_url(&self, action: &str) -> String {
        signed_provider_proxy_url(
            self.provider_name,
            self.version,
            action,
            self.signing_key,
            self.room_id,
            self.user_id,
            self.expires_at,
        )
    }
}

/// Build standard signed proxy URLs for modes whose proxy route shape is common.
///
/// Providers still own the decision to expose these URLs. This helper is a
/// mechanical formatter used from `generate_playback`; provider implementations
/// keep policy for signing timing, default-mode selection, header exposure, and
/// manifest semantics. Every action emitted here must be accepted by the
/// provider's `resolve_proxy` route and covered by real URL requests in E2E
/// verification.
#[must_use]
pub(crate) fn signed_standard_proxy_urls(
    mode_name: &str,
    info: &PlaybackInfo,
    signing: &PlaybackProxySigning<'_>,
) -> Vec<String> {
    let action = if playback_info_is_hls(mode_name, info) {
        "m3u8"
    } else {
        "stream"
    };

    info.urls
        .iter()
        .enumerate()
        .map(|(index, _)| signing.signed_url(&format!("{action}/{mode_name}/{index}")))
        .collect()
}

/// Add signed proxy sibling modes while keeping provider-owned policy explicit.
///
/// Providers call this during `generate_playback` after they have decided which
/// upstream modes are valid, which headers can be exposed, and whether the proxy
/// sibling should become the default. This helper only performs the mechanical
/// URL/subtitle rewrite for standard stream and HLS proxy routes.
///
/// Keep provider policy at the provider boundary. Bilibili DASH manifests,
/// Alist file/HLS URLs, Emby/Jellyfin transcode URLs, RTMP streams, and live
/// proxy sessions have different default-mode, header, signing, and lifecycle
/// rules. A shared helper may create `proxy_*` sibling URLs; each provider
/// chooses the modes returned to clients and guarantees its resolver accepts
/// every signed action it emits. Preserve upstream and proxy modes together when
/// both are usable for app clients; choose the default mode inside the provider.
pub(crate) fn append_signed_proxy_playback_modes(
    result: &mut PlaybackResult,
    signing: &PlaybackProxySigning<'_>,
    expose_original_headers: bool,
    prefer_proxy_default: bool,
    signed_urls: impl Fn(&str, &PlaybackInfo, &PlaybackProxySigning<'_>) -> Vec<String>,
) {
    append_signed_proxy_playback_modes_with_policy(
        result,
        signing,
        expose_original_headers,
        prefer_proxy_default,
        false,
        signed_urls,
    );
}

pub(crate) fn append_signed_proxy_playback_modes_with_policy(
    result: &mut PlaybackResult,
    signing: &PlaybackProxySigning<'_>,
    expose_original_headers: bool,
    prefer_proxy_default: bool,
    hide_header_backed_originals: bool,
    signed_urls: impl Fn(&str, &PlaybackInfo, &PlaybackProxySigning<'_>) -> Vec<String>,
) {
    let original_default_mode = result.default_mode.clone();
    let mut signed_default_mode = original_default_mode.clone();
    let original_modes = result
        .playback_infos
        .iter()
        .map(|(mode_name, info)| (mode_name.clone(), info.clone()))
        .collect::<Vec<_>>();

    for (mode_name, original_info) in original_modes {
        if original_info.urls.is_empty() || mode_name.starts_with("proxy_") {
            continue;
        }

        let original_has_transport_headers = playback_info_has_transport_headers(&original_info);
        if let Some(info) = result.playback_infos.get_mut(&mode_name) {
            info.cors_proxy_required = false;
            if !expose_original_headers {
                info.headers.clear();
                for subtitle in &mut info.subtitles {
                    subtitle.headers.clear();
                }
            }
        }

        let proxy_mode_name = format!("proxy_{mode_name}");
        if prefer_proxy_default && mode_name == original_default_mode {
            signed_default_mode.clone_from(&proxy_mode_name);
        }
        if result.playback_infos.contains_key(&proxy_mode_name) {
            if hide_header_backed_originals && original_has_transport_headers {
                result.playback_infos.remove(&mode_name);
            }
            continue;
        }

        let mut proxy_info = original_info.clone();
        proxy_info.urls = signed_urls(&mode_name, &original_info, signing);
        proxy_info.headers.clear();
        proxy_info.cors_proxy_required = false;

        for (index, subtitle) in proxy_info.subtitles.iter_mut().enumerate() {
            subtitle.url = signing.signed_url(&format!("subtitle/{mode_name}/{index}"));
            subtitle.headers.clear();
        }

        result.playback_infos.insert(proxy_mode_name, proxy_info);

        if hide_header_backed_originals && original_has_transport_headers {
            result.playback_infos.remove(&mode_name);
        }
    }

    result.default_mode = signed_default_mode;
}

fn signed_playback_default_needs_proxy(result: &PlaybackResult) -> bool {
    result
        .playback_infos
        .get(&result.default_mode)
        .is_some_and(playback_info_has_transport_headers)
}

/// Bundle of all in-process provider adapters.
///
/// `ProvidersManager` remains the source of truth for provider type
/// availability and playback resolution. `ProviderSet` is the startup-time
/// bundle used to wire proxy-capable adapters into `ProxyProviderRegistry`.
#[derive(Clone)]
pub struct ProviderSet {
    pub alist: std::sync::Arc<AlistProvider>,
    pub bilibili: std::sync::Arc<BilibiliProvider>,
    pub emby: std::sync::Arc<EmbyProvider>,
    pub direct_url: std::sync::Arc<DirectUrlProvider>,
    pub rtmp: std::sync::Arc<RtmpProvider>,
    pub live_proxy: std::sync::Arc<LiveProxyProvider>,
}

fn provider_http_client_for_ssrf_guard(
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
) -> crate::Result<reqwest::Client> {
    synctv_media_providers::build_provider_http_client(ssrf_guard).map_err(|error| {
        crate::Error::Internal(format!("Failed to build provider HTTP client: {error}"))
    })
}

impl ProviderSet {
    /// Build built-in providers with a shared local provider HTTP client and
    /// explicit global SSRF policy.
    pub fn new_with_ssrf_guard(
        provider_instance_manager: std::sync::Arc<crate::service::RemoteProviderManager>,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
    ) -> crate::Result<Self> {
        let provider_http_client = provider_http_client_for_ssrf_guard(ssrf_guard.clone())?;
        Ok(Self::new_with_provider_http_client_and_ssrf_guard(
            provider_instance_manager,
            provider_http_client,
            ssrf_guard,
        ))
    }

    /// Build built-in providers with explicit local provider transport and
    /// global SSRF policy.
    #[must_use]
    pub fn new_with_provider_http_client_and_ssrf_guard(
        provider_instance_manager: std::sync::Arc<crate::service::RemoteProviderManager>,
        provider_http_client: reqwest::Client,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
    ) -> Self {
        let client_manager = std::sync::Arc::new(
            ProviderClientManager::new_with_provider_http_client(provider_http_client),
        );
        Self {
            alist: std::sync::Arc::new(AlistProvider::with_client_manager(
                provider_instance_manager.clone(),
                client_manager.clone(),
            )),
            bilibili: std::sync::Arc::new(BilibiliProvider::with_client_manager(
                provider_instance_manager.clone(),
                client_manager.clone(),
            )),
            emby: std::sync::Arc::new(EmbyProvider::with_client_manager(
                provider_instance_manager,
                client_manager,
            )),
            direct_url: std::sync::Arc::new(DirectUrlProvider::new_with_ssrf_guard(
                ssrf_guard.clone(),
            )),
            rtmp: std::sync::Arc::new(RtmpProvider::new()),
            live_proxy: std::sync::Arc::new(LiveProxyProvider::new_with_ssrf_guard(ssrf_guard)),
        }
    }

    /// Build a `ProxyProviderRegistry` from this provider adapter bundle.
    ///
    /// Registers proxy-capable adapters under their canonical provider names.
    /// This registry is only for HTTP proxy resolution, not provider
    /// availability or playback provider selection.
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
        duration_seconds: None,
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
        synctv_media_providers::PROVIDER_USER_AGENT.to_string(),
    );
    headers
}

#[must_use]
pub(crate) fn signed_provider_proxy_url(
    provider_name: &str,
    version: &str,
    action: &str,
    signing_key: &ProxySigningKey,
    room_id: &str,
    user_id: &str,
    expires_at: i64,
) -> String {
    build_signed_proxy_url(SignedProxyUrlRequest {
        provider: provider_name,
        version,
        action,
        signing_key,
        room_id,
        user_id,
        expires_at,
    })
}

#[must_use]
pub(crate) fn build_versioned_playback_response(
    mut result: PlaybackResult,
    version: &str,
    expires_at: i64,
    ctx: &ProviderContext<'_>,
    rewrite_for_proxy: impl FnOnce(&mut PlaybackResult, &str, &ProxySigningKey, &str, &str, i64),
) -> PlaybackResult {
    // Response finalization happens here, but provider policy stays in the
    // callback supplied by the provider. The helper provides the cached version,
    // room/user binding, signing key, and expiry; the provider decides which
    // modes exist, which URLs are direct/proxy, which headers are visible, and
    // which mode becomes default.
    if let (Some(signing_key), Some(room_id), Some(user_id)) =
        (ctx.signing_key, ctx.proxy_room_id(), ctx.proxy_user_id())
    {
        rewrite_for_proxy(
            &mut result,
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
) -> std::result::Result<(), ProviderError> {
    store
        .set(&format!("v:{}", versioned.version), versioned, ttl)
        .await
        .map_err(|e| {
            ProviderError::Internal(format!(
                "Provider '{provider_name}' failed to persist signed proxy version mapping: {e}"
            ))
        })
}

pub(crate) async fn build_cached_versioned_playback_response(
    versioned: VersionedPlayback,
    provider_name: &str,
    ctx: &ProviderContext<'_>,
    rewrite_for_proxy: impl FnOnce(&mut PlaybackResult, &str, &ProxySigningKey, &str, &str, i64),
) -> std::result::Result<PlaybackResult, ProviderError> {
    // Cache hits must rebuild the same signed proxy surface as fresh provider
    // responses. Signed responses require the version mapping to be present so
    // every URL emitted by the provider can resolve back to this playback.
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

    Ok(build_versioned_playback_response(
        versioned.result,
        &versioned.version,
        versioned.expires_at,
        ctx,
        rewrite_for_proxy,
    ))
}

pub(crate) async fn cache_versioned_playback_and_build_response(
    result: PlaybackResult,
    provider_name: &str,
    cache_key: &str,
    cache_ttl: std::time::Duration,
    ctx: &ProviderContext<'_>,
    rewrite_for_proxy: impl FnOnce(&mut PlaybackResult, &str, &ProxySigningKey, &str, &str, i64),
) -> std::result::Result<PlaybackResult, ProviderError> {
    // This helper stores the provider result and version index. The provider's
    // `rewrite_for_proxy` callback performs response finalization for the
    // current request, so provider-specific signing timing, default-mode choice,
    // header exposure, manifest metadata, and live lifecycle data remain in the
    // provider implementation.
    let ttl_secs = i64::try_from(cache_ttl.as_secs()).map_err(|_| {
        ProviderError::Internal(format!(
            "Provider '{provider_name}' playback cache TTL exceeds i64::MAX seconds"
        ))
    })?;
    let expires_at = chrono::Utc::now()
        .timestamp()
        .checked_add(ttl_secs)
        .ok_or_else(|| {
            ProviderError::Internal(format!(
                "Provider '{provider_name}' playback cache expiry exceeds i64::MAX"
            ))
        })?;
    let versioned = VersionedPlayback {
        version: synctv_common::snanoid!(16),
        result: result.clone(),
        expires_at,
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

    Ok(build_versioned_playback_response(
        result,
        &versioned.version,
        versioned.expires_at,
        ctx,
        rewrite_for_proxy,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{RoomId, UserId};
    use crate::provider::store::{InMemoryProviderStore, StoreError, StoreLockGuard};
    use crate::proxy_signature::ProxySigningKey;
    use crate::test_helpers::{TestOptionExt, TestResultExt};
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
            duration_seconds: None,
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

    fn test_delivery_signer(
        result: &mut PlaybackResult,
        version: &str,
        signing_key: &ProxySigningKey,
        room_id: &str,
        user_id: &str,
        expires_at: i64,
    ) {
        if let Some(info) = result.playback_infos.get_mut("direct") {
            info.urls = vec![signed_provider_proxy_url(
                "test_provider",
                version,
                "stream",
                signing_key,
                room_id,
                user_id,
                expires_at,
            )];
            info.headers.clear();
            info.cors_proxy_required = false;
        }
    }

    #[tokio::test]
    async fn test_cache_versioned_playback_requires_store_for_signed_proxy() {
        let signing_key = ProxySigningKey::try_derive_from(b"test-jwt-secret-that-is-long-enough")
            .checked("test proxy signing key should derive");
        let ctx = ProviderContext::new("test")
            .with_user_id(UserId::expect_positive(1))
            .with_room_id(RoomId::expect_positive(10))
            .with_signing_key(&signing_key);

        let err = cache_versioned_playback_and_build_response(
            playback_result(),
            "test_provider",
            "playback:test",
            std::time::Duration::from_mins(1),
            &ctx,
            test_delivery_signer,
        )
        .await
        .failed("operation should fail");

        assert!(matches!(err, ProviderError::Internal(_)));
        assert!(
            err.to_string().contains("provider store"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_cache_versioned_playback_fails_closed_when_mapping_persist_fails() {
        let signing_key = ProxySigningKey::try_derive_from(b"test-jwt-secret-that-is-long-enough")
            .checked("test proxy signing key should derive");
        let ctx = ProviderContext::new("test")
            .with_user_id(UserId::expect_positive(1))
            .with_room_id(RoomId::expect_positive(10))
            .with_signing_key(&signing_key);
        let ctx = ctx.with_store(Arc::new(FailVersionMappingStore {
            inner: InMemoryProviderStore::new(16),
        }));

        let err = cache_versioned_playback_and_build_response(
            playback_result(),
            "test_provider",
            "playback:test",
            std::time::Duration::from_mins(1),
            &ctx,
            test_delivery_signer,
        )
        .await
        .failed("operation should fail");

        assert!(matches!(err, ProviderError::Internal(_)));
        assert!(
            err.to_string().contains("version mapping"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_cached_signed_playback_repairs_missing_version_mapping() {
        let signing_key = ProxySigningKey::try_derive_from(b"test-jwt-secret-that-is-long-enough")
            .checked("test proxy signing key should derive");
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(16));
        let ctx = ProviderContext::new("test")
            .with_user_id(UserId::expect_positive(1))
            .with_room_id(RoomId::expect_positive(10))
            .with_signing_key(&signing_key)
            .with_store(store.clone());
        let versioned = VersionedPlayback {
            version: "cached-version".to_string(),
            result: {
                let mut result = playback_result();
                result
                    .playback_infos
                    .get_mut("direct")
                    .checked("direct playback info")
                    .cors_proxy_required = true;
                result
            },
            expires_at: chrono::Utc::now().timestamp() + 60,
        };

        let signed = build_cached_versioned_playback_response(
            versioned.clone(),
            "test_provider",
            &ctx,
            test_delivery_signer,
        )
        .await;

        assert!(signed.is_ok(), "cached signing should succeed: {signed:?}");
        let stored: Option<VersionedPlayback> = store
            .get("v:cached-version")
            .await
            .checked("operation should succeed");
        assert!(
            stored.is_some(),
            "signed cache hit must restore version mapping"
        );
        let url = &signed.checked("operation should succeed").playback_infos["direct"].urls[0];
        assert!(url.contains("/test_provider/cached-version/stream"));

        let query = url.split('?').nth(1).checked("signed proxy URL query");
        let claims = signing_key
            .parse_and_verify_query(query, "test_provider", "cached-version")
            .checked("valid signed query");
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

    #[tokio::test]
    async fn test_provider_set_uses_explicit_ssrf_guard_for_builtin_url_validators() {
        let direct_url =
            DirectUrlProvider::new_with_ssrf_guard(synctv_common::ssrf::SsrfGuard::disabled());
        let live_proxy =
            LiveProxyProvider::new_with_ssrf_guard(synctv_common::ssrf::SsrfGuard::disabled());
        let ctx = ProviderContext::new("test");

        direct_url
            .validate_source_config(
                &ctx,
                SourceConfig::media(&serde_json::json!({ "url": "http://127.0.0.1/video.mp4" })),
            )
            .await
            .checked("explicit disabled SSRF guard should allow DirectUrl loopback");
        live_proxy
            .validate_source_config(
                &ctx,
                SourceConfig::media(&serde_json::json!({ "url": "http://127.0.0.1/live.flv" })),
            )
            .await
            .checked("explicit disabled SSRF guard should allow LiveProxy loopback");
    }
}
