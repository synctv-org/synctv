//! Bilibili Service - Complete implementation
//!
//! This is the full HTTP client implementation.
//! Both gRPC server and local usage call this service.

use super::{client::BilibiliClient, BilibiliError};
use super::{DanmakuMessage, HeartbeatConfig, ReconnectConfig, ReconnectResult};
use crate::transport_dto::bilibili::{
    Empty, GetDashPgcurlReq, GetDashPgcurlResp, GetDashVideoUrlReq, GetDashVideoUrlResp,
    GetLiveDanmuInfoReq, GetLiveDanmuInfoResp, GetLiveStreamsReq, GetLiveStreamsResp, GetPgcurlReq,
    GetSubtitlesReq, GetSubtitlesResp, GetVideoUrlReq, ListFavoriteFoldersReq,
    ListFavoriteFoldersResp, ListFollowedPgcReq, ListFollowedPgcResp, ListHistoryReq,
    ListHistoryResp, ListLiveAreasReq, ListLiveAreasResp, ListLiveRoomsReq, ListLiveRoomsResp,
    ListPgcSeasonsReq, ListPgcSeasonsResp, ListPgcTimelineReq, ListPgcTimelineResp,
    ListVideoPartsReq, ListVideoPartsResp, ListVideosReq, ListVideosResp, LoginWithQrCodeReq,
    LoginWithQrCodeResp, LoginWithSmsReq, LoginWithSmsResp, MatchReq, MatchResp, NewCaptchaResp,
    NewQrCodeResp, NewSmsReq, NewSmsResp, ParseLivePageReq, ParsePgcPageReq, ParseVideoPageReq,
    UserInfoReq, UserInfoResp, VideoInfo, VideoPageInfo, VideoSegment as ProtoVideoSegment,
    VideoUrl, WatchBilibiliLiveDanmakuReq,
};
use async_trait::async_trait;
use futures_util::{stream, Stream, StreamExt};
use reqwest::Client;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub const BILIBILI_LIVE_DANMAKU_FORMAT: &str = "synctv-bilibili-live";

fn checked_u32(value: i32, field: &str) -> Result<u32, BilibiliError> {
    u32::try_from(value)
        .map_err(|_| BilibiliError::InvalidConfig(format!("Bilibili {field} must be non-negative")))
}

fn checked_i32(value: u32, field: &str) -> Result<i32, BilibiliError> {
    i32::try_from(value).map_err(|_| {
        BilibiliError::InvalidConfig(format!("Bilibili {field} exceeds the supported range"))
    })
}

pub type BilibiliLiveDanmakuStream = Pin<
    Box<
        dyn Stream<
                Item = Result<
                    crate::transport_dto::bilibili::BilibiliLiveDanmakuEvent,
                    BilibiliError,
                >,
            > + Send
            + 'static,
    >,
>;

/// Unified Bilibili service interface
///
/// This trait defines all Bilibili operations using provider transport DTOs.
#[async_trait]
pub trait BilibiliInterface: Send + Sync {
    async fn new_qr_code(&self, request: Empty) -> Result<NewQrCodeResp, BilibiliError>;

    async fn login_with_qr_code(
        &self,
        request: LoginWithQrCodeReq,
    ) -> Result<LoginWithQrCodeResp, BilibiliError>;

    async fn new_captcha(&self, request: Empty) -> Result<NewCaptchaResp, BilibiliError>;

    async fn new_sms(&self, request: NewSmsReq) -> Result<NewSmsResp, BilibiliError>;

    async fn login_with_sms(
        &self,
        request: LoginWithSmsReq,
    ) -> Result<LoginWithSmsResp, BilibiliError>;

    async fn parse_video_page(
        &self,
        request: ParseVideoPageReq,
    ) -> Result<VideoPageInfo, BilibiliError>;

    async fn get_video_url(&self, request: GetVideoUrlReq) -> Result<VideoUrl, BilibiliError>;

    async fn get_dash_video_url(
        &self,
        request: GetDashVideoUrlReq,
    ) -> Result<GetDashVideoUrlResp, BilibiliError>;

    async fn get_subtitles(
        &self,
        request: GetSubtitlesReq,
    ) -> Result<GetSubtitlesResp, BilibiliError>;

    async fn parse_pgc_page(
        &self,
        request: ParsePgcPageReq,
    ) -> Result<VideoPageInfo, BilibiliError>;

    async fn get_pgcurl(&self, request: GetPgcurlReq) -> Result<VideoUrl, BilibiliError>;

    async fn get_dash_pgcurl(
        &self,
        request: GetDashPgcurlReq,
    ) -> Result<GetDashPgcurlResp, BilibiliError>;

    async fn user_info(&self, request: UserInfoReq) -> Result<UserInfoResp, BilibiliError>;

    async fn r#match(&self, request: MatchReq) -> Result<MatchResp, BilibiliError>;

    async fn get_live_streams(
        &self,
        request: GetLiveStreamsReq,
    ) -> Result<GetLiveStreamsResp, BilibiliError>;

