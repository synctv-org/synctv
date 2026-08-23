use super::*;
use synctv_api::validate_cors_origin;

fn validate_project_url(project_url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(project_url)
        .map_err(|error| format!("server.project_url is not a valid URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("server.project_url must use http:// or https://".to_string());
    }
    if parsed.host_str().is_none() {
        return Err("server.project_url must include a host".to_string());
    }
    Ok(())
}

fn valid_apple_application_identifier(value: &str) -> bool {
    if value != value.trim() {
        return false;
    }
    let Some((team_id, bundle_id)) = value.split_once('.') else {
        return false;
    };
    team_id.len() == 10
        && team_id
            .chars()
            .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
        && valid_reverse_dns_identifier(bundle_id, true)
}

fn valid_android_package_name(value: &str) -> bool {
    if value != value.trim() {
        return false;
    }
    value.split('.').count() >= 2 && valid_reverse_dns_identifier(value, false)
}

fn valid_reverse_dns_identifier(value: &str, allow_hyphen: bool) -> bool {
    value.split('.').all(|segment| {
        let mut characters = segment.chars();
        characters
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
            && characters.all(|character| {
                character.is_ascii_alphanumeric()
                    || character == '_'
                    || (allow_hyphen && character == '-')
            })
    })
}

fn valid_sha256_certificate_fingerprint(value: &str) -> bool {
    let compact = value.trim().replace(':', "");
    compact.len() == 64
        && compact
            .chars()
            .all(|character| character.is_ascii_hexdigit())
}

fn validate_hex_encryption_key(path: &str, value: &str, errors: &mut Vec<String>) {
    let key = value.trim();
    if key.len() != 64 {
        errors.push(format!(
            "{path} must be a 64-character hex string (32 bytes for AES-256-GCM)"
        ));
    } else if !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        errors.push(format!("{path} must contain only hexadecimal characters"));
    }
}

fn validate_random_secret(path: &str, value: &str, errors: &mut Vec<String>) {
    let secret = value.trim();
    if secret.len() < 32 {
        errors.push(format!(
            "{path} must contain at least 32 characters of random key material"
        ));
    } else if secret == "change-me-in-production" || secret.starts_with("CHANGE_ME_") {
        errors.push(format!("{path} appears to be a placeholder"));
    }
}

fn is_unspecified_host(host: &str) -> bool {
    matches!(host.trim(), "0.0.0.0" | "::" | "[::]")
}

impl AppConfig {
    fn database_split_config_present(&self) -> bool {
        !self.database.host.trim().is_empty()
            || self.database.port != 0
            || !self.database.username.trim().is_empty()
            || !self.database.password.trim().is_empty()
            || !self.database.name.trim().is_empty()
    }

    fn validate_database_split_config(&self, errors: &mut Vec<String>) {
        if self.database.host.trim().is_empty() {
            errors.push(
                "database.host must be set when using split database configuration".to_string(),
            );
        }
        if self.database.port == 0 {
            errors.push(
                "database.port must be greater than 0 when using split database configuration"
                    .to_string(),
            );
        }
        if self.database.username.trim().is_empty() {
            errors.push(
                "database.username must be set when using split database configuration".to_string(),
            );
        }
        if self.database.password.trim().is_empty() {
            errors.push(
                "database.password must be set when using split database configuration".to_string(),
            );
        }
        if self.database.name.trim().is_empty() {
            errors.push(
                "database.name must be set when using split database configuration".to_string(),
            );
        }
    }

    fn redis_split_config_present(&self) -> bool {
        !self.redis.host.trim().is_empty()
            || self.redis.port != 0
            || !self.redis.username.trim().is_empty()
            || !self.redis.password.trim().is_empty()
            || self.redis.database != 0
    }

    fn validate_redis_split_config(&self, errors: &mut Vec<String>) {
        if self.redis.host.trim().is_empty() {
            errors.push("redis.host must be set when using split redis configuration".to_string());
        }
        if self.redis.port == 0 {
            errors.push(
                "redis.port must be greater than 0 when using split redis configuration"
                    .to_string(),
            );
        }
    }

    /// Validate configuration at startup (fail fast on misconfigurations)
    pub fn validate(&self) -> Result<(), Vec<String>> {
        self.validate_core()
    }

    /// Collect non-blocking configuration diagnostics for emission after logging is ready.
    pub(crate) fn startup_diagnostics(&self) -> Vec<StartupDiagnostic> {
        let mut diagnostics = Vec::new();

        if self.jwt.secret == "change-me-in-production"
            || self.jwt.secret.starts_with("CHANGE_ME_")
            || is_known_dev_secret(&self.jwt.secret, KNOWN_DEV_JWT_SECRETS)
        {
            diagnostics.push(StartupDiagnostic::Warning(
                "JWT secret appears to be a placeholder. Set SYNCTV_JWT_SECRET to a strong random value (openssl rand -base64 48)".to_string(),
            ));
        }

        if self.redis.url.trim().is_empty()
            && !self.redis_split_config_present()
            && !self.cluster_runtime_enabled()
        {
            diagnostics.push(StartupDiagnostic::Info(
                "Redis is not configured - running in standalone mode with in-memory fallbacks. All features work, but data (rate limits, brute-force counters, token blacklist) will not persist across restarts.".to_string(),
            ));
        }

        if self.cluster_runtime_enabled() {
            match self.livestream.hls_storage.backend() {
                HlsStorageBackend::S3 => {}
                HlsStorageBackend::SharedFile => {
                    let path = self.livestream.hls_storage.path();
                    let is_obviously_local = path.starts_with("/tmp/")
                        || path == "/tmp"
                        || path.starts_with("/var/tmp/")
                        || path.starts_with("/dev/shm/");
                    if is_obviously_local {
                        diagnostics.push(StartupDiagnostic::Warning(format!(
                            "livestream.hls_storage.type='shared_file' but hls_storage.path '{path}' appears to be a local-only path. Ensure this path is actually mounted from shared storage (NFS, CSI volume) on every replica. Otherwise remote HLS segment requests will read from a path that is not shared."
                        )));
                    }
                }
                HlsStorageBackend::File => diagnostics.push(StartupDiagnostic::Warning(
                    "Cluster mode is enabled with livestream.hls_storage.type='file'. HLS remains functional through publisher-node proxying, but shared_file or S3 is recommended for production multi-replica HLS.".to_string(),
                )),
                HlsStorageBackend::Memory => diagnostics.push(StartupDiagnostic::Warning(
                    "Cluster mode is enabled with livestream.hls_storage.type='memory'. HLS remains functional through publisher-node proxying, but memory storage is node-local and lost on restart. Use livestream.hls_storage.type='shared_file' or 's3' for production multi-replica HLS.".to_string(),
                )),
            }
        } else if self.livestream.hls_storage.backend() == HlsStorageBackend::Memory {
            diagnostics.push(StartupDiagnostic::Warning(
                "The default HLS storage backend is MemoryStorage, which is node-local. HLS segments are lost on restart. For production multi-replica HLS, configure livestream.hls_storage.type='shared_file' with shared filesystem storage or livestream.hls_storage.type='s3' with S3-compatible object storage.".to_string(),
            ));
        }

        if self.server.cors_allowed_origins.is_empty() {
            diagnostics.push(StartupDiagnostic::Warning(
                "server.cors_allowed_origins is empty. CORS requests will be rejected. Set allowed origins.".to_string(),
            ));
        }

        if self.webrtc.enable_builtin_stun && self.webrtc.stun_external_addr.is_empty() {
            let message = if self.cluster_runtime_enabled() {
                "webrtc.enable_builtin_stun=true but stun_external_addr is not set in cluster mode. Startup will attempt STUN external address auto-detection from advertise_host, STUN_EXTERNAL_IP, or cloud metadata, and will skip the built-in STUN server if no public address is found. For deterministic production behavior, set webrtc.stun_external_addr to a client-reachable public ip:port or DNS name:port."
            } else {
                "webrtc.enable_builtin_stun=true but stun_external_addr is not set. Startup will try advertise_host, STUN_EXTERNAL_IP, and cloud metadata, and will skip the built-in STUN server if no public address is found. Set webrtc.stun_external_addr to the server's public ip:port or DNS name:port."
            };
            diagnostics.push(StartupDiagnostic::Warning(message.to_string()));
        }

        diagnostics
    }

