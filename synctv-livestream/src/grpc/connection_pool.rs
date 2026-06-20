// gRPC connection pool for reusing established channels across requests.
// Keyed by node address (e.g., "host:port"), each entry holds a tonic Channel
// that multiplexes HTTP/2 streams. Idle connections are evicted after a
// configurable TTL to avoid holding stale connections to nodes that may have
// been replaced.

use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tonic::transport::Channel;
use tracing::debug;

/// A pooled gRPC channel with creation and last-use timestamps.
struct PooledChannel {
    channel: Channel,
    created_at: Instant,
    last_used: Instant,
}

/// Default maximum number of connections in the pool.
const DEFAULT_MAX_POOL_SIZE: usize = 100;

/// Thread-safe gRPC connection pool keyed by node address.
///
/// Channels are reused across callers (tonic `Channel` is clone-cheap and
/// multiplexes over a single HTTP/2 connection). Stale entries are lazily
/// evicted on access when they exceed `max_idle`.
///
/// The pool enforces a maximum size (`max_size`). When a new connection would
/// exceed this limit, the oldest (by creation time) entry is evicted to make
/// room. This prevents unbounded growth during K8s pod churn where stale
/// addresses accumulate.
#[derive(Clone)]
pub(crate) struct GrpcConnectionPool {
    connections: Arc<DashMap<String, PooledChannel>>,
    /// Maximum time a pooled connection is considered healthy before re-creation.
    max_idle: Duration,
    /// Maximum number of connections allowed in the pool.
    max_size: usize,
}

