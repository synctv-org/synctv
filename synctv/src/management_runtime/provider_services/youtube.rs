use super::take_instance;
use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError};
use synctv_management::provider_runtime::YoutubeRuntime;
use synctv_proto::providers::youtube as youtube_proto;
pub(crate) struct ManagementYoutubeRuntime {
    inner: Arc<synctv_api::providers::YoutubeApiImpl>,
}

impl ManagementYoutubeRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::providers::YoutubeApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl YoutubeRuntime for ManagementYoutubeRuntime {
    async fn bind(
        &self,
        user: &UserId,
        mut request: youtube_proto::BindRequest,
    ) -> Result<youtube_proto::BindResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.bind(*user, request, instance.as_deref()).await
    }

    async fn get_binds(
        &self,
        user: &UserId,
        mut request: youtube_proto::GetBindsRequest,
    ) -> Result<youtube_proto::GetBindsResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.get_binds(*user, instance.as_deref()).await
    }

    async fn unbind(
        &self,
        user: &UserId,
        request: youtube_proto::UnbindRequest,
    ) -> Result<youtube_proto::UnbindResponse, ProviderError> {
        self.inner.unbind(*user, request).await
    }

    async fn resolve(
        &self,
        user: &UserId,
        mut request: youtube_proto::ResolveRequest,
    ) -> Result<youtube_proto::ResolveResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .resolve(*user, request, instance.as_deref())
            .await
    }
}
