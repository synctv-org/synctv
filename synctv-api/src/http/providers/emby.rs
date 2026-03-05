//! Emby Provider HTTP Routes
//!
//! Proxy routes use the `ProviderProxy` trait via a single wildcard handler:
//! - `/proxy/*sub_path` — dispatches to `EmbyProvider::resolve_proxy`
//! - `/thumbnail/{item_id}` — thumbnail proxy (stateless, special endpoint)

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::http::{
    error::AppResult, middleware::AuthUser, provider_common::InstanceQuery, AppError, AppState,
};

use crate::impls::providers::get_provider_binds;

use synctv_core::provider::proxy::ProxyRequestContext;
use synctv_core::provider::MediaProvider;

/// Build Emby HTTP routes
pub fn emby_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/list", post(list))
        .route("/me", post(me))
        .route("/binds", get(binds))
        // Thumbnail proxy endpoint (stateless, no version)
        .route("/thumbnail/{item_id}", get(thumbnail))
        // Wildcard proxy route (dispatches via ProviderProxy trait)
        .route(
            "/proxy/{*sub_path}",
            get(proxy_handler).options(super::proxy_options_preflight),
        )
}

// ------------------------------------------------------------------
// Generic proxy handler (delegates to ProviderProxy trait)
// ------------------------------------------------------------------

/// GET `/proxy/*sub_path` — Generic proxy handler for Emby.
///
/// Delegates to `EmbyProvider::resolve_proxy` which parses the sub_path
/// and returns a `ProxyAction` for the HTTP layer to execute.
async fn proxy_handler(
    _auth: AuthUser,
    Path(sub_path): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<axum::response::Response> {
    let proxy = state
        .emby_provider
        .as_provider_proxy()
        .ok_or_else(|| AppError::not_found("Proxy not supported"))?;
    let store = state.provider_stores.load("emby");
    let ctx = ProxyRequestContext {
        sub_path: &sub_path,
        store: Some(&store),
        proxy_base: "/api/providers/emby/proxy",
    };
    let action = proxy.resolve_proxy(&ctx).await.map_err(AppError::from)?;
    super::execute_proxy_action(action, &headers).await
}

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
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Emby login failed: {}", e);
            AppError::from(e).into_response()
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
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Emby list failed: {}", e);
            AppError::from(e).into_response()
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
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Emby me failed: {}", e);
            AppError::from(e).into_response()
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

            (StatusCode::OK, Json(json!({"binds": emby_binds}))).into_response()
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

// ------------------------------------------------------------------
// Thumbnail proxy handler
// ------------------------------------------------------------------

/// Query parameters for thumbnail endpoint
#[derive(Debug, Deserialize)]
struct ThumbnailQuery {
    #[serde(default)]
    #[allow(dead_code)]
    instance_name: Option<String>,
    #[serde(default)]
    max_height: Option<u32>,
    #[serde(default)]
    max_width: Option<u32>,
    /// Emby host URL (required to build the thumbnail URL)
    host: String,
    /// Emby API token (required for authentication)
    token: String,
}

/// GET /`thumbnail/{item_id`} - Proxy Emby/Jellyfin thumbnail images
///
/// This endpoint fetches thumbnail images from Emby/Jellyfin servers and
/// forwards them to the client while:
/// 1. Validating the Emby host URL for SSRF attacks
/// 2. Injecting the X-Emby-Token authentication header
/// 3. Not exposing the API token to the client
///
/// # Query Parameters
///
/// - `host`: Emby server base URL (e.g., "<https://emby.example.com>")
/// - `token`: Emby API key for authentication
/// - `instance_name`: Optional provider instance name
/// - `max_height`: Optional maximum thumbnail height (default: 300)
/// - `max_width`: Optional maximum thumbnail width
///
/// # Path Parameters
///
/// - `item_id`: Emby item ID to fetch thumbnail for
async fn thumbnail(
    _auth: AuthUser,
    Path(item_id): Path<String>,
    Query(query): Query<ThumbnailQuery>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    tracing::debug!(
        item_id = %item_id,
        host = %query.host,
        max_height = ?query.max_height,
        "Emby thumbnail request"
    );

    // Build the thumbnail URL using Emby's Items endpoint
    // Format: {host}/Items/{item_id}/Images/Primary?maxHeight={max_height}
    let max_height = query.max_height.unwrap_or(300).min(1920); // Cap at reasonable max
    let max_width = query.max_width.unwrap_or(0).min(1920); // Cap at reasonable max

    let thumbnail_path = if max_width > 0 {
        // Both width and height specified
        format!(
            "/Items/{item_id}/Images/Primary?maxHeight={max_height}&maxWidth={max_width}&quality=90"
        )
    } else {
        // Only height specified (default)
        format!("/Items/{item_id}/Images/Primary?maxHeight={max_height}&quality=90")
    };

    let thumbnail_url = format!("{}{}", query.host.trim_end_matches('/'), thumbnail_path);

    tracing::debug!(
        item_id = %item_id,
        thumbnail_url = %thumbnail_url,
        "Fetching Emby thumbnail"
    );

    // Prepare authentication headers
    // Use X-Emby-Token header instead of embedding the token in the URL
    // to avoid credential exposure in logs, browser history, etc.
    let mut provider_headers = std::collections::HashMap::new();
    provider_headers.insert("X-Emby-Token".to_string(), query.token.clone());

    // Configure proxy request
    let cfg = synctv_proxy::ProxyConfig {
        url: &thumbnail_url,
        provider_headers: &provider_headers,
        client_headers: &headers,
    };

    // Fetch and forward the thumbnail image
    match synctv_proxy::proxy_fetch_and_forward(cfg, &synctv_proxy::NoopMetrics).await {
        Ok(response) => response.into_response(),
        Err(e) => {
            tracing::error!(
                item_id = %item_id,
                thumbnail_url = %thumbnail_url,
                error = %e,
                "Failed to fetch Emby thumbnail"
            );
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("Failed to fetch thumbnail: {e}")})),
            )
                .into_response()
        }
    }
}
