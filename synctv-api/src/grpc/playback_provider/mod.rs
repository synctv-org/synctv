use std::pin::Pin;
use std::sync::Arc;

use futures::{Stream, TryStreamExt};
use tonic::{Request, Response, Status};

use crate::api_runtime::SharedApiRuntime;
use crate::impls::{ApiError, EndpointRateLimitCategory};

pub(crate) mod alist;
pub(crate) mod bilibili;
pub(crate) mod cctv;
pub(crate) mod direct_url;
pub(crate) mod douyin;
pub(crate) mod douyu;
pub(crate) mod emby;
pub(crate) mod fnos;
pub(crate) mod huya;
pub(crate) mod live_proxy;
pub(crate) mod nextcloud;
pub(crate) mod qnap;
pub(crate) mod rtmp;
pub(crate) mod seafile;
pub(crate) mod synology;
pub(crate) mod tiktok;
pub(crate) mod truenas;
pub(crate) mod twitch;
pub(crate) mod youtube;

pub(crate) fn playback_provider_api_runtime(
    state: &PlaybackProviderGrpcState,
) -> crate::impls::playback_provider::common::PlaybackProviderApiRuntime<'_> {
    crate::impls::playback_provider::common::PlaybackProviderApiRuntime {
        proxy_signing_key: &state.shared_api_runtime.proxy_signing_key,
        public_id_codec: &state.shared_api_runtime.public_id_codec,
        provider_stores: state.shared_api_runtime.provider_stores.as_ref(),
        user_service: &state.shared_api_runtime.client_api.user_service,
        playback_transport_services: &state.shared_api_runtime.playback_transport_services,
        proxy_http_client: &state.proxy_http_client,
        ssrf_guard: &state.ssrf_guard,
        proxy_slice_cache: &state.proxy_slice_cache,
    }
}

pub(crate) fn playback_provider_identity_runtime(
    state: &PlaybackProviderGrpcState,
) -> crate::impls::playback_provider::common::PlaybackProviderIdentityRuntime<'_> {
    crate::impls::playback_provider::common::PlaybackProviderIdentityRuntime {
        public_id_codec: &state.shared_api_runtime.public_id_codec,
    }
}

pub(crate) fn live_playback_api_runtime(
    state: &PlaybackProviderGrpcState,
) -> crate::impls::playback_provider::common::LivePlaybackApiRuntime<'_> {
    crate::impls::playback_provider::common::LivePlaybackApiRuntime {
        proxy_signing_key: &state.shared_api_runtime.proxy_signing_key,
        live_streaming_infrastructure: state.live_streaming_infrastructure.as_ref(),
        connection_runtime: state.connection_manager.as_ref(),
        livestream_config: &state.runtime_settings.livestream,
        runtime_settings_store: state.runtime_settings_store.as_deref(),
    }
}

pub type GrpcResponseStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>;

#[derive(Clone)]
pub(crate) struct PlaybackProviderGrpcState {
    pub shared_api_runtime: Arc<SharedApiRuntime>,
    pub runtime_settings: Arc<crate::ApiRuntimeSettings>,
    pub connection_manager: Arc<dyn synctv_realtime::sync::ConnectionRuntime>,
    pub runtime_settings_store: Option<Arc<synctv_core::service::RuntimeSettingsStore>>,
    pub live_streaming_infrastructure: Option<Arc<synctv_livestream::LiveStreamingInfrastructure>>,
    pub proxy_slice_cache: Arc<synctv_proxy::slice_cache::SliceCache>,
    pub ssrf_guard: synctv_common::ssrf::SsrfGuard,
    pub proxy_http_client: reqwest::Client,
}

pub(crate) async fn execute_playback_provider_stream<T, S>(
    state: Arc<PlaybackProviderGrpcState>,
    metadata: crate::impls::RequestMetadata,
    operation: impl FnOnce(
            synctv_core::provider::ExecutionControl,
        ) -> futures::future::BoxFuture<'static, Result<S, ApiError>>
        + Send
        + 'static,
) -> Result<Response<GrpcResponseStream<T>>, Status>
where
    T: Send + 'static,
    S: Stream<Item = Result<T, ApiError>> + Send + 'static,
{
    let stream = state
        .shared_api_runtime
        .request_executor
        .execute_public_with_control(
            &metadata,
            EndpointRateLimitCategory::Streaming,
            move |control| async move { operation(control).await },
        )
        .await
        .map_err(crate::grpc::map_api_error)?;
    Ok(Response::new(Box::pin(
        stream.map_err(crate::grpc::map_api_error),
    )))
}

pub(crate) fn grpc_request_metadata<T>(
    request: &Request<T>,
    runtime_settings: &crate::ApiRuntimeSettings,
) -> Result<crate::impls::RequestMetadata, Status> {
    crate::grpc::request_metadata(request, runtime_settings, None)
}
pub(crate) mod acfun;
