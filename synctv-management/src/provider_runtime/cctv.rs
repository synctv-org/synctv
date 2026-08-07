use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::cctv;

#[tonic::async_trait]
pub trait CctvRuntime: Send + Sync {
    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: cctv::ResolveRequest,
    ) -> Result<cctv::ResolveResponse, ProviderError>;
}
