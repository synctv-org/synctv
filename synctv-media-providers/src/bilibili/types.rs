//! Bilibili API Data Structures
#![allow(clippy::doc_markdown)]

use serde::{Deserialize, Serialize};

/// Video ID types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoId {
    Bvid(String),
    Aid(u64),
}

/// Anime episode ID
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeId(pub String);

/// Quality levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    #[serde(rename = "1080P")]
    P1080 = 80,
    #[serde(rename = "720P")]
    P720 = 64,
    #[serde(rename = "480P")]
    P480 = 32,
    #[serde(rename = "360P")]
    P360 = 16,
}

impl Quality {
    #[must_use]
    pub const fn to_qn(&self) -> u32 {
        *self as u32
    }

    #[must_use]
    pub const fn from_qn(qn: u32) -> Self {
        match qn {
            80 => Self::P1080,
            64 => Self::P720,
            32 => Self::P480,
            _ => Self::P360,
        }
    }

    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::P1080 => "1080P",
            Self::P720 => "720P",
            Self::P480 => "480P",
            Self::P360 => "360P",
        }
    }
}

/// Video information
#[derive(Debug, Clone, Deserialize)]
pub struct VideoInfo {
    pub bvid: String,
    pub aid: u64,
    pub cid: u64,
    pub title: String,
    pub desc: String,
    pub pic: String,
    pub duration: u64,
}

