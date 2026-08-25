use super::*;

pub static REMOTE_TRANSPORT_REQUESTS_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "grpc_requests_total",
            "Total number of remote transport requests",
            &["service", "method", "status"],
        )
    });

pub static REMOTE_TRANSPORT_REQUEST_DURATION: std::sync::LazyLock<HistogramVec> =
    std::sync::LazyLock::new(|| {
        histogram_vec(
            HistogramOpts::new(
                "grpc_request_duration_seconds",
                "Remote transport request duration in seconds",
            ),
            &["service", "method", "status"],
        )
    });

pub fn record(service: &str, method: &str, status: &str, elapsed: std::time::Duration) {
    let labels = &[service, method, status];
    REMOTE_TRANSPORT_REQUESTS_TOTAL
        .with_label_values(labels)
        .inc();
    REMOTE_TRANSPORT_REQUEST_DURATION
        .with_label_values(labels)
        .observe(elapsed.as_secs_f64());
}
