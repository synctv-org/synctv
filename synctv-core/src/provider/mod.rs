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
pub mod playback_transport;
pub mod provider_client;
pub mod store;
pub mod traits;

// Shared helpers
mod live_helpers;

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
    PlaybackAudioCapability, PlaybackClientProfile, PlaybackContainer, PlaybackStreamPreference,
    PlaybackSubtitlePreference, PlaybackVideoCodec,
};
pub use playback_transport::{PlaybackTransportAction, PlaybackTransportServices};
pub use provider_client::ProviderClientManager;
pub use store::{
    InMemoryProviderStore, PrefixedProviderStore, ProviderStore, ProviderStoreExt,
    ProviderStoreRegistry, ProviderStoreResolver, RedisProviderStore, StoreError, StoreLockGuard,
    VersionedPlayback,
};
pub use synctv_common::{ExecutionControl, ExecutionControlError};
pub use traits::{
    BilibiliLiveDanmakuEvent, BilibiliLiveDanmakuEventKind, BilibiliLiveDanmakuProvider,
    BilibiliLiveDanmakuStream, DirectoryItem, DynamicBrowsePathSegment, DynamicFolder,
    DynamicListQuery, ItemType, MediaProvider, NextPlayItem, PlaybackInfo, PlaybackResult,
    PreparedSourceConfig, ProviderCredentialDependency, SourceConfig, SourceConfigKind,
};

use crate::models::media::{PlaybackMedia, PlaybackMediaProvider, PlaybackRtmpMedia};
use crate::models::{normalize_provider_instance_name, MediaId, RoomId};
use std::future::Future;
use std::time::Duration;

pub(crate) fn subtitle_headers_for_proxy(
    media_headers: &std::collections::HashMap<String, String>,
    subtitle: &crate::models::media::PlaybackSubtitle,
) -> std::collections::HashMap<String, String> {
    let mut merged = media_headers.clone();
    merged.extend(subtitle.upstream_headers());
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
    info.medias.iter().any(|media| {
        let format = media.format.as_str();
        format == "m3u8" || format == "hls" || mode_name.contains("hls")
    })
}

fn playback_info_has_transport_headers(info: &PlaybackInfo) -> bool {
    info.medias
        .iter()
        .any(|media| !media.upstream_headers().is_empty())
        || info
            .subtitles
            .iter()
            .any(|subtitle| !subtitle.upstream_headers().is_empty())
}

fn signed_playback_default_needs_proxy(result: &PlaybackResult) -> bool {
    result
        .playback_infos
        .get(&result.default_mode)
        .is_some_and(playback_info_has_transport_headers)
}

/// Bundle of all in-process provider adapters.
///
/// Playback-provider transports call these concrete provider adapters through
/// provider-specific impl modules.
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
    use std::collections::HashMap;

    let live_expires_at = chrono::Utc::now().timestamp() + 30;

    let mut playback_infos = HashMap::new();

    playback_infos.insert(
        "hls".to_string(),
        PlaybackInfo {
            medias: vec![PlaybackMedia {
                name: "HLS".to_string(),
                format: "m3u8".to_string(),
                expire_at: chrono::DateTime::from_timestamp(live_expires_at, 0),
                metadata: None,
                provider: PlaybackMediaProvider::Rtmp(PlaybackRtmpMedia::HlsPlaylist {
                    version: String::new(),
                    expires_at: live_expires_at,
                    room_id,
                    media_id,
                }),
            }],
            default_media_index: None,
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            default_danmaku_index: None,
        },
    );

    playback_infos.insert(
        "flv".to_string(),
        PlaybackInfo {
            medias: vec![PlaybackMedia {
                name: "FLV".to_string(),
                format: "flv".to_string(),
                expire_at: chrono::DateTime::from_timestamp(live_expires_at, 0),
                metadata: None,
                provider: PlaybackMediaProvider::Rtmp(PlaybackRtmpMedia::FlvStream {
                    version: String::new(),
                    expires_at: live_expires_at,
                    room_id,
                    media_id,
                }),
            }],
            default_media_index: None,
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            default_danmaku_index: None,
        },
    );

    let metadata = crate::models::PlaybackMetadata {
        is_live: Some(true),
        media_id: Some(media_id),
        room_id: Some(room_id),
        ..Default::default()
    };

    PlaybackResult {
        playback_infos,
        default_mode: "hls".to_string(),
        provider: RtmpProvider::NAME.to_string(),
        provider_instance_name: None,
        duration_seconds: None,
        is_live: Some(true),
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
pub(crate) fn build_versioned_playback_response(
    mut result: PlaybackResult,
    provider_name: &str,
    provider_instance_name: Option<&str>,
    version: &str,
    expires_at: i64,
    mark_provider_resources: impl FnOnce(&mut PlaybackResult, &str, i64),
) -> PlaybackResult {
    result.provider = provider_name.to_string();
    result.provider_instance_name = provider_instance_name.map(str::to_string);
    mark_provider_resources(&mut result, version, expires_at);
    result
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
    mark_provider_resources: impl FnOnce(&mut PlaybackResult, &str, i64),
) -> std::result::Result<PlaybackResult, ProviderError> {
    let store = ctx.store.as_ref().ok_or_else(|| {
        ProviderError::Internal(format!(
            "Provider '{provider_name}' cannot generate playback transport without a provider store"
        ))
    })?;
    persist_versioned_mapping(
        store.as_ref(),
        &versioned,
        remaining_versioned_ttl(versioned.expires_at),
        provider_name,
    )
    .await?;

    Ok(build_versioned_playback_response(
        versioned.result,
        provider_name,
        ctx.provider_instance_name(),
        &versioned.version,
        versioned.expires_at,
        mark_provider_resources,
    ))
}

pub(crate) async fn cache_versioned_playback_and_build_response(
    result: PlaybackResult,
    provider_name: &str,
    cache_key: &str,
    cache_ttl: std::time::Duration,
    ctx: &ProviderContext<'_>,
    mark_provider_resources: impl FnOnce(&mut PlaybackResult, &str, i64),
) -> std::result::Result<PlaybackResult, ProviderError> {
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
        persist_versioned_mapping(store.as_ref(), &versioned, cache_ttl, provider_name).await?;
    } else {
        return Err(ProviderError::Internal(format!(
            "Provider '{provider_name}' cannot generate playback transport without a provider store"
        )));
    }

    Ok(build_versioned_playback_response(
        result,
        provider_name,
        ctx.provider_instance_name(),
        &versioned.version,
        versioned.expires_at,
        mark_provider_resources,
    ))
}

