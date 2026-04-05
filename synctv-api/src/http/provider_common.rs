//! Common Provider Route Utilities
//!
//! HTTP layer for provider routes - thin wrappers around impls layer

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;

use super::middleware::AuthUser;
use super::AppState;

#[derive(serde::Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct ProviderInstancesResponseDoc {
    instances: Vec<String>,
}

#[derive(serde::Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct ProviderBackendsResponseDoc {
    backends: Vec<String>,
}

fn provider_registry_unavailable_error(
    context: &str,
    error: &synctv_core::Error,
) -> super::AppError {
    tracing::error!(operation = context, error = %error, "Provider registry query failed");
    super::AppError::service_unavailable()
}

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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/provider/instances",
        tag = "Provider",
        responses(
            (status = 200, description = "Available provider instances", body = ProviderInstancesResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 503, description = "Provider registry unavailable", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn list_instances(
    _auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<ProviderInstancesResponseDoc>, super::AppError> {
    let instances = state
        .provider_instance_manager
        .list()
        .await
        .map_err(|e| provider_registry_unavailable_error("list_instances", &e))?;

    Ok(Json(ProviderInstancesResponseDoc { instances }))
}

/// List available backends for a given provider type (bilibili/alist/emby)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/provider/backends/{provider_type}",
        tag = "Provider",
        params(
            ("provider_type" = String, Path, description = "Provider type, such as bilibili, alist, or emby")
        ),
        responses(
            (status = 200, description = "Enabled backends for the provider type", body = ProviderBackendsResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 503, description = "Provider registry unavailable", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn list_backends(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(provider_type): Path<String>,
) -> Result<Json<ProviderBackendsResponseDoc>, super::AppError> {
    let instances = state
        .provider_instance_manager
        .find_instances_by_provider(&provider_type)
        .await
        .map_err(|e| {
            tracing::error!(provider_type = %provider_type, error = %e, "Failed to list provider backends");
            provider_registry_unavailable_error("list_backends", &e)
        })?
        .into_iter()
        .map(|i| i.name)
        .collect::<Vec<_>>();

    Ok(Json(ProviderBackendsResponseDoc {
        backends: instances,
    }))
}

/// Extract `instance_name` from query parameter
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams, utoipa::ToSchema))]
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[tokio::test]
    async fn provider_registry_errors_map_to_service_unavailable() {
        let err = synctv_core::Error::Internal("db unavailable".to_string());
        let mapped = provider_registry_unavailable_error("list_backends", &err);
        assert_eq!(mapped.status, StatusCode::SERVICE_UNAVAILABLE);
    }
}
