use std::time::Duration;

use synctv_core::config::ConnectionLimitsConfig;

/// Connection limits configuration
#[derive(Debug, Clone)]
pub struct ConnectionLimits {
    /// Maximum connections per user
    pub max_per_user: usize,

    /// Maximum connections per room
    pub max_per_room: usize,

    /// Maximum total connections
    pub max_total: usize,

    /// Idle timeout (disconnect if no activity)
    pub idle_timeout: Duration,

    /// Maximum connection duration
    pub max_duration: Duration,

    /// WebRTC session timeout (remove from RTC-joined set if inactive)
    pub webrtc_session_timeout: Duration,
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        Self::from(ConnectionLimitsConfig::default())
    }
}

impl From<ConnectionLimitsConfig> for ConnectionLimits {
    fn from(config: ConnectionLimitsConfig) -> Self {
        Self::from(&config)
    }
}

impl From<&ConnectionLimitsConfig> for ConnectionLimits {
    fn from(config: &ConnectionLimitsConfig) -> Self {
        Self {
            max_per_user: config.max_per_user,
            max_per_room: config.max_per_room,
            max_total: config.max_total,
            idle_timeout: Duration::from_secs(config.idle_timeout_seconds),
            max_duration: Duration::from_secs(config.max_duration_seconds),
            webrtc_session_timeout: Duration::from_hours(2),
        }
    }
}
