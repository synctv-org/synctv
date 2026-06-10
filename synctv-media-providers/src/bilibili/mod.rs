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
mod error;
mod service;
mod types;

pub use client::{
    BilibiliClient, BilibiliEndpoints, DanmakuMessage, HeartbeatConfig, LiveDanmakuConnection,
    ReconnectConfig, ReconnectResult, ReconnectableLiveDanmakuConnection,
};
pub use error::BilibiliError;
pub use service::{BilibiliInterface, BilibiliService};
pub use types::{
    AnimeEpisodeInfo, AnimeInfo, AnimeInfoResp, AnimeInfoResult, CodecEntry, DashAudio, DashInfo,
    DashPgcResp, DashPgcResult, DashVideo, DashVideoData, DashVideoResp, Dimension, DurlInfo,
    DurlItem, Episode, EpisodeId, EpisodeInfo, EpisodePage, FormatEntry, GetLiveDanmuInfoResp,
    GetLiveMasterInfoResp, GetLiveStreamResp, LiveDanmuData, LiveDanmuHost, LiveDurl,
    LiveMasterData, LiveMasterInfo, LivePageData, LiveStreamData, NavData, NavResp, Owner, Page,
    ParseLivePageResp, PgcUrlResp, PgcUrlResult, PlayUrlContainer, PlayUrlData, PlayUrlDurlItem,
    PlayUrlInfo, PlayUrlInfoWrapper, PlayUrlResp, PlayerV2Data, PlayerV2InfoResp, QrcodeData,
    QrcodeResp, Quality, QualityDesc, RoomPlayInfoData, RoomPlayInfoResp, SeasonInfoResp,
    SeasonResult, Section, SegmentBase, StreamEntry, SubtitleInfo, SubtitleItem, SupportFormat,
    UgcSeason, UrlInfoEntry, VideoId, VideoInfo, VideoInfoData, VideoInfoPage, VideoInfoResp,
    VideoPageData, VideoPageInfoResp, VideoUrlData, VideoUrlResp, WbiImg,
};
