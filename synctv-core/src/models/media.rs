use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::str::FromStr;

use super::file_storage::FileReferenceTarget;
use super::id::{MediaId, PlaylistId, RoomId, UserId};
use super::normalize_provider_instance_name_owned;
use super::query::SortDirection;

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
    pub source_provider: Option<String>,
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

/// Media provider type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    DirectUrl,
    Bilibili,
    Alist,
    Emby,
    Rtmp,
    LiveProxy,
}

impl FromStr for ProviderType {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "direct_url" | "directurl" => Ok(Self::DirectUrl),
            "bilibili" => Ok(Self::Bilibili),
            "alist" => Ok(Self::Alist),
            "emby" => Ok(Self::Emby),
            "rtmp" => Ok(Self::Rtmp),
            "live_proxy" | "liveproxy" => Ok(Self::LiveProxy),
            other => Err(format!("Unknown provider type: {other}")),
        }
    }
}

impl ProviderType {
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

impl TryFrom<i16> for ProviderType {
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

impl From<ProviderType> for i16 {
    fn from(value: ProviderType) -> Self {
        value.as_i16()
    }
}

pub fn provider_type_code_from_name(raw: &str) -> Result<i16, String> {
    raw.parse::<ProviderType>().map(ProviderType::as_i16)
}

pub fn provider_type_name_from_code(code: i16) -> Result<String, String> {
    ProviderType::try_from(code).map(|provider| provider.to_string())
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
pub struct ProviderTypeName(pub String);

impl TryFrom<i16> for ProviderTypeName {
    type Error = String;

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        provider_type_name_from_code(value).map(Self)
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
        Ok(Self(provider_type_name_from_code(code)?))
    }
}

#[derive(Debug, Clone)]
pub struct ProviderTypeNames(pub Vec<String>);

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
            .map(provider_type_name_from_code)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self(names))
    }
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Media file (video/audio)
///
/// Note: `source_config` is provider-specific and should only be parsed by the provider itself.
/// - For direct type: contains `PlaybackResult` (with danmakus in PlaybackInfo.danmakus)
/// - For provider types: contains provider-specific config (e.g., `BilibiliConfig`)
///   Provider's `generate_playback()` will deserialize `source_config` and return `PlaybackResult`
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
    /// Provider type name (e.g., "bilibili", "alist", "emby", "`direct_url`").
    /// The database stores the corresponding numeric provider type code; API
    /// and provider boundaries keep using canonical names.
    pub source_provider: String,
    /// Provider-specific configuration (JSONB)
    /// Should ONLY be parsed by the provider implementation, NOT by Media model
    pub source_config: JsonValue,
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
    pub source_config: JsonValue,
    pub provider_name: String,
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
            source_provider: params.provider_name,
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
        let default_url = default_info
            .and_then(|info| {
                info.urls
                    .get(info.default_url_index)
                    .or_else(|| info.urls.first())
            })
            .ok_or_else(|| {
                crate::Error::InvalidInput("direct media requires a playback URL".to_string())
            })?;

        let source_config = serde_json::json!({
            "url": default_url.url.as_str(),
            "headers": default_url.headers.clone(),
        });

        let now = Utc::now();
        Ok(Self {
            id: MediaId::new(),
            playlist_id: params.playlist_id,
            room_id: params.room_id,
            creator_id: params.creator_id,
            name: params.name,
            description: String::new(),
            position: params.position,
            source_provider: "direct_url".to_string(),
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

    /// Media-level metadata (duration, thumbnail, title, author, etc.)
    /// Flexible JSON structure for provider-specific metadata
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, JsonValue>,
}

/// Complete playback information for a single mode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackInfo {
    /// List of playback URLs (different qualities, codecs)
    pub urls: Vec<PlaybackUrl>,

    /// Default URL index
    #[serde(default)]
    pub default_url_index: usize,

    /// Subtitle list
    #[serde(default)]
    pub subtitles: Vec<Subtitle>,

    /// Default subtitle index
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_subtitle_index: Option<usize>,

    /// Danmaku list (each mode can have different danmaku sources)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub danmakus: Vec<Danmaku>,

    /// Format (e.g., "hls", "dash", "mp4")
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
}