    async fn parse_live_page(
        &self,
        request: ParseLivePageReq,
    ) -> Result<VideoPageInfo, BilibiliError>;

    async fn get_live_danmu_info(
        &self,
        request: GetLiveDanmuInfoReq,
    ) -> Result<GetLiveDanmuInfoResp, BilibiliError>;

    async fn list_videos(&self, request: ListVideosReq) -> Result<ListVideosResp, BilibiliError>;

    async fn list_video_parts(
        &self,
        request: ListVideoPartsReq,
    ) -> Result<ListVideoPartsResp, BilibiliError>;

    async fn list_live_rooms(
        &self,
        request: ListLiveRoomsReq,
    ) -> Result<ListLiveRoomsResp, BilibiliError>;

    async fn list_live_areas(
        &self,
        request: ListLiveAreasReq,
    ) -> Result<ListLiveAreasResp, BilibiliError>;

    async fn list_favorite_folders(
        &self,
        request: ListFavoriteFoldersReq,
    ) -> Result<ListFavoriteFoldersResp, BilibiliError>;

    async fn list_followed_pgc(
        &self,
        request: ListFollowedPgcReq,
    ) -> Result<ListFollowedPgcResp, BilibiliError>;

    async fn list_history(&self, request: ListHistoryReq)
        -> Result<ListHistoryResp, BilibiliError>;

    async fn list_pgc_timeline(
        &self,
        request: ListPgcTimelineReq,
    ) -> Result<ListPgcTimelineResp, BilibiliError>;

    async fn list_pgc_seasons(
        &self,
        request: ListPgcSeasonsReq,
    ) -> Result<ListPgcSeasonsResp, BilibiliError>;

    async fn watch_bilibili_live_danmaku(
        &self,
        request: WatchBilibiliLiveDanmakuReq,
    ) -> Result<BilibiliLiveDanmakuStream, BilibiliError>;
}

/// Bilibili service implementation
///
/// This is the complete implementation that makes actual HTTP calls.
/// Used by both local callers and gRPC server.
pub struct BilibiliService {
    client: Client,
    wbi_state: Arc<super::client::WbiState>,
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
}

impl BilibiliService {
    pub fn new() -> Result<Self, reqwest::Error> {
        let ssrf_guard = synctv_common::ssrf::SsrfGuard::strict_policy();
        let client = crate::build_provider_http_client(ssrf_guard.clone())?;
        Ok(Self {
            client,
            wbi_state: Arc::new(super::client::WbiState::default()),
            ssrf_guard,
        })
    }

    #[must_use]
    pub fn with_client(client: Client) -> Self {
        Self {
            client,
            wbi_state: Arc::new(super::client::WbiState::default()),
            ssrf_guard: synctv_common::ssrf::SsrfGuard::strict_policy(),
        }
    }

    #[must_use]
    pub fn with_client_and_ssrf_guard(
        client: Client,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
    ) -> Self {
        Self {
            client,
            wbi_state: Arc::new(super::client::WbiState::default()),
            ssrf_guard,
        }
    }
}

fn client_from_cookies_and_state(
    client: Client,
    cookies: &HashMap<String, String>,
    wbi_state: Arc<super::client::WbiState>,
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
) -> BilibiliClient {
    if cookies.is_empty() {
        BilibiliClient::new_with_transport(
            client.clone(),
            client,
            super::client::BilibiliEndpoints::default(),
            wbi_state,
            ssrf_guard,
        )
    } else {
        BilibiliClient::with_cookies_and_transport(
            cookies.clone(),
            client.clone(),
            client,
            super::client::BilibiliEndpoints::default(),
            wbi_state,
            ssrf_guard,
        )
    }
}

