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
mod p2p_media;
pub(crate) mod playback_profile;
pub(crate) mod playback_transport;
pub(crate) mod provider_client;
pub(crate) mod store;
pub(crate) mod traits;
pub(crate) mod upstream_transport;

// Shared helpers
mod live_helpers;

// MediaProvider implementations (adapters)
mod acfun;
mod alist;
mod bilibili;
mod cctv;
mod cloudreve;
mod direct_url;
mod douyin;
mod douyu;
mod emby;
mod fnos;
mod huya;
mod live_proxy;
mod nextcloud;
mod qnap;
mod rtmp;
mod seafile;
mod synology;
mod tiktok;
mod truenas;
mod twitch;
mod youtube;

use std::sync::{Arc, LazyLock};
use std::time::Duration;

pub use access::{
    AlistAccess, AlistBinding, BilibiliAccess, CachedProviderAccessService, EmbyAccess,
    ProviderAccessService, ProviderCredentialReader,
};
pub use context::ProviderContext;
pub use error::ProviderError;
pub(crate) use p2p_media::provider_p2p_swarm_id;
pub use p2p_media::{
    playback_danmaku_p2p_delivery, playback_media_p2p_delivery, playback_subtitle_p2p_delivery,
    P2pResourceDelivery,
};
pub use playback_profile::{
    PlaybackAudioCapability, PlaybackClientProfile, PlaybackContainer, PlaybackStreamPreference,
    PlaybackSubtitlePreference, PlaybackVideoCodec,
};
pub use playback_transport::{
    HlsResourceRequest, LiveFlvAccess, PlaybackTransportAction, PlaybackTransportServices,
    StatefulPlaybackResourceRequest,
};
pub use provider_client::ProviderClientManager;
pub use store::{
    InMemoryProviderStore, PrefixedProviderStore, ProviderStore, ProviderStoreExt,
    ProviderStoreRegistry, ProviderStoreResolver, RedisProviderStore, StoreError, StoreLockGuard,
    VersionedPlayback, VersionedPlaybackContext,
};
pub use synctv_common::{ExecutionControl, ExecutionControlError};
pub use traits::ProviderResourceMetadata;
pub use traits::{
    BilibiliLiveDanmakuEvent, BilibiliLiveDanmakuEventKind, BilibiliLiveDanmakuProvider,
    BilibiliLiveDanmakuStream, DynamicBrowsePathSegment, DynamicListQuery, DynamicListResult,
    DynamicPagination, DynamicPlaylistItem, DynamicPlaylistItemSourceConfig,
    DynamicPlaylistItemThumbnail, DynamicPlaylistProvider, ItemType, MediaProvider, NextPlayItem,
    PlaybackInfo, PlaybackResult, PreparedSourceConfig, ProviderCredentialDependency,
    ProviderPlaybackSessionLifecycle, SourceConfig, SourceConfigKind, SourceCover,
};

pub(crate) fn playback_profile_prefers_transcode(
    profile: Option<&PlaybackClientProfile>,
    original_format: &str,
) -> bool {
    let Some(profile) = profile else {
        return false;
    };
    match profile.stream_preference {
        PlaybackStreamPreference::Transcode => true,
        PlaybackStreamPreference::DirectPlay => false,
        PlaybackStreamPreference::Auto => {
            if profile.supported_containers.is_empty() {
                return false;
            }
            let container = match original_format.trim().to_ascii_lowercase().as_str() {
                "mp4" | "m4v" => Some(PlaybackContainer::Mp4),
                "mkv" => Some(PlaybackContainer::Mkv),
                "webm" => Some(PlaybackContainer::Webm),
                _ => None,
            };
            container.is_none_or(|container| !profile.supported_containers.contains(&container))
        }
    }
}

pub(crate) fn playback_profile_cache_token(profile: Option<&PlaybackClientProfile>) -> String {
    profile.map_or_else(
        || "default".to_string(),
        PlaybackClientProfile::cache_fingerprint,
    )
}

use crate::models::media::{
    PlaybackDanmaku, PlaybackMedia, PlaybackMediaProvider, PlaybackRtmpMedia, PlaybackSubtitle,
};
use crate::models::{normalize_provider_instance_name, MediaId, RoomId};
use sha2::{Digest, Sha256};
use std::future::Future;

use crate::cache::{SingleFlight, SingleFlightError};

pub(crate) fn apply_provider_playback_policy(
    result: &mut PlaybackResult,
    proxy_mode: crate::models::PlaybackProxyMode,
    default_proxy: bool,
) {
    let base_default = result
        .default_mode
        .strip_prefix("proxy_")
        .unwrap_or(&result.default_mode)
        .to_string();
    let proxy_default = format!("proxy_{base_default}");
    let has_proxy_default = result
        .playback_infos
        .get(&proxy_default)
        .is_some_and(|info| !info.medias.is_empty());

    match proxy_mode {
        crate::models::PlaybackProxyMode::Auto if default_proxy => {
            result.playback_infos.retain(|mode_name, info| {
                mode_name.starts_with("proxy_") && !info.medias.is_empty()
            });
            if has_proxy_default {
                result.default_mode = proxy_default;
            }
        }
        crate::models::PlaybackProxyMode::Prefer => {
            if has_proxy_default {
                result.default_mode = proxy_default;
            }
        }
        crate::models::PlaybackProxyMode::Only => {
            result.playback_infos.retain(|mode_name, info| {
                mode_name.starts_with("proxy_") && !info.medias.is_empty()
            });
            if has_proxy_default {
                result.default_mode = proxy_default;
            } else {
                let mut remaining = result.playback_infos.keys().cloned().collect::<Vec<_>>();
                remaining.sort();
                result.default_mode = remaining.into_iter().next().unwrap_or_default();
            }
        }
        crate::models::PlaybackProxyMode::Auto => {}
    }
}

