//! Bilibili gRPC Server Implementation
//!
//! Thin wrapper around `BilibiliService` that implements gRPC server trait.

use super::bilibili::{
    bilibili_server::Bilibili, Empty, GetDashPgcurlReq, GetDashPgcurlResp, GetDashVideoUrlReq,
    GetDashVideoUrlResp, GetLiveDanmuInfoReq, GetLiveDanmuInfoResp, GetLiveStreamsReq,
    GetLiveStreamsResp, GetPgcurlReq, GetSubtitlesReq, GetSubtitlesResp, GetVideoUrlReq,
    ListFavoriteFoldersReq, ListFavoriteFoldersResp, ListFollowedPgcReq, ListFollowedPgcResp,
    ListHistoryReq, ListHistoryResp, ListLiveAreasReq, ListLiveAreasResp, ListLiveRoomsReq,
    ListLiveRoomsResp, ListPgcSeasonsReq, ListPgcSeasonsResp, ListPgcTimelineReq,
    ListPgcTimelineResp, ListVideoPartsReq, ListVideoPartsResp, ListVideosReq, ListVideosResp,
    LoginWithQrCodeReq, LoginWithQrCodeResp, LoginWithSmsReq, LoginWithSmsResp, MatchReq,
    MatchResp, NewCaptchaResp, NewQrCodeResp, NewSmsReq, NewSmsResp, ParseLivePageReq,
    ParsePgcPageReq, ParseVideoPageReq, UserInfoReq, UserInfoResp, VideoPageInfo, VideoUrl,
    WatchBilibiliLiveDanmakuReq,
};
use super::error_mapper::map_provider_error;
use crate::bilibili::{BilibiliInterface, BilibiliService as BilibiliServiceImpl};
use crate::error::ProviderClientError;
use crate::validation::validate_required;
use futures_util::{Stream, StreamExt};
use tonic::{Request, Response, Status};

/// Map Bilibili provider errors to appropriate gRPC status codes using the shared mapper.
///
/// Bilibili has special API error codes (-101 for auth, -412 for rate limit)
/// that are handled before falling back to the shared mapper.
fn map_bilibili_error(context: &str, e: &ProviderClientError) -> Status {
    // Special handling for Bilibili-specific API error codes
    if let ProviderClientError::Api { code, .. } = &e {
        match code {
            -101 => return Status::unauthenticated(format!("{context}: not logged in")),
            -412 => return Status::resource_exhausted(format!("{context}: rate limited")),
            _ => {}
        }
    }
    map_provider_error(context, e)
}

/// Bilibili gRPC server
///
/// Thin wrapper that delegates to `BilibiliService` for actual implementation.
pub struct BilibiliService {
    service: BilibiliServiceImpl,
}

impl BilibiliService {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            service: BilibiliServiceImpl::new()?,
        })
    }
}

#[tonic::async_trait]
impl Bilibili for BilibiliService {
    type WatchBilibiliLiveDanmakuStream = std::pin::Pin<
        Box<
            dyn Stream<Item = Result<super::bilibili::BilibiliLiveDanmakuEvent, Status>>
                + Send
                + 'static,
        >,
    >;

