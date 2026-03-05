//! Provider HTTP Routes
//!
//! Provider-specific HTTP endpoints for parse, browse, proxy, etc.
//!
//! Each provider module exports a `{name}_routes()` function that returns
//! an Axum Router with all the provider's HTTP endpoints.
//!
//! Proxy routes use the `ProviderProxy` trait from `synctv-core` for providers
//! that support it (Bilibili, Alist, Emby). DirectUrl is stateless and handled separately.

pub mod alist;
pub mod bilibili;
pub mod direct_url;
pub mod emby;

use synctv_core::provider::proxy::ProxyAction;

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
