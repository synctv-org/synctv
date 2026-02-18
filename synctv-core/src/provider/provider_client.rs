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
use std::time::Duration;
use synctv_media_providers::alist::{AlistError, AlistInterface};
use synctv_media_providers::grpc::alist::{FsGetResp, FsListResp, FsOtherResp};

/// Macro to generate a boilerplate gRPC client method implementation.
///
/// Each generated method: creates the gRPC client from the channel,
/// sends a `tonic::Request`, maps errors, and returns the inner response.
///
/// Generates the desugared `async_trait` form directly so the macro can be
/// used inside `#[async_trait]` impl blocks (proc-macros run before
/// `macro_rules!` expansion).
macro_rules! impl_grpc_method {
    ($client_mod:path, $client_name:ident, $error:ty, $method:ident, $req:ty, $resp:ty) => {
        fn $method<'life0, 'async_trait>(
            &'life0 self,
            request: $req,
        ) -> ::core::pin::Pin<Box<dyn ::core::future::Future<Output = Result<$resp, $error>> + ::core::marker::Send + 'async_trait>>
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                use $client_mod as _client_mod;
                let mut client = _client_mod::$client_name::new(self.channel.clone());
                let response = client.$method(tonic::Request::new(request)).await
                    .map_err(|e| <$error>::Network(format!("gRPC error: {e}")))?;
                Ok(response.into_inner())
            })
        }
    };
}

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
    impl_grpc_method!(synctv_media_providers::grpc::alist::alist_client, AlistClient, AlistError, fs_get, synctv_media_providers::grpc::alist::FsGetReq, FsGetResp);
    impl_grpc_method!(synctv_media_providers::grpc::alist::alist_client, AlistClient, AlistError, fs_list, synctv_media_providers::grpc::alist::FsListReq, FsListResp);
    impl_grpc_method!(synctv_media_providers::grpc::alist::alist_client, AlistClient, AlistError, fs_other, synctv_media_providers::grpc::alist::FsOtherReq, FsOtherResp);
    impl_grpc_method!(synctv_media_providers::grpc::alist::alist_client, AlistClient, AlistError, fs_search, synctv_media_providers::grpc::alist::FsSearchReq, synctv_media_providers::grpc::alist::FsSearchResp);
    impl_grpc_method!(synctv_media_providers::grpc::alist::alist_client, AlistClient, AlistError, me, synctv_media_providers::grpc::alist::MeReq, synctv_media_providers::grpc::alist::MeResp);

    // login has a non-standard return: extracts `.token` from response
    async fn login(&self, request: synctv_media_providers::grpc::alist::LoginReq) -> Result<String, AlistError> {
        use synctv_media_providers::grpc::alist::alist_client::AlistClient;
        let mut client = AlistClient::new(self.channel.clone());
        let response = client.login(tonic::Request::new(request)).await
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
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, new_qr_code, synctv_media_providers::grpc::bilibili::Empty, synctv_media_providers::grpc::bilibili::NewQrCodeResp);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, login_with_qr_code, synctv_media_providers::grpc::bilibili::LoginWithQrCodeReq, synctv_media_providers::grpc::bilibili::LoginWithQrCodeResp);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, new_captcha, synctv_media_providers::grpc::bilibili::Empty, synctv_media_providers::grpc::bilibili::NewCaptchaResp);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, new_sms, synctv_media_providers::grpc::bilibili::NewSmsReq, synctv_media_providers::grpc::bilibili::NewSmsResp);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, login_with_sms, synctv_media_providers::grpc::bilibili::LoginWithSmsReq, synctv_media_providers::grpc::bilibili::LoginWithSmsResp);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, parse_video_page, synctv_media_providers::grpc::bilibili::ParseVideoPageReq, synctv_media_providers::grpc::bilibili::VideoPageInfo);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, get_video_url, synctv_media_providers::grpc::bilibili::GetVideoUrlReq, synctv_media_providers::grpc::bilibili::VideoUrl);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, get_dash_video_url, synctv_media_providers::grpc::bilibili::GetDashVideoUrlReq, synctv_media_providers::grpc::bilibili::GetDashVideoUrlResp);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, get_subtitles, synctv_media_providers::grpc::bilibili::GetSubtitlesReq, synctv_media_providers::grpc::bilibili::GetSubtitlesResp);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, parse_pgc_page, synctv_media_providers::grpc::bilibili::ParsePgcPageReq, synctv_media_providers::grpc::bilibili::VideoPageInfo);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, get_pgcurl, synctv_media_providers::grpc::bilibili::GetPgcurlReq, synctv_media_providers::grpc::bilibili::VideoUrl);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, get_dash_pgcurl, synctv_media_providers::grpc::bilibili::GetDashPgcurlReq, synctv_media_providers::grpc::bilibili::GetDashPgcurlResp);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, user_info, synctv_media_providers::grpc::bilibili::UserInfoReq, synctv_media_providers::grpc::bilibili::UserInfoResp);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, r#match, synctv_media_providers::grpc::bilibili::MatchReq, synctv_media_providers::grpc::bilibili::MatchResp);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, get_live_streams, synctv_media_providers::grpc::bilibili::GetLiveStreamsReq, synctv_media_providers::grpc::bilibili::GetLiveStreamsResp);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, parse_live_page, synctv_media_providers::grpc::bilibili::ParseLivePageReq, synctv_media_providers::grpc::bilibili::VideoPageInfo);
    impl_grpc_method!(synctv_media_providers::grpc::bilibili::bilibili_client, BilibiliClient, BilibiliError, get_live_danmu_info, synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoReq, synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoResp);
}


