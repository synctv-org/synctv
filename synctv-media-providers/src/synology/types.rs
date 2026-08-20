use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub type SynologyApiMap = HashMap<String, SynologyApiInfo>;

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyApiInfo {
    pub path: String,
    #[serde(rename = "minVersion")]
    pub min_version: u32,
    #[serde(rename = "maxVersion")]
    pub max_version: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyLogin {
    pub sid: String,
    #[serde(default)]
    pub did: Option<String>,
    #[serde(default)]
    pub synotoken: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyFileList {
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub total: u64,
    #[serde(default, alias = "shares")]
    pub files: Vec<SynologyFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyFile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub isdir: bool,
    #[serde(default)]
    pub additional: SynologyFileAdditional,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SynologyFileAdditional {
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub time: SynologyFileTime,
    #[serde(default)]
    pub r#type: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SynologyFileTime {
    #[serde(default)]
    pub atime: u64,
    #[serde(default)]
    pub ctime: u64,
    #[serde(default)]
    pub mtime: u64,
    #[serde(default)]
    pub crtime: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologySearchTask {
    pub taskid: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyLibraryList {
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub libraries: Vec<SynologyLibrary>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyLibrary {
    pub id: i64,
    #[serde(default)]
    pub is_public: bool,
    #[serde(default)]
    pub title: String,
    #[serde(rename = "type", default)]
    pub library_type: String,
    #[serde(default)]
    pub visible: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyMovieList {
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub movies: Vec<SynologyMovie>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyMovie {
    #[serde(flatten)]
    pub metadata: SynologyVideoMetadata,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyTvShowList {
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub tvshows: Vec<SynologyTvShow>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyTvShow {
    #[serde(flatten)]
    pub metadata: SynologyVideoMetadata,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyEpisodeList {
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub episodes: Vec<SynologyEpisode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyEpisode {
    #[serde(flatten)]
    pub metadata: SynologyVideoMetadata,
    #[serde(default)]
    pub tvshow_id: i64,
    #[serde(default)]
    pub season: u32,
    #[serde(default)]
    pub episode: u32,
    #[serde(default)]
    pub tvshow_backdrop_mtime: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyHomeVideoList {
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub total: u64,
    #[serde(default, alias = "home_videos", alias = "videos")]
    pub homevideos: Vec<SynologyHomeVideo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyHomeVideo {
    #[serde(flatten)]
    pub metadata: SynologyVideoMetadata,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyTvRecordingList {
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub total: u64,
    #[serde(default, alias = "recordings", alias = "tv_records")]
    pub tv_recordings: Vec<SynologyTvRecording>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyTvRecording {
    #[serde(flatten)]
    pub metadata: SynologyVideoMetadata,
    #[serde(default)]
    pub channel_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyVideoMetadata {
    pub id: i64,
    #[serde(default)]
    pub library_id: i64,
    #[serde(default)]
    pub mapper_id: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub sort_title: String,
    #[serde(default)]
    pub tagline: String,
    #[serde(default)]
    pub certificate: String,
    #[serde(default)]
    pub original_available: Option<String>,
    #[serde(default)]
    pub create_time: i64,
    #[serde(default)]
    pub last_watched: i64,
    #[serde(default)]
    pub rating: i32,
    #[serde(default)]
    pub additional: SynologyVideoAdditional,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SynologyVideoAdditional {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub actor: Vec<String>,
    #[serde(default)]
    pub director: Vec<String>,
    #[serde(default)]
    pub writer: Vec<String>,
    #[serde(default)]
    pub genre: Vec<String>,
    #[serde(default)]
    pub file: Vec<SynologyVideoFile>,
    #[serde(default)]
    pub poster_mtime: Option<String>,
    #[serde(default)]
    pub backdrop_mtime: Option<String>,
    #[serde(default)]
    pub watched_ratio: f64,
    #[serde(default)]
    pub is_parental_controlled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyVideoFile {
    pub id: i64,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub sharepath: String,
    #[serde(default)]
    pub filesize: u64,
    #[serde(default)]
    pub duration: String,
    #[serde(default)]
    pub position: u64,
    #[serde(default)]
    pub watched_ratio: f64,
    #[serde(default)]
    pub resolutionx: u32,
    #[serde(default)]
    pub resolutiony: u32,
    #[serde(default)]
    pub display_x: u32,
    #[serde(default)]
    pub display_y: u32,
    #[serde(default)]
    pub video_codec: String,
    #[serde(default)]
    pub audio_codec: String,
    #[serde(default)]
    pub container_type: String,
    #[serde(default)]
    pub video_bitrate: u64,
    #[serde(default)]
    pub audio_bitrate: u64,
    #[serde(default)]
    pub frame_bitrate: u64,
    #[serde(default)]
    pub frame_rate_num: u64,
    #[serde(default)]
    pub frame_rate_den: u64,
    #[serde(default)]
    pub channel: u32,
    #[serde(default)]
    pub frequency: u32,
    #[serde(default)]
    pub conversion_produced: bool,
}

impl SynologyVideoFile {
    #[must_use]
    pub fn duration_seconds(&self) -> Option<u64> {
        let mut total = 0_u64;
        for part in self.duration.split(':') {
            total = total.checked_mul(60)?.checked_add(part.parse().ok()?)?;
        }
        Some(total)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyAudioTrackList {
    #[serde(default)]
    pub trackinfo: Vec<SynologyAudioTrack>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyAudioTrack {
    pub id: i64,
    #[serde(default)]
    pub track: i64,
    #[serde(default, alias = "lang")]
    pub language: String,
    #[serde(default)]
    pub streamid: i64,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub bitrate: u64,
    #[serde(default)]
    pub sample_rate: u32,
    #[serde(default)]
    pub channel: u32,
    #[serde(default)]
    pub channel_layout: String,
    #[serde(default)]
    pub codec: String,
    #[serde(default)]
    pub codec_raw: String,
    #[serde(default)]
    pub frequency: u32,
    #[serde(default)]
    pub profile: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologySubtitle {
    #[serde(deserialize_with = "deserialize_string")]
    pub id: String,
    #[serde(default)]
    pub embedded: bool,
    #[serde(default)]
    pub format: String,
    #[serde(default, alias = "language")]
    pub lang: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub need_preview: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynologyStreamSession {
    pub stream_id: String,
    pub format: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynologyStreamProfile {
    Raw,
    HlsRemux,
    HlsMedium,
    HlsLow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynologyVideoItemKind {
    Movie,
    Episode,
    HomeVideo,
    TvRecording,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StringValue {
    String(String),
    Signed(i64),
    Unsigned(u64),
}

fn deserialize_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match StringValue::deserialize(deserializer)? {
        StringValue::String(value) => value,
        StringValue::Signed(value) => value.to_string(),
        StringValue::Unsigned(value) => value.to_string(),
    })
}

#[derive(Debug, Serialize)]
pub(crate) struct SynologyStreamFile {
    pub id: i64,
    pub path: &'static str,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SynologyEnvelope<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<SynologyApiError>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SynologyApiError {
    #[serde(default)]
    pub code: i64,
}
