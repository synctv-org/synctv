//! Config parsing edge case tests (no Docker needed)
//!
//! Tests trusted proxy matching, database URL password masking,
//! and config serialization roundtrip.
//!
//! Run with: cargo test --test `config_edge_case_tests`
#![allow(clippy::unwrap_used)]

use std::net::IpAddr;
use synctv_core::config::{Config, DatabaseConfig, RedisConfig, RedisDeploymentMode, ServerConfig};

// ============================================================================
// Trusted proxy matching
// ============================================================================

fn make_server_config(proxies: Vec<String>) -> ServerConfig {
    ServerConfig {
        trusted_proxies: proxies,
        ..ServerConfig::default()
    }
}

#[test]
fn test_is_trusted_proxy_empty_list() {
    let config = make_server_config(vec![]);
    let ip: IpAddr = "10.0.0.1".parse().unwrap();
    assert!(
        !config.is_trusted_proxy(&ip),
        "Empty proxy list should trust nothing"
    );
}

#[test]
fn test_is_trusted_proxy_single_ip() {
    let config = make_server_config(vec!["10.0.0.1".to_string()]);
    assert!(config.is_trusted_proxy(&"10.0.0.1".parse().unwrap()));
    assert!(!config.is_trusted_proxy(&"10.0.0.2".parse().unwrap()));
}

#[test]
fn test_is_trusted_proxy_cidr() {
    let config = make_server_config(vec!["10.0.0.0/8".to_string()]);
    assert!(config.is_trusted_proxy(&"10.0.0.1".parse().unwrap()));
    assert!(config.is_trusted_proxy(&"10.255.255.255".parse().unwrap()));
    assert!(!config.is_trusted_proxy(&"11.0.0.1".parse().unwrap()));
}

#[test]
fn test_is_trusted_proxy_multiple_entries() {
    let config = make_server_config(vec!["10.0.0.0/8".to_string(), "192.168.1.100".to_string()]);
    assert!(config.is_trusted_proxy(&"10.1.2.3".parse().unwrap()));
    assert!(config.is_trusted_proxy(&"192.168.1.100".parse().unwrap()));
    assert!(!config.is_trusted_proxy(&"192.168.1.101".parse().unwrap()));
}

#[test]
fn test_is_trusted_proxy_ipv6() {
    let config = make_server_config(vec!["::1".to_string()]);
    assert!(config.is_trusted_proxy(&"::1".parse().unwrap()));
    assert!(!config.is_trusted_proxy(&"::2".parse().unwrap()));
}

#[test]
fn test_is_trusted_proxy_ipv6_cidr() {
    let config = make_server_config(vec!["fd00::/8".to_string()]);
    assert!(config.is_trusted_proxy(&"fd00::1".parse().unwrap()));
    assert!(config.is_trusted_proxy(&"fdff::1".parse().unwrap()));
    assert!(!config.is_trusted_proxy(&"fe80::1".parse().unwrap()));
}

#[test]
fn test_is_trusted_proxy_invalid_entry_ignored() {
    // Invalid entries should be silently skipped
    let config = make_server_config(vec![
        "not-a-valid-ip-or-cidr".to_string(),
        "10.0.0.1".to_string(),
    ]);
    assert!(config.is_trusted_proxy(&"10.0.0.1".parse().unwrap()));
    assert!(!config.is_trusted_proxy(&"10.0.0.2".parse().unwrap()));
}

// ============================================================================
// Database URL password masking in Debug output
// ============================================================================

#[test]
fn test_database_config_debug_masks_password() {
    let config = DatabaseConfig {
        url: "postgresql://synctv:secret_password@localhost:5432/synctv".to_string(),
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
        url: "postgresql://localhost:5432/synctv".to_string(),
        ..DatabaseConfig::default()
    };
    let debug = format!("{config:?}");
    // No @ sign, so no masking needed
    assert!(debug.contains("localhost:5432"));
}

// ============================================================================
// Redis config debug masking
// ============================================================================

#[test]
fn test_redis_config_debug_masks_password() {
    let config = RedisConfig {
        url: "redis://:my_secret@redis-host:6379".to_string(),
        ..RedisConfig::default()
    };
    let debug = format!("{config:?}");
    assert!(
        !debug.contains("my_secret"),
        "Redis password should be masked in Debug output"
    );
    assert!(
        debug.contains("****"),
        "Masked password should appear as ****"
    );
}

// ============================================================================
// Redis deployment mode parsing
// ============================================================================

#[test]
fn test_redis_deployment_mode_serde_roundtrip() {
    let modes = vec![
        RedisDeploymentMode::Standalone,
        RedisDeploymentMode::Sentinel,
    ];
    for mode in modes {
        let json = serde_json::to_string(&mode).unwrap();
        let parsed: RedisDeploymentMode = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, mode);
    }
}

#[test]
fn test_redis_deployment_mode_rename_all() {
    // Serde uses rename_all = "lowercase"
    let json = serde_json::to_string(&RedisDeploymentMode::Standalone).unwrap();
    assert_eq!(json, "\"standalone\"");
    let json = serde_json::to_string(&RedisDeploymentMode::Sentinel).unwrap();
    assert_eq!(json, "\"sentinel\"");
    assert!(serde_json::from_str::<RedisDeploymentMode>("\"cluster\"").is_err());
}

// ============================================================================
// Config validation edge cases
// ============================================================================

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

// ============================================================================
// BootstrapConfig security tests
// ============================================================================

#[test]
fn test_config_validate_rejects_empty_root_password_when_creating_root() {
    // When create_root_user is true, empty password should be rejected
    let mut config = Config::default();
    config.bootstrap.create_root_user = true;
    config.bootstrap.root_password = String::new();

    let result = config.validate();
    assert!(
        result.is_err(),
        "Validation should fail with empty root password"
    );
    let errors = result.unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.to_lowercase().contains("root password")),
        "Error should mention root password"
    );
}
