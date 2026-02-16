//! Provider Client - Unified client interface
//!
//! Uses trait from synctv-media-providers directly, with thin wrappers for gRPC clients.
//!
//! Architecture:
//! ```ignore
//! AlistProvider
//!     ↓
//! Arc<dyn AlistInterface>  (from synctv-media-providers)
//!     ↓
//! ┌─────────────────┬──────────────────────┐
//! │                 │                      │
//! AlistService    GrpcAlistClient
//! (complete impl)  (thin gRPC wrapper)
//! ```

use super::ProviderError;
use async_trait::async_trait;
use std::sync::Arc;
use synctv_media_providers::alist::{AlistError, AlistInterface};
use synctv_media_providers::grpc::alist::{FsGetResp, FsListResp, FsOtherResp};

// ============================================================================
// Alist Client
// ============================================================================

/// Type alias for Alist client
pub type AlistClientArc = Arc<dyn AlistInterface>;

/// Singleton local Alist client
static LOCAL_ALIST_CLIENT: std::sync::LazyLock<AlistClientArc> = std::sync::LazyLock::new(|| {
    Arc::new(synctv_media_providers::alist::AlistService::new())
});

/// Load local Alist client (singleton)
pub fn load_local_alist_client() -> AlistClientArc {
    LOCAL_ALIST_CLIENT.clone()
}

/// Create remote Alist client (thin wrapper around gRPC client)
#[must_use] 
pub fn create_remote_alist_client(channel: tonic::transport::Channel) -> AlistClientArc {
    Arc::new(GrpcAlistClient::new(channel))
}

/// Thin wrapper around gRPC client
///
/// Implements `AlistInterface` by delegating to gRPC client.
pub struct GrpcAlistClient {
    channel: tonic::transport::Channel,
}

impl GrpcAlistClient {
    #[must_use] 
    pub const fn new(channel: tonic::transport::Channel) -> Self {
        Self { channel }
    }
}

#[async_trait]
impl AlistInterface for GrpcAlistClient {
    async fn fs_get(&self, request: synctv_media_providers::grpc::alist::FsGetReq) -> Result<FsGetResp, AlistError> {
        use synctv_media_providers::grpc::alist::alist_client::AlistClient;

        let mut client = AlistClient::new(self.channel.clone());

        let response = client
            .fs_get(tonic::Request::new(request))
            .await
            .map_err(|e| AlistError::Network(format!("gRPC error: {e}")))?;

        Ok(response.into_inner())
    }

    async fn fs_list(&self, request: synctv_media_providers::grpc::alist::FsListReq) -> Result<FsListResp, AlistError> {
        use synctv_media_providers::grpc::alist::alist_client::AlistClient;

        let mut client = AlistClient::new(self.channel.clone());

        let response = client
            .fs_list(tonic::Request::new(request))
            .await
            .map_err(|e| AlistError::Network(format!("gRPC error: {e}")))?;

        Ok(response.into_inner())
    }

    async fn fs_other(&self, request: synctv_media_providers::grpc::alist::FsOtherReq) -> Result<FsOtherResp, AlistError> {
        use synctv_media_providers::grpc::alist::alist_client::AlistClient;

        let mut client = AlistClient::new(self.channel.clone());

        let response = client
            .fs_other(tonic::Request::new(request))
            .await
            .map_err(|e| AlistError::Network(format!("gRPC error: {e}")))?;

        Ok(response.into_inner())
    }

    async fn fs_search(&self, request: synctv_media_providers::grpc::alist::FsSearchReq) -> Result<synctv_media_providers::grpc::alist::FsSearchResp, AlistError> {
        use synctv_media_providers::grpc::alist::alist_client::AlistClient;

        let mut client = AlistClient::new(self.channel.clone());

        let response = client
            .fs_search(tonic::Request::new(request))
            .await
            .map_err(|e| AlistError::Network(format!("gRPC error: {e}")))?;

        Ok(response.into_inner())
    }

