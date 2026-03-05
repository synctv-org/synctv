//! Provider HTTP Routes
//!
//! Provider-specific HTTP endpoints for parse, browse, proxy, etc.
//!
//! Each provider module exports a `{name}_routes()` function that returns
//! an Axum Router with all the provider's HTTP endpoints.
//!
//! Proxy routes are unified under `/api/providers/proxy/{provider_name}/{*sub_path}`
//! and dispatched via the `ProviderProxy` trait from `synctv-core`.

pub mod alist;
pub mod bilibili;
pub mod emby;
// direct_url module removed: proxy handled by unified_proxy_handler,
// no provider-specific API endpoints needed.

use axum::{
    extract::{Path, RawQuery, State},
    http::HeaderMap,
};

use synctv_core::models::{RoomId, UserId};
use synctv_core::provider::proxy::{ProxyAction, ProxyRequestContext};

use crate::http::{error::AppResult, AppError, AppState};

/// Execute a `ProxyAction` returned by a provider's `ProviderProxy::resolve_proxy`.
///
/// Translates the abstract action into concrete `synctv-proxy` calls.
pub(crate) async fn execute_proxy_action(
    action: ProxyAction,
    client_headers: &axum::http::HeaderMap,
) -> crate::http::error::AppResult<axum::response::Response> {
    match action {
        ProxyAction::FetchAndForward { url, headers } => {
            let cfg = synctv_proxy::ProxyConfig {
                url: &url,
                provider_headers: &headers,
                client_headers,
            };
            synctv_proxy::proxy_fetch_and_forward(cfg, &synctv_proxy::NoopMetrics)
                .await
                .map_err(Into::into)
        }
        ProxyAction::M3u8Rewrite {
            url,
            headers,
            proxy_base,
        } => synctv_proxy::proxy_m3u8_and_rewrite(&url, &headers, &proxy_base)
            .await
            .map_err(Into::into),
        ProxyAction::DirectBody {
            body,
            content_type,
            status,
        } => {
            let status_code =
                axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::OK);
            Ok(axum::response::Response::builder()
                .status(status_code)
                .header(axum::http::header::CONTENT_TYPE, content_type)
                .body(axum::body::Body::from(body))
                .expect("valid response"))
        }
    }
}

/// Wildcard CORS preflight handler for provider proxy routes.
///
/// This is the non-deprecated replacement for `synctv_proxy::proxy_options_preflight`.
/// It uses `proxy_options_preflight_with_cors` with a wildcard `CorsConfig`, which is
/// appropriate for this native-app-only project.
#[allow(clippy::unused_async)]
pub(crate) async fn proxy_options_preflight() -> axum::response::Response {
    static WILDCARD_CONFIG: std::sync::LazyLock<std::sync::Arc<synctv_proxy::CorsConfig>> =
        std::sync::LazyLock::new(|| std::sync::Arc::new(synctv_proxy::CorsConfig::new_wildcard()));
    synctv_proxy::proxy_options_preflight_with_cors(None, WILDCARD_CONFIG.clone()).await
}

/// GET `/api/providers/proxy/{provider_name}/{*sub_path}` — Unified proxy handler.
///
/// Authenticates via HMAC-signed query parameters (no JWT required).
/// The signature embeds room_id, user_id, version, and expiry directly in the URL.
///
/// Flow:
/// 1. Extract version from sub_path (first segment)
/// 2. Parse and verify HMAC signature from query string
/// 3. Verify room membership
/// 4. Resolve provider and execute proxy action
pub(crate) async fn unified_proxy_handler(
    Path((provider_name, sub_path)): Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_str = raw_query.0.as_deref().unwrap_or("");

    // 1. Extract version from sub_path (first segment)
    let version = sub_path.split('/').next().unwrap_or("");

    // 2. Parse and verify HMAC signature from query string
    let claims = state
        .proxy_signing_key
        .parse_and_verify_query(query_str, &provider_name, version)
        .map_err(|e| AppError::unauthorized(format!("Invalid proxy signature: {e}")))?;

    // 3. Room membership verification
    let uid = UserId::from_string(claims.user_id.clone());
    let rid = RoomId::from_string(claims.room_id.clone());
    state
        .proxy_services
        .room_service
        .check_membership(&rid, &uid)
        .await
        .map_err(|_| AppError::forbidden("Not a member of this room"))?;

    // 4. Resolve proxy provider from registry (no hardcoded match)
    let proxy = state
        .proxy_provider_registry
        .get(&provider_name)
        .ok_or_else(|| AppError::not_found("Unknown provider"))?;

    // 5. Build context with verified claims for M3U8 signature propagation
    let store = state.provider_stores.load(&provider_name);
    let proxy_base = format!("/api/providers/proxy/{provider_name}");
    let ctx = ProxyRequestContext {
        sub_path: &sub_path,
        query_string: Some(query_str),
        store: Some(&store),
        proxy_base: &proxy_base,
        services: &state.proxy_services,
        verified_claims: Some(&claims),
    };

    // 7. Resolve and execute
    let action = proxy.resolve_proxy(&ctx).await.map_err(AppError::from)?;
    execute_proxy_action(action, &headers).await
}
