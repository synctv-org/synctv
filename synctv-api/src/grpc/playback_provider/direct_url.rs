use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::direct_url::direct_url_playback_provider_service_server::DirectUrlPlaybackProviderService;
use synctv_proto::playback_provider::direct_url::{
    DirectUrlHlsManifestResponse, DirectUrlHlsSegmentResponse, DirectUrlStreamResponse,
    DirectUrlSubtitleResponse, GetDirectUrlHlsManifestRequest, GetDirectUrlHlsSegmentRequest,
    GetDirectUrlStreamRequest, GetDirectUrlSubtitleRequest,
};
use tonic::{Request, Response, Status};

use super::{execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream};
use crate::http::AppState;

#[derive(Clone)]
pub struct DirectUrlPlaybackProviderGrpcService {
    state: Arc<AppState>,
    config: Arc<synctv_core::Config>,
}

impl DirectUrlPlaybackProviderGrpcService {
    #[must_use]
    pub fn new(state: Arc<AppState>, config: Arc<synctv_core::Config>) -> Self {
        Self { state, config }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl DirectUrlPlaybackProviderService for DirectUrlPlaybackProviderGrpcService {
    type GetStreamStream = GrpcResponseStream<DirectUrlStreamResponse>;
    type GetHlsManifestStream = GrpcResponseStream<DirectUrlHlsManifestResponse>;
    type GetHlsSegmentStream = GrpcResponseStream<DirectUrlHlsSegmentResponse>;
    type GetSubtitleStream = GrpcResponseStream<DirectUrlSubtitleResponse>;

    async fn get_stream(
        &self,
        request: Request<GetDirectUrlStreamRequest>,
    ) -> Result<Response<Self::GetStreamStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::direct_url::get_direct_url_stream(
                    direct_url_deps(&state, Some(&request_control)),
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
        request: Request<GetDirectUrlHlsManifestRequest>,
    ) -> Result<Response<Self::GetHlsManifestStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::direct_url::get_direct_url_hls_manifest(
                    direct_url_deps(&state, Some(&request_control)),
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
        request: Request<GetDirectUrlHlsSegmentRequest>,
    ) -> Result<Response<Self::GetHlsSegmentStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::direct_url::get_direct_url_hls_segment(
                    direct_url_deps(&state, Some(&request_control)),
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
        request: Request<GetDirectUrlSubtitleRequest>,
    ) -> Result<Response<Self::GetSubtitleStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.config)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::direct_url::get_direct_url_subtitle(
                    direct_url_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }
}

fn direct_url_deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> crate::impls::playback_provider::direct_url::DirectUrlPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::direct_url::DirectUrlPlaybackProviderDeps {
        playback_provider_service: &state
            .shared_api_runtime
            .direct_url_playback_provider_service,
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
