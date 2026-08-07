use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::fnos::fnos_playback_provider_service_server::FnosPlaybackProviderService;
use synctv_proto::playback_provider::fnos::{
    FnosResourceResponse, FnosSegmentResponse, FnosSubtitleResponse, FnosThumbnailResponse,
    GetFnosResourceRequest, GetFnosSegmentRequest, GetFnosSubtitleRequest, GetFnosThumbnailRequest,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct FnosPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl FnosPlaybackProviderGrpcService {
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
impl FnosPlaybackProviderService for FnosPlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<FnosResourceResponse>;
    type GetSegmentStream = GrpcResponseStream<FnosSegmentResponse>;
    type GetSubtitleStream = GrpcResponseStream<FnosSubtitleResponse>;
    type GetThumbnailStream = GrpcResponseStream<FnosThumbnailResponse>;

    async fn get_resource(
        &self,
        request: Request<GetFnosResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::fnos::get_fnos_resource(
                    fnos_deps(&state, Some(&request_control)),
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
        request: Request<GetFnosSegmentRequest>,
    ) -> Result<Response<Self::GetSegmentStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::fnos::get_fnos_segment(
                    fnos_deps(&state, Some(&request_control)),
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
        request: Request<GetFnosSubtitleRequest>,
    ) -> Result<Response<Self::GetSubtitleStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::fnos::get_fnos_subtitle(
                    fnos_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_thumbnail(
        &self,
        request: Request<GetFnosThumbnailRequest>,
    ) -> Result<Response<Self::GetThumbnailStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::fnos::get_fnos_thumbnail(
                    fnos_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }
}

fn fnos_deps<'a>(
    state: &'a PlaybackProviderGrpcState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::fnos::FnosPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::fnos::FnosPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.fnos_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