    async fn new_qr_code(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<NewQrCodeResp>, Status> {
        let req = request.into_inner();
        let resp = self
            .service
            .new_qr_code(req)
            .await
            .map_err(|e| map_bilibili_error("new_qr_code", &e))?;
        Ok(Response::new(resp))
    }

    async fn login_with_qr_code(
        &self,
        request: Request<LoginWithQrCodeReq>,
    ) -> Result<Response<LoginWithQrCodeResp>, Status> {
        let req = request.into_inner();
        validate_required("key", &req.key)?;
        let resp = self
            .service
            .login_with_qr_code(req)
            .await
            .map_err(|e| map_bilibili_error("login_with_qr_code", &e))?;
        Ok(Response::new(resp))
    }

    async fn new_captcha(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<NewCaptchaResp>, Status> {
        let req = request.into_inner();
        let resp = self
            .service
            .new_captcha(req)
            .await
            .map_err(|e| map_bilibili_error("new_captcha", &e))?;
        Ok(Response::new(resp))
    }

    async fn new_sms(&self, request: Request<NewSmsReq>) -> Result<Response<NewSmsResp>, Status> {
        let req = request.into_inner();
        validate_required("phone", &req.phone)?;
        let resp = self
            .service
            .new_sms(req)
            .await
            .map_err(|e| map_bilibili_error("new_sms", &e))?;
        Ok(Response::new(resp))
    }

    async fn login_with_sms(
        &self,
        request: Request<LoginWithSmsReq>,
    ) -> Result<Response<LoginWithSmsResp>, Status> {
        let req = request.into_inner();
        validate_required("phone", &req.phone)?;
        validate_required("code", &req.code)?;
        let resp = self
            .service
            .login_with_sms(req)
            .await
            .map_err(|e| map_bilibili_error("login_with_sms", &e))?;
        Ok(Response::new(resp))
    }

    async fn parse_video_page(
        &self,
        request: Request<ParseVideoPageReq>,
    ) -> Result<Response<VideoPageInfo>, Status> {
        let req = request.into_inner();
        if req.aid == 0 && req.bvid.is_empty() {
            return Err(Status::invalid_argument(
                "either aid or bvid must be provided",
            ));
        }
        let resp = self
            .service
            .parse_video_page(req)
            .await
            .map_err(|e| map_bilibili_error("parse_video_page", &e))?;
        Ok(Response::new(resp))
    }

    async fn get_video_url(
        &self,
        request: Request<GetVideoUrlReq>,
    ) -> Result<Response<VideoUrl>, Status> {
        let req = request.into_inner();
        if req.aid == 0 && req.bvid.is_empty() {
            return Err(Status::invalid_argument(
                "either aid or bvid must be provided",
            ));
        }
        if req.cid == 0 {
            return Err(Status::invalid_argument("cid must not be zero"));
        }
        let resp = self
            .service
            .get_video_url(req)
            .await
            .map_err(|e| map_bilibili_error("get_video_url", &e))?;
        Ok(Response::new(resp))
    }

    async fn get_dash_video_url(
        &self,
        request: Request<GetDashVideoUrlReq>,
    ) -> Result<Response<GetDashVideoUrlResp>, Status> {
        let req = request.into_inner();
        if req.aid == 0 && req.bvid.is_empty() {
            return Err(Status::invalid_argument(
                "either aid or bvid must be provided",
            ));
        }
        if req.cid == 0 {
            return Err(Status::invalid_argument("cid must not be zero"));
        }
        let resp = self
            .service
            .get_dash_video_url(req)
            .await
            .map_err(|e| map_bilibili_error("get_dash_video_url", &e))?;
        Ok(Response::new(resp))
    }

    async fn get_subtitles(
        &self,
        request: Request<GetSubtitlesReq>,
    ) -> Result<Response<GetSubtitlesResp>, Status> {
        let req = request.into_inner();
        if req.aid == 0 && req.bvid.is_empty() {
            return Err(Status::invalid_argument(
                "either aid or bvid must be provided",
            ));
        }
        if req.cid == 0 {
            return Err(Status::invalid_argument("cid must not be zero"));
        }
        let resp = self
            .service
            .get_subtitles(req)
            .await
            .map_err(|e| map_bilibili_error("get_subtitles", &e))?;
        Ok(Response::new(resp))
    }

    async fn parse_pgc_page(
        &self,
        request: Request<ParsePgcPageReq>,
    ) -> Result<Response<VideoPageInfo>, Status> {
        let req = request.into_inner();
        if req.ssid == 0 && req.epid == 0 {
            return Err(Status::invalid_argument(
                "either ssid or epid must be provided",
            ));
        }
        let resp = self
            .service
            .parse_pgc_page(req)
            .await
            .map_err(|e| map_bilibili_error("parse_pgc_page", &e))?;
        Ok(Response::new(resp))
    }

    async fn get_pgcurl(
        &self,
        request: Request<GetPgcurlReq>,
    ) -> Result<Response<VideoUrl>, Status> {
        let req = request.into_inner();
        if req.epid == 0 {
            return Err(Status::invalid_argument("epid must not be zero"));
        }
        if req.cid == 0 {
            return Err(Status::invalid_argument("cid must not be zero"));
        }
        let resp = self
            .service
            .get_pgcurl(req)
            .await
            .map_err(|e| map_bilibili_error("get_pgcurl", &e))?;
        Ok(Response::new(resp))
    }

    async fn get_dash_pgcurl(
        &self,
        request: Request<GetDashPgcurlReq>,
    ) -> Result<Response<GetDashPgcurlResp>, Status> {
        let req = request.into_inner();
        if req.epid == 0 {
            return Err(Status::invalid_argument("epid must not be zero"));
        }
        if req.cid == 0 {
            return Err(Status::invalid_argument("cid must not be zero"));
        }
        let resp = self
            .service
            .get_dash_pgcurl(req)
            .await
            .map_err(|e| map_bilibili_error("get_dash_pgcurl", &e))?;
        Ok(Response::new(resp))
    }

    async fn user_info(
        &self,
        request: Request<UserInfoReq>,
    ) -> Result<Response<UserInfoResp>, Status> {
        let req = request.into_inner();
        if req.cookies.is_empty() {
            return Err(Status::invalid_argument(
                "cookies must not be empty for user_info",
            ));
        }
        let resp = self
            .service
            .user_info(req)
            .await
            .map_err(|e| map_bilibili_error("user_info", &e))?;
        Ok(Response::new(resp))
    }

    async fn r#match(&self, request: Request<MatchReq>) -> Result<Response<MatchResp>, Status> {
        let req = request.into_inner();
        validate_required("url", &req.url)?;
        let resp = self
            .service
            .r#match(req)
            .await
            .map_err(|e| map_bilibili_error("match", &e))?;
        Ok(Response::new(resp))
    }

    async fn get_live_streams(
        &self,
        request: Request<GetLiveStreamsReq>,
    ) -> Result<Response<GetLiveStreamsResp>, Status> {
        let req = request.into_inner();
        if req.cid == 0 {
            return Err(Status::invalid_argument("cid must not be zero"));
        }
        let resp = self
            .service
            .get_live_streams(req)
            .await
            .map_err(|e| map_bilibili_error("get_live_streams", &e))?;
        Ok(Response::new(resp))
    }

    async fn parse_live_page(
        &self,
        request: Request<ParseLivePageReq>,
    ) -> Result<Response<VideoPageInfo>, Status> {
        let req = request.into_inner();
        if req.room_id == 0 {
            return Err(Status::invalid_argument("room_id must not be zero"));
        }
        let resp = self
            .service
            .parse_live_page(req)
            .await
            .map_err(|e| map_bilibili_error("parse_live_page", &e))?;
        Ok(Response::new(resp))
    }

    async fn get_live_danmu_info(
        &self,
        request: Request<GetLiveDanmuInfoReq>,
    ) -> Result<Response<GetLiveDanmuInfoResp>, Status> {
        let req = request.into_inner();
        if req.room_id == 0 {
            return Err(Status::invalid_argument("room_id must not be zero"));
        }
        let resp = self
            .service
            .get_live_danmu_info(req)
            .await
            .map_err(|e| map_bilibili_error("get_live_danmu_info", &e))?;
        Ok(Response::new(resp))
    }

    async fn list_videos(
        &self,
        request: Request<ListVideosReq>,
    ) -> Result<Response<ListVideosResp>, Status> {
        let req = request.into_inner();
        if req.source.is_none() {
            return Err(Status::invalid_argument("source is required"));
        }
        if req.page == 0 {
            return Err(Status::invalid_argument("page must be at least one"));
        }
        if req.page_size == 0 {
            return Err(Status::invalid_argument("page_size must be at least one"));
        }
        let resp = self
            .service
            .list_videos(req)
            .await
            .map_err(|error| map_bilibili_error("list_videos", &error))?;
        Ok(Response::new(resp))
    }

    async fn list_video_parts(
        &self,
        request: Request<ListVideoPartsReq>,
    ) -> Result<Response<ListVideoPartsResp>, Status> {
        let req = request.into_inner();
        if req.aid == 0 && req.bvid.is_empty() {
            return Err(Status::invalid_argument(
                "either aid or bvid must be provided",
            ));
        }
        let resp = self
            .service
            .list_video_parts(req)
            .await
            .map_err(|error| map_bilibili_error("list_video_parts", &error))?;
        Ok(Response::new(resp))
    }

    async fn list_live_rooms(
        &self,
        request: Request<ListLiveRoomsReq>,
    ) -> Result<Response<ListLiveRoomsResp>, Status> {
        let req = request.into_inner();
        if req.source.is_none() {
            return Err(Status::invalid_argument("source is required"));
        }
        if req.page == 0 {
            return Err(Status::invalid_argument("page must be at least one"));
        }
        if req.page_size == 0 {
            return Err(Status::invalid_argument("page_size must be at least one"));
        }
        let resp = self
            .service
            .list_live_rooms(req)
            .await
            .map_err(|error| map_bilibili_error("list_live_rooms", &error))?;
        Ok(Response::new(resp))
    }

    async fn list_live_areas(
        &self,
        request: Request<ListLiveAreasReq>,
    ) -> Result<Response<ListLiveAreasResp>, Status> {
        let resp = self
            .service
            .list_live_areas(request.into_inner())
            .await
            .map_err(|error| map_bilibili_error("list_live_areas", &error))?;
        Ok(Response::new(resp))
    }

    async fn list_favorite_folders(
        &self,
        request: Request<ListFavoriteFoldersReq>,
    ) -> Result<Response<ListFavoriteFoldersResp>, Status> {
        let resp = self
            .service
            .list_favorite_folders(request.into_inner())
            .await
            .map_err(|error| map_bilibili_error("list_favorite_folders", &error))?;
        Ok(Response::new(resp))
    }

    async fn list_followed_pgc(
        &self,
        request: Request<ListFollowedPgcReq>,
    ) -> Result<Response<ListFollowedPgcResp>, Status> {
        let req = request.into_inner();
        if !matches!(req.season_type, 1 | 2) {
            return Err(Status::invalid_argument("season_type must be 1 or 2"));
        }
        if req.page == 0 || req.page_size == 0 {
            return Err(Status::invalid_argument(
                "page and page_size must be at least one",
            ));
        }
        let resp = self
            .service
            .list_followed_pgc(req)
            .await
            .map_err(|error| map_bilibili_error("list_followed_pgc", &error))?;
        Ok(Response::new(resp))
    }

    async fn list_history(
        &self,
        request: Request<ListHistoryReq>,
    ) -> Result<Response<ListHistoryResp>, Status> {
        let req = request.into_inner();
        if req.page_size == 0 {
            return Err(Status::invalid_argument("page_size must be at least one"));
        }
        let resp = self
            .service
            .list_history(req)
            .await
            .map_err(|error| map_bilibili_error("list_history", &error))?;
        Ok(Response::new(resp))
    }

    async fn list_pgc_timeline(
        &self,
        request: Request<ListPgcTimelineReq>,
    ) -> Result<Response<ListPgcTimelineResp>, Status> {
        let req = request.into_inner();
        if req.before_days > 7 || req.after_days > 7 {
            return Err(Status::invalid_argument(
                "before_days and after_days must be at most seven",
            ));
        }
        let resp = self
            .service
            .list_pgc_timeline(req)
            .await
            .map_err(|error| map_bilibili_error("list_pgc_timeline", &error))?;
        Ok(Response::new(resp))
    }

    async fn list_pgc_seasons(
        &self,
        request: Request<ListPgcSeasonsReq>,
    ) -> Result<Response<ListPgcSeasonsResp>, Status> {
        let req = request.into_inner();
        if req.page == 0 || req.page_size == 0 {
            return Err(Status::invalid_argument(
                "page and page_size must be at least one",
            ));
        }
        let resp = self
            .service
            .list_pgc_seasons(req)
            .await
            .map_err(|error| map_bilibili_error("list_pgc_seasons", &error))?;
        Ok(Response::new(resp))
    }

    async fn watch_bilibili_live_danmaku(
        &self,
        request: Request<WatchBilibiliLiveDanmakuReq>,
    ) -> Result<Response<Self::WatchBilibiliLiveDanmakuStream>, Status> {
        let req = request.into_inner();
        if req.room_id == 0 {
            return Err(Status::invalid_argument("room_id must not be zero"));
        }
        let stream = self
            .service
            .watch_bilibili_live_danmaku(req)
            .await
            .map_err(|e| map_bilibili_error("watch_bilibili_live_danmaku", &e))?;
        let stream = StreamExt::map(stream, |event| {
            event.map_err(|e| map_bilibili_error("watch_bilibili_live_danmaku", &e))
        });
        Ok(Response::new(Box::pin(stream)))
    }
}
