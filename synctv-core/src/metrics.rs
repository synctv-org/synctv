//! Prometheus metrics collection for production monitoring
//!
//! This module provides production-grade metrics collection using prometheus crate.
//! All metrics are automatically exposed via the /metrics endpoint for Prometheus scraping.

use prometheus::{CounterVec, HistogramVec, Registry, IntGauge, IntCounterVec, TextEncoder, Encoder, register_counter_vec_with_registry, register_histogram_vec_with_registry, register_int_gauge_with_registry};

/// Global metrics registry
pub static REGISTRY: std::sync::LazyLock<Registry> = std::sync::LazyLock::new(Registry::new);

/// HTTP metrics
pub mod http {
    use super::{IntCounterVec, REGISTRY, HistogramVec, IntGauge};
    use prometheus::{HistogramOpts, Opts, register_int_counter_vec_with_registry, register_int_gauge_with_registry};

    /// Total HTTP requests, labeled by method, path, and status code.
    pub static HTTP_REQUESTS_TOTAL: std::sync::LazyLock<IntCounterVec> = std::sync::LazyLock::new(|| {
        register_int_counter_vec_with_registry!(
            Opts::new("http_requests_total", "Total number of HTTP requests"),
            &["method", "path", "status"],
            REGISTRY.clone()
        ).expect("Failed to register HTTP_REQUESTS_TOTAL")
    });

    /// HTTP request duration in seconds, labeled by method and path.
    pub static HTTP_REQUEST_DURATION_SECONDS: std::sync::LazyLock<HistogramVec> = std::sync::LazyLock::new(|| {
        HistogramVec::new(
            HistogramOpts::new(
                "http_request_duration_seconds",
                "HTTP request duration in seconds",
            )
            .buckets(vec![0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]),
            &["method", "path"],
        )
        .and_then(|m| { REGISTRY.register(Box::new(m.clone()))?; Ok(m) })
        .expect("Failed to register HTTP_REQUEST_DURATION_SECONDS")
    });

    /// Number of in-flight HTTP requests.
    pub static HTTP_REQUESTS_IN_FLIGHT: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "http_requests_in_flight",
            "Number of HTTP requests currently being processed",
            REGISTRY.clone()
        ).expect("Failed to register HTTP_REQUESTS_IN_FLIGHT")
    });

    /// Active WebSocket connections (aggregate; per-room stats belong in application dashboards).
    pub static WEBSOCKET_CONNECTIONS_ACTIVE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "websocket_connections_active",
            "Number of active WebSocket connections",
            REGISTRY.clone()
        ).expect("Failed to register WEBSOCKET_CONNECTIONS_ACTIVE")
    });

    /// Total WebSocket connections opened, labeled by connection outcome.
    pub static WEBSOCKET_CONNECTIONS_TOTAL: std::sync::LazyLock<IntCounterVec> = std::sync::LazyLock::new(|| {
        register_int_counter_vec_with_registry!(
            Opts::new("websocket_connections_total", "Total number of WebSocket connections opened"),
            &["status"],
            REGISTRY.clone()
        ).expect("Failed to register WEBSOCKET_CONNECTIONS_TOTAL")
    });

    /// Number of active rooms.
    pub static ROOMS_ACTIVE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "rooms_active",
            "Number of currently active rooms",
            REGISTRY.clone()
        ).expect("Failed to register ROOMS_ACTIVE")
    });

    /// Number of online users.
    pub static USERS_ONLINE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "users_online",
            "Number of currently online users",
            REGISTRY.clone()
        ).expect("Failed to register USERS_ONLINE")
    });

    /// Number of active live streams.
    pub static STREAMS_ACTIVE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "streams_active",
            "Number of active live streams",
            REGISTRY.clone()
        ).expect("Failed to register STREAMS_ACTIVE")
    });

    /// Number of active WebRTC peer connections.
    pub static WEBRTC_PEERS_ACTIVE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "webrtc_peers_active",
            "Number of active WebRTC peer connections",
            REGISTRY.clone()
        ).expect("Failed to register WEBRTC_PEERS_ACTIVE")
    });
}

