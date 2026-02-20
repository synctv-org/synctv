//! Configuration loading

use anyhow::Result;

use crate::Config;

/// Load configuration from config file or environment variables
///
/// Config file search order:
/// 1. `SYNCTV_CONFIG_PATH` environment variable (explicit path)
/// 2. ./config.yaml (current working directory)
/// 3. /config/config.yaml (Kubernetes mount path)
/// 4. Fall back to environment variables only
pub fn load_config() -> Result<Config> {
    // Check if SYNCTV_CONFIG_PATH was explicitly set (even if file doesn't exist)
    let explicit_config_path = std::env::var("SYNCTV_CONFIG_PATH").ok();

    // Determine config file path: env var > CWD > /config/ mount
    let config_path = explicit_config_path
        .clone()
        .filter(|p| std::path::Path::new(p).exists())
        .or_else(|| {
            let cwd = "config.yaml";
            if std::path::Path::new(cwd).exists() {
                Some(cwd.to_string())
            } else {
                None
            }
        })
        .or_else(|| {
            let k8s = "/config/config.yaml";
            if std::path::Path::new(k8s).exists() {
                Some(k8s.to_string())
            } else {
                None
            }
        });

    let config = if let Some(path) = config_path {
        eprintln!("Loading config from {path}");
        match Config::from_file(&path) {
            Ok(cfg) => {
                eprintln!("Successfully loaded {path}");
                cfg
            }
            Err(e) => {
                // If SYNCTV_CONFIG_PATH was explicitly set, fail hard instead of
                // silently falling back to defaults, which would hide misconfigurations.
                if explicit_config_path.is_some() {
                    return Err(anyhow::anyhow!(
                        "Failed to load config from explicitly set SYNCTV_CONFIG_PATH '{path}': {e}"
                    ));
                }
                eprintln!("Failed to load {path}: {e}");
                eprintln!("Falling back to environment variables");
                Config::from_env().unwrap_or_default()
            }
        }
    } else if let Some(ref explicit_path) = explicit_config_path {
        // SYNCTV_CONFIG_PATH was explicitly set but the file doesn't exist
        return Err(anyhow::anyhow!(
            "Config file not found at explicitly set SYNCTV_CONFIG_PATH '{explicit_path}'"
        ));
    } else {
        eprintln!("No config file found, using environment variables");
        Config::from_env().unwrap_or_else(|e| {
            eprintln!("Failed to load config: {e}");
            eprintln!("Using default configuration");
            Config::default()
        })
    };

    // Validate configuration (fail fast on misconfigurations)
    if let Err(errors) = config.validate() {
        for error in &errors {
            eprintln!("Config validation error: {}", error);
        }
        return Err(anyhow::anyhow!(
            "Configuration validation failed with {} error(s): {}",
            errors.len(),
            errors.join("; ")
        ));
    }

    eprintln!("Configuration loaded and validated successfully");
    eprintln!("gRPC address: {}", config.grpc_address());
    eprintln!("HTTP address: {}", config.http_address());

    Ok(config)
}
