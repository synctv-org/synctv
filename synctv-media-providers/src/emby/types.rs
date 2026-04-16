//! Emby/Jellyfin API Data Structures

use serde::Deserialize;
use serde_json::Value;

/// Authentication response
#[derive(Debug, Deserialize)]
pub struct AuthResponse {
    #[serde(rename = "AccessToken")]
    pub access_token: String,
    #[serde(rename = "User")]
    pub user: User,
}

/// User information (for authentication response)
#[derive(Debug, Deserialize)]
pub struct User {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Name")]
    pub name: String,
}

/// User information (detailed)
#[derive(Debug, Deserialize)]
pub struct UserInfo {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "ServerId")]
    pub server_id: String,
    #[serde(rename = "Policy", default)]
    pub policy: Option<UserPolicy>,
}

/// User policy information
#[derive(Debug, Deserialize, Clone)]
pub struct UserPolicy {
    #[serde(rename = "IsAdministrator", default)]
    pub is_administrator: bool,
    #[serde(rename = "IsHidden", default)]
    pub is_hidden: bool,
    #[serde(rename = "IsDisabled", default)]
    pub is_disabled: bool,
    #[serde(rename = "EnableAllFolders", default)]
    pub enable_all_folders: bool,
}

/// Image tags for different image types (Primary, Thumb, etc.)
/// These tags are used to construct thumbnail URLs and for cache busting.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct ImageTags {
    /// Primary image tag (usually poster/thumbnail)
    #[serde(rename = "Primary", default)]
    pub primary: Option<String>,
    /// Thumbnail image tag
    #[serde(rename = "Thumb", default)]
    pub thumb: Option<String>,
}

impl ImageTags {
    /// Check if any image tag is available
    #[must_use]
    pub const fn has_any(&self) -> bool {
        self.primary.is_some() || self.thumb.is_some()
    }

    /// Get the preferred image tag (Primary first, then Thumb)
    #[must_use]
    pub const fn preferred_tag(&self) -> Option<(&str, &str)> {
        if let Some(ref tag) = self.primary {
            Some(("Primary", tag.as_str()))
        } else if let Some(ref tag) = self.thumb {
            Some(("Thumb", tag.as_str()))
        } else {
            None
        }
    }
}

/// Media item information
#[derive(Debug, Deserialize, Clone)]
pub struct Item {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Type")]
    pub item_type: String,
    #[serde(rename = "IsFolder", default)]
    pub is_folder: bool,
    #[serde(rename = "ParentId", default)]
    pub parent_id: Option<String>,
    #[serde(rename = "SeriesName", default)]
    pub series_name: Option<String>,
    #[serde(rename = "SeriesId", default)]
    pub series_id: Option<String>,
    #[serde(rename = "SeasonName", default)]
    pub season_name: Option<String>,
    #[serde(rename = "SeasonId", default)]
    pub season_id: Option<String>,
    #[serde(rename = "CollectionType", default)]
    pub collection_type: Option<String>,
    #[serde(rename = "MediaSources", default)]
    pub media_sources: Vec<MediaSource>,
    #[serde(rename = "RunTimeTicks", default)]
    pub run_time_ticks: Option<u64>,
    #[serde(rename = "ProductionYear", default)]
    pub production_year: Option<u32>,
    /// Image tags for different image types (Primary, Thumb, etc.)
    /// Used to construct thumbnail URLs with cache busting.
    #[serde(rename = "ImageTags", default)]
    pub image_tags: Option<ImageTags>,
}

/// Items response
#[derive(Debug, Deserialize)]
pub struct ItemsResponse {
    #[serde(rename = "Items")]
    pub items: Vec<Item>,
    #[serde(rename = "TotalRecordCount")]
    pub total_record_count: u64,
}

/// Playback information response
#[derive(Debug, Deserialize)]
pub struct PlaybackInfoResp {
    #[serde(rename = "MediaSources")]
    pub media_sources: Vec<MediaSource>,
}

