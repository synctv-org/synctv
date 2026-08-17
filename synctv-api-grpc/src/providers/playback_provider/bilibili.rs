use std::sync::Arc;

use futures::{FutureExt, TryStreamExt};
use synctv_proto::playback_provider::bilibili::bilibili_playback_provider_service_server::BilibiliPlaybackProviderService;
use synctv_proto::playback_provider::bilibili::{
    BilibiliDanmakuFileResponse, BilibiliDashManifestResponse, BilibiliDashResourceResponse,
    BilibiliHlsManifestResponse, BilibiliHlsResourceResponse, BilibiliLiveDanmakuEvent,
    BilibiliMediaStreamResponse, BilibiliSubtitleResponse, GetBilibiliDanmakuFileRequest,
    GetBilibiliDashManifestRequest, GetBilibiliDashResourceRequest, GetBilibiliHlsManifestRequest,
    GetBilibiliHlsResourceRequest, GetBilibiliMediaStreamRequest, GetBilibiliSubtitleRequest,
    WatchBilibiliLiveDanmakuRequest,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct BilibiliPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl BilibiliPlaybackProviderGrpcService {
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
impl BilibiliPlaybackProviderService for BilibiliPlaybackProviderGrpcService {
    type GetMediaStreamStream = GrpcResponseStream<BilibiliMediaStreamResponse>;
    type GetHlsManifestStream = GrpcResponseStream<BilibiliHlsManifestResponse>;
    type GetHlsResourceStream = GrpcResponseStream<BilibiliHlsResourceResponse>;
    type GetDashManifestStream = GrpcResponseStream<BilibiliDashManifestResponse>;
    type GetDashResourceStream = GrpcResponseStream<BilibiliDashResourceResponse>;
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
                synctv_api_common::playback_provider::bilibili::get_bilibili_media_stream(
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
                synctv_api_common::playback_provider::bilibili::get_bilibili_hls_manifest(
                    bilibili_deps(&state, Some(&request_control)),
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
        request: Request<GetBilibiliHlsResourceRequest>,
    ) -> Result<Response<Self::GetHlsResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::bilibili::get_bilibili_hls_resource(
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
                synctv_api_common::playback_provider::bilibili::get_bilibili_dash_manifest(
                    bilibili_deps(&state, Some(&request_control)),
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
        request: Request<GetBilibiliDashResourceRequest>,
    ) -> Result<Response<Self::GetDashResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();

        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::bilibili::get_bilibili_dash_resource(
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
                synctv_api_common::playback_provider::bilibili::get_bilibili_subtitle(
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
                synctv_api_common::playback_provider::bilibili::get_bilibili_danmaku_file(
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
        let (metadata, public_room_id) =
            super::grpc_room_request_context(&self.state, &request, &self.runtime_settings)?;
        let req = request.into_inner();
        let stream =
            synctv_api_common::impls::ClientApiImpl::execute_room_actor_endpoint_with_control(
                self.state.shared_api_runtime.client_api.clone(),
                &metadata,
                public_room_id,
                synctv_api_common::impls::EndpointRateLimitCategory::Streaming,
                move |client_api, request_control, actor| async move {
                    client_api
                        .watch_bilibili_live_danmaku_for_actor(&actor, req, Some(&request_control))
                        .await
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
) -> synctv_api_common::playback_provider::bilibili::BilibiliPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::bilibili::BilibiliPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.bilibili_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