/// Playback URL information
#[derive(Debug, Clone, Deserialize)]
pub struct PlayUrlInfo {
    pub durl: Vec<DurlItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DurlItem {
    pub url: String,
    pub size: u64,
}

/// Anime information
#[derive(Debug, Clone, Deserialize)]
pub struct AnimeInfo {
    pub season_id: u64,
    pub ep_id: u64,
    pub cid: u64,
    pub title: String,
    pub cover: String,
}

// Typed API Response Types (for get_video_info, get_play_url, get_anime_info)

/// Typed response for video_info API (`/x/web-interface/view`)
#[derive(Debug, Clone, Deserialize)]
pub struct VideoInfoResp {
    #[serde(default)]
    pub code: i64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub data: Option<VideoInfoData>,
}

/// Data payload for video_info response
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct VideoInfoData {
    pub bvid: String,
    pub aid: u64,
    pub title: String,
    pub desc: String,
    pub duration: u64,
    pub pic: String,
    pub pages: Vec<VideoInfoPage>,
}

/// Page info within video_info response
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct VideoInfoPage {
    pub cid: u64,
    pub page: u32,
    pub part: String,
}

/// Typed response for play_url API (`/x/player/playurl`)
#[derive(Debug, Clone, Deserialize)]
pub struct PlayUrlResp {
    #[serde(default)]
    pub code: i64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub data: Option<PlayUrlData>,
}

/// Data payload for play_url response
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct PlayUrlData {
    pub accept_quality: Vec<i64>,
    pub quality: i64,
    pub durl: Vec<PlayUrlDurlItem>,
}

/// Individual durl item from play_url response
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct PlayUrlDurlItem {
    pub url: String,
}

/// Typed response for anime/PGC API (`/pgc/view/web/season`)
#[derive(Debug, Clone, Deserialize)]
pub struct AnimeInfoResp {
    #[serde(default)]
    pub code: i64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub result: Option<AnimeInfoResult>,
}

/// Result payload for anime info response
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct AnimeInfoResult {
    pub episodes: Vec<AnimeEpisodeInfo>,
}

/// Episode info within anime info response
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct AnimeEpisodeInfo {
    pub ep_id: u64,
    pub cid: u64,
    pub title: String,
}

// API Response Types

/// QR code login response
#[derive(Debug, Clone, Deserialize)]
pub struct QrcodeResp {
    pub data: QrcodeData,
    pub message: String,
    pub code: i32,
    pub ttl: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QrcodeData {
    pub url: String,
    pub qrcode_key: String,
}

/// Video page info response
#[derive(Debug, Clone, Deserialize)]
pub struct VideoPageInfoResp {
    #[serde(default)]
    pub data: Option<VideoPageData>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub code: i32,
    #[serde(default)]
    pub ttl: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VideoPageData {
    pub title: String,
    pub pic: String,
    pub bvid: String,
    pub aid: u64,
    pub cid: u64,
    pub owner: Owner,
    pub pages: Vec<Page>,
    #[serde(default)]
    pub ugc_season: Option<UgcSeason>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Owner {
    pub name: String,
    pub face: String,
    pub mid: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Page {
    pub cid: u64,
    pub page: u32,
    pub part: String,
    pub duration: u64,
    pub dimension: Dimension,
    #[serde(default)]
    pub first_frame: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Dimension {
    pub width: u64,
    pub height: u64,
    pub rotate: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UgcSeason {
    pub title: String,
    pub cover: String,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Section {
    pub title: String,
    pub episodes: Vec<Episode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Episode {
    pub title: String,
    pub bvid: String,
    pub cid: u64,
    pub aid: u64,
    pub page: EpisodePage,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EpisodePage {
    pub cid: u64,
    pub part: String,
    pub duration: u64,
}

/// Video URL info response
#[derive(Debug, Clone, Deserialize)]
pub struct VideoUrlResp {
    pub data: VideoUrlData,
    pub message: String,
    pub code: i32,
    pub ttl: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VideoUrlData {
    pub accept_quality: Vec<u64>,
    pub accept_description: Vec<String>,
    pub quality: u64,
    pub durl: Vec<DurlInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DurlInfo {
    pub url: String,
    pub size: u64,
    pub length: u64,
    #[serde(default)]
    pub backup_url: Option<Vec<String>>,
}

/// Player v2 info with subtitles
#[derive(Debug, Clone, Deserialize)]
pub struct PlayerV2InfoResp {
    pub data: PlayerV2Data,
    pub message: String,
    pub code: i32,
    pub ttl: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlayerV2Data {
    pub subtitle: SubtitleInfo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubtitleInfo {
    pub subtitles: Vec<SubtitleItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubtitleItem {
    pub lan: String,
    pub lan_doc: String,
    pub subtitle_url: String,
    pub id: i64,
}

/// PGC/Bangumi season info response
#[derive(Debug, Clone, Deserialize)]
pub struct SeasonInfoResp {
    pub result: SeasonResult,
    pub message: String,
    pub code: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeasonResult {
    pub title: String,
    pub cover: String,
    pub actors: String,
    pub episodes: Vec<EpisodeInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EpisodeInfo {
    pub title: String,
    pub long_title: String,
    pub bvid: String,
    pub cid: u64,
    pub ep_id: u64,
    pub aid: u64,
    pub cover: String,
    pub duration: u64,
}

/// PGC URL info response
#[derive(Debug, Clone, Deserialize)]
pub struct PgcUrlResp {
    pub result: PgcUrlResult,
    pub message: String,
    pub code: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PgcUrlResult {
    pub accept_quality: Vec<u64>,
    pub accept_description: Vec<String>,
    pub quality: u64,
    pub durl: Vec<DurlInfo>,
}

/// Quality format descriptor from Bilibili API
#[derive(Debug, Clone, Deserialize)]
pub struct SupportFormat {
    pub quality: u64,
    pub new_description: String,
}

/// DASH format video response
#[derive(Debug, Clone, Deserialize)]
pub struct DashVideoResp {
    #[serde(default)]
    pub data: Option<DashVideoData>,
    pub code: i32,
    pub ttl: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DashVideoData {
    #[serde(default)]
    pub dash: Option<DashInfo>,
    #[serde(default)]
    pub support_formats: Vec<SupportFormat>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DashInfo {
    pub duration: f64,
    #[serde(rename = "minBufferTime")]
    pub min_buffer_time: f64,
    pub video: Vec<DashVideo>,
    pub audio: Vec<DashAudio>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DashVideo {
    pub id: u64,
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    #[serde(default, rename = "backupUrl")]
    pub backup_url: Vec<String>,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub codecs: String,
    pub width: u64,
    pub height: u64,
    #[serde(rename = "frameRate")]
    pub frame_rate: String,
    pub bandwidth: u64,
    #[serde(default)]
    pub sar: String,
    #[serde(default, rename = "startWithSap")]
    pub start_with_sap: u64,
    #[serde(rename = "SegmentBase")]
    pub segment_base: SegmentBase,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DashAudio {
    pub id: u64,
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    #[serde(default, rename = "backupUrl")]
    pub backup_url: Vec<String>,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub codecs: String,
    pub bandwidth: u64,
    #[serde(default, rename = "audioSamplingRate")]
    pub audio_sampling_rate: u32,
    #[serde(default, rename = "startWithSap")]
    pub start_with_sap: u64,
    #[serde(rename = "SegmentBase")]
    pub segment_base: SegmentBase,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SegmentBase {
    #[serde(rename = "Initialization")]
    pub initialization: String,
    #[serde(rename = "indexRange")]
    pub index_range: String,
}

/// DASH format PGC response
#[derive(Debug, Clone, Deserialize)]
pub struct DashPgcResp {
    pub result: DashPgcResult,
    pub message: String,
    pub code: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DashPgcResult {
    #[serde(default)]
    pub code: i32,
    #[serde(default)]
    pub dash: Option<DashInfo>,
    #[serde(default)]
    pub durl: Vec<DurlInfo>,
    #[serde(default)]
    pub support_formats: Vec<SupportFormat>,
}

/// Live page info response
#[derive(Debug, Clone, Deserialize)]
pub struct ParseLivePageResp {
    pub data: LivePageData,
    pub message: String,
    pub code: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LivePageData {
    pub title: String,
    pub user_cover: String,
    pub uid: u64,
    pub room_id: u64,
    pub live_status: u64,
}

/// Live master info response
#[derive(Debug, Clone, Deserialize)]
pub struct GetLiveMasterInfoResp {
    pub data: LiveMasterData,
    pub message: String,
    pub code: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LiveMasterData {
    pub info: LiveMasterInfo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LiveMasterInfo {
    pub uname: String,
    pub face: String,
    pub uid: u64,
}

/// Live stream URL response
#[derive(Debug, Clone, Deserialize)]
pub struct GetLiveStreamResp {
    pub data: LiveStreamData,
    pub message: String,
    pub code: i32,
    pub ttl: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LiveStreamData {
    pub accept_quality: Vec<String>,
    pub quality_description: Vec<QualityDesc>,
    pub durl: Vec<LiveDurl>,
    pub current_quality: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QualityDesc {
    pub desc: String,
    pub qn: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LiveDurl {
    pub url: String,
    pub order: u32,
}

/// Live danmu info response
#[derive(Debug, Clone, Deserialize)]
pub struct GetLiveDanmuInfoResp {
    pub data: LiveDanmuData,
    pub message: String,
    pub code: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LiveDanmuData {
    pub token: String,
    pub host_list: Vec<LiveDanmuHost>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LiveDanmuHost {
    pub host: String,
    pub port: u32,
    pub ws_port: u32,
    pub wss_port: u32,
}

/// User info (Nav) response
#[derive(Debug, Clone, Deserialize)]
pub struct NavResp {
    pub data: NavData,
    pub message: String,
    pub code: i32,
    pub ttl: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NavData {
    #[serde(rename = "isLogin")]
    pub is_login: bool,
    #[serde(default)]
    pub uname: String,
    #[serde(default)]
    pub face: String,
    #[serde(default, rename = "vipStatus")]
    pub vip_status: u32,
    #[serde(default)]
    pub mid: u64,
    #[serde(default)]
    pub wbi_img: Option<WbiImg>,
}

/// WBI image URLs from nav API, used for WBI parameter signing
#[derive(Debug, Clone, Deserialize)]
pub struct WbiImg {
    pub img_url: String,
    pub sub_url: String,
}

// Live Room Play Info (getRoomPlayInfo v2) Response Types

/// Top-level response from `xlive/web-room/v2/index/getRoomPlayInfo`
#[derive(Debug, Clone, Deserialize)]
pub struct RoomPlayInfoResp {
    pub code: i32,
    pub message: String,
    pub data: RoomPlayInfoData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoomPlayInfoData {
    #[serde(default)]
    pub playurl_info: Option<PlayUrlInfoWrapper>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlayUrlInfoWrapper {
    #[serde(default)]
    pub playurl: Option<PlayUrlContainer>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlayUrlContainer {
    #[serde(default)]
    pub stream: Vec<StreamEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamEntry {
    #[serde(default)]
    pub protocol_name: String,
    #[serde(default)]
    pub format: Vec<FormatEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FormatEntry {
    #[serde(default)]
    pub codec: Vec<CodecEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CodecEntry {
    #[serde(default)]
    pub current_qn: u64,
    #[serde(default)]
    pub accept_qn: Vec<u64>,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub url_info: Vec<UrlInfoEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UrlInfoEntry {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub extra: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn dash_video_response_accepts_missing_start_with_sap() -> TestResult {
        let response: DashVideoResp = serde_json::from_value(serde_json::json!({
            "code": 0,
            "message": "0",
            "ttl": 1,
            "data": {
                "dash": {
                    "duration": 120.0,
                    "minBufferTime": 1.5,
                    "video": [{
                        "id": 80,
                        "baseUrl": "https://upos.example/video.m4s",
                        "mimeType": "video/mp4",
                        "codecs": "avc1.640028",
                        "width": 1920,
                        "height": 1080,
                        "frameRate": "30",
                        "bandwidth": 1_000_000,
                        "SegmentBase": {
                            "Initialization": "0-1000",
                            "indexRange": "1001-2000"
                        }
                    }],
                    "audio": [{
                        "id": 30280,
                        "baseUrl": "https://upos.example/audio.m4s",
                        "mimeType": "audio/mp4",
                        "codecs": "mp4a.40.2",
                        "bandwidth": 128_000,
                        "SegmentBase": {
                            "Initialization": "0-999",
                            "indexRange": "1000-1999"
                        }
                    }]
                }
            }
        }))?;

        let data = response.data.expect("DASH data should deserialize");
        let dash = data.dash.expect("DASH streams should deserialize");
        assert_eq!(dash.video[0].start_with_sap, 0);
        assert_eq!(dash.audio[0].start_with_sap, 0);
        Ok(())
    }

    #[test]
    fn dash_video_response_accepts_error_envelope_without_dash() -> TestResult {
        let response: DashVideoResp = serde_json::from_value(serde_json::json!({
            "code": -101,
            "message": "not logged in",
            "ttl": 1,
            "data": {
                "login_mid": 0
            }
        }))?;

        assert_eq!(response.code, -101);
        assert!(response.data.is_some());
        assert!(response
            .data
            .expect("data should deserialize")
            .dash
            .is_none());
        Ok(())
    }
}
