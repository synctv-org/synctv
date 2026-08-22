//! Public facade for SyncTV API services.

pub use synctv_api_common::*;
pub use synctv_api_grpc::{
    build_axum_router, serve, AdminServiceImpl, ClientServiceImpl, ClientServiceOptions,
    ClusterAuthInterceptor, GrpcServerOptions,
};
#[cfg(feature = "web-ui")]
pub use synctv_api_http::web_ui_fallback;
pub use synctv_api_http::{
    create_health_router, create_metrics_router, create_router_from_options,
    create_router_from_shared_state, create_router_with_state_from_options, extract_client_ip,
    hsts_header, liveness_check, map_api_error, security_headers_middleware,
    start_proxy_cache_lifecycle, websocket_handler, AuthMethod,
};
