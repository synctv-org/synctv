//! Prometheus metrics collection for production monitoring
//!
//! This module provides production-grade metrics collection using prometheus crate.
//! All metrics are automatically exposed via the /metrics endpoint for Prometheus scraping.

use prometheus::{
    register_counter_vec_with_registry, register_histogram_vec_with_registry,
    register_int_counter_with_registry, register_int_gauge_with_registry, CounterVec, Encoder,
    HistogramVec, IntCounter, IntCounterVec, IntGauge, Registry, TextEncoder,
};

/// Global metrics registry.
pub static REGISTRY: std::sync::LazyLock<Registry> = std::sync::LazyLock::new(|| {
    let registry = Registry::new();
    #[cfg(target_os = "linux")]
    registry
        .register(Box::new(
            prometheus::process_collector::ProcessCollector::for_self(),
        ))
        .expect("failed to register Prometheus process collector");
    registry
});

/// HTTP and WebSocket metrics
pub mod http {
    use super::{HistogramVec, IntCounterVec, IntGauge, REGISTRY};
    use prometheus::{
        register_int_counter_vec_with_registry, register_int_gauge_with_registry, HistogramOpts,
        Opts,
    };

    /// Total HTTP requests, labeled by method, path, and status code.
    pub static HTTP_REQUESTS_TOTAL: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new("http_requests_total", "Total number of HTTP requests"),
                &["method", "path", "status"],
                REGISTRY.clone()
            )
            .expect("Failed to register HTTP_REQUESTS_TOTAL")
        });

    /// HTTP request duration in seconds, labeled by method and path.
    /// Buckets optimized for P50/P95/P99 calculation.
    pub static HTTP_REQUEST_DURATION_SECONDS: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            HistogramVec::new(
                HistogramOpts::new(
                    "http_request_duration_seconds",
                    "HTTP request duration in seconds (P50/P95/P99)",
                )
                .buckets(vec![
                    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
                ]),
                &["method", "path"],
            )
            .and_then(|m| {
                REGISTRY.register(Box::new(m.clone()))?;
                Ok(m)
            })
            .expect("Failed to register HTTP_REQUEST_DURATION_SECONDS")
        });

    /// HTTP error rate counter, labeled by method, path, and error type.
    pub static HTTP_ERROR_RATE: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new("http_error_rate_total", "Total HTTP errors by type"),
                &["method", "path", "error_type"],
                REGISTRY.clone()
            )
            .expect("Failed to register HTTP_ERROR_RATE")
        });

    /// Number of in-flight HTTP requests.
    pub static HTTP_REQUESTS_IN_FLIGHT: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "http_requests_in_flight",
                "Number of HTTP requests currently being processed",
                REGISTRY.clone()
            )
            .expect("Failed to register HTTP_REQUESTS_IN_FLIGHT")
        });

    /// Active WebSocket connections (aggregate; per-room stats belong in application dashboards).
    pub static WEBSOCKET_CONNECTIONS_ACTIVE: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "websocket_connections_active",
                "Number of active WebSocket connections",
                REGISTRY.clone()
            )
            .expect("Failed to register WEBSOCKET_CONNECTIONS_ACTIVE")
        });

    /// Total WebSocket connections opened, labeled by connection outcome.
    pub static WEBSOCKET_CONNECTIONS_TOTAL: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new(
                    "websocket_connections_total",
                    "Total number of WebSocket connections opened"
                ),
                &["status"],
                REGISTRY.clone()
            )
            .expect("Failed to register WEBSOCKET_CONNECTIONS_TOTAL")
        });

    /// Number of active rooms.
    pub static ROOMS_ACTIVE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "rooms_active",
            "Number of currently active rooms",
            REGISTRY.clone()
        )
        .expect("Failed to register ROOMS_ACTIVE")
    });

    /// Number of online users.
    pub static USERS_ONLINE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "users_online",
            "Number of currently online users",
            REGISTRY.clone()
        )
        .expect("Failed to register USERS_ONLINE")
    });

    /// Number of active live streams.
    pub static STREAMS_ACTIVE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "streams_active",
            "Number of active live streams",
            REGISTRY.clone()
        )
        .expect("Failed to register STREAMS_ACTIVE")
    });

    /// Number of active WebRTC peer connections.
    pub static WEBRTC_PEERS_ACTIVE: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "webrtc_peers_active",
                "Number of active WebRTC peer connections",
                REGISTRY.clone()
            )
            .expect("Failed to register WEBRTC_PEERS_ACTIVE")
        });

    /// Total WebSocket messages processed, labeled by direction (inbound/outbound) and type (text/binary/ping/pong).
    pub static WEBSOCKET_MESSAGES_TOTAL: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new(
                    "websocket_messages_total",
                    "Total number of WebSocket messages processed"
                ),
                &["direction", "type"],
                REGISTRY.clone()
            )
            .expect("Failed to register WEBSOCKET_MESSAGES_TOTAL")
        });

    /// Total WebSocket errors, labeled by error type.
    pub static WEBSOCKET_ERRORS_TOTAL: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new("websocket_errors_total", "Total number of WebSocket errors"),
                &["error_type"],
                REGISTRY.clone()
            )
            .expect("Failed to register WEBSOCKET_ERRORS_TOTAL")
        });

    /// WebSocket connection duration in seconds (how long each connection was alive).
    pub static WEBSOCKET_CONNECTION_DURATION_SECONDS: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            HistogramVec::new(
                HistogramOpts::new(
                    "websocket_connection_duration_seconds",
                    "WebSocket connection duration in seconds",
                )
                .buckets(vec![
                    1.0, 5.0, 15.0, 30.0, 60.0, 300.0, 900.0, 1800.0, 3600.0, 7200.0,
                ]),
                &[],
            )
            .and_then(|m| {
                REGISTRY.register(Box::new(m.clone()))?;
                Ok(m)
            })
            .expect("Failed to register WEBSOCKET_CONNECTION_DURATION_SECONDS")
        });

    /// Total playlist items added.
    pub static PLAYLIST_ITEMS_TOTAL: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new(
                    "playlist_items_total",
                    "Total number of playlist items added"
                ),
                &[],
                REGISTRY.clone()
            )
            .expect("Failed to register PLAYLIST_ITEMS_TOTAL")
        });

    /// Total chat messages sent.
    pub static CHAT_MESSAGES_TOTAL: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new("chat_messages_total", "Total number of chat messages sent"),
                &[],
                REGISTRY.clone()
            )
            .expect("Failed to register CHAT_MESSAGES_TOTAL")
        });
}

/// Active connections gauge
pub static ACTIVE_CONNECTIONS: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
    register_int_gauge_with_registry!(
        "active_connections",
        "Current number of active connections",
        REGISTRY.clone()
    )
    .expect("Failed to register ACTIVE_CONNECTIONS")
});

/// Cache operations
pub mod cache {
    use super::{
        register_counter_vec_with_registry, register_histogram_vec_with_registry, CounterVec,
        HistogramVec, REGISTRY,
    };
    use prometheus::{register_gauge_vec_with_registry, GaugeVec};

    /// Cache hit counter
    pub static CACHE_HITS: std::sync::LazyLock<CounterVec> = std::sync::LazyLock::new(|| {
        register_counter_vec_with_registry!(
            "cache_hits_total",
            "Total number of cache hits",
            &["cache_type", "level"],
            REGISTRY.clone()
        )
        .expect("Failed to register CACHE_HITS")
    });

    /// Cache miss counter
    pub static CACHE_MISSES: std::sync::LazyLock<CounterVec> = std::sync::LazyLock::new(|| {
        register_counter_vec_with_registry!(
            "cache_misses_total",
            "Total number of cache misses",
            &["cache_type", "level"],
            REGISTRY.clone()
        )
        .expect("Failed to register CACHE_MISSES")
    });

    /// Cache evictions counter
    pub static CACHE_EVICTIONS: std::sync::LazyLock<CounterVec> = std::sync::LazyLock::new(|| {
        register_counter_vec_with_registry!(
            "cache_evictions_total",
            "Total number of cache evictions",
            &["cache_type"],
            REGISTRY.clone()
        )
        .expect("Failed to register CACHE_EVICTIONS")
    });

    /// Cache error counter (L2 delete failures, cross-replica invalidation errors, etc.)
    pub static CACHE_ERRORS: std::sync::LazyLock<CounterVec> = std::sync::LazyLock::new(|| {
        register_counter_vec_with_registry!(
            "cache_errors_total",
            "Total number of cache operation errors",
            &["cache_type", "operation"],
            REGISTRY.clone()
        )
        .expect("Failed to register CACHE_ERRORS")
    });

