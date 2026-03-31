use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::str::FromStr;

use super::id::{MediaId, PlaylistId, RoomId, UserId};

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

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "direct_url" | "directurl" => Ok(Self::DirectUrl),
            "bilibili" => Ok(Self::Bilibili),
            "alist" => Ok(Self::Alist),
            "emby" => Ok(Self::Emby),
            "rtmp" => Ok(Self::Rtmp),
            "live_proxy" | "liveproxy" => Ok(Self::LiveProxy),
            _ => Err(format!("Unknown provider type: {s}")),
        }
    }
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DirectUrl => write!(f, "direct_url"),
            Self::Bilibili => write!(f, "bilibili"),
            Self::Alist => write!(f, "alist"),
            Self::Emby => write!(f, "emby"),
            Self::Rtmp => write!(f, "rtmp"),
            Self::LiveProxy => write!(f, "live_proxy"),
        }
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
    pub position: i32,
    /// Provider type name (e.g., "bilibili", "alist", "emby", "`direct_url`")
    /// Stored as string for flexibility, not an enum
    pub source_provider: String,
    /// Provider-specific configuration (JSONB)
    /// Should ONLY be parsed by the provider implementation, NOT by Media model
    pub source_config: JsonValue,
    /// Provider instance name (e.g., "`bilibili_main`", "`alist_company`")
    /// Used to look up the provider from the registry at playback time
    pub provider_instance_name: Option<String>,
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
    pub source_config: JsonValue,
    pub provider_name: String,
    pub provider_instance_name: String,
    pub position: i32,
}

impl Media {
    /// Create media from provider instance (registry pattern)
    ///
    /// This is the preferred way to create media when using the provider registry.
    /// The `provider_instance_name` is used to look up the provider at playback time.
    ///
    /// # Arguments
    /// * `provider_name` - Provider type name from `provider.name()` (e.g., "bilibili")
    /// * `provider_instance_name` - Instance name for lookup (e.g., "`bilibili_main`")
    ///
    /// # Example
    /// ```text
    /// let provider = providers_manager.get_provider("bilibili_main").await?;
    /// let media = Media::from_provider(..., provider.name(), "bilibili_main", ...);
    /// ```
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn from_provider(
        playlist_id: Option<PlaylistId>,
        room_id: RoomId,
        creator_id: Option<UserId>,
        name: String,
        source_config: JsonValue,
        provider_name: &str,
        provider_instance_name: String,
        position: i32,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: MediaId::new(),
            playlist_id,
            room_id,
            creator_id,
            name,
            position,
            source_provider: provider_name.to_string(),
            source_config,
            provider_instance_name: Some(provider_instance_name),
            added_at: now,
            updated_at: now,
            version: 0,
        }
    }

    /// Create media from provider with parameters struct
    #[must_use]
    pub fn from_provider_with_params(params: FromProviderParams) -> Self {
        let now = Utc::now();
        Self {
            id: MediaId::new(),
            playlist_id: params.playlist_id,
            room_id: params.room_id,
            creator_id: params.creator_id,
            name: params.name,
            position: params.position,
            source_provider: params.provider_name,
            source_config: params.source_config,
            provider_instance_name: Some(params.provider_instance_name),
            added_at: now,
            updated_at: now,
            version: 0,
        }
    }

    /// Create a direct URL media with multi-mode playback info
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn from_direct_multimode(
        playlist_id: Option<PlaylistId>,
        room_id: RoomId,
        creator_id: Option<UserId>,
        name: String,
        playback_infos: std::collections::HashMap<String, PlaybackInfo>,
        default_mode: String,
        metadata: std::collections::HashMap<String, JsonValue>,
        position: i32,
    ) -> Self {
        // Only store playback_infos, default_mode, and metadata in source_config
        // id, playlist_id, room_id, name, position are stored in Media fields
        let source_config = serde_json::json!({
            "playback_infos": playback_infos,
            "default_mode": default_mode,
            "metadata": metadata,
        });

        let now = Utc::now();
        Self {
            id: MediaId::new(),
            playlist_id,
            room_id,
            creator_id,
            name,
            position,
            source_provider: "direct_url".to_string(),
            source_config,
            provider_instance_name: None,
            added_at: now,
            updated_at: now,
            version: 0,
        }
    }

    /// Create a direct URL media with single playback info (convenience method)
    #[must_use]
    pub fn from_direct_single_mode(
        playlist_id: Option<PlaylistId>,
        room_id: RoomId,
        creator_id: Option<UserId>,
        name: String,
        mode_name: &str,
        playback_info: PlaybackInfo,
        position: i32,
    ) -> Self {
        let mut playback_infos = std::collections::HashMap::new();
        playback_infos.insert(mode_name.to_string(), playback_info);

        Self::from_direct_multimode(
            playlist_id,
            room_id,
            creator_id,
            name,
            playback_infos,
            mode_name.to_string(),
            std::collections::HashMap::new(),
            position,
        )
    }
}

