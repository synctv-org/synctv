use config::{ConfigError, FileFormat};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use synctv_common::time as common_time;

const MIN_GRPC_MESSAGE_SIZE: usize = 1024 * 1024;
const MAX_GRPC_MESSAGE_SIZE: usize = 1024 * 1024 * 1024;
const DANGEROUS_CIDR_RANGES: &[&str] = &["0.0.0.0/0", "::/0", "0.0.0.0/0,::/0"];

fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn absolute_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        })
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

pub fn absolute_display_path(path: &Path) -> String {
    if path.is_absolute() {
        return path.display().to_string();
    }

    std::env::current_dir()
        .map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
        .display()
        .to_string()
}

pub fn default_data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return user_home_dir().map_or_else(
            || std::env::temp_dir().join("synctv"),
            |home| home.join(".synctv"),
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return absolute_env_path("XDG_STATE_HOME")
            .map(|dir| dir.join("synctv"))
            .or_else(|| {
                user_home_dir().map(|home| home.join(".local").join("state").join("synctv"))
            })
            .unwrap_or_else(|| std::env::temp_dir().join("synctv"));
    }

    #[cfg(windows)]
    {
        return std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| user_home_dir().map(|home| home.join("AppData").join("Local")))
            .unwrap_or_else(std::env::temp_dir)
            .join("synctv");
    }

    #[allow(unreachable_code)]
    std::env::temp_dir().join("synctv")
}

pub fn default_management_runtime_dir() -> PathBuf {
    default_data_dir().join("run")
}

pub fn default_management_unix_socket_path() -> PathBuf {
    default_management_runtime_dir().join("synctv.sock")
}

pub fn default_config_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    push_config_path_variants(&mut paths, Path::new("synctv"));

    #[cfg(target_os = "macos")]
    if let Some(home) = user_home_dir() {
        push_config_path_variants(&mut paths, &home.join(".synctv").join("synctv"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(xdg_config_home) = absolute_env_path("XDG_CONFIG_HOME") {
            push_config_path_variants(&mut paths, &xdg_config_home.join("synctv").join("synctv"));
        } else if let Some(home) = user_home_dir() {
            push_config_path_variants(
                &mut paths,
                &home.join(".config").join("synctv").join("synctv"),
            );
        }
        push_config_path_variants(&mut paths, Path::new("/etc/synctv/synctv"));
    }

    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            push_config_path_variants(&mut paths, &appdata.join("synctv").join("synctv"));
        } else if let Some(home) = user_home_dir() {
            push_config_path_variants(
                &mut paths,
                &home
                    .join("AppData")
                    .join("Roaming")
                    .join("synctv")
                    .join("synctv"),
            );
        }
    }

    push_config_path_variants(&mut paths, Path::new("/config/synctv"));
    paths
}

fn push_config_path_variants(paths: &mut Vec<PathBuf>, base_path_without_extension: &Path) {
    for extension in ["yaml", "yml", "json", "toml"] {
        push_unique_path(paths, base_path_without_extension.with_extension(extension));
    }
}

fn config_file_format_for_path(path: &Path) -> Result<FileFormat, ConfigError> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("yaml" | "yml") => Ok(FileFormat::Yaml),
        Some("json") => Ok(FileFormat::Json),
        Some("toml") => Ok(FileFormat::Toml),
        Some(ext) => Err(ConfigError::Message(format!(
            "unsupported config file extension '.{ext}' for {} (expected .yaml, .yml, .json, or .toml)",
            absolute_display_path(path)
        ))),
        None => Err(ConfigError::Message(format!(
            "config file {} is missing an extension (expected .yaml, .yml, .json, or .toml)",
            absolute_display_path(path)
        ))),
    }
}

fn join_config_key_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

fn resolve_config_file_reference_path(base_dir: &Path, reference: &str) -> PathBuf {
    let reference_path = Path::new(reference);
    if reference_path.is_absolute() {
        reference_path.to_path_buf()
    } else {
        base_dir.join(reference_path)
    }
}

fn load_config_string_from_file(
    config_path: &Path,
    base_dir: &Path,
    key_path: &str,
    reference: &str,
) -> Result<String, ConfigError> {
    let trimmed_reference = reference.trim();
    if trimmed_reference.is_empty() {
        return Err(ConfigError::Message(format!(
            "config key '{key_path}_file' in {} must not be empty",
            absolute_display_path(config_path)
        )));
    }

    let resolved_path = resolve_config_file_reference_path(base_dir, trimmed_reference);
    let contents = std::fs::read_to_string(&resolved_path).map_err(|error| {
        ConfigError::Message(format!(
            "failed to read config file reference '{key_path}_file' from {}: {error}",
            absolute_display_path(&resolved_path)
        ))
    })?;

    Ok(contents.trim().to_string())
}