// ============================================================================
// Cached Bilibili Client
// ============================================================================

/// Caching wrapper around any `BilibiliInterface` implementation.
///
/// Caches successful responses from read-only query methods using moka.
/// - Video info queries (parse_video_page, parse_pgc_page, etc.): 5 min TTL
/// - Play URL queries (get_video_url, get_pgc_url, etc.): 2 min TTL (URLs expire)
/// - Write/auth methods (login, QR code) are NOT cached.
///
/// Cache key: `"{method}:{serialized_request}"` -- uses serde_json serialization
/// of the proto request, which derives `Serialize`.
pub struct CachedBilibiliClient {
    inner: BilibiliClientArc,
    /// Cache for video info queries (5 min TTL)
    info_cache: moka::future::Cache<String, Vec<u8>>,
    /// Cache for play URL queries (2 min TTL)
    url_cache: moka::future::Cache<String, Vec<u8>>,
}

impl CachedBilibiliClient {
    pub fn new(inner: BilibiliClientArc) -> Self {
        Self {
            inner,
            info_cache: moka::future::Cache::builder()
                .max_capacity(500)
                .time_to_live(Duration::from_secs(5 * 60))
                .build(),
            url_cache: moka::future::Cache::builder()
                .max_capacity(200)
                .time_to_live(Duration::from_secs(2 * 60))
                .build(),
        }
    }
}

/// Helper to build a cache key from a method name and a serializable request.
fn cache_key<T: serde::Serialize>(method: &str, req: &T) -> String {
    format!("{}:{}", method, serde_json::to_string(req).unwrap_or_default())
}

/// Macro to implement a cached read-only method.
///
/// On cache hit, deserializes the stored bytes back to the response type.
/// On cache miss, calls the inner client, serializes the successful response,
/// and stores it. Errors are never cached.
macro_rules! impl_cached_method {
    ($self:ident, $cache:ident, $method:literal, $trait_method:ident, $req:expr, $resp_ty:ty) => {{
        let key = cache_key($method, &$req);
        if let Some(bytes) = $self.$cache.get(&key).await {
            if let Ok(resp) = serde_json::from_slice::<$resp_ty>(&bytes) {
                return Ok(resp);
            }
        }
        let result = $self.inner.$trait_method($req).await;
        if let Ok(ref resp) = result {
            if let Ok(bytes) = serde_json::to_vec(resp) {
                $self.$cache.insert(key, bytes).await;
            }
        }
        result
    }};
}

