//! Configuration loading

use anyhow::Result;
use std::collections::HashMap;
use std::io::ErrorKind;

use crate::{
    config::{absolute_display_path, default_config_search_paths},
    time::set_default_timezone_name,
    Config,
};

#[derive(Debug, Clone, Default)]
pub struct LoadConfigOptions {
    pub config_path: Option<String>,
    pub data_dir: Option<String>,
    pub load_dotenv: bool,
    pub validate: bool,
    pub verbose: bool,
}

pub fn load_dotenv(verbose: bool) -> Result<()> {
    match dotenvy::dotenv() {
        Ok(path) => {
            if verbose {
                eprintln!("Loaded environment from {}", absolute_display_path(&path));
            }
            Ok(())
        }
        Err(dotenvy::Error::Io(err)) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(anyhow::anyhow!("Failed to load .env: {err}")),
    }
}

/// Load configuration from config file or environment variables
///
/// Config file search order:
/// 1. `SYNCTV_CONFIG_PATH` environment variable (explicit path)
/// 2. platform default search paths with extension priority
///    `synctv.yaml`, `synctv.yml`, `synctv.json`, `synctv.toml`
///    for each location (for example current directory, user config dir,
///    macOS `~/.synctv/`, Linux `/etc/synctv/`, `/config/`)
/// 4. Fall back to environment variables only
pub fn load_config() -> Result<Config> {
    load_config_with_options(&LoadConfigOptions {
        config_path: None,
        data_dir: None,
        load_dotenv: true,
        validate: true,
        verbose: false,
    })
}

