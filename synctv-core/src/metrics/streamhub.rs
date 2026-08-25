use super::*;

pub static STREAMHUB_RESTARTS_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "streamhub_restarts_total",
            "Total number of StreamHub event loop restarts",
            &["reason"],
        )
    });
