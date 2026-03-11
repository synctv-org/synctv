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
pub mod live;
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
    proxy_http_client: &reqwest::Client,
    action: ProxyAction,
    client_headers: &axum::http::HeaderMap,
) -> crate::http::error::AppResult<axum::response::Response> {
    match action {
        ProxyAction::LiveFlv { .. }
        | ProxyAction::LiveHlsPlaylist { .. }
        | ProxyAction::LiveHlsSegment { .. } => Err(AppError::internal(
            "live proxy actions must execute with application state".to_string(),
        )),
        ProxyAction::FetchAndForward { url, headers } => {
            let cfg = synctv_proxy::ProxyConfig {
                client: proxy_http_client,
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
        } => synctv_proxy::proxy_m3u8_and_rewrite(proxy_http_client, &url, &headers, &proxy_base)
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
        ProxyAction::LiveFlv { .. }
        | ProxyAction::LiveHlsPlaylist { .. }
        | ProxyAction::LiveHlsSegment { .. } => {
            live::execute_live_proxy_action(state, action, None).await
        }
        ProxyAction::FetchAndForward { url, headers } => {
            let cache_enabled =
                proxy_cache_enabled(state.settings_registry.as_ref()).map_err(|e| {
                    AppError::internal(format!("Failed to load proxy cache setting: {e}"))
                })?;
            let range_header = client_headers
                .get(axum::http::header::RANGE)
                .and_then(|value| value.to_str().ok());

            if should_use_proxy_cache(cache_enabled, range_header) {
                return synctv_proxy::slice_cache::proxy_with_cache_enabled(
                    &state.proxy_slice_cache,
                    cache_enabled,
                    range_header,
                    &url,
                    &headers,
                )
                .await
                .map_err(Into::into);
            }

            execute_proxy_action(
                &state.proxy_http_client,
                ProxyAction::FetchAndForward { url, headers },
                client_headers,
            )
            .await
        }
        other => execute_proxy_action(&state.proxy_http_client, other, client_headers).await,
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

/// CORS preflight handler for provider proxy routes.
///
/// This must follow the same origin allowlist as the main HTTP router instead
/// of returning a wildcard response, otherwise browser preflight succeeds for
/// origins that the actual API would reject.
pub(crate) async fn proxy_options_preflight(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> axum::response::Response {
    let origin = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    let cors_config = std::sync::Arc::new(synctv_proxy::CorsConfig::new(
        state.config.server.cors_allowed_origins.clone(),
    ));
    synctv_proxy::proxy_options_preflight_with_cors(origin, cors_config).await
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
        .map_err(|e| match e {
            synctv_core::Error::Authorization(_) => {
                AppError::forbidden("Not a member of this room")
            }
            other => {
                AppError::internal_server_error(format!("Failed to check room membership: {other}"))
            }
        })?;

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
    match action {
        ProxyAction::LiveFlv { .. }
        | ProxyAction::LiveHlsPlaylist { .. }
        | ProxyAction::LiveHlsSegment { .. } => {
            live::execute_live_proxy_action(&state, action, Some(query_str)).await
        }
        other => execute_proxy_action_with_state(&state, other, &headers).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header;
    use bytes::Bytes;
    use std::collections::HashMap;
    use std::sync::Arc;
    use synctv_core::cache::{KeyBuilder, NoopCacheL2, UsernameCache};
    use synctv_core::config::PasswordComplexityConfig;
    use synctv_core::provider::{
        AlistProvider, BilibiliProvider, DirectUrlProvider, EmbyProvider, LiveProxyProvider,
        ProviderSet, RtmpProvider,
    };
    use synctv_core::repository::SettingsRepository;
    use synctv_core::service::{
        AuditService, ContentFilter, InMemoryTokenBlacklistStore, RateLimitConfig, RateLimiter,
        RemoteProviderManager, RoomService, UserService,
    };
    use synctv_core::service::{SettingsRegistry, SettingsService};
    use synctv_core_testing::postgres::create_test_pool;
    use synctv_proxy::slice_cache::SliceCacheConfig;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

        assert_eq!(response1.headers().get("X-Cache-Status").unwrap(), "MISS");
        assert_eq!(response2.headers().get("X-Cache-Status").unwrap(), "HIT");
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
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
        settings_registry
            .proxy_cache_enable
            .set(true)
            .await
            .unwrap();

        assert!(proxy_cache_enabled(Some(&settings_registry)).unwrap());
        assert!(!proxy_cache_enabled(None).unwrap());
    }

    fn test_app_state_with_proxy_cache(
        settings_registry: Option<Arc<SettingsRegistry>>,
        proxy_slice_cache: Arc<synctv_proxy::slice_cache::SliceCache>,
    ) -> AppState {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
            .expect("lazy pool");
        let username_cache =
            UsernameCache::new(Arc::new(NoopCacheL2), "test:username:".to_string(), 128, 60);
        let user_service = Arc::new(UserService::new(
            pool.clone(),
            synctv_core::service::JwtService::new(
                "test-secret-key-for-http-router-tests-minimum-32-chars",
            )
            .expect("jwt"),
            username_cache,
            PasswordComplexityConfig::default(),
            Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
            KeyBuilder::new("test"),
            synctv_core::service::BruteForceProtection::in_memory("test".to_string()),
        ));
        let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new(
            Arc::new(synctv_core::repository::ProviderInstanceRepository::new(
                pool.clone(),
            )),
            None,
            None,
            "test:",
        ));
        let providers = ProviderSet {
            alist: Arc::new(AlistProvider::new(provider_instance_manager.clone())),
            bilibili: Arc::new(BilibiliProvider::new(provider_instance_manager.clone())),
            emby: Arc::new(EmbyProvider::new(provider_instance_manager.clone())),
            direct_url: Arc::new(DirectUrlProvider::new()),
            rtmp: Arc::new(RtmpProvider::new()),
            live_proxy: Arc::new(LiveProxyProvider::new()),
        };
        let jwt_service = synctv_core::service::JwtService::new(
            "test-secret-key-for-http-router-tests-minimum-32-chars",
        )
        .expect("jwt");
        let (audit_service, _audit_handle) = AuditService::new(pool.clone());

        crate::http::create_router_with_state_from_config(crate::http::RouterConfig {
            config: Arc::new(synctv_core::Config::default()),
            user_service,
            room_service,
            content_filter: ContentFilter::new(),
            provider_instance_manager,
            user_provider_credential_repository: Arc::new(
                synctv_core::repository::UserProviderCredentialRepository::new(pool),
            ),
            providers,
            cluster_manager: None,
            connection_manager: Arc::new(synctv_cluster::sync::ConnectionManager::new(
                synctv_cluster::sync::ConnectionLimits::default(),
            )),
            jwt_service,
            redis_publish_tx: None,
            oauth2_service: None,
            settings_service: None,
            settings_registry,
            email_service: None,
            email_token_service: None,
            publish_key_service: None,
            notification_service: None,
            chat_service: None,
            audit_service: Arc::new(audit_service),
            live_streaming_infrastructure: None,
            rate_limiter: RateLimiter::in_memory_only("test:".to_string()),
            ws_ticket_service: None,
            redis_conn: None,
            builtin_stun_url: None,
            turn_health_checker: None,
            credential_encryption: None,
            proxy_slice_cache,
            proxy_http_client: synctv_proxy::build_proxy_http_client()
                .expect("proxy HTTP client should build for tests"),
            messaging_rate_limit_config: RateLimitConfig::default(),
            heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
            providers_manager: None,
        })
        .1
    }

    #[tokio::test]
    async fn test_execute_proxy_action_with_state_honors_runtime_cache_toggle_even_when_cache_was_built_disabled(
    ) {
        let mock_server = MockServer::start().await;
        let total_size: u64 = 10 * 1024 * 1024;
        let slice_body = Bytes::from(vec![0xEF; 2 * 1024 * 1024]);

        Mock::given(method("HEAD"))
            .and(path("/runtime-enabled.mp4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Length", total_size.to_string())
                    .insert_header("Accept-Ranges", "bytes"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/runtime-enabled.mp4"))
            .and(header("Range", "bytes=0-2097151"))
            .respond_with(
                ResponseTemplate::new(206)
                    .set_body_bytes(slice_body)
                    .insert_header("Content-Range", format!("bytes 0-2097151/{total_size}"))
                    .insert_header("Content-Length", "2097152"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let (_pg, pool) = create_test_pool().await;
        let settings_service = Arc::new(SettingsService::new(
            SettingsRepository::new(pool.clone()),
            pool.clone(),
        ));
        settings_service.initialize().await.unwrap();
        let settings_registry = Arc::new(SettingsRegistry::new(settings_service));
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
        settings_registry
            .proxy_cache_enable
            .set(true)
            .await
            .unwrap();

        let state = test_app_state_with_proxy_cache(
            Some(settings_registry),
            Arc::new(synctv_proxy::slice_cache::SliceCache::new(
                SliceCacheConfig {
                    enabled: false,
                    ..SliceCacheConfig::default()
                },
            )),
        );

        let action = ProxyAction::FetchAndForward {
            url: format!("{}/runtime-enabled.mp4", mock_server.uri()),
            headers: HashMap::new(),
        };
        let client_headers = axum::http::HeaderMap::from_iter([(
            axum::http::header::RANGE,
            axum::http::HeaderValue::from_static("bytes=0-999"),
        )]);

        let response1 = execute_proxy_action_with_state(&state, action.clone(), &client_headers)
            .await
            .expect("first proxy response");
        let response2 = execute_proxy_action_with_state(&state, action, &client_headers)
            .await
            .expect("second proxy response");

        assert_eq!(response1.headers().get("X-Cache-Status").unwrap(), "MISS");
        assert_eq!(response2.headers().get("X-Cache-Status").unwrap(), "HIT");
    }

    #[test]
    fn test_should_use_proxy_cache_requires_both_setting_and_range() {
        assert!(should_use_proxy_cache(true, Some("bytes=0-999")));
        assert!(!should_use_proxy_cache(true, None));
        assert!(!should_use_proxy_cache(false, Some("bytes=0-999")));
        assert!(!should_use_proxy_cache(false, None));
    }

    #[tokio::test]
    async fn test_proxy_options_preflight_uses_configured_origin_allowlist() {
        let mut state = test_app_state_with_proxy_cache(
            None,
            Arc::new(synctv_proxy::slice_cache::SliceCache::new(
                SliceCacheConfig::default(),
            )),
        );
        let router_config = Arc::make_mut(&mut state.router_config);
        let config = Arc::make_mut(&mut router_config.config);
        config.server.cors_allowed_origins = vec!["https://app.example.com".to_string()];

        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "https://app.example.com".parse().unwrap());

        let response = proxy_options_preflight(State(state), headers).await;
        assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|value| value.to_str().ok()),
            Some("https://app.example.com")
        );
    }

    #[tokio::test]
    async fn test_proxy_options_preflight_rejects_unconfigured_origin() {
        let mut state = test_app_state_with_proxy_cache(
            None,
            Arc::new(synctv_proxy::slice_cache::SliceCache::new(
                SliceCacheConfig::default(),
            )),
        );
        let router_config = Arc::make_mut(&mut state.router_config);
        let config = Arc::make_mut(&mut router_config.config);
        config.server.cors_allowed_origins = vec!["https://app.example.com".to_string()];

        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "https://evil.example.com".parse().unwrap());

        let response = proxy_options_preflight(State(state), headers).await;
        assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN);
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none(),
            "rejected preflight must not advertise a wildcard or echoed origin"
        );
    }
}
