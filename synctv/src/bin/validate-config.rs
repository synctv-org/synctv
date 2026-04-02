//! Configuration Validation Tool
//!
//! Validates a `SyncTV` configuration file without starting the server.
//! Useful for CI/CD pipelines and pre-deployment validation.
//!
//! ## Usage
//!
//! ```bash
//! # Validate config.yaml in current directory
//! cargo run --bin validate-config
//!
//! # Validate specific config file
//! SYNCTV_CONFIG_PATH=/path/to/config.yaml cargo run --bin validate-config
//!
//! # In production (after building)
//! SYNCTV_CONFIG_PATH=/etc/synctv/config.yaml ./validate-config
//! ```
//!
//! ## Exit Codes
//!
//! - 0: Configuration is valid
//! - 1: Configuration has validation errors
//! - 2: Configuration file not found or parse error

use std::process;
use synctv_core::Config;

fn main() {
    // Set up basic console logging for validation output without mutating the
    // process environment (Rust 2024 treats env writes as unsafe).
    let log_level = std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_string());
    let filter = tracing_subscriber::EnvFilter::try_new(log_level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    println!("SyncTV Configuration Validator");
    println!("================================\n");

    // Try to determine which config file will be loaded
    let config_path = std::env::var("SYNCTV_CONFIG_PATH")
        .ok()
        .or_else(|| {
            if std::path::Path::new("config.yaml").exists() {
                Some("config.yaml".to_string())
            } else {
                None
            }
        })
        .or_else(|| {
            if std::path::Path::new("/config/config.yaml").exists() {
                Some("/config/config.yaml".to_string())
            } else {
                None
            }
        });

    if let Some(ref path) = config_path {
        println!("INFO: loading configuration from: {path}");
    } else {
        println!("INFO: no config file found, validating environment variables");
    }
    println!();

    // Load and parse configuration
    let config = match load_config_for_validation(&config_path) {
        Ok(cfg) => {
            println!("OK: configuration parsed successfully\n");
            cfg
        }
        Err(e) => {
            eprintln!("ERROR: failed to parse configuration: {e}\n");
            eprintln!("Please check the configuration file format and syntax.");
            process::exit(2);
        }
    };

    // Run validation
    println!("INFO: running validation checks...\n");
    match config.validate() {
        Ok(()) => {
            println!("OK: all validation checks passed\n");
            print_config_summary(&config);
            println!("\nOK: configuration is ready for deployment");
            process::exit(0);
        }
        Err(errors) => {
            eprintln!(
                "ERROR: configuration validation failed with {} error(s):\n",
                errors.len()
            );
            for (i, error) in errors.iter().enumerate() {
                eprintln!("  {}. {}", i + 1, error);
            }
            eprintln!("\nPlease fix these issues before deploying.");
            process::exit(1);
        }
    }
}

/// Load configuration without starting the server
fn load_config_for_validation(config_path: &Option<String>) -> Result<Config, String> {
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    load_config_for_validation_with_env(config_path, &env)
}

fn load_config_for_validation_with_env(
    config_path: &Option<String>,
    env: &std::collections::HashMap<String, String>,
) -> Result<Config, String> {
    if let Some(path) = config_path {
        if !std::path::Path::new(path).exists() {
            return Err(format!("config file not found: {path}"));
        }
        synctv_core::Config::load_with_env_map(Some(path), env).map_err(|e| e.to_string())
    } else {
        Config::from_env_map(env).map_err(|e| e.to_string())
    }
}