    async fn me(&self, request: synctv_media_providers::grpc::alist::MeReq) -> Result<synctv_media_providers::grpc::alist::MeResp, AlistError> {
        use synctv_media_providers::grpc::alist::alist_client::AlistClient;

        let mut client = AlistClient::new(self.channel.clone());

        let response = client
            .me(tonic::Request::new(request))
            .await
            .map_err(|e| AlistError::Network(format!("gRPC error: {e}")))?;

        Ok(response.into_inner())
    }

    async fn login(&self, request: synctv_media_providers::grpc::alist::LoginReq) -> Result<String, AlistError> {
        use synctv_media_providers::grpc::alist::alist_client::AlistClient;

        let mut client = AlistClient::new(self.channel.clone());

        let response = client
            .login(tonic::Request::new(request))
            .await
            .map_err(|e| AlistError::Network(format!("gRPC error: {e}")))?;

        Ok(response.into_inner().token)
    }
}

// ============================================================================
// Helper Types for MediaProvider
// ============================================================================

/// Wrapper types to provide cleaner API for `MediaProvider`
///
/// Alist file info for `MediaProvider`
#[derive(Debug, Clone)]
pub struct AlistFileInfo {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub raw_url: String,
    pub provider: String,
    pub thumb: String,
}

impl From<FsGetResp> for AlistFileInfo {
    fn from(data: FsGetResp) -> Self {
        Self {
            name: data.name,
            size: data.size,
            is_dir: data.is_dir,
            raw_url: data.raw_url,
            provider: data.provider,
            thumb: data.thumb,
        }
    }
}

/// Alist video preview info
#[derive(Debug, Clone)]
pub struct AlistVideoPreview {
    pub transcoding_tasks: Vec<AlistTranscodingTask>,
    pub subtitle_tasks: Vec<AlistSubtitleTask>,
    pub duration: f64,
    pub width: u64,
    pub height: u64,
}

