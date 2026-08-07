use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::cctv::cctv_playback_provider_service_server::CctvPlaybackProviderService;
use synctv_proto::playback_provider::cctv::{
    CctvResourceResponse, CctvSegmentResponse, GetCctvResourceRequest, GetCctvSegmentRequest,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct CctvPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl CctvPlaybackProviderGrpcService {
    #[must_use]
    pub fn new(
        state: Arc<PlaybackProviderGrpcState>,
        runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
    ) -> Self {
        Self {
            state,
            runtime_settings,
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl CctvPlaybackProviderService for CctvPlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<CctvResourceResponse>;
    type GetSegmentStream = GrpcResponseStream<CctvSegmentResponse>;

    async fn get_resource(
        &self,
        request: Request<GetCctvResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::cctv::get_cctv_resource(
                    cctv_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_segment(
        &self,
        request: Request<GetCctvSegmentRequest>,
    ) -> Result<Response<Self::GetSegmentStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::cctv::get_cctv_segment(
                    cctv_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }
}

fn cctv_deps<'a>(
    state: &'a PlaybackProviderGrpcState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::cctv::CctvPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::cctv::CctvPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.cctv_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