    fn validate_local_provider_http_config(
        path: &str,
        config: &LocalProviderHttpConfig,
        errors: &mut Vec<String>,
    ) {
        if config.request_timeout_seconds > 300 {
            errors.push(format!(
                "{path}.request_timeout_seconds should not exceed 300 seconds (5 minutes)"
            ));
        }
        if config.request_timeout_seconds == 0 {
            errors.push(format!(
                "{path}.request_timeout_seconds must be greater than 0"
            ));
        }
        if config.connect_timeout_seconds == 0 {
            errors.push(format!(
                "{path}.connect_timeout_seconds must be greater than 0"
            ));
        }
        if config.connect_timeout_seconds > config.request_timeout_seconds
            && config.request_timeout_seconds > 0
        {
            errors.push(format!(
                "{path}.connect_timeout_seconds should not exceed request_timeout_seconds"
            ));
        }
    }

    fn validate_rate_limit_pair(
        path: &str,
        max_requests: u32,
        window_seconds: u64,
        errors: &mut Vec<String>,
    ) {
        if max_requests == 0 {
            errors.push(format!("{path}.max_requests must be greater than 0"));
        }
        if window_seconds == 0 {
            errors.push(format!("{path}.window_seconds must be greater than 0"));
        }
    }

    fn validate_rate_limit_scopes(
        path: &str,
        scopes: &HashMap<String, RateLimitScopeRule>,
        errors: &mut Vec<String>,
    ) {
        for (scope, rule) in scopes {
            let scope_path = format!("{path}.scopes.{scope}");
            if scope.trim().is_empty() {
                errors.push(format!("{path}.scopes contains an empty scope name"));
            }
            if matches!(rule.max_requests, Some(0)) {
                errors.push(format!("{scope_path}.max_requests must be greater than 0"));
            }
            if matches!(rule.window_seconds, Some(0)) {
                errors.push(format!(
                    "{scope_path}.window_seconds must be greater than 0"
                ));
            }
        }
    }

    fn validate_api_rate_limits(&self, errors: &mut Vec<String>) {
        let config = &self.request_rate_limits;
        Self::validate_rate_limit_pair(
            "request_rate_limits.auth",
            config.auth_max_requests,
            config.auth_window_seconds,
            errors,
        );
        Self::validate_rate_limit_pair(
            "request_rate_limits.write",
            config.write_max_requests,
            config.write_window_seconds,
            errors,
        );
        Self::validate_rate_limit_pair(
            "request_rate_limits.read",
            config.read_max_requests,
            config.read_window_seconds,
            errors,
        );
        Self::validate_rate_limit_pair(
            "request_rate_limits.media",
            config.media_max_requests,
            config.media_window_seconds,
            errors,
        );
        Self::validate_rate_limit_pair(
            "request_rate_limits.admin",
            config.admin_max_requests,
            config.admin_window_seconds,
            errors,
        );
        Self::validate_rate_limit_pair(
            "request_rate_limits.streaming",
            config.streaming_max_requests,
            config.streaming_window_seconds,
            errors,
        );
        Self::validate_rate_limit_pair(
            "request_rate_limits.websocket",
            config.websocket_max_requests,
            config.websocket_window_seconds,
            errors,
        );
        Self::validate_rate_limit_scopes("request_rate_limits", &config.scopes, errors);
    }

