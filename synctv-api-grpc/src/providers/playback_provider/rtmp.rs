use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::rtmp::rtmp_playback_provider_service_server::RtmpPlaybackProviderService;
use synctv_proto::playback_provider::rtmp::{
    GetRtmpFlvStreamRequest, GetRtmpHlsPlaylistRequest, GetRtmpHlsSegmentRequest,
    RtmpFlvStreamResponse, RtmpHlsPlaylistResponse, RtmpHlsSegmentResponse,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct RtmpPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl RtmpPlaybackProviderGrpcService {
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
impl RtmpPlaybackProviderService for RtmpPlaybackProviderGrpcService {
    type GetFlvStreamStream = GrpcResponseStream<RtmpFlvStreamResponse>;
    type GetHlsPlaylistStream = GrpcResponseStream<RtmpHlsPlaylistResponse>;
    type GetHlsSegmentStream = GrpcResponseStream<RtmpHlsSegmentResponse>;

    async fn get_flv_stream(
        &self,
        request: Request<GetRtmpFlvStreamRequest>,
    ) -> Result<Response<Self::GetFlvStreamStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::rtmp::get_rtmp_flv_stream(
                    rtmp_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_hls_playlist(
        &self,
        request: Request<GetRtmpHlsPlaylistRequest>,
    ) -> Result<Response<Self::GetHlsPlaylistStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::rtmp::get_rtmp_hls_playlist(
                    rtmp_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_hls_segment(
        &self,
        request: Request<GetRtmpHlsSegmentRequest>,
    ) -> Result<Response<Self::GetHlsSegmentStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::rtmp::get_rtmp_hls_segment(
                    rtmp_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }
}

fn rtmp_deps<'a>(
    state: &'a PlaybackProviderGrpcState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::rtmp::RtmpPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::rtmp::RtmpPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.rtmp_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        live_runtime: super::live_playback_api_runtime(state),
        request_control,
    }
}