// ============================================================================
// Playback Information Structures (for all media types)
// ============================================================================
// PlaybackResult is returned when generating playback info (at playback time)
// For direct type media, source_config can store either:
// 1. PlaybackResult (multi-mode, recommended)
// 2. PlaybackInfo (single mode, will be wrapped into PlaybackResult)

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
    pub position: i32,

    /// Playback mode `HashMap` (multiple `PlaybackInfo` objects)
    /// Provider can define arbitrary mode names, such as:
    /// - "direct" and "proxied" (common)
    /// - "cdn1", "cdn2", "cdn3" (multiple CDNs)
    /// - "high", "medium", "low" (different qualities)
    pub playback_infos: std::collections::HashMap<String, PlaybackInfo>,

    /// Default mode name (must be a key in `playback_infos`)
    /// Provider decides based on `source_config.prefer_proxy` etc.
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
    /// Display name (e.g., "简体中文", "English")
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
    /// Display name (e.g., "原始", "AI翻译")
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
    /// Display name (e.g., "Bilibili弹幕", "本地弹幕")
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

// ============================================================================
// Helper implementations
// ============================================================================

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
            id: Some(media.id.clone()),
            playlist_id: media.playlist_id.clone(),
            room_id: media.room_id.clone(),
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
        position: i32,
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
    position: i32,
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

        let default_mode = self.default_mode.or_else(|| {
            // If no default mode specified, use the first inserted mode.
            // This is deterministic because IndexMap preserves insertion order.
            self.playback_infos.keys().next().cloned()
        })?;

        // Verify default_mode exists in playback_infos
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

    #[test]
    fn test_playback_result_builder_deterministic_default_mode() {
        // The builder should always pick the first-inserted mode as default
        // when no explicit default_mode is set. This must be deterministic
        // across multiple runs (IndexMap preserves insertion order).
        let playlist_id = PlaylistId::from_string("pl1".to_string());
        let room_id = RoomId::from_string("r1".to_string());

        for _ in 0..20 {
            let result = PlaybackResult::builder(
                Some(playlist_id.clone()),
                room_id.clone(),
                "test".to_string(),
                0,
            )
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
            .build()
            .expect("build should succeed");

            assert_eq!(
                result.default_mode, "alpha",
                "Default mode should always be the first inserted mode"
            );
        }
    }

    #[test]
    fn test_playback_result_builder_explicit_default_mode() {
        let playlist_id = PlaylistId::from_string("pl1".to_string());
        let room_id = RoomId::from_string("r1".to_string());

        let result = PlaybackResult::builder(Some(playlist_id), room_id, "test".to_string(), 0)
            .add_mode(
                "direct".to_string(),
                PlaybackInfo::single_url("http://d".to_string(), "D".to_string()),
            )
            .add_mode(
                "proxy".to_string(),
                PlaybackInfo::single_url("http://p".to_string(), "P".to_string()),
            )
            .default_mode("proxy".to_string())
            .build()
            .expect("build should succeed");

        assert_eq!(result.default_mode, "proxy");
    }

    #[test]
    fn test_playback_result_builder_empty_returns_none() {
        let playlist_id = PlaylistId::from_string("pl1".to_string());
        let room_id = RoomId::from_string("r1".to_string());

        let result =
            PlaybackResult::builder(Some(playlist_id), room_id, "test".to_string(), 0).build();
        assert!(result.is_none());
    }

    #[test]
    fn test_playback_result_builder_invalid_default_mode_returns_none() {
        let playlist_id = PlaylistId::from_string("pl1".to_string());
        let room_id = RoomId::from_string("r1".to_string());

        let result = PlaybackResult::builder(Some(playlist_id), room_id, "test".to_string(), 0)
            .add_mode(
                "direct".to_string(),
                PlaybackInfo::single_url("http://d".to_string(), "D".to_string()),
            )
            .default_mode("nonexistent".to_string())
            .build();

        assert!(result.is_none());
    }

    #[test]
    fn test_subtitle_url_format_field() {
        // Test that SubtitleUrl can be created with format field
        let subtitle_url = SubtitleUrl {
            name: "English".to_string(),
            url: "https://example.com/sub.vtt".to_string(),
            headers: std::collections::HashMap::new(),
            format: "vtt".to_string(),
        };

        assert_eq!(subtitle_url.name, "English");
        assert_eq!(subtitle_url.url, "https://example.com/sub.vtt");
        assert_eq!(subtitle_url.format, "vtt");

        // Test serialization includes format
        let json = serde_json::to_string(&subtitle_url).expect("should serialize");
        assert!(json.contains("\"format\":\"vtt\""));

        // Test deserialization with format
        let json_with_format =
            r#"{"name":"Chinese","url":"https://example.com/cn.srt","format":"srt"}"#;
        let deserialized: SubtitleUrl =
            serde_json::from_str(json_with_format).expect("should deserialize");
        assert_eq!(deserialized.name, "Chinese");
        assert_eq!(deserialized.format, "srt");

        // Test deserialization without format (should default to empty string)
        let json_without_format = r#"{"name":"Japanese","url":"https://example.com/jp.ass"}"#;
        let deserialized_default: SubtitleUrl =
            serde_json::from_str(json_without_format).expect("should deserialize");
        assert_eq!(deserialized_default.name, "Japanese");
        assert_eq!(deserialized_default.format, "");
    }

    #[test]
    fn test_playback_info_format_field() {
        // Test that PlaybackInfo can be created with format field using builder
        let playback_info = PlaybackInfo::builder()
            .add_url(PlaybackUrl::simple(
                "1080P".to_string(),
                "https://example.com/video.m3u8".to_string(),
            ))
            .format("hls".to_string())
            .build();

        assert_eq!(playback_info.format, "hls");
        assert_eq!(playback_info.urls.len(), 1);

        // Test serialization includes format
        let json = serde_json::to_string(&playback_info).expect("should serialize");
        assert!(json.contains("\"format\":\"hls\""));

        // Test deserialization with format
        let json_with_format =
            r#"{"urls":[{"name":"720P","url":"https://example.com/video.mp4"}],"format":"mp4"}"#;
        let deserialized: PlaybackInfo =
            serde_json::from_str(json_with_format).expect("should deserialize");
        assert_eq!(deserialized.format, "mp4");
        assert_eq!(deserialized.urls.len(), 1);

        // Test deserialization without format (should default to empty string)
        let json_without_format =
            r#"{"urls":[{"name":"480P","url":"https://example.com/video.webm"}]}"#;
        let deserialized_default: PlaybackInfo =
            serde_json::from_str(json_without_format).expect("should deserialize");
        assert_eq!(deserialized_default.format, "");
    }

    #[test]
    fn test_playback_info_single_url_has_empty_format() {
        // Test that single_url convenience method creates PlaybackInfo with empty format
        let playback_info = PlaybackInfo::single_url(
            "https://example.com/video.mp4".to_string(),
            "Direct".to_string(),
        );

        assert_eq!(playback_info.format, "");
        assert_eq!(playback_info.urls.len(), 1);
    }

    #[test]
    fn test_subtitle_url_skip_serializing_empty_format() {
        // Test that empty format is skipped during serialization
        let subtitle_url = SubtitleUrl {
            name: "Test".to_string(),
            url: "https://example.com/test.vtt".to_string(),
            headers: std::collections::HashMap::new(),
            format: String::new(),
        };

        let json = serde_json::to_string(&subtitle_url).expect("should serialize");
        assert!(!json.contains("\"format\""));
    }

    #[test]
    fn test_playback_info_skip_serializing_empty_format() {
        // Test that empty format is skipped during serialization
        let playback_info = PlaybackInfo::single_url(
            "https://example.com/video.mp4".to_string(),
            "Test".to_string(),
        );

        let json = serde_json::to_string(&playback_info).expect("should serialize");
        assert!(!json.contains("\"format\""));
    }
}
