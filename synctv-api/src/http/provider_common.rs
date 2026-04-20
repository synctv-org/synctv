//! Common Provider Route Utilities
//!
//! HTTP layer for provider routes - thin wrappers around impls layer

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};

use super::middleware::RequestMetadata;
use super::AppState;
use crate::impls::{ApiError, EndpointRateLimitCategory};
use crate::proto::client::{
    ListProviderBackendsRequest, ProviderBackendsResponse, ProviderInstanceQuery,
    ProviderInstancesResponse,
};

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> Result<Json<ProviderInstancesResponse>, super::AppError> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.client_api.clone();
    let provider_instance_manager = state.provider_instance_manager.clone();
    let instances = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Read,
            |_| async move {
                provider_instance_manager
                    .list()
                    .await
                    .map_err(ApiError::from)
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<ListProviderBackendsRequest>,
) -> Result<Json<ProviderBackendsResponse>, super::AppError> {
    let provider_type = provider_type(&req)?;
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.client_api.clone();
    let provider_instance_manager = state.provider_instance_manager.clone();
    let instances = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Read,
            |_| async move {
                provider_instance_manager
                    .find_instances_by_provider(provider_type)
                    .await
                    .map_err(ApiError::from)
            },
        )
        .await
        .map_err(super::error::map_api_error)?
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
        let err = synctv_core::Error::ServiceUnavailable(
            "Provider configuration service is temporarily unavailable.".to_string(),
        );
        let mapped = crate::http::error::map_api_error(crate::impls::ApiError::from(err));
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
