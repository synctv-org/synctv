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
pub use context::{ProviderActor, ProviderContext, ProviderCredentialPolicy};
pub use error::ProviderError;
pub(crate) use p2p_media::provider_p2p_swarm_id;
pub use p2p_media::{
    playback_danmaku_p2p_delivery, playback_media_p2p_delivery, playback_subtitle_p2p_delivery,
    P2pResourceDelivery,
};
pub use playback_profile::{
    PlaybackAudioCapability, PlaybackAudioCodec, PlaybackClientEnvironment, PlaybackClientProfile,
    PlaybackContainer, PlaybackLiveTransport, PlaybackMediaCapability, PlaybackMediaPipeline,
    PlaybackMediaTransport, PlaybackStreamPreference, PlaybackSubtitlePreference,
    PlaybackVideoCodec, CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
};
pub use playback_transport::{
    HlsResourceRequest, LiveFlvAccess, PlaybackResourceProxyStrategy, PlaybackTransportAction,
    PlaybackTransportServices, StatefulPlaybackResourceRequest,
};
pub use provider_client::ProviderClientManager;
pub use store::{
    InMemoryProviderStore, PrefixedProviderStore, ProviderStore, ProviderStoreExt,
    ProviderStoreRegistry, ProviderStoreResolver, RedisProviderStore, StoreError, StoreLockGuard,
    VersionedPlayback, VersionedPlaybackContext,
};
pub use synctv_common::{ExecutionControl, ExecutionControlError};
pub use traits::{
    BilibiliLiveDanmakuEvent, BilibiliLiveDanmakuEventKind, BilibiliLiveDanmakuProvider,
    BilibiliLiveDanmakuStream, CredentialRequirement, DynamicBrowsePathSegment, DynamicListQuery,
    DynamicListResult, DynamicPagination, DynamicPlaylistItem, DynamicPlaylistItemSourceConfig,
    DynamicPlaylistItemThumbnail, DynamicPlaylistProvider, ItemType, MediaProvider, NextPlayItem,
    PlaybackInfo, PlaybackResult, PreparedSourceConfig, ProviderCredentialDependency,
    ProviderPlaybackSessionLifecycle, SourceConfig, SourceConfigKind, SourceCover,
};
pub use traits::{
    PlaybackProxyAutoPolicy, PlaybackProxyAutoReason, PlaybackProxyPolicy, ProviderResourceMetadata,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaybackMediaRequirements {
    pub transport: PlaybackMediaTransport,
    pub container: Option<PlaybackContainer>,
    pub video_codec: Option<PlaybackVideoCodec>,
    pub audio_codec: Option<PlaybackAudioCodec>,
}

fn playback_video_codec(value: &str) -> Option<PlaybackVideoCodec> {
    let value = value.trim().to_ascii_lowercase();
    if value.starts_with("avc") || value.starts_with("h264") {
        Some(PlaybackVideoCodec::H264)
    } else if value.starts_with("hev")
        || value.starts_with("hvc")
        || value.starts_with("hevc")
        || value.starts_with("h265")
    {
        Some(PlaybackVideoCodec::Hevc)
    } else if value.starts_with("vp9") || value.starts_with("vp09") {
        Some(PlaybackVideoCodec::Vp9)
    } else if value.starts_with("av1") || value.starts_with("av01") {
        Some(PlaybackVideoCodec::Av1)
    } else {
        None
    }
}

pub(crate) fn playback_media_requirements(
    mode_name: &str,
    media: &PlaybackMedia,
) -> Option<PlaybackMediaRequirements> {
    let format = media.format.trim().to_ascii_lowercase();
    let mode_name = mode_name.trim().to_ascii_lowercase();
    let video_codec = media
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.codec.as_deref())
        .and_then(playback_video_codec)
        .or_else(|| playback_video_codec(&mode_name));
    let (transport, container, default_video_codec, audio_codec) = match format.as_str() {
        "mpd" | "dash" => (
            PlaybackMediaTransport::Dash,
            Some(PlaybackContainer::Mp4),
            None,
            Some(PlaybackAudioCodec::Aac),
        ),
        "m3u8" | "hls" => (
            PlaybackMediaTransport::Hls,
            None,
            (mode_name.contains("durl") || mode_name == "mp4").then_some(PlaybackVideoCodec::H264),
            (mode_name.contains("durl") || mode_name == "mp4").then_some(PlaybackAudioCodec::Aac),
        ),
        "flv" => (
            PlaybackMediaTransport::Flv,
            None,
            None,
            Some(PlaybackAudioCodec::Aac),
        ),
        "ts" | "mpegts" | "mpeg-ts" => (
            PlaybackMediaTransport::MpegTs,
            None,
            None,
            Some(PlaybackAudioCodec::Aac),
        ),
        "mp4" | "m4v" => (
            PlaybackMediaTransport::Progressive,
            Some(PlaybackContainer::Mp4),
            None,
            None,
        ),
        "mkv" => (
            PlaybackMediaTransport::Progressive,
            Some(PlaybackContainer::Mkv),
            None,
            None,
        ),
        "webm" => (
            PlaybackMediaTransport::Progressive,
            Some(PlaybackContainer::Webm),
            None,
            None,
        ),
        _ => return None,
    };
    Some(PlaybackMediaRequirements {
        transport,
        container,
        video_codec: video_codec.or(default_video_codec),
        audio_codec,
    })
}

