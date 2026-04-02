//! Public API endpoints
//!
//! Endpoints that can be accessed without authentication.

use axum::{extract::State, response::Json, routing::get, Router};

use crate::http::AppState;
use crate::proto::client::GetPublicSettingsResponse;

/// Create public API router
pub fn create_public_router() -> Router<AppState> {
    Router::new().route("/api/public/settings", get(get_public_settings))
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
            (status = 500, description = "Failed to load settings", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub async fn get_public_settings(
    State(state): State<AppState>,
) -> Result<Json<GetPublicSettingsResponse>, super::AppError> {
    let response = state.client_api.get_public_settings().map_err(|e| {
        tracing::error!(error = %e, "Failed to load public settings");
        super::AppError::internal_server_error("Failed to load public settings")
    })?;
    Ok(Json(response))
}
