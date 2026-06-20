//! Config parsing edge case tests (no Docker needed)
//!
//! Tests trusted proxy matching, database URL password masking,
//! and config serialization roundtrip.

use std::net::IpAddr;
use synctv_core::config::{Config, DatabaseConfig, RedisConfig, RedisDeploymentMode, ServerConfig};
use synctv_core_testing::ok;

fn ip(value: &str) -> IpAddr {
    ok(value.parse(), "IP address should parse")
}

fn make_server_config(proxies: Vec<String>) -> ServerConfig {
    ServerConfig {
        trusted_proxies: proxies,
        ..ServerConfig::default()
    }
}

#[test]
fn test_is_trusted_proxy_empty_list() {
    let config = make_server_config(vec![]);
    let ip = ip("10.0.0.1");
    assert!(
        !config.is_trusted_proxy(&ip),
        "Empty proxy list should trust nothing"
    );
}

#[test]
fn test_is_trusted_proxy_single_ip() {
    let config = make_server_config(vec!["10.0.0.1".to_string()]);
    assert!(config.is_trusted_proxy(&ip("10.0.0.1")));
    assert!(!config.is_trusted_proxy(&ip("10.0.0.2")));
}

#[test]
fn test_is_trusted_proxy_cidr() {
    let config = make_server_config(vec!["10.0.0.0/8".to_string()]);
    assert!(config.is_trusted_proxy(&ip("10.0.0.1")));
    assert!(config.is_trusted_proxy(&ip("10.255.255.255")));
    assert!(!config.is_trusted_proxy(&ip("11.0.0.1")));
}

#[test]
fn test_is_trusted_proxy_multiple_entries() {
    let config = make_server_config(vec!["10.0.0.0/8".to_string(), "192.168.1.100".to_string()]);
    assert!(config.is_trusted_proxy(&ip("10.1.2.3")));
    assert!(config.is_trusted_proxy(&ip("192.168.1.100")));
    assert!(!config.is_trusted_proxy(&ip("192.168.1.101")));
}

#[test]
fn test_is_trusted_proxy_ipv6() {
    let config = make_server_config(vec!["::1".to_string()]);
    assert!(config.is_trusted_proxy(&ip("::1")));
    assert!(!config.is_trusted_proxy(&ip("::2")));
}

#[test]
fn test_is_trusted_proxy_ipv6_cidr() {
    let config = make_server_config(vec!["fd00::/8".to_string()]);
    assert!(config.is_trusted_proxy(&ip("fd00::1")));
    assert!(config.is_trusted_proxy(&ip("fdff::1")));
    assert!(!config.is_trusted_proxy(&ip("fe80::1")));
}

#[test]
fn test_is_trusted_proxy_invalid_entry_ignored() {
    let config = make_server_config(vec![
        "not-a-valid-ip-or-cidr".to_string(),
        "10.0.0.1".to_string(),
    ]);
    assert!(config.is_trusted_proxy(&ip("10.0.0.1")));
    assert!(!config.is_trusted_proxy(&ip("10.0.0.2")));
}

#[test]
fn test_database_config_debug_masks_password() {
    let config = DatabaseConfig {
        url: "postgresql://synctv:secret_password@db.invalid/synctv".to_string(),
        ..DatabaseConfig::default()
    };
    let debug = format!("{config:?}");
    assert!(
        !debug.contains("secret_password"),
        "Password should be masked in Debug output"
    );
    assert!(
        debug.contains("****"),
        "Masked password should appear as ****"
    );
    assert!(debug.contains("synctv"), "Username should still be visible");
}

#[test]
fn test_database_config_debug_no_password() {
    let config = DatabaseConfig {
        url: "postgresql://db.invalid/synctv".to_string(),
        ..DatabaseConfig::default()
    };
    let debug = format!("{config:?}");
    assert!(debug.contains("db.invalid"));
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
    assert!(
        !debug.contains("my_secret"),
        "Redis password should be masked in Debug output"
    );
    assert!(
        !debug.contains("sentinel_secret"),
        "Redis Sentinel passwords should be masked in Debug output"
    );
    assert!(
        debug.contains("****"),
        "Masked password should appear as ****"
    );
    assert!(
        debug.contains("sentinel_user"),
        "Sentinel username should remain visible"
    );
    assert!(
        debug.contains("sentinel-b"),
        "Sentinel address without password should remain visible"
    );
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
    let mut config = Config::default();
    config.server.host = "0.0.0.0".to_string();
    config.server.port = 8080;
    assert_eq!(config.api_address(), "0.0.0.0:8080");
}

#[test]
fn test_config_debug_redacts_secrets() {
    let config = Config::default();
    let debug = format!("{config:?}");
    // Config Debug impl should redact database and jwt
    assert!(
        debug.contains("<redacted>"),
        "Secrets should be redacted in Config Debug output"
    );
}

#[test]
fn test_config_validate_rejects_empty_root_password_when_creating_root() {
    let mut config = Config::default();
    config.bootstrap.root_password = String::new();

    let errors = config.bootstrap.validate_root_password_for_creation();
    assert!(
        errors
            .iter()
            .any(|e| e.to_lowercase().contains("root password")),
        "Error should mention root password"
    );
}