    /// Cache fill duration histogram (time taken to load from DB and populate cache)
    pub static CACHE_FILL_DURATION: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            register_histogram_vec_with_registry!(
                "cache_fill_duration_seconds",
                "Time taken to fill cache from database",
                &["cache_type"],
                REGISTRY.clone()
            )
            .expect("Failed to register CACHE_FILL_DURATION")
        });

    /// `SingleFlight` merge counter (how many concurrent requests were deduplicated)
    pub static SINGLEFLIGHT_MERGES: std::sync::LazyLock<CounterVec> =
        std::sync::LazyLock::new(|| {
            register_counter_vec_with_registry!(
                "cache_singleflight_merges_total",
                "Total number of requests merged by SingleFlight",
                &["cache_type"],
                REGISTRY.clone()
            )
            .expect("Failed to register SINGLEFLIGHT_MERGES")
        });

    /// Cross-replica cache invalidation duration histogram
    pub static INVALIDATION_LATENCY: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            register_histogram_vec_with_registry!(
                "cache_invalidation_latency_seconds",
                "Time taken for cross-replica cache invalidation",
                &["cache_type"],
                REGISTRY.clone()
            )
            .expect("Failed to register INVALIDATION_LATENCY")
        });

    /// Total cache invalidations, labeled by cache type.
    pub static CACHE_INVALIDATIONS: std::sync::LazyLock<CounterVec> =
        std::sync::LazyLock::new(|| {
            register_counter_vec_with_registry!(
                "cache_invalidations_total",
                "Total number of cache invalidations",
                &["cache_type"],
                REGISTRY.clone()
            )
            .expect("Failed to register CACHE_INVALIDATIONS")
        });

    /// Cache operation duration in seconds, labeled by operation type (get/set/invalidate).
    pub static CACHE_OPERATION_DURATION: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            register_histogram_vec_with_registry!(
                "cache_operation_duration_seconds",
                "Duration of cache operations in seconds",
                &["operation"],
                REGISTRY.clone()
            )
            .expect("Failed to register CACHE_OPERATION_DURATION")
        });

    /// Counter for broadcast-channel-lag-triggered full L1 cache flushes.
    ///
    /// When the invalidation channel lags, all L1 caches are flushed.
    /// This counter lets operators observe flush frequency and tune channel capacity.
    pub static CACHE_LAG_FLUSH_TOTAL: std::sync::LazyLock<CounterVec> =
        std::sync::LazyLock::new(|| {
            register_counter_vec_with_registry!(
                "cache_lag_flush_total",
                "Total L1 cache flushes triggered by broadcast channel lag",
                &["component"],
                REGISTRY.clone()
            )
            .expect("Failed to register CACHE_LAG_FLUSH_TOTAL")
        });

    /// Version-fence coordinator operations.
    pub static CACHE_FENCE_OPERATIONS_TOTAL: std::sync::LazyLock<CounterVec> =
        std::sync::LazyLock::new(|| {
            register_counter_vec_with_registry!(
                "cache_fence_operations_total",
                "Total number of cache version-fence operations",
                &["domain", "operation", "result"],
                REGISTRY.clone()
            )
            .expect("Failed to register CACHE_FENCE_OPERATIONS_TOTAL")
        });

    /// Strong reads that bypassed cache and used PostgreSQL.
    pub static CACHE_DB_FALLBACK_TOTAL: std::sync::LazyLock<CounterVec> =
        std::sync::LazyLock::new(|| {
            register_counter_vec_with_registry!(
                "cache_db_fallback_total",
                "Total number of strong cache reads that fell back to PostgreSQL",
                &["domain", "reason"],
                REGISTRY.clone()
            )
            .expect("Failed to register CACHE_DB_FALLBACK_TOTAL")
        });

    /// Version-aware cache writes rejected because a newer value already exists.
    pub static CACHE_STALE_WRITE_REJECT_TOTAL: std::sync::LazyLock<CounterVec> =
        std::sync::LazyLock::new(|| {
            register_counter_vec_with_registry!(
                "cache_stale_write_reject_total",
                "Total number of stale version-aware cache writes rejected",
                &["cache_type", "level"],
                REGISTRY.clone()
            )
            .expect("Failed to register CACHE_STALE_WRITE_REJECT_TOTAL")
        });

    /// Pending version-fence writes by logical domain.
    pub static CACHE_FENCE_PENDING: std::sync::LazyLock<GaugeVec> =
        std::sync::LazyLock::new(|| {
            register_gauge_vec_with_registry!(
                "cache_fence_pending",
                "Whether a cache version fence domain currently has a pending write",
                &["domain"],
                REGISTRY.clone()
            )
            .expect("Failed to register CACHE_FENCE_PENDING")
        });

    /// Read-time fence repair and DB/fence comparison outcomes.
    pub static CACHE_FENCE_REPAIR_TOTAL: std::sync::LazyLock<CounterVec> =
        std::sync::LazyLock::new(|| {
            register_counter_vec_with_registry!(
                "cache_fence_repair_total",
                "Total number of read-time cache fence repair outcomes",
                &["domain", "result"],
                REGISTRY.clone()
            )
            .expect("Failed to register CACHE_FENCE_REPAIR_TOTAL")
        });

    /// Latest DB-vs-fence patrol comparison by logical domain.
    pub static CACHE_FENCE_DB_COMPARE: std::sync::LazyLock<GaugeVec> =
        std::sync::LazyLock::new(|| {
            register_gauge_vec_with_registry!(
                "cache_fence_db_compare",
                "Latest cache fence patrol comparison with PostgreSQL version (1 when observed)",
                &["domain", "relation"],
                REGISTRY.clone()
            )
            .expect("Failed to register CACHE_FENCE_DB_COMPARE")
        });
}

/// Database operations
pub mod database {
    use super::{
        register_counter_vec_with_registry, register_histogram_vec_with_registry,
        register_int_gauge_with_registry, CounterVec, HistogramVec, IntCounterVec, IntGauge,
        REGISTRY,
    };
    use prometheus::{
        register_gauge_vec_with_registry, register_int_counter_vec_with_registry, GaugeVec,
        HistogramOpts, Opts,
    };

    /// Query duration histogram with optimized buckets for P50/P95/P99.
    pub static DB_QUERY_DURATION: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            HistogramVec::new(
                HistogramOpts::new(
                    "db_query_duration_seconds",
                    "Database query duration in seconds (P50/P95/P99)",
                )
                .buckets(vec![
                    0.001, 0.002, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
                ]),
                &["operation", "table"],
            )
            .and_then(|m| {
                REGISTRY.register(Box::new(m.clone()))?;
                Ok(m)
            })
            .expect("Failed to register DB_QUERY_DURATION")
        });

    /// Total database operations, labeled by operation, table, and result.
    pub static DB_OPERATIONS_TOTAL: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new("db_operations_total", "Total database operations"),
                &["operation", "table", "result"],
                REGISTRY.clone()
            )
            .expect("Failed to register DB_OPERATIONS_TOTAL")
        });

    /// Active connections gauge
    pub static DB_CONNECTIONS_ACTIVE: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "db_connections_active",
                "Current number of active database connections",
                REGISTRY.clone()
            )
            .expect("Failed to register DB_CONNECTIONS_ACTIVE")
        });

    /// Query error counter
    pub static DB_QUERY_ERRORS: std::sync::LazyLock<CounterVec> = std::sync::LazyLock::new(|| {
        register_counter_vec_with_registry!(
            "db_query_errors_total",
            "Total number of database query errors",
            &["operation", "error_type"],
            REGISTRY.clone()
        )
        .expect("Failed to register DB_QUERY_ERRORS")
    });

    /// Pool utilization percentage (0.0 to 1.0)
    pub static DB_POOL_UTILIZATION: std::sync::LazyLock<GaugeVec> =
        std::sync::LazyLock::new(|| {
            register_gauge_vec_with_registry!(
                "db_pool_utilization_ratio",
                "Database connection pool utilization ratio (active/max)",
                &["pool"],
                REGISTRY.clone()
            )
            .expect("Failed to register DB_POOL_UTILIZATION")
        });

    /// Connections waiting for a connection from the pool
    pub static DB_CONNECTIONS_WAITING: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "db_connections_waiting",
                "Number of connections waiting for a connection from the pool",
                REGISTRY.clone()
            )
            .expect("Failed to register DB_CONNECTIONS_WAITING")
        });

    /// Connection acquire duration histogram
    pub static DB_CONNECTION_ACQUIRE_DURATION: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            register_histogram_vec_with_registry!(
                "db_connection_acquire_duration_seconds",
                "Time taken to acquire a connection from the pool",
                &["pool"],
                REGISTRY.clone()
            )
            .expect("Failed to register DB_CONNECTION_ACQUIRE_DURATION")
        });

    /// Transaction rollback counter
    pub static DB_TRANSACTION_ROLLBACKS: std::sync::LazyLock<CounterVec> =
        std::sync::LazyLock::new(|| {
            register_counter_vec_with_registry!(
                "db_transaction_rollbacks_total",
                "Total number of database transaction rollbacks",
                &["reason"],
                REGISTRY.clone()
            )
            .expect("Failed to register DB_TRANSACTION_ROLLBACKS")
        });

    /// Total connections in the pool (max pool size)
    pub static DB_POOL_SIZE_MAX: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "db_pool_size_max",
            "Maximum number of connections in the pool",
            REGISTRY.clone()
        )
        .expect("Failed to register DB_POOL_SIZE_MAX")
    });

    /// Idle connections in the pool
    pub static DB_CONNECTIONS_IDLE: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "db_connections_idle",
                "Number of idle connections in the pool",
                REGISTRY.clone()
            )
            .expect("Failed to register DB_CONNECTIONS_IDLE")
        });
}

