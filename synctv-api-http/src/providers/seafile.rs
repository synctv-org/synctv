use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::Deserialize;
use synctv_proto::providers::common::ProviderInstanceQuery;
use synctv_proto::providers::seafile::{
    GetBindsResponse, ListRepositoriesRequest, ListRequest, ListStarredRequest, LoginRequest,
    LogoutRequest, UnlockLibraryRequest,
};

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field, provider_request_metadata,
};
use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use synctv_api_common::impls::EndpointRateLimitCategory;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ThumbnailQuery {
    server_id: String,
    repository_id: String,
    path: String,
    #[serde(default = "default_size")]
    size: u32,
}

const fn default_size() -> u32 {
    640
}

pub(crate) fn seafile_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/unlock-library", post(unlock_library))
        .route("/logout", post(logout))
}

pub(crate) fn seafile_read_routes() -> Router<AppState> {
    Router::new()
        .route("/repositories", post(list_repositories))
        .route("/list", post(list))
        .route("/starred", post(list_starred))
        .route("/binds", get(binds))
        .route("/thumbnail", get(thumbnail))
}

macro_rules! user_post {
    ($name:ident, $path:literal, $req:ty, $resp:ty, $category:expr, $method:ident) => {
        #[cfg_attr(feature = "openapi", utoipa::path(post, path = $path, tag = "Provider", request_body = $req, responses((status = 200, body = $resp)), security(("bearer_auth" = []))))]
        pub(crate) async fn $name(request_meta: RequestMetadata, State(state): State<AppState>, Json(req): Json<$req>) -> AppResult<Json<$resp>> {
            let instance = provider_instance_name_from_request_field(&req.instance_name)?;
            let api = state.shared_api_runtime.seafile_api.clone();
            execute_provider_user_endpoint_with_control(&state, request_meta, $category, move |_control, auth| async move { api.$method(auth.user_id(), req, instance.as_deref()).await }.boxed()).await
        }
    };
}

user_post!(
    login,
    "/api/providers/seafile/login",
    LoginRequest,
    synctv_proto::providers::seafile::LoginResponse,
    EndpointRateLimitCategory::Auth,
    login
);
user_post!(
    unlock_library,
    "/api/providers/seafile/unlock-library",
    UnlockLibraryRequest,
    synctv_proto::providers::seafile::UnlockLibraryResponse,
    EndpointRateLimitCategory::Write,
    unlock_library
);
user_post!(
    list_repositories,
    "/api/providers/seafile/repositories",
    ListRepositoriesRequest,
    synctv_proto::providers::seafile::ListResponse,
    EndpointRateLimitCategory::Read,
    list_repositories
);
user_post!(
    list,
    "/api/providers/seafile/list",
    ListRequest,
    synctv_proto::providers::seafile::ListResponse,
    EndpointRateLimitCategory::Read,
    list
);
user_post!(
    list_starred,
    "/api/providers/seafile/starred",
    ListStarredRequest,
    synctv_proto::providers::seafile::ListResponse,
    EndpointRateLimitCategory::Read,
    list_starred
);

#[cfg_attr(feature = "openapi", utoipa::path(post, path = "/api/providers/seafile/logout", tag = "Provider", request_body = LogoutRequest, responses((status = 200, body = synctv_proto::providers::seafile::LogoutResponse)), security(("bearer_auth" = []))))]
pub(crate) async fn logout(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LogoutRequest>,
) -> AppResult<Json<synctv_proto::providers::seafile::LogoutResponse>> {
    let api = state.shared_api_runtime.seafile_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        move |_control, auth| async move { api.logout(auth.user_id(), req).await }.boxed(),
    )
    .await
}

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/providers/seafile/binds", tag = "Provider", responses((status = 200, body = GetBindsResponse)), security(("bearer_auth" = []))))]
pub(crate) async fn binds(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    let instance = provider_instance_name(&query)?.map(str::to_string);
    let api = state.shared_api_runtime.seafile_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |auth| async move { api.binds(auth.user_id(), instance.as_deref()).await }.boxed(),
    )
    .await
}

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/providers/seafile/thumbnail", tag = "Provider", params(("serverId" = String, Query), ("repositoryId" = String, Query), ("path" = String, Query), ("size" = u32, Query)), responses((status = 200, description = "Seafile thumbnail")), security(("bearer_auth" = []))))]
pub(crate) async fn thumbnail(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Query(query): Query<ThumbnailQuery>,
) -> AppResult<axum::response::Response> {
    let req = synctv_proto::providers::seafile::GetThumbnailRequest {
        server_id: query.server_id,
        repository_id: query.repository_id,
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
                    .seafile_api
                    .thumbnail_action(auth.user_id(), req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    super::execute_provider_preview_transport(&state, action, None).await
}
