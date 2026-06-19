//! Provider HTTP Routes
//!
//! Provider-specific HTTP endpoints for parse, browse, proxy, etc.
//!
//! Each provider module exports a `{name}_routes()` function that returns
//! an Axum Router with all the provider's HTTP endpoints.
//!
//! Playback-provider routes live under `/api/playback-providers`.

pub mod alist;
pub mod bilibili;
pub mod common;
pub mod emby;
pub mod live;
pub mod playback_provider;
pub mod rtmp;

use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
};

use synctv_core::provider::playback_transport::PlaybackTransportAction;
use synctv_core::provider::ExecutionControl;

use crate::http::{AppError, AppState};

#[cfg(test)]
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

/// Execute a provider-specific playback-provider action as an HTTP response.
pub(crate) async fn execute_playback_transport(
    proxy_http_client: &reqwest::Client,
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
    action: PlaybackTransportAction,
    request_control: Option<&ExecutionControl>,
) -> crate::http::error::AppResult<axum::response::Response> {
    match action {
        PlaybackTransportAction::LiveFlv { .. }
        | PlaybackTransportAction::LiveHlsPlaylist { .. }
        | PlaybackTransportAction::LiveHlsSegment { .. } => Err(AppError::internal_server_error(
            "live playback transport actions must execute with application state".to_string(),
        )),
        PlaybackTransportAction::FetchAndForward {
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
        PlaybackTransportAction::M3u8Rewrite { .. } => Err(AppError::internal_server_error(
            "M3U8 rewrite actions require provider-specific route context".to_string(),
        )),
        PlaybackTransportAction::DirectBody {
            body,
            content_type,
            status,
        } => {
            if !(100..=599).contains(&status) {
                return Err(AppError::internal_server_error(format!(
                    "provider returned invalid direct body status code {status}"
                )));
            }
            let status_code = axum::http::StatusCode::from_u16(status).map_err(|error| {
                AppError::internal_server_error(format!(
                    "provider returned invalid direct body status code {status}: {error}"
                ))
            })?;
            let content_type = HeaderValue::from_str(&content_type).map_err(|error| {
                AppError::internal_server_error(format!(
                    "provider returned invalid direct body content type: {error}"
                ))
            })?;
            let mut response = axum::response::Response::new(axum::body::Body::from(body));
            *response.status_mut() = status_code;
            response
                .headers_mut()
                .insert(axum::http::header::CONTENT_TYPE, content_type);
            Ok(response)
        }
    }
}

pub(crate) async fn execute_playback_transport_with_state(
    state: &AppState,
    action: PlaybackTransportAction,
    request_control: Option<&ExecutionControl>,
) -> crate::http::error::AppResult<axum::response::Response> {
    execute_playback_transport_with_state_for_method(state, action, request_control, Method::GET)
        .await
}

pub(crate) async fn execute_playback_transport_with_state_for_method(
    state: &AppState,
    action: PlaybackTransportAction,
    request_control: Option<&ExecutionControl>,
    method: Method,
) -> crate::http::error::AppResult<axum::response::Response> {
    match action {
        PlaybackTransportAction::LiveFlv { .. }
        | PlaybackTransportAction::LiveHlsPlaylist { .. }
        | PlaybackTransportAction::LiveHlsSegment { .. } => {
            if method != Method::GET {
                return Err(playback_transport_method_not_allowed());
            }
            live::execute_live_stream_action(state, action, None).await
        }
        other => {
            let runtime = PlaybackTransportRuntime {
                proxy_http_client: &state.proxy_http_client,
                ssrf_guard: &state.ssrf_guard,
                proxy_slice_cache: &state.proxy_slice_cache,
            };
            execute_playback_transport_with_runtime_for_method(
                &runtime,
                other,
                request_control,
                method,
            )
            .await
        }
    }
}

struct PlaybackTransportRuntime<'a> {
    proxy_http_client: &'a reqwest::Client,
    ssrf_guard: &'a synctv_common::ssrf::SsrfGuard,
    proxy_slice_cache: &'a synctv_proxy::slice_cache::SliceCache,
}

