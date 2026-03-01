//! Production-grade resilience patterns for external services
//!
//! This module provides timeout configuration and re-exports production-grade
//! circuit breaker (`failsafe`) and retry (`backon`) crates.

pub mod timeout {
    //! Timeout configuration for external service calls

    use std::time::Duration;

    /// Default timeout for database operations.
    ///
    /// This 30-second timeout is applied to all connections via `after_connect`
    /// (see `bootstrap/database.rs`) and is appropriate for OLTP workloads. It aligns
    /// with industry standards (Heroku, AWS RDS, Supabase all use 30s defaults).
    ///
    /// If queries exceed this timeout regularly, they should be optimized rather
    /// than given more time. Use `pg_stat_statements` to identify slow queries.
    ///
    /// For deployment-specific needs, modify this constant or make it configurable
    /// via `Config`. Runtime adjustment is not recommended as it adds complexity
    /// without clear benefit for this application's query patterns.
    pub const DB_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

    /// Default timeout for Redis operations
    pub const REDIS_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);

    /// Default timeout for external HTTP requests
    pub const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

    /// Default timeout for gRPC calls
    pub const GRPC_CALL_TIMEOUT: Duration = Duration::from_secs(30);

    /// Timeout configuration
    #[derive(Debug, Clone, Copy)]
    pub struct TimeoutConfig {
        pub db_query: Duration,
        pub redis: Duration,
        pub http: Duration,
        pub grpc: Duration,
    }

    impl Default for TimeoutConfig {
        fn default() -> Self {
            Self {
                db_query: DB_QUERY_TIMEOUT,
                redis: REDIS_OPERATION_TIMEOUT,
                http: HTTP_REQUEST_TIMEOUT,
                grpc: GRPC_CALL_TIMEOUT,
            }
        }
    }

    impl TimeoutConfig {
        /// Create custom timeout config
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Set database query timeout
        #[must_use]
        pub const fn with_db_query_timeout(mut self, timeout: Duration) -> Self {
            self.db_query = timeout;
            self
        }

        /// Set Redis timeout
        #[must_use]
        pub const fn with_redis_timeout(mut self, timeout: Duration) -> Self {
            self.redis = timeout;
            self
        }

        /// Set HTTP request timeout
        #[must_use]
        pub const fn with_http_timeout(mut self, timeout: Duration) -> Self {
            self.http = timeout;
            self
        }

        /// Set gRPC timeout
        #[must_use]
        pub const fn with_grpc_timeout(mut self, timeout: Duration) -> Self {
            self.grpc = timeout;
            self
        }
    }
}

pub mod retry {
    //! Retry utilities
    //!
    //! Primary retry logic is provided by the `backon` crate. This module
    //! retains the `should_retry_error` helper for error classification.

    /// Check if an error should be retried
    ///
    /// Checks the error for known transient I/O error kinds, then falls back to
    /// string matching for errors that don't expose `std::io::Error` directly.
    pub fn should_retry_error(err: &(dyn std::error::Error + 'static)) -> bool {
        // Check top-level error for std::io::Error with transient kinds
        if let Some(io_err) = err.downcast_ref::<std::io::Error>() {
            return is_transient_io_error(io_err);
        }

        // Fallback: check the display message for transient indicators.
        // This covers wrapped error types (e.g. hyper, tonic, anyhow) that
        // include the underlying I/O error message in their Display output.
        let err_msg = err.to_string().to_lowercase();
        err_msg.contains("timed out")
            || err_msg.contains("timeout")
            || err_msg.contains("connection reset")
            || err_msg.contains("connection refused")
            || err_msg.contains("connection aborted")
            || err_msg.contains("broken pipe")
    }

    /// Check if an I/O error is transient and worth retrying
    fn is_transient_io_error(err: &std::io::Error) -> bool {
        matches!(
            err.kind(),
            std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::UnexpectedEof
        )
    }
}

pub mod circuit_breaker {
    //! Circuit breaker pattern for external services
    //!
    //! Uses the `failsafe` crate for production-grade circuit breaker logic.
    //! Re-exports key types for convenience.

    use std::time::Duration;

    pub use failsafe::CircuitBreaker;

    /// Create a circuit breaker with sensible defaults for external service calls.
    ///
    /// Opens after `failure_threshold` consecutive failures.
    /// Uses exponential backoff from `min_backoff` to `max_backoff` in open state.
    #[must_use] 
    pub fn create(
        failure_threshold: u32,
        min_backoff: Duration,
        max_backoff: Duration,
    ) -> failsafe::StateMachine<
        failsafe::failure_policy::ConsecutiveFailures<failsafe::backoff::Exponential>,
        (),
    > {
        let backoff = failsafe::backoff::exponential(min_backoff, max_backoff);
        let policy = failsafe::failure_policy::consecutive_failures(failure_threshold, backoff);
        failsafe::Config::new().failure_policy(policy).build()
    }

    /// Create a circuit breaker with default settings (5 failures, 10-60s backoff)
    #[must_use] 
    pub fn create_default() -> failsafe::StateMachine<
        failsafe::failure_policy::ConsecutiveFailures<failsafe::backoff::Exponential>,
        (),
    > {
        create(5, Duration::from_secs(10), Duration::from_mins(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use timeout::TimeoutConfig;

    #[test]
    fn test_timeout_config() {
        let config = TimeoutConfig::new()
            .with_db_query_timeout(Duration::from_mins(1));

        assert_eq!(config.db_query.as_secs(), 60);
    }

    #[test]
    fn test_circuit_breaker_failsafe() {
        // failsafe requires backoff start >= 1 second
        let cb = circuit_breaker::create(3, Duration::from_secs(2), Duration::from_secs(10));

        // Initially closed (call permitted)
        assert!(cb.is_call_permitted());

        // Record failures
        for _ in 0..3 {
            cb.on_error();
        }

        // Should be open now (call NOT permitted)
        assert!(!cb.is_call_permitted());
    }

    #[test]
    fn test_circuit_breaker_recovery() {
        // Use minimum 2s backoff (failsafe requires >= 1s)
        let cb = circuit_breaker::create(2, Duration::from_secs(2), Duration::from_secs(5));

        // Open the breaker
        cb.on_error();
        cb.on_error();
        assert!(!cb.is_call_permitted());

        // Wait for backoff to elapse (2s + margin)
        std::thread::sleep(Duration::from_millis(2500));

        // Should allow a probe request (half-open)
        assert!(cb.is_call_permitted());

        // Record success to close
        cb.on_success();
        assert!(cb.is_call_permitted());
    }

    #[test]
    fn test_should_retry_error() {
        let timeout_err = std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout");
        assert!(retry::should_retry_error(&timeout_err));

        let not_found = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        assert!(!retry::should_retry_error(&not_found));
    }
}
