use crate::request_context::RequestContext;
use crate::runtime_error::RuntimeError;
use synctv_core::models::{ProviderInstanceListQuery, SourceProvider, UserId};
use synctv_proto::providers::common as provider_common_proto;

#[derive(Debug, Clone)]
pub struct ListAvailableProviderInstancesQuery {
    pub provider_type: Option<SourceProvider>,
}

#[derive(Debug, Clone)]
pub struct ListProviderBackendsQuery {
    pub provider_type: SourceProvider,
}

#[derive(Debug, Clone)]
pub struct AddProviderInstanceCommand {
    pub name: String,
    pub endpoint: String,
    pub comment: String,
    pub timeout_seconds: u32,
    pub tls: bool,
    pub insecure_tls: bool,
    pub providers: Vec<i32>,
    pub jwt_secret: Option<String>,
    pub custom_ca: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateProviderInstanceCommand {
    pub name: String,
    pub endpoint: Option<String>,
    pub comment: Option<String>,
    pub timeout_seconds: Option<u32>,
    pub tls: Option<bool>,
    pub insecure_tls: Option<bool>,
    pub providers: Vec<i32>,
    pub jwt_secret: Option<String>,
    pub custom_ca: Option<String>,
    pub clear_comment: Option<bool>,
    pub clear_jwt_secret: Option<bool>,
    pub clear_custom_ca: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ProviderInstanceNameCommand {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct ProviderCredentialServerQuery {
    pub server_id: String,
}

#[tonic::async_trait]
pub trait ProviderCommonRuntime: Send + Sync {
    async fn list_available_provider_instances(
        &self,
        query: ListAvailableProviderInstancesQuery,
    ) -> Result<provider_common_proto::ProviderInstancesResponse, RuntimeError>;

    async fn list_provider_backends(
        &self,
        query: ListProviderBackendsQuery,
    ) -> Result<provider_common_proto::ProviderBackendsResponse, RuntimeError>;

    async fn list_provider_instances(
        &self,
        query: ProviderInstanceListQuery,
    ) -> Result<provider_common_proto::ListProviderInstancesResponse, RuntimeError>;

    async fn add_provider_instance(
        &self,
        command: AddProviderInstanceCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::AddProviderInstanceResponse, RuntimeError>;

    async fn update_provider_instance(
        &self,
        command: UpdateProviderInstanceCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::UpdateProviderInstanceResponse, RuntimeError>;

    async fn delete_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::DeleteProviderInstanceResponse, RuntimeError>;

    async fn reconnect_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<provider_common_proto::ReconnectProviderInstanceResponse, RuntimeError>;

    async fn enable_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
    ) -> Result<provider_common_proto::EnableProviderInstanceResponse, RuntimeError>;

    async fn disable_provider_instance(
        &self,
        command: ProviderInstanceNameCommand,
    ) -> Result<provider_common_proto::DisableProviderInstanceResponse, RuntimeError>;
}
