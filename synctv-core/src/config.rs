use crate::models::FileBlobCompression;
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
const KNOWN_DEV_CREDENTIAL_ENCRYPTION_KEYS: &[&str] = &[
    "111102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
    "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
];
const KNOWN_DEV_OPAQUE_SERVER_SETUP_SECRETS: &[&str] = &[
    "dev-opaque-server-setup-secret-please-change-1234567890",
    "dev-opaque-server-setup-secret-please-change-in-production",
];
const KNOWN_DEV_JWT_SECRETS: &[&str] = &[
    "aDsPda5skjBg4km/8XFxBntIQ2ppbBTAAFT7P2PdzPA=",
    "dev-jwt-secret-please-change-in-production-1234567890",
];
const KNOWN_DEV_CLUSTER_SECRETS: &[&str] =
    &["dev-cluster-secret-please-change-in-production-1234567890"];
const KNOWN_DEV_ROOT_PASSWORDS: &[&str] = &["Rootpasswd1234567890!", "DevRootPass12345"];

fn is_known_dev_secret(value: &str, known_values: &[&str]) -> bool {
    known_values.contains(&value)
}

fn is_known_dev_hex_secret(value: &str, known_values: &[&str]) -> bool {
    known_values
        .iter()
        .any(|known| value.eq_ignore_ascii_case(known))
}

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnknownConfigDiagnostics {
    pub config_file: Option<String>,
    pub config_keys: Vec<String>,
    pub env_keys: Vec<String>,
}

impl UnknownConfigDiagnostics {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.config_keys.is_empty() && self.env_keys.is_empty()
    }

    #[must_use]
    pub fn strict_error_message(&self) -> String {
        let mut parts = Vec::new();
        if !self.config_keys.is_empty() {
            let source = self.config_file.as_deref().map_or_else(
                || "config file".to_string(),
                |path| format!("config file {path}"),
            );
            parts.push(format!(
                "unsupported key(s) in {source}: {}",
                self.config_keys.join(", ")
            ));
        }
        if !self.env_keys.is_empty() {
            parts.push(format!(
                "unsupported SYNCTV_ environment variable(s): {}",
                self.env_keys.join(", ")
            ));
        }
        parts.join("; ")
    }
}

struct LoadedConfig {
    config: Config,
    unknown: UnknownConfigDiagnostics,
}

pub fn default_data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        user_home_dir().map_or_else(
            || std::env::temp_dir().join("synctv"),
            |home| home.join(".synctv"),
        )
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        absolute_env_path("XDG_STATE_HOME")
            .map(|dir| dir.join("synctv"))
            .or_else(|| {
                user_home_dir().map(|home| home.join(".local").join("state").join("synctv"))
            })
            .unwrap_or_else(|| std::env::temp_dir().join("synctv"))
    }

    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| user_home_dir().map(|home| home.join("AppData").join("Local")))
            .unwrap_or_else(std::env::temp_dir)
            .join("synctv")
    }

    #[cfg(not(any(target_os = "macos", unix, windows)))]
    {
        std::env::temp_dir().join("synctv")
    }
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
        "cluster.secret"
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
            | "livestream.hls_oss.access_key_id"
            | "livestream.hls_oss.secret_access_key"
            | "file_storage.upload_token_secret"
            | "bootstrap.root_password"
    ) || (current_path.starts_with("file_storage.backends.")
        && matches!(base_key, "access_key_id" | "secret_access_key"))
        || (current_path.starts_with("media_providers.") && is_secret_like_provider_key(base_key))
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
    let has_split_config = ["host", "port", "username", "password", "name"]
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

fn trim_ipv6_host_brackets(host: &str) -> &str {
    host.trim()
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or_else(|| host.trim())
}

