//! Emby Provider HTTP Routes
//!
//! Provider API endpoints for Emby login, listing, etc.
//! Proxy routes (including thumbnail) are handled by the unified proxy handler
//! in `providers/mod.rs` via `EmbyProvider::resolve_proxy`.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

use crate::http::{middleware::AuthUser, provider_common::InstanceQuery, AppError, AppState};

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
async fn login(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::emby::LoginRequest>,
) -> impl IntoResponse {
    tracing::info!("Emby login request");

    let api = &state.emby_api;

    match api
        .login(&auth.user_id.to_string(), req, query.as_deref())
        .await
    {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Emby login failed: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// List Emby library items
async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::emby::ListRequest>,
) -> impl IntoResponse {
    tracing::info!("Emby list request");

    let api = &state.emby_api;

    match api
        .list(&auth.user_id.to_string(), req, query.as_deref())
        .await
    {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Emby list failed: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Get Emby user info
async fn me(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::emby::GetMeRequest>,
) -> impl IntoResponse {
    tracing::info!("Emby me request");

    let api = &state.emby_api;

    match api
        .get_me(&auth.user_id.to_string(), req, query.as_deref())
        .await
    {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Emby me failed: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Logout from Emby (delete stored credential)
async fn logout(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<crate::proto::providers::emby::LogoutRequest>,
) -> impl IntoResponse {
    tracing::info!("Emby logout request");

    let api = &state.emby_api;

    match api.logout(&auth.user_id.to_string(), req).await {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Emby logout failed: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Get Emby binds (saved credentials)
async fn binds(
    auth: crate::http::middleware::AuthUser,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    tracing::info!("Emby binds request for user: {}", auth.user_id);

    let provider_binds = get_provider_binds(
        &state.user_provider_credential_repository,
        &auth.user_id.to_string(),
        synctv_core::provider::EmbyProvider::NAME,
        "emby_user_id",
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to query credentials: {}", e);
        AppError::internal_server_error("Failed to query credentials")
    })?;

    let emby_binds: Vec<_> = provider_binds
        .into_iter()
        .map(|b| {
            json!({
                "id": b.id,
                "host": b.host,
                "user_id": b.label_value,
                "created_at": b.created_at_str,
            })
        })
        .collect();

    Ok((StatusCode::OK, Json(json!({"binds": emby_binds}))).into_response())
}