/// Playback URL (represents a quality/codec option)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackUrl {
    /// Display name (e.g., "1080P", "HEVC 4K", "720P")
    pub name: String,

    /// Complete URL
    pub url: String,

    /// Request headers (if needed)
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub headers: std::collections::HashMap<String, String>,

    /// Expiration time (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire_at: Option<DateTime<Utc>>,

    /// URL-level metadata (resolution, codec, bitrate, fps, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<PlaybackUrlMetadata>,
}

/// URL-level metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackUrlMetadata {
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

    /// Additional metadata
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, JsonValue>,
}

/// Subtitle information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subtitle {
    /// Display name (e.g., "Chinese (Simplified)", "English")
    pub name: String,

    /// Language code (e.g., "zh-CN", "en-US")
    pub language: String,

    /// Subtitle URL list (multiple sources/formats)
    pub urls: Vec<SubtitleUrl>,

    /// Default URL index
    #[serde(default)]
    pub default_url_index: usize,
}

/// Subtitle URL
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleUrl {
    /// Display name (e.g., "Original", "AI Translation")
    pub name: String,

    /// Subtitle file URL
    pub url: String,

    /// Request headers (if needed)
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub headers: std::collections::HashMap<String, String>,

    /// Format (e.g., "json", "srt", "vtt")
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
}

/// Danmaku (bullet comments) information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Danmaku {
    /// Display name (e.g., "Bilibili Danmaku", "Local Danmaku")
    pub name: String,

    /// Danmaku API URL or file URL
    pub url: String,

    /// Format type (e.g., "bilibili", "ass", "xml")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,

    /// Request headers (if needed)
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub headers: std::collections::HashMap<String, String>,
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
            position: media.position,
            playback_infos,
            default_mode: mode_name.to_string(),
            metadata: std::collections::HashMap::new(),
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
            position,
            playback_infos: indexmap::IndexMap::new(),
            default_mode: None,
            metadata: std::collections::HashMap::new(),
        }
    }

    /// Add metadata field
    #[must_use]
    pub fn with_metadata(mut self, key: String, value: JsonValue) -> Self {
        self.metadata.insert(key, value);
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
    position: f64,
    /// Uses `IndexMap` to guarantee insertion-order determinism when falling
    /// back to the first mode as default (avoids `HashMap::keys().next()`
    /// non-determinism).
    playback_infos: indexmap::IndexMap<String, PlaybackInfo>,
    default_mode: Option<String>,
    metadata: std::collections::HashMap<String, JsonValue>,
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

    /// Set the default mode
    #[must_use]
    pub fn default_mode(mut self, mode_name: String) -> Self {
        self.default_mode = Some(mode_name);
        self
    }

    /// Add metadata
    #[must_use]
    pub fn add_metadata(mut self, key: String, value: JsonValue) -> Self {
        self.metadata.insert(key, value);
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
            position: self.position,
            playback_infos: self.playback_infos.into_iter().collect(),
            default_mode,
            metadata: self.metadata,
        })
    }
}

