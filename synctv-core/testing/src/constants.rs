//! Test constants for synctv project tests
//!
//! This module provides centralized constants for testing to eliminate
//! magic numbers and improve test maintainability.

/// Brute force protection thresholds (matching production defaults)
pub mod brute_force {
    /// Tier 1 lockout threshold (failed attempts)
    pub const TIER1_THRESHOLD: u32 = 5;

    /// Tier 2 lockout threshold (failed attempts)
    pub const TIER2_THRESHOLD: u32 = 10;

    /// IP lockout threshold (failed attempts from single IP)
    pub const IP_THRESHOLD: u32 = 20;

    /// Tier 1 lockout duration in seconds (60 seconds)
    pub const TIER1_LOCKOUT_SECS: u64 = 60;

    /// Tier 2 lockout duration in seconds (15 minutes)
    pub const TIER2_LOCKOUT_SECS: u64 = 900;

    /// Tier 3 lockout duration in seconds (1 hour)
    pub const TIER3_LOCKOUT_SECS: u64 = 3600;

    /// IP lockout duration in seconds (24 hours)
    pub const IP_LOCKOUT_SECS: u64 = 86400;
}

/// Token blacklist constants
pub mod token_blacklist {
    /// Default capacity for in-memory blacklist
    pub const CAPACITY: usize = 10_000;

    /// Short TTL for tokens (1 hour in seconds)
    pub const SHORT_TTL_SECS: u64 = 3600;

    /// Long TTL for tokens (24 hours in seconds)
    pub const LONG_TTL_SECS: u64 = 86_400;

    /// Default TTL for blacklisted tokens
    pub const DEFAULT_TTL_SECS: u64 = SHORT_TTL_SECS;
}

/// Network test constants
pub mod network {
    /// Localhost address for testing
    pub const LOCALHOST: &str = "127.0.0.1";

    /// Test proxy IP address
    pub const PROXY_IP: &str = "10.0.0.1";

    /// Test client IP address
    pub const CLIENT_IP: &str = "192.168.1.100";

    /// Alternative test client IP address
    pub const CLIENT_IP_2: &str = "192.168.1.101";

    /// IPv6 localhost for testing
    pub const LOCALHOST_IPV6: &str = "::1";
}

/// Test timeout constants
pub mod timeout {
    /// Default timeout for quick operations (100ms)
    pub const QUICK_MS: u64 = 100;

    /// Standard timeout for most operations (1 second)
    pub const DEFAULT_MS: u64 = 1_000;

    /// Long timeout for slow operations (5 seconds)
    pub const LONG_MS: u64 = 5_000;

    /// Extra long timeout for very slow operations (30 seconds)
    pub const EXTRA_LONG_MS: u64 = 30_000;
}

/// Cache test constants
pub mod cache {
    /// L1 cache size for testing
    pub const L1_SIZE: usize = 1_000;

    /// L2 cache TTL for testing (5 minutes)
    pub const L2_TTL_SECS: u64 = 300;

    /// Cache invalidation timeout (10 seconds)
    pub const INVALIDATION_TIMEOUT_SECS: u64 = 10;
}

/// Rate limiting test constants
pub mod rate_limit {
    /// Default rate limit (requests per minute)
    pub const DEFAULT_RPM: u32 = 60;

    /// Burst capacity for rate limiting
    pub const BURST: u32 = 10;

    /// Rate limit window in seconds
    pub const WINDOW_SECS: u64 = 60;
}

/// WebSocket test constants
pub mod websocket {
    /// WebSocket message channel capacity
    pub const CHANNEL_CAPACITY: usize = 100;

    /// WebSocket heartbeat interval in seconds
    pub const HEARTBEAT_INTERVAL_SECS: u64 = 30;

    /// WebSocket connection timeout in seconds
    pub const CONNECTION_TIMEOUT_SECS: u64 = 10;

    /// Maximum message size in bytes
    pub const MAX_MESSAGE_SIZE_BYTES: usize = 65_536;
}

/// Redis test constants
pub mod redis {
    /// Default Redis port for testing
    pub const DEFAULT_PORT: u16 = 6379;

    /// Redis key prefix for test isolation
    pub const TEST_KEY_PREFIX: &str = "test:";

    /// Connection pool size for tests
    pub const POOL_SIZE: u32 = 10;
}

/// `PostgreSQL` test constants
pub mod postgres {
    /// Default `PostgreSQL` port for testing
    pub const DEFAULT_PORT: u16 = 5432;

    /// Connection pool size for tests
    pub const POOL_SIZE: u32 = 10;

    /// Maximum connections for tests
    pub const MAX_CONNECTIONS: u32 = 100;
}

/// Room test constants
pub mod room {
    /// Default max members for a test room
    pub const DEFAULT_MAX_MEMBERS: u64 = 100;

    /// Room name max length
    pub const NAME_MAX_LENGTH: usize = 100;

    /// Room description max length
    pub const DESCRIPTION_MAX_LENGTH: usize = 500;
}

/// Media test constants
pub mod media {
    /// Media URL max length
    pub const URL_MAX_LENGTH: usize = 2048;

    /// Default chunk size for streaming (in bytes)
    pub const DEFAULT_CHUNK_SIZE: usize = 8192;

    /// Buffer size for media processing
    pub const BUFFER_SIZE: usize = 65_536;
}
