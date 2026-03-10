//! Alist Provider HTTP Routes
//!
//! Provider API endpoints for Alist login, directory listing, etc.
//! Proxy routes are handled by the unified proxy handler in `providers/mod.rs`.

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

// ------------------------------------------------------------------
// Provider API handlers
// ------------------------------------------------------------------

/// Login to Alist (persist credential)
async fn login(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::alist::LoginRequest>,
) -> impl IntoResponse {
    tracing::info!("Alist login request");

    let api = &state.alist_api;

    match api
        .login(&auth.user_id.to_string(), req, query.as_deref())
        .await
    {
        Ok(resp) => {
            tracing::info!("Alist login successful");
            (StatusCode::OK, Json(json!(resp))).into_response()
        }
        Err(e) => {
            tracing::error!("Alist login failed: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// List Alist directory (uses stored credential)
async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::alist::ListRequest>,
) -> impl IntoResponse {
    tracing::info!("Alist list request");

    let api = &state.alist_api;

    match api
        .list(&auth.user_id.to_string(), req, query.as_deref())
        .await
    {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Alist list failed: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Get Alist user info (uses stored credential)
async fn me(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::alist::GetMeRequest>,
) -> impl IntoResponse {
    tracing::info!("Alist me request");

    let api = &state.alist_api;

    match api
        .get_me(&auth.user_id.to_string(), req, query.as_deref())
        .await
    {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Alist me failed: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Logout from Alist (delete stored credential)
async fn logout(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<crate::proto::providers::alist::LogoutRequest>,
) -> impl IntoResponse {
    tracing::info!("Alist logout request");

    let api = &state.alist_api;

    match api.logout(&auth.user_id.to_string(), req).await {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Alist logout failed: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Get Alist binds (saved credentials)
async fn binds(
    auth: crate::http::middleware::AuthUser,
    State(state): State<AppState>,
) -> impl IntoResponse {
    tracing::info!("Alist binds request for user: {}", auth.user_id);

    match get_provider_binds(
        &state.user_provider_credential_repository,
        &auth.user_id.to_string(),
        synctv_core::provider::AlistProvider::NAME,
        "username",
    )
    .await
    {
        Ok(provider_binds) => {
            let alist_binds: Vec<_> = provider_binds
                .into_iter()
                .map(|b| {
                    json!({
                        "id": b.id,
                        "host": b.host,
                        "username": b.label_value,
                        "created_at": b.created_at_str,
                    })
                })
                .collect();

            (StatusCode::OK, Json(json!({"binds": alist_binds}))).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to query credentials: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to query credentials"})),
            )
                .into_response()
        }
    }
}