pub(crate) fn playback_media_supported_by_client(
    profile: Option<&PlaybackClientProfile>,
    mode_name: &str,
    media: &PlaybackMedia,
) -> bool {
    let Some(profile) = profile.filter(|profile| profile.uses_explicit_capabilities()) else {
        return true;
    };
    playback_media_requirements(mode_name, media).is_some_and(|requirements| {
        profile.supports_media(
            requirements.transport,
            requirements.container,
            requirements.video_codec,
            requirements.audio_codec,
        )
    })
}

pub(crate) fn direct_playback_media_supported_by_client(
    profile: Option<&PlaybackClientProfile>,
    mode_name: &str,
    media: &PlaybackMedia,
) -> bool {
    let Some(profile) = profile.filter(|profile| profile.uses_explicit_capabilities()) else {
        return true;
    };
    if !media.upstream_headers().is_empty() && !profile.supports_custom_http_headers {
        return false;
    }
    if !direct_http_resource_supported_by_client(profile, media.upstream_url()) {
        return false;
    }
    let Some(requirements) = playback_media_requirements(mode_name, media) else {
        return false;
    };
    if !profile.supports_media(
        requirements.transport,
        requirements.container,
        requirements.video_codec,
        requirements.audio_codec,
    ) {
        return false;
    }
    // JavaScript MSE loaders fetch manifests and segments and therefore depend
    // on upstream CORS. A native HTML media pipeline can consume a direct URL;
    // other Web pipelines use the same-origin provider proxy.
    !profile.is_web()
        || profile.supports_media_with_pipeline(
            requirements.transport,
            requirements.container,
            requirements.video_codec,
            requirements.audio_codec,
            PlaybackMediaPipeline::Native,
        )
}

fn direct_http_resource_supported_by_client(
    profile: &PlaybackClientProfile,
    url: Option<&str>,
) -> bool {
    !profile.is_web()
        || profile.supports_insecure_http_media
        || url.is_none_or(|url| {
            !url::Url::parse(url).is_ok_and(|parsed| parsed.scheme().eq_ignore_ascii_case("http"))
        })
}

pub(crate) fn proxy_playback_media_supported_by_client(
    profile: Option<&PlaybackClientProfile>,
    mode_name: &str,
    media: &PlaybackMedia,
) -> bool {
    profile.is_none_or(|profile| {
        (!profile.uses_explicit_capabilities() || profile.supports_provider_proxy)
            && playback_media_supported_by_client(Some(profile), mode_name, media)
    })
}

pub(crate) fn require_compatible_playback_route(
    result: PlaybackResult,
    proxy_mode: crate::models::PlaybackProxyMode,
    profile: Option<&PlaybackClientProfile>,
) -> Result<PlaybackResult, ProviderError> {
    if !result.playback_infos.is_empty() {
        return Ok(result);
    }
    if profile.is_some_and(PlaybackClientProfile::uses_explicit_capabilities) {
        let required_capability = if matches!(
            proxy_mode,
            crate::models::PlaybackProxyMode::Only | crate::models::PlaybackProxyMode::Auto
        ) && profile
            .is_some_and(|profile| !profile.supports_provider_proxy)
        {
            Some("provider_proxy".to_string())
        } else if matches!(proxy_mode, crate::models::PlaybackProxyMode::DirectOnly)
            && profile.is_some_and(|profile| {
                profile.is_web()
                    && (!profile.supports_custom_http_headers
                        || !profile.supports_insecure_http_media)
            })
        {
            Some("browser_direct_media_access_or_provider_proxy".to_string())
        } else {
            Some("media_transport_codec_combination".to_string())
        };
        return Err(ProviderError::ClientIncompatible {
            reason: format!(
                "No playback route matches the client capabilities and proxy mode {proxy_mode:?}"
            ),
            required_capability,
        });
    }
    require_direct_playback_route(result, proxy_mode)
}

pub(crate) fn map_playback_resources<T, U>(
    resources: &[T],
    default_index: Option<usize>,
    mut map: impl FnMut(usize, &T) -> Option<U>,
) -> (Vec<U>, Option<usize>) {
    let mut mapped_default = None;
    let mut mapped = Vec::with_capacity(resources.len());
    for (source_index, resource) in resources.iter().enumerate() {
        let Some(resource) = map(source_index, resource) else {
            continue;
        };
        if default_index == Some(source_index) {
            mapped_default = Some(mapped.len());
        }
        mapped.push(resource);
    }
    (mapped, mapped_default)
}

