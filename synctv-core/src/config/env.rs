use super::*;

impl Config {
    pub(super) fn apply_redis_url_component_env_overrides(
        &mut self,
        get_env: &impl Fn(&str) -> Option<String>,
    ) -> Result<(), ConfigError> {
        if self.redis.url.trim().is_empty() {
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

        let mut url = url::Url::parse(&self.redis.url).map_err(|error| {
            ConfigError::Message(format!(
                "Cannot apply Redis environment overrides to redis.url '{}': {error}",
                self.redis.url
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
                    self.redis.url
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

        self.redis.url = url.to_string();
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
    pub(super) fn apply_env_overrides_with(
        &mut self,
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
        let env_override_json = |name: &str,
                                 target: &mut dyn std::any::Any|
         -> Result<(), ConfigError> {
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
            Err(ConfigError::Message(format!(
                "Unsupported environment JSON override target type for {name}"
            )))
        };

        env_override_str("SYNCTV_TIME_TIMEZONE", &mut self.time.timezone);

        if get_env("SYNCTV_PUBLIC_IDS_SQIDS_ALPHABET").is_some()
            || get_env("SYNCTV_PUBLIC_IDS_SQIDS_MIN_LENGTH").is_some()
        {
            let sqids = self.public_ids.sqids.get_or_insert_with(Default::default);
            env_override_opt_str("SYNCTV_PUBLIC_IDS_SQIDS_ALPHABET", &mut sqids.alphabet);
            env_override_parse("SYNCTV_PUBLIC_IDS_SQIDS_MIN_LENGTH", &mut sqids.min_length)?;
        }

        env_override_str(
            "SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY",
            &mut self.security.credential_encryption_key,
        );
        env_override_str_file(
            "SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY_FILE",
            "security.credential_encryption_key",
            &mut self.security.credential_encryption_key,
        )?;
        env_override_str(
            "SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET",
            &mut self.security.opaque_server_setup_secret,
        );
        env_override_str_file(
            "SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET_FILE",
            "security.opaque_server_setup_secret",
            &mut self.security.opaque_server_setup_secret,
        )?;
        env_override_bool(
            "SYNCTV_SECURITY_SSRF_ENABLED",
            &mut self.security.ssrf.enabled,
        )?;
        env_override_bool(
            "SYNCTV_SECURITY_SSRF_ALLOW_PRIVATE_NETWORK_TARGETS",
            &mut self.security.ssrf.allow_private_network_targets,
        )?;
        env_override_json_or_csv(
            "SYNCTV_SECURITY_SSRF_ALLOWED_HOSTS",
            &mut self.security.ssrf.allowed_hosts,
        );
        env_override_json_or_csv(
            "SYNCTV_SECURITY_SSRF_ALLOWED_IP_RANGES",
            &mut self.security.ssrf.allowed_ip_ranges,
        );

        env_override_str("SYNCTV_DATA_DIR", &mut self.data_dir);

        env_override_str("SYNCTV_SERVER_HOST", &mut self.server.host);
        env_override_parse("SYNCTV_SERVER_PORT", &mut self.server.port)?;
        env_override_bool(
            "SYNCTV_SERVER_ENABLE_REFLECTION",
            &mut self.server.enable_reflection,
        )?;
        env_override_csv(
            "SYNCTV_SERVER_TRUSTED_PROXIES",
            &mut self.server.trusted_proxies,
        );
        env_override_json_or_csv(
            "SYNCTV_SERVER_CORS_ALLOWED_ORIGINS",
            &mut self.server.cors_allowed_origins,
        );
        env_override_str("SYNCTV_CLUSTER_SECRET", &mut self.cluster.secret);
        env_override_str_file(
            "SYNCTV_CLUSTER_SECRET_FILE",
            "cluster.secret",
            &mut self.cluster.secret,
        )?;
        env_override_str(
            "SYNCTV_SERVER_ADVERTISE_HOST",
            &mut self.server.advertise_host,
        );
        env_override_parse(
            "SYNCTV_SERVER_SHUTDOWN_DRAIN_TIMEOUT_SECONDS",
            &mut self.server.shutdown_drain_timeout_seconds,
        )?;
        env_override_parse(
            "SYNCTV_SERVER_GRPC_MAX_MESSAGE_SIZE_BYTES",
            &mut self.server.grpc_max_message_size_bytes,
        )?;
        env_override_bool(
            "SYNCTV_SERVER_GRPC_COMPRESSION_ENABLED",
            &mut self.server.grpc_compression_enabled,
        )?;

        env_override_bool("SYNCTV_METRICS_ENABLED", &mut self.metrics.enabled)?;
        env_override_str("SYNCTV_METRICS_HOST", &mut self.metrics.host);
        env_override_parse("SYNCTV_METRICS_PORT", &mut self.metrics.port)?;
        env_override_bool("SYNCTV_METRICS_TLS_ENABLED", &mut self.metrics.tls.enabled)?;
        env_override_str(
            "SYNCTV_METRICS_TLS_CERT_PATH",
            &mut self.metrics.tls.cert_path,
        );
        env_override_str(
            "SYNCTV_METRICS_TLS_KEY_PATH",
            &mut self.metrics.tls.key_path,
        );
        env_override_enum("SYNCTV_METRICS_AUTH_MODE", &mut |val| {
            self.metrics.auth.mode = val.parse()?;
            Ok(())
        })?;
        env_override_str(
            "SYNCTV_METRICS_AUTH_BEARER_TOKEN",
            &mut self.metrics.auth.bearer_token,
        );
        env_override_str_file(
            "SYNCTV_METRICS_AUTH_BEARER_TOKEN_FILE",
            "metrics.auth.bearer_token",
            &mut self.metrics.auth.bearer_token,
        )?;
        env_override_str(
            "SYNCTV_METRICS_AUTH_BASIC_USERNAME",
            &mut self.metrics.auth.basic_username,
        );
        env_override_str(
            "SYNCTV_METRICS_AUTH_BASIC_PASSWORD",
            &mut self.metrics.auth.basic_password,
        );
        env_override_str_file(
            "SYNCTV_METRICS_AUTH_BASIC_PASSWORD_FILE",
            "metrics.auth.basic_password",
            &mut self.metrics.auth.basic_password,
        )?;
        env_override_str(
            "SYNCTV_METRICS_AUTH_KUBERNETES_AUDIENCE",
            &mut self.metrics.auth.kubernetes.audience,
        );
        env_override_parse(
            "SYNCTV_METRICS_AUTH_KUBERNETES_AUTHENTICATION_CACHE_TTL_SECONDS",
            &mut self
                .metrics
                .auth
                .kubernetes
                .authentication_cache_ttl_seconds,
        )?;
        env_override_parse(
            "SYNCTV_METRICS_AUTH_KUBERNETES_AUTHORIZATION_CACHE_TTL_SECONDS",
            &mut self.metrics.auth.kubernetes.authorization_cache_ttl_seconds,
        )?;

        env_override_bool("SYNCTV_MANAGEMENT_ENABLED", &mut self.management.enabled)?;
        env_override_enum("SYNCTV_MANAGEMENT_TRANSPORT", &mut |val| {
            self.management.transport = val.parse()?;
            Ok(())
        })?;
        env_override_parse("SYNCTV_MANAGEMENT_PORT", &mut self.management.port)?;
        env_override_str(
            "SYNCTV_MANAGEMENT_UNIX_SOCKET_PATH",
            &mut self.management.unix_socket_path,
        );
        env_override_str(
            "SYNCTV_MANAGEMENT_AUTH_TOKEN",
            &mut self.management.auth_token,
        );
        env_override_str_file(
            "SYNCTV_MANAGEMENT_AUTH_TOKEN_FILE",
            "management.auth_token",
            &mut self.management.auth_token,
        )?;
        env_override_bool(
            "SYNCTV_MANAGEMENT_ENABLE_REFLECTION",
            &mut self.management.enable_reflection,
        )?;

        let database_url_from_env = get_env("SYNCTV_DATABASE_URL").is_some()
            || get_env("SYNCTV_DATABASE_URL_FILE").is_some();
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
            self.database.url.clear();
        }

        env_override_str("SYNCTV_DATABASE_URL", &mut self.database.url);
        env_override_str_file(
            "SYNCTV_DATABASE_URL_FILE",
            "database.url",
            &mut self.database.url,
        )?;
        env_override_str("SYNCTV_DATABASE_HOST", &mut self.database.host);
        env_override_parse("SYNCTV_DATABASE_PORT", &mut self.database.port)?;
        env_override_str("SYNCTV_DATABASE_USERNAME", &mut self.database.username);
        env_override_str("SYNCTV_DATABASE_PASSWORD", &mut self.database.password);
        env_override_str_file(
            "SYNCTV_DATABASE_PASSWORD_FILE",
            "database.password",
            &mut self.database.password,
        )?;
        env_override_str("SYNCTV_DATABASE_NAME", &mut self.database.name);
        env_override_parse(
            "SYNCTV_DATABASE_MAX_CONNECTIONS",
            &mut self.database.max_connections,
        )?;
        env_override_parse(
            "SYNCTV_DATABASE_MIN_CONNECTIONS",
            &mut self.database.min_connections,
        )?;
        env_override_parse(
            "SYNCTV_DATABASE_CONNECT_TIMEOUT_SECONDS",
            &mut self.database.connect_timeout_seconds,
        )?;
        env_override_parse(
            "SYNCTV_DATABASE_IDLE_TIMEOUT_SECONDS",
            &mut self.database.idle_timeout_seconds,
        )?;
        env_override_parse(
            "SYNCTV_DATABASE_MAX_LIFETIME_SECONDS",
            &mut self.database.max_lifetime_seconds,
        )?;

        let redis_url_from_env =
            get_env("SYNCTV_REDIS_URL").is_some() || get_env("SYNCTV_REDIS_URL_FILE").is_some();

        env_override_str("SYNCTV_REDIS_URL", &mut self.redis.url);
        env_override_str_file("SYNCTV_REDIS_URL_FILE", "redis.url", &mut self.redis.url)?;
        if self.redis.url.trim().is_empty() || redis_url_from_env {
            env_override_str("SYNCTV_REDIS_HOST", &mut self.redis.host);
            env_override_parse("SYNCTV_REDIS_PORT", &mut self.redis.port)?;
            env_override_str("SYNCTV_REDIS_USERNAME", &mut self.redis.username);
            env_override_str("SYNCTV_REDIS_PASSWORD", &mut self.redis.password);
            env_override_str_file(
                "SYNCTV_REDIS_PASSWORD_FILE",
                "redis.password",
                &mut self.redis.password,
            )?;
            env_override_parse("SYNCTV_REDIS_DATABASE", &mut self.redis.database)?;
        } else {
            self.apply_redis_url_component_env_overrides(get_env)?;
        }
        env_override_parse(
            "SYNCTV_REDIS_CONNECT_TIMEOUT_SECONDS",
            &mut self.redis.connect_timeout_seconds,
        )?;
        env_override_parse(
            "SYNCTV_REDIS_RESPONSE_TIMEOUT_SECONDS",
            &mut self.redis.response_timeout_seconds,
        )?;
        env_override_parse(
            "SYNCTV_REDIS_PIPELINE_BUFFER_SIZE",
            &mut self.redis.pipeline_buffer_size,
        )?;
        env_override_str("SYNCTV_REDIS_KEY_PREFIX", &mut self.redis.key_prefix);
        env_override_enum("SYNCTV_REDIS_DEPLOYMENT_MODE", &mut |val| {
            match val.to_lowercase().as_str() {
                "standalone" => self.redis.deployment_mode = RedisDeploymentMode::Standalone,
                "sentinel" => self.redis.deployment_mode = RedisDeploymentMode::Sentinel,
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
            &mut self.redis.sentinel_master_name,
        );
        env_override_csv(
            "SYNCTV_REDIS_SENTINEL_ADDRESSES",
            &mut self.redis.sentinel_addresses,
        );

        env_override_str("SYNCTV_JWT_SECRET", &mut self.jwt.secret);
        env_override_str_file("SYNCTV_JWT_SECRET_FILE", "jwt.secret", &mut self.jwt.secret)?;
        env_override_parse(
            "SYNCTV_JWT_ACCESS_TOKEN_DURATION_HOURS",
            &mut self.jwt.access_token_duration_hours,
        )?;
        env_override_parse(
            "SYNCTV_JWT_REFRESH_TOKEN_DURATION_DAYS",
            &mut self.jwt.refresh_token_duration_days,
        )?;
        env_override_parse(
            "SYNCTV_JWT_GUEST_TOKEN_DURATION_HOURS",
            &mut self.jwt.guest_token_duration_hours,
        )?;
        env_override_parse(
            "SYNCTV_JWT_CLOCK_SKEW_LEEWAY_SECS",
            &mut self.jwt.clock_skew_leeway_secs,
        )?;

        env_override_bool("SYNCTV_WEBAUTHN_ENABLED", &mut self.webauthn.enabled)?;
        env_override_str("SYNCTV_WEBAUTHN_RP_ID", &mut self.webauthn.rp_id);
        env_override_str("SYNCTV_WEBAUTHN_RP_ORIGIN", &mut self.webauthn.rp_origin);
        env_override_str("SYNCTV_WEBAUTHN_RP_NAME", &mut self.webauthn.rp_name);
        env_override_json_or_csv(
            "SYNCTV_WEBAUTHN_ALLOWED_ORIGINS",
            &mut self.webauthn.allowed_origins,
        );
        env_override_bool(
            "SYNCTV_WEBAUTHN_ALLOW_SUBDOMAINS",
            &mut self.webauthn.allow_subdomains,
        )?;
        env_override_bool(
            "SYNCTV_WEBAUTHN_ALLOW_ANY_PORT",
            &mut self.webauthn.allow_any_port,
        )?;
        env_override_parse(
            "SYNCTV_WEBAUTHN_TIMEOUT_SECONDS",
            &mut self.webauthn.timeout_seconds,
        )?;

        env_override_str("SYNCTV_LOGGING_LEVEL", &mut self.logging.level);
        env_override_str("SYNCTV_LOGGING_FORMAT", &mut self.logging.format);
        env_override_opt_str("SYNCTV_LOGGING_FILTER", &mut self.logging.filter);
        env_override_bool("SYNCTV_LOGGING_BACKTRACE", &mut self.logging.backtrace)?;
        env_override_opt_str("SYNCTV_LOGGING_FILE_PATH", &mut self.logging.file_path);

        env_override_parse(
            "SYNCTV_LIVESTREAM_RTMP_PORT",
            &mut self.livestream.rtmp_port,
        )?;
        env_override_str(
            "SYNCTV_LIVESTREAM_PUBLIC_RTMP_HOST",
            &mut self.livestream.public_rtmp_host,
        );
        env_override_parse(
            "SYNCTV_LIVESTREAM_GOP_CACHE_SIZE",
            &mut self.livestream.gop_cache_size,
        )?;
        env_override_parse(
            "SYNCTV_LIVESTREAM_STREAM_TIMEOUT_SECONDS",
            &mut self.livestream.stream_timeout_seconds,
        )?;
        env_override_parse(
            "SYNCTV_LIVESTREAM_CLEANUP_CHECK_INTERVAL_SECONDS",
            &mut self.livestream.cleanup_check_interval_seconds,
        )?;
        env_override_parse(
            "SYNCTV_LIVESTREAM_PULL_MAX_RETRIES",
            &mut self.livestream.pull_max_retries,
        )?;
        env_override_parse(
            "SYNCTV_LIVESTREAM_PULL_INITIAL_BACKOFF_MS",
            &mut self.livestream.pull_initial_backoff_ms,
        )?;
        env_override_parse(
            "SYNCTV_LIVESTREAM_PULL_MAX_BACKOFF_MS",
            &mut self.livestream.pull_max_backoff_ms,
        )?;
        env_override_parse(
            "SYNCTV_LIVESTREAM_MAX_FLV_TAG_SIZE_BYTES",
            &mut self.livestream.max_flv_tag_size_bytes,
        )?;
        env_override_parse(
            "SYNCTV_LIVESTREAM_GOP_CACHE_MAX_MEMORY_MB",
            &mut self.livestream.gop_cache_max_memory_mb,
        )?;
        env_override_parse(
            "SYNCTV_LIVESTREAM_HLS_MEMORY_MAX_MB",
            &mut self.livestream.hls_memory_max_mb,
        )?;
        env_override_enum("SYNCTV_LIVESTREAM_HLS_STORAGE_BACKEND", &mut |val| {
            self.livestream.hls_storage_backend = val.parse()?;
            Ok(())
        })?;
        env_override_str(
            "SYNCTV_LIVESTREAM_HLS_STORAGE_PATH",
            &mut self.livestream.hls_storage_path,
        );
        env_override_str(
            "SYNCTV_LIVESTREAM_HLS_OSS_ENDPOINT",
            &mut self.livestream.hls_oss.endpoint,
        );
        env_override_str(
            "SYNCTV_LIVESTREAM_HLS_OSS_ACCESS_KEY_ID",
            &mut self.livestream.hls_oss.access_key_id,
        );
        env_override_str_file(
            "SYNCTV_LIVESTREAM_HLS_OSS_ACCESS_KEY_ID_FILE",
            "livestream.hls_oss.access_key_id",
            &mut self.livestream.hls_oss.access_key_id,
        )?;
        env_override_str(
            "SYNCTV_LIVESTREAM_HLS_OSS_SECRET_ACCESS_KEY",
            &mut self.livestream.hls_oss.secret_access_key,
        );
        env_override_str_file(
            "SYNCTV_LIVESTREAM_HLS_OSS_SECRET_ACCESS_KEY_FILE",
            "livestream.hls_oss.secret_access_key",
            &mut self.livestream.hls_oss.secret_access_key,
        )?;
        env_override_str(
            "SYNCTV_LIVESTREAM_HLS_OSS_BUCKET",
            &mut self.livestream.hls_oss.bucket,
        );
        env_override_opt_str(
            "SYNCTV_LIVESTREAM_HLS_OSS_REGION",
            &mut self.livestream.hls_oss.region,
        );
        env_override_str(
            "SYNCTV_LIVESTREAM_HLS_OSS_BASE_PATH",
            &mut self.livestream.hls_oss.base_path,
        );

        env_override_str(
            "SYNCTV_FILE_UPLOAD_TOKEN_SECRET",
            &mut self.file_storage.upload_token_secret,
        );
        env_override_str_file(
            "SYNCTV_FILE_UPLOAD_TOKEN_SECRET_FILE",
            "file_storage.upload_token_secret",
            &mut self.file_storage.upload_token_secret,
        )?;
        env_override_str(
            "SYNCTV_FILE_STORAGE_DEFAULT_BACKEND",
            &mut self.file_storage.default_backend,
        );
        env_override_str(
            "SYNCTV_FILE_STORAGE_CHAT_ATTACHMENTS_BACKEND",
            &mut self.file_storage.chat_attachments_backend,
        );
        env_override_str(
            "SYNCTV_FILE_STORAGE_USER_AVATARS_BACKEND",
            &mut self.file_storage.user_avatars_backend,
        );
        env_override_str(
            "SYNCTV_FILE_STORAGE_MEDIA_COVERS_BACKEND",
            &mut self.file_storage.media_covers_backend,
        );
        env_override_str(
            "SYNCTV_FILE_STORAGE_ROOM_COVERS_BACKEND",
            &mut self.file_storage.room_covers_backend,
        );
        env_override_str(
            "SYNCTV_FILE_STORAGE_PLAYLIST_COVERS_BACKEND",
            &mut self.file_storage.playlist_covers_backend,
        );
        env_override_parse(
            "SYNCTV_FILE_STORAGE_UNREFERENCED_OBJECT_RETENTION_SECONDS",
            &mut self.file_storage.unreferenced_object_retention_seconds,
        )?;
        env_override_json(
            "SYNCTV_FILE_STORAGE_BACKENDS",
            &mut self.file_storage.backends,
        )?;

        env_override_parse(
            "SYNCTV_LIVESTREAM_FLV_MAX_CONNECTION_DURATION_SECONDS",
            &mut self.livestream.flv_max_connection_duration_seconds,
        )?;
        env_override_parse(
            "SYNCTV_LIVESTREAM_FLV_WRITE_TIMEOUT_SECONDS",
            &mut self.livestream.flv_write_timeout_seconds,
        )?;

        env_override_parse(
            "SYNCTV_MEDIA_PROVIDERS_ALIST_REQUEST_TIMEOUT_SECONDS",
            &mut self.media_providers.alist.request_timeout_seconds,
        )?;
        env_override_parse(
            "SYNCTV_MEDIA_PROVIDERS_ALIST_CONNECT_TIMEOUT_SECONDS",
            &mut self.media_providers.alist.connect_timeout_seconds,
        )?;
        env_override_parse(
            "SYNCTV_MEDIA_PROVIDERS_BILIBILI_REQUEST_TIMEOUT_SECONDS",
            &mut self.media_providers.bilibili.request_timeout_seconds,
        )?;
        env_override_parse(
            "SYNCTV_MEDIA_PROVIDERS_BILIBILI_CONNECT_TIMEOUT_SECONDS",
            &mut self.media_providers.bilibili.connect_timeout_seconds,
        )?;
        env_override_parse(
            "SYNCTV_MEDIA_PROVIDERS_EMBY_REQUEST_TIMEOUT_SECONDS",
            &mut self.media_providers.emby.request_timeout_seconds,
        )?;
        env_override_parse(
            "SYNCTV_MEDIA_PROVIDERS_EMBY_CONNECT_TIMEOUT_SECONDS",
            &mut self.media_providers.emby.connect_timeout_seconds,
        )?;

        env_override_enum("SYNCTV_WEBRTC_MODE", &mut |val| {
            match val.to_lowercase().as_str() {
                "signaling_only" => self.webrtc.mode = WebRTCMode::SignalingOnly,
                "peer_to_peer" => self.webrtc.mode = WebRTCMode::PeerToPeer,
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
            &mut self.webrtc.enable_builtin_stun,
        )?;
        env_override_parse("SYNCTV_WEBRTC_STUN_PORT", &mut self.webrtc.stun_port)?;
        env_override_str("SYNCTV_WEBRTC_STUN_HOST", &mut self.webrtc.stun_host);
        env_override_str(
            "SYNCTV_WEBRTC_STUN_EXTERNAL_ADDR",
            &mut self.webrtc.stun_external_addr,
        );
        env_override_bool(
            "SYNCTV_WEBRTC_FILTER_PRIVATE_ICE_CANDIDATES",
            &mut self.webrtc.filter_private_ice_candidates,
        )?;

        env_override_parse(
            "SYNCTV_CONNECTION_LIMITS_MAX_PER_USER",
            &mut self.connection_limits.max_per_user,
        )?;
        env_override_parse(
            "SYNCTV_CONNECTION_LIMITS_MAX_PER_ROOM",
            &mut self.connection_limits.max_per_room,
        )?;
        env_override_parse(
            "SYNCTV_CONNECTION_LIMITS_MAX_TOTAL",
            &mut self.connection_limits.max_total,
        )?;
        env_override_parse(
            "SYNCTV_CONNECTION_LIMITS_IDLE_TIMEOUT_SECONDS",
            &mut self.connection_limits.idle_timeout_seconds,
        )?;
        env_override_parse(
            "SYNCTV_CONNECTION_LIMITS_MAX_DURATION_SECONDS",
            &mut self.connection_limits.max_duration_seconds,
        )?;
        env_override_parse(
            "SYNCTV_CONNECTION_LIMITS_WS_MESSAGE_RATE_LIMIT_PER_SECOND",
            &mut self.connection_limits.ws_message_rate_limit_per_second,
        )?;

        env_override_parse(
            "SYNCTV_MESSAGING_RATE_LIMITS_CHAT_PER_SECOND",
            &mut self.messaging_rate_limits.chat_per_second,
        )?;
        env_override_parse(
            "SYNCTV_MESSAGING_RATE_LIMITS_WINDOW_SECONDS",
            &mut self.messaging_rate_limits.window_seconds,
        )?;

        env_override_bool(
            "SYNCTV_BOOTSTRAP_CREATE_ROOT_USER",
            &mut self.bootstrap.create_root_user,
        )?;
        env_override_str(
            "SYNCTV_BOOTSTRAP_ROOT_USERNAME",
            &mut self.bootstrap.root_username,
        );
        env_override_str(
            "SYNCTV_BOOTSTRAP_ROOT_PASSWORD",
            &mut self.bootstrap.root_password,
        );
        env_override_str_file(
            "SYNCTV_BOOTSTRAP_ROOT_PASSWORD_FILE",
            "bootstrap.root_password",
            &mut self.bootstrap.root_password,
        )?;

        env_override_bool("SYNCTV_CLUSTER_ENABLED", &mut self.cluster.enabled)?;
        env_override_parse(
            "SYNCTV_CLUSTER_CRITICAL_CHANNEL_CAPACITY",
            &mut self.cluster.critical_channel_capacity,
        )?;
        env_override_parse(
            "SYNCTV_CLUSTER_PUBLISH_CHANNEL_CAPACITY",
            &mut self.cluster.publish_channel_capacity,
        )?;
        env_override_enum("SYNCTV_CLUSTER_DISCOVERY_MODE", &mut |val| {
            self.cluster.discovery_mode = val.parse().map_err(|error| {
                ConfigError::Message(format!(
                    "Invalid value for environment variable SYNCTV_CLUSTER_DISCOVERY_MODE: '{val}' ({error})"
                ))
            })?;
            Ok(())
        })?;
        env_override_enum("SYNCTV_CLUSTER_LEADER_ELECTION_MODE", &mut |val| {
            self.cluster.leader_election_mode = val.parse().map_err(|error| {
                ConfigError::Message(format!(
                    "Invalid value for environment variable SYNCTV_CLUSTER_LEADER_ELECTION_MODE: '{val}' ({error})"
                ))
            })?;
            Ok(())
        })?;
        env_override_csv("SYNCTV_CLUSTER_PEERS", &mut self.cluster.peers);
        env_override_parse(
            "SYNCTV_CLUSTER_CATCHUP_WINDOW_SECS",
            &mut self.cluster.catchup_window_secs,
        )?;
        env_override_parse(
            "SYNCTV_CLUSTER_STREAM_MAX_LENGTH",
            &mut self.cluster.stream_max_length,
        )?;

        env_override_parse(
            "SYNCTV_PASSWORD_COMPLEXITY_MIN_LENGTH",
            &mut self.password_complexity.min_length,
        )?;
        env_override_bool(
            "SYNCTV_PASSWORD_COMPLEXITY_REQUIRE_UPPERCASE",
            &mut self.password_complexity.require_uppercase,
        )?;
        env_override_bool(
            "SYNCTV_PASSWORD_COMPLEXITY_REQUIRE_LOWERCASE",
            &mut self.password_complexity.require_lowercase,
        )?;
        env_override_bool(
            "SYNCTV_PASSWORD_COMPLEXITY_REQUIRE_DIGIT",
            &mut self.password_complexity.require_digit,
        )?;
        env_override_bool(
            "SYNCTV_PASSWORD_COMPLEXITY_REQUIRE_SPECIAL",
            &mut self.password_complexity.require_special,
        )?;
        env_override_parse(
            "SYNCTV_PASSWORD_COMPLEXITY_MAX_REPEATED_CHARS",
            &mut self.password_complexity.max_repeated_chars,
        )?;
        env_override_bool(
            "SYNCTV_PASSWORD_COMPLEXITY_ZXCVBN_ENABLED",
            &mut self.password_complexity.zxcvbn_enabled,
        )?;
        env_override_parse(
            "SYNCTV_PASSWORD_COMPLEXITY_ZXCVBN_MIN_SCORE",
            &mut self.password_complexity.zxcvbn_min_score,
        )?;

        env_override_parse(
            "SYNCTV_BUFFER_SIZES_WEBSOCKET_OUTBOUND",
            &mut self.buffer_sizes.websocket_outbound,
        )?;
        env_override_parse(
            "SYNCTV_BUFFER_SIZES_AUDIT_BUFFER",
            &mut self.buffer_sizes.audit_buffer,
        )?;

        env_override_parse("SYNCTV_CACHE_L1_CAPACITY", &mut self.cache.l1_capacity)?;
        env_override_parse(
            "SYNCTV_CACHE_L1_TTL_SECONDS",
            &mut self.cache.l1_ttl_seconds,
        )?;
        env_override_parse(
            "SYNCTV_CACHE_L2_TTL_SECONDS",
            &mut self.cache.l2_ttl_seconds,
        )?;
        env_override_parse(
            "SYNCTV_CACHE_USERNAME_CACHE_CAPACITY",
            &mut self.cache.username_cache_capacity,
        )?;
        env_override_parse(
            "SYNCTV_CACHE_USERNAME_CACHE_TTL_SECONDS",
            &mut self.cache.username_cache_ttl_seconds,
        )?;
        env_override_parse(
            "SYNCTV_CACHE_PERMISSION_CACHE_CAPACITY",
            &mut self.cache.permission_cache_capacity,
        )?;
        env_override_parse(
            "SYNCTV_CACHE_PERMISSION_CACHE_TTL_SECONDS",
            &mut self.cache.permission_cache_ttl_seconds,
        )?;
        env_override_bool(
            "SYNCTV_PROXY_SLICE_CACHE_ENABLED",
            &mut self.proxy_slice_cache.enabled,
        )?;
        env_override_parse(
            "SYNCTV_PROXY_SLICE_CACHE_SLICE_SIZE_BYTES",
            &mut self.proxy_slice_cache.slice_size_bytes,
        )?;
        env_override_parse(
            "SYNCTV_PROXY_SLICE_CACHE_MAX_CACHE_SIZE_BYTES",
            &mut self.proxy_slice_cache.max_cache_size_bytes,
        )?;
        env_override_parse(
            "SYNCTV_PROXY_SLICE_CACHE_SEGMENT_TTL_SECONDS",
            &mut self.proxy_slice_cache.segment_ttl_seconds,
        )?;
        env_override_parse(
            "SYNCTV_PROXY_SLICE_CACHE_STALE_MAX_AGE_SECONDS",
            &mut self.proxy_slice_cache.stale_max_age_seconds,
        )?;
        env_override_bool(
            "SYNCTV_PROXY_SLICE_CACHE_STALE_WHILE_REVALIDATE",
            &mut self.proxy_slice_cache.stale_while_revalidate,
        )?;
        env_override_bool(
            "SYNCTV_PROXY_SLICE_CACHE_FILE_BACKEND_ENABLED",
            &mut self.proxy_slice_cache.file_backend_enabled,
        )?;
        env_override_str(
            "SYNCTV_PROXY_SLICE_CACHE_FILE_CACHE_DIR",
            &mut self.proxy_slice_cache.file_cache_dir,
        );
        env_override_parse(
            "SYNCTV_PROXY_SLICE_CACHE_EVICTION_INTERVAL_SECONDS",
            &mut self.proxy_slice_cache.eviction_interval_seconds,
        )?;
        env_override_parse(
            "SYNCTV_PROXY_SLICE_CACHE_WATERMARK_RATIO",
            &mut self.proxy_slice_cache.watermark_ratio,
        )?;

        env_override_parse(
            "SYNCTV_REQUEST_RATE_LIMITS_AUTH_MAX_REQUESTS",
            &mut self.request_rate_limits.auth_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_REQUEST_RATE_LIMITS_AUTH_WINDOW_SECONDS",
            &mut self.request_rate_limits.auth_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_REQUEST_RATE_LIMITS_WRITE_MAX_REQUESTS",
            &mut self.request_rate_limits.write_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_REQUEST_RATE_LIMITS_WRITE_WINDOW_SECONDS",
            &mut self.request_rate_limits.write_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_REQUEST_RATE_LIMITS_READ_MAX_REQUESTS",
            &mut self.request_rate_limits.read_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_REQUEST_RATE_LIMITS_READ_WINDOW_SECONDS",
            &mut self.request_rate_limits.read_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_REQUEST_RATE_LIMITS_MEDIA_MAX_REQUESTS",
            &mut self.request_rate_limits.media_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_REQUEST_RATE_LIMITS_MEDIA_WINDOW_SECONDS",
            &mut self.request_rate_limits.media_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_REQUEST_RATE_LIMITS_ADMIN_MAX_REQUESTS",
            &mut self.request_rate_limits.admin_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_REQUEST_RATE_LIMITS_ADMIN_WINDOW_SECONDS",
            &mut self.request_rate_limits.admin_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_REQUEST_RATE_LIMITS_STREAMING_MAX_REQUESTS",
            &mut self.request_rate_limits.streaming_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_REQUEST_RATE_LIMITS_STREAMING_WINDOW_SECONDS",
            &mut self.request_rate_limits.streaming_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_REQUEST_RATE_LIMITS_WEBSOCKET_MAX_REQUESTS",
            &mut self.request_rate_limits.websocket_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_REQUEST_RATE_LIMITS_WEBSOCKET_WINDOW_SECONDS",
            &mut self.request_rate_limits.websocket_window_seconds,
        )?;
        env_override_json(
            "SYNCTV_REQUEST_RATE_LIMITS_SCOPES",
            &mut self.request_rate_limits.scopes,
        )?;

        Ok(())
    }

    pub(super) fn resolve_owned_local_paths(
        &mut self,
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
                let configured = self.data_dir.trim();
                if configured.is_empty() {
                    None
                } else if Path::new(configured).is_absolute() {
                    Some(PathBuf::from(configured))
                } else {
                    Some(resolve_relative_path_from(configured, &runtime_base_dir))
                }
            })
            .unwrap_or_else(default_data_dir);
        self.data_dir = data_dir.display().to_string();

        let default_socket_path = default_management_unix_socket_path();
        let socket_path = self.management.unix_socket_path.trim();
        let socket_uses_default =
            socket_path.is_empty() || Path::new(socket_path) == default_socket_path;
        self.management.unix_socket_path = if socket_uses_default {
            data_dir
                .join(default_runtime_socket_relative_path())
                .display()
                .to_string()
        } else {
            resolve_relative_path_from(socket_path, &data_dir)
                .display()
                .to_string()
        };

        self.logging.file_path = self
            .logging
            .file_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                resolve_relative_path_from(value, &data_dir)
                    .display()
                    .to_string()
            });

        let cert_path = self.metrics.tls.cert_path.trim();
        self.metrics.tls.cert_path = if cert_path.is_empty() {
            String::new()
        } else {
            resolve_relative_path_from(cert_path, &config_base_dir)
                .display()
                .to_string()
        };

        let key_path = self.metrics.tls.key_path.trim();
        self.metrics.tls.key_path = if key_path.is_empty() {
            String::new()
        } else {
            resolve_relative_path_from(key_path, &config_base_dir)
                .display()
                .to_string()
        };

        let hls_storage_path = self.livestream.hls_storage_path.trim();
        self.livestream.hls_storage_path = if hls_storage_path.is_empty() {
            String::new()
        } else {
            resolve_relative_path_from(hls_storage_path, &data_dir)
                .display()
                .to_string()
        };

        let hls_oss_base_path = self
            .livestream
            .hls_oss
            .base_path
            .trim()
            .trim_start_matches('/');
        self.livestream.hls_oss.base_path = if hls_oss_base_path.is_empty() {
            String::new()
        } else if hls_oss_base_path.ends_with('/') {
            hls_oss_base_path.to_string()
        } else {
            format!("{hls_oss_base_path}/")
        };

        for backend in self.file_storage.backends.values_mut() {
            let file_storage_s3_base_path = backend.s3.base_path.trim().trim_start_matches('/');
            backend.s3.base_path = if file_storage_s3_base_path.is_empty() {
                String::new()
            } else if file_storage_s3_base_path.ends_with('/') {
                file_storage_s3_base_path.to_string()
            } else {
                format!("{file_storage_s3_base_path}/")
            };
        }

        let proxy_slice_cache_dir = self.proxy_slice_cache.file_cache_dir.trim();
        self.proxy_slice_cache.file_cache_dir = if proxy_slice_cache_dir.is_empty() {
            if self.proxy_slice_cache.file_backend_enabled {
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

    pub(super) fn resolve_time_defaults_with(
        &mut self,
        get_env: &impl Fn(&str) -> Option<String>,
    ) -> Result<(), ConfigError> {
        let resolved = common_time::resolve_timezone_name_with(
            Some(self.time.timezone.as_str()).filter(|value| !value.trim().is_empty()),
            get_env,
        )
        .map_err(|error| ConfigError::Message(error.to_string()))?;
        self.time.timezone = resolved;
        Ok(())
    }
}
