// Media Provider System
// Three-tier architecture:
// Tier 1: synctv-media-providers (provider upstream clients)
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
pub(crate) mod access;
pub(crate) mod context;
pub(crate) mod credential_resolver;
pub(crate) mod error;
pub(crate) mod playback_profile;
pub(crate) mod playback_transport;
pub(crate) mod provider_client;
pub(crate) mod store;
pub(crate) mod traits;
pub(crate) mod upstream_transport;

// Shared helpers
mod live_helpers;

// MediaProvider implementations (adapters)
mod alist;
mod bilibili;
mod direct_url;
mod emby;
mod live_proxy;
mod rtmp;

use std::sync::{Arc, LazyLock};

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
pub use playback_transport::{LiveFlvAccess, PlaybackTransportAction, PlaybackTransportServices};
pub use provider_client::ProviderClientManager;
pub use store::{
    InMemoryProviderStore, PrefixedProviderStore, ProviderStore, ProviderStoreExt,
    ProviderStoreRegistry, ProviderStoreResolver, RedisProviderStore, StoreError, StoreLockGuard,
    VersionedPlayback,
};
pub use synctv_common::{ExecutionControl, ExecutionControlError};
pub use traits::{
    BilibiliLiveDanmakuEvent, BilibiliLiveDanmakuEventKind, BilibiliLiveDanmakuProvider,
    BilibiliLiveDanmakuStream, DirectoryItem, DirectoryItemThumbnail, DynamicBrowsePathSegment,
    DynamicFolder, DynamicListQuery, ItemType, MediaProvider, NextPlayItem, PlaybackInfo,
    PlaybackResult, PreparedSourceConfig, ProviderCredentialDependency, SourceConfig,
    SourceConfigKind, SourceCover,
};

use crate::models::media::{PlaybackMedia, PlaybackMediaProvider, PlaybackRtmpMedia};
use crate::models::{normalize_provider_instance_name, MediaId, RoomId};
use std::future::Future;
use std::time::Duration;

use crate::cache::{SingleFlight, SingleFlightError};

pub(crate) fn subtitle_headers_for_proxy(
    media_headers: &std::collections::HashMap<String, String>,
    subtitle: &crate::models::media::PlaybackSubtitle,
) -> std::collections::HashMap<String, String> {
    let mut merged = media_headers.clone();
    merged.extend(subtitle.upstream_headers());
    merged
}

// Re-export providers
pub use alist::{
    AlistListItem, AlistListRequest, AlistListResponse, AlistLoginAndPersistRequest,
    AlistLoginCredential, AlistLoginRequest, AlistMeRequest, AlistMeResponse,
    AlistPersistLoginCredentialRequest, AlistPersistedLoginResponse, AlistProvider,
    AlistSearchItem, AlistSearchRequest, AlistSearchResponse,
};
pub use bilibili::{
    BilibiliCaptchaResponse, BilibiliDashManifestMode, BilibiliDashProxyUrlMapper,
    BilibiliLiveDanmuHost, BilibiliLiveDanmuInfoRequest, BilibiliLiveDanmuInfoResponse,
    BilibiliMatchRequest, BilibiliMatchResponse, BilibiliPageInfo, BilibiliParseLivePageRequest,
    BilibiliParsePgcPageRequest, BilibiliParseVideoPageRequest, BilibiliPersistedQrLoginResponse,
    BilibiliProvider, BilibiliQrCodeResponse, BilibiliQrLoginRequest, BilibiliQrLoginResponse,
    BilibiliQrLoginStatus, BilibiliSmsLoginRequest, BilibiliSmsLoginResponse,
    BilibiliSmsLoginTokenCodec, BilibiliSmsRequest, BilibiliSmsResponse, BilibiliUserInfoRequest,
    BilibiliUserInfoResponse, BilibiliVideoInfo, DASH_MANIFEST_METADATA_KEY, LIVE_DANMAKU_FORMAT,
    LIVE_DANMAKU_TRACK_NAME,
};
pub use direct_url::DirectUrlProvider;
pub use emby::{
    EmbyListItem, EmbyListRequest, EmbyListResponse, EmbyLoginAndPersistRequest,
    EmbyLoginCredential, EmbyLoginRequest, EmbyLoginResponse, EmbyMeRequest, EmbyMeResponse,
    EmbyPersistedLoginResponse, EmbyProvider, EmbyUserPolicy,
};
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

    #[must_use]
    pub fn with_credential_repo(
        &self,
        credential_repo: Arc<crate::repository::UserProviderCredentialRepository>,
    ) -> Self {
        Self {
            alist: Arc::new(self.alist.with_credential_repo(credential_repo.clone())),
            bilibili: Arc::new(self.bilibili.with_credential_repo(credential_repo.clone())),
            emby: Arc::new(self.emby.with_credential_repo(credential_repo)),
            direct_url: self.direct_url.clone(),
            rtmp: self.rtmp.clone(),
            live_proxy: self.live_proxy.clone(),
        }
    }
}

