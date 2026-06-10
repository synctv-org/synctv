use super::*;
use crate::test_helpers::{TestOptionExt, TestResultExt};
use synctv_core_testing::start_redis_with_client;

#[tokio::test]
async fn test_init_redis_standalone_without_url_returns_none() {
    let mut config = Config::default();
    config.redis.url.clear();
    config.redis.deployment_mode = RedisDeploymentMode::Standalone;

    let result = init_redis(&config, None)
        .await
        .checked("standalone without redis.url should be allowed");

    assert!(result.runtime.is_none());
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
    let mut config = Config::default();
    config.redis.url.clear();
    config.redis.deployment_mode = RedisDeploymentMode::Sentinel;
    config.redis.sentinel_master_name = Some("mymaster".to_string());
    config.redis.sentinel_addresses = vec!["127.0.0.1:26379".to_string()];

    let err = init_redis(&config, None)
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
    let mut config = Config::default();
    config.redis.url = "redis://sync-user:secret@redis.example.com:6380/7".to_string();

    let redis_settings = parse_redis_node_settings(&config)
        .checked("parse redis settings")
        .checked("redis.url should produce redis settings");

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
        .checked("parse redis settings")
        .checked("split redis config should produce redis settings");

    assert_eq!(redis_settings.username(), Some("sync-user"));
    assert_eq!(redis_settings.password(), Some("secret"));
    assert_eq!(redis_settings.db(), 7);
}

#[test]
fn test_redis_connection_manager_config_uses_connect_timeout() {
    let mut config = Config::default();
    config.redis.connect_timeout_seconds = 9;
    config.redis.response_timeout_seconds = 11;
    config.redis.pipeline_buffer_size = 768;

    let manager_config = build_redis_connection_manager_config(&config);

    assert_eq!(
        manager_config.connection_timeout(),
        Some(std::time::Duration::from_secs(9))
    );
    assert_eq!(
        manager_config.response_timeout(),
        Some(std::time::Duration::from_secs(11))
    );
    assert!(
        format!("{manager_config:?}").contains("pipeline_buffer_size: Some(768)"),
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
    let conn = redis::aio::ConnectionManager::new(client.clone())
        .await
        .checked("operation should succeed");
    let runtime = ManagedRedisRuntime::new(client, Arc::new(RwLock::new(conn)));

    let mut snapshot = runtime
        .snapshot()
        .await
        .checked("snapshot should return a Redis connection");
    let pong: String = redis::cmd("PING")
        .query_async(&mut snapshot)
        .await
        .checked("operation should succeed");
    assert_eq!(pong, "PONG");
}