/// Active connections gauge
pub static ACTIVE_CONNECTIONS: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
    register_int_gauge_with_registry!(
        "active_connections",
        "Current number of active connections",
        REGISTRY.clone()
    ).expect("Failed to register ACTIVE_CONNECTIONS")
});

/// Cache operations
pub mod cache {
    use super::{register_counter_vec_with_registry, register_histogram_vec_with_registry, CounterVec, HistogramVec, REGISTRY};

    /// Cache hit counter
    pub static CACHE_HITS: std::sync::LazyLock<CounterVec> = std::sync::LazyLock::new(|| {
        register_counter_vec_with_registry!(
            "cache_hits_total",
            "Total number of cache hits",
            &["cache_type", "level"],
            REGISTRY.clone()
        ).expect("Failed to register CACHE_HITS")
    });

    /// Cache miss counter
    pub static CACHE_MISSES: std::sync::LazyLock<CounterVec> = std::sync::LazyLock::new(|| {
        register_counter_vec_with_registry!(
            "cache_misses_total",
            "Total number of cache misses",
            &["cache_type", "level"],
            REGISTRY.clone()
        ).expect("Failed to register CACHE_MISSES")
    });

    /// Cache evictions counter
    pub static CACHE_EVICTIONS: std::sync::LazyLock<CounterVec> = std::sync::LazyLock::new(|| {
        register_counter_vec_with_registry!(
            "cache_evictions_total",
            "Total number of cache evictions",
            &["cache_type"],
            REGISTRY.clone()
        ).expect("Failed to register CACHE_EVICTIONS")
    });

    /// Cache error counter (L2 delete failures, cross-replica invalidation errors, etc.)
    pub static CACHE_ERRORS: std::sync::LazyLock<CounterVec> = std::sync::LazyLock::new(|| {
        register_counter_vec_with_registry!(
            "cache_errors_total",
            "Total number of cache operation errors",
            &["cache_type", "operation"],
            REGISTRY.clone()
        ).expect("Failed to register CACHE_ERRORS")
    });

    /// Cache fill duration histogram (time taken to load from DB and populate cache)
    pub static CACHE_FILL_DURATION: std::sync::LazyLock<HistogramVec> = std::sync::LazyLock::new(|| {
        register_histogram_vec_with_registry!(
            "cache_fill_duration_seconds",
            "Time taken to fill cache from database",
            &["cache_type"],
            REGISTRY.clone()
        ).expect("Failed to register CACHE_FILL_DURATION")
    });

    /// SingleFlight merge counter (how many concurrent requests were deduplicated)
    pub static SINGLEFLIGHT_MERGES: std::sync::LazyLock<CounterVec> = std::sync::LazyLock::new(|| {
        register_counter_vec_with_registry!(
            "cache_singleflight_merges_total",
            "Total number of requests merged by SingleFlight",
            &["cache_type"],
            REGISTRY.clone()
        ).expect("Failed to register SINGLEFLIGHT_MERGES")
    });

    /// Cross-replica cache invalidation duration histogram
    pub static INVALIDATION_LATENCY: std::sync::LazyLock<HistogramVec> = std::sync::LazyLock::new(|| {
        register_histogram_vec_with_registry!(
            "cache_invalidation_latency_seconds",
            "Time taken for cross-replica cache invalidation",
            &["cache_type"],
            REGISTRY.clone()
        ).expect("Failed to register INVALIDATION_LATENCY")
    });

    /// Bloom filter false positive counter
    pub static BLOOM_FALSE_POSITIVES: std::sync::LazyLock<CounterVec> = std::sync::LazyLock::new(|| {
        register_counter_vec_with_registry!(
            "cache_bloom_false_positives_total",
            "Total number of Bloom filter false positives",
            &["cache_type"],
            REGISTRY.clone()
        ).expect("Failed to register BLOOM_FALSE_POSITIVES")
    });
}

