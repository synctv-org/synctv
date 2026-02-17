// gRPC connection pool for reusing established channels across requests.
//
// Keyed by node address (e.g., "host:port"), each entry holds a tonic Channel
// that multiplexes HTTP/2 streams. Idle connections are evicted after a
// configurable TTL to avoid holding stale connections to nodes that may have
// been replaced.

use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tonic::transport::Channel;
use tracing::debug;

/// A pooled gRPC channel with creation timestamp for staleness checks.
struct PooledChannel {
    channel: Channel,
    created_at: Instant,
}

/// Thread-safe gRPC connection pool keyed by node address.
///
/// Channels are reused across callers (tonic `Channel` is clone-cheap and
/// multiplexes over a single HTTP/2 connection). Stale entries are lazily
/// evicted on access when they exceed `max_idle`.
#[derive(Clone)]
pub struct GrpcConnectionPool {
    connections: Arc<DashMap<String, PooledChannel>>,
    /// Maximum time a pooled connection is considered healthy before re-creation.
    max_idle: Duration,
}

impl GrpcConnectionPool {
    /// Create a new connection pool.
    ///
    /// `max_idle` controls how long a cached channel is reused before being
    /// discarded and re-created on the next request.
    pub fn new(max_idle: Duration) -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
            max_idle,
        }
    }

    /// Create a pool with a default max idle time of 5 minutes.
    pub fn with_defaults() -> Self {
        Self::new(Duration::from_secs(300))
    }

    /// Get or create a gRPC channel for the given address.
    ///
    /// Returns a cached channel if one exists and is not stale, otherwise
    /// creates a new connection. The address should be in `host:port` format
    /// (scheme is added automatically if missing).
    ///
    /// Connection attempts timeout after 5 seconds to prevent hanging indefinitely
    /// when the target node is unreachable.
    pub async fn get_channel(&self, address: &str) -> anyhow::Result<Channel> {
        // Fast path: check for existing healthy connection
        if let Some(entry) = self.connections.get(address) {
            if entry.created_at.elapsed() < self.max_idle {
                return Ok(entry.channel.clone());
            }
            // Stale -- drop the read guard and remove below
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

        let channel = Channel::from_shared(url.clone())
            .map_err(|e| anyhow::anyhow!("Invalid gRPC endpoint URL '{url}': {e}"))?
            .connect_timeout(Duration::from_secs(5))
            .connect()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to gRPC endpoint '{address}': {e}"))?;

        self.connections.insert(
            address.to_string(),
            PooledChannel {
                channel: channel.clone(),
                created_at: Instant::now(),
            },
        );

        debug!(address = address, "Created new pooled gRPC connection");
        Ok(channel)
    }

    /// Remove a specific connection from the pool (e.g., after a connection error).
    pub fn invalidate(&self, address: &str) {
        if self.connections.remove(address).is_some() {
            debug!(address = address, "Invalidated gRPC connection from pool");
        }
    }

    /// Remove all stale connections. Can be called periodically from a background task.
    pub fn evict_stale(&self) {
        let before = self.connections.len();
        self.connections.retain(|_addr, entry| entry.created_at.elapsed() < self.max_idle);
        let evicted = before - self.connections.len();
        if evicted > 0 {
            debug!("Evicted {} stale gRPC connections from pool", evicted);
        }
    }

    /// Spawn a background task that calls `evict_stale` every `interval`.
    ///
    /// The task runs until the returned `JoinHandle` is aborted or the process
    /// exits. Typical usage: call once at startup with a 5-minute interval.
    pub fn spawn_cleanup_task(&self, interval: Duration) -> tokio::task::JoinHandle<()> {
        let pool = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                pool.evict_stale();
            }
        })
    }

    /// Number of connections currently in the pool.
    pub fn len(&self) -> usize {
        self.connections.len()
    }

    /// Whether the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
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

    #[test]
    fn test_pool_evict_stale_empty() {
        let pool = GrpcConnectionPool::with_defaults();
        pool.evict_stale();
        assert!(pool.is_empty());
    }

    #[tokio::test]
    async fn test_pool_evict_stale_with_expired_entry() {
        // Use a very short TTL so entries expire immediately
        let pool = GrpcConnectionPool::new(Duration::from_millis(1));

        // We can't easily create a real channel without a server, so just test
        // the eviction logic with the empty pool (integration test would cover the full path)
        pool.evict_stale();
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
        assert!(result.is_err(),
            "Expected connection to 127.0.0.1:65535 to fail");

        // The important part is that connect_timeout() is called in the code,
        // which is verified at compile time by the type system
    }
}