fn live_danmaku_event_from_message(
    message: DanmakuMessage,
) -> crate::transport_dto::bilibili::BilibiliLiveDanmakuEvent {
    match message {
        DanmakuMessage::Chat {
            user,
            message,
            timestamp,
        } => crate::transport_dto::bilibili::BilibiliLiveDanmakuEvent {
            format: BILIBILI_LIVE_DANMAKU_FORMAT.to_string(),
            event_type: "chat".to_string(),
            r#type: crate::transport_dto::bilibili::BilibiliLiveDanmakuEventType::Chat as i32,
            user,
            message,
            timestamp,
            ..crate::transport_dto::bilibili::BilibiliLiveDanmakuEvent::default()
        },
        DanmakuMessage::UserEnter { user } => {
            crate::transport_dto::bilibili::BilibiliLiveDanmakuEvent {
                format: BILIBILI_LIVE_DANMAKU_FORMAT.to_string(),
                event_type: "user_enter".to_string(),
                r#type: crate::transport_dto::bilibili::BilibiliLiveDanmakuEventType::UserEnter
                    as i32,
                user,
                ..crate::transport_dto::bilibili::BilibiliLiveDanmakuEvent::default()
            }
        }
        DanmakuMessage::Gift {
            user,
            gift_name,
            count,
        } => crate::transport_dto::bilibili::BilibiliLiveDanmakuEvent {
            format: BILIBILI_LIVE_DANMAKU_FORMAT.to_string(),
            event_type: "gift".to_string(),
            r#type: crate::transport_dto::bilibili::BilibiliLiveDanmakuEventType::Gift as i32,
            user,
            gift_name,
            gift_count: count,
            ..crate::transport_dto::bilibili::BilibiliLiveDanmakuEvent::default()
        },
        DanmakuMessage::Heartbeat { online_count } => {
            crate::transport_dto::bilibili::BilibiliLiveDanmakuEvent {
                format: BILIBILI_LIVE_DANMAKU_FORMAT.to_string(),
                event_type: "heartbeat".to_string(),
                r#type: crate::transport_dto::bilibili::BilibiliLiveDanmakuEventType::Heartbeat
                    as i32,
                online_count,
                ..crate::transport_dto::bilibili::BilibiliLiveDanmakuEvent::default()
            }
        }
        DanmakuMessage::Unknown => crate::transport_dto::bilibili::BilibiliLiveDanmakuEvent {
            format: BILIBILI_LIVE_DANMAKU_FORMAT.to_string(),
            event_type: "unknown".to_string(),
            r#type: crate::transport_dto::bilibili::BilibiliLiveDanmakuEventType::Unknown as i32,
            ..crate::transport_dto::bilibili::BilibiliLiveDanmakuEvent::default()
        },
    }
}

/// Parse a proto quality field (0 = unset/default) into an optional `u32` qn.
fn parse_quality(quality: u64) -> Result<Option<u32>, BilibiliError> {
    if quality == 0 {
        return Ok(None);
    }
    u32::try_from(quality)
        .map(Some)
        .map_err(|_| BilibiliError::Parse(format!("quality {quality} exceeds u32 range")))
}

/// Convert a client-layer `VideoUrlInfo` into the proto `VideoUrl`.
fn to_proto_video_url(url_info: super::client::VideoUrlInfo) -> VideoUrl {
    VideoUrl {
        accept_description: url_info.accept_description,
        accept_quality: url_info.accept_quality.into_iter().map(u64::from).collect(),
        current_quality: u64::from(url_info.current_quality),
        url: url_info.url,
        segments: url_info
            .segments
            .into_iter()
            .map(|s| ProtoVideoSegment {
                url: s.url,
                size: s.size,
                duration_millis: s.duration_millis,
                backup_urls: s.backup_urls,
            })
            .collect(),
    }
}

/// Convert client-layer `VideoPageInfo` to proto `VideoPageInfo`
fn to_proto_page_info(page_info: super::client::VideoPageInfo) -> VideoPageInfo {
    VideoPageInfo {
        title: page_info.title,
        actors: page_info.actors.join(", "),
        season_id: page_info.season_id,
        cover: page_info.cover,
        collection: page_info.collection.map(|collection| {
            crate::transport_dto::bilibili::BilibiliCollectionInfo {
                mid: collection.mid,
                season_id: collection.season_id,
                title: collection.title,
                cover: collection.cover,
            }
        }),
        live_started_at: page_info.live_started_at,
        video_infos: page_info
            .video_infos
            .into_iter()
            .map(|v| VideoInfo {
                bvid: v.bvid,
                aid: v.aid,
                cid: v.cid,
                epid: v.epid,
                page: v.page,
                name: v.name,
                cover_image: v.cover_image,
                live: v.live,
                duration_seconds: v.duration_seconds,
                width: v.width,
                height: v.height,
            })
            .collect(),
    }
}

fn to_proto_video_list_item(
    item: super::BilibiliVideoListItem,
) -> crate::transport_dto::bilibili::BilibiliVideoListItem {
    crate::transport_dto::bilibili::BilibiliVideoListItem {
        bvid: item.bvid,
        aid: item.aid,
        cid: item.cid,
        epid: item.epid,
        title: item.title,
        cover: item.cover,
        author: item.author,
        description: item.description,
        duration_seconds: item.duration_seconds,
        part_count: item.part_count,
        published_at: item.published_at,
    }
}

fn live_room_to_proto(
    room: super::client::LiveRoomListItem,
) -> crate::transport_dto::bilibili::BilibiliLiveRoomItem {
    crate::transport_dto::bilibili::BilibiliLiveRoomItem {
        room_id: room.room_id,
        title: room.title,
        cover: room.cover,
        author: room.author,
        author_id: room.author_id,
        author_avatar: room.author_avatar,
        parent_area_id: room.parent_area_id,
        parent_area_name: room.parent_area_name,
        area_id: room.area_id,
        area_name: room.area_name,
        online: room.online,
    }
}

