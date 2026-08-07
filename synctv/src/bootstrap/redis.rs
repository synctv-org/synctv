//! Redis initialization
//!
//! Creates a single shared Redis runtime at startup and passes trait-based
//! capabilities everywhere, eliminating duplicate `redis::Client::open` calls
//! and concrete client leakage across the codebase.

use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::info;

use synctv_core::{
    redis_connection_manager_options, ManagedRedisRuntime, RedisConnectionRuntime,
    RedisCoordinationRuntime, RedisDeploymentMode,
};

type SentinelNodeConnectionInfo = redis::sentinel::SentinelNodeConnectionInfo;
type RedisNodeSettings = redis::RedisConnectionInfo;

pub struct RedisInit {
    pub runtime: Option<Arc<dyn RedisCoordinationRuntime>>,
    pub sentinel_health_check_task: Option<JoinHandle<()>>,
}

fn mask_url_password_for_debug(url: &str) -> String {
    let Some(at_pos) = url.find('@') else {
        return url.to_string();
    };
    let Some(colon_pos) = url[..at_pos].rfind(':') else {
        return url.to_string();
    };
    let scheme_end = url.find("://").map_or(0, |p| p + 3);
    if colon_pos < scheme_end {
        return url.to_string();
    }
    format!("{}:****@{}", &url[..colon_pos], &url[at_pos + 1..])
}

fn trim_ipv6_host_brackets(host: &str) -> &str {
    host.trim()
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or_else(|| host.trim())
}

fn build_url_from_split_parts(
    scheme: &str,
    host: &str,
    port: u16,
    username: Option<&str>,
    password: Option<&str>,
    path_segment: Option<&str>,
) -> String {
    let Ok(mut url) = url::Url::parse(&format!("{scheme}://localhost/")) else {
        return String::new();
    };

    if url.set_host(Some(trim_ipv6_host_brackets(host))).is_err()
        || url.set_port(Some(port)).is_err()
    {
        return String::new();
    }

    if let Some(username) = username {
        if url.set_username(username).is_err() {
            return String::new();
        }
    }
    if let Some(password) = password {
        if url.set_password(Some(password)).is_err() {
            return String::new();
        }
    }
    if let Some(path_segment) = path_segment {
        let Ok(mut segments) = url.path_segments_mut() else {
            return String::new();
        };
        segments.clear().push(path_segment);
        drop(segments);
    }

    url.to_string()
}

#[derive(Clone)]
pub struct RedisConnectionOptions {
    pub url: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: i64,
    pub connect_timeout_seconds: u64,
    pub response_timeout_seconds: u64,
    pub pipeline_buffer_size: usize,
    pub key_prefix: String,
    pub deployment_mode: RedisDeploymentMode,
    pub sentinel_master_name: Option<String>,
    pub sentinel_addresses: Vec<String>,
}

impl std::fmt::Debug for RedisConnectionOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let masked_sentinel: Vec<String> = self
            .sentinel_addresses
            .iter()
            .map(|url| mask_url_password_for_debug(url))
            .collect();
        f.debug_struct("RedisConnectionOptions")
            .field("url", &mask_url_password_for_debug(&self.url))
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("database", &self.database)
            .field("connect_timeout_seconds", &self.connect_timeout_seconds)
            .field("response_timeout_seconds", &self.response_timeout_seconds)
            .field("pipeline_buffer_size", &self.pipeline_buffer_size)
            .field("key_prefix", &self.key_prefix)
            .field("deployment_mode", &self.deployment_mode)
            .field("sentinel_master_name", &self.sentinel_master_name)
            .field("sentinel_addresses", &masked_sentinel)
            .finish()
    }
}

impl Default for RedisConnectionOptions {
    fn default() -> Self {
        Self {
            url: String::new(),
            host: String::new(),
            port: 0,
            username: String::new(),
            password: String::new(),
            database: 0,
            connect_timeout_seconds: 5,
            response_timeout_seconds: 5,
            pipeline_buffer_size: 512,
            key_prefix: "synctv:".to_string(),
            deployment_mode: RedisDeploymentMode::Standalone,
            sentinel_master_name: None,
            sentinel_addresses: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RedisInitOptions {
    pub redis: RedisConnectionOptions,
}

impl RedisInitOptions {
    #[must_use]
    pub fn redis_url(&self) -> String {
        if !self.redis.url.trim().is_empty() {
            return self.redis.url.clone();
        }

        if self.redis.host.trim().is_empty() || self.redis.port == 0 {
            return String::new();
        }

        if !self.redis.username.is_empty() {
            build_url_from_split_parts(
                "redis",
                &self.redis.host,
                self.redis.port,
                Some(&self.redis.username),
                Some(&self.redis.password),
                Some(&self.redis.database.to_string()),
            )
        } else if !self.redis.password.is_empty() {
            build_url_from_split_parts(
                "redis",
                &self.redis.host,
                self.redis.port,
                Some(""),
                Some(&self.redis.password),
                Some(&self.redis.database.to_string()),
            )
        } else {
            build_url_from_split_parts(
                "redis",
                &self.redis.host,
                self.redis.port,
                None,
                None,
                Some(&self.redis.database.to_string()),
            )
        }
    }
}

impl std::fmt::Debug for RedisInit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisInit")
            .field("runtime_configured", &self.runtime.is_some())
            .field(
                "sentinel_health_check_task_running",
                &self.sentinel_health_check_task.is_some(),
            )
            .finish()
    }
}

impl RedisInit {
    #[must_use]
    pub fn connection_runtime(&self) -> Option<Arc<dyn RedisConnectionRuntime>> {
        self.runtime
            .clone()
            .map(|runtime| runtime as Arc<dyn RedisConnectionRuntime>)
    }

