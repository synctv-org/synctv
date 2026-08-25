//! Prometheus metrics collection for production monitoring.

use prometheus::{GaugeVec, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge};

mod guard;
mod registry;

pub use guard::{GaugeGuard, InFlightTimer};
pub use registry::MetricsError;
use registry::{gather, gauge_vec, histogram_vec, int_counter, int_counter_vec, int_gauge};

/// HTTP and WebSocket transport metrics.
pub mod http;

/// Application runtime metrics shared by core services and transport adapters.
pub mod application;

/// Asynchronous email delivery metrics.
pub mod email;

/// Active connections gauge
pub static ACTIVE_CONNECTIONS: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
    int_gauge("active_connections", "Current number of active connections")
});

pub mod cache;

/// Database operations
pub mod database;

/// Remote transport operations.
pub mod remote_transport;

pub mod cluster;

pub mod file_storage;

pub mod task;

pub mod logging;

pub mod rate_limit;

pub mod stream;

pub mod streamhub;

pub mod livestream;

/// Registers every metric family so definition conflicts fail when metrics start.
pub fn initialize() {
    fn force<T>(metric: &'static std::sync::LazyLock<T>) {
        std::sync::LazyLock::force(metric);
    }

    force(&http::HTTP_REQUESTS_TOTAL);
    force(&http::HTTP_REQUEST_DURATION_SECONDS);
    force(&http::HTTP_REQUESTS_IN_FLIGHT);
    force(&http::WEBSOCKET_CONNECTIONS_ACTIVE);
    force(&http::WEBSOCKET_CONNECTIONS_TOTAL);
    force(&http::WEBSOCKET_ERRORS_TOTAL);
    force(&application::ROOMS_ACTIVE);
    force(&application::USERS_ONLINE);
    force(&application::STREAMS_ACTIVE);
    force(&application::WEBRTC_PEERS_ACTIVE);
    force(&application::CHAT_MESSAGES_TOTAL);
    force(&email::EMAIL_DELIVERY_QUEUE_DEPTH);
    force(&email::EMAIL_DELIVERY_IN_FLIGHT);
    force(&email::EMAIL_DELIVERY_JOBS_TOTAL);
    force(&email::EMAIL_DELIVERY_DURATION_SECONDS);
    force(&ACTIVE_CONNECTIONS);
    force(&cache::CACHE_HITS);
    force(&cache::CACHE_MISSES);
    force(&cache::CACHE_EVICTIONS);
    force(&cache::CACHE_ERRORS);
    force(&cache::CACHE_INVALIDATIONS);
    force(&cache::CACHE_OPERATION_DURATION);
    force(&cache::CACHE_LAG_FLUSH_TOTAL);
    force(&cache::CACHE_FENCE_OPERATIONS_TOTAL);
    force(&cache::CACHE_DB_FALLBACK_TOTAL);
    force(&cache::CACHE_STALE_WRITE_REJECT_TOTAL);
    force(&cache::CACHE_FENCE_PENDING);
    force(&cache::CACHE_FENCE_REPAIR_TOTAL);
    force(&cache::CACHE_FENCE_DB_COMPARE);
    force(&database::DB_CONNECTIONS_ACTIVE);
    force(&database::DB_POOL_UTILIZATION);
    force(&database::DB_POOL_SIZE_MAX);
    force(&database::DB_CONNECTIONS_IDLE);
    force(&remote_transport::REMOTE_TRANSPORT_REQUESTS_TOTAL);
    force(&remote_transport::REMOTE_TRANSPORT_REQUEST_DURATION);
    force(&cluster::CLUSTER_CONNECTIONS);
    force(&cluster::NODE_ACTIVE_ROOMS);
    force(&cluster::REALTIME_EVENTS_PUBLISHED);
    force(&cluster::REALTIME_EVENTS_RECEIVED);
    force(&cluster::REALTIME_EVENTS_DROPPED);
    force(&cluster::CLUSTER_HEARTBEAT_FAILURES);
    force(&cluster::LEADER_ELECTION_STATE);
    force(&cluster::LEADER_ELECTION_EPOCH);
    force(&cluster::LEADER_ELECTION_CONSECUTIVE_FAILURES);
    force(&cluster::CLUSTER_EPOCH_MISMATCH_QUARANTINE);
    force(&cluster::LEADER_ELECTION_MODE);
    force(&cluster::DISTRIBUTED_COUNTER_TTL_REFRESHES);
    force(&cluster::DISTRIBUTED_COUNTER_TTL_KEYS_REFRESHED);
    force(&cluster::DISTRIBUTED_COUNTER_TTL_CONSECUTIVE_FAILURES);
    force(&file_storage::FILE_OBJECT_DELETE_ATTEMPTS);
    force(&file_storage::FILE_OBJECT_DELETE_FAILURES);
    force(&file_storage::FILE_CLEANUP_JOBS_DUE);
    force(&file_storage::FILE_CLEANUP_JOBS_TOTAL);
    force(&task::TASK_PANICS_TOTAL);
    force(&logging::LOGGING_DROPPED_LINES_TOTAL);
    force(&rate_limit::RATE_LIMIT_REDIS_FALLBACKS_TOTAL);
    force(&stream::STREAM_RELAY_DURATION);
    force(&stream::ACTIVE_RELAY_STREAMS);
    force(&stream::STREAM_ERRORS);
    force(&streamhub::STREAMHUB_RESTARTS_TOTAL);
    force(&livestream::PUBLISHER_HEARTBEAT_FAILURES);
    force(&livestream::LIVESTREAM_ACTIVE_PUBLISHERS);
    force(&livestream::LIVESTREAM_ACTIVE_VIEWERS);
    force(&livestream::LIVESTREAM_RELAY_FRAME_DROPS);
    force(&livestream::LIVESTREAM_FLV_SLOW_CLIENT_TERMINATIONS_TOTAL);
}