#[async_trait]
impl BilibiliInterface for BilibiliService {
    async fn new_qr_code(&self, _request: Empty) -> Result<NewQrCodeResp, BilibiliError> {
        let client = BilibiliClient::new_with_transport(
            self.client.clone(),
            self.client.clone(),
            super::client::BilibiliEndpoints::default(),
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let (url, key) = client.new_qr_code().await?;
        Ok(NewQrCodeResp { url, key })
    }

    async fn login_with_qr_code(
        &self,
        request: LoginWithQrCodeReq,
    ) -> Result<LoginWithQrCodeResp, BilibiliError> {
        let client = BilibiliClient::new_with_transport(
            self.client.clone(),
            self.client.clone(),
            super::client::BilibiliEndpoints::default(),
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let (raw_status, cookies) = client.login_with_qr_code(&request.key).await?;

        let status = map_qr_status(raw_status);

        Ok(LoginWithQrCodeResp {
            status,
            cookies: cookies.unwrap_or_default(),
        })
    }

    async fn new_captcha(&self, _request: Empty) -> Result<NewCaptchaResp, BilibiliError> {
        let client = BilibiliClient::new_with_transport(
            self.client.clone(),
            self.client.clone(),
            super::client::BilibiliEndpoints::default(),
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let (token, gt, challenge) = client.new_captcha().await?;

        Ok(NewCaptchaResp {
            token,
            gt,
            challenge,
        })
    }

    async fn new_sms(&self, request: NewSmsReq) -> Result<NewSmsResp, BilibiliError> {
        let client = BilibiliClient::new_with_transport(
            self.client.clone(),
            self.client.clone(),
            super::client::BilibiliEndpoints::default(),
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let captcha_key = client
            .new_sms(
                &request.phone,
                &request.token,
                &request.challenge,
                &request.validate,
            )
            .await?;

        Ok(NewSmsResp { captcha_key })
    }

    async fn login_with_sms(
        &self,
        request: LoginWithSmsReq,
    ) -> Result<LoginWithSmsResp, BilibiliError> {
        let client = BilibiliClient::new_with_transport(
            self.client.clone(),
            self.client.clone(),
            super::client::BilibiliEndpoints::default(),
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let cookies = client
            .login_with_sms(&request.phone, &request.code, &request.captcha_key)
            .await?;

        Ok(LoginWithSmsResp { cookies })
    }

    async fn parse_video_page(
        &self,
        request: ParseVideoPageReq,
    ) -> Result<VideoPageInfo, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let page_info = client.parse_video_page(request.aid, &request.bvid).await?;
        Ok(to_proto_page_info(page_info))
    }

    async fn get_video_url(&self, request: GetVideoUrlReq) -> Result<VideoUrl, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let quality = parse_quality(request.quality)?;
        let url_info = client
            .get_video_url(request.aid, &request.bvid, request.cid, quality)
            .await?;

        Ok(to_proto_video_url(url_info))
    }

    async fn get_dash_video_url(
        &self,
        request: GetDashVideoUrlReq,
    ) -> Result<GetDashVideoUrlResp, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let (dash, hevc_dash) = client
            .get_dash_video_url(request.aid, &request.bvid, request.cid)
            .await?;

        Ok(GetDashVideoUrlResp {
            dash: Some((&dash).into()),
            hevc_dash: if hevc_dash.video_streams.is_empty() {
                None
            } else {
                Some((&hevc_dash).into())
            },
        })
    }

    async fn get_subtitles(
        &self,
        request: GetSubtitlesReq,
    ) -> Result<GetSubtitlesResp, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let subtitles = client
            .get_subtitles(request.aid, &request.bvid, request.cid)
            .await?;
        Ok(GetSubtitlesResp { subtitles })
    }

    async fn parse_pgc_page(
        &self,
        request: ParsePgcPageReq,
    ) -> Result<VideoPageInfo, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let page_info = client.parse_pgc_page(request.epid, request.ssid).await?;
        Ok(to_proto_page_info(page_info))
    }

    async fn get_pgcurl(&self, request: GetPgcurlReq) -> Result<VideoUrl, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let quality = parse_quality(request.quality)?;
        let url_info = client
            .get_pgc_url(request.epid, request.cid, quality)
            .await?;

