//! Error types for cluster module
//!
//! All public cluster APIs return [`Result<T>`] using the [`Error`] enum below.
//! Internal helpers may use `anyhow::Result` for ergonomic `.context()` chains;
//! the `From<anyhow::Error>` impl bridges the two.

use thiserror::Error;

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
