//! Public API endpoints
//!
//! Endpoints that can be accessed without authentication.

use axum::{extract::State, response::Json, routing::get, Router};

use crate::http::AppState;
use synctv_proto::client::{GetPublicSettingsResponse, GetServerInfoResponse};

/// Create public API router
pub(crate) fn create_public_router() -> Router<AppState> {
    Router::new()
        .route("/api/public/settings", get(get_public_settings))
        .route("/api/public/server-info", get(get_server_info))
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
) -> Result<Json<GetPublicSettingsResponse>, super::AppError> {
    let response = state
        .shared_api_runtime
        .client_api
        .get_public_settings()
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to load public settings");
            super::AppError::internal_server_error("Failed to load public settings")
        })?;
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
) -> Result<Json<GetServerInfoResponse>, super::AppError> {
    let response = state
        .shared_api_runtime
        .client_api
        .get_server_info()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to load server identity");
            super::AppError::internal_server_error("Failed to load server identity")
        })?;
    Ok(Json(response))
}
