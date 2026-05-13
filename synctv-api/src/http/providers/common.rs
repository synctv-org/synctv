use axum::{
    extract::{Path, State},
    routing::{get, post, put},
    Json, Router,
};
use futures::future::BoxFuture;
use synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT;

use super::super::middleware::RequestMetadata;
use super::super::validation::ProtoQuery;
use super::super::{
    admin_execute::{
        execute_admin_endpoint, execute_admin_endpoint_with_control, request_metadata,
    },
    AppState,
};
use super::super::{error::map_api_error, AppResult};
use crate::impls::{ApiError, EndpointRateLimitCategory};
use crate::proto::providers::common::{
    AddProviderInstanceRequest, AddProviderInstanceResponse, DeleteProviderInstanceRequest,
    DeleteProviderInstanceResponse, DisableProviderInstanceRequest,
    DisableProviderInstanceResponse, EnableProviderInstanceRequest, EnableProviderInstanceResponse,
    ListAvailableProviderInstancesRequest, ListProviderBackendsRequest,
    ListProviderInstancesRequest, ListProviderInstancesResponse, ProviderBackendsResponse,
    ProviderInstanceQuery, ProviderInstancesResponse, ReconnectProviderInstanceRequest,
    ReconnectProviderInstanceResponse, UpdateProviderInstanceRequest,
    UpdateProviderInstanceResponse,
};