/// Encodes the current registry in the Prometheus text exposition format.
pub fn gather_metrics() -> Result<String, MetricsError> {
    logging::sync_dropped_lines(&crate::logging::dropped_lines_by_component());
    gather()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn parse_catalog(source: &str) -> BTreeMap<String, (&str, Vec<String>)> {
        source
            .lines()
            .filter_map(|line| {
                let columns = line.split('|').map(str::trim).collect::<Vec<_>>();
                let name = columns.get(1)?.strip_prefix('`')?.strip_suffix('`')?;
                let kind = *columns.get(2)?;
                if !matches!(kind, "counter" | "gauge" | "histogram") {
                    return None;
                }
                let labels = columns
                    .get(3)?
                    .split(',')
                    .map(str::trim)
                    .filter_map(|label| {
                        label
                            .strip_prefix('`')
                            .and_then(|label| label.strip_suffix('`'))
                            .map(str::to_string)
                    })
                    .collect();
                Some((name.to_string(), (kind, labels)))
            })
            .collect()
    }

    #[test]
    fn every_metric_definition_is_eagerly_initialized() {
        let sources = [
            include_str!("metrics.rs"),
            include_str!("metrics/http.rs"),
            include_str!("metrics/application.rs"),
            include_str!("metrics/email.rs"),
            include_str!("metrics/cache.rs"),
            include_str!("metrics/database.rs"),
            include_str!("metrics/remote_transport.rs"),
            include_str!("metrics/cluster.rs"),
            include_str!("metrics/file_storage.rs"),
            include_str!("metrics/task.rs"),
            include_str!("metrics/logging.rs"),
            include_str!("metrics/rate_limit.rs"),
            include_str!("metrics/stream.rs"),
            include_str!("metrics/streamhub.rs"),
            include_str!("metrics/livestream.rs"),
        ];
        let definition_count = sources
            .iter()
            .flat_map(|source| source.lines())
            .filter(|line| line.trim_start().starts_with("pub static "))
            .count();
        let initialization_count = sources[0]
            .lines()
            .filter(|line| line.trim_start().starts_with("force(&"))
            .count();

        assert_eq!(definition_count, initialization_count);
    }

    #[test]
    fn metrics_catalog_matches_registered_descriptors() {
        initialize();
        let descriptors = registry::descriptors()
            .into_iter()
            .map(|(name, descriptor)| {
                let kind = match descriptor.kind {
                    registry::MetricKind::Counter => "counter",
                    registry::MetricKind::Gauge => "gauge",
                    registry::MetricKind::Histogram => "histogram",
                };
                (name, (kind, descriptor.labels))
            })
            .collect::<BTreeMap<_, _>>();
        let english = parse_catalog(include_str!(
            "../../docs/src/content/docs/en/reference/metrics-catalog.mdx"
        ));
        let chinese = parse_catalog(include_str!(
            "../../docs/src/content/docs/reference/metrics-catalog.mdx"
        ));

        assert_eq!(english, descriptors);
        assert_eq!(chinese, descriptors);
    }

    #[test]
    fn representative_metrics_are_encoded_with_expected_names() {
        initialize();
        // Vector families appear after their first labeled sample.
        http::HTTP_REQUESTS_IN_FLIGHT.inc();
        http::HTTP_REQUESTS_IN_FLIGHT.dec();
        http::WEBSOCKET_CONNECTIONS_ACTIVE.set(0);
        http::WEBSOCKET_CONNECTIONS_TOTAL
            .with_label_values(&["success"])
            .inc();
        http::WEBSOCKET_ERRORS_TOTAL
            .with_label_values(&["_test"])
            .inc();
        application::ROOMS_ACTIVE.set(0);
        application::USERS_ONLINE.set(0);
        application::STREAMS_ACTIVE.set(0);
        application::WEBRTC_PEERS_ACTIVE.set(0);
        application::CHAT_MESSAGES_TOTAL.inc();
        email::EMAIL_DELIVERY_QUEUE_DEPTH.set(0);
        email::EMAIL_DELIVERY_IN_FLIGHT.set(0);
        email::EMAIL_DELIVERY_JOBS_TOTAL
            .with_label_values(&["_test", "queued"])
            .inc();
        email::EMAIL_DELIVERY_DURATION_SECONDS
            .with_label_values(&["_test", "sent"])
            .observe(0.0);
        database::DB_CONNECTIONS_ACTIVE.set(0);
        database::DB_CONNECTIONS_IDLE.set(0);
        database::DB_POOL_SIZE_MAX.set(0);
        remote_transport::REMOTE_TRANSPORT_REQUESTS_TOTAL
            .with_label_values(&["_test", "_test", "_test"])
            .inc();
        remote_transport::REMOTE_TRANSPORT_REQUEST_DURATION
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
        logging::sync_dropped_lines(&[("_test".to_string(), 0)]);

        let output = gather_metrics().expect("metrics should encode");

        assert!(
            output.contains("websocket_connections_active"),
            "Missing websocket_connections_active"
        );
        assert!(
            output.contains("websocket_connections_total"),
            "Missing websocket_connections_total"
        );
        assert!(
            output.contains("websocket_errors_total"),
            "Missing websocket_errors_total"
        );
        assert!(
            output.contains("logging_dropped_lines_total"),
            "Missing logging_dropped_lines_total"
        );
        assert!(output.contains("rooms_active"), "Missing rooms_active");
        assert!(output.contains("users_online"), "Missing users_online");
        assert!(output.contains("streams_active"), "Missing streams_active");
        assert!(
            output.contains("webrtc_peers_active"),
            "Missing webrtc_peers_active"
        );
        assert!(
            output.contains("chat_messages_total"),
            "Missing chat_messages_total"
        );
        assert!(
            output.contains("email_delivery_queue_depth"),
            "Missing email_delivery_queue_depth"
        );
        assert!(
            output.contains("email_delivery_in_flight"),
            "Missing email_delivery_in_flight"
        );
        assert!(
            output.contains("email_delivery_jobs_total"),
            "Missing email_delivery_jobs_total"
        );
        assert!(
            output.contains("email_delivery_duration_seconds"),
            "Missing email_delivery_duration_seconds"
        );

        assert!(
            output.contains("cache_invalidations_total"),
            "Missing cache_invalidations_total"
        );
        assert!(
            output.contains("cache_operation_duration_seconds"),
            "Missing cache_operation_duration_seconds"
        );

        assert!(
            output.contains("livestream_active_publishers"),
            "Missing livestream_active_publishers"
        );
        assert!(
            output.contains("livestream_active_viewers"),
            "Missing livestream_active_viewers"
        );
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
    fn logging_dropped_lines_are_synchronized_as_counter_deltas() {
        const COMPONENT: &str = "_logging_delta_test";
        let counter = logging::LOGGING_DROPPED_LINES_TOTAL.with_label_values(&[COMPONENT]);
        let initial = counter.get();

        logging::sync_dropped_lines(&[(COMPONENT.to_string(), 3)]);
        assert_eq!(counter.get(), initial + 3);

        logging::sync_dropped_lines(&[(COMPONENT.to_string(), 5)]);
        assert_eq!(counter.get(), initial + 5);
    }
}
