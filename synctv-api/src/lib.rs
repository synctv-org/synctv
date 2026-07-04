#![recursion_limit = "256"]

// SyncTV API Library
// Provides gRPC and HTTP API services for SyncTV

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

pub(crate) mod api_error_model;
pub(crate) mod api_runtime;
pub(crate) mod chat_event_dispatcher;
pub(crate) mod client_ip;
pub(crate) mod config_adapters;
pub(crate) mod emby_thumbnail_urls;
pub(crate) mod fanout;
pub(crate) mod grpc;
pub(crate) mod grpc_support;
pub(crate) mod http;
pub(crate) mod impls;
pub(crate) mod media_fanout;
pub(crate) mod membership_event_fanout;
pub(crate) mod metrics_auth;
pub(crate) mod observability;
#[cfg(feature = "openapi")]
pub(crate) mod openapi;
pub(crate) mod playback_fanout;
pub(crate) mod playlist_fanout;
pub(crate) mod proxy_signature;
pub(crate) mod public_id;
pub(crate) mod realtime_fanout;
pub(crate) mod realtime_lifecycle;
pub(crate) mod resource_change;
pub(crate) mod room_cache_fanout;
pub(crate) mod room_lifecycle_fanout;
pub(crate) mod runtime;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod webrtc_status;

pub use metrics_auth::{MetricsAccessController, MetricsAccessError};
pub use proxy_signature::{
    build_signed_query, build_signed_query_with_target_url, parse_and_verify_query,
    ProxySignatureError, ProxySignatureQueryError, ProxySigningKey, ProxySigningKeyQueryExt,
    ProxyUrlClaims,
};
pub use public_id::{PublicIdCodec, PublicIdKind, PublicIdType};

pub use config_adapters::proxy_slice_cache_config_from_app_config;
pub use grpc::{
    build_axum_router, serve, AdminServiceImpl, ClientServiceConfig, ClientServiceImpl,
    ClusterAuthInterceptor, GrpcServerConfig,
};
pub use grpc_support::{
    extract_client_ip, grpc_unary_request_timeout, map_api_error, map_api_error_ref,
    map_auth_authorization_error, request_metadata, request_user_agent,
};
pub use http::{
    create_app_state_from_config, create_metrics_router, create_router_from_config,
    create_router_from_shared_state, create_router_with_state_from_config,
    extract_client_ip as extract_http_client_ip, hsts_header, liveness_check,
    map_api_error as map_http_api_error, security_headers_middleware, start_proxy_cache_lifecycle,
    websocket_handler, AppError, AppResult, AppState, AuthMethod, ProxyCacheLifecycleRuntime,
    RouterConfig,
};
pub use impls::*;
pub use realtime_fanout::{
    disabled_realtime_fanout_service, distributed_realtime_fanout_service,
    local_realtime_fanout_service, publish_best_effort, LocalRealtimeFanoutService,
    NoopRealtimeFanoutService, OutboxRealtimeFanoutService, PreparedOutboxFanout,
    PreparedRealtimeFanoutPlan, RealtimeFanoutService,
};
pub use runtime::{
    LocalNoopRealtimeEventService, RealtimeAdmissionError, RealtimeDeliveryOutcome,
    RealtimeDeliveryRequirement, RealtimeEventService, RealtimeMetrics,
};
