use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::live_proxy::live_proxy_playback_provider_service_server::LiveProxyPlaybackProviderService;
use synctv_proto::playback_provider::live_proxy::{
    GetLiveProxyFlvStreamRequest, GetLiveProxyHlsMasterRequest, GetLiveProxyHlsPlaylistRequest,
    GetLiveProxyHlsSegmentRequest, LiveProxyFlvStreamResponse, LiveProxyHlsMasterResponse,
    LiveProxyHlsPlaylistResponse, LiveProxyHlsSegmentResponse,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct LiveProxyPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl LiveProxyPlaybackProviderGrpcService {
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
impl LiveProxyPlaybackProviderService for LiveProxyPlaybackProviderGrpcService {
    type GetFlvStreamStream = GrpcResponseStream<LiveProxyFlvStreamResponse>;
    type GetHlsMasterStream = GrpcResponseStream<LiveProxyHlsMasterResponse>;
    type GetHlsPlaylistStream = GrpcResponseStream<LiveProxyHlsPlaylistResponse>;
    type GetHlsSegmentStream = GrpcResponseStream<LiveProxyHlsSegmentResponse>;

    async fn get_flv_stream(
        &self,
        request: Request<GetLiveProxyFlvStreamRequest>,
    ) -> Result<Response<Self::GetFlvStreamStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::live_proxy::get_live_proxy_flv_stream(
                    live_proxy_deps(&state, Some(&request_control)),
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
        request: Request<GetLiveProxyHlsPlaylistRequest>,
    ) -> Result<Response<Self::GetHlsPlaylistStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::live_proxy::get_live_proxy_hls_playlist(
                    live_proxy_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_hls_master(
        &self,
        request: Request<GetLiveProxyHlsMasterRequest>,
    ) -> Result<Response<Self::GetHlsMasterStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::live_proxy::get_live_proxy_hls_master(
                    live_proxy_deps(&state, Some(&request_control)),
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
        request: Request<GetLiveProxyHlsSegmentRequest>,
    ) -> Result<Response<Self::GetHlsSegmentStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::live_proxy::get_live_proxy_hls_segment(
                    live_proxy_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }
}

fn live_proxy_deps<'a>(
    state: &'a PlaybackProviderGrpcState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::live_proxy::LiveProxyPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::live_proxy::LiveProxyPlaybackProviderDeps {
        playback_provider_service: &state
            .shared_api_runtime
            .live_proxy_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        live_runtime: super::live_playback_api_runtime(state),
        request_control,
    }
}