/// Playback information response (detailed)
#[derive(Debug, Deserialize)]
pub struct PlaybackInfoResponse {
    #[serde(rename = "PlaySessionId")]
    pub play_session_id: String,
    #[serde(rename = "MediaSources")]
    pub media_sources: Vec<MediaSource>,
}

/// System information
#[derive(Debug, Deserialize)]
pub struct SystemInfo {
    #[serde(rename = "SystemUpdateLevel", default)]
    pub system_update_level: String,
    #[serde(rename = "OperatingSystemDisplayName", default)]
    pub operating_system_display_name: String,
    #[serde(rename = "PackageName", default)]
    pub package_name: String,
    #[serde(rename = "HasPendingRestart", default)]
    pub has_pending_restart: bool,
    #[serde(rename = "IsShuttingDown", default)]
    pub is_shutting_down: bool,
    #[serde(rename = "SupportsLibraryMonitor", default)]
    pub supports_library_monitor: bool,
    #[serde(rename = "WebSocketPortNumber", default)]
    pub web_socket_port_number: i32,
    #[serde(rename = "CanSelfRestart", default)]
    pub can_self_restart: bool,
    #[serde(rename = "CanSelfUpdate", default)]
    pub can_self_update: bool,
    #[serde(rename = "CanLaunchWebBrowser", default)]
    pub can_launch_web_browser: bool,
    #[serde(rename = "ProgramDataPath", default)]
    pub program_data_path: String,
    #[serde(rename = "ItemsByNamePath", default)]
    pub items_by_name_path: String,
    #[serde(rename = "CachePath", default)]
    pub cache_path: String,
    #[serde(rename = "LogPath", default)]
    pub log_path: String,
    #[serde(rename = "InternalMetadataPath", default)]
    pub internal_metadata_path: String,
    #[serde(rename = "TranscodingTempPath", default)]
    pub transcoding_temp_path: String,
    #[serde(rename = "HttpServerPortNumber", default)]
    pub http_server_port_number: i32,
    #[serde(rename = "SupportsHttps", default)]
    pub supports_https: bool,
    #[serde(rename = "HttpsPortNumber", default)]
    pub https_port_number: i32,
    #[serde(rename = "HasUpdateAvailable", default)]
    pub has_update_available: bool,
    #[serde(rename = "SupportsAutoRunAtStartup", default)]
    pub supports_auto_run_at_startup: bool,
    #[serde(rename = "HardwareAccelerationRequiresPremiere", default)]
    pub hardware_acceleration_requires_premiere: bool,
    #[serde(rename = "LocalAddress", default)]
    pub local_address: String,
    #[serde(rename = "WanAddress", default)]
    pub wan_address: String,
    #[serde(rename = "ServerName", default)]
    pub server_name: String,
    #[serde(rename = "Version", default)]
    pub version: String,
    #[serde(rename = "OperatingSystem", default)]
    pub operating_system: String,
    #[serde(rename = "Id", default)]
    pub id: String,
}

/// Filesystem list response
#[derive(Debug)]
pub struct FsListResponse {
    pub items: Vec<Item>,
    pub paths: Vec<PathInfo>,
    pub total: u64,
}

/// Path information
#[derive(Debug, Clone)]
pub struct PathInfo {
    pub name: String,
    pub path: String,
}

/// Media source with playback URLs
#[derive(Debug, Deserialize, Clone)]
pub struct MediaSource {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Name", default)]
    pub name: String,
    #[serde(rename = "Path", default)]
    pub path: String,
    #[serde(rename = "Container", default)]
    pub container: String,
    #[serde(rename = "Protocol", default)]
    pub protocol: String,
    #[serde(rename = "DefaultSubtitleStreamIndex", default)]
    pub default_subtitle_stream_index: i64,
    #[serde(rename = "DefaultAudioStreamIndex", default)]
    pub default_audio_stream_index: i64,
    #[serde(rename = "MediaStreams", default)]
    pub media_streams: Vec<MediaStream>,
    #[serde(rename = "DirectStreamUrl", default)]
    pub direct_stream_url: String,
    #[serde(rename = "TranscodingUrl", default)]
    pub transcoding_url: String,
    #[serde(rename = "SupportsDirectPlay", default)]
    pub supports_direct_play: bool,
    #[serde(rename = "SupportsTranscoding", default)]
    pub supports_transcoding: bool,
}

