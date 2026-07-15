use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::acfun::ac_fun_playback_provider_service_server::AcFunPlaybackProviderService;
use synctv_proto::playback_provider::acfun::{
    AcFunDanmakuEvent, AcFunDanmakuFileResponse, AcFunResourceResponse, AcFunSegmentResponse,
    GetAcFunDanmakuFileRequest, GetAcFunResourceRequest, GetAcFunSegmentRequest,
    WatchAcFunDanmakuRequest,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct AcFunPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<crate::ApiRuntimeSettings>,
}

impl AcFunPlaybackProviderGrpcService {
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
impl AcFunPlaybackProviderService for AcFunPlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<AcFunResourceResponse>;
    type GetSegmentStream = GrpcResponseStream<AcFunSegmentResponse>;
    type GetDanmakuFileStream = GrpcResponseStream<AcFunDanmakuFileResponse>;
    type WatchDanmakuStream = GrpcResponseStream<AcFunDanmakuEvent>;

    async fn get_resource(
        &self,
        request: Request<GetAcFunResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::acfun::get_acfun_resource(
                    acfun_deps(&state, Some(&request_control)),
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
        request: Request<GetAcFunSegmentRequest>,
    ) -> Result<Response<Self::GetSegmentStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::acfun::get_acfun_segment(
                    acfun_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_danmaku_file(
        &self,
        request: Request<GetAcFunDanmakuFileRequest>,
    ) -> Result<Response<Self::GetDanmakuFileStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::acfun::get_acfun_danmaku_file(
                    acfun_deps(&state, Some(&request_control)),
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
        request: Request<WatchAcFunDanmakuRequest>,
    ) -> Result<Response<Self::WatchDanmakuStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::acfun::watch_acfun_danmaku(
                    acfun_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }
}

fn acfun_deps<'a>(
    state: &'a PlaybackProviderGrpcState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> crate::impls::playback_provider::acfun::AcFunPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::acfun::AcFunPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.acfun_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
