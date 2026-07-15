use std::sync::Arc;

use synctv_core::models::{
    ProviderInstanceListQuery, ProviderInstanceListSortBy, SortDirection, UserId,
};
use synctv_core::provider::ProviderError;
use synctv_management::provider_runtime::{
    AddProviderInstanceCommand, AlistListQuery, AlistLoginCommand, AlistLoginCredential,
    AlistRuntime, AlistSearchQuery, BilibiliCheckQrQuery, BilibiliLoginQrCommand,
    BilibiliLoginSmsCommand, BilibiliLogoutCommand, BilibiliParseQuery, BilibiliRuntime,
    BilibiliSendSmsCommand, BilibiliStartSmsLoginCommand, BilibiliUserInfoQuery, DouyinRuntime,
    EmbyListQuery, EmbyLoginCommand, EmbyLoginCredential, EmbyRuntime,
    ListAvailableProviderInstancesQuery, ListProviderBackendsQuery, ProviderCommonRuntime,
    ProviderCredentialServerQuery, ProviderInstanceNameCommand, TikTokRuntime, TwitchRuntime,
    UpdateProviderInstanceCommand,
};
use synctv_management::request_context::RequestContext;
use synctv_management::runtime_error::RuntimeError;
use synctv_proto::providers::{
    alist as alist_proto, bilibili as bilibili_proto, common as provider_common_proto,
    douyin as douyin_proto, emby as emby_proto, tiktok as tiktok_proto, twitch as twitch_proto,
};
use synctv_proto::source_config as source_config_proto;

use super::map_runtime_error;

pub(crate) struct ManagementAlistRuntime {
    inner: Arc<synctv_api::AlistApiImpl>,
}

impl ManagementAlistRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::AlistApiImpl>) -> Self {
        Self { inner }
    }
}

fn alist_login_command_to_proto(command: AlistLoginCommand) -> alist_proto::LoginRequest {
    alist_proto::LoginRequest {
        host: command.host,
        username: command.username,
        credential: command.credential.map(|credential| match credential {
            AlistLoginCredential::Password(password) => {
                alist_proto::login_request::Credential::Password(password)
            }
            AlistLoginCredential::HashedPassword(hashed_password) => {
                alist_proto::login_request::Credential::HashedPassword(hashed_password)
            }
        }),
        otp_code: command.otp_code,
        otp_secret: command.otp_secret,
        instance_name: String::new(),
    }
}

fn emby_login_command_to_proto(command: EmbyLoginCommand) -> emby_proto::LoginRequest {
    emby_proto::LoginRequest {
        host: command.host,
        username: command.username,
        credential: command.credential.map(|credential| match credential {
            EmbyLoginCredential::Password(password) => {
                emby_proto::login_request::Credential::Password(password)
            }
            EmbyLoginCredential::ApiKey(api_key) => {
                emby_proto::login_request::Credential::ApiKey(api_key)
            }
        }),
        instance_name: String::new(),
    }
}

fn api_request_context(ctx: &RequestContext) -> synctv_api::AdminRequestContext {
    // Management owns the public runtime contract; API context conversion
    // stays in this startup bridge while synctv-api has its own context type.
    synctv_api::AdminRequestContext {
        ip_address: ctx.ip_address.clone(),
        user_agent: ctx.user_agent.clone(),
    }
}

