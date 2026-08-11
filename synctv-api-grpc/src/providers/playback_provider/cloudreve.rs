use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};
use futures::FutureExt;
use std::sync::Arc;
use synctv_proto::playback_provider::cloudreve::cloudreve_playback_provider_service_server::CloudrevePlaybackProviderService;
use synctv_proto::playback_provider::cloudreve::{
    CloudreveHlsManifestResponse, CloudreveHlsResourceResponse, CloudreveResourceResponse,
    CloudreveSubtitleResponse, GetCloudreveHlsManifestRequest, GetCloudreveHlsResourceRequest,
    GetCloudreveResourceRequest, GetCloudreveSubtitleRequest,
};
use tonic::{Request, Response, Status};

#[derive(Clone)]
pub struct CloudrevePlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl CloudrevePlaybackProviderGrpcService {
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
impl CloudrevePlaybackProviderService for CloudrevePlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<CloudreveResourceResponse>;
    type GetHlsManifestStream = GrpcResponseStream<CloudreveHlsManifestResponse>;
    type GetHlsResourceStream = GrpcResponseStream<CloudreveHlsResourceResponse>;
    type GetSubtitleStream = GrpcResponseStream<CloudreveSubtitleResponse>;

    async fn get_resource(
        &self,
        request: Request<GetCloudreveResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::cloudreve::get_cloudreve_resource(
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
        request: Request<GetCloudreveHlsManifestRequest>,
    ) -> Result<Response<Self::GetHlsManifestStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::cloudreve::get_cloudreve_hls_manifest(
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
        request: Request<GetCloudreveHlsResourceRequest>,
    ) -> Result<Response<Self::GetHlsResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::cloudreve::get_cloudreve_hls_resource(
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
        request: Request<GetCloudreveSubtitleRequest>,
    ) -> Result<Response<Self::GetSubtitleStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::cloudreve::get_cloudreve_subtitle(
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
) -> synctv_api_common::playback_provider::cloudreve::CloudrevePlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::cloudreve::CloudrevePlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.cloudreve_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