pub fn register_common_routes() -> Router<AppState> {
    Router::new()
        .route("/instances/available", get(list_instances))
        .route("/instances", get(list_provider_instances))
        .route("/instances", post(add_provider_instance))
        .route(
            "/instances/{name}",
            put(update_provider_instance).delete(delete_provider_instance),
        )
        .route(
            "/instances/{name}/reconnect",
            post(reconnect_provider_instance),
        )
        .route("/instances/{name}/enable", post(enable_provider_instance))
        .route("/instances/{name}/disable", post(disable_provider_instance))
        .route("/backends/{provider_type}", get(list_backends))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/instances/available",
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
) -> Result<Json<ProviderInstancesResponse>, super::super::AppError> {
    let request_meta = request_metadata(request_meta);
    let api = state.shared_api_runtime.provider_common_api.clone();
    let executor = api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            crate::impls::EndpointRateLimitCategory::Read,
            |_| async move {
                api.list_available_provider_instances(ListAvailableProviderInstancesRequest {})
                    .await
            },
        )
        .await
        .map_err(super::super::error::map_api_error)?;
    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/instances",
        tag = "Provider",
        params(
            ("page" = Option<i32>, Query, description = "Page number"),
            ("page_size" = Option<i32>, Query, description = "Page size"),
            ("provider_type" = Option<String>, Query, description = "Provider type filter"),
            ("search" = Option<String>, Query, description = "Search by name or endpoint"),
            ("enabled" = Option<bool>, Query, description = "Enabled filter"),
            ("tls" = Option<bool>, Query, description = "TLS filter"),
            ("sort_by" = Option<i32>, Query, description = "Sort field enum value"),
            ("sort_direction" = Option<i32>, Query, description = "Sort direction enum value")
        ),
        responses(
            (status = 200, description = "Provider instances", body = ListProviderInstancesResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Admin role required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_provider_instances(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<ListProviderInstancesRequest>,
) -> Result<Json<ListProviderInstancesResponse>, super::super::AppError> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        |state| Ok(state.shared_api_runtime.provider_common_api.clone()),
        move |api, _, _| async move { api.list_provider_instances(req).await },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/instances",
        tag = "Provider",
        request_body = AddProviderInstanceRequest,
        responses(
            (status = 200, description = "Provider instance added", body = AddProviderInstanceResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn add_provider_instance(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<AddProviderInstanceRequest>,
) -> Result<Json<AddProviderInstanceResponse>, super::super::AppError> {
    let resp = execute_admin_endpoint_with_control(
        &state,
        request_meta,
        |state| Ok(state.shared_api_runtime.provider_common_api.clone()),
        move |api, request_control, validated, ctx| async move {
            api.add_provider_instance(req, &validated.user_id, &ctx, Some(&request_control))
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        put,
        path = "/api/providers/instances/{name}",
        tag = "Provider",
        params(("name" = String, Path, description = "Provider instance name")),
        request_body = UpdateProviderInstanceRequest,
        responses(
            (status = 200, description = "Provider instance updated", body = UpdateProviderInstanceResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn update_provider_instance(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<DeleteProviderInstanceRequest>,
    Json(mut req): Json<UpdateProviderInstanceRequest>,
) -> Result<Json<UpdateProviderInstanceResponse>, super::super::AppError> {
    req.name = path.name;
    let resp = execute_admin_endpoint_with_control(
        &state,
        request_meta,
        |state| Ok(state.shared_api_runtime.provider_common_api.clone()),
        move |api, request_control, validated, ctx| async move {
            api.update_provider_instance(req, &validated.user_id, &ctx, Some(&request_control))
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/providers/instances/{name}",
        tag = "Provider",
        params(("name" = String, Path, description = "Provider instance name")),
        responses(
            (status = 200, description = "Provider instance deleted", body = DeleteProviderInstanceResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn delete_provider_instance(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<DeleteProviderInstanceRequest>,
) -> Result<Json<DeleteProviderInstanceResponse>, super::super::AppError> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        |state| Ok(state.shared_api_runtime.provider_common_api.clone()),
        move |api, validated, ctx| async move {
            api.delete_provider_instance(req, &validated.user_id, &ctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/instances/{name}/reconnect",
        tag = "Provider",
        params(("name" = String, Path, description = "Provider instance name")),
        responses(
            (status = 200, description = "Provider instance reconnected", body = ReconnectProviderInstanceResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn reconnect_provider_instance(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<ReconnectProviderInstanceRequest>,
) -> Result<Json<ReconnectProviderInstanceResponse>, super::super::AppError> {
    let resp = execute_admin_endpoint_with_control(
        &state,
        request_meta,
        |state| Ok(state.shared_api_runtime.provider_common_api.clone()),
        move |api, request_control, validated, ctx| async move {
            api.reconnect_provider_instance(req, &validated.user_id, &ctx, Some(&request_control))
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/instances/{name}/enable",
        tag = "Provider",
        params(("name" = String, Path, description = "Provider instance name")),
        responses(
            (status = 200, description = "Provider instance enabled", body = EnableProviderInstanceResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn enable_provider_instance(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<EnableProviderInstanceRequest>,
) -> Result<Json<EnableProviderInstanceResponse>, super::super::AppError> {
    let resp = execute_admin_endpoint_with_control(
        &state,
        request_meta,
        |state| Ok(state.shared_api_runtime.provider_common_api.clone()),
        move |api, request_control, _, _| async move {
            api.enable_provider_instance(req, Some(&request_control))
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/instances/{name}/disable",
        tag = "Provider",
        params(("name" = String, Path, description = "Provider instance name")),
        responses(
            (status = 200, description = "Provider instance disabled", body = DisableProviderInstanceResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn disable_provider_instance(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<DisableProviderInstanceRequest>,
) -> Result<Json<DisableProviderInstanceResponse>, super::super::AppError> {
    let resp = execute_admin_endpoint_with_control(
        &state,
        request_meta,
        |state| Ok(state.shared_api_runtime.provider_common_api.clone()),
        move |api, request_control, _, _| async move {
            api.disable_provider_instance(req, Some(&request_control))
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/backends/{provider_type}",
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
) -> Result<Json<ProviderBackendsResponse>, super::super::AppError> {
    let request_meta = request_metadata(request_meta);
    let api = state.shared_api_runtime.provider_common_api.clone();
    let executor = api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            crate::impls::EndpointRateLimitCategory::Read,
            |_| async move { api.list_provider_backends(req).await },
        )
        .await
        .map_err(super::super::error::map_api_error)?;
    Ok(Json(response))
}

pub(crate) fn provider_instance_name(
    query: &ProviderInstanceQuery,
) -> Result<Option<&str>, super::super::AppError> {
    crate::impls::providers::common::provider_instance_name_from_query(query)
        .map_err(super::super::error::map_api_error)
}

pub(crate) fn apply_provider_instance_name(
    body_instance_name: &mut String,
    query: &ProviderInstanceQuery,
) -> Result<Option<String>, super::super::AppError> {
    if let Some(query_instance_name) = provider_instance_name(query)? {
        *body_instance_name = query_instance_name.to_string();
    }

    crate::impls::providers::common::provider_instance_name_from_value(body_instance_name)
        .map(|name| name.map(str::to_owned))
        .map_err(super::super::error::map_api_error)
}

pub(crate) fn provider_request_metadata(
    request_meta: RequestMetadata,
) -> crate::impls::RequestMetadata {
    request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT))
}

pub(crate) async fn execute_provider_user_endpoint<T, E, F>(
    state: &AppState,
    request_meta: RequestMetadata,
    category: EndpointRateLimitCategory,
    operation: F,
) -> AppResult<Json<T>>
where
    T: Send + 'static,
    E: Into<ApiError> + Send + 'static,
    F: FnOnce(synctv_core::service::AuthenticatedToken) -> BoxFuture<'static, Result<T, E>>
        + Send
        + 'static,
{
    let request_meta = provider_request_metadata(request_meta);
    let response = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(&request_meta, category, operation)
        .await
        .map_err(map_api_error)?;
    Ok(Json(response))
}

pub(crate) async fn execute_provider_user_endpoint_with_control<T, E, F>(
    state: &AppState,
    request_meta: RequestMetadata,
    category: EndpointRateLimitCategory,
    operation: F,
) -> AppResult<Json<T>>
where
    T: Send + 'static,
    E: Into<ApiError> + Send + 'static,
    F: FnOnce(
            synctv_core::provider::ExecutionControl,
            synctv_core::service::AuthenticatedToken,
        ) -> BoxFuture<'static, Result<T, E>>
        + Send
        + 'static,
{
    let request_meta = provider_request_metadata(request_meta);
    let response = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint_with_control(&request_meta, category, operation)
        .await
        .map_err(map_api_error)?;
    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use std::sync::Arc;
    use synctv_core::cache::{KeyBuilder, UsernameCache};
    use synctv_core::config::PasswordComplexityConfig;
    use synctv_core::models::ProviderInstance;
    use synctv_core::repository::ProviderInstanceRepository;
    use synctv_core::service::{
        auth::JwtService, AuditService, BruteForceProtection, InMemoryTokenBlacklistStore,
        ProvidersManager, RemoteProviderManager, UserService,
    };
    use synctv_core_testing::create_test_pool;
    use synctv_proto::providers::common::ListProviderBackendsRequest;

    fn test_user_service(pool: &sqlx::PgPool) -> UserService {
        UserService::new(
            pool,
            JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!")
                .expect("test JWT service should build"),
            UsernameCache::local_only("test:username:".to_string(), 100, 60),
            PasswordComplexityConfig::default(),
            Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400)),
            KeyBuilder::new("test"),
            BruteForceProtection::in_memory("test".to_string()),
        )
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn provider_common_api_list_backends_includes_local_default_and_enabled_remote_instances()
    {
        let (_postgres, pool) = create_test_pool().await;
        let repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new(repo.clone()));
        let providers_manager = Arc::new(ProvidersManager::new(provider_instance_manager.clone()));
        providers_manager
            .create_builtin_defaults()
            .await
            .expect("built-in providers should initialize");

        let now = Utc::now();
        repo.create(&ProviderInstance {
            name: "alist-remote".to_string(),
            endpoint: "http://provider.example.com:50051".to_string(),
            comment: Some("remote alist backend".to_string()),
            jwt_secret: None,
            custom_ca: None,
            timeout: "10s".to_string(),
            tls: false,
            insecure_tls: false,
            providers: vec!["alist".to_string()],
            enabled: true,
            created_at: now,
            updated_at: now,
        })
        .await
        .expect("remote backend row should persist");

        let (audit_service, _flush_handle) = AuditService::new(pool.clone());
        let api = crate::impls::ProviderCommonApiImpl::new(
            provider_instance_manager,
            Arc::new(test_user_service(&pool)),
            Arc::new(audit_service),
        )
        .with_providers_manager(Some(providers_manager));
        let backends = api
            .list_provider_backends(ListProviderBackendsRequest {
                provider_type: "alist".to_string(),
            })
            .await
            .expect("backend collection should succeed");

        assert_eq!(
            backends.backends,
            vec!["alist".to_string(), "alist-remote".to_string()]
        );
    }
}