/// Media stream (video/audio/subtitle)
#[derive(Debug, Deserialize, Clone)]
pub struct MediaStream {
    #[serde(rename = "Codec", default)]
    pub codec: String,
    #[serde(rename = "Language", default)]
    pub language: String,
    #[serde(rename = "Type", default)]
    pub stream_type: String,
    #[serde(rename = "Title", default)]
    pub title: String,
    #[serde(rename = "DisplayTitle", default)]
    pub display_title: String,
    #[serde(rename = "DisplayLanguage", default)]
    pub display_language: String,
    #[serde(rename = "IsDefault", default)]
    pub is_default: bool,
    #[serde(rename = "Index", default)]
    pub index: u64,
    #[serde(rename = "Protocol", default)]
    pub protocol: String,
    #[serde(rename = "DeliveryUrl", default)]
    pub delivery_url: String,
}

/// Device profile for codec negotiation
#[must_use]
pub fn default_device_profile() -> Value {
    serde_json::json!({
        "DirectPlayProfiles": [
            {
                "Container": "webm",
                "VideoCodec": "vp8,vp9,av1",
                "AudioCodec": "vorbis,opus",
                "Type": "Video"
            },
            {
                "Container": "mp4,m4v",
                "VideoCodec": "h264,hevc,vp9,av1",
                "AudioCodec": "aac,mp3,ac3,eac3,flac,alac",
                "Type": "Video"
            },
            {
                "Container": "mkv",
                "VideoCodec": "h264,hevc,vp9,av1",
                "AudioCodec": "aac,mp3,ac3,eac3,flac,alac,dts,truehd",
                "Type": "Video"
            }
        ],
        "TranscodingProfiles": [
            {
                "Container": "ts",
                "Type": "Video",
                "VideoCodec": "h264",
                "AudioCodec": "aac",
                "Protocol": "hls",
                "EstimateContentLength": false,
                "EnableMpegtsM2TsMode": false,
                "TranscodeSeekInfo": "Auto",
                "CopyTimestamps": false,
                "Context": "Streaming",
                "MaxAudioChannels": "2"
            }
        ],
        "SubtitleProfiles": [
            {
                "Format": "srt",
                "Method": "External"
            },
            {
                "Format": "vtt",
                "Method": "External"
            },
            {
                "Format": "ass",
                "Method": "External"
            }
        ]
    })
}

// From trait implementations for proto conversion

impl From<MediaStream> for crate::grpc::emby::MediaStreamInfo {
    fn from(stream: MediaStream) -> Self {
        Self {
            codec: stream.codec,
            language: stream.language,
            r#type: stream.stream_type,
            title: stream.title,
            display_title: stream.display_title,
            display_language: stream.display_language,
            is_default: stream.is_default,
            index: stream.index,
            protocol: stream.protocol,
        }
    }
}

impl From<MediaSource> for crate::grpc::emby::MediaSourceInfo {
    fn from(source: MediaSource) -> Self {
        Self {
            id: source.id,
            name: source.name,
            path: source.path,
            container: source.container,
            protocol: source.protocol,
            default_subtitle_stream_index: source.default_subtitle_stream_index,
            default_audio_stream_index: source.default_audio_stream_index,
            media_stream_info: source
                .media_streams
                .into_iter()
                .map(std::convert::Into::into)
                .collect(),
            direct_play_url: source.direct_stream_url,
            transcoding_url: source.transcoding_url,
        }
    }
}