/// gRPC operations
pub mod grpc {
    use super::{
        register_histogram_vec_with_registry, register_int_gauge_with_registry, HistogramVec,
        IntCounterVec, IntGauge, REGISTRY,
    };
    use prometheus::{register_int_counter_vec_with_registry, Opts};

    /// Total gRPC requests, labeled by service, method, and status code.
    pub static GRPC_REQUESTS_TOTAL: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new("grpc_requests_total", "Total number of gRPC requests"),
                &["service", "method", "status"],
                REGISTRY.clone()
            )
            .expect("Failed to register GRPC_REQUESTS_TOTAL")
        });

    /// RPC request duration histogram
    pub static GRPC_REQUEST_DURATION: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            register_histogram_vec_with_registry!(
                "grpc_request_duration_seconds",
                "gRPC request duration in seconds",
                &["service", "method", "status"],
                REGISTRY.clone()
            )
            .expect("Failed to register GRPC_REQUEST_DURATION")
        });

    /// Active RPC streams gauge
    pub static GRPC_ACTIVE_STREAMS: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "grpc_active_streams",
                "Current number of active gRPC streams",
                REGISTRY.clone()
            )
            .expect("Failed to register GRPC_ACTIVE_STREAMS")
        });
}

/// Redis operations
pub mod redis {
    use super::{HistogramVec, IntCounterVec, REGISTRY};
    use prometheus::{
        register_int_counter_vec_with_registry, register_int_gauge_vec_with_registry,
        HistogramOpts, IntGaugeVec, Opts,
    };

    /// Total Redis operation errors, labeled by operation type.
    pub static REDIS_ERRORS: std::sync::LazyLock<IntCounterVec> = std::sync::LazyLock::new(|| {
        register_int_counter_vec_with_registry!(
            Opts::new("redis_errors_total", "Total Redis operation errors"),
            &["operation"],
            REGISTRY.clone()
        )
        .expect("Failed to register REDIS_ERRORS")
    });

    /// Redis operation duration in seconds, labeled by operation type.
    /// Buckets optimized for P50/P95/P99 calculation.
    pub static REDIS_OPERATION_DURATION: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            HistogramVec::new(
                HistogramOpts::new(
                    "redis_operation_duration_seconds",
                    "Redis operation duration in seconds (P50/P95/P99)",
                )
                .buckets(vec![
                    0.001, 0.002, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
                ]),
                &["operation"],
            )
            .and_then(|m| {
                REGISTRY.register(Box::new(m.clone()))?;
                Ok(m)
            })
            .expect("Failed to register REDIS_OPERATION_DURATION")
        });

    /// Total Redis operations, labeled by operation and result (success/error).
    pub static REDIS_OPERATIONS_TOTAL: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new("redis_operations_total", "Total Redis operations"),
                &["operation", "result"],
                REGISTRY.clone()
            )
            .expect("Failed to register REDIS_OPERATIONS_TOTAL")
        });

    /// Redis connection pool size.
    pub static REDIS_POOL_SIZE: std::sync::LazyLock<IntGaugeVec> = std::sync::LazyLock::new(|| {
        register_int_gauge_vec_with_registry!(
            Opts::new("redis_pool_size", "Redis connection pool size"),
            &["pool"],
            REGISTRY.clone()
        )
        .expect("Failed to register REDIS_POOL_SIZE")
    });
}

/// Cluster operations
pub mod cluster {
    use super::{HistogramVec, IntCounterVec, IntGauge, REGISTRY};
    use prometheus::{
        register_int_counter_vec_with_registry, register_int_gauge_with_registry, HistogramOpts,
        Opts,
    };

