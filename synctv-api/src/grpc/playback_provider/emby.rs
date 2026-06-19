use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::emby::emby_playback_provider_service_server::EmbyPlaybackProviderService;
use synctv_proto::playback_provider::emby::{
    EmbyHlsManifestResponse, EmbyHlsSegmentResponse, EmbyMediaStreamResponse, EmbySubtitleResponse,
    GetEmbyHlsManifestRequest, GetEmbyHlsSegmentRequest, GetEmbyMediaStreamRequest,
    GetEmbySubtitleRequest,
};
use tonic::{Request, Response, Status};

use super::{execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream};
use crate::http::AppState;

#[derive(Clone)]
pub struct EmbyPlaybackProviderGrpcService {
    state: Arc<AppState>,
    config: Arc<synctv_core::Config>,
}

impl EmbyPlaybackProviderGrpcService {
    #[must_use]
    pub fn new(state: Arc<AppState>, config: Arc<synctv_core::Config>) -> Self {
        Self { state, config }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl EmbyPlaybackProviderService for EmbyPlaybackProviderGrpcService {
    type GetMediaStreamStream = GrpcResponseStream<EmbyMediaStreamResponse>;
    type GetHlsManifestStream = GrpcResponseStream<EmbyHlsManifestResponse>;
    type GetHlsSegmentStream = GrpcResponseStream<EmbyHlsSegmentResponse>;
    type GetSubtitleStream = GrpcResponseStream<EmbySubtitleResponse>;

    async fn get_media_stream(
        &self,
        request: Request<GetEmbyMediaStreamRequest>,
    ) -> Result<Response<Self::GetMediaStreamStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::emby::get_emby_media_stream(
                    emby_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_hls_manifest(
        &self,
        request: Request<GetEmbyHlsManifestRequest>,
    ) -> Result<Response<Self::GetHlsManifestStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::emby::get_emby_hls_manifest(
                    emby_deps(&state, Some(&request_control)),
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
        request: Request<GetEmbyHlsSegmentRequest>,
    ) -> Result<Response<Self::GetHlsSegmentStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::emby::get_emby_hls_segment(
                    emby_deps(&state, Some(&request_control)),
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
        request: Request<GetEmbySubtitleRequest>,
    ) -> Result<Response<Self::GetSubtitleStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::emby::get_emby_subtitle(
                    emby_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }
}

fn emby_deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> crate::impls::playback_provider::emby::EmbyPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::emby::EmbyPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.emby_playback_provider_service,
        proxy_signing_key: &state.shared_api_runtime.proxy_signing_key,
        public_id_codec: &state.shared_api_runtime.public_id_codec,
        provider_stores: state.shared_api_runtime.provider_stores.as_ref(),
        user_service: &state.shared_api_runtime.client_api.user_service,
        playback_transport_services: &state.shared_api_runtime.playback_transport_services,
        request_control,
        proxy_http_client: &state.proxy_http_client,
        ssrf_guard: &state.ssrf_guard,
        proxy_slice_cache: &state.proxy_slice_cache,
    }
}