    fn validate_core(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.password_complexity.zxcvbn_min_score > 4 {
            errors.push("password_complexity.zxcvbn_min_score must be between 0 and 4".to_string());
        }

        validate_hex_encryption_key(
            "security.credential_encryption_key",
            &self.security.credential_encryption_key,
            &mut errors,
        );
        if is_known_dev_hex_secret(
            self.security.credential_encryption_key.trim(),
            KNOWN_DEV_CREDENTIAL_ENCRYPTION_KEYS,
        ) {
            errors.push("security.credential_encryption_key uses a known development value. Generate a unique key with `openssl rand -hex 32`".to_string());
        }
        validate_hex_encryption_key(
            "security.totp_encryption_key",
            &self.security.totp_encryption_key,
            &mut errors,
        );
        validate_hex_encryption_key(
            "security.email_outbox_encryption_key",
            &self.security.email_outbox_encryption_key,
            &mut errors,
        );

        let opaque_secret = self.security.opaque_server_setup_secret.trim();
        if opaque_secret.is_empty() {
            errors.push("security.opaque_server_setup_secret is empty. Set SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET or SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET_FILE to a stable random value".to_string());
        } else if opaque_secret == "change-me-in-production"
            || opaque_secret.starts_with("CHANGE_ME_")
            || is_known_dev_secret(opaque_secret, KNOWN_DEV_OPAQUE_SERVER_SETUP_SECRETS)
        {
            errors.push("security.opaque_server_setup_secret appears to be a placeholder. Set it to a stable random value (openssl rand -base64 48)".to_string());
        } else if opaque_secret.len() < 32 {
            errors.push(format!(
                "security.opaque_server_setup_secret is too short ({} chars). Minimum 32 characters required for OPAQUE setup stability.",
                opaque_secret.len()
            ));
        }
        validate_random_secret(
            "security.proxy_signing_key",
            &self.security.proxy_signing_key,
            &mut errors,
        );
        validate_random_secret(
            "security.media_swarm_signing_key",
            &self.security.media_swarm_signing_key,
            &mut errors,
        );
        validate_random_secret(
            "security.provider_session_encryption_key",
            &self.security.provider_session_encryption_key,
            &mut errors,
        );
        validate_random_secret(
            "security.login_discovery_key",
            &self.security.login_discovery_key,
            &mut errors,
        );
        validate_random_secret(
            "security.webauthn_enumeration_key",
            &self.security.webauthn_enumeration_key,
            &mut errors,
        );
        validate_random_secret(
            "file_storage.upload_token_secret",
            &self.file_storage.upload_token_secret,
            &mut errors,
        );

        let secrets = [
            ("jwt.secret", self.jwt.secret.trim()),
            ("cluster.secret", self.cluster.secret.trim()),
            (
                "security.credential_encryption_key",
                self.security.credential_encryption_key.trim(),
            ),
            (
                "security.totp_encryption_key",
                self.security.totp_encryption_key.trim(),
            ),
            (
                "security.email_outbox_encryption_key",
                self.security.email_outbox_encryption_key.trim(),
            ),
            (
                "security.opaque_server_setup_secret",
                self.security.opaque_server_setup_secret.trim(),
            ),
            (
                "security.proxy_signing_key",
                self.security.proxy_signing_key.trim(),
            ),
            (
                "security.media_swarm_signing_key",
                self.security.media_swarm_signing_key.trim(),
            ),
            (
                "security.provider_session_encryption_key",
                self.security.provider_session_encryption_key.trim(),
            ),
            (
                "security.login_discovery_key",
                self.security.login_discovery_key.trim(),
            ),
            (
                "security.webauthn_enumeration_key",
                self.security.webauthn_enumeration_key.trim(),
            ),
            (
                "file_storage.upload_token_secret",
                self.file_storage.upload_token_secret.trim(),
            ),
        ];
        for (index, (left_path, left_value)) in secrets.iter().enumerate() {
            if left_value.is_empty() {
                continue;
            }
            for (right_path, right_value) in &secrets[index + 1..] {
                let same_hex_key = left_value.len() == 64
                    && right_value.len() == 64
                    && left_value.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && right_value.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && left_value.eq_ignore_ascii_case(right_value);
                if left_value == right_value || same_hex_key {
                    errors.push(format!(
                        "{left_path} and {right_path} must use different secret values"
                    ));
                }
            }
        }
        for range in &self.security.ssrf.allowed_ip_ranges {
            if range.parse::<ipnet::IpNet>().is_err() {
                errors.push(format!(
                    "security.ssrf.allowed_ip_ranges contains invalid CIDR/IP range: {range}"
                ));
            }
        }

        // Validate database pool settings
        if self.database.min_connections > self.database.max_connections {
            errors.push(format!(
                "database.min_connections ({}) must not exceed database.max_connections ({})",
                self.database.min_connections, self.database.max_connections
            ));
        }
        if self.database.max_connections == 0 {
            errors.push("database.max_connections must be greater than 0".to_string());
        }
        if self.database.connect_timeout_seconds == 0 {
            errors.push("database.connect_timeout_seconds must be greater than 0".to_string());
        }
        if self.database.idle_timeout_seconds == 0 {
            errors.push("database.idle_timeout_seconds must be greater than 0".to_string());
        }
        if self.database.max_lifetime_seconds == 0 {
            errors.push("database.max_lifetime_seconds must be greater than 0".to_string());
        }
        let database_url_present = !self.database.url.trim().is_empty();
        let database_split_present = self.database_split_config_present();
        if database_url_present && database_split_present {
            errors.push(
                "database.url is mutually exclusive with database.host/port/user/password/name"
                    .to_string(),
            );
        }
        if !database_url_present && !database_split_present {
            errors.push(
                "database configuration requires either database.url or database.host/port/user/password/name"
                    .to_string(),
            );
        }
        if !database_url_present && database_split_present {
            self.validate_database_split_config(&mut errors);
        }

        if self.server.shutdown_drain_timeout_seconds == 0 {
            errors.push("server.shutdown_drain_timeout_seconds must be greater than 0".to_string());
        }
        if let Err(error) = validate_project_url(&self.server.project_url) {
            errors.push(error);
        }

        if self.metrics.enabled {
            match self.metrics.auth.mode {
                MetricsAuthMode::BearerToken => {
                    if self.metrics.auth.bearer_token.trim().is_empty() {
                        errors.push(
                            "metrics.auth.bearer_token must be set when metrics.enabled=true \
                             and metrics.auth.mode='bearer_token'"
                                .to_string(),
                        );
                    }
                }
                MetricsAuthMode::Basic => {
                    if self.metrics.auth.basic_username.trim().is_empty() {
                        errors.push(
                            "metrics.auth.basic_username must be set when metrics.enabled=true \
                             and metrics.auth.mode='basic'"
                                .to_string(),
                        );
                    }
                    if self.metrics.auth.basic_password.trim().is_empty() {
                        errors.push(
                            "metrics.auth.basic_password must be set when metrics.enabled=true \
                             and metrics.auth.mode='basic'"
                                .to_string(),
                        );
                    }
                }
                MetricsAuthMode::Kubernetes => {
                    #[cfg(not(feature = "k8s"))]
                    errors.push(
                        "metrics.auth.mode='kubernetes' requires the 'k8s' feature to be compiled in"
                            .to_string(),
                    );

                    if self
                        .metrics
                        .auth
                        .kubernetes
                        .authentication_cache_ttl_seconds
                        == 0
                    {
                        errors.push(
                            "metrics.auth.kubernetes.authentication_cache_ttl_seconds must be greater than 0"
                                .to_string(),
                        );
                    }
                    if self.metrics.auth.kubernetes.authorization_cache_ttl_seconds == 0 {
                        errors.push(
                            "metrics.auth.kubernetes.authorization_cache_ttl_seconds must be greater than 0"
                                .to_string(),
                        );
                    }
                }
            }

            if self.metrics.tls.enabled {
                if self.metrics.tls.cert_path.trim().is_empty() {
                    errors.push(
                        "metrics.tls.cert_path must be set when metrics.tls.enabled=true"
                            .to_string(),
                    );
                }
                if self.metrics.tls.key_path.trim().is_empty() {
                    errors.push(
                        "metrics.tls.key_path must be set when metrics.tls.enabled=true"
                            .to_string(),
                    );
                }
            }
        }

        let mut listener_ports = vec![
            ("server.port", self.server.port),
            ("livestream.rtmp_port", self.livestream.rtmp_port),
        ];
        if self.health.enabled {
            listener_ports.push(("health.port", self.health.port));
        }
        if self.metrics.enabled {
            listener_ports.push(("metrics.port", self.metrics.port));
        }
        if self.cluster.enabled {
            listener_ports.push(("cluster.port", self.cluster.port));
        }
        if self.management.enabled && matches!(self.management.transport, ManagementTransport::Tcp)
        {
            listener_ports.push(("management.port", self.management.port));
        }
        for (index, (left_name, left_port)) in listener_ports.iter().enumerate() {
            for (right_name, right_port) in listener_ports.iter().skip(index + 1) {
                if *left_port != 0 && left_port == right_port {
                    errors.push(format!(
                        "{left_name} and {right_name} must use different ports ({left_port})"
                    ));
                }
            }
        }

        for (path, logging) in [
            ("logging", &self.logging),
            ("server.logging", &self.server.logging),
            ("server.access_log", &self.server.access_log.logging),
            ("health.logging", &self.health.logging),
            ("metrics.logging", &self.metrics.logging),
            ("cluster.logging", &self.cluster.logging),
            ("management.logging", &self.management.logging),
            ("livestream.logging", &self.livestream.logging),
            ("webrtc.logging", &self.webrtc.logging),
        ] {
            if logging.level.parse::<tracing::Level>().is_err() {
                errors.push(format!(
                    "{path}.level '{}' must be one of: trace, debug, info, warn, error",
                    logging.level
                ));
            }
            if !matches!(logging.format.as_str(), "text" | "json") {
                errors.push(format!(
                    "{path}.format '{}' must be text or json",
                    logging.format
                ));
            }
            if let LogOutput::File(output) = &logging.output {
                if output.r#type != "file" {
                    errors.push(format!("{path}.output.type must be file"));
                }
                if output.path.trim().is_empty() {
                    errors.push(format!("{path}.output.path must not be empty"));
                }
                if !matches!(
                    output.rotation.strategy.as_str(),
                    "daily" | "hourly" | "never"
                ) {
                    errors.push(format!(
                        "{path}.output.rotation.strategy must be daily, hourly, or never"
                    ));
                }
                if output.rotation.max_files == 0 {
                    errors.push(format!(
                        "{path}.output.rotation.max_files must be at least 1"
                    ));
                }
            }
        }