    /// Current number of active connections on this cluster node.
    pub static CLUSTER_CONNECTIONS: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "synctv_cluster_connections_total",
                "Current number of active connections on this cluster node",
                REGISTRY.clone()
            )
            .expect("Failed to register CLUSTER_CONNECTIONS")
        });

    /// Current number of active rooms on this node (per-node, not cluster-wide).
    pub static NODE_ACTIVE_ROOMS: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "synctv_node_active_rooms",
            "Current number of active rooms on this node",
            REGISTRY.clone()
        )
        .expect("Failed to register NODE_ACTIVE_ROOMS")
    });

    /// Total realtime events published, labeled by event type.
    pub static REALTIME_EVENTS_PUBLISHED: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new(
                    "synctv_realtime_events_published_total",
                    "Total realtime events published"
                ),
                &["event_type"],
                REGISTRY.clone()
            )
            .expect("Failed to register REALTIME_EVENTS_PUBLISHED")
        });

    /// Total realtime events received from other nodes, labeled by event type.
    pub static REALTIME_EVENTS_RECEIVED: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new(
                    "synctv_realtime_events_received_total",
                    "Total realtime events received from other nodes"
                ),
                &["event_type"],
                REGISTRY.clone()
            )
            .expect("Failed to register REALTIME_EVENTS_RECEIVED")
        });

    /// Total realtime events dropped (channel full or subscriber disconnected).
    pub static REALTIME_EVENTS_DROPPED: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new(
                    "synctv_realtime_events_dropped_total",
                    "Total realtime events dropped"
                ),
                &["reason"],
                REGISTRY.clone()
            )
            .expect("Failed to register REALTIME_EVENTS_DROPPED")
        });

    /// Consecutive heartbeat failures (network partition detection).
    /// Reset to 0 on successful heartbeat. Values >= 3 indicate possible partition.
    pub static CLUSTER_HEARTBEAT_FAILURES: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "synctv_cluster_heartbeat_failures",
                "Consecutive Redis heartbeat failures for network partition detection",
                REGISTRY.clone()
            )
            .expect("Failed to register CLUSTER_HEARTBEAT_FAILURES")
        });

    /// Node health status (1 = healthy, 0 = unhealthy).
    pub static NODE_HEALTH_STATUS: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "synctv_cluster_node_health_status",
            "Node health status (1 = healthy, 0 = unhealthy)",
            REGISTRY.clone()
        )
        .expect("Failed to register NODE_HEALTH_STATUS")
    });

    /// Leader election state (1 = leader, 0 = follower).
    pub static LEADER_ELECTION_STATE: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "synctv_cluster_leader_election_state",
                "Leader election state (1 = leader, 0 = follower)",
                REGISTRY.clone()
            )
            .expect("Failed to register LEADER_ELECTION_STATE")
        });

    /// Leader election epoch (fencing token), incremented on each leadership acquisition.
    ///
    /// Used to detect split-brain scenarios: if two nodes report the same epoch,
    /// or if a node performs singleton tasks with an outdated epoch.
    pub static LEADER_ELECTION_EPOCH: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "synctv_cluster_leader_election_epoch",
                "Leader election epoch (fencing token), incremented on each leadership acquisition",
                REGISTRY.clone()
            )
            .expect("Failed to register LEADER_ELECTION_EPOCH")
        });

    /// Leader election consecutive failures counter.
    /// High values indicate prolonged leader vacancy (network partition, Redis/K8s outage).
    /// Alert threshold: > 3 consecutive failures for > 30 seconds.
    pub static LEADER_ELECTION_CONSECUTIVE_FAILURES: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
            "synctv_cluster_leader_election_consecutive_failures",
            "Consecutive leader election failures (network partition or backend outage detection)",
            REGISTRY.clone()
        )
            .expect("Failed to register LEADER_ELECTION_CONSECUTIVE_FAILURES")
        });

    /// Epoch mismatch quarantine state (1 = quarantined, 0 = normal).
    ///
    /// When set to 1, this node has detected split-brain (epoch mismatch) and
    /// should reject fan-out requests and leadership operations until successfully
    /// re-registered with a new epoch.
    pub static CLUSTER_EPOCH_MISMATCH_QUARANTINE: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "synctv_cluster_epoch_mismatch_quarantine",
                "Epoch mismatch quarantine state (1 = quarantined due to split-brain, 0 = normal)",
                REGISTRY.clone()
            )
            .expect("Failed to register CLUSTER_EPOCH_MISMATCH_QUARANTINE")
        });

    /// Leader election mode (0 = `standalone/always_leader`, 1 = redis, 2 = `k8s_lease`).
    /// Helps operators understand the active election strategy at a glance.
    pub static LEADER_ELECTION_MODE: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "synctv_cluster_leader_election_mode",
                "Leader election mode (0=standalone, 1=redis, 2=k8s_lease)",
                REGISTRY.clone()
            )
            .expect("Failed to register LEADER_ELECTION_MODE")
        });

    /// Redis pub/sub connection health (1 = connected, 0 = disconnected).
    pub static REDIS_PUBSUB_HEALTH: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "synctv_cluster_redis_pubsub_health",
                "Redis pub/sub connection health (1 = connected, 0 = disconnected)",
                REGISTRY.clone()
            )
            .expect("Failed to register REDIS_PUBSUB_HEALTH")
        });

    /// Total number of cluster members.
    pub static CLUSTER_MEMBER_COUNT: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "synctv_cluster_member_count",
                "Total number of cluster members",
                REGISTRY.clone()
            )
            .expect("Failed to register CLUSTER_MEMBER_COUNT")
        });

    /// Leader election duration in seconds.
    pub static LEADER_ELECTION_DURATION: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            HistogramVec::new(
                HistogramOpts::new(
                    "synctv_cluster_leader_election_duration_seconds",
                    "Leader election duration in seconds",
                ),
                &["result"],
            )
            .and_then(|m| {
                REGISTRY.register(Box::new(m.clone()))?;
                Ok(m)
            })
            .expect("Failed to register LEADER_ELECTION_DURATION")
        });

    /// Redis pub/sub message publish latency in seconds.
    pub static REDIS_PUBSUB_PUBLISH_LATENCY: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            HistogramVec::new(
                HistogramOpts::new(
                    "synctv_cluster_redis_pubsub_publish_latency_seconds",
                    "Redis pub/sub message publish latency in seconds",
                )
                .buckets(vec![0.0005, 0.001, 0.002, 0.005, 0.01, 0.025, 0.05, 0.1]),
                &["channel"],
            )
            .and_then(|m| {
                REGISTRY.register(Box::new(m.clone()))?;
                Ok(m)
            })
            .expect("Failed to register REDIS_PUBSUB_PUBLISH_LATENCY")
        });

    /// Node-to-node message latency in seconds (end-to-end).
    pub static NODE_MESSAGE_LATENCY: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            HistogramVec::new(
                HistogramOpts::new(
                    "synctv_cluster_node_message_latency_seconds",
                    "Node-to-node message latency in seconds",
                )
                .buckets(vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5]),
                &["event_type"],
            )
            .and_then(|m| {
                REGISTRY.register(Box::new(m.clone()))?;
                Ok(m)
            })
            .expect("Failed to register NODE_MESSAGE_LATENCY")
        });

    /// Total cluster synchronization errors, labeled by error type.
    pub static CLUSTER_SYNC_ERRORS: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new(
                    "synctv_cluster_sync_errors_total",
                    "Total cluster synchronization errors"
                ),
                &["error_type"],
                REGISTRY.clone()
            )
            .expect("Failed to register CLUSTER_SYNC_ERRORS")
        });

    /// This node's last successful heartbeat timestamp (Unix timestamp).
    /// Reports only this node's own heartbeat (no per-node_id label to avoid unbounded cardinality).
    pub static NODE_LAST_HEARTBEAT: std::sync::LazyLock<prometheus::Gauge> =
        std::sync::LazyLock::new(|| {
            prometheus::Gauge::with_opts(prometheus::Opts::new(
                "synctv_cluster_node_last_heartbeat_timestamp",
                "This node's last successful heartbeat timestamp (Unix)",
            ))
            .and_then(|g| {
                REGISTRY.register(Box::new(g.clone()))?;
                Ok(g)
            })
            .expect("Failed to register NODE_LAST_HEARTBEAT")
        });

    /// Total distributed counter TTL refresh operations, labeled by result.
    ///
    /// Labels: "success", "failure".
    /// Alert condition: if `failure` count increases while `success` stays
    /// flat, distributed rate limiting may silently stop working.
    pub static DISTRIBUTED_COUNTER_TTL_REFRESHES: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new(
                    "synctv_cluster_distributed_counter_ttl_refreshes_total",
                    "Total distributed counter TTL refresh operations"
                ),
                &["result"],
                REGISTRY.clone()
            )
            .expect("Failed to register DISTRIBUTED_COUNTER_TTL_REFRESHES")
        });

    /// Number of keys refreshed in the last TTL refresh cycle.
    /// A sudden drop to 0 while connections are active indicates a problem.
    pub static DISTRIBUTED_COUNTER_TTL_KEYS_REFRESHED: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "synctv_cluster_distributed_counter_ttl_keys_refreshed",
                "Number of keys refreshed in the last TTL refresh cycle",
                REGISTRY.clone()
            )
            .expect("Failed to register DISTRIBUTED_COUNTER_TTL_KEYS_REFRESHED")
        });

    /// Consecutive TTL refresh failures. Reset to 0 on success.
    /// Alert when value >= 3 (counters may have expired).
    pub static DISTRIBUTED_COUNTER_TTL_CONSECUTIVE_FAILURES: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "synctv_cluster_distributed_counter_ttl_consecutive_failures",
                "Consecutive TTL refresh failures (alert when >= 3)",
                REGISTRY.clone()
            )
            .expect("Failed to register DISTRIBUTED_COUNTER_TTL_CONSECUTIVE_FAILURES")
        });
}

/// Generic file storage metrics.
pub mod file_storage {
    use super::{IntCounterVec, IntGauge, REGISTRY};
    use prometheus::{
        register_int_counter_vec_with_registry, register_int_gauge_with_registry, Opts,
    };

    /// File object delete attempts, labeled by cleanup origin and storage backend.
    pub static FILE_OBJECT_DELETE_ATTEMPTS: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new(
                    "synctv_file_object_delete_attempts_total",
                    "Total file object delete attempts"
                ),
                &["origin", "backend"],
                REGISTRY.clone()
            )
            .expect("Failed to register FILE_OBJECT_DELETE_ATTEMPTS")
        });

    /// File object delete failures, labeled by cleanup origin and storage backend.
    pub static FILE_OBJECT_DELETE_FAILURES: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new(
                    "synctv_file_object_delete_failures_total",
                    "Total file object delete failures"
                ),
                &["origin", "backend"],
                REGISTRY.clone()
            )
            .expect("Failed to register FILE_OBJECT_DELETE_FAILURES")
        });

    /// Due file cleanup jobs waiting for retry.
    pub static FILE_CLEANUP_JOBS_DUE: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "synctv_file_cleanup_jobs_due",
                "File cleanup jobs due for retry",
                REGISTRY.clone()
            )
            .expect("Failed to register FILE_CLEANUP_JOBS_DUE")
        });

    /// File cleanup retry job actions.
    pub static FILE_CLEANUP_JOBS_TOTAL: std::sync::LazyLock<IntCounterVec> =
        std::sync::LazyLock::new(|| {
            register_int_counter_vec_with_registry!(
                Opts::new(
                    "synctv_file_cleanup_jobs_total",
                    "Total file cleanup retry job actions"
                ),
                &["action", "origin", "backend"],
                REGISTRY.clone()
            )
            .expect("Failed to register FILE_CLEANUP_JOBS_TOTAL")
        });
}

/// Spawned task monitoring
pub mod task {
    use super::{register_counter_vec_with_registry, CounterVec, REGISTRY};

    /// Total spawned task panics, labeled by task name.
    pub static TASK_PANICS_TOTAL: std::sync::LazyLock<CounterVec> =
        std::sync::LazyLock::new(|| {
            register_counter_vec_with_registry!(
                "spawned_task_panics_total",
                "Total number of spawned task panics caught by spawn_monitored",
                &["task_name"],
                REGISTRY.clone()
            )
            .expect("Failed to register TASK_PANICS_TOTAL")
        });
}

