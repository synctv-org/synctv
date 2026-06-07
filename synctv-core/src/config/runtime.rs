use super::*;

impl Config {
    /// Load configuration from multiple sources with priority:
    /// 1. Environment variables (highest priority)
    /// 2. Config file (if provided)
    /// 3. Defaults (lowest priority)
    pub fn load(config_file: Option<&str>) -> Result<Self, ConfigError> {
        let env: HashMap<String, String> = std::env::vars().collect();
        Self::load_with_env_map(config_file, &env)
    }

    pub fn load_with_env_map(
        config_file: Option<&str>,
        env: &HashMap<String, String>,
    ) -> Result<Self, ConfigError> {
        Self::load_with_env(config_file, env, None).map(|loaded| loaded.config)
    }

    pub fn load_with_env_map_and_data_dir_override(
        config_file: Option<&str>,
        env: &HashMap<String, String>,
        data_dir_override: Option<&str>,
    ) -> Result<Self, ConfigError> {
        Self::load_with_env(config_file, env, data_dir_override).map(|loaded| loaded.config)
    }

    pub fn inspect_unknowns_with_env_map(
        config_file: Option<&str>,
        env: &HashMap<String, String>,
        data_dir_override: Option<&str>,
    ) -> Result<UnknownConfigDiagnostics, ConfigError> {
        Self::load_with_env_lenient(config_file, env, data_dir_override)
            .map(|loaded| loaded.unknown)
    }

    fn load_with_env(
        config_file: Option<&str>,
        env: &HashMap<String, String>,
        data_dir_override: Option<&str>,
    ) -> Result<LoadedConfig, ConfigError> {
        let loaded = Self::load_with_env_lenient(config_file, env, data_dir_override)?;
        if !loaded.unknown.is_empty() {
            return Err(ConfigError::Message(format!(
                "strict configuration rejected unknown setting(s): {}",
                loaded.unknown.strict_error_message()
            )));
        }
        Ok(loaded)
    }

    fn load_with_env_lenient(
        config_file: Option<&str>,
        env: &HashMap<String, String>,
        data_dir_override: Option<&str>,
    ) -> Result<LoadedConfig, ConfigError> {
        let seen_env_keys = std::cell::RefCell::new(std::collections::HashSet::<String>::new());
        if config_file.is_some() && env.contains_key("SYNCTV_CONFIG_PATH") {
            seen_env_keys
                .borrow_mut()
                .insert("SYNCTV_CONFIG_PATH".to_string());
        }
        let get_env = |name: &str| {
            seen_env_keys.borrow_mut().insert(name.to_string());
            env.get(name).cloned()
        };
        let (mut config, mut unknown) = match config_file {
            Some(path) => Self::load_config_file(path)?,
            None => (Self::default(), UnknownConfigDiagnostics::default()),
        };

        // Apply SYNCTV_* environment variable overrides (single underscore format).
        // We don't use the config crate's Environment source because its separator
        // cannot distinguish nesting from underscores within field names.
        // Instead, every SYNCTV_ env var is mapped explicitly here.
        config.apply_env_overrides_with(&get_env)?;
        config.resolve_owned_local_paths(
            config_file.map(Path::new),
            env.contains_key("SYNCTV_DATA_DIR"),
            data_dir_override,
        );
        config.resolve_time_defaults_with(&get_env)?;
        let seen_env_keys = seen_env_keys.into_inner();
        unknown.env_keys = Self::collect_unknown_synctv_env_vars(env, &seen_env_keys);

        Ok(LoadedConfig { config, unknown })
    }

    pub(super) fn load_config_file(
        path: &str,
    ) -> Result<(Self, UnknownConfigDiagnostics), ConfigError> {
        let path = Path::new(path);
        if !path.exists() {
            return Err(ConfigError::Message(format!(
                "config file not found: {}",
                absolute_display_path(path)
            )));
        }
        let (config, unknown_keys) = Self::deserialize_config_file(path)?;
        Ok((
            config,
            UnknownConfigDiagnostics {
                config_file: Some(absolute_display_path(path)),
                config_keys: unknown_keys,
                env_keys: Vec::new(),
            },
        ))
    }

    #[cfg(test)]
    pub(super) fn collect_unknown_config_file_keys(path: &str) -> Result<Vec<String>, ConfigError> {
        let path = Path::new(path);
        let (_, unknown_keys) = Self::deserialize_config_file(path)?;
        Ok(unknown_keys)
    }