        // Validate JWT secret
        if self.jwt.secret.is_empty() {
            errors.push("JWT secret is empty".to_string());
        } else if self.jwt.secret.chars().count() < 32 {
            errors.push(format!(
                "JWT secret is too short ({} chars). Minimum 32 characters required for security. \
                 Set SYNCTV_JWT_SECRET to a strong random value.",
                self.jwt.secret.chars().count()
            ));
        }

        // Validate root identity. Password validation runs only when bootstrap
        // confirms it needs to create a root user.
        if self.bootstrap.create_root_user && self.bootstrap.root_username.len() < 3 {
            errors.push("Root username must be at least 3 characters".to_string());
        }

        // Validate remote transport max message size (prevent OOM attacks)
        if self.server.grpc_max_message_size_bytes < MIN_GRPC_MESSAGE_SIZE {
            errors.push(format!(
                "server.grpc_max_message_size_bytes ({}) must be at least {} (1 MB)",
                self.server.grpc_max_message_size_bytes, MIN_GRPC_MESSAGE_SIZE
            ));
        }
        if self.server.grpc_max_message_size_bytes > MAX_GRPC_MESSAGE_SIZE {
            errors.push(format!(
                "server.grpc_max_message_size_bytes ({}) must be at most {} (1 GB)",
                self.server.grpc_max_message_size_bytes, MAX_GRPC_MESSAGE_SIZE
            ));
        }

        if self.data_dir.trim().is_empty() {
            errors.push("data_dir must not be empty".to_string());
        } else if !Path::new(&self.data_dir).is_absolute() {
            errors.push("data_dir must be an absolute path".to_string());
        }

        if self.management.enabled {
            match self.management.transport {
                ManagementTransport::Tcp => {
                    if self.management.port != 0 && self.management.port == self.server.port {
                        errors.push(format!(
                            "management bind target ({}) must be different from server.host:port ({})",
                            self.management_bind_target(),
                            self.api_address()
                        ));
                    }
                    if self.management.auth_token.trim().is_empty() {
                        errors.push(
                            "management.auth_token must be set when management.enabled=true \
                             and management.transport='tcp'"
                                .to_string(),
                        );
                    }
                }
                ManagementTransport::Unix => {
                    if self.management.unix_socket_path.trim().is_empty() {
                        errors.push(
                            "management.unix_socket_path must not be empty when transport=unix"
                                .to_string(),
                        );
                    } else if !Path::new(&self.management.unix_socket_path).is_absolute() {
                        errors.push(
                            "management.unix_socket_path must be an absolute path".to_string(),
                        );
                    }

                    if !cfg!(unix) {
                        errors.push(
                            "management.transport=unix is only supported on unix-like platforms"
                                .to_string(),
                        );
                    }
                }
            }
        }

        // Validate trusted_proxies CIDR format
        for (i, proxy) in self.server.trusted_proxies.iter().enumerate() {
            // Check for dangerous overly-broad CIDR ranges first
            let proxy_normalized = proxy.replace(' ', "").to_lowercase();
            if DANGEROUS_CIDR_RANGES
                .iter()
                .any(|dangerous| proxy_normalized == dangerous.replace(' ', "").to_lowercase())
            {
                errors.push(format!(
                    "server.trusted_proxies[{i}] '{proxy}' is a dangerous configuration \
                     that trusts ALL IP addresses. This allows IP spoofing attacks via \
                     X-Forwarded-For headers. Use specific IP ranges (e.g., '10.0.0.0/8', \
                     '172.16.0.0/12', '192.168.0.0/16') for your trusted proxies/_load balancers."
                ));
                continue;
            }

            // Also check if a CIDR covers all addresses (e.g., "0.0.0.0/0" parsed)
            if let Ok(network) = proxy.parse::<ipnet::IpNet>() {
                // Check if the network covers all possible addresses
                match network {
                    ipnet::IpNet::V4(v4) => {
                        if v4.prefix_len() == 0 {
                            errors.push(format!(
                                "server.trusted_proxies[{i}] '{proxy}' covers all IPv4 addresses (/{prefix}). \
                                 This allows IP spoofing attacks via X-Forwarded-For headers. \
                                 Use specific IP ranges for your trusted proxies/load balancers.",
                                prefix = v4.prefix_len()
                            ));
                        }
                    }
                    ipnet::IpNet::V6(v6) => {
                        if v6.prefix_len() == 0 {
                            errors.push(format!(
                                "server.trusted_proxies[{i}] '{proxy}' covers all IPv6 addresses (/{prefix}). \
                                 This allows IP spoofing attacks via X-Forwarded-For headers. \
                                 Use specific IP ranges for your trusted proxies/load balancers.",
                                prefix = v6.prefix_len()
                            ));
                        }
                    }
                }
            }

            // Each entry must be a valid CIDR notation or IP address
            if proxy.parse::<ipnet::IpNet>().is_err() && proxy.parse::<std::net::IpAddr>().is_err()
            {
                errors.push(format!(
                    "server.trusted_proxies[{i}] '{proxy}' is not a valid CIDR or IP address"
                ));
            }
        }

        // Validate Redis Sentinel mode required fields
        if self.redis.deployment_mode == RedisDeploymentMode::Sentinel {
            if self.redis.sentinel_master_name.is_none()
                || self
                    .redis
                    .sentinel_master_name
                    .as_ref()
                    .is_none_or(std::string::String::is_empty)
            {
                errors.push(
                    "redis.sentinel_master_name is required when deployment_mode is 'sentinel'"
                        .to_string(),
                );
            }
            if self.redis.sentinel_addresses.is_empty() {
                errors.push(
                    "redis.sentinel_addresses cannot be empty when deployment_mode is 'sentinel'"
                        .to_string(),
                );
            }
        }

        // Log info when Redis is not configured in standalone mode.
        let redis_url_present = !self.redis.url.trim().is_empty();
        let redis_split_present = self.redis_split_config_present();
        if redis_url_present && redis_split_present {
            errors.push(
                "redis.url is mutually exclusive with redis.host/port/user/password/database"
                    .to_string(),
            );
        }
        if !redis_url_present && redis_split_present {
            self.validate_redis_split_config(&mut errors);
        }