/// Rate limiting operations
pub mod rate_limit {
    use super::{register_counter_vec_with_registry, CounterVec, REGISTRY};

    /// Total rate limit checks, labeled by backend ("redis" or "memory") and category.
    pub static RATE_LIMIT_CHECKS_TOTAL: std::sync::LazyLock<CounterVec> =
        std::sync::LazyLock::new(|| {
            register_counter_vec_with_registry!(
                "rate_limit_checks_total",
                "Total number of rate limit checks",
                &["backend", "category"],
                REGISTRY.clone()
            )
            .expect("Failed to register RATE_LIMIT_CHECKS_TOTAL")
        });

    /// Total rate limit rejections (429s), labeled by backend and category.
    pub static RATE_LIMIT_REJECTIONS_TOTAL: std::sync::LazyLock<CounterVec> =
        std::sync::LazyLock::new(|| {
            register_counter_vec_with_registry!(
                "rate_limit_rejections_total",
                "Total number of rate limit rejections (429)",
                &["backend", "category"],
                REGISTRY.clone()
            )
            .expect("Failed to register RATE_LIMIT_REJECTIONS_TOTAL")
        });

    /// Redis errors that triggered fallback to in-memory rate limiting.
    pub static RATE_LIMIT_REDIS_FALLBACKS_TOTAL: std::sync::LazyLock<CounterVec> =
        std::sync::LazyLock::new(|| {
            register_counter_vec_with_registry!(
                "rate_limit_redis_fallbacks_total",
                "Total Redis errors that triggered in-memory rate limit fallback",
                &["category"],
                REGISTRY.clone()
            )
            .expect("Failed to register RATE_LIMIT_REDIS_FALLBACKS_TOTAL")
        });
}

/// Stream operations
pub mod stream {
    use super::{
        register_counter_vec_with_registry, register_histogram_vec_with_registry,
        register_int_gauge_with_registry, CounterVec, HistogramVec, IntGauge, REGISTRY,
    };

    /// Stream relay duration histogram, labeled by stream type (rtmp/hls/webrtc).
    pub static STREAM_RELAY_DURATION: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            register_histogram_vec_with_registry!(
                "stream_relay_duration_seconds",
                "Stream relay operation duration in seconds",
                &["stream_type"],
                REGISTRY.clone()
            )
            .expect("Failed to register STREAM_RELAY_DURATION")
        });

    /// Active relay streams gauge
    pub static ACTIVE_RELAY_STREAMS: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "active_relay_streams",
                "Current number of active relay streams",
                REGISTRY.clone()
            )
            .expect("Failed to register ACTIVE_RELAY_STREAMS")
        });

    /// Stream error counter, labeled by stream type and error classification.
    pub static STREAM_ERRORS: std::sync::LazyLock<CounterVec> = std::sync::LazyLock::new(|| {
        register_counter_vec_with_registry!(
            "stream_errors_total",
            "Total number of stream errors",
            &["stream_type", "error_type"],
            REGISTRY.clone()
        )
        .expect("Failed to register STREAM_ERRORS")
    });
}

/// `StreamHub` infrastructure metrics
pub mod streamhub {
    use super::{register_counter_vec_with_registry, CounterVec, REGISTRY};

    /// Total number of `StreamHub` event loop restarts, labeled by exit reason.
    /// Reasons: "panic" (`event_loop` panicked), "`channel_closed`" (all senders dropped).
    pub static STREAMHUB_RESTARTS_TOTAL: std::sync::LazyLock<CounterVec> =
        std::sync::LazyLock::new(|| {
            register_counter_vec_with_registry!(
                "streamhub_restarts_total",
                "Total number of StreamHub event loop restarts",
                &["reason"],
                REGISTRY.clone()
            )
            .expect("Failed to register STREAMHUB_RESTARTS_TOTAL")
        });
}

/// Livestream metrics
pub mod livestream {
    use super::{
        register_counter_vec_with_registry, register_histogram_vec_with_registry,
        register_int_counter_with_registry, register_int_gauge_with_registry, CounterVec,
        HistogramVec, IntCounter, IntGauge, REGISTRY,
    };
    use prometheus::Opts;

    /// Total publisher cleanups due to heartbeat failure.
    pub static PUBLISHER_HEARTBEAT_FAILURES: std::sync::LazyLock<IntCounter> =
        std::sync::LazyLock::new(|| {
            IntCounter::with_opts(Opts::new(
                "synctv_publisher_heartbeat_failures_total",
                "Total publisher cleanups due to heartbeat failure",
            ))
            .and_then(|c| {
                REGISTRY.register(Box::new(c.clone()))?;
                Ok(c)
            })
            .expect("Failed to register PUBLISHER_HEARTBEAT_FAILURES")
        });

    /// Number of active publishers (streams being pushed).
    pub static LIVESTREAM_ACTIVE_PUBLISHERS: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "livestream_active_publishers",
                "Number of active livestream publishers",
                REGISTRY.clone()
            )
            .expect("Failed to register LIVESTREAM_ACTIVE_PUBLISHERS")
        });

    /// Number of active viewers (clients consuming live streams).
    pub static LIVESTREAM_ACTIVE_VIEWERS: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "livestream_active_viewers",
                "Number of active livestream viewers",
                REGISTRY.clone()
            )
            .expect("Failed to register LIVESTREAM_ACTIVE_VIEWERS")
        });

    /// Total bytes transferred for livestream, labeled by direction (in/out).
    pub static LIVESTREAM_BYTES_TOTAL: std::sync::LazyLock<CounterVec> =
        std::sync::LazyLock::new(|| {
            register_counter_vec_with_registry!(
                "livestream_bytes_total",
                "Total bytes transferred for livestream",
                &["direction"],
                REGISTRY.clone()
            )
            .expect("Failed to register LIVESTREAM_BYTES_TOTAL")
        });

    /// Livestream duration in seconds (how long each stream session lasted).
    pub static LIVESTREAM_STREAM_DURATION_SECONDS: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            register_histogram_vec_with_registry!(
                "livestream_stream_duration_seconds",
                "Livestream session duration in seconds",
                &["stream_type"],
                REGISTRY.clone()
            )
            .expect("Failed to register LIVESTREAM_STREAM_DURATION_SECONDS")
        });

    /// Total stream pull errors, labeled by error type.
    pub static LIVESTREAM_PULL_ERRORS_TOTAL: std::sync::LazyLock<CounterVec> =
        std::sync::LazyLock::new(|| {
            register_counter_vec_with_registry!(
                "livestream_pull_errors_total",
                "Total number of livestream pull errors",
                &["error_type"],
                REGISTRY.clone()
            )
            .expect("Failed to register LIVESTREAM_PULL_ERRORS_TOTAL")
        });

    /// Total relay frames dropped due to backpressure.
    pub static LIVESTREAM_RELAY_FRAME_DROPS: std::sync::LazyLock<IntCounter> =
        std::sync::LazyLock::new(|| {
            IntCounter::with_opts(Opts::new(
                "livestream_relay_frame_drops_total",
                "Total relay frames dropped due to backpressure",
            ))
            .and_then(|c| {
                REGISTRY.register(Box::new(c.clone()))?;
                Ok(c)
            })
            .expect("Failed to register LIVESTREAM_RELAY_FRAME_DROPS")
        });

    /// Number of cached GOPs across all active streams.
    pub static GOP_CACHE_SIZE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
        register_int_gauge_with_registry!(
            "gop_cache_size",
            "Number of cached GOPs across all active streams",
            REGISTRY.clone()
        )
        .expect("Failed to register GOP_CACHE_SIZE")
    });

    /// Total number of GOPs evicted due to memory limits since process start.
    pub static GOP_CACHE_DROPS_TOTAL: std::sync::LazyLock<IntCounter> =
        std::sync::LazyLock::new(|| {
            register_int_counter_with_registry!(
                "gop_cache_drops_total",
                "Total number of GOPs evicted due to memory limits",
                REGISTRY.clone()
            )
            .expect("Failed to register GOP_CACHE_DROPS_TOTAL")
        });

    /// Current memory usage in bytes of the GOP cache across all active streams.
    pub static GOP_CACHE_MEMORY_BYTES: std::sync::LazyLock<IntGauge> =
        std::sync::LazyLock::new(|| {
            register_int_gauge_with_registry!(
                "gop_cache_memory_bytes",
                "Current memory usage in bytes of the GOP cache across all active streams",
                REGISTRY.clone()
            )
            .expect("Failed to register GOP_CACHE_MEMORY_BYTES")
        });

    /// Total FLV stream terminations due to slow client (exceeded consecutive frame drops).
    pub static LIVESTREAM_FLV_SLOW_CLIENT_TERMINATIONS_TOTAL: std::sync::LazyLock<IntCounter> =
        std::sync::LazyLock::new(|| {
            register_int_counter_with_registry!(
                "livestream_flv_slow_client_terminations_total",
                "Total FLV stream terminations due to slow client",
                REGISTRY.clone()
            )
            .expect("Failed to register LIVESTREAM_FLV_SLOW_CLIENT_TERMINATIONS_TOTAL")
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
        let is_id = matches!(
            prev,
            Some(
                &"rooms"
                    | &"media"
                    | &"chat"
                    | &"playlists"
                    | &"users"
                    | &"notifications"
                    | &"settings"
                    | &"members"
            )
        );

        if is_id || is_dynamic_segment(segment) {
            result.push(":id");
        } else {
            result.push(segment);
        }
    }

    result.join("/")
}

