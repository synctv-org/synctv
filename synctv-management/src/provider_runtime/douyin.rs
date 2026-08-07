use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::douyin as douyin_proto;

#[tonic::async_trait]
pub trait DouyinRuntime: Send + Sync {
    async fn bind(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::BindRequest,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::BindResponse, ProviderError>;
    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::GetBindsResponse, ProviderError>;
    async fn unbind(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::UnbindRequest,
    ) -> Result<douyin_proto::UnbindResponse, ProviderError>;
    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::ResolveRequest,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::ResolveResponse, ProviderError>;
    async fn list_user_posts(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::ListUserPostsRequest,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::ListUserPostsResponse, ProviderError>;
}