        if self.redis.connect_timeout_seconds == 0 {
            errors.push("redis.connect_timeout_seconds must be greater than 0".to_string());
        }
        if self.redis.response_timeout_seconds == 0 {
            errors.push("redis.response_timeout_seconds must be greater than 0".to_string());
        }
        if self.redis.pipeline_buffer_size == 0 {
            errors.push("redis.pipeline_buffer_size must be greater than 0".to_string());
        }

        // Validate connection limits
        if self.connection_limits.max_per_user == 0 {
            errors.push("connection_limits.max_per_user must be greater than 0".to_string());
        }
        if self.connection_limits.max_per_room == 0 {
            errors.push("connection_limits.max_per_room must be greater than 0".to_string());
        }
        if self.connection_limits.max_total == 0 {
            errors.push("connection_limits.max_total must be greater than 0".to_string());
        }
        if self.connection_limits.max_per_user > self.connection_limits.max_total {
            errors.push(format!(
                "connection_limits.max_per_user ({}) must not exceed connection_limits.max_total ({})",
                self.connection_limits.max_per_user, self.connection_limits.max_total
            ));
        }
        if self.connection_limits.max_per_room > self.connection_limits.max_total {
            errors.push(format!(
                "connection_limits.max_per_room ({}) must not exceed connection_limits.max_total ({})",
                self.connection_limits.max_per_room, self.connection_limits.max_total
            ));
        }
        if self.connection_limits.idle_timeout_seconds > 0
            && self.connection_limits.idle_timeout_seconds < 10
        {
            errors.push(
                "connection_limits.idle_timeout_seconds must be at least 10 seconds".to_string(),
            );
        }
        if self.connection_limits.ws_message_rate_limit_per_second == 0 {
            errors.push(
                "connection_limits.ws_message_rate_limit_per_second must be greater than 0"
                    .to_string(),
            );
        }
        if self.messaging_rate_limits.chat_per_second == 0 {
            errors.push("messaging_rate_limits.chat_per_second must be greater than 0".to_string());
        }
        if self.messaging_rate_limits.window_seconds == 0 {
            errors.push("messaging_rate_limits.window_seconds must be greater than 0".to_string());
        }
        self.validate_api_rate_limits(&mut errors);

        // Validate livestream config
        if self.livestream.stream_timeout_seconds == 0 {
            errors.push("livestream.stream_timeout_seconds must be greater than 0".to_string());
        }
        if self.livestream.cleanup_check_interval_seconds == 0 {
            errors.push(
                "livestream.cleanup_check_interval_seconds must be greater than 0".to_string(),
            );
        }

        Self::validate_local_provider_http_config(
            "media_providers.alist",
            &self.media_providers.alist,
            &mut errors,
        );
        Self::validate_local_provider_http_config(
            "media_providers.bilibili",
            &self.media_providers.bilibili,
            &mut errors,
        );
        Self::validate_local_provider_http_config(
            "media_providers.emby",
            &self.media_providers.emby,
            &mut errors,
        );
        Self::validate_local_provider_http_config(
            "media_providers.cloudreve",
            &self.media_providers.cloudreve,
            &mut errors,
        );

        // Validate CORS origins
        if !self.server.cors_allowed_origins.is_empty() {
            for origin in &self.server.cors_allowed_origins {
                if origin == "*" {
                    errors.push(
                        "CORS wildcard '*' is not allowed. Specify exact origins.".to_string(),
                    );
                    break;
                }
                if let Err(error) = validate_cors_origin(origin) {
                    errors.push(error);
                }
            }
        }

        if self.time.clock_sync.enabled {
            match &self.time.clock_sync.provider {
                ClockSyncProvider::Sntp(config) => {
                    if config.servers.iter().all(|server| server.trim().is_empty()) {
                        errors.push(
                            "time.clock_sync.provider.servers must contain at least one server when clock sync is enabled"
                                .to_string(),
                        );
                    }
                    if config.interval_seconds == 0 {
                        errors.push(
                            "time.clock_sync.provider.interval_seconds must be greater than 0"
                                .to_string(),
                        );
                    }
                    if config.timeout_millis == 0 {
                        errors.push(
                            "time.clock_sync.provider.timeout_millis must be greater than 0"
                                .to_string(),
                        );
                    }
                }
            }
        }

        match &self.livestream.hls_storage {
            HlsStorageConfig::Memory(_) => {}
            HlsStorageConfig::File(config) | HlsStorageConfig::SharedFile(config) => {
                if config.path.trim().is_empty() {
                    errors.push(
                        "livestream.hls_storage.path must be set when livestream.hls_storage.type is 'file' or 'shared_file'"
                            .to_string(),
                    );
                }
            }
            HlsStorageConfig::S3(config) => {
                if config.endpoint.trim().is_empty() {
                    errors.push(
                        "livestream.hls_storage.endpoint must be set when livestream.hls_storage.type='s3'"
                            .to_string(),
                    );
                }
                if config.bucket.trim().is_empty() {
                    errors.push(
                        "livestream.hls_storage.bucket must be set when livestream.hls_storage.type='s3'"
                            .to_string(),
                    );
                }
                if config.access_key_id.trim().is_empty() {
                    errors.push(
                        "livestream.hls_storage.access_key_id must be set when livestream.hls_storage.type='s3'"
                            .to_string(),
                    );
                }
                if config.secret_access_key.trim().is_empty() {
                    errors.push(
                        "livestream.hls_storage.secret_access_key must be set when livestream.hls_storage.type='s3'"
                            .to_string(),
                    );
                }
            }
        }

        let mut validate_selected_file_backend = |field: &str, name: &str| {
            let name = name.trim();
            if name.is_empty() {
                errors.push(format!("{field} must not be empty"));
            } else if name != "disabled" && !self.file_storage.backends.contains_key(name) {
                errors.push(format!(
                    "{field} references unknown file storage backend '{name}'"
                ));
            }
        };
        validate_selected_file_backend(
            "file_storage.default_backend",
            &self.file_storage.default_backend,
        );
        validate_selected_file_backend(
            "file_storage.chat_attachments_backend",
            self.file_storage.backend_for_chat_attachments(),
        );
        validate_selected_file_backend(
            "file_storage.user_avatars_backend",
            self.file_storage.backend_for_user_avatars(),
        );
        validate_selected_file_backend(
            "file_storage.media_covers_backend",
            self.file_storage.backend_for_media_covers(),
        );
        validate_selected_file_backend(
            "file_storage.room_covers_backend",
            self.file_storage.backend_for_room_covers(),
        );
        validate_selected_file_backend(
            "file_storage.playlist_covers_backend",
            self.file_storage.backend_for_playlist_covers(),
        );

