use crate::request_context::RequestContext;
use crate::runtime_error::RuntimeError;
use synctv_core::models::UserId;
use synctv_core::models::{ProviderInstanceListQuery, SourceProvider};
use synctv_core::provider::ProviderError;
use synctv_proto::providers::{
    alist as alist_proto, bilibili as bilibili_proto, common as provider_common_proto,
    douyin as douyin_proto, emby as emby_proto, tiktok as tiktok_proto, twitch as twitch_proto,
};

#[derive(Debug, Clone)]
pub struct ListAvailableProviderInstancesQuery {
    pub provider_type: Option<SourceProvider>,
}

#[derive(Debug, Clone)]
pub struct ListProviderBackendsQuery {
    pub provider_type: SourceProvider,
}

#[derive(Debug, Clone)]
pub struct AddProviderInstanceCommand {
    pub name: String,
    pub endpoint: String,
    pub comment: String,
    pub timeout_seconds: u32,
    pub tls: bool,
    pub insecure_tls: bool,
    pub providers: Vec<i32>,
    pub jwt_secret: Option<String>,
    pub custom_ca: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateProviderInstanceCommand {
    pub name: String,
    pub endpoint: Option<String>,
    pub comment: Option<String>,
    pub timeout_seconds: Option<u32>,
    pub tls: Option<bool>,
    pub insecure_tls: Option<bool>,
    pub providers: Vec<i32>,
    pub jwt_secret: Option<String>,
    pub custom_ca: Option<String>,
    pub clear_comment: Option<bool>,
    pub clear_jwt_secret: Option<bool>,
    pub clear_custom_ca: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ProviderInstanceNameCommand {
    pub name: String,
}

#[derive(Debug, Clone)]
pub enum AlistLoginCredential {
    Password(String),
    HashedPassword(String),
}

#[derive(Debug, Clone)]
pub struct AlistLoginCommand {
    pub host: String,
    pub username: String,
    pub credential: Option<AlistLoginCredential>,
    pub otp_code: String,
    pub otp_secret: String,
}

#[derive(Debug, Clone)]
pub struct AlistListQuery {
    pub server_id: String,
    pub path: String,
    pub password: String,
    pub page: u64,
    pub per_page: u64,
    pub refresh: bool,
}

#[derive(Debug, Clone)]
pub struct AlistSearchQuery {
    pub server_id: String,
    pub parent: String,
    pub keywords: String,
    pub scope: u64,
    pub page: u64,
    pub per_page: u64,
    pub password: String,
}

#[derive(Debug, Clone)]
pub struct ProviderCredentialServerQuery {
    pub server_id: String,
}

#[derive(Debug, Clone)]
pub enum EmbyLoginCredential {
    Password(String),
    ApiKey(String),
}

#[derive(Debug, Clone)]
pub struct EmbyLoginCommand {
    pub host: String,
    pub username: String,
    pub credential: Option<EmbyLoginCredential>,
}

#[derive(Debug, Clone)]
pub struct EmbyListQuery {
    pub server_id: String,
    pub path: String,
    pub start_index: u64,
    pub limit: u64,
    pub search_term: String,
}

#[derive(Debug, Clone)]
pub struct BilibiliParseQuery {
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct BilibiliLoginQrCommand;

#[derive(Debug, Clone)]
pub struct BilibiliCheckQrQuery {
    pub key: String,
}

#[derive(Debug, Clone)]
pub struct BilibiliStartSmsLoginCommand;

#[derive(Debug, Clone)]
pub struct BilibiliSendSmsCommand {
    pub session_token: String,
    pub phone: String,
    pub validate: String,
}

#[derive(Debug, Clone)]
pub struct BilibiliLoginSmsCommand {
    pub session_token: String,
    pub code: String,
}

#[derive(Debug, Clone)]
pub struct BilibiliUserInfoQuery;

#[derive(Debug, Clone)]
pub struct BilibiliLogoutCommand;

#[tonic::async_trait]
pub trait AlistRuntime: Send + Sync {
    async fn login(
        &self,
        caller_user_id: &UserId,
        command: AlistLoginCommand,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::LoginResponse, ProviderError>;

    async fn list(
        &self,
        caller_user_id: &UserId,
        query: AlistListQuery,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::ListResponse, ProviderError>;

    async fn search(
        &self,
        caller_user_id: &UserId,
        query: AlistSearchQuery,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::SearchResponse, ProviderError>;

    async fn get_me(
        &self,
        caller_user_id: &UserId,
        query: ProviderCredentialServerQuery,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::GetMeResponse, ProviderError>;

    async fn logout(
        &self,
        caller_user_id: &UserId,
        command: ProviderCredentialServerQuery,
    ) -> Result<alist_proto::LogoutResponse, ProviderError>;

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::GetBindsResponse, RuntimeError>;
}

#[tonic::async_trait]
pub trait EmbyRuntime: Send + Sync {
    async fn login(
        &self,
        caller_user_id: &UserId,
        command: EmbyLoginCommand,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::LoginResponse, ProviderError>;

    async fn list(
        &self,
        caller_user_id: &UserId,
        query: EmbyListQuery,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::ListResponse, ProviderError>;

    async fn get_me(
        &self,
        caller_user_id: &UserId,
        query: ProviderCredentialServerQuery,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::GetMeResponse, ProviderError>;

    async fn logout(
        &self,
        caller_user_id: &UserId,
        command: ProviderCredentialServerQuery,
    ) -> Result<emby_proto::LogoutResponse, ProviderError>;

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::GetBindsResponse, RuntimeError>;
}

#[tonic::async_trait]
pub trait DouyinRuntime: Send + Sync {
    async fn bind(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::BindRequest,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::BindResponse, ProviderError>;

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::GetBindsResponse, ProviderError>;

    async fn unbind(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::UnbindRequest,
    ) -> Result<douyin_proto::UnbindResponse, ProviderError>;

    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::ResolveRequest,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::ResolveResponse, ProviderError>;

    async fn list_user_posts(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::ListUserPostsRequest,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::ListUserPostsResponse, ProviderError>;
}

#[tonic::async_trait]
pub trait TikTokRuntime: Send + Sync {
    async fn bind(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::BindRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::BindResponse, ProviderError>;

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::GetBindsResponse, ProviderError>;

    async fn unbind(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::UnbindRequest,
    ) -> Result<tiktok_proto::UnbindResponse, ProviderError>;

    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::ResolveRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::ResolveResponse, ProviderError>;

    async fn get_user(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::GetUserRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::GetUserResponse, ProviderError>;

    async fn list_user_posts(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::ListUserPostsRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::ListUserPostsResponse, ProviderError>;
}

#[tonic::async_trait]
pub trait TwitchRuntime: Send + Sync {
    async fn bind(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::BindRequest,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::BindResponse, ProviderError>;

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::GetBindsResponse, ProviderError>;

    async fn unbind(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::UnbindRequest,
    ) -> Result<twitch_proto::UnbindResponse, ProviderError>;

    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::ResolveRequest,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::ResolveResponse, ProviderError>;

    async fn list_channel_items(
        &self,
        caller_user_id: &UserId,
        request: twitch_proto::ListChannelItemsRequest,
        instance_name: Option<&str>,
    ) -> Result<twitch_proto::ListChannelItemsResponse, ProviderError>;
}

#[tonic::async_trait]
pub trait BilibiliRuntime: Send + Sync {
    async fn parse(
        &self,
        caller_user_id: &UserId,
        query: BilibiliParseQuery,
        instance_name: Option<&str>,
    ) -> Result<bilibili_proto::ParseResponse, ProviderError>;

    async fn login_qr(
        &self,
        command: BilibiliLoginQrCommand,
        instance_name: Option<&str>,
    ) -> Result<bilibili_proto::QrCodeResponse, ProviderError>;

    async fn check_qr(
        &self,
        caller_user_id: &UserId,
        query: BilibiliCheckQrQuery,
        instance_name: Option<&str>,
    ) -> Result<bilibili_proto::QrStatusResponse, ProviderError>;

    async fn start_sms_login(
        &self,
        command: BilibiliStartSmsLoginCommand,
        instance_name: Option<&str>,
    ) -> Result<bilibili_proto::StartSmsLoginResponse, ProviderError>;

    async fn send_sms(
        &self,
        command: BilibiliSendSmsCommand,
    ) -> Result<bilibili_proto::SendSmsResponse, ProviderError>;

    async fn login_sms(
        &self,
        caller_user_id: &UserId,
        command: BilibiliLoginSmsCommand,
    ) -> Result<bilibili_proto::LoginSmsResponse, ProviderError>;

    async fn get_user_info(
        &self,
        caller_user_id: &UserId,
        query: BilibiliUserInfoQuery,
        instance_name: Option<&str>,
    ) -> Result<bilibili_proto::UserInfoResponse, ProviderError>;

    async fn logout(
        &self,
        caller_user_id: &UserId,
        command: BilibiliLogoutCommand,
    ) -> Result<bilibili_proto::LogoutResponse, ProviderError>;

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<bilibili_proto::GetBindsResponse, RuntimeError>;
}

#[tonic::async_trait]
pub trait ProviderCommonRuntime: Send + Sync {
    async fn list_available_provider_instances(
        &self,
        query: ListAvailableProviderInstancesQuery,
    ) -> Result<provider_common_proto::ProviderInstancesResponse, RuntimeError>;

    async fn list_provider_backends(
        &self,
        query: ListProviderBackendsQuery,
    ) -> Result<provider_common_proto::ProviderBackendsResponse, RuntimeError>;

    async fn list_provider_instances(
        &self,
        query: ProviderInstanceListQuery,
    ) -> Result<provider_common_proto::ListProviderInstancesResponse, RuntimeError>;

    async fn add_provider_instance(
        &self,
        command: AddProviderInstanceCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::AddProviderInstanceResponse, RuntimeError>;

    async fn update_provider_instance(
        &self,
        command: UpdateProviderInstanceCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::UpdateProviderInstanceResponse, RuntimeError>;

    async fn delete_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::DeleteProviderInstanceResponse, RuntimeError>;

    async fn reconnect_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::ReconnectProviderInstanceResponse, RuntimeError>;

    async fn enable_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
    ) -> Result<provider_common_proto::EnableProviderInstanceResponse, RuntimeError>;

    async fn disable_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
    ) -> Result<provider_common_proto::DisableProviderInstanceResponse, RuntimeError>;
}