pub(crate) fn playback_session_registration(
    ctx: &ProviderContext<'_>,
    resource_key: String,
    resource_version: Option<String>,
    session: crate::models::ProviderPlaybackSession,
) -> Result<
    Option<(
        crate::repository::ProviderPlaybackSessionRepository,
        crate::repository::NewProviderPlaybackSession,
    )>,
    ProviderError,
> {
    let Some(playback_generation) = ctx.playback_generation() else {
        return Ok(None);
    };
    let room_id = ctx.room_id().copied().ok_or_else(|| {
        ProviderError::Internal(
            "playback generation requires room_id in provider context".to_string(),
        )
    })?;
    let credential_owner_id = ctx
        .credential_owner_or_user_id()
        .copied()
        .ok_or(ProviderError::CredentialRequired)?;
    let db = ctx.db.ok_or_else(|| {
        ProviderError::Internal(
            "playback session registration requires database context".to_string(),
        )
    })?;
    Ok(Some((
        crate::repository::ProviderPlaybackSessionRepository::new(db.clone()),
        crate::repository::NewProviderPlaybackSession {
            room_id,
            playback_generation,
            provider_instance_name: normalize_provider_instance_name(ctx.provider_instance_name())
                .map(str::to_owned),
            credential_owner_id,
            resource_key,
            resource_version,
            session,
            paused: !ctx.playback_is_playing().unwrap_or(false),
        },
    )))
}

pub(crate) fn subtitle_headers_for_proxy(
    media_headers: &std::collections::HashMap<String, String>,
    subtitle: &crate::models::media::PlaybackSubtitle,
) -> std::collections::HashMap<String, String> {
    let mut merged = media_headers.clone();
    merged.extend(subtitle.upstream_headers());
    merged
}

// Re-export providers
pub use acfun::{AcFunDanmakuStream, AcFunLiveDanmakuEvent, AcFunProvider};
pub use alist::{
    AlistFileStreamRequest, AlistHlsResourceRequest, AlistListItem, AlistListRequest,
    AlistListResponse, AlistLoginAndPersistRequest, AlistLoginCredential, AlistLoginRequest,
    AlistMeRequest, AlistMeResponse, AlistPersistLoginCredentialRequest,
    AlistPersistedLoginResponse, AlistProvider, AlistSearchItem, AlistSearchRequest,
    AlistSearchResponse,
};
pub use bilibili::{
    BilibiliCaptchaResponse, BilibiliDashManifestMode, BilibiliDashResourceRequest,
    BilibiliFavoriteFolder, BilibiliFollowedPgcPage, BilibiliFollowedPgcSeason,
    BilibiliHistoryItem, BilibiliHistoryPage, BilibiliHlsResourceRequest, BilibiliLiveArea,
    BilibiliLiveDanmuHost, BilibiliLiveDanmuInfoRequest, BilibiliLiveDanmuInfoResponse,
    BilibiliMatchRequest, BilibiliMatchResponse, BilibiliMatchedResource, BilibiliPageInfo,
    BilibiliParseLivePageRequest, BilibiliParsePgcPageRequest, BilibiliParseVideoPageRequest,
    BilibiliPersistedQrLoginResponse, BilibiliPgcSeasonIndexItem, BilibiliPgcSeasonIndexPage,
    BilibiliPgcTimelineItem, BilibiliProvider, BilibiliQrCodeResponse, BilibiliQrLoginRequest,
    BilibiliQrLoginResponse, BilibiliQrLoginStatus, BilibiliSmsLoginRequest,
    BilibiliSmsLoginResponse, BilibiliSmsLoginTokenCodec, BilibiliSmsRequest, BilibiliSmsResponse,
    BilibiliUserInfoRequest, BilibiliUserInfoResponse, BilibiliVideoInfo,
    DASH_MANIFEST_METADATA_KEY, LIVE_DANMAKU_FORMAT, LIVE_DANMAKU_TRACK_NAME,
};
pub use cctv::CctvProvider;
pub use cloudreve::{
    CloudreveBind, CloudreveHlsResourceRequest, CloudreveListResponse, CloudreveProvider,
};
pub use direct_url::{
    DirectUrlDashResourceRequest, DirectUrlHlsResourceRequest, DirectUrlProvider,
};
pub use douyin::{DouyinBind, DouyinDanmakuEvent, DouyinDanmakuStream, DouyinProvider};
pub use douyu::{DouyuDanmakuEvent, DouyuDanmakuStream, DouyuProvider};
pub use emby::{
    EmbyHlsResourceRequest, EmbyListItem, EmbyListRequest, EmbyListResponse,
    EmbyLoginAndPersistRequest, EmbyLoginCredential, EmbyLoginRequest, EmbyLoginResponse,
    EmbyMeRequest, EmbyMeResponse, EmbyPersistedLoginResponse, EmbyProvider, EmbyUserPolicy,
};
pub use fnos::{FnosBind, FnosLoginResult, FnosProvider};
pub use huya::{HuyaDanmakuEvent, HuyaDanmakuStream, HuyaProvider};
pub use live_proxy::LiveProxyProvider;
pub use nextcloud::{
    NextcloudBind, NextcloudHlsResourceRequest, NextcloudListResponse, NextcloudProvider,
};
pub use qnap::{
    QnapBind, QnapCapabilities, QnapHlsResourceRequest, QnapListItem, QnapListResponse,
    QnapProvider,
};
pub use rtmp::RtmpProvider;
pub use seafile::{
    SeafileBind, SeafileHlsResourceRequest, SeafileListRequest, SeafileListResponse,
    SeafileProvider,
};
pub use synology::{
    SynologyBind, SynologyProvider, SynologyVideoEntry, SynologyVideoEntryKind, SynologyVideoPage,
};
pub use tiktok::{TikTokBind, TikTokProvider};
pub use truenas::{TrueNasBind, TrueNasHlsResourceRequest, TrueNasListResponse, TrueNasProvider};
pub use twitch::{TwitchBind, TwitchChatEvent, TwitchChatStream, TwitchProvider};
pub use youtube::{YoutubeBind, YoutubeProvider};

