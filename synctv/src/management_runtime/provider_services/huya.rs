use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError, service::HuyaPlaybackProviderService};
use synctv_management::provider_runtime::HuyaRuntime;
use synctv_proto::providers::huya as huya_proto;
pub(crate) struct ManagementHuyaRuntime {
    inner: Arc<HuyaPlaybackProviderService>,
}

impl ManagementHuyaRuntime {
    pub(crate) fn new(inner: Arc<HuyaPlaybackProviderService>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl HuyaRuntime for ManagementHuyaRuntime {
    async fn resolve(
        &self,
        _: &UserId,
        request: huya_proto::ResolveRequest,
    ) -> Result<huya_proto::ResolveResponse, ProviderError> {
        self.inner
            .resolve_resource(&request.resource)
            .await
            .map(synctv_api::providers::huya::resolve_response)
    }
}
