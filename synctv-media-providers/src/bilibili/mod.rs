//! Bilibili Provider Client
//!
//! Pure HTTP client for Bilibili API, independent of `MediaProvider`.
//!
//! # Features
//! - Video parsing (BVID/EPID extraction)
//! - Quality selection
//! - Short link resolution
//! - Anti-crawler handling

mod client;
mod service;
mod types;

pub use crate::error::ProviderClientError as BilibiliError;
pub use client::{
    BilibiliClient, BilibiliEndpoints, BilibiliResource, DanmakuMessage, HeartbeatConfig,
    HistoryCursor, HistoryItem, HistoryPage, HistoryResource, LiveDanmakuConnection,
    MatchedBilibiliResource, PgcSeasonIndexItem, PgcSeasonIndexPage, PgcTimelineItem,
    ReconnectConfig, ReconnectResult, ReconnectableLiveDanmakuConnection,
};
pub use service::{BilibiliInterface, BilibiliLiveDanmakuStream, BilibiliService};
pub use types::{
    BilibiliVideoListItem, BilibiliVideoListPage, BilibiliVideoPart, BilibiliVideoParts,
    CodecEntry, DashAudio, DashInfo, DashPgcResp, DashPgcResult, DashVideo, DashVideoData,
    DashVideoResp, Dimension, DurlInfo, Episode, EpisodeInfo, EpisodePage, FormatEntry,
    GetLiveMasterInfoResp, LiveDanmuData, LiveDanmuHost, LiveMasterData, LiveMasterInfo,
    LivePageData, NavData, NavResp, Owner, Page, ParseLivePageResp, PgcUrlResp, PgcUrlResult,
    PlayUrlContainer, PlayUrlInfoWrapper, PlayerV2Data, PlayerV2InfoResp, QrcodeData, QrcodeResp,
    Quality, RoomPlayInfoData, RoomPlayInfoResp, SeasonInfoResp, SeasonResult, Section,
    SegmentBase, StreamEntry, SubtitleInfo, SubtitleItem, SupportFormat, UgcSeason, UrlInfoEntry,
    VideoId, VideoPageData, VideoPageInfoResp, VideoUrlData, VideoUrlResp, WbiImg,
};
