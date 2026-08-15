use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;

use super::file_storage::FileReferenceTarget;
use super::id::{MediaId, PlaylistId, RoomId, UserId};
use super::normalize_provider_instance_name_owned;
use super::query::SortDirection;
use super::{MediaSourceConfig, PlaybackKind, ProviderTarget, SynologyLibraryItemKind};

sort_field_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum MediaListSortBy {
        Name => { display: "name", sql: "name" },
        AddedAt => { display: "added_at", sql: "added_at" },
        UpdatedAt => { display: "updated_at", sql: "updated_at" },
        SourceProvider => {
            display: "source_provider",
            sql: "source_provider"
        },
        ProviderInstanceName => {
            display: "provider_instance_name",
            sql: "provider_instance_name"
        },
        Position => { display: "position", sql: "position" },
    }
    default = Position;
    error = "Unknown media list sort field";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaListQuery {
    pub pagination: super::pagination::PageParams,
    pub search: Option<String>,
    pub source_provider: Option<SourceProvider>,
    pub provider_instance_name: Option<String>,
    pub availability: Option<bool>,
    #[serde(default)]
    pub sort_by: MediaListSortBy,
    #[serde(default)]
    pub sort_direction: SortDirection,
}

impl Default for MediaListQuery {
    fn default() -> Self {
        Self {
            pagination: super::pagination::PageParams::default(),
            search: None,
            source_provider: None,
            provider_instance_name: None,
            availability: None,
            sort_by: MediaListSortBy::Position,
            sort_direction: SortDirection::Asc,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceProvider {
    DirectUrl,
    Bilibili,
    Alist,
    Emby,
    Rtmp,
    LiveProxy,
    Cloudreve,
    Twitch,
    Huya,
    Douyu,
    Douyin,
    AcFun,
    Cctv,
    Fnos,
    Qnap,
    Synology,
    Nextcloud,
    Seafile,
    TrueNas,
    Youtube,
    TikTok,
}

pub type ProviderType = SourceProvider;

impl FromStr for SourceProvider {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "direct_url" => Ok(Self::DirectUrl),
            "bilibili" => Ok(Self::Bilibili),
            "alist" => Ok(Self::Alist),
            "emby" => Ok(Self::Emby),
            "rtmp" => Ok(Self::Rtmp),
            "live_proxy" => Ok(Self::LiveProxy),
            "cloudreve" => Ok(Self::Cloudreve),
            "twitch" => Ok(Self::Twitch),
            "huya" => Ok(Self::Huya),
            "douyu" => Ok(Self::Douyu),
            "douyin" => Ok(Self::Douyin),
            "acfun" => Ok(Self::AcFun),
            "cctv" => Ok(Self::Cctv),
            "fnos" => Ok(Self::Fnos),
            "qnap" => Ok(Self::Qnap),
            "synology" => Ok(Self::Synology),
            "nextcloud" => Ok(Self::Nextcloud),
            "seafile" => Ok(Self::Seafile),
            "truenas" => Ok(Self::TrueNas),
            "youtube" => Ok(Self::Youtube),
            "tiktok" => Ok(Self::TikTok),
            other => Err(format!("Unknown provider type: {other}")),
        }
    }
}

impl SourceProvider {
    pub const ALL: &'static [Self] = &[
        Self::DirectUrl,
        Self::Bilibili,
        Self::Alist,
        Self::Emby,
        Self::Rtmp,
        Self::LiveProxy,
        Self::Cloudreve,
        Self::Twitch,
        Self::Huya,
        Self::Douyu,
        Self::Douyin,
        Self::AcFun,
        Self::Cctv,
        Self::Fnos,
        Self::Qnap,
        Self::Synology,
        Self::Nextcloud,
        Self::Seafile,
        Self::TrueNas,
        Self::Youtube,
        Self::TikTok,
    ];

    #[must_use]
    pub const fn as_i16(self) -> i16 {
        match self {
            Self::DirectUrl => 1,
            Self::Bilibili => 2,
            Self::Alist => 3,
            Self::Emby => 4,
            Self::Rtmp => 5,
            Self::LiveProxy => 6,
            Self::Cloudreve => 7,
            Self::Twitch => 8,
            Self::Huya => 9,
            Self::Douyu => 10,
            Self::Douyin => 11,
            Self::AcFun => 12,
            Self::Cctv => 13,
            Self::Fnos => 14,
            Self::Qnap => 15,
            Self::Synology => 16,
            Self::Nextcloud => 17,
            Self::Seafile => 18,
            Self::TrueNas => 19,
            Self::Youtube => 20,
            Self::TikTok => 21,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DirectUrl => "direct_url",
            Self::Bilibili => "bilibili",
            Self::Alist => "alist",
            Self::Emby => "emby",
            Self::Rtmp => "rtmp",
            Self::LiveProxy => "live_proxy",
            Self::Cloudreve => "cloudreve",
            Self::Twitch => "twitch",
            Self::Huya => "huya",
            Self::Douyu => "douyu",
            Self::Douyin => "douyin",
            Self::AcFun => "acfun",
            Self::Cctv => "cctv",
            Self::Fnos => "fnos",
            Self::Qnap => "qnap",
            Self::Synology => "synology",
            Self::Nextcloud => "nextcloud",
            Self::Seafile => "seafile",
            Self::TrueNas => "truenas",
            Self::Youtube => "youtube",
            Self::TikTok => "tiktok",
        }
    }
}

impl TryFrom<i16> for SourceProvider {
    type Error = String;

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::DirectUrl),
            2 => Ok(Self::Bilibili),
            3 => Ok(Self::Alist),
            4 => Ok(Self::Emby),
            5 => Ok(Self::Rtmp),
            6 => Ok(Self::LiveProxy),
            7 => Ok(Self::Cloudreve),
            8 => Ok(Self::Twitch),
            9 => Ok(Self::Huya),
            10 => Ok(Self::Douyu),
            11 => Ok(Self::Douyin),
            12 => Ok(Self::AcFun),
            13 => Ok(Self::Cctv),
            14 => Ok(Self::Fnos),
            15 => Ok(Self::Qnap),
            16 => Ok(Self::Synology),
            17 => Ok(Self::Nextcloud),
            18 => Ok(Self::Seafile),
            19 => Ok(Self::TrueNas),
            20 => Ok(Self::Youtube),
            21 => Ok(Self::TikTok),
            other => Err(format!("Unknown provider type code: {other}")),
        }
    }
}

impl From<SourceProvider> for i16 {
    fn from(value: SourceProvider) -> Self {
        value.as_i16()
    }
}

pub fn provider_type_code_from_name(raw: &str) -> Result<i16, String> {
    raw.parse::<SourceProvider>().map(SourceProvider::as_i16)
}

pub fn provider_type_name_from_code(code: i16) -> Result<String, String> {
    SourceProvider::try_from(code).map(|provider| provider.to_string())
}

pub fn provider_type_codes_from_names<'a>(
    names: impl IntoIterator<Item = &'a String>,
) -> Result<Vec<i16>, String> {
    names
        .into_iter()
        .map(|name| provider_type_code_from_name(name))
        .collect()
}

impl std::fmt::Display for SourceProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Media file (video/audio)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Media {
    pub id: MediaId,
    pub playlist_id: Option<PlaylistId>,
    pub room_id: RoomId,
    pub creator_id: Option<UserId>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub position: f64,
    pub source_provider: SourceProvider,
    pub source_config: MediaSourceConfig,
    /// Provider instance name (e.g., "`bilibili_main`", "`alist_company`")
    /// Used to look up the provider from the registry at playback time.
    /// `None` means use the default local instance for `source_provider`.
    pub provider_instance_name: Option<String>,
    pub cover_file_reference_id: Option<i64>,
    pub thumbnail_file_reference_id: Option<i64>,
    pub added_at: DateTime<Utc>,
    /// Timestamp of last update (auto-maintained by database trigger)
    pub updated_at: DateTime<Utc>,
    /// Optimistic locking version, incremented on each update
    pub version: i32,
}

/// Parameters for creating media from a provider
#[derive(Debug, Clone)]
pub struct FromProviderParams {
    pub playlist_id: Option<PlaylistId>,
    pub room_id: RoomId,
    pub creator_id: Option<UserId>,
    pub name: String,
    pub description: String,
    pub source_config: MediaSourceConfig,
    pub source_provider: SourceProvider,
    pub provider_instance_name: Option<String>,
    pub position: f64,
}

