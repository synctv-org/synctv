//! Emby Provider HTTP Routes
//!
//! Provider API endpoints for Emby login, listing, etc.
//! Proxy routes (including thumbnail) are handled by the unified proxy handler
//! in `providers/mod.rs` via `EmbyProvider::resolve_proxy`.

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
use crate::proto::providers::emby::{BindInfo, GetBindsResponse};

use crate::impls::providers::get_provider_binds;

/// Emby endpoints that perform authentication or credential mutation.
pub fn emby_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
}

/// Emby read/query endpoints.
pub fn emby_read_routes() -> Router<AppState> {
    Router::new()
        .route("/list", post(list))
        .route("/me", post(me))
        .route("/binds", get(binds))
}

// ------------------------------------------------------------------
// Existing provider API handlers
// ------------------------------------------------------------------

/// Login to Emby/Jellyfin (validate API key and persist credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/emby/login",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = crate::proto::providers::emby::LoginRequest,
        responses(
            (status = 200, description = "Emby login succeeded", body = crate::proto::providers::emby::LoginResponse),
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
    Json(req): Json<crate::proto::providers::emby::LoginRequest>,
) -> AppResult<Json<crate::proto::providers::emby::LoginResponse>> {
    tracing::info!("Emby login request");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.emby_api;
    let resp = api
        .login(&auth.user_id.to_string(), req, instance_name)
        .await
        .map_err(|e| {
            tracing::error!("Emby login failed: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
}

/// List Emby library items
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/emby/list",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = crate::proto::providers::emby::ListRequest,
        responses(
            (status = 200, description = "Emby library listing", body = crate::proto::providers::emby::ListResponse),
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
    Json(req): Json<crate::proto::providers::emby::ListRequest>,
) -> AppResult<Json<crate::proto::providers::emby::ListResponse>> {
    tracing::info!("Emby list request");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.emby_api;
    let resp = api
        .list(&auth.user_id.to_string(), req, instance_name)
        .await
        .map_err(|e| {
            tracing::error!("Emby list failed: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
}

/// Get Emby user info
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/emby/me",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = crate::proto::providers::emby::GetMeRequest,
        responses(
            (status = 200, description = "Emby account info", body = crate::proto::providers::emby::GetMeResponse),
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
    Json(req): Json<crate::proto::providers::emby::GetMeRequest>,
) -> AppResult<Json<crate::proto::providers::emby::GetMeResponse>> {
    tracing::info!("Emby me request");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.emby_api;
    let resp = api
        .get_me(&auth.user_id.to_string(), req, instance_name)
        .await
        .map_err(|e| {
            tracing::error!("Emby me failed: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
}

/// Logout from Emby (delete stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/emby/logout",
        tag = "Provider",
        request_body = crate::proto::providers::emby::LogoutRequest,
        responses(
            (status = 200, description = "Emby credential removed", body = crate::proto::providers::emby::LogoutResponse),
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
    Json(req): Json<crate::proto::providers::emby::LogoutRequest>,
) -> AppResult<Json<crate::proto::providers::emby::LogoutResponse>> {
    tracing::info!("Emby logout request");

    let api = &state.emby_api;
    let resp = api
        .logout(&auth.user_id.to_string(), req)
        .await
        .map_err(|e| {
            tracing::error!("Emby logout failed: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/emby/binds",
        tag = "Provider",
        responses(
            (status = 200, description = "Saved Emby credentials", body = GetBindsResponse),
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
) -> AppResult<Json<GetBindsResponse>> {
    tracing::info!("Emby binds request for user: {}", auth.user_id);

    let provider_binds = get_provider_binds(
        &state.user_provider_credential_repository,
        &auth.user_id.to_string(),
        synctv_core::provider::EmbyProvider::NAME,
        "emby_user_id",
    )
    .await
    .map_err(AppError::from)?;

    let emby_binds: Vec<_> = provider_binds
        .into_iter()
        .map(|b| BindInfo {
            id: b.id,
            host: b.host,
            user_id: b.label_value,
            created_at: b.created_at,
        })
        .collect();

    Ok(Json(GetBindsResponse { binds: emby_binds }))
}
