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
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Duration;
use synctv_media_providers::alist::{AlistError, AlistInterface};
use synctv_media_providers::grpc::alist::{FsGetResp, FsListResp, FsOtherResp};
use tonic::{Code, Request, Status};

#[cfg(test)]
static PROVIDER_CLIENT_MANAGER_MARKER_SEQ: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone, Debug)]
pub struct RemoteProviderConnection {
    channel: tonic::transport::Channel,
    auth_secret: Option<Arc<str>>,
}

impl RemoteProviderConnection {
    #[must_use]
    pub fn new(channel: tonic::transport::Channel, auth_secret: Option<impl Into<String>>) -> Self {
        Self {
            channel,
            auth_secret: auth_secret.map(|secret| Arc::<str>::from(secret.into())),
        }
    }

    #[must_use]
    pub fn channel(&self) -> tonic::transport::Channel {
        self.channel.clone()
    }

    #[must_use]
    pub fn auth_secret(&self) -> Option<&str> {
        self.auth_secret.as_deref()
    }
}

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
        ) -> ::core::pin::Pin<
            Box<
                dyn ::core::future::Future<Output = Result<$resp, $error>>
                    + ::core::marker::Send
                    + 'async_trait,
            >,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                use $client_mod as _client_mod;
                let mut client = _client_mod::$client_name::new(self.connection.channel());
                let request = build_grpc_request(self.connection.auth_secret(), request)
                    .map_err(<$error>::from)?;
                let response = tokio::time::timeout(GRPC_REQUEST_TIMEOUT, client.$method(request))
                    .await
                    .map_err(|_| {
                        <$error>::Network(format!(
                            "gRPC request timeout ({}s) for {}",
                            GRPC_REQUEST_TIMEOUT.as_secs(),
                            stringify!($method),
                        ))
                    })?
                    .map_err(|e| <$error>::from(map_grpc_status(stringify!($method), e)))?;
                Ok(response.into_inner())
            })
        }
    };
}

fn build_grpc_request<T>(
    auth_secret: Option<&str>,
    payload: T,
) -> Result<Request<T>, synctv_media_providers::ProviderClientError> {
    let mut request = Request::new(payload);
    let Some(auth_secret) = auth_secret
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
    else {
        return Ok(request);
    };

    let metadata_value = auth_secret.parse().map_err(|e| {
        synctv_media_providers::ProviderClientError::InvalidHeader(format!(
            "invalid x-provider-secret metadata value: {e}"
        ))
    })?;

    request
        .metadata_mut()
        .insert("x-provider-secret", metadata_value);
    Ok(request)
}

pub(crate) fn validate_auth_secret(
    auth_secret: Option<&str>,
) -> Result<Option<&str>, ProviderError> {
    match auth_secret.map(str::trim) {
        Some("") => Err(ProviderError::InvalidConfig(
            "remote provider auth secret must not be empty".to_string(),
        )),
        Some(secret) => {
            if !secret.is_ascii() {
                return Err(ProviderError::InvalidConfig(
                    "remote provider auth secret must be valid ASCII gRPC metadata".to_string(),
                ));
            }
            tonic::metadata::MetadataValue::try_from(secret).map_err(|_| {
                ProviderError::InvalidConfig(
                    "remote provider auth secret must be valid ASCII gRPC metadata".to_string(),
                )
            })?;
            Ok(Some(secret))
        }
        None => Ok(None),
    }
}

fn grpc_status_to_http_status(code: Code) -> Option<reqwest::StatusCode> {
    match code {
        Code::NotFound => Some(reqwest::StatusCode::NOT_FOUND),
        Code::PermissionDenied => Some(reqwest::StatusCode::FORBIDDEN),
        Code::ResourceExhausted => Some(reqwest::StatusCode::TOO_MANY_REQUESTS),
        Code::FailedPrecondition | Code::AlreadyExists => Some(reqwest::StatusCode::CONFLICT),
        _ => None,
    }
}

