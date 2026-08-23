//! Prometheus metrics for `SyncTV`
//!
//! This module exposes the metrics used by the API crate from synctv-core's
//! unified registry.

pub use synctv_core::metrics::http::{
    HTTP_REQUESTS_IN_FLIGHT, HTTP_REQUESTS_TOTAL, HTTP_REQUEST_DURATION_SECONDS,
};
pub use synctv_core::metrics::remote_transport::{
    REMOTE_TRANSPORT_REQUESTS_TOTAL, REMOTE_TRANSPORT_REQUEST_DURATION,
};

pub use synctv_core::metrics::livestream::LIVESTREAM_FLV_SLOW_CLIENT_TERMINATIONS_TOTAL;

pub use synctv_core::metrics::gather_metrics;