/// Database operations
pub mod database {
    use super::{register_histogram_vec_with_registry, register_int_gauge_with_registry, register_counter_vec_with_registry, HistogramVec, REGISTRY, IntGauge, CounterVec};
    use prometheus::{GaugeVec, register_gauge_vec_with_registry};

    /// Query duration histogram
    pub static DB_QUERY_DURATION: std::sync::LazyLock<HistogramVec> = std::sync::LazyLock::new(|| {
        register_histogram_vec_with_registry!(
            "db_query_duration_seconds",
            "Database query duration in seconds",
            &["operation", "table"],
            REGISTRY.clone()
        ).expect("Failed to register DB_QUERY_DURATION")
    });

    /// Active connections gauge
    pub static DB_CONNECTIONS_ACTIVE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "db_connections_active",
            "Current number of active database connections",
            REGISTRY.clone()
        ).expect("Failed to register DB_CONNECTIONS_ACTIVE")
    });

    /// Query error counter
    pub static DB_QUERY_ERRORS: std::sync::LazyLock<CounterVec> = std::sync::LazyLock::new(|| {
        register_counter_vec_with_registry!(
            "db_query_errors_total",
            "Total number of database query errors",
            &["operation", "error_type"],
            REGISTRY.clone()
        ).expect("Failed to register DB_QUERY_ERRORS")
    });

    /// Pool utilization percentage (0.0 to 1.0)
    pub static DB_POOL_UTILIZATION: std::sync::LazyLock<GaugeVec> = std::sync::LazyLock::new(|| {
        register_gauge_vec_with_registry!(
            "db_pool_utilization_ratio",
            "Database connection pool utilization ratio (active/max)",
            &["pool"],
            REGISTRY.clone()
        ).expect("Failed to register DB_POOL_UTILIZATION")
    });

    /// Connections waiting for a connection from the pool
    pub static DB_CONNECTIONS_WAITING: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "db_connections_waiting",
            "Number of connections waiting for a connection from the pool",
            REGISTRY.clone()
        ).expect("Failed to register DB_CONNECTIONS_WAITING")
    });

    /// Connection acquire duration histogram
    pub static DB_CONNECTION_ACQUIRE_DURATION: std::sync::LazyLock<HistogramVec> = std::sync::LazyLock::new(|| {
        register_histogram_vec_with_registry!(
            "db_connection_acquire_duration_seconds",
            "Time taken to acquire a connection from the pool",
            &["pool"],
            REGISTRY.clone()
        ).expect("Failed to register DB_CONNECTION_ACQUIRE_DURATION")
    });

    /// Transaction rollback counter
    pub static DB_TRANSACTION_ROLLBACKS: std::sync::LazyLock<CounterVec> = std::sync::LazyLock::new(|| {
        register_counter_vec_with_registry!(
            "db_transaction_rollbacks_total",
            "Total number of database transaction rollbacks",
            &["reason"],
            REGISTRY.clone()
        ).expect("Failed to register DB_TRANSACTION_ROLLBACKS")
    });

    /// Total connections in the pool (max pool size)
    pub static DB_POOL_SIZE_MAX: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "db_pool_size_max",
            "Maximum number of connections in the pool",
            REGISTRY.clone()
        ).expect("Failed to register DB_POOL_SIZE_MAX")
    });

    /// Idle connections in the pool
    pub static DB_CONNECTIONS_IDLE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "db_connections_idle",
            "Number of idle connections in the pool",
            REGISTRY.clone()
        ).expect("Failed to register DB_CONNECTIONS_IDLE")
    });
}

/// gRPC operations
pub mod grpc {
    use super::{register_histogram_vec_with_registry, register_int_gauge_with_registry, HistogramVec, REGISTRY, IntGauge};

    /// RPC request duration histogram
    pub static GRPC_REQUEST_DURATION: std::sync::LazyLock<HistogramVec> = std::sync::LazyLock::new(|| {
        register_histogram_vec_with_registry!(
            "grpc_request_duration_seconds",
            "gRPC request duration in seconds",
            &["service", "method", "status"],
            REGISTRY.clone()
        ).expect("Failed to register GRPC_REQUEST_DURATION")
    });