fn map_grpc_status(context: &str, status: Status) -> synctv_media_providers::ProviderClientError {
    let message = status.message().to_string();
    match status.code() {
        Code::Unauthenticated => synctv_media_providers::ProviderClientError::Auth(message),
        Code::InvalidArgument => {
            synctv_media_providers::ProviderClientError::InvalidConfig(message)
        }
        Code::Unimplemented => synctv_media_providers::ProviderClientError::NotImplemented(message),
        Code::DeadlineExceeded | Code::Unavailable | Code::Cancelled => {
            synctv_media_providers::ProviderClientError::Network(format!(
                "gRPC {} for {}: {}",
                status.code(),
                context,
                message
            ))
        }
        code => {
            if let Some(http_status) = grpc_status_to_http_status(code) {
                synctv_media_providers::ProviderClientError::Http {
                    status: http_status,
                    url: format!("http://remote/{context}"),
                    retry_after_secs: None,
                    body: message,
                }
            } else {
                synctv_media_providers::ProviderClientError::Api {
                    code: i64::from(code as i32),
                    message,
                }
            }
        }
    }
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
    #[cfg(test)]
    marker: usize,
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
        let provider_client = synctv_common::http::build_provider_client()
            .expect("default provider HTTP client should build");
        Self::new_with_provider_http_client(provider_client)
    }

    /// Create a new `ProviderClientManager` with a shared local provider HTTP client.
    #[must_use]
    pub fn new_with_provider_http_client(client: reqwest::Client) -> Self {
        Self {
            local_alist: Arc::new(synctv_media_providers::alist::AlistService::with_client(
                client.clone(),
            )),
            local_bilibili: Arc::new(
                synctv_media_providers::bilibili::BilibiliService::with_client(client.clone()),
            ),
            local_emby: Arc::new(synctv_media_providers::emby::EmbyService::with_client(
                client,
            )),
            #[cfg(test)]
            marker: PROVIDER_CLIENT_MANAGER_MARKER_SEQ.fetch_add(1, AtomicOrdering::Relaxed),
        }
    }

    /// Create a manager from concrete local service implementations.
    #[must_use]
    pub fn with_local_services(
        alist: synctv_media_providers::alist::AlistService,
        bilibili: synctv_media_providers::bilibili::BilibiliService,
        emby: synctv_media_providers::emby::EmbyService,
    ) -> Self {
        Self {
            local_alist: Arc::new(alist),
            local_bilibili: Arc::new(bilibili),
            local_emby: Arc::new(emby),
            #[cfg(test)]
            marker: PROVIDER_CLIENT_MANAGER_MARKER_SEQ.fetch_add(1, AtomicOrdering::Relaxed),
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
            #[cfg(test)]
            marker: PROVIDER_CLIENT_MANAGER_MARKER_SEQ.fetch_add(1, AtomicOrdering::Relaxed),
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
    #[must_use]
    pub fn resolve_alist_client(
        &self,
        remote_connection: Option<RemoteProviderConnection>,
    ) -> AlistClientArc {
        match remote_connection {
            Some(connection) => create_remote_alist_client(connection),
            None => self.local_alist_client(),
        }
    }

    /// Resolve a Bilibili client: use remote if channel provided, otherwise local.
    ///
    /// This is the preferred method for obtaining a Bilibili client.
    #[must_use]
    pub fn resolve_bilibili_client(
        &self,
        remote_connection: Option<RemoteProviderConnection>,
    ) -> BilibiliClientArc {
        match remote_connection {
            Some(connection) => create_remote_bilibili_client(connection),
            None => self.local_bilibili_client(),
        }
    }

    /// Resolve an Emby client: use remote if channel provided, otherwise local.
    ///
    /// This is the preferred method for obtaining an Emby client.
    #[must_use]
    pub fn resolve_emby_client(
        &self,
        remote_connection: Option<RemoteProviderConnection>,
    ) -> EmbyClientArc {
        match remote_connection {
            Some(connection) => create_remote_emby_client(connection),
            None => self.local_emby_client(),
        }
    }

    #[cfg(test)]
    pub(crate) fn marker(&self) -> usize {
        self.marker
    }
}

// ============================================================================
// Alist Client
// ============================================================================

/// Type alias for Alist client
pub type AlistClientArc = Arc<dyn AlistInterface>;

/// Create remote Alist client (thin wrapper around gRPC client)
#[must_use]
pub fn create_remote_alist_client(connection: RemoteProviderConnection) -> AlistClientArc {
    Arc::new(GrpcAlistClient::new(connection))
}

/// Thin wrapper around gRPC client
///
/// Implements `AlistInterface` by delegating to gRPC client.
pub struct GrpcAlistClient {
    connection: RemoteProviderConnection,
}

impl GrpcAlistClient {
    #[must_use]
    pub fn new(connection: RemoteProviderConnection) -> Self {
        Self { connection }
    }
}

#[async_trait]
impl AlistInterface for GrpcAlistClient {
    impl_grpc_method!(
        synctv_media_providers::grpc::alist::alist_client,
        AlistClient,
        AlistError,
        fs_get,
        synctv_media_providers::grpc::alist::FsGetReq,
        FsGetResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::alist::alist_client,
        AlistClient,
        AlistError,
        fs_list,
        synctv_media_providers::grpc::alist::FsListReq,
        FsListResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::alist::alist_client,
        AlistClient,
        AlistError,
        fs_other,
        synctv_media_providers::grpc::alist::FsOtherReq,
        FsOtherResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::alist::alist_client,
        AlistClient,
        AlistError,
        fs_search,
        synctv_media_providers::grpc::alist::FsSearchReq,
        synctv_media_providers::grpc::alist::FsSearchResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::alist::alist_client,
        AlistClient,
        AlistError,
        me,
        synctv_media_providers::grpc::alist::MeReq,
        synctv_media_providers::grpc::alist::MeResp
    );

    // login has a non-standard return: extracts `.token` from response
    async fn login(
        &self,
        request: synctv_media_providers::grpc::alist::LoginReq,
    ) -> Result<String, AlistError> {
        use synctv_media_providers::grpc::alist::alist_client::AlistClient;
        let mut client = AlistClient::new(self.connection.channel());
        let request = build_grpc_request(self.connection.auth_secret(), request)?;
        let response = tokio::time::timeout(GRPC_REQUEST_TIMEOUT, client.login(request))
            .await
            .map_err(|_| {
                AlistError::Network(format!(
                    "gRPC request timeout ({}s) for login",
                    GRPC_REQUEST_TIMEOUT.as_secs(),
                ))
            })?
            .map_err(|e| map_grpc_status("login", e))?;
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

        Ok(other_data
            .video_preview_play_info
            .map(|preview| AlistVideoPreview {
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
            ProviderClientError::NotImplemented(msg) => {
                Self::ApiError(format!("Not implemented: {msg}"))
            }
            ProviderClientError::Http { status, url, .. } => Self::UpstreamHttp {
                status: status.as_u16(),
                url,
            },
            ProviderClientError::ResponseTooLarge { size } => {
                Self::ApiError(format!("Response too large ({size} bytes)"))
            }
        }
    }
}

// ============================================================================
// Bilibili Client
// ============================================================================

use synctv_media_providers::bilibili::{BilibiliError, BilibiliInterface};

/// Type alias for Bilibili client
pub type BilibiliClientArc = Arc<dyn BilibiliInterface>;

/// Create remote Bilibili client (thin wrapper around gRPC client)
#[must_use]
pub fn create_remote_bilibili_client(connection: RemoteProviderConnection) -> BilibiliClientArc {
    Arc::new(GrpcBilibiliClient::new(connection))
}

/// Thin wrapper around gRPC client for Bilibili
pub struct GrpcBilibiliClient {
    connection: RemoteProviderConnection,
}

impl GrpcBilibiliClient {
    #[must_use]
    pub fn new(connection: RemoteProviderConnection) -> Self {
        Self { connection }
    }
}

#[async_trait]
impl BilibiliInterface for GrpcBilibiliClient {
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        new_qr_code,
        synctv_media_providers::grpc::bilibili::Empty,
        synctv_media_providers::grpc::bilibili::NewQrCodeResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        login_with_qr_code,
        synctv_media_providers::grpc::bilibili::LoginWithQrCodeReq,
        synctv_media_providers::grpc::bilibili::LoginWithQrCodeResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        new_captcha,
        synctv_media_providers::grpc::bilibili::Empty,
        synctv_media_providers::grpc::bilibili::NewCaptchaResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        new_sms,
        synctv_media_providers::grpc::bilibili::NewSmsReq,
        synctv_media_providers::grpc::bilibili::NewSmsResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        login_with_sms,
        synctv_media_providers::grpc::bilibili::LoginWithSmsReq,
        synctv_media_providers::grpc::bilibili::LoginWithSmsResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        parse_video_page,
        synctv_media_providers::grpc::bilibili::ParseVideoPageReq,
        synctv_media_providers::grpc::bilibili::VideoPageInfo
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        get_video_url,
        synctv_media_providers::grpc::bilibili::GetVideoUrlReq,
        synctv_media_providers::grpc::bilibili::VideoUrl
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        get_dash_video_url,
        synctv_media_providers::grpc::bilibili::GetDashVideoUrlReq,
        synctv_media_providers::grpc::bilibili::GetDashVideoUrlResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        get_subtitles,
        synctv_media_providers::grpc::bilibili::GetSubtitlesReq,
        synctv_media_providers::grpc::bilibili::GetSubtitlesResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        parse_pgc_page,
        synctv_media_providers::grpc::bilibili::ParsePgcPageReq,
        synctv_media_providers::grpc::bilibili::VideoPageInfo
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        get_pgcurl,
        synctv_media_providers::grpc::bilibili::GetPgcurlReq,
        synctv_media_providers::grpc::bilibili::VideoUrl
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        get_dash_pgcurl,
        synctv_media_providers::grpc::bilibili::GetDashPgcurlReq,
        synctv_media_providers::grpc::bilibili::GetDashPgcurlResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        user_info,
        synctv_media_providers::grpc::bilibili::UserInfoReq,
        synctv_media_providers::grpc::bilibili::UserInfoResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        r#match,
        synctv_media_providers::grpc::bilibili::MatchReq,
        synctv_media_providers::grpc::bilibili::MatchResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        get_live_streams,
        synctv_media_providers::grpc::bilibili::GetLiveStreamsReq,
        synctv_media_providers::grpc::bilibili::GetLiveStreamsResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        parse_live_page,
        synctv_media_providers::grpc::bilibili::ParseLivePageReq,
        synctv_media_providers::grpc::bilibili::VideoPageInfo
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::bilibili::bilibili_client,
        BilibiliClient,
        BilibiliError,
        get_live_danmu_info,
        synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoReq,
        synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoResp
    );
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

/// Create remote Emby client (thin wrapper around gRPC client)
#[must_use]
pub fn create_remote_emby_client(connection: RemoteProviderConnection) -> EmbyClientArc {
    Arc::new(GrpcEmbyClient::new(connection))
}

/// Thin wrapper around gRPC client for Emby
pub struct GrpcEmbyClient {
    connection: RemoteProviderConnection,
}

impl GrpcEmbyClient {
    #[must_use]
    pub fn new(connection: RemoteProviderConnection) -> Self {
        Self { connection }
    }
}

#[async_trait]
impl EmbyInterface for GrpcEmbyClient {
    impl_grpc_method!(
        synctv_media_providers::grpc::emby::emby_client,
        EmbyClient,
        EmbyError,
        login,
        synctv_media_providers::grpc::emby::LoginReq,
        synctv_media_providers::grpc::emby::LoginResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::emby::emby_client,
        EmbyClient,
        EmbyError,
        me,
        synctv_media_providers::grpc::emby::MeReq,
        synctv_media_providers::grpc::emby::MeResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::emby::emby_client,
        EmbyClient,
        EmbyError,
        get_items,
        synctv_media_providers::grpc::emby::GetItemsReq,
        synctv_media_providers::grpc::emby::GetItemsResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::emby::emby_client,
        EmbyClient,
        EmbyError,
        get_item,
        synctv_media_providers::grpc::emby::GetItemReq,
        synctv_media_providers::grpc::emby::Item
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::emby::emby_client,
        EmbyClient,
        EmbyError,
        fs_list,
        synctv_media_providers::grpc::emby::FsListReq,
        synctv_media_providers::grpc::emby::FsListResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::emby::emby_client,
        EmbyClient,
        EmbyError,
        get_system_info,
        synctv_media_providers::grpc::emby::SystemInfoReq,
        synctv_media_providers::grpc::emby::SystemInfoResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::emby::emby_client,
        EmbyClient,
        EmbyError,
        logout,
        synctv_media_providers::grpc::emby::LogoutReq,
        synctv_media_providers::grpc::emby::Empty
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::emby::emby_client,
        EmbyClient,
        EmbyError,
        playback_info,
        synctv_media_providers::grpc::emby::PlaybackInfoReq,
        synctv_media_providers::grpc::emby::PlaybackInfoResp
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::emby::emby_client,
        EmbyClient,
        EmbyError,
        delete_active_encodings,
        synctv_media_providers::grpc::emby::DeleteActiveEncodingsReq,
        synctv_media_providers::grpc::emby::Empty
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::emby::emby_client,
        EmbyClient,
        EmbyError,
        report_playback_start,
        synctv_media_providers::grpc::emby::ReportPlaybackStartReq,
        synctv_media_providers::grpc::emby::Empty
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::emby::emby_client,
        EmbyClient,
        EmbyError,
        report_playback_stop,
        synctv_media_providers::grpc::emby::ReportPlaybackStopReq,
        synctv_media_providers::grpc::emby::Empty
    );
    impl_grpc_method!(
        synctv_media_providers::grpc::emby::emby_client,
        EmbyClient,
        EmbyError,
        report_playback_progress,
        synctv_media_providers::grpc::emby::ReportPlaybackProgressReq,
        synctv_media_providers::grpc::emby::Empty
    );
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;
    use synctv_media_providers::ProviderClientError;
    use tonic::metadata::MetadataValue;

    /// Test that `resolve_alist_client` returns local client when no channel is provided
    #[test]
    fn test_resolve_alist_client_returns_local_when_no_channel() {
        let manager = ProviderClientManager::new();
        let local_client = manager.local_alist_client();
        let resolved_client = manager.resolve_alist_client(None);

        assert!(Arc::ptr_eq(&local_client, &resolved_client));
    }

    /// Test that `resolve_bilibili_client` returns local client when no channel is provided
    #[test]
    fn test_resolve_bilibili_client_returns_local_when_no_channel() {
        let manager = ProviderClientManager::new();
        let local_client = manager.local_bilibili_client();
        let resolved_client = manager.resolve_bilibili_client(None);

        assert!(Arc::ptr_eq(&local_client, &resolved_client));
    }

    /// Test that `resolve_emby_client` returns local client when no channel is provided
    #[test]
    fn test_resolve_emby_client_returns_local_when_no_channel() {
        let manager = ProviderClientManager::new();
        let local_client = manager.local_emby_client();
        let resolved_client = manager.resolve_emby_client(None);

        assert!(Arc::ptr_eq(&local_client, &resolved_client));
    }

    /// Test that `ProviderClientManager::with_custom_clients` allows mock injection
    #[test]
    fn test_custom_clients_injection() {
        // Create custom clients
        let custom_alist: AlistClientArc =
            Arc::new(synctv_media_providers::alist::AlistService::new());
        let custom_bilibili: BilibiliClientArc =
            Arc::new(synctv_media_providers::bilibili::BilibiliService::new());
        let custom_emby: EmbyClientArc = Arc::new(synctv_media_providers::emby::EmbyService::new());

        // Store Arc pointers for comparison
        let alist_ptr = Arc::as_ptr(&custom_alist);
        let bilibili_ptr = Arc::as_ptr(&custom_bilibili);
        let emby_ptr = Arc::as_ptr(&custom_emby);

        // Create manager with custom clients
        let manager =
            ProviderClientManager::with_custom_clients(custom_alist, custom_bilibili, custom_emby);

        // Verify the manager uses the custom clients
        let alist = manager.local_alist_client();
        let bilibili = manager.local_bilibili_client();
        let emby = manager.local_emby_client();

        assert_eq!(Arc::as_ptr(&alist), alist_ptr);
        assert_eq!(Arc::as_ptr(&bilibili), bilibili_ptr);
        assert_eq!(Arc::as_ptr(&emby), emby_ptr);
    }

    #[test]
    fn test_build_grpc_request_inserts_x_provider_secret() {
        let request =
            build_grpc_request(Some("shared-secret"), 42_u32).expect("request should build");
        assert_eq!(request.get_ref(), &42_u32);
        assert_eq!(
            request.metadata().get("x-provider-secret"),
            Some(&MetadataValue::from_static("shared-secret"))
        );
    }

    #[test]
    fn test_build_grpc_request_omits_header_when_secret_is_blank() {
        let request = build_grpc_request(Some("   "), 42_u32).expect("request should build");
        assert_eq!(request.get_ref(), &42_u32);
        assert!(
            request.metadata().get("x-provider-secret").is_none(),
            "blank secrets must not produce a malformed header"
        );
    }

    #[test]
    fn test_validate_auth_secret_rejects_empty_secret() {
        let error = validate_auth_secret(Some("   ")).expect_err("empty secret must fail");
        assert!(matches!(
            error,
            ProviderError::InvalidConfig(message)
                if message.contains("auth secret must not be empty")
        ));
    }

    #[test]
    fn test_validate_auth_secret_allows_absent_secret_only_for_non_remote_callers() {
        assert_eq!(validate_auth_secret(None).unwrap(), None);
        assert_eq!(
            validate_auth_secret(Some("  shared-secret  ")).unwrap(),
            Some("shared-secret")
        );
    }

    #[test]
    fn test_validate_auth_secret_rejects_non_ascii_secret() {
        let error = validate_auth_secret(Some("密钥")).expect_err("non-ASCII secret must fail");
        assert!(matches!(
            error,
            ProviderError::InvalidConfig(message)
                if message.contains("valid ASCII gRPC metadata")
        ));
    }

    #[test]
    fn test_validate_auth_secret_rejects_control_characters() {
        let error =
            validate_auth_secret(Some("shared\nsecret")).expect_err("control chars must fail");
        assert!(matches!(
            error,
            ProviderError::InvalidConfig(message)
                if message.contains("valid ASCII gRPC metadata")
        ));
    }

    #[test]
    fn test_map_grpc_status_unauthenticated_to_auth() {
        let error = map_grpc_status("login", Status::unauthenticated("Invalid provider secret"));
        assert!(matches!(
            error,
            ProviderClientError::Auth(message) if message == "Invalid provider secret"
        ));
    }

    #[test]
    fn test_map_grpc_status_invalid_argument_to_invalid_config() {
        let error = map_grpc_status("fs_get", Status::invalid_argument("missing host parameter"));
        assert!(matches!(
            error,
            ProviderClientError::InvalidConfig(message) if message == "missing host parameter"
        ));
    }

    #[test]
    fn test_map_grpc_status_not_found_to_http_404() {
        let error = map_grpc_status("me", Status::not_found("user not found"));
        assert!(matches!(
            error,
            ProviderClientError::Http { status, ref url, ref body, retry_after_secs: None }
                if status == StatusCode::NOT_FOUND
                    && url == "http://remote/me"
                    && body == "user not found"
        ));
    }

    #[test]
    fn test_map_grpc_status_unimplemented_to_not_implemented() {
        let error = map_grpc_status("future_method", Status::unimplemented("not available"));
        assert!(matches!(
            error,
            ProviderClientError::NotImplemented(message) if message == "not available"
        ));
    }
}
