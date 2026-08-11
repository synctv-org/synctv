use crate::provider_runtime::ProviderCredentialServerQuery;
use crate::runtime_error::RuntimeError;
use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::emby as emby_proto;

#[derive(Debug, Clone)]
pub enum EmbyLoginCredential {
    Password(String),
    ApiKey(String),
}

#[derive(Debug, Clone)]
pub struct EmbyLoginCommand {
    pub host: String,
    pub username: String,
    pub credential: Option<EmbyLoginCredential>,
}

#[derive(Debug, Clone)]
pub struct EmbyListQuery {
    pub server_id: String,
    pub mode: emby_proto::ListMode,
    pub target_id: String,
    pub item_types: Vec<String>,
    pub start_index: u64,
    pub limit: u64,
    pub search_term: String,
}

#[tonic::async_trait]
pub trait EmbyRuntime: Send + Sync {
    async fn login(
        &self,
        caller_user_id: &UserId,
        command: EmbyLoginCommand,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::LoginResponse, ProviderError>;
    async fn list(
        &self,
        caller_user_id: &UserId,
        query: EmbyListQuery,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::ListResponse, ProviderError>;
    async fn get_me(
        &self,
        caller_user_id: &UserId,
        query: ProviderCredentialServerQuery,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::GetMeResponse, ProviderError>;
    async fn logout(
        &self,
        caller_user_id: &UserId,
        command: ProviderCredentialServerQuery,
    ) -> Result<emby_proto::LogoutResponse, ProviderError>;
    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<emby_proto::GetBindsResponse, RuntimeError>;
}