#[tonic::async_trait]
impl AlistRuntime for ManagementAlistRuntime {
    async fn login(
        &self,
        caller_user_id: &UserId,
        command: AlistLoginCommand,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::LoginResponse, ProviderError> {
        let req = alist_login_command_to_proto(command);
        self.inner
            .login_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn list(
        &self,
        caller_user_id: &UserId,
        query: AlistListQuery,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::ListResponse, ProviderError> {
        let req = alist_proto::ListRequest {
            server_id: query.server_id,
            path: query.path,
            password: query.password,
            page: query.page,
            per_page: query.per_page,
            refresh: query.refresh,
            instance_name: String::new(),
        };
        self.inner
            .list_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn search(
        &self,
        caller_user_id: &UserId,
        query: AlistSearchQuery,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::SearchResponse, ProviderError> {
        let req = alist_proto::SearchRequest {
            server_id: query.server_id,
            parent: query.parent,
            keywords: query.keywords,
            scope: query.scope,
            page: query.page,
            per_page: query.per_page,
            password: query.password,
            instance_name: String::new(),
        };
        self.inner
            .search_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn get_me(
        &self,
        caller_user_id: &UserId,
        query: ProviderCredentialServerQuery,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::GetMeResponse, ProviderError> {
        let req = alist_proto::GetMeRequest {
            server_id: query.server_id,
            instance_name: String::new(),
        };
        self.inner
            .get_me_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn logout(
        &self,
        caller_user_id: &UserId,
        command: ProviderCredentialServerQuery,
    ) -> Result<alist_proto::LogoutResponse, ProviderError> {
        let req = alist_proto::LogoutRequest {
            server_id: command.server_id,
            instance_name: String::new(),
        };
        self.inner.logout(caller_user_id, req).await
    }

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::GetBindsResponse, RuntimeError> {
        self.inner
            .get_binds(caller_user_id, instance_name)
            .await
            .map_err(|error| map_runtime_error(&error))
    }
}

pub(crate) struct ManagementEmbyRuntime {
    inner: Arc<synctv_api::EmbyApiImpl>,
}

impl ManagementEmbyRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::EmbyApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl EmbyRuntime for ManagementEmbyRuntime {
    async fn login(
        &self,
        caller_user_id: &UserId,
        command: EmbyLoginCommand,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::LoginResponse, ProviderError> {
        let req = emby_login_command_to_proto(command);
        self.inner
            .login_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn list(
        &self,
        caller_user_id: &UserId,
        query: EmbyListQuery,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::ListResponse, ProviderError> {
        let req = emby_proto::ListRequest {
            server_id: query.server_id,
            path: query.path,
            start_index: query.start_index,
            limit: query.limit,
            search_term: query.search_term,
            instance_name: String::new(),
        };
        self.inner
            .list_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn get_me(
        &self,
        caller_user_id: &UserId,
        query: ProviderCredentialServerQuery,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::GetMeResponse, ProviderError> {
        let req = emby_proto::GetMeRequest {
            server_id: query.server_id,
            instance_name: String::new(),
        };
        self.inner
            .get_me_with_context(caller_user_id, req, instance_name, None)
            .await
    }

    async fn logout(
        &self,
        caller_user_id: &UserId,
        command: ProviderCredentialServerQuery,
    ) -> Result<emby_proto::LogoutResponse, ProviderError> {
        let req = emby_proto::LogoutRequest {
            server_id: command.server_id,
            instance_name: String::new(),
        };
        self.inner.logout(caller_user_id, req).await
    }

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::GetBindsResponse, RuntimeError> {
        self.inner
            .get_binds(caller_user_id, instance_name)
            .await
            .map_err(|error| map_runtime_error(&error))
    }
}

pub(crate) struct ManagementDouyinRuntime {
    inner: Arc<synctv_api::DouyinApiImpl>,
}

impl ManagementDouyinRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::DouyinApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl DouyinRuntime for ManagementDouyinRuntime {
    async fn bind(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::BindRequest,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::BindResponse, ProviderError> {
        self.inner
            .bind(*caller_user_id, request, instance_name)
            .await
    }

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::GetBindsResponse, ProviderError> {
        self.inner.get_binds(*caller_user_id, instance_name).await
    }

    async fn unbind(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::UnbindRequest,
    ) -> Result<douyin_proto::UnbindResponse, ProviderError> {
        self.inner.unbind(*caller_user_id, request).await
    }

    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::ResolveRequest,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::ResolveResponse, ProviderError> {
        self.inner
            .resolve(*caller_user_id, request, instance_name)
            .await
    }

    async fn list_user_posts(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::ListUserPostsRequest,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::ListUserPostsResponse, ProviderError> {
        self.inner
            .list_user_posts(*caller_user_id, request, instance_name)
            .await
    }
}

pub(crate) struct ManagementTikTokRuntime {
    inner: Arc<synctv_api::TikTokApiImpl>,
}

impl ManagementTikTokRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::TikTokApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl TikTokRuntime for ManagementTikTokRuntime {
    async fn bind(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::BindRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::BindResponse, ProviderError> {
        self.inner
            .bind(*caller_user_id, request, instance_name)
            .await
    }

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::GetBindsResponse, ProviderError> {
        self.inner.get_binds(*caller_user_id, instance_name).await
    }

    async fn unbind(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::UnbindRequest,
    ) -> Result<tiktok_proto::UnbindResponse, ProviderError> {
        self.inner.unbind(*caller_user_id, request).await
    }

    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::ResolveRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::ResolveResponse, ProviderError> {
        self.inner
            .resolve(*caller_user_id, request, instance_name)
            .await
    }

    async fn get_user(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::GetUserRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::GetUserResponse, ProviderError> {
        self.inner
            .get_user(*caller_user_id, request, instance_name)
            .await
    }

    async fn list_user_posts(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::ListUserPostsRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::ListUserPostsResponse, ProviderError> {
        self.inner
            .list_user_posts(*caller_user_id, request, instance_name)
            .await
    }
}

pub(crate) struct ManagementTwitchRuntime {
    inner: Arc<synctv_api::TwitchApiImpl>,
}

impl ManagementTwitchRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::TwitchApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl TwitchRuntime for ManagementTwitchRuntime {
    async fn bind(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::BindRequest,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::BindResponse, ProviderError> {
        self.inner
            .bind(*caller_user_id, request, instance_name)
            .await
    }

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::GetBindsResponse, ProviderError> {
        self.inner.get_binds(*caller_user_id, instance_name).await
    }

    async fn unbind(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::UnbindRequest,
    ) -> Result<twitch_proto::UnbindResponse, ProviderError> {
        self.inner.unbind(*caller_user_id, request).await
    }

    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::ResolveRequest,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::ResolveResponse, ProviderError> {
        self.inner
            .resolve(*caller_user_id, request, instance_name)
            .await
    }

    async fn list_channel_items(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::ListChannelItemsRequest,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::ListChannelItemsResponse, ProviderError> {
        self.inner
            .list_channel_items(*caller_user_id, request, instance_name)
            .await
    }
}

pub(crate) struct ManagementBilibiliRuntime {
    inner: Arc<synctv_api::BilibiliApiImpl>,
}

impl ManagementBilibiliRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::BilibiliApiImpl>) -> Self {
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
        let req = bilibili_proto::LogoutRequest {
            instance_name: String::new(),
        };
        self.inner.logout(caller_user_id, req).await
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
}

pub(crate) struct ManagementProviderCommonRuntime {
    inner: Arc<synctv_api::ProviderCommonApiImpl>,
}

impl ManagementProviderCommonRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::ProviderCommonApiImpl>) -> Self {
        Self { inner }
    }
}

fn source_provider_to_proto(provider: Option<synctv_core::models::SourceProvider>) -> i32 {
    match provider {
        Some(synctv_core::models::SourceProvider::DirectUrl) => {
            source_config_proto::SourceProvider::DirectUrl as i32
        }
        Some(synctv_core::models::SourceProvider::Bilibili) => {
            source_config_proto::SourceProvider::Bilibili as i32
        }
        Some(synctv_core::models::SourceProvider::Alist) => {
            source_config_proto::SourceProvider::Alist as i32
        }
        Some(synctv_core::models::SourceProvider::Emby) => {
            source_config_proto::SourceProvider::Emby as i32
        }
        Some(synctv_core::models::SourceProvider::Rtmp) => {
            source_config_proto::SourceProvider::Rtmp as i32
        }
        Some(synctv_core::models::SourceProvider::LiveProxy) => {
            source_config_proto::SourceProvider::LiveProxy as i32
        }
        Some(synctv_core::models::SourceProvider::Cloudreve) => {
            source_config_proto::SourceProvider::Cloudreve as i32
        }
        Some(synctv_core::models::SourceProvider::Twitch) => {
            source_config_proto::SourceProvider::Twitch as i32
        }
        Some(synctv_core::models::SourceProvider::Huya) => {
            source_config_proto::SourceProvider::Huya as i32
        }
        Some(synctv_core::models::SourceProvider::Douyu) => {
            source_config_proto::SourceProvider::Douyu as i32
        }
        Some(synctv_core::models::SourceProvider::Douyin) => {
            source_config_proto::SourceProvider::Douyin as i32
        }
        Some(synctv_core::models::SourceProvider::TikTok) => {
            source_config_proto::SourceProvider::Tiktok as i32
        }
        Some(synctv_core::models::SourceProvider::AcFun) => {
            source_config_proto::SourceProvider::Acfun as i32
        }
        Some(synctv_core::models::SourceProvider::Cctv) => {
            source_config_proto::SourceProvider::Cctv as i32
        }
        Some(synctv_core::models::SourceProvider::Fnos) => {
            source_config_proto::SourceProvider::Fnos as i32
        }
        Some(synctv_core::models::SourceProvider::Qnap) => {
            source_config_proto::SourceProvider::Qnap as i32
        }
        Some(synctv_core::models::SourceProvider::Synology) => {
            source_config_proto::SourceProvider::Synology as i32
        }
        Some(synctv_core::models::SourceProvider::Nextcloud) => {
            source_config_proto::SourceProvider::Nextcloud as i32
        }
        Some(synctv_core::models::SourceProvider::Seafile) => {
            source_config_proto::SourceProvider::Seafile as i32
        }
        Some(synctv_core::models::SourceProvider::TrueNas) => {
            source_config_proto::SourceProvider::Truenas as i32
        }
        Some(synctv_core::models::SourceProvider::Youtube) => {
            source_config_proto::SourceProvider::Youtube as i32
        }
        None => source_config_proto::SourceProvider::Unspecified as i32,
    }
}

fn required_source_provider_to_proto(provider: synctv_core::models::SourceProvider) -> i32 {
    source_provider_to_proto(Some(provider))
}

fn provider_instance_sort_by_to_proto(sort_by: ProviderInstanceListSortBy) -> i32 {
    match sort_by {
        ProviderInstanceListSortBy::Name => {
            provider_common_proto::ProviderInstanceListSortBy::Name as i32
        }
        ProviderInstanceListSortBy::Endpoint => {
            provider_common_proto::ProviderInstanceListSortBy::Endpoint as i32
        }
        ProviderInstanceListSortBy::UpdatedAt => {
            provider_common_proto::ProviderInstanceListSortBy::UpdatedAt as i32
        }
        ProviderInstanceListSortBy::CreatedAt => {
            provider_common_proto::ProviderInstanceListSortBy::CreatedAt as i32
        }
    }
}

fn provider_sort_direction_to_proto(sort_direction: SortDirection) -> i32 {
    match sort_direction {
        SortDirection::Asc => provider_common_proto::SortDirection::Asc as i32,
        SortDirection::Desc => provider_common_proto::SortDirection::Desc as i32,
    }
}

#[tonic::async_trait]
impl ProviderCommonRuntime for ManagementProviderCommonRuntime {
    async fn list_available_provider_instances(
        &self,
        query: ListAvailableProviderInstancesQuery,
    ) -> Result<provider_common_proto::ProviderInstancesResponse, RuntimeError> {
        let req = provider_common_proto::ListAvailableProviderInstancesRequest {
            provider_type: source_provider_to_proto(query.provider_type),
        };
        self.inner
            .list_available_provider_instances(req)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_provider_backends(
        &self,
        query: ListProviderBackendsQuery,
    ) -> Result<provider_common_proto::ProviderBackendsResponse, RuntimeError> {
        let req = provider_common_proto::ListProviderBackendsRequest {
            provider_type: required_source_provider_to_proto(query.provider_type),
        };
        self.inner
            .list_provider_backends(req)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_provider_instances(
        &self,
        query: ProviderInstanceListQuery,
    ) -> Result<provider_common_proto::ListProviderInstancesResponse, RuntimeError> {
        let req = provider_common_proto::ListProviderInstancesRequest {
            page: i32::try_from(query.pagination.page).unwrap_or(i32::MAX),
            page_size: i32::try_from(query.pagination.page_size).unwrap_or(i32::MAX),
            provider_type: source_provider_to_proto(query.provider_type),
            search: query.search.unwrap_or_default(),
            enabled: query.enabled,
            tls: query.tls,
            sort_by: provider_instance_sort_by_to_proto(query.sort_by),
            sort_direction: provider_sort_direction_to_proto(query.sort_direction),
        };
        self.inner
            .list_provider_instances(req)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn add_provider_instance(
        &self,
        command: AddProviderInstanceCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::AddProviderInstanceResponse, RuntimeError> {
        let req = provider_common_proto::AddProviderInstanceRequest {
            name: command.name,
            endpoint: command.endpoint,
            comment: command.comment,
            timeout_seconds: command.timeout_seconds,
            tls: command.tls,
            insecure_tls: command.insecure_tls,
            providers: command.providers,
            jwt_secret: command.jwt_secret,
            custom_ca: command.custom_ca,
        };
        self.inner
            .add_provider_instance(req, admin_user_id, &api_request_context(ctx), None)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn update_provider_instance(
        &self,
        command: UpdateProviderInstanceCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::UpdateProviderInstanceResponse, RuntimeError> {
        let req = provider_common_proto::UpdateProviderInstanceRequest {
            name: command.name,
            endpoint: command.endpoint,
            comment: command.comment,
            timeout_seconds: command.timeout_seconds,
            tls: command.tls,
            insecure_tls: command.insecure_tls,
            providers: command.providers,
            jwt_secret: command.jwt_secret,
            custom_ca: command.custom_ca,
            clear_comment: command.clear_comment,
            clear_jwt_secret: command.clear_jwt_secret,
            clear_custom_ca: command.clear_custom_ca,
        };
        self.inner
            .update_provider_instance(req, admin_user_id, &api_request_context(ctx), None)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn delete_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::DeleteProviderInstanceResponse, RuntimeError> {
        let req = provider_common_proto::DeleteProviderInstanceRequest { name: command.name };
        self.inner
            .delete_provider_instance(req, admin_user_id, &api_request_context(ctx))
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn reconnect_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::ReconnectProviderInstanceResponse, RuntimeError> {
        let req = provider_common_proto::ReconnectProviderInstanceRequest { name: command.name };
        self.inner
            .reconnect_provider_instance(req, admin_user_id, &api_request_context(ctx), None)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn enable_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
    ) -> Result<provider_common_proto::EnableProviderInstanceResponse, RuntimeError> {
        let req = provider_common_proto::EnableProviderInstanceRequest { name: command.name };
        self.inner
            .enable_provider_instance(req, None)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn disable_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
    ) -> Result<provider_common_proto::DisableProviderInstanceResponse, RuntimeError> {
        let req = provider_common_proto::DisableProviderInstanceRequest { name: command.name };
        self.inner
            .disable_provider_instance(req, None)
            .await
            .map_err(|error| map_runtime_error(&error))
    }
}