/// Print a summary of key configuration settings
fn print_config_summary(config: &Config) {
    println!("Configuration Summary");
    println!("---------------------");
    println!("  Server:");
    println!("    - API: {}", config.api_address());
    if config.cluster.enabled {
        println!("    - Cluster mode: enabled");
    }

    println!("  Database:");
    println!("    - URL: {}", mask_sensitive(&config.database.url));

    if !config.redis.url.is_empty() {
        println!("  Redis:");
        println!("    - URL: {}", mask_sensitive(&config.redis.url));
    }

    println!("  WebRTC:");
    println!("    - Mode: {:?}", config.webrtc.mode);
    if config.webrtc.enable_builtin_stun {
        println!("    - STUN port: {}", config.webrtc.stun_port);
    }

    println!("  Livestream:");
    println!("    - RTMP port: {}", config.livestream.rtmp_port);

    // OAuth2 providers
    if let Some(providers) = config.oauth2.providers.as_object() {
        if !providers.is_empty() {
            println!("  OAuth2:");
            for provider_name in providers.keys() {
                println!("    - {provider_name}");
            }
        }
    }
}

/// Mask sensitive information in URLs
fn mask_sensitive(url: &str) -> String {
    if url.is_empty() {
        return "(not configured)".to_string();
    }

    // Mask password in database URLs
    if let Some(at_pos) = url.rfind('@') {
        if let Some(colon_pos) = url[..at_pos].rfind(':') {
            let mut masked = String::from(&url[..colon_pos]);
            masked.push_str(":****");
            masked.push_str(&url[at_pos..]);
            return masked;
        }
    }

    // For Redis URLs or other formats
    if url.contains("://") {
        if let Some(start) = url.find("://") {
            if let Some(at_pos) = url[start + 3..].find('@') {
                let mut masked = String::from(&url[..start + 3]);
                masked.push_str("****");
                masked.push_str(&url[start + 3 + at_pos..]);
                return masked;
            }
        }
    }

    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct EnvVarGuard {
        key: &'static str,
        value: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl Into<String>) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value.into());
            Self {
                key,
                value: previous,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.value.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn load_config_for_validation_rejects_invalid_env_override() {
        let result = load_config_for_validation_with_env(
            &Some("config.yaml".to_string()),
            &HashMap::from([("SYNCTV_SERVER_PORT".to_string(), "invalid-port".to_string())]),
        );
        assert!(
            result.is_err(),
            "invalid env override must not fall back to defaults"
        );
    }

    #[test]
    fn load_config_for_validation_uses_defaults_when_env_is_empty() {
        let result = load_config_for_validation_with_env(&None, &HashMap::new());
        assert!(
            result.is_ok(),
            "empty environment should still load default config"
        );
    }

    #[test]
    fn load_config_for_validation_rejects_explicit_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing_path = dir.path().join("missing-config.yaml");

        let result = load_config_for_validation_with_env(
            &Some(missing_path.display().to_string()),
            &HashMap::new(),
        );

        assert!(
            result.is_err(),
            "explicit missing config path must fail instead of falling back to defaults"
        );
    }

    #[test]
    fn load_config_for_validation_applies_env_overrides_on_top_of_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            r#"
server:
  port: 50051
database:
  url: "postgresql://user:pass@localhost/db"
  max_connections: 20
  min_connections: 5
  connect_timeout_seconds: 10
  idle_timeout_seconds: 600
  max_lifetime_seconds: 1800
jwt:
  secret: "12345678901234567890123456789012"
"#,
        )
        .expect("write config");

        let result = load_config_for_validation_with_env(
            &Some(path.display().to_string()),
            &HashMap::from([("SYNCTV_SERVER_PORT".to_string(), "50061".to_string())]),
        )
        .expect("config with env override should load");

        assert_eq!(result.server.port, 50061);
    }

    #[test]
    fn load_config_for_validation_reads_process_env_overrides() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            r#"
server:
  port: 50051
jwt:
  secret: "12345678901234567890123456789012"
"#,
        )
        .expect("write config");
        let _env = EnvVarGuard::set("SYNCTV_SERVER_PORT", "50061");

        let result = load_config_for_validation(&Some(path.display().to_string()))
            .expect("config loader should honor process env overrides");

        assert_eq!(result.server.port, 50061);
    }
}