pub fn load_config_with_options(options: &LoadConfigOptions) -> Result<Config> {
    if options.load_dotenv {
        load_dotenv(options.verbose)?;
    }

    let explicit_config_path = options
        .config_path
        .clone()
        .or_else(|| std::env::var("SYNCTV_CONFIG_PATH").ok());

    let discovered_config_path = explicit_config_path
        .clone()
        .filter(|p| std::path::Path::new(p).exists())
        .or_else(|| {
            default_config_search_paths()
                .into_iter()
                .find(|path| path.exists())
                .map(|path| path.display().to_string())
        });

    let env: HashMap<String, String> = std::env::vars().collect();
    let config = if let Some(path) = discovered_config_path {
        let display_path = absolute_display_path(std::path::Path::new(&path));
        if options.verbose {
            eprintln!("Loading config from {display_path}");
        }
        match Config::load_with_env_map_and_data_dir_override(
            Some(&path),
            &env,
            options.data_dir.as_deref(),
        ) {
            Ok(cfg) => {
                if options.verbose {
                    eprintln!("Successfully loaded {display_path}");
                }
                cfg
            }
            Err(e) => {
                let source = if options.config_path.is_some() {
                    "explicit CLI --config"
                } else if explicit_config_path.is_some() {
                    "explicitly set SYNCTV_CONFIG_PATH"
                } else {
                    "auto-discovered config file"
                };
                return Err(anyhow::anyhow!(
                    "Failed to load config from {source} '{display_path}': {e}"
                ));
            }
        }
    } else if let Some(ref explicit_path) = explicit_config_path {
        // CLI --config or SYNCTV_CONFIG_PATH was explicitly set but the file doesn't exist.
        let source = if options.config_path.is_some() {
            "CLI --config"
        } else {
            "SYNCTV_CONFIG_PATH"
        };
        return Err(anyhow::anyhow!(
            "Config file not found at explicitly set {source} '{}'",
            absolute_display_path(std::path::Path::new(explicit_path))
        ));
    } else {
        if options.verbose {
            eprintln!("No config file found, using environment variables");
        }
        Config::load_with_env_map_and_data_dir_override(None, &env, options.data_dir.as_deref())?
    };

    set_default_timezone_name(&config.time.timezone).map_err(|error| {
        anyhow::anyhow!(
            "Failed to initialize default timezone '{}': {error}",
            config.time.timezone
        )
    })?;

    if options.validate {
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

        if options.verbose {
            eprintln!("Configuration loaded and validated successfully");
            eprintln!("API address: {}", config.api_address());
        }
    } else if options.verbose {
        eprintln!("Configuration loaded successfully");
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::{load_config, load_config_with_options, LoadConfigOptions};
    use crate::time::{default_timezone_name, set_default_timezone_name};
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::tempdir;

    static CONFIG_TEST_SERIAL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn acquire_process_config_test_lock() -> MutexGuard<'static, ()> {
        CONFIG_TEST_SERIAL_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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

    struct TimeZoneGuard {
        previous: String,
    }

    impl TimeZoneGuard {
        fn capture() -> Self {
            Self {
                previous: default_timezone_name(),
            }
        }
    }

    impl Drop for TimeZoneGuard {
        fn drop(&mut self) {
            let _ = set_default_timezone_name(&self.previous);
        }
    }

    fn management_auth_token_guard() -> EnvVarGuard {
        EnvVarGuard::set("SYNCTV_MANAGEMENT_AUTH_TOKEN", "test-management-auth-token")
    }

    #[test]
    fn test_load_config_fails_for_invalid_auto_discovered_file() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().expect("temp dir should be created");
        let config_path = dir.path().join("synctv.yaml");
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
    fn test_load_config_prefers_first_supported_default_extension_in_order() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().expect("temp dir should be created");
        let _cwd = CurrentDirGuard::change_to(dir.path());
        let _env = EnvVarGuard::remove("SYNCTV_CONFIG_PATH");
        let _management_auth = management_auth_token_guard();

        std::fs::write(
            dir.path().join("synctv.yml"),
            "server:\n  port: 58082\njwt:\n  secret: \"12345678901234567890123456789012\"\n",
        )
        .expect("yml config should be written");
        std::fs::write(
            dir.path().join("synctv.json"),
            "{\"server\":{\"port\":58083},\"jwt\":{\"secret\":\"12345678901234567890123456789012\"}}",
        )
        .expect("json config should be written");

        let config = load_config().expect("first discovered config should load");

        assert_eq!(
            config.server.port, 58082,
            "default search must prefer .yml before .json"
        );
    }

    #[test]
    fn test_load_config_fails_for_invalid_explicit_file() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().expect("temp dir should be created");
        let config_path = dir.path().join("explicit-synctv.yaml");
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
        let _cwd = CurrentDirGuard::change_to(dir.path());
        let _management_auth = management_auth_token_guard();
        let config_path = dir.path().join("explicit-synctv.yaml");
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

    #[test]
    fn test_load_config_reads_dotenv_before_resolving_config() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().expect("temp dir should be created");
        let _cwd = CurrentDirGuard::change_to(dir.path());
        let _config_path = EnvVarGuard::remove("SYNCTV_CONFIG_PATH");
        let _management_auth = management_auth_token_guard();
        let _jwt = EnvVarGuard::remove("SYNCTV_JWT_SECRET");
        let _port = EnvVarGuard::remove("SYNCTV_SERVER_PORT");
        std::fs::write(
            dir.path().join(".env"),
            "SYNCTV_JWT_SECRET=12345678901234567890123456789012\nSYNCTV_SERVER_PORT=50061\n",
        )
        .expect(".env should be written");

        let config = load_config().expect(".env-backed config should load successfully");

        assert_eq!(config.jwt.secret, "12345678901234567890123456789012");
        assert_eq!(config.server.port, 50061);
    }

    #[test]
    fn test_load_config_with_options_accepts_explicit_cli_config_path() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().expect("temp dir should be created");
        let _management_auth = management_auth_token_guard();
        let config_path = dir.path().join("cli-synctv.yaml");
        std::fs::write(
            &config_path,
            r#"
jwt:
  secret: "12345678901234567890123456789012"
server:
  port: 58080
"#,
        )
        .expect("valid config should be written");

        let config = load_config_with_options(&LoadConfigOptions {
            config_path: Some(config_path.to_string_lossy().to_string()),
            data_dir: None,
            load_dotenv: false,
            validate: true,
            verbose: false,
        })
        .expect("explicit CLI config path should load successfully");

        assert_eq!(config.server.port, 58080);
    }

    #[test]
    fn test_load_config_with_options_can_skip_validation() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().expect("temp dir should be created");
        let config_path = dir.path().join("invalid-but-loadable.yaml");
        std::fs::write(
            &config_path,
            r#"
jwt:
  secret: ""
"#,
        )
        .expect("config should be written");

        let config = load_config_with_options(&LoadConfigOptions {
            config_path: Some(config_path.to_string_lossy().to_string()),
            data_dir: None,
            load_dotenv: false,
            validate: false,
            verbose: false,
        })
        .expect("loading without validation should succeed");

        assert!(config.jwt.secret.is_empty());
    }

    #[test]
    fn test_load_config_with_options_honors_synctv_config_path_when_cli_path_absent() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().expect("temp dir should be created");
        let config_path = dir.path().join("env-synctv.yaml");
        std::fs::write(
            &config_path,
            r#"
management:
  transport: "tcp"
  port: 58081
"#,
        )
        .expect("config should be written");
        let _env = EnvVarGuard::set(
            "SYNCTV_CONFIG_PATH",
            config_path.to_string_lossy().to_string(),
        );

        let config = load_config_with_options(&LoadConfigOptions {
            config_path: None,
            data_dir: None,
            load_dotenv: false,
            validate: false,
            verbose: false,
        })
        .expect("SYNCTV_CONFIG_PATH should be honored when CLI path is absent");

        assert_eq!(config.management.port, 58081);
    }

    #[test]
    fn test_load_config_with_options_initializes_default_timezone() {
        let _lock = acquire_process_config_test_lock();
        let _timezone = TimeZoneGuard::capture();
        let dir = tempdir().expect("temp dir should be created");
        let config_path = dir.path().join("timezone-synctv.yaml");
        std::fs::write(
            &config_path,
            r#"
time:
  timezone: "Asia/Shanghai"
jwt:
  secret: "12345678901234567890123456789012"
"#,
        )
        .expect("config should be written");

        let config = load_config_with_options(&LoadConfigOptions {
            config_path: Some(config_path.to_string_lossy().to_string()),
            data_dir: None,
            load_dotenv: false,
            validate: false,
            verbose: false,
        })
        .expect("timezone config should load");

        assert_eq!(config.time.timezone, "Asia/Shanghai");
        assert_eq!(default_timezone_name(), "Asia/Shanghai");
    }

    #[cfg(unix)]
    #[test]
    fn test_load_config_with_options_resolves_default_management_socket_from_cli_data_dir() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().expect("temp dir should be created");
        let data_dir = dir.path().join("state");

        let config = load_config_with_options(&LoadConfigOptions {
            config_path: None,
            data_dir: Some(data_dir.to_string_lossy().to_string()),
            load_dotenv: false,
            validate: false,
            verbose: false,
        })
        .expect("cli data_dir should be applied to default runtime paths");

        assert_eq!(
            std::path::Path::new(&config.management.data_dir),
            data_dir.as_path()
        );
        assert_eq!(
            std::path::Path::new(&config.management.unix_socket_path),
            data_dir.join("run").join("synctv.sock").as_path()
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_load_config_with_options_cli_data_dir_overrides_env_data_dir() {
        let _lock = acquire_process_config_test_lock();
        let dir = tempdir().expect("temp dir should be created");
        let cli_data_dir = dir.path().join("cli-state");
        let _env = EnvVarGuard::set(
            "SYNCTV_DATA_DIR",
            dir.path().join("env-state").display().to_string(),
        );

        let config = load_config_with_options(&LoadConfigOptions {
            config_path: None,
            data_dir: Some(cli_data_dir.to_string_lossy().to_string()),
            load_dotenv: false,
            validate: false,
            verbose: false,
        })
        .expect("cli data_dir should override SYNCTV_DATA_DIR");

        assert_eq!(
            std::path::Path::new(&config.management.data_dir),
            cli_data_dir.as_path()
        );
        assert_eq!(
            std::path::Path::new(&config.management.unix_socket_path),
            cli_data_dir.join("run").join("synctv.sock").as_path()
        );
    }
}