    fn deserialize_config_file(path: &Path) -> Result<(Self, Vec<String>), ConfigError> {
        let contents = std::fs::read_to_string(path).map_err(|error| {
            ConfigError::Message(format!(
                "failed to read config file {}: {error}",
                absolute_display_path(path)
            ))
        })?;
        let format = config_file_format_for_path(path)?;
        let mut parsed_value = match format {
            FileFormat::Yaml => serde_yaml::from_str::<serde_json::Value>(&contents)
                .map_err(|error| ConfigError::Message(error.to_string()))?,
            FileFormat::Json => serde_json::from_str::<serde_json::Value>(&contents)
                .map_err(|error| ConfigError::Message(error.to_string()))?,
            FileFormat::Toml => {
                let parsed = toml::from_str::<toml::Value>(&contents)
                    .map_err(|error| ConfigError::Message(error.to_string()))?;
                serde_json::to_value(parsed)
                    .map_err(|error| ConfigError::Message(error.to_string()))?
            }
            _ => {
                return Err(ConfigError::Message(
                    "unsupported config file format".to_string(),
                ));
            }
        };
        let config_base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        resolve_secret_file_references_in_json_value(&mut parsed_value, path, "", config_base_dir)?;
        normalize_split_database_config_value(&mut parsed_value);
        let normalized_contents = serde_json::to_string(&parsed_value)
            .map_err(|error| ConfigError::Message(error.to_string()))?;
        Self::deserialize_config_contents(&normalized_contents, FileFormat::Json)
    }

    fn finalize_unknown_keys(mut unknown_keys: Vec<String>) -> Vec<String> {
        unknown_keys.sort_unstable();
        unknown_keys.dedup();
        unknown_keys
    }

    fn deserialize_config_contents(
        contents: &str,
        format: FileFormat,
    ) -> Result<(Self, Vec<String>), ConfigError> {
        let mut unknown_keys = Vec::new();
        let config = match format {
            FileFormat::Yaml => {
                let deserializer = serde_yaml::Deserializer::from_str(contents);
                serde_ignored::deserialize::<_, _, Self>(deserializer, |path| {
                    unknown_keys.push(path.to_string());
                })
                .map_err(|error| ConfigError::Message(error.to_string()))?
            }
            FileFormat::Json => {
                let mut deserializer = serde_json::Deserializer::from_str(contents);
                serde_ignored::deserialize::<_, _, Self>(&mut deserializer, |path| {
                    unknown_keys.push(path.to_string());
                })
                .map_err(|error| ConfigError::Message(error.to_string()))?
            }
            FileFormat::Toml => {
                let deserializer = toml::Deserializer::parse(contents)
                    .map_err(|error| ConfigError::Message(error.to_string()))?;
                serde_ignored::deserialize::<_, _, Self>(deserializer, |path| {
                    unknown_keys.push(path.to_string());
                })
                .map_err(|error| ConfigError::Message(error.to_string()))?
            }
            _ => {
                return Err(ConfigError::Message(
                    "unsupported config file format".to_string(),
                ));
            }
        };

        Ok((config, Self::finalize_unknown_keys(unknown_keys)))
    }

    pub(super) fn collect_unknown_synctv_env_vars(
        env: &HashMap<String, String>,
        seen_env_keys: &std::collections::HashSet<String>,
    ) -> Vec<String> {
        let mut unknown_keys: Vec<String> = env
            .keys()
            .filter(|key| {
                key.starts_with("SYNCTV_")
                    && !seen_env_keys.contains(*key)
                    && !is_cli_only_synctv_env_var(key)
            })
            .cloned()
            .collect();
        unknown_keys.sort_unstable();
        unknown_keys
    }

    /// Load from environment variables only (for Docker/K8s)
    pub fn from_env() -> Result<Self, ConfigError> {
        let env: HashMap<String, String> = std::env::vars().collect();
        Self::load_with_env_map(None, &env)
    }

    pub fn from_env_map(env: &HashMap<String, String>) -> Result<Self, ConfigError> {
        Self::load_with_env_map(None, env)
    }

    /// Load from file path
    pub fn from_file(path: &str) -> Result<Self, ConfigError> {
        Self::load(Some(path))
    }

    /// Get database URL
    #[must_use]
    pub fn database_url(&self) -> String {
        if !self.database.url.trim().is_empty() {
            return self.database.url.clone();
        }

        if self.database.host.trim().is_empty()
            || self.database.port == 0
            || self.database.username.trim().is_empty()
            || self.database.name.trim().is_empty()
        {
            return String::new();
        }

        build_url_from_split_parts(
            "postgresql",
            &self.database.host,
            self.database.port,
            Some(&self.database.username),
            Some(&self.database.password),
            Some(&self.database.name),
        )
    }