fn build_url_from_split_parts(
    scheme: &str,
    host: &str,
    port: u16,
    username: Option<&str>,
    password: Option<&str>,
    path_segment: Option<&str>,
) -> String {
    let Ok(mut url) = url::Url::parse(&format!("{scheme}://localhost/")) else {
        return String::new();
    };

    if url.set_host(Some(trim_ipv6_host_brackets(host))).is_err()
        || url.set_port(Some(port)).is_err()
    {
        return String::new();
    }

    if let Some(username) = username {
        if url.set_username(username).is_err() {
            return String::new();
        }
    }
    if let Some(password) = password {
        if url.set_password(Some(password)).is_err() {
            return String::new();
        }
    }
    if let Some(path_segment) = path_segment {
        let Ok(mut segments) = url.path_segments_mut() else {
            return String::new();
        };
        segments.clear().push(path_segment);
        drop(segments);
    }

    url.to_string()
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
    pub file_storage: FileStorageConfig,
    pub chat: ChatConfig,
    pub webauthn: WebAuthnConfig,
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
    pub request_rate_limits: RequestRateLimitConfig,
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
            .field("file_storage", &self.file_storage)
            .field("chat", &self.chat)
            .field("webauthn", &self.webauthn)
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
            .field("request_rate_limits", &self.request_rate_limits)
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
            file_storage: FileStorageConfig::default(),
            chat: ChatConfig::default(),
            webauthn: WebAuthnConfig::default(),
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
            request_rate_limits: RequestRateLimitConfig::default(),
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
    /// Global SSRF policy for all outbound server-side requests.
    pub ssrf: SsrfConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct SsrfConfig {
    /// Enable SSRF protection for outbound server-side requests.
    ///
    /// Defaults to `false` so self-hosted deployments can bind private media
    /// providers without extra configuration. Public deployments should enable
    /// this and configure the narrowest allowlist that covers their providers.
    pub enabled: bool,
    /// Allow outbound server-side requests to private, loopback, link-local,
    /// reserved, and metadata-network targets.
    pub allow_private_network_targets: bool,
    /// Additional hostnames allowed by the global SSRF policy.
    pub allowed_hosts: Vec<String>,
    /// Additional IP/CIDR ranges allowed by the global SSRF policy.
    pub allowed_ip_ranges: Vec<String>,
}

impl SecurityConfig {
    #[must_use]
    pub fn ssrf_guard(&self) -> synctv_common::ssrf::SsrfGuard {
        if !self.ssrf.enabled {
            return synctv_common::ssrf::SsrfGuard::disabled();
        }

        let mut builder = synctv_common::ssrf::SsrfGuard::builder();
        if self.ssrf.allow_private_network_targets {
            builder = builder.allow_private_network_targets(true);
        }
        for host in &self.ssrf.allowed_hosts {
            builder = builder.extra_allowed_host(host.clone());
        }
        for range in &self.ssrf.allowed_ip_ranges {
            if let Ok(range) = range.parse() {
                builder = builder.extra_allowed_ip_range(range);
            }
        }
        builder.build()
    }
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

/// Domain-level messaging rate limits for chat.
///
/// These limits are enforced by the shared chat/messaging business logic and
/// therefore must come from configuration rather than hard-coded defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MessagingRateLimitConfig {
    /// Maximum chat messages allowed within the configured window.
    pub chat_per_second: u32,
    /// Sliding-window size for chat enforcement.
    pub window_seconds: u64,
}

impl Default for MessagingRateLimitConfig {
    fn default() -> Self {
        Self {
            chat_per_second: 10,
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
    /// Enable gzip compression negotiation for gRPC request and response bodies.
    /// Compression is only used when the peer advertises support.
    pub grpc_compression_enabled: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8080,
            enable_reflection: false,
            trusted_proxies: Vec::new(),
            cors_allowed_origins: Vec::new(),
            advertise_host: String::new(),
            shutdown_drain_timeout_seconds: 30,
            grpc_max_message_size_bytes: 16 * 1024 * 1024, // 16 MB default
            grpc_compression_enabled: true,
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
    pub username: String,
    pub password: String,
    pub database: i64,
    pub connect_timeout_seconds: u64,
    pub response_timeout_seconds: u64,
    pub pipeline_buffer_size: usize,
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
            .field("response_timeout_seconds", &self.response_timeout_seconds)
            .field("pipeline_buffer_size", &self.pipeline_buffer_size)
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
            response_timeout_seconds: 5,
            pipeline_buffer_size: 512,
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
    SharedFile,
    Oss,
}

impl FromStr for HlsStorageBackend {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "memory" => Ok(Self::Memory),
            "file" => Ok(Self::File),
            "shared_file" => Ok(Self::SharedFile),
            "oss" => Ok(Self::Oss),
            _ => Err(ConfigError::Message(format!(
                "livestream.hls_storage_backend '{value}' must be one of: memory, file, shared_file, oss"
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
    /// Set this when publishers must connect through an external domain,
    /// LoadBalancer, or node address. If empty, SyncTV falls back only to the
    /// local bind host for single-node/local use; it intentionally does not
    /// reuse `server.advertise_host`, which may be an internal Pod IP or
    /// service DNS name used for cluster traffic.
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
    /// Maximum memory (in megabytes) for the GOP cache per stream.
    /// When exceeded, the oldest GOP is evicted even if `gop_cache_size` hasn't
    /// been reached. Default: 100 MB. Set to 0 to use the built-in default (500 MB).
    pub gop_cache_max_memory_mb: u64,
    /// Maximum memory (in megabytes) for in-memory HLS segment storage.
    /// 0 means use the built-in default (512 MB).
    pub hls_memory_max_mb: u64,
    /// HLS segment storage backend.
    ///
    /// - `memory`: in-process memory storage.
    /// - `file`: node-local filesystem storage at `hls_storage_path`.
    /// - `shared_file`: shared filesystem storage at `hls_storage_path`.
    /// - `oss`: S3-compatible object storage configured by `hls_oss`.
    pub hls_storage_backend: HlsStorageBackend,
    /// Base path for HLS segment storage.
    ///
    /// Used for validation: paths that are obviously local-only (e.g. /tmp/)
    /// trigger a stronger warning in cluster mode when `hls_storage_backend=shared_file`.
    /// Required when `hls_storage_backend=file` or `shared_file`.
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
            hls_storage_path: String::new(),
            hls_oss: HlsOssConfig::default(),
            flv_max_connection_duration_seconds: 86400, // 24 hours
            flv_write_timeout_seconds: 30,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum FileStorageBackendType {
    #[default]
    Disabled,
    Database,
    S3,
}

impl FromStr for FileStorageBackendType {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" => Ok(Self::Disabled),
            "database" => Ok(Self::Database),
            "s3" => Ok(Self::S3),
            _ => Err(ConfigError::Message(format!(
                "file storage backend type '{value}' must be one of: disabled, database, s3"
            ))),
        }
    }
}

impl FileStorageBackendType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Database => "database",
            Self::S3 => "s3",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FileStorageS3Config {
    /// S3-compatible endpoint, for example `https://s3.amazonaws.com` or `https://minio.example.com`.
    pub endpoint: String,
    /// Access key ID used to presign file uploads.
    pub access_key_id: String,
    /// Secret access key used to presign file uploads.
    pub secret_access_key: String,
    /// Bucket name.
    pub bucket: String,
    /// S3 region. Use `auto` for providers that accept it.
    pub region: String,
    /// Object key prefix inside the bucket, for example `files/`.
    pub base_path: String,
    /// Optional public base URL for serving file objects.
    pub public_base_url: Option<String>,
    /// Presigned upload URL TTL in seconds.
    pub upload_expires_seconds: i64,
}

impl Default for FileStorageS3Config {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            bucket: String::new(),
            region: "auto".to_string(),
            base_path: "files/".to_string(),
            public_base_url: None,
            upload_expires_seconds: 900,
        }
    }
}

impl std::fmt::Debug for FileStorageS3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStorageS3Config")
            .field("endpoint", &self.endpoint)
            .field("access_key_id", &"<redacted>")
            .field("secret_access_key", &"<redacted>")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("base_path", &self.base_path)
            .field("public_base_url", &self.public_base_url)
            .field("upload_expires_seconds", &self.upload_expires_seconds)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum FileStorageDatabaseCompression {
    None,
    Lz4,
    #[default]
    Zstd,
}

impl FromStr for FileStorageDatabaseCompression {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "lz4" => Ok(Self::Lz4),
            "zstd" => Ok(Self::Zstd),
            _ => Err(ConfigError::Message(format!(
                "database file storage compression '{value}' must be one of: none, lz4, zstd"
            ))),
        }
    }
}

impl From<FileStorageDatabaseCompression> for FileBlobCompression {
    fn from(value: FileStorageDatabaseCompression) -> Self {
        match value {
            FileStorageDatabaseCompression::None => Self::None,
            FileStorageDatabaseCompression::Lz4 => Self::Lz4,
            FileStorageDatabaseCompression::Zstd => Self::Zstd,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FileStorageDatabaseConfig {
    /// Compression algorithm used for payload bytes stored in `file_blobs`.
    pub compression: FileStorageDatabaseCompression,
}

impl Default for FileStorageDatabaseConfig {
    fn default() -> Self {
        Self {
            compression: FileStorageDatabaseCompression::Zstd,
        }
    }
}

impl std::fmt::Debug for FileStorageDatabaseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStorageDatabaseConfig")
            .field("compression", &self.compression)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FileStorageBackendConfig {
    /// Backend implementation type.
    #[serde(rename = "type")]
    pub backend_type: FileStorageBackendType,
    /// Database settings used when `type=database`.
    pub database: FileStorageDatabaseConfig,
    /// S3-compatible settings used when `type=s3`.
    pub s3: FileStorageS3Config,
}

impl Default for FileStorageBackendConfig {
    fn default() -> Self {
        Self {
            backend_type: FileStorageBackendType::Disabled,
            database: FileStorageDatabaseConfig::default(),
            s3: FileStorageS3Config::default(),
        }
    }
}

impl std::fmt::Debug for FileStorageBackendConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStorageBackendConfig")
            .field("type", &self.backend_type)
            .field("database", &self.database)
            .field("s3", &self.s3)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FileStorageConfig {
    /// Secret used to sign short-lived file upload session tokens.
    /// When empty, startup derives a stable secret from `jwt.secret`.
    pub upload_token_secret: String,
    /// Fallback backend name used by product features with no explicit selection.
    pub default_backend: String,
    /// Backend name for chat attachment uploads.
    pub chat_attachments_backend: String,
    /// Backend name for user avatar uploads.
    pub user_avatars_backend: String,
    /// Backend name for media cover uploads.
    pub media_covers_backend: String,
    /// Backend name for room cover uploads.
    pub room_covers_backend: String,
    /// Backend name for playlist cover uploads.
    pub playlist_covers_backend: String,
    /// Seconds to keep uploaded file objects that have no active product reference.
    /// Set to 0 to disable orphan cleanup.
    pub unreferenced_object_retention_seconds: u64,
    /// Registered file storage backends keyed by stable storage name.
    pub backends: HashMap<String, FileStorageBackendConfig>,
}

impl Default for FileStorageConfig {
    fn default() -> Self {
        Self {
            upload_token_secret: String::new(),
            default_backend: "disabled".to_string(),
            chat_attachments_backend: String::new(),
            user_avatars_backend: String::new(),
            media_covers_backend: String::new(),
            room_covers_backend: String::new(),
            playlist_covers_backend: String::new(),
            unreferenced_object_retention_seconds: 86_400,
            backends: HashMap::new(),
        }
    }
}

impl std::fmt::Debug for FileStorageConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStorageConfig")
            .field("upload_token_secret", &"<redacted>")
            .field("default_backend", &self.default_backend)
            .field("chat_attachments_backend", &self.chat_attachments_backend)
            .field("user_avatars_backend", &self.user_avatars_backend)
            .field("media_covers_backend", &self.media_covers_backend)
            .field("room_covers_backend", &self.room_covers_backend)
            .field("playlist_covers_backend", &self.playlist_covers_backend)
            .field(
                "unreferenced_object_retention_seconds",
                &self.unreferenced_object_retention_seconds,
            )
            .field("backends", &self.backends)
            .finish()
    }
}

impl FileStorageConfig {
    #[must_use]
    pub fn backend_for_chat_attachments(&self) -> &str {
        self.selected_backend_or_default(&self.chat_attachments_backend)
    }

    #[must_use]
    pub fn backend_for_user_avatars(&self) -> &str {
        self.selected_backend_or_default(&self.user_avatars_backend)
    }

    #[must_use]
    pub fn backend_for_media_covers(&self) -> &str {
        self.selected_backend_or_default(&self.media_covers_backend)
    }

    #[must_use]
    pub fn backend_for_room_covers(&self) -> &str {
        self.selected_backend_or_default(&self.room_covers_backend)
    }

    #[must_use]
    pub fn backend_for_playlist_covers(&self) -> &str {
        self.selected_backend_or_default(&self.playlist_covers_backend)
    }

    fn selected_backend_or_default<'a>(&'a self, selected: &'a str) -> &'a str {
        let selected = selected.trim();
        if selected.is_empty() {
            self.default_backend.trim()
        } else {
            selected
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ChatConfig {}

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
    /// In K8s/NAT environments, set this to a client-reachable public
    /// `ip:port` or DNS name, such as a LoadBalancer IP, node public IP,
    /// or STUN hostname. If empty, runtime bootstrap tries advertise_host,
    /// STUN_EXTERNAL_IP, and cloud metadata, then skips built-in STUN when
    /// no public address is found.
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

mod env;
mod runtime;
mod validation;

fn is_cli_only_synctv_env_var(key: &str) -> bool {
    matches!(key, "SYNCTV_MANAGEMENT_ENDPOINT") || key.starts_with("SYNCTV_TEST_")
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
            max_per_user: 20,
            max_per_room: 2000,
            max_total: 100_000,
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
    /// Root password (IMPORTANT: Change this in production!)
    pub root_password: String,
}

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self {
            create_root_user: false,
            root_username: "root".to_string(),
            root_password: String::new(),
        }
    }
}

impl BootstrapConfig {
    #[must_use]
    pub fn validate_root_password_for_creation(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let pwd = &self.root_password;
        if pwd.is_empty() {
            errors.push(
                "Root password is empty. Set SYNCTV_BOOTSTRAP_ROOT_PASSWORD environment variable"
                    .to_string(),
            );
            return errors;
        }
        if pwd == "root" || is_known_dev_secret(pwd, KNOWN_DEV_ROOT_PASSWORDS) {
            errors.push("Root password is set to default value 'root'. Set SYNCTV_BOOTSTRAP_ROOT_PASSWORD environment variable".to_string());
            return errors;
        }
        if pwd.len() < 12 {
            errors.push("Root password must be at least 12 characters".to_string());
        }
        if !pwd.chars().any(char::is_uppercase) {
            errors.push("Root password must contain at least one uppercase letter".to_string());
        }
        if !pwd.chars().any(char::is_lowercase) {
            errors.push("Root password must contain at least one lowercase letter".to_string());
        }
        if !pwd.chars().any(|c| c.is_ascii_digit()) {
            errors.push("Root password must contain at least one digit".to_string());
        }
        errors
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

    /// Shared secret for authenticating cluster gRPC calls between nodes.
    /// When set, all inter-node gRPC requests must include this secret in the
    /// `x-cluster-secret` metadata header. If empty, cluster endpoints are disabled.
    pub secret: String,

    /// Capacity for the high-priority critical event channel.
    /// Critical events (`KickPublisher`, `KickUser`, `PermissionChanged`) are never dropped;
    /// when this channel is full, senders block until space is available.
    /// Default: 10000
    pub critical_channel_capacity: usize,

    /// Capacity for the normal-priority Redis publish channel.
    /// Normal events are dropped with a warning when this channel is full
    /// (e.g., during a prolonged Redis outage).
    /// Default: 100000
    pub publish_channel_capacity: usize,

    /// Discovery mode for cluster node registration.
    /// - `redis`: Use Redis-based node registry (default, works everywhere)
    /// - `static`: Use the configured `cluster.peers` list
    /// - `k8s_dns`: Use Kubernetes headless service DNS for peer discovery
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
    /// Default: 100000
    pub stream_max_length: usize,
}

impl Default for ClusterChannelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            secret: String::new(),
            critical_channel_capacity: 10_000,
            publish_channel_capacity: 100_000,
            discovery_mode: ClusterDiscoveryMode::Redis,
            leader_election_mode: ClusterLeaderElectionMode::Redis,
            peers: Vec::new(),
            catchup_window_secs: 300,
            stream_max_length: 100_000,
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
    pub zxcvbn_enabled: bool,
    pub zxcvbn_min_score: u8,
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
            zxcvbn_enabled: false,
            zxcvbn_min_score: 3,
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
            l1_capacity: 5000,
            l1_ttl_seconds: 300, // 5 minutes (was hardcoded as 5 min TTL)
            l2_ttl_seconds: 300, // 5 minutes
            username_cache_capacity: 10_000,
            username_cache_ttl_seconds: 3600, // 1 hour
            permission_cache_capacity: 20_000,
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
    /// Size of each cached byte-range slice.
    pub slice_size_bytes: usize,
    /// Maximum total cache size across all entries.
    pub max_cache_size_bytes: u64,
    /// TTL for fresh cached media slices.
    pub segment_ttl_seconds: u64,
    /// Maximum time an expired entry can be served as stale.
    pub stale_max_age_seconds: u64,
    /// Serve stale entries while a background revalidation is in progress.
    pub stale_while_revalidate: bool,
    /// Whether the proxy slice cache should persist entries to disk.
    pub file_backend_enabled: bool,
    /// Root directory for persisted proxy slice cache entries.
    ///
    /// Relative paths are resolved against the effective `data_dir`.
    pub file_cache_dir: String,
    /// Background eviction interval.
    pub eviction_interval_seconds: u64,
    /// Eviction target watermark as a fraction of max cache size.
    pub watermark_ratio: f64,
}

impl Default for ProxySliceCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            slice_size_bytes: 2 * 1024 * 1024,
            max_cache_size_bytes: 512 * 1024 * 1024,
            segment_ttl_seconds: 300,
            stale_max_age_seconds: 60,
            stale_while_revalidate: true,
            file_backend_enabled: false,
            file_cache_dir: String::new(),
            eviction_interval_seconds: 60,
            watermark_ratio: 0.875,
        }
    }
}

