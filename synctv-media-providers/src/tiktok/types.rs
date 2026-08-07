use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TikTokResource {
    Video { video_id: String },
    Live { unique_id: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TikTokSession {
    pub cookie: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TikTokMediaKind {
    Video,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TikTokStreamFormat {
    Mp4,
    Flv,
    Hls,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TikTokImage {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TikTokAuthor {
    pub id: String,
    pub sec_uid: String,
    pub unique_id: String,
    pub nickname: String,
    pub avatar: Option<TikTokImage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TikTokSubtitle {
    pub language: String,
    pub format: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TikTokMetadata {
    pub id: String,
    pub kind: TikTokMediaKind,
    pub title: String,
    pub description: String,
    pub author: TikTokAuthor,
    pub cover: Option<TikTokImage>,
    pub dynamic_cover: Option<TikTokImage>,
    pub duration_ms: Option<u64>,
    pub created_at: Option<i64>,
    pub is_live: bool,
    pub view_count: Option<u64>,
    pub like_count: Option<u64>,
    pub comment_count: Option<u64>,
    pub share_count: Option<u64>,
    pub collect_count: Option<u64>,
    pub concurrent_viewers: Option<u64>,
    pub music_title: Option<String>,
    pub music_author: Option<String>,
    pub subtitles: Vec<TikTokSubtitle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TikTokVariant {
    pub url: String,
    pub format: TikTokStreamFormat,
    pub quality: String,
    pub codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bitrate: Option<u64>,
    pub audio_only: bool,
    pub watermarked: bool,
    pub headers_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TikTokMedia {
    pub resource: TikTokResource,
    pub metadata: TikTokMetadata,
    pub room_id: Option<String>,
    pub variants: Vec<TikTokVariant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TikTokListItem {
    pub video_id: String,
    pub title: String,
    pub author: TikTokAuthor,
    pub cover: Option<TikTokImage>,
    pub duration_ms: Option<u64>,
    pub created_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TikTokListPage {
    pub items: Vec<TikTokListItem>,
    pub cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawItem {
    #[serde(default, alias = "aweme_id")]
    pub id: String,
    #[serde(default)]
    pub desc: String,
    #[serde(default, deserialize_with = "optional_i64_from_any")]
    pub create_time: Option<i64>,
    pub author: Option<RawAuthor>,
    pub video: Option<RawVideo>,
    pub stats: Option<RawStats>,
    pub music: Option<RawMusic>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawAuthor {
    #[serde(default, alias = "uid")]
    pub id: String,
    #[serde(default)]
    pub sec_uid: String,
    #[serde(default)]
    pub unique_id: String,
    #[serde(default)]
    pub nickname: String,
    pub avatar_thumb: Option<String>,
    pub avatar_larger: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawVideo {
    #[serde(
        default,
        rename = "duration",
        deserialize_with = "optional_u64_from_any"
    )]
    pub duration_seconds: Option<u64>,
    #[serde(default, deserialize_with = "optional_u32_from_any")]
    pub width: Option<u32>,
    #[serde(default, deserialize_with = "optional_u32_from_any")]
    pub height: Option<u32>,
    pub cover: Option<String>,
    pub origin_cover: Option<String>,
    pub dynamic_cover: Option<String>,
    pub play_addr: Option<serde_json::Value>,
    pub download_addr: Option<serde_json::Value>,
    #[serde(default)]
    pub bitrate_info: Vec<RawBitrate>,
    #[serde(default)]
    pub subtitle_infos: Vec<RawSubtitle>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawBitrate {
    pub gear_name: Option<String>,
    #[serde(default, deserialize_with = "optional_u64_from_any")]
    pub bit_rate: Option<u64>,
    pub play_addr: Option<RawPlayAddress>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawPlayAddress {
    #[serde(default)]
    pub url_list: Vec<String>,
    pub url_key: Option<String>,
    #[serde(default, deserialize_with = "optional_u64_from_any")]
    pub data_size: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawSubtitle {
    pub language_code_name: Option<String>,
    pub format: Option<String>,
    #[serde(rename = "Url")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawStats {
    #[serde(default, deserialize_with = "optional_u64_from_any")]
    pub play_count: Option<u64>,
    #[serde(default, deserialize_with = "optional_u64_from_any")]
    pub digg_count: Option<u64>,
    #[serde(default, deserialize_with = "optional_u64_from_any")]
    pub comment_count: Option<u64>,
    #[serde(default, deserialize_with = "optional_u64_from_any")]
    pub share_count: Option<u64>,
    #[serde(default, deserialize_with = "optional_u64_from_any")]
    pub collect_count: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawMusic {
    pub title: Option<String>,
    pub author_name: Option<String>,
    pub play_url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawUserDetail {
    pub user_info: Option<RawUserInfo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawUserInfo {
    pub user: Option<RawAuthor>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawListEnvelope {
    #[serde(default)]
    pub item_list: Vec<RawItem>,
    #[serde(default)]
    pub has_more_previous: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawLiveEnvelope {
    #[serde(default, deserialize_with = "optional_i64_from_any")]
    pub status_code: Option<i64>,
    pub message: Option<String>,
    pub data: Option<RawLiveData>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawLiveData {
    pub live_room: Option<RawLiveRoom>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawLiveRoom {
    #[serde(default, deserialize_with = "optional_i64_from_any")]
    pub status: Option<i64>,
    pub stream_id: Option<String>,
    #[serde(default)]
    pub title: String,
    pub stream_data: Option<RawLiveStreamData>,
    pub owner_info: Option<RawAuthor>,
    pub cover_url: Option<String>,
    #[serde(default, deserialize_with = "optional_u64_from_any")]
    pub user_count: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawLiveStreamData {
    pub pull_data: Option<RawLivePullData>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RawLivePullData {
    #[serde(default, deserialize_with = "json_string_or_value")]
    pub stream_data: serde_json::Value,
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

#[derive(Deserialize)]
#[serde(untagged)]
enum SignedNumberValue {
    Number(i64),
    String(String),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum UnsignedNumberValue {
    Number(u64),
    String(String),
}

fn optional_i64_from_any<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<SignedNumberValue>::deserialize(deserializer)?
        .map(|value| match value {
            SignedNumberValue::Number(value) => Ok(value),
            SignedNumberValue::String(value) => {
                value.trim().parse().map_err(serde::de::Error::custom)
            }
        })
        .transpose()
}

fn optional_u64_from_any<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<UnsignedNumberValue>::deserialize(deserializer)?
        .map(|value| match value {
            UnsignedNumberValue::Number(value) => Ok(value),
            UnsignedNumberValue::String(value) => {
                value.trim().parse().map_err(serde::de::Error::custom)
            }
        })
        .transpose()
}

fn optional_u32_from_any<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    optional_u64_from_any(deserializer)?
        .map(|value| value.try_into().map_err(serde::de::Error::custom))
        .transpose()
}