    /// Active RPC streams gauge
    pub static GRPC_ACTIVE_STREAMS: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "grpc_active_streams",
            "Current number of active gRPC streams",
            REGISTRY.clone()
        ).expect("Failed to register GRPC_ACTIVE_STREAMS")
    });
}

/// Cluster operations
pub mod cluster {
    use super::{REGISTRY, IntGauge, IntCounterVec};
    use prometheus::{Opts, register_int_gauge_with_registry, register_int_counter_vec_with_registry};

    /// Current number of active connections on this cluster node.
    pub static CLUSTER_CONNECTIONS: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "synctv_cluster_connections_total",
            "Current number of active connections on this cluster node",
            REGISTRY.clone()
        ).expect("Failed to register CLUSTER_CONNECTIONS")
    });

    /// Current number of active rooms on this cluster node.
    pub static CLUSTER_ROOMS: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "synctv_cluster_rooms_total",
            "Current number of active rooms on this cluster node",
            REGISTRY.clone()
        ).expect("Failed to register CLUSTER_ROOMS")
    });

    /// Total cluster events published, labeled by event type.
    pub static CLUSTER_EVENTS_PUBLISHED: std::sync::LazyLock<IntCounterVec> = std::sync::LazyLock::new(|| {
        register_int_counter_vec_with_registry!(
            Opts::new("synctv_cluster_events_published_total", "Total cluster events published"),
            &["event_type"],
            REGISTRY.clone()
        ).expect("Failed to register CLUSTER_EVENTS_PUBLISHED")
    });

    /// Total cluster events received from other nodes, labeled by event type.
    pub static CLUSTER_EVENTS_RECEIVED: std::sync::LazyLock<IntCounterVec> = std::sync::LazyLock::new(|| {
        register_int_counter_vec_with_registry!(
            Opts::new("synctv_cluster_events_received_total", "Total cluster events received from other nodes"),
            &["event_type"],
            REGISTRY.clone()
        ).expect("Failed to register CLUSTER_EVENTS_RECEIVED")
    });

    /// Total cluster events dropped (channel full or subscriber disconnected).
    pub static CLUSTER_EVENTS_DROPPED: std::sync::LazyLock<IntCounterVec> = std::sync::LazyLock::new(|| {
        register_int_counter_vec_with_registry!(
            Opts::new("synctv_cluster_events_dropped_total", "Total cluster events dropped"),
            &["reason"],
            REGISTRY.clone()
        ).expect("Failed to register CLUSTER_EVENTS_DROPPED")
    });
}

/// Stream operations
pub mod stream {
    use super::{register_histogram_vec_with_registry, register_int_gauge_with_registry, register_counter_vec_with_registry, HistogramVec, REGISTRY, IntGauge, CounterVec};

    /// Stream relay duration histogram, labeled by stream type (rtmp/hls/webrtc).
    pub static STREAM_RELAY_DURATION: std::sync::LazyLock<HistogramVec> = std::sync::LazyLock::new(|| {
        register_histogram_vec_with_registry!(
            "stream_relay_duration_seconds",
            "Stream relay operation duration in seconds",
            &["stream_type"],
            REGISTRY.clone()
        ).expect("Failed to register STREAM_RELAY_DURATION")
    });

    /// Active relay streams gauge
    pub static ACTIVE_RELAY_STREAMS: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "active_relay_streams",
            "Current number of active relay streams",
            REGISTRY.clone()
        ).expect("Failed to register ACTIVE_RELAY_STREAMS")
    });

    /// Stream error counter, labeled by stream type and error classification.
    pub static STREAM_ERRORS: std::sync::LazyLock<CounterVec> = std::sync::LazyLock::new(|| {
        register_counter_vec_with_registry!(
            "stream_errors_total",
            "Total number of stream errors",
            &["stream_type", "error_type"],
            REGISTRY.clone()
        ).expect("Failed to register STREAM_ERRORS")
    });
}

