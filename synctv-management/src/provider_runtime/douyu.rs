use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::douyu;

#[tonic::async_trait]
pub trait DouyuRuntime: Send + Sync {
    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: douyu::ResolveRequest,
    ) -> Result<douyu::ResolveResponse, ProviderError>;
}