impl PlaybackInfo {
    /// Create a simple playback info with a single URL
    #[must_use]
    pub fn single_url(url: String, name: String) -> Self {
        Self {
            urls: vec![PlaybackUrl {
                name,
                url,
                headers: std::collections::HashMap::new(),
                expire_at: None,
                metadata: None,
            }],
            default_url_index: 0,
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            format: String::new(),
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
    urls: Vec<PlaybackUrl>,
    default_url_index: usize,
    subtitles: Vec<Subtitle>,
    default_subtitle_index: Option<usize>,
    danmakus: Vec<Danmaku>,
    format: String,
}

impl PlaybackInfoBuilder {
    /// Add a playback URL
    #[must_use]
    pub fn add_url(mut self, url: PlaybackUrl) -> Self {
        self.urls.push(url);
        self
    }

    /// Set the default URL index
    #[must_use]
    pub const fn default_url_index(mut self, index: usize) -> Self {
        self.default_url_index = index;
        self
    }

    /// Add a subtitle
    #[must_use]
    pub fn add_subtitle(mut self, subtitle: Subtitle) -> Self {
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
    pub fn add_danmaku(mut self, danmaku: Danmaku) -> Self {
        self.danmakus.push(danmaku);
        self
    }

    /// Set the format (e.g., "hls", "dash", "mp4")
    #[must_use]
    pub fn format(mut self, format: String) -> Self {
        self.format = format;
        self
    }

    /// Build the `PlaybackInfo`
    #[must_use]
    pub fn build(self) -> PlaybackInfo {
        PlaybackInfo {
            urls: self.urls,
            default_url_index: self.default_url_index,
            subtitles: self.subtitles,
            default_subtitle_index: self.default_subtitle_index,
            danmakus: self.danmakus,
            format: self.format,
        }
    }
}

impl PlaybackUrl {
    /// Create a simple playback URL
    #[must_use]
    pub fn simple(name: String, url: String) -> Self {
        Self {
            name,
            url,
            headers: std::collections::HashMap::new(),
            expire_at: None,
            metadata: None,
        }
    }

    /// Create with metadata
    #[must_use]
    pub fn with_metadata(name: String, url: String, metadata: PlaybackUrlMetadata) -> Self {
        Self {
            name,
            url,
            headers: std::collections::HashMap::new(),
            expire_at: None,
            metadata: Some(metadata),
        }
    }
}

impl PlaybackUrlMetadata {
    /// Create metadata with resolution and codec
    #[must_use]
    pub fn new(resolution: String, codec: String) -> Self {
        Self {
            resolution: Some(resolution),
            codec: Some(codec),
            bitrate: None,
            fps: None,
            extra: std::collections::HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn some<T>(value: Option<T>, context: &str) -> T {
        match value {
            Some(value) => value,
            None => std::panic::panic_any(context.to_string()),
        }
    }

    #[test]
    fn test_provider_type_parse_trimmed_case_insensitive_names() {
        assert_eq!(
            ok(
                " alist ".parse::<ProviderType>(),
                "alist provider type should parse"
            ),
            ProviderType::Alist
        );
        assert_eq!(
            ok(
                " DIRECTURL ".parse::<ProviderType>(),
                "direct-url provider type should parse"
            ),
            ProviderType::DirectUrl
        );
        assert_eq!(
            ok(
                " live_proxy ".parse::<ProviderType>(),
                "live-proxy provider type should parse"
            ),
            ProviderType::LiveProxy
        );
    }

    #[test]
    fn test_playback_result_builder_deterministic_default_mode() {
        let playlist_id = PlaylistId::expect_positive(60_001);
        let room_id = RoomId::expect_positive(60_002);

        for _ in 0..20 {
            let result = some(
                PlaybackResult::builder(Some(playlist_id), room_id, "test".to_string(), 0.0)
                    .add_mode(
                        "alpha".to_string(),
                        PlaybackInfo::single_url("http://a".to_string(), "A".to_string()),
                    )
                    .add_mode(
                        "beta".to_string(),
                        PlaybackInfo::single_url("http://b".to_string(), "B".to_string()),
                    )
                    .add_mode(
                        "gamma".to_string(),
                        PlaybackInfo::single_url("http://c".to_string(), "C".to_string()),
                    )
                    .build(),
                "playback result should build",
            );

            assert_eq!(
                result.default_mode, "alpha",
                "default mode should follow insertion order"
            );
        }
    }

    #[test]
    fn test_playback_result_builder_explicit_default_mode() {
        let playlist_id = PlaylistId::expect_positive(60_003);
        let room_id = RoomId::expect_positive(60_004);

        let result = some(
            PlaybackResult::builder(Some(playlist_id), room_id, "test".to_string(), 0.0)
                .add_mode(
                    "direct".to_string(),
                    PlaybackInfo::single_url("http://d".to_string(), "D".to_string()),
                )
                .add_mode(
                    "proxy".to_string(),
                    PlaybackInfo::single_url("http://p".to_string(), "P".to_string()),
                )
                .default_mode("proxy".to_string())
                .build(),
            "playback result should build",
        );

        assert_eq!(result.default_mode, "proxy");
    }

    #[test]
    fn test_playback_result_builder_empty_returns_none() {
        let playlist_id = PlaylistId::expect_positive(60_005);
        let room_id = RoomId::expect_positive(60_006);

        let result =
            PlaybackResult::builder(Some(playlist_id), room_id, "test".to_string(), 0.0).build();
        assert!(result.is_none());
    }

    #[test]
    fn test_playback_result_builder_invalid_default_mode_returns_none() {
        let playlist_id = PlaylistId::expect_positive(60_007);
        let room_id = RoomId::expect_positive(60_008);

        let result = PlaybackResult::builder(Some(playlist_id), room_id, "test".to_string(), 0.0)
            .add_mode(
                "direct".to_string(),
                PlaybackInfo::single_url("http://d".to_string(), "D".to_string()),
            )
            .default_mode("nonexistent".to_string())
            .build();

        assert!(result.is_none());
    }

    #[test]
    fn subtitle_url_format_is_optional_in_json() {
        let subtitle_url = SubtitleUrl {
            name: "English".to_string(),
            url: "https://example.com/sub.vtt".to_string(),
            headers: std::collections::HashMap::new(),
            format: "vtt".to_string(),
        };

        let json = ok(
            serde_json::to_string(&subtitle_url),
            "subtitle URL should serialize",
        );
        assert!(json.contains("\"format\":\"vtt\""));

        let json_with_format =
            r#"{"name":"Chinese","url":"https://example.com/cn.srt","format":"srt"}"#;
        let deserialized: SubtitleUrl = ok(
            serde_json::from_str(json_with_format),
            "subtitle URL should deserialize",
        );
        assert_eq!(deserialized.name, "Chinese");
        assert_eq!(deserialized.format, "srt");

        let json_without_format = r#"{"name":"Japanese","url":"https://example.com/jp.ass"}"#;
        let deserialized_default: SubtitleUrl = ok(
            serde_json::from_str(json_without_format),
            "subtitle URL default format should deserialize",
        );
        assert_eq!(deserialized_default.name, "Japanese");
        assert_eq!(deserialized_default.format, "");

        let empty_format = SubtitleUrl {
            name: "Test".to_string(),
            url: "https://example.com/test.vtt".to_string(),
            headers: std::collections::HashMap::new(),
            format: String::new(),
        };
        let json = ok(
            serde_json::to_string(&empty_format),
            "subtitle URL without format should serialize",
        );
        assert!(!json.contains("\"format\""));
    }

    #[test]
    fn playback_info_format_is_optional_in_json() {
        let playback_info = PlaybackInfo::builder()
            .add_url(PlaybackUrl::simple(
                "1080P".to_string(),
                "https://example.com/video.m3u8".to_string(),
            ))
            .format("hls".to_string())
            .build();
        let json = ok(
            serde_json::to_string(&playback_info),
            "playback info should serialize",
        );
        assert!(json.contains("\"format\":\"hls\""));

        let json_with_format =
            r#"{"urls":[{"name":"720P","url":"https://example.com/video.mp4"}],"format":"mp4"}"#;
        let deserialized: PlaybackInfo = ok(
            serde_json::from_str(json_with_format),
            "playback info should deserialize",
        );
        assert_eq!(deserialized.format, "mp4");
        assert_eq!(deserialized.urls.len(), 1);

        let json_without_format =
            r#"{"urls":[{"name":"480P","url":"https://example.com/video.webm"}]}"#;
        let deserialized_default: PlaybackInfo = ok(
            serde_json::from_str(json_without_format),
            "playback info default format should deserialize",
        );
        assert_eq!(deserialized_default.format, "");

        let playback_info = PlaybackInfo::single_url(
            "https://example.com/video.mp4".to_string(),
            "Test".to_string(),
        );
        let json = ok(
            serde_json::to_string(&playback_info),
            "playback info without format should serialize",
        );
        assert!(!json.contains("\"format\""));
    }
}
