use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::huya::huya_playback_provider_service_server::HuyaPlaybackProviderService;
use synctv_proto::playback_provider::huya::{
    GetHuyaResourceRequest, GetHuyaSegmentRequest, HuyaDanmakuEvent, HuyaResourceResponse,
    HuyaSegmentResponse, WatchHuyaDanmakuRequest,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct HuyaPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl HuyaPlaybackProviderGrpcService {
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
impl HuyaPlaybackProviderService for HuyaPlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<HuyaResourceResponse>;
    type GetSegmentStream = GrpcResponseStream<HuyaSegmentResponse>;
    type WatchDanmakuStream = GrpcResponseStream<HuyaDanmakuEvent>;

    async fn get_resource(
        &self,
        request: Request<GetHuyaResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::huya::get_huya_resource(
                    huya_deps(&state, Some(&request_control)),
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
        request: Request<GetHuyaSegmentRequest>,
    ) -> Result<Response<Self::GetSegmentStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::huya::get_huya_segment(
                    huya_deps(&state, Some(&request_control)),
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
        request: Request<WatchHuyaDanmakuRequest>,
    ) -> Result<Response<Self::WatchDanmakuStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::huya::watch_huya_danmaku(
                    huya_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }
}

fn huya_deps<'a>(
    state: &'a PlaybackProviderGrpcState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::huya::HuyaPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::huya::HuyaPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.huya_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
