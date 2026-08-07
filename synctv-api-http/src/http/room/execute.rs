use futures::future::BoxFuture;
use futures::FutureExt;
use std::future::Future;

use super::{AppState, RequestMetadata};
use synctv_api_common::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};

pub(in crate::http::room) fn request_metadata(
    request_meta: RequestMetadata,
) -> synctv_api_common::impls::RequestMetadata {
    request_meta.0
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
    F: FnOnce(std::sync::Arc<synctv_api_common::impls::ClientApiImpl>) -> Fut + Send + 'a,
    Fut: Future<Output = Result<T, synctv_api_common::impls::ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_metadata(request_meta);
        let client_api = state.shared_api_runtime.client_api.clone();
        let client_api_clone = client_api.clone();
        client_api
            .execute_scoped_public_endpoint(&request_meta, category, scope, move || {
                operation(client_api_clone)
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
            std::sync::Arc<synctv_api_common::impls::ClientApiImpl>,
            synctv_core::service::AuthenticatedToken,
        ) -> Fut
        + Send
        + 'a,
    Fut: Future<Output = Result<T, synctv_api_common::impls::ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_metadata(request_meta);
        let client_api = state.shared_api_runtime.client_api.clone();
        let client_api_clone = client_api.clone();
        client_api
            .execute_scoped_user_endpoint(&request_meta, category, scope, move |authenticated| {
                operation(client_api_clone, authenticated)
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
    F: FnOnce(
            std::sync::Arc<synctv_api_common::impls::ClientApiImpl>,
            synctv_api_common::impls::client::RoomActor,
        ) -> Fut
        + Send
        + 'a,
    Fut: Future<Output = Result<T, synctv_api_common::impls::ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_metadata(request_meta);
        let client_api = state.shared_api_runtime.client_api.clone();
        synctv_api_common::impls::ClientApiImpl::execute_scoped_room_actor_endpoint(
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
            std::sync::Arc<synctv_api_common::impls::ClientApiImpl>,
            synctv_core::provider::ExecutionControl,
            synctv_api_common::impls::client::RoomActor,
        ) -> Fut
        + Send
        + 'a,
    Fut: Future<Output = Result<T, synctv_api_common::impls::ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_metadata(request_meta);
        let client_api = state.shared_api_runtime.client_api.clone();
        synctv_api_common::impls::ClientApiImpl::execute_scoped_room_actor_endpoint_with_control(
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