fn playback_info_is_hls(mode_name: &str, info: &PlaybackInfo) -> bool {
    info.medias
        .iter()
        .any(|media| playback_media_is_hls(mode_name, media))
}

fn playback_media_is_hls(mode_name: &str, media: &PlaybackMedia) -> bool {
    let format = media.format.trim().to_ascii_lowercase();
    format == "m3u8" || format == "hls" || mode_name.contains("hls")
}

fn playback_media_is_dash(mode_name: &str, media: &PlaybackMedia) -> bool {
    let format = media.format.trim().to_ascii_lowercase();
    format == "mpd" || format == "dash" || mode_name.contains("dash")
}

/// Bundle of all in-process provider adapters.
///
/// Playback-provider transports call these concrete provider adapters through
/// provider-specific impl modules.
#[derive(Clone)]
pub struct ProviderSet {
    pub acfun: std::sync::Arc<AcFunProvider>,
    pub cctv: std::sync::Arc<CctvProvider>,
    pub fnos: std::sync::Arc<FnosProvider>,
    pub qnap: std::sync::Arc<QnapProvider>,
    pub synology: std::sync::Arc<SynologyProvider>,
    pub nextcloud: std::sync::Arc<NextcloudProvider>,
    pub seafile: std::sync::Arc<SeafileProvider>,
    pub truenas: std::sync::Arc<TrueNasProvider>,
    pub alist: std::sync::Arc<AlistProvider>,
    pub bilibili: std::sync::Arc<BilibiliProvider>,
    pub emby: std::sync::Arc<EmbyProvider>,
    pub direct_url: std::sync::Arc<DirectUrlProvider>,
    pub rtmp: std::sync::Arc<RtmpProvider>,
    pub live_proxy: std::sync::Arc<LiveProxyProvider>,
    pub cloudreve: std::sync::Arc<CloudreveProvider>,
    pub twitch: std::sync::Arc<TwitchProvider>,
    pub youtube: std::sync::Arc<YoutubeProvider>,
    pub huya: std::sync::Arc<HuyaProvider>,
    pub douyu: std::sync::Arc<DouyuProvider>,
    pub douyin: std::sync::Arc<DouyinProvider>,
    pub tiktok: std::sync::Arc<TikTokProvider>,
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
            ProviderClientManager::new_with_provider_http_client(provider_http_client.clone()),
        );
        Self {
            acfun: std::sync::Arc::new(AcFunProvider::with_http_client(
                provider_http_client.clone(),
            )),
            cctv: std::sync::Arc::new(CctvProvider::with_http_client(provider_http_client.clone())),
            fnos: std::sync::Arc::new(FnosProvider::new(ssrf_guard.clone())),
            qnap: std::sync::Arc::new(QnapProvider::with_http_client(provider_http_client.clone())),
            synology: std::sync::Arc::new(SynologyProvider::with_http_client(
                provider_http_client.clone(),
            )),
            nextcloud: std::sync::Arc::new(NextcloudProvider::with_http_client(
                provider_http_client.clone(),
            )),
            seafile: std::sync::Arc::new(SeafileProvider::with_http_client(
                provider_http_client.clone(),
            )),
            truenas: std::sync::Arc::new(TrueNasProvider::with_http_client(
                provider_http_client.clone(),
            )),
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
            cloudreve: std::sync::Arc::new(CloudreveProvider::with_http_client(
                provider_http_client.clone(),
            )),
            huya: std::sync::Arc::new(HuyaProvider::with_http_client(provider_http_client.clone())),
            douyu: std::sync::Arc::new(DouyuProvider::with_http_client(
                provider_http_client.clone(),
            )),
            douyin: std::sync::Arc::new(DouyinProvider::with_http_client(
                provider_http_client.clone(),
            )),
            twitch: std::sync::Arc::new(TwitchProvider::with_http_client(
                provider_http_client.clone(),
            )),
            youtube: std::sync::Arc::new(YoutubeProvider::with_http_client(
                provider_http_client.clone(),
            )),
            tiktok: std::sync::Arc::new(TikTokProvider::with_http_client(provider_http_client)),
        }
    }

    #[must_use]
    pub fn with_credential_repo(
        &self,
        credential_repo: Arc<crate::repository::UserProviderCredentialRepository>,
    ) -> Self {
        Self {
            acfun: self.acfun.clone(),
            cctv: self.cctv.clone(),
            fnos: Arc::new(self.fnos.with_credential_repo(credential_repo.clone())),
            qnap: Arc::new(self.qnap.with_credential_repo(credential_repo.clone())),
            synology: Arc::new(self.synology.with_credential_repo(credential_repo.clone())),
            nextcloud: Arc::new(self.nextcloud.with_credential_repo(credential_repo.clone())),
            seafile: Arc::new(self.seafile.with_credential_repo(credential_repo.clone())),
            truenas: Arc::new(self.truenas.with_credential_repo(credential_repo.clone())),
            alist: Arc::new(self.alist.with_credential_repo(credential_repo.clone())),
            bilibili: Arc::new(self.bilibili.with_credential_repo(credential_repo.clone())),
            emby: Arc::new(self.emby.with_credential_repo(credential_repo.clone())),
            direct_url: self.direct_url.clone(),
            rtmp: self.rtmp.clone(),
            live_proxy: self.live_proxy.clone(),
            cloudreve: Arc::new(self.cloudreve.with_credential_repo(credential_repo.clone())),
            twitch: Arc::new(self.twitch.with_credential_repo(credential_repo.clone())),
            youtube: Arc::new(self.youtube.with_credential_repo(credential_repo.clone())),
            huya: self.huya.clone(),
            douyu: self.douyu.clone(),
            douyin: Arc::new(self.douyin.with_credential_repo(credential_repo.clone())),
            tiktok: Arc::new(self.tiktok.with_credential_repo(credential_repo)),
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
static PROVIDER_METADATA_CACHE: LazyLock<ProviderMetadataCache> =
    LazyLock::new(ProviderMetadataCache::new);
static SOURCE_COVER_CACHE: LazyLock<ProviderSourceCoverCache> =
    LazyLock::new(ProviderSourceCoverCache::new);

#[must_use]
pub fn provider_requires_credential_repo(provider_name: &str) -> bool {
    matches!(
        provider_name,
        AlistProvider::NAME
            | EmbyProvider::NAME
            | CloudreveProvider::NAME
            | FnosProvider::NAME
            | QnapProvider::NAME
            | TwitchProvider::NAME
            | YoutubeProvider::NAME
    )
}

#[must_use]
pub fn build_live_playback(media_id: MediaId, room_id: RoomId) -> PlaybackResult {
    build_live_playback_with_flv(media_id, room_id, true)
}

fn build_live_playback_with_flv(
    media_id: MediaId,
    room_id: RoomId,
    include_flv: bool,
) -> PlaybackResult {
    use std::collections::HashMap;

    let mut playback_infos = HashMap::new();

    playback_infos.insert(
        "hls".to_string(),
        PlaybackInfo {
            thumbnail: None,
            medias: vec![PlaybackMedia {
                name: "HLS".to_string(),
                format: "m3u8".to_string(),
                expire_at: None,
                metadata: None,
                p2p_swarm_id: None,
                provider: PlaybackMediaProvider::Rtmp(PlaybackRtmpMedia::HlsMaster {
                    version: String::new(),
                    expires_at: 0,
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

    if include_flv {
        playback_infos.insert(
            "flv".to_string(),
            PlaybackInfo {
                thumbnail: None,
                medias: vec![PlaybackMedia {
                    name: "FLV".to_string(),
                    format: "flv".to_string(),
                    expire_at: None,
                    metadata: None,
                    p2p_swarm_id: None,
                    provider: PlaybackMediaProvider::Rtmp(PlaybackRtmpMedia::FlvStream {
                        version: String::new(),
                        expires_at: 0,
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
    }

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
        playback_kind: Some(crate::models::PlaybackKind::Live),
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

/// Parse expiration parameters whose timestamp semantics are defined by common
/// signed-URL protocols. Unknown token formats intentionally remain unset.
pub(crate) fn url_expiration_timestamp(value: &str) -> Option<i64> {
    let url = url::Url::parse(value).ok()?;
    let query = url
        .query_pairs()
        .map(|(key, value)| (key.to_ascii_lowercase(), value.into_owned()))
        .collect::<std::collections::HashMap<_, _>>();
    let mut candidates = Vec::new();

    // V2-style signatures use an absolute `Expires` value. Requiring both the
    // signature and a protocol identity avoids treating application-specific
    // `expires` query parameters as Unix timestamps.
    if query.contains_key("signature")
        && [
            "awsaccesskeyid",
            "googleaccessid",
            "ossaccesskeyid",
            "key-pair-id",
        ]
        .iter()
        .any(|key| query.contains_key(*key))
    {
        if let Some(expires_at) = query
            .get("expires")
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|expires_at| *expires_at > 0)
        {
            candidates.push(expires_at);
        }
    }

    // Azure SAS pairs the RFC3339 `se` timestamp with `sig`.
    if query.contains_key("sig") {
        if let Some(expires_at) = query
            .get("se")
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|expires_at| expires_at.timestamp())
            .filter(|expires_at| *expires_at > 0)
        {
            candidates.push(expires_at);
        }
    }

    // V4 signatures express expiry as signing time plus a relative TTL.
    for (date_key, ttl_key, credential_key, signature_key) in [
        (
            "x-amz-date",
            "x-amz-expires",
            "x-amz-credential",
            "x-amz-signature",
        ),
        (
            "x-goog-date",
            "x-goog-expires",
            "x-goog-credential",
            "x-goog-signature",
        ),
        (
            "x-oss-date",
            "x-oss-expires",
            "x-oss-credential",
            "x-oss-signature",
        ),
    ] {
        if !query.contains_key(credential_key) || !query.contains_key(signature_key) {
            continue;
        }
        let Some(date) = query
            .get(date_key)
            .and_then(|value| chrono::NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%SZ").ok())
        else {
            continue;
        };
        let Some(ttl) = query
            .get(ttl_key)
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|ttl| *ttl >= 0)
        else {
            continue;
        };
        if let Some(expires_at) = date.and_utc().timestamp().checked_add(ttl) {
            candidates.push(expires_at);
        }
    }

    candidates
        .into_iter()
        .filter(|timestamp| chrono::DateTime::from_timestamp(*timestamp, 0).is_some())
        .min()
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
    let expires_at = playback_transport_expires_at(cache_ttl, provider_name)?;
    let versioned = VersionedPlayback {
        version: synctv_common::snanoid!(16),
        result: result.clone(),
        expires_at,
        playback_context: match (ctx.room_id().copied(), ctx.playback_generation()) {
            (Some(room_id), Some(playback_generation)) => Some(VersionedPlaybackContext {
                room_id,
                playback_generation,
                is_playing: ctx.playback_is_playing().unwrap_or(false),
            }),
            _ => None,
        },
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

fn playback_transport_expires_at(
    cache_ttl: Duration,
    provider_name: &str,
) -> Result<i64, ProviderError> {
    let transport_ttl = cache_ttl.max(DEFAULT_PLAYBACK_TRANSPORT_TTL);
    let ttl_secs = i64::try_from(transport_ttl.as_secs()).map_err(|_| {
        ProviderError::Internal(format!(
            "Provider '{provider_name}' playback transport TTL exceeds i64::MAX seconds"
        ))
    })?;
    let now = crate::SystemClock.now().timestamp();
    let transport_expires_at = now.checked_add(ttl_secs).ok_or_else(|| {
        ProviderError::Internal(format!(
            "Provider '{provider_name}' playback transport expiry exceeds i64::MAX"
        ))
    })?;
    Ok(transport_expires_at)
}

const PLAYBACK_CACHE_LOCK_TTL: Duration = Duration::from_secs(30);
const DEFAULT_PLAYBACK_TRANSPORT_TTL: Duration = Duration::from_hours(1);
const PLAYBACK_TRANSPORT_REFRESH_MARGIN_SECONDS: i64 = 60;
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
        Ok(Some(cached)) if versioned_playback_is_fresh(&cached) => Some(cached),
        _ => None,
    }
}

fn versioned_playback_is_fresh(versioned: &VersionedPlayback) -> bool {
    crate::SystemClock
        .now()
        .timestamp()
        .saturating_add(PLAYBACK_TRANSPORT_REFRESH_MARGIN_SECONDS)
        < playback_refresh_deadline(versioned)
}

fn playback_refresh_deadline(versioned: &VersionedPlayback) -> i64 {
    versioned
        .result
        .playback_infos
        .values()
        .flat_map(|info| {
            info.medias
                .iter()
                .filter_map(|media| media.expire_at.map(|value| value.timestamp()))
                .chain(
                    info.subtitles
                        .iter()
                        .filter_map(PlaybackSubtitle::expiration_timestamp),
                )
                .chain(
                    info.danmakus
                        .iter()
                        .filter_map(PlaybackDanmaku::expiration_timestamp),
                )
        })
        .min()
        .map_or(versioned.expires_at, |upstream_expires_at| {
            upstream_expires_at.min(versioned.expires_at)
        })
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

pub(crate) async fn cached_versioned_playback_or_fill<F, Fut, M>(
    provider_name: &'static str,
    cache_key: &str,
    cache_ttl: Duration,
    ctx: &ProviderContext<'_>,
    mark_provider_resources: M,
    fill: F,
) -> std::result::Result<PlaybackResult, ProviderError>
where
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = std::result::Result<PlaybackResult, ProviderError>> + Send,
    M: Fn(&mut PlaybackResult, &str, i64) + Copy,
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

fn provider_metadata_cache_key(
    provider_name: &str,
    resource_key: &str,
    ctx: &ProviderContext<'_>,
) -> String {
    let user_id = ctx.user_id.map(|id| id.to_string()).unwrap_or_default();
    let credential_owner_id = ctx
        .credential_owner_id
        .map(|id| id.to_string())
        .unwrap_or_default();
    let room_id = ctx.room_id.map(|id| id.to_string()).unwrap_or_default();
    let provider_instance_name = ctx.provider_instance_name.unwrap_or_default();
    let mut hasher = Sha256::new();
    for component in [
        provider_name,
        user_id.as_str(),
        credential_owner_id.as_str(),
        room_id.as_str(),
        provider_instance_name,
        resource_key,
    ] {
        hasher.update(component.len().to_le_bytes());
        hasher.update(component.as_bytes());
    }
    format!("metadata:{}", hex::encode(hasher.finalize()))
}

struct ProviderMetadataCache {
    l1: moka::future::Cache<String, Option<ProviderResourceMetadata>>,
    singleflight: SingleFlight<String, Option<ProviderResourceMetadata>, ProviderPlaybackFillError>,
}

impl ProviderMetadataCache {
    fn new() -> Self {
        Self {
            l1: moka::future::Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(10))
                .build(),
            singleflight: SingleFlight::new(),
        }
    }
}

/// Cache provider-owned resource metadata independently from signed playback
/// URLs. Live providers use a short TTL so an offline/online transition is
/// reflected quickly while repeated library reads avoid duplicate upstream
/// requests.
pub(crate) async fn cached_provider_metadata_or_fill<F, Fut>(
    provider_name: &'static str,
    cache_key: &str,
    cache_ttl: Duration,
    ctx: &ProviderContext<'_>,
    fill: F,
) -> std::result::Result<Option<ProviderResourceMetadata>, ProviderError>
where
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = std::result::Result<Option<ProviderResourceMetadata>, ProviderError>>
        + Send,
{
    let key = provider_metadata_cache_key(provider_name, cache_key, ctx);
    if let Some(cached) = PROVIDER_METADATA_CACHE.l1.get(&key).await {
        return Ok(cached);
    }
    let Some(store) = ctx.store.as_ref() else {
        let fill = Box::pin(fill());
        let metadata = PROVIDER_METADATA_CACHE
            .singleflight
            .do_work(key.clone(), async {
                fill.await.map_err(ProviderPlaybackFillError::from)
            })
            .await
            .map_err(|error| match error {
                SingleFlightError::Inner(error) => ProviderError::from(error),
                SingleFlightError::WorkerFailed => ProviderError::Internal(format!(
                    "Provider '{provider_name}' metadata singleflight worker failed"
                )),
            })?;
        PROVIDER_METADATA_CACHE
            .l1
            .insert(key, metadata.clone())
            .await;
        return Ok(metadata);
    };
    if let Ok(Some(cached)) = store.get::<Option<ProviderResourceMetadata>>(&key).await {
        PROVIDER_METADATA_CACHE.l1.insert(key, cached.clone()).await;
        return Ok(cached);
    }
    let lock_key = format!("lock:{key}");
    let lock = store.lock(&lock_key, Duration::from_secs(5)).await.ok();
    if lock.is_none() {
        for _ in 0..4 {
            tokio::time::sleep(Duration::from_millis(40)).await;
            if let Ok(Some(cached)) = store.get::<Option<ProviderResourceMetadata>>(&key).await {
                PROVIDER_METADATA_CACHE.l1.insert(key, cached.clone()).await;
                return Ok(cached);
            }
        }
    } else if let Ok(Some(cached)) = store.get::<Option<ProviderResourceMetadata>>(&key).await {
        PROVIDER_METADATA_CACHE.l1.insert(key, cached.clone()).await;
        return Ok(cached);
    }
    let fill = Box::pin(fill());
    let metadata = PROVIDER_METADATA_CACHE
        .singleflight
        .do_work(key.clone(), async {
            if let Ok(Some(cached)) = store.get::<Option<ProviderResourceMetadata>>(&key).await {
                return Ok(cached);
            }
            let metadata = fill.await.map_err(ProviderPlaybackFillError::from)?;
            if let Err(error) = store.set(&key, &metadata, cache_ttl).await {
                tracing::debug!(
                    provider = provider_name,
                    cache_key = key,
                    error = %error,
                    "Provider metadata cache write failed"
                );
            }
            Ok(metadata)
        })
        .await
        .map_err(|error| match error {
            SingleFlightError::Inner(error) => ProviderError::from(error),
            SingleFlightError::WorkerFailed => ProviderError::Internal(format!(
                "Provider '{provider_name}' metadata singleflight worker failed"
            )),
        })?;
    PROVIDER_METADATA_CACHE
        .l1
        .insert(key, metadata.clone())
        .await;
    Ok(metadata)
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
mod playback_policy_tests {
    use super::*;
    use crate::models::{
        PlaybackDirectUrlMedia, PlaybackMedia, PlaybackMediaProvider, PlaybackProxyMode,
    };
    use std::collections::HashMap;

    fn playback_result() -> PlaybackResult {
        let info = PlaybackInfo {
            thumbnail: None,
            medias: vec![PlaybackMedia {
                name: "Movie".to_string(),
                format: "mp4".to_string(),
                expire_at: None,
                metadata: None,
                p2p_swarm_id: None,
                provider: PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct {
                    url: "https://media.example/movie.mp4".to_string(),
                    headers: HashMap::new(),
                }),
            }],
            default_media_index: Some(0),
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            default_danmaku_index: None,
        };
        PlaybackResult {
            playback_infos: HashMap::from([
                ("direct".to_string(), info.clone()),
                ("proxy_direct".to_string(), info),
            ]),
            default_mode: "direct".to_string(),
            provider: "test".to_string(),
            provider_instance_name: None,
            duration_seconds: None,
            playback_kind: None,
            metadata: None,
        }
    }

    #[test]
    fn auto_uses_only_proxy_routes_for_private_providers() {
        let mut result = playback_result();

        apply_provider_playback_policy(&mut result, PlaybackProxyMode::Auto, true);

        assert_eq!(result.default_mode, "proxy_direct");
        assert_eq!(
            result.playback_infos.keys().cloned().collect::<Vec<_>>(),
            vec!["proxy_direct"]
        );
    }

    #[test]
    fn prefer_keeps_both_routes_and_selects_proxy() {
        let mut result = playback_result();

        apply_provider_playback_policy(&mut result, PlaybackProxyMode::Prefer, true);

        assert_eq!(result.default_mode, "proxy_direct");
        assert!(result.playback_infos.contains_key("direct"));
        assert!(result.playback_infos.contains_key("proxy_direct"));
    }

    #[test]
    fn only_filters_direct_routes() {
        let mut result = playback_result();

        apply_provider_playback_policy(&mut result, PlaybackProxyMode::Only, true);

        assert_eq!(result.default_mode, "proxy_direct");
        assert_eq!(
            result.playback_infos.keys().cloned().collect::<Vec<_>>(),
            vec!["proxy_direct"]
        );
    }

    #[test]
    fn auto_keeps_direct_route_for_public_url_sources() {
        let mut result = playback_result();

        apply_provider_playback_policy(&mut result, PlaybackProxyMode::Auto, false);

        assert_eq!(result.default_mode, "direct");
        assert_eq!(result.playback_infos.len(), 2);
    }
}

#[cfg(test)]
mod playback_transport_expiry_tests {
    use super::*;

    fn live_playback() -> PlaybackResult {
        build_live_playback(MediaId::new(), RoomId::new())
    }

    #[test]
    fn signed_url_expiration_parser_supports_standard_protocols() {
        assert_eq!(
            url_expiration_timestamp(
                "https://s3.example/object?AWSAccessKeyId=key&Signature=private&Expires=1785945600"
            ),
            Some(1_785_945_600),
        );
        assert_eq!(
            url_expiration_timestamp(
                "https://cdn.example/object?Key-Pair-Id=key&Signature=private&Expires=1785945601"
            ),
            Some(1_785_945_601),
        );
        assert_eq!(
            url_expiration_timestamp(
                "https://s3.example/object?X-Amz-Date=20260805T120000Z&X-Amz-Expires=600&X-Amz-Credential=key&X-Amz-Signature=private"
            ),
            Some(1_785_931_800)
        );
        assert_eq!(
            url_expiration_timestamp(
                "https://storage.example/object?X-Goog-Date=20260805T120000Z&X-Goog-Expires=600&X-Goog-Credential=key&X-Goog-Signature=private"
            ),
            Some(1_785_931_800)
        );
        assert_eq!(
            url_expiration_timestamp(
                "https://oss.example/object?x-oss-date=20260805T120000Z&x-oss-expires=600&x-oss-credential=key&x-oss-signature=private"
            ),
            Some(1_785_931_800)
        );
        assert_eq!(
            url_expiration_timestamp(
                "https://blob.example/object?se=2026-08-05T12%3A10%3A00Z&sig=private"
            ),
            Some(1_785_931_800)
        );
        assert_eq!(
            url_expiration_timestamp("https://cdn.example/live.m3u8?expires=3600"),
            None
        );
        assert_eq!(
            url_expiration_timestamp("https://cdn.example/video.m4s?deadline=1785945601"),
            None
        );
        assert_eq!(
            url_expiration_timestamp("https://cdn.example/video.m3u8?token=private"),
            None
        );
        assert_eq!(
            url_expiration_timestamp(
                "https://s3.example/object?X-Amz-Date=invalid&X-Amz-Expires=600&X-Amz-Credential=key&X-Amz-Signature=private"
            ),
            None
        );
        assert_eq!(
            url_expiration_timestamp(
                "https://s3.example/object?AWSAccessKeyId=key&Signature=private&Expires=1785945600&X-Amz-Date=20260805T120000Z&X-Amz-Expires=600&X-Amz-Credential=key&X-Amz-Signature=private"
            ),
            Some(1_785_931_800)
        );
    }

    #[test]
    fn short_provider_cache_does_not_shorten_transport_lifetime() {
        let before = crate::SystemClock.now().timestamp() + 3600;
        let expires_at = playback_transport_expires_at(Duration::from_mins(2), "test-provider")
            .expect("transport expiry should be calculated");
        let after = crate::SystemClock.now().timestamp() + 3600;

        assert!((before..=after).contains(&expires_at));
    }

    #[test]
    fn upstream_expiry_only_advances_snapshot_refresh() {
        let mut result = live_playback();
        let upstream_expires_at = crate::SystemClock.now().timestamp() + 600;
        result
            .playback_infos
            .values_mut()
            .flat_map(|info| &mut info.medias)
            .for_each(|media| {
                media.expire_at = chrono::DateTime::from_timestamp(upstream_expires_at, 0);
            });

        let hard_expires_at =
            playback_transport_expires_at(Duration::from_mins(2), "test-provider")
                .expect("transport expiry should be calculated");
        let mut versioned = VersionedPlayback {
            version: "test".to_string(),
            result,
            expires_at: hard_expires_at,
            playback_context: None,
        };

        assert_eq!(playback_refresh_deadline(&versioned), upstream_expires_at);
        assert!(versioned.expires_at > upstream_expires_at);
        assert!(versioned_playback_is_fresh(&versioned));

        versioned
            .result
            .playback_infos
            .values_mut()
            .flat_map(|info| &mut info.medias)
            .for_each(|media| {
                media.expire_at = chrono::DateTime::from_timestamp(
                    crate::SystemClock.now().timestamp()
                        + PLAYBACK_TRANSPORT_REFRESH_MARGIN_SECONDS,
                    0,
                );
            });
        assert!(!versioned_playback_is_fresh(&versioned));
    }

    #[test]
    fn auxiliary_expiry_advances_snapshot_refresh() {
        let mut result = live_playback();
        let subtitle_expires_at = crate::SystemClock.now().timestamp() + 300;
        result
            .playback_infos
            .values_mut()
            .next()
            .expect("live playback should have a mode")
            .subtitles
            .push(crate::models::PlaybackSubtitle {
                name: "English".to_string(),
                language: "en".to_string(),
                format: "vtt".to_string(),
                p2p_swarm_id: None,
                provider: crate::models::PlaybackSubtitleProvider::DirectUrl(
                    crate::models::PlaybackDirectUrlSubtitle::Direct {
                        url: "https://cdn.example/subtitle.vtt".to_string(),
                        headers: Default::default(),
                        expire_at: chrono::DateTime::from_timestamp(subtitle_expires_at, 0),
                    },
                ),
            });
        let versioned = VersionedPlayback {
            version: "test".to_string(),
            result,
            expires_at: crate::SystemClock.now().timestamp() + 3600,
            playback_context: None,
        };

        assert_eq!(playback_refresh_deadline(&versioned), subtitle_expires_at);
    }

    #[test]
    fn long_provider_cache_remains_the_transport_lifetime() {
        let before = crate::SystemClock.now().timestamp() + 7200;
        let expires_at = playback_transport_expires_at(Duration::from_hours(2), "test-provider")
            .expect("transport expiry should be calculated");
        let after = crate::SystemClock.now().timestamp() + 7200;

        assert!((before..=after).contains(&expires_at));
    }
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

#[cfg(test)]
mod provider_metadata_cache_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn metadata(kind: crate::models::BilibiliPlaybackKind) -> Option<ProviderResourceMetadata> {
        Some(crate::models::PlaybackMetadata::Bilibili(
            crate::models::BilibiliPlaybackMetadata::new(kind),
        ))
    }

    #[tokio::test]
    async fn provider_metadata_cache_keeps_values_and_negative_results() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(16));
        let ctx = ProviderContext::new("metadata-cache-test")
            .with_user_id(crate::models::UserId::expect_positive(1))
            .with_store(store);
        let calls = Arc::new(AtomicUsize::new(0));

        for (resource_key, value) in [
            ("value", metadata(crate::models::BilibiliPlaybackKind::Live)),
            ("none", None),
        ] {
            let first_calls = calls.clone();
            let first_value = value.clone();
            let first = cached_provider_metadata_or_fill(
                "bilibili",
                resource_key,
                Duration::from_secs(60),
                &ctx,
                || async move {
                    first_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(first_value)
                },
            )
            .await
            .expect("first metadata fill should succeed");
            assert_eq!(first.is_some(), value.is_some());

            let second_calls = calls.clone();
            let second = cached_provider_metadata_or_fill(
                "bilibili",
                resource_key,
                Duration::from_secs(60),
                &ctx,
                || async move {
                    second_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(metadata(crate::models::BilibiliPlaybackKind::Video))
                },
            )
            .await
            .expect("cached metadata read should succeed");
            assert_eq!(second.is_some(), value.is_some());
        }

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn provider_metadata_cache_isolates_ambiguous_instance_and_resource_keys() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(16));
        let first_ctx = ProviderContext::new("metadata-cache-test")
            .with_provider_instance_name("instance:resource")
            .with_store(store.clone());
        let second_ctx = ProviderContext::new("metadata-cache-test")
            .with_provider_instance_name("instance")
            .with_store(store);

        let first = cached_provider_metadata_or_fill(
            "bilibili",
            "key",
            Duration::from_secs(60),
            &first_ctx,
            || async { Ok(metadata(crate::models::BilibiliPlaybackKind::Video)) },
        )
        .await
        .expect("first metadata fill should succeed");
        let second = cached_provider_metadata_or_fill(
            "bilibili",
            "resource:key",
            Duration::from_secs(60),
            &second_ctx,
            || async { Ok(metadata(crate::models::BilibiliPlaybackKind::Pgc)) },
        )
        .await
        .expect("second metadata fill should succeed");

        let kind = |value: Option<ProviderResourceMetadata>| match value {
            Some(crate::models::PlaybackMetadata::Bilibili(metadata)) => Some(metadata.kind),
            _ => None,
        };
        assert_eq!(
            kind(first),
            Some(crate::models::BilibiliPlaybackKind::Video)
        );
        assert_eq!(kind(second), Some(crate::models::BilibiliPlaybackKind::Pgc));
        assert_ne!(
            provider_metadata_cache_key("bilibili", "key", &first_ctx),
            provider_metadata_cache_key("bilibili", "resource:key", &second_ctx),
        );
    }

    #[tokio::test]
    async fn provider_metadata_cache_deduplicates_slow_concurrent_fills() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(16));
        let ctx = ProviderContext::new("metadata-cache-test").with_store(store);
        let calls = Arc::new(AtomicUsize::new(0));

        let resolve = || {
            let calls = calls.clone();
            cached_provider_metadata_or_fill(
                "bilibili",
                "slow-concurrent-fill",
                Duration::from_secs(60),
                &ctx,
                || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    Ok(metadata(crate::models::BilibiliPlaybackKind::Live))
                },
            )
        };
        let (first, second) = tokio::join!(resolve(), resolve());

        assert!(first.expect("first fill should succeed").is_some());
        assert!(second.expect("second fill should succeed").is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn provider_metadata_cache_deduplicates_without_store() {
        let ctx = ProviderContext::new("metadata-cache-test");
        let calls = Arc::new(AtomicUsize::new(0));

        let resolve = || {
            let calls = calls.clone();
            cached_provider_metadata_or_fill(
                "bilibili",
                "slow-concurrent-fill-without-store",
                Duration::from_secs(60),
                &ctx,
                || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    Ok(metadata(crate::models::BilibiliPlaybackKind::Live))
                },
            )
        };
        let (first, second) = tokio::join!(resolve(), resolve());

        assert!(first.expect("first fill should succeed").is_some());
        assert!(second.expect("second fill should succeed").is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
