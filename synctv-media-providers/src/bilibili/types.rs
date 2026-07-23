//! Bilibili API Data Structures
#![allow(clippy::doc_markdown)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BilibiliVideoListItem {
    pub bvid: String,
    pub aid: u64,
    pub cid: u64,
    pub epid: u64,
    pub title: String,
    pub cover: String,
    pub author: String,
    pub description: String,
    pub duration_seconds: u64,
    pub part_count: u32,
    pub published_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BilibiliVideoListPage {
    pub items: Vec<BilibiliVideoListItem>,
    pub total: Option<u64>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BilibiliVideoPart {
    pub bvid: String,
    pub aid: u64,
    pub cid: u64,
    pub page: u32,
    pub title: String,
    pub cover: String,
    pub duration_seconds: u64,
    pub width: u64,
    pub height: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BilibiliVideoParts {
    pub title: String,
    pub author: String,
    pub parts: Vec<BilibiliVideoPart>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub(crate) struct ApiEnvelope<T> {
    #[serde(default)]
    pub code: i32,
    #[serde(default, rename = "message")]
    pub _message: String,
    #[serde(default)]
    pub data: Option<T>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ArchiveOwnerDto {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ArchiveSummaryDto {
    #[serde(default, alias = "id")]
    pub aid: u64,
    #[serde(default, alias = "bv_id")]
    pub bvid: String,
    #[serde(default)]
    pub cid: u64,
    #[serde(default)]
    pub epid: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default, alias = "cover")]
    pub pic: String,
    #[serde(default, alias = "intro")]
    pub desc: String,
    #[serde(default)]
    pub duration: u64,
    #[serde(default, alias = "page")]
    pub videos: u32,
    #[serde(default)]
    pub pubdate: i64,
    #[serde(default)]
    pub ctime: i64,
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub owner: Option<ArchiveOwnerDto>,
    #[serde(default)]
    pub upper: Option<ArchiveOwnerDto>,
    #[serde(default)]
    pub author: String,
}

impl ArchiveSummaryDto {
    pub(crate) fn into_item(self) -> BilibiliVideoListItem {
        let author = self
            .owner
            .or(self.upper)
            .map(|owner| owner.name)
            .filter(|name| !name.is_empty())
            .unwrap_or(self.author);
        BilibiliVideoListItem {
            bvid: self.bvid,
            aid: self.aid,
            cid: self.cid,
            epid: self.epid,
            title: self.title,
            cover: self.pic,
            author,
            description: self.desc,
            duration_seconds: self.duration,
            part_count: self.videos,
            published_at: [self.pubdate, self.ctime, self.created]
                .into_iter()
                .find(|value| *value > 0)
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ListPageDto {
    #[serde(
        default,
        rename = "page",
        alias = "num",
        alias = "pn",
        alias = "page_num"
    )]
    pub _page: u64,
    #[serde(default)]
    pub total: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct PopularListData {
    #[serde(default)]
    pub list: Vec<ArchiveSummaryDto>,
    #[serde(default)]
    pub no_more: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RecommendedArchiveDto {
    #[serde(default, alias = "aid")]
    pub id: u64,
    #[serde(default)]
    pub bvid: String,
    #[serde(default)]
    pub cid: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub pic: String,
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub pubdate: i64,
    #[serde(default)]
    pub owner: Option<ArchiveOwnerDto>,
    #[serde(default)]
    pub goto: String,
}

impl RecommendedArchiveDto {
    pub(crate) fn into_item(self) -> Option<BilibiliVideoListItem> {
        if self.goto != "av" || self.bvid.is_empty() {
            return None;
        }
        Some(BilibiliVideoListItem {
            bvid: self.bvid,
            aid: self.id,
            cid: self.cid,
            epid: 0,
            title: self.title,
            cover: self.pic,
            author: self.owner.map_or_else(String::new, |owner| owner.name),
            description: String::new(),
            duration_seconds: self.duration,
            part_count: 0,
            published_at: self.pubdate,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RecommendedListData {
    #[serde(default)]
    pub item: Vec<RecommendedArchiveDto>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UpVideoListData {
    #[serde(default)]
    pub list: UpVideoList,
    #[serde(default)]
    pub page: ListPageDto,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UpVideoList {
    #[serde(default)]
    pub vlist: Vec<UpVideoDto>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UpVideoDto {
    #[serde(default)]
    pub aid: u64,
    #[serde(default)]
    pub bvid: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub pic: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub length: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub created: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct FavoriteListData {
    #[serde(default)]
    pub medias: Vec<ArchiveSummaryDto>,
    #[serde(default)]
    pub has_more: bool,
    #[serde(default)]
    pub info: FavoriteInfoDto,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct FavoriteInfoDto {
    #[serde(default)]
    pub media_count: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ArchivePageData {
    #[serde(default)]
    pub archives: Vec<ArchiveSummaryDto>,
    #[serde(default)]
    pub page: ListPageDto,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct WatchLaterData {
    #[serde(default)]
    pub list: Vec<ArchiveSummaryDto>,
    #[serde(default)]
    pub count: u64,
}

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
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub mid: u64,
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
    #[serde(default)]
    pub season_id: u64,
    pub title: String,
    pub cover: String,
    pub actors: String,
    pub episodes: Vec<EpisodeInfo>,
    #[serde(default, rename = "section")]
    pub sections: Vec<PgcSection>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PgcSection {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
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
    #[serde(default)]
    pub audio: Vec<DashAudio>,
    #[serde(default)]
    pub dolby: Option<DashDolby>,
    #[serde(default)]
    pub flac: Option<DashFlac>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DashDolby {
    #[serde(default)]
    pub audio: Vec<DashAudio>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DashFlac {
    #[serde(default)]
    pub audio: Option<DashAudio>,
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
    pub codecid: u32,
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

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(untagged)]
pub(crate) enum FlexibleU64 {
    Number(u64),
    String(String),
    #[default]
    Null,
}

impl FlexibleU64 {
    pub(crate) fn value(&self) -> u64 {
        match self {
            Self::Number(value) => *value,
            Self::String(value) => value.parse().unwrap_or_default(),
            Self::Null => 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct LiveRoomCard {
    #[serde(default, alias = "room_id")]
    pub roomid: FlexibleU64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub cover: String,
    #[serde(default)]
    pub keyframe: String,
    #[serde(default)]
    pub user_cover: String,
    #[serde(default, alias = "name")]
    pub uname: String,
    #[serde(default)]
    pub uid: FlexibleU64,
    #[serde(default)]
    pub face: String,
    #[serde(default, alias = "parent_area_id")]
    pub area_v2_parent_id: FlexibleU64,
    #[serde(default, alias = "parent_area_name")]
    pub area_v2_parent_name: String,
    #[serde(default, alias = "area_id")]
    pub area_v2_id: FlexibleU64,
    #[serde(default, alias = "area_name")]
    pub area_v2_name: String,
    #[serde(default)]
    pub online: FlexibleU64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct LiveRecommendedData {
    #[serde(default)]
    pub recommend_room_list: Vec<LiveRoomCard>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct LiveRecommendedResp {
    pub code: i32,
    #[serde(default)]
    pub data: LiveRecommendedData,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct LiveFollowingData {
    #[serde(default)]
    pub list: Vec<LiveRoomCard>,
    #[serde(default)]
    pub count: FlexibleU64,
    #[serde(default, rename = "totalPage")]
    pub total_page: FlexibleU64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct LiveFollowingResp {
    pub code: i32,
    #[serde(default)]
    pub data: LiveFollowingData,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct LiveAreaRoomsData {
    #[serde(default)]
    pub list: Vec<LiveRoomCard>,
    #[serde(default)]
    pub count: FlexibleU64,
    #[serde(default)]
    pub has_more: FlexibleU64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct LiveAreaRoomsResp {
    pub code: i32,
    #[serde(default)]
    pub data: LiveAreaRoomsData,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct LiveAreaChild {
    #[serde(default)]
    pub id: FlexibleU64,
    #[serde(default)]
    pub parent_id: FlexibleU64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub parent_name: String,
    #[serde(default)]
    pub pic: String,
    #[serde(default)]
    pub hot_status: FlexibleU64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct LiveAreaParent {
    #[serde(default)]
    pub id: FlexibleU64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub list: Vec<LiveAreaChild>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct LiveAreasResp {
    pub code: i32,
    #[serde(default)]
    pub data: Vec<LiveAreaParent>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct FavoriteFolderItem {
    pub id: u64,
    #[serde(default)]
    pub attr: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub media_count: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct FavoriteFoldersData {
    #[serde(default)]
    pub list: Vec<FavoriteFolderItem>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct FavoriteFoldersResp {
    pub code: i32,
    #[serde(default)]
    pub data: FavoriteFoldersData,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct FollowedPgcNewEpisode {
    #[serde(default)]
    pub index_show: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct FollowedPgcItem {
    #[serde(default)]
    pub season_id: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub cover: String,
    #[serde(default)]
    pub evaluate: String,
    #[serde(default)]
    pub new_ep: FollowedPgcNewEpisode,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct FollowedPgcData {
    #[serde(default)]
    pub list: Vec<FollowedPgcItem>,
    #[serde(default)]
    pub total: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct FollowedPgcResp {
    pub code: i32,
    #[serde(default)]
    pub data: FollowedPgcData,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct HistoryCursorDto {
    #[serde(default)]
    pub max: u64,
    #[serde(default)]
    pub view_at: i64,
    #[serde(default)]
    pub business: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct HistoryTargetDto {
    #[serde(default)]
    pub oid: u64,
    #[serde(default)]
    pub epid: u64,
    #[serde(default)]
    pub bvid: String,
    #[serde(default)]
    pub cid: u64,
    #[serde(default)]
    pub business: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct HistoryItemDto {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub long_title: String,
    #[serde(default)]
    pub cover: String,
    #[serde(default)]
    pub author_name: String,
    #[serde(default)]
    pub view_at: i64,
    #[serde(default)]
    pub progress: i64,
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub live_status: u32,
    #[serde(default)]
    pub history: HistoryTargetDto,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct HistoryDataDto {
    #[serde(default)]
    pub cursor: HistoryCursorDto,
    #[serde(default)]
    pub list: Vec<HistoryItemDto>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct TimelineEpisodeDto {
    #[serde(default)]
    pub episode_id: u64,
    #[serde(default)]
    pub season_id: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub pub_index: String,
    #[serde(default)]
    pub cover: String,
    #[serde(default)]
    pub ep_cover: String,
    #[serde(default)]
    pub pub_ts: i64,
    #[serde(default)]
    pub published: u32,
    #[serde(default)]
    pub delay: u32,
    #[serde(default)]
    pub delay_reason: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct TimelineDayDto {
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub day_of_week: u32,
    #[serde(default)]
    pub episodes: Vec<TimelineEpisodeDto>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct TimelineResp {
    pub code: i32,
    #[serde(default)]
    pub result: Vec<TimelineDayDto>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct SeasonIndexFirstEpisodeDto {
    #[serde(default)]
    pub cover: String,
    #[serde(default)]
    pub ep_id: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct SeasonIndexItemDto {
    #[serde(default)]
    pub season_id: u64,
    #[serde(default)]
    pub media_id: u64,
    #[serde(default)]
    pub title: String,
    #[serde(rename = "subTitle", default)]
    pub subtitle: String,
    #[serde(default)]
    pub cover: String,
    #[serde(default)]
    pub badge: String,
    #[serde(default)]
    pub index_show: String,
    #[serde(default)]
    pub score: String,
    #[serde(default)]
    pub is_finish: u32,
    #[serde(default)]
    pub season_type: u32,
    #[serde(default)]
    pub first_ep: SeasonIndexFirstEpisodeDto,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct SeasonIndexDataDto {
    #[serde(default)]
    pub has_next: u32,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub list: Vec<SeasonIndexItemDto>,
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
    #[serde(default)]
    pub g_qn_desc: Vec<LiveQualityDescription>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LiveQualityDescription {
    #[serde(default)]
    pub qn: u64,
    #[serde(default)]
    pub desc: String,
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
    pub format_name: String,
    #[serde(default)]
    pub codec: Vec<CodecEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CodecEntry {
    #[serde(default)]
    pub codec_name: String,
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
