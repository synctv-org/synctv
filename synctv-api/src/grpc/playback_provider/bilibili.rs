use std::sync::Arc;

use futures::{FutureExt, TryStreamExt};
use synctv_proto::playback_provider::bilibili::bilibili_playback_provider_service_server::BilibiliPlaybackProviderService;
use synctv_proto::playback_provider::bilibili::{
    BilibiliDanmakuFileResponse, BilibiliDashManifestResponse, BilibiliDashSegmentResponse,
    BilibiliHlsManifestResponse, BilibiliHlsSegmentResponse, BilibiliLiveDanmakuEvent,
    BilibiliMediaStreamResponse, BilibiliSubtitleResponse, GetBilibiliDanmakuFileRequest,
    GetBilibiliDashManifestRequest, GetBilibiliDashSegmentRequest, GetBilibiliHlsManifestRequest,
    GetBilibiliHlsSegmentRequest, GetBilibiliMediaStreamRequest, GetBilibiliSubtitleRequest,
    WatchBilibiliLiveDanmakuRequest,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};
use crate::impls::EndpointRateLimitCategory;

#[derive(Clone)]
pub struct BilibiliPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<crate::ApiRuntimeSettings>,
}

impl BilibiliPlaybackProviderGrpcService {
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
impl BilibiliPlaybackProviderService for BilibiliPlaybackProviderGrpcService {
    type GetMediaStreamStream = GrpcResponseStream<BilibiliMediaStreamResponse>;
    type GetHlsManifestStream = GrpcResponseStream<BilibiliHlsManifestResponse>;
    type GetHlsSegmentStream = GrpcResponseStream<BilibiliHlsSegmentResponse>;
    type GetDashManifestStream = GrpcResponseStream<BilibiliDashManifestResponse>;
    type GetDashSegmentStream = GrpcResponseStream<BilibiliDashSegmentResponse>;
    type GetSubtitleStream = GrpcResponseStream<BilibiliSubtitleResponse>;
    type GetDanmakuFileStream = GrpcResponseStream<BilibiliDanmakuFileResponse>;
    type WatchLiveDanmakuStream = GrpcResponseStream<BilibiliLiveDanmakuEvent>;

    async fn get_media_stream(
        &self,
        request: Request<GetBilibiliMediaStreamRequest>,
    ) -> Result<Response<Self::GetMediaStreamStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::bilibili::get_bilibili_media_stream(
                    bilibili_deps(&state, Some(&request_control)),
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
        request: Request<GetBilibiliHlsManifestRequest>,
    ) -> Result<Response<Self::GetHlsManifestStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::bilibili::get_bilibili_hls_manifest(
                    bilibili_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_hls_segment(
        &self,
        request: Request<GetBilibiliHlsSegmentRequest>,
    ) -> Result<Response<Self::GetHlsSegmentStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::bilibili::get_bilibili_hls_segment(
                    bilibili_deps(&state, Some(&request_control)),
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
        request: Request<GetBilibiliDashManifestRequest>,
    ) -> Result<Response<Self::GetDashManifestStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::bilibili::get_bilibili_dash_manifest(
                    bilibili_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_dash_segment(
        &self,
        request: Request<GetBilibiliDashSegmentRequest>,
    ) -> Result<Response<Self::GetDashSegmentStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::bilibili::get_bilibili_dash_segment(
                    bilibili_deps(&state, Some(&request_control)),
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
        request: Request<GetBilibiliSubtitleRequest>,
    ) -> Result<Response<Self::GetSubtitleStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::bilibili::get_bilibili_subtitle(
                    bilibili_deps(&state, Some(&request_control)),
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
        request: Request<GetBilibiliDanmakuFileRequest>,
    ) -> Result<Response<Self::GetDanmakuFileStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::bilibili::get_bilibili_danmaku_file(
                    bilibili_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn watch_live_danmaku(
        &self,
        request: Request<WatchBilibiliLiveDanmakuRequest>,
    ) -> Result<Response<Self::WatchLiveDanmakuStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        let state_for_stream = state.clone();

        let stream = state
            .shared_api_runtime
            .request_executor
            .execute_user_with_control(
                &metadata,
                EndpointRateLimitCategory::Streaming,
                move |request_control, authenticated| {
                    let state = state_for_stream;
                    async move {
                        crate::impls::playback_provider::bilibili::watch_bilibili_live_danmaku(
                            crate::impls::playback_provider::bilibili::BilibiliLiveDanmakuDeps {
                                playback_provider_service: &state
                                    .shared_api_runtime
                                    .bilibili_playback_provider_service,
                                identity_runtime: super::playback_provider_identity_runtime(&state),
                                actor_user_id: authenticated.user_id,
                                request_control: Some(&request_control),
                            },
                            req,
                        )
                        .await
                    }
                },
            )
            .await
            .map_err(crate::grpc::map_api_error)?;
        Ok(Response::new(Box::pin(
            stream.map_err(crate::grpc::map_api_error),
        )))
    }
}

fn bilibili_deps<'a>(
    state: &'a PlaybackProviderGrpcState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> crate::impls::playback_provider::bilibili::BilibiliPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::bilibili::BilibiliPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.bilibili_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
