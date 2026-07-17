use serde::{de::Error as _, Deserialize, Deserializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcFunResourceKind {
    Video,
    Bangumi,
    Live,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcFunResource {
    pub kind: AcFunResourceKind,
    pub id: String,
    pub query: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcFunSession {
    pub cookie: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcFunMetadata {
    pub id: String,
    pub title: String,
    pub author: String,
    pub author_id: Option<String>,
    pub category: Option<String>,
    pub thumbnail_url: Option<String>,
    pub avatar_url: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub is_live: bool,
    pub duration_seconds: Option<f64>,
    pub view_count: Option<u64>,
    pub like_count: Option<u64>,
    pub comment_count: Option<u64>,
    pub published_at: Option<i64>,
    pub started_at: Option<i64>,
    pub danmaku_resource_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcFunStreamFormat {
    Hls,
    Flv,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcFunQuality {
    pub name: String,
    pub url: String,
    pub format: AcFunStreamFormat,
    pub bitrate: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub codec: Option<String>,
    pub quality_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcFunPlayback {
    pub resource: AcFunResource,
    pub qualities: Vec<AcFunQuality>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcFunMedia {
    pub metadata: AcFunMetadata,
    pub playback: AcFunPlayback,
    pub live_session: Option<AcFunLiveSession>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcFunLiveSession {
    pub user_id: i64,
    pub author_id: i64,
    pub device_id: String,
    pub security_key: String,
    pub service_token: String,
    pub live_id: String,
    pub tickets: Vec<String>,
    pub enter_room_attach: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcFunDanmaku {
    pub id: String,
    pub user_id: String,
    pub text: String,
    pub color: u32,
    pub position_ms: u64,
    pub created_at_ms: Option<u64>,
    pub mode: u32,
    pub size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcFunLiveDanmakuEvent {
    pub id: String,
    pub user_id: String,
    pub user_name: String,
    pub avatar_url: Option<String>,
    pub text: String,
    pub color: Option<String>,
    pub badge_name: Option<String>,
    pub badge_level: Option<u32>,
    pub sent_at_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VideoPage {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub cover_url: String,
    pub description: Option<String>,
    #[serde(default)]
    pub user: PageUser,
    #[serde(default)]
    pub tag_list: Vec<PageTag>,
    #[serde(default, deserialize_with = "deserialize_optional_u64")]
    pub view_count: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64")]
    pub like_count_show: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64")]
    pub comment_count_show: Option<u64>,
    pub current_video_info: VideoInfo,
    #[serde(default)]
    pub video_list: Vec<VideoPart>,
}

fn deserialize_optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<serde_json::Value>::deserialize(deserializer)? {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(value)) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| D::Error::custom("expected a non-negative integer")),
        Some(serde_json::Value::String(value)) => {
            value.parse::<u64>().map(Some).map_err(D::Error::custom)
        }
        Some(value) => Err(D::Error::custom(format!(
            "expected an integer, numeric string, or null, got {value}"
        ))),
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct PageUser {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub href: String,
    #[serde(default, rename = "avatarImage")]
    pub avatar_image: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PageTag {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VideoPart {
    pub id: serde_json::Value,
    #[serde(default)]
    pub title: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VideoInfo {
    pub id: serde_json::Value,
    #[serde(default)]
    pub title: String,
    pub ks_play_json: String,
    pub duration_millis: Option<u64>,
    pub upload_time: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BangumiPage {
    #[serde(default)]
    pub show_title: String,
    #[serde(default)]
    pub bangumi_title: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub image: String,
    pub comment_count: Option<u64>,
    pub current_video_info: VideoInfo,
    pub hl_video_info: Option<VideoInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlayJson {
    #[serde(default)]
    pub adaptation_set: Vec<AdaptationSet>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AdaptationSet {
    #[serde(default)]
    pub representation: Vec<Representation>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Representation {
    #[serde(default)]
    pub name: String,
    pub url: String,
    pub avg_bitrate: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<serde_json::Value>,
    pub codecs: Option<String>,
    pub quality_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VisitorResponse {
    pub result: i64,
    pub user_id: i64,
    pub ac_security: String,
    #[serde(rename = "acfun.api.visitor_st")]
    pub visitor_st: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct StartPlayResponse {
    pub result: i64,
    pub data: Option<StartPlayData>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StartPlayData {
    pub live_id: String,
    #[serde(default)]
    pub available_tickets: Vec<String>,
    #[serde(default)]
    pub enter_room_attach: String,
    #[serde(default)]
    pub caption: String,
    pub video_play_res: String,
    pub live_start_time: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LivePlayJson {
    #[serde(default)]
    pub live_adaptive_manifest: Vec<LiveManifest>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LiveManifest {
    pub adaptation_set: LiveAdaptationSet,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LiveAdaptationSet {
    #[serde(default)]
    pub representation: Vec<LiveRepresentation>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LiveRepresentation {
    #[serde(default)]
    pub name: String,
    pub url: String,
    pub bitrate: Option<u64>,
    pub quality_type: Option<String>,
    pub media_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LiveInfoResponse {
    #[serde(default)]
    pub user: LiveUser,
    pub title: Option<String>,
    pub cover_urls: Option<Vec<String>>,
    pub create_time: Option<i64>,
    pub online_count: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LiveUser {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub head_url: String,
    pub signature: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DanmakuResponse {
    pub result: i64,
    #[serde(default)]
    pub danmakus: Vec<RawDanmaku>,
    #[serde(default)]
    pub pcursor: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawDanmaku {
    pub danmaku_id: serde_json::Value,
    pub user_id: serde_json::Value,
    #[serde(default)]
    pub body: String,
    pub color: Option<u32>,
    pub position: Option<u64>,
    pub create_time: Option<u64>,
    pub mode: Option<u32>,
    pub size: Option<u32>,
}
