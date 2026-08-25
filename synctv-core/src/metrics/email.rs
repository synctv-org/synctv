use super::*;

pub static EMAIL_DELIVERY_QUEUE_DEPTH: std::sync::LazyLock<IntGauge> =
    std::sync::LazyLock::new(|| {
        int_gauge(
            "email_delivery_queue_depth",
            "Number of queued email delivery jobs",
        )
    });

pub static EMAIL_DELIVERY_IN_FLIGHT: std::sync::LazyLock<IntGauge> =
    std::sync::LazyLock::new(|| {
        int_gauge(
            "email_delivery_in_flight",
            "Number of email delivery jobs currently being processed",
        )
    });

pub static EMAIL_DELIVERY_JOBS_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "email_delivery_jobs_total",
            "Total email delivery job transitions",
            &["kind", "status"],
        )
    });

pub static EMAIL_DELIVERY_DURATION_SECONDS: std::sync::LazyLock<HistogramVec> =
    std::sync::LazyLock::new(|| {
        histogram_vec(
            HistogramOpts::new(
                "email_delivery_duration_seconds",
                "Email delivery processing duration in seconds",
            )
            .buckets(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0]),
            &["kind", "status"],
        )
    });
