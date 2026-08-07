use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::cloudreve;

#[tonic::async_trait]
pub trait CloudreveRuntime: Send + Sync {
    async fn login(
        &self,
        caller_user_id: &UserId,
        request: cloudreve::LoginRequest,
    ) -> Result<cloudreve::LoginResponse, ProviderError>;
    async fn list(
        &self,
        caller_user_id: &UserId,
        request: cloudreve::ListRequest,
    ) -> Result<cloudreve::ListResponse, ProviderError>;
    async fn search(
        &self,
        caller_user_id: &UserId,
        request: cloudreve::SearchRequest,
    ) -> Result<cloudreve::SearchResponse, ProviderError>;
    async fn get_me(
        &self,
        caller_user_id: &UserId,
        request: cloudreve::GetMeRequest,
    ) -> Result<cloudreve::GetMeResponse, ProviderError>;
    async fn logout(
        &self,
        caller_user_id: &UserId,
        request: cloudreve::LogoutRequest,
    ) -> Result<cloudreve::LogoutResponse, ProviderError>;
    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        request: cloudreve::GetBindsRequest,
    ) -> Result<cloudreve::GetBindsResponse, ProviderError>;
}