fn resolve_relative_path_from(reference: &str, base_dir: &Path) -> PathBuf {
    let path = Path::new(reference.trim());
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn default_runtime_socket_relative_path() -> PathBuf {
    PathBuf::from("run").join("synctv.sock")
}

fn default_proxy_slice_cache_relative_path() -> PathBuf {
    PathBuf::from("cache").join("proxy-slice")
}

fn is_secret_like_provider_key(base_key: &str) -> bool {
    matches!(
        base_key,
        "access_token"
            | "api_key"
            | "client_secret"
            | "password"
            | "refresh_token"
            | "secret"
            | "shared_secret"
            | "token"
    )
}

fn supports_secret_file_reference(current_path: &str, base_key: &str) -> bool {
    let key_path = join_config_key_path(current_path, base_key);
    matches!(
        key_path.as_str(),
        "server.cluster_secret"
            | "security.credential_encryption_key"
            | "security.opaque_server_setup_secret"
            | "management.auth_token"
            | "metrics.auth.basic_password"
            | "metrics.auth.bearer_token"
            | "database.password"
            | "database.url"
            | "redis.password"
            | "redis.url"
            | "jwt.secret"
            | "email.smtp_password"
            | "livestream.hls_oss.access_key_id"
            | "livestream.hls_oss.secret_access_key"
            | "bootstrap.root_password"
    ) || (current_path.starts_with("media_providers.") && is_secret_like_provider_key(base_key))
}

fn resolve_secret_file_references_in_json_value(
    value: &mut serde_json::Value,
    config_path: &Path,
    current_path: &str,
    config_base_dir: &Path,
) -> Result<(), ConfigError> {
    match value {
        serde_json::Value::Object(map) => {
            let file_keys = map
                .keys()
                .filter(|key| key.ends_with("_file"))
                .cloned()
                .collect::<Vec<_>>();

            for file_key in file_keys {
                let Some(file_reference) = map.get(&file_key).and_then(serde_json::Value::as_str)
                else {
                    let key_path = join_config_key_path(current_path, &file_key);
                    return Err(ConfigError::Message(format!(
                        "config key '{key_path}' in {} must be a string path",
                        absolute_display_path(config_path)
                    )));
                };

                let Some(base_key) = file_key.strip_suffix("_file") else {
                    continue;
                };
                if base_key.is_empty() {
                    let key_path = join_config_key_path(current_path, &file_key);
                    return Err(ConfigError::Message(format!(
                        "config key '{key_path}' in {} has an invalid _file suffix",
                        absolute_display_path(config_path)
                    )));
                }

                if supports_secret_file_reference(current_path, base_key) {
                    let key_path = join_config_key_path(current_path, base_key);
                    let resolved_value = load_config_string_from_file(
                        config_path,
                        config_base_dir,
                        &key_path,
                        file_reference,
                    )?;
                    map.insert(
                        base_key.to_string(),
                        serde_json::Value::String(resolved_value),
                    );
                    map.remove(&file_key);
                }
            }

            let child_keys = map.keys().cloned().collect::<Vec<_>>();
            for key in child_keys {
                if let Some(child) = map.get_mut(&key) {
                    resolve_secret_file_references_in_json_value(
                        child,
                        config_path,
                        &join_config_key_path(current_path, &key),
                        config_base_dir,
                    )?;
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                resolve_secret_file_references_in_json_value(
                    item,
                    config_path,
                    current_path,
                    config_base_dir,
                )?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn normalize_split_database_config_value(value: &mut serde_json::Value) {
    let Some(database) = value
        .get_mut("database")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };

    let has_explicit_url = database.contains_key("url");
    let has_split_config = ["host", "port", "username", "user", "password", "name"]
        .iter()
        .any(|key| database.contains_key(*key));

    if has_split_config && !has_explicit_url {
        database.insert("url".to_string(), serde_json::Value::String(String::new()));
    }
}

pub fn validate_cors_origin(origin: &str) -> Result<(), String> {
    let parsed = url::Url::parse(origin)
        .map_err(|_| format!("CORS origin '{origin}' is not a valid URL"))?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!(
            "CORS origin '{origin}' must use http:// or https://"
        ));
    }

    if parsed.host_str().is_none() {
        return Err(format!("CORS origin '{origin}' must include a host"));
    }

    if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(format!(
            "CORS origin '{origin}' must not include a path, query, or fragment"
        ));
    }

    Ok(())
}

/// Application configuration
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub time: TimeConfig,
    pub public_ids: PublicIdsConfig,
    pub security: SecurityConfig,
    /// Shared root directory for runtime-owned local files.
    ///
    /// This affects default runtime paths and relative overrides for
    /// `management.unix_socket_path`, `logging.file_path`,
    /// `livestream.hls_storage_path`, and
    /// `proxy_slice_cache.file_cache_dir`.
    ///
    /// It does not rebase static input files such as `*_file` secrets or
    /// `metrics.tls.cert_path` / `metrics.tls.key_path`.
    pub data_dir: String,
    pub metrics: MetricsConfig,
    pub management: ManagementConfig,
    pub database: DatabaseConfig,
    pub redis: RedisConfig,
    pub jwt: JwtConfig,
    pub logging: LoggingConfig,
    pub livestream: LivestreamConfig,
    pub webauthn: WebAuthnConfig,
    pub email: EmailConfig,
    pub media_providers: MediaProvidersConfig,
    pub webrtc: WebRTCConfig,
    pub connection_limits: ConnectionLimitsConfig,
    pub bootstrap: BootstrapConfig,
    pub cluster: ClusterChannelConfig,
    pub password_complexity: PasswordComplexityConfig,
    pub buffer_sizes: BufferSizesConfig,
    pub cache: CacheConfig,
    pub proxy_slice_cache: ProxySliceCacheConfig,
    pub messaging_rate_limits: MessagingRateLimitConfig,
    pub http_rate_limits: HttpRateLimitConfig,
    pub grpc_rate_limits: GrpcRateLimitConfig,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("server", &self.server)
            .field("time", &self.time)
            .field("public_ids", &self.public_ids)
            .field("security", &"<redacted>")
            .field("data_dir", &self.data_dir)
            .field("metrics", &self.metrics)
            .field("management", &self.management)
            .field("database", &"<redacted>")
            .field("redis", &self.redis)
            .field("jwt", &"<redacted>")
            .field("logging", &self.logging)
            .field("livestream", &self.livestream)
            .field("webauthn", &self.webauthn)
            .field("email", &"<redacted>")
            .field("media_providers", &self.media_providers)
            .field("webrtc", &self.webrtc)
            .field("connection_limits", &self.connection_limits)
            .field("bootstrap", &"<redacted>")
            .field("cluster", &self.cluster)
            .field("password_complexity", &self.password_complexity)
            .field("buffer_sizes", &self.buffer_sizes)
            .field("cache", &self.cache)
            .field("proxy_slice_cache", &self.proxy_slice_cache)
            .field("messaging_rate_limits", &self.messaging_rate_limits)
            .field("http_rate_limits", &self.http_rate_limits)
            .field("grpc_rate_limits", &self.grpc_rate_limits)
            .finish()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            time: TimeConfig::default(),
            public_ids: PublicIdsConfig::default(),
            security: SecurityConfig::default(),
            data_dir: default_data_dir().display().to_string(),
            metrics: MetricsConfig::default(),
            management: ManagementConfig::default(),
            database: DatabaseConfig::default(),
            redis: RedisConfig::default(),
            jwt: JwtConfig::default(),
            logging: LoggingConfig::default(),
            livestream: LivestreamConfig::default(),
            webauthn: WebAuthnConfig::default(),
            email: EmailConfig::default(),
            media_providers: MediaProvidersConfig::default(),
            webrtc: WebRTCConfig::default(),
            connection_limits: ConnectionLimitsConfig::default(),
            bootstrap: BootstrapConfig::default(),
            cluster: ClusterChannelConfig::default(),
            password_complexity: PasswordComplexityConfig::default(),
            buffer_sizes: BufferSizesConfig::default(),
            cache: CacheConfig::default(),
            proxy_slice_cache: ProxySliceCacheConfig::default(),
            messaging_rate_limits: MessagingRateLimitConfig::default(),
            http_rate_limits: HttpRateLimitConfig::default(),
            grpc_rate_limits: GrpcRateLimitConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PublicIdsConfig {
    /// Optional sqids configuration for API-facing public IDs.
    ///
    /// Leave unset to use the default prefixed decimal format.
    pub sqids: Option<PublicIdsSqidsConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PublicIdsSqidsConfig {
    /// Optional sqids alphabet. Leave empty/None to use the crate default.
    pub alphabet: Option<String>,
    /// Minimum public ID length for API-facing sqids.
    pub min_length: u8,
}

impl Default for PublicIdsSqidsConfig {
    fn default() -> Self {
        Self {
            alphabet: None,
            min_length: 12,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    /// AES-256-GCM key used to encrypt sensitive provider credentials.
    ///
    /// This must be a 64-character hex string when set. Prefer
    /// `credential_encryption_key_file` in config files so the key is loaded
    /// from a secret mount instead of being stored inline.
    pub credential_encryption_key: String,
    /// Stable OPAQUE server setup derivation secret.
    ///
    /// This must be kept stable across restarts and JWT secret rotations,
    /// otherwise existing OPAQUE password records cannot be used. Prefer
    /// `opaque_server_setup_secret_file` in config files.
    pub opaque_server_setup_secret: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TimeConfig {
    /// Default IANA timezone used for human-readable formatting and local datetime parsing.
    ///
    /// Resolution priority:
    /// 1. `time.timezone` from config file
    /// 2. `SYNCTV_TIME_TIMEZONE`
    /// 3. `TZ`
    /// 4. system timezone
    /// 5. `UTC`
    pub timezone: String,
}

/// Domain-level messaging rate limits for chat and danmaku.
///
/// These limits are enforced by the shared chat/messaging business logic and
/// therefore must come from configuration rather than hard-coded defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MessagingRateLimitConfig {
    /// Maximum chat messages allowed within the configured window.
    pub chat_per_second: u32,
    /// Maximum danmaku messages allowed within the configured window.
    pub danmaku_per_second: u32,
    /// Sliding-window size for chat/danmaku enforcement.
    pub window_seconds: u64,
}

impl Default for MessagingRateLimitConfig {
    fn default() -> Self {
        Self {
            chat_per_second: 10,
            danmaku_per_second: 3,
            window_seconds: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub enable_reflection: bool,
    /// Trusted proxy IP addresses/CIDRs for X-Forwarded-For validation.
    /// When set, X-Forwarded-For/X-Real-IP headers are only trusted from these addresses.
    /// Example: `["10.0.0.0/8", "192.168.0.0/16"]` for internal load balancers.
    /// If empty, X-Forwarded-For headers are NOT trusted (socket address is used).
    pub trusted_proxies: Vec<String>,
    /// CORS allowed origins. Must be set to specific domains.
    /// Example: `["https://app.example.com", "https://admin.example.com"]`
    pub cors_allowed_origins: Vec<String>,
    /// Shared secret for authenticating cluster gRPC calls between nodes.
    /// When set, all inter-node gRPC requests must include this secret in the
    /// `x-cluster-secret` metadata header. If empty, cluster endpoints are disabled.
    pub cluster_secret: String,
    /// Advertise host for cluster node registration.
    /// This is the address other nodes use to reach this instance.
    /// Reads from `SYNCTV_SERVER_ADVERTISE_HOST` env var. In Kubernetes, set this
    /// to the pod IP via the downward API (status.podIP).
    /// If empty, falls back to `POD_IP` env var, then to the system hostname.
    pub advertise_host: String,
    /// Maximum time in seconds to wait for active connections to drain during shutdown.
    /// Defaults to 30 seconds. Increase for deployments with many long-lived connections.
    pub shutdown_drain_timeout_seconds: u64,
    /// Maximum gRPC message size in bytes for both incoming (decoding) and
    /// outgoing (encoding) messages. Prevents OOM attacks from oversized messages.
    /// Default: 16777216 (16 MB). Minimum: 1048576 (1 MB).
    /// Set via `SYNCTV_SERVER_GRPC_MAX_MESSAGE_SIZE_BYTES` env var or config file.
    pub grpc_max_message_size_bytes: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8080,
            enable_reflection: true,
            trusted_proxies: Vec::new(),
            cors_allowed_origins: Vec::new(),
            cluster_secret: String::new(),
            advertise_host: String::new(),
            shutdown_drain_timeout_seconds: 30,
            grpc_max_message_size_bytes: 16 * 1024 * 1024, // 16 MB default
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct MetricsTlsConfig {
    /// Enable TLS for the dedicated metrics listener.
    pub enabled: bool,
    /// PEM-encoded certificate chain served by the metrics listener.
    ///
    /// Relative paths stay anchored to the config file directory (or the
    /// current working directory when loading only from env), not `data_dir`.
    pub cert_path: String,
    /// PEM-encoded private key for the metrics listener.
    ///
    /// Relative paths stay anchored to the config file directory (or the
    /// current working directory when loading only from env), not `data_dir`.
    pub key_path: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum MetricsAuthMode {
    #[default]
    BearerToken,
    Basic,
    Kubernetes,
}

impl std::str::FromStr for MetricsAuthMode {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "bearer_token" => Ok(Self::BearerToken),
            "basic" => Ok(Self::Basic),
            "kubernetes" => Ok(Self::Kubernetes),
            _ => Err(ConfigError::Message(format!(
                "metrics.auth.mode '{value}' must be one of: bearer_token, basic, kubernetes"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsKubernetesAuthConfig {
    /// Optional audience forwarded to Kubernetes TokenReview.
    pub audience: String,
    /// Authentication cache TTL in seconds.
    pub authentication_cache_ttl_seconds: u64,
    /// Authorization cache TTL in seconds.
    pub authorization_cache_ttl_seconds: u64,
}

impl Default for MetricsKubernetesAuthConfig {
    fn default() -> Self {
        Self {
            audience: String::new(),
            authentication_cache_ttl_seconds: 60,
            authorization_cache_ttl_seconds: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsAuthConfig {
    /// Authentication mode for `/metrics`.
    pub mode: MetricsAuthMode,
    /// Static bearer token used when `mode=bearer_token`.
    pub bearer_token: String,
    /// Static username used when `mode=basic`.
    pub basic_username: String,
    /// Static password used when `mode=basic`.
    pub basic_password: String,
    /// Kubernetes TokenReview + SubjectAccessReview settings.
    pub kubernetes: MetricsKubernetesAuthConfig,
}

impl Default for MetricsAuthConfig {
    fn default() -> Self {
        Self {
            mode: MetricsAuthMode::BearerToken,
            bearer_token: String::new(),
            basic_username: String::new(),
            basic_password: String::new(),
            kubernetes: MetricsKubernetesAuthConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsConfig {
    /// Enable the dedicated `/metrics` Prometheus listener.
    pub enabled: bool,
    /// Host/interface for the dedicated metrics listener.
    pub host: String,
    /// Port for the dedicated metrics listener.
    pub port: u16,
    /// Metrics listener TLS configuration.
    pub tls: MetricsTlsConfig,
    /// Metrics endpoint authentication configuration.
    pub auth: MetricsAuthConfig,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "0.0.0.0".to_string(),
            port: 9090,
            tls: MetricsTlsConfig::default(),
            auth: MetricsAuthConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ManagementTransport {
    #[default]
    Tcp,
    Unix,
}

impl std::str::FromStr for ManagementTransport {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "tcp" => Ok(Self::Tcp),
            "unix" => Ok(Self::Unix),
            _ => Err(ConfigError::Message(format!(
                "management.transport '{value}' must be either 'tcp' or 'unix'"
            ))),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ManagementConfig {
    pub enabled: bool,
    pub transport: ManagementTransport,
    pub port: u16,
    pub unix_socket_path: String,
    pub auth_token: String,
    pub enable_reflection: bool,
}

impl Default for ManagementConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            transport: if cfg!(unix) {
                ManagementTransport::Unix
            } else {
                ManagementTransport::Tcp
            },
            port: 50052,
            unix_socket_path: default_management_unix_socket_path().display().to_string(),
            auth_token: String::new(),
            enable_reflection: false,
        }
    }
}

impl std::fmt::Debug for ManagementConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagementConfig")
            .field("enabled", &self.enabled)
            .field("transport", &self.transport)
            .field("port", &self.port)
            .field("unix_socket_path", &self.unix_socket_path)
            .field("auth_token", &"<redacted>")
            .field("enable_reflection", &self.enable_reflection)
            .finish()
    }
}

impl ServerConfig {
    /// Check if an IP address is from a trusted proxy.
    ///
    /// Returns `true` if the IP matches any of the configured trusted proxies
    /// (supports both single IPs and CIDR notation like "10.0.0.0/8").
    /// Returns `false` if no trusted proxies are configured or if the IP doesn't match.
    #[must_use]
    pub fn is_trusted_proxy(&self, ip: &std::net::IpAddr) -> bool {
        if self.trusted_proxies.is_empty() {
            return false;
        }

        for proxy in &self.trusted_proxies {
            // Try parsing as CIDR network first
            if let Ok(network) = proxy.parse::<ipnet::IpNet>() {
                if network.contains(ip) {
                    return true;
                }
            }
            // Try parsing as single IP address
            if let Ok(proxy_ip) = proxy.parse::<std::net::IpAddr>() {
                if &proxy_ip == ip {
                    return true;
                }
            }
        }
        false
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub url: String,
    pub host: String,
    pub port: u16,
    #[serde(alias = "user")]
    pub username: String,
    pub password: String,
    pub name: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub connect_timeout_seconds: u64,
    pub idle_timeout_seconds: u64,
    pub max_lifetime_seconds: u64,
}

impl std::fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Mask password in database URL if present
        let masked_url = if let Some(at_pos) = self.url.find('@') {
            if let Some(colon_pos) = self.url[..at_pos].rfind(':') {
                let scheme_end = self.url.find("://").map_or(0, |p| p + 3);
                if colon_pos > scheme_end {
                    // Has password - mask it
                    format!(
                        "{}:****@{}",
                        &self.url[..colon_pos],
                        &self.url[at_pos + 1..]
                    )
                } else {
                    self.url.clone()
                }
            } else {
                self.url.clone()
            }
        } else {
            self.url.clone()
        };

        f.debug_struct("DatabaseConfig")
            .field("url", &masked_url)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("name", &self.name)
            .field("max_connections", &self.max_connections)
            .field("min_connections", &self.min_connections)
            .field("connect_timeout_seconds", &self.connect_timeout_seconds)
            .field("idle_timeout_seconds", &self.idle_timeout_seconds)
            .field("max_lifetime_seconds", &self.max_lifetime_seconds)
            .finish()
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "postgresql://synctv:synctv@localhost:5432/synctv".to_string(),
            host: String::new(),
            port: 0,
            username: String::new(),
            password: String::new(),
            name: String::new(),
            max_connections: 20,
            min_connections: 5,
            connect_timeout_seconds: 10,
            idle_timeout_seconds: 600,
            max_lifetime_seconds: 1800, // 30 minutes
        }
    }
}

/// Redis deployment mode
///
/// # Supported modes
///
/// - **Standalone** (default): Single Redis instance. Works with all features.
/// - **Sentinel**: Redis Sentinel for high availability. SyncTV performs
///   best-effort master rediscovery and connection hot-swap after repeated
///   health-check failures, but in-flight operations can still fail during a
///   failover window. Sentinel mode is intentionally rejected in cluster mode.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RedisDeploymentMode {
    #[default]
    Standalone,
    Sentinel,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RedisConfig {
    pub url: String,
    pub host: String,
    pub port: u16,
    #[serde(alias = "user")]
    pub username: String,
    pub password: String,
    pub database: i64,
    pub connect_timeout_seconds: u64,
    pub key_prefix: String,
    /// Deployment mode: standalone (default) or sentinel
    pub deployment_mode: RedisDeploymentMode,
    /// Sentinel master name (required for sentinel mode)
    pub sentinel_master_name: Option<String>,
    /// Sentinel node addresses (required for sentinel mode)
    pub sentinel_addresses: Vec<String>,
}

impl std::fmt::Debug for RedisConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Mask password in Redis URL if present (redis://:password@host or redis://user:password@host)
        let masked_url = if self.url.contains('@') {
            if let Some(at_pos) = self.url.find('@') {
                if let Some(colon_pos) = self.url[..at_pos].rfind(':') {
                    let scheme_end = self.url.find("://").map_or(0, |p| p + 3);
                    if colon_pos >= scheme_end && colon_pos < at_pos {
                        // Has password - mask it
                        format!(
                            "{}:****@{}",
                            &self.url[..colon_pos],
                            &self.url[at_pos + 1..]
                        )
                    } else {
                        self.url.clone()
                    }
                } else {
                    self.url.clone()
                }
            } else {
                self.url.clone()
            }
        } else {
            self.url.clone()
        };

        // Helper to mask password in Redis URLs
        let mask_url = |url: &str| -> String {
            if url.contains('@') {
                if let Some(at_pos) = url.find('@') {
                    if let Some(colon_pos) = url[..at_pos].rfind(':') {
                        let scheme_end = url.find("://").map_or(0, |p| p + 3);
                        if colon_pos >= scheme_end && colon_pos < at_pos {
                            return format!("{}:****@{}", &url[..colon_pos], &url[at_pos + 1..]);
                        }
                    }
                }
            }
            url.to_string()
        };

        let masked_sentinel: Vec<String> = self
            .sentinel_addresses
            .iter()
            .map(|u| mask_url(u))
            .collect();
        f.debug_struct("RedisConfig")
            .field("url", &masked_url)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("database", &self.database)
            .field("connect_timeout_seconds", &self.connect_timeout_seconds)
            .field("key_prefix", &self.key_prefix)
            .field("deployment_mode", &self.deployment_mode)
            .field("sentinel_master_name", &self.sentinel_master_name)
            .field("sentinel_addresses", &masked_sentinel)
            .finish()
    }
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            host: String::new(),
            port: 0,
            username: String::new(),
            password: String::new(),
            database: 0,
            connect_timeout_seconds: 5,
            key_prefix: "synctv:".to_string(),
            deployment_mode: RedisDeploymentMode::Standalone,
            sentinel_master_name: None,
            sentinel_addresses: Vec::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JwtConfig {
    pub secret: String,
    pub access_token_duration_hours: u64,
    pub refresh_token_duration_days: u64,
    pub guest_token_duration_hours: u64,
    pub clock_skew_leeway_secs: u64,
}

impl std::fmt::Debug for JwtConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtConfig")
            .field("secret", &"<redacted>")
            .field(
                "access_token_duration_hours",
                &self.access_token_duration_hours,
            )
            .field(
                "refresh_token_duration_days",
                &self.refresh_token_duration_days,
            )
            .field(
                "guest_token_duration_hours",
                &self.guest_token_duration_hours,
            )
            .field("clock_skew_leeway_secs", &self.clock_skew_leeway_secs)
            .finish()
    }
}

impl Default for JwtConfig {
    fn default() -> Self {
        Self {
            secret: "change-me-in-production".to_string(),
            access_token_duration_hours: 1,
            refresh_token_duration_days: 30,
            guest_token_duration_hours: 4,
            clock_skew_leeway_secs: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: String,
    pub format: String, // "json" or "pretty"
    pub filter: Option<String>,
    pub backtrace: bool,
    /// Optional log file path.
    ///
    /// Relative paths are treated as runtime-owned output and resolved against
    /// the effective `data_dir`.
    pub file_path: Option<String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: "pretty".to_string(),
            filter: None,
            backtrace: false,
            file_path: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum HlsStorageBackend {
    #[default]
    Memory,
    File,
    Oss,
}

impl FromStr for HlsStorageBackend {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "memory" => Ok(Self::Memory),
            "file" | "filesystem" => Ok(Self::File),
            "oss" | "s3" | "object_storage" => Ok(Self::Oss),
            _ => Err(ConfigError::Message(format!(
                "livestream.hls_storage_backend '{value}' must be one of: memory, file, oss"
            ))),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HlsOssConfig {
    /// S3/OSS endpoint, for example `https://s3.amazonaws.com` or `https://minio.example.com`.
    pub endpoint: String,
    /// Access key ID used by the object storage backend.
    pub access_key_id: String,
    /// Secret access key used by the object storage backend.
    pub secret_access_key: String,
    /// Bucket name.
    pub bucket: String,
    /// Optional S3 region.
    pub region: Option<String>,
    /// Object key prefix inside the bucket, for example `synctv/hls/`.
    pub base_path: String,
}

impl Default for HlsOssConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            bucket: String::new(),
            region: None,
            base_path: "hls/".to_string(),
        }
    }
}

impl std::fmt::Debug for HlsOssConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HlsOssConfig")
            .field("endpoint", &self.endpoint)
            .field("access_key_id", &"<redacted>")
            .field("secret_access_key", &"<redacted>")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("base_path", &self.base_path)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LivestreamConfig {
    pub rtmp_port: u16,
    /// Publicly reachable RTMP host returned to publishers.
    ///
    /// If empty, falls back to `server.advertise_host`. Use this when the
    /// cluster advertise address is internal-only (pod IP / service DNS) but
    /// publishers must connect via an external ingress or hostname.
    pub public_rtmp_host: String,
    pub gop_cache_size: u32,
    /// Idle timeout before auto-stopping a pull stream (seconds)
    pub stream_timeout_seconds: u64,
    /// How often to check for idle streams (seconds)
    pub cleanup_check_interval_seconds: u64,
    /// Max retries for pull stream connections
    pub pull_max_retries: u32,
    /// Initial backoff for pull retries (milliseconds)
    pub pull_initial_backoff_ms: u64,
    /// Max backoff for pull retries (milliseconds)
    pub pull_max_backoff_ms: u64,
    /// Max FLV tag size to accept (bytes, prevents OOM)
    pub max_flv_tag_size_bytes: usize,
    /// Maximum memory (in megabytes) for the GOP cache across all GOPs per stream.
    /// When exceeded, the oldest GOP is evicted even if `gop_cache_size` hasn't
    /// been reached. Default: 100 MB. Set to 0 to use the built-in default (50 MB).
    pub gop_cache_max_memory_mb: u64,
    /// Maximum memory (in megabytes) for in-memory HLS segment storage.
    /// 0 means use the built-in default (512 MB).
    pub hls_memory_max_mb: u64,
    /// HLS segment storage backend.
    ///
    /// - `memory`: in-process memory storage.
    /// - `file`: filesystem storage at `hls_storage_path`.
    /// - `oss`: S3-compatible object storage configured by `hls_oss`.
    pub hls_storage_backend: HlsStorageBackend,
    /// Whether HLS segment storage is on shared storage accessible by all replicas.
    ///
    /// Only meaningful for the `file` backend. Set to true when
    /// `hls_storage_path` is backed by a filesystem mount visible to every
    /// replica, such as NFS or a RWX CSI/PVC volume.
    ///
    /// Default: false (local storage, single-node safe).
    pub hls_shared_storage: bool,
    /// Base path for HLS segment storage.
    ///
    /// Used for validation: paths that are obviously local-only (e.g. /tmp/)
    /// trigger a stronger warning in cluster mode even when `hls_shared_storage=true`.
    /// Required when `hls_storage_backend=file`.
    /// Relative paths are resolved against the effective `data_dir`.
    pub hls_storage_path: String,
    /// S3-compatible object storage settings used when `hls_storage_backend=oss`.
    pub hls_oss: HlsOssConfig,
    /// Maximum HTTP-FLV connection duration in seconds.
    ///
    /// Prevents slow-client `DoS` attacks by enforcing a maximum connection lifetime.
    /// Set to 0 for no limit (not recommended for production).
    /// Default: 86400 (24 hours).
    pub flv_max_connection_duration_seconds: u64,
    /// HTTP-FLV write timeout in seconds.
    ///
    /// Maximum time to wait for a client to accept data before terminating the connection.
    /// This protects against slow clients that read data very slowly.
    /// Set to 0 to disable (not recommended for production).
    /// Default: 30 seconds.
    pub flv_write_timeout_seconds: u64,
}

impl Default for LivestreamConfig {
    fn default() -> Self {
        Self {
            rtmp_port: 1935,
            public_rtmp_host: String::new(),
            gop_cache_size: 2,
            stream_timeout_seconds: 300,
            cleanup_check_interval_seconds: 60,
            pull_max_retries: 10,
            pull_initial_backoff_ms: 1000,
            pull_max_backoff_ms: 30_000,
            max_flv_tag_size_bytes: 10 * 1024 * 1024,
            gop_cache_max_memory_mb: 100,
            hls_memory_max_mb: 0,
            hls_storage_backend: HlsStorageBackend::Memory,
            hls_shared_storage: false,
            hls_storage_path: String::new(),
            hls_oss: HlsOssConfig::default(),
            flv_max_connection_duration_seconds: 86400, // 24 hours
            flv_write_timeout_seconds: 30,
        }
    }
}

/// WebAuthn/passkey configuration.
///
/// Public product wording should use "passkey"; configuration keeps the
/// standards name because these values are WebAuthn relying-party parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebAuthnConfig {
    /// Enable passkey registration and login endpoints.
    pub enabled: bool,
    /// Relying party ID. This must be the effective domain of `rp_origin`.
    pub rp_id: String,
    /// Primary origin, for example `https://app.example.com`.
    pub rp_origin: String,
    /// Human-readable relying party name shown by authenticators.
    pub rp_name: String,
    /// Extra accepted origins for native apps or alternate frontends.
    pub allowed_origins: Vec<String>,
    /// Allow subdomains of configured origins. Keep false unless required.
    pub allow_subdomains: bool,
    /// Ignore origin port when validating assertions. Useful for local dev only.
    pub allow_any_port: bool,
    /// Browser authenticator challenge timeout.
    pub timeout_seconds: u64,
}

impl Default for WebAuthnConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rp_id: String::new(),
            rp_origin: String::new(),
            rp_name: "SyncTV".to_string(),
            allowed_origins: Vec::new(),
            allow_subdomains: false,
            allow_any_port: false,
            timeout_seconds: 300,
        }
    }
}

/// HTTP transport configuration for a local provider adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalProviderHttpConfig {
    /// Total timeout for one upstream provider request.
    pub request_timeout_seconds: u64,
    /// Timeout for establishing an upstream provider connection.
    pub connect_timeout_seconds: u64,
}

impl Default for LocalProviderHttpConfig {
    fn default() -> Self {
        Self {
            request_timeout_seconds: 30,
            connect_timeout_seconds: 10,
        }
    }
}

/// Local media provider adapter configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MediaProvidersConfig {
    /// Local Alist adapter configuration.
    pub alist: LocalProviderHttpConfig,
    /// Local Bilibili adapter configuration.
    pub bilibili: LocalProviderHttpConfig,
    /// Local Emby/Jellyfin adapter configuration.
    pub emby: LocalProviderHttpConfig,
}

/// WebRTC configuration for audio/video calls
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebRTCConfig {
    /// WebRTC operation mode
    pub mode: WebRTCMode,

    // STUN Configuration
    /// Enable built-in STUN server
    pub enable_builtin_stun: bool,
    /// STUN server port
    pub stun_port: u16,
    /// STUN server bind host
    pub stun_host: String,
    /// STUN server external address for reflexive candidates.
    /// In K8s/NAT environments, set this to the routable address
    /// (e.g., pod IP or service IP). If empty, falls back to
    /// `advertise_host:stun_port`.
    pub stun_external_addr: String,

    /// Filter private/internal ICE candidates before sending to clients.
    /// When true (default), host candidates with private IPs (RFC 1918,
    /// loopback, link-local) are stripped to prevent leaking internal
    /// network topology. Set to false in development or when clients are
    /// on the same private network.
    pub filter_private_ice_candidates: bool,
}

/// WebRTC operation mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebRTCMode {
    /// Pure P2P mode (zero server cost)
    /// - Signaling only, no STUN
    /// - Best for: personal deployments
    /// - Connection success rate: ~70-75%
    SignalingOnly,

    /// P2P with STUN support (recommended for most deployments)
    /// - P2P connections with NAT traversal
    /// - STUN for reflexive candidates
    /// - Best for: small to medium deployments
    /// - Connection success rate depends on peer NAT compatibility
    PeerToPeer,
}

impl Default for WebRTCConfig {
    fn default() -> Self {
        Self {
            // Default to PeerToPeer mode (recommended for most deployments)
            mode: WebRTCMode::PeerToPeer,

            // STUN enabled by default
            enable_builtin_stun: true,
            stun_port: 3478,
            stun_host: "0.0.0.0".to_string(),
            stun_external_addr: String::new(),

            filter_private_ice_candidates: true,
        }
    }
}

/// Email configuration for SMTP
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmailConfig {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: String,
    pub from_email: String,
    pub from_name: String,
    pub use_tls: bool,
}

impl std::fmt::Debug for EmailConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailConfig")
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_username", &self.smtp_username)
            .field("smtp_password", &"<redacted>")
            .field("from_email", &self.from_email)
            .field("from_name", &self.from_name)
            .field("use_tls", &self.use_tls)
            .finish()
    }
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_username: String::new(),
            smtp_password: String::new(),
            from_email: String::new(),
            from_name: "SyncTV".to_string(),
            use_tls: true,
        }
    }
}

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
        Self::load_with_env(config_file, env, None)
    }

    pub fn load_with_env_map_and_data_dir_override(
        config_file: Option<&str>,
        env: &HashMap<String, String>,
        data_dir_override: Option<&str>,
    ) -> Result<Self, ConfigError> {
        Self::load_with_env(config_file, env, data_dir_override)
    }

    fn load_with_env(
        config_file: Option<&str>,
        env: &HashMap<String, String>,
        data_dir_override: Option<&str>,
    ) -> Result<Self, ConfigError> {
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
        let mut config = match config_file {
            Some(path) if Path::new(path).exists() => Self::load_config_file(path)?,
            _ => Self::default(),
        };

        // Apply SYNCTV_* environment variable overrides (single underscore format).
        // We don't use the config crate's Environment source because its separator
        // cannot distinguish nesting from underscores within field names.
        // Instead, every SYNCTV_ env var is mapped explicitly here.
        config.apply_env_overrides_with(&get_env)?;
        config.resolve_owned_local_paths(
            config_file
                .filter(|path| Path::new(path).exists())
                .map(Path::new),
            env.contains_key("SYNCTV_DATA_DIR"),
            data_dir_override,
        );
        config.resolve_time_defaults_with(&get_env)?;
        Self::emit_unknown_synctv_env_var_warnings(env, &seen_env_keys.into_inner());

        Ok(config)
    }

    fn emit_unknown_config_file_warnings(path: &Path, unknown_keys: &[String]) {
        if !unknown_keys.is_empty() {
            eprintln!(
                "Warning: ignoring unsupported config file key(s) in {}: {}",
                absolute_display_path(path),
                unknown_keys.join(", ")
            );
        }
    }

    fn load_config_file(path: &str) -> Result<Self, ConfigError> {
        let path = Path::new(path);
        let (config, unknown_keys) = Self::deserialize_config_file(path)?;
        Self::emit_unknown_config_file_warnings(path, &unknown_keys);
        Ok(config)
    }

    #[cfg(test)]
    fn collect_unknown_config_file_keys(path: &str) -> Result<Vec<String>, ConfigError> {
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

    fn emit_unknown_synctv_env_var_warnings(
        env: &HashMap<String, String>,
        seen_env_keys: &std::collections::HashSet<String>,
    ) {
        let unknown_keys = Self::collect_unknown_synctv_env_vars(env, seen_env_keys);

        if !unknown_keys.is_empty() {
            eprintln!(
                "Warning: ignoring unsupported SYNCTV_ environment variable(s): {}",
                unknown_keys.join(", ")
            );
        }
    }

    fn collect_unknown_synctv_env_vars(
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
        if !Path::new(path).exists() {
            return Err(ConfigError::Message(format!(
                "config file not found: {path}"
            )));
        }
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

        format!(
            "postgresql://{}:{}@{}:{}/{}",
            self.database.username,
            self.database.password,
            self.database.host,
            self.database.port,
            self.database.name
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

        let authority = if !self.redis.username.is_empty() {
            format!(
                "{}:{}@{}:{}",
                self.redis.username, self.redis.password, self.redis.host, self.redis.port
            )
        } else if !self.redis.password.is_empty() {
            format!(
                ":{}@{}:{}",
                self.redis.password, self.redis.host, self.redis.port
            )
        } else {
            format!("{}:{}", self.redis.host, self.redis.port)
        };

        format!("redis://{authority}/{}", self.redis.database)
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

    fn advertise_host_with(&self, get_env: &impl Fn(&str) -> Option<String>) -> String {
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

    fn has_explicit_advertise_host_source(
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
        if !self.livestream.public_rtmp_host.is_empty() {
            return self.livestream.public_rtmp_host.clone();
        }

        if !self.server.advertise_host.is_empty() {
            return self.server.advertise_host.clone();
        }

        if let Some(pod_ip) = process_env("POD_IP").filter(|value| !value.is_empty()) {
            return pod_ip;
        }

        self.local_publish_host()
    }

    /// Apply environment variable overrides using single-underscore format.
    ///
    /// Format: `SYNCTV_<SECTION>_<FIELD>=<value>`
    ///
    /// Examples:
    /// - `SYNCTV_SERVER_HOST=0.0.0.0`
    /// - `SYNCTV_DATABASE_URL=postgresql://...`
    /// - `SYNCTV_SERVER_ADVERTISE_HOST=10.0.0.1`
    fn apply_env_overrides_with(
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
        env_override_str(
            "SYNCTV_SERVER_CLUSTER_SECRET",
            &mut self.server.cluster_secret,
        );
        env_override_str_file(
            "SYNCTV_SERVER_CLUSTER_SECRET_FILE",
            "server.cluster_secret",
            &mut self.server.cluster_secret,
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
            "SYNCTV_DATABASE_USER",
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
        env_override_str("SYNCTV_DATABASE_USER", &mut self.database.username);
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

        env_override_str("SYNCTV_REDIS_URL", &mut self.redis.url);
        env_override_str_file("SYNCTV_REDIS_URL_FILE", "redis.url", &mut self.redis.url)?;
        env_override_str("SYNCTV_REDIS_HOST", &mut self.redis.host);
        env_override_parse("SYNCTV_REDIS_PORT", &mut self.redis.port)?;
        env_override_str("SYNCTV_REDIS_USER", &mut self.redis.username);
        env_override_str("SYNCTV_REDIS_USERNAME", &mut self.redis.username);
        env_override_str("SYNCTV_REDIS_PASSWORD", &mut self.redis.password);
        env_override_str_file(
            "SYNCTV_REDIS_PASSWORD_FILE",
            "redis.password",
            &mut self.redis.password,
        )?;
        env_override_parse("SYNCTV_REDIS_DATABASE", &mut self.redis.database)?;
        env_override_parse(
            "SYNCTV_REDIS_CONNECT_TIMEOUT_SECONDS",
            &mut self.redis.connect_timeout_seconds,
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
        env_override_bool(
            "SYNCTV_LIVESTREAM_HLS_SHARED_STORAGE",
            &mut self.livestream.hls_shared_storage,
        )?;
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
        env_override_parse(
            "SYNCTV_LIVESTREAM_FLV_MAX_CONNECTION_DURATION_SECONDS",
            &mut self.livestream.flv_max_connection_duration_seconds,
        )?;
        env_override_parse(
            "SYNCTV_LIVESTREAM_FLV_WRITE_TIMEOUT_SECONDS",
            &mut self.livestream.flv_write_timeout_seconds,
        )?;

        env_override_str("SYNCTV_EMAIL_SMTP_HOST", &mut self.email.smtp_host);
        env_override_parse("SYNCTV_EMAIL_SMTP_PORT", &mut self.email.smtp_port)?;
        env_override_str("SYNCTV_EMAIL_SMTP_USERNAME", &mut self.email.smtp_username);
        env_override_str("SYNCTV_EMAIL_SMTP_PASSWORD", &mut self.email.smtp_password);
        env_override_str_file(
            "SYNCTV_EMAIL_SMTP_PASSWORD_FILE",
            "email.smtp_password",
            &mut self.email.smtp_password,
        )?;
        env_override_str("SYNCTV_EMAIL_FROM_EMAIL", &mut self.email.from_email);
        env_override_str("SYNCTV_EMAIL_FROM_NAME", &mut self.email.from_name);
        env_override_bool("SYNCTV_EMAIL_USE_TLS", &mut self.email.use_tls)?;

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
            "SYNCTV_MESSAGING_RATE_LIMITS_DANMAKU_PER_SECOND",
            &mut self.messaging_rate_limits.danmaku_per_second,
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
            "SYNCTV_BOOTSTRAP_ROOT_EMAIL",
            &mut self.bootstrap.root_email,
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
        env_override_bool(
            "SYNCTV_PROXY_SLICE_CACHE_FILE_BACKEND_ENABLED",
            &mut self.proxy_slice_cache.file_backend_enabled,
        )?;
        env_override_str(
            "SYNCTV_PROXY_SLICE_CACHE_FILE_CACHE_DIR",
            &mut self.proxy_slice_cache.file_cache_dir,
        );

        env_override_parse(
            "SYNCTV_HTTP_RATE_LIMITS_AUTH_MAX_REQUESTS",
            &mut self.http_rate_limits.auth_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_HTTP_RATE_LIMITS_AUTH_WINDOW_SECONDS",
            &mut self.http_rate_limits.auth_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_HTTP_RATE_LIMITS_WRITE_MAX_REQUESTS",
            &mut self.http_rate_limits.write_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_HTTP_RATE_LIMITS_WRITE_WINDOW_SECONDS",
            &mut self.http_rate_limits.write_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_HTTP_RATE_LIMITS_READ_MAX_REQUESTS",
            &mut self.http_rate_limits.read_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_HTTP_RATE_LIMITS_READ_WINDOW_SECONDS",
            &mut self.http_rate_limits.read_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_HTTP_RATE_LIMITS_MEDIA_MAX_REQUESTS",
            &mut self.http_rate_limits.media_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_HTTP_RATE_LIMITS_MEDIA_WINDOW_SECONDS",
            &mut self.http_rate_limits.media_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_HTTP_RATE_LIMITS_ADMIN_MAX_REQUESTS",
            &mut self.http_rate_limits.admin_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_HTTP_RATE_LIMITS_ADMIN_WINDOW_SECONDS",
            &mut self.http_rate_limits.admin_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_HTTP_RATE_LIMITS_STREAMING_MAX_REQUESTS",
            &mut self.http_rate_limits.streaming_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_HTTP_RATE_LIMITS_STREAMING_WINDOW_SECONDS",
            &mut self.http_rate_limits.streaming_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_HTTP_RATE_LIMITS_WEBSOCKET_MAX_REQUESTS",
            &mut self.http_rate_limits.websocket_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_HTTP_RATE_LIMITS_WEBSOCKET_WINDOW_SECONDS",
            &mut self.http_rate_limits.websocket_window_seconds,
        )?;

        env_override_parse(
            "SYNCTV_GRPC_RATE_LIMITS_AUTH_MAX_REQUESTS",
            &mut self.grpc_rate_limits.auth_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_GRPC_RATE_LIMITS_AUTH_WINDOW_SECONDS",
            &mut self.grpc_rate_limits.auth_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_GRPC_RATE_LIMITS_EMAIL_MAX_REQUESTS",
            &mut self.grpc_rate_limits.email_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_GRPC_RATE_LIMITS_EMAIL_WINDOW_SECONDS",
            &mut self.grpc_rate_limits.email_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_GRPC_RATE_LIMITS_MEDIA_MAX_REQUESTS",
            &mut self.grpc_rate_limits.media_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_GRPC_RATE_LIMITS_MEDIA_WINDOW_SECONDS",
            &mut self.grpc_rate_limits.media_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_GRPC_RATE_LIMITS_WRITE_MAX_REQUESTS",
            &mut self.grpc_rate_limits.write_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_GRPC_RATE_LIMITS_WRITE_WINDOW_SECONDS",
            &mut self.grpc_rate_limits.write_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_GRPC_RATE_LIMITS_ADMIN_MAX_REQUESTS",
            &mut self.grpc_rate_limits.admin_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_GRPC_RATE_LIMITS_ADMIN_WINDOW_SECONDS",
            &mut self.grpc_rate_limits.admin_window_seconds,
        )?;
        env_override_parse(
            "SYNCTV_GRPC_RATE_LIMITS_READ_MAX_REQUESTS",
            &mut self.grpc_rate_limits.read_max_requests,
        )?;
        env_override_parse(
            "SYNCTV_GRPC_RATE_LIMITS_READ_WINDOW_SECONDS",
            &mut self.grpc_rate_limits.read_window_seconds,
        )?;

        Ok(())
    }

    fn resolve_owned_local_paths(
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

    fn resolve_time_defaults_with(
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
        self.validate_with_env(&process_env)
    }

    pub fn validate_with_env_map(&self, env: &HashMap<String, String>) -> Result<(), Vec<String>> {
        self.validate_with_env(&|name| env.get(name).cloned())
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

    fn validate_with_env(
        &self,
        get_env: &impl Fn(&str) -> Option<String>,
    ) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        // Validate port numbers are in valid range (1-65535)
        let ports_to_check: &[(&str, u16)] = &[
            ("server.port", self.server.port),
            ("livestream.rtmp_port", self.livestream.rtmp_port),
        ];
        for (name, port) in ports_to_check {
            if *port == 0 {
                errors.push(format!("{name} must be between 1 and 65535, got 0"));
            }
        }

        if let Some(sqids) = self.public_ids.sqids.as_ref() {
            if let Some(alphabet) = sqids.alphabet.as_ref() {
                if alphabet.is_empty() {
                    errors.push("public_ids.sqids.alphabet must not be empty when set".to_string());
                } else if alphabet.chars().any(|ch| ch.len_utf8() > 1) {
                    errors.push(
                        "public_ids.sqids.alphabet must contain only single-byte characters"
                            .to_string(),
                    );
                } else if alphabet.chars().any(|ch| !ch.is_ascii_alphanumeric()) {
                    errors.push(
                        "public_ids.sqids.alphabet must contain only ASCII alphanumeric characters"
                            .to_string(),
                    );
                } else if alphabet
                    .chars()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    != alphabet.chars().count()
                {
                    errors.push(
                        "public_ids.sqids.alphabet must not contain duplicate characters"
                            .to_string(),
                    );
                } else if alphabet.chars().count() < 3 {
                    errors.push(
                        "public_ids.sqids.alphabet must contain at least 3 characters".to_string(),
                    );
                }
            }
        }
        if let Err(error) = crate::PublicIdCodec::from_config(&self.public_ids) {
            errors.push(error);
        }

        if !self.security.credential_encryption_key.is_empty() {
            let key = self.security.credential_encryption_key.trim();
            if key.len() != 64 {
                errors.push(
                    "security.credential_encryption_key must be a 64-character hex string (32 bytes for AES-256-GCM)"
                        .to_string(),
                );
            } else if !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                errors.push(
                    "security.credential_encryption_key must contain only hexadecimal characters"
                        .to_string(),
                );
            }
        }

        let opaque_secret = self.security.opaque_server_setup_secret.trim();
        if opaque_secret.is_empty() {
            errors.push("security.opaque_server_setup_secret is empty. Set SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET or SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET_FILE to a stable random value".to_string());
        } else if opaque_secret == "change-me-in-production"
            || opaque_secret.starts_with("CHANGE_ME_")
        {
            errors.push("security.opaque_server_setup_secret appears to be a placeholder. Set it to a stable random value (openssl rand -base64 48)".to_string());
        } else if opaque_secret.len() < 32 {
            errors.push(format!(
                "security.opaque_server_setup_secret is too short ({} chars). Minimum 32 characters required for OPAQUE setup stability.",
                opaque_secret.len()
            ));
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

        if crate::logging::parse_log_level(&self.logging.level).is_err() {
            errors.push(format!(
                "logging.level '{}' must be one of: trace, debug, info, warn, error",
                self.logging.level
            ));
        }
        if !matches!(self.logging.format.as_str(), "pretty" | "json") {
            errors.push(format!(
                "logging.format '{}' must be either 'pretty' or 'json'",
                self.logging.format
            ));
        }
        if let Some(filter) = self.logging.filter.as_deref().map(str::trim) {
            if !filter.is_empty() && tracing_subscriber::EnvFilter::try_new(filter).is_err() {
                errors.push(format!(
                    "logging.filter '{filter}' is not a valid tracing filter specification"
                ));
            }
        }

        // Validate JWT secret
        if self.jwt.secret.is_empty() {
            errors.push("JWT secret is empty".to_string());
        } else if self.jwt.secret == "change-me-in-production"
            || self.jwt.secret.starts_with("CHANGE_ME_")
        {
            errors.push("JWT secret appears to be a placeholder. Set SYNCTV_JWT_SECRET to a strong random value (openssl rand -base64 48)".to_string());
        } else if self.jwt.secret.len() < 32 {
            errors.push(format!(
                "JWT secret is too short ({} chars). Minimum 32 characters required for security. \
                 Set SYNCTV_JWT_SECRET to a strong random value.",
                self.jwt.secret.len()
            ));
        }

        // Validate root credentials
        if self.bootstrap.create_root_user {
            let pwd = &self.bootstrap.root_password;
            if pwd.is_empty() {
                errors.push("Root password is empty. Set SYNCTV_BOOTSTRAP_ROOT_PASSWORD environment variable".to_string());
            } else if pwd == "root" {
                errors.push("Root password is set to default value 'root'. Set SYNCTV_BOOTSTRAP_ROOT_PASSWORD environment variable".to_string());
            } else {
                // Only run complexity checks once a non-empty, non-placeholder
                // password is present; otherwise a single root cause fans out
                // into multiple derivative errors.
                if pwd.len() < 12 {
                    errors.push("Root password must be at least 12 characters".to_string());
                }
                if !pwd.chars().any(char::is_uppercase) {
                    errors.push(
                        "Root password must contain at least one uppercase letter".to_string(),
                    );
                }
                if !pwd.chars().any(char::is_lowercase) {
                    errors.push(
                        "Root password must contain at least one lowercase letter".to_string(),
                    );
                }
                if !pwd.chars().any(|c| c.is_ascii_digit()) {
                    errors.push("Root password must contain at least one digit".to_string());
                }
            }
            if self.bootstrap.root_username.len() < 3 {
                errors.push("Root username must be at least 3 characters".to_string());
            }
            if !self.bootstrap.root_email.is_empty()
                && crate::validation::EmailValidator::new()
                    .validate(&self.bootstrap.root_email)
                    .is_err()
            {
                errors.push("Root email must be a valid email address".to_string());
            }
        }

        // Validate port conflicts: RTMP must not collide with the unified API port.
        if self.livestream.rtmp_port == self.server.port {
            errors.push(format!(
                "livestream.rtmp_port ({}) and server.port ({}) must be different",
                self.livestream.rtmp_port, self.server.port
            ));
        }

        if self.metrics.enabled {
            if self.metrics.port == 0 {
                errors.push("metrics.port must be between 1 and 65535, got 0".to_string());
            }
            if self.metrics.port == self.server.port {
                errors.push(format!(
                    "metrics bind target ({}) must be different from server.host:port ({})",
                    self.metrics_address(),
                    self.api_address()
                ));
            }
            if self.metrics.port == self.livestream.rtmp_port {
                errors.push(format!(
                    "metrics bind target ({}) must be different from livestream port ({})",
                    self.metrics_address(),
                    self.livestream.rtmp_port
                ));
            }
            if self.management.enabled
                && matches!(self.management.transport, ManagementTransport::Tcp)
                && self.metrics.port == self.management.port
            {
                errors.push(format!(
                    "metrics bind target ({}) must be different from management bind target ({})",
                    self.metrics_address(),
                    self.management_bind_target()
                ));
            }
        }

        // Validate gRPC max message size (prevent OOM attacks)
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
                    if self.management.port == self.server.port {
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

        if !redis_url_present && !redis_split_present && !self.cluster_runtime_enabled() {
            tracing::info!(
                "Redis is not configured — running in standalone mode with in-memory fallbacks. \
                 All features work, but data (rate limits, brute-force counters, token blacklist) \
                 will not persist across restarts."
            );
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
        if self.messaging_rate_limits.danmaku_per_second == 0 {
            errors.push(
                "messaging_rate_limits.danmaku_per_second must be greater than 0".to_string(),
            );
        }
        if self.messaging_rate_limits.window_seconds == 0 {
            errors.push("messaging_rate_limits.window_seconds must be greater than 0".to_string());
        }

        // Validate livestream config
        if self.livestream.stream_timeout_seconds == 0 {
            errors.push("livestream.stream_timeout_seconds must be greater than 0".to_string());
        }
        if self.livestream.cleanup_check_interval_seconds == 0 {
            errors.push(
                "livestream.cleanup_check_interval_seconds must be greater than 0".to_string(),
            );
        }

        // Validate email config (only when SMTP is configured)
        if !self.email.smtp_host.is_empty() {
            if self.email.smtp_port == 0 {
                errors.push(
                    "email.smtp_port must be between 1 and 65535 when smtp_host is set".to_string(),
                );
            }
            if self.email.from_email.is_empty() {
                errors
                    .push("email.from_email must be set when smtp_host is configured".to_string());
            } else if !self.email.from_email.contains('@')
                || self.email.from_email.starts_with('@')
                || self.email.from_email.ends_with('@')
            {
                errors.push(format!(
                    "email.from_email '{}' is not a valid email address",
                    self.email.from_email
                ));
            }
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

        match self.livestream.hls_storage_backend {
            HlsStorageBackend::Memory => {
                if self.livestream.hls_shared_storage {
                    errors.push(
                        "livestream.hls_shared_storage=true is only valid when livestream.hls_storage_backend='file'"
                            .to_string(),
                    );
                }
            }
            HlsStorageBackend::File => {
                if self.livestream.hls_storage_path.trim().is_empty() {
                    errors.push(
                        "livestream.hls_storage_path must be set when livestream.hls_storage_backend='file'"
                            .to_string(),
                    );
                }
            }
            HlsStorageBackend::Oss => {
                if self.livestream.hls_shared_storage {
                    errors.push(
                        "livestream.hls_shared_storage is not used with livestream.hls_storage_backend='oss'; object storage is inherently shared across replicas"
                            .to_string(),
                    );
                }

                let oss = &self.livestream.hls_oss;
                if oss.endpoint.trim().is_empty() {
                    errors.push(
                        "livestream.hls_oss.endpoint must be set when livestream.hls_storage_backend='oss'"
                            .to_string(),
                    );
                }
                if oss.bucket.trim().is_empty() {
                    errors.push(
                        "livestream.hls_oss.bucket must be set when livestream.hls_storage_backend='oss'"
                            .to_string(),
                    );
                }
                if oss.access_key_id.trim().is_empty() {
                    errors.push(
                        "livestream.hls_oss.access_key_id must be set when livestream.hls_storage_backend='oss'"
                            .to_string(),
                    );
                }
                if oss.secret_access_key.trim().is_empty() {
                    errors.push(
                        "livestream.hls_oss.secret_access_key must be set when livestream.hls_storage_backend='oss'"
                            .to_string(),
                    );
                }
            }
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
                "cluster mode requires Redis to be configured. \
                 Configure standalone Redis via redis.url or redis.host/redis.port \
                 before enabling cluster.enabled=true."
                    .to_string(),
            );
        }

        if cluster_mode_active && self.cluster.discovery_mode == ClusterDiscoveryMode::K8sDns {
            if !cfg!(feature = "k8s") {
                errors.push(
                    "cluster.discovery_mode='k8s_dns' requires the 'k8s' feature to be compiled in. \
                     Rebuild with Kubernetes support enabled."
                        .to_string(),
                );
            }

            match get_env("HEADLESS_SERVICE_NAME") {
                Some(value) if !value.trim().is_empty() => {}
                _ => errors.push(
                    "cluster.discovery_mode='k8s_dns' requires HEADLESS_SERVICE_NAME to be set \
                     during configuration validation."
                        .to_string(),
                ),
            }

            match get_env("POD_NAMESPACE") {
                Some(value) if !value.trim().is_empty() => {}
                _ => errors.push(
                    "cluster.discovery_mode='k8s_dns' requires POD_NAMESPACE to be set \
                     during configuration validation."
                        .to_string(),
                ),
            }
        }

        if cluster_mode_active
            && self.cluster.leader_election_mode == ClusterLeaderElectionMode::K8sLease
        {
            if !cfg!(feature = "k8s") {
                errors.push(
                    "cluster.leader_election_mode='k8s_lease' requires the 'k8s' feature to be compiled in. \
                     Rebuild with Kubernetes support enabled or switch to 'redis'."
                        .to_string(),
                );
            }

            match get_env("POD_NAME") {
                Some(value) if !value.trim().is_empty() => {}
                _ => errors.push(
                    "cluster.leader_election_mode='k8s_lease' requires POD_NAME to be set \
                     during configuration validation."
                        .to_string(),
                ),
            }

            match get_env("POD_NAMESPACE") {
                Some(value) if !value.trim().is_empty() => {}
                _ => errors.push(
                    "cluster.leader_election_mode='k8s_lease' requires POD_NAMESPACE to be set \
                     during configuration validation."
                        .to_string(),
                ),
            }
        }

        if cluster_mode_active && self.redis.deployment_mode == RedisDeploymentMode::Sentinel {
            errors.push(
                "cluster.enabled=true is not supported with Redis Sentinel. \
                 Startup still relies on Redis distributed locks for room coordination, \
                 and Sentinel failover can create split-brain coordination windows. \
                 Switch Redis to a non-Sentinel deployment before enabling cluster mode."
                    .to_string(),
            );
        }

        // HLS storage validation in cluster mode.
        // Local backends are functional because non-publisher nodes can proxy HLS
        // playlist/segment reads to the publisher node. Shared storage is still
        // preferable for high-traffic production HLS because it avoids routing
        // every remote segment request through the publisher node.
        if cluster_mode_active {
            match self.livestream.hls_storage_backend {
                HlsStorageBackend::Oss => {}
                HlsStorageBackend::File if self.livestream.hls_shared_storage => {
                    // shared_storage=true but check for obviously-local paths
                    let path = &self.livestream.hls_storage_path;
                    let is_obviously_local = path.starts_with("/tmp/")
                        || path == "/tmp"
                        || path.starts_with("/var/tmp/")
                        || path.starts_with("/dev/shm/");
                    if is_obviously_local {
                        tracing::warn!(
                            hls_storage_path = %path,
                            "livestream.hls_shared_storage=true but hls_storage_path '{}' appears \
                             to be a local-only path. Ensure this path is actually mounted from \
                             shared storage (NFS, CSI volume) on every replica. Otherwise remote \
                             HLS requests will fall back to publisher-node gRPC proxying instead \
                             of direct shared-storage reads.",
                            path
                        );
                    }
                }
                HlsStorageBackend::File => {
                    tracing::warn!(
                        "Cluster mode is enabled with livestream.hls_storage_backend='file' and livestream.hls_shared_storage=false. \
                         HLS remains functional through publisher-node gRPC proxying, but shared filesystem storage or OSS is recommended for production multi-replica HLS."
                    );
                }
                HlsStorageBackend::Memory => {
                    tracing::warn!(
                        "Cluster mode is enabled with livestream.hls_storage_backend='memory'. \
                         HLS remains functional through publisher-node gRPC proxying, but memory storage is node-local and lost on restart. \
                         Use shared filesystem storage or OSS for production multi-replica HLS."
                    );
                }
            }
        } else if self.livestream.hls_storage_backend == HlsStorageBackend::Memory {
            // Single-node: warn about MemoryStorage only when the effective
            // storage backend is actually the in-memory default.
            tracing::warn!(
                "The default HLS storage backend is MemoryStorage, which is node-local. \
                 HLS segments are lost on restart. For production multi-replica HLS, \
                 configure livestream.hls_storage_backend='file' with shared filesystem storage \
                 or 'oss' with S3-compatible object storage."
            );
        }

        // Require cluster_secret when cluster mode is enabled.
        // An empty `cluster_secret` means that ANY node claiming to be part of the
        // cluster can call inter-node gRPC endpoints without authentication.
        // In standalone mode, `cluster_secret` is not required even with Redis
        // configured, because there are no inter-node gRPC endpoints to protect.
        if self.cluster.enabled && self.server.cluster_secret.is_empty() {
            errors.push(
                "server.cluster_secret must be set when cluster mode is enabled. \
                 An empty cluster_secret leaves inter-node gRPC endpoints unauthenticated. \
                 Generate a secret with: openssl rand -hex 32 \
                 and set it as SYNCTV_SERVER_CLUSTER_SECRET or server.cluster_secret in your config."
                    .to_string(),
            );
        }

        // Validate cluster_secret strength when set.
        if !self.server.cluster_secret.is_empty() {
            const MIN_CLUSTER_SECRET_LEN: usize = 16;
            if self.server.cluster_secret.len() < MIN_CLUSTER_SECRET_LEN {
                errors.push(format!(
                    "server.cluster_secret is too short ({} chars, minimum {}). \
                         Use: openssl rand -hex 16",
                    self.server.cluster_secret.len(),
                    MIN_CLUSTER_SECRET_LEN
                ));
            }
        }

        if self.cluster.enabled && !self.has_explicit_advertise_host_source(get_env) {
            errors.push(
                "server.advertise_host must be set explicitly when cluster mode is enabled. \
                 Refusing to fall back to the local hostname because other replicas may not be able to route to it. \
                 Set SYNCTV_SERVER_ADVERTISE_HOST (or server.advertise_host), or provide POD_IP via the Kubernetes downward API."
                    .to_string(),
            );
        }

        if self.cluster.enabled && self.advertise_host_with(get_env) == "0.0.0.0" {
            errors.push(
                "server.advertise_host must resolve to a routable address when cluster mode is enabled. \
                 The current advertise host resolves to 0.0.0.0, which other replicas cannot reach for gRPC/HLS proxying. \
                 Set SYNCTV_SERVER_ADVERTISE_HOST (or server.advertise_host) to the pod IP, node IP, or service-reachable hostname."
                    .to_string(),
            );
        }

        // Warn if cors_allowed_origins is empty
        if self.server.cors_allowed_origins.is_empty() {
            tracing::warn!(
                "server.cors_allowed_origins is empty. \
                 CORS requests will be rejected. Set allowed origins."
            );
        }

        // Validate STUN external address.
        // In cluster/K8s/NAT environments, an explicit stun_external_addr is
        // preferred, but runtime bootstrap also supports auto-detecting a
        // usable external address from advertise_host / POD_IP / cloud
        // metadata. Configuration validation should therefore not fail-closed
        // just because the explicit field is empty.
        if self.webrtc.enable_builtin_stun && self.webrtc.stun_external_addr.is_empty() {
            if self.cluster_runtime_enabled() {
                tracing::warn!(
                    "webrtc.enable_builtin_stun=true but stun_external_addr is not set in cluster mode. \
                     Startup will attempt STUN external address auto-detection from advertise_host, POD_IP, \
                     or cloud metadata. For deterministic production behavior, prefer setting \
                     webrtc.stun_external_addr explicitly."
                );
            } else {
                tracing::warn!(
                    "webrtc.enable_builtin_stun=true but stun_external_addr is not set. \
                         STUN server will advertise reflexive candidates using advertise_host. \
                         This may not work correctly behind NAT or load balancers. \
                         Set webrtc.stun_external_addr to the server's public IP:port."
                );
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

fn is_cli_only_synctv_env_var(key: &str) -> bool {
    matches!(key, "SYNCTV_MANAGEMENT_ENDPOINT")
}

/// Connection limits configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionLimitsConfig {
    /// Maximum concurrent connections per user
    pub max_per_user: usize,

    /// Maximum concurrent connections per room
    pub max_per_room: usize,

    /// Maximum total concurrent connections
    pub max_total: usize,

    /// Idle timeout in seconds (disconnect if no activity)
    pub idle_timeout_seconds: u64,

    /// Maximum connection duration in seconds
    pub max_duration_seconds: u64,

    /// Global per-connection WebSocket message rate limit (messages per second).
    /// Applies to all message types before per-type rate limiting.
    /// Prevents abuse from flooding the server with rapid messages.
    /// Defaults to 50 messages per second.
    pub ws_message_rate_limit_per_second: u32,
}

impl Default for ConnectionLimitsConfig {
    fn default() -> Self {
        Self {
            max_per_user: 5,
            max_per_room: 200,
            max_total: 10000,
            idle_timeout_seconds: 300,   // 5 minutes
            max_duration_seconds: 86400, // 24 hours
            ws_message_rate_limit_per_second: 50,
        }
    }
}

/// Bootstrap configuration for initial setup
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BootstrapConfig {
    /// Whether to create root user on first startup
    pub create_root_user: bool,
    /// Root username (default: "root")
    pub root_username: String,
    /// Optional email for the bootstrapped root user.
    pub root_email: String,
    /// Root password (IMPORTANT: Change this in production!)
    pub root_password: String,
}

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self {
            create_root_user: false,
            root_username: "root".to_string(),
            root_email: String::new(),
            root_password: String::new(),
        }
    }
}

/// Cluster channel capacity configuration
///
/// Controls the buffer sizes for internal channels used in cluster communication.
/// Larger values provide more resilience during traffic spikes but use more memory.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterDiscoveryMode {
    #[default]
    Redis,
    Static,
    K8sDns,
}

impl ClusterDiscoveryMode {
    pub const ALLOWED_VALUES: &'static str = "redis, static, k8s_dns";
}

impl fmt::Display for ClusterDiscoveryMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Redis => "redis",
            Self::Static => "static",
            Self::K8sDns => "k8s_dns",
        };
        f.write_str(value)
    }
}

impl FromStr for ClusterDiscoveryMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "redis" => Ok(Self::Redis),
            "static" => Ok(Self::Static),
            "k8s_dns" => Ok(Self::K8sDns),
            _ => Err(format!("expected one of: {}", Self::ALLOWED_VALUES)),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterLeaderElectionMode {
    #[default]
    Redis,
    K8sLease,
}

impl ClusterLeaderElectionMode {
    pub const ALLOWED_VALUES: &'static str = "redis, k8s_lease";
}

impl fmt::Display for ClusterLeaderElectionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Redis => "redis",
            Self::K8sLease => "k8s_lease",
        };
        f.write_str(value)
    }
}

impl FromStr for ClusterLeaderElectionMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "redis" => Ok(Self::Redis),
            "k8s_lease" => Ok(Self::K8sLease),
            _ => Err(format!("expected one of: {}", Self::ALLOWED_VALUES)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClusterChannelConfig {
    /// Whether cluster mode is explicitly enabled.
    ///
    /// When true, Redis is **mandatory** and startup will fail if no standalone
    /// Redis backend is configured through `redis.url` or `redis.host`/`redis.port`.
    /// Cluster mode uses Redis for:
    /// - Cross-replica pub/sub (`PlaybackStateChanged`, `KickUser`, etc.)
    /// - Distributed leader election (singleton tasks)
    /// - Node registry and health monitoring
    /// - Brute-force protection and rate limiting (shared counters)
    ///
    /// Defaults to false (single-node mode). In Kubernetes / Docker Compose
    /// multi-replica deployments, set this to true.
    pub enabled: bool,

    /// Capacity for the high-priority critical event channel.
    /// Critical events (`KickPublisher`, `KickUser`, `PermissionChanged`) are never dropped;
    /// when this channel is full, senders block until space is available.
    /// Default: 1000
    pub critical_channel_capacity: usize,

    /// Capacity for the normal-priority Redis publish channel.
    /// Normal events are dropped with a warning when this channel is full
    /// (e.g., during a prolonged Redis outage).
    /// Default: 10000
    pub publish_channel_capacity: usize,

    /// Discovery mode for cluster node registration.
    /// - `redis`: Use Redis-based node registry (default, works everywhere)
    /// - "`k8s_dns"`: Use Kubernetes headless service DNS for peer discovery
    ///   (requires `HEADLESS_SERVICE_NAME` and `POD_NAMESPACE` env vars).
    ///   NOTE: K8s DNS mode still requires Redis for health monitoring, load
    ///   balancing, and cluster pub/sub. DNS only supplements peer discovery
    ///   (faster detection of new pods). Without Redis, `k8s_dns` mode provides
    ///   DNS resolution only -- no `NodeRegistry`, `HealthMonitor`, or `LoadBalancer`.
    pub discovery_mode: ClusterDiscoveryMode,

    /// Leader election mode for singleton operations.
    /// - `redis`: Use Redis-based distributed locks (default, works everywhere)
    /// - "`k8s_lease"`: Use Kubernetes coordination.k8s.io/v1 Lease resource
    ///   (requires `POD_NAME` and `POD_NAMESPACE` env vars, RBAC permissions)
    pub leader_election_mode: ClusterLeaderElectionMode,

    /// Static peer addresses for non-K8s / non-Redis cluster discovery.
    /// When configured, each peer is periodically health-checked via gRPC
    /// and registered into the `NodeRegistry` if alive.
    /// Example: `["host1:50051", "host2:50051"]`
    pub peers: Vec<String>,

    /// How far back (in seconds) to replay Redis Stream events when a new node
    /// first connects to the cluster. Replaying recent history ensures that
    /// events published just before this node subscribed are not silently missed.
    /// Setting this too large increases startup latency in busy clusters; setting
    /// it too small risks missing events published during a brief delay between
    /// Redis subscription and stream snapshot.
    /// Default: 300 (5 minutes)
    pub catchup_window_secs: u64,

    /// Maximum number of entries per Redis Stream (approximate, uses MAXLEN ~).
    /// Controls how many events are retained in each per-room stream for catch-up
    /// after reconnection. In high-throughput scenarios, increase this to avoid
    /// trimming events that disconnected nodes still need to catch up on.
    /// Default: 10000
    pub stream_max_length: usize,
}

impl Default for ClusterChannelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            discovery_mode: ClusterDiscoveryMode::Redis,
            leader_election_mode: ClusterLeaderElectionMode::Redis,
            peers: Vec::new(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
        }
    }
}

/// Password complexity requirements
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PasswordComplexityConfig {
    pub min_length: usize,
    pub require_uppercase: bool,
    pub require_lowercase: bool,
    pub require_digit: bool,
    pub require_special: bool,
    pub max_repeated_chars: usize,
}

impl Default for PasswordComplexityConfig {
    fn default() -> Self {
        Self {
            min_length: 8,
            require_uppercase: true,
            require_lowercase: true,
            require_digit: true,
            require_special: false,
            max_repeated_chars: 3,
        }
    }
}

/// Internal channel buffer sizes
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BufferSizesConfig {
    /// Per-connection WebSocket outbound message queue size
    pub websocket_outbound: usize,
    /// Audit log event buffer size
    pub audit_buffer: usize,
}

impl Default for BufferSizesConfig {
    fn default() -> Self {
        Self {
            websocket_outbound: 256,
            audit_buffer: 10_000,
        }
    }
}

/// Business cache layer configuration.
///
/// Controls cache capacities and TTLs for the L1 (in-memory) and L2 (Redis)
/// tiers used by application data such as rooms, users, usernames, and
/// permissions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    /// L1 (Moka in-memory) cache max capacity for user/room caches
    pub l1_capacity: u64,
    /// L1 cache TTL in seconds
    pub l1_ttl_seconds: u64,
    /// L2 (Redis) cache TTL in seconds
    pub l2_ttl_seconds: u64,
    /// Username cache L1 max capacity
    pub username_cache_capacity: u64,
    /// Username cache L2 (Redis) TTL in seconds
    pub username_cache_ttl_seconds: u64,
    /// Permission cache max capacity (reserved for future use)
    pub permission_cache_capacity: u64,
    /// Permission cache TTL in seconds (reserved for future use)
    pub permission_cache_ttl_seconds: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            l1_capacity: 500,
            l1_ttl_seconds: 300, // 5 minutes (was hardcoded as 5 min TTL)
            l2_ttl_seconds: 300, // 5 minutes
            username_cache_capacity: 1000,
            username_cache_ttl_seconds: 3600, // 1 hour
            permission_cache_capacity: 1000,
            permission_cache_ttl_seconds: 300,
        }
    }
}

