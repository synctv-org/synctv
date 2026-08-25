use super::*;

pub static ROOMS_ACTIVE: std::sync::LazyLock<IntGauge> =
    std::sync::LazyLock::new(|| int_gauge("rooms_active", "Number of currently active rooms"));

pub static USERS_ONLINE: std::sync::LazyLock<IntGauge> =
    std::sync::LazyLock::new(|| int_gauge("users_online", "Number of currently online users"));

pub static STREAMS_ACTIVE: std::sync::LazyLock<IntGauge> =
    std::sync::LazyLock::new(|| int_gauge("streams_active", "Number of active live streams"));

pub static WEBRTC_PEERS_ACTIVE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
    int_gauge(
        "webrtc_peers_active",
        "Number of active WebRTC peer connections",
    )
});

pub static CHAT_MESSAGES_TOTAL: std::sync::LazyLock<IntCounter> = std::sync::LazyLock::new(|| {
    int_counter("chat_messages_total", "Total number of chat messages sent")
});
