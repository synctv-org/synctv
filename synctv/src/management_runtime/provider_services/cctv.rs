use std::sync::Arc;

use synctv_core::{models::UserId, provider::ProviderError, service::CctvPlaybackProviderService};
use synctv_management::provider_runtime::CctvRuntime;
use synctv_proto::providers::cctv as cctv_proto;
pub(crate) struct ManagementCctvRuntime {
    inner: Arc<CctvPlaybackProviderService>,
}

impl ManagementCctvRuntime {
    pub(crate) fn new(inner: Arc<CctvPlaybackProviderService>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl CctvRuntime for ManagementCctvRuntime {
    async fn resolve(
        &self,
        _: &UserId,
        request: cctv_proto::ResolveRequest,
    ) -> Result<cctv_proto::ResolveResponse, ProviderError> {
        let resource = request.resource;
        let instance_name = request.instance_name;
        self.inner.resolve_resource(&resource).await.map(|media| {
            synctv_api::providers::cctv::resolve_response(
                media,
                resource,
                (!instance_name.is_empty()).then_some(instance_name.as_str()),
            )
        })
    }
}
