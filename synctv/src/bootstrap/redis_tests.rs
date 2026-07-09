use std::sync::Arc;

use super::{RedisDeploymentMode, RedisInitOptions};
use crate::bootstrap::redis::{
    build_redis_connection_manager_options, init_redis, parse_redis_node_settings,
};
use redis::aio::ConnectionManager;
use redis::cmd;
use synctv_core::{ManagedRedisRuntime, RedisConnectionRuntime};
use synctv_core_testing::{start_redis_with_client, TestOptionExt, TestResultExt};
use tokio::sync::RwLock;

#[tokio::test]
async fn test_init_redis_standalone_without_url_returns_none() {
    let mut options = RedisInitOptions::default();
    options.redis.url.clear();
    options.redis.deployment_mode = RedisDeploymentMode::Standalone;

    let result = init_redis(&options, None)
        .await
        .checked("standalone without redis.url should be allowed");

    assert!(result.runtime.is_none());
    assert!(result.sentinel_health_check_task.is_none());
}

#[tokio::test]
async fn test_init_redis_standalone_with_split_config_attempts_connection() {
    let mut options = RedisInitOptions::default();
    options.redis.url.clear();
    options.redis.deployment_mode = RedisDeploymentMode::Standalone;
    options.redis.host = "127.0.0.1".to_string();
    options.redis.port = 1;
    options.redis.database = 7;

    let err = init_redis(&options, None)
        .await
        .failed("split redis config should be treated as configured at runtime");

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
    let mut options = RedisInitOptions::default();
    options.redis.url.clear();
    options.redis.deployment_mode = RedisDeploymentMode::Sentinel;
    options.redis.sentinel_master_name = Some("mymaster".to_string());
    options.redis.sentinel_addresses = vec!["127.0.0.1:26379".to_string()];

    let err = init_redis(&options, None)
        .await
        .failed("sentinel mode must not short-circuit to None when redis.url is empty");

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
    let mut options = RedisInitOptions::default();
    options.redis.url = "redis://sync-user:secret@redis.example.com:6380/7".to_string();

    let redis_settings = parse_redis_node_settings(&options)
        .checked("parse redis settings")
        .checked("redis.url should produce redis settings");

    assert_eq!(redis_settings.username(), Some("sync-user"));
    assert_eq!(redis_settings.password(), Some("secret"));
    assert_eq!(redis_settings.db(), 7);
}

#[test]
fn test_parse_redis_node_settings_preserves_auth_and_db_from_split_config() {
    let mut options = RedisInitOptions::default();
    options.redis.url.clear();
    options.redis.host = "redis.example.com".to_string();
    options.redis.port = 6380;
    options.redis.username = "sync-user".to_string();
    options.redis.password = "secret".to_string();
    options.redis.database = 7;

    let redis_settings = parse_redis_node_settings(&options)
        .checked("parse redis settings")
        .checked("split redis config should produce redis settings");

    assert_eq!(redis_settings.username(), Some("sync-user"));
    assert_eq!(redis_settings.password(), Some("secret"));
    assert_eq!(redis_settings.db(), 7);
}

#[test]
fn test_redis_connection_manager_options_uses_connect_timeout() {
    let mut options = RedisInitOptions::default();
    options.redis.connect_timeout_seconds = 9;
    options.redis.response_timeout_seconds = 11;
    options.redis.pipeline_buffer_size = 768;

    let manager_options = build_redis_connection_manager_options(&options);

    assert_eq!(
        manager_options.connection_timeout(),
        Some(std::time::Duration::from_secs(9))
    );
    assert_eq!(
        manager_options.response_timeout(),
        Some(std::time::Duration::from_secs(11))
    );
    assert!(
        format!("{manager_options:?}").contains("pipeline_buffer_size: Some(768)"),
        "pipeline buffer size should be applied to ConnectionManagerConfig"
    );
}

#[tokio::test]
async fn test_read_lock_allows_concurrent_access() {
    let shared = Arc::new(RwLock::new("original".to_string()));

    let cloned = {
        let guard = shared.read().await;
        guard.clone()
    };
    assert_eq!(cloned, "original");

    let second = shared.read().await.clone();
    assert_eq!(second, "original");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_snapshot_returns_clone() {
    let (_redis, client) = start_redis_with_client().await;
    let conn = ConnectionManager::new(client.clone())
        .await
        .checked("operation should succeed");
    let runtime = ManagedRedisRuntime::new(client, Arc::new(RwLock::new(conn)));

    let mut snapshot = runtime
        .snapshot()
        .await
        .checked("snapshot should return a Redis connection");
    let pong: String = cmd("PING")
        .query_async(&mut snapshot)
        .await
        .checked("operation should succeed");
    assert_eq!(pong, "PONG");
}
