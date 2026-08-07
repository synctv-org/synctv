use super::take_instance;
use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError};
use synctv_management::provider_runtime::CloudreveRuntime;
use synctv_proto::providers::cloudreve as cloudreve_proto;
pub(crate) struct ManagementCloudreveRuntime {
    inner: Arc<synctv_api::providers::CloudreveApiImpl>,
}

impl ManagementCloudreveRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::providers::CloudreveApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl CloudreveRuntime for ManagementCloudreveRuntime {
    async fn login(
        &self,
        user: &UserId,
        mut request: cloudreve_proto::LoginRequest,
    ) -> Result<cloudreve_proto::LoginResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.login(*user, request, instance.as_deref()).await
    }

    async fn list(
        &self,
        user: &UserId,
        mut request: cloudreve_proto::ListRequest,
    ) -> Result<cloudreve_proto::ListResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.list(*user, request, instance.as_deref()).await
    }

    async fn search(
        &self,
        user: &UserId,
        mut request: cloudreve_proto::SearchRequest,
    ) -> Result<cloudreve_proto::SearchResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.search(*user, request, instance.as_deref()).await
    }

    async fn get_me(
        &self,
        user: &UserId,
        mut request: cloudreve_proto::GetMeRequest,
    ) -> Result<cloudreve_proto::GetMeResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.get_me(*user, request, instance.as_deref()).await
    }

    async fn logout(
        &self,
        user: &UserId,
        request: cloudreve_proto::LogoutRequest,
    ) -> Result<cloudreve_proto::LogoutResponse, ProviderError> {
        self.inner.logout(*user, request).await
    }

    async fn get_binds(
        &self,
        user: &UserId,
        mut request: cloudreve_proto::GetBindsRequest,
    ) -> Result<cloudreve_proto::GetBindsResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.get_binds(*user, instance.as_deref()).await
    }
}
