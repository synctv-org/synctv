use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::seafile::seafile_playback_provider_service_server::SeafilePlaybackProviderService;
use synctv_proto::playback_provider::seafile::{
    GetSeafileResourceRequest, GetSeafileSubtitleRequest, SeafileResourceResponse,
    SeafileSubtitleResponse,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct SeafilePlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<crate::ApiRuntimeSettings>,
}

impl SeafilePlaybackProviderGrpcService {
    #[must_use]
    pub fn new(
        state: Arc<PlaybackProviderGrpcState>,
        runtime_settings: Arc<crate::ApiRuntimeSettings>,
    ) -> Self {
        Self {
            state,
            runtime_settings,
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl SeafilePlaybackProviderService for SeafilePlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<SeafileResourceResponse>;
    type GetSubtitleStream = GrpcResponseStream<SeafileSubtitleResponse>;

    async fn get_resource(
        &self,
        request: Request<GetSeafileResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::seafile::get_seafile_resource(
                    deps(&state, Some(&control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_subtitle(
        &self,
        request: Request<GetSeafileSubtitleRequest>,
    ) -> Result<Response<Self::GetSubtitleStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::seafile::get_seafile_subtitle(
                    deps(&state, Some(&control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }
}

fn deps<'a>(
    state: &'a PlaybackProviderGrpcState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> crate::impls::playback_provider::seafile::SeafilePlaybackProviderDeps<'a> {
    crate::impls::playback_provider::seafile::SeafilePlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.seafile_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
