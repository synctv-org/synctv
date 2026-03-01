//! Provider HTTP Routes
//!
//! Provider-specific HTTP endpoints for parse, browse, proxy, etc.
//!
//! Each provider module exports a `{name}_routes()` function that returns
//! an Axum Router with all the provider's HTTP endpoints.

pub mod alist;
pub mod bilibili;
pub mod direct_url;
pub mod emby;

/// Generate `proxy_stream` and `proxy_m3u8` handler functions for a media provider.
///
/// Parameters:
/// - `$provider_accessor`: field name on `AppState` (e.g. `alist_provider`)
/// - `$provider_name`: string literal used in log messages (e.g. `"Alist"`)
/// - `$proxy_path_prefix`: string literal for the proxy URL base (e.g. `"/api/providers/alist/proxy"`)
macro_rules! provider_proxy_handlers {
    ($provider_accessor:ident, $provider_name:expr, $proxy_path_prefix:expr) => {
        /// GET /`proxy/:room_id/:media_id` - Proxy provider video stream
        async fn proxy_stream(
            auth: crate::http::middleware::AuthUser,
            axum::extract::Path((room_id, media_id)): axum::extract::Path<(String, String)>,
            axum::extract::State(app_state): axum::extract::State<crate::http::AppState>,
            headers: axum::http::HeaderMap,
        ) -> crate::http::error::AppResult<axum::response::Response> {
            let room_id = synctv_core::models::RoomId::from_string(room_id);
            let media_id = synctv_core::models::MediaId::from_string(media_id);

            let resolved_conn = app_state.resolve_redis_conn().await;
            let (url, provider_headers) = crate::impls::provider::resolve_provider_playback_url(
                &auth.user_id,
                &room_id,
                &media_id,
                app_state.$provider_accessor.as_ref(),
                &app_state.room_service,
                resolved_conn.as_ref(),
                app_state.credential_encryption.as_ref(),
            )
            .await
            .map_err(crate::http::error::map_api_error)?;

            tracing::debug!(concat!("Proxying ", $provider_name, " media: {}"), url);

            let cfg = synctv_proxy::ProxyConfig {
                url: &url,
                provider_headers: &provider_headers,
                client_headers: &headers,
            };

            synctv_proxy::proxy_fetch_and_forward(cfg, &synctv_proxy::NoopMetrics)
                .await
                .map_err(Into::into)
        }

        /// GET /`proxy/:room_id/:media_id/m3u8` - Proxy provider M3U8
        async fn proxy_m3u8(
            auth: crate::http::middleware::AuthUser,
            axum::extract::Path((room_id, media_id)): axum::extract::Path<(String, String)>,
            axum::extract::State(app_state): axum::extract::State<crate::http::AppState>,
        ) -> crate::http::error::AppResult<axum::response::Response> {
            let room_id_parsed = synctv_core::models::RoomId::from_string(room_id.clone());
            let media_id_parsed = synctv_core::models::MediaId::from_string(media_id.clone());

            let resolved_conn = app_state.resolve_redis_conn().await;
            let (url, provider_headers) = crate::impls::provider::resolve_provider_playback_url(
                &auth.user_id,
                &room_id_parsed,
                &media_id_parsed,
                app_state.$provider_accessor.as_ref(),
                &app_state.room_service,
                resolved_conn.as_ref(),
                app_state.credential_encryption.as_ref(),
            )
            .await
            .map_err(crate::http::error::map_api_error)?;

            let proxy_base = format!("{}/{}/{}", $proxy_path_prefix, room_id, media_id);

            synctv_proxy::proxy_m3u8_and_rewrite(&url, &provider_headers, &proxy_base)
                .await
                .map_err(Into::into)
        }
    };
}

pub(crate) use provider_proxy_handlers;

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
