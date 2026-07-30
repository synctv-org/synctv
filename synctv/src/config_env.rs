use std::collections::HashMap;
use std::path::{Path, PathBuf};

use config::ConfigError;
use synctv_common::time as common_time;

use crate::app_config::AppConfig as Config;
use crate::app_config::*;

use crate::config_loader::{
    default_data_dir, default_management_unix_socket_path, default_proxy_slice_cache_relative_path,
    default_runtime_socket_relative_path, load_config_string_from_file, resolve_relative_path_from,
};

fn apply_redis_url_component_env_overrides(
    config: &mut Config,
    get_env: &impl Fn(&str) -> Option<String>,
) -> Result<(), ConfigError> {
    if config.redis.url.trim().is_empty() {
        return Ok(());
    }

    let host = get_env("SYNCTV_REDIS_HOST");
    let port = get_env("SYNCTV_REDIS_PORT");
    let username = get_env("SYNCTV_REDIS_USERNAME");
    let mut password = get_env("SYNCTV_REDIS_PASSWORD");
    if let Some(path) = get_env("SYNCTV_REDIS_PASSWORD_FILE") {
        password = Some(load_config_string_from_file(
            Path::new("<environment>"),
            &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            "redis.password",
            &path,
        )?);
    }
    let database = get_env("SYNCTV_REDIS_DATABASE");

    if host.is_none()
        && port.is_none()
        && username.is_none()
        && password.is_none()
        && database.is_none()
    {
        return Ok(());
    }

    let mut url = url::Url::parse(&config.redis.url).map_err(|error| {
        ConfigError::Message(format!(
            "Cannot apply Redis environment overrides to redis.url '{}': {error}",
            config.redis.url
        ))
    })?;

    if let Some(host) = host {
        url.set_host(Some(host.trim())).map_err(|_| {
            ConfigError::Message(format!(
                "Invalid value for environment variable SYNCTV_REDIS_HOST: '{host}'"
            ))
        })?;
    }
    if let Some(port) = port {
        let port = port.parse::<u16>().map_err(|error| {
            ConfigError::Message(format!(
                "Invalid value for environment variable SYNCTV_REDIS_PORT: '{port}' ({error})"
            ))
        })?;
        url.set_port(Some(port)).map_err(|()| {
            ConfigError::Message(format!(
                "Cannot apply SYNCTV_REDIS_PORT to redis.url '{}'",
                config.redis.url
            ))
        })?;
    }
    if let Some(username) = username {
        url.set_username(&username).map_err(|()| {
            ConfigError::Message("Cannot apply SYNCTV_REDIS_USERNAME to redis.url".to_string())
        })?;
    }
    if let Some(password) = password {
        url.set_password(Some(&password)).map_err(|()| {
            ConfigError::Message("Cannot apply SYNCTV_REDIS_PASSWORD to redis.url".to_string())
        })?;
    }
    if let Some(database) = database {
        let database = database.parse::<i64>().map_err(|error| {
                ConfigError::Message(format!(
                    "Invalid value for environment variable SYNCTV_REDIS_DATABASE: '{database}' ({error})"
                ))
            })?;
        url.set_path(&format!("/{database}"));
    }

    config.redis.url = url.to_string();
    Ok(())
}

