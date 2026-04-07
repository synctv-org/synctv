//! Common Provider Route Utilities
//!
//! HTTP layer for provider routes - thin wrappers around impls layer

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};

use super::middleware::AuthUser;
use super::AppState;
use crate::proto::client::{
    ListProviderBackendsRequest, ProviderBackendsResponse, ProviderInstanceQuery,
    ProviderInstancesResponse,
};

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
            (status = 200, description = "Available provider instances", body = ProviderInstancesResponse),
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
) -> Result<Json<ProviderInstancesResponse>, super::AppError> {
    let instances = state
        .provider_instance_manager
        .list()
        .await
        .map_err(|e| provider_registry_unavailable_error("list_instances", &e))?;

    Ok(Json(ProviderInstancesResponse { instances }))
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
            (status = 200, description = "Enabled backends for the provider type", body = ProviderBackendsResponse),
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
    Path(req): Path<ListProviderBackendsRequest>,
) -> Result<Json<ProviderBackendsResponse>, super::AppError> {
    let provider_type = provider_type(&req)?;
    let instances = state
        .provider_instance_manager
        .find_instances_by_provider(provider_type)
        .await
        .map_err(|e| {
            tracing::error!(provider_type = %provider_type, error = %e, "Failed to list provider backends");
            provider_registry_unavailable_error("list_backends", &e)
        })?
        .into_iter()
        .map(|i| i.name)
        .collect::<Vec<_>>();

    Ok(Json(ProviderBackendsResponse {
        backends: instances,
    }))
}

pub(crate) fn provider_instance_name(
    query: &ProviderInstanceQuery,
) -> Result<Option<&str>, super::AppError> {
    crate::impls::validate_proto_request(query).map_err(super::error::map_api_error)?;
    Ok((!query.instance_name.is_empty()).then_some(query.instance_name.as_str()))
}

pub(crate) fn provider_type(
    request: &ListProviderBackendsRequest,
) -> Result<&str, super::AppError> {
    crate::impls::validate_proto_request(request).map_err(super::error::map_api_error)?;
    Ok(request.provider_type.as_str())
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

    #[test]
    fn provider_instance_name_allows_empty_query() {
        let query = ProviderInstanceQuery {
            instance_name: String::new(),
        };

        assert_eq!(provider_instance_name(&query).unwrap(), None);
    }

    #[test]
    fn provider_instance_name_rejects_invalid_query_contract() {
        let query = ProviderInstanceQuery {
            instance_name: "bad name".to_string(),
        };

        let err = provider_instance_name(&query).expect_err("query should be invalid");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("instance_name"));
    }

    #[test]
    fn provider_type_accepts_valid_proto_contract() {
        let request = ListProviderBackendsRequest {
            provider_type: "fake_dynamic".to_string(),
        };

        assert_eq!(provider_type(&request).unwrap(), "fake_dynamic");
    }

    #[test]
    fn provider_type_rejects_invalid_proto_contract() {
        let request = ListProviderBackendsRequest {
            provider_type: "bad-name".to_string(),
        };

        let err = provider_type(&request).expect_err("request should be invalid");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("provider_type"));
    }
}
