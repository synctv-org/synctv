use std::pin::Pin;
use std::sync::Arc;

use futures::{Stream, TryStreamExt};
use tonic::{Request, Response, Status};

use crate::http::AppState;
use crate::impls::{ApiError, EndpointRateLimitCategory};

pub mod alist;
pub mod bilibili;
pub mod direct_url;
pub mod emby;
pub mod live_proxy;
pub mod rtmp;

pub type GrpcResponseStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>;

pub(crate) async fn execute_playback_provider_stream<T, S>(
    state: Arc<AppState>,
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
    config: &synctv_core::Config,
) -> Result<crate::impls::RequestMetadata, Status> {
    crate::grpc::request_metadata(request, config, None)
}
