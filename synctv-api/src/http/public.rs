//! Public API endpoints
//!
//! Endpoints that can be accessed without authentication.

use axum::{extract::State, response::Json, routing::get, Router};

use crate::http::{
    error::map_api_error, middleware::RequestMetadata, validation::ProtoQuery, AppState,
};
use crate::impls::EndpointRateLimitCategory;
use synctv_proto::client::{
    GetPublicSettingsResponse, GetServerInfoResponse, GetServerTimeRequest, GetServerTimeResponse,
};

/// Create public API router
pub(crate) fn create_public_router() -> Router<AppState> {
    Router::new()
        .route("/api/public/settings", get(get_public_settings))
        .route("/api/public/server-info", get(get_server_info))
        .route("/api/public/time", get(get_server_time))
}

/// Get public server settings
///
/// This endpoint can be called without authentication and returns
/// public server configuration that clients need to know.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/public/settings",
        tag = "Public",
        responses(
            (status = 200, description = "Public server settings", body = GetPublicSettingsResponse),
            (status = 500, description = "Failed to load settings", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub async fn get_public_settings(
    State(state): State<AppState>,
    request_meta: RequestMetadata,
) -> Result<Json<GetPublicSettingsResponse>, super::AppError> {
    let metadata = request_meta.0;
    let client_api = state.shared_api_runtime.client_api.clone();
    let client_api_for_op = client_api.clone();
    let response = client_api
        .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, || async move {
            client_api_for_op.get_public_settings()
        })
        .await
        .map_err(map_api_error)?;
    Ok(Json(response))
}

/// Get public server identity
///
/// This endpoint can be called without authentication and returns the stable
/// logical server id used by native clients to group multiple endpoints that
/// belong to the same SyncTV deployment.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/public/server-info",
        tag = "Public",
        responses(
            (status = 200, description = "Public server identity", body = GetServerInfoResponse),
            (status = 500, description = "Failed to load server identity", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub async fn get_server_info(
    State(state): State<AppState>,
    request_meta: RequestMetadata,
) -> Result<Json<GetServerInfoResponse>, super::AppError> {
    let metadata = request_meta.0;
    let client_api = state.shared_api_runtime.client_api.clone();
    let client_api_for_op = client_api.clone();
    let response = client_api
        .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, || async move {
            client_api_for_op.get_server_info().await
        })
        .await
        .map_err(map_api_error)?;
    Ok(Json(response))
}

/// Get public server time
///
/// This endpoint can be called without authentication and returns enough
/// timestamps for clients to estimate clock offset and network delay.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/public/time",
        tag = "Public",
        params(GetServerTimeRequest),
        responses(
            (status = 200, description = "Public server time", body = GetServerTimeResponse),
            (status = 400, description = "Invalid query", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub async fn get_server_time(
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    ProtoQuery(req): ProtoQuery<GetServerTimeRequest>,
) -> Result<Json<GetServerTimeResponse>, super::AppError> {
    let metadata = request_meta.0;
    let client_api = state.shared_api_runtime.client_api.clone();
    let client_api_for_op = client_api.clone();
    let response = client_api
        .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, || async move {
            Ok::<_, crate::impls::ApiError>(client_api_for_op.get_server_time(req))
        })
        .await
        .map_err(map_api_error)?;
    Ok(Json(response))
}
