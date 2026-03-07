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
use std::sync::Arc;

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

async fn execute_proxy_action_with_state(
    state: &AppState,
    action: ProxyAction,
    client_headers: &axum::http::HeaderMap,
) -> crate::http::error::AppResult<axum::response::Response> {
    match action {
        ProxyAction::FetchAndForward { url, headers } => {
            let cache_enabled = proxy_cache_enabled(state.settings_registry.as_ref())
                .map_err(|e| AppError::internal(format!("Failed to load proxy cache setting: {e}")))?;
            let range_header = client_headers
                .get(axum::http::header::RANGE)
                .and_then(|value| value.to_str().ok());

            if should_use_proxy_cache(cache_enabled, range_header) {

                return synctv_proxy::slice_cache::proxy_with_cache(
                    &state.proxy_slice_cache,
                    range_header,
                    &url,
                    &headers,
                )
                .await
                .map_err(Into::into);
            }

            execute_proxy_action(ProxyAction::FetchAndForward { url, headers }, client_headers).await
        }
        other => execute_proxy_action(other, client_headers).await,
    }
}

fn proxy_cache_enabled(
    settings_registry: Option<&Arc<synctv_core::service::SettingsRegistry>>,
) -> Result<bool, synctv_core::Error> {
    settings_registry
        .map(|registry| registry.proxy_cache_enable.get())
        .transpose()
        .map(|value: Option<bool>| value.unwrap_or(false))
}

fn should_use_proxy_cache(cache_enabled: bool, range_header: Option<&str>) -> bool {
    cache_enabled && range_header.is_some()
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
    execute_proxy_action_with_state(&state, action, &headers).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use bytes::Bytes;
    use std::collections::HashMap;
    use synctv_core::service::{SettingsRegistry, SettingsService};
    use synctv_core::repository::SettingsRepository;
    use synctv_core_testing::postgres::create_test_pool;
    use synctv_proxy::slice_cache::SliceCacheConfig;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_execute_proxy_action_fetch_and_forward_does_not_cache_by_default() {
        let mock_server = MockServer::start().await;
        let body = Bytes::from_static(b"video-body");

        Mock::given(method("GET"))
            .and(path("/video.mp4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(body.clone())
                    .insert_header("Content-Length", body.len().to_string()),
            )
            .expect(2)
            .mount(&mock_server)
            .await;

        let headers = HeaderMap::new();
        let action = ProxyAction::FetchAndForward {
            url: format!("{}/video.mp4", mock_server.uri()),
            headers: HashMap::new(),
        };

        let response1 = execute_proxy_action(action.clone(), &headers).await.unwrap();
        let response2 = execute_proxy_action(action, &headers).await.unwrap();

        let body1 = to_bytes(response1.into_body(), usize::MAX).await.unwrap();
        let body2 = to_bytes(response2.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body1, body);
        assert_eq!(body2, body);
    }

    #[tokio::test]
    async fn test_slice_cache_hits_second_range_request() {
        let mock_server = MockServer::start().await;
        let total_size: u64 = 10 * 1024 * 1024;
        let slice_body = Bytes::from(vec![0xAB; 2 * 1024 * 1024]);

        Mock::given(method("HEAD"))
            .and(path("/video.mp4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Length", total_size.to_string())
                    .insert_header("Accept-Ranges", "bytes"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/video.mp4"))
            .and(header("Range", "bytes=0-2097151"))
            .respond_with(
                ResponseTemplate::new(206)
                    .set_body_bytes(slice_body.clone())
                    .insert_header("Content-Range", format!("bytes 0-2097151/{total_size}"))
                    .insert_header("Content-Length", "2097152"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let cache = synctv_proxy::slice_cache::SliceCache::new(SliceCacheConfig::default());
        let url = format!("{}/video.mp4", mock_server.uri());
        let headers = HashMap::new();

        let response1 = synctv_proxy::slice_cache::proxy_with_cache(
            &cache,
            Some("bytes=0-999"),
            &url,
            &headers,
        )
        .await
        .unwrap();
        let response2 = synctv_proxy::slice_cache::proxy_with_cache(
            &cache,
            Some("bytes=0-999"),
            &url,
            &headers,
        )
        .await
        .unwrap();

        assert_eq!(
            response1.headers().get("X-Cache-Status").unwrap(),
            "MISS"
        );
        assert_eq!(
            response2.headers().get("X-Cache-Status").unwrap(),
            "HIT"
        );
    }

    #[tokio::test]
    async fn test_proxy_cache_enabled_reads_runtime_setting() {
        let (_pg, pool) = create_test_pool().await;

        let settings_service = Arc::new(SettingsService::new(
            SettingsRepository::new(pool.clone()),
            pool.clone(),
        ));
        settings_service.initialize().await.unwrap();
        let settings_registry = Arc::new(SettingsRegistry::new(settings_service.clone()));
        sqlx::query(
            "INSERT INTO settings (key, group_name, value) VALUES ($1, $2, $3) \
             ON CONFLICT (key) DO NOTHING",
        )
        .bind("proxy.proxy_cache_enable")
        .bind("proxy")
        .bind("false")
        .execute(&pool)
        .await
        .unwrap();
        settings_registry.proxy_cache_enable.set(true).await.unwrap();

        assert!(proxy_cache_enabled(Some(&settings_registry)).unwrap());
        assert!(!proxy_cache_enabled(None).unwrap());
    }

    #[test]
    fn test_should_use_proxy_cache_requires_both_setting_and_range() {
        assert!(should_use_proxy_cache(true, Some("bytes=0-999")));
        assert!(!should_use_proxy_cache(true, None));
        assert!(!should_use_proxy_cache(false, Some("bytes=0-999")));
        assert!(!should_use_proxy_cache(false, None));
    }
}
