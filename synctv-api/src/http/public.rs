//! Public API endpoints
//!
//! Endpoints that can be accessed without authentication.

use axum::{extract::State, response::Json, routing::get, Router};

use crate::http::AppState;

/// Create public API router
pub fn create_public_router() -> Router<AppState> {
    Router::new().route("/api/public/settings", get(get_public_settings))
}

/// Get public server settings
///
/// This endpoint can be called without authentication and returns
/// public server configuration that clients need to know.
pub async fn get_public_settings(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, super::AppError> {
    let response = state.client_api.get_public_settings().map_err(|e| {
        tracing::error!(error = %e, "Failed to load public settings");
        super::AppError::internal_server_error("Failed to load public settings")
    })?;
    let value = serde_json::to_value(response).map_err(|e| {
        tracing::error!(error = %e, "Failed to serialize public settings");
        super::AppError::internal_server_error("Failed to serialize public settings")
    })?;
    Ok(Json(value))
}