pub(crate) fn bound_provider_instance_name<'a>(ctx: &'a ProviderContext<'a>) -> Option<&'a str> {
    normalize_provider_instance_name(ctx.provider_instance_name())
}

#[derive(Debug, Clone, thiserror::Error)]
enum ProviderPlaybackFillError {
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("Missing required field: {0}")]
    MissingField(String),
    #[error("Network error: {0}")]
    NetworkError(String),
    #[error("Authentication required")]
    AuthRequired,
    #[error("Credential required")]
    CredentialRequired,
    #[error("Invalid credential type")]
    InvalidCredentialType,
    #[error("Provider authentication failed: {0}")]
    Authentication(String),
    #[error("Resource not found")]
    NotFound,
    #[error("Provider API error: {0}")]
    ApiError(String),
    #[error("Upstream HTTP {status} for {url}")]
    UpstreamHttp { status: u16, url: String },
    #[error("Unsupported format: {0}")]
    UnsupportedFormat(String),
    #[error("Parse error: {0}")]
    ParseError(String),
    #[error("Missing provider instance")]
    MissingInstance,
    #[error("Provider instance not found: {0}")]
    InstanceNotFound(String),
    #[error("Credential not found: {0}")]
    CredentialNotFound(String),
    #[error("Credential expired: {0}")]
    CredentialExpired(String),
    #[error("Credential encryption required for sensitive provider '{0}'. Configure credential_encryption in server settings.")]
    EncryptionRequired(&'static str),
    #[error("Route registration failed: {0}")]
    RouteRegistrationFailed(String),
    #[error("IO error: {0}")]
    IoError(String),
    #[error("Internal error: {0}")]
    Internal(String),
    #[error("JSON error: {0}")]
    JsonError(String),
}

impl From<ProviderError> for ProviderPlaybackFillError {
    fn from(error: ProviderError) -> Self {
        match error {
            ProviderError::InvalidUrl(message) => Self::InvalidUrl(message),
            ProviderError::InvalidConfig(message) => Self::InvalidConfig(message),
            ProviderError::MissingField(message) => Self::MissingField(message),
            ProviderError::NetworkError(message) => Self::NetworkError(message),
            ProviderError::AuthRequired => Self::AuthRequired,
            ProviderError::CredentialRequired => Self::CredentialRequired,
            ProviderError::InvalidCredentialType => Self::InvalidCredentialType,
            ProviderError::Authentication(message) => Self::Authentication(message),
            ProviderError::NotFound => Self::NotFound,
            ProviderError::ApiError(message) => Self::ApiError(message),
            ProviderError::UpstreamHttp { status, url } => Self::UpstreamHttp { status, url },
            ProviderError::UnsupportedFormat(message) => Self::UnsupportedFormat(message),
            ProviderError::ParseError(message) => Self::ParseError(message),
            ProviderError::MissingInstance => Self::MissingInstance,
            ProviderError::InstanceNotFound(message) => Self::InstanceNotFound(message),
            ProviderError::CredentialNotFound(message) => Self::CredentialNotFound(message),
            ProviderError::CredentialExpired(message) => Self::CredentialExpired(message),
            ProviderError::EncryptionRequired(provider) => Self::EncryptionRequired(provider),
            ProviderError::RouteRegistrationFailed(message) => {
                Self::RouteRegistrationFailed(message)
            }
            ProviderError::IoError(error) => Self::IoError(error.to_string()),
            ProviderError::Internal(message) => Self::Internal(message),
            ProviderError::JsonError(error) => Self::JsonError(error.to_string()),
        }
    }
}

