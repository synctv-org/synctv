use super::*;

const WEBSOCKET_SUCCESS: &str = "success";

/// Total HTTP requests, labeled by method, path, and status code.
pub static HTTP_REQUESTS_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "http_requests_total",
            "Total number of HTTP requests",
            &["method", "path", "status"],
        )
    });

/// HTTP request duration in seconds, labeled by method and path.
pub static HTTP_REQUEST_DURATION_SECONDS: std::sync::LazyLock<HistogramVec> =
    std::sync::LazyLock::new(|| {
        histogram_vec(
            HistogramOpts::new(
                "http_request_duration_seconds",
                "HTTP request duration in seconds (P50/P95/P99)",
            )
            .buckets(vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ]),
            &["method", "path"],
        )
    });

/// Number of in-flight HTTP requests.
pub static HTTP_REQUESTS_IN_FLIGHT: std::sync::LazyLock<IntGauge> =
    std::sync::LazyLock::new(|| {
        int_gauge(
            "http_requests_in_flight",
            "Number of HTTP requests currently being processed",
        )
    });

/// Active WebSocket connections.
pub static WEBSOCKET_CONNECTIONS_ACTIVE: std::sync::LazyLock<IntGauge> =
    std::sync::LazyLock::new(|| {
        int_gauge(
            "websocket_connections_active",
            "Number of active WebSocket connections",
        )
    });

/// Total WebSocket connections opened, labeled by connection outcome.
pub static WEBSOCKET_CONNECTIONS_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "websocket_connections_total",
            "Total number of WebSocket connections opened",
            &["status"],
        )
    });

/// Total WebSocket errors, labeled by error type.
pub static WEBSOCKET_ERRORS_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "websocket_errors_total",
            "Total number of WebSocket errors",
            &["error_type"],
        )
    });

pub fn start_request() -> GaugeGuard {
    GaugeGuard::increment(&HTTP_REQUESTS_IN_FLIGHT)
}

pub fn record_request(method: &str, path: &str, status: u16, elapsed: std::time::Duration) {
    let status = status.to_string();
    HTTP_REQUESTS_TOTAL
        .with_label_values(&[method, path, &status])
        .inc();
    HTTP_REQUEST_DURATION_SECONDS
        .with_label_values(&[method, path])
        .observe(elapsed.as_secs_f64());
}

pub fn track_websocket_connection() -> GaugeGuard {
    WEBSOCKET_CONNECTIONS_TOTAL
        .with_label_values(&[WEBSOCKET_SUCCESS])
        .inc();
    GaugeGuard::increment(&WEBSOCKET_CONNECTIONS_ACTIVE)
}

pub fn record_websocket_error(error_type: &'static str) {
    WEBSOCKET_ERRORS_TOTAL
        .with_label_values(&[error_type])
        .inc();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_facade_uses_bounded_route_labels() {
        let requests = HTTP_REQUESTS_TOTAL
            .with_label_values(&["GET", "/items/{item_id}", "200"])
            .get();
        let observations = HTTP_REQUEST_DURATION_SECONDS
            .with_label_values(&["GET", "/items/{item_id}"])
            .get_sample_count();

        record_request(
            "GET",
            "/items/{item_id}",
            200,
            std::time::Duration::from_millis(5),
        );

        assert_eq!(
            HTTP_REQUESTS_TOTAL
                .with_label_values(&["GET", "/items/{item_id}", "200"])
                .get(),
            requests + 1
        );
        assert_eq!(
            HTTP_REQUEST_DURATION_SECONDS
                .with_label_values(&["GET", "/items/{item_id}"])
                .get_sample_count(),
            observations + 1
        );
    }
}