        for (name, backend) in &self.file_storage.backends {
            let trimmed_name = name.trim();
            if trimmed_name.is_empty() || trimmed_name != name {
                errors
                    .push("file_storage.backends keys must be non-empty trimmed names".to_string());
            }
            if name == "disabled" && !matches!(backend, FileStorageBackendConfig::Disabled) {
                errors.push("file_storage.backends.disabled must use type='disabled'".to_string());
            }
            match backend {
                FileStorageBackendConfig::Disabled => {}
                FileStorageBackendConfig::Database(database) => {
                    if database.compression_min_size_bytes < 0 {
                        errors.push(format!(
                            "file_storage.backends.{name}.compression_min_size_bytes must be non-negative"
                        ));
                    }
                    if database.compression_min_savings_percent > 100 {
                        errors.push(format!(
                            "file_storage.backends.{name}.compression_min_savings_percent must be between 0 and 100"
                        ));
                    }
                }
                FileStorageBackendConfig::S3(s3) => {
                    if s3.endpoint.trim().is_empty() {
                        errors.push(format!(
                            "file_storage.backends.{name}.endpoint must be set when type='s3'"
                        ));
                    }
                    if s3.bucket.trim().is_empty() {
                        errors.push(format!(
                            "file_storage.backends.{name}.bucket must be set when type='s3'"
                        ));
                    }
                    if s3.access_key_id.trim().is_empty() {
                        errors.push(format!(
                            "file_storage.backends.{name}.access_key_id must be set when type='s3'"
                        ));
                    }
                    if s3.secret_access_key.trim().is_empty() {
                        errors.push(format!(
                            "file_storage.backends.{name}.secret_access_key must be set when type='s3'"
                        ));
                    }
                    if s3.region.trim().is_empty() {
                        errors.push(format!(
                            "file_storage.backends.{name}.region must be set when type='s3'"
                        ));
                    }
                    if s3
                        .public_base_url
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty())
                    {
                        errors.push(format!(
                            "file_storage.backends.{name}.public_base_url must be set when type='s3'"
                        ));
                    }
                    if s3.upload_expires_seconds <= 0 {
                        errors.push(format!(
                            "file_storage.backends.{name}.upload_expires_seconds must be greater than 0"
                        ));
                    }
                }
            }
        }
        if self.file_storage.unreferenced_object_retention_seconds > 0
            && self.file_storage.unreferenced_object_retention_seconds < 3600
        {
            errors.push(
                "file_storage.unreferenced_object_retention_seconds must be 0 or at least 3600"
                    .to_string(),
            );
        }

        if self.proxy_slice_cache.file_backend_enabled {
            if self.proxy_slice_cache.file_cache_dir.trim().is_empty() {
                errors.push(
                    "proxy_slice_cache.file_cache_dir must not be empty when proxy_slice_cache.file_backend_enabled=true"
                        .to_string(),
                );
            } else if !Path::new(&self.proxy_slice_cache.file_cache_dir).is_absolute() {
                errors
                    .push("proxy_slice_cache.file_cache_dir must be an absolute path".to_string());
            }
        }
        if self.proxy_slice_cache.slice_size_bytes == 0 {
            errors.push("proxy_slice_cache.slice_size_bytes must be greater than 0".to_string());
        }
        if self.proxy_slice_cache.max_cache_size_bytes == 0 {
            errors
                .push("proxy_slice_cache.max_cache_size_bytes must be greater than 0".to_string());
        }
        if self.proxy_slice_cache.segment_ttl_seconds == 0 {
            errors.push("proxy_slice_cache.segment_ttl_seconds must be greater than 0".to_string());
        }
        if self.proxy_slice_cache.eviction_interval_seconds == 0 {
            errors.push(
                "proxy_slice_cache.eviction_interval_seconds must be greater than 0".to_string(),
            );
        }
        if !self.proxy_slice_cache.watermark_ratio.is_finite()
            || self.proxy_slice_cache.watermark_ratio <= 0.0
            || self.proxy_slice_cache.watermark_ratio > 1.0
        {
            errors.push(
                "proxy_slice_cache.watermark_ratio must be greater than 0 and at most 1"
                    .to_string(),
            );
        }
        if self.proxy_slice_cache.max_cache_size_bytes
            < self.proxy_slice_cache.slice_size_bytes as u64
        {
            errors.push(
                "proxy_slice_cache.max_cache_size_bytes must be at least proxy_slice_cache.slice_size_bytes"
                    .to_string(),
            );
        }

        // Validate: cluster mode requires a Redis backend to be configured.
        // Redis is essential for cross-replica pub/sub, leader election, node registry,
        // distributed rate limiting, and brute-force protection. Running cluster mode
        // without Redis causes silent data loss and broken multi-replica coordination.
        let cluster_mode_active = self.cluster_runtime_enabled();
        let redis_backend_configured = match self.redis.deployment_mode {
            RedisDeploymentMode::Standalone => redis_url_present || redis_split_present,
            RedisDeploymentMode::Sentinel => {
                self.redis
                    .sentinel_master_name
                    .as_ref()
                    .is_some_and(|name| !name.is_empty())
                    && !self.redis.sentinel_addresses.is_empty()
            }
        };
        if cluster_mode_active && !redis_backend_configured {
            errors.push(
                "distributed mode requires Redis to be configured. \
                 Configure standalone Redis via redis.url or redis.host/redis.port \
                 before enabling cluster.enabled=true."
                    .to_string(),
            );
        }

        if cluster_mode_active
            && self.cluster.discovery_mode == ClusterDiscoveryMode::K8sDns
            && !cfg!(feature = "k8s")
        {
            errors.push(
                "cluster.discovery_mode='k8s_dns' requires the 'k8s' feature to be compiled in. \
                     Rebuild with Kubernetes support enabled."
                    .to_string(),
            );
        }

        if cluster_mode_active && self.cluster.discovery_mode == ClusterDiscoveryMode::Static {
            for (index, peer) in self.cluster.peers.iter().enumerate() {
                if let Err(error) =
                    synctv_cluster::normalize_static_peer_address(peer, self.cluster.port)
                {
                    errors.push(format!("cluster.peers[{index}] is invalid: {error}"));
                }
            }
        }

        // Kubernetes metadata is collected by the outer runtime bootstrap.

        if cluster_mode_active
            && self.cluster.leader_election_mode == ClusterLeaderElectionMode::K8sLease
            && !cfg!(feature = "k8s")
        {
            errors.push(
                    "cluster.leader_election_mode='k8s_lease' requires the 'k8s' feature to be compiled in. \
                     Rebuild with Kubernetes support enabled or switch to 'redis'."
                        .to_string(),
                );
        }

        // Kubernetes metadata is collected by the outer runtime bootstrap.

        if cluster_mode_active && self.redis.deployment_mode == RedisDeploymentMode::Sentinel {
            errors.push(
                "cluster.enabled=true is not supported with Redis Sentinel. \
                 Startup still relies on Redis distributed locks for room coordination, \
                 and Sentinel failover can create split-brain coordination windows. \
                 Switch Redis to a non-Sentinel deployment before enabling cluster mode."
                    .to_string(),
            );
        }

        // Require cluster.secret when distributed mode is enabled.
        // An empty `cluster.secret` means that ANY node claiming to be part of the
        // cluster can call inter-node transport endpoints without authentication.
        // In standalone mode, `cluster.secret` is not required even with Redis
        // configured, because there are no inter-node transport endpoints to protect.
        if self.cluster.enabled && self.cluster.secret.is_empty() {
            errors.push(
                "cluster.secret must be set when distributed mode is enabled. \
                 An empty cluster.secret leaves inter-node transport endpoints unauthenticated. \
                 Generate a secret with: openssl rand -hex 32 \
                 and pass it as the cluster.secret initialization value."
                    .to_string(),
            );
        }

        // Validate cluster.secret strength when set.
        if !self.cluster.secret.is_empty() {
            const MIN_CLUSTER_SECRET_LEN: usize = 16;
            if is_known_dev_secret(&self.cluster.secret, KNOWN_DEV_CLUSTER_SECRETS) {
                errors.push(
                    "cluster.secret uses a known development value. Generate a unique key with `openssl rand -hex 32`"
                        .to_string(),
                );
            } else if self.cluster.secret.len() < MIN_CLUSTER_SECRET_LEN {
                errors.push(format!(
                    "cluster.secret is too short ({} chars, minimum {}). \
                         Use: openssl rand -hex 16",
                    self.cluster.secret.len(),
                    MIN_CLUSTER_SECRET_LEN
                ));
            }
        }

        if self.cluster.enabled && self.server.advertise_host.trim().is_empty() {
            errors.push(
                "server.advertise_host must be set explicitly when distributed mode is enabled. \
                 Refusing to fall back to the local hostname because other replicas may not be able to route to it."
                    .to_string(),
            );
        }

        if self.cluster.enabled && is_unspecified_host(&self.advertise_host()) {
            errors.push(
                "server.advertise_host must resolve to a routable address when distributed mode is enabled. \
                 The current advertise host resolves to an unspecified address, which other replicas cannot reach for cluster/HLS proxying. \
                 Use the pod IP, node IP, or service-reachable hostname."
                    .to_string(),
            );
        }

        if self.cluster.enabled {
            let cluster_advertise_host = if self.cluster.advertise_host.trim().is_empty() {
                self.advertise_host()
            } else {
                self.cluster.advertise_host.clone()
            };
            if is_unspecified_host(&cluster_advertise_host) {
                errors.push(
                    "cluster.advertise_host must resolve to a routable address when distributed mode is enabled. \
                     Use the pod IP, node IP, or service-reachable hostname."
                        .to_string(),
                );
            }
        }

        let mut apple_app_ids = std::collections::HashSet::new();
        for (index, app_id) in self.webauthn.apple_app_ids.iter().enumerate() {
            if !valid_apple_application_identifier(app_id) {
                errors.push(format!(
                    "webauthn.apple_app_ids[{index}] must be an Apple application identifier such as TEAMID.org.example.app"
                ));
            } else if !apple_app_ids.insert(app_id.as_str()) {
                errors.push(format!(
                    "webauthn.apple_app_ids[{index}] duplicates another application identifier"
                ));
            }
        }
        let mut android_packages = std::collections::HashSet::new();
        for (app_index, app) in self.webauthn.android_apps.iter().enumerate() {
            if !valid_android_package_name(&app.package_name) {
                errors.push(format!(
                    "webauthn.android_apps[{app_index}].package_name must be a valid Android package name"
                ));
            } else if !android_packages.insert(app.package_name.as_str()) {
                errors.push(format!(
                    "webauthn.android_apps[{app_index}].package_name duplicates another Android app"
                ));
            }
            if app.sha256_cert_fingerprints.is_empty() {
                errors.push(format!(
                    "webauthn.android_apps[{app_index}].sha256_cert_fingerprints must contain at least one certificate fingerprint"
                ));
            }
            for (fingerprint_index, fingerprint) in app.sha256_cert_fingerprints.iter().enumerate()
            {
                if !valid_sha256_certificate_fingerprint(fingerprint) {
                    errors.push(format!(
                        "webauthn.android_apps[{app_index}].sha256_cert_fingerprints[{fingerprint_index}] must contain exactly 32 SHA-256 bytes"
                    ));
                }
            }
        }

        if self.webauthn.enabled {
            if self.webauthn.rp_id.trim().is_empty() {
                errors.push("webauthn.rp_id must be set when webauthn.enabled=true".to_string());
            }
            if self.webauthn.rp_origin.trim().is_empty() {
                errors
                    .push("webauthn.rp_origin must be set when webauthn.enabled=true".to_string());
            } else {
                match url::Url::parse(&self.webauthn.rp_origin) {
                    Ok(origin) => {
                        if !matches!(origin.scheme(), "http" | "https") {
                            errors.push(
                                "webauthn.rp_origin must use http:// or https://".to_string(),
                            );
                        }
                        if origin.host_str().is_none() {
                            errors.push("webauthn.rp_origin must include a host".to_string());
                        }
                        if origin.path() != "/"
                            || origin.query().is_some()
                            || origin.fragment().is_some()
                        {
                            errors.push(
                                "webauthn.rp_origin must be an origin only, without path, query, or fragment"
                                    .to_string(),
                            );
                        }
                    }
                    Err(error) => {
                        errors.push(format!("webauthn.rp_origin is not a valid URL: {error}"));
                    }
                }
            }
            if self.webauthn.rp_name.trim().is_empty() {
                errors.push("webauthn.rp_name must not be empty".to_string());
            }
            if self.webauthn.timeout_seconds == 0 {
                errors.push("webauthn.timeout_seconds must be greater than 0".to_string());
            }
            for (index, origin) in self.webauthn.allowed_origins.iter().enumerate() {
                if let Err(error) = validate_cors_origin(origin) {
                    errors.push(format!("webauthn.allowed_origins[{index}]: {error}"));
                }
            }
            if self.cluster.enabled && !redis_backend_configured {
                errors.push(
                    "WebAuthn/passkey requires Redis for challenge state in cluster mode. \
                     Configure a Redis backend or disable WebAuthn."
                        .to_string(),
                );
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        valid_android_package_name, valid_apple_application_identifier,
        valid_sha256_certificate_fingerprint, validate_project_url,
    };
    use crate::app_config::{
        AppConfig, ClusterChannelConfig, ClusterDiscoveryMode, LogFileOutput, LogOutput,
        SecurityConfig, SsrfConfig, StartupDiagnostic,
    };

    #[test]
    fn placeholder_jwt_secret_is_reported_as_a_startup_warning() {
        let mut config = AppConfig::default();
        config.jwt.secret = "CHANGE_ME_this-placeholder-is-long-enough".to_string();

        assert!(config.startup_diagnostics().iter().any(|diagnostic| {
            matches!(
                diagnostic,
                StartupDiagnostic::Warning(message)
                    if message.contains("JWT secret appears to be a placeholder")
            )
        }));
    }

    #[test]
    fn short_jwt_secret_remains_a_validation_error() {
        let mut config = AppConfig::default();
        config.jwt.secret = "Short-secret-with-2-classes".to_string();

        let errors = config.validate().expect_err("short JWT secrets must fail");
        assert!(errors
            .iter()
            .any(|error| error.contains("JWT secret is too short")));
    }

    #[test]
    fn project_url_accepts_http_urls_with_project_paths() {
        assert!(validate_project_url("https://github.com/synctv-org/synctv").is_ok());
        assert!(validate_project_url("http://example.com/project?source=api#readme").is_ok());
    }

    #[test]
    fn project_url_requires_an_http_url_with_a_host() {
        for value in ["", "synctv-org/synctv", "file:///tmp/synctv", "https://"] {
            assert!(validate_project_url(value).is_err(), "value: {value}");
        }
    }

    #[test]
    fn logging_validation_covers_global_and_network_components() {
        let mut config = AppConfig::default();
        config.logging.level = "verbose".to_string();
        config.livestream.logging.format = "pretty".to_string();
        config.webrtc.logging.output = LogOutput::File(LogFileOutput::default());

        let errors = config.validate().expect_err("logging settings must fail");

        assert!(errors
            .iter()
            .any(|error| error.contains("logging.level 'verbose'")));
        assert!(errors
            .iter()
            .any(|error| error.contains("livestream.logging.format 'pretty'")));
        assert!(errors
            .iter()
            .any(|error| error.contains("webrtc.logging.output.path must not be empty")));
    }

    #[test]
    fn listener_ports_accept_kernel_assignment() {
        let mut config = AppConfig::default();
        config.server.port = 0;
        config.livestream.rtmp_port = 0;
        config.health.port = 0;
        config.management.transport = crate::app_config::ManagementTransport::Tcp;
        config.management.port = 0;

        let errors = config
            .validate()
            .expect_err("default security settings should remain invalid");
        for path in [
            "server.port",
            "livestream.rtmp_port",
            "health.port",
            "management.port",
        ] {
            assert!(
                errors.iter().all(|error| !error.contains(path)),
                "kernel-assigned {path} must pass listener validation: {errors:?}"
            );
        }
    }

    #[test]
    fn native_app_association_identifiers_accept_platform_formats() {
        assert!(valid_apple_application_identifier(
            "85KBWFQ6F6.org.synctv.app"
        ));
        assert!(valid_android_package_name("org.synctv.app"));
        assert!(valid_sha256_certificate_fingerprint(
            "AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99"
        ));
        assert!(valid_sha256_certificate_fingerprint(
            "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899"
        ));
    }

    #[test]
    fn native_app_association_identifiers_reject_ambiguous_values() {
        assert!(!valid_apple_application_identifier("org.synctv.app"));
        assert!(!valid_apple_application_identifier(
            "85kbwfq6f6.org.synctv.app"
        ));
        assert!(!valid_android_package_name("synctv"));
        assert!(!valid_android_package_name("org.-synctv.app"));
        assert!(!valid_apple_application_identifier(
            " 85KBWFQ6F6.org.synctv.app"
        ));
        assert!(!valid_android_package_name("org.synctv.app "));
        assert!(!valid_sha256_certificate_fingerprint("AA:BB"));
    }

    #[test]
    fn security_secrets_must_be_distinct() {
        let mut config = AppConfig::default();
        config.jwt.secret = "jwt-secret-value-with-at-least-32-characters".to_string();
        config.security.credential_encryption_key =
            "9191919191919191919191919191919191919191919191919191919191919191".to_string();
        config.security.totp_encryption_key =
            "9292929292929292929292929292929292929292929292929292929292929292".to_string();
        config.security.email_outbox_encryption_key =
            "9393939393939393939393939393939393939393939393939393939393939393".to_string();
        config.security.opaque_server_setup_secret =
            "opaque-secret-value-with-at-least-32-characters".to_string();
        config.security.proxy_signing_key = config.jwt.secret.clone();
        config.security.media_swarm_signing_key =
            "media-swarm-signing-secret-with-at-least-32-characters".to_string();
        config.security.provider_session_encryption_key =
            "provider-session-secret-with-at-least-32-characters".to_string();
        config.security.login_discovery_key =
            "login-discovery-secret-with-at-least-32-characters".to_string();
        config.security.webauthn_enumeration_key =
            "webauthn-enumeration-secret-with-at-least-32-characters".to_string();
        config.file_storage.upload_token_secret =
            "file-upload-secret-with-at-least-32-characters".to_string();

        let errors = config.validate().expect_err("duplicate secrets must fail");
        assert!(errors.iter().any(|error| {
            error.contains("jwt.secret and security.proxy_signing_key must use different")
        }));
    }

    #[test]
    fn media_swarm_signing_key_cannot_reuse_proxy_signing_key() {
        let mut config = AppConfig::default();
        config.security.proxy_signing_key =
            "shared-proxy-and-swarm-key-with-at-least-32-characters".to_string();
        config.security.media_swarm_signing_key = config.security.proxy_signing_key.clone();

        let errors = config.validate().expect_err("duplicate secrets must fail");
        assert!(errors.iter().any(|error| {
            error.contains(
                "security.proxy_signing_key and security.media_swarm_signing_key must use different",
            )
        }));
    }

    #[test]
    fn security_config_debug_redacts_every_key() {
        let config = SecurityConfig {
            credential_encryption_key: "credential-key".to_string(),
            totp_encryption_key: "totp-key".to_string(),
            email_outbox_encryption_key: "outbox-key".to_string(),
            opaque_server_setup_secret: "opaque-key".to_string(),
            proxy_signing_key: "proxy-key".to_string(),
            media_swarm_signing_key: "swarm-key".to_string(),
            provider_session_encryption_key: "provider-session-key".to_string(),
            login_discovery_key: "login-discovery-key".to_string(),
            webauthn_enumeration_key: "webauthn-enumeration-key".to_string(),
            ssrf: SsrfConfig::default(),
        };

        let debug = format!("{config:?}");
        assert!(debug.contains("<redacted>"));
        for key in [
            "credential-key",
            "totp-key",
            "outbox-key",
            "opaque-key",
            "proxy-key",
            "swarm-key",
            "provider-session-key",
            "login-discovery-key",
            "webauthn-enumeration-key",
        ] {
            assert!(!debug.contains(key), "debug output leaked {key}");
        }
    }

    #[test]
    fn cluster_config_debug_redacts_secret() {
        let config = ClusterChannelConfig {
            secret: "cluster-debug-secret".to_string(),
            ..ClusterChannelConfig::default()
        };

        let debug = format!("{config:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("cluster-debug-secret"));
    }

    #[test]
    fn cluster_advertise_host_rejects_unspecified_addresses() {
        for host in ["0.0.0.0", "::", "[::]"] {
            let mut config = AppConfig::default();
            config.cluster.enabled = true;
            config.server.advertise_host = "api.example.com".to_string();
            config.cluster.advertise_host = host.to_string();

            let errors = config
                .validate()
                .expect_err("an unspecified cluster advertise host must fail validation");
            assert!(errors
                .iter()
                .any(|error| error.contains("cluster.advertise_host must resolve")));
        }
    }

    #[test]
    fn static_discovery_rejects_malformed_peer_addresses() {
        let mut config = AppConfig::default();
        config.cluster.enabled = true;
        config.cluster.discovery_mode = ClusterDiscoveryMode::Static;
        config.cluster.peers = vec!["node.example.com:abc".to_string()];

        let errors = config
            .validate()
            .expect_err("a malformed static peer must fail validation");
        assert!(errors
            .iter()
            .any(|error| error.contains("cluster.peers[0] is invalid")));
    }
}
