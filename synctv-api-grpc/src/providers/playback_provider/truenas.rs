use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::truenas::true_nas_playback_provider_service_server::TrueNasPlaybackProviderService;
use synctv_proto::playback_provider::truenas::{
    GetTrueNasHlsManifestRequest, GetTrueNasHlsResourceRequest, GetTrueNasResourceRequest,
    GetTrueNasSubtitleRequest, TrueNasHlsManifestResponse, TrueNasHlsResourceResponse,
    TrueNasResourceResponse, TrueNasSubtitleResponse,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct TrueNasPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl TrueNasPlaybackProviderGrpcService {
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
impl TrueNasPlaybackProviderService for TrueNasPlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<TrueNasResourceResponse>;
    type GetHlsManifestStream = GrpcResponseStream<TrueNasHlsManifestResponse>;
    type GetHlsResourceStream = GrpcResponseStream<TrueNasHlsResourceResponse>;
    type GetSubtitleStream = GrpcResponseStream<TrueNasSubtitleResponse>;

    async fn get_resource(
        &self,
        request: Request<GetTrueNasResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::truenas::get_truenas_resource(
                    deps(&state, Some(&control)),
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
        request: Request<GetTrueNasHlsManifestRequest>,
    ) -> Result<Response<Self::GetHlsManifestStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::truenas::get_truenas_hls_manifest(
                    deps(&state, Some(&control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_hls_resource(
        &self,
        request: Request<GetTrueNasHlsResourceRequest>,
    ) -> Result<Response<Self::GetHlsResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::truenas::get_truenas_hls_resource(
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
        request: Request<GetTrueNasSubtitleRequest>,
    ) -> Result<Response<Self::GetSubtitleStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::truenas::get_truenas_subtitle(
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
) -> synctv_api_common::playback_provider::truenas::TrueNasPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::truenas::TrueNasPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.truenas_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
