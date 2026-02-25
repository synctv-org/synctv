//! Provider Client - Unified client interface
//!
//! Uses trait from synctv-media-providers directly, with thin wrappers for gRPC clients.
//!
//! Architecture:
//! ```text
//! AlistProvider
//!     ↓
//! Arc<dyn AlistInterface>  (from synctv-media-providers)
//!     ↓
//! ┌─────────────────┬──────────────────────┐
//! │                 │                      │
//! AlistService    GrpcAlistClient
//! (complete impl)  (thin gRPC wrapper)
//! ```
//!
//! ## Dependency Injection
//!
//! Local clients are managed by `ProviderClientManager` rather than global statics.
//! This enables proper sharing across the application and testability.

use super::ProviderError;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use synctv_media_providers::alist::{AlistError, AlistInterface};
use synctv_media_providers::grpc::alist::{FsGetResp, FsListResp, FsOtherResp};

/// Default per-request timeout for gRPC calls to remote providers.
///
/// Reduced from 30s to 10s (Issue #35): hung requests under load consume threads.
/// Providers that genuinely need longer should use explicit deadlines.
const GRPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Macro to generate a boilerplate gRPC client method implementation.
///
/// Each generated method: creates the gRPC client from the channel,
/// sends a `tonic::Request` with a per-request timeout, maps errors,
/// and returns the inner response.
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
                let response = tokio::time::timeout(
                    GRPC_REQUEST_TIMEOUT,
                    client.$method(tonic::Request::new(request)),
                )
                .await
                .map_err(|_| <$error>::Network(format!(
                    "gRPC request timeout ({}s) for {}",
                    GRPC_REQUEST_TIMEOUT.as_secs(),
                    stringify!($method),
                )))?
                .map_err(|e| <$error>::Network(format!("gRPC error: {e}")))?;
                Ok(response.into_inner())
            })
        }
    };
}

// ============================================================================
// ProviderClientManager - Dependency Injection for Local Clients
// ============================================================================

/// Manager for provider clients that supports dependency injection.
///
/// In a multi-replica architecture, local clients should be managed through
/// this struct rather than global statics. This enables:
/// - Proper sharing of client instances across the application
/// - Testability through mock injection
/// - Consistent behavior across replicas
///
/// # Example
///
/// ```
/// use synctv_core::provider::provider_client::ProviderClientManager;
/// use std::sync::Arc;
///
/// let manager = ProviderClientManager::new();
///
/// // Get local Alist client
/// let alist_client = manager.local_alist_client();
///
/// // Get local Bilibili client
/// let bilibili_client = manager.local_bilibili_client();
///
/// // Get local Emby client
/// let emby_client = manager.local_emby_client();
/// ```
#[derive(Clone)]
pub struct ProviderClientManager {
    /// Local Alist client (singleton within this manager)
    local_alist: AlistClientArc,
    /// Local Bilibili client (singleton within this manager)
    local_bilibili: BilibiliClientArc,
    /// Local Emby client (singleton within this manager)
    local_emby: EmbyClientArc,
}

impl std::fmt::Debug for ProviderClientManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderClientManager")
            .field("local_alist", &"AlistClientArc")
            .field("local_bilibili", &"BilibiliClientArc")
            .field("local_emby", &"EmbyClientArc")
            .finish()
    }
}

impl Default for ProviderClientManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderClientManager {
    /// Create a new `ProviderClientManager` with default local clients.
    #[must_use]
    pub fn new() -> Self {
        Self {
            local_alist: Arc::new(synctv_media_providers::alist::AlistService::new()),
            local_bilibili: Arc::new(synctv_media_providers::bilibili::BilibiliService::new()),
            local_emby: Arc::new(synctv_media_providers::emby::EmbyService::new()),
        }
    }

    /// Create a new `ProviderClientManager` with custom local clients.
    ///
    /// This is useful for testing with mock clients.
    #[must_use]
    pub fn with_custom_clients(
        local_alist: AlistClientArc,
        local_bilibili: BilibiliClientArc,
        local_emby: EmbyClientArc,
    ) -> Self {
        Self {
            local_alist,
            local_bilibili,
            local_emby,
        }
    }

    /// Get the local Alist client.
    #[must_use]
    pub fn local_alist_client(&self) -> AlistClientArc {
        self.local_alist.clone()
    }

    /// Get the local Bilibili client.
    #[must_use]
    pub fn local_bilibili_client(&self) -> BilibiliClientArc {
        self.local_bilibili.clone()
    }

    /// Get the local Emby client.
    #[must_use]
    pub fn local_emby_client(&self) -> EmbyClientArc {
        self.local_emby.clone()
    }

