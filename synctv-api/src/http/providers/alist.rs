//! Alist Provider HTTP Routes
//!
//! Provider API endpoints for Alist login, directory listing, etc.
//! Proxy routes are handled by the unified proxy handler in `providers/mod.rs`.

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT;

use crate::http::{
    error::map_api_error, middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState,
};
use crate::impls::EndpointRateLimitCategory;
use crate::proto::providers::alist::{
    GetBindsResponse, GetMeRequest, ListRequest, LoginRequest, LogoutRequest, SearchRequest,
};
use crate::proto::providers::common::ProviderInstanceQuery;

use super::common::provider_instance_name;

/// Alist endpoints that perform authentication or credential mutation.
pub fn alist_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
}

/// Alist read/query endpoints.
pub fn alist_read_routes() -> Router<AppState> {
    Router::new()
        .route("/list", post(list))
        .route("/search", post(search))
        .route("/me", post(me))
        .route("/binds", get(binds))
}

fn request_metadata(request_meta: RequestMetadata) -> crate::impls::RequestMetadata {
    request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT))
}

// Provider API handlers

/// Login to Alist (persist credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/alist/login",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = LoginRequest,
        responses(
            (status = 200, description = "Alist login succeeded", body = crate::proto::providers::alist::LoginResponse),
            (status = 400, description = "Invalid login request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn login(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
    Json(mut req): Json<LoginRequest>,
) -> AppResult<Json<crate::proto::providers::alist::LoginResponse>> {
    tracing::info!("Alist login request");

    if let Some(query_instance_name) = provider_instance_name(&query)? {
        req.instance_name = query_instance_name.to_string();
    }
    let instance_name = crate::impls::providers::extract_instance_name(&req.instance_name);
    let api = state.shared_api_runtime.alist_api.clone();
    let request_meta = request_metadata(request_meta);
    let resp = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |control, authenticated| async move {
                api.login_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            },
        )
        .await
        .map_err(map_api_error)
        .map_err(|e| {
            tracing::error!("Alist login failed: {}", e);
            e
        })?;
    tracing::info!("Alist login successful");
    Ok(Json(resp))
}

/// List Alist directory (uses stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/alist/list",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = ListRequest,
        responses(
            (status = 200, description = "Alist directory listing", body = crate::proto::providers::alist::ListResponse),
            (status = 400, description = "Invalid list request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn list(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
    Json(mut req): Json<ListRequest>,
) -> AppResult<Json<crate::proto::providers::alist::ListResponse>> {
    tracing::info!("Alist list request");

    if let Some(query_instance_name) = provider_instance_name(&query)? {
        req.instance_name = query_instance_name.to_string();
    }
    let instance_name = crate::impls::providers::extract_instance_name(&req.instance_name);
    let api = state.shared_api_runtime.alist_api.clone();
    let request_meta = request_metadata(request_meta);
    let resp = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Read,
            move |control, authenticated| async move {
                api.list_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            },
        )
        .await
        .map_err(map_api_error)
        .map_err(|e| {
            tracing::error!("Alist list failed: {}", e);
            e
        })?;
    Ok(Json(resp))
}

/// Search Alist files and directories (uses stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/alist/search",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = SearchRequest,
        responses(
            (status = 200, description = "Alist search results", body = crate::proto::providers::alist::SearchResponse),
            (status = 400, description = "Invalid search request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn search(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
    Json(mut req): Json<SearchRequest>,
) -> AppResult<Json<crate::proto::providers::alist::SearchResponse>> {
    tracing::info!("Alist search request");

    if let Some(query_instance_name) = provider_instance_name(&query)? {
        req.instance_name = query_instance_name.to_string();
    }
    let instance_name = crate::impls::providers::extract_instance_name(&req.instance_name);
    let api = state.shared_api_runtime.alist_api.clone();
    let request_meta = request_metadata(request_meta);
    let resp = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Read,
            move |control, authenticated| async move {
                api.search_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            },
        )
        .await
        .map_err(map_api_error)
        .map_err(|e| {
            tracing::error!("Alist search failed: {}", e);
            e
        })?;
    Ok(Json(resp))
}

/// Get Alist user info (uses stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/alist/me",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = GetMeRequest,
        responses(
            (status = 200, description = "Alist account info", body = crate::proto::providers::alist::GetMeResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn me(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
    Json(mut req): Json<GetMeRequest>,
) -> AppResult<Json<crate::proto::providers::alist::GetMeResponse>> {
    tracing::info!("Alist me request");

    if let Some(query_instance_name) = provider_instance_name(&query)? {
        req.instance_name = query_instance_name.to_string();
    }
    let instance_name = crate::impls::providers::extract_instance_name(&req.instance_name);
    let api = state.shared_api_runtime.alist_api.clone();
    let request_meta = request_metadata(request_meta);
    let resp = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Read,
            move |control, authenticated| async move {
                api.get_me_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            },
        )
        .await
        .map_err(map_api_error)
        .map_err(|e| {
            tracing::error!("Alist me failed: {}", e);
            e
        })?;
    Ok(Json(resp))
}

/// Logout from Alist (delete stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/alist/logout",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = LogoutRequest,
        responses(
            (status = 200, description = "Alist credential removed", body = crate::proto::providers::alist::LogoutResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn logout(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
    Json(mut req): Json<LogoutRequest>,
) -> AppResult<Json<crate::proto::providers::alist::LogoutResponse>> {
    tracing::info!("Alist logout request");

    if let Some(query_instance_name) = provider_instance_name(&query)? {
        req.instance_name = query_instance_name.to_string();
    }
    let api = state.shared_api_runtime.alist_api.clone();
    let request_meta = request_metadata(request_meta);
    let resp = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |authenticated| async move { api.logout(&authenticated.user_id, req).await },
        )
        .await
        .map_err(|e| {
            tracing::error!("Alist logout failed: {}", e);
            e
        })?;
    Ok(Json(resp))
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
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 503, description = "Provider bind information unavailable", body = crate::openapi::ErrorResponseDoc)
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
    let instance_name = provider_instance_name(&query)?;
    let api = state.shared_api_runtime.alist_api.clone();
    let request_meta = request_metadata(request_meta);
    let response = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Read,
            move |authenticated| async move {
                tracing::info!("Alist binds request for user: {}", authenticated.user_id);
                api.get_binds(&authenticated.user_id, instance_name).await
            },
        )
        .await
        .map_err(map_api_error)?;

    Ok(Json(response))
}
