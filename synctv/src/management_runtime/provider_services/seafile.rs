use super::take_instance;
use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError};
use synctv_management::provider_runtime::SeafileRuntime;
use synctv_proto::providers::seafile as seafile_proto;
pub(crate) struct ManagementSeafileRuntime {
    inner: Arc<synctv_api::providers::SeafileApiImpl>,
}

impl ManagementSeafileRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::providers::SeafileApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl SeafileRuntime for ManagementSeafileRuntime {
    async fn login(
        &self,
        user: &UserId,
        mut request: seafile_proto::LoginRequest,
    ) -> Result<seafile_proto::LoginResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.login(*user, request, instance.as_deref()).await
    }

    async fn unlock_library(
        &self,
        user: &UserId,
        mut request: seafile_proto::UnlockLibraryRequest,
    ) -> Result<seafile_proto::UnlockLibraryResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .unlock_library(*user, request, instance.as_deref())
            .await
    }

    async fn list_repositories(
        &self,
        user: &UserId,
        mut request: seafile_proto::ListRepositoriesRequest,
    ) -> Result<seafile_proto::ListResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .list_repositories(*user, request, instance.as_deref())
            .await
    }

    async fn list(
        &self,
        user: &UserId,
        mut request: seafile_proto::ListRequest,
    ) -> Result<seafile_proto::ListResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.list(*user, request, instance.as_deref()).await
    }

    async fn list_starred(
        &self,
        user: &UserId,
        mut request: seafile_proto::ListStarredRequest,
    ) -> Result<seafile_proto::ListResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .list_starred(*user, request, instance.as_deref())
            .await
    }

    async fn logout(
        &self,
        user: &UserId,
        request: seafile_proto::LogoutRequest,
    ) -> Result<seafile_proto::LogoutResponse, ProviderError> {
        self.inner.logout(*user, request).await
    }

    async fn get_binds(
        &self,
        user: &UserId,
        mut request: seafile_proto::GetBindsRequest,
    ) -> Result<seafile_proto::GetBindsResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.binds(*user, instance.as_deref()).await
    }
}
