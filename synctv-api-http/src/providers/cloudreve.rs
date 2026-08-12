use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;

use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::providers::cloudreve::{
    GetBindsResponse, GetMeRequest, ListRequest, LogoutRequest, SearchRequest,
};
use synctv_proto::providers::common::ProviderInstanceQuery;

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field,
};

pub(crate) fn cloudreve_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
}

pub(crate) fn cloudreve_read_routes() -> Router<AppState> {
    Router::new()
        .route("/list", post(list))
        .route("/search", post(search))
        .route("/me", post(me))
        .route("/binds", get(binds))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/cloudreve/login",
        tag = "Provider",
        request_body = synctv_proto::providers::cloudreve::LoginRequest,
        responses(
            (status = 200, description = "Cloudreve login succeeded", body = synctv_proto::providers::cloudreve::LoginResponse),
            (status = 400, description = "Invalid login request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn login(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<synctv_proto::providers::cloudreve::LoginRequest>,
) -> AppResult<Json<synctv_proto::providers::cloudreve::LoginResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.cloudreve_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |_control, authenticated| {
            async move {
                api.login(authenticated.user_id(), req, instance_name.as_deref())
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
        path = "/api/providers/cloudreve/list",
        tag = "Provider",
        request_body = ListRequest,
        responses(
            (status = 200, description = "Cloudreve directory listing", body = synctv_proto::providers::cloudreve::ListResponse),
            (status = 400, description = "Invalid list request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ListRequest>,
) -> AppResult<Json<synctv_proto::providers::cloudreve::ListResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.cloudreve_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |_control, authenticated| {
            async move {
                api.list(authenticated.user_id(), req, instance_name.as_deref())
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
        path = "/api/providers/cloudreve/search",
        tag = "Provider",
        request_body = SearchRequest,
        responses(
            (status = 200, description = "Cloudreve search results", body = synctv_proto::providers::cloudreve::SearchResponse),
            (status = 400, description = "Invalid search request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn search(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<SearchRequest>,
) -> AppResult<Json<synctv_proto::providers::cloudreve::SearchResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.cloudreve_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |_control, authenticated| {
            async move {
                api.search(authenticated.user_id(), req, instance_name.as_deref())
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
        path = "/api/providers/cloudreve/me",
        tag = "Provider",
        request_body = GetMeRequest,
        responses(
            (status = 200, description = "Cloudreve account information", body = synctv_proto::providers::cloudreve::GetMeResponse),
            (status = 400, description = "Invalid account request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn me(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<GetMeRequest>,
) -> AppResult<Json<synctv_proto::providers::cloudreve::GetMeResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.cloudreve_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |_control, authenticated| {
            async move {
                api.get_me(authenticated.user_id(), req, instance_name.as_deref())
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
        path = "/api/providers/cloudreve/logout",
        tag = "Provider",
        request_body = LogoutRequest,
        responses(
            (status = 200, description = "Cloudreve credential removed", body = synctv_proto::providers::cloudreve::LogoutResponse),
            (status = 400, description = "Invalid logout request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn logout(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LogoutRequest>,
) -> AppResult<Json<synctv_proto::providers::cloudreve::LogoutResponse>> {
    let api = state.shared_api_runtime.cloudreve_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |_control, authenticated| {
            async move { api.logout(authenticated.user_id(), req).await }.boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/cloudreve/binds",
        tag = "Provider",
        params(ProviderInstanceQuery),
        responses(
            (status = 200, description = "Saved Cloudreve credentials", body = GetBindsResponse),
            (status = 400, description = "Invalid provider instance query", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider bind request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider bind information unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn binds(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    let instance_name = provider_instance_name(&query)?.map(str::to_owned);
    let api = state.shared_api_runtime.cloudreve_api.clone();
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