    /// Resolve an Alist client: use remote if channel provided, otherwise local.
    ///
    /// This is the preferred method for obtaining an Alist client.
    pub fn resolve_alist_client(&self, remote_channel: Option<tonic::transport::Channel>) -> AlistClientArc {
        match remote_channel {
            Some(channel) => create_remote_alist_client(channel),
            None => self.local_alist_client(),
        }
    }

    /// Resolve a Bilibili client: use remote if channel provided, otherwise local.
    ///
    /// This is the preferred method for obtaining a Bilibili client.
    pub fn resolve_bilibili_client(&self, remote_channel: Option<tonic::transport::Channel>) -> BilibiliClientArc {
        match remote_channel {
            Some(channel) => create_remote_bilibili_client(channel),
            None => self.local_bilibili_client(),
        }
    }

    /// Resolve an Emby client: use remote if channel provided, otherwise local.
    ///
    /// This is the preferred method for obtaining an Emby client.
    pub fn resolve_emby_client(&self, remote_channel: Option<tonic::transport::Channel>) -> EmbyClientArc {
        match remote_channel {
            Some(channel) => create_remote_emby_client(channel),
            None => self.local_emby_client(),
        }
    }
}

// ============================================================================
// Global ProviderClientManager (for backward compatibility)
// ============================================================================

/// Global default `ProviderClientManager` instance.
///
/// This is used by `load_local_xxx_client()` functions for backward compatibility.
/// New code should prefer creating a `ProviderClientManager` explicitly and
/// passing it through the dependency injection system.
static GLOBAL_CLIENT_MANAGER: std::sync::LazyLock<ProviderClientManager> = std::sync::LazyLock::new(ProviderClientManager::new);

/// Get the global `ProviderClientManager` instance.
///
/// Prefer using dependency injection over this function.
#[must_use]
pub fn global_client_manager() -> &'static ProviderClientManager {
    &GLOBAL_CLIENT_MANAGER
}

// ============================================================================
// Alist Client
// ============================================================================

/// Type alias for Alist client
pub type AlistClientArc = Arc<dyn AlistInterface>;

