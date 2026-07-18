use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DouyinResource {
    Video { aweme_id: String },
    Live { web_rid: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DouyinSession {
    pub cookie: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DouyinMediaKind {
    Video,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DouyinStreamFormat {
    Mp4,
    Flv,
    Hls,
    Dash,
    Cmaf,
    LlHls,
    HttpTs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyinImage {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyinAuthor {
    pub id: String,
    pub sec_uid: String,
    pub unique_id: Option<String>,
    pub nickname: String,
    pub avatar: Option<DouyinImage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyinMetadata {
    pub id: String,
    pub kind: DouyinMediaKind,
    pub title: String,
    pub description: String,
    pub author: DouyinAuthor,
    pub cover: Option<DouyinImage>,
    pub dynamic_cover: Option<DouyinImage>,
    pub duration_ms: Option<u64>,
    pub created_at: Option<i64>,
    pub is_live: bool,
    pub view_count: Option<u64>,
    pub like_count: Option<u64>,
    pub comment_count: Option<u64>,
    pub share_count: Option<u64>,
    pub collect_count: Option<u64>,
    pub music_title: Option<String>,
    pub music_author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyinVariant {
    pub url: String,
    pub format: DouyinStreamFormat,
    pub quality: String,
    pub codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub bitrate: Option<u64>,
    pub audio_only: bool,
    pub headers_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyinMedia {
    pub resource: DouyinResource,
    pub metadata: DouyinMetadata,
    pub room_id: Option<String>,
    pub variants: Vec<DouyinVariant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyinListItem {
    pub aweme_id: String,
    pub title: String,
    pub author: DouyinAuthor,
    pub cover: Option<DouyinImage>,
    pub duration_ms: Option<u64>,
    pub created_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyinListPage {
    pub items: Vec<DouyinListItem>,
    pub cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DouyinDanmakuEvent {
    Chat {
        id: String,
        user_id: String,
        user_name: String,
        text: String,
        color: Option<String>,
        sent_at_ms: Option<u64>,
    },
    StreamClosed {
        action: u64,
        message: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
pub(crate) struct AwemeDetailEnvelope {
    #[serde(default)]
    pub status_code: i64,
    #[serde(default)]
    pub status_msg: String,
    pub aweme_detail: Option<Aweme>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AwemeListEnvelope {
    #[serde(default)]
    pub status_code: i64,
    #[serde(default)]
    pub status_msg: String,
    pub aweme_list: Option<Vec<Aweme>>,
    pub max_cursor: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "boolish")]
    pub has_more: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Aweme {
    #[serde(default)]
    pub aweme_id: String,
    #[serde(default)]
    pub desc: String,
    pub create_time: Option<i64>,
    pub duration: Option<u64>,
    pub author: Option<RawAuthor>,
    pub video: Option<RawVideo>,
    pub statistics: Option<RawStatistics>,
    pub music: Option<RawMusic>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawAuthor {
    #[serde(default)]
    pub uid: String,
    #[serde(default)]
    pub sec_uid: String,
    pub unique_id: Option<String>,
    #[serde(default)]
    pub nickname: String,
    pub avatar_thumb: Option<RawImage>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawImage {
    #[serde(default)]
    pub url_list: Vec<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawVideo {
    pub duration: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub cover: Option<RawImage>,
    pub dynamic_cover: Option<RawImage>,
    pub origin_cover: Option<RawImage>,
    pub play_addr: Option<RawPlayAddress>,
    #[serde(default)]
    pub bit_rate: Vec<RawBitRate>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawPlayAddress {
    #[serde(default)]
    pub url_list: Vec<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub data_size: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawBitRate {
    #[serde(default)]
    pub gear_name: String,
    #[serde(default)]
    pub quality_type: i64,
    pub bit_rate: Option<u64>,
    pub fps: Option<u32>,
    #[serde(default)]
    pub is_bytevc1: i64,
    pub play_addr: Option<RawPlayAddress>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawStatistics {
    pub play_count: Option<u64>,
    pub digg_count: Option<u64>,
    pub comment_count: Option<u64>,
    pub share_count: Option<u64>,
    pub collect_count: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawMusic {
    pub title: Option<String>,
    pub author: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LiveEnvelope {
    #[serde(default)]
    pub data: LiveEnvelopeData,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct LiveEnvelopeData {
    #[serde(default)]
    pub data: Vec<LiveRoom>,
    pub room: Option<LiveRoom>,
    pub user: Option<RawLiveAuthor>,
    pub prompts: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawLiveAuthor {
    #[serde(default)]
    pub id_str: String,
    #[serde(default)]
    pub sec_uid: String,
    #[serde(default)]
    pub nickname: String,
    pub avatar_thumb: Option<RawImage>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct LiveRoom {
    #[serde(default)]
    pub id_str: String,
    #[serde(default)]
    pub status: i64,
    #[serde(default)]
    pub title: String,
    pub cover: Option<RawImage>,
    pub owner: Option<RawLiveAuthor>,
    pub stream_url: Option<RawStreamUrl>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawStreamUrl {
    #[serde(default)]
    pub flv_pull_url: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub hls_pull_url_map: std::collections::HashMap<String, String>,
    pub live_core_sdk_data: Option<RawLiveCoreSdkData>,
    #[serde(default)]
    pub pull_datas: std::collections::HashMap<String, RawPullData>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawLiveCoreSdkData {
    pub pull_data: Option<RawPullData>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawPullData {
    pub options: Option<RawStreamOptions>,
    #[serde(default, deserialize_with = "json_string_or_value")]
    pub stream_data: serde_json::Value,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawStreamOptions {
    #[serde(default)]
    pub qualities: Vec<RawLiveQuality>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawLiveQuality {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub sdk_key: String,
    #[serde(default)]
    pub v_codec: String,
    #[serde(default)]
    pub resolution: String,
    pub v_bit_rate: Option<u64>,
    pub fps: Option<u32>,
}

fn boolish<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_bool().unwrap_or_else(|| {
        value.as_i64().is_some_and(|value| value != 0)
            || value.as_str().is_some_and(|value| value == "1")
    }))
}

fn json_string_or_value<'de, D>(deserializer: D) -> Result<serde_json::Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::String(value) => {
            serde_json::from_str(&value).map_err(serde::de::Error::custom)
        }
        value => Ok(value),
    }
}
