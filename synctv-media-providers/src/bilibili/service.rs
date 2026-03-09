//! Bilibili Service - Complete implementation
//!
//! This is the full HTTP client implementation.
//! Both gRPC server and local usage call this service.

use super::{client::BilibiliClient, BilibiliError};
use crate::grpc::bilibili::{
    Empty, GetDashPgcurlReq, GetDashPgcurlResp, GetDashVideoUrlReq, GetDashVideoUrlResp,
    GetLiveDanmuInfoReq, GetLiveDanmuInfoResp, GetLiveStreamsReq, GetLiveStreamsResp, GetPgcurlReq,
    GetSubtitlesReq, GetSubtitlesResp, GetVideoUrlReq, LoginWithQrCodeReq, LoginWithQrCodeResp,
    LoginWithSmsReq, LoginWithSmsResp, MatchReq, MatchResp, NewCaptchaResp, NewQrCodeResp,
    NewSmsReq, NewSmsResp, ParseLivePageReq, ParsePgcPageReq, ParseVideoPageReq, UserInfoReq,
    UserInfoResp, VideoInfo, VideoPageInfo, VideoSegment as ProtoVideoSegment, VideoUrl,
};
use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashMap;
use std::sync::Arc;

/// Unified Bilibili service interface
///
/// This trait defines all Bilibili operations using proto request/response types.
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
}

/// Bilibili service implementation
///
/// This is the complete implementation that makes actual HTTP calls.
/// Used by both local callers and gRPC server.
pub struct BilibiliService {
    client: Client,
    wbi_state: Arc<super::client::WbiState>,
}

impl BilibiliService {
    pub fn new() -> Self {
        Self {
            client: synctv_common::http::build_provider_client()
                .expect("default provider HTTP client should build"),
            wbi_state: Arc::new(super::client::WbiState::default()),
        }
    }

    #[must_use]
    pub fn with_client(client: Client) -> Self {
        Self {
            client,
            wbi_state: Arc::new(super::client::WbiState::default()),
        }
    }
}

impl Default for BilibiliService {
    fn default() -> Self {
        Self::new()
    }
}

fn client_from_cookies_and_state(
    client: Client,
    cookies: &HashMap<String, String>,
    wbi_state: Arc<super::client::WbiState>,
) -> Result<BilibiliClient, BilibiliError> {
    if cookies.is_empty() {
        BilibiliClient::new_with_transport(
            client,
            super::client::BilibiliEndpoints::default(),
            wbi_state,
        )
    } else {
        BilibiliClient::with_cookies_and_transport(
            cookies.clone(),
            client,
            super::client::BilibiliEndpoints::default(),
            wbi_state,
        )
    }
}

/// Convert client-layer `VideoPageInfo` to proto `VideoPageInfo`
fn to_proto_page_info(page_info: super::client::VideoPageInfo) -> VideoPageInfo {
    VideoPageInfo {
        title: page_info.title,
        actors: page_info.actors.join(", "),
        video_infos: page_info
            .video_infos
            .into_iter()
            .map(|v| VideoInfo {
                bvid: v.bvid,
                cid: v.cid,
                epid: v.epid,
                name: v.name,
                cover_image: v.cover_image,
                live: v.live,
            })
            .collect(),
    }
}

#[async_trait]
impl BilibiliInterface for BilibiliService {
    async fn new_qr_code(&self, _request: Empty) -> Result<NewQrCodeResp, BilibiliError> {
        let client = BilibiliClient::new_with_transport(
            self.client.clone(),
            super::client::BilibiliEndpoints::default(),
            self.wbi_state.clone(),
        )?;
        let (url, key) = client.new_qr_code().await?;
        Ok(NewQrCodeResp { url, key })
    }

