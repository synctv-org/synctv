use axum::{
    extract::{Path, Query, State},
    routing::{get, post, put},
    Json, Router,
};
use futures::future::BoxFuture;

use super::super::middleware::RequestMetadata;
use super::super::{
    admin_execute::{execute_admin_endpoint, execute_admin_endpoint_with_control},
    AppState,
};
use super::super::{error::map_api_error, AppResult};
use crate::impls::{ApiError, EndpointRateLimitCategory};
use std::str::FromStr as _;
use synctv_proto::providers::common::{
    AddProviderInstanceRequest, AddProviderInstanceResponse, DeleteProviderInstanceRequest,
    DeleteProviderInstanceResponse, DisableProviderInstanceRequest,
    DisableProviderInstanceResponse, EnableProviderInstanceRequest, EnableProviderInstanceResponse,
    ListAvailableProviderInstancesRequest, ListProviderBackendsRequest,
    ListProviderInstancesRequest, ListProviderInstancesResponse, ProviderBackendsResponse,
    ProviderInstanceQuery, ProviderInstancesResponse, ReconnectProviderInstanceRequest,
    ReconnectProviderInstanceResponse, UpdateProviderInstanceRequest,
    UpdateProviderInstanceResponse,
};

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderInstancesAvailableQuery {
    provider_type: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderInstancesQuery {
    page: Option<i32>,
    page_size: Option<i32>,
    provider_type: Option<String>,
    search: Option<String>,
    enabled: Option<bool>,
    tls: Option<bool>,
    sort_by: Option<i32>,
    sort_direction: Option<i32>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderBackendsPath {
    provider_type: String,
}

fn source_provider_param(value: Option<&str>) -> Result<i32, super::super::AppError> {
    let Some(raw) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(synctv_proto::source_config::SourceProvider::Unspecified as i32);
    };
    if let Ok(value) = raw.parse::<i32>() {
        return Ok(value);
    }
    let normalized = raw
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|ch| *ch != '-' && *ch != '_')
        .collect::<String>();
    let canonical = match normalized.as_str() {
        "directurl" => "direct_url",
        "bilibili" => "bilibili",
        "alist" => "alist",
        "emby" => "emby",
        "rtmp" => "rtmp",
        "liveproxy" => "live_proxy",
        _ => raw,
    };
    synctv_core::models::SourceProvider::from_str(canonical)
        .map(|provider| crate::impls::source_provider::core_source_provider_to_proto(provider))
        .map_err(|_| super::super::AppError::bad_request("Invalid providerType"))
}

