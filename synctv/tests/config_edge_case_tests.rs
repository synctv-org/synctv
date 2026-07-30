use synctv::app_config::{AppConfig, DatabaseConfig, RedisConfig, RedisDeploymentMode};

fn ok<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

#[test]
fn test_database_config_debug_masks_password() {
    let config = DatabaseConfig {
        url: "postgresql://synctv:secret_password@db.invalid/synctv".to_string(),
        ..DatabaseConfig::default()
    };
    let debug = format!("{config:?}");

    assert!(!debug.contains("secret_password"));
    assert!(debug.contains("****"));
    assert!(debug.contains("synctv"));
}

#[test]
fn test_database_config_debug_no_password() {
    let config = DatabaseConfig {
        url: "postgresql://db.invalid/synctv".to_string(),
        ..DatabaseConfig::default()
    };

    assert!(format!("{config:?}").contains("db.invalid"));
}

#[test]
fn test_redis_config_debug_masks_password() {
    let config = RedisConfig {
        url: "redis://:my_secret@redis-host:6379".to_string(),
        sentinel_addresses: vec![
            "redis://sentinel_user:sentinel_secret@sentinel-a:26379".to_string(),
            "redis://sentinel-b:26379".to_string(),
        ],
        ..RedisConfig::default()
    };
    let debug = format!("{config:?}");

    assert!(!debug.contains("my_secret"));
    assert!(!debug.contains("sentinel_secret"));
    assert!(debug.contains("****"));
    assert!(debug.contains("sentinel_user"));
    assert!(debug.contains("sentinel-b"));
}

#[test]
fn test_redis_deployment_mode_rename_all() {
    let json = ok(
        serde_json::to_string(&RedisDeploymentMode::Standalone),
        "standalone mode should serialize",
    );
    assert_eq!(json, "\"standalone\"");

    let json = ok(
        serde_json::to_string(&RedisDeploymentMode::Sentinel),
        "sentinel mode should serialize",
    );
    assert_eq!(json, "\"sentinel\"");
    assert!(serde_json::from_str::<RedisDeploymentMode>("\"cluster\"").is_err());
}

#[test]
fn test_config_api_address() {
    let mut config = AppConfig::default();
    config.server.host = "0.0.0.0".to_string();
    config.server.port = 8080;

    assert_eq!(config.api_address(), "0.0.0.0:8080");
}

#[test]
fn listener_and_advertise_addresses_format_ipv6_hosts() {
    let mut config = AppConfig::default();
    config.server.host = "::1".to_string();
    config.server.advertise_host = "2001:db8::10".to_string();
    config.health.host = "[::1]".to_string();
    config.cluster.host = "::".to_string();
    config.cluster.advertise_host = "2001:db8::20".to_string();

    assert_eq!(config.api_address(), "[::1]:8080");
    assert_eq!(config.health_address(), "[::1]:8081");
    assert_eq!(config.cluster_address(), "[::]:50051");
    assert_eq!(config.advertise_cluster_address(), "[2001:db8::20]:50051");
    assert_eq!(config.advertise_api_address(), "[2001:db8::10]:8080");
}

#[test]
fn test_config_debug_redacts_secrets() {
    let config = AppConfig::default();
    let debug = format!("{config:?}");

    assert!(debug.contains("<redacted>"));
}

#[test]
fn test_config_validate_rejects_empty_root_password_when_creating_root() {
    let mut config = AppConfig::default();
    config.bootstrap.root_password = String::new();

    let errors = config.bootstrap.validate_root_password_for_creation();

    assert!(errors
        .iter()
        .any(|e| e.to_lowercase().contains("root password")));
}