impl From<Item> for crate::grpc::emby::Item {
    fn from(item: Item) -> Self {
        Self {
            name: item.name,
            id: item.id,
            r#type: item.item_type,
            parent_id: item.parent_id.unwrap_or_default(),
            series_name: item.series_name.unwrap_or_default(),
            series_id: item.series_id.unwrap_or_default(),
            season_name: item.season_name.unwrap_or_default(),
            season_id: item.season_id.unwrap_or_default(),
            is_folder: item.is_folder,
            media_source_info: item
                .media_sources
                .into_iter()
                .map(std::convert::Into::into)
                .collect(),
            collection_type: item.collection_type.unwrap_or_default(),
        }
    }
}

impl From<PathInfo> for crate::grpc::emby::Path {
    fn from(path: PathInfo) -> Self {
        Self {
            name: path.name,
            path: path.path,
        }
    }
}

impl From<SystemInfo> for crate::grpc::emby::SystemInfoResp {
    fn from(info: SystemInfo) -> Self {
        Self {
            system_update_level: info.system_update_level,
            operating_system_display_name: info.operating_system_display_name,
            package_name: info.package_name,
            has_pending_restart: info.has_pending_restart,
            is_shutting_down: info.is_shutting_down,
            supports_library_monitor: info.supports_library_monitor,
            web_socket_port_number: info.web_socket_port_number,
            can_self_restart: info.can_self_restart,
            can_self_update: info.can_self_update,
            can_launch_web_browser: info.can_launch_web_browser,
            program_data_path: info.program_data_path,
            items_by_name_path: info.items_by_name_path,
            cache_path: info.cache_path,
            log_path: info.log_path,
            internal_metadata_path: info.internal_metadata_path,
            transcoding_temp_path: info.transcoding_temp_path,
            http_server_port_number: info.http_server_port_number,
            supports_https: info.supports_https,
            https_port_number: info.https_port_number,
            has_update_available: info.has_update_available,
            supports_auto_run_at_startup: info.supports_auto_run_at_startup,
            hardware_acceleration_requires_premiere: info.hardware_acceleration_requires_premiere,
            local_address: info.local_address,
            wan_address: info.wan_address,
            server_name: info.server_name,
            version: info.version,
            operating_system: info.operating_system,
            id: info.id,
        }
    }
}

impl From<UserPolicy> for crate::grpc::emby::UserPolicy {
    fn from(policy: UserPolicy) -> Self {
        Self {
            is_administrator: policy.is_administrator,
            is_hidden: policy.is_hidden,
            is_disabled: policy.is_disabled,
            enable_all_folders: policy.enable_all_folders,
        }
    }
}

impl From<UserInfo> for crate::grpc::emby::MeResp {
    fn from(user_info: UserInfo) -> Self {
        Self {
            id: user_info.id,
            name: user_info.name,
            server_id: user_info.server_id,
            policy: user_info.policy.map(std::convert::Into::into),
        }
    }
}

impl From<FsListResponse> for crate::grpc::emby::FsListResp {
    fn from(response: FsListResponse) -> Self {
        Self {
            paths: response
                .paths
                .into_iter()
                .map(std::convert::Into::into)
                .collect(),
            items: response
                .items
                .into_iter()
                .map(std::convert::Into::into)
                .collect(),
            total: response.total,
        }
    }
}

impl From<ItemsResponse> for crate::grpc::emby::GetItemsResp {
    fn from(response: ItemsResponse) -> Self {
        Self {
            items: response
                .items
                .into_iter()
                .map(std::convert::Into::into)
                .collect(),
            total_record_count: response.total_record_count,
        }
    }
}

impl From<PlaybackInfoResponse> for crate::grpc::emby::PlaybackInfoResp {
    fn from(response: PlaybackInfoResponse) -> Self {
        Self {
            play_session_id: response.play_session_id,
            media_source_info: response
                .media_sources
                .into_iter()
                .map(std::convert::Into::into)
                .collect(),
        }
    }
}
