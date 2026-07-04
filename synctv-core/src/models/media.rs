use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;

use super::file_storage::FileReferenceTarget;
use super::id::{MediaId, PlaylistId, RoomId, UserId};
use super::normalize_provider_instance_name_owned;
use super::query::SortDirection;
use super::{MediaSourceConfig, ProviderTarget};

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
            other => Err(format!("Unknown provider type: {other}")),
        }
    }
}

impl SourceProvider {
    #[must_use]
    pub const fn as_i16(self) -> i16 {
        match self {
            Self::DirectUrl => 1,
            Self::Bilibili => 2,
            Self::Alist => 3,
            Self::Emby => 4,
            Self::Rtmp => 5,
            Self::LiveProxy => 6,
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

#[derive(Debug, Clone)]
pub struct ProviderTypeName(pub SourceProvider);

impl TryFrom<i16> for ProviderTypeName {
    type Error = String;

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        SourceProvider::try_from(value).map(Self)
    }
}

impl sqlx::Type<sqlx::Postgres> for ProviderTypeName {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i16 as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <i16 as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for ProviderTypeName {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let code = <i16 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self(SourceProvider::try_from(code)?))
    }
}

#[derive(Debug, Clone)]
pub struct ProviderTypeNames(pub Vec<SourceProvider>);

impl sqlx::Type<sqlx::Postgres> for ProviderTypeNames {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <Vec<i16> as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <Vec<i16> as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for ProviderTypeNames {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let codes = <Vec<i16> as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        let names = codes
            .into_iter()
            .map(SourceProvider::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self(names))
    }
}

impl std::fmt::Display for SourceProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Media file (video/audio)
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
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
        let now = Utc::now();
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

        let now = Utc::now();
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
    #[serde(default)]
    pub provider: String,

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

    /// Whether this playback source is a live stream.
    #[serde(default)]
    pub is_live: bool,

    /// Provider-facing playback target for dynamic playlist items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ProviderTarget>,

    /// Media-level provider metadata for display-only, provider-specific fields.
    #[serde(default)]
    pub metadata: PlaybackMetadata,
}

