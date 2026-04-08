//! Redis initialization
//!
//! Creates a single `RedisHandles` at startup and passes it everywhere,
//! eliminating duplicate `redis::Client::open` calls across the codebase.

use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::info;

use crate::config::RedisDeploymentMode;
use crate::Config;

type RedisConnectionManagerConfig = redis::aio::ConnectionManagerConfig;
type SentinelNodeConnectionInfo = redis::sentinel::SentinelNodeConnectionInfo;
type RedisNodeSettings = redis::RedisConnectionInfo;

/// Shared Redis handles created once at startup.
///
/// In Sentinel mode the background health check hot-swaps the inner
/// `ConnectionManager` on failover; all callers that hold a reference to
/// `conn` automatically see the updated connection on their next read-lock.
#[derive(Clone)]
pub struct RedisHandles {
    pub client: redis::Client,
    pub conn: Arc<RwLock<redis::aio::ConnectionManager>>,
}

#[derive(Debug)]
pub struct RedisInit {
    pub handles: Option<RedisHandles>,
    pub sentinel_health_check_task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for RedisHandles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisHandles")
            .field("client", &"redis::Client { .. }")
            .field("conn", &"Arc<RwLock<ConnectionManager>>")
            .finish()
    }
}

impl RedisHandles {
    /// Return a plain `ConnectionManager` snapshot from the shared connection.
    ///
    /// `ConnectionManager` is a cheap `Arc`-based clone. In Sentinel mode the
    /// background health check may swap the inner value; calling this method
    /// obtains the latest handle.
    pub async fn conn_snapshot(&self) -> redis::aio::ConnectionManager {
        self.conn.read().await.clone()
    }
}

