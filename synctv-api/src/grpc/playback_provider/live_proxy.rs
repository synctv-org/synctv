use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::live_proxy::live_proxy_playback_provider_service_server::LiveProxyPlaybackProviderService;
use synctv_proto::playback_provider::live_proxy::{
    GetLiveProxyFlvStreamRequest, GetLiveProxyHlsPlaylistRequest, GetLiveProxyHlsSegmentRequest,
    LiveProxyFlvStreamResponse, LiveProxyHlsPlaylistResponse, LiveProxyHlsSegmentResponse,
};
use tonic::{Request, Response, Status};

use super::{execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream};
use crate::http::AppState;

#[derive(Clone)]
pub struct LiveProxyPlaybackProviderGrpcService {
    state: Arc<AppState>,
    config: Arc<synctv_core::Config>,
}

impl LiveProxyPlaybackProviderGrpcService {
    #[must_use]
    pub fn new(state: Arc<AppState>, config: Arc<synctv_core::Config>) -> Self {
        Self { state, config }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl LiveProxyPlaybackProviderService for LiveProxyPlaybackProviderGrpcService {
    type GetFlvStreamStream = GrpcResponseStream<LiveProxyFlvStreamResponse>;
    type GetHlsPlaylistStream = GrpcResponseStream<LiveProxyHlsPlaylistResponse>;
    type GetHlsSegmentStream = GrpcResponseStream<LiveProxyHlsSegmentResponse>;

    async fn get_flv_stream(
        &self,
        request: Request<GetLiveProxyFlvStreamRequest>,
    ) -> Result<Response<Self::GetFlvStreamStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::live_proxy::get_live_proxy_flv_stream(
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
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::live_proxy::get_live_proxy_hls_playlist(
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
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::live_proxy::get_live_proxy_hls_segment(
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
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> crate::impls::playback_provider::live_proxy::LiveProxyPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::live_proxy::LiveProxyPlaybackProviderDeps {
        playback_provider_service: &state
            .shared_api_runtime
            .live_proxy_playback_provider_service,
        proxy_signing_key: &state.shared_api_runtime.proxy_signing_key,
        public_id_codec: &state.shared_api_runtime.public_id_codec,
        provider_stores: state.shared_api_runtime.provider_stores.as_ref(),
        user_service: &state.shared_api_runtime.client_api.user_service,
        playback_transport_services: &state.shared_api_runtime.playback_transport_services,
        request_control,
        proxy_http_client: &state.proxy_http_client,
        ssrf_guard: &state.ssrf_guard,
        proxy_slice_cache: &state.proxy_slice_cache,
        live_streaming_infrastructure: state.shared_api_runtime.client_api.live_infrastructure(),
        connection_runtime: state.connection_manager.as_ref(),
        livestream_config: &state.config.livestream,
        runtime_settings_store: state.runtime_settings_store.as_deref(),
    }
}
