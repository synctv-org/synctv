use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError};
use synctv_management::provider_runtime::TikTokRuntime;
use synctv_proto::providers::tiktok as tiktok_proto;

pub(crate) struct ManagementTikTokRuntime {
    inner: Arc<synctv_api::providers::TikTokApiImpl>,
}

impl ManagementTikTokRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::providers::TikTokApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl TikTokRuntime for ManagementTikTokRuntime {
    async fn bind(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::BindRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::BindResponse, ProviderError> {
        self.inner
            .bind(*caller_user_id, request, instance_name)
            .await
    }

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::GetBindsResponse, ProviderError> {
        self.inner.get_binds(*caller_user_id, instance_name).await
    }

    async fn unbind(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::UnbindRequest,
    ) -> Result<tiktok_proto::UnbindResponse, ProviderError> {
        self.inner.unbind(*caller_user_id, request).await
    }

    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::ResolveRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::ResolveResponse, ProviderError> {
        self.inner
            .resolve(*caller_user_id, request, instance_name)
            .await
    }

    async fn get_user(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::GetUserRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::GetUserResponse, ProviderError> {
        self.inner
            .get_user(*caller_user_id, request, instance_name)
            .await
    }

    async fn list_user_posts(
        &self,
        caller_user_id: &UserId,
        request: tiktok_proto::ListUserPostsRequest,
        instance_name: Option<&str>,
    ) -> Result<tiktok_proto::ListUserPostsResponse, ProviderError> {
        self.inner
            .list_user_posts(*caller_user_id, request, instance_name)
            .await
    }
}
