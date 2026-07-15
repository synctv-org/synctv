use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::synology::synology_playback_provider_service_server::SynologyPlaybackProviderService;
use synctv_proto::playback_provider::synology::{
    GetSynologyResourceRequest, GetSynologySegmentRequest, GetSynologySubtitleRequest,
    SynologyResourceResponse, SynologySegmentResponse, SynologySubtitleResponse,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct SynologyPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<crate::ApiRuntimeSettings>,
}

impl SynologyPlaybackProviderGrpcService {
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
impl SynologyPlaybackProviderService for SynologyPlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<SynologyResourceResponse>;
    type GetSegmentStream = GrpcResponseStream<SynologySegmentResponse>;
    type GetSubtitleStream = GrpcResponseStream<SynologySubtitleResponse>;

    async fn get_resource(
        &self,
        request: Request<GetSynologyResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::synology::get_synology_resource(
                    deps(&state, Some(&control)),
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
        request: Request<GetSynologySegmentRequest>,
    ) -> Result<Response<Self::GetSegmentStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::synology::get_synology_segment(
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
        request: Request<GetSynologySubtitleRequest>,
    ) -> Result<Response<Self::GetSubtitleStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::synology::get_synology_subtitle(
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
) -> crate::impls::playback_provider::synology::SynologyPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::synology::SynologyPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.synology_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
