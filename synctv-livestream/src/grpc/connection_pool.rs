use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use moka::future::Cache;
use tonic::transport::Channel;
use tracing::debug;

type ConnectFuture = Pin<Box<dyn Future<Output = anyhow::Result<Channel>> + Send>>;
type ChannelConnector = dyn Fn(String) -> ConnectFuture + Send + Sync;

const DEFAULT_MAX_POOL_SIZE: usize = 100;

#[derive(Clone)]
pub(crate) struct GrpcConnectionPool {
    connections: Cache<String, Channel>,
    connector: Arc<ChannelConnector>,
    #[cfg(test)]
    max_size: usize,
}

impl GrpcConnectionPool {
    #[must_use]
    pub(crate) fn new(max_idle: Duration, max_size: usize) -> Self {
        Self::with_connector(max_idle, max_size, |address| {
            Box::pin(Self::connect_channel(address))
        })
    }

    fn with_connector<F>(max_idle: Duration, max_size: usize, connector: F) -> Self
    where
        F: Fn(String) -> ConnectFuture + Send + Sync + 'static,
    {
        let max_size = max_size.max(1);
        Self {
            connections: Cache::builder()
                .max_capacity(max_size as u64)
                .time_to_idle(max_idle)
                .build(),
            connector: Arc::new(connector),
            #[cfg(test)]
            max_size,
        }
    }

    #[must_use]
    pub(crate) fn with_defaults() -> Self {
        Self::new(Duration::from_mins(5), DEFAULT_MAX_POOL_SIZE)
    }

    async fn connect_channel(address: String) -> anyhow::Result<Channel> {
        let url = if address.starts_with("http://") || address.starts_with("https://") {
            address.clone()
        } else {
            format!("http://{address}")
        };
        let channel = Channel::from_shared(url.clone())
            .map_err(|error| anyhow::anyhow!("Invalid gRPC endpoint URL '{url}': {error}"))?
            .connect_timeout(Duration::from_secs(5))
            .connect()
            .await
            .map_err(|error| {
                anyhow::anyhow!("Failed to connect to gRPC endpoint '{address}': {error}")
            })?;
        debug!(address, "Created new pooled gRPC connection");
        Ok(channel)
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) const fn max_size(&self) -> usize {
        self.max_size
    }

    pub(crate) async fn get_channel(&self, address: &str) -> anyhow::Result<Channel> {
        let connector = Arc::clone(&self.connector);
        let address = address.to_string();
        self.connections
            .try_get_with(address.clone(), async move { connector(address).await })
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    pub(crate) async fn invalidate(&self, address: &str) {
        self.connections.invalidate(address).await;
        debug!(address, "Invalidated gRPC connection from pool");
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        usize::try_from(self.connections.entry_count()).unwrap_or(usize::MAX)
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.connections.entry_count() == 0
    }

    #[cfg(test)]
    fn test_channel() -> Channel {
        Channel::from_static("http://livestream-test-channel.invalid").connect_lazy()
    }

    #[cfg(test)]
    pub(crate) async fn insert_test_channel(&self, address: &str) {
        self.connections
            .insert(address.to_string(), Self::test_channel())
            .await;
        self.connections.run_pending_tasks().await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::future::join_all;

    use super::*;

    #[test]
    fn test_pool_creation() {
        let pool = GrpcConnectionPool::with_defaults();
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
    }

    #[tokio::test]
    async fn test_pool_invalidate_nonexistent() {
        let pool = GrpcConnectionPool::with_defaults();
        pool.invalidate("nonexistent:50051").await;
        assert!(pool.is_empty());
    }

    #[tokio::test]
    async fn test_connection_failure_is_retryable() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_connector = Arc::clone(&attempts);
        let pool = GrpcConnectionPool::with_connector(Duration::from_mins(5), 3, move |_address| {
            let attempts = Arc::clone(&attempts_for_connector);
            Box::pin(async move {
                attempts.fetch_add(1, Ordering::Relaxed);
                Err(anyhow::anyhow!("connect failed"))
            })
        });

        assert!(pool.get_channel("node:50051").await.is_err());
        assert!(pool.get_channel("node:50051").await.is_err());
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
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

    #[tokio::test]
    async fn test_concurrent_miss_connects_once() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_connector = Arc::clone(&attempts);
        let pool = GrpcConnectionPool::with_connector(Duration::from_mins(5), 3, move |_address| {
            let attempts = Arc::clone(&attempts_for_connector);
            Box::pin(async move {
                attempts.fetch_add(1, Ordering::Relaxed);
                tokio::task::yield_now().await;
                Ok(GrpcConnectionPool::test_channel())
            })
        });

        let results = join_all((0..32).map(|_| pool.get_channel("node:50051"))).await;
        assert!(results.iter().all(Result::is_ok));
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_invalidate_forces_reconnect() -> anyhow::Result<()> {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_connector = Arc::clone(&attempts);
        let pool = GrpcConnectionPool::with_connector(Duration::from_mins(5), 3, move |_address| {
            let attempts = Arc::clone(&attempts_for_connector);
            Box::pin(async move {
                attempts.fetch_add(1, Ordering::Relaxed);
                Ok(GrpcConnectionPool::test_channel())
            })
        });

        pool.get_channel("node:50051").await?;
        pool.invalidate("node:50051").await;
        pool.get_channel("node:50051").await?;
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
        Ok(())
    }
}
