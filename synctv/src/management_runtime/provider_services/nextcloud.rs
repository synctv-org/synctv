use super::take_instance;
use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError};
use synctv_management::provider_runtime::NextcloudRuntime;
use synctv_proto::providers::nextcloud as nextcloud_proto;
pub(crate) struct ManagementNextcloudRuntime {
    inner: Arc<synctv_api::providers::NextcloudApiImpl>,
}

impl ManagementNextcloudRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::providers::NextcloudApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl NextcloudRuntime for ManagementNextcloudRuntime {
    async fn login(
        &self,
        user: &UserId,
        mut request: nextcloud_proto::LoginRequest,
    ) -> Result<nextcloud_proto::LoginResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.login(*user, request, instance.as_deref()).await
    }

    async fn start_login_flow(
        &self,
        _: &UserId,
        request: nextcloud_proto::StartLoginFlowRequest,
    ) -> Result<nextcloud_proto::StartLoginFlowResponse, ProviderError> {
        self.inner.start_login_flow(request).await
    }

    async fn poll_login_flow(
        &self,
        user: &UserId,
        mut request: nextcloud_proto::PollLoginFlowRequest,
    ) -> Result<nextcloud_proto::LoginResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .poll_login_flow(*user, request, instance.as_deref())
            .await
    }

    async fn list(
        &self,
        user: &UserId,
        mut request: nextcloud_proto::ListRequest,
    ) -> Result<nextcloud_proto::ListResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.list(*user, request, instance.as_deref()).await
    }

    async fn list_favorites(
        &self,
        user: &UserId,
        mut request: nextcloud_proto::ListFavoritesRequest,
    ) -> Result<nextcloud_proto::ListResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .list_favorites(*user, request, instance.as_deref())
            .await
    }

    async fn logout(
        &self,
        user: &UserId,
        request: nextcloud_proto::LogoutRequest,
    ) -> Result<nextcloud_proto::LogoutResponse, ProviderError> {
        self.inner.logout(*user, request).await
    }

    async fn get_binds(
        &self,
        user: &UserId,
        mut request: nextcloud_proto::GetBindsRequest,
    ) -> Result<nextcloud_proto::GetBindsResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.binds(*user, instance.as_deref()).await
    }
}
