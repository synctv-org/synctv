use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use url::Url;

use super::media::SourceProvider;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "provider",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum MediaSourceConfig {
    DirectUrl(DirectUrlMediaSourceConfig),
    Bilibili(BilibiliMediaSourceConfig),
    Alist(AlistMediaSourceConfig),
    Emby(EmbyMediaSourceConfig),
    Rtmp(RtmpMediaSourceConfig),
    LiveProxy(LiveProxyMediaSourceConfig),
    Cloudreve(CloudreveMediaSourceConfig),
    Twitch(TwitchMediaSourceConfig),
    Youtube(YoutubeMediaSourceConfig),
    Huya(HuyaMediaSourceConfig),
    Douyu(DouyuMediaSourceConfig),
    Douyin(DouyinMediaSourceConfig),
    AcFun(AcFunMediaSourceConfig),
    Cctv(CctvMediaSourceConfig),
    Fnos(FnosMediaSourceConfig),
    Qnap(QnapMediaSourceConfig),
    Synology(SynologyMediaSourceConfig),
    Nextcloud(NextcloudMediaSourceConfig),
    Seafile(SeafileMediaSourceConfig),
    TrueNas(TrueNasMediaSourceConfig),
    TikTok(TikTokMediaSourceConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "provider",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PlaylistSourceConfig {
    Alist(AlistPlaylistSourceConfig),
    Bilibili(BilibiliPlaylistSourceConfig),
    Emby(EmbyPlaylistSourceConfig),
    Cloudreve(CloudrevePlaylistSourceConfig),
    Twitch(TwitchPlaylistSourceConfig),
    Youtube(YoutubePlaylistSourceConfig),
    Douyin(DouyinPlaylistSourceConfig),
    Fnos(FnosPlaylistSourceConfig),
    Qnap(QnapPlaylistSourceConfig),
    Synology(SynologyPlaylistSourceConfig),
    Nextcloud(NextcloudPlaylistSourceConfig),
    Seafile(SeafilePlaylistSourceConfig),
    TrueNas(TrueNasPlaylistSourceConfig),
    TikTok(TikTokPlaylistSourceConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectUrlMediaSourceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_live: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_proxy: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub medias: Vec<DirectUrlMediaResourceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_media_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtitles: Vec<DirectUrlSubtitleSourceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_subtitle_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub danmakus: Vec<DirectUrlDanmakuSourceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_danmaku_index: Option<usize>,
}

impl DirectUrlMediaSourceConfig {
    /// Build a config wrapping a single unnamed media resource, with no
    /// subtitles or danmaku tracks.
    #[must_use]
    pub fn single(url: String, headers: HashMap<String, String>) -> Self {
        Self {
            is_live: None,
            duration_seconds: None,
            prefer_proxy: None,
            medias: vec![DirectUrlMediaResourceConfig {
                name: String::new(),
                url,
                headers,
                format: String::new(),
            }],
            default_media_index: None,
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            default_danmaku_index: None,
        }
    }

    #[must_use]
    pub fn inferred_live_status(&self) -> Option<bool> {
        self.is_live.or_else(|| {
            self.has_positive_duration().then_some(false).or_else(|| {
                self.default_media()
                    .and_then(DirectUrlMediaResourceConfig::is_file_video)
            })
        })
    }

    #[must_use]
    pub fn positive_duration_seconds(&self) -> Option<f64> {
        self.duration_seconds
            .filter(|duration| duration.is_finite() && *duration > 0.0)
    }

    fn has_positive_duration(&self) -> bool {
        self.positive_duration_seconds().is_some()
    }

    fn default_media(&self) -> Option<&DirectUrlMediaResourceConfig> {
        self.default_media_index
            .and_then(|index| self.medias.get(index))
            .or_else(|| self.medias.first())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectUrlMediaResourceConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
}

impl DirectUrlMediaResourceConfig {
    #[must_use]
    pub fn inferred_format(&self) -> String {
        if self.format.is_empty() {
            detect_direct_url_format(&self.url).to_string()
        } else {
            self.format.clone()
        }
    }

    fn is_file_video(&self) -> Option<bool> {
        let format = self.inferred_format();
        match format.trim().to_ascii_lowercase().as_str() {
            "mp4" | "mkv" | "webm" | "avi" => Some(false),
            _ => None,
        }
    }
}

#[must_use]
pub fn detect_direct_url_format(url: &str) -> &'static str {
    let path = Url::parse(url).map_or_else(|_| url.to_string(), |url| url.path().to_string());
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "m3u8" => "m3u8",
        "mpd" => "mpd",
        "flv" => "flv",
        "mp4" | "m4v" | "mov" => "mp4",
        "mkv" => "mkv",
        "webm" => "webm",
        "avi" => "avi",
        _ => "video",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectUrlSubtitleSourceConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub language: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectUrlDanmakuSourceConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum BilibiliMediaSourceConfig {
    Video(BilibiliVideoSourceConfig),
    Pgc(BilibiliPgcSourceConfig),
    Live(BilibiliLiveSourceConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BilibiliPlaylistSourceConfig {
    pub source: BilibiliPlaylistSource,
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum BilibiliPlaylistSource {
    VideoParts {
        bvid: String,
        aid: Option<u64>,
    },
    Popular,
    Recommended,
    UpVideos {
        mid: u64,
        keyword: String,
    },
    FavoriteVideos {
        media_id: u64,
    },
    CollectionVideos {
        mid: u64,
        season_id: u64,
    },
    SeriesVideos {
        mid: u64,
        series_id: u64,
    },
    WatchLater,
    PgcSeason {
        season_id: u64,
    },
    LiveRecommended,
    LiveFollowed,
    LiveArea {
        parent_area_id: u64,
        area_id: u64,
    },
    History {
        history_type: BilibiliHistoryType,
    },
    PgcTimeline {
        timeline_type: BilibiliPgcTimelineType,
        before_days: u32,
        after_days: u32,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BilibiliHistoryType {
    All,
    Archive,
    Live,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BilibiliPgcTimelineType {
    Anime,
    Cinema,
    Guochuang,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum HuyaMediaSourceConfig {
    Live { room_id: String },
    Video { video_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DouyuMediaSourceConfig {
    pub room: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum DouyinMediaSourceConfig {
    Video {
        aweme_id: String,
        #[serde(default)]
        shared: bool,
    },
    Live {
        web_rid: String,
        #[serde(default)]
        shared: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DouyinPlaylistSourceConfig {
    pub sec_uid: String,
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum TikTokMediaSourceConfig {
    Video {
        video_id: String,
        #[serde(default)]
        shared: bool,
    },
    Live {
        unique_id: String,
        #[serde(default)]
        shared: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TikTokPlaylistSourceConfig {
    pub sec_uid: String,
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum AcFunMediaSourceConfig {
    Video {
        video_id: String,
    },
    Bangumi {
        bangumi_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        episode_query: Option<String>,
    },
    Live {
        author_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CctvMediaSourceConfig {
    pub resource: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FnosMediaSourceConfig {
    pub server_id: String,
    pub source: FnosMediaSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum FnosMediaSource {
    File {
        path: String,
    },
    LibraryItem {
        item_guid: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_guid: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FnosPlaylistSourceConfig {
    pub server_id: String,
    pub source: FnosPlaylistSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum FnosPlaylistSource {
    Files {
        path: String,
    },
    MediaLibrary {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ancestor_guid: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        media_types: Vec<String>,
    },
    Favorites {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        media_types: Vec<String>,
    },
    History,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QnapMediaSourceConfig {
    pub server_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QnapPlaylistSourceConfig {
    pub server_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NextcloudMediaSourceConfig {
    pub server_id: String,
    pub path: String,
    pub file_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NextcloudPlaylistSourceConfig {
    pub server_id: String,
    pub source: NextcloudPlaylistSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum NextcloudPlaylistSource {
    Folder { path: String },
    Favorites,
    Search { path: String, query: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SeafileMediaSourceConfig {
    pub server_id: String,
    pub repository_id: String,
    pub path: String,
    pub object_id: String,
    pub has_thumbnail: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SeafilePlaylistSourceConfig {
    pub server_id: String,
    pub source: SeafilePlaylistSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum SeafilePlaylistSource {
    Folder {
        repository_id: String,
        path: String,
    },
    Starred,
    Search {
        repository_id: String,
        query: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrueNasMediaSourceConfig {
    pub server_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrueNasPlaylistSourceConfig {
    pub server_id: String,
    pub source: TrueNasPlaylistSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum TrueNasPlaylistSource {
    Folder { path: String },
    Search { path: String, query: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SynologyMediaSourceConfig {
    pub server_id: String,
    pub source: SynologyMediaSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum SynologyMediaSource {
    File {
        path: String,
    },
    LibraryItem {
        kind: SynologyLibraryItemKind,
        item_id: i64,
        file_id: i64,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum SynologyLibraryItemKind {
    Movie,
    Episode,
    HomeVideo,
    TvRecording,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SynologyPlaylistSourceConfig {
    pub server_id: String,
    pub source: SynologyPlaylistSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum SynologyPlaylistSource {
    Files { path: String },
    Movies { library_id: i64 },
    TvShows { library_id: i64 },
    Episodes { library_id: i64, tv_show_id: i64 },
    HomeVideos { library_id: i64 },
    TvRecordings { library_id: i64 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BilibiliVideoSourceConfig {
    pub bvid: Option<String>,
    pub aid: Option<u64>,
    pub cid: u64,
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BilibiliPgcSourceConfig {
    pub epid: u64,
    pub cid: u64,
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BilibiliLiveSourceConfig {
    pub room_id: u64,
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlistMediaSourceConfig {
    pub path: String,
    #[serde(default)]
    pub password: Option<String>,
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlistPlaylistSourceConfig {
    pub path: String,
    #[serde(default)]
    pub password: Option<String>,
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudreveMediaSourceConfig {
    pub path: String,
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudrevePlaylistSourceConfig {
    pub path: String,
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum TwitchMediaSourceConfig {
    Live {
        channel: String,
        #[serde(default)]
        shared: bool,
    },
    Video {
        video_id: String,
        #[serde(default)]
        shared: bool,
    },
    Clip {
        slug: String,
        #[serde(default)]
        shared: bool,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TwitchPlaylistContent {
    Videos,
    Highlights,
    Uploads,
    Clips,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum TwitchPlaylistSourceConfig {
    Channel {
        channel: String,
        content: TwitchPlaylistContent,
        #[serde(default)]
        shared: bool,
    },
    FollowedLive {
        #[serde(default)]
        shared: bool,
    },
    CategoryLive {
        category_id: String,
        category_name: String,
        #[serde(default)]
        shared: bool,
    },
    SearchLive {
        query: String,
        #[serde(default)]
        shared: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct YoutubeMediaSourceConfig {
    pub video_id: String,
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum YoutubePlaylistSourceConfig {
    Playlist {
        playlist_id: String,
        #[serde(default)]
        shared: bool,
    },
    Channel {
        channel_id: String,
        content: YoutubeChannelContent,
        #[serde(default)]
        shared: bool,
    },
    Search {
        query: String,
        #[serde(default)]
        shared: bool,
    },
    Trending {
        #[serde(default)]
        shared: bool,
    },
    Subscriptions {
        #[serde(default)]
        shared: bool,
    },
    LikedVideos {
        #[serde(default)]
        shared: bool,
    },
    WatchLater {
        #[serde(default)]
        shared: bool,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum YoutubeChannelContent {
    Videos,
    Shorts,
    Live,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmbyMediaSourceConfig {
    pub item_id: String,
    pub server_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmbyPlaylistSourceConfig {
    pub server_id: String,
    pub source: EmbyPlaylistSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EmbyPlaylistSource {
    Folder {
        item_id: String,
    },
    FavoriteItems {
        item_types: Vec<String>,
    },
    FavoritePeople,
    PersonItems {
        person_id: String,
        item_types: Vec<String>,
    },
    ContinueWatching,
    NextUp,
    RecentlyAdded {
        item_types: Vec<String>,
    },
    Playlists,
    Collections,
    Genres {
        item_types: Vec<String>,
    },
    GenreItems {
        genre_id: String,
        item_types: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RtmpMediaSourceConfig {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LiveProxyMediaSourceConfig {
    pub url: String,
}

impl MediaSourceConfig {
    #[must_use]
    pub const fn provider(&self) -> SourceProvider {
        match self {
            Self::DirectUrl(_) => SourceProvider::DirectUrl,
            Self::Bilibili(_) => SourceProvider::Bilibili,
            Self::Alist(_) => SourceProvider::Alist,
            Self::Emby(_) => SourceProvider::Emby,
            Self::Rtmp(_) => SourceProvider::Rtmp,
            Self::LiveProxy(_) => SourceProvider::LiveProxy,
            Self::Cloudreve(_) => SourceProvider::Cloudreve,
            Self::Twitch(_) => SourceProvider::Twitch,
            Self::Youtube(_) => SourceProvider::Youtube,
            Self::Huya(_) => SourceProvider::Huya,
            Self::Douyu(_) => SourceProvider::Douyu,
            Self::Douyin(_) => SourceProvider::Douyin,
            Self::TikTok(_) => SourceProvider::TikTok,
            Self::AcFun(_) => SourceProvider::AcFun,
            Self::Cctv(_) => SourceProvider::Cctv,
            Self::Fnos(_) => SourceProvider::Fnos,
            Self::Qnap(_) => SourceProvider::Qnap,
            Self::Synology(_) => SourceProvider::Synology,
            Self::Nextcloud(_) => SourceProvider::Nextcloud,
            Self::Seafile(_) => SourceProvider::Seafile,
            Self::TrueNas(_) => SourceProvider::TrueNas,
        }
    }

    pub fn ensure_provider(self, provider: SourceProvider) -> Result<Self, String> {
        if self.provider() == provider {
            Ok(self)
        } else {
            Err(format!(
                "media source_config provider '{}' does not match source_provider '{}'",
                self.provider(),
                provider
            ))
        }
    }
}

impl PlaylistSourceConfig {
    #[must_use]
    pub const fn provider(&self) -> SourceProvider {
        match self {
            Self::Alist(_) => SourceProvider::Alist,
            Self::Bilibili(_) => SourceProvider::Bilibili,
            Self::Emby(_) => SourceProvider::Emby,
            Self::Cloudreve(_) => SourceProvider::Cloudreve,
            Self::Twitch(_) => SourceProvider::Twitch,
            Self::Youtube(_) => SourceProvider::Youtube,
            Self::Douyin(_) => SourceProvider::Douyin,
            Self::TikTok(_) => SourceProvider::TikTok,
            Self::Fnos(_) => SourceProvider::Fnos,
            Self::Qnap(_) => SourceProvider::Qnap,
            Self::Synology(_) => SourceProvider::Synology,
            Self::Nextcloud(_) => SourceProvider::Nextcloud,
            Self::Seafile(_) => SourceProvider::Seafile,
            Self::TrueNas(_) => SourceProvider::TrueNas,
        }
    }

    pub fn ensure_provider(self, provider: SourceProvider) -> Result<Self, String> {
        if self.provider() == provider {
            Ok(self)
        } else {
            Err(format!(
                "playlist source_config provider '{}' does not match source_provider '{}'",
                self.provider(),
                provider
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn media_round_trip(config: &MediaSourceConfig, expected: &serde_json::Value) {
        let storage = serde_json::to_value(config).expect("media source config should serialize");
        assert_eq!(&storage, expected);
        let decoded = serde_json::from_value::<MediaSourceConfig>(storage)
            .expect("media source config should deserialize");
        assert_eq!(&decoded, config);
    }

    fn playlist_round_trip(config: &PlaylistSourceConfig, expected: &serde_json::Value) {
        let storage =
            serde_json::to_value(config).expect("playlist source config should serialize");
        assert_eq!(&storage, expected);
        let decoded = serde_json::from_value::<PlaylistSourceConfig>(storage)
            .expect("playlist source config should deserialize");
        assert_eq!(&decoded, config);
    }

    #[test]
    fn direct_url_inferred_live_status_treats_file_video_as_finite() {
        let config = DirectUrlMediaSourceConfig::single(
            "https://example.com/video.mp4?token=m3u8".to_string(),
            HashMap::new(),
        );

        assert_eq!(config.inferred_live_status(), Some(false));
    }

    #[test]
    fn direct_url_inferred_live_status_keeps_manifest_unknown() {
        let config = DirectUrlMediaSourceConfig::single(
            "https://example.com/live.m3u8".to_string(),
            HashMap::new(),
        );

        assert_eq!(config.inferred_live_status(), None);
    }

    #[test]
    fn direct_url_inferred_live_status_uses_default_media() {
        let config = DirectUrlMediaSourceConfig {
            is_live: None,
            duration_seconds: None,
            prefer_proxy: None,
            medias: vec![
                DirectUrlMediaResourceConfig {
                    name: "manifest".to_string(),
                    url: "https://example.com/live.m3u8".to_string(),
                    headers: HashMap::new(),
                    format: String::new(),
                },
                DirectUrlMediaResourceConfig {
                    name: "file".to_string(),
                    url: "https://example.com/video.mp4".to_string(),
                    headers: HashMap::new(),
                    format: String::new(),
                },
            ],
            default_media_index: Some(1),
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            default_danmaku_index: None,
        };

        assert_eq!(config.inferred_live_status(), Some(false));
    }

    #[test]
    fn direct_url_inferred_live_status_honors_explicit_live_flag() {
        let mut config = DirectUrlMediaSourceConfig::single(
            "https://example.com/video.mp4".to_string(),
            HashMap::new(),
        );
        config.is_live = Some(true);

        assert_eq!(config.inferred_live_status(), Some(true));
    }

    #[test]
    fn media_source_configs_round_trip_provider_storage() {
        media_round_trip(
            &MediaSourceConfig::DirectUrl(DirectUrlMediaSourceConfig {
                is_live: Some(false),
                duration_seconds: Some(120.5),
                prefer_proxy: Some(true),
                medias: vec![DirectUrlMediaResourceConfig {
                    name: "1080p".to_string(),
                    url: "https://example.com/video.mp4".to_string(),
                    headers: HashMap::from([("User-Agent".to_string(), "SyncTV".to_string())]),
                    format: "mp4".to_string(),
                }],
                default_media_index: Some(0),
                subtitles: vec![DirectUrlSubtitleSourceConfig {
                    name: "English".to_string(),
                    language: "en".to_string(),
                    url: "https://example.com/subtitle.vtt".to_string(),
                    headers: HashMap::new(),
                    format: "vtt".to_string(),
                }],
                default_subtitle_index: Some(0),
                danmakus: vec![DirectUrlDanmakuSourceConfig {
                    name: "Danmaku".to_string(),
                    url: "https://example.com/danmaku.xml".to_string(),
                    headers: HashMap::new(),
                    format: Some("xml".to_string()),
                }],
                default_danmaku_index: Some(0),
            }),
            &json!({
                "provider": "directUrl",
                "isLive": false,
                "durationSeconds": 120.5,
                "preferProxy": true,
                "medias": [{
                    "name": "1080p",
                    "url": "https://example.com/video.mp4",
                    "headers": {"User-Agent": "SyncTV"},
                    "format": "mp4"
                }],
                "defaultMediaIndex": 0,
                "subtitles": [{
                    "name": "English",
                    "language": "en",
                    "url": "https://example.com/subtitle.vtt",
                    "format": "vtt"
                }],
                "defaultSubtitleIndex": 0,
                "danmakus": [{
                    "name": "Danmaku",
                    "url": "https://example.com/danmaku.xml",
                    "format": "xml"
                }],
                "defaultDanmakuIndex": 0
            }),
        );
        media_round_trip(
            &MediaSourceConfig::Bilibili(BilibiliMediaSourceConfig::Video(
                BilibiliVideoSourceConfig {
                    bvid: Some("BV1234567890".to_string()),
                    aid: None,
                    cid: 42,
                    shared: true,
                },
            )),
            &json!({
                "provider": "bilibili",
                "kind": "video",
                "bvid": "BV1234567890",
                "aid": null,
                "cid": 42,
                "shared": true
            }),
        );
        media_round_trip(
            &MediaSourceConfig::Alist(AlistMediaSourceConfig {
                server_id: "alist-main".to_string(),
                path: "/movies/demo.mkv".to_string(),
                password: None,
            }),
            &json!({
                "provider": "alist",
                "serverId": "alist-main",
                "path": "/movies/demo.mkv",
                "password": null
            }),
        );
        media_round_trip(
            &MediaSourceConfig::Emby(EmbyMediaSourceConfig {
                server_id: "emby-main".to_string(),
                item_id: "item-1".to_string(),
            }),
            &json!({
                "provider": "emby",
                "serverId": "emby-main",
                "itemId": "item-1"
            }),
        );
        media_round_trip(
            &MediaSourceConfig::Rtmp(RtmpMediaSourceConfig {}),
            &json!({
                "provider": "rtmp"
            }),
        );
        media_round_trip(
            &MediaSourceConfig::LiveProxy(LiveProxyMediaSourceConfig {
                url: "rtmp://example.com/live/room".to_string(),
            }),
            &json!({
                "provider": "liveProxy",
                "url": "rtmp://example.com/live/room"
            }),
        );
    }

    #[test]
    fn media_source_config_storage_rejects_obsolete_shapes() {
        let snake_case_provider_error = serde_json::from_value::<MediaSourceConfig>(json!({
            "provider": "direct_url",
            "medias": [{"url": "https://example.com/video.mp4"}]
        }))
        .expect_err("storage uses ProtoJSON lowerCamelCase provider names");
        assert!(
            snake_case_provider_error
                .to_string()
                .contains("unknown variant `direct_url`"),
            "{snake_case_provider_error}"
        );

        let direct_url_error = serde_json::from_value::<MediaSourceConfig>(json!({
            "provider": "directUrl",
            "url": "https://example.com/video.mp4",
            "headers": {"User-Agent": "SyncTV"}
        }))
        .expect_err("direct_url storage requires medias[]");
        let direct_url_error = direct_url_error.to_string();
        assert!(
            direct_url_error.contains("unknown field `url`")
                || direct_url_error.contains("unknown field `headers`"),
            "{direct_url_error}"
        );

        let bilibili_error = serde_json::from_value::<MediaSourceConfig>(json!({
            "provider": "bilibili",
            "type": "video",
            "bvid": "BV1234567890",
            "cid": 42
        }))
        .expect_err("bilibili storage requires kind");
        assert!(
            bilibili_error.to_string().contains("unknown field `type`")
                || bilibili_error.to_string().contains("missing field `kind`"),
            "{bilibili_error}"
        );

        let alist_error = serde_json::from_value::<MediaSourceConfig>(json!({
            "provider": "alist",
            "server_id": "alist-main",
            "path": "/movies/demo.mkv"
        }))
        .expect_err("storage uses ProtoJSON lowerCamelCase field names");
        assert!(
            alist_error
                .to_string()
                .contains("unknown field `server_id`"),
            "{alist_error}"
        );
    }

    #[test]
    fn playlist_source_configs_round_trip_provider_storage() {
        playlist_round_trip(
            &PlaylistSourceConfig::Alist(AlistPlaylistSourceConfig {
                server_id: "alist-main".to_string(),
                path: "/shows".to_string(),
                password: Some("pw".to_string()),
            }),
            &json!({
                "provider": "alist",
                "serverId": "alist-main",
                "path": "/shows",
                "password": "pw"
            }),
        );
        playlist_round_trip(
            &PlaylistSourceConfig::Emby(EmbyPlaylistSourceConfig {
                server_id: "emby-main".to_string(),
                source: EmbyPlaylistSource::Folder {
                    item_id: "folder-1".to_string(),
                },
            }),
            &json!({
                "provider": "emby",
                "serverId": "emby-main",
                "source": {
                    "type": "folder",
                    "itemId": "folder-1"
                }
            }),
        );
    }
}