/// Proxy Range slice cache configuration.
///
/// This cache belongs to media proxying, not the business L1/L2 cache layer.
/// It stores byte-range slices only and never turns upstream responses into a
/// full-file cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxySliceCacheConfig {
    /// Whether proxy slice caching is enabled at process startup.
    pub enabled: bool,
    /// Whether the proxy slice cache should persist entries to disk.
    pub file_backend_enabled: bool,
    /// Root directory for persisted proxy slice cache entries.
    ///
    /// Relative paths are resolved against the effective `data_dir`.
    pub file_cache_dir: String,
}

impl Default for ProxySliceCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            file_backend_enabled: false,
            file_cache_dir: String::new(),
        }
    }
}

/// HTTP API rate limit configuration for different endpoint categories.
///
/// This is separate from the domain-level `RateLimitConfig` in
/// `synctv_core::service::rate_limit` (which controls chat/danmaku rates).
/// This struct configures the per-category request rate limits applied by the
/// shared HTTP request execution path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpRateLimitConfig {
    /// Authentication endpoints (login, register) - stricter limits
    pub auth_max_requests: u32,
    pub auth_window_seconds: u64,

    /// Write operations (create, update, delete) - moderate limits
    pub write_max_requests: u32,
    pub write_window_seconds: u64,

    /// Read operations (get, list) - relaxed limits
    pub read_max_requests: u32,
    pub read_window_seconds: u64,

    /// Media operations (add, remove media) - moderate limits
    pub media_max_requests: u32,
    pub media_window_seconds: u64,

    /// Admin operations - moderate limits to prevent brute force
    pub admin_max_requests: u32,
    pub admin_window_seconds: u64,

    /// Streaming operations (FLV/HLS) - per-user concurrency limits
    pub streaming_max_requests: u32,
    pub streaming_window_seconds: u64,

    /// WebSocket connection attempts
    pub websocket_max_requests: u32,
    pub websocket_window_seconds: u64,
}

