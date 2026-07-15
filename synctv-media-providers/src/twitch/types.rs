use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TwitchResourceKind {
    Channel,
    Video,
    Clip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchResource {
    pub kind: TwitchResourceKind,
    pub id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TwitchSession {
    pub login: Option<String>,
    pub user_id: Option<String>,
    pub client_id: Option<String>,
    pub scopes: Vec<String>,
    pub auth_token: Option<String>,
    pub device_id: Option<String>,
    pub client_integrity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchChatEvent {
    pub id: String,
    pub user_name: String,
    pub text: String,
    pub color: Option<String>,
    pub badges: Vec<String>,
    pub sent_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchSessionIdentity {
    pub client_id: String,
    pub login: String,
    pub user_id: String,
    pub expires_in: Option<u64>,
    pub scopes: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawSessionIdentity {
    pub client_id: String,
    pub login: String,
    pub user_id: String,
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchMetadata {
    pub id: String,
    pub title: String,
    pub author: String,
    pub game: Option<String>,
    pub thumbnail_url: Option<String>,
    pub is_live: bool,
    pub description: Option<String>,
    pub duration_seconds: Option<u64>,
    pub view_count: Option<u64>,
    pub published_at: Option<String>,
    pub chapters: Vec<TwitchChapter>,
    pub storyboard_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchChapter {
    pub title: String,
    pub start_seconds: u64,
    pub end_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchAccessToken {
    pub signature: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchQuality {
    pub name: String,
    pub url: String,
    pub bandwidth: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<String>,
    pub codecs: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchPlayback {
    pub resource: TwitchResource,
    pub master_url: Option<String>,
    pub qualities: Vec<TwitchQuality>,
    pub token: Option<TwitchAccessToken>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TwitchBrowseKind {
    Videos,
    Highlights,
    Uploads,
    Clips,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchBrowseItem {
    pub resource: TwitchResource,
    pub title: String,
    pub thumbnail_url: Option<String>,
    pub duration_seconds: Option<u64>,
    pub view_count: Option<u64>,
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchBrowsePage {
    pub items: Vec<TwitchBrowseItem>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchStreamItem {
    pub stream_id: String,
    pub user_id: String,
    pub channel: String,
    pub display_name: String,
    pub title: String,
    pub category_id: String,
    pub category_name: String,
    pub thumbnail_url: String,
    pub viewer_count: u64,
    pub started_at: String,
    pub language: String,
    pub tags: Vec<String>,
    pub is_mature: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchStreamPage {
    pub items: Vec<TwitchStreamItem>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchCategory {
    pub id: String,
    pub name: String,
    pub box_art_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchCategoryPage {
    pub items: Vec<TwitchCategory>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchChannelSearchItem {
    pub user_id: String,
    pub channel: String,
    pub display_name: String,
    pub title: String,
    pub category_id: String,
    pub category_name: String,
    pub thumbnail_url: String,
    pub is_live: bool,
    pub started_at: String,
    pub language: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchChannelSearchPage {
    pub items: Vec<TwitchChannelSearchItem>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchScheduleSegment {
    pub id: String,
    pub start_time: String,
    pub end_time: String,
    pub title: String,
    pub category_id: Option<String>,
    pub category_name: Option<String>,
    pub canceled_until: Option<String>,
    pub is_recurring: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwitchSchedulePage {
    pub broadcaster_id: String,
    pub broadcaster_login: String,
    pub broadcaster_name: String,
    pub segments: Vec<TwitchScheduleSegment>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct HelixPage<T> {
    pub data: Vec<T>,
    #[serde(default)]
    pub pagination: HelixPagination,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct HelixPagination {
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawHelixStream {
    pub id: String,
    pub user_id: String,
    pub user_login: String,
    pub user_name: String,
    pub game_id: String,
    pub game_name: String,
    pub title: String,
    pub viewer_count: u64,
    pub started_at: String,
    pub language: String,
    pub thumbnail_url: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub is_mature: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawHelixCategory {
    pub id: String,
    pub name: String,
    pub box_art_url: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawHelixChannelSearchItem {
    pub id: String,
    pub broadcaster_login: String,
    pub display_name: String,
    pub title: String,
    pub game_id: String,
    pub game_name: String,
    pub thumbnail_url: String,
    pub is_live: bool,
    pub started_at: String,
    pub broadcaster_language: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawHelixScheduleResponse {
    pub data: RawHelixSchedule,
    #[serde(default)]
    pub pagination: HelixPagination,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawHelixSchedule {
    #[serde(default)]
    pub segments: Option<Vec<RawHelixScheduleSegment>>,
    pub broadcaster_id: String,
    pub broadcaster_name: String,
    pub broadcaster_login: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawHelixScheduleSegment {
    pub id: String,
    pub start_time: String,
    pub end_time: String,
    pub title: String,
    pub canceled_until: Option<String>,
    pub category: Option<RawHelixScheduleCategory>,
    pub is_recurring: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawHelixScheduleCategory {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GraphQlEnvelope<T> {
    pub data: Option<T>,
    #[serde(default)]
    pub errors: Vec<GraphQlError>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GraphQlError {
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccessTokenData {
    pub stream_playback_access_token: Option<RawAccessToken>,
    pub video_playback_access_token: Option<RawAccessToken>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawAccessToken {
    pub signature: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClipData {
    pub clip: Option<RawClip>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawClip {
    pub id: String,
    pub title: String,
    pub broadcaster: Option<RawDisplayName>,
    pub game: Option<RawName>,
    pub thumbnail_url: Option<String>,
    pub duration_seconds: Option<f64>,
    pub view_count: Option<u64>,
    pub created_at: Option<String>,
    #[serde(default)]
    pub video_qualities: Vec<RawClipQuality>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawClipQuality {
    pub frame_rate: Option<f64>,
    pub quality: Option<String>,
    pub source_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawDisplayName {
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawName {
    pub name: String,
}