impl From<ProviderPlaybackFillError> for ProviderError {
    fn from(error: ProviderPlaybackFillError) -> Self {
        match error {
            ProviderPlaybackFillError::InvalidUrl(message) => Self::InvalidUrl(message),
            ProviderPlaybackFillError::InvalidConfig(message) => Self::InvalidConfig(message),
            ProviderPlaybackFillError::MissingField(message) => Self::MissingField(message),
            ProviderPlaybackFillError::NetworkError(message) => Self::NetworkError(message),
            ProviderPlaybackFillError::AuthRequired => Self::AuthRequired,
            ProviderPlaybackFillError::CredentialRequired => Self::CredentialRequired,
            ProviderPlaybackFillError::InvalidCredentialType => Self::InvalidCredentialType,
            ProviderPlaybackFillError::Authentication(message) => Self::Authentication(message),
            ProviderPlaybackFillError::NotFound => Self::NotFound,
            ProviderPlaybackFillError::ApiError(message) => Self::ApiError(message),
            ProviderPlaybackFillError::UpstreamHttp { status, url } => {
                Self::UpstreamHttp { status, url }
            }
            ProviderPlaybackFillError::UnsupportedFormat(message) => {
                Self::UnsupportedFormat(message)
            }
            ProviderPlaybackFillError::ParseError(message) => Self::ParseError(message),
            ProviderPlaybackFillError::MissingInstance => Self::MissingInstance,
            ProviderPlaybackFillError::InstanceNotFound(message) => Self::InstanceNotFound(message),
            ProviderPlaybackFillError::CredentialNotFound(message) => {
                Self::CredentialNotFound(message)
            }
            ProviderPlaybackFillError::CredentialExpired(message) => {
                Self::CredentialExpired(message)
            }
            ProviderPlaybackFillError::EncryptionRequired(provider) => {
                Self::EncryptionRequired(provider)
            }
            ProviderPlaybackFillError::RouteRegistrationFailed(message) => {
                Self::RouteRegistrationFailed(message)
            }
            ProviderPlaybackFillError::IoError(message) => {
                Self::IoError(std::io::Error::other(message))
            }
            ProviderPlaybackFillError::Internal(message) => Self::Internal(message),
            ProviderPlaybackFillError::JsonError(message) => {
                Self::JsonError(serde_json::Error::io(std::io::Error::other(message)))
            }
        }
    }
}

static PLAYBACK_FILL_SINGLEFLIGHT: LazyLock<
    SingleFlight<String, VersionedPlayback, ProviderPlaybackFillError>,
> = LazyLock::new(SingleFlight::new);
static SOURCE_COVER_CACHE: LazyLock<ProviderSourceCoverCache> =
    LazyLock::new(ProviderSourceCoverCache::new);

#[must_use]
pub fn provider_requires_credential_repo(provider_name: &str) -> bool {
    matches!(provider_name, AlistProvider::NAME | EmbyProvider::NAME)
}

