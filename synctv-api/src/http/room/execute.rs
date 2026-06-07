use futures::future::BoxFuture;
use futures::FutureExt;
use std::future::Future;
use synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT;

use super::{AppState, RequestMetadata};
use crate::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};

pub(in crate::http::room) fn request_metadata(
    request_meta: RequestMetadata,
) -> crate::impls::RequestMetadata {
    request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT))
}

pub(in crate::http::room) fn execute_public_endpoint<'a, T, F, Fut>(
    state: &'a AppState,
    request_meta: RequestMetadata,
    category: EndpointRateLimitCategory,
    scope: EndpointRateLimitScope,
    operation: F,
) -> BoxFuture<'a, Result<T, super::super::AppError>>
where
    T: Send + 'a,
    F: FnOnce(std::sync::Arc<crate::impls::ClientApiImpl>) -> Fut + Send + 'a,
    Fut: Future<Output = Result<T, crate::impls::ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_metadata(request_meta);
        let executor = state.shared_api_runtime.client_api.clone();
        let client_api = state.shared_api_runtime.client_api.clone();
        executor
            .execute_scoped_public_endpoint(&request_meta, category, scope, move || {
                operation(client_api)
            })
            .await
            .map_err(super::super::error::map_api_error)
    }
    .boxed()
}

pub(in crate::http::room) fn execute_user_endpoint<'a, T, F, Fut>(
    state: &'a AppState,
    request_meta: RequestMetadata,
    category: EndpointRateLimitCategory,
    scope: EndpointRateLimitScope,
    operation: F,
) -> BoxFuture<'a, Result<T, super::super::AppError>>
where
    T: Send + 'a,
    F: FnOnce(
            std::sync::Arc<crate::impls::ClientApiImpl>,
            synctv_core::service::AuthenticatedToken,
        ) -> Fut
        + Send
        + 'a,
    Fut: Future<Output = Result<T, crate::impls::ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_metadata(request_meta);
        let executor = state.shared_api_runtime.client_api.clone();
        let client_api = state.shared_api_runtime.client_api.clone();
        executor
            .execute_scoped_user_endpoint(&request_meta, category, scope, move |authenticated| {
                operation(client_api, authenticated)
            })
            .await
            .map_err(super::super::error::map_api_error)
    }
    .boxed()
}

pub(crate) fn execute_room_actor_endpoint<'a, T, F, Fut>(
    state: &'a AppState,
    request_meta: RequestMetadata,
    public_room_id: String,
    category: EndpointRateLimitCategory,
    scope: EndpointRateLimitScope,
    operation: F,
) -> BoxFuture<'a, Result<T, super::super::AppError>>
where
    T: Send + 'a,
    F: FnOnce(std::sync::Arc<crate::impls::ClientApiImpl>, crate::impls::client::RoomActor) -> Fut
        + Send
        + 'a,
    Fut: Future<Output = Result<T, crate::impls::ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_metadata(request_meta);
        let client_api = state.shared_api_runtime.client_api.clone();
        crate::impls::ClientApiImpl::execute_scoped_room_actor_endpoint(
            client_api,
            &request_meta,
            public_room_id,
            category,
            scope,
            operation,
        )
        .await
        .map_err(super::super::error::map_api_error)
    }
    .boxed()
}

pub(in crate::http::room) fn execute_room_actor_endpoint_with_control<'a, T, F, Fut>(
    state: &'a AppState,
    request_meta: RequestMetadata,
    public_room_id: String,
    category: EndpointRateLimitCategory,
    scope: EndpointRateLimitScope,
    operation: F,
) -> BoxFuture<'a, Result<T, super::super::AppError>>
where
    T: Send + 'a,
    F: FnOnce(
            std::sync::Arc<crate::impls::ClientApiImpl>,
            synctv_core::provider::ExecutionControl,
            crate::impls::client::RoomActor,
        ) -> Fut
        + Send
        + 'a,
    Fut: Future<Output = Result<T, crate::impls::ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_metadata(request_meta);
        let client_api = state.shared_api_runtime.client_api.clone();
        crate::impls::ClientApiImpl::execute_scoped_room_actor_endpoint_with_control(
            client_api,
            &request_meta,
            public_room_id,
            category,
            scope,
            operation,
        )
        .await
        .map_err(super::super::error::map_api_error)
    }
    .boxed()
}
