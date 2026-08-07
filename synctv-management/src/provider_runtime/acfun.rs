use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::acfun;

#[tonic::async_trait]
pub trait AcfunRuntime: Send + Sync {
    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: acfun::ResolveRequest,
    ) -> Result<acfun::ResolveResponse, ProviderError>;
}
