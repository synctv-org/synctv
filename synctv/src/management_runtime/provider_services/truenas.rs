use super::take_instance;
use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError};
use synctv_management::provider_runtime::TruenasRuntime;
use synctv_proto::providers::truenas as truenas_proto;
pub(crate) struct ManagementTruenasRuntime {
    inner: Arc<synctv_api::providers::TrueNasApiImpl>,
}

impl ManagementTruenasRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::providers::TrueNasApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl TruenasRuntime for ManagementTruenasRuntime {
    async fn login(
        &self,
        user: &UserId,
        mut request: truenas_proto::LoginRequest,
    ) -> Result<truenas_proto::LoginResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.login(*user, request, instance.as_deref()).await
    }

    async fn list(
        &self,
        user: &UserId,
        mut request: truenas_proto::ListRequest,
    ) -> Result<truenas_proto::ListResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.list(*user, request, instance.as_deref()).await
    }

    async fn logout(
        &self,
        user: &UserId,
        request: truenas_proto::LogoutRequest,
    ) -> Result<truenas_proto::LogoutResponse, ProviderError> {
        self.inner.logout(*user, request).await
    }

    async fn get_binds(
        &self,
        user: &UserId,
        mut request: truenas_proto::GetBindsRequest,
    ) -> Result<truenas_proto::GetBindsResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.binds(*user, instance.as_deref()).await
    }
}
