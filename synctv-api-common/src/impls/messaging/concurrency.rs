use std::sync::Arc;
use tokio::sync::Semaphore;

/// Default maximum concurrent message processing operations across all connections.
///
/// This provides backpressure when the system is under heavy load.
/// When exceeded, new messages receive a `ResourceExhausted` error.
const DEFAULT_MAX_CONCURRENT_MESSAGE_PROCESSING: usize = 1000;

/// Configuration for message processing concurrency.
#[derive(Clone, Debug)]
pub struct MessageConcurrencyConfig {
    /// Semaphore for limiting concurrent message processing.
    /// This is shared across all connections for the same `AppState`.
    semaphore: Arc<Semaphore>,
}

impl MessageConcurrencyConfig {
    /// Create a new concurrency config with the specified limit.
    #[must_use]
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    /// Get the semaphore for acquiring permits.
    ///
    /// Returns a cloned `Arc<Semaphore>` that can be used to acquire permits
    /// for message processing.
    #[must_use]
    pub fn semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.semaphore)
    }

    /// Get the number of available permits.
    ///
    /// This is useful for monitoring and health checks.
    #[cfg(test)]
    #[must_use]
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }
}

impl Default for MessageConcurrencyConfig {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_CONCURRENT_MESSAGE_PROCESSING)
    }
}