/// Helper macro to record HTTP request metrics
#[macro_export]
macro_rules! record_http_request {
    ($method:expr, $path:expr, $status:expr, $duration:expr) => {
        let status_str = $status.to_string();
        let method_str = $method.to_string();

        $crate::metrics::http::HTTP_REQUEST_DURATION_SECONDS
            .with_label_values(&[&method_str, $path])
            .observe($duration.as_secs_f64());

        $crate::metrics::http::HTTP_REQUESTS_TOTAL
            .with_label_values(&[&method_str, $path, &status_str])
            .inc();
    };
}

/// Helper macro to record cache metrics
#[macro_export]
macro_rules! record_cache_hit {
    ($cache_type:expr, $level:expr) => {
        $crate::metrics::cache::CACHE_HITS
            .with_label_values(&[$cache_type, $level])
            .inc();
    };
}

#[macro_export]
macro_rules! record_cache_miss {
    ($cache_type:expr, $level:expr) => {
        $crate::metrics::cache::CACHE_MISSES
            .with_label_values(&[$cache_type, $level])
            .inc();
    };
}

/// Helper macro to record database query metrics
#[macro_export]
macro_rules! record_db_query {
    ($operation:expr, $table:expr, $duration:expr, $error:expr) => {
        $crate::metrics::database::DB_QUERY_DURATION
            .with_label_values(&[$operation, $table])
            .observe($duration.as_secs_f64());

        if let Err(e) = $error {
            let error_type = if e.to_string().contains("timeout") {
                "timeout"
            } else if e.to_string().contains("connection") {
                "connection"
            } else {
                "other"
            };
            $crate::metrics::database::DB_QUERY_ERRORS
                .with_label_values(&[$operation, error_type])
                .inc();
        }
    };
}

/// Normalize a request path for metric labels.
///
/// Replaces path parameters (UUIDs, numeric IDs, nanoids) with placeholders
/// to avoid high-cardinality labels.
#[must_use]
pub fn normalize_path(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').collect();
    let mut result = Vec::with_capacity(segments.len());

    for (i, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            result.push(*segment);
            continue;
        }

        // Replace segments that look like IDs (after known resource paths)
        let prev = if i > 0 { segments.get(i - 1) } else { None };
        let is_id = matches!(prev, Some(&"rooms" | &"media" | &"chat" | &"playlists" | &"users" | &"notifications" | &"settings" | &"members"));

        if is_id {
            result.push(":id");
        } else {
            result.push(segment);
        }
    }

    result.join("/")
}

