//! Remote provider client adapters.
//!
//! These adapters implement the upstream provider traits over the remote
//! transport connection. `provider_client` owns local client construction and
//! re-exports the stable facade used by provider adapters.

use super::provider_client::{AlistClientArc, BilibiliClientArc, EmbyClientArc};
use super::remote_transport::{
    apply_provider_client_compression, build_remote_request, execute_remote_call,
    map_remote_status, RemoteProviderConnection,
};
use super::upstream_transport::alist::{FsGetResp, FsListResp, FsOtherResp};
use super::ProviderError;
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::Arc;
use synctv_media_providers::alist::AlistInterface;
use synctv_media_providers::bilibili::BilibiliInterface;
use synctv_media_providers::emby::EmbyInterface;
use synctv_media_providers::ProviderClientError;

macro_rules! impl_remote_method {
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
                let mut client = apply_provider_client_compression(
                    self.connection
                        .build_provider_client(_client_mod::$client_name::new),
                    self.connection.transport_compression_enabled(),
                );
                let request = build_remote_request(self.connection.auth_secret(), request)
                    .map_err(<$error>::from)?;
                let response =
                    execute_remote_call(&self.connection, stringify!($method), async move {
                        client
                            .$method(request)
                            .await
                            .map_err(|e| <$error>::from(map_remote_status(stringify!($method), &e)))
                    })
                    .await?;
                Ok(response.into_inner())
            })
        }
    };
}

#[must_use]
pub(crate) fn create_remote_alist_client(connection: RemoteProviderConnection) -> AlistClientArc {
    Arc::new(RemoteAlistClient::new(connection))
}

struct RemoteAlistClient {
    connection: RemoteProviderConnection,
}

impl RemoteAlistClient {
    #[must_use]
    pub const fn new(connection: RemoteProviderConnection) -> Self {
        Self { connection }
    }
}

#[async_trait]
impl AlistInterface for RemoteAlistClient {
    impl_remote_method!(
        super::upstream_transport::alist::alist_client,
        AlistClient,
        ProviderClientError,
        fs_get,
        super::upstream_transport::alist::FsGetReq,
        FsGetResp
    );
    impl_remote_method!(
        super::upstream_transport::alist::alist_client,
        AlistClient,
        ProviderClientError,
        fs_list,
        super::upstream_transport::alist::FsListReq,
        FsListResp
    );
    impl_remote_method!(
        super::upstream_transport::alist::alist_client,
        AlistClient,
        ProviderClientError,
        fs_other,
        super::upstream_transport::alist::FsOtherReq,
        FsOtherResp
    );
    impl_remote_method!(
        super::upstream_transport::alist::alist_client,
        AlistClient,
        ProviderClientError,
        fs_search,
        super::upstream_transport::alist::FsSearchReq,
        super::upstream_transport::alist::FsSearchResp
    );
    impl_remote_method!(
        super::upstream_transport::alist::alist_client,
        AlistClient,
        ProviderClientError,
        me,
        super::upstream_transport::alist::MeReq,
        super::upstream_transport::alist::MeResp
    );

    async fn login(
        &self,
        request: super::upstream_transport::alist::LoginReq,
    ) -> Result<String, ProviderClientError> {
        use super::upstream_transport::alist::alist_client::AlistClient;
        let mut client = apply_provider_client_compression(
            self.connection.build_provider_client(AlistClient::new),
            self.connection.transport_compression_enabled(),
        );
        let request = build_remote_request(self.connection.auth_secret(), request)?;
        let response = execute_remote_call(&self.connection, "login", async move {
            client
                .login(request)
                .await
                .map_err(|e| map_remote_status("login", &e))
        })
        .await?;
        Ok(response.into_inner().token)
    }
}

#[derive(Debug, Clone)]
pub struct AlistFileInfo {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub raw_url: String,
    pub provider: String,
    pub thumb: String,
    pub related: Vec<AlistRelatedFile>,
}

