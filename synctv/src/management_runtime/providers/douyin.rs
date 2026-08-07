use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError};
use synctv_management::provider_runtime::DouyinRuntime;
use synctv_proto::providers::douyin as douyin_proto;

pub(crate) struct ManagementDouyinRuntime {
    inner: Arc<synctv_api::providers::DouyinApiImpl>,
}

impl ManagementDouyinRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::providers::DouyinApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl DouyinRuntime for ManagementDouyinRuntime {
    async fn bind(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::BindRequest,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::BindResponse, ProviderError> {
        self.inner
            .bind(*caller_user_id, request, instance_name)
            .await
    }

    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::GetBindsResponse, ProviderError> {
        self.inner.get_binds(*caller_user_id, instance_name).await
    }

    async fn unbind(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::UnbindRequest,
    ) -> Result<douyin_proto::UnbindResponse, ProviderError> {
        self.inner.unbind(*caller_user_id, request).await
    }

    async fn resolve(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::ResolveRequest,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::ResolveResponse, ProviderError> {
        self.inner
            .resolve(*caller_user_id, request, instance_name)
            .await
    }

    async fn list_user_posts(
        &self,
        caller_user_id: &UserId,
        request: douyin_proto::ListUserPostsRequest,
        instance_name: Option<&str>,
    ) -> Result<douyin_proto::ListUserPostsResponse, ProviderError> {
        self.inner
            .list_user_posts(*caller_user_id, request, instance_name)
            .await
    }
}
