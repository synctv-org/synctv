//! Remote provider client adapters.
//!
//! These adapters implement the upstream provider traits over the remote
//! transport connection. `provider_client` owns local client construction and
//! re-exports the stable facade used by provider adapters.

use super::{
    apply_provider_client_compression, build_remote_request, execute_remote_call,
    RemoteProviderConnection,
};
use crate::alist::AlistInterface;
use crate::bilibili::BilibiliInterface;
use crate::emby::EmbyInterface;
use crate::grpc::map_remote_status;
use crate::transport_dto::alist::{FsGetResp, FsListResp, FsOtherResp};
use crate::ProviderClientError;
use async_trait::async_trait;
use futures_util::StreamExt;
use std::sync::Arc;

type AlistClientArc = Arc<dyn AlistInterface>;
type BilibiliClientArc = Arc<dyn BilibiliInterface>;
type EmbyClientArc = Arc<dyn EmbyInterface>;

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
pub fn create_remote_alist_client(connection: RemoteProviderConnection) -> AlistClientArc {
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
        crate::grpc::alist::alist_client,
        AlistClient,
        ProviderClientError,
        fs_get,
        crate::transport_dto::alist::FsGetReq,
        FsGetResp
    );
    impl_remote_method!(
        crate::grpc::alist::alist_client,
        AlistClient,
        ProviderClientError,
        fs_list,
        crate::transport_dto::alist::FsListReq,
        FsListResp
    );
    impl_remote_method!(
        crate::grpc::alist::alist_client,
        AlistClient,
        ProviderClientError,
        fs_other,
        crate::transport_dto::alist::FsOtherReq,
        FsOtherResp
    );
    impl_remote_method!(
        crate::grpc::alist::alist_client,
        AlistClient,
        ProviderClientError,
        fs_search,
        crate::transport_dto::alist::FsSearchReq,
        crate::transport_dto::alist::FsSearchResp
    );
    impl_remote_method!(
        crate::grpc::alist::alist_client,
        AlistClient,
        ProviderClientError,
        me,
        crate::transport_dto::alist::MeReq,
        crate::transport_dto::alist::MeResp
    );

    async fn login(
        &self,
        request: crate::transport_dto::alist::LoginReq,
    ) -> Result<String, ProviderClientError> {
        use crate::grpc::alist::alist_client::AlistClient;
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

#[must_use]
pub fn create_remote_bilibili_client(connection: RemoteProviderConnection) -> BilibiliClientArc {
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
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        new_qr_code,
        crate::transport_dto::bilibili::Empty,
        crate::transport_dto::bilibili::NewQrCodeResp
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        login_with_qr_code,
        crate::transport_dto::bilibili::LoginWithQrCodeReq,
        crate::transport_dto::bilibili::LoginWithQrCodeResp
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        new_captcha,
        crate::transport_dto::bilibili::Empty,
        crate::transport_dto::bilibili::NewCaptchaResp
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        new_sms,
        crate::transport_dto::bilibili::NewSmsReq,
        crate::transport_dto::bilibili::NewSmsResp
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        login_with_sms,
        crate::transport_dto::bilibili::LoginWithSmsReq,
        crate::transport_dto::bilibili::LoginWithSmsResp
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        parse_video_page,
        crate::transport_dto::bilibili::ParseVideoPageReq,
        crate::transport_dto::bilibili::VideoPageInfo
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        get_video_url,
        crate::transport_dto::bilibili::GetVideoUrlReq,
        crate::transport_dto::bilibili::VideoUrl
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        get_dash_video_url,
        crate::transport_dto::bilibili::GetDashVideoUrlReq,
        crate::transport_dto::bilibili::GetDashVideoUrlResp
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        get_subtitles,
        crate::transport_dto::bilibili::GetSubtitlesReq,
        crate::transport_dto::bilibili::GetSubtitlesResp
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        parse_pgc_page,
        crate::transport_dto::bilibili::ParsePgcPageReq,
        crate::transport_dto::bilibili::VideoPageInfo
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        get_pgcurl,
        crate::transport_dto::bilibili::GetPgcurlReq,
        crate::transport_dto::bilibili::VideoUrl
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        get_dash_pgcurl,
        crate::transport_dto::bilibili::GetDashPgcurlReq,
        crate::transport_dto::bilibili::GetDashPgcurlResp
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        user_info,
        crate::transport_dto::bilibili::UserInfoReq,
        crate::transport_dto::bilibili::UserInfoResp
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        r#match,
        crate::transport_dto::bilibili::MatchReq,
        crate::transport_dto::bilibili::MatchResp
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        get_live_streams,
        crate::transport_dto::bilibili::GetLiveStreamsReq,
        crate::transport_dto::bilibili::GetLiveStreamsResp
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        parse_live_page,
        crate::transport_dto::bilibili::ParseLivePageReq,
        crate::transport_dto::bilibili::VideoPageInfo
    );
    impl_remote_method!(
        crate::grpc::bilibili::bilibili_client,
        BilibiliClient,
        ProviderClientError,
        get_live_danmu_info,
        crate::transport_dto::bilibili::GetLiveDanmuInfoReq,
        crate::transport_dto::bilibili::GetLiveDanmuInfoResp
    );

    fn watch_bilibili_live_danmaku<'life0, 'async_trait>(
        &'life0 self,
        request: crate::transport_dto::bilibili::WatchBilibiliLiveDanmakuReq,
    ) -> ::core::pin::Pin<
        Box<
            dyn ::core::future::Future<
                    Output = Result<
                        crate::bilibili::BilibiliLiveDanmakuStream,
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
            use crate::grpc::bilibili::bilibili_client::BilibiliClient;
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
            Ok(Box::pin(stream) as crate::bilibili::BilibiliLiveDanmakuStream)
        })
    }
}

