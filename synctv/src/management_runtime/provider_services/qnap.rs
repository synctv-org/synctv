use super::take_instance;
use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError};
use synctv_management::provider_runtime::QnapRuntime;
use synctv_proto::providers::qnap as qnap_proto;
pub(crate) struct ManagementQnapRuntime {
    inner: Arc<synctv_api::providers::QnapApiImpl>,
}

impl ManagementQnapRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::providers::QnapApiImpl>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl QnapRuntime for ManagementQnapRuntime {
    async fn login(
        &self,
        user: &UserId,
        mut request: qnap_proto::LoginRequest,
    ) -> Result<qnap_proto::LoginResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.login(*user, request, instance.as_deref()).await
    }

    async fn list(
        &self,
        user: &UserId,
        mut request: qnap_proto::ListRequest,
    ) -> Result<qnap_proto::ListResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.list(*user, request, instance.as_deref()).await
    }

    async fn get_capabilities(
        &self,
        user: &UserId,
        mut request: qnap_proto::GetCapabilitiesRequest,
    ) -> Result<qnap_proto::GetCapabilitiesResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner
            .capabilities(*user, request, instance.as_deref())
            .await
    }

    async fn logout(
        &self,
        user: &UserId,
        request: qnap_proto::LogoutRequest,
    ) -> Result<qnap_proto::LogoutResponse, ProviderError> {
        self.inner.logout(*user, request).await
    }

    async fn get_binds(
        &self,
        user: &UserId,
        mut request: qnap_proto::GetBindsRequest,
    ) -> Result<qnap_proto::GetBindsResponse, ProviderError> {
        let instance = take_instance(&mut request.instance_name);
        self.inner.binds(*user, instance.as_deref()).await
    }
}
