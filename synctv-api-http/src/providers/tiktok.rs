use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;

use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::providers::common::ProviderInstanceQuery;
use synctv_proto::providers::tiktok::{
    BindRequest, BindResponse, GetBindsResponse, GetUserRequest, GetUserResponse,
    ListUserPostsRequest, ListUserPostsResponse, ResolveRequest, ResolveResponse, UnbindRequest,
    UnbindResponse,
};

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field,
};

pub(crate) fn tiktok_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/bind", post(bind))
        .route("/unbind", post(unbind))
}

pub(crate) fn tiktok_read_routes() -> Router<AppState> {
    Router::new()
        .route("/binds", get(binds))
        .route("/resolve", post(resolve))
        .route("/user", post(get_user))
        .route("/user-posts", post(list_user_posts))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/tiktok/bind",
        tag = "Provider",
        request_body = BindRequest,
        responses(
            (status = 200, description = "TikTok credential bound", body = BindResponse),
            (status = 400, description = "Invalid TikTok credential", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn bind(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<BindRequest>,
) -> AppResult<Json<BindResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.tiktok_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |_control, authenticated| {
            async move {
                api.bind(authenticated.user_id(), req, instance_name.as_deref())
                    .await
            }
            .boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/tiktok/binds",
        tag = "Provider",
        params(ProviderInstanceQuery),
        responses((status = 200, description = "Saved TikTok credentials", body = GetBindsResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn binds(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    let instance_name = provider_instance_name(&query)?.map(str::to_owned);
    let api = state.shared_api_runtime.tiktok_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |authenticated| {
            async move {
                api.get_binds(authenticated.user_id(), instance_name.as_deref())
                    .await
            }
            .boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/tiktok/unbind",
        tag = "Provider",
        request_body = UnbindRequest,
        responses((status = 200, description = "TikTok credential removed", body = UnbindResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn unbind(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<UnbindRequest>,
) -> AppResult<Json<UnbindResponse>> {
    let api = state.shared_api_runtime.tiktok_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |authenticated| async move { api.unbind(authenticated.user_id(), req).await }.boxed(),
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/tiktok/resolve",
        tag = "Provider",
        request_body = ResolveRequest,
        responses((status = 200, description = "Resolved TikTok video or live room", body = ResolveResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn resolve(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ResolveRequest>,
) -> AppResult<Json<ResolveResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.tiktok_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |authenticated| {
            async move {
                api.resolve(authenticated.user_id(), req, instance_name.as_deref())
                    .await
            }
            .boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/tiktok/user-posts",
        tag = "Provider",
        request_body = ListUserPostsRequest,
        responses((status = 200, description = "TikTok user works cursor page", body = ListUserPostsResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_user_posts(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ListUserPostsRequest>,
) -> AppResult<Json<ListUserPostsResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.tiktok_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |authenticated| {
            async move {
                api.list_user_posts(authenticated.user_id(), req, instance_name.as_deref())
                    .await
            }
            .boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/tiktok/user",
        tag = "Provider",
        request_body = GetUserRequest,
        responses((status = 200, description = "Resolved TikTok user", body = GetUserResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_user(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<GetUserRequest>,
) -> AppResult<Json<GetUserResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.tiktok_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |authenticated| {
            async move {
                api.get_user(authenticated.user_id(), req, instance_name.as_deref())
                    .await
            }
            .boxed()
        },
    )
    .await
}