#[must_use]
pub fn create_remote_emby_client(connection: RemoteProviderConnection) -> EmbyClientArc {
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
        crate::grpc::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        login,
        crate::transport_dto::emby::LoginReq,
        crate::transport_dto::emby::LoginResp
    );
    impl_remote_method!(
        crate::grpc::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        me,
        crate::transport_dto::emby::MeReq,
        crate::transport_dto::emby::MeResp
    );
    impl_remote_method!(
        crate::grpc::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        get_items,
        crate::transport_dto::emby::GetItemsReq,
        crate::transport_dto::emby::GetItemsResp
    );
    impl_remote_method!(
        crate::grpc::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        get_item,
        crate::transport_dto::emby::GetItemReq,
        crate::transport_dto::emby::Item
    );
    impl_remote_method!(
        crate::grpc::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        fs_list,
        crate::transport_dto::emby::FsListReq,
        crate::transport_dto::emby::FsListResp
    );
    impl_remote_method!(
        crate::grpc::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        get_system_info,
        crate::transport_dto::emby::SystemInfoReq,
        crate::transport_dto::emby::SystemInfoResp
    );
    impl_remote_method!(
        crate::grpc::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        logout,
        crate::transport_dto::emby::LogoutReq,
        crate::transport_dto::emby::Empty
    );
    impl_remote_method!(
        crate::grpc::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        playback_info,
        crate::transport_dto::emby::PlaybackInfoReq,
        crate::transport_dto::emby::PlaybackInfoResp
    );
    impl_remote_method!(
        crate::grpc::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        delete_active_encodings,
        crate::transport_dto::emby::DeleteActiveEncodingsReq,
        crate::transport_dto::emby::Empty
    );
    impl_remote_method!(
        crate::grpc::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        report_playback_start,
        crate::transport_dto::emby::ReportPlaybackStartReq,
        crate::transport_dto::emby::Empty
    );
    impl_remote_method!(
        crate::grpc::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        report_playback_stop,
        crate::transport_dto::emby::ReportPlaybackStopReq,
        crate::transport_dto::emby::Empty
    );
    impl_remote_method!(
        crate::grpc::emby::emby_client,
        EmbyClient,
        ProviderClientError,
        report_playback_progress,
        crate::transport_dto::emby::ReportPlaybackProgressReq,
        crate::transport_dto::emby::Empty
    );
}