/// Check if a path segment looks like a dynamic ID (UUID, numeric, or base62 ID).
fn is_dynamic_segment(segment: &str) -> bool {
    // UUID format: 8-4-4-4-12 hex chars (with hyphens, 36 chars total)
    if segment.len() == 36 && segment.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        let parts: Vec<&str> = segment.split('-').collect();
        if parts.len() == 5
            && parts[0].len() == 8
            && parts[1].len() == 4
            && parts[2].len() == 4
            && parts[3].len() == 4
            && parts[4].len() == 12
        {
            return true;
        }
    }
    // Pure numeric IDs
    if segment.chars().all(|c| c.is_ascii_digit()) && !segment.is_empty() {
        return true;
    }
    // Hex strings of 32 chars (UUID without hyphens)
    if segment.len() == 32 && segment.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    false
}

/// Hot path metrics
pub mod hot_paths {
    use super::{HistogramVec, REGISTRY};
    use prometheus::HistogramOpts;

    /// API endpoint latency for hot paths, optimized for P50/P95/P99.
    pub static API_HOT_PATH_LATENCY: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            HistogramVec::new(
                HistogramOpts::new(
                    "api_hot_path_latency_seconds",
                    "API hot path latency in seconds (P50/P95/P99)",
                )
                .buckets(vec![
                    0.001, 0.002, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0,
                ]),
                &["endpoint", "method"],
            )
            .and_then(|m| {
                REGISTRY.register(Box::new(m.clone()))?;
                Ok(m)
            })
            .expect("Failed to register API_HOT_PATH_LATENCY")
        });

    /// Database query latency for hot paths.
    pub static DB_HOT_PATH_LATENCY: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            HistogramVec::new(
                HistogramOpts::new(
                    "db_hot_path_latency_seconds",
                    "Database hot path query latency in seconds (P50/P95/P99)",
                )
                .buckets(vec![
                    0.0005, 0.001, 0.002, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5,
                ]),
                &["query_name", "table"],
            )
            .and_then(|m| {
                REGISTRY.register(Box::new(m.clone()))?;
                Ok(m)
            })
            .expect("Failed to register DB_HOT_PATH_LATENCY")
        });

    /// Redis operation latency for hot paths.
    pub static REDIS_HOT_PATH_LATENCY: std::sync::LazyLock<HistogramVec> =
        std::sync::LazyLock::new(|| {
            HistogramVec::new(
                HistogramOpts::new(
                    "redis_hot_path_latency_seconds",
                    "Redis hot path operation latency in seconds (P50/P95/P99)",
                )
                .buckets(vec![0.0005, 0.001, 0.002, 0.005, 0.01, 0.025, 0.05, 0.1]),
                &["operation", "key_pattern"],
            )
            .and_then(|m| {
                REGISTRY.register(Box::new(m.clone()))?;
                Ok(m)
            })
            .expect("Failed to register REDIS_HOT_PATH_LATENCY")
        });
}

