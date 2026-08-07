use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError};
use synctv_management::{
    provider_runtime::{
        BilibiliCheckQrQuery, BilibiliLoginQrCommand, BilibiliLoginSmsCommand,
        BilibiliLogoutCommand, BilibiliParseQuery, BilibiliRuntime, BilibiliSendSmsCommand,
        BilibiliStartSmsLoginCommand, BilibiliUserInfoQuery,
    },
    runtime_error::RuntimeError,
};
use synctv_proto::providers::bilibili as bilibili_proto;

use super::{super::map_runtime_error, take_instance};

pub(crate) struct ManagementBilibiliRuntime {
    inner: Arc<synctv_api::providers::BilibiliApiImpl>,
}

impl ManagementBilibiliRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::providers::BilibiliApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl BilibiliRuntime for ManagementBilibiliRuntime {
    async fn parse(
        &self,
        caller_user_id: &UserId,
        query: BilibiliParseQuery,
        instance_name: Option<&str>,
    ) -> Result<bilibili_proto::ParseResponse, ProviderError> {
        let req = bilibili_proto::ParseRequest {
            url: query.url,
            instance_name: String::new(),
        };
        self.inner
            .parse_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn login_qr(
        &self,
        _command: BilibiliLoginQrCommand,
        instance_name: Option<&str>,
    ) -> Result<bilibili_proto::QrCodeResponse, ProviderError> {
        let req = bilibili_proto::LoginQrRequest {
            instance_name: String::new(),
        };
        self.inner
            .login_qr_with_context(req, instance_name, None)
            .await
    }

    async fn check_qr(
        &self,
        caller_user_id: &UserId,
        query: BilibiliCheckQrQuery,
        instance_name: Option<&str>,
    ) -> Result<bilibili_proto::QrStatusResponse, ProviderError> {
        let req = bilibili_proto::CheckQrRequest {
            key: query.key,
            instance_name: String::new(),
        };
        self.inner
            .check_qr_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn start_sms_login(
        &self,
        _command: BilibiliStartSmsLoginCommand,
        instance_name: Option<&str>,
    ) -> Result<bilibili_proto::StartSmsLoginResponse, ProviderError> {
        let req = bilibili_proto::StartSmsLoginRequest {
            instance_name: String::new(),
        };
        self.inner
            .start_sms_login_with_context(req, instance_name, None)
            .await
    }

    async fn send_sms(
        &self,
        command: BilibiliSendSmsCommand,
    ) -> Result<bilibili_proto::SendSmsResponse, ProviderError> {
        let req = bilibili_proto::SendSmsRequest {
            session_token: command.session_token,
            phone: command.phone,
            validate: command.validate,
        };
        self.inner.send_sms_with_context(req, None, None).await
    }

    async fn login_sms(
        &self,
        caller_user_id: &UserId,
        command: BilibiliLoginSmsCommand,
    ) -> Result<bilibili_proto::LoginSmsResponse, ProviderError> {
        let req = bilibili_proto::LoginSmsRequest {
            session_token: command.session_token,
            code: command.code,
        };
        self.inner
            .login_sms_with_context(caller_user_id, req, None, None)
            .await
    }

    async fn get_user_info(
        &self,
        caller_user_id: &UserId,
        _query: BilibiliUserInfoQuery,
        instance_name: Option<&str>,
    ) -> Result<bilibili_proto::UserInfoResponse, ProviderError> {
        let req = bilibili_proto::UserInfoRequest {
            instance_name: String::new(),
        };
        self.inner
            .get_user_info_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn logout(
        &self,
        caller_user_id: &UserId,
        _command: BilibiliLogoutCommand,
    ) -> Result<bilibili_proto::LogoutResponse, ProviderError> {
        self.inner
            .logout(caller_user_id, bilibili_proto::LogoutRequest {})
            .await
    }

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<bilibili_proto::GetBindsResponse, RuntimeError> {
        self.inner
            .get_binds(caller_user_id, instance_name)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_live_areas(
        &self,
        mut request: bilibili_proto::ListLiveAreasRequest,
    ) -> Result<bilibili_proto::ListLiveAreasResponse, ProviderError> {
        let instance_name = take_instance(&mut request.instance_name);
        self.inner
            .list_live_areas_with_context(request, instance_name.as_deref(), None)
            .await
    }

    async fn list_favorite_folders(
        &self,
        caller_user_id: &UserId,
        mut request: bilibili_proto::ListFavoriteFoldersRequest,
    ) -> Result<bilibili_proto::ListFavoriteFoldersResponse, ProviderError> {
        let instance_name = take_instance(&mut request.instance_name);
        self.inner
            .list_favorite_folders_with_context(
                caller_user_id,
                request,
                instance_name.as_deref(),
                None,
            )
            .await
    }

    async fn list_followed_pgc(
        &self,
        caller_user_id: &UserId,
        mut request: bilibili_proto::ListFollowedPgcRequest,
    ) -> Result<bilibili_proto::ListFollowedPgcResponse, ProviderError> {
        let instance_name = take_instance(&mut request.instance_name);
        self.inner
            .list_followed_pgc_with_context(caller_user_id, request, instance_name.as_deref(), None)
            .await
    }

    async fn list_history(
        &self,
        caller_user_id: &UserId,
        mut request: bilibili_proto::ListHistoryRequest,
    ) -> Result<bilibili_proto::ListHistoryResponse, ProviderError> {
        let instance_name = take_instance(&mut request.instance_name);
        self.inner
            .list_history_with_context(caller_user_id, request, instance_name.as_deref(), None)
            .await
    }

    async fn list_pgc_timeline(
        &self,
        caller_user_id: &UserId,
        mut request: bilibili_proto::ListPgcTimelineRequest,
    ) -> Result<bilibili_proto::ListPgcTimelineResponse, ProviderError> {
        let instance_name = take_instance(&mut request.instance_name);
        self.inner
            .list_pgc_timeline_with_context(caller_user_id, request, instance_name.as_deref(), None)
            .await
    }

    async fn list_pgc_seasons(
        &self,
        caller_user_id: &UserId,
        mut request: bilibili_proto::ListPgcSeasonsRequest,
    ) -> Result<bilibili_proto::ListPgcSeasonsResponse, ProviderError> {
        let instance_name = take_instance(&mut request.instance_name);
        self.inner
            .list_pgc_seasons_with_context(caller_user_id, request, instance_name.as_deref(), None)
            .await
    }
}
