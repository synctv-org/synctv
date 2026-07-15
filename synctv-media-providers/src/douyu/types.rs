use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyuResource {
    pub room: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DouyuSession {
    pub cookie: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyuMetadata {
    pub room_id: String,
    pub title: String,
    pub author: String,
    pub category: Option<String>,
    pub thumbnail_url: Option<String>,
    pub avatar_url: Option<String>,
    pub is_live: bool,
    pub is_replay: bool,
    pub is_vip: bool,
    pub viewer_count: Option<u64>,
    pub started_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DouyuStreamFormat {
    Flv,
    Hls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DouyuCodec {
    Avc,
    Hevc,
    Aac,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyuQuality {
    pub name: String,
    pub cdn: String,
    pub cdn_name: String,
    pub rate: i64,
    pub bitrate: Option<u64>,
    pub codec: DouyuCodec,
    pub format: DouyuStreamFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyuPlayback {
    pub room_id: String,
    pub qualities: Vec<DouyuQuality>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyuMedia {
    pub metadata: DouyuMetadata,
    pub playback: DouyuPlayback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyuVariant {
    pub url: String,
    pub format: DouyuStreamFormat,
    pub codec: DouyuCodec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DouyuDanmakuEvent {
    pub id: String,
    pub user_id: String,
    pub user_name: String,
    pub text: String,
    pub color: Option<String>,
    pub level: Option<u32>,
    pub badge_name: Option<String>,
    pub badge_level: Option<u32>,
    pub sent_at_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct SignedRequest {
    pub auth: String,
    pub timestamp: u64,
    pub device_id: String,
    pub enc_data: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct EncryptionEnvelope {
    pub error: i64,
    #[serde(default)]
    pub msg: String,
    pub data: Option<EncryptionData>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct EncryptionData {
    pub rand_str: String,
    pub enc_time: u32,
    pub key: String,
    #[serde(deserialize_with = "deserialize_boolish")]
    pub is_special: bool,
    pub enc_data: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BetardEnvelope {
    pub room: Option<BetardRoom>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BetardRoom {
    #[serde(deserialize_with = "deserialize_stringish")]
    pub room_id: String,
    #[serde(default, deserialize_with = "deserialize_stringish_default")]
    pub room_name: String,
    #[serde(default, deserialize_with = "deserialize_stringish_default")]
    pub owner_name: String,
    #[serde(default)]
    pub show_status: u64,
    #[serde(default, rename = "videoLoop")]
    pub video_loop: u64,
    #[serde(default, rename = "isVip")]
    pub is_vip: u64,
    #[serde(default, deserialize_with = "deserialize_stringish_default")]
    pub room_thumb: String,
    #[serde(default)]
    pub avatar: BetardAvatar,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct BetardAvatar {
    #[serde(default)]
    pub big: String,
    #[serde(default)]
    pub middle: String,
    #[serde(default)]
    pub small: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RoomInfoEnvelope {
    pub error: i64,
    pub data: Option<RoomInfoData>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RoomInfoData {
    #[serde(default)]
    pub room_id: String,
    #[serde(default)]
    pub room_thumb: String,
    #[serde(default)]
    pub cate_name: String,
    #[serde(default)]
    pub room_name: String,
    #[serde(default)]
    pub room_status: String,
    #[serde(default)]
    pub start_time: String,
    #[serde(default)]
    pub owner_name: String,
    #[serde(default)]
    pub avatar: String,
    #[serde(default, deserialize_with = "deserialize_u64ish_default")]
    pub online: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PlayEnvelope {
    pub error: i64,
    #[serde(default)]
    pub msg: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PlayData {
    pub room_id: u64,
    #[serde(default)]
    pub rtmp_cdn: String,
    pub rtmp_url: String,
    pub rtmp_live: String,
    #[serde(default, rename = "cdnsWithName")]
    pub cdns: Vec<RawCdn>,
    #[serde(default)]
    pub multirates: Vec<RawRate>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawCdn {
    #[serde(default)]
    pub name: String,
    pub cdn: String,
    #[serde(
        default,
        rename = "isH265",
        deserialize_with = "deserialize_boolish_default"
    )]
    pub is_h265: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawRate {
    #[serde(default)]
    pub name: String,
    #[serde(deserialize_with = "deserialize_i64ish")]
    pub rate: i64,
    #[serde(default, deserialize_with = "deserialize_u64ish_default")]
    pub bit: u64,
}

fn deserialize_boolish<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(matches!(value, serde_json::Value::Bool(true))
        || value.as_i64().is_some_and(|value| value != 0)
        || value
            .as_str()
            .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true")))
}

fn deserialize_boolish_default<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_boolish(deserializer)
}

fn deserialize_stringish<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::String(value) => value,
        serde_json::Value::Number(value) => value.to_string(),
        _ => String::new(),
    })
}

fn deserialize_stringish_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_stringish(deserializer)
}

fn deserialize_i64ish<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .unwrap_or_default())
}

fn deserialize_u64ish_default<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .unwrap_or_default())
}
