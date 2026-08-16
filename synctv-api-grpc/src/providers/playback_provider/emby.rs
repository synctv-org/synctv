use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::emby::emby_playback_provider_service_server::EmbyPlaybackProviderService;
use synctv_proto::playback_provider::emby::{
    EmbyHlsManifestResponse, EmbyHlsResourceResponse, EmbyMediaStreamResponse,
    EmbySubtitleResponse, EmbyThumbnailResourceResponse, GetEmbyHlsManifestRequest,
    GetEmbyHlsResourceRequest, GetEmbyMediaStreamRequest, GetEmbySubtitleRequest,
    GetEmbyThumbnailResourceRequest,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct EmbyPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl EmbyPlaybackProviderGrpcService {
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
impl EmbyPlaybackProviderService for EmbyPlaybackProviderGrpcService {
    type GetMediaStreamStream = GrpcResponseStream<EmbyMediaStreamResponse>;
    type GetHlsManifestStream = GrpcResponseStream<EmbyHlsManifestResponse>;
    type GetHlsResourceStream = GrpcResponseStream<EmbyHlsResourceResponse>;
    type GetSubtitleStream = GrpcResponseStream<EmbySubtitleResponse>;
    type GetThumbnailResourceStream = GrpcResponseStream<EmbyThumbnailResourceResponse>;

    async fn get_media_stream(
        &self,
        request: Request<GetEmbyMediaStreamRequest>,
    ) -> Result<Response<Self::GetMediaStreamStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::emby::get_emby_media_stream(
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
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::emby::get_emby_hls_manifest(
                    emby_deps(&state, Some(&request_control)),
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
        request: Request<GetEmbyHlsResourceRequest>,
    ) -> Result<Response<Self::GetHlsResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::emby::get_emby_hls_resource(
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
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::emby::get_emby_subtitle(
                    emby_deps(&state, Some(&request_control)),
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
        request: Request<GetEmbyThumbnailResourceRequest>,
    ) -> Result<Response<Self::GetThumbnailResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::emby::get_emby_thumbnail_resource(
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
    state: &'a PlaybackProviderGrpcState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::emby::EmbyPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::emby::EmbyPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.emby_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