const PLAYBACK_CACHE_LOCK_TTL: Duration = Duration::from_secs(30);
const PLAYBACK_CACHE_LOCK_WAIT_ATTEMPTS: usize = 5;
const PLAYBACK_CACHE_LOCK_WAIT_DELAY: Duration = Duration::from_millis(50);

async fn read_fresh_versioned_playback(
    store: &dyn ProviderStore,
    cache_key: &str,
) -> Option<VersionedPlayback> {
    match store.get::<VersionedPlayback>(cache_key).await {
        Ok(Some(cached)) if !cached.is_expired() => Some(cached),
        _ => None,
    }
}

async fn wait_for_fresh_versioned_playback(
    store: &dyn ProviderStore,
    cache_key: &str,
) -> Option<VersionedPlayback> {
    for _ in 0..PLAYBACK_CACHE_LOCK_WAIT_ATTEMPTS {
        tokio::time::sleep(PLAYBACK_CACHE_LOCK_WAIT_DELAY).await;
        if let Some(cached) = read_fresh_versioned_playback(store, cache_key).await {
            return Some(cached);
        }
    }
    None
}

pub(crate) async fn cached_versioned_playback_or_fill<F, Fut>(
    provider_name: &'static str,
    cache_key: &str,
    cache_ttl: Duration,
    ctx: &ProviderContext<'_>,
    mark_provider_resources: fn(&mut PlaybackResult, &str, i64),
    fill: F,
) -> std::result::Result<PlaybackResult, ProviderError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = std::result::Result<PlaybackResult, ProviderError>>,
{
    let store = ctx.store.as_ref().ok_or_else(|| {
        ProviderError::Internal(format!(
            "Provider '{provider_name}' cannot generate playback transport without a provider store"
        ))
    })?;

    if let Some(cached) = read_fresh_versioned_playback(store.as_ref(), cache_key).await {
        return build_cached_versioned_playback_response(
            cached,
            provider_name,
            ctx,
            mark_provider_resources,
        )
        .await;
    }

    let lock_key = format!("lock:{cache_key}");
    let lock = match store.lock(&lock_key, PLAYBACK_CACHE_LOCK_TTL).await {
        Ok(lock) => Some(lock),
        Err(error) => {
            tracing::warn!(
                provider = provider_name,
                cache_key = cache_key,
                error = %error,
                "Provider playback cache lock unavailable; waiting for peer cache fill"
            );
            if let Some(cached) = wait_for_fresh_versioned_playback(store.as_ref(), cache_key).await
            {
                return build_cached_versioned_playback_response(
                    cached,
                    provider_name,
                    ctx,
                    mark_provider_resources,
                )
                .await;
            }
            None
        }
    };

    if lock.is_some() {
        if let Some(cached) = read_fresh_versioned_playback(store.as_ref(), cache_key).await {
            return build_cached_versioned_playback_response(
                cached,
                provider_name,
                ctx,
                mark_provider_resources,
            )
            .await;
        }
    }

    let result = fill().await?;
    cache_versioned_playback_and_build_response(
        result,
        provider_name,
        cache_key,
        cache_ttl,
        ctx,
        mark_provider_resources,
    )
    .await
}