    #[must_use]
    pub fn coordination_runtime(&self) -> Option<Arc<dyn RedisCoordinationRuntime>> {
        self.runtime.clone()
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
    options: &RedisInitOptions,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<RedisInit, anyhow::Error> {
    match options.redis.deployment_mode {
        RedisDeploymentMode::Standalone => {
            let redis_url = options.redis_url();
            if redis_url.is_empty() {
                info!("Redis URL is not configured — running without Redis");
                return Ok(RedisInit {
                    runtime: None,
                    sentinel_health_check_task: None,
                });
            }
            Ok(RedisInit {
                runtime: Some(init_standalone(options, &redis_url).await?),
                sentinel_health_check_task: None,
            })
        }
        RedisDeploymentMode::Sentinel => init_sentinel(options, cancel).await,
    }
}

pub(super) fn build_redis_connection_manager_options(
    options: &RedisInitOptions,
) -> redis::aio::ConnectionManagerConfig {
    redis_connection_manager_options(
        std::time::Duration::from_secs(options.redis.connect_timeout_seconds),
        std::time::Duration::from_secs(options.redis.response_timeout_seconds),
        options.redis.pipeline_buffer_size,
    )
}

pub(super) fn parse_redis_node_settings(
    options: &RedisInitOptions,
) -> Result<Option<RedisNodeSettings>, anyhow::Error> {
    let redis_url = options.redis_url();
    if redis_url.is_empty() {
        return Ok(None);
    }

    let connection_info: redis::ConnectionInfo = redis_url.parse()?;
    Ok(Some(connection_info.redis_settings().clone()))
}

fn build_sentinel_node_info(
    options: &RedisInitOptions,
) -> Result<Option<SentinelNodeConnectionInfo>, anyhow::Error> {
    Ok(parse_redis_node_settings(options)?.map(|redis_settings| {
        SentinelNodeConnectionInfo::default().set_redis_connection_info(redis_settings)
    }))
}

async fn init_standalone(
    options: &RedisInitOptions,
    redis_url: &str,
) -> Result<Arc<dyn RedisCoordinationRuntime>, anyhow::Error> {
    info!("Initializing Redis in standalone mode");
    let client = redis::Client::open(redis_url.to_string())?;
    let conn = redis::aio::ConnectionManager::new_with_config(
        client.clone(),
        build_redis_connection_manager_options(options),
    )
    .await?;
    let runtime = ManagedRedisRuntime::new_with_operation_timeout(
        client,
        Arc::new(RwLock::new(conn)),
        std::time::Duration::from_secs(options.redis.response_timeout_seconds),
    );
    Ok(Arc::new(runtime))
}

async fn init_sentinel(
    options: &RedisInitOptions,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> Result<RedisInit, anyhow::Error> {
    info!("Initializing Redis in sentinel mode");
    let master_name = options
        .redis
        .sentinel_master_name
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("sentinel_master_name is required for sentinel mode"))?;

    if options.redis.sentinel_addresses.is_empty() {
        return Err(anyhow::anyhow!(
            "sentinel_addresses cannot be empty for sentinel mode"
        ));
    }

    let sentinel_addrs: Vec<&str> = options
        .redis
        .sentinel_addresses
        .iter()
        .map(String::as_str)
        .collect();
    let mut sentinel = redis::sentinel::Sentinel::build(sentinel_addrs.clone())?;
    let node_info = build_sentinel_node_info(options)?;
    let client = sentinel
        .async_master_for(master_name.as_str(), node_info.as_ref())
        .await
        .map_err(|e| anyhow::anyhow!("Sentinel master discovery failed: {e}"))?;

    let initial_master_addr = client.get_connection_info().addr().to_string();
    info!(master = %initial_master_addr, "Sentinel discovered initial master");

    let conn = redis::aio::ConnectionManager::new_with_config(
        client.clone(),
        build_redis_connection_manager_options(options),
    )
    .await?;
    let shared_conn = Arc::new(RwLock::new(conn));

    // Start background health check for Sentinel failover detection.
    {
        let sentinel_addresses = options.redis.sentinel_addresses.clone();
        let master_name = master_name.clone();
        let known_master_addr = initial_master_addr.clone();
        let node_info = node_info.clone();
        let manager_options = build_redis_connection_manager_options(options);
        let shared_conn_clone = shared_conn.clone();
        let health_check_task = synctv_core::spawn::spawn_monitored(
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
                                manager_options.clone(),
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
            runtime: Some(Arc::new(ManagedRedisRuntime::new_with_operation_timeout(
                client,
                shared_conn,
                std::time::Duration::from_secs(options.redis.response_timeout_seconds),
            ))),
            sentinel_health_check_task: Some(health_check_task),
        })
    }
}

#[cfg(test)]
#[path = "redis_tests.rs"]
mod tests;