/// Request API rate limit configuration.
///
/// This is separate from the domain-level `RateLimitConfig` in
/// `synctv_core::service::rate_limit` (which controls chat rates).
/// This struct configures the shared request limits used by all transports.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestRateLimitConfig {
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

    /// Endpoint-level rules keyed by business scope name.
    pub scopes: HashMap<String, RateLimitScopeRule>,
}

impl Default for RequestRateLimitConfig {
    fn default() -> Self {
        Self {
            // Auth: 5 requests per minute
            auth_max_requests: 5,
            auth_window_seconds: 60,

            // Write: 120 requests per minute
            write_max_requests: 120,
            write_window_seconds: 60,

            // Read: 600 requests per minute
            read_max_requests: 600,
            read_window_seconds: 60,

            // Media: 120 requests per minute
            media_max_requests: 120,
            media_window_seconds: 60,

            // Admin: 180 requests per minute
            admin_max_requests: 180,
            admin_window_seconds: 60,

            // Streaming: 1200 requests per minute (playlist + segment fetches)
            streaming_max_requests: 1200,
            streaming_window_seconds: 60,

            // WebSocket: 60 connection attempts per minute
            websocket_max_requests: 60,
            websocket_window_seconds: 60,

            scopes: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitScopeStrategy {
    #[default]
    FixedWindow,
    Disabled,
}

impl RateLimitScopeStrategy {
    #[must_use]
    pub const fn enabled(self) -> bool {
        matches!(self, Self::FixedWindow)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimitScopeRule {
    /// Scope max requests. Falls back to the category-derived default when unset.
    pub max_requests: Option<u32>,
    /// Scope window. Falls back to the category-derived default when unset.
    pub window_seconds: Option<u64>,
    /// Limiting strategy for this scope.
    pub strategy: RateLimitScopeStrategy,
}

#[cfg(test)]
mod tests;