/// Load local Alist client (from global manager).
///
/// Prefer using `ProviderClientManager::local_alist_client()` with dependency injection.
pub fn load_local_alist_client() -> AlistClientArc {
    GLOBAL_CLIENT_MANAGER.local_alist_client()
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
        let response = tokio::time::timeout(
            GRPC_REQUEST_TIMEOUT,
            client.login(tonic::Request::new(request)),
        )
        .await
        .map_err(|_| AlistError::Network(format!(
            "gRPC request timeout ({}s) for login",
            GRPC_REQUEST_TIMEOUT.as_secs(),
        )))?
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
            ProviderClientError::Http { status, url, .. } => Self::UpstreamHttp { status: status.as_u16(), url },
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

/// Load local Bilibili client (from global manager).
///
/// Prefer using `ProviderClientManager::local_bilibili_client()` with dependency injection.
pub fn load_local_bilibili_client() -> BilibiliClientArc {
    GLOBAL_CLIENT_MANAGER.local_bilibili_client()
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


// Note: Local moka-based `CachedBilibiliClient` was removed.
// Caching is handled solely by the Redis cache-aside layer in the API/service
// tier, which ensures consistency across replicas in a multi-node deployment.
// A local in-process cache would serve stale data after another node invalidates
// the cache, leading to subtle inconsistencies.

// ============================================================================
// Emby Client
// ============================================================================

use synctv_media_providers::emby::{EmbyError, EmbyInterface};

/// Type alias for Emby client
pub type EmbyClientArc = Arc<dyn EmbyInterface>;

/// Load local Emby client (from global manager).
///
/// Prefer using `ProviderClientManager::local_emby_client()` with dependency injection.
pub fn load_local_emby_client() -> EmbyClientArc {
    GLOBAL_CLIENT_MANAGER.local_emby_client()
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that ProviderClientManager can be created with default clients
    #[test]
    fn test_provider_client_manager_new() {
        let manager = ProviderClientManager::new();

        // Verify we can get clients
        let _alist = manager.local_alist_client();
        let _bilibili = manager.local_bilibili_client();
        let _emby = manager.local_emby_client();
    }

    /// Test that ProviderClientManager implements Clone
    #[test]
    fn test_provider_client_manager_clone() {
        let manager = ProviderClientManager::new();
        let cloned = manager.clone();

        // Both managers should return the same client instances (Arc clone)
        let client1 = manager.local_alist_client();
        let client2 = cloned.local_alist_client();

        // Arc::ptr_eq checks if they point to the same allocation
        assert!(Arc::ptr_eq(&client1, &client2));
    }

    /// Test that ProviderClientManager::default() works
    #[test]
    fn test_provider_client_manager_default() {
        let manager = ProviderClientManager::default();
        let _ = manager.local_alist_client();
    }

    /// Test that resolve_alist_client returns local client when no channel is provided
    #[test]
    fn test_resolve_alist_client_returns_local_when_no_channel() {
        let manager = ProviderClientManager::new();
        let local_client = manager.local_alist_client();
        let resolved_client = manager.resolve_alist_client(None);

        assert!(Arc::ptr_eq(&local_client, &resolved_client));
    }

    /// Test that resolve_bilibili_client returns local client when no channel is provided
    #[test]
    fn test_resolve_bilibili_client_returns_local_when_no_channel() {
        let manager = ProviderClientManager::new();
        let local_client = manager.local_bilibili_client();
        let resolved_client = manager.resolve_bilibili_client(None);

        assert!(Arc::ptr_eq(&local_client, &resolved_client));
    }

    /// Test that resolve_emby_client returns local client when no channel is provided
    #[test]
    fn test_resolve_emby_client_returns_local_when_no_channel() {
        let manager = ProviderClientManager::new();
        let local_client = manager.local_emby_client();
        let resolved_client = manager.resolve_emby_client(None);

        assert!(Arc::ptr_eq(&local_client, &resolved_client));
    }

    /// Test that global client manager provides consistent clients
    #[test]
    fn test_global_client_manager_consistency() {
        let client1 = load_local_alist_client();
        let client2 = load_local_alist_client();

        // Both calls should return the same client (from global manager)
        assert!(Arc::ptr_eq(&client1, &client2));
    }

    /// Test that global_client_manager returns a valid reference
    #[test]
    fn test_global_client_manager_returns_valid_reference() {
        let manager = global_client_manager();
        let _ = manager.local_alist_client();
        let _ = manager.local_bilibili_client();
        let _ = manager.local_emby_client();
    }

    /// Test that multiple calls to global_client_manager return the same instance
    #[test]
    fn test_global_client_manager_singleton() {
        let manager1 = global_client_manager() as *const ProviderClientManager;
        let manager2 = global_client_manager() as *const ProviderClientManager;

        assert_eq!(manager1, manager2);
    }

    /// Test backward compatibility: load_local_xxx_client functions work
    #[test]
    fn test_backward_compatibility_load_functions() {
        // These functions should work as before
        let _alist = load_local_alist_client();
        let _bilibili = load_local_bilibili_client();
        let _emby = load_local_emby_client();
    }

    /// Test that create_remote_xxx_client functions work
    #[test]
    fn test_create_remote_client_functions() {
        // Note: We can't actually test with a real channel here, but we can
        // verify the function signatures compile correctly
        fn _test_alist(channel: tonic::transport::Channel) -> AlistClientArc {
            create_remote_alist_client(channel)
        }
        fn _test_bilibili(channel: tonic::transport::Channel) -> BilibiliClientArc {
            create_remote_bilibili_client(channel)
        }
        fn _test_emby(channel: tonic::transport::Channel) -> EmbyClientArc {
            create_remote_emby_client(channel)
        }
        // Just verify they exist and compile
        assert!(true);
    }

    /// Test that ProviderClientManager::with_custom_clients allows mock injection
    #[test]
    fn test_custom_clients_injection() {
        // Create custom clients
        let custom_alist: AlistClientArc = Arc::new(synctv_media_providers::alist::AlistService::new());
        let custom_bilibili: BilibiliClientArc = Arc::new(synctv_media_providers::bilibili::BilibiliService::new());
        let custom_emby: EmbyClientArc = Arc::new(synctv_media_providers::emby::EmbyService::new());

        // Store Arc pointers for comparison
        let alist_ptr = Arc::as_ptr(&custom_alist);
        let bilibili_ptr = Arc::as_ptr(&custom_bilibili);
        let emby_ptr = Arc::as_ptr(&custom_emby);

        // Create manager with custom clients
        let manager = ProviderClientManager::with_custom_clients(
            custom_alist,
            custom_bilibili,
            custom_emby,
        );

        // Verify the manager uses the custom clients
        let alist = manager.local_alist_client();
        let bilibili = manager.local_bilibili_client();
        let emby = manager.local_emby_client();

        assert_eq!(Arc::as_ptr(&alist), alist_ptr);
        assert_eq!(Arc::as_ptr(&bilibili), bilibili_ptr);
        assert_eq!(Arc::as_ptr(&emby), emby_ptr);
    }
}

