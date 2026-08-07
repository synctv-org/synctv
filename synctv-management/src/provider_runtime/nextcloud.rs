use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::nextcloud;

#[tonic::async_trait]
pub trait NextcloudRuntime: Send + Sync {
    async fn login(
        &self,
        caller_user_id: &UserId,
        request: nextcloud::LoginRequest,
    ) -> Result<nextcloud::LoginResponse, ProviderError>;
    async fn start_login_flow(
        &self,
        caller_user_id: &UserId,
        request: nextcloud::StartLoginFlowRequest,
    ) -> Result<nextcloud::StartLoginFlowResponse, ProviderError>;
    async fn poll_login_flow(
        &self,
        caller_user_id: &UserId,
        request: nextcloud::PollLoginFlowRequest,
    ) -> Result<nextcloud::LoginResponse, ProviderError>;
    async fn list(
        &self,
        caller_user_id: &UserId,
        request: nextcloud::ListRequest,
    ) -> Result<nextcloud::ListResponse, ProviderError>;
    async fn list_favorites(
        &self,
        caller_user_id: &UserId,
        request: nextcloud::ListFavoritesRequest,
    ) -> Result<nextcloud::ListResponse, ProviderError>;
    async fn logout(
        &self,
        caller_user_id: &UserId,
        request: nextcloud::LogoutRequest,
    ) -> Result<nextcloud::LogoutResponse, ProviderError>;
    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        request: nextcloud::GetBindsRequest,
    ) -> Result<nextcloud::GetBindsResponse, ProviderError>;
}