#[derive(Debug, Clone)]
pub struct DirectMultimodeParams {
    pub playlist_id: Option<PlaylistId>,
    pub room_id: RoomId,
    pub creator_id: Option<UserId>,
    pub name: String,
    pub playback_infos: HashMap<String, PlaybackInfo>,
    pub default_mode: String,
    pub position: f64,
}

impl Media {
    #[must_use]
    pub fn from_provider_with_params(params: FromProviderParams) -> Self {
        let now = crate::SystemClock.now();
        Self {
            id: MediaId::new(),
            playlist_id: params.playlist_id,
            room_id: params.room_id,
            creator_id: params.creator_id,
            name: params.name,
            description: params.description,
            position: params.position,
            source_provider: params.source_provider,
            source_config: params.source_config,
            provider_instance_name: normalize_provider_instance_name_owned(
                params.provider_instance_name,
            ),
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: now,
            updated_at: now,
            version: 0,
        }
    }

    pub fn from_direct_multimode(params: DirectMultimodeParams) -> crate::Result<Self> {
        let default_info = params
            .playback_infos
            .get(&params.default_mode)
            .or_else(|| params.playback_infos.values().next());
        let default_media = default_info.and_then(|info| {
            info.default_media_index
                .and_then(|index| info.medias.get(index))
                .or_else(|| info.medias.first())
        });
        let default_url = default_media
            .and_then(PlaybackMedia::direct_url)
            .ok_or_else(|| {
                crate::Error::InvalidInput("direct media requires a playback URL".to_string())
            })?;
        let default_headers = default_media.map_or_else(
            std::collections::HashMap::new,
            PlaybackMedia::upstream_headers,
        );

        let source_config = super::MediaSourceConfig::DirectUrl(
            super::DirectUrlMediaSourceConfig::single(default_url.to_string(), default_headers),
        );

        let now = crate::SystemClock.now();
        Ok(Self {
            id: MediaId::new(),
            playlist_id: params.playlist_id,
            room_id: params.room_id,
            creator_id: params.creator_id,
            name: params.name,
            description: String::new(),
            position: params.position,
            source_provider: SourceProvider::DirectUrl,
            source_config,
            provider_instance_name: None,
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: now,
            updated_at: now,
            version: 0,
        })
    }

    /// Create a direct URL media with single playback info (convenience method)
    pub fn from_direct_single_mode(
        playlist_id: Option<PlaylistId>,
        room_id: RoomId,
        creator_id: Option<UserId>,
        name: String,
        mode_name: &str,
        playback_info: PlaybackInfo,
        position: f64,
    ) -> crate::Result<Self> {
        let mut playback_infos = HashMap::new();
        playback_infos.insert(mode_name.to_string(), playback_info);

        Self::from_direct_multimode(DirectMultimodeParams {
            playlist_id,
            room_id,
            creator_id,
            name,
            playback_infos,
            default_mode: mode_name.to_string(),
            position,
        })
    }

    #[must_use]
    pub fn cover_file_reference_target(
        &self,
        file: &crate::models::StoredFileReference,
    ) -> FileReferenceTarget {
        file.reference_target("media_cover", self.id.as_i64().to_string())
    }

    #[must_use]
    pub fn thumbnail_file_reference_target(
        &self,
        file: &crate::models::StoredFileReference,
    ) -> FileReferenceTarget {
        file.reference_target("media_thumbnail", self.id.as_i64().to_string())
    }
}

// Playback Information Structures (for all media types)
// PlaybackResult is returned when generating playback info (at playback time)
// For direct URL media, `source_config` stores provider input (`url` and optional
// `headers`). Runtime playback modes are produced by the
// provider instead of being embedded in persisted media rows.

/// Playback information generation result (returned by `generate_playback`)
/// This structure supports multiple playback modes (e.g., "direct" and "proxied")
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackResult {
    /// Media ID (optional, only set when returning playback for existing media)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<MediaId>,

    /// Playlist ID. `None` means the media lives directly under the room root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist_id: Option<PlaylistId>,

    /// Room ID
    pub room_id: RoomId,

    /// Media name
    pub name: String,

    /// Provider that generated this playback result.
    pub provider: SourceProvider,

    /// Provider instance selected for this playback result, when a named
    /// instance was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instance_name: Option<String>,

    /// Position in playlist
    pub position: f64,

    /// Playback mode `HashMap` (multiple `PlaybackInfo` objects)
    /// Provider can define arbitrary mode names, such as:
    /// - "direct" and "proxied" (common)
    /// - "cdn1", "cdn2", "cdn3" (multiple CDNs)
    /// - "high", "medium", "low" (different qualities)
    pub playback_infos: std::collections::HashMap<String, PlaybackInfo>,

    /// Default mode name (must be a key in `playback_infos`).
    /// Provider decides this from its own source configuration and runtime context.
    pub default_mode: String,

    /// Backend-owned source duration in seconds when the provider knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,

    /// Behavioral kind of this playback source.
    #[serde(default)]
    pub playback_kind: PlaybackKind,

    /// Provider-facing playback target for dynamic playlist items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ProviderTarget>,

    /// Media-level provider metadata for display-only, provider-specific fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<PlaybackMetadata>,
}

/// Complete playback information for a single mode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackInfo {
    /// Thumbnail URL for this playback mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,

    /// Media resources (different qualities, codecs, or provider-owned resources).
    pub medias: Vec<PlaybackMedia>,

    /// Default media index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_media_index: Option<usize>,

    /// Subtitle list
    #[serde(default)]
    pub subtitles: Vec<PlaybackSubtitle>,

    /// Default subtitle index
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_subtitle_index: Option<usize>,

    /// Danmaku list (each mode can have different danmaku sources)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub danmakus: Vec<PlaybackDanmaku>,

    /// Default danmaku index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_danmaku_index: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackMedia {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<PlaybackMediaMetadata>,
    /// Provider-generated identity for one immutable byte representation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p2p_swarm_id: Option<String>,
    #[serde(flatten)]
    pub provider: PlaybackMediaProvider,
}

