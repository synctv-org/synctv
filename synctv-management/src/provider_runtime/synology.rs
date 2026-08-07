use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::synology;

#[tonic::async_trait]
pub trait SynologyRuntime: Send + Sync {
    async fn login(
        &self,
        caller_user_id: &UserId,
        request: synology::LoginRequest,
    ) -> Result<synology::LoginResponse, ProviderError>;
    async fn list_files(
        &self,
        caller_user_id: &UserId,
        request: synology::ListFilesRequest,
    ) -> Result<synology::ListFilesResponse, ProviderError>;
    async fn list_libraries(
        &self,
        caller_user_id: &UserId,
        request: synology::ListLibrariesRequest,
    ) -> Result<synology::ListLibrariesResponse, ProviderError>;
    async fn list_movies(
        &self,
        caller_user_id: &UserId,
        request: synology::ListMoviesRequest,
    ) -> Result<synology::ListVideoItemsResponse, ProviderError>;
    async fn list_tv_shows(
        &self,
        caller_user_id: &UserId,
        request: synology::ListTvShowsRequest,
    ) -> Result<synology::ListVideoItemsResponse, ProviderError>;
    async fn list_episodes(
        &self,
        caller_user_id: &UserId,
        request: synology::ListEpisodesRequest,
    ) -> Result<synology::ListVideoItemsResponse, ProviderError>;
    async fn list_home_videos(
        &self,
        caller_user_id: &UserId,
        request: synology::ListHomeVideosRequest,
    ) -> Result<synology::ListVideoItemsResponse, ProviderError>;
    async fn list_tv_recordings(
        &self,
        caller_user_id: &UserId,
        request: synology::ListTvRecordingsRequest,
    ) -> Result<synology::ListVideoItemsResponse, ProviderError>;
    async fn logout(
        &self,
        caller_user_id: &UserId,
        request: synology::LogoutRequest,
    ) -> Result<synology::LogoutResponse, ProviderError>;
    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        request: synology::GetBindsRequest,
    ) -> Result<synology::GetBindsResponse, ProviderError>;
}
