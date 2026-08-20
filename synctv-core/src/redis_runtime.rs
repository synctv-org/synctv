use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RedisDeploymentMode {
    #[default]
    Standalone,
    Sentinel,
}

#[async_trait]
pub trait RedisConnectionRuntime: Send + Sync {
    async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager>;

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
        .and_then(|result| {
            result.map_err(|error| {
                crate::Error::Internal(format!(
                    "Redis connection error during {operation}: {error}"
                ))
            })
        })
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
    async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
        Ok(self.conn.clone())
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
    async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
        Ok(self.conn.read().await.clone())
    }

    fn operation_timeout(&self) -> Duration {
        self.operation_timeout
    }
}

#[derive(Clone)]
pub struct OnDemandRedisRuntime {
    client: redis::Client,
    manager_options: redis::aio::ConnectionManagerConfig,
    operation_timeout: Duration,
}

impl OnDemandRedisRuntime {
    #[must_use]
    pub fn new(client: redis::Client) -> Self {
        Self::new_with_connection_options_and_operation_timeout(
            client,
            redis::aio::ConnectionManagerConfig::new(),
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
        )
    }

    #[must_use]
    pub fn new_with_connection_options_and_operation_timeout(
        client: redis::Client,
        manager_options: redis::aio::ConnectionManagerConfig,
        operation_timeout: Duration,
    ) -> Self {
        Self {
            client,
            manager_options,
            operation_timeout,
        }
    }
}

#[async_trait]
impl RedisConnectionRuntime for OnDemandRedisRuntime {
    async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
        redis::aio::ConnectionManager::new_with_config(
            self.client.clone(),
            self.manager_options.clone(),
        )
        .await
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
}

#[async_trait]
impl RedisConnectionRuntime for ManagedRedisRuntime {
    async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
        Ok(self.conn.read().await.clone())
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
pub fn coordination_runtime_from_client_with_connection_options_and_operation_timeout(
    client: redis::Client,
    manager_options: redis::aio::ConnectionManagerConfig,
    operation_timeout: Duration,
) -> Arc<dyn RedisCoordinationRuntime> {
    Arc::new(
        OnDemandRedisRuntime::new_with_connection_options_and_operation_timeout(
            client,
            manager_options,
            operation_timeout,
        ),
    )
}

#[must_use]
pub fn redis_connection_manager_options(
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
        redis_connection_manager_options, redis_runtime_snapshot, shared_runtime,
        shared_runtime_from_conn, ManagedRedisRuntime, RedisConnectionRuntime,
    };
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::RwLock;

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn err<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> E {
        match result {
            Ok(_) => std::panic::panic_any(context.to_string()),
            Err(error) => error,
        }
    }

    #[test]
    fn test_shared_runtime_from_conn_preserves_none() {
        assert!(shared_runtime_from_conn(None).is_none());
    }

    #[test]
    fn test_redis_connection_manager_options_sets_timeouts_and_pipeline_buffer() {
        let manager_options =
            redis_connection_manager_options(Duration::from_secs(3), Duration::from_secs(4), 256);

        assert_eq!(
            manager_options.connection_timeout(),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            manager_options.response_timeout(),
            Some(Duration::from_secs(4))
        );
        assert!(
            format!("{manager_options:?}").contains("pipeline_buffer_size: Some(256)"),
            "pipeline buffer size should be applied to ConnectionManagerConfig"
        );
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_managed_runtime_preserves_configured_operation_timeout() {
        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let conn = ok(
            redis::aio::ConnectionManager::new(client.clone()).await,
            "redis connection manager",
        );
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
            async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
                tokio::time::sleep(Duration::from_mins(1)).await;
                std::panic::panic_any("snapshot timeout should cancel this future")
            }

            fn operation_timeout(&self) -> Duration {
                Duration::from_millis(1)
            }
        }

        let error = err(
            redis_runtime_snapshot(&HangingRedisRuntime, "test snapshot").await,
            "hanging snapshot should time out",
        );

        assert!(
            matches!(error, crate::Error::Timeout(ref msg) if msg == "Redis timeout: test snapshot"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_shared_runtime_reads_hot_swapped_connection() {
        let (_redis, client) = synctv_core_testing::start_redis_with_client().await;
        let first = ok(
            redis::aio::ConnectionManager::new(client.clone()).await,
            "first connection manager should initialize",
        );
        let shared_conn = Arc::new(RwLock::new(first));
        let runtime = shared_runtime(shared_conn.clone());

        let replacement = ok(
            redis::aio::ConnectionManager::new(client).await,
            "replacement connection manager should initialize",
        );
        *shared_conn.write().await = replacement.clone();

        let snapshot = ok(
            runtime.snapshot().await,
            "snapshot should return a Redis connection",
        );

        let _: redis::aio::ConnectionManager = snapshot;
        let _: redis::aio::ConnectionManager = replacement;
    }
}
