use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HuyaResourceKind {
    Live,
    Video,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HuyaResource {
    pub kind: HuyaResourceKind,
    pub id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HuyaSession {
    pub cookie: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HuyaMetadata {
    pub id: String,
    pub title: String,
    pub author: String,
    pub author_id: Option<String>,
    pub category: Option<String>,
    pub thumbnail_url: Option<String>,
    pub avatar_url: Option<String>,
    pub is_live: bool,
    pub description: Option<String>,
    pub duration_seconds: Option<u64>,
    pub view_count: Option<u64>,
    pub comment_count: Option<u64>,
    pub like_count: Option<u64>,
    pub published_at: Option<i64>,
    pub presenter_uid: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HuyaStreamFormat {
    Flv,
    Hls,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HuyaQuality {
    pub name: String,
    pub cdn: String,
    pub format: HuyaStreamFormat,
    pub url: String,
    pub bitrate: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HuyaPlayback {
    pub resource: HuyaResource,
    pub qualities: Vec<HuyaQuality>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HuyaMedia {
    pub metadata: HuyaMetadata,
    pub playback: HuyaPlayback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HuyaChatIdentity {
    pub presenter_uid: i64,
    pub top_sid: i64,
    pub sub_sid: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HuyaDanmakuEvent {
    pub id: String,
    pub user_id: String,
    pub user_name: String,
    pub text: String,
    pub color: Option<String>,
    pub avatar_url: Option<String>,
    pub sent_at_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WebStreamResponse {
    #[serde(default)]
    pub data: Vec<WebStreamContainer>,
    #[serde(default, rename = "vMultiStreamInfo")]
    pub multi_streams: Vec<RawBitrate>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WebStreamContainer {
    pub game_live_info: RawLiveInfo,
    #[serde(default, rename = "gameStreamInfoList")]
    pub streams: Vec<RawStream>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawLiveInfo {
    #[serde(default, deserialize_with = "deserialize_i64_default")]
    pub uid: i64,
    #[serde(default)]
    pub room_name: String,
    #[serde(default)]
    pub introduction: String,
    #[serde(default)]
    pub nick: String,
    #[serde(default)]
    pub screenshot: String,
    #[serde(default)]
    pub content_intro: String,
    #[serde(default)]
    pub game_full_name: String,
    #[serde(default)]
    pub total_count: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawStream {
    pub s_stream_name: String,
    #[serde(default)]
    pub s_flv_url: String,
    #[serde(default)]
    pub s_flv_url_suffix: String,
    #[serde(default)]
    pub s_flv_anti_code: String,
    #[serde(default)]
    pub s_hls_url: String,
    #[serde(default)]
    pub s_hls_url_suffix: String,
    #[serde(default)]
    pub s_hls_anti_code: String,
    #[serde(default)]
    pub s_cdn_type: String,
    #[serde(default, deserialize_with = "deserialize_i64_default")]
    pub l_presenter_uid: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawBitrate {
    #[serde(default)]
    pub s_display_name: String,
    #[serde(default)]
    pub i_bit_rate: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MomentEnvelope {
    pub status: Option<u32>,
    pub message: Option<String>,
    pub data: Option<MomentData>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MomentData {
    pub moment: Option<RawMoment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawMoment {
    #[serde(default)]
    pub content: String,
    pub comment_count: Option<u64>,
    pub favor_count: Option<u64>,
    pub c_time: Option<i64>,
    pub video_info: RawVideoInfo,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawVideoInfo {
    pub video_title: String,
    #[serde(default)]
    pub category: serde_json::Value,
    #[serde(default)]
    pub video_duration: serde_json::Value,
    #[serde(default)]
    pub video_big_cover: String,
    #[serde(default)]
    pub video_cover: String,
    #[serde(default)]
    pub nick_name: String,
    #[serde(default)]
    pub uid: serde_json::Value,
    pub video_play_num: Option<u64>,
    #[serde(default)]
    pub definitions: Vec<RawDefinition>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawDefinition {
    pub m3u8: Option<String>,
    pub def_name: Option<String>,
    pub height: Option<String>,
    pub width: Option<String>,
    pub definition: Option<String>,
}

fn deserialize_i64_default<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Number(value) => value.as_i64().unwrap_or_default(),
        serde_json::Value::String(value) => value.parse().unwrap_or_default(),
        _ => 0,
    })
}
