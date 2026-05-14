use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

#[async_trait]
pub trait RedisConnectionRuntime: Send + Sync {
    async fn snapshot(&self) -> redis::aio::ConnectionManager;

    fn operation_timeout(&self) -> Duration {
        crate::resilience::timeout::REDIS_OPERATION_TIMEOUT
    }
}

pub async fn redis_runtime_snapshot(
    runtime: &dyn RedisConnectionRuntime,
    operation: impl Into<String>,
) -> crate::Result<redis::aio::ConnectionManager> {
    let operation = operation.into();
    tokio::time::timeout(runtime.operation_timeout(), runtime.snapshot())
        .await
        .map_err(|_| crate::Error::Timeout(format!("Redis timeout: {operation}")))
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
    operation_timeout: Duration,
}

impl DirectRedisConnectionRuntime {
    #[must_use]
    pub const fn new(conn: redis::aio::ConnectionManager) -> Self {
        Self::new_with_operation_timeout(conn, crate::resilience::timeout::REDIS_OPERATION_TIMEOUT)
    }

    #[must_use]
    pub const fn new_with_operation_timeout(
        conn: redis::aio::ConnectionManager,
        operation_timeout: Duration,
    ) -> Self {
        Self {
            conn,
            operation_timeout,
        }
    }
}

#[async_trait]
impl RedisConnectionRuntime for DirectRedisConnectionRuntime {
    async fn snapshot(&self) -> redis::aio::ConnectionManager {
        self.conn.clone()
    }

    fn operation_timeout(&self) -> Duration {
        self.operation_timeout
    }
}

#[derive(Clone)]
pub struct SharedRedisConnectionRuntime {
    conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
    operation_timeout: Duration,
}

impl SharedRedisConnectionRuntime {
    #[must_use]
    pub const fn new(conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>) -> Self {
        Self::new_with_operation_timeout(conn, crate::resilience::timeout::REDIS_OPERATION_TIMEOUT)
    }

    #[must_use]
    pub const fn new_with_operation_timeout(
        conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
        operation_timeout: Duration,
    ) -> Self {
        Self {
            conn,
            operation_timeout,
        }
    }
}

#[async_trait]
impl RedisConnectionRuntime for SharedRedisConnectionRuntime {
    async fn snapshot(&self) -> redis::aio::ConnectionManager {
        self.conn.read().await.clone()
    }

    fn operation_timeout(&self) -> Duration {
        self.operation_timeout
    }
}

#[derive(Clone)]
pub struct OnDemandRedisRuntime {
    client: redis::Client,
    manager_config: redis::aio::ConnectionManagerConfig,
    operation_timeout: Duration,
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
        Self::new_with_config_and_operation_timeout(
            client,
            manager_config,
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
        )
    }

    #[must_use]
    pub fn new_with_config_and_operation_timeout(
        client: redis::Client,
        manager_config: redis::aio::ConnectionManagerConfig,
        operation_timeout: Duration,
    ) -> Self {
        Self {
            client,
            manager_config,
            operation_timeout,
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

    fn operation_timeout(&self) -> Duration {
        self.operation_timeout
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
    operation_timeout: Duration,
}

impl ManagedRedisRuntime {
    #[must_use]
    pub const fn new(
        client: redis::Client,
        conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
    ) -> Self {
        Self::new_with_operation_timeout(
            client,
            conn,
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
        )
    }

    #[must_use]
    pub const fn new_with_operation_timeout(
        client: redis::Client,
        conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
        operation_timeout: Duration,
    ) -> Self {
        Self {
            client,
            conn,
            operation_timeout,
        }
    }

    pub async fn from_client(client: redis::Client) -> redis::RedisResult<Self> {
        Self::from_client_with_config(client, redis::aio::ConnectionManagerConfig::new()).await
    }

    pub async fn from_client_with_config(
        client: redis::Client,
        manager_config: redis::aio::ConnectionManagerConfig,
    ) -> redis::RedisResult<Self> {
        Self::from_client_with_config_and_operation_timeout(
            client,
            manager_config,
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
        )
        .await
    }

    pub async fn from_client_with_config_and_operation_timeout(
        client: redis::Client,
        manager_config: redis::aio::ConnectionManagerConfig,
        operation_timeout: Duration,
    ) -> redis::RedisResult<Self> {
        let conn =
            redis::aio::ConnectionManager::new_with_config(client.clone(), manager_config).await?;
        Ok(Self::new_with_operation_timeout(
            client,
            Arc::new(tokio::sync::RwLock::new(conn)),
            operation_timeout,
        ))
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

    fn operation_timeout(&self) -> Duration {
        self.operation_timeout
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
pub fn direct_runtime_with_operation_timeout(
    conn: redis::aio::ConnectionManager,
    operation_timeout: Duration,
) -> Arc<dyn RedisConnectionRuntime> {
    Arc::new(DirectRedisConnectionRuntime::new_with_operation_timeout(
        conn,
        operation_timeout,
    ))
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
pub fn shared_runtime_with_operation_timeout(
    conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
    operation_timeout: Duration,
) -> Arc<dyn RedisConnectionRuntime> {
    Arc::new(SharedRedisConnectionRuntime::new_with_operation_timeout(
        conn,
        operation_timeout,
    ))
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
pub fn coordination_runtime_from_client_with_config_and_operation_timeout(
    client: redis::Client,
    manager_config: redis::aio::ConnectionManagerConfig,
    operation_timeout: Duration,
) -> Arc<dyn RedisCoordinationRuntime> {
    Arc::new(OnDemandRedisRuntime::new_with_config_and_operation_timeout(
        client,
        manager_config,
        operation_timeout,
    ))
}

#[must_use]
pub fn redis_operation_timeout_from_config(config: &crate::Config) -> Duration {
    Duration::from_secs(config.redis.response_timeout_seconds)
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
    use super::{
        redis_connection_manager_config, redis_operation_timeout_from_config,
        redis_runtime_snapshot, shared_runtime, shared_runtime_from_conn, ManagedRedisRuntime,
        RedisConnectionRuntime,
    };
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

    #[test]
    fn test_redis_operation_timeout_comes_from_response_timeout_config() {
        let mut config = crate::Config::default();
        config.redis.response_timeout_seconds = 13;

        assert_eq!(
            redis_operation_timeout_from_config(&config),
            Duration::from_secs(13)
        );
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_managed_runtime_preserves_configured_operation_timeout() {
        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .expect("redis connection manager");
        let runtime = ManagedRedisRuntime::new_with_operation_timeout(
            client,
            Arc::new(RwLock::new(conn)),
            Duration::from_secs(17),
        );

        assert_eq!(runtime.operation_timeout(), Duration::from_secs(17));
    }

    #[tokio::test]
    async fn test_redis_runtime_snapshot_times_out() {
        struct HangingRedisRuntime;

        #[async_trait::async_trait]
        impl RedisConnectionRuntime for HangingRedisRuntime {
            async fn snapshot(&self) -> redis::aio::ConnectionManager {
                tokio::time::sleep(Duration::from_mins(1)).await;
                panic!("snapshot timeout should cancel this future")
            }

            fn operation_timeout(&self) -> Duration {
                Duration::from_millis(1)
            }
        }

        let error = redis_runtime_snapshot(&HangingRedisRuntime, "test snapshot")
            .await
            .expect_err("hanging snapshot should time out");

        assert!(
            matches!(error, crate::Error::Timeout(ref msg) if msg == "Redis timeout: test snapshot"),
            "unexpected error: {error}"
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