#[must_use]
pub fn build_live_playback(media_id: MediaId, room_id: RoomId) -> PlaybackResult {
    use std::collections::HashMap;

    let live_expires_at = crate::SystemClock.now().timestamp() + 30;

    let mut playback_infos = HashMap::new();

    playback_infos.insert(
        "hls".to_string(),
        PlaybackInfo {
            thumbnail: None,
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
            thumbnail: None,
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

    let metadata = crate::models::PlaybackMetadata::Live(crate::models::LivePlaybackMetadata {
        media_id,
        room_id,
    });

    PlaybackResult {
        playback_infos,
        default_mode: "hls".to_string(),
        provider: RtmpProvider::NAME.to_string(),
        provider_instance_name: None,
        duration_seconds: None,
        is_live: Some(true),
        metadata: Some(metadata),
    }
}

/// Standard Bilibili upstream headers required for CDN requests.
///
/// These headers must be sent with all Bilibili media requests (video, audio, subtitles)
/// to avoid being blocked by Bilibili's CDN. Shared between playback result
/// metadata and transport adapters.
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
    let remaining_secs = (expires_at - crate::SystemClock.now().timestamp())
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
    let versioned =
        cache_versioned_playback(result, provider_name, cache_key, cache_ttl, ctx).await?;

    build_cached_versioned_playback_response(versioned, provider_name, ctx, mark_provider_resources)
        .await
}

async fn cache_versioned_playback(
    result: PlaybackResult,
    provider_name: &str,
    cache_key: &str,
    cache_ttl: std::time::Duration,
    ctx: &ProviderContext<'_>,
) -> std::result::Result<VersionedPlayback, ProviderError> {
    let ttl_secs = i64::try_from(cache_ttl.as_secs()).map_err(|_| {
        ProviderError::Internal(format!(
            "Provider '{provider_name}' playback cache TTL exceeds i64::MAX seconds"
        ))
    })?;
    let expires_at = crate::SystemClock
        .now()
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
    } else {
        return Err(ProviderError::Internal(format!(
            "Provider '{provider_name}' cannot generate playback transport without a provider store"
        )));
    }

    Ok(versioned)
}

const PLAYBACK_CACHE_LOCK_TTL: Duration = Duration::from_secs(30);
const PLAYBACK_CACHE_LOCK_WAIT_ATTEMPTS: usize = 5;
const PLAYBACK_CACHE_LOCK_WAIT_DELAY: Duration = Duration::from_millis(50);
const SOURCE_COVER_CACHE_LOCK_TTL: Duration = Duration::from_secs(10);
const SOURCE_COVER_CACHE_LOCK_WAIT_ATTEMPTS: usize = 3;
const SOURCE_COVER_CACHE_LOCK_WAIT_DELAY: Duration = Duration::from_millis(40);

struct ProviderSourceCoverCache {
    l1: moka::future::Cache<String, Option<SourceCover>>,
    singleflight: SingleFlight<String, Option<SourceCover>, ProviderPlaybackFillError>,
}

