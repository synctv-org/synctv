use crate::provider_runtime::ProviderCredentialServerQuery;
use crate::runtime_error::RuntimeError;
use synctv_core::models::UserId;
use synctv_core::provider::ProviderError;
use synctv_proto::providers::alist as alist_proto;

#[derive(Debug, Clone)]
pub enum AlistLoginCredential {
    Password(String),
    HashedPassword(String),
}

#[derive(Debug, Clone)]
pub struct AlistLoginCommand {
    pub host: String,
    pub username: String,
    pub credential: Option<AlistLoginCredential>,
    pub otp_code: String,
    pub otp_secret: String,
}

#[derive(Debug, Clone)]
pub struct AlistListQuery {
    pub server_id: String,
    pub path: String,
    pub password: String,
    pub page: u64,
    pub per_page: u64,
    pub refresh: bool,
}

#[derive(Debug, Clone)]
pub struct AlistSearchQuery {
    pub server_id: String,
    pub parent: String,
    pub keywords: String,
    pub scope: u64,
    pub page: u64,
    pub per_page: u64,
    pub password: String,
}

#[tonic::async_trait]
pub trait AlistRuntime: Send + Sync {
    async fn login(
        &self,
        caller_user_id: &UserId,
        command: AlistLoginCommand,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::LoginResponse, ProviderError>;
    async fn list(
        &self,
        caller_user_id: &UserId,
        query: AlistListQuery,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::ListResponse, ProviderError>;
    async fn search(
        &self,
        caller_user_id: &UserId,
        query: AlistSearchQuery,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::SearchResponse, ProviderError>;
    async fn get_me(
        &self,
        caller_user_id: &UserId,
        query: ProviderCredentialServerQuery,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::GetMeResponse, ProviderError>;
    async fn logout(
        &self,
        caller_user_id: &UserId,
        command: ProviderCredentialServerQuery,
    ) -> Result<alist_proto::LogoutResponse, ProviderError>;
    async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<alist_proto::GetBindsResponse, RuntimeError>;
}
