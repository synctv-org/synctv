//! Common Provider Route Utilities
//!
//! HTTP layer for provider routes - thin wrappers around impls layer

use axum::{extract::{Path, State}, routing::get, Json, Router, response::IntoResponse};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use synctv_core::provider::ProviderError;

use super::AppState;
use super::middleware::AuthUser;

/// Register common provider routes
///
/// Routes:
/// - GET /instances - List all available provider instances
/// - GET /`backends/:provider_type` - List available backends for a provider type
pub fn register_common_routes() -> Router<AppState> {
    Router::new()
        .route("/instances", get(list_instances))
        .route("/backends/{provider_type}", get(list_backends))
}

/// List all available provider instances
async fn list_instances(_auth: AuthUser, State(state): State<AppState>) -> impl IntoResponse {
    let instances = state.provider_instance_manager.list().await;

    Json(json!({
        "instances": instances
    }))
}

/// List available backends for a given provider type (bilibili/alist/emby)
async fn list_backends(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(provider_type): Path<String>,
) -> impl IntoResponse {
    let instances = match state.provider_instance_manager.get_all_instances().await {
        Ok(all) => all
            .into_iter()
            .filter(|i| i.enabled && i.providers.iter().any(|p| p == &provider_type))
            .map(|i| i.name)
            .collect::<Vec<_>>(),
        Err(_) => vec![],
    };

    Json(json!({
        "backends": instances
    }))
}

/// Convert `ProviderError` to HTTP response
pub fn error_response(e: ProviderError) -> (StatusCode, Json<serde_json::Value>) {
    let (status, message, details) = match &e {
        ProviderError::NetworkError(msg) => (StatusCode::BAD_GATEWAY, msg.clone(), msg.clone()),
        ProviderError::ApiError(msg) => (StatusCode::BAD_GATEWAY, msg.clone(), msg.clone()),
        ProviderError::ParseError(msg) => (StatusCode::BAD_REQUEST, msg.clone(), msg.clone()),
        ProviderError::InvalidConfig(msg) => (StatusCode::BAD_REQUEST, msg.clone(), msg.clone()),
        ProviderError::NotFound => (StatusCode::NOT_FOUND, "Resource not found".to_string(), "Resource not found".to_string()),
        ProviderError::InstanceNotFound(msg) => (StatusCode::NOT_FOUND, msg.clone(), msg.clone()),
        _ => {
            tracing::error!(error = %e, "Internal provider error");
            (StatusCode::INTERNAL_SERVER_ERROR, "Provider error".to_string(), "An internal error occurred".to_string())
        }
    };

    let body = json!({
        "error": message,
        "details": details
    });

    (status, Json(body))
}

/// Convert String error to HTTP response (for implementation layer errors)
#[must_use] 
pub fn parse_provider_error(error_msg: &str) -> ProviderError {
    // Parse common error patterns and convert to ProviderError
    let lower = error_msg.to_lowercase();

    if lower.contains("network") || lower.contains("connection") {
        ProviderError::NetworkError(error_msg.to_string())
    } else if lower.contains("not found") {
        ProviderError::NotFound
    } else if lower.contains("parse") || lower.contains("invalid") {
        ProviderError::ParseError(error_msg.to_string())
    } else {
        // Unauthorized, authentication, or any other error
        ProviderError::ApiError(error_msg.to_string())
    }
}

/// Extract `instance_name` from query parameter
#[derive(Debug, Deserialize)]
pub struct InstanceQuery {
    #[serde(default)]
    pub instance_name: Option<String>,
}

impl InstanceQuery {
    #[must_use]
    pub fn as_deref(&self) -> Option<&str> {
        self.instance_name.as_deref()
    }
}