        Ok(to_proto_video_url(url_info))
    }

    async fn get_dash_pgcurl(
        &self,
        request: GetDashPgcurlReq,
    ) -> Result<GetDashPgcurlResp, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let (dash, hevc_dash) = client.get_dash_pgc_url(request.epid, request.cid).await?;

        Ok(GetDashPgcurlResp {
            dash: Some((&dash).into()),
            hevc_dash: if hevc_dash.video_streams.is_empty() {
                None
            } else {
                Some((&hevc_dash).into())
            },
        })
    }

    async fn user_info(&self, request: UserInfoReq) -> Result<UserInfoResp, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let user_info = client.user_info().await?;

        Ok(UserInfoResp {
            is_login: user_info.is_login,
            user_id: user_info.user_id,
            username: user_info.username,
            face: user_info.face,
            is_vip: user_info.is_vip,
        })
    }

    async fn r#match(&self, request: MatchReq) -> Result<MatchResp, BilibiliError> {
        use crate::transport_dto::bilibili::match_resp::Resource;

        let client = client_from_cookies_and_state(
            self.client.clone(),
            &HashMap::new(),
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let matched = client.match_resource(&request.url).await?;
        let resource = match matched.resource {
            super::client::BilibiliResource::Video { bvid, aid, page } => {
                Resource::Video(crate::transport_dto::bilibili::MatchedVideoResource {
                    bvid,
                    aid,
                    page,
                })
            }
            super::client::BilibiliResource::PgcEpisode { episode_id } => {
                Resource::PgcEpisode(crate::transport_dto::bilibili::MatchedPgcEpisodeResource {
                    episode_id,
                })
            }
            super::client::BilibiliResource::PgcSeason { season_id } => {
                Resource::PgcSeason(crate::transport_dto::bilibili::MatchedPgcSeasonResource {
                    season_id,
                })
            }
            super::client::BilibiliResource::Live { room_id } => {
                Resource::Live(crate::transport_dto::bilibili::MatchedLiveResource { room_id })
            }
            super::client::BilibiliResource::LiveRecommended => Resource::LiveRecommended(
                crate::transport_dto::bilibili::MatchedLiveRecommendedResource {},
            ),
            super::client::BilibiliResource::LiveArea {
                parent_area_id,
                area_id,
            } => Resource::LiveArea(crate::transport_dto::bilibili::MatchedLiveAreaResource {
                parent_area_id,
                area_id,
            }),
            super::client::BilibiliResource::UpVideos { mid } => {
                Resource::UpVideos(crate::transport_dto::bilibili::MatchedUpVideosResource { mid })
            }
            super::client::BilibiliResource::FavoriteVideos { media_id } => {
                Resource::FavoriteVideos(
                    crate::transport_dto::bilibili::MatchedFavoriteVideosResource { media_id },
                )
            }
            super::client::BilibiliResource::CollectionVideos { mid, season_id } => {
                Resource::CollectionVideos(
                    crate::transport_dto::bilibili::MatchedCollectionVideosResource {
                        mid,
                        season_id,
                    },
                )
            }
            super::client::BilibiliResource::SeriesVideos { mid, series_id } => {
                Resource::SeriesVideos(
                    crate::transport_dto::bilibili::MatchedSeriesVideosResource { mid, series_id },
                )
            }
            super::client::BilibiliResource::WatchLater => {
                Resource::WatchLater(crate::transport_dto::bilibili::MatchedWatchLaterResource {})
            }
        };
        Ok(MatchResp {
            normalized_url: matched.normalized_url,
            resource: Some(resource),
        })
    }

    async fn get_live_streams(
        &self,
        request: GetLiveStreamsReq,
    ) -> Result<GetLiveStreamsResp, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let streams = client.get_live_streams(request.cid, request.hls).await?;

        Ok(GetLiveStreamsResp {
            live_streams: streams
                .into_iter()
                .map(|s| crate::transport_dto::bilibili::LiveStream {
                    quality: u64::from(s.quality),
                    urls: s
                        .urls
                        .into_iter()
                        .map(|url| crate::transport_dto::bilibili::LiveStreamUrl {
                            host: url.host,
                            url: url.url,
                            expires_at: url.expires_at,
                        })
                        .collect(),
                    quality_name: s.quality_name,
                    protocol: s.protocol,
                    format: s.format,
                    codec: s.codec,
                })
                .collect(),
        })
    }

    async fn parse_live_page(
        &self,
        request: ParseLivePageReq,
    ) -> Result<VideoPageInfo, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let page_info = client.parse_live_page(request.room_id).await?;
        Ok(to_proto_page_info(page_info))
    }

    async fn get_live_danmu_info(
        &self,
        request: GetLiveDanmuInfoReq,
    ) -> Result<GetLiveDanmuInfoResp, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let danmu_info = client.get_live_danmu_info(request.room_id).await?;

        Ok(GetLiveDanmuInfoResp {
            token: danmu_info.token,
            host_list: danmu_info
                .host_list
                .into_iter()
                .map(
                    |h| crate::transport_dto::bilibili::get_live_danmu_info_resp::Host {
                        host: h.host,
                        port: h.port,
                        wss_port: h.wss_port,
                        ws_port: h.ws_port,
                    },
                )
                .collect(),
        })
    }

    async fn list_videos(&self, request: ListVideosReq) -> Result<ListVideosResp, BilibiliError> {
        use crate::transport_dto::bilibili::list_videos_req::Source;

        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let page = request.page.max(1);
        let page_size = request.page_size.clamp(1, 50);
        let mut result = match request.source.ok_or_else(|| {
            BilibiliError::Parse("Bilibili video list source is required".to_string())
        })? {
            Source::Popular(_) => client.list_popular_videos(page, page_size).await?,
            Source::Recommended(_) => client.list_recommended_videos(page, page_size).await?,
            Source::UpVideos(source) => {
                client
                    .list_up_videos(source.mid, &source.keyword, page, page_size)
                    .await?
            }
            Source::FavoriteVideos(source) => {
                client
                    .list_favorite_videos(source.media_id, page, page_size)
                    .await?
            }
            Source::CollectionVideos(source) => {
                client
                    .list_collection_videos(source.mid, source.season_id, page, page_size)
                    .await?
            }
            Source::SeriesVideos(source) => {
                client
                    .list_series_videos(source.mid, source.series_id, page, page_size)
                    .await?
            }
            Source::WatchLater(_) => client.list_watch_later_videos(page, page_size).await?,
            Source::PgcSeason(source) => {
                let info = client.parse_pgc_page(source.season_id, 0).await?;
                let total = info.video_infos.len();
                let start =
                    usize::try_from(page.saturating_sub(1).saturating_mul(u64::from(page_size)))
                        .unwrap_or(usize::MAX);
                let items = info
                    .video_infos
                    .into_iter()
                    .skip(start)
                    .take(page_size as usize)
                    .map(|video| super::BilibiliVideoListItem {
                        bvid: video.bvid,
                        aid: 0,
                        cid: video.cid,
                        epid: video.epid,
                        title: video.name,
                        cover: video.cover_image,
                        author: info.actors.first().cloned().unwrap_or_default(),
                        description: info.title.clone(),
                        duration_seconds: 0,
                        part_count: 1,
                        published_at: 0,
                    })
                    .collect::<Vec<_>>();
                super::BilibiliVideoListPage {
                    has_more: start.saturating_add(items.len()) < total,
                    items,
                    total: Some(total as u64),
                }
            }
        };

        result.items = stream::iter(result.items)
            .map(|mut item| {
                let client = &client;
                async move {
                    if item.epid == 0 && (item.cid == 0 || item.part_count == 0) {
                        match client.list_video_parts(item.aid, &item.bvid).await {
                            Ok(details) => {
                                item.part_count =
                                    u32::try_from(details.parts.len()).unwrap_or(u32::MAX);
                                if let Some(first) = details.parts.first() {
                                    item.cid = first.cid;
                                    if item.duration_seconds == 0 {
                                        item.duration_seconds = details
                                            .parts
                                            .iter()
                                            .map(|part| part.duration_seconds)
                                            .sum();
                                    }
                                    if item.cover.is_empty() {
                                        item.cover.clone_from(&first.cover);
                                    }
                                }
                                if item.author.is_empty() {
                                    item.author = details.author;
                                }
                            }
                            Err(error) => tracing::warn!(
                                bvid = %item.bvid,
                                aid = item.aid,
                                %error,
                                "failed to enrich Bilibili list item"
                            ),
                        }
                    }
                    item
                }
            })
            .buffered(8)
            .collect()
            .await;

        Ok(ListVideosResp {
            items: result
                .items
                .into_iter()
                .map(to_proto_video_list_item)
                .collect(),
            total: result.total,
            has_more: result.has_more,
        })
    }

    async fn list_video_parts(
        &self,
        request: ListVideoPartsReq,
    ) -> Result<ListVideoPartsResp, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let result = client.list_video_parts(request.aid, &request.bvid).await?;
        Ok(ListVideoPartsResp {
            title: result.title,
            author: result.author,
            parts: result
                .parts
                .into_iter()
                .map(|part| crate::transport_dto::bilibili::BilibiliVideoPart {
                    bvid: part.bvid,
                    aid: part.aid,
                    cid: part.cid,
                    page: part.page,
                    title: part.title,
                    cover: part.cover,
                    duration_seconds: part.duration_seconds,
                    width: part.width,
                    height: part.height,
                })
                .collect(),
        })
    }

    async fn list_live_rooms(
        &self,
        request: ListLiveRoomsReq,
    ) -> Result<ListLiveRoomsResp, BilibiliError> {
        use crate::transport_dto::bilibili::list_live_rooms_req::Source;

        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let page = request.page.max(1);
        let page_size = request.page_size.clamp(1, 50);
        let result = match request.source.ok_or_else(|| {
            BilibiliError::Parse("Bilibili live-room source is required".to_string())
        })? {
            Source::Recommended(_) => client.list_recommended_live_rooms(page, page_size).await?,
            Source::Followed(_) => {
                client
                    .list_followed_live_rooms(page, page_size.min(10))
                    .await?
            }
            Source::Area(source) => {
                client
                    .list_area_live_rooms(source.parent_area_id, source.area_id, page, page_size)
                    .await?
            }
        };
        Ok(ListLiveRoomsResp {
            items: result.items.into_iter().map(live_room_to_proto).collect(),
            total: result.total,
            has_more: result.has_more,
        })
    }

    async fn list_live_areas(
        &self,
        _request: ListLiveAreasReq,
    ) -> Result<ListLiveAreasResp, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &HashMap::new(),
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        Ok(ListLiveAreasResp {
            items: client
                .list_live_areas()
                .await?
                .into_iter()
                .map(
                    |area| crate::transport_dto::bilibili::BilibiliLiveAreaItem {
                        id: area.id,
                        parent_id: area.parent_id,
                        name: area.name,
                        parent_name: area.parent_name,
                        picture: area.picture,
                        hot: area.hot,
                    },
                )
                .collect(),
        })
    }

    async fn list_favorite_folders(
        &self,
        request: ListFavoriteFoldersReq,
    ) -> Result<ListFavoriteFoldersResp, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        Ok(ListFavoriteFoldersResp {
            items: client
                .list_favorite_folders()
                .await?
                .into_iter()
                .map(
                    |folder| crate::transport_dto::bilibili::BilibiliFavoriteFolderItem {
                        media_id: folder.media_id,
                        title: folder.title,
                        media_count: folder.media_count,
                        private: folder.private,
                        default_folder: folder.default_folder,
                    },
                )
                .collect(),
        })
    }

    async fn list_followed_pgc(
        &self,
        request: ListFollowedPgcReq,
    ) -> Result<ListFollowedPgcResp, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let result = client
            .list_followed_pgc(request.season_type, request.page, request.page_size)
            .await?;
        Ok(ListFollowedPgcResp {
            items: result
                .items
                .into_iter()
                .map(
                    |season| crate::transport_dto::bilibili::BilibiliFollowedPgcItem {
                        season_id: season.season_id,
                        title: season.title,
                        cover: season.cover,
                        description: season.description,
                        latest_episode: season.latest_episode,
                    },
                )
                .collect(),
            total: result.total,
            has_more: result.has_more,
        })
    }

    async fn list_history(
        &self,
        request: ListHistoryReq,
    ) -> Result<ListHistoryResp, BilibiliError> {
        use crate::transport_dto::bilibili::{
            bilibili_history_item, BilibiliHistoryItem, HistoryCursor, HistoryLiveTarget,
            HistoryPgcTarget, HistoryType, HistoryVideoTarget,
        };

        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let history_type = match HistoryType::try_from(request.r#type).unwrap_or(HistoryType::All) {
            HistoryType::All => "all",
            HistoryType::Archive => "archive",
            HistoryType::Live => "live",
        };
        let cursor = request.cursor.map(|cursor| super::client::HistoryCursor {
            max: cursor.max,
            view_at: cursor.view_at,
            business: cursor.business,
        });
        let page = client
            .list_history(history_type, cursor.as_ref(), request.page_size)
            .await?;
        Ok(ListHistoryResp {
            items: page
                .items
                .into_iter()
                .map(|item| BilibiliHistoryItem {
                    target: Some(match item.resource {
                        super::client::HistoryResource::Video { bvid, aid, cid } => {
                            bilibili_history_item::Target::Video(HistoryVideoTarget {
                                bvid,
                                aid,
                                cid,
                            })
                        }
                        super::client::HistoryResource::Pgc { epid, cid } => {
                            bilibili_history_item::Target::Pgc(HistoryPgcTarget { epid, cid })
                        }
                        super::client::HistoryResource::Live { room_id } => {
                            bilibili_history_item::Target::Live(HistoryLiveTarget { room_id })
                        }
                    }),
                    title: item.title,
                    subtitle: item.subtitle,
                    cover: item.cover,
                    author: item.author,
                    viewed_at: item.viewed_at,
                    progress_seconds: item.progress_seconds,
                    duration_seconds: item.duration_seconds,
                })
                .collect(),
            cursor: page.cursor.map(|cursor| HistoryCursor {
                max: cursor.max,
                view_at: cursor.view_at,
                business: cursor.business,
            }),
            has_more: page.has_more,
        })
    }

    async fn list_pgc_timeline(
        &self,
        request: ListPgcTimelineReq,
    ) -> Result<ListPgcTimelineResp, BilibiliError> {
        let timeline_type = checked_u32(request.r#type, "timeline type")?;
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        Ok(ListPgcTimelineResp {
            items: client
                .list_pgc_timeline(timeline_type, request.before_days, request.after_days)
                .await?
                .into_iter()
                .map(
                    |item| crate::transport_dto::bilibili::BilibiliPgcTimelineItem {
                        episode_id: item.episode_id,
                        season_id: item.season_id,
                        cid: item.cid,
                        title: item.title,
                        episode_title: item.episode_title,
                        cover: item.cover,
                        episode_cover: item.episode_cover,
                        publish_at: item.publish_at,
                        published: item.published,
                        date: item.date,
                        day_of_week: item.day_of_week,
                        delayed: item.delayed,
                        delay_reason: item.delay_reason,
                    },
                )
                .collect(),
        })
    }

    async fn list_pgc_seasons(
        &self,
        request: ListPgcSeasonsReq,
    ) -> Result<ListPgcSeasonsResp, BilibiliError> {
        let season_type = checked_u32(request.r#type, "season type")?;
        let order = checked_u32(request.order, "season order")?;
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        );
        let page = client
            .list_pgc_seasons(
                season_type,
                request.page,
                request.page_size,
                order,
                request.ascending,
                request.finished,
                request.area.as_deref(),
                request.year.as_deref(),
                request.style_id,
            )
            .await?;
        Ok(ListPgcSeasonsResp {
            items: page
                .items
                .into_iter()
                .map(|item| {
                    Ok(crate::transport_dto::bilibili::BilibiliPgcSeasonItem {
                        season_id: item.season_id,
                        media_id: item.media_id,
                        first_episode_id: item.first_episode_id,
                        title: item.title,
                        subtitle: item.subtitle,
                        cover: item.cover,
                        first_episode_cover: item.first_episode_cover,
                        badge: item.badge,
                        progress: item.progress,
                        score: item.score,
                        finished: item.finished,
                        r#type: checked_i32(item.season_type, "season type")?,
                    })
                })
                .collect::<Result<Vec<_>, BilibiliError>>()?,
            total: page.total,
            has_more: page.has_more,
        })
    }

    async fn watch_bilibili_live_danmaku(
        &self,
        request: WatchBilibiliLiveDanmakuReq,
    ) -> Result<BilibiliLiveDanmakuStream, BilibiliError> {
        let client = Arc::new(client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
            self.ssrf_guard.clone(),
        ));
        let mut connection = client
            .connect_live_danmaku_with_reconnect(
                request.room_id,
                ReconnectConfig {
                    max_retries: 8,
                    initial_delay: Duration::from_millis(500),
                    max_delay: Duration::from_secs(10),
                    backoff_multiplier: 2.0,
                },
            )
            .await?;
        let heartbeat_config = HeartbeatConfig {
            interval: Duration::from_secs(20),
        };
        connection.set_heartbeat_config(heartbeat_config);
        if let Some(conn) = connection.connection() {
            conn.start_heartbeat_loop(heartbeat_config).await;
        }

        let stream = stream::unfold(Some(connection), |state| async move {
            let mut connection = state?;
            loop {
                match connection.recv().await {
                    Ok(ReconnectResult::Messages(messages)) => {
                        if messages.is_empty() {
                            continue;
                        }
                        let events = messages
                            .into_iter()
                            .map(live_danmaku_event_from_message)
                            .map(Ok)
                            .collect::<Vec<_>>();
                        return Some((stream::iter(events), Some(connection)));
                    }
                    Ok(ReconnectResult::Reconnected { .. }) => {}
                    Ok(ReconnectResult::Failed { error, .. }) | Err(error) => {
                        connection.stop().await;
                        return Some((stream::iter(vec![Err(error)]), None));
                    }
                }
            }
        })
        .flatten();

        Ok(Box::pin(stream))
    }
}