impl PlaybackMedia {
    #[must_use]
    pub fn with_p2p_swarm_id(mut self, swarm_id: String) -> Self {
        self.p2p_swarm_id = Some(swarm_id);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "provider",
    content = "media",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackMediaProvider {
    Alist(PlaybackAlistMedia),
    Bilibili(PlaybackBilibiliMedia),
    Cloudreve(PlaybackCloudreveMedia),
    DirectUrl(PlaybackDirectUrlMedia),
    Emby(PlaybackEmbyMedia),
    Rtmp(PlaybackRtmpMedia),
    LiveProxy(PlaybackLiveProxyMedia),
    Twitch(PlaybackTwitchMedia),
    Youtube(PlaybackYoutubeMedia),
    Huya(PlaybackHuyaMedia),
    Douyu(PlaybackDouyuMedia),
    Douyin(PlaybackDouyinMedia),
    AcFun(PlaybackAcFunMedia),
    Cctv(PlaybackCctvMedia),
    Fnos(PlaybackFnosMedia),
    Qnap(PlaybackQnapMedia),
    Synology(PlaybackSynologyMedia),
    Nextcloud(PlaybackNextcloudMedia),
    Seafile(PlaybackSeafileMedia),
    TrueNas(PlaybackTrueNasMedia),
    TikTok(PlaybackTikTokMedia),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlistPlaybackLocator {
    pub server_id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    pub credential_owner_id: UserId,
    pub credential_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AlistPlaybackMediaLocator {
    File,
    Transcoded {
        template_id: String,
        template_name: String,
        fallback_index: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AlistPlaybackSubtitleLocator {
    RelatedFile {
        path: String,
    },
    Transcoded {
        language: String,
        fallback_index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackTrueNasMedia {
    Direct {
        url: String,
        headers: HashMap<String, String>,
    },
    Refresh {
        credential_owner_id: String,
        server_id: String,
        path: String,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
        credential_owner_id: String,
        server_id: String,
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackSeafileMedia {
    Direct {
        url: String,
        headers: HashMap<String, String>,
    },
    Refresh {
        credential_owner_id: String,
        server_id: String,
        repository_id: String,
        path: String,
        object_id: String,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
        credential_owner_id: String,
        server_id: String,
        repository_id: String,
        path: String,
        object_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackNextcloudMedia {
    Direct {
        url: String,
        headers: HashMap<String, String>,
    },
    Refresh {
        credential_owner_id: String,
        server_id: String,
        path: String,
        file_id: u64,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
        credential_owner_id: String,
        server_id: String,
        path: String,
        file_id: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackSynologyMedia {
    Direct {
        url: String,
        headers: HashMap<String, String>,
    },
    Refresh {
        credential_owner_id: String,
        server_id: String,
        resource: SynologyPlaybackResource,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
        credential_owner_id: String,
        server_id: String,
        resource: SynologyPlaybackResource,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SynologyPlaybackResource {
    File {
        path: String,
    },
    VideoStation {
        file_id: i64,
        profile: SynologyPlaybackProfile,
        audio_track: Option<i64>,
        ac3_passthrough: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SynologyPlaybackProfile {
    Raw,
    HlsRemux,
    HlsMedium,
    HlsLow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackFnosMedia {
    Direct {
        url: String,
        headers: HashMap<String, String>,
    },
    FileRefresh {
        credential_owner_id: String,
        server_id: String,
        path: String,
    },
    MediaRefresh {
        credential_owner_id: String,
        server_id: String,
        media_guid: String,
        quality_index: Option<usize>,
    },
    MediaOriginalRefresh {
        credential_owner_id: String,
        server_id: String,
        media_guid: String,
        path: String,
    },
    TranscodeRefresh {
        credential_owner_id: String,
        server_id: String,
        spec: FnosTranscodeResource,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
        credential_owner_id: String,
        server_id: String,
        resource: FnosProxyResource,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FnosProxyResource {
    File {
        path: String,
    },
    Media {
        media_guid: String,
        quality_index: Option<usize>,
    },
    MediaOriginal {
        media_guid: String,
        path: String,
    },
    Transcode {
        spec: FnosTranscodeResource,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FnosTranscodeResource {
    pub media_guid: String,
    pub video_guid: String,
    pub video_encoder: String,
    pub resolution: String,
    pub bitrate: u64,
    pub audio_guid: String,
    pub subtitle_guid: String,
    pub channels: u32,
    pub forced_sdr: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackQnapMedia {
    Direct {
        url: String,
        headers: HashMap<String, String>,
    },
    Refresh {
        credential_owner_id: String,
        server_id: String,
        resource: QnapPlaybackResource,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
        credential_owner_id: String,
        server_id: String,
        resource: QnapPlaybackResource,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QnapPlaybackResource {
    pub path: String,
    pub mode: QnapPlaybackMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum QnapPlaybackMode {
    Original,
    PreTranscoded { height: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackCctvMedia {
    Refresh {
        resource: String,
        stream_name: String,
        stream_kind: CctvPlaybackStreamKind,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CctvPlaybackStreamKind {
    VideoHls,
    AudioHls,
    Http,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackAcFunMedia {
    Refresh {
        resource_kind: AcFunPlaybackResourceKind,
        resource_id: String,
        query: Option<String>,
        quality_name: String,
        quality_type: Option<String>,
        format: AcFunPlaybackFormat,
        bitrate: Option<u64>,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AcFunPlaybackResourceKind {
    Video,
    Bangumi,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AcFunPlaybackFormat {
    Hls,
    Flv,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackDouyuMedia {
    Refresh {
        room_id: String,
        quality_name: String,
        cdn: String,
        rate: i64,
        codec: DouyuPlaybackCodec,
        format: DouyuPlaybackFormat,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DouyuPlaybackCodec {
    Avc,
    Hevc,
    Aac,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DouyuPlaybackFormat {
    Flv,
    Hls,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackHuyaMedia {
    Refresh {
        resource_kind: HuyaPlaybackResourceKind,
        resource_id: String,
        quality_name: String,
        cdn: String,
        format: HuyaPlaybackFormat,
        bitrate: Option<u64>,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HuyaPlaybackResourceKind {
    Live,
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HuyaPlaybackFormat {
    Flv,
    Hls,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackCloudreveMedia {
    Direct {
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
    ProxyStream {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
    ProxyHlsManifest {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackTwitchMedia {
    Refresh {
        resource_kind: TwitchPlaybackResourceKind,
        resource_id: String,
        quality_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential_owner_id: Option<UserId>,
        provider_instance_name: Option<String>,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackYoutubeMedia {
    Refresh {
        video_id: String,
        resource: YoutubePlaybackResource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential_owner_id: Option<UserId>,
        provider_instance_name: Option<String>,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackDouyinMedia {
    Refresh {
        resource: DouyinPlaybackResource,
        variant_key: String,
        root_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential_owner_id: Option<UserId>,
        provider_instance_name: Option<String>,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackTikTokMedia {
    Refresh {
        resource: TikTokPlaybackResource,
        variant_key: String,
        root_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential_owner_id: Option<UserId>,
        provider_instance_name: Option<String>,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TikTokPlaybackResource {
    Video { video_id: String },
    Live { unique_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DouyinPlaybackResource {
    Video { aweme_id: String },
    Live { web_rid: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum YoutubePlaybackResource {
    Format { itag: u32 },
    HlsManifest,
    DashManifest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TwitchPlaybackResourceKind {
    Channel,
    Video,
    Clip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackAlistMedia {
    Direct {
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
        locator: AlistPlaybackLocator,
        resource: AlistPlaybackMediaLocator,
    },
    ProxyFile {
        version: String,
        expires_at: i64,
        mode_name: String,
        url_index: usize,
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
    ProxyTranscodedHlsManifest {
        version: String,
        expires_at: i64,
        mode_name: String,
        url_index: usize,
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackBilibiliMedia {
    Direct {
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
    DirectDashManifest {
        version: String,
        expires_at: i64,
        mode_name: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
    /// A SyncTV-generated HLS manifest whose Bilibili media segments are
    /// fetched by the client directly.
    DirectDurlManifest {
        version: String,
        expires_at: i64,
        mode_name: String,
        segments: Vec<BilibiliDurlSegment>,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
    /// A SyncTV-generated HLS manifest whose media segments are forwarded by
    /// the server so backup CDN candidates remain available.
    DurlManifest {
        version: String,
        expires_at: i64,
        mode_name: String,
        segments: Vec<BilibiliDurlSegment>,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
    ProxyMediaStream {
        version: String,
        expires_at: i64,
        mode_name: String,
        url_index: usize,
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
    ProxyHlsManifest {
        version: String,
        expires_at: i64,
        mode_name: String,
        url_index: usize,
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
    ProxyDashManifest {
        version: String,
        expires_at: i64,
        mode_name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliDurlSegment {
    pub url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backup_urls: Vec<String>,
    pub duration_millis: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackDirectUrlMedia {
    Direct {
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
    ProxyStream {
        version: String,
        expires_at: i64,
        mode_name: String,
        url_index: usize,
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
    ProxyHlsManifest {
        version: String,
        expires_at: i64,
        mode_name: String,
        url_index: usize,
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
    ProxyDashManifest {
        version: String,
        expires_at: i64,
        mode_name: String,
        url_index: usize,
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackEmbyMedia {
    Direct {
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
    ProxyMediaStream {
        version: String,
        expires_at: i64,
        mode_name: String,
        url_index: usize,
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
    ProxyHlsManifest {
        version: String,
        expires_at: i64,
        mode_name: String,
        url_index: usize,
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackRtmpMedia {
    FlvStream {
        version: String,
        expires_at: i64,
        room_id: RoomId,
        media_id: MediaId,
    },
    HlsMaster {
        version: String,
        expires_at: i64,
        room_id: RoomId,
        media_id: MediaId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackLiveProxyMedia {
    FlvStream {
        version: String,
        expires_at: i64,
        room_id: RoomId,
        media_id: MediaId,
    },
    HlsMaster {
        version: String,
        expires_at: i64,
        room_id: RoomId,
        media_id: MediaId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSubtitle {
    pub name: String,
    pub language: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
    /// Provider-generated identity for one immutable subtitle document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p2p_swarm_id: Option<String>,
    #[serde(flatten)]
    pub provider: PlaybackSubtitleProvider,
}

impl PlaybackSubtitle {
    #[must_use]
    pub fn with_p2p_swarm_id(mut self, swarm_id: String) -> Self {
        self.p2p_swarm_id = Some(swarm_id);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "provider",
    content = "subtitle",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackSubtitleProvider {
    Alist(PlaybackAlistSubtitle),
    Bilibili(PlaybackBilibiliSubtitle),
    Cloudreve(PlaybackCloudreveSubtitle),
    DirectUrl(PlaybackDirectUrlSubtitle),
    Emby(PlaybackEmbySubtitle),
    Fnos(PlaybackFnosSubtitle),
    Qnap(PlaybackQnapSubtitle),
    Synology(PlaybackSynologySubtitle),
    Nextcloud(PlaybackNextcloudSubtitle),
    Seafile(PlaybackSeafileSubtitle),
    TrueNas(PlaybackTrueNasSubtitle),
    Youtube(PlaybackYoutubeSubtitle),
    TikTok(PlaybackTikTokSubtitle),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackYoutubeSubtitle {
    Refresh {
        video_id: String,
        track_id: String,
        target_language_code: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential_owner_id: Option<UserId>,
        provider_instance_name: Option<String>,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        subtitle_index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackTikTokSubtitle {
    Refresh {
        resource: TikTokPlaybackResource,
        language: String,
        format: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential_owner_id: Option<UserId>,
        provider_instance_name: Option<String>,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        subtitle_index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackCloudreveSubtitle {
    Direct {
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expire_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        subtitle_index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackAlistSubtitle {
    Refresh {
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<i64>,
        locator: AlistPlaybackLocator,
        resource: AlistPlaybackSubtitleLocator,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        subtitle_index: usize,
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackBilibiliSubtitle {
    Direct {
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expire_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        subtitle_index: usize,
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackDirectUrlSubtitle {
    Direct {
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expire_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        subtitle_index: usize,
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackEmbySubtitle {
    Direct {
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expire_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        subtitle_index: usize,
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackFnosSubtitle {
    Direct {
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expire_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        subtitle_index: usize,
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackQnapSubtitle {
    pub version: String,
    pub expires_at: i64,
    pub mode_name: String,
    pub subtitle_index: usize,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackNextcloudSubtitle {
    pub version: String,
    pub expires_at: i64,
    pub mode_name: String,
    pub subtitle_index: usize,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSeafileSubtitle {
    pub version: String,
    pub expires_at: i64,
    pub mode_name: String,
    pub subtitle_index: usize,
    pub repository_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackTrueNasSubtitle {
    pub version: String,
    pub expires_at: i64,
    pub mode_name: String,
    pub subtitle_index: usize,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackSynologySubtitle {
    File {
        version: String,
        expires_at: i64,
        mode_name: String,
        subtitle_index: usize,
        credential_owner_id: String,
        server_id: String,
        path: String,
    },
    VideoStation {
        version: String,
        expires_at: i64,
        mode_name: String,
        subtitle_index: usize,
        credential_owner_id: String,
        server_id: String,
        file_id: i64,
        subtitle_id: String,
        preview: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackDanmaku {
    pub name: String,
    pub format: Option<String>,
    /// Provider-generated identity for one immutable danmaku document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p2p_swarm_id: Option<String>,
    #[serde(flatten)]
    pub provider: PlaybackDanmakuProvider,
}

impl PlaybackDanmaku {
    #[must_use]
    pub fn with_p2p_swarm_id(mut self, swarm_id: String) -> Self {
        self.p2p_swarm_id = Some(swarm_id);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "provider",
    content = "danmaku",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackDanmakuProvider {
    Bilibili(PlaybackBilibiliDanmaku),
    DirectUrl(PlaybackDirectUrlDanmaku),
    Twitch(PlaybackTwitchDanmaku),
    Douyin(PlaybackDouyinDanmaku),
    Huya(PlaybackHuyaDanmaku),
    Douyu(PlaybackDouyuDanmaku),
    AcFun(PlaybackAcFunDanmaku),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackAcFunDanmaku {
    FileRefresh {
        media_index: usize,
    },
    LiveRefresh {
        media_index: usize,
    },
    FileProxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
    LiveProxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackDouyuDanmaku {
    Refresh {
        media_index: usize,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackHuyaDanmaku {
    Refresh {
        media_index: usize,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackTwitchDanmaku {
    Refresh {
        media_index: usize,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackDouyinDanmaku {
    Refresh {
        media_index: usize,
    },
    Proxy {
        version: String,
        expires_at: i64,
        mode_name: String,
        media_index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackDirectUrlDanmaku {
    pub url: String,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub headers: std::collections::HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackBilibiliDanmaku {
    FileDirect {
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expire_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    FileProxy {
        version: String,
        expires_at: i64,
        danmaku_index: usize,
        url: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        headers: std::collections::HashMap<String, String>,
    },
    Live {
        room_id: RoomId,
        media_id: MediaId,
    },
}

/// Media-level metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackMediaMetadata {
    /// Resolution (e.g., "1920x1080", "1280x720")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,

    /// Bitrate in bps
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<i64>,

    /// Video codec (e.g., "avc", "hevc", "av1")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,

    /// Frame rate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackMetadata {
    Alist(AlistPlaybackMetadata),
    Bilibili(BilibiliPlaybackMetadata),
    Emby(EmbyPlaybackMetadata),
    DirectUrl(DirectUrlPlaybackMetadata),
    LiveProxy(LiveProxyPlaybackMetadata),
    Live(LivePlaybackMetadata),
    Twitch(TwitchPlaybackMetadata),
    Youtube(YoutubePlaybackMetadata),
    Douyin(DouyinPlaybackMetadata),
    TikTok(TikTokPlaybackMetadata),
    Huya(HuyaPlaybackMetadata),
    Douyu(DouyuPlaybackMetadata),
    AcFun(AcFunPlaybackMetadata),
    Cctv(CctvPlaybackMetadata),
    Fnos(FnosPlaybackMetadata),
    Qnap(QnapPlaybackMetadata),
    Synology(SynologyPlaybackMetadata),
    Nextcloud(NextcloudPlaybackMetadata),
    Seafile(SeafilePlaybackMetadata),
    TrueNas(TrueNasPlaybackMetadata),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubePlaybackMetadata {
    pub video_id: String,
    pub channel_id: String,
    pub channel_name: String,
    pub description: String,
    pub view_count: Option<u64>,
    pub publish_date: Option<String>,
    pub upload_date: Option<String>,
    pub category: Option<String>,
    pub is_live: bool,
    pub is_currently_live: Option<bool>,
    pub live_start: Option<String>,
    pub live_end: Option<String>,
    pub storyboard_spec: Option<String>,
    pub automatic_caption_count: usize,
    pub manual_caption_count: usize,
    pub translation_languages: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DouyinPlaybackKind {
    Video,
    Live,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DouyinPlaybackMetadata {
    pub id: String,
    pub kind: DouyinPlaybackKind,
    pub author_id: String,
    pub author_sec_uid: String,
    pub author_name: String,
    pub description: String,
    pub view_count: Option<u64>,
    pub like_count: Option<u64>,
    pub comment_count: Option<u64>,
    pub share_count: Option<u64>,
    pub collect_count: Option<u64>,
    pub created_at: Option<i64>,
    pub music_title: Option<String>,
    pub music_author: Option<String>,
    pub is_live: bool,
    pub is_currently_live: Option<bool>,
    pub room_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TikTokPlaybackKind {
    Video,
    Live,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TikTokPlaybackMetadata {
    pub id: String,
    pub kind: TikTokPlaybackKind,
    pub author_id: String,
    pub author_sec_uid: String,
    pub author_unique_id: String,
    pub author_name: String,
    pub description: String,
    pub view_count: Option<u64>,
    pub like_count: Option<u64>,
    pub comment_count: Option<u64>,
    pub share_count: Option<u64>,
    pub collect_count: Option<u64>,
    pub concurrent_viewers: Option<u64>,
    pub created_at: Option<i64>,
    pub music_title: Option<String>,
    pub music_author: Option<String>,
    pub subtitle_count: usize,
    pub is_live: bool,
    pub is_currently_live: Option<bool>,
    pub room_id: Option<String>,
}

impl PlaybackMetadata {
    #[must_use]
    pub const fn as_bilibili(&self) -> Option<&BilibiliPlaybackMetadata> {
        match self {
            Self::Bilibili(metadata) => Some(metadata),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_bilibili_mut(&mut self) -> Option<&mut BilibiliPlaybackMetadata> {
        match self {
            Self::Bilibili(metadata) => Some(metadata),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_emby(&self) -> Option<&EmbyPlaybackMetadata> {
        match self {
            Self::Emby(metadata) => Some(metadata),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_live(&self) -> Option<&LivePlaybackMetadata> {
        match self {
            Self::Live(metadata) => Some(metadata),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlistPlaybackMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_subtitle_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_preview_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transcoding_tasks: Vec<AlistTranscodingTaskMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_preview: Option<AlistVideoPreviewMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlistTranscodingTaskMetadata {
    pub mode_name: String,
    pub template_id: String,
    pub template_name: String,
    pub template_width: u64,
    pub template_height: u64,
    pub stage: String,
    pub status: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlistVideoPreviewMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drive_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub transcoding_count: usize,
    pub subtitle_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliPlaybackMetadata {
    pub kind: BilibiliPlaybackKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bvid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aid: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epid: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cid: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_buffer_time: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_started_at: Option<i64>,
    /// Whether this provider resource uses live playback semantics.
    #[serde(default)]
    pub is_live: bool,
    /// Current upstream state when the provider could determine it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_currently_live: Option<bool>,
    #[serde(default, skip_serializing_if = "BilibiliDashManifests::is_empty")]
    pub dash_manifests: BilibiliDashManifests,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BilibiliPlaybackKind {
    Video,
    Pgc,
    Live,
}

impl BilibiliPlaybackMetadata {
    #[must_use]
    pub fn new(kind: BilibiliPlaybackKind) -> Self {
        Self {
            kind,
            bvid: None,
            aid: None,
            epid: None,
            cid: None,
            min_buffer_time: None,
            fallback_format: None,
            quality: None,
            room_id: None,
            live_started_at: None,
            is_live: false,
            is_currently_live: None,
            dash_manifests: BilibiliDashManifests::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliDashManifests {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dash: Option<BilibiliDashManifest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hevc: Option<BilibiliDashManifest>,
}

impl BilibiliDashManifests {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.dash.is_none() && self.hevc.is_none()
    }

    pub fn set(&mut self, mode: BilibiliDashManifestSlot, manifest: BilibiliDashManifest) {
        match mode {
            BilibiliDashManifestSlot::Dash => self.dash = Some(manifest),
            BilibiliDashManifestSlot::Hevc => self.hevc = Some(manifest),
        }
    }

    #[must_use]
    pub const fn get(&self, mode: BilibiliDashManifestSlot) -> Option<&BilibiliDashManifest> {
        match mode {
            BilibiliDashManifestSlot::Dash => self.dash.as_ref(),
            BilibiliDashManifestSlot::Hevc => self.hevc.as_ref(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliDashManifest {
    pub duration: f64,
    pub min_buffer_time: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub video_streams: Vec<BilibiliDashVideoStream>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_streams: Vec<BilibiliDashAudioStream>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliDashVideoStream {
    pub id: u64,
    pub quality_name: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backup_urls: Vec<String>,
    pub mime_type: String,
    pub codecs: String,
    pub width: u64,
    pub height: u64,
    pub frame_rate: String,
    pub bandwidth: u64,
    pub codecid: u32,
    pub sar: String,
    pub start_with_sap: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_base: Option<BilibiliDashSegmentBase>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliDashAudioStream {
    pub id: u64,
    pub quality_name: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backup_urls: Vec<String>,
    pub mime_type: String,
    pub codecs: String,
    pub bandwidth: u64,
    pub start_with_sap: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_base: Option<BilibiliDashSegmentBase>,
    pub audio_sampling_rate: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliDashSegmentBase {
    pub index_range: String,
    pub initialization_range: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BilibiliDashManifestSlot {
    Dash,
    Hevc,
}

impl BilibiliDashManifestSlot {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "dash" => Some(Self::Dash),
            "hevc" => Some(Self::Hevc),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dash => "dash",
            Self::Hevc => "hevc",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbyPlaybackMetadata {
    pub kind: EmbyPlaybackKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play_session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EmbyPlaybackKind {
    Movie,
    Episode,
    Video,
    Audio,
    MusicAlbum,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectUrlPlaybackMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveProxyPlaybackMetadata {
    pub media_id: MediaId,
    pub room_id: RoomId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_host: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LivePlaybackMetadata {
    pub media_id: MediaId,
    pub room_id: RoomId,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwitchPlaybackMetadata {
    pub resource_id: String,
    pub title: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chapters: Vec<TwitchChapterMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storyboard_url: Option<String>,
    /// Whether this provider resource uses live playback semantics.
    #[serde(default)]
    pub is_live: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_currently_live: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwitchChapterMetadata {
    pub title: String,
    pub start_seconds: u64,
    pub end_seconds: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HuyaPlaybackMetadata {
    pub resource_id: String,
    pub title: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub like_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<i64>,
    #[serde(default)]
    pub is_live: bool,
    pub is_currently_live: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DouyuPlaybackMetadata {
    pub room_id: String,
    pub title: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    pub is_replay: bool,
    pub is_vip: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default)]
    pub is_live: bool,
    pub is_currently_live: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcFunPlaybackMetadata {
    pub resource_id: String,
    pub title: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub like_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub is_live: bool,
    pub is_currently_live: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CctvPlaybackMetadata {
    pub video_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uploader: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<i64>,
    pub protected: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chapters: Vec<CctvChapterMetadata>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CctvChapterMetadata {
    pub id: String,
    pub title: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FnosPlaybackMetadata {
    File(FnosFilePlaybackMetadata),
    Media(FnosMediaPlaybackMetadata),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FnosFilePlaybackMetadata {
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FnosMediaPlaybackMetadata {
    pub item_guid: String,
    pub media_guid: String,
    pub title: String,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub video_codec: Option<String>,
    pub video_profile: Option<String>,
    pub bit_depth: Option<u32>,
    pub dolby_vision_profile: Option<i32>,
    pub frame_rate: Option<String>,
    pub season_number: Option<u32>,
    pub episode_number: Option<u32>,
    pub progress_seconds: u64,
    pub duration_seconds: u64,
    pub watched: bool,
    pub audio_tracks: Vec<FnosAudioTrackMetadata>,
    pub subtitle_tracks: Vec<FnosSubtitleTrackMetadata>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FnosAudioTrackMetadata {
    pub guid: Option<String>,
    pub title: Option<String>,
    pub language: Option<String>,
    pub codec: Option<String>,
    pub channels: u32,
    pub bitrate: u64,
    pub default: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FnosSubtitleTrackMetadata {
    pub guid: Option<String>,
    pub title: Option<String>,
    pub language: Option<String>,
    pub codec: Option<String>,
    pub format: Option<String>,
    pub external: bool,
    pub default: bool,
    pub forced: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QnapPlaybackMetadata {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub modified_at: u64,
    pub file_type: u64,
    pub realtime_transcode: bool,
    pub hardware_transcode: bool,
    pub multimedia_codec: bool,
    #[serde(default)]
    pub pre_transcoded_heights: Vec<u32>,
    #[serde(default)]
    pub realtime_heights: Vec<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextcloudPlaybackMetadata {
    pub file_id: u64,
    pub name: String,
    pub path: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_display_name: Option<String>,
    pub favorite: bool,
    pub has_preview: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blurhash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_millis: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeafilePlaybackMetadata {
    pub repository_id: String,
    pub object_id: String,
    pub name: String,
    pub path: String,
    pub size: u64,
    pub modified_at: String,
    pub is_locked: bool,
    pub can_preview: bool,
    pub can_edit: bool,
    pub has_thumbnail: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrueNasPlaybackMetadata {
    pub realpath: String,
    pub size: u64,
    pub allocation_size: u64,
    pub mode: u32,
    pub mount_id: u64,
    pub uid: u32,
    pub gid: u32,
    pub atime: f64,
    pub mtime: f64,
    pub ctime: f64,
    pub btime: f64,
    pub dev: u64,
    pub inode: u64,
    pub nlink: u64,
    pub acl: bool,
    pub is_mountpoint: bool,
    pub is_ctldir: bool,
    #[serde(default)]
    pub attributes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynologyPlaybackMetadata {
    pub title: String,
    pub summary: String,
    pub tagline: String,
    pub certificate: String,
    pub rating: i32,
    pub actors: Vec<String>,
    pub directors: Vec<String>,
    pub writers: Vec<String>,
    pub genres: Vec<String>,
    pub item_id: i64,
    pub file_id: i64,
    pub kind: SynologyLibraryItemKind,
    pub path: String,
    pub size: u64,
    pub duration_seconds: u64,
    pub progress_seconds: u64,
    pub width: u32,
    pub height: u32,
    pub video_codec: String,
    pub audio_codec: String,
    pub container: String,
    pub video_bitrate: u64,
    pub audio_bitrate: u64,
    pub frame_rate_numerator: u64,
    pub frame_rate_denominator: u64,
    pub audio_channels: u32,
    pub audio_frequency_hz: u32,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub watched: bool,
    pub watched_ratio: f64,
    pub parental_controlled: bool,
    pub create_time: i64,
    pub last_watched: i64,
    pub audio_tracks: Vec<SynologyAudioTrackMetadata>,
    pub subtitles: Vec<SynologySubtitleMetadata>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynologyAudioTrackMetadata {
    pub id: i64,
    pub language: String,
    pub codec: String,
    pub channels: u32,
    pub bitrate: u64,
    pub default: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynologySubtitleMetadata {
    pub id: String,
    pub language: String,
    pub title: String,
    pub format: String,
    pub embedded: bool,
}

// Helper implementations

impl PlaybackResult {
    /// Create a `PlaybackResult` from Media and single mode `PlaybackInfo`
    #[must_use]
    pub fn from_media_single_mode(
        media: &Media,
        mode_name: &str,
        playback_info: PlaybackInfo,
    ) -> Self {
        let mut playback_infos = std::collections::HashMap::new();
        playback_infos.insert(mode_name.to_string(), playback_info);

        Self {
            id: Some(media.id),
            playlist_id: media.playlist_id,
            room_id: media.room_id,
            name: media.name.clone(),
            provider: media.source_provider,
            provider_instance_name: media.provider_instance_name.clone(),
            position: media.position,
            playback_infos,
            default_mode: mode_name.to_string(),
            duration_seconds: None,
            playback_kind: PlaybackKind::Regular,
            target: None,
            metadata: None,
        }
    }

    /// Create a new builder
    #[must_use]
    pub fn builder(
        playlist_id: Option<PlaylistId>,
        room_id: RoomId,
        name: String,
        position: f64,
    ) -> PlaybackResultBuilder {
        PlaybackResultBuilder {
            id: None,
            playlist_id,
            room_id,
            name,
            provider: None,
            provider_instance_name: None,
            position,
            playback_infos: indexmap::IndexMap::new(),
            default_mode: None,
            duration_seconds: None,
            playback_kind: PlaybackKind::Regular,
            target: None,
            metadata: None,
        }
    }

    /// Replace metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: PlaybackMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Get the default playback info
    #[must_use]
    pub fn get_default_playback_info(&self) -> Option<&PlaybackInfo> {
        self.playback_infos.get(&self.default_mode)
    }
}

/// Builder for `PlaybackResult`
pub struct PlaybackResultBuilder {
    id: Option<MediaId>,
    playlist_id: Option<PlaylistId>,
    room_id: RoomId,
    name: String,
    provider: Option<SourceProvider>,
    provider_instance_name: Option<String>,
    position: f64,
    /// Uses `IndexMap` to guarantee insertion-order determinism when falling
    /// back to the first mode as default (avoids `HashMap::keys().next()`
    /// non-determinism).
    playback_infos: indexmap::IndexMap<String, PlaybackInfo>,
    default_mode: Option<String>,
    duration_seconds: Option<f64>,
    playback_kind: PlaybackKind,
    target: Option<ProviderTarget>,
    metadata: Option<PlaybackMetadata>,
}

impl PlaybackResultBuilder {
    /// Set media ID (optional)
    #[must_use]
    pub fn id(mut self, id: MediaId) -> Self {
        self.id = Some(id);
        self
    }

    /// Add a playback mode
    #[must_use]
    pub fn add_mode(mut self, mode_name: String, info: PlaybackInfo) -> Self {
        self.playback_infos.insert(mode_name, info);
        self
    }

    #[must_use]
    pub fn provider(mut self, provider: SourceProvider) -> Self {
        self.provider = Some(provider);
        self
    }

    #[must_use]
    pub fn provider_instance_name(mut self, provider_instance_name: Option<String>) -> Self {
        self.provider_instance_name = provider_instance_name;
        self
    }

    /// Set the default mode
    #[must_use]
    pub fn default_mode(mut self, mode_name: String) -> Self {
        self.default_mode = Some(mode_name);
        self
    }

    #[must_use]
    pub const fn duration_seconds(mut self, duration_seconds: Option<f64>) -> Self {
        self.duration_seconds = duration_seconds;
        self
    }

    #[must_use]
    pub const fn playback_kind(mut self, playback_kind: PlaybackKind) -> Self {
        self.playback_kind = playback_kind;
        self
    }

    #[must_use]
    pub fn target(mut self, target: ProviderTarget) -> Self {
        self.target = Some(target);
        self
    }

    /// Replace metadata.
    #[must_use]
    pub fn metadata(mut self, metadata: Option<PlaybackMetadata>) -> Self {
        self.metadata = metadata;
        self
    }

    /// Build the `PlaybackResult`
    ///
    /// Returns None if no modes were added or `default_mode` is not set.
    /// When no explicit `default_mode` is set, the first inserted mode is used
    /// (deterministic because `IndexMap` preserves insertion order).
    #[must_use]
    pub fn build(self) -> Option<PlaybackResult> {
        if self.playback_infos.is_empty() {
            return None;
        }

        let default_mode = self
            .default_mode
            .or_else(|| self.playback_infos.keys().next().cloned())?;

        if !self.playback_infos.contains_key(&default_mode) {
            return None;
        }

        Some(PlaybackResult {
            id: self.id,
            playlist_id: self.playlist_id,
            room_id: self.room_id,
            name: self.name,
            provider: self.provider?,
            provider_instance_name: self.provider_instance_name,
            position: self.position,
            playback_infos: self.playback_infos.into_iter().collect(),
            default_mode,
            duration_seconds: self.duration_seconds,
            playback_kind: self.playback_kind,
            target: self.target,
            metadata: self.metadata,
        })
    }
}

impl PlaybackInfo {
    /// Create a new builder
    #[must_use]
    pub fn builder() -> PlaybackInfoBuilder {
        PlaybackInfoBuilder::default()
    }
}

/// Builder for `PlaybackInfo`
#[derive(Default)]
pub struct PlaybackInfoBuilder {
    thumbnail: Option<String>,
    medias: Vec<PlaybackMedia>,
    default_media_index: Option<usize>,
    subtitles: Vec<PlaybackSubtitle>,
    default_subtitle_index: Option<usize>,
    danmakus: Vec<PlaybackDanmaku>,
    default_danmaku_index: Option<usize>,
}

impl PlaybackInfoBuilder {
    /// Add a playback media
    #[must_use]
    pub fn add_media(mut self, media: PlaybackMedia) -> Self {
        self.medias.push(media);
        self
    }

    #[must_use]
    pub fn thumbnail(mut self, thumbnail: Option<String>) -> Self {
        self.thumbnail = thumbnail;
        self
    }

    /// Set the default media index
    #[must_use]
    pub const fn default_media_index(mut self, index: usize) -> Self {
        self.default_media_index = Some(index);
        self
    }

    /// Add a subtitle
    #[must_use]
    pub fn add_subtitle(mut self, subtitle: PlaybackSubtitle) -> Self {
        self.subtitles.push(subtitle);
        self
    }

    /// Set the default subtitle index
    #[must_use]
    pub const fn default_subtitle_index(mut self, index: usize) -> Self {
        self.default_subtitle_index = Some(index);
        self
    }

    /// Add a danmaku source
    #[must_use]
    pub fn add_danmaku(mut self, danmaku: PlaybackDanmaku) -> Self {
        self.danmakus.push(danmaku);
        self
    }

    /// Set the default danmaku index
    #[must_use]
    pub const fn default_danmaku_index(mut self, index: usize) -> Self {
        self.default_danmaku_index = Some(index);
        self
    }

    /// Build the `PlaybackInfo`
    #[must_use]
    pub fn build(self) -> PlaybackInfo {
        PlaybackInfo {
            thumbnail: self.thumbnail,
            medias: self.medias,
            default_media_index: self.default_media_index,
            subtitles: self.subtitles,
            default_subtitle_index: self.default_subtitle_index,
            danmakus: self.danmakus,
            default_danmaku_index: self.default_danmaku_index,
        }
    }
}

impl PlaybackMedia {
    #[must_use]
    pub fn direct_url(&self) -> Option<&str> {
        match &self.provider {
            PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::Direct { url, .. })
            | PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct { url, .. }) => {
                Some(url)
            }
            PlaybackMediaProvider::Alist(PlaybackAlistMedia::Direct { url, .. })
            | PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::Direct { url, .. })
            | PlaybackMediaProvider::Emby(PlaybackEmbyMedia::Direct { url, .. }) => Some(url),
            _ => None,
        }
    }

    #[must_use]
    pub fn upstream_url(&self) -> Option<&str> {
        match &self.provider {
            PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::Direct { url, .. }) => {
                Some(url)
            }
            PlaybackMediaProvider::Alist(
                PlaybackAlistMedia::Direct { url, .. }
                | PlaybackAlistMedia::ProxyFile { url, .. }
                | PlaybackAlistMedia::ProxyTranscodedHlsManifest { url, .. },
            )
            | PlaybackMediaProvider::Bilibili(
                PlaybackBilibiliMedia::Direct { url, .. }
                | PlaybackBilibiliMedia::ProxyMediaStream { url, .. }
                | PlaybackBilibiliMedia::ProxyHlsManifest { url, .. },
            )
            | PlaybackMediaProvider::DirectUrl(
                PlaybackDirectUrlMedia::Direct { url, .. }
                | PlaybackDirectUrlMedia::ProxyStream { url, .. }
                | PlaybackDirectUrlMedia::ProxyHlsManifest { url, .. }
                | PlaybackDirectUrlMedia::ProxyDashManifest { url, .. },
            )
            | PlaybackMediaProvider::Emby(
                PlaybackEmbyMedia::Direct { url, .. }
                | PlaybackEmbyMedia::ProxyMediaStream { url, .. }
                | PlaybackEmbyMedia::ProxyHlsManifest { url, .. },
            )
            | PlaybackMediaProvider::Fnos(PlaybackFnosMedia::Direct { url, .. })
            | PlaybackMediaProvider::Qnap(PlaybackQnapMedia::Direct { url, .. })
            | PlaybackMediaProvider::Synology(PlaybackSynologyMedia::Direct { url, .. })
            | PlaybackMediaProvider::Nextcloud(PlaybackNextcloudMedia::Direct { url, .. })
            | PlaybackMediaProvider::Seafile(PlaybackSeafileMedia::Direct { url, .. })
            | PlaybackMediaProvider::TrueNas(PlaybackTrueNasMedia::Direct { url, .. }) => Some(url),
            _ => None,
        }
    }

    #[must_use]
    pub fn upstream_headers(&self) -> std::collections::HashMap<String, String> {
        match &self.provider {
            PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::Direct {
                headers, ..
            })
            | PlaybackMediaProvider::Alist(
                PlaybackAlistMedia::Direct { headers, .. }
                | PlaybackAlistMedia::ProxyFile { headers, .. }
                | PlaybackAlistMedia::ProxyTranscodedHlsManifest { headers, .. },
            )
            | PlaybackMediaProvider::Bilibili(
                PlaybackBilibiliMedia::Direct { headers, .. }
                | PlaybackBilibiliMedia::DirectDashManifest { headers, .. }
                | PlaybackBilibiliMedia::DirectDurlManifest { headers, .. }
                | PlaybackBilibiliMedia::DurlManifest { headers, .. }
                | PlaybackBilibiliMedia::ProxyMediaStream { headers, .. }
                | PlaybackBilibiliMedia::ProxyHlsManifest { headers, .. },
            )
            | PlaybackMediaProvider::DirectUrl(
                PlaybackDirectUrlMedia::Direct { headers, .. }
                | PlaybackDirectUrlMedia::ProxyStream { headers, .. }
                | PlaybackDirectUrlMedia::ProxyHlsManifest { headers, .. }
                | PlaybackDirectUrlMedia::ProxyDashManifest { headers, .. },
            )
            | PlaybackMediaProvider::Emby(
                PlaybackEmbyMedia::Direct { headers, .. }
                | PlaybackEmbyMedia::ProxyMediaStream { headers, .. }
                | PlaybackEmbyMedia::ProxyHlsManifest { headers, .. },
            )
            | PlaybackMediaProvider::Fnos(PlaybackFnosMedia::Direct { headers, .. })
            | PlaybackMediaProvider::Qnap(PlaybackQnapMedia::Direct { headers, .. })
            | PlaybackMediaProvider::Synology(PlaybackSynologyMedia::Direct { headers, .. })
            | PlaybackMediaProvider::Nextcloud(PlaybackNextcloudMedia::Direct {
                headers, ..
            })
            | PlaybackMediaProvider::Seafile(PlaybackSeafileMedia::Direct { headers, .. })
            | PlaybackMediaProvider::TrueNas(PlaybackTrueNasMedia::Direct { headers, .. }) => {
                headers.clone()
            }
            _ => std::collections::HashMap::new(),
        }
    }

    #[must_use]
    pub fn requires_provider_url(&self) -> bool {
        !matches!(
            self.provider,
            PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::Direct { .. })
                | PlaybackMediaProvider::Alist(PlaybackAlistMedia::Direct { .. })
                | PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::Direct { .. })
                | PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct { .. })
                | PlaybackMediaProvider::Emby(PlaybackEmbyMedia::Direct { .. })
                | PlaybackMediaProvider::Fnos(PlaybackFnosMedia::Direct { .. })
                | PlaybackMediaProvider::Qnap(PlaybackQnapMedia::Direct { .. })
                | PlaybackMediaProvider::Synology(PlaybackSynologyMedia::Direct { .. })
                | PlaybackMediaProvider::Nextcloud(PlaybackNextcloudMedia::Direct { .. })
                | PlaybackMediaProvider::Seafile(PlaybackSeafileMedia::Direct { .. })
                | PlaybackMediaProvider::TrueNas(PlaybackTrueNasMedia::Direct { .. })
        )
    }
}

impl PlaybackSubtitle {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    #[must_use]
    pub fn format(&self) -> &str {
        &self.format
    }

    #[must_use]
    pub fn expiration_timestamp(&self) -> Option<i64> {
        match &self.provider {
            PlaybackSubtitleProvider::Cloudreve(PlaybackCloudreveSubtitle::Direct {
                expire_at,
                ..
            }) => expire_at.map(|value| value.timestamp()),
            PlaybackSubtitleProvider::Alist(PlaybackAlistSubtitle::Refresh {
                expires_at, ..
            }) => *expires_at,
            PlaybackSubtitleProvider::Bilibili(PlaybackBilibiliSubtitle::Direct {
                expire_at,
                ..
            })
            | PlaybackSubtitleProvider::DirectUrl(PlaybackDirectUrlSubtitle::Direct {
                expire_at,
                ..
            })
            | PlaybackSubtitleProvider::Emby(PlaybackEmbySubtitle::Direct { expire_at, .. })
            | PlaybackSubtitleProvider::Fnos(PlaybackFnosSubtitle::Direct { expire_at, .. }) => {
                expire_at.map(|value| value.timestamp())
            }
            PlaybackSubtitleProvider::Alist(PlaybackAlistSubtitle::Proxy {
                expires_at, ..
            })
            | PlaybackSubtitleProvider::Bilibili(PlaybackBilibiliSubtitle::Proxy {
                expires_at,
                ..
            })
            | PlaybackSubtitleProvider::DirectUrl(PlaybackDirectUrlSubtitle::Proxy {
                expires_at,
                ..
            })
            | PlaybackSubtitleProvider::Emby(PlaybackEmbySubtitle::Proxy { expires_at, .. })
            | PlaybackSubtitleProvider::Fnos(PlaybackFnosSubtitle::Proxy { expires_at, .. })
            | PlaybackSubtitleProvider::Qnap(PlaybackQnapSubtitle { expires_at, .. })
            | PlaybackSubtitleProvider::Nextcloud(PlaybackNextcloudSubtitle {
                expires_at, ..
            })
            | PlaybackSubtitleProvider::Seafile(PlaybackSeafileSubtitle { expires_at, .. })
            | PlaybackSubtitleProvider::TrueNas(PlaybackTrueNasSubtitle { expires_at, .. })
            | PlaybackSubtitleProvider::Synology(
                PlaybackSynologySubtitle::File { expires_at, .. }
                | PlaybackSynologySubtitle::VideoStation { expires_at, .. },
            )
            | PlaybackSubtitleProvider::Youtube(PlaybackYoutubeSubtitle::Proxy {
                expires_at,
                ..
            })
            | PlaybackSubtitleProvider::TikTok(PlaybackTikTokSubtitle::Proxy {
                expires_at, ..
            })
            | PlaybackSubtitleProvider::Cloudreve(PlaybackCloudreveSubtitle::Proxy {
                expires_at,
                ..
            }) => Some(*expires_at),
            PlaybackSubtitleProvider::Youtube(PlaybackYoutubeSubtitle::Refresh { .. })
            | PlaybackSubtitleProvider::TikTok(PlaybackTikTokSubtitle::Refresh { .. }) => None,
        }
    }

    #[must_use]
    pub fn upstream_url(&self) -> &str {
        match &self.provider {
            PlaybackSubtitleProvider::Cloudreve(PlaybackCloudreveSubtitle::Direct {
                url, ..
            })
            | PlaybackSubtitleProvider::Alist(
                PlaybackAlistSubtitle::Refresh { url, .. }
                | PlaybackAlistSubtitle::Proxy { url, .. },
            )
            | PlaybackSubtitleProvider::Bilibili(
                PlaybackBilibiliSubtitle::Direct { url, .. }
                | PlaybackBilibiliSubtitle::Proxy { url, .. },
            )
            | PlaybackSubtitleProvider::DirectUrl(
                PlaybackDirectUrlSubtitle::Direct { url, .. }
                | PlaybackDirectUrlSubtitle::Proxy { url, .. },
            )
            | PlaybackSubtitleProvider::Emby(
                PlaybackEmbySubtitle::Direct { url, .. } | PlaybackEmbySubtitle::Proxy { url, .. },
            )
            | PlaybackSubtitleProvider::Fnos(
                PlaybackFnosSubtitle::Direct { url, .. } | PlaybackFnosSubtitle::Proxy { url, .. },
            ) => url,
            PlaybackSubtitleProvider::Qnap(subtitle) => &subtitle.path,
            PlaybackSubtitleProvider::Nextcloud(subtitle) => &subtitle.path,
            PlaybackSubtitleProvider::Seafile(subtitle) => &subtitle.path,
            PlaybackSubtitleProvider::TrueNas(subtitle) => &subtitle.path,
            PlaybackSubtitleProvider::Synology(PlaybackSynologySubtitle::File { path, .. }) => path,
            PlaybackSubtitleProvider::Synology(PlaybackSynologySubtitle::VideoStation {
                subtitle_id,
                ..
            }) => subtitle_id,
            PlaybackSubtitleProvider::Youtube(PlaybackYoutubeSubtitle::Refresh {
                video_id,
                ..
            }) => video_id,
            PlaybackSubtitleProvider::Youtube(PlaybackYoutubeSubtitle::Proxy {
                version, ..
            })
            | PlaybackSubtitleProvider::Cloudreve(PlaybackCloudreveSubtitle::Proxy {
                version,
                ..
            }) => version,
            PlaybackSubtitleProvider::TikTok(PlaybackTikTokSubtitle::Refresh {
                resource, ..
            }) => match resource {
                TikTokPlaybackResource::Video { video_id } => video_id,
                TikTokPlaybackResource::Live { unique_id } => unique_id,
            },
            PlaybackSubtitleProvider::TikTok(PlaybackTikTokSubtitle::Proxy { version, .. }) => {
                version
            }
        }
    }

    #[must_use]
    pub fn upstream_headers(&self) -> std::collections::HashMap<String, String> {
        match &self.provider {
            PlaybackSubtitleProvider::Cloudreve(PlaybackCloudreveSubtitle::Direct {
                headers,
                ..
            })
            | PlaybackSubtitleProvider::Alist(
                PlaybackAlistSubtitle::Refresh { headers, .. }
                | PlaybackAlistSubtitle::Proxy { headers, .. },
            )
            | PlaybackSubtitleProvider::Bilibili(
                PlaybackBilibiliSubtitle::Direct { headers, .. }
                | PlaybackBilibiliSubtitle::Proxy { headers, .. },
            )
            | PlaybackSubtitleProvider::DirectUrl(
                PlaybackDirectUrlSubtitle::Direct { headers, .. }
                | PlaybackDirectUrlSubtitle::Proxy { headers, .. },
            )
            | PlaybackSubtitleProvider::Emby(
                PlaybackEmbySubtitle::Direct { headers, .. }
                | PlaybackEmbySubtitle::Proxy { headers, .. },
            )
            | PlaybackSubtitleProvider::Fnos(
                PlaybackFnosSubtitle::Direct { headers, .. }
                | PlaybackFnosSubtitle::Proxy { headers, .. },
            ) => headers.clone(),
            PlaybackSubtitleProvider::Cloudreve(PlaybackCloudreveSubtitle::Proxy { .. })
            | PlaybackSubtitleProvider::Qnap(_)
            | PlaybackSubtitleProvider::Nextcloud(_)
            | PlaybackSubtitleProvider::Seafile(_)
            | PlaybackSubtitleProvider::TrueNas(_)
            | PlaybackSubtitleProvider::Synology(_)
            | PlaybackSubtitleProvider::Youtube(_)
            | PlaybackSubtitleProvider::TikTok(_) => std::collections::HashMap::new(),
        }
    }
}

impl PlaybackDanmaku {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn expiration_timestamp(&self) -> Option<i64> {
        match &self.provider {
            PlaybackDanmakuProvider::DirectUrl(danmaku) => {
                danmaku.expire_at.map(|value| value.timestamp())
            }
            PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::FileDirect {
                expire_at,
                ..
            }) => expire_at.map(|value| value.timestamp()),
            PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::FileProxy {
                expires_at,
                ..
            })
            | PlaybackDanmakuProvider::Twitch(PlaybackTwitchDanmaku::Proxy {
                expires_at, ..
            })
            | PlaybackDanmakuProvider::Douyin(PlaybackDouyinDanmaku::Proxy {
                expires_at, ..
            })
            | PlaybackDanmakuProvider::Huya(PlaybackHuyaDanmaku::Proxy { expires_at, .. })
            | PlaybackDanmakuProvider::Douyu(PlaybackDouyuDanmaku::Proxy { expires_at, .. })
            | PlaybackDanmakuProvider::AcFun(
                PlaybackAcFunDanmaku::FileProxy { expires_at, .. }
                | PlaybackAcFunDanmaku::LiveProxy { expires_at, .. },
            ) => Some(*expires_at),
            PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::Live { .. })
            | PlaybackDanmakuProvider::Twitch(PlaybackTwitchDanmaku::Refresh { .. })
            | PlaybackDanmakuProvider::Douyin(PlaybackDouyinDanmaku::Refresh { .. })
            | PlaybackDanmakuProvider::Huya(PlaybackHuyaDanmaku::Refresh { .. })
            | PlaybackDanmakuProvider::Douyu(PlaybackDouyuDanmaku::Refresh { .. })
            | PlaybackDanmakuProvider::AcFun(
                PlaybackAcFunDanmaku::FileRefresh { .. } | PlaybackAcFunDanmaku::LiveRefresh { .. },
            ) => None,
        }
    }

    #[must_use]
    pub fn upstream_url(&self) -> Option<&str> {
        match &self.provider {
            PlaybackDanmakuProvider::DirectUrl(danmaku) => Some(&danmaku.url),
            PlaybackDanmakuProvider::Bilibili(
                PlaybackBilibiliDanmaku::FileDirect { url, .. }
                | PlaybackBilibiliDanmaku::FileProxy { url, .. },
            ) => Some(url),
            PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::Live { .. })
            | PlaybackDanmakuProvider::Twitch(_)
            | PlaybackDanmakuProvider::Huya(_)
            | PlaybackDanmakuProvider::Douyu(_)
            | PlaybackDanmakuProvider::Douyin(_)
            | PlaybackDanmakuProvider::AcFun(_) => None,
        }
    }

    #[must_use]
    pub fn upstream_headers(&self) -> std::collections::HashMap<String, String> {
        match &self.provider {
            PlaybackDanmakuProvider::DirectUrl(danmaku) => danmaku.headers.clone(),
            PlaybackDanmakuProvider::Bilibili(
                PlaybackBilibiliDanmaku::FileDirect { headers, .. }
                | PlaybackBilibiliDanmaku::FileProxy { headers, .. },
            ) => headers.clone(),
            PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::Live { .. })
            | PlaybackDanmakuProvider::Twitch(_)
            | PlaybackDanmakuProvider::Huya(_)
            | PlaybackDanmakuProvider::Douyu(_)
            | PlaybackDanmakuProvider::Douyin(_)
            | PlaybackDanmakuProvider::AcFun(_) => std::collections::HashMap::new(),
        }
    }

    #[must_use]
    pub fn format(&self) -> Option<&str> {
        self.format.as_deref()
    }
}

impl PlaybackMediaMetadata {
    /// Create metadata with resolution and codec
    #[must_use]
    pub fn new(resolution: String, codec: String) -> Self {
        Self {
            resolution: Some(resolution),
            codec: Some(codec),
            bitrate: None,
            fps: None,
        }
    }
}
