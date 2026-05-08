use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

#[async_trait]
pub trait RedisConnectionRuntime: Send + Sync {
    async fn snapshot(&self) -> redis::aio::ConnectionManager;
}

#[async_trait]
pub trait RedisCoordinationRuntime: RedisConnectionRuntime {
    async fn multiplexed_connection(&self)
        -> redis::RedisResult<redis::aio::MultiplexedConnection>;

    async fn async_pubsub(&self) -> redis::RedisResult<redis::aio::PubSub>;
}

#[derive(Clone)]
pub struct DirectRedisConnectionRuntime {
    conn: redis::aio::ConnectionManager,
}

impl DirectRedisConnectionRuntime {
    #[must_use]
    pub const fn new(conn: redis::aio::ConnectionManager) -> Self {
        Self { conn }
    }
}

#[async_trait]
impl RedisConnectionRuntime for DirectRedisConnectionRuntime {
    async fn snapshot(&self) -> redis::aio::ConnectionManager {
        self.conn.clone()
    }
}

#[derive(Clone)]
pub struct SharedRedisConnectionRuntime {
    conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
}

impl SharedRedisConnectionRuntime {
    #[must_use]
    pub const fn new(conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>) -> Self {
        Self { conn }
    }
}

#[async_trait]
impl RedisConnectionRuntime for SharedRedisConnectionRuntime {
    async fn snapshot(&self) -> redis::aio::ConnectionManager {
        self.conn.read().await.clone()
    }
}

#[derive(Clone)]
pub struct OnDemandRedisRuntime {
    client: redis::Client,
    manager_config: redis::aio::ConnectionManagerConfig,
}

impl OnDemandRedisRuntime {
    #[must_use]
    pub fn new(client: redis::Client) -> Self {
        Self::new_with_config(client, redis::aio::ConnectionManagerConfig::new())
    }

    #[must_use]
    pub fn new_with_config(
        client: redis::Client,
        manager_config: redis::aio::ConnectionManagerConfig,
    ) -> Self {
        Self {
            client,
            manager_config,
        }
    }
}

#[async_trait]
impl RedisConnectionRuntime for OnDemandRedisRuntime {
    async fn snapshot(&self) -> redis::aio::ConnectionManager {
        redis::aio::ConnectionManager::new_with_config(
            self.client.clone(),
            self.manager_config.clone(),
        )
        .await
        .expect("on-demand redis runtime failed to create connection manager")
    }
}

#[async_trait]
impl RedisCoordinationRuntime for OnDemandRedisRuntime {
    async fn multiplexed_connection(
        &self,
    ) -> redis::RedisResult<redis::aio::MultiplexedConnection> {
        self.client.get_multiplexed_async_connection().await
    }

    async fn async_pubsub(&self) -> redis::RedisResult<redis::aio::PubSub> {
        self.client.get_async_pubsub().await
    }
}

#[derive(Clone)]
pub struct ManagedRedisRuntime {
    client: redis::Client,
    conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
}

impl ManagedRedisRuntime {
    #[must_use]
    pub const fn new(
        client: redis::Client,
        conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
    ) -> Self {
        Self { client, conn }
    }

    pub async fn from_client(client: redis::Client) -> redis::RedisResult<Self> {
        Self::from_client_with_config(client, redis::aio::ConnectionManagerConfig::new()).await
    }

    pub async fn from_client_with_config(
        client: redis::Client,
        manager_config: redis::aio::ConnectionManagerConfig,
    ) -> redis::RedisResult<Self> {
        let conn =
            redis::aio::ConnectionManager::new_with_config(client.clone(), manager_config).await?;
        Ok(Self::new(client, Arc::new(tokio::sync::RwLock::new(conn))))
    }

    #[must_use]
    pub fn shared_conn(&self) -> Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>> {
        self.conn.clone()
    }
}

#[async_trait]
impl RedisConnectionRuntime for ManagedRedisRuntime {
    async fn snapshot(&self) -> redis::aio::ConnectionManager {
        self.conn.read().await.clone()
    }
}

#[async_trait]
impl RedisCoordinationRuntime for ManagedRedisRuntime {
    async fn multiplexed_connection(
        &self,
    ) -> redis::RedisResult<redis::aio::MultiplexedConnection> {
        self.client.get_multiplexed_async_connection().await
    }

    async fn async_pubsub(&self) -> redis::RedisResult<redis::aio::PubSub> {
        self.client.get_async_pubsub().await
    }
}

#[must_use]
pub fn direct_runtime(conn: redis::aio::ConnectionManager) -> Arc<dyn RedisConnectionRuntime> {
    Arc::new(DirectRedisConnectionRuntime::new(conn))
}

#[must_use]
pub fn direct_runtime_from_conn(
    conn: Option<redis::aio::ConnectionManager>,
) -> Option<Arc<dyn RedisConnectionRuntime>> {
    conn.map(direct_runtime)
}

#[must_use]
pub fn shared_runtime(
    conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
) -> Arc<dyn RedisConnectionRuntime> {
    Arc::new(SharedRedisConnectionRuntime::new(conn))
}

#[must_use]
pub fn shared_runtime_from_conn(
    conn: Option<Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>,
) -> Option<Arc<dyn RedisConnectionRuntime>> {
    conn.map(shared_runtime)
}

#[must_use]
pub fn coordination_runtime_from_client(
    client: redis::Client,
) -> Arc<dyn RedisCoordinationRuntime> {
    Arc::new(OnDemandRedisRuntime::new(client))
}

#[must_use]
pub fn coordination_runtime_from_client_with_config(
    client: redis::Client,
    manager_config: redis::aio::ConnectionManagerConfig,
) -> Arc<dyn RedisCoordinationRuntime> {
    Arc::new(OnDemandRedisRuntime::new_with_config(
        client,
        manager_config,
    ))
}

#[must_use]
pub fn redis_connection_manager_config(
    connect_timeout: Duration,
    response_timeout: Duration,
    pipeline_buffer_size: usize,
) -> redis::aio::ConnectionManagerConfig {
    redis::aio::ConnectionManagerConfig::new()
        .set_connection_timeout(Some(connect_timeout))
        .set_response_timeout(Some(response_timeout))
        .set_pipeline_buffer_size(pipeline_buffer_size)
}

#[cfg(test)]
mod tests {
    use super::{redis_connection_manager_config, shared_runtime, shared_runtime_from_conn};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::RwLock;

    #[test]
    fn test_shared_runtime_from_conn_preserves_none() {
        assert!(shared_runtime_from_conn(None).is_none());
    }

    #[test]
    fn test_redis_connection_manager_config_sets_timeouts_and_pipeline_buffer() {
        let manager_config =
            redis_connection_manager_config(Duration::from_secs(3), Duration::from_secs(4), 256);

        assert_eq!(
            manager_config.connection_timeout(),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            manager_config.response_timeout(),
            Some(Duration::from_secs(4))
        );
        assert!(
            format!("{manager_config:?}").contains("pipeline_buffer_size: Some(256)"),
            "pipeline buffer size should be applied to ConnectionManagerConfig"
        );
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_shared_runtime_reads_hot_swapped_connection() {
        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let first = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("first connection manager should initialize");
        let shared_conn = Arc::new(RwLock::new(first));
        let runtime = shared_runtime(shared_conn.clone());

        let replacement = redis::aio::ConnectionManager::new(client)
            .await
            .expect("replacement connection manager should initialize");
        *shared_conn.write().await = replacement.clone();

        let snapshot = runtime.snapshot().await;

        let _: redis::aio::ConnectionManager = snapshot;
        let _: redis::aio::ConnectionManager = replacement;
    }
}
