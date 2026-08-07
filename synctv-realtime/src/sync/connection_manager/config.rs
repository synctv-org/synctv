use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ConnectionLimitsOptions {
    pub max_per_user: usize,
    pub max_per_room: usize,
    pub max_total: usize,
    pub idle_timeout_seconds: u64,
    pub max_duration_seconds: u64,
    pub ws_message_rate_limit_per_second: u32,
}

impl Default for ConnectionLimitsOptions {
    fn default() -> Self {
        Self {
            max_per_user: 20,
            max_per_room: 2000,
            max_total: 100_000,
            idle_timeout_seconds: 300,
            max_duration_seconds: 86400,
            ws_message_rate_limit_per_second: 50,
        }
    }
}

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
        Self::from(ConnectionLimitsOptions::default())
    }
}

impl From<ConnectionLimitsOptions> for ConnectionLimits {
    fn from(options: ConnectionLimitsOptions) -> Self {
        Self::from(&options)
    }
}

impl From<&ConnectionLimitsOptions> for ConnectionLimits {
    fn from(options: &ConnectionLimitsOptions) -> Self {
        Self {
            max_per_user: options.max_per_user,
            max_per_room: options.max_per_room,
            max_total: options.max_total,
            idle_timeout: Duration::from_secs(options.idle_timeout_seconds),
            max_duration: Duration::from_secs(options.max_duration_seconds),
            webrtc_session_timeout: Duration::from_hours(2),
        }
    }
}