    /// Get Redis URL
    #[must_use]
    pub fn redis_url(&self) -> String {
        if !self.redis.url.trim().is_empty() {
            return self.redis.url.clone();
        }

        if self.redis.host.trim().is_empty() || self.redis.port == 0 {
            return String::new();
        }

        if !self.redis.username.is_empty() {
            build_url_from_split_parts(
                "redis",
                &self.redis.host,
                self.redis.port,
                Some(&self.redis.username),
                Some(&self.redis.password),
                Some(&self.redis.database.to_string()),
            )
        } else if !self.redis.password.is_empty() {
            build_url_from_split_parts(
                "redis",
                &self.redis.host,
                self.redis.port,
                Some(""),
                Some(&self.redis.password),
                Some(&self.redis.database.to_string()),
            )
        } else {
            build_url_from_split_parts(
                "redis",
                &self.redis.host,
                self.redis.port,
                None,
                None,
                Some(&self.redis.database.to_string()),
            )
        }
    }

    /// Whether cross-replica cluster runtime is enabled.
    ///
    /// `cluster.enabled` is the single source of truth for activating
    /// multi-replica coordination (leader election, cluster pub/sub,
    /// discovery, and other cross-node runtime services). Standalone
    /// deployments may still configure Redis for caching and other
    /// non-cluster features.
    #[must_use]
    pub const fn cluster_runtime_enabled(&self) -> bool {
        self.cluster.enabled
    }

    /// Get unified API address
    #[must_use]
    pub fn api_address(&self) -> String {
        format!("{}:{}", self.server.host, self.server.port)
    }

    /// Get dedicated metrics address.
    #[must_use]
    pub fn metrics_address(&self) -> String {
        format!("{}:{}", self.metrics.host, self.metrics.port)
    }

    /// Get the dedicated management endpoint used by local/operational CLI commands.
    #[must_use]
    pub fn management_endpoint(&self) -> String {
        match self.management.transport {
            ManagementTransport::Tcp => format!("http://127.0.0.1:{}", self.management.port),
            ManagementTransport::Unix => format!("unix://{}", self.management.unix_socket_path),
        }
    }

    /// Get the management listener target for logs and binding.
    #[must_use]
    pub fn management_bind_target(&self) -> String {
        match self.management.transport {
            ManagementTransport::Tcp => format!("127.0.0.1:{}", self.management.port),
            ManagementTransport::Unix => self.management.unix_socket_path.clone(),
        }
    }

    /// Resolve the advertise host for cluster node registration.
    ///
    /// Priority: `server.advertise_host` config > `POD_IP` env var >
    /// system hostname > `server.host`.
    /// This address must be routable from other nodes (never `0.0.0.0`).
    #[must_use]
    pub fn advertise_host(&self) -> String {
        self.advertise_host_with(&process_env)
    }

    #[must_use]
    pub fn advertise_host_with_env_map(&self, env: &HashMap<String, String>) -> String {
        self.advertise_host_with(&|name| env.get(name).cloned())
    }

    pub(super) fn advertise_host_with(&self, get_env: &impl Fn(&str) -> Option<String>) -> String {
        // 1. Explicit config value (set via SYNCTV_SERVER_ADVERTISE_HOST)
        if !self.server.advertise_host.is_empty() {
            return self.server.advertise_host.clone();
        }

        // 2. POD_IP env var (set by Kubernetes downward API)
        if let Some(pod_ip) = get_env("POD_IP").filter(|value| !value.is_empty()) {
            return pod_ip;
        }

        // 3. System hostname (local-only fallback; avoids external network dependency)
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| self.server.host.clone())
    }

    pub(super) fn has_explicit_advertise_host_source(
        &self,
        get_env: &impl Fn(&str) -> Option<String>,
    ) -> bool {
        !self.server.advertise_host.is_empty()
            || get_env("POD_IP").is_some_and(|value| !value.is_empty())
    }

    fn local_publish_host(&self) -> String {
        let host = match self.server.host.trim() {
            "" | "0.0.0.0" => "127.0.0.1".to_string(),
            "::" | "[::]" => "::1".to_string(),
            host => host.to_string(),
        };

        if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host
        }
    }

    /// Get the unified API address advertised to other cluster nodes.
    #[must_use]
    pub fn advertise_api_address(&self) -> String {
        format!("{}:{}", self.advertise_host(), self.server.port)
    }

    /// Public RTMP host for publisher-facing URLs.
    #[must_use]
    pub fn public_rtmp_host(&self) -> String {
        self.public_rtmp_host_without_internal_advertise_fallback()
    }

    pub(super) fn public_rtmp_host_without_internal_advertise_fallback(&self) -> String {
        if !self.livestream.public_rtmp_host.is_empty() {
            return self.livestream.public_rtmp_host.clone();
        }

        self.local_publish_host()
    }
}
