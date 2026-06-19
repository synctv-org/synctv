//! Alist Provider HTTP Routes
//!
//! Provider API endpoints for Alist login, directory listing, etc.
//! Playback transport routes live under the Alist playback-provider module.

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;

use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use crate::impls::{ApiError, EndpointRateLimitCategory};
use synctv_proto::providers::alist::{
    GetBindsResponse, GetMeRequest, ListRequest, LogoutRequest, SearchRequest,
};
use synctv_proto::providers::common::ProviderInstanceQuery;

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_body,
};

/// Alist endpoints that perform authentication or credential mutation.
pub(crate) fn alist_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
}

/// Alist read/query endpoints.
pub(crate) fn alist_read_routes() -> Router<AppState> {
    Router::new()
        .route("/list", post(list))
        .route("/search", post(search))
        .route("/me", post(me))
        .route("/binds", get(binds))
}

// Provider API handlers

/// Login to Alist (persist credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/alist/login",
        tag = "Provider",
        request_body = synctv_proto::http_serde::AlistLoginRequestDef,
        responses(
            (status = 200, description = "Alist login succeeded", body = synctv_proto::providers::alist::LoginResponse),
            (status = 400, description = "Invalid login request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Provider access denied", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Provider resource not found", body = synctv_proto::client::ApiErrorResponse),
            (status = 408, description = "Provider request timed out", body = synctv_proto::client::ApiErrorResponse),
            (status = 409, description = "Provider request conflict", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Provider service unavailable", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn login(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<synctv_proto::http_serde::AlistLoginRequestDef>,
) -> AppResult<Json<synctv_proto::providers::alist::LoginResponse>> {
    tracing::info!("Alist login request");

    let req = synctv_proto::providers::alist::LoginRequest::try_from(req)
        .map_err(ApiError::InvalidInput)?;
    let instance_name = provider_instance_name_from_body(&req.instance_name)?;
    let api = state.shared_api_runtime.alist_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |control, authenticated| {
            async move {
                api.login_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            }
            .boxed()
        },
    )
    .await
    .map_err(|e| {
        tracing::error!("Alist login failed: {}", e);
        e
    })
    .inspect(|_| {
        tracing::info!("Alist login successful");
    })
}

/// List Alist directory (uses stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/alist/list",
        tag = "Provider",
        request_body = ListRequest,
        params(
            ("instance_name" = Option<String>, Query, description = "Optional provider instance name")
        ),
        responses(
            (status = 200, description = "Alist directory listing", body = synctv_proto::providers::alist::ListResponse),
            (status = 400, description = "Invalid list request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Provider access denied", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Provider resource not found", body = synctv_proto::client::ApiErrorResponse),
            (status = 408, description = "Provider request timed out", body = synctv_proto::client::ApiErrorResponse),
            (status = 409, description = "Provider request conflict", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Provider service unavailable", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn list(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ListRequest>,
) -> AppResult<Json<synctv_proto::providers::alist::ListResponse>> {
    tracing::info!("Alist list request");

    let instance_name = provider_instance_name_from_body(&req.instance_name)?;
    let api = state.shared_api_runtime.alist_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |control, authenticated| {
            async move {
                api.list_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            }
            .boxed()
        },
    )
    .await
    .map_err(|e| {
        tracing::error!("Alist list failed: {}", e);
        e
    })
}

/// Search Alist files and directories (uses stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/alist/search",
        tag = "Provider",
        request_body = SearchRequest,
        responses(
            (status = 200, description = "Alist search results", body = synctv_proto::providers::alist::SearchResponse),
            (status = 400, description = "Invalid search request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Provider access denied", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Provider resource not found", body = synctv_proto::client::ApiErrorResponse),
            (status = 408, description = "Provider request timed out", body = synctv_proto::client::ApiErrorResponse),
            (status = 409, description = "Provider request conflict", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Provider service unavailable", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn search(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<SearchRequest>,
) -> AppResult<Json<synctv_proto::providers::alist::SearchResponse>> {
    tracing::info!("Alist search request");

    let instance_name = provider_instance_name_from_body(&req.instance_name)?;
    let api = state.shared_api_runtime.alist_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |control, authenticated| {
            async move {
                api.search_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            }
            .boxed()
        },
    )
    .await
    .map_err(|e| {
        tracing::error!("Alist search failed: {}", e);
        e
    })
}

/// Get Alist user info (uses stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/alist/me",
        tag = "Provider",
        request_body = GetMeRequest,
        responses(
            (status = 200, description = "Alist account info", body = synctv_proto::providers::alist::GetMeResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Provider access denied", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Provider resource not found", body = synctv_proto::client::ApiErrorResponse),
            (status = 408, description = "Provider request timed out", body = synctv_proto::client::ApiErrorResponse),
            (status = 409, description = "Provider request conflict", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Provider service unavailable", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn me(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<GetMeRequest>,
) -> AppResult<Json<synctv_proto::providers::alist::GetMeResponse>> {
    tracing::info!("Alist me request");

    let instance_name = provider_instance_name_from_body(&req.instance_name)?;
    let api = state.shared_api_runtime.alist_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |control, authenticated| {
            async move {
                api.get_me_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            }
            .boxed()
        },
    )
    .await
    .map_err(|e| {
        tracing::error!("Alist me failed: {}", e);
        e
    })
}

/// Logout from Alist (delete stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/alist/logout",
        tag = "Provider",
        request_body = LogoutRequest,
        responses(
            (status = 200, description = "Alist credential removed", body = synctv_proto::providers::alist::LogoutResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Provider access denied", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Provider resource not found", body = synctv_proto::client::ApiErrorResponse),
            (status = 408, description = "Provider request timed out", body = synctv_proto::client::ApiErrorResponse),
            (status = 409, description = "Provider request conflict", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Provider service unavailable", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn logout(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LogoutRequest>,
) -> AppResult<Json<synctv_proto::providers::alist::LogoutResponse>> {
    tracing::info!("Alist logout request");

    provider_instance_name_from_body(&req.instance_name)?;
    let api = state.shared_api_runtime.alist_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |authenticated| async move { api.logout(&authenticated.user_id, req).await }.boxed(),
    )
    .await
    .map_err(|e| {
        tracing::error!("Alist logout failed: {}", e);
        e
    })
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/alist/binds",
        tag = "Provider",
        params(ProviderInstanceQuery),
        responses(
            (status = 200, description = "Saved Alist credentials", body = GetBindsResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 400, description = "Invalid provider instance query", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Provider access denied", body = synctv_proto::client::ApiErrorResponse),
            (status = 408, description = "Provider bind request timed out", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Provider bind information unavailable", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn binds(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    let instance_name = provider_instance_name(&query)?.map(str::to_owned);
    let api = state.shared_api_runtime.alist_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |authenticated| {
            async move {
                tracing::info!("Alist binds request for user: {}", authenticated.user_id);
                api.get_binds(&authenticated.user_id, instance_name.as_deref())
                    .await
            }
            .boxed()
        },
    )
    .await
}
