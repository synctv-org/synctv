use super::take_instance;
use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError};
use synctv_management::provider_runtime::FnosRuntime;
use synctv_proto::providers::fnos as fnos_proto;
pub(crate) struct ManagementFnosRuntime {
    inner: Arc<synctv_api::providers::FnosApiImpl>,
}

impl ManagementFnosRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::providers::FnosApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl FnosRuntime for ManagementFnosRuntime {
    async fn login(
        &self,
        user: &UserId,
        mut request: fnos_proto::LoginRequest,
    ) -> Result<fnos_proto::LoginResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.login(*user, request, instance.as_deref()).await
    }

    async fn list(
        &self,
        user: &UserId,
        mut request: fnos_proto::ListRequest,
    ) -> Result<fnos_proto::ListResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.list(*user, request, instance.as_deref()).await
    }

    async fn list_media_libraries(
        &self,
        user: &UserId,
        mut request: fnos_proto::ListMediaLibrariesRequest,
    ) -> Result<fnos_proto::ListMediaLibrariesResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .list_media_libraries(*user, request, instance.as_deref())
            .await
    }

    async fn list_media_items(
        &self,
        user: &UserId,
        mut request: fnos_proto::ListMediaItemsRequest,
    ) -> Result<fnos_proto::ListMediaItemsResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .list_media_items(*user, request, instance.as_deref())
            .await
    }

    async fn set_favorite(
        &self,
        user: &UserId,
        mut request: fnos_proto::SetFavoriteRequest,
    ) -> Result<fnos_proto::SetFavoriteResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .set_favorite(*user, request, instance.as_deref())
            .await
    }

    async fn set_watched(
        &self,
        user: &UserId,
        mut request: fnos_proto::SetWatchedRequest,
    ) -> Result<fnos_proto::SetWatchedResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .set_watched(*user, request, instance.as_deref())
            .await
    }

    async fn get_server_info(
        &self,
        user: &UserId,
        mut request: fnos_proto::GetServerInfoRequest,
    ) -> Result<fnos_proto::GetServerInfoResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .get_server_info(*user, request, instance.as_deref())
            .await
    }

    async fn logout(
        &self,
        user: &UserId,
        request: fnos_proto::LogoutRequest,
    ) -> Result<fnos_proto::LogoutResponse, ProviderError> {
        self.inner.logout(*user, request).await
    }

    async fn get_binds(
        &self,
        user: &UserId,
        mut request: fnos_proto::GetBindsRequest,
    ) -> Result<fnos_proto::GetBindsResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.get_binds(*user, instance.as_deref()).await
    }
}
