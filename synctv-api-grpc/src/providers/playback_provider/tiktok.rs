use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::tiktok::tik_tok_playback_provider_service_server::TikTokPlaybackProviderService;
use synctv_proto::playback_provider::tiktok::{
    GetTikTokHlsResourceRequest, GetTikTokResourceRequest, GetTikTokSubtitleRequest,
    TikTokHlsResourceResponse, TikTokResourceResponse, TikTokSubtitleResponse,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct TikTokPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl TikTokPlaybackProviderGrpcService {
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
impl TikTokPlaybackProviderService for TikTokPlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<TikTokResourceResponse>;
    type GetHlsResourceStream = GrpcResponseStream<TikTokHlsResourceResponse>;
    type GetSubtitleStream = GrpcResponseStream<TikTokSubtitleResponse>;

    async fn get_resource(
        &self,
        request: Request<GetTikTokResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::tiktok::get_tiktok_resource(
                    tiktok_deps(&state, Some(&request_control)),
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
        request: Request<GetTikTokHlsResourceRequest>,
    ) -> Result<Response<Self::GetHlsResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::tiktok::get_tiktok_hls_resource(
                    tiktok_deps(&state, Some(&request_control)),
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
        request: Request<GetTikTokSubtitleRequest>,
    ) -> Result<Response<Self::GetSubtitleStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::tiktok::get_tiktok_subtitle(
                    tiktok_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }
}

fn tiktok_deps<'a>(
    state: &'a PlaybackProviderGrpcState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::tiktok::TikTokPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::tiktok::TikTokPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.tiktok_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
