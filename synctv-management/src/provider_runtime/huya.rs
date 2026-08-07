use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::huya;

#[tonic::async_trait]
pub trait HuyaRuntime: Send + Sync {
    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: huya::ResolveRequest,
    ) -> Result<huya::ResolveResponse, ProviderError>;
}
