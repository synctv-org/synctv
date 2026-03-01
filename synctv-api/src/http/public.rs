//! Public API endpoints
//!
//! Endpoints that can be accessed without authentication.

use axum::{
    extract::State,
    response::{IntoResponse, Json},
    routing::get,
    Router,
};

use crate::http::AppState;

/// Create public API router
pub fn create_public_router() -> Router<AppState> {
    Router::new().route("/api/public/settings", get(get_public_settings))
}

/// Get public server settings
///
/// This endpoint can be called without authentication and returns
/// public server configuration that clients need to know.
pub async fn get_public_settings(State(state): State<AppState>) -> impl IntoResponse {
    match state.client_api.get_public_settings() {
        Ok(response) => Json(serde_json::to_value(response).unwrap_or_default()),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load public settings, returning defaults");
            Json(
                serde_json::to_value(synctv_core::service::PublicSettings::defaults())
                    .unwrap_or_default(),
            )
        }
    }
}
