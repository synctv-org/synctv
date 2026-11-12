//! Public facade for SyncTV API services.

pub use synctv_api_common::*;
pub use synctv_api_grpc::{
    AdminServiceImpl, ClientServiceImpl, ClientServiceOptions, ClusterAuthInterceptor,
    GrpcServerOptions, build_axum_router, serve,
};
pub use synctv_api_http::{
    AuthMethod, create_health_router, create_metrics_router, create_router_from_options,
    create_router_from_shared_state, create_router_with_state_from_options, extract_client_ip,
    hsts_header, liveness_check, map_api_error, security_headers_middleware,
    start_proxy_cache_lifecycle, websocket_handler,
};
pub use synctv_api_http::{RoomWebTransportHandler, WEBTRANSPORT_ROOMS_PATH, get_server_info};
