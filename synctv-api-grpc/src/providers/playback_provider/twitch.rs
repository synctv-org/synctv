use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::twitch::twitch_playback_provider_service_server::TwitchPlaybackProviderService;
use synctv_proto::playback_provider::twitch::{
    GetTwitchResourceRequest, GetTwitchSegmentRequest, TwitchChatEvent, TwitchResourceResponse,
    TwitchSegmentResponse, WatchTwitchChatRequest,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct TwitchPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<synctv_api_common::ApiRuntimeSettings>,
}

impl TwitchPlaybackProviderGrpcService {
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
impl TwitchPlaybackProviderService for TwitchPlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<TwitchResourceResponse>;
    type GetSegmentStream = GrpcResponseStream<TwitchSegmentResponse>;
    type WatchChatStream = GrpcResponseStream<TwitchChatEvent>;

    async fn get_resource(
        &self,
        request: Request<GetTwitchResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::twitch::get_twitch_resource(
                    twitch_deps(&state, Some(&request_control)),
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
        request: Request<GetTwitchSegmentRequest>,
    ) -> Result<Response<Self::GetSegmentStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::twitch::get_twitch_segment(
                    twitch_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn watch_chat(
        &self,
        request: Request<WatchTwitchChatRequest>,
    ) -> Result<Response<Self::WatchChatStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |request_control| {
            let state = state.clone();
            async move {
                synctv_api_common::playback_provider::twitch::watch_twitch_chat(
                    twitch_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }
}

fn twitch_deps<'a>(
    state: &'a PlaybackProviderGrpcState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::twitch::TwitchPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::twitch::TwitchPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.twitch_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
