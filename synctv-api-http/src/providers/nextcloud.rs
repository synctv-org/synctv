use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::Deserialize;
use synctv_proto::providers::common::ProviderInstanceQuery;
use synctv_proto::providers::nextcloud::{
    GetBindsResponse, ListFavoritesRequest, ListRequest, LoginRequest, LogoutRequest,
    PollLoginFlowRequest, StartLoginFlowRequest,
};

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field, provider_request_metadata,
};
use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use synctv_api_common::impls::EndpointRateLimitCategory;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PreviewQuery {
    server_id: String,
    file_id: u64,
    #[serde(default = "default_size")]
    width: u32,
    #[serde(default = "default_size")]
    height: u32,
    #[serde(default = "default_crop")]
    crop: bool,
}

const fn default_size() -> u32 {
    640
}
const fn default_crop() -> bool {
    true
}

pub(crate) fn nextcloud_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/login-flow/start", post(start_login_flow))
        .route("/login-flow/poll", post(poll_login_flow))
        .route("/logout", post(logout))
}

pub(crate) fn nextcloud_read_routes() -> Router<AppState> {
    Router::new()
        .route("/list", post(list))
        .route("/favorites", post(list_favorites))
        .route("/binds", get(binds))
        .route("/preview", get(preview))
}

macro_rules! user_post {
    ($name:ident, $path:literal, $req:ty, $resp:ty, $category:expr, $method:ident) => {
        #[cfg_attr(feature = "openapi", utoipa::path(post, path = $path, tag = "Provider", request_body = $req, responses((status = 200, body = $resp)), security(("bearer_auth" = []))))]
        pub(crate) async fn $name(request_meta: RequestMetadata, State(state): State<AppState>, Json(req): Json<$req>) -> AppResult<Json<$resp>> {
            let instance = provider_instance_name_from_request_field(&req.instance_name)?;
            let api = state.shared_api_runtime.nextcloud_api.clone();
            execute_provider_user_endpoint_with_control(&state, request_meta, $category, move |_control, auth| { async move { api.$method(auth.user_id(), req, instance.as_deref()).await }.boxed() }).await
        }
    };
}

user_post!(
    login,
    "/api/providers/nextcloud/login",
    LoginRequest,
    synctv_proto::providers::nextcloud::LoginResponse,
    EndpointRateLimitCategory::Auth,
    login
);
user_post!(
    poll_login_flow,
    "/api/providers/nextcloud/login-flow/poll",
    PollLoginFlowRequest,
    synctv_proto::providers::nextcloud::LoginResponse,
    EndpointRateLimitCategory::Auth,
    poll_login_flow
);
user_post!(
    list,
    "/api/providers/nextcloud/list",
    ListRequest,
    synctv_proto::providers::nextcloud::ListResponse,
    EndpointRateLimitCategory::Read,
    list
);
user_post!(
    list_favorites,
    "/api/providers/nextcloud/favorites",
    ListFavoritesRequest,
    synctv_proto::providers::nextcloud::ListResponse,
    EndpointRateLimitCategory::Read,
    list_favorites
);

#[cfg_attr(feature = "openapi", utoipa::path(post, path = "/api/providers/nextcloud/login-flow/start", tag = "Provider", request_body = StartLoginFlowRequest, responses((status = 200, body = synctv_proto::providers::nextcloud::StartLoginFlowResponse)), security(("bearer_auth" = []))))]
pub(crate) async fn start_login_flow(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<StartLoginFlowRequest>,
) -> AppResult<Json<synctv_proto::providers::nextcloud::StartLoginFlowResponse>> {
    let api = state.shared_api_runtime.nextcloud_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |_auth| async move { api.start_login_flow(req).await }.boxed(),
    )
    .await
}

#[cfg_attr(feature = "openapi", utoipa::path(post, path = "/api/providers/nextcloud/logout", tag = "Provider", request_body = LogoutRequest, responses((status = 200, body = synctv_proto::providers::nextcloud::LogoutResponse)), security(("bearer_auth" = []))))]
pub(crate) async fn logout(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LogoutRequest>,
) -> AppResult<Json<synctv_proto::providers::nextcloud::LogoutResponse>> {
    let api = state.shared_api_runtime.nextcloud_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        move |_control, auth| async move { api.logout(auth.user_id(), req).await }.boxed(),
    )
    .await
}

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/providers/nextcloud/binds", tag = "Provider", responses((status = 200, body = GetBindsResponse)), security(("bearer_auth" = []))))]
pub(crate) async fn binds(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    let instance = provider_instance_name(&query)?.map(str::to_string);
    let api = state.shared_api_runtime.nextcloud_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |auth| async move { api.binds(auth.user_id(), instance.as_deref()).await }.boxed(),
    )
    .await
}

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/providers/nextcloud/preview", tag = "Provider", params(("serverId" = String, Query), ("fileId" = u64, Query), ("width" = u32, Query), ("height" = u32, Query), ("crop" = bool, Query)), responses((status = 200, description = "Nextcloud preview")), security(("bearer_auth" = []))))]
pub(crate) async fn preview(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Query(query): Query<PreviewQuery>,
) -> AppResult<axum::response::Response> {
    let req = synctv_proto::providers::nextcloud::GetPreviewRequest {
        server_id: query.server_id,
        file_id: query.file_id,
        width: query.width,
        height: query.height,
        crop: Some(query.crop),
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
                    .nextcloud_api
                    .preview_action(auth.user_id(), req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    super::execute_provider_preview_transport(&state, action, None).await
}