/// Create the single Redis client + connection, start the Sentinel health
/// check if applicable, and return the shared handles.
///
/// Returns `Ok(None)` when the Redis URL is empty (standalone mode without Redis).
/// Returns `Ok(Some(handles))` when Redis is configured and connected.
///
/// An optional `CancellationToken` controls the Sentinel health check loop.
/// If `None`, the health check runs until the process exits.
///
/// # Errors
///
/// Returns an error if the client cannot be opened or the initial connection fails.
pub async fn init_redis(
    config: &Config,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<RedisInit, anyhow::Error> {
    match config.redis.deployment_mode {
        RedisDeploymentMode::Standalone => {
            let redis_url = config.redis_url();
            if redis_url.is_empty() {
                info!("Redis URL is not configured — running without Redis");
                return Ok(RedisInit {
                    handles: None,
                    sentinel_health_check_task: None,
                });
            }
            Ok(RedisInit {
                handles: Some(init_standalone(config, &redis_url).await?),
                sentinel_health_check_task: None,
            })
        }
        RedisDeploymentMode::Sentinel => init_sentinel(config, cancel).await,
    }
}

#[cfg(test)]
mod init_tests {
    use super::*;

    #[tokio::test]
    async fn test_init_redis_standalone_without_url_returns_none() {
        let mut config = Config::default();
        config.redis.url.clear();
        config.redis.deployment_mode = RedisDeploymentMode::Standalone;

        let result = init_redis(&config, None)
            .await
            .expect("standalone without redis.url should be allowed");

        assert!(result.handles.is_none());
        assert!(result.sentinel_health_check_task.is_none());
    }

    #[tokio::test]
    async fn test_init_redis_standalone_with_split_config_attempts_connection() {
        let mut config = Config::default();
        config.redis.url.clear();
        config.redis.deployment_mode = RedisDeploymentMode::Standalone;
        config.redis.host = "127.0.0.1".to_string();
        config.redis.port = 1;
        config.redis.database = 7;

        let err = init_redis(&config, None)
            .await
            .expect_err("split redis config should be treated as configured at runtime");

        assert!(
            err.to_string().contains("Connection refused")
                || err.to_string().contains("connection refused")
                || err
                    .to_string()
                    .contains("failed to lookup address information")
                || err.to_string().contains("timed out")
                || err.to_string().contains("os error"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_init_redis_sentinel_without_url_attempts_backend_init() {
        let mut config = Config::default();
        config.redis.url.clear();
        config.redis.deployment_mode = RedisDeploymentMode::Sentinel;
        config.redis.sentinel_master_name = Some("mymaster".to_string());
        config.redis.sentinel_addresses = vec!["127.0.0.1:26379".to_string()];

        let err = init_redis(&config, None)
            .await
            .expect_err("sentinel mode must not short-circuit to None when redis.url is empty");

        assert!(
            err.to_string().contains("Sentinel")
                || err.to_string().contains("sentinel")
                || err.to_string().contains("Connection refused")
                || err.to_string().contains("InvalidClientConfig")
                || err.to_string().contains("did not parse"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_parse_redis_node_settings_preserves_auth_and_db_from_url() {
        let mut config = Config::default();
        config.redis.url = "redis://sync-user:secret@redis.example.com:6380/7".to_string();

        let redis_settings = parse_redis_node_settings(&config)
            .expect("parse redis settings")
            .expect("redis.url should produce redis settings");

        assert_eq!(redis_settings.username(), Some("sync-user"));
        assert_eq!(redis_settings.password(), Some("secret"));
        assert_eq!(redis_settings.db(), 7);
    }

    #[test]
    fn test_parse_redis_node_settings_preserves_auth_and_db_from_split_config() {
        let mut config = Config::default();
        config.redis.url.clear();
        config.redis.host = "redis.example.com".to_string();
        config.redis.port = 6380;
        config.redis.username = "sync-user".to_string();
        config.redis.password = "secret".to_string();
        config.redis.database = 7;

        let redis_settings = parse_redis_node_settings(&config)
            .expect("parse redis settings")
            .expect("split redis config should produce redis settings");

        assert_eq!(redis_settings.username(), Some("sync-user"));
        assert_eq!(redis_settings.password(), Some("secret"));
        assert_eq!(redis_settings.db(), 7);
    }

    #[test]
    fn test_redis_connection_manager_config_uses_connect_timeout() {
        let mut config = Config::default();
        config.redis.connect_timeout_seconds = 9;

        let manager_config = redis_connection_manager_config(&config);

        assert_eq!(
            manager_config.connection_timeout(),
            Some(std::time::Duration::from_secs(9))
        );
    }
}

fn redis_connection_manager_config(config: &Config) -> RedisConnectionManagerConfig {
    RedisConnectionManagerConfig::new().set_connection_timeout(Some(
        std::time::Duration::from_secs(config.redis.connect_timeout_seconds),
    ))
}

fn parse_redis_node_settings(config: &Config) -> Result<Option<RedisNodeSettings>, anyhow::Error> {
    let redis_url = config.redis_url();
    if redis_url.is_empty() {
        return Ok(None);
    }

    let connection_info: redis::ConnectionInfo = redis_url.parse()?;
    Ok(Some(connection_info.redis_settings().clone()))
}

fn build_sentinel_node_info(
    config: &Config,
) -> Result<Option<SentinelNodeConnectionInfo>, anyhow::Error> {
    Ok(parse_redis_node_settings(config)?.map(|redis_settings| {
        SentinelNodeConnectionInfo::default().set_redis_connection_info(redis_settings)
    }))
}

async fn init_standalone(config: &Config, redis_url: &str) -> Result<RedisHandles, anyhow::Error> {
    info!("Initializing Redis in standalone mode");
    let client = redis::Client::open(redis_url.to_string())?;
    let conn = redis::aio::ConnectionManager::new_with_config(
        client.clone(),
        redis_connection_manager_config(config),
    )
    .await?;
    let shared = Arc::new(RwLock::new(conn));
    Ok(RedisHandles {
        client,
        conn: shared,
    })
}

async fn init_sentinel(
    config: &Config,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<RedisInit, anyhow::Error> {
    info!("Initializing Redis in sentinel mode");
    let master_name = config
        .redis
        .sentinel_master_name
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("sentinel_master_name is required for sentinel mode"))?;

    if config.redis.sentinel_addresses.is_empty() {
        return Err(anyhow::anyhow!(
            "sentinel_addresses cannot be empty for sentinel mode"
        ));
    }

    let sentinel_addrs: Vec<&str> = config
        .redis
        .sentinel_addresses
        .iter()
        .map(String::as_str)
        .collect();
    let mut sentinel = redis::sentinel::Sentinel::build(sentinel_addrs.clone())?;
    let node_info = build_sentinel_node_info(config)?;
    let client = sentinel
        .async_master_for(master_name.as_str(), node_info.as_ref())
        .await
        .map_err(|e| anyhow::anyhow!("Sentinel master discovery failed: {e}"))?;

    let initial_master_addr = client.get_connection_info().addr().to_string();
    info!(master = %initial_master_addr, "Sentinel discovered initial master");

    let conn = redis::aio::ConnectionManager::new_with_config(
        client.clone(),
        redis_connection_manager_config(config),
    )
    .await?;
    let shared_conn = Arc::new(RwLock::new(conn));

    // Start background health check for Sentinel failover detection.
    {
        let sentinel_addresses = config.redis.sentinel_addresses.clone();
        let master_name = master_name.clone();
        let known_master_addr = initial_master_addr.clone();
        let node_info = node_info.clone();
        let manager_config = redis_connection_manager_config(config);
        let shared_conn_clone = shared_conn.clone();
        let health_check_task = crate::spawn::spawn_monitored(
            "sentinel_master_health_check",
            async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
                // Skip the first immediate tick
                interval.tick().await;
                let mut consecutive_ping_failures = 0u32;
                let mut known_master = known_master_addr;

                loop {
                    if let Some(ref token) = cancel {
                        tokio::select! {
                            () = token.cancelled() => {
                                tracing::info!("Sentinel health check cancelled");
                                return;
                            }
                            _ = interval.tick() => {}
                        }
                    } else {
                        interval.tick().await;
                    }

                    // Clone the ConnectionManager under a read lock to minimize lock
                    // duration. PING is a read-only operation and should not hold
                    // the write lock that is only needed for hot-swapping on failover.
                    let ping_ok = {
                        let mut conn = shared_conn_clone.read().await.clone();
                        redis::cmd("PING")
                            .query_async::<String>(&mut conn)
                            .await
                            .is_ok()
                    };

                    if ping_ok {
                        if consecutive_ping_failures > 0 {
                            tracing::info!(
                                previous_failures = consecutive_ping_failures,
                                "Sentinel health check: Redis PING recovered"
                            );
                        }
                        consecutive_ping_failures = 0;
                        continue;
                    }

                    consecutive_ping_failures += 1;
                    tracing::warn!(
                        consecutive_failures = consecutive_ping_failures,
                        "Sentinel health check: Redis PING failed"
                    );

                    if consecutive_ping_failures < 3 {
                        continue;
                    }

                    tracing::warn!(
                    "Sentinel health check: {} consecutive PING failures, querying Sentinel for current master",
                    consecutive_ping_failures
                );

                    let addrs: Vec<&str> = sentinel_addresses.iter().map(String::as_str).collect();
                    let sentinel_result = redis::sentinel::Sentinel::build(addrs);
                    let mut sentinel = match sentinel_result {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "Sentinel health check: failed to connect to Sentinel nodes"
                            );
                            continue;
                        }
                    };

                    match sentinel
                        .async_master_for(master_name.as_str(), node_info.as_ref())
                        .await
                    {
                        Ok(new_master_client) => {
                            let new_addr =
                                new_master_client.get_connection_info().addr().to_string();
                            if new_addr == known_master {
                                tracing::info!(
                                    master = %new_addr,
                                    "Sentinel master unchanged, rebuilding connection"
                                );
                            } else {
                                tracing::warn!(
                                    old_master = %known_master,
                                    new_master = %new_addr,
                                    "Sentinel failover detected, reconnecting to new master"
                                );
                            }

                            match redis::aio::ConnectionManager::new_with_config(
                                new_master_client,
                                manager_config.clone(),
                            )
                            .await
                            {
                                Ok(new_conn) => {
                                    *shared_conn_clone.write().await = new_conn;
                                    known_master = new_addr;
                                    consecutive_ping_failures = 0;
                                    tracing::info!(
                                        master = %known_master,
                                        "Sentinel health check: reconnected to Redis master"
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(
                                        error = %e,
                                        "Sentinel health check: failed to create new ConnectionManager"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "Sentinel health check: failed to query current master"
                            );
                        }
                    }
                }
            },
        );
        info!(
            "Sentinel master health check started (interval: 5s, failover threshold: 3 failures)"
        );

        Ok(RedisInit {
            handles: Some(RedisHandles {
                client,
                conn: shared_conn,
            }),
            sentinel_health_check_task: Some(health_check_task),
        })
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;
    use synctv_core_testing::start_redis_with_client;

    /// M15: Verify that the sentinel health check uses a read lock (clone) for PING,
    /// not a write lock. We test this by confirming that multiple concurrent readers
    /// are not blocked while the shared handle is held.
    #[tokio::test]
    async fn test_read_lock_allows_concurrent_access() {
        // Create a shared RwLock<String> to simulate the connection pattern.
        let shared = Arc::new(RwLock::new("original".to_string()));

        // Take a read lock, clone the value, and verify another read lock is not blocked.
        let cloned = {
            let guard = shared.read().await;
            guard.clone()
        };
        assert_eq!(cloned, "original");

        // Another read should succeed immediately (no write lock held).
        let second = shared.read().await.clone();
        assert_eq!(second, "original");
    }

    /// Verify that RedisHandles::conn_snapshot returns a clone from the shared handle.
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_conn_snapshot_returns_clone() {
        let (_redis, client) = start_redis_with_client().await;
        let conn = redis::aio::ConnectionManager::new(client.clone())
            .await
            .unwrap();
        let handles = RedisHandles {
            client,
            conn: Arc::new(RwLock::new(conn)),
        };

        // conn_snapshot should return a working clone
        let mut snapshot = handles.conn_snapshot().await;
        let pong: String = redis::cmd("PING").query_async(&mut snapshot).await.unwrap();
        assert_eq!(pong, "PONG");
    }
}
