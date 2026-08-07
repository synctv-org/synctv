use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::tiktok as tiktok_proto;

#[tonic::async_trait]
pub trait TikTokRuntime: Send + Sync {
    async fn bind(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::BindRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::BindResponse, ProviderError>;
    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::GetBindsResponse, ProviderError>;
    async fn unbind(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::UnbindRequest,
    ) -> Result<tiktok_proto::UnbindResponse, ProviderError>;
    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::ResolveRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::ResolveResponse, ProviderError>;
    async fn get_user(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::GetUserRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::GetUserResponse, ProviderError>;
    async fn list_user_posts(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::ListUserPostsRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::ListUserPostsResponse, ProviderError>;
}
