use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::Deserialize;

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field, provider_request_metadata,
};
use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::providers::common::ProviderInstanceQuery;
use synctv_proto::providers::qnap::{
    GetBindsResponse, GetCapabilitiesRequest, ListRequest, LoginRequest, LogoutRequest,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ThumbnailQuery {
    server_id: String,
    path: String,
    #[serde(default = "default_size")]
    size: u32,
}

const fn default_size() -> u32 {
    640
}

pub(crate) fn qnap_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
}

pub(crate) fn qnap_read_routes() -> Router<AppState> {
    Router::new()
        .route("/list", post(list))
        .route("/capabilities", post(capabilities))
        .route("/binds", get(binds))
        .route("/thumbnail", get(thumbnail))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/qnap/login",
        tag = "Provider",
        request_body = LoginRequest,
        responses((status = 200, body = synctv_proto::providers::qnap::LoginResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn login(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> AppResult<Json<synctv_proto::providers::qnap::LoginResponse>> {
    let instance = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.qnap_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |_control, auth| {
            async move { api.login(auth.user_id(), req, instance.as_deref()).await }.boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/qnap/list",
        tag = "Provider",
        request_body = ListRequest,
        responses((status = 200, body = synctv_proto::providers::qnap::ListResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ListRequest>,
) -> AppResult<Json<synctv_proto::providers::qnap::ListResponse>> {
    let instance = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.qnap_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |_control, auth| {
            async move { api.list(auth.user_id(), req, instance.as_deref()).await }.boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/qnap/capabilities",
        tag = "Provider",
        request_body = GetCapabilitiesRequest,
        responses((
            status = 200,
            body = synctv_proto::providers::qnap::GetCapabilitiesResponse
        )),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn capabilities(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<GetCapabilitiesRequest>,
) -> AppResult<Json<synctv_proto::providers::qnap::GetCapabilitiesResponse>> {
    let instance = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.qnap_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |_control, auth| {
            async move {
                api.capabilities(auth.user_id(), req, instance.as_deref())
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
        path = "/api/providers/qnap/logout",
        tag = "Provider",
        request_body = LogoutRequest,
        responses((status = 200, body = synctv_proto::providers::qnap::LogoutResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn logout(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LogoutRequest>,
) -> AppResult<Json<synctv_proto::providers::qnap::LogoutResponse>> {
    let api = state.shared_api_runtime.qnap_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        move |_control, auth| async move { api.logout(auth.user_id(), req).await }.boxed(),
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/qnap/binds",
        tag = "Provider",
        responses((status = 200, body = GetBindsResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn binds(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    let instance = provider_instance_name(&query)?.map(str::to_string);
    let api = state.shared_api_runtime.qnap_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |auth| async move { api.binds(auth.user_id(), instance.as_deref()).await }.boxed(),
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/qnap/thumbnail",
        tag = "Provider",
        params(
            ("serverId" = String, Query),
            ("path" = String, Query),
            ("size" = u32, Query)
        ),
        responses((status = 200, description = "QNAP thumbnail")),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn thumbnail(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Query(query): Query<ThumbnailQuery>,
) -> AppResult<axum::response::Response> {
    let req = synctv_proto::providers::qnap::GetThumbnailRequest {
        server_id: query.server_id,
        path: query.path,
        size: query.size,
    };
    let operation_state = state.clone();
    let metadata = provider_request_metadata(request_meta);
    let action = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Read,
            move |auth| async move {
                let state = operation_state;
                state
                    .shared_api_runtime
                    .qnap_api
                    .thumbnail_action(auth.user_id(), req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    super::execute_provider_preview_transport(&state, action, None).await
}