impl Default for HttpRateLimitConfig {
    fn default() -> Self {
        Self {
            // Auth: 5 requests per minute
            auth_max_requests: 5,
            auth_window_seconds: 60,

            // Write: 30 requests per minute
            write_max_requests: 30,
            write_window_seconds: 60,

            // Read: 100 requests per minute
            read_max_requests: 100,
            read_window_seconds: 60,

            // Media: 20 requests per minute
            media_max_requests: 20,
            media_window_seconds: 60,

            // Admin: 30 requests per minute
            admin_max_requests: 30,
            admin_window_seconds: 60,

            // Streaming: 200 requests per minute (playlist + segment fetches)
            streaming_max_requests: 200,
            streaming_window_seconds: 60,

            // WebSocket: 10 connection attempts per minute
            websocket_max_requests: 10,
            websocket_window_seconds: 60,
        }
    }
}

/// gRPC API rate limit configuration for different endpoint tiers.
///
/// Mirrors the HTTP rate limit tiers but with separate values for the gRPC API.
/// By default, gRPC limits are lower than HTTP because gRPC clients are typically
/// automated (SDKs, bots) rather than human-driven browsers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GrpcRateLimitConfig {
    /// Authentication endpoints (Login, Register, `RefreshToken`)
    pub auth_max_requests: u32,
    pub auth_window_seconds: u64,

    /// Email endpoints (`SendVerification`, `PasswordReset`)
    pub email_max_requests: u32,
    pub email_window_seconds: u64,

    /// Media mutation endpoints (`AddMedia`, `RemoveMedia`, `BatchAdd`)
    pub media_max_requests: u32,
    pub media_window_seconds: u64,

    /// Write endpoints (`CreateRoom`, `UpdateRoom`, `JoinRoom`, `SendChat`)
    pub write_max_requests: u32,
    pub write_window_seconds: u64,

    /// Admin endpoints
    pub admin_max_requests: u32,
    pub admin_window_seconds: u64,

    /// Read endpoints (`GetRoom`, `ListRooms`, `GetUser`, `GetPlaylist`)
    pub read_max_requests: u32,
    pub read_window_seconds: u64,
}