impl ProviderSourceCoverCache {
    fn new() -> Self {
        Self {
            l1: moka::future::Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(30))
                .build(),
            singleflight: SingleFlight::new(),
        }
    }

    async fn get_l1(&self, key: &str) -> Option<Option<SourceCover>> {
        self.l1.get(key).await
    }

    async fn put_l1(&self, key: &str, cover: Option<SourceCover>) {
        self.l1.insert(key.to_string(), cover).await;
    }

    async fn get_l2(
        &self,
        store: &dyn ProviderStore,
        l1_key: &str,
        l2_key: &str,
    ) -> Option<Option<SourceCover>> {
        let cached = read_cached_source_cover(store, l2_key).await?;
        self.put_l1(l1_key, cached.clone()).await;
        Some(cached)
    }

    async fn wait_for_l2(
        &self,
        store: &dyn ProviderStore,
        l1_key: &str,
        l2_key: &str,
    ) -> Option<Option<SourceCover>> {
        for _ in 0..SOURCE_COVER_CACHE_LOCK_WAIT_ATTEMPTS {
            tokio::time::sleep(SOURCE_COVER_CACHE_LOCK_WAIT_DELAY).await;
            if let Some(cached) = self.get_l2(store, l1_key, l2_key).await {
                return Some(cached);
            }
        }
        None
    }

    async fn set_l2(
        &self,
        provider_name: &'static str,
        store: &dyn ProviderStore,
        l2_key: &str,
        cover: Option<&SourceCover>,
        ttl: Duration,
    ) {
        if let Err(error) = store.set(l2_key, &cover, ttl).await {
            tracing::debug!(
                provider = provider_name,
                cache_key = l2_key,
                error = %error,
                "Provider source cover L2 write failed"
            );
        }
    }

    async fn get_or_fill<F, Fut>(
        &self,
        provider_name: &'static str,
        cache_key: &str,
        cache_ttl: Duration,
        ctx: &ProviderContext<'_>,
        fill: F,
    ) -> std::result::Result<Option<SourceCover>, ProviderError>
    where
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = std::result::Result<Option<SourceCover>, ProviderError>> + Send,
    {
        let l1_key = format!("{provider_name}:{cache_key}");
        if let Some(cached) = self.get_l1(&l1_key).await {
            return Ok(cached);
        }

        let Some(store) = ctx.store.as_ref() else {
            let cover = fill().await?;
            self.put_l1(&l1_key, cover.clone()).await;
            return Ok(cover);
        };

        if let Some(cached) = self.get_l2(store.as_ref(), &l1_key, cache_key).await {
            return Ok(cached);
        }

        let lock_key = format!("lock:{cache_key}");
        let lock = match store.lock(&lock_key, SOURCE_COVER_CACHE_LOCK_TTL).await {
            Ok(lock) => Some(lock),
            Err(error) => {
                tracing::debug!(
                    provider = provider_name,
                    cache_key = cache_key,
                    error = %error,
                    "Provider source cover L2 lock unavailable; waiting for peer fill"
                );
                if let Some(cached) = self.wait_for_l2(store.as_ref(), &l1_key, cache_key).await {
                    return Ok(cached);
                }
                None
            }
        };

        if lock.is_some() {
            if let Some(cached) = self.get_l2(store.as_ref(), &l1_key, cache_key).await {
                return Ok(cached);
            }
        }

        let singleflight_key = format!("{provider_name}:{cache_key}");
        let cover = self
            .singleflight
            .do_work(singleflight_key, async {
                if let Some(cached) = read_cached_source_cover(store.as_ref(), cache_key).await {
                    return Ok(cached);
                }

                let cover = fill().await.map_err(ProviderPlaybackFillError::from)?;
                self.set_l2(
                    provider_name,
                    store.as_ref(),
                    cache_key,
                    cover.as_ref(),
                    cache_ttl,
                )
                .await;
                Ok(cover)
            })
            .await
            .map_err(|error| match error {
                SingleFlightError::Inner(error) => ProviderError::from(error),
                SingleFlightError::WorkerFailed => ProviderError::Internal(format!(
                    "Provider '{provider_name}' source cover singleflight worker failed"
                )),
            })?;

        self.put_l1(&l1_key, cover.clone()).await;
        Ok(cover)
    }
}

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
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = std::result::Result<PlaybackResult, ProviderError>> + Send,
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

    let singleflight_key = format!("{provider_name}:{cache_key}");
    let versioned = PLAYBACK_FILL_SINGLEFLIGHT
        .do_work(singleflight_key, async {
            if let Some(cached) = read_fresh_versioned_playback(store.as_ref(), cache_key).await {
                return Ok(cached);
            }

            let result = fill().await.map_err(ProviderPlaybackFillError::from)?;
            cache_versioned_playback(result, provider_name, cache_key, cache_ttl, ctx)
                .await
                .map_err(ProviderPlaybackFillError::from)
        })
        .await
        .map_err(|error| match error {
            SingleFlightError::Inner(error) => ProviderError::from(error),
            SingleFlightError::WorkerFailed => ProviderError::Internal(format!(
                "Provider '{provider_name}' playback singleflight worker failed"
            )),
        })?;

    build_cached_versioned_playback_response(versioned, provider_name, ctx, mark_provider_resources)
        .await
}

async fn read_cached_source_cover(
    store: &dyn ProviderStore,
    cache_key: &str,
) -> Option<Option<SourceCover>> {
    match store.get::<Option<SourceCover>>(cache_key).await {
        Ok(Some(cached)) => Some(cached),
        _ => None,
    }
}

pub(crate) async fn cached_source_cover_or_fill<F, Fut>(
    provider_name: &'static str,
    cache_key: &str,
    cache_ttl: Duration,
    ctx: &ProviderContext<'_>,
    fill: F,
) -> std::result::Result<Option<SourceCover>, ProviderError>
where
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = std::result::Result<Option<SourceCover>, ProviderError>> + Send,
{
    SOURCE_COVER_CACHE
        .get_or_fill(provider_name, cache_key, cache_ttl, ctx, fill)
        .await
}

