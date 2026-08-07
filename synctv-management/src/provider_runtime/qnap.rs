use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::qnap;

#[tonic::async_trait]
pub trait QnapRuntime: Send + Sync {
    async fn login(
        &self,
        caller_user_id: &UserId,
        request: qnap::LoginRequest,
    ) -> Result<qnap::LoginResponse, ProviderError>;
    async fn list(
        &self,
        caller_user_id: &UserId,
        request: qnap::ListRequest,
    ) -> Result<qnap::ListResponse, ProviderError>;
    async fn get_capabilities(
        &self,
        caller_user_id: &UserId,
        request: qnap::GetCapabilitiesRequest,
    ) -> Result<qnap::GetCapabilitiesResponse, ProviderError>;
    async fn logout(
        &self,
        caller_user_id: &UserId,
        request: qnap::LogoutRequest,
    ) -> Result<qnap::LogoutResponse, ProviderError>;
    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        request: qnap::GetBindsRequest,
    ) -> Result<qnap::GetBindsResponse, ProviderError>;
}
