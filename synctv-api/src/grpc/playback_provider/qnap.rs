use std::sync::Arc;

use futures::FutureExt;
use synctv_proto::playback_provider::qnap::qnap_playback_provider_service_server::QnapPlaybackProviderService;
use synctv_proto::playback_provider::qnap::{
    GetQnapResourceRequest, GetQnapSubtitleRequest, GetQnapThumbnailRequest, QnapResourceResponse,
    QnapSubtitleResponse, QnapThumbnailResponse,
};
use tonic::{Request, Response, Status};

use super::{
    execute_playback_provider_stream, grpc_request_metadata, GrpcResponseStream,
    PlaybackProviderGrpcState,
};

#[derive(Clone)]
pub struct QnapPlaybackProviderGrpcService {
    state: Arc<PlaybackProviderGrpcState>,
    runtime_settings: Arc<crate::ApiRuntimeSettings>,
}

impl QnapPlaybackProviderGrpcService {
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
impl QnapPlaybackProviderService for QnapPlaybackProviderGrpcService {
    type GetResourceStream = GrpcResponseStream<QnapResourceResponse>;
    type GetSubtitleStream = GrpcResponseStream<QnapSubtitleResponse>;
    type GetThumbnailStream = GrpcResponseStream<QnapThumbnailResponse>;

    async fn get_resource(
        &self,
        request: Request<GetQnapResourceRequest>,
    ) -> Result<Response<Self::GetResourceStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::qnap::get_qnap_resource(
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
        request: Request<GetQnapSubtitleRequest>,
    ) -> Result<Response<Self::GetSubtitleStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::qnap::get_qnap_subtitle(
                    deps(&state, Some(&control)),
                    req,
                )
                .await
            }
            .boxed()
        })
        .await
    }

    async fn get_thumbnail(
        &self,
        request: Request<GetQnapThumbnailRequest>,
    ) -> Result<Response<Self::GetThumbnailStream>, Status> {
        let metadata = grpc_request_metadata(&request, &self.runtime_settings)?;
        let req = request.into_inner();
        let state = self.state.clone();
        execute_playback_provider_stream(state.clone(), metadata, move |control| {
            let state = state.clone();
            async move {
                crate::impls::playback_provider::qnap::get_qnap_thumbnail(
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
) -> crate::impls::playback_provider::qnap::QnapPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::qnap::QnapPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.qnap_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
