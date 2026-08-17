use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::seafile::seafile_playback_provider_service_server::SeafilePlaybackProviderService;
use synctv_proto::playback_provider::seafile::{
    GetSeafileHlsManifestRequest, GetSeafileHlsResourceRequest, GetSeafileResourceRequest,
    GetSeafileSubtitleRequest, GetSeafileThumbnailResourceRequest, SeafileHlsManifestResponse,
    SeafileHlsResourceResponse, SeafileResourceResponse, SeafileSubtitleResponse,
    SeafileThumbnailResourceResponse,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct SeafilePlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl SeafilePlaybackProviderGrpcService {
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
impl SeafilePlaybackProviderService for SeafilePlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<SeafileResourceResponse>;
    type GetHlsManifestStream = GrpcResponseStream<SeafileHlsManifestResponse>;
    type GetHlsResourceStream = GrpcResponseStream<SeafileHlsResourceResponse>;
    type GetSubtitleStream = GrpcResponseStream<SeafileSubtitleResponse>;
    type GetThumbnailResourceStream = GrpcResponseStream<SeafileThumbnailResourceResponse>;

    async fn get_resource(
        &self,
        request: Request<GetSeafileResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::seafile::get_seafile_resource(
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
        request: Request<GetSeafileHlsManifestRequest>,
    ) -> Result<Response<Self::GetHlsManifestStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::seafile::get_seafile_hls_manifest(
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
        request: Request<GetSeafileHlsResourceRequest>,
    ) -> Result<Response<Self::GetHlsResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::seafile::get_seafile_hls_resource(
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
        request: Request<GetSeafileSubtitleRequest>,
    ) -> Result<Response<Self::GetSubtitleStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::seafile::get_seafile_subtitle(
                    deps(&state, Some(&control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_thumbnail_resource(
        &self,
        request: Request<GetSeafileThumbnailResourceRequest>,
    ) -> Result<Response<Self::GetThumbnailResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::seafile::get_seafile_thumbnail_resource(
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
) -> synctv_api_common::playback_provider::seafile::SeafilePlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::seafile::SeafilePlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.seafile_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
