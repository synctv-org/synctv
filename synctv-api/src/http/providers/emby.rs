//! Emby Provider HTTP Routes

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

use crate::http::{AppState, middleware::AuthUser, provider_common::{InstanceQuery, error_response, parse_provider_error}};

use crate::impls::providers::get_provider_binds;

/// Build Emby HTTP routes
pub fn emby_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/list", post(list))
        .route("/me", post(me))
        .route("/binds", get(binds))
        // Provider-specific proxy routes
        .route(
            "/proxy/{room_id}/{media_id}",
            get(proxy_stream).options(synctv_proxy::proxy_options_preflight),
        )
        .route("/proxy/{room_id}/{media_id}/m3u8", get(proxy_m3u8))
}

// ------------------------------------------------------------------
// Proxy handlers (generated via macro to avoid duplication with alist)
// ------------------------------------------------------------------

super::provider_proxy_handlers!(emby_provider, "Emby", "/api/providers/emby/proxy");

// ------------------------------------------------------------------
// Existing provider API handlers
// ------------------------------------------------------------------

/// Login to Emby/Jellyfin (validate API key)
async fn login(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::emby::LoginRequest>,
) -> impl IntoResponse {
    tracing::info!("Emby login request");

    let api = &state.emby_api;

    match api.login(req, query.as_deref()).await {
        Ok(resp) => {
            (StatusCode::OK, Json(json!(resp))).into_response()
        }
        Err(e) => {
            tracing::error!("Emby login failed: {}", e);
            error_response(parse_provider_error(&e)).into_response()
        }
    }
}

/// List Emby library items
async fn list(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::emby::ListRequest>,
) -> impl IntoResponse {
    tracing::info!("Emby list request");

    let api = &state.emby_api;

    match api.list(req, query.as_deref()).await {
        Ok(resp) => {
            (StatusCode::OK, Json(json!(resp))).into_response()
        }
        Err(e) => {
            tracing::error!("Emby list failed: {}", e);
            error_response(parse_provider_error(&e)).into_response()
        }
    }
}

/// Get Emby user info
async fn me(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::emby::GetMeRequest>,
) -> impl IntoResponse {
    tracing::info!("Emby me request");

    let api = &state.emby_api;

    match api.get_me(req, query.as_deref()).await {
        Ok(resp) => {
            (StatusCode::OK, Json(json!(resp))).into_response()
        }
        Err(e) => {
            tracing::error!("Emby me failed: {}", e);
            error_response(parse_provider_error(&e)).into_response()
        }
    }
}

/// Logout from Emby
async fn logout() -> impl IntoResponse {
    tracing::info!("Emby logout request");
    (
        StatusCode::OK,
        Json(json!({"message": "Logout successful"})),
    )
        .into_response()
}

/// Get Emby binds (saved credentials)
async fn binds(
    auth: crate::http::middleware::AuthUser,
    State(state): State<AppState>,
) -> impl IntoResponse {
    tracing::info!("Emby binds request for user: {}", auth.user_id);

    match get_provider_binds(
        &state.user_provider_credential_repository,
        &auth.user_id.to_string(),
        "emby",
        "emby_user_id",
    )
    .await
    {
        Ok(provider_binds) => {
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

            (
                StatusCode::OK,
                Json(json!({"binds": emby_binds})),
            )
                .into_response()
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