#[cfg(test)]
mod source_cover_cache_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn test_context(store: Arc<dyn ProviderStore>) -> ProviderContext<'static> {
        ProviderContext::new("source-cover-cache-test").with_store(store)
    }

    #[tokio::test]
    async fn source_cover_cache_l1_hit_skips_fill() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(16));
        let ctx = test_context(store);
        let calls = Arc::new(AtomicUsize::new(0));
        let key = format!("source-cover:test:l1:{}", synctv_common::snanoid!(8));

        let first_calls = calls.clone();
        let first = cached_source_cover_or_fill(
            "test",
            &key,
            Duration::from_secs(60),
            &ctx,
            || async move {
                first_calls.fetch_add(1, Ordering::SeqCst);
                Ok(Some(SourceCover::Url {
                    url: "https://cdn.example.test/a.jpg".into(),
                }))
            },
        )
        .await
        .expect("first fill should succeed");
        assert!(matches!(first, Some(SourceCover::Url { url }) if url.ends_with("/a.jpg")));

        let second_calls = calls.clone();
        let second = cached_source_cover_or_fill(
            "test",
            &key,
            Duration::from_secs(60),
            &ctx,
            || async move {
                second_calls.fetch_add(1, Ordering::SeqCst);
                Ok(Some(SourceCover::Url {
                    url: "https://cdn.example.test/b.jpg".into(),
                }))
            },
        )
        .await
        .expect("second read should succeed");

        assert!(matches!(second, Some(SourceCover::Url { url }) if url.ends_with("/a.jpg")));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn source_cover_cache_l2_hit_backfills_l1() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(16));
        let ctx = test_context(store.clone());
        let calls = Arc::new(AtomicUsize::new(0));
        let key = format!("source-cover:test:l2:{}", synctv_common::snanoid!(8));
        let cached = Some(SourceCover::Url {
            url: "https://cdn.example.test/l2.jpg".into(),
        });
        store
            .set(&key, &cached, Duration::from_secs(60))
            .await
            .expect("L2 seed should succeed");

        let first_calls = calls.clone();
        let first = cached_source_cover_or_fill(
            "test",
            &key,
            Duration::from_secs(60),
            &ctx,
            || async move {
                first_calls.fetch_add(1, Ordering::SeqCst);
                Ok(Some(SourceCover::Url {
                    url: "https://cdn.example.test/fill.jpg".into(),
                }))
            },
        )
        .await
        .expect("L2 read should succeed");
        assert!(matches!(first, Some(SourceCover::Url { url }) if url.ends_with("/l2.jpg")));

        store.delete(&key).await.expect("L2 delete should succeed");
        let second_calls = calls.clone();
        let second = cached_source_cover_or_fill(
            "test",
            &key,
            Duration::from_secs(60),
            &ctx,
            || async move {
                second_calls.fetch_add(1, Ordering::SeqCst);
                Ok(Some(SourceCover::Url {
                    url: "https://cdn.example.test/fill.jpg".into(),
                }))
            },
        )
        .await
        .expect("L1 backfill read should succeed");

        assert!(matches!(second, Some(SourceCover::Url { url }) if url.ends_with("/l2.jpg")));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn source_cover_cache_keeps_negative_results() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(16));
        let ctx = test_context(store);
        let calls = Arc::new(AtomicUsize::new(0));
        let key = format!("source-cover:test:none:{}", synctv_common::snanoid!(8));

        let first_calls = calls.clone();
        let first = cached_source_cover_or_fill(
            "test",
            &key,
            Duration::from_secs(60),
            &ctx,
            || async move {
                first_calls.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            },
        )
        .await
        .expect("negative fill should succeed");
        assert!(first.is_none());

        let second_calls = calls.clone();
        let second = cached_source_cover_or_fill(
            "test",
            &key,
            Duration::from_secs(60),
            &ctx,
            || async move {
                second_calls.fetch_add(1, Ordering::SeqCst);
                Ok(Some(SourceCover::Url {
                    url: "https://cdn.example.test/fill.jpg".into(),
                }))
            },
        )
        .await
        .expect("negative cache read should succeed");
        assert!(second.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