/// Complete playback information for a single mode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackInfo {
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
    #[serde(flatten)]
    pub provider: PlaybackMediaProvider,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "provider",
    content = "media",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackMediaProvider {
    External(PlaybackExternalMedia),
    Alist(PlaybackAlistMedia),
    Bilibili(PlaybackBilibiliMedia),
    DirectUrl(PlaybackDirectUrlMedia),
    Emby(PlaybackEmbyMedia),
    Rtmp(PlaybackRtmpMedia),
    LiveProxy(PlaybackLiveProxyMedia),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackExternalMedia {
    pub url: String,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub headers: std::collections::HashMap<String, String>,
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
    HlsPlaylist {
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
    HlsPlaylist {
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
    #[serde(flatten)]
    pub provider: PlaybackSubtitleProvider,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "provider",
    content = "subtitle",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackSubtitleProvider {
    External(PlaybackExternalSubtitle),
    Alist(PlaybackAlistSubtitle),
    Bilibili(PlaybackBilibiliSubtitle),
    DirectUrl(PlaybackDirectUrlSubtitle),
    Emby(PlaybackEmbySubtitle),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackExternalSubtitle {
    pub url: String,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub headers: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackAlistSubtitle {
    pub version: String,
    pub expires_at: i64,
    pub mode_name: String,
    pub subtitle_index: usize,
    pub url: String,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub headers: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackBilibiliSubtitle {
    pub version: String,
    pub expires_at: i64,
    pub mode_name: String,
    pub subtitle_index: usize,
    pub url: String,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub headers: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackDirectUrlSubtitle {
    pub version: String,
    pub expires_at: i64,
    pub mode_name: String,
    pub subtitle_index: usize,
    pub url: String,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub headers: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackEmbySubtitle {
    pub version: String,
    pub expires_at: i64,
    pub mode_name: String,
    pub subtitle_index: usize,
    pub url: String,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub headers: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackDanmaku {
    pub name: String,
    pub format: Option<String>,
    #[serde(flatten)]
    pub provider: PlaybackDanmakuProvider,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "provider",
    content = "danmaku",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackDanmakuProvider {
    External(PlaybackExternalDanmaku),
    Bilibili(PlaybackBilibiliDanmaku),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackExternalDanmaku {
    pub url: String,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub headers: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlaybackBilibiliDanmaku {
    File {
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_thumbnail: Option<PlaybackProxyResourceMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_subtitle_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_preview_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transcoding_tasks: Vec<AlistTranscodingTaskMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_preview: Option<AlistVideoPreviewMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_live: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_id: Option<MediaId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<RoomId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bilibili: Option<BilibiliPlaybackMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emby: Option<EmbyPlaybackMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackProxyResourceMetadata {
    pub version: String,
    pub expires_at: i64,
    pub resource: PlaybackProxyResource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlaybackProxyResource {
    Thumbnail,
}

impl PlaybackProxyResource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Thumbnail => "thumbnail",
        }
    }
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliPlaybackMetadata {
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
    #[serde(default, skip_serializing_if = "BilibiliDashManifests::is_empty")]
    pub dash_manifests: BilibiliDashManifests,
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
    pub base_url: String,
    pub mime_type: String,
    pub codecs: String,
    pub width: u64,
    pub height: u64,
    pub frame_rate: String,
    pub bandwidth: u64,
    pub start_with_sap: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_base: Option<BilibiliDashSegmentBase>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliDashAudioStream {
    pub id: u64,
    pub base_url: String,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbyPlaybackMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play_session_id: Option<String>,
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
            provider: media.source_provider.to_string(),
            provider_instance_name: media.provider_instance_name.clone(),
            position: media.position,
            playback_infos,
            default_mode: mode_name.to_string(),
            duration_seconds: None,
            is_live: false,
            target: None,
            metadata: PlaybackMetadata::default(),
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
            provider: String::new(),
            provider_instance_name: None,
            position,
            playback_infos: indexmap::IndexMap::new(),
            default_mode: None,
            duration_seconds: None,
            is_live: false,
            target: None,
            metadata: PlaybackMetadata::default(),
        }
    }

    /// Replace metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: PlaybackMetadata) -> Self {
        self.metadata = metadata;
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
    provider: String,
    provider_instance_name: Option<String>,
    position: f64,
    /// Uses `IndexMap` to guarantee insertion-order determinism when falling
    /// back to the first mode as default (avoids `HashMap::keys().next()`
    /// non-determinism).
    playback_infos: indexmap::IndexMap<String, PlaybackInfo>,
    default_mode: Option<String>,
    duration_seconds: Option<f64>,
    is_live: bool,
    target: Option<ProviderTarget>,
    metadata: PlaybackMetadata,
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
    pub fn provider(mut self, provider: String) -> Self {
        self.provider = provider;
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
    pub const fn is_live(mut self, is_live: bool) -> Self {
        self.is_live = is_live;
        self
    }

    #[must_use]
    pub fn target(mut self, target: ProviderTarget) -> Self {
        self.target = Some(target);
        self
    }

    /// Replace metadata.
    #[must_use]
    pub fn metadata(mut self, metadata: PlaybackMetadata) -> Self {
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
            provider: self.provider,
            provider_instance_name: self.provider_instance_name,
            position: self.position,
            playback_infos: self.playback_infos.into_iter().collect(),
            default_mode,
            duration_seconds: self.duration_seconds,
            is_live: self.is_live,
            target: self.target,
            metadata: self.metadata,
        })
    }
}

impl PlaybackInfo {
    /// Create a simple playback info with a single URL
    #[must_use]
    pub fn single_url(url: String, name: String) -> Self {
        Self {
            medias: vec![PlaybackMedia {
                name,
                format: String::new(),
                expire_at: None,
                metadata: None,
                provider: PlaybackMediaProvider::External(PlaybackExternalMedia {
                    url,
                    headers: std::collections::HashMap::new(),
                }),
            }],
            default_media_index: None,
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            default_danmaku_index: None,
        }
    }

    /// Create a new builder
    #[must_use]
    pub fn builder() -> PlaybackInfoBuilder {
        PlaybackInfoBuilder::default()
    }
}

/// Builder for `PlaybackInfo`
#[derive(Default)]
pub struct PlaybackInfoBuilder {
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
    /// Create a simple playback URL
    #[must_use]
    pub fn simple(name: String, url: String) -> Self {
        Self {
            name,
            format: String::new(),
            expire_at: None,
            metadata: None,
            provider: PlaybackMediaProvider::External(PlaybackExternalMedia {
                url,
                headers: std::collections::HashMap::new(),
            }),
        }
    }

    /// Create with metadata
    #[must_use]
    pub fn with_metadata(name: String, url: String, metadata: PlaybackMediaMetadata) -> Self {
        Self {
            name,
            format: String::new(),
            expire_at: None,
            metadata: Some(metadata),
            provider: PlaybackMediaProvider::External(PlaybackExternalMedia {
                url,
                headers: std::collections::HashMap::new(),
            }),
        }
    }

    #[must_use]
    pub fn direct_url(&self) -> Option<&str> {
        match &self.provider {
            PlaybackMediaProvider::External(media) => Some(&media.url),
            PlaybackMediaProvider::Alist(PlaybackAlistMedia::Direct { url, .. })
            | PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::Direct { url, .. })
            | PlaybackMediaProvider::Emby(PlaybackEmbyMedia::Direct { url, .. }) => Some(url),
            PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct { url, .. }) => {
                Some(url)
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn upstream_url(&self) -> Option<&str> {
        match &self.provider {
            PlaybackMediaProvider::External(media) => Some(&media.url),
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
                | PlaybackDirectUrlMedia::ProxyHlsManifest { url, .. },
            )
            | PlaybackMediaProvider::Emby(
                PlaybackEmbyMedia::Direct { url, .. }
                | PlaybackEmbyMedia::ProxyMediaStream { url, .. }
                | PlaybackEmbyMedia::ProxyHlsManifest { url, .. },
            ) => Some(url),
            _ => None,
        }
    }

    #[must_use]
    pub fn upstream_headers(&self) -> std::collections::HashMap<String, String> {
        match &self.provider {
            PlaybackMediaProvider::External(media) => media.headers.clone(),
            PlaybackMediaProvider::Alist(
                PlaybackAlistMedia::Direct { headers, .. }
                | PlaybackAlistMedia::ProxyFile { headers, .. }
                | PlaybackAlistMedia::ProxyTranscodedHlsManifest { headers, .. },
            )
            | PlaybackMediaProvider::Bilibili(
                PlaybackBilibiliMedia::Direct { headers, .. }
                | PlaybackBilibiliMedia::DirectDashManifest { headers, .. }
                | PlaybackBilibiliMedia::ProxyMediaStream { headers, .. }
                | PlaybackBilibiliMedia::ProxyHlsManifest { headers, .. },
            )
            | PlaybackMediaProvider::DirectUrl(
                PlaybackDirectUrlMedia::Direct { headers, .. }
                | PlaybackDirectUrlMedia::ProxyStream { headers, .. }
                | PlaybackDirectUrlMedia::ProxyHlsManifest { headers, .. },
            )
            | PlaybackMediaProvider::Emby(
                PlaybackEmbyMedia::Direct { headers, .. }
                | PlaybackEmbyMedia::ProxyMediaStream { headers, .. }
                | PlaybackEmbyMedia::ProxyHlsManifest { headers, .. },
            ) => headers.clone(),
            _ => std::collections::HashMap::new(),
        }
    }

    #[must_use]
    pub fn requires_provider_url(&self) -> bool {
        !matches!(
            self.provider,
            PlaybackMediaProvider::External(_)
                | PlaybackMediaProvider::Alist(PlaybackAlistMedia::Direct { .. })
                | PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::Direct { .. })
                | PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct { .. })
                | PlaybackMediaProvider::Emby(PlaybackEmbyMedia::Direct { .. })
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
    pub fn upstream_url(&self) -> &str {
        match &self.provider {
            PlaybackSubtitleProvider::External(subtitle) => &subtitle.url,
            PlaybackSubtitleProvider::Alist(subtitle) => &subtitle.url,
            PlaybackSubtitleProvider::Bilibili(subtitle) => &subtitle.url,
            PlaybackSubtitleProvider::DirectUrl(subtitle) => &subtitle.url,
            PlaybackSubtitleProvider::Emby(subtitle) => &subtitle.url,
        }
    }

    #[must_use]
    pub fn upstream_headers(&self) -> std::collections::HashMap<String, String> {
        match &self.provider {
            PlaybackSubtitleProvider::External(subtitle) => subtitle.headers.clone(),
            PlaybackSubtitleProvider::Alist(subtitle) => subtitle.headers.clone(),
            PlaybackSubtitleProvider::Bilibili(subtitle) => subtitle.headers.clone(),
            PlaybackSubtitleProvider::DirectUrl(subtitle) => subtitle.headers.clone(),
            PlaybackSubtitleProvider::Emby(subtitle) => subtitle.headers.clone(),
        }
    }
}

impl PlaybackDanmaku {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn upstream_url(&self) -> Option<&str> {
        match &self.provider {
            PlaybackDanmakuProvider::External(danmaku) => Some(&danmaku.url),
            PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::File { url, .. }) => {
                Some(url)
            }
            PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::Live { .. }) => None,
        }
    }

    #[must_use]
    pub fn upstream_headers(&self) -> std::collections::HashMap<String, String> {
        match &self.provider {
            PlaybackDanmakuProvider::External(danmaku) => danmaku.headers.clone(),
            PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::File {
                headers, ..
            }) => headers.clone(),
            PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::Live { .. }) => {
                std::collections::HashMap::new()
            }
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
