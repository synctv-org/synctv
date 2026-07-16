//! Error types for cluster module
//!
//! All public cluster APIs return [`Result<T>`] using the [`Error`] enum below.
//! Internal helpers may use `anyhow::Result` for ergonomic `.context()` chains;
//! the `From<anyhow::Error>` impl bridges the two.

use thiserror::Error;

/// Returns whether a Redis error indicates an in-progress Sentinel failover.
pub(crate) fn is_sentinel_failover_error(error_message: &str) -> bool {
    error_message.contains("READONLY") || error_message.contains("LOADING")
}

/// Cluster error types
#[derive(Debug, Error)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("RPC error: {0}")]
    Rpc(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Redis error: {0}")]
    Redis(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    /// Catch-all for errors originating from internal `anyhow::Error` chains
    /// (e.g. redis_pubsub, WAL). Preserves the full error context.
    #[error("{0:#}")]
    Internal(#[from] anyhow::Error),
}

/// Result type for cluster operations
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::is_sentinel_failover_error;

    #[test]
    fn identifies_sentinel_failover_errors() {
        assert!(is_sentinel_failover_error(
            "READONLY You can't write against a read only replica"
        ));
        assert!(is_sentinel_failover_error(
            "LOADING Redis is loading the dataset in memory"
        ));
        assert!(!is_sentinel_failover_error("connection refused"));
    }
}
