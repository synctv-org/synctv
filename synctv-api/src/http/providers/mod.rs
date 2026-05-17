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
pub mod common;
pub mod emby;
pub mod live;
pub mod rtmp;
// direct_url module removed: proxy handled by unified_proxy_handler,
// no provider-specific API endpoints needed.

use axum::{
    extract::{Path, RawQuery, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
};
use futures::future::BoxFuture;
use futures::FutureExt;

use synctv_core::provider::proxy::ProxyAction;
use synctv_core::provider::ExecutionControl;

use crate::http::{
    error::{map_api_error, AppResult},
    middleware::RequestMetadata,
    AppError, AppState,
};
use crate::impls::providers::proxy::{resolve_provider_proxy_action, ProviderProxyResolution};
use crate::impls::{ApiError, EndpointRateLimitCategory};

fn set_default_cache_control(
    mut response: axum::response::Response,
    value: &'static str,
) -> axum::response::Response {
    if !response.headers().contains_key(header::CACHE_CONTROL) {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static(value),
        );
    }
    response
}

/// Execute a `ProxyAction` returned by a provider's `ProviderProxy::resolve_proxy`.
///
/// Translates the abstract action into concrete `synctv-proxy` calls.
pub(crate) async fn execute_proxy_action(
    proxy_http_client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    action: ProxyAction,
    _client_headers: &axum::http::HeaderMap,
    request_control: Option<&ExecutionControl>,
) -> crate::http::error::AppResult<axum::response::Response> {
    match action {
        ProxyAction::LiveFlv { .. }
        | ProxyAction::LiveHlsPlaylist { .. }
        | ProxyAction::LiveHlsSegment { .. } => Err(AppError::internal(
            "live proxy actions must execute with application state".to_string(),
        )),
        ProxyAction::FetchAndForward {
            url,
            headers,
            range_header,
        } => {
            let cfg = synctv_proxy::ProxyConfig {
                ssrf_guard,
                client: proxy_http_client,
                url: &url,
                provider_headers: &headers,
                range_header: range_header.as_deref(),
                request_control,
                upstream_header_timeout: Some(synctv_proxy::DEFAULT_UPSTREAM_HEADER_TIMEOUT),
            };
            synctv_proxy::proxy_fetch_and_forward(cfg, &synctv_proxy::NoopMetrics)
                .await
                .map_err(map_proxy_execution_error)
        }
        ProxyAction::M3u8Rewrite {
            url,
            headers,
            proxy_base,
            proxy_url_claims,
        } => {
            if proxy_url_claims.is_some() {
                return Err(AppError::internal(
                    "signed M3U8 proxy actions require application state".to_string(),
                ));
            }
            synctv_proxy::proxy_m3u8_and_rewrite_with_control(
                proxy_http_client,
                ssrf_guard,
                &url,
                &headers,
                &proxy_base,
                request_control,
            )
            .await
            .map(|response| set_default_cache_control(response, "no-cache, no-store"))
            .map_err(map_proxy_execution_error)
        }
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

pub(crate) async fn execute_proxy_action_with_state(
    state: &AppState,
    action: ProxyAction,
    client_headers: &axum::http::HeaderMap,
    request_control: Option<&ExecutionControl>,
) -> crate::http::error::AppResult<axum::response::Response> {
    execute_proxy_action_with_state_for_method(
        state,
        action,
        client_headers,
        request_control,
        Method::GET,
    )
    .await
}

pub(crate) async fn execute_proxy_action_with_state_for_method(
    state: &AppState,
    action: ProxyAction,
    client_headers: &axum::http::HeaderMap,
    request_control: Option<&ExecutionControl>,
    method: Method,
) -> crate::http::error::AppResult<axum::response::Response> {
    match action {
        ProxyAction::LiveFlv { .. }
        | ProxyAction::LiveHlsPlaylist { .. }
        | ProxyAction::LiveHlsSegment { .. } => {
            if method != Method::GET {
                return Err(proxy_method_not_allowed());
            }
            live::execute_live_stream_action(state, action, None).await
        }
        ProxyAction::FetchAndForward {
            url,
            headers,
            range_header,
        } => {
            let cache_enabled = state.proxy_slice_cache.config().enabled;
            let proxy_control = proxy_execution_control(request_control);

            if method == Method::HEAD {
                if should_use_proxy_cache(cache_enabled) {
                    return synctv_proxy::slice_cache::proxy_head_with_cache_enabled_with_control_and_timeout(
                        &state.proxy_slice_cache,
                        cache_enabled,
                        range_header.as_deref(),
                        &url,
                        &headers,
                        proxy_control.as_ref(),
                        Some(synctv_proxy::DEFAULT_UPSTREAM_HEADER_TIMEOUT),
                    )
                    .await
                    .map_err(map_proxy_execution_error);
                }

                let cfg = synctv_proxy::ProxyConfig {
                    ssrf_guard: &state.ssrf_guard,
                    client: &state.proxy_http_client,
                    url: &url,
                    provider_headers: &headers,
                    range_header: range_header.as_deref(),
                    request_control: proxy_control.as_ref(),
                    upstream_header_timeout: Some(synctv_proxy::DEFAULT_UPSTREAM_HEADER_TIMEOUT),
                };
                return synctv_proxy::proxy_head_and_forward(cfg)
                    .await
                    .map_err(map_proxy_execution_error);
            }

            if method != Method::GET {
                return Err(proxy_method_not_allowed());
            }

            if should_use_proxy_cache(cache_enabled) {
                return synctv_proxy::slice_cache::proxy_with_cache_enabled_with_control_and_timeout(
                    &state.proxy_slice_cache,
                    cache_enabled,
                    range_header.as_deref(),
                    &url,
                    &headers,
                    proxy_control.as_ref(),
                    Some(synctv_proxy::DEFAULT_UPSTREAM_HEADER_TIMEOUT),
                )
                .await
                .map_err(map_proxy_execution_error);
            }

            execute_proxy_action(
                &state.proxy_http_client,
                &state.ssrf_guard,
                ProxyAction::FetchAndForward {
                    url,
                    headers,
                    range_header,
                },
                client_headers,
                proxy_control.as_ref(),
            )
            .await
        }
        ProxyAction::M3u8Rewrite {
            url,
            headers,
            proxy_base,
            proxy_url_claims,
        } => {
            if method != Method::GET {
                return Err(proxy_method_not_allowed());
            }
            let proxy_control = proxy_execution_control(request_control);
            if let Some(claims) = proxy_url_claims {
                let signing_key = state.shared_api_runtime.proxy_signing_key.clone();
                synctv_proxy::proxy_m3u8_and_rewrite_with_control_and_mapper(
                    synctv_proxy::M3u8RewriteConfig {
                        client: &state.proxy_http_client,
                        ssrf_guard: &state.ssrf_guard,
                        url: &url,
                        provider_headers: &headers,
                        proxy_base: &proxy_base,
                        request_control: proxy_control.as_ref(),
                        upstream_header_timeout: Some(
                            synctv_proxy::DEFAULT_UPSTREAM_HEADER_TIMEOUT,
                        ),
                    },
                    move |proxy_base, target_url| {
                        let signed_query =
                            signing_key.build_signed_query_with_target_url(&claims, target_url);
                        format!("{proxy_base}?{signed_query}")
                    },
                )
                .await
                .map(|response| set_default_cache_control(response, "no-cache, no-store"))
                .map_err(map_proxy_execution_error)
            } else {
                execute_proxy_action(
                    &state.proxy_http_client,
                    &state.ssrf_guard,
                    ProxyAction::M3u8Rewrite {
                        url,
                        headers,
                        proxy_base,
                        proxy_url_claims: None,
                    },
                    client_headers,
                    proxy_control.as_ref(),
                )
                .await
            }
        }
        other => {
            if method != Method::GET {
                return Err(proxy_method_not_allowed());
            }
            let proxy_control = proxy_execution_control(request_control);
            execute_proxy_action(
                &state.proxy_http_client,
                &state.ssrf_guard,
                other,
                client_headers,
                proxy_control.as_ref(),
            )
            .await
        }
    }
}

const fn should_use_proxy_cache(cache_enabled: bool) -> bool {
    cache_enabled
}

fn proxy_method_not_allowed() -> AppError {
    AppError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "Proxy action does not support this HTTP method",
    )
}

/// CORS preflight handler for provider proxy routes.
///
/// This must follow the same origin allowlist as the main HTTP router instead
/// of returning a wildcard response, otherwise browser preflight succeeds for
/// origins that the actual API would reject.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        options,
        path = "/api/providers/proxy/{provider_name}/{sub_path}",
        tag = "Provider Proxy",
        params(
            ("provider_name" = String, Path, description = "Provider backend name"),
            ("sub_path" = String, Path, description = "Provider-specific proxy path")
        ),
        responses(
            (status = 204, description = "CORS preflight accepted"),
            (status = 403, description = "Origin is not allowed", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
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
/// 3. Revalidate current user/room/member access
/// 4. Resolve provider and execute proxy action
///
/// Proxy handling is cancellation-driven.
///
/// Important timeout model:
/// - Proxy routes do not inherit a whole-request/unary timeout budget.
/// - Optional timeouts, if configured inside `synctv-proxy`, apply only to a
///   single upstream HTTP hop up to response headers.
/// - Response bodies are never timed out here; they stop only on cancellation.
/// - Slice-cache proxying may perform multiple upstream hops, and each hop is
///   independent rather than sharing one end-to-end deadline.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/proxy/{provider_name}/{sub_path}",
        tag = "Provider Proxy",
        params(
            ("provider_name" = String, Path, description = "Provider backend name"),
            ("sub_path" = String, Path, description = "Provider-specific proxy path"),
            ("sig" = String, Query, description = "HMAC signature for the proxy URL"),
            ("exp" = i64, Query, description = "Unix timestamp when the proxy URL expires")
        ),
        responses(
            (status = 200, description = "Proxied provider response"),
            (status = 400, description = "Invalid proxy request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Invalid or expired proxy signature", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Proxy access denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Provider proxy target not found", body = crate::openapi::ErrorResponseDoc),
            (status = 502, description = "Upstream provider error", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub(crate) fn unified_proxy_handler(
    Path(path): Path<crate::proto::providers::common::ProviderProxyPathRequest>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> BoxFuture<'static, AppResult<axum::response::Response>> {
    execute_unified_proxy_handler(
        path,
        state,
        request_meta.0.with_timeout(None),
        headers,
        raw_query,
        Method::GET,
    )
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        head,
        path = "/api/providers/proxy/{provider_name}/{sub_path}",
        tag = "Provider Proxy",
        params(
            ("provider_name" = String, Path, description = "Provider backend name"),
            ("sub_path" = String, Path, description = "Provider-specific proxy path"),
            ("sig" = String, Query, description = "HMAC signature for the proxy URL"),
            ("exp" = i64, Query, description = "Unix timestamp when the proxy URL expires")
        ),
        responses(
            (status = 200, description = "Proxied provider response headers"),
            (status = 400, description = "Invalid proxy request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Invalid or expired proxy signature", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Proxy access denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Provider proxy target not found", body = crate::openapi::ErrorResponseDoc),
            (status = 405, description = "Proxy action does not support HEAD", body = crate::openapi::ErrorResponseDoc),
            (status = 502, description = "Upstream provider error", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub(crate) fn unified_proxy_head_handler(
    Path(path): Path<crate::proto::providers::common::ProviderProxyPathRequest>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> BoxFuture<'static, AppResult<axum::response::Response>> {
    execute_unified_proxy_handler(
        path,
        state,
        request_meta.0.with_timeout(None),
        headers,
        raw_query,
        Method::HEAD,
    )
}

fn proxy_execution_control(request_control: Option<&ExecutionControl>) -> Option<ExecutionControl> {
    request_control.map(|control| ExecutionControl::from_parts(None, control.cancellation_token()))
}

fn build_proxy_action_control(
    action_cancellation: tokio_util::sync::CancellationToken,
) -> ExecutionControl {
    ExecutionControl::from_parts(None, action_cancellation)
}

fn execute_unified_proxy_handler(
    path: crate::proto::providers::common::ProviderProxyPathRequest,
    state: AppState,
    request_meta: crate::impls::RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
    method: Method,
) -> BoxFuture<'static, AppResult<axum::response::Response>> {
    async move {
        let query_str = raw_query.0.unwrap_or_default();

        let request_executor = state.shared_api_runtime.request_executor.clone();
        let resolve_request_meta = request_meta.clone();
        let state_for_resolution = state.clone();
        let query_str_for_resolution = query_str.clone();
        let headers_for_resolution = headers.clone();

        let (action, action_cancellation) = request_executor
            .execute_public_with_control(
                &resolve_request_meta,
                EndpointRateLimitCategory::Streaming,
                move |request_control| async move {
                    let action = resolve_provider_proxy_action(ProviderProxyResolution {
                        path,
                        query_string: &query_str_for_resolution,
                        request_headers: &headers_for_resolution,
                        public_id_codec: &state_for_resolution.shared_api_runtime.public_id_codec,
                        proxy_signing_key: &state_for_resolution
                            .shared_api_runtime
                            .proxy_signing_key,
                        proxy_provider_registry: &state_for_resolution
                            .shared_api_runtime
                            .proxy_provider_registry,
                        provider_stores: state_for_resolution
                            .shared_api_runtime
                            .provider_stores
                            .as_ref(),
                        proxy_services: &state_for_resolution.shared_api_runtime.proxy_services,
                        user_service: &state_for_resolution.user_service,
                        request_control: &request_control,
                    })
                    .await?;

                    Ok::<_, ApiError>((action, request_control.cancellation_token()))
                },
            )
            .await
            .map_err(map_api_error)?;

        let action_control = build_proxy_action_control(action_cancellation);
        action_control
            .check_active()
            .map_err(|err| app_error_from_control(&err))?;

        match action {
            ProxyAction::LiveFlv { .. }
            | ProxyAction::LiveHlsPlaylist { .. }
            | ProxyAction::LiveHlsSegment { .. } => {
                if method != Method::GET {
                    return Err(proxy_method_not_allowed());
                }
                live::execute_live_stream_action(&state, action, Some(query_str.as_str())).await
            }
            other @ ProxyAction::FetchAndForward { .. } => {
                execute_proxy_action_with_state_for_method(
                    &state,
                    other,
                    &headers,
                    Some(&action_control),
                    method,
                )
                .await
            }
            other => {
                if method != Method::GET {
                    return Err(proxy_method_not_allowed());
                }
                execute_proxy_action_with_state_for_method(
                    &state,
                    other,
                    &headers,
                    Some(&action_control),
                    method,
                )
                .await
            }
        }
    }
    .boxed()
}

fn app_error_from_control(err: &synctv_common::ExecutionControlError) -> AppError {
    match err {
        synctv_common::ExecutionControlError::Cancelled => AppError::service_unavailable(),
        synctv_common::ExecutionControlError::DeadlineExceeded => AppError::new(
            StatusCode::REQUEST_TIMEOUT,
            synctv_common::messages::REQUEST_TIMED_OUT,
        ),
    }
}

fn map_proxy_execution_error(err: anyhow::Error) -> AppError {
    match synctv_proxy::proxy_error_kind(&err) {
        Some(synctv_proxy::ProxyErrorKind::Cancelled | synctv_proxy::ProxyErrorKind::Timeout) => {
            AppError::new(StatusCode::REQUEST_TIMEOUT, err.to_string())
        }
        Some(
            synctv_proxy::ProxyErrorKind::Connection
            | synctv_proxy::ProxyErrorKind::BodyTooLarge
            | synctv_proxy::ProxyErrorKind::Upstream,
        ) => AppError::new(StatusCode::BAD_GATEWAY, err.to_string()),
        Some(synctv_proxy::ProxyErrorKind::Ssrf) => {
            AppError::forbidden("Proxy target is not allowed by SSRF policy")
        }
        Some(synctv_proxy::ProxyErrorKind::RangeNotSatisfiable) => {
            let mut app_error = AppError::new(StatusCode::RANGE_NOT_SATISFIABLE, err.to_string());
            if let Some(total_size) = synctv_proxy::proxy_range_not_satisfiable_total_size(&err) {
                if let Ok(value) = HeaderValue::from_str(&format!("bytes */{total_size}")) {
                    app_error = app_error.with_header(header::CONTENT_RANGE, value);
                }
            }
            app_error
        }
        Some(synctv_proxy::ProxyErrorKind::InvalidRequest) => {
            AppError::bad_request(err.to_string())
        }
        Some(synctv_proxy::ProxyErrorKind::Other) | None => AppError::from(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;
    use bytes::Bytes;
    use std::collections::HashMap;
    use std::sync::Arc;
    use synctv_core::cache::{KeyBuilder, UsernameCache};
    use synctv_core::config::PasswordComplexityConfig;
    use synctv_core::models::{RoomStatus, SignupMethod, UserStatus};
    use synctv_core::provider::error::ProviderError;
    use synctv_core::provider::proxy::{ProxyProviderRegistry, ProxyRequestContext};
    use synctv_core::provider::{
        AlistProvider, BilibiliProvider, DirectUrlProvider, EmbyProvider, LiveProxyProvider,
        ProviderProxy, ProviderSet, RtmpProvider,
    };
    use synctv_core::proxy_signature::{ProxySigningKey, ProxyUrlClaims};
    use synctv_core::repository::UserRepository;
    use synctv_core::service::SettingsRegistry;
    use synctv_core::service::{
        AuditService, ContentFilter, InMemoryTokenBlacklistStore, RateLimitConfig, RateLimiter,
        RemoteProviderManager, RoomService, UserService,
    };
    use synctv_core_testing::postgres::create_test_pool;
    use synctv_proxy::slice_cache::SliceCacheConfig;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

    struct HeaderAbsent(&'static str);

    impl Match for HeaderAbsent {
        fn matches(&self, request: &Request) -> bool {
            !request.headers.contains_key(self.0)
        }
    }

    fn test_request_metadata() -> RequestMetadata {
        RequestMetadata(crate::impls::RequestMetadata::new(
            crate::impls::TransportProtocol::Http,
        ))
    }

    #[test]
    fn test_provider_proxy_path_request_deserializes_proto_field_names() {
        let req: crate::proto::providers::common::ProviderProxyPathRequest = serde_json::from_str(
            r#"{"provider_name":"test_provider","sub_path":"v1/media/stream.m3u8"}"#,
        )
        .expect("deserialize path request");

        assert_eq!(req.provider_name, "test_provider");
        assert_eq!(req.sub_path, "v1/media/stream.m3u8");
    }

    #[test]
    fn test_set_default_cache_control_inserts_when_missing() {
        let response = axum::response::Response::builder()
            .status(StatusCode::OK)
            .body(axum::body::Body::empty())
            .expect("response");

        let response = set_default_cache_control(response, "no-cache, no-store");
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-cache, no-store")
        );
    }

    #[test]
    fn test_set_default_cache_control_preserves_existing_value() {
        let response = axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CACHE_CONTROL, "public, max-age=60")
            .body(axum::body::Body::empty())
            .expect("response");

        let response = set_default_cache_control(response, "no-cache, no-store");
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=60")
        );
    }

    async fn start_mock_server_or_skip() -> Option<MockServer> {
        match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => {
                drop(listener);
                Some(MockServer::start().await)
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!(
                    "skipping provider proxy test: mock server cannot bind a local port in this environment"
                );
                None
            }
            Err(error) => panic!("preflight bind for provider proxy test should succeed: {error}"),
        }
    }

    fn mock_public_origin(mock_server: &MockServer) -> String {
        format!("http://cdn.example.com:{}", mock_server.address().port())
    }

    fn mock_public_url(mock_server: &MockServer, path: &str) -> String {
        format!("{}{}", mock_public_origin(mock_server), path)
    }

    fn mock_proxy_client(mock_server: &MockServer) -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("cdn.example.com", *mock_server.address())
            .build()
            .expect("client should build")
    }

    fn test_slice_cache_for_mock(
        config: SliceCacheConfig,
        mock_server: &MockServer,
    ) -> synctv_proxy::slice_cache::SliceCache {
        let client = mock_proxy_client(mock_server);
        synctv_proxy::slice_cache::SliceCache::new_with_client_and_ssrf_guard(
            config,
            client,
            synctv_common::ssrf::SsrfGuard::builder()
                .extra_allowed_host("cdn.example.com".to_string())
                .build(),
        )
    }

    #[tokio::test]
    async fn test_slice_cache_hits_second_range_request() {
        let Some(mock_server) = start_mock_server_or_skip().await else {
            return;
        };
        let total_size: u64 = 10 * 1024 * 1024;
        let slice_body = Bytes::from(vec![0xAB; 2 * 1024 * 1024]);

        Mock::given(method("HEAD"))
            .and(path("/video.mp4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Length", total_size.to_string())
                    .insert_header("Accept-Ranges", "bytes"),
            )
            .expect(0)
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

        let cache = test_slice_cache_for_mock(SliceCacheConfig::default(), &mock_server);
        let url = mock_public_url(&mock_server, "/video.mp4");
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
    async fn test_execute_proxy_action_head_uses_upstream_head_not_get() {
        let Some(mock_server) = start_mock_server_or_skip().await else {
            return;
        };
        let total_size: u64 = 4096;

        Mock::given(method("HEAD"))
            .and(path("/video.mp4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Length", total_size.to_string())
                    .insert_header("Content-Type", "video/mp4")
                    .insert_header("Accept-Ranges", "bytes")
                    .insert_header("ETag", "\"video-v1\""),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/video.mp4"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&mock_server)
            .await;

        let state = test_app_state_with_proxy_cache(
            None,
            Arc::new(test_slice_cache_for_mock(
                SliceCacheConfig {
                    slice_size: 1024,
                    ..Default::default()
                },
                &mock_server,
            )),
        );
        let action = ProxyAction::FetchAndForward {
            url: mock_public_url(&mock_server, "/video.mp4"),
            headers: HashMap::new(),
            range_header: None,
        };

        let response = execute_proxy_action_with_state_for_method(
            &state,
            action,
            &HeaderMap::new(),
            None,
            Method::HEAD,
        )
        .await
        .expect("HEAD proxy action should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("X-Cache-Status").unwrap(), "MISS");
        assert_eq!(
            response.headers().get(header::CONTENT_LENGTH).unwrap(),
            total_size.to_string().as_str()
        );
    }

    #[tokio::test]
    async fn test_execute_proxy_action_ignores_raw_client_range_when_provider_does_not_select_it() {
        let Some(mock_server) = start_mock_server_or_skip().await else {
            return;
        };
        let body = Bytes::from_static(b"full-body");

        Mock::given(method("GET"))
            .and(path("/video.mp4"))
            .and(HeaderAbsent("range"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(body.clone())
                    .insert_header("Content-Length", body.len().to_string()),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/video.mp4"))
            .and(header("Range", "bytes=0-3"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&mock_server)
            .await;

        let client = mock_proxy_client(&mock_server);
        let action = ProxyAction::FetchAndForward {
            url: mock_public_url(&mock_server, "/video.mp4"),
            headers: HashMap::new(),
            range_header: None,
        };
        let mut client_headers = HeaderMap::new();
        client_headers.insert(header::RANGE, "bytes=0-3".parse().unwrap());

        let response = execute_proxy_action(
            &client,
            &synctv_common::ssrf::SsrfGuard::disabled(),
            action,
            &client_headers,
            None,
        )
        .await
        .expect("raw client Range should be ignored unless provider selects it");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_execute_proxy_action_maps_unsatisfiable_range_to_416() {
        let Some(mock_server) = start_mock_server_or_skip().await else {
            return;
        };
        let total_size: u64 = 1024;

        Mock::given(method("GET"))
            .and(path("/video.mp4"))
            .and(header("Range", "bytes=0-1023"))
            .respond_with(
                ResponseTemplate::new(206)
                    .set_body_bytes(vec![0xCD; 1024])
                    .insert_header("Content-Range", format!("bytes 0-1023/{total_size}"))
                    .insert_header("Content-Length", "1024")
                    .insert_header("Accept-Ranges", "bytes"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let state = test_app_state_with_proxy_cache(
            None,
            Arc::new(test_slice_cache_for_mock(
                SliceCacheConfig {
                    slice_size: 1024,
                    ..Default::default()
                },
                &mock_server,
            )),
        );
        let action = ProxyAction::FetchAndForward {
            url: mock_public_url(&mock_server, "/video.mp4"),
            headers: HashMap::new(),
            range_header: Some("bytes=0-1".to_string()),
        };
        execute_proxy_action_with_state_for_method(
            &state,
            action,
            &HeaderMap::new(),
            None,
            Method::GET,
        )
        .await
        .expect("first satisfiable range should populate resource metadata");

        let action = ProxyAction::FetchAndForward {
            url: mock_public_url(&mock_server, "/video.mp4"),
            headers: HashMap::new(),
            range_header: Some("bytes=1024-".to_string()),
        };

        let err = execute_proxy_action_with_state_for_method(
            &state,
            action,
            &HeaderMap::new(),
            None,
            Method::GET,
        )
        .await
        .expect_err("unsatisfiable range should map to HTTP 416");

        assert_eq!(err.status, StatusCode::RANGE_NOT_SATISFIABLE);
        let response = err.into_response();
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            &axum::http::HeaderValue::from_static("bytes */1024")
        );
    }

    #[tokio::test]
    async fn test_execute_m3u8_rewrite_signs_each_segment_target_url() {
        let Some(mock_server) = start_mock_server_or_skip().await else {
            return;
        };

        Mock::given(method("GET"))
            .and(path("/master.m3u8"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("#EXTM3U\nseg-1.ts\nseg-2.ts\n"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let state = test_app_state_with_proxy_cache(
            None,
            Arc::new(test_slice_cache_for_mock(
                SliceCacheConfig::default(),
                &mock_server,
            )),
        );

        let claims = ProxyUrlClaims {
            provider: "alist".to_string(),
            version: "version-1".to_string(),
            room_id: "room-1".to_string(),
            user_id: "user-1".to_string(),
            expires_at: chrono::Utc::now().timestamp() + 300,
            target_url: None,
        };
        let action = ProxyAction::M3u8Rewrite {
            url: format!("{}/master.m3u8", mock_server.uri()),
            headers: HashMap::new(),
            proxy_base: "/api/providers/proxy/alist/version-1".to_string(),
            proxy_url_claims: Some(claims),
        };

        let response = execute_proxy_action_with_state_for_method(
            &state,
            action,
            &HeaderMap::new(),
            None,
            Method::GET,
        )
        .await
        .expect("signed M3U8 proxy action should succeed");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let playlist = String::from_utf8(body.to_vec()).expect("playlist should be utf8");
        let first_segment = playlist
            .lines()
            .find(|line| line.starts_with("/api/providers/proxy/alist/version-1?"))
            .expect("rewritten playlist should contain signed segment URL");
        let query = first_segment
            .split_once('?')
            .map(|(_, query)| query)
            .expect("segment URL should include query");
        let parsed = state
            .shared_api_runtime
            .proxy_signing_key
            .parse_and_verify_query(query, "alist", "version-1")
            .expect("segment query should verify");

        assert_eq!(
            parsed.target_url.as_deref(),
            Some(format!("{}/seg-1.ts", mock_server.uri()).as_str())
        );

        let (prefix, _) = query
            .split_once("&url=")
            .expect("signed segment query should include url");
        let tampered = format!(
            "{prefix}&url={}",
            synctv_proxy::percent_encode(&format!("{}/seg-2.ts", mock_server.uri()))
        );
        assert!(
            state
                .shared_api_runtime
                .proxy_signing_key
                .parse_and_verify_query(&tampered, "alist", "version-1")
                .is_err(),
            "changing the segment target URL must invalidate the signature"
        );
    }

    #[test]
    fn proxy_membership_probe_backend_outage_maps_to_503() {
        let err = map_api_error(
            crate::impls::providers::proxy::map_proxy_membership_probe_error(
                synctv_core::Error::ServiceUnavailable(
                    "membership backend temporarily unavailable".to_string(),
                ),
            ),
        );
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn proxy_membership_probe_authorization_stays_403() {
        let err = map_api_error(
            crate::impls::providers::proxy::map_proxy_membership_probe_error(
                synctv_core::Error::Authorization("Not a member of this room".to_string()),
            ),
        );
        assert_eq!(err.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn proxy_signature_errors_hide_verification_details() {
        let err = AppError::unauthorized(synctv_common::messages::INVALID_PROXY_SIGNATURE);

        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            err.message,
            synctv_common::messages::INVALID_PROXY_SIGNATURE
        );
    }

    #[tokio::test]
    async fn proxy_ssrf_errors_are_forbidden_and_sanitized() {
        let guard = synctv_common::ssrf::SsrfGuard::strict_policy();
        let client = reqwest::Client::new();
        let headers = HashMap::new();
        let proxy_error = synctv_proxy::proxy_fetch_and_forward(
            synctv_proxy::ProxyConfig {
                ssrf_guard: &guard,
                client: &client,
                url: "http://169.254.169.254/latest/meta-data",
                provider_headers: &headers,
                range_header: None,
                request_control: None,
                upstream_header_timeout: None,
            },
            &synctv_proxy::NoopMetrics,
        )
        .await
        .expect_err("strict SSRF policy must block metadata IPs");
        let err = map_proxy_execution_error(proxy_error);

        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(err.message, "Proxy target is not allowed by SSRF policy");
    }

    fn test_app_state_with_proxy_cache(
        settings_registry: Option<Arc<SettingsRegistry>>,
        proxy_slice_cache: Arc<synctv_proxy::slice_cache::SliceCache>,
    ) -> AppState {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
            .expect("lazy pool");
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 128, 60);
        let user_service = Arc::new(UserService::new(
            &pool,
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
        let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
            synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
        )));
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
            user_cache: Arc::new(
                synctv_core::cache::UserCache::local_only(128, 60, 300, "test:user:".to_string())
                    .expect("user cache"),
            ),
            room_service,
            content_filter: ContentFilter::new(),
            provider_instance_manager,
            user_provider_credential_repository: Arc::new(
                synctv_core::repository::UserProviderCredentialRepository::new(pool),
            ),
            providers,
            event_service: None,
            connection_manager: Arc::new(synctv_realtime::sync::ConnectionManager::new(
                synctv_realtime::sync::ConnectionLimits::default(),
            )),
            jwt_service,
            realtime_fanout_service: crate::realtime_fanout::default_realtime_fanout_service(
                None, false,
            ),
            oauth2_service: None,
            passkey_service: None,
            settings_service: None,
            settings_registry,
            email_service: None,
            email_token_service: None,
            publish_key_service: None,
            notification_service: None,
            chat_service: None,
            audit_service: Arc::new(audit_service),
            live_streaming_infrastructure: None,
            rate_limiter: Arc::new(RateLimiter::local_only("test:".to_string())),
            ws_ticket_service: Arc::new(synctv_core::service::WsTicketService::local_only(None)),
            redis_runtime: None,
            shared_provider_stores: None,
            shared_proxy_signing_key: None,
            builtin_stun_url: None,
            webrtc_status: synctv_core::service::WebRtcRuntimeStatus::peer_to_peer_stun_disabled(),
            credential_encryption: None,
            proxy_slice_cache,
            ssrf_guard: synctv_common::ssrf::SsrfGuard::disabled(),
            proxy_http_client: synctv_proxy::build_proxy_http_client(
                synctv_common::ssrf::SsrfGuard::disabled(),
            )
            .expect("proxy HTTP client should build for tests"),
            messaging_rate_limit_config: RateLimitConfig::default(),
            heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
            providers_manager: None,
        })
        .expect("provider test router should build")
        .1
    }

    #[derive(Debug)]
    struct TestProxyProvider;

    #[async_trait]
    impl ProviderProxy for TestProxyProvider {
        async fn resolve_proxy(
            &self,
            _ctx: &ProxyRequestContext<'_>,
        ) -> Result<ProxyAction, ProviderError> {
            Ok(ProxyAction::DirectBody {
                body: b"ok".to_vec(),
                content_type: "text/plain".to_string(),
                status: 200,
            })
        }
    }

    fn make_proxy_test_user(username: &str) -> synctv_core::models::User {
        synctv_core::models::User::new_with_status(
            username.to_string(),
            Some(format!("{username}@example.com")),
            "hashed-password".to_string(),
            SignupMethod::Email,
            UserStatus::Active,
        )
    }

    fn make_proxy_test_state(pool: sqlx::PgPool) -> AppState {
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 128, 60);
        let jwt_service = synctv_core::service::JwtService::new(
            "test-secret-key-for-http-router-tests-minimum-32-chars",
        )
        .expect("jwt");
        let user_service = Arc::new(UserService::new(
            &pool,
            jwt_service.clone(),
            username_cache,
            PasswordComplexityConfig::default(),
            Arc::new(InMemoryTokenBlacklistStore::new(128, 3600, 86400)),
            KeyBuilder::new("test"),
            synctv_core::service::BruteForceProtection::in_memory("test".to_string()),
        ));
        let room_service = Arc::new(RoomService::new(pool.clone(), (*user_service).clone()));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
            synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
        )));
        let providers = ProviderSet {
            alist: Arc::new(AlistProvider::new(provider_instance_manager.clone())),
            bilibili: Arc::new(BilibiliProvider::new(provider_instance_manager.clone())),
            emby: Arc::new(EmbyProvider::new(provider_instance_manager.clone())),
            direct_url: Arc::new(DirectUrlProvider::new()),
            rtmp: Arc::new(RtmpProvider::new()),
            live_proxy: Arc::new(LiveProxyProvider::new()),
        };
        let (audit_service, _audit_handle) = AuditService::new(pool.clone());
        let mut state =
            crate::http::create_router_with_state_from_config(crate::http::RouterConfig {
                config: Arc::new(synctv_core::Config::default()),
                user_service,
                user_cache: Arc::new(
                    synctv_core::cache::UserCache::local_only(
                        128,
                        60,
                        300,
                        "test:user:".to_string(),
                    )
                    .expect("user cache"),
                ),
                room_service: room_service.clone(),
                content_filter: ContentFilter::new(),
                provider_instance_manager,
                user_provider_credential_repository: Arc::new(
                    synctv_core::repository::UserProviderCredentialRepository::new(pool),
                ),
                providers,
                event_service: None,
                connection_manager: Arc::new(synctv_realtime::sync::ConnectionManager::new(
                    synctv_realtime::sync::ConnectionLimits::default(),
                )),
                jwt_service,
                realtime_fanout_service: crate::realtime_fanout::default_realtime_fanout_service(
                    None, false,
                ),
                oauth2_service: None,
                passkey_service: None,
                settings_service: None,
                settings_registry: None,
                email_service: None,
                email_token_service: None,
                publish_key_service: None,
                notification_service: None,
                chat_service: None,
                audit_service: Arc::new(audit_service),
                live_streaming_infrastructure: None,
                rate_limiter: Arc::new(RateLimiter::local_only("test:".to_string())),
                ws_ticket_service: Arc::new(synctv_core::service::WsTicketService::local_only(
                    None,
                )),
                redis_runtime: None,
                shared_provider_stores: None,
                shared_proxy_signing_key: None,
                builtin_stun_url: None,
                webrtc_status:
                    synctv_core::service::WebRtcRuntimeStatus::peer_to_peer_stun_disabled(),
                credential_encryption: None,
                proxy_slice_cache: Arc::new(synctv_proxy::slice_cache::SliceCache::new(
                    SliceCacheConfig::default(),
                )),
                ssrf_guard: synctv_common::ssrf::SsrfGuard::disabled(),
                proxy_http_client: synctv_proxy::build_proxy_http_client(
                    synctv_common::ssrf::SsrfGuard::disabled(),
                )
                .expect("proxy HTTP client should build for tests"),
                messaging_rate_limit_config: RateLimitConfig::default(),
                heartbeat_schedule: crate::impls::HeartbeatSchedule::production(),
                providers_manager: None,
            })
            .expect("provider test router should build")
            .1;

        let registry = ProxyProviderRegistry::new();
        registry.register("test_provider", Arc::new(TestProxyProvider));
        let shared_runtime = Arc::make_mut(&mut state.shared_api_runtime);
        shared_runtime.proxy_provider_registry = Arc::new(registry);
        shared_runtime.proxy_services = Arc::new(synctv_core::provider::proxy::ProxyServices {
            room_service,
            credential_encryption: None,
            credential_repo: Arc::new(
                synctv_core::repository::UserProviderCredentialRepository::new(
                    sqlx::postgres::PgPoolOptions::new()
                        .connect_lazy("postgresql://synctv:synctv@localhost:5432/synctv")
                        .expect("lazy pool"),
                ),
            ),
            provider_access_service: None,
            signing_key: shared_runtime.proxy_signing_key.clone(),
            public_id_codec: shared_runtime.public_id_codec.clone(),
        });
        state
    }

    fn build_proxy_query(
        signing_key: &ProxySigningKey,
        room_id: &str,
        user_id: &str,
        version: &str,
    ) -> String {
        signing_key.build_signed_query(&ProxyUrlClaims {
            provider: "test_provider".to_string(),
            version: version.to_string(),
            room_id: room_id.to_string(),
            user_id: user_id.to_string(),
            expires_at: chrono::Utc::now().timestamp() + 300,
            target_url: None,
        })
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_unified_proxy_handler_rejects_banned_user_even_with_valid_signature() {
        let (_pg, pool) = create_test_pool().await;
        let state = make_proxy_test_state(pool.clone());
        let user_repo = UserRepository::new(pool.clone());

        let owner = user_repo
            .create(&make_proxy_test_user("proxy_owner"))
            .await
            .expect("owner");
        let member = user_repo
            .create(&make_proxy_test_user("proxy_member"))
            .await
            .expect("member");

        let (room, _) = state
            .shared_api_runtime
            .proxy_services
            .room_service
            .create_room(
                "Proxy Room".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room");
        state
            .shared_api_runtime
            .proxy_services
            .room_service
            .join_room(room.id, member.id, None)
            .await
            .expect("join");

        let raw_query = build_proxy_query(
            state.shared_api_runtime.proxy_signing_key.as_ref(),
            &state
                .shared_api_runtime
                .public_id_codec
                .encode_room_id(room.id)
                .unwrap(),
            &state
                .shared_api_runtime
                .public_id_codec
                .encode_user_id(member.id)
                .unwrap(),
            "v1",
        );

        state
            .user_service
            .ban_user_and_cleanup_memberships(&member.id, None, None)
            .await
            .expect("ban user");

        let err = unified_proxy_handler(
            Path(crate::proto::providers::common::ProviderProxyPathRequest {
                provider_name: "test_provider".to_string(),
                sub_path: "v1/media".to_string(),
            }),
            State(state),
            test_request_metadata(),
            HeaderMap::new(),
            RawQuery(Some(raw_query)),
        )
        .await
        .expect_err("banned user must not keep using old proxy URL");

        assert_eq!(err.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_unified_proxy_handler_rejects_closed_room_even_with_valid_signature() {
        let (_pg, pool) = create_test_pool().await;
        let state = make_proxy_test_state(pool.clone());
        let user_repo = UserRepository::new(pool.clone());

        let owner = user_repo
            .create(&make_proxy_test_user("proxy_room_owner"))
            .await
            .expect("owner");
        let member = user_repo
            .create(&make_proxy_test_user("proxy_room_member"))
            .await
            .expect("member");

        let (room, _) = state
            .shared_api_runtime
            .proxy_services
            .room_service
            .create_room(
                "Proxy Closed Room".to_string(),
                String::new(),
                owner.id,
                None,
                None,
            )
            .await
            .expect("room");
        state
            .shared_api_runtime
            .proxy_services
            .room_service
            .join_room(room.id, member.id, None)
            .await
            .expect("join");

        let raw_query = build_proxy_query(
            state.shared_api_runtime.proxy_signing_key.as_ref(),
            &state
                .shared_api_runtime
                .public_id_codec
                .encode_room_id(room.id)
                .unwrap(),
            &state
                .shared_api_runtime
                .public_id_codec
                .encode_user_id(member.id)
                .unwrap(),
            "v1",
        );

        state
            .shared_api_runtime
            .proxy_services
            .room_service
            .update_room_status(&room.id, RoomStatus::Closed)
            .await
            .expect("close room");

        let err = unified_proxy_handler(
            Path(crate::proto::providers::common::ProviderProxyPathRequest {
                provider_name: "test_provider".to_string(),
                sub_path: "v1/media".to_string(),
            }),
            State(state),
            test_request_metadata(),
            HeaderMap::new(),
            RawQuery(Some(raw_query)),
        )
        .await
        .expect_err("closed room must not keep serving old proxy URLs");

        assert_eq!(err.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_should_use_proxy_cache_requires_enabled_cache_only() {
        assert!(should_use_proxy_cache(true));
        assert!(!should_use_proxy_cache(false));
    }

    #[test]
    fn test_build_proxy_action_control_drops_deadline_but_preserves_cancellation() {
        let cancellation = tokio_util::sync::CancellationToken::new();
        let control = build_proxy_action_control(cancellation.clone());

        assert_eq!(control.deadline(), None);
        cancellation.cancel();
        assert!(matches!(
            control.check_active(),
            Err(synctv_common::ExecutionControlError::Cancelled)
        ));
    }

    #[test]
    fn test_proxy_execution_control_drops_deadline_but_preserves_cancellation() {
        let parent = ExecutionControl::from_timeout(Some(std::time::Duration::from_secs(5)));

        let derived = proxy_execution_control(Some(&parent)).expect("derived proxy control");

        assert_eq!(derived.deadline(), None);
        parent.cancel();
        assert!(matches!(
            derived.check_active(),
            Err(synctv_common::ExecutionControlError::Cancelled)
        ));
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
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_METHODS)
                .and_then(|value| value.to_str().ok()),
            Some("GET, HEAD, OPTIONS")
        );
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
                .is_none(),
            "provider proxy preflight should match the main router and avoid credentialed browser requests by default"
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