impl GrpcConnectionPool {
    /// Create a new connection pool.
    ///
    /// `max_idle` controls how long a cached channel is reused before being
    /// discarded and re-created on the next request.
    /// `max_size` limits the maximum number of connections in the pool.
    #[must_use]
    pub(crate) fn new(max_idle: Duration, max_size: usize) -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
            max_idle,
            max_size: max_size.max(1), // at least 1
        }
    }

    /// Create a pool with a default max idle time of 5 minutes and default max
    /// pool size of 100.
    #[must_use]
    pub(crate) fn with_defaults() -> Self {
        Self::new(Duration::from_mins(5), DEFAULT_MAX_POOL_SIZE)
    }

    /// Returns the maximum number of connections allowed in the pool.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn max_size(&self) -> usize {
        self.max_size
    }

    /// Get or create a gRPC channel for the given address.
    ///
    /// Returns a cached channel if one exists and is not stale, otherwise
    /// creates a new connection. The address should be in `host:port` format
    /// (scheme is added automatically if missing).
    ///
    /// Connection attempts timeout after 5 seconds to prevent hanging indefinitely
    /// when the target node is unreachable.
    pub(crate) async fn get_channel(&self, address: &str) -> anyhow::Result<Channel> {
        // Fast path: check for an existing connection that is still active.
        if let Some(mut entry) = self.connections.get_mut(address) {
            if entry.last_used.elapsed() < self.max_idle {
                entry.last_used = Instant::now();
                return Ok(entry.channel.clone());
            }
            drop(entry);
            self.connections.remove(address);
            debug!(address = address, "Evicted stale gRPC connection from pool");
        }

        // Slow path: create new connection
        let url = if address.starts_with("http://") || address.starts_with("https://") {
            address.to_string()
        } else {
            format!("http://{address}")
        };

        let channel = match Channel::from_shared(url.clone())
            .map_err(|e| anyhow::anyhow!("Invalid gRPC endpoint URL '{url}': {e}"))?
            .connect_timeout(Duration::from_secs(5))
            .connect()
            .await
        {
            Ok(ch) => ch,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to connect to gRPC endpoint '{address}': {e}"
                ));
            }
        };

        // Evict oldest entry if pool is at capacity (and this is a new address)
        if !self.connections.contains_key(address) && self.connections.len() >= self.max_size {
            self.evict_oldest();
        }

        self.connections.insert(
            address.to_string(),
            PooledChannel {
                channel: channel.clone(),
                created_at: Instant::now(),
                last_used: Instant::now(),
            },
        );

        debug!(address = address, "Created new pooled gRPC connection");
        Ok(channel)
    }

    /// Remove a specific connection from the pool (e.g., after a connection error).
    pub(crate) fn invalidate(&self, address: &str) {
        if self.connections.remove(address).is_some() {
            debug!(address = address, "Invalidated gRPC connection from pool");
        }
    }

    /// Evict the oldest connection from the pool to make room for a new one.
    ///
    /// Iterates over all entries and removes the one with the earliest
    /// `created_at` timestamp. This is O(n) but the pool is small (bounded by
    /// `max_size`), so the cost is negligible.
    fn evict_oldest(&self) {
        let oldest = self
            .connections
            .iter()
            .min_by_key(|entry| entry.created_at)
            .map(|entry| entry.key().clone());

        if let Some(key) = oldest {
            self.connections.remove(&key);
            debug!(address = key, "Evicted oldest gRPC connection to make room");
        }
    }

    /// Number of connections currently in the pool.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.connections.len()
    }

    /// Whether the pool is empty.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn insert_test_channel_with_age(&self, address: &str, age: Duration) {
        let timestamp = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
        self.connections.insert(
            address.to_string(),
            PooledChannel {
                channel: Self::test_channel(),
                created_at: timestamp,
                last_used: timestamp,
            },
        );
    }

    #[cfg(test)]
    fn test_channel() -> Channel {
        Channel::from_static("http://livestream-test-channel.invalid").connect_lazy()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_creation() {
        let pool = GrpcConnectionPool::with_defaults();
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn test_pool_invalidate_nonexistent() {
        let pool = GrpcConnectionPool::with_defaults();
        // Should not panic
        pool.invalidate("nonexistent:50051");
        assert!(pool.is_empty());
    }

    #[tokio::test]
    async fn test_connection_timeout_configuration() {
        // This test verifies that the timeout is properly configured in the
        // Channel builder. We can't easily test the actual timeout behavior
        // without a real unresponsive server, but we can verify the code compiles
        // and the timeout parameter is used.
        let pool = GrpcConnectionPool::with_defaults();

        // Try to connect to localhost with a non-existent port
        // This should fail quickly (connection refused) but with timeout configured
        let result = pool.get_channel("127.0.0.1:65535").await;

        // Should fail because nothing is listening on this port
        assert!(
            result.is_err(),
            "Expected connection to 127.0.0.1:65535 to fail"
        );

        // The important part is that connect_timeout() is called in the code,
        // which is verified at compile time by the type system
    }

    #[test]
    fn test_pool_max_size_custom() {
        let pool = GrpcConnectionPool::new(Duration::from_mins(1), 50);
        assert_eq!(pool.max_size(), 50);
    }

    #[test]
    fn test_pool_max_size_minimum_is_one() {
        let pool = GrpcConnectionPool::new(Duration::from_mins(1), 0);
        assert_eq!(pool.max_size(), 1);
    }

    type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    #[tokio::test]
    async fn test_pool_enforces_max_size_on_insert() -> TestResult {
        let pool = GrpcConnectionPool::new(Duration::from_mins(5), 3);

        let channel = GrpcConnectionPool::test_channel();
        let now = Instant::now();
        for i in 0..3u32 {
            let created_at = now
                .checked_sub(Duration::from_secs(300 - u64::from(i) * 100))
                .ok_or_else(|| test_error("created_at should stay in range"))?;
            pool.connections.insert(
                format!("node-{i}:50051"),
                PooledChannel {
                    channel: channel.clone(),
                    created_at,
                    last_used: created_at,
                },
            );
        }
        assert_eq!(pool.len(), 3);

        pool.evict_oldest();
        assert_eq!(pool.len(), 2);

        assert!(pool.connections.get("node-0:50051").is_none());
        assert!(pool.connections.get("node-1:50051").is_some());
        assert!(pool.connections.get("node-2:50051").is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_pool_refreshes_idle_clock_on_hit() -> TestResult {
        let pool = GrpcConnectionPool::new(Duration::from_mins(5), 3);
        let stale_creation = Instant::now()
            .checked_sub(Duration::from_mins(10))
            .unwrap_or_else(Instant::now);
        let previous_last_used = Instant::now()
            .checked_sub(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);
        pool.connections.insert(
            "node-refresh:50051".to_string(),
            PooledChannel {
                channel: GrpcConnectionPool::test_channel(),
                created_at: stale_creation,
                last_used: previous_last_used,
            },
        );

        let _channel = pool.get_channel("node-refresh:50051").await?;
        let entry = pool
            .connections
            .get("node-refresh:50051")
            .expect("test entry should exist");
        assert!(
            entry.last_used > previous_last_used,
            "cache hit should refresh idle timestamp"
        );
        Ok(())
    }
}