#[derive(Debug, Clone)]
pub struct AlistRelatedFile {
    pub name: String,
    pub is_dir: bool,
    pub raw_url: String,
    pub provider: String,
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
            related: data
                .related
                .into_iter()
                .map(|related| AlistRelatedFile {
                    name: related.name,
                    is_dir: related.is_dir,
                    raw_url: related.raw_url,
                    provider: related.provider,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AlistVideoPreview {
    pub transcoding_tasks: Vec<AlistTranscodingTask>,
    pub subtitle_tasks: Vec<AlistSubtitleTask>,
    pub drive_id: String,
    pub file_id: String,
    pub provider: String,
    pub category: String,
    pub duration: f64,
    pub width: u64,
    pub height: u64,
}

impl AlistVideoPreview {
    #[must_use]
    pub fn from_fs_other_resp(other_data: FsOtherResp) -> Option<Self> {
        other_data.video_preview_play_info.map(|preview| Self {
            transcoding_tasks: preview
                .live_transcoding_task_list
                .into_iter()
                .map(|task| AlistTranscodingTask {
                    template_name: task.template_name,
                    template_id: task.template_id,
                    template_width: task.template_width,
                    template_height: task.template_height,
                    stage: task.stage,
                    status: task.status,
                    url: task.url,
                })
                .collect(),
            subtitle_tasks: preview
                .live_transcoding_subtitle_task_list
                .into_iter()
                .map(|sub| AlistSubtitleTask {
                    language: sub.language,
                    status: sub.status,
                    url: sub.url,
                })
                .collect(),
            drive_id: other_data.drive_id,
            file_id: other_data.file_id,
            provider: other_data.provider,
            category: preview.category,
            duration: preview.meta.as_ref().map_or(0.0, |m| m.duration),
            width: preview.meta.as_ref().map_or(0, |m| m.width),
            height: preview.meta.as_ref().map_or(0, |m| m.height),
        })
    }
}

#[derive(Debug, Clone)]
pub struct AlistTranscodingTask {
    pub template_name: String,
    pub template_id: String,
    pub template_width: u64,
    pub template_height: u64,
    pub stage: String,
    pub status: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct AlistSubtitleTask {
    pub language: String,
    #[allow(dead_code)]
    pub status: String,
    pub url: String,
}

#[async_trait]
pub(crate) trait AlistClientExt {
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
        let request = super::upstream_transport::alist::FsOtherReq {
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

        Ok(AlistVideoPreview::from_fs_other_resp(other_data))
    }
}

#[must_use]
pub(crate) fn create_remote_bilibili_client(
    connection: RemoteProviderConnection,
) -> BilibiliClientArc {
    Arc::new(RemoteBilibiliClient::new(connection))
}

struct RemoteBilibiliClient {
    connection: RemoteProviderConnection,
}

impl RemoteBilibiliClient {
    #[must_use]
    pub const fn new(connection: RemoteProviderConnection) -> Self {
        Self { connection }
    }
}

#[async_trait]
impl BilibiliInterface for RemoteBilibiliClient {
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        new_qr_code,
        super::upstream_transport::bilibili::Empty,
        super::upstream_transport::bilibili::NewQrCodeResp
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        login_with_qr_code,
        super::upstream_transport::bilibili::LoginWithQrCodeReq,
        super::upstream_transport::bilibili::LoginWithQrCodeResp
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        new_captcha,
        super::upstream_transport::bilibili::Empty,
        super::upstream_transport::bilibili::NewCaptchaResp
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        new_sms,
        super::upstream_transport::bilibili::NewSmsReq,
        super::upstream_transport::bilibili::NewSmsResp
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        login_with_sms,
        super::upstream_transport::bilibili::LoginWithSmsReq,
        super::upstream_transport::bilibili::LoginWithSmsResp
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        parse_video_page,
        super::upstream_transport::bilibili::ParseVideoPageReq,
        super::upstream_transport::bilibili::VideoPageInfo
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        get_video_url,
        super::upstream_transport::bilibili::GetVideoUrlReq,
        super::upstream_transport::bilibili::VideoUrl
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        get_dash_video_url,
        super::upstream_transport::bilibili::GetDashVideoUrlReq,
        super::upstream_transport::bilibili::GetDashVideoUrlResp
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        get_subtitles,
        super::upstream_transport::bilibili::GetSubtitlesReq,
        super::upstream_transport::bilibili::GetSubtitlesResp
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        parse_pgc_page,
        super::upstream_transport::bilibili::ParsePgcPageReq,
        super::upstream_transport::bilibili::VideoPageInfo
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        get_pgcurl,
        super::upstream_transport::bilibili::GetPgcurlReq,
        super::upstream_transport::bilibili::VideoUrl
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        get_dash_pgcurl,
        super::upstream_transport::bilibili::GetDashPgcurlReq,
        super::upstream_transport::bilibili::GetDashPgcurlResp
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        user_info,
        super::upstream_transport::bilibili::UserInfoReq,
        super::upstream_transport::bilibili::UserInfoResp
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        r#match,
        super::upstream_transport::bilibili::MatchReq,
        super::upstream_transport::bilibili::MatchResp
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        get_live_streams,
        super::upstream_transport::bilibili::GetLiveStreamsReq,
        super::upstream_transport::bilibili::GetLiveStreamsResp
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        parse_live_page,
        super::upstream_transport::bilibili::ParseLivePageReq,
        super::upstream_transport::bilibili::VideoPageInfo
    );
    impl_remote_method!(
        super::upstream_transport::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        get_live_danmu_info,
        super::upstream_transport::bilibili::GetLiveDanmuInfoReq,
        super::upstream_transport::bilibili::GetLiveDanmuInfoResp
    );

    fn watch_bilibili_live_danmaku<'life0, 'async_trait>(
        &'life0 self,
        request: super::upstream_transport::bilibili::WatchBilibiliLiveDanmakuReq,
    ) -> ::core::pin::Pin<
        Box<
            dyn ::core::future::Future<
                    Output = Result<
                        synctv_media_providers::bilibili::BilibiliLiveDanmakuStream,
                        ProviderClientError,
                    >,
                > + ::core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            use super::upstream_transport::bilibili::bilibili_client::BilibiliClient;
            let mut client = apply_provider_client_compression(
                self.connection.build_provider_client(BilibiliClient::new),
                self.connection.transport_compression_enabled(),
            );
            let request = build_remote_request(self.connection.auth_secret(), request)?;
            let response = execute_remote_call(
                &self.connection,
                "watch_bilibili_live_danmaku",
                async move {
                    client
                        .watch_bilibili_live_danmaku(request)
                        .await
                        .map_err(|e| map_remote_status("watch_bilibili_live_danmaku", &e))
                },
            )
            .await?;
            let stream = response.into_inner().map(|item| {
                item.map_err(|status| map_remote_status("watch_bilibili_live_danmaku", &status))
            });
            Ok(Box::pin(stream) as synctv_media_providers::bilibili::BilibiliLiveDanmakuStream)
        })
    }
}

