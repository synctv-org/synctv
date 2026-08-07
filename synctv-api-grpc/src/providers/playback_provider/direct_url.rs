use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::direct_url::direct_url_playback_provider_service_server::DirectUrlPlaybackProviderService;
use synctv_proto::playback_provider::direct_url::{
    DirectUrlDashManifestResponse, DirectUrlDashResourceResponse, DirectUrlHlsManifestResponse,
    DirectUrlHlsResourceResponse, DirectUrlStreamResponse, DirectUrlSubtitleResponse,
    GetDirectUrlDashManifestRequest, GetDirectUrlDashResourceRequest,
    GetDirectUrlHlsManifestRequest, GetDirectUrlHlsResourceRequest, GetDirectUrlStreamRequest,
    GetDirectUrlSubtitleRequest,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct DirectUrlPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl DirectUrlPlaybackProviderGrpcService {
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
impl DirectUrlPlaybackProviderService for DirectUrlPlaybackProviderGrpcService {
    type GetStreamStream = GrpcResponseStream<DirectUrlStreamResponse>;
    type GetHlsManifestStream = GrpcResponseStream<DirectUrlHlsManifestResponse>;
    type GetHlsResourceStream = GrpcResponseStream<DirectUrlHlsResourceResponse>;
    type GetDashManifestStream = GrpcResponseStream<DirectUrlDashManifestResponse>;
    type GetDashResourceStream = GrpcResponseStream<DirectUrlDashResourceResponse>;
    type GetSubtitleStream = GrpcResponseStream<DirectUrlSubtitleResponse>;

    async fn get_stream(
        &self,
        request: Request<GetDirectUrlStreamRequest>,
    ) -> Result<Response<Self::GetStreamStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::direct_url::get_direct_url_stream(
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
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::direct_url::get_direct_url_hls_manifest(
                    direct_url_deps(&state, Some(&request_control)),
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
        request: Request<GetDirectUrlHlsResourceRequest>,
    ) -> Result<Response<Self::GetHlsResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::direct_url::get_direct_url_hls_resource(
                    direct_url_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_dash_manifest(
        &self,
        request: Request<GetDirectUrlDashManifestRequest>,
    ) -> Result<Response<Self::GetDashManifestStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::direct_url::get_direct_url_dash_manifest(
                    direct_url_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_dash_resource(
        &self,
        request: Request<GetDirectUrlDashResourceRequest>,
    ) -> Result<Response<Self::GetDashResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::direct_url::get_direct_url_dash_resource(
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
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::direct_url::get_direct_url_subtitle(
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
    state: &'a PlaybackProviderGrpcState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::direct_url::DirectUrlPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::direct_url::DirectUrlPlaybackProviderDeps {
        playback_provider_service: &state
            .shared_api_runtime
            .direct_url_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
