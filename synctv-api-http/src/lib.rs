#![recursion_limit = "256"]

pub mod http;
#[cfg(feature = "openapi")]
pub mod openapi;
pub(crate) mod providers;

#[cfg(feature = "web-ui")]
pub use http::web_ui::fallback as web_ui_fallback;
pub use http::{
    build_app_state, create_health_router, create_metrics_router, create_router_from_options,
    create_router_from_shared_state, create_router_with_state_from_options, extract_client_ip,
    hsts_header, liveness_check, map_api_error, security_headers_middleware,
    start_proxy_cache_lifecycle, websocket_handler, AppError, AppResult, AppState, AuthMethod,
    ProxyCacheLifecycleRuntime, RouterOptions,
};