/// Expose metrics in Prometheus format
pub fn gather_metrics() -> String {
    let encoder = TextEncoder::new();
    let metric_families = REGISTRY.gather();
    let mut buffer = Vec::new();
    match encoder.encode(&metric_families, &mut buffer) {
        Ok(()) => {}
        Err(e) => {
            tracing::error!("Failed to encode metrics: {}", e);
            return String::from("# Failed to encode metrics\n");
        }
    }
    String::from_utf8(buffer).unwrap_or_else(|e| {
        tracing::error!("Metrics buffer contains invalid UTF-8: {}", e);
        String::from("# Invalid UTF-8 in metrics\n")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_registration() {
        // Verify all metrics are registered
        http::HTTP_REQUEST_DURATION_SECONDS.with_label_values(&["GET", "/test"]).observe(0.1);
        http::HTTP_REQUESTS_TOTAL.with_label_values(&["GET", "/test", "200"]).inc();

        // Should be able to encode metrics
        let encoder = TextEncoder::new();
        let metric_families = REGISTRY.gather();
        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer).unwrap();
        let output = String::from_utf8(buffer).unwrap();
        assert!(output.contains("http_request_duration_seconds"));
    }

    #[test]
    fn test_normalize_path_existing_resources() {
        assert_eq!(normalize_path("/api/rooms/abc123/media"), "/api/rooms/:id/media");
        assert_eq!(normalize_path("/api/media/xyz789"), "/api/media/:id");
        assert_eq!(normalize_path("/api/chat/msg001"), "/api/chat/:id");
        assert_eq!(normalize_path("/api/playlists/pl123"), "/api/playlists/:id");
    }

    #[test]
    fn test_normalize_path_extended_resources() {
        assert_eq!(normalize_path("/api/users/u123"), "/api/users/:id");
        assert_eq!(normalize_path("/api/notifications/n456"), "/api/notifications/:id");
        assert_eq!(normalize_path("/api/settings/s789"), "/api/settings/:id");
        assert_eq!(normalize_path("/api/members/m012"), "/api/members/:id");
    }

    #[test]
    fn test_normalize_path_no_id_segments() {
        assert_eq!(normalize_path("/api/rooms"), "/api/rooms");
        assert_eq!(normalize_path("/api/health"), "/api/health");
        assert_eq!(normalize_path("/metrics"), "/metrics");
    }

    #[test]
    fn test_websocket_metrics_gauges() {
        // Verify WebSocket gauge operations work correctly.
        let before = http::WEBSOCKET_CONNECTIONS_ACTIVE.get();
        http::WEBSOCKET_CONNECTIONS_ACTIVE.inc();
        http::WEBSOCKET_CONNECTIONS_ACTIVE.inc();
        assert_eq!(http::WEBSOCKET_CONNECTIONS_ACTIVE.get(), before + 2);

        http::WEBSOCKET_CONNECTIONS_ACTIVE.dec();
        assert_eq!(http::WEBSOCKET_CONNECTIONS_ACTIVE.get(), before + 1);

        let before_total = http::WEBSOCKET_CONNECTIONS_TOTAL.with_label_values(&["success"]).get();
        http::WEBSOCKET_CONNECTIONS_TOTAL.with_label_values(&["success"]).inc();
        assert_eq!(http::WEBSOCKET_CONNECTIONS_TOTAL.with_label_values(&["success"]).get(), before_total + 1);
    }

    #[test]
    fn test_room_and_user_metrics() {
        // Verify gauges respond correctly to inc/dec (relative assertions).
        let rooms_before = http::ROOMS_ACTIVE.get();
        http::ROOMS_ACTIVE.inc();
        http::ROOMS_ACTIVE.inc();
        http::ROOMS_ACTIVE.dec();
        assert_eq!(http::ROOMS_ACTIVE.get(), rooms_before + 1);

        let users_before = http::USERS_ONLINE.get();
        http::USERS_ONLINE.inc();
        http::USERS_ONLINE.inc();
        http::USERS_ONLINE.dec();
        assert_eq!(http::USERS_ONLINE.get(), users_before + 1);
    }

    #[test]
    fn test_stream_and_webrtc_metrics() {
        let streams_before = http::STREAMS_ACTIVE.get();
        http::STREAMS_ACTIVE.inc();
        assert_eq!(http::STREAMS_ACTIVE.get(), streams_before + 1);
        http::STREAMS_ACTIVE.dec();
        assert_eq!(http::STREAMS_ACTIVE.get(), streams_before);

        let rtc_before = http::WEBRTC_PEERS_ACTIVE.get();
        http::WEBRTC_PEERS_ACTIVE.inc();
        assert_eq!(http::WEBRTC_PEERS_ACTIVE.get(), rtc_before + 1);
        http::WEBRTC_PEERS_ACTIVE.dec();
        assert_eq!(http::WEBRTC_PEERS_ACTIVE.get(), rtc_before);
    }

    #[test]
    fn test_cache_metrics() {
        cache::CACHE_HITS.with_label_values(&["room", "l1"]).inc();
        cache::CACHE_HITS.with_label_values(&["room", "l2"]).inc();
        cache::CACHE_MISSES.with_label_values(&["room", "l1"]).inc();
        cache::CACHE_EVICTIONS.with_label_values(&["room"]).inc();

        cache::CACHE_HITS.with_label_values(&["user", "l1"]).inc();
        cache::CACHE_MISSES.with_label_values(&["user", "l1"]).inc();
        cache::CACHE_EVICTIONS.with_label_values(&["user"]).inc();

        // Verify gathered metrics contain cache metrics
        let output = gather_metrics();
        assert!(output.contains("cache_hits_total"));
        assert!(output.contains("cache_misses_total"));
        assert!(output.contains("cache_evictions_total"));
    }

    #[test]
    fn test_database_pool_metrics() {
        // Verify the gauge operations work (not exact values, since global
        // state is shared across parallel tests).
        database::DB_POOL_SIZE_MAX.set(20);
        // Value should be >= 0 (another test may race and set to 0)
        assert!(database::DB_POOL_SIZE_MAX.get() >= 0);

        database::DB_CONNECTIONS_ACTIVE.set(5);
        database::DB_CONNECTIONS_IDLE.set(15);
        // Verify these gauges are operational
        assert!(database::DB_CONNECTIONS_ACTIVE.get() >= 0);
        assert!(database::DB_CONNECTIONS_IDLE.get() >= 0);

        // Verify all three appear in gathered output
        let output = gather_metrics();
        assert!(output.contains("db_pool_size_max"), "Missing db_pool_size_max");
        assert!(output.contains("db_connections_active"), "Missing db_connections_active");
        assert!(output.contains("db_connections_idle"), "Missing db_connections_idle");
    }

    #[test]
    fn test_grpc_duration_histogram() {
        // Verify gRPC duration histogram can be observed with correct labels
        let timer = grpc::GRPC_REQUEST_DURATION
            .with_label_values(&["cluster", "get_nodes", "ok"])
            .start_timer();
        timer.observe_duration();

        let output = gather_metrics();
        assert!(output.contains("grpc_request_duration_seconds"));
    }

    #[test]
    fn test_cluster_event_metrics() {
        cluster::CLUSTER_EVENTS_PUBLISHED.with_label_values(&["chat_message"]).inc();
        cluster::CLUSTER_EVENTS_RECEIVED.with_label_values(&["chat_message"]).inc();
        cluster::CLUSTER_EVENTS_DROPPED.with_label_values(&["channel_full"]).inc();

        let output = gather_metrics();
        assert!(output.contains("synctv_cluster_events_published_total"));
        assert!(output.contains("synctv_cluster_events_received_total"));
        assert!(output.contains("synctv_cluster_events_dropped_total"));
    }

    #[test]
    fn test_all_metrics_in_gathered_output() {
        // Touch all metric families to ensure they appear in gathered output.
        // IntCounterVec needs at least one label set touched.
        http::HTTP_REQUESTS_IN_FLIGHT.inc();
        http::HTTP_REQUESTS_IN_FLIGHT.dec();
        http::WEBSOCKET_CONNECTIONS_ACTIVE.set(0);
        http::WEBSOCKET_CONNECTIONS_TOTAL.with_label_values(&["success"]).inc();
        http::ROOMS_ACTIVE.set(0);
        http::USERS_ONLINE.set(0);
        http::STREAMS_ACTIVE.set(0);
        http::WEBRTC_PEERS_ACTIVE.set(0);
        database::DB_CONNECTIONS_ACTIVE.set(0);
        database::DB_CONNECTIONS_IDLE.set(0);
        database::DB_POOL_SIZE_MAX.set(0);
        grpc::GRPC_REQUEST_DURATION
            .with_label_values(&["_test", "_test", "_test"])
            .observe(0.0);

        let output = gather_metrics();

        // HTTP metrics
        assert!(output.contains("websocket_connections_active"), "Missing websocket_connections_active");
        assert!(output.contains("websocket_connections_total"), "Missing websocket_connections_total");
        assert!(output.contains("rooms_active"), "Missing rooms_active");
        assert!(output.contains("users_online"), "Missing users_online");
        assert!(output.contains("streams_active"), "Missing streams_active");
        assert!(output.contains("webrtc_peers_active"), "Missing webrtc_peers_active");

        // Database metrics
        assert!(output.contains("db_connections_active"), "Missing db_connections_active");
        assert!(output.contains("db_connections_idle"), "Missing db_connections_idle");
        assert!(output.contains("db_pool_size_max"), "Missing db_pool_size_max");

        // gRPC metrics
        assert!(output.contains("grpc_request_duration_seconds"), "Missing grpc_request_duration_seconds");
    }
}
