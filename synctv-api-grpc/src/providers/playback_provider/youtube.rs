use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::youtube::youtube_playback_provider_service_server::YoutubePlaybackProviderService;
use synctv_proto::playback_provider::youtube::{
    GetYoutubeResourceRequest, GetYoutubeSegmentRequest, GetYoutubeSubtitleRequest,
    YoutubeResourceResponse, YoutubeSegmentResponse, YoutubeSubtitleResponse,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct YoutubePlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl YoutubePlaybackProviderGrpcService {
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
impl YoutubePlaybackProviderService for YoutubePlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<YoutubeResourceResponse>;
    type GetSegmentStream = GrpcResponseStream<YoutubeSegmentResponse>;
    type GetSubtitleStream = GrpcResponseStream<YoutubeSubtitleResponse>;

    async fn get_resource(
        &self,
        request: Request<GetYoutubeResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::youtube::get_youtube_resource(
                    youtube_deps(&state, Some(&request_control)),
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
        request: Request<GetYoutubeSegmentRequest>,
    ) -> Result<Response<Self::GetSegmentStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::youtube::get_youtube_segment(
                    youtube_deps(&state, Some(&request_control)),
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
        request: Request<GetYoutubeSubtitleRequest>,
    ) -> Result<Response<Self::GetSubtitleStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::youtube::get_youtube_subtitle(
                    youtube_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }
}

fn youtube_deps<'a>(
    state: &'a PlaybackProviderGrpcState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::youtube::YoutubePlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::youtube::YoutubePlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.youtube_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
