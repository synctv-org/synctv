use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError, service::AcFunPlaybackProviderService};
use synctv_management::provider_runtime::AcfunRuntime;
use synctv_proto::providers::acfun as acfun_proto;
pub(crate) struct ManagementAcfunRuntime {
    inner: Arc<AcFunPlaybackProviderService>,
}

impl ManagementAcfunRuntime {
    pub(crate) fn new(inner: Arc<AcFunPlaybackProviderService>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl AcfunRuntime for ManagementAcfunRuntime {
    async fn resolve(
        &self,
        _: &UserId,
        request: acfun_proto::ResolveRequest,
    ) -> Result<acfun_proto::ResolveResponse, ProviderError> {
        self.inner
            .resolve_resource(&request.resource)
            .await
            .map(synctv_api::providers::acfun::resolve_response)
    }
}
