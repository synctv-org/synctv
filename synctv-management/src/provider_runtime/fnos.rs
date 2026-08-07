use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::fnos;

#[tonic::async_trait]
pub trait FnosRuntime: Send + Sync {
    async fn login(
        &self,
        caller_user_id: &UserId,
        request: fnos::LoginRequest,
    ) -> Result<fnos::LoginResponse, ProviderError>;
    async fn list(
        &self,
        caller_user_id: &UserId,
        request: fnos::ListRequest,
    ) -> Result<fnos::ListResponse, ProviderError>;
    async fn list_media_libraries(
        &self,
        caller_user_id: &UserId,
        request: fnos::ListMediaLibrariesRequest,
    ) -> Result<fnos::ListMediaLibrariesResponse, ProviderError>;
    async fn list_media_items(
        &self,
        caller_user_id: &UserId,
        request: fnos::ListMediaItemsRequest,
    ) -> Result<fnos::ListMediaItemsResponse, ProviderError>;
    async fn set_favorite(
        &self,
        caller_user_id: &UserId,
        request: fnos::SetFavoriteRequest,
    ) -> Result<fnos::SetFavoriteResponse, ProviderError>;
    async fn set_watched(
        &self,
        caller_user_id: &UserId,
        request: fnos::SetWatchedRequest,
    ) -> Result<fnos::SetWatchedResponse, ProviderError>;
    async fn get_server_info(
        &self,
        caller_user_id: &UserId,
        request: fnos::GetServerInfoRequest,
    ) -> Result<fnos::GetServerInfoResponse, ProviderError>;
    async fn logout(
        &self,
        caller_user_id: &UserId,
        request: fnos::LogoutRequest,
    ) -> Result<fnos::LogoutResponse, ProviderError>;
    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        request: fnos::GetBindsRequest,
    ) -> Result<fnos::GetBindsResponse, ProviderError>;
}
