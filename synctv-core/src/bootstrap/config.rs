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
                let source = if explicit_config_path.is_some() {
                    "explicitly set SYNCTV_CONFIG_PATH"
                } else {
                    "auto-discovered config file"
                };
                return Err(anyhow::anyhow!(
                    "Failed to load config from {source} '{path}': {e}"
                ));
            }
        }
    } else if let Some(ref explicit_path) = explicit_config_path {
        // SYNCTV_CONFIG_PATH was explicitly set but the file doesn't exist
        return Err(anyhow::anyhow!(
            "Config file not found at explicitly set SYNCTV_CONFIG_PATH '{explicit_path}'"
        ));
    } else {
        eprintln!("No config file found, using environment variables");
        Config::from_env()?
    };

    // Validate configuration (fail fast on misconfigurations)
    if let Err(errors) = config.validate() {
        for error in &errors {
            eprintln!("Config validation error: {error}");
        }
        return Err(anyhow::anyhow!(
            "Configuration validation failed with {} error(s): {}",
            errors.len(),
            errors.join("; ")
        ));
    }

    eprintln!("Configuration loaded and validated successfully");
    eprintln!("API address: {}", config.api_address());

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::load_config;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::tempdir;

    static CONFIG_TEST_SERIAL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn acquire_process_config_test_lock() -> MutexGuard<'static, ()> {
        CONFIG_TEST_SERIAL_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("config test lock should not be poisoned")
    }

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

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
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

    struct CurrentDirGuard {
        previous: std::path::PathBuf,
    }

    impl CurrentDirGuard {
        fn change_to(path: &std::path::Path) -> Self {
            let previous = std::env::current_dir().expect("current dir should be readable");
            std::env::set_current_dir(path).expect("current dir should be settable");
            Self { previous }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous).expect("current dir should be restored");
        }
    }

    #[test]
    fn test_load_config_fails_for_invalid_auto_discovered_file() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().expect("temp dir should be created");
        let config_path = dir.path().join("config.yaml");
        std::fs::write(&config_path, "not: [valid").expect("invalid config should be written");
        let _cwd = CurrentDirGuard::change_to(dir.path());
        let _env = EnvVarGuard::remove("SYNCTV_CONFIG_PATH");

        let err = load_config().expect_err("invalid auto-discovered config must fail closed");

        assert!(
            err.to_string().contains("auto-discovered config file"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_load_config_fails_for_invalid_explicit_file() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().expect("temp dir should be created");
        let config_path = dir.path().join("explicit-config.yaml");
        std::fs::write(&config_path, "not: [valid").expect("invalid config should be written");
        let _env = EnvVarGuard::set(
            "SYNCTV_CONFIG_PATH",
            config_path.to_string_lossy().to_string(),
        );

        let err = load_config().expect_err("invalid explicit config must fail closed");

        assert!(
            err.to_string()
                .contains("explicitly set SYNCTV_CONFIG_PATH"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_load_config_accepts_valid_explicit_file_when_synctv_config_path_is_set() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().expect("temp dir should be created");
        let config_path = dir.path().join("explicit-config.yaml");
        std::fs::write(
            &config_path,
            r#"
jwt:
  secret: "12345678901234567890123456789012"
"#,
        )
        .expect("valid config should be written");
        let _env = EnvVarGuard::set(
            "SYNCTV_CONFIG_PATH",
            config_path.to_string_lossy().to_string(),
        );

        let config = load_config().expect("valid explicit config path should load successfully");

        assert_eq!(config.jwt.secret, "12345678901234567890123456789012");
    }
}
