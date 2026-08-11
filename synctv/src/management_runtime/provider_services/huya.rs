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
        let instance_name = request.instance_name.clone();
        self.inner
            .resolve_resource(&request.resource)
            .await
            .map(|media| {
                synctv_api::providers::huya::resolve_response(
                    media,
                    (!instance_name.is_empty()).then_some(instance_name.as_str()),
                )
            })
    }
}
