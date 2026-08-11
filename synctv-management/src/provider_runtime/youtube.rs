use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::youtube;

#[tonic::async_trait]
pub trait YoutubeRuntime: Send + Sync {
    async fn bind(
        &self,
        caller_user_id: &UserId,
        request: youtube::BindRequest,
    ) -> Result<youtube::BindResponse, ProviderError>;
    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        request: youtube::GetBindsRequest,
    ) -> Result<youtube::GetBindsResponse, ProviderError>;
    async fn unbind(
        &self,
        caller_user_id: &UserId,
        request: youtube::UnbindRequest,
    ) -> Result<youtube::UnbindResponse, ProviderError>;
    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: youtube::ResolveRequest,
    ) -> Result<youtube::ResolveResponse, ProviderError>;
    async fn list(
        &self,
        caller_user_id: &UserId,
        request: youtube::ListRequest,
    ) -> Result<youtube::ListResponse, ProviderError>;
}
