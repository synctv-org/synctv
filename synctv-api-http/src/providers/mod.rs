//! Provider HTTP Routes
//!
//! Provider-specific HTTP endpoints for authentication, discovery, browsing,
//! and pre-add media previews.
//!
//! Each provider module exports a `{name}_routes()` function that returns
//! an Axum Router with all the provider's HTTP endpoints.
//!
//! Playback-provider routes live under `/api/playback-providers`.

pub(crate) mod acfun;
pub(crate) mod alist;
pub(crate) mod bilibili;
pub(crate) mod cctv;
pub(crate) mod cloudreve;
pub(crate) mod common;
pub(crate) mod douyin;
pub(crate) mod douyu;
pub(crate) mod emby;
pub(crate) mod fnos;
pub(crate) mod huya;
pub(crate) mod nextcloud;
pub(crate) mod playback_provider;
pub(crate) mod qnap;
pub(crate) mod seafile;
pub(crate) mod synology;
pub(crate) mod tiktok;
pub(crate) mod truenas;
pub(crate) mod twitch;
pub(crate) mod youtube;

use axum::{
    extract::State,
    http::{HeaderMap, Method, StatusCode},
};

use synctv_core::provider::ExecutionControl;
use synctv_core::provider::PlaybackTransportAction;

use crate::http::{AppError, AppState};

pub(crate) async fn execute_provider_preview_transport(
    state: &AppState,
    action: PlaybackTransportAction,
    request_control: Option<&ExecutionControl>,
) -> crate::http::error::AppResult<axum::response::Response> {
    let stream =
        synctv_api_common::playback_provider::common::playback_transport_action_to_chunk_stream(
            synctv_api_common::playback_provider::common::PlaybackTransportExecutorDeps {
                proxy_signing_key: &state.shared_api_runtime.proxy_signing_key,
                proxy_http_client: &state.proxy_http_client,
                ssrf_guard: &state.ssrf_guard,
                proxy_slice_cache: &state.proxy_slice_cache,
                request_control,
                hls_rewrite: None,
            },
            action,
            false,
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    playback_provider::transport::stream_chunk_http_response(stream, Method::GET).await
}

pub(crate) async fn playback_provider_options_preflight(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> axum::response::Response {
    playback_provider_options_preflight_for_server(&state.runtime_settings.server, &headers)
}

fn playback_provider_options_preflight_for_server(
    server: &synctv_api_common::ApiServerSettings,
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
    synctv_proxy::handle_cors_preflight(origin, &cors_config)
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header;

    type TestResult<T = ()> = anyhow::Result<T>;

    #[test]
    fn playback_provider_preflight_uses_configured_origin_allowlist() -> TestResult {
        let server = synctv_api_common::ApiServerSettings {
            cors_allowed_origins: vec!["https://app.example.com".to_string()],
            ..synctv_api_common::ApiServerSettings::default()
        };
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "https://app.example.com".parse()?);

        let response = playback_provider_options_preflight_for_server(&server, &headers);

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
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
        Ok(())
    }

    #[test]
    fn playback_provider_preflight_rejects_unconfigured_origin() -> TestResult {
        let server = synctv_api_common::ApiServerSettings {
            cors_allowed_origins: vec!["https://app.example.com".to_string()],
            ..synctv_api_common::ApiServerSettings::default()
        };
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "https://other.example.com".parse()?);

        let response = playback_provider_options_preflight_for_server(&server, &headers);

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
        Ok(())
    }

    #[test]
    fn playback_provider_preflight_rejects_non_utf8_origin() -> TestResult {
        let server = synctv_api_common::ApiServerSettings::default();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            axum::http::HeaderValue::from_bytes(b"https://app.example.com\xff")?,
        );

        let response = playback_provider_options_preflight_for_server(&server, &headers);

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
        Ok(())
    }
}