#[async_trait]
impl BilibiliInterface for CachedBilibiliClient {
    // Auth/login methods: NOT cached (delegate directly)
    async fn new_qr_code(&self, request: synctv_media_providers::grpc::bilibili::Empty) -> Result<synctv_media_providers::grpc::bilibili::NewQrCodeResp, BilibiliError> {
        self.inner.new_qr_code(request).await
    }
    async fn login_with_qr_code(&self, request: synctv_media_providers::grpc::bilibili::LoginWithQrCodeReq) -> Result<synctv_media_providers::grpc::bilibili::LoginWithQrCodeResp, BilibiliError> {
        self.inner.login_with_qr_code(request).await
    }
    async fn new_captcha(&self, request: synctv_media_providers::grpc::bilibili::Empty) -> Result<synctv_media_providers::grpc::bilibili::NewCaptchaResp, BilibiliError> {
        self.inner.new_captcha(request).await
    }
    async fn new_sms(&self, request: synctv_media_providers::grpc::bilibili::NewSmsReq) -> Result<synctv_media_providers::grpc::bilibili::NewSmsResp, BilibiliError> {
        self.inner.new_sms(request).await
    }
    async fn login_with_sms(&self, request: synctv_media_providers::grpc::bilibili::LoginWithSmsReq) -> Result<synctv_media_providers::grpc::bilibili::LoginWithSmsResp, BilibiliError> {
        self.inner.login_with_sms(request).await
    }

    // Info queries: cached with 5 min TTL
    async fn parse_video_page(&self, request: synctv_media_providers::grpc::bilibili::ParseVideoPageReq) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, BilibiliError> {
        impl_cached_method!(self, info_cache, "parse_video_page", parse_video_page, request, synctv_media_providers::grpc::bilibili::VideoPageInfo)
    }
    async fn parse_pgc_page(&self, request: synctv_media_providers::grpc::bilibili::ParsePgcPageReq) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, BilibiliError> {
        impl_cached_method!(self, info_cache, "parse_pgc_page", parse_pgc_page, request, synctv_media_providers::grpc::bilibili::VideoPageInfo)
    }
    async fn get_subtitles(&self, request: synctv_media_providers::grpc::bilibili::GetSubtitlesReq) -> Result<synctv_media_providers::grpc::bilibili::GetSubtitlesResp, BilibiliError> {
        impl_cached_method!(self, info_cache, "get_subtitles", get_subtitles, request, synctv_media_providers::grpc::bilibili::GetSubtitlesResp)
    }
    async fn user_info(&self, request: synctv_media_providers::grpc::bilibili::UserInfoReq) -> Result<synctv_media_providers::grpc::bilibili::UserInfoResp, BilibiliError> {
        impl_cached_method!(self, info_cache, "user_info", user_info, request, synctv_media_providers::grpc::bilibili::UserInfoResp)
    }
    async fn r#match(&self, request: synctv_media_providers::grpc::bilibili::MatchReq) -> Result<synctv_media_providers::grpc::bilibili::MatchResp, BilibiliError> {
        impl_cached_method!(self, info_cache, "match", r#match, request, synctv_media_providers::grpc::bilibili::MatchResp)
    }
    async fn parse_live_page(&self, request: synctv_media_providers::grpc::bilibili::ParseLivePageReq) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, BilibiliError> {
        impl_cached_method!(self, info_cache, "parse_live_page", parse_live_page, request, synctv_media_providers::grpc::bilibili::VideoPageInfo)
    }

    // URL queries: cached with 2 min TTL (URLs may be short-lived)
    async fn get_video_url(&self, request: synctv_media_providers::grpc::bilibili::GetVideoUrlReq) -> Result<synctv_media_providers::grpc::bilibili::VideoUrl, BilibiliError> {
        impl_cached_method!(self, url_cache, "get_video_url", get_video_url, request, synctv_media_providers::grpc::bilibili::VideoUrl)
    }
    async fn get_dash_video_url(&self, request: synctv_media_providers::grpc::bilibili::GetDashVideoUrlReq) -> Result<synctv_media_providers::grpc::bilibili::GetDashVideoUrlResp, BilibiliError> {
        impl_cached_method!(self, url_cache, "get_dash_video_url", get_dash_video_url, request, synctv_media_providers::grpc::bilibili::GetDashVideoUrlResp)
    }
    async fn get_pgcurl(&self, request: synctv_media_providers::grpc::bilibili::GetPgcurlReq) -> Result<synctv_media_providers::grpc::bilibili::VideoUrl, BilibiliError> {
        impl_cached_method!(self, url_cache, "get_pgcurl", get_pgcurl, request, synctv_media_providers::grpc::bilibili::VideoUrl)
    }
    async fn get_dash_pgcurl(&self, request: synctv_media_providers::grpc::bilibili::GetDashPgcurlReq) -> Result<synctv_media_providers::grpc::bilibili::GetDashPgcurlResp, BilibiliError> {
        impl_cached_method!(self, url_cache, "get_dash_pgcurl", get_dash_pgcurl, request, synctv_media_providers::grpc::bilibili::GetDashPgcurlResp)
    }
    async fn get_live_streams(&self, request: synctv_media_providers::grpc::bilibili::GetLiveStreamsReq) -> Result<synctv_media_providers::grpc::bilibili::GetLiveStreamsResp, BilibiliError> {
        impl_cached_method!(self, url_cache, "get_live_streams", get_live_streams, request, synctv_media_providers::grpc::bilibili::GetLiveStreamsResp)
    }

