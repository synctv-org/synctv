use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::alist::alist_playback_provider_service_server::AlistPlaybackProviderService;
use synctv_proto::playback_provider::alist::{
    AlistFileStreamResponse, AlistSubtitleResponse, AlistThumbnailResponse,
    AlistTranscodedHlsManifestResponse, AlistTranscodedHlsSegmentResponse,
    GetAlistFileStreamRequest, GetAlistSubtitleRequest, GetAlistThumbnailRequest,
    GetAlistTranscodedHlsManifestRequest, GetAlistTranscodedHlsSegmentRequest,
};
use tonic::{Request, Response, Status};

use super::{execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream};
use crate::http::AppState;

#[derive(Clone)]
pub struct AlistPlaybackProviderGrpcService {
    state: Arc<AppState>,
    config: Arc<synctv_core::Config>,
}

impl AlistPlaybackProviderGrpcService {
    #[must_use]
    pub fn new(state: Arc<AppState>, config: Arc<synctv_core::Config>) -> Self {
        Self { state, config }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl AlistPlaybackProviderService for AlistPlaybackProviderGrpcService {
    type GetFileStreamStream = GrpcResponseStream<AlistFileStreamResponse>;
    type GetTranscodedHlsManifestStream = GrpcResponseStream<AlistTranscodedHlsManifestResponse>;
    type GetTranscodedHlsSegmentStream = GrpcResponseStream<AlistTranscodedHlsSegmentResponse>;
    type GetSubtitleStream = GrpcResponseStream<AlistSubtitleResponse>;
    type GetThumbnailStream = GrpcResponseStream<AlistThumbnailResponse>;

    async fn get_file_stream(
        &self,
        request: Request<GetAlistFileStreamRequest>,
    ) -> Result<Response<Self::GetFileStreamStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::alist::get_alist_file_stream(
                    alist_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_transcoded_hls_manifest(
        &self,
        request: Request<GetAlistTranscodedHlsManifestRequest>,
    ) -> Result<Response<Self::GetTranscodedHlsManifestStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::alist::get_alist_transcoded_hls_manifest(
                    alist_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_transcoded_hls_segment(
        &self,
        request: Request<GetAlistTranscodedHlsSegmentRequest>,
    ) -> Result<Response<Self::GetTranscodedHlsSegmentStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();
        let state_for_stream = state.clone();
        execute_playback_provider_stream(state, metadata, move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::alist::get_alist_transcoded_hls_segment(
                    alist_deps(&state, Some(&request_control)),
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
        request: Request<GetAlistSubtitleRequest>,
    ) -> Result<Response<Self::GetSubtitleStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();
        let state_for_stream = state.clone();
        execute_playback_provider_stream(state, metadata, move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::alist::get_alist_subtitle(
                    alist_deps(&state, Some(&request_control)),
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
        request: Request<GetAlistThumbnailRequest>,
    ) -> Result<Response<Self::GetThumbnailStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();
        let state_for_stream = state.clone();
        execute_playback_provider_stream(state, metadata, move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::alist::get_alist_thumbnail(
                    alist_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }
}

fn alist_deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> crate::impls::playback_provider::alist::AlistPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::alist::AlistPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.alist_playback_provider_service,
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