impl Default for GrpcRateLimitConfig {
    fn default() -> Self {
        Self {
            // Auth: 5 requests per 60 seconds
            auth_max_requests: 5,
            auth_window_seconds: 60,

            // Email: 5 requests per 60 seconds
            email_max_requests: 5,
            email_window_seconds: 60,

            // Media: 20 requests per 60 seconds
            media_max_requests: 20,
            media_window_seconds: 60,

            // Write: 30 requests per 60 seconds
            write_max_requests: 30,
            write_window_seconds: 60,

            // Admin: 30 requests per 60 seconds
            admin_max_requests: 30,
            admin_window_seconds: 60,

            // Read: 100 requests per 60 seconds
            read_max_requests: 100,
            read_window_seconds: 60,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn env_map(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn test_grpc_message_size_validation() {
        let mut config = valid_prod_config();

        // Valid: within range
        config.server.grpc_max_message_size_bytes = 8 * 1024 * 1024; // 8 MB
        assert!(config.validate().is_ok());

        // Valid: minimum (1 MB)
        config.server.grpc_max_message_size_bytes = 1024 * 1024;
        assert!(config.validate().is_ok());

        // Valid: maximum (1 GB)
        config.server.grpc_max_message_size_bytes = 1024 * 1024 * 1024;
        assert!(config.validate().is_ok());

        // Invalid: below minimum
        config.server.grpc_max_message_size_bytes = 1024 * 1024 - 1; // Just under 1 MB
        let errors = config.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("grpc_max_message_size_bytes") && e.contains("1 MB")));

        // Invalid: above maximum
        config.server.grpc_max_message_size_bytes = 1024 * 1024 * 1024 + 1; // Just over 1 GB
        let errors = config.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("grpc_max_message_size_bytes") && e.contains("1 GB")));
    }

    #[test]
    fn test_validate_rejects_cors_origin_with_path() {
        let mut config = valid_prod_config();
        config.server.cors_allowed_origins = vec!["https://app.example.com/foo".to_string()];

        let errors = config
            .validate()
            .expect_err("CORS origins with paths must be rejected during config validation");

        assert!(
            errors
                .iter()
                .any(|e| e.contains("cors origin") || e.contains("CORS origin")),
            "unexpected errors: {errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("must not include a path")),
            "unexpected errors: {errors:?}"
        );
    }

    #[test]
    fn test_api_address() {
        let config = Config {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                port: 8080,
                enable_reflection: true,
                grpc_max_message_size_bytes: 16 * 1024 * 1024,
                trusted_proxies: Vec::new(),
                cors_allowed_origins: Vec::new(),
                cluster_secret: String::new(),
                advertise_host: String::new(),
                shutdown_drain_timeout_seconds: 30,
            },
            time: TimeConfig::default(),
            public_ids: PublicIdsConfig::default(),
            security: SecurityConfig {
                opaque_server_setup_secret: "test-opaque-server-setup-secret-that-is-long-enough"
                    .to_string(),
                ..SecurityConfig::default()
            },
            data_dir: default_data_dir().display().to_string(),
            metrics: MetricsConfig::default(),
            management: ManagementConfig::default(),
            database: DatabaseConfig::default(),
            redis: RedisConfig::default(),
            jwt: JwtConfig::default(),
            logging: LoggingConfig::default(),
            livestream: LivestreamConfig::default(),
            webauthn: WebAuthnConfig::default(),
            email: EmailConfig::default(),
            media_providers: MediaProvidersConfig::default(),
            webrtc: WebRTCConfig::default(),
            connection_limits: ConnectionLimitsConfig::default(),
            bootstrap: BootstrapConfig::default(),
            cluster: ClusterChannelConfig::default(),
            password_complexity: PasswordComplexityConfig::default(),
            buffer_sizes: BufferSizesConfig::default(),
            cache: CacheConfig::default(),
            proxy_slice_cache: ProxySliceCacheConfig::default(),
            messaging_rate_limits: MessagingRateLimitConfig::default(),
            http_rate_limits: HttpRateLimitConfig::default(),
            grpc_rate_limits: GrpcRateLimitConfig::default(),
        };

        assert_eq!(config.api_address(), "127.0.0.1:8080");
    }

    #[test]
    fn test_metrics_address() {
        let config = Config {
            server: ServerConfig::default(),
            time: TimeConfig::default(),
            public_ids: PublicIdsConfig::default(),
            security: SecurityConfig::default(),
            data_dir: default_data_dir().display().to_string(),
            metrics: MetricsConfig {
                enabled: true,
                host: "127.0.0.1".to_string(),
                port: 9090,
                tls: MetricsTlsConfig::default(),
                auth: MetricsAuthConfig {
                    bearer_token: "metrics-secret".to_string(),
                    ..MetricsAuthConfig::default()
                },
            },
            management: ManagementConfig::default(),
            database: DatabaseConfig::default(),
            redis: RedisConfig::default(),
            jwt: JwtConfig::default(),
            logging: LoggingConfig::default(),
            livestream: LivestreamConfig::default(),
            webauthn: WebAuthnConfig::default(),
            email: EmailConfig::default(),
            media_providers: MediaProvidersConfig::default(),
            webrtc: WebRTCConfig::default(),
            connection_limits: ConnectionLimitsConfig::default(),
            bootstrap: BootstrapConfig::default(),
            cluster: ClusterChannelConfig::default(),
            password_complexity: PasswordComplexityConfig::default(),
            buffer_sizes: BufferSizesConfig::default(),
            cache: CacheConfig::default(),
            proxy_slice_cache: ProxySliceCacheConfig::default(),
            messaging_rate_limits: MessagingRateLimitConfig::default(),
            http_rate_limits: HttpRateLimitConfig::default(),
            grpc_rate_limits: GrpcRateLimitConfig::default(),
        };

        assert_eq!(config.metrics_address(), "127.0.0.1:9090");
    }

    #[test]
    fn test_advertise_host_prefers_explicit_config_over_env() {
        let mut config = valid_prod_config();
        config.server.advertise_host = "10.1.2.3".to_string();

        assert_eq!(
            config.advertise_host_with_env_map(&env_map(&[("POD_IP", "10.0.0.99")])),
            "10.1.2.3"
        );
    }

    #[test]
    fn test_advertise_host_uses_pod_ip_before_hostname() {
        let config = valid_prod_config();

        assert_eq!(
            config.advertise_host_with_env_map(&env_map(&[("POD_IP", "10.2.3.4")])),
            "10.2.3.4"
        );
    }

    #[test]
    fn test_advertise_host_falls_back_to_hostname_without_pod_ip() {
        let config = valid_prod_config();
        let advertise_host = config.advertise_host_with_env_map(&HashMap::new());

        assert!(
            !advertise_host.is_empty(),
            "hostname fallback should produce a non-empty advertise host"
        );
        assert_ne!(advertise_host, "0.0.0.0");
    }

    #[test]
    fn test_cluster_mode_rejects_unroutable_advertise_host() {
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.server.advertise_host = "0.0.0.0".to_string();

        let errors = config
            .validate_with_env_map(&HashMap::new())
            .expect_err("cluster mode must reject unroutable advertise_host");

        assert!(
            errors.iter().any(|e| e.contains("server.advertise_host")),
            "unexpected errors: {errors:?}"
        );
    }

    #[test]
    fn test_cluster_mode_requires_explicit_routable_advertise_host_source() {
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.server.advertise_host.clear();

        let errors = config
            .validate_with_env_map(&HashMap::new())
            .expect_err("cluster mode must not fall back to implicit hostname advertise address");

        assert!(
            errors.iter().any(|e| {
                e.contains("server.advertise_host")
                    && e.contains("SYNCTV_SERVER_ADVERTISE_HOST")
                    && e.contains("POD_IP")
            }),
            "unexpected errors: {errors:?}"
        );
    }

    #[test]
    fn test_cluster_mode_accepts_pod_ip_as_explicit_advertise_host_source() {
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.server.advertise_host.clear();

        config
            .validate_with_env_map(&env_map(&[("POD_IP", "10.2.3.4")]))
            .expect("cluster mode should accept POD_IP as the explicit advertise host source");
    }

    #[test]
    fn test_from_env_rejects_invalid_numeric_override() {
        let error = Config::from_env_map(&env_map(&[("SYNCTV_SERVER_PORT", "not-a-port")]))
            .expect_err("invalid numeric override must fail closed");

        let message = error.to_string();
        assert!(message.contains("SYNCTV_SERVER_PORT"));
        assert!(message.contains("not-a-port"));
    }

    #[test]
    fn test_from_env_rejects_invalid_boolean_override() {
        let error = Config::from_env_map(&env_map(&[("SYNCTV_METRICS_ENABLED", "maybe")]))
            .expect_err("invalid boolean override must fail closed");

        let message = error.to_string();
        assert!(message.contains("SYNCTV_METRICS_ENABLED"));
        assert!(message.contains("maybe"));
    }

    #[test]
    fn test_from_env_rejects_invalid_redis_deployment_mode_override() {
        let error = Config::from_env_map(&env_map(&[("SYNCTV_REDIS_DEPLOYMENT_MODE", "sentinal")]))
            .expect_err("invalid redis deployment mode override must fail closed");

        let message = error.to_string();
        assert!(message.contains("SYNCTV_REDIS_DEPLOYMENT_MODE"));
        assert!(message.contains("sentinal"));
    }

    #[test]
    fn test_from_env_rejects_unsupported_redis_cluster_mode_override() {
        let error = Config::from_env_map(&env_map(&[("SYNCTV_REDIS_DEPLOYMENT_MODE", "cluster")]))
            .expect_err("unsupported redis cluster mode override must fail closed");

        let message = error.to_string();
        assert!(message.contains("SYNCTV_REDIS_DEPLOYMENT_MODE"));
        assert!(message.contains("cluster"));
        assert!(message.contains("standalone"));
        assert!(message.contains("sentinel"));
    }

    #[test]
    fn test_from_env_rejects_invalid_webrtc_mode_override() {
        let error = Config::from_env_map(&env_map(&[("SYNCTV_WEBRTC_MODE", "p2p")]))
            .expect_err("invalid webrtc mode override must fail closed");

        let message = error.to_string();
        assert!(message.contains("SYNCTV_WEBRTC_MODE"));
        assert!(message.contains("p2p"));
    }

    #[test]
    fn test_from_env_rejects_invalid_hls_storage_backend_override() {
        let error = Config::from_env_map(&env_map(&[(
            "SYNCTV_LIVESTREAM_HLS_STORAGE_BACKEND",
            "nfs",
        )]))
        .expect_err("invalid HLS storage backend override must fail closed");

        let message = error.to_string();
        assert!(message.contains("SYNCTV_LIVESTREAM_HLS_STORAGE_BACKEND"));
        assert!(message.contains("memory"));
        assert!(message.contains("file"));
        assert!(message.contains("oss"));
    }

    #[test]
    fn test_from_env_ignores_unknown_server_port_env_vars() {
        let config = Config::from_env_map(&env_map(&[
            ("SYNCTV_SERVER_GRPC_PORT", "50051"),
            ("SYNCTV_SERVER_HTTP_PORT", "8080"),
            ("SYNCTV_SERVER_PORT", "18080"),
        ]))
        .expect("unknown split-port env vars should be ignored with a warning");

        assert_eq!(config.server.port, 18080);
    }

    #[test]
    fn test_collect_unknown_synctv_env_vars_only_returns_unhandled_synctv_keys() {
        let env = env_map(&[
            ("SYNCTV_SERVER_PORT", "18080"),
            ("SYNCTV_UNKNOWN_FLAG", "1"),
            ("SYNCTV_ANOTHER_UNKNOWN", "2"),
            ("SYNCTV_MANAGEMENT_ENDPOINT", "unix:///tmp/synctv.sock"),
            ("PATH", "/usr/bin"),
        ]);
        let seen = std::collections::HashSet::from(["SYNCTV_SERVER_PORT".to_string()]);

        let unknown = Config::collect_unknown_synctv_env_vars(&env, &seen);

        assert_eq!(
            unknown,
            vec![
                "SYNCTV_ANOTHER_UNKNOWN".to_string(),
                "SYNCTV_UNKNOWN_FLAG".to_string()
            ]
        );
    }

    #[test]
    fn test_public_ids_default_to_prefixed_decimal_ids() {
        let config = Config::from_env_map(&HashMap::new()).expect("default config should load");
        let codec = crate::PublicIdCodec::from_config(&config.public_ids)
            .expect("default public IDs config should be valid");

        assert!(config.public_ids.sqids.is_none());
        assert_eq!(
            codec
                .encode_user_id(crate::models::UserId::from(1))
                .expect("user ID should encode"),
            "usr_1"
        );
    }

    #[test]
    fn test_public_ids_sqids_env_enables_prefixed_sqids() {
        let config = Config::from_env_map(&env_map(&[("SYNCTV_PUBLIC_IDS_SQIDS_MIN_LENGTH", "8")]))
            .expect("sqids env config should load");
        let codec = crate::PublicIdCodec::from_config(&config.public_ids)
            .expect("sqids public IDs config should be valid");
        let encoded = codec
            .encode_user_id(crate::models::UserId::from(1))
            .expect("user ID should encode");

        assert_eq!(
            config
                .public_ids
                .sqids
                .as_ref()
                .expect("sqids should be enabled")
                .min_length,
            8
        );
        assert!(encoded.starts_with("usr_"));
        assert_ne!(encoded, "usr_1");
        assert_eq!(
            codec
                .decode_user_id(&encoded)
                .expect("user ID should decode"),
            crate::models::UserId::from(1)
        );
    }

    #[test]
    fn test_checked_in_yaml_configs_deserialize() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("synctv-core should be inside the workspace root");

        for config_file in ["synctv.example.yaml"] {
            let path = workspace_root.join(config_file);
            Config::load_config_file(
                path.to_str()
                    .expect("checked-in config path should be valid UTF-8"),
            )
            .unwrap_or_else(|error| panic!("{config_file} should deserialize: {error}"));
        }
    }

    /// Helper to create a valid production config for validation tests
    fn valid_prod_config() -> Config {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 8080,
                grpc_max_message_size_bytes: 16 * 1024 * 1024,
                enable_reflection: false,
                trusted_proxies: Vec::new(),
                cors_allowed_origins: Vec::new(),
                cluster_secret: "test-cluster-secret-for-validation".to_string(),
                advertise_host: String::new(),
                shutdown_drain_timeout_seconds: 30,
            },
            time: TimeConfig::default(),
            public_ids: PublicIdsConfig::default(),
            security: SecurityConfig {
                opaque_server_setup_secret: "test-opaque-server-setup-secret-that-is-long-enough"
                    .to_string(),
                ..SecurityConfig::default()
            },
            data_dir: default_data_dir().display().to_string(),
            metrics: MetricsConfig::default(),
            management: ManagementConfig {
                auth_token: "test-management-auth-token".to_string(),
                ..ManagementConfig::default()
            },
            database: DatabaseConfig::default(),
            redis: RedisConfig {
                url: "redis://localhost:6379".to_string(),
                ..RedisConfig::default()
            },
            jwt: JwtConfig {
                secret: "my-very-secret-production-key-that-is-long-enough".to_string(),
                ..JwtConfig::default()
            },
            logging: LoggingConfig::default(),
            livestream: LivestreamConfig {
                // Keep a valid file backend so cluster-mode tests can opt in by
                // toggling `cluster.enabled` without unrelated HLS path errors.
                hls_storage_backend: HlsStorageBackend::File,
                hls_shared_storage: true,
                hls_storage_path: "/var/lib/synctv/hls".to_string(),
                ..LivestreamConfig::default()
            },
            webauthn: WebAuthnConfig::default(),
            email: EmailConfig::default(),
            media_providers: MediaProvidersConfig::default(),
            webrtc: WebRTCConfig {
                // Keep a valid external STUN address so cluster-mode tests can opt in by
                // toggling `cluster.enabled` without additional changes.
                stun_external_addr: "203.0.113.1:3478".to_string(),
                ..WebRTCConfig::default()
            },
            connection_limits: ConnectionLimitsConfig::default(),
            bootstrap: BootstrapConfig {
                create_root_user: true,
                root_username: "admin".to_string(),
                root_email: "admin@example.com".to_string(),
                root_password: "StrongPwd12345!".to_string(),
            },
            cluster: ClusterChannelConfig::default(),
            password_complexity: PasswordComplexityConfig::default(),
            buffer_sizes: BufferSizesConfig::default(),
            cache: CacheConfig::default(),
            proxy_slice_cache: ProxySliceCacheConfig::default(),
            messaging_rate_limits: MessagingRateLimitConfig::default(),
            http_rate_limits: HttpRateLimitConfig::default(),
            grpc_rate_limits: GrpcRateLimitConfig::default(),
        }
    }

    #[test]
    fn test_validate_valid_production_config() {
        let config = valid_prod_config();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_cluster_mode_allows_local_hls_storage() {
        // In cluster mode, local HLS backends are allowed because non-publisher
        // nodes proxy playlist/segment reads to the publisher node.
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.server.advertise_host = "10.0.0.12".to_string();
        config.livestream.hls_shared_storage = false;
        assert!(config.validate().is_ok());

        config.livestream.hls_storage_backend = HlsStorageBackend::Memory;
        config.livestream.hls_storage_path = String::new();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_standalone_mode_allows_hls_local_storage() {
        // In standalone mode (no cluster_secret and cluster.enabled=false),
        // hls_shared_storage=false should be allowed (only a warning is logged).
        let mut config = valid_prod_config();
        // Disable cluster mode by clearing cluster_secret and ensuring cluster.enabled is false
        config.server.cluster_secret = String::new();
        config.cluster.enabled = false;
        // Remove Redis to ensure cluster mode is fully disabled
        config.redis.url = String::new();
        // Also need to clear stun_external_addr since standalone mode no longer
        // requires an external STUN address.
        config.webrtc.stun_external_addr = String::new();
        // hls_shared_storage=false should be allowed in standalone mode
        config.livestream.hls_shared_storage = false;
        // This should pass validation (only a warning is logged)
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_single_api_port_is_allowed() {
        let mut config = valid_prod_config();
        config.server.port = 8080;
        config.validate().expect("single API port should be valid");
    }

    #[test]
    fn test_validate_port_conflict_rtmp_http() {
        let mut config = valid_prod_config();
        config.livestream.rtmp_port = 8080;
        let errors = config.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("rtmp_port") && e.contains("server.port")));
    }

    #[test]
    fn test_validate_zero_port() {
        let mut config = valid_prod_config();
        config.server.port = 0;
        let errors = config.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("server.port") && e.contains('0')));
    }

    #[test]
    fn test_validate_default_jwt_secret_production() {
        let mut config = valid_prod_config();
        config.jwt.secret = "change-me-in-production".to_string();
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("JWT secret")));
    }

    #[test]
    fn test_validate_empty_jwt_secret() {
        let mut config = valid_prod_config();
        config.jwt.secret = String::new();
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("JWT secret is empty")));
    }

    #[test]
    fn test_validate_jwt_secret_too_short() {
        let mut config = valid_prod_config();
        // 31 characters - just under the 32 minimum
        config.jwt.secret = "a".repeat(31);
        let errors = config.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("JWT secret") && e.contains("32") && e.contains("characters")));
    }

    #[test]
    fn test_validate_jwt_secret_exactly_32_chars() {
        let mut config = valid_prod_config();
        // Exactly 32 characters - should pass
        config.jwt.secret = "a".repeat(32);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_from_file_merges_partial_nested_sections_with_defaults() {
        let unique = format!(
            "synctv-config-test-{}-{}.yaml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::write(
            &path,
            r#"
server:
  port: 50051
database:
  url: "postgresql://user:pass@localhost/db"
jwt:
  secret: "12345678901234567890123456789012"
"#,
        )
        .expect("write config");

        let config =
            Config::load_with_env_map(Some(path.to_str().expect("utf-8 path")), &HashMap::new())
                .expect("partial config should merge with defaults");
        let _ = std::fs::remove_file(&path);

        assert_eq!(config.server.port, 50051);
        assert_eq!(config.jwt.secret, "12345678901234567890123456789012");
        assert_eq!(
            config.jwt.access_token_duration_hours,
            JwtConfig::default().access_token_duration_hours
        );
        assert_eq!(config.logging.level, LoggingConfig::default().level);
        assert_eq!(config.logging.filter, LoggingConfig::default().filter);
        assert_eq!(config.logging.backtrace, LoggingConfig::default().backtrace);
    }

    #[test]
    fn test_from_file_parses_explicit_local_media_provider_config() {
        let unique = format!(
            "synctv-media-providers-config-test-{}-{}.yaml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::write(
            &path,
            r"
media_providers:
  alist:
    request_timeout_seconds: 40
    connect_timeout_seconds: 8
  bilibili:
    request_timeout_seconds: 50
    connect_timeout_seconds: 9
  emby:
    request_timeout_seconds: 60
    connect_timeout_seconds: 10
",
        )
        .expect("write config");

        let config =
            Config::load_with_env_map(Some(path.to_str().expect("utf-8 path")), &HashMap::new())
                .expect("explicit local media provider config should load");
        let _ = std::fs::remove_file(&path);

        assert_eq!(config.media_providers.alist.request_timeout_seconds, 40);
        assert_eq!(config.media_providers.alist.connect_timeout_seconds, 8);
        assert_eq!(config.media_providers.bilibili.request_timeout_seconds, 50);
        assert_eq!(config.media_providers.bilibili.connect_timeout_seconds, 9);
        assert_eq!(config.media_providers.emby.request_timeout_seconds, 60);
        assert_eq!(config.media_providers.emby.connect_timeout_seconds, 10);
    }

    #[test]
    fn test_from_file_resolves_typed_secret_file_references_relative_to_config_path() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let config_dir = temp_dir.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("config dir should be created");
        let data_dir = config_dir.join("state");
        std::fs::create_dir_all(&data_dir).expect("data dir should be created");

        std::fs::write(config_dir.join("jwt.secret"), "jwt-secret-from-file\n")
            .expect("jwt secret file should be written");
        std::fs::write(
            config_dir.join("cluster.secret"),
            "cluster-secret-from-file\n",
        )
        .expect("cluster secret file should be written");
        std::fs::write(
            config_dir.join("management.token"),
            "management-token-from-file\n",
        )
        .expect("management token file should be written");
        std::fs::write(
            config_dir.join("metrics.password"),
            "metrics-basic-password\n",
        )
        .expect("metrics password file should be written");
        std::fs::write(config_dir.join("metrics.bearer"), "metrics-bearer-token\n")
            .expect("metrics bearer token file should be written");
        std::fs::write(
            config_dir.join("database.url"),
            "postgresql://synctv:secret@db.example.com:5432/synctv\n",
        )
        .expect("database url file should be written");
        std::fs::write(config_dir.join("database.password"), "database-password\n")
            .expect("database password file should be written");
        std::fs::write(
            config_dir.join("redis.url"),
            "redis://:secret@redis.example.com:6379/0\n",
        )
        .expect("redis url file should be written");
        std::fs::write(config_dir.join("redis.password"), "redis-password\n")
            .expect("redis password file should be written");
        std::fs::write(
            config_dir.join("smtp.password"),
            "smtp-password-from-file\n",
        )
        .expect("smtp password file should be written");
        std::fs::write(config_dir.join("root.password"), "StrongPwd12345!\n")
            .expect("root password file should be written");
        std::fs::write(
            config_dir.join("credential.key"),
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\n",
        )
        .expect("credential encryption key file should be written");
        std::fs::write(
            config_dir.join("opaque.secret"),
            "opaque-server-setup-secret-from-file\n",
        )
        .expect("opaque server setup secret file should be written");

        let config_path = config_dir.join("synctv.yaml");
        std::fs::write(
            &config_path,
            r#"
data_dir: "./state"
server:
  cluster_secret_file: "./cluster.secret"
management:
  transport: "unix"
  auth_token_file: "./management.token"
metrics:
  auth:
    mode: "basic"
    basic_username: "metrics"
    bearer_token_file: "./metrics.bearer"
    basic_password_file: "./metrics.password"
database:
  url_file: "./database.url"
  password_file: "./database.password"
redis:
  url_file: "./redis.url"
  password_file: "./redis.password"
jwt:
  secret_file: "./jwt.secret"
security:
  credential_encryption_key_file: "./credential.key"
  opaque_server_setup_secret_file: "./opaque.secret"
email:
  smtp_host: "smtp.example.com"
  smtp_password_file: "./smtp.password"
bootstrap:
  create_root_user: true
  root_username: "admin"
  root_email: "admin@example.com"
  root_password_file: "./root.password"
"#,
        )
        .expect("config file should be written");

        let unknown_keys =
            Config::collect_unknown_config_file_keys(config_path.to_str().expect("utf-8 path"))
                .expect("supported _file keys should not be reported as unknown");
        let config = Config::load_with_env_map(
            Some(config_path.to_str().expect("utf-8 path")),
            &HashMap::new(),
        )
        .expect("typed _file references should load");

        assert!(
            unknown_keys.is_empty(),
            "supported _file keys should not be treated as unknown: {unknown_keys:?}"
        );
        assert_eq!(config.jwt.secret, "jwt-secret-from-file");
        assert_eq!(config.server.cluster_secret, "cluster-secret-from-file");
        assert_eq!(config.management.auth_token, "management-token-from-file");
        assert_eq!(config.metrics.auth.basic_password, "metrics-basic-password");
        assert_eq!(config.metrics.auth.bearer_token, "metrics-bearer-token");
        assert_eq!(
            config.database.url,
            "postgresql://synctv:secret@db.example.com:5432/synctv"
        );
        assert_eq!(config.database.password, "database-password");
        assert_eq!(config.redis.url, "redis://:secret@redis.example.com:6379/0");
        assert_eq!(config.redis.password, "redis-password");
        assert_eq!(
            config.security.credential_encryption_key,
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
        );
        assert_eq!(
            config.security.opaque_server_setup_secret,
            "opaque-server-setup-secret-from-file"
        );
        assert_eq!(config.email.smtp_password, "smtp-password-from-file");
        assert_eq!(config.bootstrap.root_password, "StrongPwd12345!");
    }

    #[test]
    fn test_from_file_builds_database_and_redis_urls_from_split_config() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let config_dir = temp_dir.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("config dir should be created");
        std::fs::write(config_dir.join("database.password"), "pg-password\n")
            .expect("database password file should be written");
        std::fs::write(config_dir.join("redis.password"), "redis-password\n")
            .expect("redis password file should be written");

        let config_path = config_dir.join("synctv.yaml");
        std::fs::write(
            &config_path,
            r#"
database:
  host: "db.example.com"
  port: 5433
  user: "synctv"
  password_file: "./database.password"
  name: "synctv_prod"
redis:
  host: "redis.example.com"
  port: 6380
  user: "cache-user"
  password_file: "./redis.password"
  database: 7
"#,
        )
        .expect("config file should be written");

        let config = Config::load_with_env_map(
            Some(config_path.to_str().expect("utf-8 path")),
            &HashMap::new(),
        )
        .expect("split database config should load");

        assert!(config.database.url.is_empty());
        assert_eq!(config.database.username, "synctv");
        assert_eq!(
            config.database_url(),
            "postgresql://synctv:pg-password@db.example.com:5433/synctv_prod"
        );
        assert_eq!(config.redis.username, "cache-user");
        assert_eq!(
            config.redis_url(),
            "redis://cache-user:redis-password@redis.example.com:6380/7"
        );
    }

    #[test]
    fn test_data_dir_override_does_not_rebase_typed_secret_file_references() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let config_dir = temp_dir.path().join("config");
        let override_data_dir = temp_dir.path().join("override-state");
        std::fs::create_dir_all(&config_dir).expect("config dir should be created");
        std::fs::create_dir_all(&override_data_dir).expect("override data dir should be created");

        std::fs::write(
            config_dir.join("jwt.secret"),
            "jwt-secret-from-config-dir\n",
        )
        .expect("jwt secret file should be written");

        let config_path = config_dir.join("synctv.yaml");
        std::fs::write(
            &config_path,
            r#"
jwt:
  secret_file: "./jwt.secret"
"#,
        )
        .expect("config file should be written");

        let config = Config::load_with_env_map_and_data_dir_override(
            Some(config_path.to_str().expect("utf-8 path")),
            &HashMap::new(),
            Some(override_data_dir.to_str().expect("utf-8 path")),
        )
        .expect("data_dir override should not change secret file lookup");

        assert_eq!(config.jwt.secret, "jwt-secret-from-config-dir");
    }

    #[test]
    fn test_from_file_resolves_owned_local_paths_relative_to_data_dir() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let config_dir = temp_dir.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("config dir should be created");
        let config_path = config_dir.join("synctv.yaml");
        std::fs::write(
            &config_path,
            r#"
data_dir: "./state"
management:
  transport: "unix"
  unix_socket_path: "sockets/admin.sock"
metrics:
  tls:
    cert_path: "tls/metrics.crt"
    key_path: "tls/metrics.key"
proxy_slice_cache:
  enabled: false
  file_backend_enabled: true
  file_cache_dir: "proxy-cache"
logging:
  file_path: "logs/server.log"
livestream:
  hls_storage_path: "hls"
"#,
        )
        .expect("config file should be written");

        let config = Config::from_file(config_path.to_str().expect("utf-8 path"))
            .expect("config file with data_dir should load");
        let expected_data_dir = config_dir.join("state");

        assert_eq!(Path::new(&config.data_dir), expected_data_dir);
        assert_eq!(
            Path::new(&config.management.unix_socket_path),
            expected_data_dir.join("sockets").join("admin.sock")
        );
        assert_eq!(
            config.logging.file_path.as_deref().map(Path::new),
            Some(expected_data_dir.join("logs").join("server.log").as_path())
        );
        assert_eq!(
            Path::new(&config.metrics.tls.cert_path),
            config_dir.join("tls").join("metrics.crt")
        );
        assert_eq!(
            Path::new(&config.metrics.tls.key_path),
            config_dir.join("tls").join("metrics.key")
        );
        assert!(!config.proxy_slice_cache.enabled);
        assert!(config.proxy_slice_cache.file_backend_enabled);
        assert_eq!(
            Path::new(&config.proxy_slice_cache.file_cache_dir),
            expected_data_dir.join("proxy-cache")
        );
        assert_eq!(
            Path::new(&config.livestream.hls_storage_path),
            expected_data_dir.join("hls")
        );
    }

    #[test]
    fn test_collect_unknown_config_file_keys_ignores_top_level_data_dir() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let config_path = temp_dir.path().join("synctv.yaml");
        std::fs::write(
            &config_path,
            r#"
data_dir: "./state"
management:
  transport: "unix"
"#,
        )
        .expect("config file should be written");

        let unknown_keys =
            Config::collect_unknown_config_file_keys(config_path.to_str().expect("utf-8 path"))
                .expect("top-level data_dir should deserialize cleanly");

        assert!(
            unknown_keys.is_empty(),
            "top-level data_dir should not be reported as unknown: {unknown_keys:?}"
        );
    }

    #[test]
    fn test_from_env_map_resolves_relative_data_dir_from_current_dir() {
        let cwd = std::env::current_dir().expect("current dir should resolve");
        let env = HashMap::from([
            ("SYNCTV_DATA_DIR".to_string(), "var/synctv".to_string()),
            (
                "SYNCTV_MANAGEMENT_UNIX_SOCKET_PATH".to_string(),
                "ops/management.sock".to_string(),
            ),
            (
                "SYNCTV_LOGGING_FILE_PATH".to_string(),
                "logs/server.log".to_string(),
            ),
            (
                "SYNCTV_LIVESTREAM_HLS_STORAGE_PATH".to_string(),
                "livestream/hls".to_string(),
            ),
            (
                "SYNCTV_METRICS_TLS_CERT_PATH".to_string(),
                "tls/metrics.crt".to_string(),
            ),
            (
                "SYNCTV_METRICS_TLS_KEY_PATH".to_string(),
                "tls/metrics.key".to_string(),
            ),
        ]);

        let config = Config::from_env_map(&env).expect("env-backed config should load");
        let expected_data_dir = cwd.join("var").join("synctv");

        assert_eq!(Path::new(&config.data_dir), expected_data_dir);
        assert_eq!(
            Path::new(&config.management.unix_socket_path),
            expected_data_dir.join("ops").join("management.sock")
        );
        assert_eq!(
            config.logging.file_path.as_deref().map(Path::new),
            Some(expected_data_dir.join("logs").join("server.log").as_path())
        );
        assert_eq!(
            Path::new(&config.metrics.tls.cert_path),
            cwd.join("tls").join("metrics.crt")
        );
        assert_eq!(
            Path::new(&config.metrics.tls.key_path),
            cwd.join("tls").join("metrics.key")
        );
        assert_eq!(
            Path::new(&config.livestream.hls_storage_path),
            expected_data_dir.join("livestream").join("hls")
        );
    }

    #[test]
    fn test_from_env_map_resolves_proxy_slice_cache_dir_relative_to_data_dir() {
        let cwd = std::env::current_dir().expect("current dir should resolve");
        let env = HashMap::from([
            ("SYNCTV_DATA_DIR".to_string(), "var/synctv".to_string()),
            (
                "SYNCTV_PROXY_SLICE_CACHE_FILE_BACKEND_ENABLED".to_string(),
                "true".to_string(),
            ),
            (
                "SYNCTV_PROXY_SLICE_CACHE_ENABLED".to_string(),
                "false".to_string(),
            ),
        ]);

        let config = Config::from_env_map(&env).expect("env-backed config should load");
        let expected_data_dir = cwd.join("var").join("synctv");

        assert!(!config.proxy_slice_cache.enabled);
        assert!(config.proxy_slice_cache.file_backend_enabled);
        assert_eq!(
            Path::new(&config.proxy_slice_cache.file_cache_dir),
            expected_data_dir.join("cache").join("proxy-slice")
        );
    }

    #[test]
    fn test_from_file_rejects_missing_secret_file_reference() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let config_path = temp_dir.path().join("synctv.yaml");
        std::fs::write(
            &config_path,
            r#"
jwt:
  secret_file: "./missing.secret"
"#,
        )
        .expect("config file should be written");

        let error = Config::from_file(config_path.to_str().expect("utf-8 path"))
            .expect_err("missing _file target must fail closed");

        assert!(
            error.to_string().contains("jwt.secret_file"),
            "missing file error should mention the failing _file key: {error}"
        );
    }

    #[test]
    fn test_from_file_rejects_missing_path() {
        let unique = format!(
            "synctv-missing-config-{}-{}.yaml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);

        let error = Config::from_file(path.to_str().expect("utf-8 path"))
            .expect_err("missing file must not fall back to defaults");

        assert!(
            error.to_string().contains("not found"),
            "missing file error should mention not found: {error}"
        );
    }

    #[test]
    fn test_from_file_rejects_unknown_server_port_keys() {
        let unique = format!(
            "synctv-unknown-port-config-{}-{}.yaml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::write(
            &path,
            r#"
server:
  host: "0.0.0.0"
  grpc_port: 50051
  http_port: 8080
jwt:
  secret: "12345678901234567890123456789012"
"#,
        )
        .expect("write config");

        let unknown_keys =
            Config::collect_unknown_config_file_keys(path.to_str().expect("utf-8 path"))
                .expect("unknown split-port keys should be collected");
        let config = Config::from_file(path.to_str().expect("utf-8 path"))
            .expect("unknown split-port file keys should warn and fall back to defaults");
        let _ = std::fs::remove_file(&path);

        assert!(
            unknown_keys.contains(&"server.grpc_port".to_string()),
            "server.grpc_port should be reported as unknown: {unknown_keys:?}"
        );
        assert!(
            unknown_keys.contains(&"server.http_port".to_string()),
            "server.http_port should be reported as unknown: {unknown_keys:?}"
        );
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, ServerConfig::default().port);
    }

    #[test]
    fn test_validate_default_root_password() {
        let mut config = valid_prod_config();
        config.bootstrap.root_password = "root".to_string();
        let errors = config.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("Root password") && e.contains("default")));
    }

    #[test]
    fn test_validate_empty_root_password_reports_single_root_cause() {
        let mut config = valid_prod_config();
        config.bootstrap.root_password.clear();
        let errors = config.validate().unwrap_err();

        assert!(
            errors.iter().any(|e| e.contains("Root password is empty")),
            "empty password should still report the required-value error: {errors:?}"
        );
        assert!(
            !errors.iter().any(|e| e.contains("12 characters")),
            "empty password should not duplicate into length errors: {errors:?}"
        );
        assert!(
            !errors.iter().any(|e| e.contains("uppercase")),
            "empty password should not duplicate into uppercase errors: {errors:?}"
        );
        assert!(
            !errors.iter().any(|e| e.contains("lowercase")),
            "empty password should not duplicate into lowercase errors: {errors:?}"
        );
        assert!(
            !errors.iter().any(|e| e.contains("digit")),
            "empty password should not duplicate into digit errors: {errors:?}"
        );
    }

    #[test]
    fn test_validate_root_email_must_be_valid_when_provided() {
        let mut config = valid_prod_config();
        config.bootstrap.root_email = "not-an-email".to_string();

        let errors = config.validate().unwrap_err();

        assert!(
            errors
                .iter()
                .any(|e| e.contains("Root email must be a valid email address")),
            "invalid bootstrap email should be rejected: {errors:?}"
        );
    }

    #[test]
    fn test_validate_root_password_too_short() {
        let mut config = valid_prod_config();
        config.bootstrap.root_password = "Short1aA".to_string(); // 8 chars, < 12
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("12 characters")));
    }

    #[test]
    fn test_validate_root_password_no_uppercase() {
        let mut config = valid_prod_config();
        config.bootstrap.root_password = "allowercase123".to_string();
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("uppercase")));
    }

    #[test]
    fn test_validate_root_password_no_lowercase() {
        let mut config = valid_prod_config();
        config.bootstrap.root_password = "ALLUPPERCASE123".to_string();
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("lowercase")));
    }

    #[test]
    fn test_validate_root_password_no_digit() {
        let mut config = valid_prod_config();
        config.bootstrap.root_password = "NoDigitsHereABC".to_string();
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("digit")));
    }

    #[test]
    fn test_validate_root_username_too_short() {
        let mut config = valid_prod_config();
        config.bootstrap.root_username = "ab".to_string();
        let errors = config.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("Root username") && e.contains('3')));
    }

    #[test]
    fn test_validate_db_pool_min_exceeds_max() {
        let mut config = valid_prod_config();
        config.database.min_connections = 30;
        config.database.max_connections = 10;
        let errors = config.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("min_connections") && e.contains("max_connections")));
    }

    #[test]
    fn test_validate_db_pool_max_zero() {
        let mut config = valid_prod_config();
        config.database.max_connections = 0;
        let errors = config.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("max_connections") && e.contains("greater than 0")));
    }

    #[test]
    fn test_validate_shutdown_drain_timeout_zero() {
        let mut config = valid_prod_config();
        config.server.shutdown_drain_timeout_seconds = 0;

        let errors = config.validate().unwrap_err();

        assert!(errors.iter().any(|e| {
            e.contains("shutdown_drain_timeout_seconds") && e.contains("greater than 0")
        }));
    }

    #[test]
    fn test_validate_database_timeouts_zero() {
        let mut config = valid_prod_config();
        config.database.connect_timeout_seconds = 0;
        config.database.idle_timeout_seconds = 0;
        config.database.max_lifetime_seconds = 0;

        let errors = config.validate().unwrap_err();

        assert!(errors
            .iter()
            .any(|e| e.contains("database.connect_timeout_seconds")));
        assert!(errors
            .iter()
            .any(|e| e.contains("database.idle_timeout_seconds")));
        assert!(errors
            .iter()
            .any(|e| e.contains("database.max_lifetime_seconds")));
    }

    #[test]
    fn test_validate_connection_limits_zero() {
        let mut config = valid_prod_config();
        config.connection_limits.max_per_user = 0;
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("max_per_user")));

        let mut config = valid_prod_config();
        config.connection_limits.max_per_room = 0;
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("max_per_room")));

        let mut config = valid_prod_config();
        config.connection_limits.max_total = 0;
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("max_total")));
    }

    #[test]
    fn test_validate_messaging_rate_limits_zero() {
        let mut config = valid_prod_config();

        config.messaging_rate_limits.chat_per_second = 0;
        let errors = config
            .validate()
            .expect_err("chat rate limit must be validated");
        assert!(errors
            .iter()
            .any(|e| e.contains("messaging_rate_limits.chat_per_second")));

        config.messaging_rate_limits.chat_per_second = 1;
        config.messaging_rate_limits.danmaku_per_second = 0;
        let errors = config
            .validate()
            .expect_err("danmaku rate limit must be validated");
        assert!(errors
            .iter()
            .any(|e| e.contains("messaging_rate_limits.danmaku_per_second")));

        config.messaging_rate_limits.danmaku_per_second = 1;
        config.messaging_rate_limits.window_seconds = 0;
        let errors = config.validate().expect_err("window must be validated");
        assert!(errors
            .iter()
            .any(|e| e.contains("messaging_rate_limits.window_seconds")));
    }

    #[test]
    fn test_from_env_overrides_messaging_rate_limits() {
        let config = Config::from_env_map(&env_map(&[
            ("SYNCTV_MESSAGING_RATE_LIMITS_CHAT_PER_SECOND", "17"),
            ("SYNCTV_MESSAGING_RATE_LIMITS_DANMAKU_PER_SECOND", "9"),
            ("SYNCTV_MESSAGING_RATE_LIMITS_WINDOW_SECONDS", "4"),
        ]))
        .expect("messaging rate env overrides should parse");

        assert_eq!(config.messaging_rate_limits.chat_per_second, 17);
        assert_eq!(config.messaging_rate_limits.danmaku_per_second, 9);
        assert_eq!(config.messaging_rate_limits.window_seconds, 4);
    }

    #[test]
    fn test_validate_database_url_is_mutually_exclusive_with_split_database_fields() {
        let mut config = valid_prod_config();
        config.database.url = "postgresql://user:pass@db.example.com:5432/synctv".to_string();
        config.database.host = "db.example.com".to_string();

        let errors = config
            .validate()
            .expect_err("database URL and split fields must be exclusive");
        assert!(errors
            .iter()
            .any(|e| e.contains("database.url is mutually exclusive")));
    }

    #[test]
    fn test_validate_redis_url_is_mutually_exclusive_with_split_redis_fields() {
        let mut config = valid_prod_config();
        config.redis.url = "redis://:secret@redis.example.com:6379/0".to_string();
        config.redis.host = "redis.example.com".to_string();

        let errors = config
            .validate()
            .expect_err("redis URL and split fields must be exclusive");
        assert!(errors
            .iter()
            .any(|e| e.contains("redis.url is mutually exclusive")));
    }

    #[test]
    fn test_validate_split_database_config_requires_all_fields() {
        let mut config = valid_prod_config();
        config.database.url.clear();
        config.database.host = "db.example.com".to_string();
        config.database.port = 5432;
        config.database.username = "synctv".to_string();
        config.database.password.clear();
        config.database.name.clear();

        let errors = config
            .validate()
            .expect_err("incomplete split database config must fail");
        assert!(errors
            .iter()
            .any(|e| e.contains("database.password must be set")));
        assert!(errors
            .iter()
            .any(|e| e.contains("database.name must be set")));
    }

    #[test]
    fn test_validate_split_redis_config_requires_host_and_port() {
        let mut config = valid_prod_config();
        config.redis.url.clear();
        config.redis.host = "redis.example.com".to_string();
        config.redis.port = 0;

        let errors = config
            .validate()
            .expect_err("incomplete split redis config must fail");
        assert!(errors
            .iter()
            .any(|e| e.contains("redis.port must be greater than 0")));
    }

    #[test]
    fn test_from_env_overrides_logging_filter_and_backtrace() {
        let config = Config::from_env_map(&env_map(&[
            ("SYNCTV_LOGGING_FILTER", "info,synctv=debug"),
            ("SYNCTV_LOGGING_BACKTRACE", "true"),
        ]))
        .expect("logging env overrides should parse");

        assert_eq!(config.logging.filter.as_deref(), Some("info,synctv=debug"));
        assert!(config.logging.backtrace);
    }

    #[test]
    fn test_from_env_resolves_timezone_from_synctv_env() {
        let config = Config::from_env_map(&env_map(&[("SYNCTV_TIME_TIMEZONE", "Asia/Shanghai")]))
            .expect("SYNCTV_TIME_TIMEZONE should resolve");

        assert_eq!(config.time.timezone, "Asia/Shanghai");
    }

    #[test]
    fn test_from_env_resolves_timezone_from_tz_fallback() {
        let config = Config::from_env_map(&env_map(&[("TZ", "America/New_York")]))
            .expect("TZ fallback should resolve");

        assert_eq!(config.time.timezone, "America/New_York");
    }

    #[test]
    fn test_management_tcp_endpoint_is_always_loopback() {
        let mut config = Config::default();
        config.management.transport = ManagementTransport::Tcp;
        config.management.port = 50099;

        assert_eq!(config.management_endpoint(), "http://127.0.0.1:50099");
        assert_eq!(config.management_bind_target(), "127.0.0.1:50099");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_default_management_unix_socket_path_uses_home_hidden_runtime_dir_on_macos() {
        let socket_path = default_management_unix_socket_path();
        let home = user_home_dir().expect("macOS test environment should expose HOME");
        assert_eq!(
            socket_path,
            home.join(".synctv").join("run").join("synctv.sock")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_default_data_dir_uses_home_hidden_dir_on_macos() {
        let data_dir = default_data_dir();
        let home = user_home_dir().expect("macOS test environment should expose HOME");

        assert_eq!(data_dir, home.join(".synctv"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_default_config_search_paths_use_home_hidden_config_dir_on_macos() {
        let home = user_home_dir().expect("macOS test environment should expose HOME");
        let expected = [
            home.join(".synctv").join("synctv.yaml"),
            home.join(".synctv").join("synctv.yml"),
            home.join(".synctv").join("synctv.json"),
            home.join(".synctv").join("synctv.toml"),
        ];
        let paths = default_config_search_paths();
        assert!(
            expected.iter().all(|path| paths.contains(path)),
            "macOS default config search paths should include ~/.synctv variants, got: {paths:?}"
        );
    }

    #[test]
    fn test_validate_management_tcp_requires_auth_token() {
        let mut config = valid_prod_config();
        config.management.transport = ManagementTransport::Tcp;
        config.management.auth_token.clear();

        let errors = config
            .validate()
            .expect_err("management tcp transport must reject missing auth token");

        assert!(errors
            .iter()
            .any(|error| error.contains("management.auth_token")));
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_management_unix_allows_empty_auth_token() {
        let mut config = valid_prod_config();
        config.management.transport = ManagementTransport::Unix;
        config.management.auth_token.clear();

        assert!(
            config.validate().is_ok(),
            "unix management transport may rely on owner-only socket permissions without a bearer token"
        );
    }

    #[test]
    fn test_from_env_overrides_management_auth_token() {
        let config =
            Config::from_env_map(&env_map(&[("SYNCTV_MANAGEMENT_AUTH_TOKEN", "mgmt-secret")]))
                .expect("management auth token env override should parse");

        assert_eq!(config.management.auth_token, "mgmt-secret");
    }

    #[test]
    fn test_from_env_loads_management_auth_token_file() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let token_path = temp_dir.path().join("management.token");
        std::fs::write(&token_path, "mgmt-file-secret\n")
            .expect("management token file should be written");
        let config = Config::from_env_map(&env_map(&[(
            "SYNCTV_MANAGEMENT_AUTH_TOKEN_FILE",
            token_path.to_str().expect("token path should be utf-8"),
        )]))
        .expect("management auth token file env override should parse");

        assert_eq!(config.management.auth_token, "mgmt-file-secret");
    }

    #[test]
    fn test_validate_webauthn_requires_rp_id_and_origin_when_enabled() {
        let mut config = valid_prod_config();
        config.webauthn.enabled = true;
        config.webauthn.rp_id.clear();
        config.webauthn.rp_origin.clear();

        let errors = config
            .validate()
            .expect_err("enabled WebAuthn must require relying-party identity");

        assert!(errors.iter().any(|error| error.contains("webauthn.rp_id")));
        assert!(errors
            .iter()
            .any(|error| error.contains("webauthn.rp_origin")));
    }

    #[test]
    fn test_validate_webauthn_rejects_origin_with_path_query_or_fragment() {
        let mut config = valid_prod_config();
        config.webauthn.enabled = true;
        config.webauthn.rp_id = "app.example.com".to_string();
        config.webauthn.rp_origin = "https://app.example.com/login?next=/#section".to_string();

        let errors = config
            .validate()
            .expect_err("WebAuthn origins must be bare origins");

        assert!(errors
            .iter()
            .any(|error| error.contains("without path, query, or fragment")));
    }

    #[test]
    fn test_validate_webauthn_accepts_minimal_valid_config() {
        let mut config = valid_prod_config();
        config.webauthn.enabled = true;
        config.webauthn.rp_id = "app.example.com".to_string();
        config.webauthn.rp_origin = "https://app.example.com".to_string();
        config.webauthn.rp_name = "SyncTV".to_string();

        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_webauthn_requires_redis_in_cluster_mode() {
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.server.cluster_secret = "cluster-secret-long-enough".to_string();
        config.server.advertise_host = "10.0.0.12".to_string();
        config.redis.url.clear();
        config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();
        config.webauthn.enabled = true;
        config.webauthn.rp_id = "app.example.com".to_string();
        config.webauthn.rp_origin = "https://app.example.com".to_string();

        let errors = config
            .validate()
            .expect_err("clustered WebAuthn must use shared challenge storage");

        assert!(errors.iter().any(|error| {
            error.contains("WebAuthn/passkey requires Redis for challenge state in cluster mode")
        }));
    }

    #[test]
    fn test_from_env_loads_top_level_secret_file_overrides() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let write_secret = |name: &str, value: &str| -> std::path::PathBuf {
            let path = temp_dir.path().join(name);
            std::fs::write(&path, format!("{value}\n")).expect("secret file should be written");
            path
        };
        let jwt_secret = write_secret("jwt.secret", "jwt-secret-from-env-file");
        let cluster_secret = write_secret("cluster.secret", "cluster-secret-from-env-file");
        let credential_key = write_secret(
            "credential.key",
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        );
        let opaque_secret =
            write_secret("opaque.secret", "opaque-server-setup-secret-from-env-file");
        let metrics_bearer = write_secret("metrics.bearer", "metrics-bearer-from-env-file");
        let metrics_password = write_secret("metrics.password", "metrics-password-from-env-file");
        let database_url = write_secret(
            "database.url",
            "postgresql://synctv:secret@db.example.com:5432/synctv",
        );
        let database_password =
            write_secret("database.password", "database-password-from-env-file");
        let redis_url = write_secret("redis.url", "redis://:secret@redis.example.com:6379/0");
        let redis_password = write_secret("redis.password", "redis-password-from-env-file");
        let smtp_password = write_secret("smtp.password", "smtp-password-from-env-file");
        let root_password = write_secret("root.password", "RootPassword12345");

        let config = Config::from_env_map(&env_map(&[
            (
                "SYNCTV_JWT_SECRET_FILE",
                jwt_secret.to_str().expect("utf-8 path"),
            ),
            (
                "SYNCTV_SERVER_CLUSTER_SECRET_FILE",
                cluster_secret.to_str().expect("utf-8 path"),
            ),
            (
                "SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY_FILE",
                credential_key.to_str().expect("utf-8 path"),
            ),
            (
                "SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET_FILE",
                opaque_secret.to_str().expect("utf-8 path"),
            ),
            (
                "SYNCTV_METRICS_AUTH_BEARER_TOKEN_FILE",
                metrics_bearer.to_str().expect("utf-8 path"),
            ),
            (
                "SYNCTV_METRICS_AUTH_BASIC_PASSWORD_FILE",
                metrics_password.to_str().expect("utf-8 path"),
            ),
            (
                "SYNCTV_DATABASE_URL_FILE",
                database_url.to_str().expect("utf-8 path"),
            ),
            (
                "SYNCTV_DATABASE_PASSWORD_FILE",
                database_password.to_str().expect("utf-8 path"),
            ),
            (
                "SYNCTV_REDIS_URL_FILE",
                redis_url.to_str().expect("utf-8 path"),
            ),
            (
                "SYNCTV_REDIS_PASSWORD_FILE",
                redis_password.to_str().expect("utf-8 path"),
            ),
            (
                "SYNCTV_EMAIL_SMTP_PASSWORD_FILE",
                smtp_password.to_str().expect("utf-8 path"),
            ),
            (
                "SYNCTV_BOOTSTRAP_ROOT_PASSWORD_FILE",
                root_password.to_str().expect("utf-8 path"),
            ),
        ]))
        .expect("secret file env overrides should parse");

        assert_eq!(config.jwt.secret, "jwt-secret-from-env-file");
        assert_eq!(config.server.cluster_secret, "cluster-secret-from-env-file");
        assert_eq!(
            config.security.credential_encryption_key,
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
        );
        assert_eq!(
            config.security.opaque_server_setup_secret,
            "opaque-server-setup-secret-from-env-file"
        );
        assert_eq!(
            config.metrics.auth.bearer_token,
            "metrics-bearer-from-env-file"
        );
        assert_eq!(
            config.metrics.auth.basic_password,
            "metrics-password-from-env-file"
        );
        assert_eq!(
            config.database.url,
            "postgresql://synctv:secret@db.example.com:5432/synctv"
        );
        assert_eq!(config.database.password, "database-password-from-env-file");
        assert_eq!(config.redis.url, "redis://:secret@redis.example.com:6379/0");
        assert_eq!(config.redis.password, "redis-password-from-env-file");
        assert_eq!(config.email.smtp_password, "smtp-password-from-env-file");
        assert_eq!(config.bootstrap.root_password, "RootPassword12345");
    }

    #[test]
    fn test_from_env_overrides_cluster_stream_max_length() {
        let config =
            Config::from_env_map(&env_map(&[("SYNCTV_CLUSTER_STREAM_MAX_LENGTH", "25000")]))
                .expect("cluster stream max length env override should parse");

        assert_eq!(config.cluster.stream_max_length, 25_000);
    }

    #[test]
    fn test_from_env_builds_database_and_redis_urls_from_split_config() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let database_password = temp_dir.path().join("database.password");
        let redis_password = temp_dir.path().join("redis.password");
        std::fs::write(&database_password, "pg-password\n")
            .expect("database password file should be written");
        std::fs::write(&redis_password, "redis-password\n")
            .expect("redis password file should be written");

        let config = Config::from_env_map(&env_map(&[
            ("SYNCTV_DATABASE_HOST", "db.example.com"),
            ("SYNCTV_DATABASE_PORT", "5433"),
            ("SYNCTV_DATABASE_USERNAME", "synctv"),
            (
                "SYNCTV_DATABASE_PASSWORD_FILE",
                database_password.to_str().expect("utf-8 path"),
            ),
            ("SYNCTV_DATABASE_NAME", "synctv_prod"),
            ("SYNCTV_REDIS_HOST", "redis.example.com"),
            ("SYNCTV_REDIS_PORT", "6380"),
            ("SYNCTV_REDIS_USERNAME", "cache-user"),
            (
                "SYNCTV_REDIS_PASSWORD_FILE",
                redis_password.to_str().expect("utf-8 path"),
            ),
            ("SYNCTV_REDIS_DATABASE", "7"),
        ]))
        .expect("split database env config should parse");

        assert!(config.database.url.is_empty());
        assert_eq!(config.database.username, "synctv");
        assert_eq!(
            config.database_url(),
            "postgresql://synctv:pg-password@db.example.com:5433/synctv_prod"
        );
        assert_eq!(config.redis.username, "cache-user");
        assert_eq!(
            config.redis_url(),
            "redis://cache-user:redis-password@redis.example.com:6380/7"
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn test_validate_rejects_unix_management_transport_on_unsupported_platform() {
        let mut config = Config::default();
        config.jwt.secret = "12345678901234567890123456789012".to_string();
        config.management.transport = ManagementTransport::Unix;
        config.management.unix_socket_path = "C:/synctv/synctv.sock".to_string();

        let errors = config
            .validate()
            .expect_err("unix management transport must be rejected on unsupported platforms");

        assert!(errors.iter().any(|error| {
            error.contains("management.transport=unix")
                && error.contains("only supported on unix-like platforms")
        }));
    }

    #[test]
    fn test_from_file_supports_yaml_yml_json_and_toml() {
        let secret = "12345678901234567890123456789012";
        let fixtures = [
            (
                "yaml",
                format!("jwt:\n  secret: \"{secret}\"\nserver:\n  port: 50051\n"),
            ),
            (
                "yml",
                format!("jwt:\n  secret: \"{secret}\"\nserver:\n  port: 50052\n"),
            ),
            (
                "json",
                format!(r#"{{"jwt":{{"secret":"{secret}"}},"server":{{"port":50053}}}}"#),
            ),
            (
                "toml",
                format!("server.port = 50054\njwt.secret = \"{secret}\"\n"),
            ),
        ];

        for (extension, contents) in fixtures {
            let unique = format!(
                "synctv-config-format-{}-{}-{}.{}",
                extension,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system time before unix epoch")
                    .as_nanos(),
                extension
            );
            let path = std::env::temp_dir().join(unique);
            std::fs::write(&path, contents).expect("write config fixture");

            let config = Config::load_with_env_map(
                Some(path.to_str().expect("utf-8 path")),
                &HashMap::new(),
            )
            .expect("supported config format should load");
            let _ = std::fs::remove_file(&path);

            assert_eq!(config.jwt.secret, secret);
            assert!(
                (50051..=50054).contains(&config.server.port),
                "unexpected port for extension {extension}: {}",
                config.server.port
            );
        }
    }

    #[test]
    fn test_from_file_ignores_unknown_keys_for_json_and_toml() {
        let secret = "12345678901234567890123456789012";
        let fixtures = [
            (
                "json",
                format!(
                    r#"{{"jwt":{{"secret":"{secret}"}},"metrics":{{"enabled":true,"obsolete_token":"ignored"}}}}"#
                ),
                "metrics.obsolete_token",
            ),
            (
                "toml",
                format!(
                    "jwt.secret = \"{secret}\"\n[metrics]\nenabled = true\nobsolete_token = \"ignored\"\n"
                ),
                "metrics.obsolete_token",
            ),
        ];

        for (extension, contents, unknown_key) in fixtures {
            let unique = format!(
                "synctv-config-unknown-{}-{}-{}.{}",
                extension,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system time before unix epoch")
                    .as_nanos(),
                extension
            );
            let path = std::env::temp_dir().join(unique);
            std::fs::write(&path, contents).expect("write config fixture");

            let unknown_keys =
                Config::collect_unknown_config_file_keys(path.to_str().expect("utf-8 path"))
                    .expect("unknown keys should be collected");
            let config = Config::from_file(path.to_str().expect("utf-8 path"))
                .expect("unknown config keys should warn and continue");
            let _ = std::fs::remove_file(&path);

            assert!(
                unknown_keys.contains(&unknown_key.to_string()),
                "missing unknown key {unknown_key} for {extension}: {unknown_keys:?}"
            );
            assert!(
                config.metrics.enabled,
                "known keys should still deserialize"
            );
            assert!(
                config.metrics.auth.bearer_token.is_empty(),
                "unknown key should not affect nested auth config for {extension}"
            );
        }
    }

    #[test]
    fn test_from_file_rejects_unsupported_extension() {
        let unique = format!(
            "synctv-config-unsupported-{}-{}.ini",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::write(
            &path,
            "[jwt]\nsecret = \"12345678901234567890123456789012\"\n",
        )
        .expect("write config");

        let error = Config::from_file(path.to_str().expect("utf-8 path"))
            .expect_err("unsupported extension must fail");
        let _ = std::fs::remove_file(&path);

        assert!(
            error
                .to_string()
                .contains("unsupported config file extension"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_validate_rejects_invalid_logging_filter() {
        let mut config = Config::default();
        config.jwt.secret = "12345678901234567890123456789012".to_string();
        config.logging.filter = Some("not a valid filter ==".to_string());

        let errors = config
            .validate()
            .expect_err("invalid logging.filter must fail validation");

        assert!(errors.iter().any(|error| error.contains("logging.filter")));
    }

    #[test]
    fn test_from_env_overrides_livestream_extended_runtime_limits() {
        let config = Config::from_env_map(&env_map(&[
            ("SYNCTV_LIVESTREAM_HLS_MEMORY_MAX_MB", "768"),
            ("SYNCTV_LIVESTREAM_HLS_STORAGE_BACKEND", "oss"),
            (
                "SYNCTV_LIVESTREAM_HLS_OSS_ENDPOINT",
                "https://s3.example.com",
            ),
            ("SYNCTV_LIVESTREAM_HLS_OSS_BUCKET", "synctv-hls"),
            ("SYNCTV_LIVESTREAM_HLS_OSS_REGION", "auto"),
            ("SYNCTV_LIVESTREAM_HLS_OSS_BASE_PATH", "/synctv/hls"),
            ("SYNCTV_LIVESTREAM_HLS_OSS_ACCESS_KEY_ID", "access-key"),
            ("SYNCTV_LIVESTREAM_HLS_OSS_SECRET_ACCESS_KEY", "secret-key"),
            (
                "SYNCTV_LIVESTREAM_FLV_MAX_CONNECTION_DURATION_SECONDS",
                "7200",
            ),
            ("SYNCTV_LIVESTREAM_FLV_WRITE_TIMEOUT_SECONDS", "45"),
            ("SYNCTV_LIVESTREAM_PUBLIC_RTMP_HOST", "stream.example.com"),
        ]))
        .expect("livestream env overrides should parse");

        assert_eq!(config.livestream.hls_memory_max_mb, 768);
        assert_eq!(
            config.livestream.hls_storage_backend,
            HlsStorageBackend::Oss
        );
        assert_eq!(config.livestream.hls_oss.endpoint, "https://s3.example.com");
        assert_eq!(config.livestream.hls_oss.bucket, "synctv-hls");
        assert_eq!(config.livestream.hls_oss.region.as_deref(), Some("auto"));
        assert_eq!(config.livestream.hls_oss.base_path, "synctv/hls/");
        assert_eq!(config.livestream.hls_oss.access_key_id, "access-key");
        assert_eq!(config.livestream.hls_oss.secret_access_key, "secret-key");
        assert_eq!(config.livestream.flv_max_connection_duration_seconds, 7200);
        assert_eq!(config.livestream.flv_write_timeout_seconds, 45);
        assert_eq!(config.livestream.public_rtmp_host, "stream.example.com");
    }

    #[test]
    fn test_from_env_overrides_local_media_provider_timeouts() {
        let config = Config::from_env_map(&env_map(&[
            ("SYNCTV_MEDIA_PROVIDERS_ALIST_REQUEST_TIMEOUT_SECONDS", "40"),
            ("SYNCTV_MEDIA_PROVIDERS_ALIST_CONNECT_TIMEOUT_SECONDS", "8"),
            (
                "SYNCTV_MEDIA_PROVIDERS_BILIBILI_REQUEST_TIMEOUT_SECONDS",
                "50",
            ),
            (
                "SYNCTV_MEDIA_PROVIDERS_BILIBILI_CONNECT_TIMEOUT_SECONDS",
                "9",
            ),
            ("SYNCTV_MEDIA_PROVIDERS_EMBY_REQUEST_TIMEOUT_SECONDS", "60"),
            ("SYNCTV_MEDIA_PROVIDERS_EMBY_CONNECT_TIMEOUT_SECONDS", "10"),
        ]))
        .expect("local media provider env overrides should parse");

        assert_eq!(config.media_providers.alist.request_timeout_seconds, 40);
        assert_eq!(config.media_providers.alist.connect_timeout_seconds, 8);
        assert_eq!(config.media_providers.bilibili.request_timeout_seconds, 50);
        assert_eq!(config.media_providers.bilibili.connect_timeout_seconds, 9);
        assert_eq!(config.media_providers.emby.request_timeout_seconds, 60);
        assert_eq!(config.media_providers.emby.connect_timeout_seconds, 10);
    }

    #[test]
    fn test_validate_local_media_provider_timeouts() {
        let mut config = valid_prod_config();
        config.media_providers.alist.request_timeout_seconds = 0;
        config.media_providers.bilibili.connect_timeout_seconds = 31;
        config.media_providers.bilibili.request_timeout_seconds = 30;
        config.media_providers.emby.request_timeout_seconds = 301;

        let errors = config
            .validate()
            .expect_err("invalid local provider timeout config must fail validation");

        assert!(errors
            .iter()
            .any(|error| error.contains("media_providers.alist.request_timeout_seconds")));
        assert!(errors
            .iter()
            .any(|error| error.contains("media_providers.bilibili.connect_timeout_seconds")));
        assert!(errors
            .iter()
            .any(|error| error.contains("media_providers.emby.request_timeout_seconds")));
    }

    #[test]
    fn test_public_rtmp_host_prefers_explicit_override() {
        let mut config = Config::default();
        config.server.advertise_host = "10.0.0.12".to_string();
        config.livestream.public_rtmp_host = "stream.example.com".to_string();

        assert_eq!(config.public_rtmp_host(), "stream.example.com");
    }

    #[test]
    fn test_public_rtmp_host_prefers_explicit_advertise_host_when_no_public_override() {
        let mut config = Config::default();
        config.server.host = "0.0.0.0".to_string();
        config.server.advertise_host = "10.0.0.12".to_string();

        assert_eq!(config.public_rtmp_host(), "10.0.0.12");
    }

    #[test]
    fn test_public_rtmp_host_falls_back_to_local_loopback_for_wildcard_bind() {
        let mut config = Config::default();
        config.server.host = "0.0.0.0".to_string();
        config.server.advertise_host.clear();
        config.livestream.public_rtmp_host.clear();

        assert_eq!(config.public_rtmp_host(), "127.0.0.1");
    }

    #[test]
    fn test_public_rtmp_host_falls_back_to_bound_host_when_specific() {
        let mut config = Config::default();
        config.server.host = "192.168.10.15".to_string();
        config.server.advertise_host.clear();
        config.livestream.public_rtmp_host.clear();

        assert_eq!(config.public_rtmp_host(), "192.168.10.15");
    }

    #[test]
    fn test_public_rtmp_host_formats_ipv6_bind_host_for_urls() {
        let mut config = Config::default();
        config.server.host = "::".to_string();
        config.server.advertise_host.clear();
        config.livestream.public_rtmp_host.clear();

        assert_eq!(config.public_rtmp_host(), "[::1]");
    }

    #[test]
    fn test_validate_email_config_partial() {
        let mut config = valid_prod_config();
        config.email.smtp_host = "smtp.example.com".to_string();
        config.email.smtp_port = 0;
        config.email.from_email = String::new();
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("smtp_port")));
        assert!(errors.iter().any(|e| e.contains("from_email")));
    }

    #[test]
    fn test_validate_email_invalid_from_email() {
        let mut config = valid_prod_config();
        config.email.smtp_host = "smtp.example.com".to_string();
        config.email.from_email = "@invalid".to_string();
        let errors = config.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("from_email") && e.contains("not a valid")));
    }

    #[test]
    fn test_validate_livestream_zero_timeout() {
        let mut config = valid_prod_config();
        config.livestream.stream_timeout_seconds = 0;
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("stream_timeout_seconds")));
    }

    #[test]
    fn test_validate_webrtc_p2p_mode_allowed_in_cluster() {
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.server.cluster_secret = "shared-secret-123".to_string();
        config.server.advertise_host = "10.0.0.12".to_string();
        config.webrtc.mode = WebRTCMode::PeerToPeer;
        config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_webrtc_signaling_only_mode_allowed_in_cluster() {
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.server.cluster_secret = "shared-secret-123".to_string();
        config.server.advertise_host = "10.0.0.12".to_string();
        config.webrtc.mode = WebRTCMode::SignalingOnly;
        config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_redis_standalone_mode_allowed() {
        let config = valid_prod_config();
        // Default is Standalone, should pass
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_redis_sentinel_mode_allowed() {
        let mut config = valid_prod_config();
        config.redis.deployment_mode = RedisDeploymentMode::Sentinel;
        config.redis.sentinel_master_name = Some("mymaster".to_string());
        config.redis.sentinel_addresses = vec!["127.0.0.1:26379".to_string()];
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_cluster_enabled_requires_redis() {
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.redis.url = String::new();
        // cluster.enabled=true with no Redis URL must produce an error
        let errors = config.validate().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("cluster mode requires Redis to be configured")),
            "Expected cluster+no-redis error, got: {errors:?}"
        );
    }

    #[test]
    fn test_validate_cluster_enabled_with_redis_ok() {
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.server.advertise_host = "10.0.0.12".to_string();
        // valid_prod_config() includes redis.url, so this should pass
        // (assuming webrtc.stun_external_addr is set for cluster mode)
        config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_cluster_enabled_allows_builtin_stun_without_explicit_external_addr() {
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.server.advertise_host = "10.0.0.12".to_string();
        config.webrtc.enable_builtin_stun = true;
        config.webrtc.stun_external_addr.clear();

        assert!(
            config.validate().is_ok(),
            "cluster mode should not reject STUN auto-detection paths during config validation"
        );
    }

    #[test]
    fn test_validate_cluster_enabled_with_sentinel_rejects_k8s_lease() {
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.redis.url = String::new();
        config.redis.deployment_mode = RedisDeploymentMode::Sentinel;
        config.redis.sentinel_master_name = Some("mymaster".to_string());
        config.redis.sentinel_addresses = vec!["127.0.0.1:26379".to_string()];
        config.cluster.leader_election_mode = ClusterLeaderElectionMode::K8sLease;
        config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();

        let errors = config
            .validate_with_env_map(&env_map(&[
                ("POD_NAME", "synctv-0"),
                ("POD_NAMESPACE", "default"),
            ]))
            .unwrap_err();

        assert!(
            errors
                .iter()
                .any(|e| e.contains("cluster.enabled=true is not supported with Redis Sentinel")),
            "expected Sentinel + k8s_lease rejection while Redis locks are still required, got: {errors:?}"
        );
    }

    #[test]
    fn test_validate_cluster_enabled_with_sentinel_rejects_redis_leader_election() {
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.redis.url = String::new();
        config.redis.deployment_mode = RedisDeploymentMode::Sentinel;
        config.redis.sentinel_master_name = Some("mymaster".to_string());
        config.redis.sentinel_addresses = vec!["127.0.0.1:26379".to_string()];
        config.cluster.leader_election_mode = ClusterLeaderElectionMode::Redis;
        config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();

        let errors = config.validate().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("cluster.enabled=true is not supported with Redis Sentinel")),
            "expected Sentinel rejection in cluster mode, got: {errors:?}"
        );
    }

    #[test]
    fn test_validate_cluster_secret_without_cluster_enabled_is_standalone() {
        let mut config = valid_prod_config();
        // cluster_secret alone must not implicitly enable cluster mode
        config.server.cluster_secret = "shared-secret-long-enough".to_string();
        config.redis.url = String::new();
        config.livestream.hls_shared_storage = false;
        config.webrtc.stun_external_addr = String::new();
        assert!(
            config.validate().is_ok(),
            "cluster_secret alone should not require cluster runtime services"
        );
    }

    #[test]
    fn test_validate_metrics_endpoint_requires_bearer_token_when_enabled() {
        let mut config = valid_prod_config();
        config.metrics.enabled = true;
        config.metrics.auth.mode = MetricsAuthMode::BearerToken;
        config.metrics.auth.bearer_token.clear();

        let errors = config
            .validate()
            .expect_err("metrics endpoint must fail closed when enabled without auth");

        assert!(
            errors.iter().any(|e| {
                e.contains("metrics.auth.bearer_token")
                    && e.contains("metrics.enabled")
                    && e.contains("must be set")
            }),
            "unexpected errors: {errors:?}"
        );
    }

    #[test]
    fn test_validate_metrics_endpoint_accepts_bearer_token_when_enabled() {
        let mut config = valid_prod_config();
        config.metrics.enabled = true;
        config.metrics.auth.mode = MetricsAuthMode::BearerToken;
        config.metrics.auth.bearer_token = "metrics-secret".to_string();

        assert!(
            config.validate().is_ok(),
            "authenticated metrics endpoint should be allowed"
        );
    }

    #[test]
    fn test_validate_metrics_endpoint_accepts_basic_auth_when_enabled() {
        let mut config = valid_prod_config();
        config.metrics.enabled = true;
        config.metrics.auth.mode = MetricsAuthMode::Basic;
        config.metrics.auth.basic_username = "metrics".to_string();
        config.metrics.auth.basic_password = "metrics-password".to_string();

        assert!(
            config.validate().is_ok(),
            "basic-authenticated metrics endpoint should be allowed"
        );
    }

    #[test]
    fn test_validate_metrics_endpoint_requires_basic_password_when_basic_auth_enabled() {
        let mut config = valid_prod_config();
        config.metrics.enabled = true;
        config.metrics.auth.mode = MetricsAuthMode::Basic;
        config.metrics.auth.basic_username = "metrics".to_string();
        config.metrics.auth.basic_password.clear();

        let errors = config
            .validate()
            .expect_err("metrics basic auth must reject missing password");

        assert!(
            errors
                .iter()
                .any(|e| e.contains("metrics.auth.basic_password")),
            "unexpected errors: {errors:?}"
        );
    }

    #[test]
    fn test_validate_metrics_tls_requires_cert_and_key_paths() {
        let mut config = valid_prod_config();
        config.metrics.enabled = true;
        config.metrics.auth.mode = MetricsAuthMode::BearerToken;
        config.metrics.auth.bearer_token = "metrics-secret".to_string();
        config.metrics.tls.enabled = true;
        config.metrics.tls.cert_path.clear();
        config.metrics.tls.key_path.clear();

        let errors = config
            .validate()
            .expect_err("metrics TLS must require cert and key paths");

        assert!(
            errors.iter().any(|e| e.contains("metrics.tls.cert_path")),
            "unexpected errors: {errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("metrics.tls.key_path")),
            "unexpected errors: {errors:?}"
        );
    }

    #[test]
    fn test_validate_cluster_enabled_requires_cluster_secret() {
        // cluster.enabled=true + cluster_secret empty → must be an error
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.server.cluster_secret = String::new(); // clear the secret
        let errors = config.validate().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("cluster_secret must be set when cluster mode is enabled")),
            "Expected cluster_secret error, got: {errors:?}"
        );
    }

    #[test]
    fn test_validate_standalone_redis_without_cluster_secret_ok() {
        // Standalone mode (cluster.enabled=false) with Redis but no cluster_secret → OK
        let mut config = valid_prod_config();
        config.cluster.enabled = false;
        config.server.cluster_secret = String::new();
        assert!(
            config.validate().is_ok(),
            "Expected Ok in standalone mode without cluster_secret"
        );
    }

    #[test]
    fn test_validate_cluster_enabled_with_cluster_secret_ok() {
        // cluster.enabled=true + cluster_secret set → should pass
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.server.advertise_host = "10.0.0.12".to_string();
        assert!(
            config.validate().is_ok(),
            "Expected Ok with cluster mode + cluster_secret set"
        );
    }

    #[test]
    fn test_validate_cluster_secret_too_short_rejected() {
        // cluster_secret too short must be rejected
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.server.cluster_secret = "short".to_string();
        let errors = config.validate().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("cluster_secret is too short")),
            "Expected short cluster_secret error, got: {errors:?}"
        );
    }

    #[test]
    fn test_from_env_rejects_unknown_cluster_discovery_mode() {
        let error = Config::from_env_map(&env_map(&[("SYNCTV_CLUSTER_DISCOVERY_MODE", "mystery")]))
            .expect_err("invalid discovery mode override must fail closed");

        assert!(
            error.to_string().contains("SYNCTV_CLUSTER_DISCOVERY_MODE")
                && error
                    .to_string()
                    .contains(ClusterDiscoveryMode::ALLOWED_VALUES),
            "Expected discovery_mode parse error, got: {error}"
        );
    }

    #[test]
    fn test_from_env_rejects_unknown_cluster_leader_election_mode() {
        let error = Config::from_env_map(&env_map(&[(
            "SYNCTV_CLUSTER_LEADER_ELECTION_MODE",
            "mystery",
        )]))
        .expect_err("invalid leader election mode override must fail closed");

        assert!(
            error
                .to_string()
                .contains("SYNCTV_CLUSTER_LEADER_ELECTION_MODE")
                && error
                    .to_string()
                    .contains(ClusterLeaderElectionMode::ALLOWED_VALUES),
            "Expected leader_election_mode parse error, got: {error}"
        );
    }

    #[test]
    fn test_validate_k8s_dns_requires_env_vars_in_cluster_mode() {
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.cluster.discovery_mode = ClusterDiscoveryMode::K8sDns;

        let errors = config.validate_with_env_map(&HashMap::new()).unwrap_err();

        assert!(
            errors
                .iter()
                .any(|e| e.contains("HEADLESS_SERVICE_NAME") && e.contains("k8s_dns")),
            "Expected HEADLESS_SERVICE_NAME validation error, got: {errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("POD_NAMESPACE") && e.contains("k8s_dns")),
            "Expected POD_NAMESPACE validation error, got: {errors:?}"
        );
        assert!(
            errors
                .iter()
                .all(|e| !e.contains("POD_IP") || !e.contains("k8s_dns")),
            "Offline validation must not require runtime-only POD_IP, got: {errors:?}"
        );
    }

    #[test]
    fn test_validate_k8s_lease_requires_env_vars_in_cluster_mode() {
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.cluster.leader_election_mode = ClusterLeaderElectionMode::K8sLease;

        let errors = config.validate_with_env_map(&HashMap::new()).unwrap_err();

        assert!(
            errors
                .iter()
                .any(|e| e.contains("POD_NAME") && e.contains("k8s_lease")),
            "Expected POD_NAME validation error, got: {errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("POD_NAMESPACE") && e.contains("k8s_lease")),
            "Expected POD_NAMESPACE validation error, got: {errors:?}"
        );

        if !cfg!(feature = "k8s") {
            assert!(
                errors
                    .iter()
                    .any(|e| e.contains("k8s_lease") && e.contains("requires the 'k8s' feature")),
                "Expected k8s feature validation error, got: {errors:?}"
            );
        }
    }

    #[test]
    fn test_validate_k8s_dns_requires_compiled_k8s_support() {
        if cfg!(feature = "k8s") {
            return;
        }

        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.cluster.discovery_mode = ClusterDiscoveryMode::K8sDns;

        let errors = config
            .validate_with_env_map(&env_map(&[
                ("HEADLESS_SERVICE_NAME", "synctv-headless"),
                ("POD_NAMESPACE", "default"),
            ]))
            .unwrap_err();

        assert!(
            errors
                .iter()
                .any(|e| e.contains("k8s_dns") && e.contains("requires the 'k8s' feature")),
            "Expected k8s feature validation error, got: {errors:?}"
        );
    }

    #[test]
    fn test_validate_k8s_lease_requires_compiled_k8s_support() {
        if cfg!(feature = "k8s") {
            return;
        }

        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        config.cluster.leader_election_mode = ClusterLeaderElectionMode::K8sLease;

        let errors = config
            .validate_with_env_map(&env_map(&[
                ("POD_NAME", "synctv-0"),
                ("POD_NAMESPACE", "default"),
            ]))
            .unwrap_err();

        assert!(
            errors
                .iter()
                .any(|e| e.contains("k8s_lease") && e.contains("requires the 'k8s' feature")),
            "Expected k8s feature validation error, got: {errors:?}"
        );
    }

    #[test]
    fn test_validate_shared_hls_storage_requires_storage_path() {
        let mut config = valid_prod_config();
        config.livestream.hls_storage_backend = HlsStorageBackend::File;
        config.livestream.hls_shared_storage = true;
        config.livestream.hls_storage_path = String::new();

        let errors = config.validate().unwrap_err();

        assert!(
            errors
                .iter()
                .any(|e| e.contains("hls_storage_path") && e.contains("must be set")),
            "Expected hls_storage_path validation error, got: {errors:?}"
        );
    }

    #[test]
    fn test_validate_oss_hls_storage_requires_required_fields() {
        let mut config = valid_prod_config();
        config.cluster.enabled = false;
        config.server.cluster_secret.clear();
        config.livestream.hls_storage_backend = HlsStorageBackend::Oss;
        config.livestream.hls_shared_storage = false;
        config.livestream.hls_oss = HlsOssConfig::default();

        let errors = config.validate().unwrap_err();

        assert!(
            errors.iter().any(|e| e.contains("hls_oss.endpoint")),
            "Expected hls_oss.endpoint validation error, got: {errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("hls_oss.bucket")),
            "Expected hls_oss.bucket validation error, got: {errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("hls_oss.access_key_id")),
            "Expected hls_oss.access_key_id validation error, got: {errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("hls_oss.secret_access_key")),
            "Expected hls_oss.secret_access_key validation error, got: {errors:?}"
        );
    }
}
