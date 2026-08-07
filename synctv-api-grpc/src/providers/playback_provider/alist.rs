use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::alist::alist_playback_provider_service_server::AlistPlaybackProviderService;
use synctv_proto::playback_provider::alist::{
    AlistFileStreamResponse, AlistSubtitleResponse, AlistThumbnailResponse,
    AlistTranscodedHlsManifestResponse, AlistTranscodedHlsResourceResponse,
    GetAlistFileStreamRequest, GetAlistSubtitleRequest, GetAlistThumbnailRequest,
    GetAlistTranscodedHlsManifestRequest, GetAlistTranscodedHlsResourceRequest,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct AlistPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl AlistPlaybackProviderGrpcService {
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
impl AlistPlaybackProviderService for AlistPlaybackProviderGrpcService {
    type GetFileStreamStream = GrpcResponseStream<AlistFileStreamResponse>;
    type GetTranscodedHlsManifestStream = GrpcResponseStream<AlistTranscodedHlsManifestResponse>;
    type GetTranscodedHlsResourceStream = GrpcResponseStream<AlistTranscodedHlsResourceResponse>;
    type GetSubtitleStream = GrpcResponseStream<AlistSubtitleResponse>;
    type GetThumbnailStream = GrpcResponseStream<AlistThumbnailResponse>;

    async fn get_file_stream(
        &self,
        request: Request<GetAlistFileStreamRequest>,
    ) -> Result<Response<Self::GetFileStreamStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::alist::get_alist_file_stream(
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
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::alist::get_alist_transcoded_hls_manifest(
                    alist_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_transcoded_hls_resource(
        &self,
        request: Request<GetAlistTranscodedHlsResourceRequest>,
    ) -> Result<Response<Self::GetTranscodedHlsResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        let state_for_stream = state.clone();
        execute_playback_provider_stream(state, metadata, move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::alist::get_alist_transcoded_hls_resource(
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
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        let state_for_stream = state.clone();
        execute_playback_provider_stream(state, metadata, move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::alist::get_alist_subtitle(
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
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        let state_for_stream = state.clone();
        execute_playback_provider_stream(state, metadata, move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::alist::get_alist_thumbnail(
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
    state: &'a PlaybackProviderGrpcState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::alist::AlistPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::alist::AlistPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.alist_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
