use super::take_instance;
use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError};
use synctv_management::provider_runtime::SynologyRuntime;
use synctv_proto::providers::synology as synology_proto;
pub(crate) struct ManagementSynologyRuntime {
    inner: Arc<synctv_api::providers::SynologyApiImpl>,
}

impl ManagementSynologyRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::providers::SynologyApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl SynologyRuntime for ManagementSynologyRuntime {
    async fn login(
        &self,
        user: &UserId,
        mut request: synology_proto::LoginRequest,
    ) -> Result<synology_proto::LoginResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.login(*user, request, instance.as_deref()).await
    }

    async fn list_files(
        &self,
        user: &UserId,
        mut request: synology_proto::ListFilesRequest,
    ) -> Result<synology_proto::ListFilesResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .list_files(*user, request, instance.as_deref())
            .await
    }

    async fn list_libraries(
        &self,
        user: &UserId,
        mut request: synology_proto::ListLibrariesRequest,
    ) -> Result<synology_proto::ListLibrariesResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .list_libraries(*user, request, instance.as_deref())
            .await
    }

    async fn list_movies(
        &self,
        user: &UserId,
        mut request: synology_proto::ListMoviesRequest,
    ) -> Result<synology_proto::ListVideoItemsResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .list_movies(*user, request, instance.as_deref())
            .await
    }

    async fn list_tv_shows(
        &self,
        user: &UserId,
        mut request: synology_proto::ListTvShowsRequest,
    ) -> Result<synology_proto::ListVideoItemsResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .list_tv_shows(*user, request, instance.as_deref())
            .await
    }

    async fn list_episodes(
        &self,
        user: &UserId,
        mut request: synology_proto::ListEpisodesRequest,
    ) -> Result<synology_proto::ListVideoItemsResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .list_episodes(*user, request, instance.as_deref())
            .await
    }

    async fn list_home_videos(
        &self,
        user: &UserId,
        mut request: synology_proto::ListHomeVideosRequest,
    ) -> Result<synology_proto::ListVideoItemsResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .list_home_videos(*user, request, instance.as_deref())
            .await
    }

    async fn list_tv_recordings(
        &self,
        user: &UserId,
        mut request: synology_proto::ListTvRecordingsRequest,
    ) -> Result<synology_proto::ListVideoItemsResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .list_tv_recordings(*user, request, instance.as_deref())
            .await
    }

    async fn logout(
        &self,
        user: &UserId,
        request: synology_proto::LogoutRequest,
    ) -> Result<synology_proto::LogoutResponse, ProviderError> {
        self.inner.logout(*user, request).await
    }

    async fn get_binds(
        &self,
        user: &UserId,
        mut request: synology_proto::GetBindsRequest,
    ) -> Result<synology_proto::GetBindsResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.binds(*user, instance.as_deref()).await
    }
}
