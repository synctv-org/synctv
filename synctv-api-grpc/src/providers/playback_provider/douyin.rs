use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::douyin::douyin_playback_provider_service_server::DouyinPlaybackProviderService;
use synctv_proto::playback_provider::douyin::{
    DouyinDanmakuEvent, DouyinHlsResourceResponse, DouyinResourceResponse,
    GetDouyinHlsResourceRequest, GetDouyinResourceRequest, WatchDouyinDanmakuRequest,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct DouyinPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl DouyinPlaybackProviderGrpcService {
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
impl DouyinPlaybackProviderService for DouyinPlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<DouyinResourceResponse>;
    type GetHlsResourceStream = GrpcResponseStream<DouyinHlsResourceResponse>;
    type WatchDanmakuStream = GrpcResponseStream<DouyinDanmakuEvent>;

    async fn get_resource(
        &self,
        request: Request<GetDouyinResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::douyin::get_douyin_resource(
                    deps(&state, Some(&request_control)),
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
        request: Request<GetDouyinHlsResourceRequest>,
    ) -> Result<Response<Self::GetHlsResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::douyin::get_douyin_hls_resource(
                    deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn watch_danmaku(
        &self,
        request: Request<WatchDouyinDanmakuRequest>,
    ) -> Result<Response<Self::WatchDanmakuStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::douyin::watch_douyin_danmaku(
                    deps(&state, Some(&request_control)),
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
) -> synctv_api_common::playback_provider::douyin::DouyinPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::douyin::DouyinPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.douyin_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