/// Apply environment variable overrides using single-underscore format.
///
/// Format: `SYNCTV_<SECTION>_<FIELD>=<value>`
///
/// Examples:
/// - `SYNCTV_SERVER_HOST=0.0.0.0`
/// - `SYNCTV_DATABASE_URL=postgresql://...`
/// - `SYNCTV_SERVER_ADVERTISE_HOST=10.0.0.1`
pub(crate) fn apply_env_overrides_with(
    config: &mut Config,
    get_env: &impl Fn(&str) -> Option<String>,
) -> Result<(), ConfigError> {
    let env_override_str = |name: &str, target: &mut String| {
        if let Some(val) = get_env(name) {
            *target = val;
        }
    };
    let env_override_str_file =
        |name: &str, key_path: &str, target: &mut String| -> Result<(), ConfigError> {
            if let Some(path) = get_env(name) {
                *target = load_config_string_from_file(
                    Path::new("<environment>"),
                    &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                    key_path,
                    &path,
                )?;
            }
            Ok(())
        };
    let env_override_opt_str = |name: &str, target: &mut Option<String>| {
        if let Some(val) = get_env(name) {
            *target = Some(val);
        }
    };
    let apply_logging_env =
        |service: &str, logging: &mut LoggingConfig| -> Result<(), ConfigError> {
            let prefix = format!("SYNCTV_{service}_LOGGING");
            if let Some(value) = get_env(&format!("{prefix}_LEVEL")) {
                logging.level = value;
            }
            if let Some(value) = get_env(&format!("{prefix}_FORMAT")) {
                logging.format = value;
            }
            if let Some(value) = get_env(&format!("{prefix}_COLOR")) {
                logging.color = serde_json::from_value(serde_json::Value::String(value.clone()))
                    .map_err(|error| {
                        ConfigError::Message(format!("Invalid {prefix}_COLOR '{value}': {error}"))
                    })?;
            }
            if let Some(value) = get_env(&format!("{prefix}_OUTPUT")) {
                logging.output = match value.as_str() {
                    "stdout" => LogOutput::Named(LogOutputName::Stdout),
                    "stderr" => LogOutput::Named(LogOutputName::Stderr),
                    _ => serde_json::from_str(&value).map_err(|error| {
                        ConfigError::Message(format!("Invalid {prefix}_OUTPUT: {error}"))
                    })?,
                };
            }
            if let Some(value) = get_env(&format!("{prefix}_OUTPUT_PATH")) {
                if value.trim().is_empty() {
                    return Err(ConfigError::Message(format!(
                        "{prefix}_OUTPUT_PATH must not be empty"
                    )));
                }
                let rotation = match &logging.output {
                    LogOutput::File(file) => file.rotation.clone(),
                    _ => LogRotation::default(),
                };
                logging.output = LogOutput::File(LogFileOutput {
                    r#type: "file".to_string(),
                    path: value,
                    rotation,
                });
            }
            if let Some(value) = get_env(&format!("{prefix}_OUTPUT_ROTATION_STRATEGY")) {
                let file = ensure_file_log_output(logging);
                file.rotation.strategy = value;
            }
            if let Some(value) = get_env(&format!("{prefix}_OUTPUT_ROTATION_MAX_FILES")) {
                let max_files = value.parse::<usize>().map_err(|error| {
                    ConfigError::Message(format!(
                        "Invalid {prefix}_OUTPUT_ROTATION_MAX_FILES '{value}': {error}"
                    ))
                })?;
                let file = ensure_file_log_output(logging);
                file.rotation.max_files = max_files;
            }
            Ok(())
        };
    let env_override_parse = |name: &str,
                              target: &mut dyn std::any::Any|
     -> Result<(), ConfigError> {
        macro_rules! parse_into {
                    ($ty:ty) => {
                        if let Some(target) = target.downcast_mut::<$ty>() {
                            if let Some(val) = get_env(name) {
                                let parsed = val.parse().map_err(|error| {
                                    ConfigError::Message(format!(
                                        "Invalid value for environment variable {name}: '{val}' ({error})"
                                    ))
                                })?;
                                *target = parsed;
                            }
                            return Ok(());
                        }
                    };
                }
        parse_into!(u16);
        parse_into!(u8);
        parse_into!(u32);
        parse_into!(u64);
        parse_into!(usize);
        parse_into!(i32);
        parse_into!(i64);
        parse_into!(f64);
        Err(ConfigError::Message(format!(
            "Unsupported environment override target type for {name}"
        )))
    };
    let env_override_bool = |name: &str, target: &mut bool| -> Result<(), ConfigError> {
        if let Some(val) = get_env(name) {
            match val.to_lowercase().as_str() {
                "true" | "1" | "yes" => *target = true,
                "false" | "0" | "no" => *target = false,
                _ => {
                    return Err(ConfigError::Message(format!(
                        "Invalid boolean value for environment variable {name}: '{val}'"
                    )));
                }
            }
        }
        Ok(())
    };
    let env_override_enum = |name: &str,
                             apply: &mut dyn FnMut(&str) -> Result<(), ConfigError>|
     -> Result<(), ConfigError> {
        if let Some(val) = get_env(name) {
            apply(&val).map_err(|error| {
                ConfigError::Message(format!(
                    "Invalid value for environment variable {name}: '{val}' ({error})"
                ))
            })?;
        }
        Ok(())
    };
    let env_override_csv = |name: &str, target: &mut Vec<String>| {
        if let Some(val) = get_env(name) {
            *target = val
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    };
    let env_override_json_or_csv = |name: &str, target: &mut Vec<String>| {
        if let Some(val) = get_env(name) {
            if let Ok(parsed) = serde_json::from_str::<Vec<String>>(&val) {
                *target = parsed;
            } else {
                *target = val
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
    };
    let env_override_json =
        |name: &str, target: &mut dyn std::any::Any| -> Result<(), ConfigError> {
            macro_rules! parse_json_into {
            ($ty:ty) => {
                if let Some(target) = target.downcast_mut::<$ty>() {
                    if let Some(val) = get_env(name) {
                        let parsed = serde_json::from_str::<$ty>(&val).map_err(|error| {
                            ConfigError::Message(format!(
                                "Invalid JSON value for environment variable {name}: {error}"
                            ))
                        })?;
                        *target = parsed;
                    }
                    return Ok(());
                }
            };
        }
            parse_json_into!(HashMap<String, FileStorageBackendConfig>);
            parse_json_into!(HashMap<String, RateLimitScopeRule>);
            parse_json_into!(Vec<AndroidAppAssociationConfig>);
            Err(ConfigError::Message(format!(
                "Unsupported environment JSON override target type for {name}"
            )))
        };

    env_override_str("SYNCTV_TIME_TIMEZONE", &mut config.time.timezone);
    env_override_bool(
        "SYNCTV_TIME_CLOCK_SYNC_ENABLED",
        &mut config.time.clock_sync.enabled,
    )?;
    env_override_enum("SYNCTV_TIME_CLOCK_SYNC_PROVIDER_TYPE", &mut |val| {
        config.time.clock_sync.provider = val.parse()?;
        Ok(())
    })?;
    match &mut config.time.clock_sync.provider {
        ClockSyncProvider::Sntp(config) => {
            env_override_json_or_csv(
                "SYNCTV_TIME_CLOCK_SYNC_PROVIDER_SERVERS",
                &mut config.servers,
            );
            env_override_parse(
                "SYNCTV_TIME_CLOCK_SYNC_PROVIDER_INTERVAL_SECONDS",
                &mut config.interval_seconds,
            )?;
            env_override_parse(
                "SYNCTV_TIME_CLOCK_SYNC_PROVIDER_TIMEOUT_MILLIS",
                &mut config.timeout_millis,
            )?;
        }
    }

    env_override_str(
        "SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY",
        &mut config.security.credential_encryption_key,
    );
    env_override_str_file(
        "SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY_FILE",
        "security.credential_encryption_key",
        &mut config.security.credential_encryption_key,
    )?;
    env_override_str(
        "SYNCTV_SECURITY_TOTP_ENCRYPTION_KEY",
        &mut config.security.totp_encryption_key,
    );
    env_override_str_file(
        "SYNCTV_SECURITY_TOTP_ENCRYPTION_KEY_FILE",
        "security.totp_encryption_key",
        &mut config.security.totp_encryption_key,
    )?;
    env_override_str(
        "SYNCTV_SECURITY_EMAIL_OUTBOX_ENCRYPTION_KEY",
        &mut config.security.email_outbox_encryption_key,
    );
    env_override_str_file(
        "SYNCTV_SECURITY_EMAIL_OUTBOX_ENCRYPTION_KEY_FILE",
        "security.email_outbox_encryption_key",
        &mut config.security.email_outbox_encryption_key,
    )?;
    env_override_str(
        "SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET",
        &mut config.security.opaque_server_setup_secret,
    );
    env_override_str_file(
        "SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET_FILE",
        "security.opaque_server_setup_secret",
        &mut config.security.opaque_server_setup_secret,
    )?;
    env_override_str(
        "SYNCTV_SECURITY_PROXY_SIGNING_KEY",
        &mut config.security.proxy_signing_key,
    );
    env_override_str_file(
        "SYNCTV_SECURITY_PROXY_SIGNING_KEY_FILE",
        "security.proxy_signing_key",
        &mut config.security.proxy_signing_key,
    )?;
    env_override_str(
        "SYNCTV_SECURITY_MEDIA_SWARM_SIGNING_KEY",
        &mut config.security.media_swarm_signing_key,
    );
    env_override_str_file(
        "SYNCTV_SECURITY_MEDIA_SWARM_SIGNING_KEY_FILE",
        "security.media_swarm_signing_key",
        &mut config.security.media_swarm_signing_key,
    )?;
    env_override_str(
        "SYNCTV_SECURITY_PROVIDER_SESSION_ENCRYPTION_KEY",
        &mut config.security.provider_session_encryption_key,
    );
    env_override_str_file(
        "SYNCTV_SECURITY_PROVIDER_SESSION_ENCRYPTION_KEY_FILE",
        "security.provider_session_encryption_key",
        &mut config.security.provider_session_encryption_key,
    )?;
    env_override_str(
        "SYNCTV_SECURITY_LOGIN_DISCOVERY_KEY",
        &mut config.security.login_discovery_key,
    );
    env_override_str_file(
        "SYNCTV_SECURITY_LOGIN_DISCOVERY_KEY_FILE",
        "security.login_discovery_key",
        &mut config.security.login_discovery_key,
    )?;
    env_override_str(
        "SYNCTV_SECURITY_WEBAUTHN_ENUMERATION_KEY",
        &mut config.security.webauthn_enumeration_key,
    );
    env_override_str_file(
        "SYNCTV_SECURITY_WEBAUTHN_ENUMERATION_KEY_FILE",
        "security.webauthn_enumeration_key",
        &mut config.security.webauthn_enumeration_key,
    )?;
    env_override_bool(
        "SYNCTV_SECURITY_SSRF_ENABLED",
        &mut config.security.ssrf.enabled,
    )?;
    env_override_bool(
        "SYNCTV_SECURITY_SSRF_ALLOW_PRIVATE_NETWORK_TARGETS",
        &mut config.security.ssrf.allow_private_network_targets,
    )?;
    env_override_json_or_csv(
        "SYNCTV_SECURITY_SSRF_ALLOWED_HOSTS",
        &mut config.security.ssrf.allowed_hosts,
    );
    env_override_json_or_csv(
        "SYNCTV_SECURITY_SSRF_ALLOWED_IP_RANGES",
        &mut config.security.ssrf.allowed_ip_ranges,
    );

    env_override_str("SYNCTV_DATA_DIR", &mut config.data_dir);

    env_override_str("SYNCTV_SERVER_HOST", &mut config.server.host);
    env_override_parse("SYNCTV_SERVER_PORT", &mut config.server.port)?;
    env_override_bool(
        "SYNCTV_SERVER_ENABLE_REFLECTION",
        &mut config.server.enable_reflection,
    )?;
    env_override_csv(
        "SYNCTV_SERVER_TRUSTED_PROXIES",
        &mut config.server.trusted_proxies,
    );
    env_override_json_or_csv(
        "SYNCTV_SERVER_CORS_ALLOWED_ORIGINS",
        &mut config.server.cors_allowed_origins,
    );
    env_override_str("SYNCTV_CLUSTER_SECRET", &mut config.cluster.secret);
    env_override_str_file(
        "SYNCTV_CLUSTER_SECRET_FILE",
        "cluster.secret",
        &mut config.cluster.secret,
    )?;
    env_override_str(
        "SYNCTV_SERVER_ADVERTISE_HOST",
        &mut config.server.advertise_host,
    );
    if config.server.advertise_host.trim().is_empty() {
        env_override_str("POD_IP", &mut config.server.advertise_host);
    }
    env_override_parse(
        "SYNCTV_SERVER_SHUTDOWN_DRAIN_TIMEOUT_SECONDS",
        &mut config.server.shutdown_drain_timeout_seconds,
    )?;
    env_override_parse(
        "SYNCTV_SERVER_GRPC_MAX_MESSAGE_SIZE_BYTES",
        &mut config.server.grpc_max_message_size_bytes,
    )?;
    env_override_bool(
        "SYNCTV_SERVER_GRPC_COMPRESSION_ENABLED",
        &mut config.server.grpc_compression_enabled,
    )?;

    env_override_bool("SYNCTV_HEALTH_ENABLED", &mut config.health.enabled)?;
    env_override_str("SYNCTV_HEALTH_HOST", &mut config.health.host);
    env_override_parse("SYNCTV_HEALTH_PORT", &mut config.health.port)?;

    env_override_bool("SYNCTV_METRICS_ENABLED", &mut config.metrics.enabled)?;
    env_override_str("SYNCTV_METRICS_HOST", &mut config.metrics.host);
    env_override_parse("SYNCTV_METRICS_PORT", &mut config.metrics.port)?;
    env_override_bool(
        "SYNCTV_METRICS_TLS_ENABLED",
        &mut config.metrics.tls.enabled,
    )?;
    env_override_str(
        "SYNCTV_METRICS_TLS_CERT_PATH",
        &mut config.metrics.tls.cert_path,
    );
    env_override_str(
        "SYNCTV_METRICS_TLS_KEY_PATH",
        &mut config.metrics.tls.key_path,
    );
    env_override_enum("SYNCTV_METRICS_AUTH_MODE", &mut |val| {
        config.metrics.auth.mode = val.parse()?;
        Ok(())
    })?;
    env_override_str(
        "SYNCTV_METRICS_AUTH_BEARER_TOKEN",
        &mut config.metrics.auth.bearer_token,
    );
    env_override_str_file(
        "SYNCTV_METRICS_AUTH_BEARER_TOKEN_FILE",
        "metrics.auth.bearer_token",
        &mut config.metrics.auth.bearer_token,
    )?;
    env_override_str(
        "SYNCTV_METRICS_AUTH_BASIC_USERNAME",
        &mut config.metrics.auth.basic_username,
    );
    env_override_str(
        "SYNCTV_METRICS_AUTH_BASIC_PASSWORD",
        &mut config.metrics.auth.basic_password,
    );
    env_override_str_file(
        "SYNCTV_METRICS_AUTH_BASIC_PASSWORD_FILE",
        "metrics.auth.basic_password",
        &mut config.metrics.auth.basic_password,
    )?;
    env_override_str(
        "SYNCTV_METRICS_AUTH_KUBERNETES_AUDIENCE",
        &mut config.metrics.auth.kubernetes.audience,
    );
    env_override_parse(
        "SYNCTV_METRICS_AUTH_KUBERNETES_AUTHENTICATION_CACHE_TTL_SECONDS",
        &mut config
            .metrics
            .auth
            .kubernetes
            .authentication_cache_ttl_seconds,
    )?;
    env_override_parse(
        "SYNCTV_METRICS_AUTH_KUBERNETES_AUTHORIZATION_CACHE_TTL_SECONDS",
        &mut config
            .metrics
            .auth
            .kubernetes
            .authorization_cache_ttl_seconds,
    )?;

    env_override_bool("SYNCTV_MANAGEMENT_ENABLED", &mut config.management.enabled)?;
    env_override_enum("SYNCTV_MANAGEMENT_TRANSPORT", &mut |val| {
        config.management.transport = val.parse()?;
        Ok(())
    })?;
    env_override_parse("SYNCTV_MANAGEMENT_PORT", &mut config.management.port)?;
    env_override_str(
        "SYNCTV_MANAGEMENT_UNIX_SOCKET_PATH",
        &mut config.management.unix_socket_path,
    );
    env_override_str(
        "SYNCTV_MANAGEMENT_AUTH_TOKEN",
        &mut config.management.auth_token,
    );
    env_override_str_file(
        "SYNCTV_MANAGEMENT_AUTH_TOKEN_FILE",
        "management.auth_token",
        &mut config.management.auth_token,
    )?;
    env_override_bool(
        "SYNCTV_MANAGEMENT_ENABLE_REFLECTION",
        &mut config.management.enable_reflection,
    )?;

    let database_url_from_env =
        get_env("SYNCTV_DATABASE_URL").is_some() || get_env("SYNCTV_DATABASE_URL_FILE").is_some();
    let database_split_from_env = [
        "SYNCTV_DATABASE_HOST",
        "SYNCTV_DATABASE_PORT",
        "SYNCTV_DATABASE_USERNAME",
        "SYNCTV_DATABASE_PASSWORD",
        "SYNCTV_DATABASE_PASSWORD_FILE",
        "SYNCTV_DATABASE_NAME",
    ]
    .iter()
    .any(|name| get_env(name).is_some());
    if database_split_from_env && !database_url_from_env {
        config.database.url.clear();
    }

    env_override_str("SYNCTV_DATABASE_URL", &mut config.database.url);
    env_override_str_file(
        "SYNCTV_DATABASE_URL_FILE",
        "database.url",
        &mut config.database.url,
    )?;
    env_override_str("SYNCTV_DATABASE_READ_URL", &mut config.database.read_url);
    env_override_str_file(
        "SYNCTV_DATABASE_READ_URL_FILE",
        "database.read_url",
        &mut config.database.read_url,
    )?;
    env_override_str("SYNCTV_DATABASE_READ_HOST", &mut config.database.read_host);
    env_override_parse("SYNCTV_DATABASE_READ_PORT", &mut config.database.read_port)?;
    env_override_str("SYNCTV_DATABASE_HOST", &mut config.database.host);
    env_override_parse("SYNCTV_DATABASE_PORT", &mut config.database.port)?;
    env_override_str("SYNCTV_DATABASE_USERNAME", &mut config.database.username);
    env_override_str("SYNCTV_DATABASE_PASSWORD", &mut config.database.password);
    env_override_str_file(
        "SYNCTV_DATABASE_PASSWORD_FILE",
        "database.password",
        &mut config.database.password,
    )?;
    env_override_str("SYNCTV_DATABASE_NAME", &mut config.database.name);
    env_override_parse(
        "SYNCTV_DATABASE_MAX_CONNECTIONS",
        &mut config.database.max_connections,
    )?;
    env_override_parse(
        "SYNCTV_DATABASE_MIN_CONNECTIONS",
        &mut config.database.min_connections,
    )?;
    env_override_parse(
        "SYNCTV_DATABASE_CONNECT_TIMEOUT_SECONDS",
        &mut config.database.connect_timeout_seconds,
    )?;
    env_override_parse(
        "SYNCTV_DATABASE_IDLE_TIMEOUT_SECONDS",
        &mut config.database.idle_timeout_seconds,
    )?;
    env_override_parse(
        "SYNCTV_DATABASE_MAX_LIFETIME_SECONDS",
        &mut config.database.max_lifetime_seconds,
    )?;

    let redis_url_from_env =
        get_env("SYNCTV_REDIS_URL").is_some() || get_env("SYNCTV_REDIS_URL_FILE").is_some();

    env_override_str("SYNCTV_REDIS_URL", &mut config.redis.url);
    env_override_str_file("SYNCTV_REDIS_URL_FILE", "redis.url", &mut config.redis.url)?;
    if config.redis.url.trim().is_empty() || redis_url_from_env {
        env_override_str("SYNCTV_REDIS_HOST", &mut config.redis.host);
        env_override_parse("SYNCTV_REDIS_PORT", &mut config.redis.port)?;
        env_override_str("SYNCTV_REDIS_USERNAME", &mut config.redis.username);
        env_override_str("SYNCTV_REDIS_PASSWORD", &mut config.redis.password);
        env_override_str_file(
            "SYNCTV_REDIS_PASSWORD_FILE",
            "redis.password",
            &mut config.redis.password,
        )?;
        env_override_parse("SYNCTV_REDIS_DATABASE", &mut config.redis.database)?;
    } else {
        apply_redis_url_component_env_overrides(config, get_env)?;
    }
    env_override_parse(
        "SYNCTV_REDIS_CONNECT_TIMEOUT_SECONDS",
        &mut config.redis.connect_timeout_seconds,
    )?;
    env_override_parse(
        "SYNCTV_REDIS_RESPONSE_TIMEOUT_SECONDS",
        &mut config.redis.response_timeout_seconds,
    )?;
    env_override_parse(
        "SYNCTV_REDIS_PIPELINE_BUFFER_SIZE",
        &mut config.redis.pipeline_buffer_size,
    )?;
    env_override_str("SYNCTV_REDIS_KEY_PREFIX", &mut config.redis.key_prefix);
    env_override_enum("SYNCTV_REDIS_DEPLOYMENT_MODE", &mut |val| {
        match val.to_lowercase().as_str() {
            "standalone" => config.redis.deployment_mode = RedisDeploymentMode::Standalone,
            "sentinel" => config.redis.deployment_mode = RedisDeploymentMode::Sentinel,
            _ => {
                return Err(ConfigError::Message(format!(
                        "Invalid value for environment variable SYNCTV_REDIS_DEPLOYMENT_MODE: '{val}' (expected one of: standalone, sentinel)"
                    )));
            }
        }
        Ok(())
    })?;
    env_override_opt_str(
        "SYNCTV_REDIS_SENTINEL_MASTER_NAME",
        &mut config.redis.sentinel_master_name,
    );
    env_override_csv(
        "SYNCTV_REDIS_SENTINEL_ADDRESSES",
        &mut config.redis.sentinel_addresses,
    );

    env_override_str("SYNCTV_JWT_SECRET", &mut config.jwt.secret);
    env_override_str_file(
        "SYNCTV_JWT_SECRET_FILE",
        "jwt.secret",
        &mut config.jwt.secret,
    )?;
    env_override_parse(
        "SYNCTV_JWT_ACCESS_TOKEN_DURATION_HOURS",
        &mut config.jwt.access_token_duration_hours,
    )?;
    env_override_parse(
        "SYNCTV_JWT_REFRESH_TOKEN_DURATION_DAYS",
        &mut config.jwt.refresh_token_duration_days,
    )?;
    env_override_parse(
        "SYNCTV_JWT_GUEST_TOKEN_DURATION_HOURS",
        &mut config.jwt.guest_token_duration_hours,
    )?;
    env_override_parse(
        "SYNCTV_JWT_CLOCK_SKEW_LEEWAY_SECS",
        &mut config.jwt.clock_skew_leeway_secs,
    )?;

    env_override_bool("SYNCTV_WEBAUTHN_ENABLED", &mut config.webauthn.enabled)?;
    env_override_str("SYNCTV_WEBAUTHN_RP_ID", &mut config.webauthn.rp_id);
    env_override_str("SYNCTV_WEBAUTHN_RP_ORIGIN", &mut config.webauthn.rp_origin);
    env_override_str("SYNCTV_WEBAUTHN_RP_NAME", &mut config.webauthn.rp_name);
    env_override_json_or_csv(
        "SYNCTV_WEBAUTHN_ALLOWED_ORIGINS",
        &mut config.webauthn.allowed_origins,
    );
    env_override_json_or_csv(
        "SYNCTV_WEBAUTHN_APPLE_APP_IDS",
        &mut config.webauthn.apple_app_ids,
    );
    env_override_json(
        "SYNCTV_WEBAUTHN_ANDROID_APPS",
        &mut config.webauthn.android_apps,
    )?;
    env_override_bool(
        "SYNCTV_WEBAUTHN_ALLOW_SUBDOMAINS",
        &mut config.webauthn.allow_subdomains,
    )?;
    env_override_bool(
        "SYNCTV_WEBAUTHN_ALLOW_ANY_PORT",
        &mut config.webauthn.allow_any_port,
    )?;
    env_override_parse(
        "SYNCTV_WEBAUTHN_TIMEOUT_SECONDS",
        &mut config.webauthn.timeout_seconds,
    )?;

    apply_logging_env("SERVER", &mut config.server.logging)?;
    apply_logging_env("HEALTH", &mut config.health.logging)?;
    apply_logging_env("METRICS", &mut config.metrics.logging)?;
    apply_logging_env("CLUSTER", &mut config.cluster.logging)?;
    apply_logging_env("MANAGEMENT", &mut config.management.logging)?;

    env_override_parse(
        "SYNCTV_LIVESTREAM_RTMP_PORT",
        &mut config.livestream.rtmp_port,
    )?;
    env_override_str(
        "SYNCTV_LIVESTREAM_PUBLIC_RTMP_HOST",
        &mut config.livestream.public_rtmp_host,
    );
    env_override_parse(
        "SYNCTV_LIVESTREAM_GOP_CACHE_SIZE",
        &mut config.livestream.gop_cache_size,
    )?;
    env_override_parse(
        "SYNCTV_LIVESTREAM_STREAM_TIMEOUT_SECONDS",
        &mut config.livestream.stream_timeout_seconds,
    )?;
    env_override_parse(
        "SYNCTV_LIVESTREAM_CLEANUP_CHECK_INTERVAL_SECONDS",
        &mut config.livestream.cleanup_check_interval_seconds,
    )?;
    env_override_parse(
        "SYNCTV_LIVESTREAM_PULL_MAX_RETRIES",
        &mut config.livestream.pull_max_retries,
    )?;
    env_override_parse(
        "SYNCTV_LIVESTREAM_PULL_INITIAL_BACKOFF_MS",
        &mut config.livestream.pull_initial_backoff_ms,
    )?;
    env_override_parse(
        "SYNCTV_LIVESTREAM_PULL_MAX_BACKOFF_MS",
        &mut config.livestream.pull_max_backoff_ms,
    )?;
    env_override_parse(
        "SYNCTV_LIVESTREAM_MAX_FLV_TAG_SIZE_BYTES",
        &mut config.livestream.max_flv_tag_size_bytes,
    )?;
    env_override_parse(
        "SYNCTV_LIVESTREAM_GOP_CACHE_MAX_MEMORY_MB",
        &mut config.livestream.gop_cache_max_memory_mb,
    )?;
    env_override_enum("SYNCTV_LIVESTREAM_HLS_STORAGE_TYPE", &mut |val| {
        let backend = val.parse()?;
        config.livestream.hls_storage = config
            .livestream
            .hls_storage
            .clone()
            .select_backend(backend);
        Ok(())
    })?;
    match &mut config.livestream.hls_storage {
        HlsStorageConfig::Memory(config) => {
            env_override_parse(
                "SYNCTV_LIVESTREAM_HLS_STORAGE_MEMORY_MAX_MB",
                &mut config.memory_max_mb,
            )?;
        }
        HlsStorageConfig::File(config) | HlsStorageConfig::SharedFile(config) => {
            env_override_str("SYNCTV_LIVESTREAM_HLS_STORAGE_PATH", &mut config.path);
        }
        HlsStorageConfig::Oss(config) => {
            env_override_str(
                "SYNCTV_LIVESTREAM_HLS_STORAGE_ENDPOINT",
                &mut config.endpoint,
            );
            env_override_str(
                "SYNCTV_LIVESTREAM_HLS_STORAGE_ACCESS_KEY_ID",
                &mut config.access_key_id,
            );
            env_override_str_file(
                "SYNCTV_LIVESTREAM_HLS_STORAGE_ACCESS_KEY_ID_FILE",
                "livestream.hls_storage.access_key_id",
                &mut config.access_key_id,
            )?;
            env_override_str(
                "SYNCTV_LIVESTREAM_HLS_STORAGE_SECRET_ACCESS_KEY",
                &mut config.secret_access_key,
            );
            env_override_str_file(
                "SYNCTV_LIVESTREAM_HLS_STORAGE_SECRET_ACCESS_KEY_FILE",
                "livestream.hls_storage.secret_access_key",
                &mut config.secret_access_key,
            )?;
            env_override_str("SYNCTV_LIVESTREAM_HLS_STORAGE_BUCKET", &mut config.bucket);
            env_override_opt_str("SYNCTV_LIVESTREAM_HLS_STORAGE_REGION", &mut config.region);
            env_override_str(
                "SYNCTV_LIVESTREAM_HLS_STORAGE_BASE_PATH",
                &mut config.base_path,
            );
        }
    }

    env_override_str(
        "SYNCTV_FILE_UPLOAD_TOKEN_SECRET",
        &mut config.file_storage.upload_token_secret,
    );
    env_override_str_file(
        "SYNCTV_FILE_UPLOAD_TOKEN_SECRET_FILE",
        "file_storage.upload_token_secret",
        &mut config.file_storage.upload_token_secret,
    )?;
    env_override_str(
        "SYNCTV_FILE_STORAGE_DEFAULT_BACKEND",
        &mut config.file_storage.default_backend,
    );
    env_override_str(
        "SYNCTV_FILE_STORAGE_CHAT_ATTACHMENTS_BACKEND",
        &mut config.file_storage.chat_attachments_backend,
    );
    env_override_str(
        "SYNCTV_FILE_STORAGE_USER_AVATARS_BACKEND",
        &mut config.file_storage.user_avatars_backend,
    );
    env_override_str(
        "SYNCTV_FILE_STORAGE_MEDIA_COVERS_BACKEND",
        &mut config.file_storage.media_covers_backend,
    );
    env_override_str(
        "SYNCTV_FILE_STORAGE_ROOM_COVERS_BACKEND",
        &mut config.file_storage.room_covers_backend,
    );
    env_override_str(
        "SYNCTV_FILE_STORAGE_PLAYLIST_COVERS_BACKEND",
        &mut config.file_storage.playlist_covers_backend,
    );
    env_override_parse(
        "SYNCTV_FILE_STORAGE_UNREFERENCED_OBJECT_RETENTION_SECONDS",
        &mut config.file_storage.unreferenced_object_retention_seconds,
    )?;
    env_override_json(
        "SYNCTV_FILE_STORAGE_BACKENDS",
        &mut config.file_storage.backends,
    )?;

    env_override_parse(
        "SYNCTV_LIVESTREAM_FLV_MAX_CONNECTION_DURATION_SECONDS",
        &mut config.livestream.flv_max_connection_duration_seconds,
    )?;
    env_override_parse(
        "SYNCTV_LIVESTREAM_FLV_WRITE_TIMEOUT_SECONDS",
        &mut config.livestream.flv_write_timeout_seconds,
    )?;

    env_override_parse(
        "SYNCTV_MEDIA_PROVIDERS_ALIST_REQUEST_TIMEOUT_SECONDS",
        &mut config.media_providers.alist.request_timeout_seconds,
    )?;
    env_override_parse(
        "SYNCTV_MEDIA_PROVIDERS_ALIST_CONNECT_TIMEOUT_SECONDS",
        &mut config.media_providers.alist.connect_timeout_seconds,
    )?;
    env_override_parse(
        "SYNCTV_MEDIA_PROVIDERS_BILIBILI_REQUEST_TIMEOUT_SECONDS",
        &mut config.media_providers.bilibili.request_timeout_seconds,
    )?;
    env_override_parse(
        "SYNCTV_MEDIA_PROVIDERS_BILIBILI_CONNECT_TIMEOUT_SECONDS",
        &mut config.media_providers.bilibili.connect_timeout_seconds,
    )?;
    env_override_parse(
        "SYNCTV_MEDIA_PROVIDERS_EMBY_REQUEST_TIMEOUT_SECONDS",
        &mut config.media_providers.emby.request_timeout_seconds,
    )?;
    env_override_parse(
        "SYNCTV_MEDIA_PROVIDERS_EMBY_CONNECT_TIMEOUT_SECONDS",
        &mut config.media_providers.emby.connect_timeout_seconds,
    )?;
    env_override_parse(
        "SYNCTV_MEDIA_PROVIDERS_CLOUDREVE_REQUEST_TIMEOUT_SECONDS",
        &mut config.media_providers.cloudreve.request_timeout_seconds,
    )?;
    env_override_parse(
        "SYNCTV_MEDIA_PROVIDERS_CLOUDREVE_CONNECT_TIMEOUT_SECONDS",
        &mut config.media_providers.cloudreve.connect_timeout_seconds,
    )?;

    env_override_enum("SYNCTV_WEBRTC_MODE", &mut |val| {
        match val.to_lowercase().as_str() {
            "signaling_only" => config.webrtc.mode = WebRTCMode::SignalingOnly,
            "peer_to_peer" => config.webrtc.mode = WebRTCMode::PeerToPeer,
            _ => {
                return Err(ConfigError::Message(format!(
                        "Invalid value for environment variable SYNCTV_WEBRTC_MODE: '{val}' (expected one of: signaling_only, peer_to_peer)"
                    )));
            }
        }
        Ok(())
    })?;
    env_override_bool(
        "SYNCTV_WEBRTC_ENABLE_BUILTIN_STUN",
        &mut config.webrtc.enable_builtin_stun,
    )?;
    env_override_parse("SYNCTV_WEBRTC_STUN_PORT", &mut config.webrtc.stun_port)?;
    env_override_str("SYNCTV_WEBRTC_STUN_HOST", &mut config.webrtc.stun_host);
    env_override_str(
        "SYNCTV_WEBRTC_STUN_EXTERNAL_ADDR",
        &mut config.webrtc.stun_external_addr,
    );
    env_override_bool(
        "SYNCTV_WEBRTC_FILTER_PRIVATE_ICE_CANDIDATES",
        &mut config.webrtc.filter_private_ice_candidates,
    )?;

    env_override_parse(
        "SYNCTV_CONNECTION_LIMITS_MAX_PER_USER",
        &mut config.connection_limits.max_per_user,
    )?;
    env_override_parse(
        "SYNCTV_CONNECTION_LIMITS_MAX_PER_ROOM",
        &mut config.connection_limits.max_per_room,
    )?;
    env_override_parse(
        "SYNCTV_CONNECTION_LIMITS_MAX_TOTAL",
        &mut config.connection_limits.max_total,
    )?;
    env_override_parse(
        "SYNCTV_CONNECTION_LIMITS_IDLE_TIMEOUT_SECONDS",
        &mut config.connection_limits.idle_timeout_seconds,
    )?;
    env_override_parse(
        "SYNCTV_CONNECTION_LIMITS_MAX_DURATION_SECONDS",
        &mut config.connection_limits.max_duration_seconds,
    )?;
    env_override_parse(
        "SYNCTV_CONNECTION_LIMITS_WS_MESSAGE_RATE_LIMIT_PER_SECOND",
        &mut config.connection_limits.ws_message_rate_limit_per_second,
    )?;

    env_override_parse(
        "SYNCTV_MESSAGING_RATE_LIMITS_CHAT_PER_SECOND",
        &mut config.messaging_rate_limits.chat_per_second,
    )?;
    env_override_parse(
        "SYNCTV_MESSAGING_RATE_LIMITS_WINDOW_SECONDS",
        &mut config.messaging_rate_limits.window_seconds,
    )?;

    env_override_bool(
        "SYNCTV_BOOTSTRAP_CREATE_ROOT_USER",
        &mut config.bootstrap.create_root_user,
    )?;
    env_override_str(
        "SYNCTV_BOOTSTRAP_ROOT_USERNAME",
        &mut config.bootstrap.root_username,
    );
    env_override_str(
        "SYNCTV_BOOTSTRAP_ROOT_PASSWORD",
        &mut config.bootstrap.root_password,
    );
    env_override_str_file(
        "SYNCTV_BOOTSTRAP_ROOT_PASSWORD_FILE",
        "bootstrap.root_password",
        &mut config.bootstrap.root_password,
    )?;

    env_override_bool("SYNCTV_CLUSTER_ENABLED", &mut config.cluster.enabled)?;
    env_override_str("SYNCTV_CLUSTER_HOST", &mut config.cluster.host);
    env_override_parse("SYNCTV_CLUSTER_PORT", &mut config.cluster.port)?;
    env_override_str(
        "SYNCTV_CLUSTER_ADVERTISE_HOST",
        &mut config.cluster.advertise_host,
    );
    env_override_parse(
        "SYNCTV_CLUSTER_ADVERTISE_PORT",
        &mut config.cluster.advertise_port,
    )?;
    env_override_parse(
        "SYNCTV_CLUSTER_CRITICAL_CHANNEL_CAPACITY",
        &mut config.cluster.critical_channel_capacity,
    )?;
    env_override_parse(
        "SYNCTV_CLUSTER_PUBLISH_CHANNEL_CAPACITY",
        &mut config.cluster.publish_channel_capacity,
    )?;
    env_override_enum("SYNCTV_CLUSTER_DISCOVERY_MODE", &mut |val| {
        config.cluster.discovery_mode = val.parse().map_err(|error| {
                ConfigError::Message(format!(
                    "Invalid value for environment variable SYNCTV_CLUSTER_DISCOVERY_MODE: '{val}' ({error})"
                ))
            })?;
        Ok(())
    })?;
    env_override_enum("SYNCTV_CLUSTER_LEADER_ELECTION_MODE", &mut |val| {
        config.cluster.leader_election_mode = val.parse().map_err(|error| {
                ConfigError::Message(format!(
                    "Invalid value for environment variable SYNCTV_CLUSTER_LEADER_ELECTION_MODE: '{val}' ({error})"
                ))
            })?;
        Ok(())
    })?;
    env_override_csv("SYNCTV_CLUSTER_PEERS", &mut config.cluster.peers);
    env_override_parse(
        "SYNCTV_CLUSTER_CATCHUP_WINDOW_SECS",
        &mut config.cluster.catchup_window_secs,
    )?;
    env_override_parse(
        "SYNCTV_CLUSTER_STREAM_MAX_LENGTH",
        &mut config.cluster.stream_max_length,
    )?;

    env_override_parse(
        "SYNCTV_PASSWORD_COMPLEXITY_MIN_LENGTH",
        &mut config.password_complexity.min_length,
    )?;
    env_override_bool(
        "SYNCTV_PASSWORD_COMPLEXITY_REQUIRE_UPPERCASE",
        &mut config.password_complexity.require_uppercase,
    )?;
    env_override_bool(
        "SYNCTV_PASSWORD_COMPLEXITY_REQUIRE_LOWERCASE",
        &mut config.password_complexity.require_lowercase,
    )?;
    env_override_bool(
        "SYNCTV_PASSWORD_COMPLEXITY_REQUIRE_DIGIT",
        &mut config.password_complexity.require_digit,
    )?;
    env_override_bool(
        "SYNCTV_PASSWORD_COMPLEXITY_REQUIRE_SPECIAL",
        &mut config.password_complexity.require_special,
    )?;
    env_override_parse(
        "SYNCTV_PASSWORD_COMPLEXITY_MAX_REPEATED_CHARS",
        &mut config.password_complexity.max_repeated_chars,
    )?;
    env_override_bool(
        "SYNCTV_PASSWORD_COMPLEXITY_ZXCVBN_ENABLED",
        &mut config.password_complexity.zxcvbn_enabled,
    )?;
    env_override_parse(
        "SYNCTV_PASSWORD_COMPLEXITY_ZXCVBN_MIN_SCORE",
        &mut config.password_complexity.zxcvbn_min_score,
    )?;

    env_override_parse(
        "SYNCTV_BUFFER_SIZES_WEBSOCKET_OUTBOUND",
        &mut config.buffer_sizes.websocket_outbound,
    )?;
    env_override_parse(
        "SYNCTV_BUFFER_SIZES_AUDIT_BUFFER",
        &mut config.buffer_sizes.audit_buffer,
    )?;

    env_override_parse("SYNCTV_CACHE_L1_CAPACITY", &mut config.cache.l1_capacity)?;
    env_override_parse(
        "SYNCTV_CACHE_L1_TTL_SECONDS",
        &mut config.cache.l1_ttl_seconds,
    )?;
    env_override_parse(
        "SYNCTV_CACHE_L2_TTL_SECONDS",
        &mut config.cache.l2_ttl_seconds,
    )?;
    env_override_parse(
        "SYNCTV_CACHE_USERNAME_CACHE_CAPACITY",
        &mut config.cache.username_cache_capacity,
    )?;
    env_override_parse(
        "SYNCTV_CACHE_USERNAME_CACHE_TTL_SECONDS",
        &mut config.cache.username_cache_ttl_seconds,
    )?;
    env_override_bool(
        "SYNCTV_PROXY_SLICE_CACHE_ENABLED",
        &mut config.proxy_slice_cache.enabled,
    )?;
    env_override_parse(
        "SYNCTV_PROXY_SLICE_CACHE_SLICE_SIZE_BYTES",
        &mut config.proxy_slice_cache.slice_size_bytes,
    )?;
    env_override_parse(
        "SYNCTV_PROXY_SLICE_CACHE_MAX_CACHE_SIZE_BYTES",
        &mut config.proxy_slice_cache.max_cache_size_bytes,
    )?;
    env_override_parse(
        "SYNCTV_PROXY_SLICE_CACHE_SEGMENT_TTL_SECONDS",
        &mut config.proxy_slice_cache.segment_ttl_seconds,
    )?;
    env_override_parse(
        "SYNCTV_PROXY_SLICE_CACHE_STALE_MAX_AGE_SECONDS",
        &mut config.proxy_slice_cache.stale_max_age_seconds,
    )?;
    env_override_bool(
        "SYNCTV_PROXY_SLICE_CACHE_STALE_WHILE_REVALIDATE",
        &mut config.proxy_slice_cache.stale_while_revalidate,
    )?;
    env_override_bool(
        "SYNCTV_PROXY_SLICE_CACHE_FILE_BACKEND_ENABLED",
        &mut config.proxy_slice_cache.file_backend_enabled,
    )?;
    env_override_str(
        "SYNCTV_PROXY_SLICE_CACHE_FILE_CACHE_DIR",
        &mut config.proxy_slice_cache.file_cache_dir,
    );
    env_override_parse(
        "SYNCTV_PROXY_SLICE_CACHE_EVICTION_INTERVAL_SECONDS",
        &mut config.proxy_slice_cache.eviction_interval_seconds,
    )?;
    env_override_parse(
        "SYNCTV_PROXY_SLICE_CACHE_WATERMARK_RATIO",
        &mut config.proxy_slice_cache.watermark_ratio,
    )?;

    env_override_parse(
        "SYNCTV_REQUEST_RATE_LIMITS_AUTH_MAX_REQUESTS",
        &mut config.request_rate_limits.auth_max_requests,
    )?;
    env_override_parse(
        "SYNCTV_REQUEST_RATE_LIMITS_AUTH_WINDOW_SECONDS",
        &mut config.request_rate_limits.auth_window_seconds,
    )?;
    env_override_parse(
        "SYNCTV_REQUEST_RATE_LIMITS_WRITE_MAX_REQUESTS",
        &mut config.request_rate_limits.write_max_requests,
    )?;
    env_override_parse(
        "SYNCTV_REQUEST_RATE_LIMITS_WRITE_WINDOW_SECONDS",
        &mut config.request_rate_limits.write_window_seconds,
    )?;
    env_override_parse(
        "SYNCTV_REQUEST_RATE_LIMITS_READ_MAX_REQUESTS",
        &mut config.request_rate_limits.read_max_requests,
    )?;
    env_override_parse(
        "SYNCTV_REQUEST_RATE_LIMITS_READ_WINDOW_SECONDS",
        &mut config.request_rate_limits.read_window_seconds,
    )?;
    env_override_parse(
        "SYNCTV_REQUEST_RATE_LIMITS_MEDIA_MAX_REQUESTS",
        &mut config.request_rate_limits.media_max_requests,
    )?;
    env_override_parse(
        "SYNCTV_REQUEST_RATE_LIMITS_MEDIA_WINDOW_SECONDS",
        &mut config.request_rate_limits.media_window_seconds,
    )?;
    env_override_parse(
        "SYNCTV_REQUEST_RATE_LIMITS_ADMIN_MAX_REQUESTS",
        &mut config.request_rate_limits.admin_max_requests,
    )?;
    env_override_parse(
        "SYNCTV_REQUEST_RATE_LIMITS_ADMIN_WINDOW_SECONDS",
        &mut config.request_rate_limits.admin_window_seconds,
    )?;
    env_override_parse(
        "SYNCTV_REQUEST_RATE_LIMITS_STREAMING_MAX_REQUESTS",
        &mut config.request_rate_limits.streaming_max_requests,
    )?;
    env_override_parse(
        "SYNCTV_REQUEST_RATE_LIMITS_STREAMING_WINDOW_SECONDS",
        &mut config.request_rate_limits.streaming_window_seconds,
    )?;
    env_override_parse(
        "SYNCTV_REQUEST_RATE_LIMITS_WEBSOCKET_MAX_REQUESTS",
        &mut config.request_rate_limits.websocket_max_requests,
    )?;
    env_override_parse(
        "SYNCTV_REQUEST_RATE_LIMITS_WEBSOCKET_WINDOW_SECONDS",
        &mut config.request_rate_limits.websocket_window_seconds,
    )?;
    env_override_json(
        "SYNCTV_REQUEST_RATE_LIMITS_SCOPES",
        &mut config.request_rate_limits.scopes,
    )?;

    Ok(())
}

fn ensure_file_log_output(logging: &mut LoggingConfig) -> &mut LogFileOutput {
    if !matches!(logging.output, LogOutput::File(_)) {
        logging.output = LogOutput::File(LogFileOutput::default());
    }
    let LogOutput::File(file) = &mut logging.output else {
        unreachable!("logging output was initialized as a file")
    };
    file
}

pub(crate) fn resolve_owned_local_paths(
    config: &mut Config,
    config_file: Option<&Path>,
    data_dir_from_env: bool,
    data_dir_override: Option<&str>,
) {
    let config_base_dir = config_file.and_then(Path::parent).map_or_else(
        || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        Path::to_path_buf,
    );
    let runtime_base_dir = if data_dir_override.is_some() || data_dir_from_env {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        config_base_dir.clone()
    };

    let data_dir = data_dir_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| resolve_relative_path_from(value, &runtime_base_dir))
        .or_else(|| {
            let configured = config.data_dir.trim();
            if configured.is_empty() {
                None
            } else if Path::new(configured).is_absolute() {
                Some(PathBuf::from(configured))
            } else {
                Some(resolve_relative_path_from(configured, &runtime_base_dir))
            }
        })
        .unwrap_or_else(default_data_dir);
    config.data_dir = data_dir.display().to_string();

    let default_socket_path = default_management_unix_socket_path();
    let socket_path = config.management.unix_socket_path.trim();
    let socket_uses_default =
        socket_path.is_empty() || Path::new(socket_path) == default_socket_path;
    config.management.unix_socket_path = if socket_uses_default {
        data_dir
            .join(default_runtime_socket_relative_path())
            .display()
            .to_string()
    } else {
        resolve_relative_path_from(socket_path, &data_dir)
            .display()
            .to_string()
    };

    for logging in [
        &mut config.server.logging,
        &mut config.health.logging,
        &mut config.metrics.logging,
        &mut config.cluster.logging,
        &mut config.management.logging,
    ] {
        if let LogOutput::File(output) = &mut logging.output {
            let path = output.path.trim();
            if path.is_empty() {
                output.path.clear();
                continue;
            }
            output.path = resolve_relative_path_from(path, &data_dir)
                .display()
                .to_string();
        }
    }

    let cert_path = config.metrics.tls.cert_path.trim();
    config.metrics.tls.cert_path = if cert_path.is_empty() {
        String::new()
    } else {
        resolve_relative_path_from(cert_path, &config_base_dir)
            .display()
            .to_string()
    };

    let key_path = config.metrics.tls.key_path.trim();
    config.metrics.tls.key_path = if key_path.is_empty() {
        String::new()
    } else {
        resolve_relative_path_from(key_path, &config_base_dir)
            .display()
            .to_string()
    };

    if let Some(file_storage) = config.livestream.hls_storage.file_mut() {
        let storage_path = file_storage.path.trim();
        file_storage.path = if storage_path.is_empty() {
            String::new()
        } else {
            resolve_relative_path_from(storage_path, &data_dir)
                .display()
                .to_string()
        };
    }

    if let Some(oss) = config.livestream.hls_storage.oss_mut() {
        let storage_base_path = oss.base_path.trim().trim_start_matches('/');
        oss.base_path = if storage_base_path.is_empty() {
            String::new()
        } else if storage_base_path.ends_with('/') {
            storage_base_path.to_string()
        } else {
            format!("{storage_base_path}/")
        };
    }

    for backend in config.file_storage.backends.values_mut() {
        if let Some(s3) = backend.s3_mut() {
            let file_storage_s3_base_path = s3.base_path.trim().trim_start_matches('/');
            s3.base_path = if file_storage_s3_base_path.is_empty() {
                String::new()
            } else if file_storage_s3_base_path.ends_with('/') {
                file_storage_s3_base_path.to_string()
            } else {
                format!("{file_storage_s3_base_path}/")
            };
        }
    }

    let proxy_slice_cache_dir = config.proxy_slice_cache.file_cache_dir.trim();
    config.proxy_slice_cache.file_cache_dir = if proxy_slice_cache_dir.is_empty() {
        if config.proxy_slice_cache.file_backend_enabled {
            data_dir
                .join(default_proxy_slice_cache_relative_path())
                .display()
                .to_string()
        } else {
            String::new()
        }
    } else {
        resolve_relative_path_from(proxy_slice_cache_dir, &data_dir)
            .display()
            .to_string()
    };
}

pub(crate) fn resolve_time_defaults_with(
    config: &mut Config,
    get_env: &impl Fn(&str) -> Option<String>,
) -> Result<(), ConfigError> {
    let resolved = common_time::resolve_timezone_name_with(
        Some(config.time.timezone.as_str()).filter(|value| !value.trim().is_empty()),
        get_env,
    )
    .map_err(|error| ConfigError::Message(error.to_string()))?;
    config.time.timezone = resolved;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tempfile::tempdir;

    use super::*;

    const EMAIL_OUTBOX_KEY: &str =
        "5757575757575757575757575757575757575757575757575757575757575757";

    #[test]
    fn email_outbox_key_accepts_direct_environment_override() {
        let mut config = Config::default();
        let env = HashMap::from([(
            "SYNCTV_SECURITY_EMAIL_OUTBOX_ENCRYPTION_KEY",
            EMAIL_OUTBOX_KEY.to_string(),
        )]);

        apply_env_overrides_with(&mut config, &|name| env.get(name).cloned())
            .expect("environment override should apply");

        assert_eq!(
            config.security.email_outbox_encryption_key,
            EMAIL_OUTBOX_KEY
        );
    }

    #[test]
    fn email_outbox_key_accepts_file_environment_override() {
        let dir = tempdir().expect("temp dir should be created");
        let key_path = dir.path().join("email-outbox-key");
        std::fs::write(&key_path, format!("{EMAIL_OUTBOX_KEY}\n"))
            .expect("key file should be written");
        let env = HashMap::from([(
            "SYNCTV_SECURITY_EMAIL_OUTBOX_ENCRYPTION_KEY_FILE",
            key_path.display().to_string(),
        )]);
        let mut config = Config::default();

        apply_env_overrides_with(&mut config, &|name| env.get(name).cloned())
            .expect("file environment override should apply");

        assert_eq!(
            config.security.email_outbox_encryption_key,
            EMAIL_OUTBOX_KEY
        );
    }

    #[test]
    fn separated_security_domains_accept_environment_overrides() {
        let mut config = Config::default();
        let env = HashMap::from([
            (
                "SYNCTV_SECURITY_TOTP_ENCRYPTION_KEY",
                "6767676767676767676767676767676767676767676767676767676767676767".to_string(),
            ),
            (
                "SYNCTV_SECURITY_PROXY_SIGNING_KEY",
                "proxy-signing-key-from-environment-123456".to_string(),
            ),
            (
                "SYNCTV_SECURITY_MEDIA_SWARM_SIGNING_KEY",
                "media-swarm-signing-key-from-environment-123456".to_string(),
            ),
            (
                "SYNCTV_SECURITY_PROVIDER_SESSION_ENCRYPTION_KEY",
                "provider-session-key-from-environment-123456".to_string(),
            ),
            (
                "SYNCTV_SECURITY_LOGIN_DISCOVERY_KEY",
                "login-discovery-key-from-environment-123456".to_string(),
            ),
            (
                "SYNCTV_SECURITY_WEBAUTHN_ENUMERATION_KEY",
                "webauthn-enumeration-key-from-environment-123456".to_string(),
            ),
            (
                "SYNCTV_FILE_UPLOAD_TOKEN_SECRET",
                "file-upload-token-key-from-environment-123456".to_string(),
            ),
        ]);

        apply_env_overrides_with(&mut config, &|name| env.get(name).cloned())
            .expect("environment overrides should apply");

        assert_eq!(
            config.security.totp_encryption_key,
            env["SYNCTV_SECURITY_TOTP_ENCRYPTION_KEY"]
        );
        assert_eq!(
            config.security.proxy_signing_key,
            env["SYNCTV_SECURITY_PROXY_SIGNING_KEY"]
        );
        assert_eq!(
            config.security.media_swarm_signing_key,
            env["SYNCTV_SECURITY_MEDIA_SWARM_SIGNING_KEY"]
        );
        assert_eq!(
            config.security.provider_session_encryption_key,
            env["SYNCTV_SECURITY_PROVIDER_SESSION_ENCRYPTION_KEY"]
        );
        assert_eq!(
            config.security.login_discovery_key,
            env["SYNCTV_SECURITY_LOGIN_DISCOVERY_KEY"]
        );
        assert_eq!(
            config.security.webauthn_enumeration_key,
            env["SYNCTV_SECURITY_WEBAUTHN_ENUMERATION_KEY"]
        );
        assert_eq!(
            config.file_storage.upload_token_secret,
            env["SYNCTV_FILE_UPLOAD_TOKEN_SECRET"]
        );
    }

    #[test]
    fn service_logging_and_internal_listener_environment_overrides() {
        let mut config = Config::default();
        let env = HashMap::from([
            ("SYNCTV_SERVER_LOGGING_LEVEL", "debug".to_string()),
            ("SYNCTV_SERVER_LOGGING_FORMAT", "json".to_string()),
            ("SYNCTV_SERVER_LOGGING_COLOR", "never".to_string()),
            ("SYNCTV_HEALTH_LOGGING_LEVEL", "error".to_string()),
            ("SYNCTV_HEALTH_LOGGING_FORMAT", "json".to_string()),
            ("SYNCTV_HEALTH_LOGGING_COLOR", "never".to_string()),
            (
                "SYNCTV_HEALTH_LOGGING_OUTPUT_PATH",
                "logs/health".to_string(),
            ),
            (
                "SYNCTV_HEALTH_LOGGING_OUTPUT_ROTATION_STRATEGY",
                "hourly".to_string(),
            ),
            (
                "SYNCTV_HEALTH_LOGGING_OUTPUT_ROTATION_MAX_FILES",
                "72".to_string(),
            ),
            ("SYNCTV_HEALTH_PORT", "18081".to_string()),
            ("SYNCTV_CLUSTER_ENABLED", "true".to_string()),
            ("SYNCTV_CLUSTER_HOST", "0.0.0.0".to_string()),
            ("SYNCTV_CLUSTER_PORT", "15051".to_string()),
            (
                "SYNCTV_CLUSTER_ADVERTISE_HOST",
                "node-0.internal".to_string(),
            ),
        ]);

        apply_env_overrides_with(&mut config, &|name| env.get(name).cloned())
            .expect("service configuration environment overrides should apply");

        assert_eq!(config.server.logging.level, "debug");
        assert_eq!(config.server.logging.format, "json");
        assert!(matches!(config.server.logging.color, LogColor::Never));
        assert_eq!(config.health.logging.level, "error");
        assert_eq!(config.health.logging.format, "json");
        assert!(matches!(config.health.logging.color, LogColor::Never));
        let LogOutput::File(health_output) = &config.health.logging.output else {
            panic!("health logging output should be a file output");
        };
        assert_eq!(health_output.path, "logs/health");
        assert_eq!(health_output.rotation.strategy, "hourly");
        assert_eq!(health_output.rotation.max_files, 72);
        assert_eq!(config.health.port, 18081);
        assert!(config.cluster.enabled);
        assert_eq!(config.cluster.port, 15051);
        assert_eq!(config.advertise_cluster_address(), "node-0.internal:15051");
    }

    #[test]
    fn relative_component_log_paths_resolve_under_data_dir() {
        let dir = tempdir().expect("temp dir should be created");
        let config_path = dir.path().join("synctv.yaml");
        let data_dir = dir.path().join("data");
        let mut config = Config::default();
        config.data_dir = data_dir.display().to_string();
        config.server.logging.output = LogOutput::File(LogFileOutput {
            path: "logs/server".to_string(),
            ..LogFileOutput::default()
        });
        config.health.logging.output = LogOutput::File(LogFileOutput {
            path: "logs/health".to_string(),
            ..LogFileOutput::default()
        });

        resolve_owned_local_paths(&mut config, Some(&config_path), false, None);

        let LogOutput::File(output) = config.server.logging.output else {
            panic!("server logging output should remain a file output");
        };
        assert_eq!(
            output.path,
            data_dir.join("logs/server").display().to_string()
        );
        let LogOutput::File(output) = config.health.logging.output else {
            panic!("health logging output should remain a file output");
        };
        assert_eq!(
            output.path,
            data_dir.join("logs/health").display().to_string()
        );
    }
}
