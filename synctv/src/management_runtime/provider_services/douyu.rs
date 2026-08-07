use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError, service::DouyuPlaybackProviderService};
use synctv_management::provider_runtime::DouyuRuntime;
use synctv_proto::providers::douyu as douyu_proto;
pub(crate) struct ManagementDouyuRuntime {
    inner: Arc<DouyuPlaybackProviderService>,
}

impl ManagementDouyuRuntime {
    pub(crate) fn new(inner: Arc<DouyuPlaybackProviderService>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl DouyuRuntime for ManagementDouyuRuntime {
    async fn resolve(
        &self,
        _: &UserId,
        request: douyu_proto::ResolveRequest,
    ) -> Result<douyu_proto::ResolveResponse, ProviderError> {
        self.inner
            .resolve_resource(&request.resource)
            .await
            .map(synctv_api::providers::douyu::resolve_response)
    }
}