/// Build one direct route from a provider's cached upstream response.
///
/// Providers call this before inserting the route into the generated result,
/// so unsupported resources never become public playback routes.
pub(crate) fn build_direct_playback_info_for_client(
    mode_name: &str,
    source: &PlaybackInfo,
    profile: Option<&PlaybackClientProfile>,
) -> Option<PlaybackInfo> {
    let Some(profile) = profile.filter(|profile| profile.uses_explicit_capabilities()) else {
        return Some(source.clone());
    };
    let (medias, default_media_index) =
        map_playback_resources(&source.medias, source.default_media_index, |_, media| {
            direct_playback_media_supported_by_client(Some(profile), mode_name, media)
                .then(|| media.clone())
        });
    if medias.is_empty() {
        return None;
    }
    let (subtitles, default_subtitle_index) = map_playback_resources(
        &source.subtitles,
        source.default_subtitle_index,
        |_, subtitle| {
            (subtitle.requires_provider_url()
                || (direct_http_resource_supported_by_client(
                    profile,
                    Some(subtitle.upstream_url()),
                ) && (profile.supports_custom_http_headers
                    || subtitle.upstream_headers().is_empty())))
            .then(|| subtitle.clone())
        },
    );
    let (danmakus, default_danmaku_index) = map_playback_resources(
        &source.danmakus,
        source.default_danmaku_index,
        |_, danmaku| {
            (danmaku.requires_provider_url()
                || (direct_http_resource_supported_by_client(profile, danmaku.upstream_url())
                    && (profile.supports_custom_http_headers
                        || danmaku.upstream_headers().is_empty())))
            .then(|| danmaku.clone())
        },
    );
    Some(PlaybackInfo {
        thumbnail: source.thumbnail.clone(),
        medias,
        default_media_index,
        subtitles,
        default_subtitle_index,
        danmakus,
        default_danmaku_index,
    })
}

/// Build one provider-proxy route from a provider's cached upstream response.
pub(crate) fn build_proxy_playback_info_for_client(
    mode_name: &str,
    source: &PlaybackInfo,
    profile: Option<&PlaybackClientProfile>,
) -> Option<PlaybackInfo> {
    let (medias, default_media_index) =
        map_playback_resources(&source.medias, source.default_media_index, |_, media| {
            proxy_playback_media_supported_by_client(profile, mode_name, media)
                .then(|| media.clone())
        });
    (!medias.is_empty()).then(|| PlaybackInfo {
        thumbnail: source.thumbnail.clone(),
        medias,
        default_media_index,
        subtitles: source.subtitles.clone(),
        default_subtitle_index: source.default_subtitle_index,
        danmakus: source.danmakus.clone(),
        default_danmaku_index: source.default_danmaku_index,
    })
}

use crate::models::media::{
    PlaybackDanmaku, PlaybackMedia, PlaybackMediaProvider, PlaybackRtmpMedia, PlaybackSubtitle,
};
use crate::models::{normalize_provider_instance_name, MediaId, RoomId};
use sha2::{Digest, Sha256};
use std::future::Future;

