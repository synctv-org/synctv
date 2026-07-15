use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::douyu::douyu_playback_provider_service_server::DouyuPlaybackProviderService;
use synctv_proto::playback_provider::douyu::{
    DouyuDanmakuEvent, DouyuResourceResponse, DouyuSegmentResponse, GetDouyuResourceRequest,
    GetDouyuSegmentRequest, WatchDouyuDanmakuRequest,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct DouyuPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<crate::ApiRuntimeSettings>,
}

impl DouyuPlaybackProviderGrpcService {
    #[must_use]
    pub fn new(
        state: Arc<PlaybackProviderGrpcState>,
        runtime_settings: Arc<crate::ApiRuntimeSettings>,
    ) -> Self {
        Self {
            state,
            runtime_settings,
        }
    }
}

#[tonic::async_trait]
#[allow(clippy::result_large_err)]
impl DouyuPlaybackProviderService for DouyuPlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<DouyuResourceResponse>;
    type GetSegmentStream = GrpcResponseStream<DouyuSegmentResponse>;
    type WatchDanmakuStream = GrpcResponseStream<DouyuDanmakuEvent>;

    async fn get_resource(
        &self,
        request: Request<GetDouyuResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::douyu::get_douyu_resource(
                    douyu_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_segment(
        &self,
        request: Request<GetDouyuSegmentRequest>,
    ) -> Result<Response<Self::GetSegmentStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::douyu::get_douyu_segment(
                    douyu_deps(&state, Some(&request_control)),
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
        request: Request<WatchDouyuDanmakuRequest>,
    ) -> Result<Response<Self::WatchDanmakuStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::douyu::watch_douyu_danmaku(
                    douyu_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }
}

fn douyu_deps<'a>(
    state: &'a PlaybackProviderGrpcState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> crate::impls::playback_provider::douyu::DouyuPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::douyu::DouyuPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.douyu_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
