use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use synctv_proto::providers::common::ProviderInstanceQuery;
use synctv_proto::providers::truenas::{
    GetBindsResponse, ListRequest, LoginRequest, LogoutRequest,
};

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field,
};
use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use crate::impls::EndpointRateLimitCategory;

pub(crate) fn truenas_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
}

pub(crate) fn truenas_read_routes() -> Router<AppState> {
    Router::new()
        .route("/list", post(list))
        .route("/binds", get(binds))
}

macro_rules! user_post {
    ($name:ident, $path:literal, $req:ty, $resp:ty, $category:expr, $method:ident) => {
        #[cfg_attr(feature = "openapi", utoipa::path(post, path = $path, tag = "Provider", request_body = $req, responses((status = 200, body = $resp)), security(("bearer_auth" = []))))]
        pub(crate) async fn $name(request_meta: RequestMetadata, State(state): State<AppState>, Json(req): Json<$req>) -> AppResult<Json<$resp>> {
            let instance = provider_instance_name_from_request_field(&req.instance_name)?;
            let api = state.shared_api_runtime.truenas_api.clone();
            execute_provider_user_endpoint_with_control(&state, request_meta, $category, move |_control, auth| async move { api.$method(auth.user_id, req, instance.as_deref()).await }.boxed()).await
        }
    };
}

user_post!(
    login,
    "/api/providers/truenas/login",
    LoginRequest,
    synctv_proto::providers::truenas::LoginResponse,
    EndpointRateLimitCategory::Auth,
    login
);
user_post!(
    list,
    "/api/providers/truenas/list",
    ListRequest,
    synctv_proto::providers::truenas::ListResponse,
    EndpointRateLimitCategory::Read,
    list
);
user_post!(
    logout,
    "/api/providers/truenas/logout",
    LogoutRequest,
    synctv_proto::providers::truenas::LogoutResponse,
    EndpointRateLimitCategory::Write,
    logout
);

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/providers/truenas/binds", tag = "Provider", responses((status = 200, body = GetBindsResponse)), security(("bearer_auth" = []))))]
pub(crate) async fn binds(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    let instance = provider_instance_name(&query)?.map(str::to_string);
    let api = state.shared_api_runtime.truenas_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |auth| async move { api.binds(auth.user_id, instance.as_deref()).await }.boxed(),
    )
    .await
}