#[must_use]
pub(crate) fn create_remote_emby_client(connection: RemoteProviderConnection) -> EmbyClientArc {
    Arc::new(RemoteEmbyClient::new(connection))
}

struct RemoteEmbyClient {
    connection: RemoteProviderConnection,
}

impl RemoteEmbyClient {
    #[must_use]
    pub const fn new(connection: RemoteProviderConnection) -> Self {
        Self { connection }
    }
}

#[async_trait]
impl EmbyInterface for RemoteEmbyClient {
    impl_remote_method!(
        super::upstream_transport::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        login,
        super::upstream_transport::emby::LoginReq,
        super::upstream_transport::emby::LoginResp
    );
    impl_remote_method!(
        super::upstream_transport::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        me,
        super::upstream_transport::emby::MeReq,
        super::upstream_transport::emby::MeResp
    );
    impl_remote_method!(
        super::upstream_transport::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        get_items,
        super::upstream_transport::emby::GetItemsReq,
        super::upstream_transport::emby::GetItemsResp
    );
    impl_remote_method!(
        super::upstream_transport::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        get_item,
        super::upstream_transport::emby::GetItemReq,
        super::upstream_transport::emby::Item
    );
    impl_remote_method!(
        super::upstream_transport::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        fs_list,
        super::upstream_transport::emby::FsListReq,
        super::upstream_transport::emby::FsListResp
    );
    impl_remote_method!(
        super::upstream_transport::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        get_system_info,
        super::upstream_transport::emby::SystemInfoReq,
        super::upstream_transport::emby::SystemInfoResp
    );
    impl_remote_method!(
        super::upstream_transport::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        logout,
        super::upstream_transport::emby::LogoutReq,
        super::upstream_transport::emby::Empty
    );
    impl_remote_method!(
        super::upstream_transport::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        playback_info,
        super::upstream_transport::emby::PlaybackInfoReq,
        super::upstream_transport::emby::PlaybackInfoResp
    );
    impl_remote_method!(
        super::upstream_transport::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        delete_active_encodings,
        super::upstream_transport::emby::DeleteActiveEncodingsReq,
        super::upstream_transport::emby::Empty
    );
    impl_remote_method!(
        super::upstream_transport::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        report_playback_start,
        super::upstream_transport::emby::ReportPlaybackStartReq,
        super::upstream_transport::emby::Empty
    );
    impl_remote_method!(
        super::upstream_transport::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        report_playback_stop,
        super::upstream_transport::emby::ReportPlaybackStopReq,
        super::upstream_transport::emby::Empty
    );
    impl_remote_method!(
        super::upstream_transport::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        report_playback_progress,
        super::upstream_transport::emby::ReportPlaybackProgressReq,
        super::upstream_transport::emby::Empty
    );
}
