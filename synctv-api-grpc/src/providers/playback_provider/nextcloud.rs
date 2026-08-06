use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};
use futures::FutureExt;
use std::sync::Arc;
use synctv_proto::playback_provider::nextcloud::nextcloud_playback_provider_service_server::NextcloudPlaybackProviderService;
use synctv_proto::playback_provider::nextcloud::{
    GetNextcloudHlsManifestRequest, GetNextcloudHlsResourceRequest, GetNextcloudResourceRequest,
    GetNextcloudSubtitleRequest, NextcloudHlsManifestResponse, NextcloudHlsResourceResponse,
    NextcloudResourceResponse, NextcloudSubtitleResponse,
};
use tonic::{Request, Response, Status};

#[derive(Clone)]
pub struct NextcloudPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl NextcloudPlaybackProviderGrpcService {
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
impl NextcloudPlaybackProviderService for NextcloudPlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<NextcloudResourceResponse>;
    type GetHlsManifestStream = GrpcResponseStream<NextcloudHlsManifestResponse>;
    type GetHlsResourceStream = GrpcResponseStream<NextcloudHlsResourceResponse>;
    type GetSubtitleStream = GrpcResponseStream<NextcloudSubtitleResponse>;
    async fn get_resource(
        &self,
        request: Request<GetNextcloudResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::nextcloud::get_nextcloud_resource(
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
        request: Request<GetNextcloudHlsManifestRequest>,
    ) -> Result<Response<Self::GetHlsManifestStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::nextcloud::get_nextcloud_hls_manifest(
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
        request: Request<GetNextcloudHlsResourceRequest>,
    ) -> Result<Response<Self::GetHlsResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::nextcloud::get_nextcloud_hls_resource(
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
        request: Request<GetNextcloudSubtitleRequest>,
    ) -> Result<Response<Self::GetSubtitleStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::nextcloud::get_nextcloud_subtitle(
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
) -> synctv_api_common::playback_provider::nextcloud::NextcloudPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::nextcloud::NextcloudPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.nextcloud_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