/// Tracing and observability.
pub mod tracing_spans {}

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
        http::HTTP_REQUEST_DURATION_SECONDS
            .with_label_values(&["GET", "/test"])
            .observe(0.1);
        http::HTTP_REQUESTS_TOTAL
            .with_label_values(&["GET", "/test", "200"])
            .inc();

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
        assert_eq!(
            normalize_path("/api/rooms/abc123/media"),
            "/api/rooms/:id/media"
        );
        assert_eq!(normalize_path("/api/media/xyz789"), "/api/media/:id");
        assert_eq!(normalize_path("/api/chat/msg001"), "/api/chat/:id");
        assert_eq!(normalize_path("/api/playlists/pl123"), "/api/playlists/:id");
    }

    #[test]
    fn test_normalize_path_extended_resources() {
        assert_eq!(normalize_path("/api/users/u123"), "/api/users/:id");
        assert_eq!(
            normalize_path("/api/notifications/n456"),
            "/api/notifications/:id"
        );
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

        let before_total = http::WEBSOCKET_CONNECTIONS_TOTAL
            .with_label_values(&["success"])
            .get();
        http::WEBSOCKET_CONNECTIONS_TOTAL
            .with_label_values(&["success"])
            .inc();
        assert_eq!(
            http::WEBSOCKET_CONNECTIONS_TOTAL
                .with_label_values(&["success"])
                .get(),
            before_total + 1
        );
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
        assert!(
            output.contains("db_pool_size_max"),
            "Missing db_pool_size_max"
        );
        assert!(
            output.contains("db_connections_active"),
            "Missing db_connections_active"
        );
        assert!(
            output.contains("db_connections_idle"),
            "Missing db_connections_idle"
        );
    }

    #[test]
    fn test_grpc_metrics() {
        // Verify gRPC request counter
        grpc::GRPC_REQUESTS_TOTAL
            .with_label_values(&["cluster", "get_nodes", "ok"])
            .inc();

        // Verify gRPC duration histogram can be observed with correct labels
        let timer = grpc::GRPC_REQUEST_DURATION
            .with_label_values(&["cluster", "get_nodes", "ok"])
            .start_timer();
        timer.observe_duration();

        let output = gather_metrics();
        assert!(
            output.contains("grpc_requests_total"),
            "Missing grpc_requests_total"
        );
        assert!(output.contains("grpc_request_duration_seconds"));
    }

    #[test]
    fn test_realtime_event_metrics() {
        cluster::REALTIME_EVENTS_PUBLISHED
            .with_label_values(&["chat_message"])
            .inc();
        cluster::REALTIME_EVENTS_RECEIVED
            .with_label_values(&["chat_message"])
            .inc();
        cluster::REALTIME_EVENTS_DROPPED
            .with_label_values(&["channel_full"])
            .inc();

        let output = gather_metrics();
        assert!(output.contains("synctv_realtime_events_published_total"));
        assert!(output.contains("synctv_realtime_events_received_total"));
        assert!(output.contains("synctv_realtime_events_dropped_total"));
    }

    #[test]
    fn test_file_cleanup_metrics() {
        file_storage::FILE_CLEANUP_JOBS_DUE.set(3);
        file_storage::FILE_CLEANUP_JOBS_TOTAL
            .with_label_values(&["completed", "cleanup_retry", "s3"])
            .inc();

        let output = gather_metrics();
        assert!(output.contains("synctv_file_cleanup_jobs_due"));
        assert!(output.contains("synctv_file_cleanup_jobs_total"));
    }

    #[test]
    fn test_file_storage_object_delete_metrics() {
        file_storage::FILE_OBJECT_DELETE_ATTEMPTS
            .with_label_values(&["reference_released", "s3"])
            .inc();
        file_storage::FILE_OBJECT_DELETE_FAILURES
            .with_label_values(&["reference_released", "s3"])
            .inc();

        let output = gather_metrics();
        assert!(output.contains("synctv_file_object_delete_attempts_total"));
        assert!(output.contains("synctv_file_object_delete_failures_total"));
    }

    #[test]
    fn test_websocket_message_metrics() {
        // Test WebSocket message counter with direction and type labels
        let before_inbound = http::WEBSOCKET_MESSAGES_TOTAL
            .with_label_values(&["inbound", "binary"])
            .get();
        http::WEBSOCKET_MESSAGES_TOTAL
            .with_label_values(&["inbound", "binary"])
            .inc();
        http::WEBSOCKET_MESSAGES_TOTAL
            .with_label_values(&["outbound", "binary"])
            .inc();
        http::WEBSOCKET_MESSAGES_TOTAL
            .with_label_values(&["inbound", "ping"])
            .inc();
        assert_eq!(
            http::WEBSOCKET_MESSAGES_TOTAL
                .with_label_values(&["inbound", "binary"])
                .get(),
            before_inbound + 1
        );

        // Test WebSocket errors
        let before_err = http::WEBSOCKET_ERRORS_TOTAL
            .with_label_values(&["decode_error"])
            .get();
        http::WEBSOCKET_ERRORS_TOTAL
            .with_label_values(&["decode_error"])
            .inc();
        assert_eq!(
            http::WEBSOCKET_ERRORS_TOTAL
                .with_label_values(&["decode_error"])
                .get(),
            before_err + 1
        );

        // Test WebSocket connection duration histogram
        http::WEBSOCKET_CONNECTION_DURATION_SECONDS
            .with_label_values(&[] as &[&str])
            .observe(45.0);

        let output = gather_metrics();
        assert!(
            output.contains("websocket_messages_total"),
            "Missing websocket_messages_total"
        );
        assert!(
            output.contains("websocket_errors_total"),
            "Missing websocket_errors_total"
        );
        assert!(
            output.contains("websocket_connection_duration_seconds"),
            "Missing websocket_connection_duration_seconds"
        );
    }

    #[test]
    fn test_business_metrics() {
        // Test playlist items counter
        let before_playlist = http::PLAYLIST_ITEMS_TOTAL
            .with_label_values(&[] as &[&str])
            .get();
        http::PLAYLIST_ITEMS_TOTAL
            .with_label_values(&[] as &[&str])
            .inc();
        assert_eq!(
            http::PLAYLIST_ITEMS_TOTAL
                .with_label_values(&[] as &[&str])
                .get(),
            before_playlist + 1
        );

        // Test chat messages counter
        let before_chat = http::CHAT_MESSAGES_TOTAL
            .with_label_values(&[] as &[&str])
            .get();
        http::CHAT_MESSAGES_TOTAL
            .with_label_values(&[] as &[&str])
            .inc();
        assert_eq!(
            http::CHAT_MESSAGES_TOTAL
                .with_label_values(&[] as &[&str])
                .get(),
            before_chat + 1
        );

        let output = gather_metrics();
        assert!(
            output.contains("playlist_items_total"),
            "Missing playlist_items_total"
        );
        assert!(
            output.contains("chat_messages_total"),
            "Missing chat_messages_total"
        );
    }

    #[test]
    fn test_cache_invalidation_and_operation_metrics() {
        // Test cache invalidations counter
        cache::CACHE_INVALIDATIONS
            .with_label_values(&["room"])
            .inc();
        cache::CACHE_INVALIDATIONS
            .with_label_values(&["user"])
            .inc();

        // Test cache operation duration histogram
        cache::CACHE_OPERATION_DURATION
            .with_label_values(&["get"])
            .observe(0.001);
        cache::CACHE_OPERATION_DURATION
            .with_label_values(&["set"])
            .observe(0.002);
        cache::CACHE_OPERATION_DURATION
            .with_label_values(&["invalidate"])
            .observe(0.005);

        let output = gather_metrics();
        assert!(
            output.contains("cache_invalidations_total"),
            "Missing cache_invalidations_total"
        );
        assert!(
            output.contains("cache_operation_duration_seconds"),
            "Missing cache_operation_duration_seconds"
        );
    }

    #[test]
    fn test_livestream_metrics() {
        // Test publisher/viewer gauges
        let before_pub = livestream::LIVESTREAM_ACTIVE_PUBLISHERS.get();
        livestream::LIVESTREAM_ACTIVE_PUBLISHERS.inc();
        assert_eq!(
            livestream::LIVESTREAM_ACTIVE_PUBLISHERS.get(),
            before_pub + 1
        );
        livestream::LIVESTREAM_ACTIVE_PUBLISHERS.dec();
        assert_eq!(livestream::LIVESTREAM_ACTIVE_PUBLISHERS.get(), before_pub);

        let before_view = livestream::LIVESTREAM_ACTIVE_VIEWERS.get();
        livestream::LIVESTREAM_ACTIVE_VIEWERS.inc();
        assert_eq!(livestream::LIVESTREAM_ACTIVE_VIEWERS.get(), before_view + 1);
        livestream::LIVESTREAM_ACTIVE_VIEWERS.dec();

        // Test bytes counter
        livestream::LIVESTREAM_BYTES_TOTAL
            .with_label_values(&["in"])
            .inc_by(1024.0);
        livestream::LIVESTREAM_BYTES_TOTAL
            .with_label_values(&["out"])
            .inc_by(2048.0);

        // Test stream duration histogram
        livestream::LIVESTREAM_STREAM_DURATION_SECONDS
            .with_label_values(&["rtmp"])
            .observe(120.0);

        // Test pull errors counter
        livestream::LIVESTREAM_PULL_ERRORS_TOTAL
            .with_label_values(&["connection"])
            .inc();
        livestream::LIVESTREAM_PULL_ERRORS_TOTAL
            .with_label_values(&["timeout"])
            .inc();

        let output = gather_metrics();
        assert!(
            output.contains("livestream_active_publishers"),
            "Missing livestream_active_publishers"
        );
        assert!(
            output.contains("livestream_active_viewers"),
            "Missing livestream_active_viewers"
        );
        assert!(
            output.contains("livestream_bytes_total"),
            "Missing livestream_bytes_total"
        );
        assert!(
            output.contains("livestream_stream_duration_seconds"),
            "Missing livestream_stream_duration_seconds"
        );
        assert!(
            output.contains("livestream_pull_errors_total"),
            "Missing livestream_pull_errors_total"
        );
    }

    #[test]
    fn test_gop_cache_metrics() {
        // Test GOP cache size gauge
        let before_size = livestream::GOP_CACHE_SIZE.get();
        livestream::GOP_CACHE_SIZE.set(5);
        assert_eq!(livestream::GOP_CACHE_SIZE.get(), 5);
        livestream::GOP_CACHE_SIZE.set(before_size);

        // Test GOP cache drops counter (tracks cumulative evictions)
        let before_drops = livestream::GOP_CACHE_DROPS_TOTAL.get();
        livestream::GOP_CACHE_DROPS_TOTAL.inc_by(3);
        assert_eq!(livestream::GOP_CACHE_DROPS_TOTAL.get(), before_drops + 3);

        // Test GOP cache memory bytes gauge
        let before_mem = livestream::GOP_CACHE_MEMORY_BYTES.get();
        livestream::GOP_CACHE_MEMORY_BYTES.set(50 * 1024 * 1024);
        assert_eq!(livestream::GOP_CACHE_MEMORY_BYTES.get(), 50 * 1024 * 1024);
        livestream::GOP_CACHE_MEMORY_BYTES.set(before_mem);

        let output = gather_metrics();
        assert!(output.contains("gop_cache_size"), "Missing gop_cache_size");
        assert!(
            output.contains("gop_cache_drops_total"),
            "Missing gop_cache_drops_total"
        );
        assert!(
            output.contains("gop_cache_memory_bytes"),
            "Missing gop_cache_memory_bytes"
        );
    }

    #[test]
    fn test_all_metrics_in_gathered_output() {
        // Touch all metric families to ensure they appear in gathered output.
        // IntCounterVec needs at least one label set touched.
        http::HTTP_REQUESTS_IN_FLIGHT.inc();
        http::HTTP_REQUESTS_IN_FLIGHT.dec();
        http::WEBSOCKET_CONNECTIONS_ACTIVE.set(0);
        http::WEBSOCKET_CONNECTIONS_TOTAL
            .with_label_values(&["success"])
            .inc();
        http::WEBSOCKET_MESSAGES_TOTAL
            .with_label_values(&["inbound", "binary"])
            .inc();
        http::WEBSOCKET_ERRORS_TOTAL
            .with_label_values(&["_test"])
            .inc();
        http::WEBSOCKET_CONNECTION_DURATION_SECONDS
            .with_label_values(&[] as &[&str])
            .observe(1.0);
        http::ROOMS_ACTIVE.set(0);
        http::USERS_ONLINE.set(0);
        http::STREAMS_ACTIVE.set(0);
        http::WEBRTC_PEERS_ACTIVE.set(0);
        http::PLAYLIST_ITEMS_TOTAL
            .with_label_values(&[] as &[&str])
            .inc();
        http::CHAT_MESSAGES_TOTAL
            .with_label_values(&[] as &[&str])
            .inc();
        database::DB_CONNECTIONS_ACTIVE.set(0);
        database::DB_CONNECTIONS_IDLE.set(0);
        database::DB_POOL_SIZE_MAX.set(0);
        grpc::GRPC_REQUESTS_TOTAL
            .with_label_values(&["_test", "_test", "_test"])
            .inc();
        grpc::GRPC_REQUEST_DURATION
            .with_label_values(&["_test", "_test", "_test"])
            .observe(0.0);
        cache::CACHE_INVALIDATIONS
            .with_label_values(&["_test"])
            .inc();
        cache::CACHE_OPERATION_DURATION
            .with_label_values(&["_test"])
            .observe(0.0);
        livestream::LIVESTREAM_ACTIVE_PUBLISHERS.set(0);
        livestream::LIVESTREAM_ACTIVE_VIEWERS.set(0);
        livestream::LIVESTREAM_BYTES_TOTAL
            .with_label_values(&["in"])
            .inc_by(0.0);
        livestream::LIVESTREAM_STREAM_DURATION_SECONDS
            .with_label_values(&["_test"])
            .observe(0.0);
        livestream::LIVESTREAM_PULL_ERRORS_TOTAL
            .with_label_values(&["_test"])
            .inc();
        livestream::GOP_CACHE_SIZE.set(0);
        livestream::GOP_CACHE_DROPS_TOTAL.inc();
        livestream::GOP_CACHE_MEMORY_BYTES.set(0);

        let output = gather_metrics();

        // HTTP metrics
        assert!(
            output.contains("websocket_connections_active"),
            "Missing websocket_connections_active"
        );
        assert!(
            output.contains("websocket_connections_total"),
            "Missing websocket_connections_total"
        );
        assert!(
            output.contains("websocket_messages_total"),
            "Missing websocket_messages_total"
        );
        assert!(
            output.contains("websocket_errors_total"),
            "Missing websocket_errors_total"
        );
        assert!(
            output.contains("websocket_connection_duration_seconds"),
            "Missing websocket_connection_duration_seconds"
        );
        assert!(output.contains("rooms_active"), "Missing rooms_active");
        assert!(output.contains("users_online"), "Missing users_online");
        assert!(output.contains("streams_active"), "Missing streams_active");
        assert!(
            output.contains("webrtc_peers_active"),
            "Missing webrtc_peers_active"
        );
        assert!(
            output.contains("playlist_items_total"),
            "Missing playlist_items_total"
        );
        assert!(
            output.contains("chat_messages_total"),
            "Missing chat_messages_total"
        );

        // Cache metrics
        assert!(
            output.contains("cache_invalidations_total"),
            "Missing cache_invalidations_total"
        );
        assert!(
            output.contains("cache_operation_duration_seconds"),
            "Missing cache_operation_duration_seconds"
        );

        // Livestream metrics
        assert!(
            output.contains("livestream_active_publishers"),
            "Missing livestream_active_publishers"
        );
        assert!(
            output.contains("livestream_active_viewers"),
            "Missing livestream_active_viewers"
        );
        assert!(
            output.contains("livestream_bytes_total"),
            "Missing livestream_bytes_total"
        );
        assert!(
            output.contains("livestream_stream_duration_seconds"),
            "Missing livestream_stream_duration_seconds"
        );
        assert!(
            output.contains("livestream_pull_errors_total"),
            "Missing livestream_pull_errors_total"
        );
        assert!(output.contains("gop_cache_size"), "Missing gop_cache_size");
        assert!(
            output.contains("gop_cache_drops_total"),
            "Missing gop_cache_drops_total"
        );
        assert!(
            output.contains("gop_cache_memory_bytes"),
            "Missing gop_cache_memory_bytes"
        );

        // Database metrics
        assert!(
            output.contains("db_connections_active"),
            "Missing db_connections_active"
        );
        assert!(
            output.contains("db_connections_idle"),
            "Missing db_connections_idle"
        );
        assert!(
            output.contains("db_pool_size_max"),
            "Missing db_pool_size_max"
        );

        // gRPC metrics
        assert!(
            output.contains("grpc_requests_total"),
            "Missing grpc_requests_total"
        );
        assert!(
            output.contains("grpc_request_duration_seconds"),
            "Missing grpc_request_duration_seconds"
        );
    }

    #[test]
    fn test_cluster_health_metrics() {
        // Test Cluster health metrics
        cluster::NODE_HEALTH_STATUS.set(1);
        assert_eq!(cluster::NODE_HEALTH_STATUS.get(), 1);

        cluster::LEADER_ELECTION_STATE.set(1);
        assert_eq!(cluster::LEADER_ELECTION_STATE.get(), 1);

        cluster::REDIS_PUBSUB_HEALTH.set(1);
        assert_eq!(cluster::REDIS_PUBSUB_HEALTH.get(), 1);

        cluster::CLUSTER_MEMBER_COUNT.set(3);
        assert_eq!(cluster::CLUSTER_MEMBER_COUNT.get(), 3);

        cluster::LEADER_ELECTION_DURATION
            .with_label_values(&["success"])
            .observe(0.5);

        cluster::REDIS_PUBSUB_PUBLISH_LATENCY
            .with_label_values(&["realtime_events"])
            .observe(0.002);

        cluster::NODE_MESSAGE_LATENCY
            .with_label_values(&["sync"])
            .observe(0.010);

        cluster::CLUSTER_SYNC_ERRORS
            .with_label_values(&["timeout"])
            .inc();

        cluster::NODE_LAST_HEARTBEAT.set(1_234_567_890.0);

        let output = gather_metrics();
        assert!(
            output.contains("synctv_cluster_node_health_status"),
            "Missing node health status"
        );
        assert!(
            output.contains("synctv_cluster_leader_election_state"),
            "Missing leader election state"
        );
        assert!(
            output.contains("synctv_cluster_redis_pubsub_health"),
            "Missing redis pubsub health"
        );
        assert!(
            output.contains("synctv_cluster_member_count"),
            "Missing cluster member count"
        );
        assert!(
            output.contains("synctv_cluster_leader_election_duration_seconds"),
            "Missing leader election duration"
        );
        assert!(
            output.contains("synctv_cluster_redis_pubsub_publish_latency_seconds"),
            "Missing redis pubsub latency"
        );
        assert!(
            output.contains("synctv_cluster_node_message_latency_seconds"),
            "Missing node message latency"
        );
        assert!(
            output.contains("synctv_cluster_sync_errors_total"),
            "Missing cluster sync errors"
        );
        assert!(
            output.contains("synctv_cluster_node_last_heartbeat_timestamp"),
            "Missing node last heartbeat"
        );
    }

    #[test]
    fn test_redis_hot_path_metrics() {
        // Test Redis hot path metrics
        redis::REDIS_OPERATION_DURATION
            .with_label_values(&["get"])
            .observe(0.002);

        redis::REDIS_OPERATIONS_TOTAL
            .with_label_values(&["get", "success"])
            .inc();

        redis::REDIS_OPERATIONS_TOTAL
            .with_label_values(&["set", "error"])
            .inc();

        let output = gather_metrics();
        assert!(
            output.contains("redis_operation_duration_seconds"),
            "Missing redis operation duration"
        );
        assert!(
            output.contains("redis_operations_total"),
            "Missing redis operations total"
        );
    }

    #[test]
    fn test_hot_path_metrics() {
        // Test Hot path metrics
        hot_paths::API_HOT_PATH_LATENCY
            .with_label_values(&["/api/rooms/:id", "GET"])
            .observe(0.015);

        hot_paths::DB_HOT_PATH_LATENCY
            .with_label_values(&["get_room_by_id", "rooms"])
            .observe(0.003);

        hot_paths::REDIS_HOT_PATH_LATENCY
            .with_label_values(&["get", "room:*"])
            .observe(0.001);

        let output = gather_metrics();
        assert!(
            output.contains("api_hot_path_latency_seconds"),
            "Missing API hot path latency"
        );
        assert!(
            output.contains("db_hot_path_latency_seconds"),
            "Missing DB hot path latency"
        );
        assert!(
            output.contains("redis_hot_path_latency_seconds"),
            "Missing Redis hot path latency"
        );
    }

    #[test]
    fn test_http_error_rate_metrics() {
        // Test HTTP error rate metrics
        http::HTTP_ERROR_RATE
            .with_label_values(&["GET", "/api/rooms/:id", "timeout"])
            .inc();

        http::HTTP_ERROR_RATE
            .with_label_values(&["POST", "/api/rooms", "validation"])
            .inc();

        let output = gather_metrics();
        assert!(
            output.contains("http_error_rate_total"),
            "Missing HTTP error rate"
        );
    }

    #[test]
    fn test_database_operations_total() {
        // Test Database operations total
        database::DB_OPERATIONS_TOTAL
            .with_label_values(&["select", "rooms", "success"])
            .inc();

        database::DB_OPERATIONS_TOTAL
            .with_label_values(&["insert", "messages", "error"])
            .inc();

        let output = gather_metrics();
        assert!(
            output.contains("db_operations_total"),
            "Missing db operations total"
        );
    }
}