use crate::cache::{SingleFlight, SingleFlightError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaybackRouteSelection {
    pub direct: bool,
    pub proxy: bool,
    pub prefer_proxy: bool,
}

impl PlaybackRouteSelection {
    pub const DIRECT_ONLY: Self = Self {
        direct: true,
        proxy: false,
        prefer_proxy: false,
    };
    pub const PROXY_ONLY: Self = Self {
        direct: false,
        proxy: true,
        prefer_proxy: true,
    };
    pub const PROXY_PREFERRED: Self = Self {
        direct: true,
        proxy: true,
        prefer_proxy: true,
    };
    pub const DIRECT_PREFERRED: Self = Self {
        direct: true,
        proxy: true,
        prefer_proxy: false,
    };
}

pub(crate) fn select_generated_playback_default(
    result: &mut PlaybackResult,
    original_default: &str,
    prefer_proxy: bool,
) {
    let base_default = original_default
        .strip_prefix("proxy_")
        .or_else(|| original_default.strip_prefix("direct_"))
        .unwrap_or(original_default);
    let direct_default = [
        original_default.to_string(),
        format!("direct_{base_default}"),
        "direct".to_string(),
    ]
    .into_iter()
    .find(|mode_name| result.playback_infos.contains_key(mode_name))
    .or_else(|| {
        result
            .playback_infos
            .keys()
            .filter(|mode_name| !mode_name.starts_with("proxy_"))
            .min()
            .cloned()
    });
    let proxy_default = format!("proxy_{base_default}");
    let proxy_default = result
        .playback_infos
        .contains_key(&proxy_default)
        .then_some(proxy_default)
        .or_else(|| {
            result
                .playback_infos
                .keys()
                .filter(|mode_name| mode_name.starts_with("proxy_"))
                .min()
                .cloned()
        });
    result.default_mode = if prefer_proxy {
        proxy_default.or(direct_default)
    } else {
        direct_default.or(proxy_default)
    }
    .unwrap_or_default();
}

pub(crate) fn require_direct_playback_route(
    result: PlaybackResult,
    proxy_mode: crate::models::PlaybackProxyMode,
) -> std::result::Result<PlaybackResult, ProviderError> {
    if matches!(proxy_mode, crate::models::PlaybackProxyMode::DirectOnly)
        && result.playback_infos.is_empty()
    {
        return Err(ProviderError::UnsupportedFormat(
            "This media source cannot provide a direct playback route".to_string(),
        ));
    }
    Ok(result)
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
    BilibiliMatchRequest, BilibiliMatchResponse, BilibiliMatchedResource, BilibiliPageInfo,
    BilibiliParseLivePageRequest, BilibiliParsePgcPageRequest, BilibiliParseVideoPageRequest,
    BilibiliPersistedQrLoginResponse, BilibiliPgcSeasonIndexItem, BilibiliPgcSeasonIndexPage,
    BilibiliPgcTimelineItem, BilibiliProvider, BilibiliQrCodeResponse, BilibiliQrLoginRequest,
    BilibiliQrLoginResponse, BilibiliQrLoginStatus, BilibiliSmsLoginRequest,
    BilibiliSmsLoginResponse, BilibiliSmsLoginTokenCodec, BilibiliSmsRequest, BilibiliSmsResponse,
    BilibiliUserInfoRequest, BilibiliUserInfoResponse, BilibiliVideoInfo, LIVE_DANMAKU_FORMAT,
    LIVE_DANMAKU_TRACK_NAME,
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
    #[error("Client cannot play this resource: {reason}")]
    ClientIncompatible {
        reason: String,
        required_capability: Option<String>,
    },
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
            ProviderError::ClientIncompatible {
                reason,
                required_capability,
            } => Self::ClientIncompatible {
                reason,
                required_capability,
            },
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
            ProviderPlaybackFillError::ClientIncompatible {
                reason,
                required_capability,
            } => Self::ClientIncompatible {
                reason,
                required_capability,
            },
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
        provider: crate::models::SourceProvider::Rtmp,
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
    provider_instance_name: Option<&str>,
    version: &str,
    expires_at: i64,
    mark_provider_resources: impl FnOnce(&mut PlaybackResult, &str, i64),
) -> PlaybackResult {
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
    cached: VersionedPlayback,
    provider_name: &str,
    ctx: &ProviderContext<'_>,
    mark_provider_resources: impl FnOnce(&mut PlaybackResult, &str, i64),
) -> std::result::Result<PlaybackResult, ProviderError> {
    let store = ctx.store.as_ref().ok_or_else(|| {
        ProviderError::Internal(format!(
            "Provider '{provider_name}' cannot generate playback transport without a provider store"
        ))
    })?;
    let versioned = VersionedPlayback {
        version: synctv_common::snanoid!(16),
        result: cached.result,
        expires_at: cached.expires_at,
        playback_context: versioned_playback_context(ctx),
    };
    persist_versioned_mapping(
        store.as_ref(),
        &versioned,
        remaining_versioned_ttl(versioned.expires_at),
        provider_name,
    )
    .await?;

    Ok(build_versioned_playback_response(
        versioned.result,
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
        playback_context: None,
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

fn versioned_playback_context(ctx: &ProviderContext<'_>) -> Option<VersionedPlaybackContext> {
    match (ctx.room_id().copied(), ctx.playback_generation()) {
        (Some(room_id), Some(playback_generation)) => Some(VersionedPlaybackContext {
            room_id,
            playback_generation,
            is_playing: ctx.playback_is_playing().unwrap_or(false),
            resource_owner_id: ctx.credential_owner_id().copied(),
        }),
        _ => None,
    }
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
    let actor = match ctx.actor() {
        ProviderActor::System => "system".to_string(),
        ProviderActor::User(user_id) => format!("user:{user_id}"),
        ProviderActor::Guest => "guest".to_string(),
    };
    let credential_owner_id = ctx
        .credential_owner_id()
        .map(std::string::ToString::to_string)
        .unwrap_or_default();
    let room_id = ctx.room_id.map(|id| id.to_string()).unwrap_or_default();
    let provider_instance_name = ctx.provider_instance_name.unwrap_or_default();
    let mut hasher = Sha256::new();
    for component in [
        provider_name,
        actor.as_str(),
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
mod playback_route_capability_tests {
    use super::*;
    use crate::models::media::{
        PlaybackDanmakuProvider, PlaybackDirectUrlDanmaku, PlaybackDirectUrlMedia,
        PlaybackDirectUrlSubtitle, PlaybackMediaProvider, PlaybackSubtitleProvider,
    };
    use std::collections::HashMap;

    fn web_profile() -> PlaybackClientProfile {
        PlaybackClientProfile {
            profile_version: CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
            environment: PlaybackClientEnvironment::Web,
            media_capabilities: vec![PlaybackMediaCapability {
                transport: PlaybackMediaTransport::Progressive,
                container: Some(PlaybackContainer::Mp4),
                video_codec: Some(PlaybackVideoCodec::H264),
                audio_codec: Some(PlaybackAudioCodec::Aac),
                pipeline: PlaybackMediaPipeline::Native,
                codec_string: Some("avc1.42E01E,mp4a.40.2".to_string()),
            }],
            supports_custom_http_headers: false,
            supports_provider_proxy: true,
            supports_insecure_http_media: false,
            ..Default::default()
        }
    }

    fn web_streaming_profile(
        transport: PlaybackMediaTransport,
        pipeline: PlaybackMediaPipeline,
    ) -> PlaybackClientProfile {
        PlaybackClientProfile {
            profile_version: CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
            environment: PlaybackClientEnvironment::Web,
            media_capabilities: vec![PlaybackMediaCapability {
                transport,
                container: None,
                video_codec: Some(PlaybackVideoCodec::H264),
                audio_codec: Some(PlaybackAudioCodec::Aac),
                pipeline,
                codec_string: None,
            }],
            supports_custom_http_headers: false,
            supports_provider_proxy: true,
            supports_insecure_http_media: false,
            ..Default::default()
        }
    }

    fn media(provider: PlaybackMediaProvider) -> PlaybackMedia {
        media_with_format("mp4", provider)
    }

    fn media_with_format(format: &str, provider: PlaybackMediaProvider) -> PlaybackMedia {
        PlaybackMedia {
            name: "video".to_string(),
            format: format.to_string(),
            expire_at: None,
            metadata: None,
            p2p_swarm_id: None,
            provider,
        }
    }

    fn info(media: PlaybackMedia) -> PlaybackInfo {
        PlaybackInfo {
            thumbnail: None,
            medias: vec![media],
            default_media_index: Some(0),
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            default_danmaku_index: None,
        }
    }

    fn result(playback_infos: std::collections::HashMap<String, PlaybackInfo>) -> PlaybackResult {
        PlaybackResult {
            playback_infos,
            default_mode: "direct".to_string(),
            provider: crate::models::SourceProvider::DirectUrl,
            provider_instance_name: None,
            duration_seconds: None,
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            metadata: None,
        }
    }

    fn generate_test_routes(
        mut result: PlaybackResult,
        proxy_mode: crate::models::PlaybackProxyMode,
        profile: Option<&PlaybackClientProfile>,
    ) -> Result<PlaybackResult, ProviderError> {
        let original_default = result.default_mode.clone();
        result.playback_infos = std::mem::take(&mut result.playback_infos)
            .into_iter()
            .filter_map(|(mode_name, info)| {
                let prepared = if mode_name.starts_with("proxy_") {
                    build_proxy_playback_info_for_client(&mode_name, &info, profile)
                } else {
                    build_direct_playback_info_for_client(&mode_name, &info, profile)
                };
                prepared.map(|info| (mode_name, info))
            })
            .collect();
        select_generated_playback_default(
            &mut result,
            &original_default,
            matches!(
                proxy_mode,
                crate::models::PlaybackProxyMode::Only | crate::models::PlaybackProxyMode::Prefer
            ),
        );
        require_compatible_playback_route(result, proxy_mode, profile)
    }

    #[test]
    fn mapped_playback_resources_keep_source_indices_and_remap_the_default() {
        let resources = ["unsupported", "selected", "fallback"];
        let (mapped, default_index) =
            map_playback_resources(&resources, Some(1), |source_index, resource| {
                (source_index > 0).then_some((source_index, *resource))
            });

        assert_eq!(mapped, vec![(1, "selected"), (2, "fallback")]);
        assert_eq!(default_index, Some(0));
    }

    #[test]
    fn web_header_bound_direct_media_falls_back_to_provider_proxy() {
        let headers = HashMap::from([("Referer".to_string(), "https://example.test".to_string())]);
        let direct = media(PlaybackMediaProvider::DirectUrl(
            PlaybackDirectUrlMedia::Direct {
                url: "https://cdn.example.test/video.mp4".to_string(),
                headers: headers.clone(),
            },
        ));
        let proxy = media(PlaybackMediaProvider::DirectUrl(
            PlaybackDirectUrlMedia::ProxyStream {
                version: "v1".to_string(),
                expires_at: 1,
                mode_name: "direct".to_string(),
                url_index: 0,
                url: "https://cdn.example.test/video.mp4".to_string(),
                headers,
            },
        ));
        let playback = result(HashMap::from([
            ("direct".to_string(), info(direct)),
            ("proxy_direct".to_string(), info(proxy)),
        ]));

        let filtered = generate_test_routes(
            playback,
            crate::models::PlaybackProxyMode::DirectPrefer,
            Some(&web_profile()),
        )
        .expect("proxy route should remain compatible");

        assert!(!filtered.playback_infos.contains_key("direct"));
        assert!(filtered.playback_infos.contains_key("proxy_direct"));
        assert_eq!(filtered.default_mode, "proxy_direct");
    }

    #[test]
    fn web_media_source_flv_uses_the_provider_proxy_route() {
        let direct = media_with_format(
            "flv",
            PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct {
                url: "https://cdn.example.test/live.flv".to_string(),
                headers: HashMap::new(),
            }),
        );
        let proxy = media_with_format(
            "flv",
            PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::ProxyStream {
                version: "v1".to_string(),
                expires_at: 1,
                mode_name: "flv".to_string(),
                url_index: 0,
                url: "https://cdn.example.test/live.flv".to_string(),
                headers: HashMap::new(),
            }),
        );
        let playback = result(HashMap::from([
            ("flv".to_string(), info(direct)),
            ("proxy_flv".to_string(), info(proxy)),
        ]));
        let profile = web_streaming_profile(
            PlaybackMediaTransport::Flv,
            PlaybackMediaPipeline::MediaSource,
        );

        let filtered = generate_test_routes(
            playback,
            crate::models::PlaybackProxyMode::DirectPrefer,
            Some(&profile),
        )
        .expect("same-origin FLV proxy should support a MediaSource loader");

        assert!(!filtered.playback_infos.contains_key("flv"));
        assert!(filtered.playback_infos.contains_key("proxy_flv"));
        assert_eq!(filtered.default_mode, "proxy_flv");
    }

    #[test]
    fn web_native_hls_can_keep_a_direct_route() {
        let direct = media_with_format(
            "m3u8",
            PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct {
                url: "https://cdn.example.test/live.m3u8".to_string(),
                headers: HashMap::new(),
            }),
        );
        let profile =
            web_streaming_profile(PlaybackMediaTransport::Hls, PlaybackMediaPipeline::Native);

        let filtered = generate_test_routes(
            result(HashMap::from([("hls".to_string(), info(direct))])),
            crate::models::PlaybackProxyMode::DirectOnly,
            Some(&profile),
        )
        .expect("native browser HLS should consume a public direct URL");

        assert!(filtered.playback_infos.contains_key("hls"));
    }

    #[test]
    fn secure_web_client_replaces_insecure_direct_media_with_proxy() {
        let direct = media(PlaybackMediaProvider::DirectUrl(
            PlaybackDirectUrlMedia::Direct {
                url: "http://media.example.test/video.mp4".to_string(),
                headers: HashMap::new(),
            },
        ));
        let proxy = media(PlaybackMediaProvider::DirectUrl(
            PlaybackDirectUrlMedia::ProxyStream {
                version: "v1".to_string(),
                expires_at: 1,
                mode_name: "direct".to_string(),
                url_index: 0,
                url: "http://media.example.test/video.mp4".to_string(),
                headers: HashMap::new(),
            },
        ));

        let filtered = generate_test_routes(
            result(HashMap::from([
                ("direct".to_string(), info(direct)),
                ("proxy_direct".to_string(), info(proxy)),
            ])),
            crate::models::PlaybackProxyMode::DirectPrefer,
            Some(&web_profile()),
        )
        .expect("same-origin proxy should replace mixed-content direct media");

        assert!(!filtered.playback_infos.contains_key("direct"));
        assert!(filtered.playback_infos.contains_key("proxy_direct"));
        assert_eq!(filtered.default_mode, "proxy_direct");
    }

    #[test]
    fn secure_web_direct_only_reports_mixed_content_capability() {
        let direct = media(PlaybackMediaProvider::DirectUrl(
            PlaybackDirectUrlMedia::Direct {
                url: "http://media.example.test/video.mp4".to_string(),
                headers: HashMap::new(),
            },
        ));

        let error = generate_test_routes(
            result(HashMap::from([("direct".to_string(), info(direct))])),
            crate::models::PlaybackProxyMode::DirectOnly,
            Some(&web_profile()),
        )
        .expect_err("mixed-content direct-only playback must fail clearly");

        assert!(matches!(
            error,
            ProviderError::ClientIncompatible {
                required_capability: Some(ref capability),
                ..
            } if capability == "browser_direct_media_access_or_provider_proxy"
        ));
    }

    #[test]
    fn secure_web_client_filters_insecure_direct_attachments() {
        let mut playback_info = info(media(PlaybackMediaProvider::DirectUrl(
            PlaybackDirectUrlMedia::Direct {
                url: "https://media.example.test/video.mp4".to_string(),
                headers: HashMap::new(),
            },
        )));
        playback_info.subtitles = vec![PlaybackSubtitle {
            name: "mixed content".to_string(),
            language: "en".to_string(),
            format: "vtt".to_string(),
            p2p_swarm_id: None,
            provider: PlaybackSubtitleProvider::DirectUrl(PlaybackDirectUrlSubtitle::Direct {
                url: "http://media.example.test/subtitle.vtt".to_string(),
                headers: HashMap::new(),
                expire_at: None,
            }),
        }];
        playback_info.default_subtitle_index = Some(0);
        playback_info.danmakus = vec![PlaybackDanmaku {
            name: "mixed content".to_string(),
            format: Some("xml".to_string()),
            p2p_swarm_id: None,
            provider: PlaybackDanmakuProvider::DirectUrl(PlaybackDirectUrlDanmaku {
                url: "http://media.example.test/danmaku.xml".to_string(),
                headers: HashMap::new(),
                expire_at: None,
            }),
        }];
        playback_info.default_danmaku_index = Some(0);

        let filtered = generate_test_routes(
            result(HashMap::from([("direct".to_string(), playback_info)])),
            crate::models::PlaybackProxyMode::DirectOnly,
            Some(&web_profile()),
        )
        .expect("secure media remains playable without mixed-content attachments");
        let info = &filtered.playback_infos["direct"];

        assert!(info.subtitles.is_empty());
        assert_eq!(info.default_subtitle_index, None);
        assert!(info.danmakus.is_empty());
        assert_eq!(info.default_danmaku_index, None);
    }

    #[test]
    fn unsupported_live_transport_returns_structured_client_incompatibility() {
        let proxy = media_with_format(
            "flv",
            PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::ProxyStream {
                version: "v1".to_string(),
                expires_at: 1,
                mode_name: "flv".to_string(),
                url_index: 0,
                url: "https://cdn.example.test/live.flv".to_string(),
                headers: HashMap::new(),
            }),
        );
        let profile = web_streaming_profile(
            PlaybackMediaTransport::Hls,
            PlaybackMediaPipeline::MediaSource,
        );

        let error = generate_test_routes(
            result(HashMap::from([("proxy_flv".to_string(), info(proxy))])),
            crate::models::PlaybackProxyMode::Only,
            Some(&profile),
        )
        .expect_err("an HLS-only browser must reject FLV playback");

        assert!(matches!(
            error,
            ProviderError::ClientIncompatible {
                required_capability: Some(ref capability),
                ..
            } if capability == "media_transport_codec_combination"
        ));
    }

    #[test]
    fn header_bound_direct_only_reports_the_missing_browser_capability() {
        let headers = HashMap::from([("Referer".to_string(), "https://example.test".to_string())]);
        let direct = media(PlaybackMediaProvider::DirectUrl(
            PlaybackDirectUrlMedia::Direct {
                url: "https://cdn.example.test/video.mp4".to_string(),
                headers,
            },
        ));

        let error = generate_test_routes(
            result(HashMap::from([("direct".to_string(), info(direct))])),
            crate::models::PlaybackProxyMode::DirectOnly,
            Some(&web_profile()),
        )
        .expect_err("browser direct playback cannot attach a Referer header");

        assert!(matches!(
            error,
            ProviderError::ClientIncompatible {
                required_capability: Some(ref capability),
                ..
            } if capability == "browser_direct_media_access_or_provider_proxy"
        ));
    }

    #[test]
    fn header_filtering_remaps_attachment_default_indices() {
        let mut playback_info = info(media(PlaybackMediaProvider::DirectUrl(
            PlaybackDirectUrlMedia::Direct {
                url: "https://cdn.example.test/video.mp4".to_string(),
                headers: HashMap::new(),
            },
        )));
        let headers = HashMap::from([("Authorization".to_string(), "secret".to_string())]);
        playback_info.subtitles = vec![
            PlaybackSubtitle {
                name: "header-bound".to_string(),
                language: "en".to_string(),
                format: "vtt".to_string(),
                p2p_swarm_id: None,
                provider: PlaybackSubtitleProvider::DirectUrl(PlaybackDirectUrlSubtitle::Direct {
                    url: "https://cdn.example.test/private.vtt".to_string(),
                    headers: headers.clone(),
                    expire_at: None,
                }),
            },
            PlaybackSubtitle {
                name: "public".to_string(),
                language: "en".to_string(),
                format: "vtt".to_string(),
                p2p_swarm_id: None,
                provider: PlaybackSubtitleProvider::DirectUrl(PlaybackDirectUrlSubtitle::Direct {
                    url: "https://cdn.example.test/public.vtt".to_string(),
                    headers: HashMap::new(),
                    expire_at: None,
                }),
            },
        ];
        playback_info.default_subtitle_index = Some(1);
        playback_info.danmakus = vec![
            PlaybackDanmaku {
                name: "header-bound".to_string(),
                format: Some("xml".to_string()),
                p2p_swarm_id: None,
                provider: PlaybackDanmakuProvider::DirectUrl(PlaybackDirectUrlDanmaku {
                    url: "https://cdn.example.test/private.xml".to_string(),
                    headers,
                    expire_at: None,
                }),
            },
            PlaybackDanmaku {
                name: "public".to_string(),
                format: Some("xml".to_string()),
                p2p_swarm_id: None,
                provider: PlaybackDanmakuProvider::DirectUrl(PlaybackDirectUrlDanmaku {
                    url: "https://cdn.example.test/public.xml".to_string(),
                    headers: HashMap::new(),
                    expire_at: None,
                }),
            },
        ];
        playback_info.default_danmaku_index = Some(1);

        let filtered = generate_test_routes(
            result(HashMap::from([("direct".to_string(), playback_info)])),
            crate::models::PlaybackProxyMode::DirectOnly,
            Some(&web_profile()),
        )
        .expect("public direct resources should remain compatible");
        let info = &filtered.playback_infos["direct"];

        assert_eq!(info.subtitles.len(), 1);
        assert_eq!(info.default_subtitle_index, Some(0));
        assert_eq!(info.danmakus.len(), 1);
        assert_eq!(info.default_danmaku_index, Some(0));
    }
}

#[cfg(test)]
mod playback_transport_expiry_tests {
    use super::*;

    fn live_playback() -> PlaybackResult {
        build_live_playback(MediaId::new(), RoomId::new())
    }

    #[tokio::test]
    async fn cached_playback_gets_a_fresh_request_lifecycle_mapping() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(16));
        let old_room_id = RoomId::expect_positive(1);
        let old_owner_id = crate::models::UserId::expect_positive(2);
        let preflight_ctx = ProviderContext::new("test", ProviderActor::System)
            .with_store(store.clone())
            .with_room_id(old_room_id);
        let mut cached = cache_versioned_playback(
            live_playback(),
            "test-provider",
            "playback-cache-key",
            Duration::from_secs(60),
            &preflight_ctx,
        )
        .await
        .expect("preflight cache should be written");
        assert!(cached.playback_context.is_none());
        cached.version = "cached-version".to_string();
        cached.playback_context = Some(VersionedPlaybackContext {
            room_id: old_room_id,
            playback_generation: 10,
            is_playing: false,
            resource_owner_id: Some(old_owner_id),
        });

        let first_room_id = RoomId::expect_positive(3);
        let first_owner_id = crate::models::UserId::expect_positive(4);
        let first_ctx = ProviderContext::new("test", ProviderActor::User(first_owner_id))
            .with_store(store.clone())
            .with_room_id(first_room_id)
            .with_credential_owner_id(first_owner_id)
            .with_playback_generation(11)
            .with_playback_is_playing(true);
        let first_version = Arc::new(std::sync::Mutex::new(None));
        let captured_first_version = first_version.clone();
        build_cached_versioned_playback_response(
            cached.clone(),
            "test-provider",
            &first_ctx,
            move |_, version, _| {
                *captured_first_version.lock().expect("version lock") = Some(version.to_string());
            },
        )
        .await
        .expect("first response should be built");

        let second_room_id = RoomId::expect_positive(5);
        let second_owner_id = crate::models::UserId::expect_positive(6);
        let second_ctx = ProviderContext::new("test", ProviderActor::User(second_owner_id))
            .with_store(store.clone())
            .with_room_id(second_room_id)
            .with_credential_owner_id(second_owner_id)
            .with_playback_generation(12);
        let second_version = Arc::new(std::sync::Mutex::new(None));
        let captured_second_version = second_version.clone();
        build_cached_versioned_playback_response(
            cached,
            "test-provider",
            &second_ctx,
            move |_, version, _| {
                *captured_second_version.lock().expect("version lock") = Some(version.to_string());
            },
        )
        .await
        .expect("second response should be built");

        let first_version = first_version
            .lock()
            .expect("version lock")
            .clone()
            .expect("first version should be captured");
        let second_version = second_version
            .lock()
            .expect("version lock")
            .clone()
            .expect("second version should be captured");
        assert_ne!(first_version, "cached-version");
        assert_ne!(second_version, "cached-version");
        assert_ne!(first_version, second_version);

        let first_mapping = store
            .get::<VersionedPlayback>(&format!("v:{first_version}"))
            .await
            .expect("first mapping should be readable")
            .expect("first mapping should exist");
        let second_mapping = store
            .get::<VersionedPlayback>(&format!("v:{second_version}"))
            .await
            .expect("second mapping should be readable")
            .expect("second mapping should exist");

        let first_context = first_mapping
            .playback_context
            .expect("first mapping should have lifecycle context");
        assert_eq!(first_context.room_id, first_room_id);
        assert_eq!(first_context.playback_generation, 11);
        assert!(first_context.is_playing);
        assert_eq!(first_context.resource_owner_id, Some(first_owner_id));

        let second_context = second_mapping
            .playback_context
            .expect("second mapping should have lifecycle context");
        assert_eq!(second_context.room_id, second_room_id);
        assert_eq!(second_context.playback_generation, 12);
        assert!(!second_context.is_playing);
        assert_eq!(second_context.resource_owner_id, Some(second_owner_id));
        assert!(store
            .get::<VersionedPlayback>("v:cached-version")
            .await
            .expect("old mapping lookup should succeed")
            .is_none());
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
        ProviderContext::new("source-cover-cache-test", ProviderActor::System).with_store(store)
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
        let ctx = ProviderContext::new(
            "metadata-cache-test",
            ProviderActor::User(crate::models::UserId::expect_positive(1)),
        )
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
        let first_ctx = ProviderContext::new("metadata-cache-test", ProviderActor::System)
            .with_provider_instance_name("instance:resource")
            .with_store(store.clone());
        let second_ctx = ProviderContext::new("metadata-cache-test", ProviderActor::System)
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
        let ctx =
            ProviderContext::new("metadata-cache-test", ProviderActor::System).with_store(store);
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
        let ctx = ProviderContext::new("metadata-cache-test", ProviderActor::System);
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
