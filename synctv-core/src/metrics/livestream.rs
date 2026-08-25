use super::*;

pub static PUBLISHER_HEARTBEAT_FAILURES: std::sync::LazyLock<IntCounter> =
    std::sync::LazyLock::new(|| {
        int_counter(
            "synctv_publisher_heartbeat_failures_total",
            "Total publisher cleanups due to heartbeat failure",
        )
    });

pub static LIVESTREAM_ACTIVE_PUBLISHERS: std::sync::LazyLock<IntGauge> =
    std::sync::LazyLock::new(|| {
        int_gauge(
            "livestream_active_publishers",
            "Number of active livestream publishers",
        )
    });

pub static LIVESTREAM_ACTIVE_VIEWERS: std::sync::LazyLock<IntGauge> =
    std::sync::LazyLock::new(|| {
        int_gauge(
            "livestream_active_viewers",
            "Number of active livestream viewers",
        )
    });

pub static LIVESTREAM_RELAY_FRAME_DROPS: std::sync::LazyLock<IntCounter> =
    std::sync::LazyLock::new(|| {
        int_counter(
            "livestream_relay_frame_drops_total",
            "Total relay frames dropped due to backpressure",
        )
    });

pub static LIVESTREAM_FLV_SLOW_CLIENT_TERMINATIONS_TOTAL: std::sync::LazyLock<IntCounter> =
    std::sync::LazyLock::new(|| {
        int_counter(
            "livestream_flv_slow_client_terminations_total",
            "Total FLV stream terminations due to slow client",
        )
    });