    // Live danmu info: NOT cached (connection-specific tokens)
    async fn get_live_danmu_info(&self, request: synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoReq) -> Result<synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoResp, BilibiliError> {
        self.inner.get_live_danmu_info(request).await
    }
}

/// Wrap a Bilibili client with caching
#[must_use]
pub fn with_bilibili_cache(inner: BilibiliClientArc) -> BilibiliClientArc {
    Arc::new(CachedBilibiliClient::new(inner))
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
    impl_grpc_method!(synctv_media_providers::grpc::emby::emby_client, EmbyClient, EmbyError, login, synctv_media_providers::grpc::emby::LoginReq, synctv_media_providers::grpc::emby::LoginResp);
    impl_grpc_method!(synctv_media_providers::grpc::emby::emby_client, EmbyClient, EmbyError, me, synctv_media_providers::grpc::emby::MeReq, synctv_media_providers::grpc::emby::MeResp);
    impl_grpc_method!(synctv_media_providers::grpc::emby::emby_client, EmbyClient, EmbyError, get_items, synctv_media_providers::grpc::emby::GetItemsReq, synctv_media_providers::grpc::emby::GetItemsResp);
    impl_grpc_method!(synctv_media_providers::grpc::emby::emby_client, EmbyClient, EmbyError, get_item, synctv_media_providers::grpc::emby::GetItemReq, synctv_media_providers::grpc::emby::Item);
    impl_grpc_method!(synctv_media_providers::grpc::emby::emby_client, EmbyClient, EmbyError, fs_list, synctv_media_providers::grpc::emby::FsListReq, synctv_media_providers::grpc::emby::FsListResp);
    impl_grpc_method!(synctv_media_providers::grpc::emby::emby_client, EmbyClient, EmbyError, get_system_info, synctv_media_providers::grpc::emby::SystemInfoReq, synctv_media_providers::grpc::emby::SystemInfoResp);
    impl_grpc_method!(synctv_media_providers::grpc::emby::emby_client, EmbyClient, EmbyError, logout, synctv_media_providers::grpc::emby::LogoutReq, synctv_media_providers::grpc::emby::Empty);
    impl_grpc_method!(synctv_media_providers::grpc::emby::emby_client, EmbyClient, EmbyError, playback_info, synctv_media_providers::grpc::emby::PlaybackInfoReq, synctv_media_providers::grpc::emby::PlaybackInfoResp);
    impl_grpc_method!(synctv_media_providers::grpc::emby::emby_client, EmbyClient, EmbyError, delete_active_encodings, synctv_media_providers::grpc::emby::DeleteActiveEncodingsReq, synctv_media_providers::grpc::emby::Empty);
    impl_grpc_method!(synctv_media_providers::grpc::emby::emby_client, EmbyClient, EmbyError, report_playback_start, synctv_media_providers::grpc::emby::ReportPlaybackStartReq, synctv_media_providers::grpc::emby::Empty);
    impl_grpc_method!(synctv_media_providers::grpc::emby::emby_client, EmbyClient, EmbyError, report_playback_stop, synctv_media_providers::grpc::emby::ReportPlaybackStopReq, synctv_media_providers::grpc::emby::Empty);
    impl_grpc_method!(synctv_media_providers::grpc::emby::emby_client, EmbyClient, EmbyError, report_playback_progress, synctv_media_providers::grpc::emby::ReportPlaybackProgressReq, synctv_media_providers::grpc::emby::Empty);
}

