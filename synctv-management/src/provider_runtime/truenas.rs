use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::truenas;

#[tonic::async_trait]
pub trait TruenasRuntime: Send + Sync {
    async fn login(
        &self,
        caller_user_id: &UserId,
        request: truenas::LoginRequest,
    ) -> Result<truenas::LoginResponse, ProviderError>;
    async fn list(
        &self,
        caller_user_id: &UserId,
        request: truenas::ListRequest,
    ) -> Result<truenas::ListResponse, ProviderError>;
    async fn logout(
        &self,
        caller_user_id: &UserId,
        request: truenas::LogoutRequest,
    ) -> Result<truenas::LogoutResponse, ProviderError>;
    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        request: truenas::GetBindsRequest,
    ) -> Result<truenas::GetBindsResponse, ProviderError>;
}