/// Map Bilibili raw QR code status to proto `QrCodeStatus` enum i32 value.
///
/// Extracted as a standalone function so it can be unit-tested without
/// needing an actual HTTP response.
pub(crate) const fn map_qr_status(raw: u32) -> i32 {
    match raw {
        0 => 4,     // SUCCESS
        86038 => 1, // EXPIRED
        86101 => 2, // NOTSCANNED
        86090 => 3, // SCANNED
        _ => 0,     // UNKNOWN
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport_dto::bilibili::QrCodeStatus;

    #[test]
    fn test_qr_status_success() {
        assert_eq!(map_qr_status(0), QrCodeStatus::Success as i32);
    }

    #[test]
    fn test_qr_status_expired() {
        assert_eq!(map_qr_status(86038), QrCodeStatus::Expired as i32);
    }

    #[test]
    fn test_qr_status_not_scanned() {
        assert_eq!(map_qr_status(86101), QrCodeStatus::Notscanned as i32);
    }

    #[test]
    fn test_qr_status_scanned() {
        assert_eq!(map_qr_status(86090), QrCodeStatus::Scanned as i32);
    }

    #[test]
    fn test_qr_status_unknown() {
        assert_eq!(map_qr_status(99999), QrCodeStatus::Unknown as i32);
    }

    #[tokio::test]
    async fn test_service_reuses_shared_wbi_state(
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let service = BilibiliService::new()?;
        service.wbi_state.reset_for_tests();
        service.wbi_state.set_wbi_key("shared-key".to_string());

        let client_a = super::client_from_cookies_and_state(
            service.client.clone(),
            &std::collections::HashMap::new(),
            service.wbi_state.clone(),
            service.ssrf_guard.clone(),
        );
        let client_b = super::client_from_cookies_and_state(
            service.client.clone(),
            &std::collections::HashMap::new(),
            service.wbi_state.clone(),
            service.ssrf_guard.clone(),
        );

        assert_eq!(
            client_a.shared_wbi_state().get_valid_wbi_key().as_deref(),
            Some("shared-key")
        );
        assert_eq!(
            client_b.shared_wbi_state().get_valid_wbi_key().as_deref(),
            Some("shared-key")
        );
        Ok(())
    }
}
