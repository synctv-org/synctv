use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::seafile;

#[tonic::async_trait]
pub trait SeafileRuntime: Send + Sync {
    async fn login(
        &self,
        caller_user_id: &UserId,
        request: seafile::LoginRequest,
    ) -> Result<seafile::LoginResponse, ProviderError>;
    async fn unlock_library(
        &self,
        caller_user_id: &UserId,
        request: seafile::UnlockLibraryRequest,
    ) -> Result<seafile::UnlockLibraryResponse, ProviderError>;
    async fn list_repositories(
        &self,
        caller_user_id: &UserId,
        request: seafile::ListRepositoriesRequest,
    ) -> Result<seafile::ListResponse, ProviderError>;
    async fn list(
        &self,
        caller_user_id: &UserId,
        request: seafile::ListRequest,
    ) -> Result<seafile::ListResponse, ProviderError>;
    async fn list_starred(
        &self,
        caller_user_id: &UserId,
        request: seafile::ListStarredRequest,
    ) -> Result<seafile::ListResponse, ProviderError>;
    async fn logout(
        &self,
        caller_user_id: &UserId,
        request: seafile::LogoutRequest,
    ) -> Result<seafile::LogoutResponse, ProviderError>;
    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        request: seafile::GetBindsRequest,
    ) -> Result<seafile::GetBindsResponse, ProviderError>;
}