async fn execute_playback_transport_with_runtime_for_method(
    runtime: &PlaybackTransportRuntime<'_>,
    action: PlaybackTransportAction,
    request_control: Option<&ExecutionControl>,
    method: Method,
) -> crate::http::error::AppResult<axum::response::Response> {
    match action {
        PlaybackTransportAction::LiveFlv { .. }
        | PlaybackTransportAction::LiveHlsPlaylist { .. }
        | PlaybackTransportAction::LiveHlsSegment { .. } => Err(AppError::internal_server_error(
            "live playback transport actions require application state".to_string(),
        )),
        PlaybackTransportAction::FetchAndForward {
            url,
            headers,
            range_header,
        } => {
            let cache_enabled = runtime.proxy_slice_cache.config().enabled;
            let proxy_control = playback_transport_execution_control(request_control);

            if method == Method::HEAD {
                if cache_enabled {
                    return synctv_proxy::slice_cache::proxy_head_with_cache_enabled_with_control_and_timeout(
                        runtime.proxy_slice_cache,
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
                    ssrf_guard: runtime.ssrf_guard,
                    client: runtime.proxy_http_client,
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
                return Err(playback_transport_method_not_allowed());
            }

            if cache_enabled {
                return synctv_proxy::slice_cache::proxy_with_cache_enabled_with_control_and_timeout(
                    runtime.proxy_slice_cache,
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

            execute_playback_transport(
                runtime.proxy_http_client,
                runtime.ssrf_guard,
                PlaybackTransportAction::FetchAndForward {
                    url,
                    headers,
                    range_header,
                },
                proxy_control.as_ref(),
            )
            .await
        }
        PlaybackTransportAction::M3u8Rewrite { .. } => {
            if method != Method::GET {
                return Err(playback_transport_method_not_allowed());
            }
            Err(AppError::internal_server_error(
                "M3U8 rewrite actions require provider-specific route context".to_string(),
            ))
        }
        other => {
            if method != Method::GET {
                return Err(playback_transport_method_not_allowed());
            }
            let proxy_control = playback_transport_execution_control(request_control);
            execute_playback_transport(
                runtime.proxy_http_client,
                runtime.ssrf_guard,
                other,
                proxy_control.as_ref(),
            )
            .await
        }
    }
}

fn playback_transport_execution_control(
    parent: Option<&ExecutionControl>,
) -> Option<ExecutionControl> {
    parent.map(|control| ExecutionControl::from_parts(None, control.cancellation_token()))
}

pub(crate) fn playback_transport_method_not_allowed() -> AppError {
    AppError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "Playback transport does not support this HTTP method",
    )
}

pub(crate) async fn playback_provider_options_preflight(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> axum::response::Response {
    playback_provider_options_preflight_for_server(&state.config.server, &headers)
}

fn playback_provider_options_preflight_for_server(
    server: &synctv_core::config::ServerConfig,
    headers: &HeaderMap,
) -> axum::response::Response {
    let origin = match headers.get(axum::http::header::ORIGIN) {
        Some(value) => {
            if let Ok(origin) = value.to_str() {
                Some(origin)
            } else {
                let mut response =
                    axum::response::Response::new(axum::body::Body::from("Invalid Origin header"));
                *response.status_mut() = StatusCode::FORBIDDEN;
                return response;
            }
        }
        None => None,
    };
    let cors_config = synctv_proxy::CorsConfig::new(server.cors_allowed_origins.clone());
    synctv_proxy::proxy_options_preflight_with_cors(origin, &cors_config)
}

pub(crate) fn app_error_from_control(err: &synctv_common::ExecutionControlError) -> AppError {
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
        None => AppError::from(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::error::map_api_error;
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;
    use bytes::Bytes;
    use std::collections::HashMap;
    use std::sync::Arc;
    use synctv_proxy::slice_cache::SliceCacheConfig;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

    struct HeaderAbsent(&'static str);

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn app_ok<T>(result: Result<T, crate::http::AppError>) -> TestResult<T> {
        result.map_err(|error| test_error(format!("{error:?}")))
    }

    fn cache_ok<T>(result: anyhow::Result<T>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
    }

    impl Match for HeaderAbsent {
        fn matches(&self, request: &Request) -> bool {
            !request.headers.contains_key(self.0)
        }
    }

    #[test]
    fn test_set_default_cache_control_inserts_when_missing() -> TestResult {
        let response = axum::response::Response::builder()
            .status(StatusCode::OK)
            .body(axum::body::Body::empty())?;

        let response = set_default_cache_control(response, "no-cache, no-store");
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-cache, no-store")
        );
        Ok(())
    }

    #[test]
    fn test_set_default_cache_control_preserves_existing_value() -> TestResult {
        let response = axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CACHE_CONTROL, "public, max-age=60")
            .body(axum::body::Body::empty())?;

        let response = set_default_cache_control(response, "no-cache, no-store");
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=60")
        );
        Ok(())
    }

    async fn start_mock_server_or_skip() -> TestResult<Option<MockServer>> {
        match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => {
                drop(listener);
                Ok(Some(MockServer::start().await))
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                tracing::debug!(
                    error = %error,
                    "Skipping playback provider test because mock server cannot bind a local port"
                );
                Ok(None)
            }
            Err(error) => Err(test_error(format!(
                "preflight bind for playback provider test should succeed: {error}"
            ))),
        }
    }

    fn mock_public_origin(mock_server: &MockServer) -> String {
        format!("http://cdn.example.com:{}", mock_server.address().port())
    }

    fn mock_public_url(mock_server: &MockServer, path: &str) -> String {
        format!("{}{}", mock_public_origin(mock_server), path)
    }

    fn mock_proxy_client(mock_server: &MockServer) -> TestResult<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("cdn.example.com", *mock_server.address())
            .build()?)
    }

    fn test_slice_cache_for_mock(
        config: SliceCacheConfig,
        mock_server: &MockServer,
    ) -> TestResult<synctv_proxy::slice_cache::SliceCache> {
        let client = mock_proxy_client(mock_server)?;
        cache_ok(
            synctv_proxy::slice_cache::SliceCache::new_with_client_and_ssrf_guard(
                config,
                client,
                synctv_common::ssrf::SsrfGuard::builder()
                    .extra_allowed_host("cdn.example.com".to_string())
                    .build(),
            ),
        )
    }

    struct TestPlaybackTransportRuntime {
        proxy_http_client: reqwest::Client,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
        proxy_slice_cache: Arc<synctv_proxy::slice_cache::SliceCache>,
    }

    impl TestPlaybackTransportRuntime {
        fn new(proxy_slice_cache: Arc<synctv_proxy::slice_cache::SliceCache>) -> TestResult<Self> {
            Ok(Self {
                proxy_http_client: cache_ok(synctv_proxy::build_proxy_http_client(
                    synctv_common::ssrf::SsrfGuard::disabled(),
                ))?,
                ssrf_guard: synctv_common::ssrf::SsrfGuard::disabled(),
                proxy_slice_cache,
            })
        }

        fn as_runtime(&self) -> PlaybackTransportRuntime<'_> {
            PlaybackTransportRuntime {
                proxy_http_client: &self.proxy_http_client,
                ssrf_guard: &self.ssrf_guard,
                proxy_slice_cache: &self.proxy_slice_cache,
            }
        }
    }

    #[tokio::test]
    async fn test_slice_cache_hits_second_range_request() -> TestResult {
        let Some(mock_server) = start_mock_server_or_skip().await? else {
            return Ok(());
        };
        let total_size: u64 = 10 * 1024 * 1024;
        let slice_body = Bytes::from(vec![0xAB; 2 * 1024 * 1024]);

        Mock::expect(
            Mock::given(method("HEAD"))
                .and(path("/video.mp4"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("Content-Length", total_size.to_string())
                        .insert_header("Accept-Ranges", "bytes"),
                ),
            0,
        )
        .mount(&mock_server)
        .await;

        Mock::expect(
            Mock::given(method("GET"))
                .and(path("/video.mp4"))
                .and(header("Range", "bytes=0-2097151"))
                .respond_with(
                    ResponseTemplate::new(206)
                        .set_body_bytes(slice_body.clone())
                        .insert_header("Content-Range", format!("bytes 0-2097151/{total_size}"))
                        .insert_header("Content-Length", "2097152"),
                ),
            1,
        )
        .mount(&mock_server)
        .await;

        let cache = test_slice_cache_for_mock(SliceCacheConfig::default(), &mock_server)?;
        let url = mock_public_url(&mock_server, "/video.mp4");
        let headers = HashMap::new();

        let response1 = cache_ok(
            synctv_proxy::slice_cache::proxy_with_cache(
                &cache,
                Some("bytes=0-999"),
                &url,
                &headers,
            )
            .await,
        )?;
        let response2 = cache_ok(
            synctv_proxy::slice_cache::proxy_with_cache(
                &cache,
                Some("bytes=0-999"),
                &url,
                &headers,
            )
            .await,
        )?;

        assert_eq!(
            response1
                .headers()
                .get("X-Cache-Status")
                .ok_or_else(|| test_error("first response should include cache status"))?,
            "MISS"
        );
        assert_eq!(
            response2
                .headers()
                .get("X-Cache-Status")
                .ok_or_else(|| test_error("second response should include cache status"))?,
            "HIT"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_playback_transport_head_uses_upstream_head_not_get() -> TestResult {
        let Some(mock_server) = start_mock_server_or_skip().await? else {
            return Ok(());
        };
        let total_size: u64 = 4096;

        Mock::expect(
            Mock::given(method("HEAD"))
                .and(path("/video.mp4"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("Content-Length", total_size.to_string())
                        .insert_header("Content-Type", "video/mp4")
                        .insert_header("Accept-Ranges", "bytes")
                        .insert_header("ETag", "\"video-v1\""),
                ),
            1,
        )
        .mount(&mock_server)
        .await;

        Mock::expect(
            Mock::given(method("GET"))
                .and(path("/video.mp4"))
                .respond_with(ResponseTemplate::new(500)),
            0,
        )
        .mount(&mock_server)
        .await;

        let delivery_runtime =
            TestPlaybackTransportRuntime::new(Arc::new(test_slice_cache_for_mock(
                SliceCacheConfig {
                    slice_size: 1024,
                    ..Default::default()
                },
                &mock_server,
            )?))?;
        let action = PlaybackTransportAction::FetchAndForward {
            url: mock_public_url(&mock_server, "/video.mp4"),
            headers: HashMap::new(),
            range_header: None,
        };

        let response = app_ok(
            execute_playback_transport_with_runtime_for_method(
                &delivery_runtime.as_runtime(),
                action,
                None,
                Method::HEAD,
            )
            .await,
        )?;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("X-Cache-Status")
                .ok_or_else(|| test_error("HEAD response should include cache status"))?,
            "MISS"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_LENGTH)
                .ok_or_else(|| test_error("HEAD response should include content length"))?,
            total_size.to_string().as_str()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_playback_transport_uses_provider_selected_range() -> TestResult {
        let Some(mock_server) = start_mock_server_or_skip().await? else {
            return Ok(());
        };
        let body = Bytes::from_static(b"full-body");

        Mock::expect(
            Mock::given(method("GET"))
                .and(path("/video.mp4"))
                .and(HeaderAbsent("range"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_bytes(body.clone())
                        .insert_header("Content-Length", body.len().to_string()),
                ),
            1,
        )
        .mount(&mock_server)
        .await;

        Mock::expect(
            Mock::given(method("GET"))
                .and(path("/video.mp4"))
                .and(header("Range", "bytes=0-3"))
                .respond_with(ResponseTemplate::new(500)),
            0,
        )
        .mount(&mock_server)
        .await;

        let client = mock_proxy_client(&mock_server)?;
        let action = PlaybackTransportAction::FetchAndForward {
            url: mock_public_url(&mock_server, "/video.mp4"),
            headers: HashMap::new(),
            range_header: None,
        };
        let response = app_ok(
            execute_playback_transport(
                &client,
                &synctv_common::ssrf::SsrfGuard::disabled(),
                action,
                None,
            )
            .await,
        )?;

        assert_eq!(response.status(), StatusCode::OK);
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_playback_transport_direct_body_sets_status_content_type_and_body(
    ) -> TestResult {
        let action = PlaybackTransportAction::DirectBody {
            body: b"provider-body".to_vec(),
            content_type: "application/vnd.synctv.test+json".to_string(),
            status: 202,
        };

        let response = app_ok(
            execute_playback_transport(
                &reqwest::Client::new(),
                &synctv_common::ssrf::SsrfGuard::disabled(),
                action,
                None,
            )
            .await,
        )?;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .ok_or_else(|| test_error("direct body response should include content type"))?,
            "application/vnd.synctv.test+json"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        assert_eq!(body.as_ref(), b"provider-body");
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_playback_transport_direct_body_rejects_invalid_status() -> TestResult {
        let action = PlaybackTransportAction::DirectBody {
            body: b"provider-body".to_vec(),
            content_type: "text/plain".to_string(),
            status: 999,
        };

        let Err(err) = execute_playback_transport(
            &reqwest::Client::new(),
            &synctv_common::ssrf::SsrfGuard::disabled(),
            action,
            None,
        )
        .await
        else {
            return Err(test_error("invalid provider status should fail"));
        };

        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("invalid direct body status code"));
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_playback_transport_direct_body_rejects_invalid_content_type() -> TestResult
    {
        let action = PlaybackTransportAction::DirectBody {
            body: b"provider-body".to_vec(),
            content_type: "text/plain\r\nx-bad: yes".to_string(),
            status: 200,
        };

        let Err(err) = execute_playback_transport(
            &reqwest::Client::new(),
            &synctv_common::ssrf::SsrfGuard::disabled(),
            action,
            None,
        )
        .await
        else {
            return Err(test_error("invalid provider content type should fail"));
        };

        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("invalid direct body content type"));
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_playback_transport_maps_unsatisfiable_range_to_416() -> TestResult {
        let Some(mock_server) = start_mock_server_or_skip().await? else {
            return Ok(());
        };
        let total_size: u64 = 1024;

        Mock::expect(
            Mock::given(method("GET"))
                .and(path("/video.mp4"))
                .and(header("Range", "bytes=0-1023"))
                .respond_with(
                    ResponseTemplate::new(206)
                        .set_body_bytes(vec![0xCD; 1024])
                        .insert_header("Content-Range", format!("bytes 0-1023/{total_size}"))
                        .insert_header("Content-Length", "1024")
                        .insert_header("Accept-Ranges", "bytes"),
                ),
            1,
        )
        .mount(&mock_server)
        .await;

        let delivery_runtime =
            TestPlaybackTransportRuntime::new(Arc::new(test_slice_cache_for_mock(
                SliceCacheConfig {
                    slice_size: 1024,
                    ..Default::default()
                },
                &mock_server,
            )?))?;
        let action = PlaybackTransportAction::FetchAndForward {
            url: mock_public_url(&mock_server, "/video.mp4"),
            headers: HashMap::new(),
            range_header: Some("bytes=0-1".to_string()),
        };
        app_ok(
            execute_playback_transport_with_runtime_for_method(
                &delivery_runtime.as_runtime(),
                action,
                None,
                Method::GET,
            )
            .await,
        )?;

        let action = PlaybackTransportAction::FetchAndForward {
            url: mock_public_url(&mock_server, "/video.mp4"),
            headers: HashMap::new(),
            range_header: Some("bytes=1024-".to_string()),
        };

        let Err(err) = execute_playback_transport_with_runtime_for_method(
            &delivery_runtime.as_runtime(),
            action,
            None,
            Method::GET,
        )
        .await
        else {
            return Err(test_error("unsatisfiable range should map to HTTP 416"));
        };

        assert_eq!(err.status, StatusCode::RANGE_NOT_SATISFIABLE);
        let response = err.into_response();
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_RANGE)
                .ok_or_else(|| test_error("416 response should include content range"))?,
            &axum::http::HeaderValue::from_static("bytes */1024")
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_generic_transport_rejects_m3u8_without_provider_route_context() -> TestResult {
        let delivery_runtime = TestPlaybackTransportRuntime::new(Arc::new(
            synctv_proxy::slice_cache::SliceCache::new(SliceCacheConfig::default())?,
        ))?;
        let action = PlaybackTransportAction::M3u8Rewrite {
            url: "https://cdn.example.com/master.m3u8".to_string(),
            headers: HashMap::new(),
        };

        let Err(err) = execute_playback_transport_with_runtime_for_method(
            &delivery_runtime.as_runtime(),
            action,
            None,
            Method::GET,
        )
        .await
        else {
            return Err(test_error(
                "generic transport should reject M3U8 actions without provider route context",
            ));
        };

        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        Ok(())
    }

    #[test]
    fn proxy_membership_probe_backend_outage_maps_to_503() {
        let err = map_api_error(
            crate::impls::playback_provider::common::map_playback_provider_membership_probe_error(
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
            crate::impls::playback_provider::common::map_playback_provider_membership_probe_error(
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

    #[test]
    fn test_playback_transport_execution_control_drops_deadline_but_preserves_cancellation(
    ) -> TestResult {
        let parent = ExecutionControl::from_timeout(Some(std::time::Duration::from_secs(5)));

        let derived = playback_transport_execution_control(Some(&parent))
            .ok_or_else(|| test_error("derived playback transport control should exist"))?;

        assert_eq!(derived.deadline(), None);
        parent.cancel();
        assert!(matches!(
            derived.check_active(),
            Err(synctv_common::ExecutionControlError::Cancelled)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn test_proxy_options_preflight_uses_configured_origin_allowlist() -> TestResult {
        let server = synctv_core::config::ServerConfig {
            cors_allowed_origins: vec!["https://app.example.com".to_string()],
            ..synctv_core::config::ServerConfig::default()
        };

        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "https://app.example.com".parse()?);

        let response = playback_provider_options_preflight_for_server(&server, &headers);
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
            "playback provider preflight should match the main router and avoid credentialed browser requests by default"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_proxy_options_preflight_rejects_unconfigured_origin() -> TestResult {
        let server = synctv_core::config::ServerConfig {
            cors_allowed_origins: vec!["https://app.example.com".to_string()],
            ..synctv_core::config::ServerConfig::default()
        };

        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "https://evil.example.com".parse()?);

        let response = playback_provider_options_preflight_for_server(&server, &headers);
        assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN);
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none(),
            "rejected preflight must not advertise a wildcard or echoed origin"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_proxy_options_preflight_rejects_non_utf8_origin() -> TestResult {
        let server = synctv_core::config::ServerConfig::default();

        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            axum::http::HeaderValue::from_bytes(b"https://app.example.com\xff")?,
        );

        let response = playback_provider_options_preflight_for_server(&server, &headers);
        assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN);
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none(),
            "invalid preflight origin must not produce a CORS allow-origin header"
        );
        Ok(())
    }
}
