//! Bilibili gRPC Server Implementation
//!
//! Thin wrapper around `BilibiliService` that implements gRPC server trait.

use super::bilibili::{
    bilibili_server::Bilibili, Empty, GetDashPgcurlReq, GetDashPgcurlResp, GetDashVideoUrlReq,
    GetDashVideoUrlResp, GetLiveDanmuInfoReq, GetLiveDanmuInfoResp, GetLiveStreamsReq,
    GetLiveStreamsResp, GetPgcurlReq, GetSubtitlesReq, GetSubtitlesResp, GetVideoUrlReq,
    LoginWithQrCodeReq, LoginWithQrCodeResp, LoginWithSmsReq, LoginWithSmsResp, MatchReq,
    MatchResp, NewCaptchaResp, NewQrCodeResp, NewSmsReq, NewSmsResp, ParseLivePageReq,
    ParsePgcPageReq, ParseVideoPageReq, UserInfoReq, UserInfoResp, VideoPageInfo, VideoUrl,
};
use super::error_mapper::map_provider_error;
use crate::bilibili::{BilibiliInterface, BilibiliService as BilibiliServiceImpl};
use crate::error::ProviderClientError;
use crate::validation::validate_required;
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
}
