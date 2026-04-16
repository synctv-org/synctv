//! Alist Provider HTTP Routes
//!
//! Provider API endpoints for Alist login, directory listing, etc.
//! Proxy routes are handled by the unified proxy handler in `providers/mod.rs`.

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};

use crate::http::{
    middleware::AuthUser, provider_common::provider_instance_name, validation::ValidatedQuery,
    AppError, AppResult, AppState,
};
use crate::proto::client::ProviderInstanceQuery;
use crate::proto::providers::alist::{BindInfo, GetBindsResponse};

use crate::impls::providers::get_provider_binds;

/// Alist endpoints that perform authentication or credential mutation.
pub fn alist_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
}

/// Alist read/query endpoints.
pub fn alist_read_routes() -> Router<AppState> {
    Router::new()
        .route("/list", post(list))
        .route("/me", post(me))
        .route("/binds", get(binds))
}

// Provider API handlers

/// Login to Alist (persist credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/alist/login",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = crate::proto::providers::alist::LoginRequest,
        responses(
            (status = 200, description = "Alist login succeeded", body = crate::proto::providers::alist::LoginResponse),
            (status = 400, description = "Invalid login request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn login(
    auth: AuthUser,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::alist::LoginRequest>,
) -> AppResult<Json<crate::proto::providers::alist::LoginResponse>> {
    tracing::info!("Alist login request");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.alist_api;
    let resp = api
        .login(&auth.user_id.to_string(), req, instance_name)
        .await
        .map_err(|e| {
            tracing::error!("Alist login failed: {}", e);
            AppError::from(e)
        })?;
    tracing::info!("Alist login successful");
    Ok(Json(resp))
}

/// List Alist directory (uses stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/alist/list",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = crate::proto::providers::alist::ListRequest,
        responses(
            (status = 200, description = "Alist directory listing", body = crate::proto::providers::alist::ListResponse),
            (status = 400, description = "Invalid list request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::alist::ListRequest>,
) -> AppResult<Json<crate::proto::providers::alist::ListResponse>> {
    tracing::info!("Alist list request");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.alist_api;
    let resp = api
        .list(&auth.user_id.to_string(), req, instance_name)
        .await
        .map_err(|e| {
            tracing::error!("Alist list failed: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
}

/// Get Alist user info (uses stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/alist/me",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = crate::proto::providers::alist::GetMeRequest,
        responses(
            (status = 200, description = "Alist account info", body = crate::proto::providers::alist::GetMeResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn me(
    auth: AuthUser,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::alist::GetMeRequest>,
) -> AppResult<Json<crate::proto::providers::alist::GetMeResponse>> {
    tracing::info!("Alist me request");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.alist_api;
    let resp = api
        .get_me(&auth.user_id.to_string(), req, instance_name)
        .await
        .map_err(|e| {
            tracing::error!("Alist me failed: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
}

/// Logout from Alist (delete stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/alist/logout",
        tag = "Provider",
        request_body = crate::proto::providers::alist::LogoutRequest,
        responses(
            (status = 200, description = "Alist credential removed", body = crate::proto::providers::alist::LogoutResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn logout(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<crate::proto::providers::alist::LogoutRequest>,
) -> AppResult<Json<crate::proto::providers::alist::LogoutResponse>> {
    tracing::info!("Alist logout request");

    let api = &state.alist_api;
    let resp = api
        .logout(&auth.user_id.to_string(), req)
        .await
        .map_err(|e| {
            tracing::error!("Alist logout failed: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/alist/binds",
        tag = "Provider",
        params(ProviderInstanceQuery),
        responses(
            (status = 200, description = "Saved Alist credentials", body = GetBindsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 503, description = "Provider bind information unavailable", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn binds(
    auth: crate::http::middleware::AuthUser,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    tracing::info!("Alist binds request for user: {}", auth.user_id);
    let instance_name = provider_instance_name(&query)?;

    let provider_binds = get_provider_binds(
        &state.user_provider_credential_repository,
        &auth.user_id.to_string(),
        synctv_core::provider::AlistProvider::NAME,
        "username",
        instance_name,
    )
    .await
    .map_err(AppError::from)?;

    let alist_binds: Vec<_> = provider_binds
        .into_iter()
        .map(|b| BindInfo {
            id: b.id,
            host: b.host,
            username: b.label_value,
            created_at: b.created_at,
        })
        .collect();

    Ok(Json(GetBindsResponse { binds: alist_binds }))
}