    async fn login_with_qr_code(
        &self,
        request: LoginWithQrCodeReq,
    ) -> Result<LoginWithQrCodeResp, BilibiliError> {
        let client = BilibiliClient::new_with_transport(
            self.client.clone(),
            super::client::BilibiliEndpoints::default(),
            self.wbi_state.clone(),
        )?;
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
            super::client::BilibiliEndpoints::default(),
            self.wbi_state.clone(),
        )?;
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
            super::client::BilibiliEndpoints::default(),
            self.wbi_state.clone(),
        )?;
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
            super::client::BilibiliEndpoints::default(),
            self.wbi_state.clone(),
        )?;
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
        )?;
        let page_info = client.parse_video_page(request.aid, &request.bvid).await?;
        Ok(to_proto_page_info(page_info))
    }

    async fn get_video_url(&self, request: GetVideoUrlReq) -> Result<VideoUrl, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
        )?;
        let quality = if request.quality == 0 {
            None
        } else {
            Some(request.quality as u32)
        };
        let url_info = client
            .get_video_url(request.aid, &request.bvid, request.cid, quality)
            .await?;

        Ok(VideoUrl {
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
                })
                .collect(),
        })
    }

    async fn get_dash_video_url(
        &self,
        request: GetDashVideoUrlReq,
    ) -> Result<GetDashVideoUrlResp, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
        )?;
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
        )?;
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
        )?;
        let page_info = client.parse_pgc_page(request.epid, request.ssid).await?;
        Ok(to_proto_page_info(page_info))
    }

    async fn get_pgcurl(&self, request: GetPgcurlReq) -> Result<VideoUrl, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
        )?;
        let quality = if request.quality == 0 {
            None
        } else {
            Some(request.quality as u32)
        };
        let url_info = client
            .get_pgc_url(request.epid, request.cid, quality)
            .await?;

        Ok(VideoUrl {
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
                })
                .collect(),
        })
    }

    async fn get_dash_pgcurl(
        &self,
        request: GetDashPgcurlReq,
    ) -> Result<GetDashPgcurlResp, BilibiliError> {
        let client = client_from_cookies_and_state(
            self.client.clone(),
            &request.cookies,
            self.wbi_state.clone(),
        )?;
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
        )?;
        let user_info = client.user_info().await?;

        Ok(UserInfoResp {
            is_login: user_info.is_login,
            username: user_info.username,
            face: user_info.face,
            is_vip: user_info.is_vip,
        })
    }

    async fn r#match(&self, request: MatchReq) -> Result<MatchResp, BilibiliError> {
        let (match_type, id) = BilibiliClient::match_url(&request.url)?;
        Ok(MatchResp {
            r#type: match_type,
            id,
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
        )?;
        let streams = client.get_live_streams(request.cid, request.hls).await?;

        use crate::grpc::bilibili::LiveStream;
        Ok(GetLiveStreamsResp {
            live_streams: streams
                .into_iter()
                .map(|s| LiveStream {
                    quality: u64::from(s.quality),
                    urls: s.urls,
                    desc: s.desc,
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
        )?;
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
        )?;
        let danmu_info = client.get_live_danmu_info(request.room_id).await?;

        use crate::grpc::bilibili::get_live_danmu_info_resp::Host;
        Ok(GetLiveDanmuInfoResp {
            token: danmu_info.token,
            host_list: danmu_info
                .host_list
                .into_iter()
                .map(|h| Host {
                    host: h.host,
                    port: h.port,
                    wss_port: h.wss_port,
                    ws_port: h.ws_port,
                })
                .collect(),
        })
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
    use crate::grpc::bilibili::QrCodeStatus;

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
    async fn test_service_reuses_shared_wbi_state() {
        let service = BilibiliService::new();
        service.wbi_state.reset_for_tests().await;
        service
            .wbi_state
            .set_wbi_key("shared-key".to_string())
            .await;

        let client_a = super::client_from_cookies_and_state(
            service.client.clone(),
            &std::collections::HashMap::new(),
            service.wbi_state.clone(),
        )
        .unwrap();
        let client_b = super::client_from_cookies_and_state(
            service.client.clone(),
            &std::collections::HashMap::new(),
            service.wbi_state.clone(),
        )
        .unwrap();

        assert_eq!(
            client_a
                .shared_wbi_state()
                .get_valid_wbi_key()
                .await
                .as_deref(),
            Some("shared-key")
        );
        assert_eq!(
            client_b
                .shared_wbi_state()
                .get_valid_wbi_key()
                .await
                .as_deref(),
            Some("shared-key")
        );
    }
}