#[derive(Debug, Clone)]
pub struct AlistTranscodingTask {
    pub template_name: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct AlistSubtitleTask {
    pub language: String,
    pub url: String,
}

/// Extension trait for convenient access to video preview
#[async_trait]
pub trait AlistClientExt {
    async fn get_video_preview(
        &self,
        host: &str,
        token: &str,
        path: &str,
        password: Option<&str>,
    ) -> Result<Option<AlistVideoPreview>, ProviderError>;
}

#[async_trait]
impl AlistClientExt for Arc<dyn AlistInterface> {
    async fn get_video_preview(
        &self,
        host: &str,
        token: &str,
        path: &str,
        password: Option<&str>,
    ) -> Result<Option<AlistVideoPreview>, ProviderError> {
        let request = synctv_media_providers::grpc::alist::FsOtherReq {
            host: host.to_string(),
            token: token.to_string(),
            path: path.to_string(),
            method: "video_preview".to_string(),
            password: password.unwrap_or("").to_string(),
        };

        let other_data = self
            .fs_other(request)
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        Ok(other_data.video_preview_play_info.map(|preview| {
            AlistVideoPreview {
                transcoding_tasks: preview
                    .live_transcoding_task_list
                    .into_iter()
                    .map(|task| AlistTranscodingTask {
                        template_name: task.template_name,
                        url: task.url,
                    })
                    .collect(),
                subtitle_tasks: preview
                    .live_transcoding_subtitle_task_list
                    .into_iter()
                    .map(|sub| AlistSubtitleTask {
                        language: sub.language,
                        url: sub.url,
                    })
                    .collect(),
                duration: preview.meta.as_ref().map_or(0.0, |m| m.duration),
                width: preview.meta.as_ref().map_or(0, |m| m.width),
                height: preview.meta.as_ref().map_or(0, |m| m.height),
            }
        }))
    }
}

// Error conversion (all provider errors are ProviderClientError aliases)
impl From<synctv_media_providers::ProviderClientError> for ProviderError {
    fn from(error: synctv_media_providers::ProviderClientError) -> Self {
        use synctv_media_providers::ProviderClientError;
        match error {
            ProviderClientError::Network(msg) => Self::NetworkError(msg),
            ProviderClientError::Api { message, .. } => Self::ApiError(message),
            ProviderClientError::Parse(msg) => Self::ParseError(msg),
            ProviderClientError::Auth(msg) => Self::ApiError(msg),
            ProviderClientError::InvalidConfig(msg) => Self::InvalidConfig(msg),
            ProviderClientError::InvalidHeader(msg) => Self::ParseError(msg),
            ProviderClientError::NotImplemented(msg) => Self::ApiError(format!("Not implemented: {msg}")),
            ProviderClientError::Http { status, url, .. } => Self::ApiError(format!("HTTP {status} for {url}")),
            ProviderClientError::ResponseTooLarge { size } => Self::ApiError(format!("Response too large ({size} bytes)")),
        }
    }
}

// ============================================================================
// Bilibili Client
// ============================================================================

use synctv_media_providers::bilibili::{BilibiliError, BilibiliInterface};

/// Type alias for Bilibili client
pub type BilibiliClientArc = Arc<dyn BilibiliInterface>;

/// Singleton local Bilibili client
static LOCAL_BILIBILI_CLIENT: std::sync::LazyLock<BilibiliClientArc> = std::sync::LazyLock::new(|| {
    Arc::new(synctv_media_providers::bilibili::BilibiliService::new())
});

/// Load local Bilibili client (singleton)
pub fn load_local_bilibili_client() -> BilibiliClientArc {
    LOCAL_BILIBILI_CLIENT.clone()
}

/// Create remote Bilibili client (thin wrapper around gRPC client)
#[must_use] 
pub fn create_remote_bilibili_client(channel: tonic::transport::Channel) -> BilibiliClientArc {
    Arc::new(GrpcBilibiliClient::new(channel))
}

/// Thin wrapper around gRPC client for Bilibili
pub struct GrpcBilibiliClient {
    channel: tonic::transport::Channel,
}

impl GrpcBilibiliClient {
    #[must_use] 
    pub const fn new(channel: tonic::transport::Channel) -> Self {
        Self { channel }
    }
}

#[async_trait]
impl BilibiliInterface for GrpcBilibiliClient {
    async fn new_qr_code(&self, request: synctv_media_providers::grpc::bilibili::Empty)
        -> Result<synctv_media_providers::grpc::bilibili::NewQrCodeResp, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.new_qr_code(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn login_with_qr_code(&self, request: synctv_media_providers::grpc::bilibili::LoginWithQrCodeReq)
        -> Result<synctv_media_providers::grpc::bilibili::LoginWithQrCodeResp, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.login_with_qr_code(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn new_captcha(&self, request: synctv_media_providers::grpc::bilibili::Empty)
        -> Result<synctv_media_providers::grpc::bilibili::NewCaptchaResp, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.new_captcha(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn new_sms(&self, request: synctv_media_providers::grpc::bilibili::NewSmsReq)
        -> Result<synctv_media_providers::grpc::bilibili::NewSmsResp, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.new_sms(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn login_with_sms(&self, request: synctv_media_providers::grpc::bilibili::LoginWithSmsReq)
        -> Result<synctv_media_providers::grpc::bilibili::LoginWithSmsResp, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.login_with_sms(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn parse_video_page(&self, request: synctv_media_providers::grpc::bilibili::ParseVideoPageReq)
        -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.parse_video_page(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn get_video_url(&self, request: synctv_media_providers::grpc::bilibili::GetVideoUrlReq)
        -> Result<synctv_media_providers::grpc::bilibili::VideoUrl, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.get_video_url(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn get_dash_video_url(&self, request: synctv_media_providers::grpc::bilibili::GetDashVideoUrlReq)
        -> Result<synctv_media_providers::grpc::bilibili::GetDashVideoUrlResp, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.get_dash_video_url(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn get_subtitles(&self, request: synctv_media_providers::grpc::bilibili::GetSubtitlesReq)
        -> Result<synctv_media_providers::grpc::bilibili::GetSubtitlesResp, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.get_subtitles(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn parse_pgc_page(&self, request: synctv_media_providers::grpc::bilibili::ParsePgcPageReq)
        -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.parse_pgc_page(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn get_pgcurl(&self, request: synctv_media_providers::grpc::bilibili::GetPgcurlReq)
        -> Result<synctv_media_providers::grpc::bilibili::VideoUrl, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.get_pgcurl(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn get_dash_pgcurl(&self, request: synctv_media_providers::grpc::bilibili::GetDashPgcurlReq)
        -> Result<synctv_media_providers::grpc::bilibili::GetDashPgcurlResp, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.get_dash_pgcurl(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn user_info(&self, request: synctv_media_providers::grpc::bilibili::UserInfoReq)
        -> Result<synctv_media_providers::grpc::bilibili::UserInfoResp, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.user_info(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn r#match(&self, request: synctv_media_providers::grpc::bilibili::MatchReq)
        -> Result<synctv_media_providers::grpc::bilibili::MatchResp, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.r#match(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn get_live_streams(&self, request: synctv_media_providers::grpc::bilibili::GetLiveStreamsReq)
        -> Result<synctv_media_providers::grpc::bilibili::GetLiveStreamsResp, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.get_live_streams(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn parse_live_page(&self, request: synctv_media_providers::grpc::bilibili::ParseLivePageReq)
        -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.parse_live_page(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn get_live_danmu_info(&self, request: synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoReq)
        -> Result<synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoResp, BilibiliError>
    {
        use synctv_media_providers::grpc::bilibili::bilibili_client::BilibiliClient;
        let mut client = BilibiliClient::new(self.channel.clone());
        let response = client.get_live_danmu_info(tonic::Request::new(request)).await
            .map_err(|e| BilibiliError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }
}


// ============================================================================
// Emby Client
// ============================================================================

use synctv_media_providers::emby::{EmbyError, EmbyInterface};

/// Type alias for Emby client
pub type EmbyClientArc = Arc<dyn EmbyInterface>;

/// Singleton local Emby client
static LOCAL_EMBY_CLIENT: std::sync::LazyLock<EmbyClientArc> = std::sync::LazyLock::new(|| {
    Arc::new(synctv_media_providers::emby::EmbyService::new())
});

/// Load local Emby client (singleton)
pub fn load_local_emby_client() -> EmbyClientArc {
    LOCAL_EMBY_CLIENT.clone()
}

/// Create remote Emby client (thin wrapper around gRPC client)
#[must_use] 
pub fn create_remote_emby_client(channel: tonic::transport::Channel) -> EmbyClientArc {
    Arc::new(GrpcEmbyClient::new(channel))
}

/// Thin wrapper around gRPC client for Emby
pub struct GrpcEmbyClient {
    channel: tonic::transport::Channel,
}

impl GrpcEmbyClient {
    #[must_use] 
    pub const fn new(channel: tonic::transport::Channel) -> Self {
        Self { channel }
    }
}

#[async_trait]
impl EmbyInterface for GrpcEmbyClient {
    async fn login(&self, request: synctv_media_providers::grpc::emby::LoginReq)
        -> Result<synctv_media_providers::grpc::emby::LoginResp, EmbyError>
    {
        use synctv_media_providers::grpc::emby::emby_client::EmbyClient;
        let mut client = EmbyClient::new(self.channel.clone());
        let response = client.login(tonic::Request::new(request)).await
            .map_err(|e| EmbyError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn me(&self, request: synctv_media_providers::grpc::emby::MeReq)
        -> Result<synctv_media_providers::grpc::emby::MeResp, EmbyError>
    {
        use synctv_media_providers::grpc::emby::emby_client::EmbyClient;
        let mut client = EmbyClient::new(self.channel.clone());
        let response = client.me(tonic::Request::new(request)).await
            .map_err(|e| EmbyError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn get_items(&self, request: synctv_media_providers::grpc::emby::GetItemsReq)
        -> Result<synctv_media_providers::grpc::emby::GetItemsResp, EmbyError>
    {
        use synctv_media_providers::grpc::emby::emby_client::EmbyClient;
        let mut client = EmbyClient::new(self.channel.clone());
        let response = client.get_items(tonic::Request::new(request)).await
            .map_err(|e| EmbyError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn get_item(&self, request: synctv_media_providers::grpc::emby::GetItemReq)
        -> Result<synctv_media_providers::grpc::emby::Item, EmbyError>
    {
        use synctv_media_providers::grpc::emby::emby_client::EmbyClient;
        let mut client = EmbyClient::new(self.channel.clone());
        let response = client.get_item(tonic::Request::new(request)).await
            .map_err(|e| EmbyError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn fs_list(&self, request: synctv_media_providers::grpc::emby::FsListReq)
        -> Result<synctv_media_providers::grpc::emby::FsListResp, EmbyError>
    {
        use synctv_media_providers::grpc::emby::emby_client::EmbyClient;
        let mut client = EmbyClient::new(self.channel.clone());
        let response = client.fs_list(tonic::Request::new(request)).await
            .map_err(|e| EmbyError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn get_system_info(&self, request: synctv_media_providers::grpc::emby::SystemInfoReq)
        -> Result<synctv_media_providers::grpc::emby::SystemInfoResp, EmbyError>
    {
        use synctv_media_providers::grpc::emby::emby_client::EmbyClient;
        let mut client = EmbyClient::new(self.channel.clone());
        let response = client.get_system_info(tonic::Request::new(request)).await
            .map_err(|e| EmbyError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn logout(&self, request: synctv_media_providers::grpc::emby::LogoutReq)
        -> Result<synctv_media_providers::grpc::emby::Empty, EmbyError>
    {
        use synctv_media_providers::grpc::emby::emby_client::EmbyClient;
        let mut client = EmbyClient::new(self.channel.clone());
        let response = client.logout(tonic::Request::new(request)).await
            .map_err(|e| EmbyError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn playback_info(&self, request: synctv_media_providers::grpc::emby::PlaybackInfoReq)
        -> Result<synctv_media_providers::grpc::emby::PlaybackInfoResp, EmbyError>
    {
        use synctv_media_providers::grpc::emby::emby_client::EmbyClient;
        let mut client = EmbyClient::new(self.channel.clone());
        let response = client.playback_info(tonic::Request::new(request)).await
            .map_err(|e| EmbyError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn delete_active_encodings(&self, request: synctv_media_providers::grpc::emby::DeleteActiveEncodingsReq)
        -> Result<synctv_media_providers::grpc::emby::Empty, EmbyError>
    {
        use synctv_media_providers::grpc::emby::emby_client::EmbyClient;
        let mut client = EmbyClient::new(self.channel.clone());
        let response = client.delete_active_encodings(tonic::Request::new(request)).await
            .map_err(|e| EmbyError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn report_playback_start(&self, request: synctv_media_providers::grpc::emby::ReportPlaybackStartReq)
        -> Result<synctv_media_providers::grpc::emby::Empty, EmbyError>
    {
        use synctv_media_providers::grpc::emby::emby_client::EmbyClient;
        let mut client = EmbyClient::new(self.channel.clone());
        let response = client.report_playback_start(tonic::Request::new(request)).await
            .map_err(|e| EmbyError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn report_playback_stop(&self, request: synctv_media_providers::grpc::emby::ReportPlaybackStopReq)
        -> Result<synctv_media_providers::grpc::emby::Empty, EmbyError>
    {
        use synctv_media_providers::grpc::emby::emby_client::EmbyClient;
        let mut client = EmbyClient::new(self.channel.clone());
        let response = client.report_playback_stop(tonic::Request::new(request)).await
            .map_err(|e| EmbyError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }

    async fn report_playback_progress(&self, request: synctv_media_providers::grpc::emby::ReportPlaybackProgressReq)
        -> Result<synctv_media_providers::grpc::emby::Empty, EmbyError>
    {
        use synctv_media_providers::grpc::emby::emby_client::EmbyClient;
        let mut client = EmbyClient::new(self.channel.clone());
        let response = client.report_playback_progress(tonic::Request::new(request)).await
            .map_err(|e| EmbyError::Network(format!("gRPC error: {e}")))?;
        Ok(response.into_inner())
    }
}