pub(crate) fn register_common_routes() -> Router<AppState> {
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
        .route("/backends/{providerType}", get(list_backends))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/instances/available",
        tag = "Provider",
        params(
            ("providerType" = Option<String>, Query, description = "Provider type filter")
        ),
        responses(
            (status = 200, description = "Available provider instances", body = ProviderInstancesResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 408, description = "Provider registry request timed out", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Provider registry unavailable", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn list_instances(
    request_meta: RequestMetadata,
    Query(query): Query<ProviderInstancesAvailableQuery>,
    State(state): State<AppState>,
) -> Result<Json<ProviderInstancesResponse>, super::super::AppError> {
    let req = ListAvailableProviderInstancesRequest {
        provider_type: source_provider_param(query.provider_type.as_deref())?,
    };
    let request_meta = request_meta.0;
    let api = state.shared_api_runtime.provider_common_api.clone();
    let executor = api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            crate::impls::EndpointRateLimitCategory::Read,
            |_| async move { api.list_available_provider_instances(req).await },
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
            ("pageSize" = Option<i32>, Query, description = "Page size"),
            ("providerType" = Option<String>, Query, description = "Provider type filter"),
            ("search" = Option<String>, Query, description = "Search by name or endpoint"),
            ("enabled" = Option<bool>, Query, description = "Enabled filter"),
            ("tls" = Option<bool>, Query, description = "TLS filter"),
            ("sortBy" = Option<i32>, Query, description = "Sort field enum value"),
            ("sortDirection" = Option<i32>, Query, description = "Sort direction enum value")
        ),
        responses(
            (status = 200, description = "Provider instances", body = ListProviderInstancesResponse),
            (status = 400, description = "Invalid provider instance query", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Admin authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Admin role required", body = synctv_proto::client::ApiErrorResponse),
            (status = 408, description = "Provider instance request timed out", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Provider registry unavailable", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_provider_instances(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Query(query): Query<ProviderInstancesQuery>,
) -> Result<Json<ListProviderInstancesResponse>, super::super::AppError> {
    let req = ListProviderInstancesRequest {
        page: query.page.unwrap_or_default(),
        page_size: query.page_size.unwrap_or_default(),
        provider_type: source_provider_param(query.provider_type.as_deref())?,
        search: query.search.unwrap_or_default(),
        enabled: query.enabled,
        tls: query.tls,
        sort_by: query.sort_by.unwrap_or_default(),
        sort_direction: query.sort_direction.unwrap_or_default(),
    };
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Admin authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Admin role required", body = synctv_proto::client::ApiErrorResponse),
            (status = 408, description = "Provider instance request timed out", body = synctv_proto::client::ApiErrorResponse),
            (status = 409, description = "Provider instance conflict", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Provider registry unavailable", body = synctv_proto::client::ApiErrorResponse)
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Admin authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Admin role required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Provider instance not found", body = synctv_proto::client::ApiErrorResponse),
            (status = 408, description = "Provider instance request timed out", body = synctv_proto::client::ApiErrorResponse),
            (status = 409, description = "Provider instance conflict", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Provider registry unavailable", body = synctv_proto::client::ApiErrorResponse)
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Admin authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Admin role required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Provider instance not found", body = synctv_proto::client::ApiErrorResponse),
            (status = 408, description = "Provider instance request timed out", body = synctv_proto::client::ApiErrorResponse),
            (status = 409, description = "Provider instance conflict", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Provider registry unavailable", body = synctv_proto::client::ApiErrorResponse)
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Admin authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Admin role required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Provider instance not found", body = synctv_proto::client::ApiErrorResponse),
            (status = 408, description = "Provider instance request timed out", body = synctv_proto::client::ApiErrorResponse),
            (status = 409, description = "Provider instance conflict", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Provider registry unavailable", body = synctv_proto::client::ApiErrorResponse)
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Admin authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Admin role required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Provider instance not found", body = synctv_proto::client::ApiErrorResponse),
            (status = 408, description = "Provider instance request timed out", body = synctv_proto::client::ApiErrorResponse),
            (status = 409, description = "Provider instance conflict", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Provider registry unavailable", body = synctv_proto::client::ApiErrorResponse)
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Admin authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Admin role required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Provider instance not found", body = synctv_proto::client::ApiErrorResponse),
            (status = 408, description = "Provider instance request timed out", body = synctv_proto::client::ApiErrorResponse),
            (status = 409, description = "Provider instance conflict", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Provider registry unavailable", body = synctv_proto::client::ApiErrorResponse)
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
        path = "/api/providers/backends/{providerType}",
        tag = "Provider",
        params(
            ("providerType" = String, Path, description = "Provider type, such as bilibili, alist, or emby")
        ),
        responses(
            (status = 200, description = "Enabled backends for the provider type", body = ProviderBackendsResponse),
            (status = 400, description = "Invalid provider backend request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 408, description = "Provider registry request timed out", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Provider registry unavailable", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn list_backends(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ProviderBackendsPath>,
) -> Result<Json<ProviderBackendsResponse>, super::super::AppError> {
    let req = ListProviderBackendsRequest {
        provider_type: source_provider_param(Some(&path.provider_type))?,
    };
    let request_meta = request_meta.0;
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

pub(crate) fn provider_instance_name_from_request_field(
    body_instance_name: &str,
) -> Result<Option<String>, super::super::AppError> {
    crate::impls::providers::common::provider_instance_name_from_value(body_instance_name)
        .map(|name| name.map(str::to_owned))
        .map_err(super::super::error::map_api_error)
}

pub(crate) fn provider_request_metadata(
    request_meta: RequestMetadata,
) -> crate::impls::RequestMetadata {
    request_meta.0
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
    use synctv_core::models::ProviderInstance;
    use synctv_core::repository::ProviderInstanceRepository;
    use synctv_core::service::{
        auth::JwtService, AuditService, BruteForceProtection, InMemoryTokenBlacklistStore,
        ProvidersManager, RemoteProviderManager, UserService,
    };
    use synctv_core_testing::create_test_pool;
    use synctv_proto::providers::common::{
        ListProviderBackendsRequest, UpdateProviderInstanceRequest,
    };

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn core_ok<T>(result: synctv_core::Result<T>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
    }

    #[test]
    fn update_provider_instance_request_overrides_path_name() -> TestResult {
        let mut req: UpdateProviderInstanceRequest = serde_json::from_str(
            r#"{"name":"body-name","endpoint":"https://provider.internal","providers":[3]}"#,
        )?;
        req.name = "alist-main".to_string();

        assert_eq!(req.name, "alist-main");
        assert_eq!(req.endpoint.as_deref(), Some("https://provider.internal"));
        assert_eq!(
            req.providers,
            vec![synctv_proto::source_config::SourceProvider::Alist as i32]
        );
        Ok(())
    }

    #[test]
    fn provider_type_param_accepts_rest_names_and_numbers() -> TestResult {
        assert_eq!(
            super::source_provider_param(Some("alist"))?,
            synctv_proto::source_config::SourceProvider::Alist as i32
        );
        assert_eq!(
            super::source_provider_param(Some("liveProxy"))?,
            synctv_proto::source_config::SourceProvider::LiveProxy as i32
        );
        assert_eq!(
            super::source_provider_param(Some("live_proxy"))?,
            synctv_proto::source_config::SourceProvider::LiveProxy as i32
        );
        assert_eq!(
            super::source_provider_param(Some("3"))?,
            synctv_proto::source_config::SourceProvider::Alist as i32
        );
        assert_eq!(
            super::source_provider_param(None)?,
            synctv_proto::source_config::SourceProvider::Unspecified as i32
        );
        Ok(())
    }

    #[test]
    fn provider_type_param_rejects_unknown_rest_name() {
        assert!(super::source_provider_param(Some("unknown-provider")).is_err());
    }

    fn test_user_service(pool: &sqlx::PgPool) -> TestResult<UserService> {
        Ok(UserService::new_for_tests(
            pool,
            JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!")?,
            UsernameCache::local_only("test:username:".to_string(), 100, 60),
            Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400)),
            KeyBuilder::new("test"),
            BruteForceProtection::in_memory("test".to_string()),
        ))
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn provider_common_api_list_backends_includes_local_default_and_enabled_remote_instances(
    ) -> TestResult {
        let (_postgres, pool) = create_test_pool().await;
        let repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new(repo.clone()));
        let providers_manager = Arc::new(core_ok(ProvidersManager::new(
            provider_instance_manager.clone(),
        ))?);
        core_ok(providers_manager.create_builtin_defaults().await)?;

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
            providers: vec![synctv_core::models::SourceProvider::Alist],
            enabled: true,
            created_at: now,
            updated_at: now,
        })
        .await?;

        let (audit_service, _flush_handle) = AuditService::new(pool.clone());
        let api = crate::impls::ProviderCommonApiImpl::new_with_runtime(
            provider_instance_manager,
            Arc::new(test_user_service(&pool)?),
            Arc::new(audit_service),
            crate::impls::ProviderCommonApiRuntime {
                providers_manager,
                request_executor: Arc::new(crate::test_support::local_request_executor()),
            },
        );
        let backends = api
            .list_provider_backends(ListProviderBackendsRequest {
                provider_type: synctv_proto::source_config::SourceProvider::Alist as i32,
            })
            .await
            .map_err(|error| test_error(format!("{error:?}")))?;

        assert_eq!(
            backends.backends,
            vec!["alist".to_string(), "alist-remote".to_string()]
        );
        Ok(())
    }
}
