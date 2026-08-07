use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub(crate) struct MediaResponse<T> {
    pub code: i64,
    #[serde(default)]
    pub msg: String,
    pub data: Option<T>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FnosMediaLogin {
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FnosMediaLibrary {
    pub guid: String,
    #[serde(default)]
    pub title: String,
    pub poster: Option<String>,
    #[serde(default)]
    pub posters: Vec<String>,
    pub category: Option<String>,
    #[serde(default)]
    pub view_type: i32,
    #[serde(default)]
    pub poster_type: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FnosMediaListRequest {
    pub ancestor_guid: Option<String>,
    pub exclude_grouped_video: u32,
    pub sort_type: String,
    pub sort_column: String,
    pub page_size: u32,
    pub page: u32,
    pub tags: FnosMediaTags,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FnosMediaTags {
    #[serde(rename = "type")]
    pub media_types: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FnosMediaList {
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub list: Vec<FnosMediaItem>,
    pub mdb_name: Option<String>,
    pub mdb_category: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FnosMediaItem {
    pub guid: String,
    #[serde(default)]
    pub title: String,
    #[serde(rename = "type", default)]
    pub item_type: String,
    pub poster: Option<String>,
    pub tv_title: Option<String>,
    pub parent_title: Option<String>,
    pub parent_guid: Option<String>,
    pub ancestor_guid: Option<String>,
    pub ancestor_name: Option<String>,
    pub ancestor_category: Option<String>,
    #[serde(default)]
    pub watched: i32,
    #[serde(default)]
    pub is_favorite: i32,
    #[serde(default)]
    pub ts: u64,
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub episode_number: u32,
    #[serde(default)]
    pub season_number: u32,
    pub vote_average: Option<String>,
    pub overview: Option<String>,
    pub media_guid: Option<String>,
    pub video_guid: Option<String>,
    pub audio_guid: Option<String>,
    pub subtitle_guid: Option<String>,
    pub single_child_guid: Option<String>,
}

impl FnosMediaItem {
    #[must_use]
    pub fn is_folder(&self) -> bool {
        matches!(
            self.item_type.to_ascii_lowercase().as_str(),
            "directory" | "folder" | "tv" | "season"
        )
    }

    #[must_use]
    pub fn is_playable(&self) -> bool {
        matches!(
            self.item_type.to_ascii_lowercase().as_str(),
            "movie" | "video" | "episode"
        )
    }

    #[must_use]
    pub fn display_title(&self) -> String {
        self.tv_title
            .as_deref()
            .filter(|value| !value.is_empty())
            .or_else(|| (!self.title.is_empty()).then_some(self.title.as_str()))
            .unwrap_or("FNOS media")
            .to_string()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FnosPlayInfoRequest {
    pub item_guid: String,
    pub media_guid: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FnosPlayInfo {
    pub guid: String,
    pub media_guid: String,
    pub video_guid: Option<String>,
    pub audio_guid: Option<String>,
    pub subtitle_guid: Option<String>,
    #[serde(default)]
    pub ts: u64,
    pub item: FnosPlayItem,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FnosPlayItem {
    pub guid: String,
    pub parent_guid: Option<String>,
    pub title: Option<String>,
    pub tv_title: Option<String>,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub poster: Option<String>,
    pub posters: Option<String>,
    pub backdrops: Option<String>,
    pub release_date: Option<String>,
    pub air_date: Option<String>,
    #[serde(default)]
    pub runtime: u64,
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub episode_number: u32,
    #[serde(default)]
    pub season_number: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FnosStreamRequest {
    pub header: std::collections::HashMap<String, Vec<String>>,
    pub level: u32,
    pub media_guid: String,
    pub ip: String,
    pub nonce: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FnosStream {
    pub file_stream: Option<FnosFileStream>,
    pub video_stream: Option<FnosVideoStream>,
    #[serde(default)]
    pub audio_streams: Vec<FnosAudioStream>,
    #[serde(default)]
    pub subtitle_streams: Vec<FnosSubtitleStream>,
    #[serde(default)]
    pub direct_link_qualities: Vec<FnosDirectLinkQuality>,
    #[serde(default)]
    pub qualities: Vec<FnosQuality>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FnosFileStream {
    #[serde(default)]
    pub size: u64,
    pub path: Option<String>,
    pub file_name: Option<String>,
    #[serde(default)]
    pub duration: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FnosVideoStream {
    pub guid: Option<String>,
    pub resolution_type: Option<String>,
    pub color_range_type: Option<String>,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub bps: u64,
    pub codec_name: Option<String>,
    pub profile: Option<String>,
    #[serde(default)]
    pub bit_depth: u32,
    #[serde(default)]
    pub dv_profile: i32,
    pub r_frame_rate: Option<String>,
    #[serde(default)]
    pub duration: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FnosAudioStream {
    pub guid: Option<String>,
    pub title: Option<String>,
    pub language: Option<String>,
    pub codec_name: Option<String>,
    #[serde(default)]
    pub channels: u32,
    #[serde(default)]
    pub bps: u64,
    #[serde(default)]
    pub is_default: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FnosSubtitleStream {
    pub guid: Option<String>,
    pub title: Option<String>,
    pub language: Option<String>,
    pub codec_name: Option<String>,
    pub format: Option<String>,
    #[serde(default)]
    pub is_external: i32,
    #[serde(default)]
    pub is_default: i32,
    #[serde(default)]
    pub forced: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FnosDirectLinkQuality {
    #[serde(default)]
    pub bitrate: u64,
    pub resolution: Option<String>,
    #[serde(default)]
    pub progressive: bool,
    pub url: String,
    #[serde(default)]
    pub is_m3u8: bool,
    #[serde(default)]
    pub expired_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FnosQuality {
    #[serde(default)]
    pub bitrate: u64,
    pub resolution: Option<String>,
    #[serde(default)]
    pub progressive: bool,
    #[serde(default)]
    pub is_m3u8: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FnosPlayRequest {
    pub media_guid: String,
    pub video_guid: String,
    pub video_encoder: String,
    pub resolution: String,
    pub bitrate: u64,
    #[serde(rename = "startTimestamp")]
    pub start_timestamp: u64,
    pub audio_encoder: String,
    pub audio_guid: String,
    pub subtitle_guid: String,
    pub channels: u32,
    pub forced_sdr: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FnosPlayResponse {
    pub play_link: String,
    pub media_guid: Option<String>,
    pub video_guid: Option<String>,
    pub audio_guid: Option<String>,
    pub subtitle_guid: Option<String>,
    #[serde(default)]
    pub video_index: u32,
    #[serde(default)]
    pub audio_index: u32,
    #[serde(default)]
    pub subtitle_index: u32,
    pub subtitle_link: Option<String>,
    pub non_fatal_errno: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FnosPlayRecordRequest {
    pub item_guid: String,
    pub media_guid: String,
    pub video_guid: String,
    pub audio_guid: String,
    pub subtitle_guid: Option<String>,
    pub resolution: String,
    pub bitrate: u64,
    pub ts: u64,
    pub duration: u64,
    pub play_link: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FnosMediaCommandRequest {
    pub req: String,
    pub reqid: String,
    #[serde(rename = "playLink")]
    pub play_link: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct FnosItemGuidRequest<'a> {
    pub guid: &'a str,
}
