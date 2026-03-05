//! Alist Provider HTTP Routes
//!
//! Proxy routes use the `ProviderProxy` trait via a single wildcard handler:
//! - `/proxy/*sub_path` — dispatches to `AlistProvider::resolve_proxy`

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

use crate::http::{
    error::AppResult, middleware::AuthUser, provider_common::InstanceQuery, AppError, AppState,
};

use crate::impls::providers::get_provider_binds;

use synctv_core::provider::proxy::ProxyRequestContext;
use synctv_core::provider::MediaProvider;

/// Build Alist HTTP routes
pub fn alist_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/list", post(list))
        .route("/me", post(me))
        .route("/binds", get(binds))
        // Wildcard proxy route (dispatches via ProviderProxy trait)
        .route(
            "/proxy/{*sub_path}",
            get(proxy_handler).options(super::proxy_options_preflight),
        )
}

// ------------------------------------------------------------------
// Generic proxy handler (delegates to ProviderProxy trait)
// ------------------------------------------------------------------

/// GET `/proxy/*sub_path` — Generic proxy handler for Alist.
///
/// Delegates to `AlistProvider::resolve_proxy` which parses the sub_path
/// and returns a `ProxyAction` for the HTTP layer to execute.
async fn proxy_handler(
    _auth: AuthUser,
    Path(sub_path): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<axum::response::Response> {
    let proxy = state
        .alist_provider
        .as_provider_proxy()
        .ok_or_else(|| AppError::not_found("Proxy not supported"))?;
    let store = state.provider_stores.get("alist");
    let ctx = ProxyRequestContext {
        sub_path: &sub_path,
        store,
        proxy_base: "/api/providers/alist/proxy",
    };
    let action = proxy.resolve_proxy(&ctx).await.map_err(AppError::from)?;
    super::execute_proxy_action(action, &headers).await
}

// ------------------------------------------------------------------
// Existing provider API handlers
// ------------------------------------------------------------------

/// Login to Alist
async fn login(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::alist::LoginRequest>,
) -> impl IntoResponse {
    tracing::info!("Alist login request");

    let api = &state.alist_api;

    match api.login(req, query.as_deref()).await {
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

/// List Alist directory
async fn list(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::alist::ListRequest>,
) -> impl IntoResponse {
    tracing::info!("Alist list request");

    let api = &state.alist_api;

    match api.list(req, query.as_deref()).await {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Alist list failed: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Get Alist user info
async fn me(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::alist::GetMeRequest>,
) -> impl IntoResponse {
    tracing::info!("Alist me request");

    let api = &state.alist_api;

    match api.get_me(req, query.as_deref()).await {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Alist me failed: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Logout from Alist
async fn logout() -> impl IntoResponse {
    tracing::info!("Alist logout request");
    (
        StatusCode::OK,
        Json(json!({"message": "Logout successful"})),
    )
        .into_response()
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
        "alist",
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
