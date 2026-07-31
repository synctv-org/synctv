use crate::runtime_error::RuntimeError;
use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::bilibili as bilibili_proto;

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
    async fn list_live_areas(
        &self,
        request: bilibili_proto::ListLiveAreasRequest,
    ) -> Result<bilibili_proto::ListLiveAreasResponse, ProviderError>;
    async fn list_favorite_folders(
        &self,
        caller_user_id: &UserId,
        request: bilibili_proto::ListFavoriteFoldersRequest,
    ) -> Result<bilibili_proto::ListFavoriteFoldersResponse, ProviderError>;
    async fn list_followed_pgc(
        &self,
        caller_user_id: &UserId,
        request: bilibili_proto::ListFollowedPgcRequest,
    ) -> Result<bilibili_proto::ListFollowedPgcResponse, ProviderError>;
    async fn list_history(
        &self,
        caller_user_id: &UserId,
        request: bilibili_proto::ListHistoryRequest,
    ) -> Result<bilibili_proto::ListHistoryResponse, ProviderError>;
    async fn list_pgc_timeline(
        &self,
        caller_user_id: &UserId,
        request: bilibili_proto::ListPgcTimelineRequest,
    ) -> Result<bilibili_proto::ListPgcTimelineResponse, ProviderError>;
    async fn list_pgc_seasons(
        &self,
        caller_user_id: &UserId,
        request: bilibili_proto::ListPgcSeasonsRequest,
    ) -> Result<bilibili_proto::ListPgcSeasonsResponse, ProviderError>;
}
