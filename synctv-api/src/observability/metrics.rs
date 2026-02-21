//! Prometheus metrics for `SyncTV`
//!
//! This module re-exports metrics from synctv-core's unified registry.
//! All metrics are registered in a single global registry to ensure
//! the /metrics endpoint exposes everything.

// Re-export HTTP and WebSocket metrics from core
pub use synctv_core::metrics::http::{
    HTTP_REQUESTS_TOTAL,
    HTTP_REQUEST_DURATION_SECONDS,
    HTTP_REQUESTS_IN_FLIGHT,
    WEBSOCKET_CONNECTIONS_ACTIVE,
    WEBSOCKET_CONNECTIONS_TOTAL,
    WEBSOCKET_MESSAGES_TOTAL,
    WEBSOCKET_ERRORS_TOTAL,
    WEBSOCKET_CONNECTION_DURATION_SECONDS,
    ROOMS_ACTIVE,
    USERS_ONLINE,
    STREAMS_ACTIVE,
    WEBRTC_PEERS_ACTIVE,
    PLAYLIST_ITEMS_TOTAL,
    CHAT_MESSAGES_TOTAL,
};

// Re-export rate limiting metrics from core
pub use synctv_core::metrics::rate_limit::{
    RATE_LIMIT_CHECKS_TOTAL,
    RATE_LIMIT_REJECTIONS_TOTAL,
    RATE_LIMIT_REDIS_FALLBACKS_TOTAL,
};

// Re-export livestream metrics from core
pub use synctv_core::metrics::livestream::{
    LIVESTREAM_ACTIVE_PUBLISHERS,
    LIVESTREAM_ACTIVE_VIEWERS,
    LIVESTREAM_BYTES_TOTAL,
    LIVESTREAM_STREAM_DURATION_SECONDS,
    LIVESTREAM_PULL_ERRORS_TOTAL,
    LIVESTREAM_FLV_SLOW_CLIENT_TERMINATIONS_TOTAL,
};

// Re-export cache metrics from core
pub use synctv_core::metrics::cache::{
    CACHE_HITS,
    CACHE_MISSES,
    CACHE_EVICTIONS,
    CACHE_INVALIDATIONS,
    CACHE_OPERATION_DURATION,
};

// Re-export gather and normalize from core
pub use synctv_core::metrics::{gather_metrics, normalize_path};
