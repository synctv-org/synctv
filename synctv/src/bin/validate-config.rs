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
    // Set up basic console logging for validation output
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "warn");
    }
    tracing_subscriber::fmt::init();

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
        println!("📁 Loading configuration from: {path}");
    } else {
        println!("📁 No config file found, will validate environment variables");
    }
    println!();

    // Load and parse configuration
    let config = match load_config_for_validation(&config_path) {
        Ok(cfg) => {
            println!("✅ Configuration parsed successfully\n");
            cfg
        }
        Err(e) => {
            eprintln!("❌ Failed to parse configuration: {e}\n");
            eprintln!("Please check the configuration file format and syntax.");
            process::exit(2);
        }
    };

    // Run validation
    println!("🔍 Running validation checks...\n");
    match config.validate() {
        Ok(()) => {
            println!("✅ All validation checks passed!\n");
            print_config_summary(&config);
            println!("\n✨ Configuration is ready for deployment");
            process::exit(0);
        }
        Err(errors) => {
            eprintln!("❌ Configuration validation failed with {} error(s):\n", errors.len());
            for (i, error) in errors.iter().enumerate() {
                eprintln!("  {}. {}", i + 1, error);
            }
            eprintln!("\n💡 Please fix these issues before deploying.");
            process::exit(1);
        }
    }
}

/// Load configuration without starting the server
fn load_config_for_validation(config_path: &Option<String>) -> Result<Config, String> {
    if let Some(path) = config_path {
        Config::from_file(path).map_err(|e| e.to_string())
    } else {
        Config::from_env()
            .or_else(|_| Ok(Config::default()))
            .map_err(|e: std::convert::Infallible| e.to_string())
    }
}

/// Print a summary of key configuration settings
fn print_config_summary(config: &Config) {
    println!("📋 Configuration Summary");
    println!("   ─────────────────────");
    println!("   Server:");
    println!("     • gRPC: {}", config.grpc_address());
    println!("     • HTTP: {}", config.http_address());
    if !config.server.cluster_secret.is_empty() {
        println!("     • Cluster mode: enabled");
    }

    println!("   Database:");
    println!("     • URL: {}", mask_sensitive(&config.database.url));

    if !config.redis.url.is_empty() {
        println!("   Redis:");
        println!("     • URL: {}", mask_sensitive(&config.redis.url));
    }

    println!("   WebRTC:");
    println!("     • Mode: {:?}", config.webrtc.mode);
    if config.webrtc.enable_builtin_stun {
        println!("     • STUN port: {}", config.webrtc.stun_port);
    }

    println!("   Livestream:");
    println!("     • RTMP port: {}", config.livestream.rtmp_port);

    // OAuth2 providers
    if let Some(providers) = config.oauth2.providers.as_object() {
        if !providers.is_empty() {
            println!("   OAuth2:");
            for provider_name in providers.keys() {
                println!("     • {provider_name}");
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
