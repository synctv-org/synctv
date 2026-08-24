use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

mod runtime;
mod validation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StartupDiagnostic {
    Info(String),
    Warning(String),
}

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

fn is_known_dev_secret(value: &str, known_values: &[&str]) -> bool {
    known_values.contains(&value)
}

fn is_known_dev_hex_secret(value: &str, known_values: &[&str]) -> bool {
    known_values
        .iter()
        .any(|known| value.eq_ignore_ascii_case(known))
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub project_url: String,
    pub web_ui_directory: Option<PathBuf>,
    pub enable_reflection: bool,
    pub trusted_proxies: Vec<String>,
    pub cors_allowed_origins: Vec<String>,
    pub advertise_host: String,
    pub shutdown_drain_timeout_seconds: u64,
    pub grpc_max_message_size_bytes: usize,
    pub grpc_compression_enabled: bool,
    pub logging: LoggingConfig,
    pub access_log: AccessLogConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8080,
            project_url: synctv_api::DEFAULT_PROJECT_URL.to_string(),
            web_ui_directory: None,
            enable_reflection: false,
            trusted_proxies: Vec::new(),
            cors_allowed_origins: Vec::new(),
            advertise_host: String::new(),
            shutdown_drain_timeout_seconds: 30,
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            grpc_compression_enabled: true,
            logging: LoggingConfig::default(),
            access_log: AccessLogConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AccessLogConfig {
    pub enabled: bool,
    pub slow_request_threshold_ms: u64,
    #[serde(flatten)]
    pub logging: LoggingConfig,
}

impl Default for AccessLogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            slow_request_threshold_ms: 1_000,
            logging: LoggingConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: String,
    pub format: String,
    pub output: LogOutput,
    pub color: LogColor,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: "text".to_string(),
            output: LogOutput::Named(LogOutputName::Stdout),
            color: LogColor::Auto,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogColor {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LogOutput {
    Named(LogOutputName),
    File(LogFileOutput),
}

impl Default for LogOutput {
    fn default() -> Self {
        Self::Named(LogOutputName::Stdout)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogOutputName {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogFileOutput {
    pub r#type: String,
    pub path: String,
    pub rotation: LogRotation,
}

impl Default for LogFileOutput {
    fn default() -> Self {
        Self {
            r#type: "file".to_string(),
            path: String::new(),
            rotation: LogRotation::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogRotation {
    pub strategy: String,
    pub max_files: usize,
}

impl Default for LogRotation {
    fn default() -> Self {
        Self {
            strategy: "daily".to_string(),
            max_files: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub logging: LoggingConfig,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            host: "127.0.0.1".to_string(),
            port: 8081,
            logging: LoggingConfig::default(),
        }
    }
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

#[cfg(all(unix, not(target_os = "macos")))]
fn absolute_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
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

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SecurityConfig {
    pub credential_encryption_key: String,
    pub totp_encryption_key: String,
    pub email_outbox_encryption_key: String,
    pub opaque_server_setup_secret: String,
    pub proxy_signing_key: String,
    pub media_swarm_signing_key: String,
    pub provider_session_encryption_key: String,
    pub login_discovery_key: String,
    pub webauthn_enumeration_key: String,
    pub ssrf: SsrfConfig,
}

impl std::fmt::Debug for SecurityConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityConfig")
            .field("credential_encryption_key", &"<redacted>")
            .field("totp_encryption_key", &"<redacted>")
            .field("email_outbox_encryption_key", &"<redacted>")
            .field("opaque_server_setup_secret", &"<redacted>")
            .field("proxy_signing_key", &"<redacted>")
            .field("media_swarm_signing_key", &"<redacted>")
            .field("provider_session_encryption_key", &"<redacted>")
            .field("login_discovery_key", &"<redacted>")
            .field("webauthn_enumeration_key", &"<redacted>")
            .field("ssrf", &self.ssrf)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SsrfConfig {
    pub enabled: bool,
    pub allow_private_network_targets: bool,
    pub allowed_hosts: Vec<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TimeConfig {
    pub timezone: String,
    pub clock_sync: ClockSyncConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ClockSyncConfig {
    pub enabled: bool,
    pub provider: ClockSyncProvider,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClockSyncProvider {
    Sntp(ClockSyncSntpProviderConfig),
}

impl Default for ClockSyncProvider {
    fn default() -> Self {
        Self::Sntp(ClockSyncSntpProviderConfig::default())
    }
}

impl std::str::FromStr for ClockSyncProvider {
    type Err = config::ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "sntp" => Ok(Self::Sntp(ClockSyncSntpProviderConfig::default())),
            _ => Err(config::ConfigError::Message(format!(
                "clock sync provider type '{value}' must be one of: sntp"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ClockSyncSntpProviderConfig {
    pub servers: Vec<String>,
    pub interval_seconds: u64,
    pub timeout_millis: u64,
}

impl Default for ClockSyncSntpProviderConfig {
    fn default() -> Self {
        Self {
            servers: vec![
                "time.cloudflare.com:123".to_string(),
                "pool.ntp.org:123".to_string(),
            ],
            interval_seconds: 300,
            timeout_millis: 1_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MessagingRateLimitConfig {
    pub chat_per_second: u32,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MetricsAuthMode {
    #[default]
    BearerToken,
    Basic,
    Kubernetes,
}

impl std::str::FromStr for MetricsAuthMode {
    type Err = config::ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "bearer_token" => Ok(Self::BearerToken),
            "basic" => Ok(Self::Basic),
            "kubernetes" => Ok(Self::Kubernetes),
            _ => Err(config::ConfigError::Message(format!(
                "metrics.auth.mode '{value}' must be one of: bearer_token, basic, kubernetes"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MetricsTlsConfig {
    pub enabled: bool,
    pub cert_path: String,
    pub key_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsKubernetesAuthConfig {
    pub audience: String,
    pub authentication_cache_ttl_seconds: u64,
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
    pub mode: MetricsAuthMode,
    pub bearer_token: String,
    pub basic_username: String,
    pub basic_password: String,
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
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub tls: MetricsTlsConfig,
    pub auth: MetricsAuthConfig,
    pub logging: LoggingConfig,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "0.0.0.0".to_string(),
            port: 9090,
            tls: MetricsTlsConfig::default(),
            auth: MetricsAuthConfig::default(),
            logging: LoggingConfig {
                level: "warn".to_string(),
                ..LoggingConfig::default()
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ManagementTransport {
    #[default]
    Tcp,
    Unix,
}

impl std::str::FromStr for ManagementTransport {
    type Err = config::ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "tcp" => Ok(Self::Tcp),
            "unix" => Ok(Self::Unix),
            _ => Err(config::ConfigError::Message(format!(
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
    pub logging: LoggingConfig,
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
            logging: LoggingConfig::default(),
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

fn mask_url_password_for_debug(url: &str) -> String {
    let Some(at_pos) = url.find('@') else {
        return url.to_string();
    };
    let Some(colon_pos) = url[..at_pos].rfind(':') else {
        return url.to_string();
    };
    let scheme_end = url.find("://").map_or(0, |p| p + 3);
    if colon_pos < scheme_end {
        return url.to_string();
    }
    format!("{}:****@{}", &url[..colon_pos], &url[at_pos + 1..])
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub url: String,
    pub read_url: String,
    pub read_host: String,
    pub read_port: u16,
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

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "postgresql://synctv:synctv@localhost:5432/synctv".to_string(),
            read_url: String::new(),
            read_host: String::new(),
            read_port: 0,
            host: String::new(),
            port: 0,
            username: String::new(),
            password: String::new(),
            name: String::new(),
            max_connections: 20,
            min_connections: 5,
            connect_timeout_seconds: 10,
            idle_timeout_seconds: 600,
            max_lifetime_seconds: 1800,
        }
    }
}

impl std::fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseConfig")
            .field("url", &mask_url_password_for_debug(&self.url))
            .field("read_url", &mask_url_password_for_debug(&self.read_url))
            .field("read_host", &self.read_host)
            .field("read_port", &self.read_port)
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
    pub deployment_mode: RedisDeploymentMode,
    pub sentinel_master_name: Option<String>,
    pub sentinel_addresses: Vec<String>,
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

impl std::fmt::Debug for RedisConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let masked_sentinel: Vec<String> = self
            .sentinel_addresses
            .iter()
            .map(|url| mask_url_password_for_debug(url))
            .collect();
        f.debug_struct("RedisConfig")
            .field("url", &mask_url_password_for_debug(&self.url))
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

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JwtConfig {
    pub secret: String,
    pub access_token_duration_hours: u64,
    pub refresh_token_duration_days: u64,
    pub guest_token_duration_hours: u64,
    pub clock_skew_leeway_secs: u64,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HlsStorageBackend {
    #[default]
    Memory,
    File,
    SharedFile,
    S3,
}

impl std::str::FromStr for HlsStorageBackend {
    type Err = config::ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "memory" => Ok(Self::Memory),
            "file" => Ok(Self::File),
            "shared_file" => Ok(Self::SharedFile),
            "s3" => Ok(Self::S3),
            _ => Err(config::ConfigError::Message(format!(
                "livestream HLS storage type '{value}' must be one of: memory, file, shared_file, s3"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HlsStorageConfig {
    Memory(HlsMemoryStorageConfig),
    File(HlsFileStorageConfig),
    SharedFile(HlsFileStorageConfig),
    S3(HlsS3Config),
}

impl Default for HlsStorageConfig {
    fn default() -> Self {
        Self::Memory(HlsMemoryStorageConfig::default())
    }
}

impl HlsStorageConfig {
    #[must_use]
    pub fn select_backend(self, backend: HlsStorageBackend) -> Self {
        match (backend, self) {
            (HlsStorageBackend::Memory, Self::Memory(config)) => Self::Memory(config),
            (HlsStorageBackend::File, Self::File(config) | Self::SharedFile(config)) => {
                Self::File(config)
            }
            (HlsStorageBackend::SharedFile, Self::File(config) | Self::SharedFile(config)) => {
                Self::SharedFile(config)
            }
            (HlsStorageBackend::S3, Self::S3(config)) => Self::S3(config),
            (HlsStorageBackend::Memory, _) => Self::Memory(HlsMemoryStorageConfig::default()),
            (HlsStorageBackend::File, _) => Self::File(HlsFileStorageConfig::default()),
            (HlsStorageBackend::SharedFile, _) => Self::SharedFile(HlsFileStorageConfig::default()),
            (HlsStorageBackend::S3, _) => Self::S3(HlsS3Config::default()),
        }
    }

    #[must_use]
    pub const fn backend(&self) -> HlsStorageBackend {
        match self {
            Self::Memory(_) => HlsStorageBackend::Memory,
            Self::File(_) => HlsStorageBackend::File,
            Self::SharedFile(_) => HlsStorageBackend::SharedFile,
            Self::S3(_) => HlsStorageBackend::S3,
        }
    }

    #[must_use]
    pub const fn memory(&self) -> Option<&HlsMemoryStorageConfig> {
        match self {
            Self::Memory(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn file(&self) -> Option<&HlsFileStorageConfig> {
        match self {
            Self::File(config) | Self::SharedFile(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn file_mut(&mut self) -> Option<&mut HlsFileStorageConfig> {
        match self {
            Self::File(config) | Self::SharedFile(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn s3(&self) -> Option<&HlsS3Config> {
        match self {
            Self::S3(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn s3_mut(&mut self) -> Option<&mut HlsS3Config> {
        match self {
            Self::S3(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub fn path(&self) -> &str {
        self.file().map_or("", |config| config.path.as_str())
    }

    #[must_use]
    pub fn memory_max_mb(&self) -> u64 {
        self.memory().map_or(0, |config| config.memory_max_mb)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct HlsMemoryStorageConfig {
    pub memory_max_mb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct HlsFileStorageConfig {
    pub path: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct HlsS3Config {
    pub endpoint: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket: String,
    pub region: Option<String>,
    pub base_path: String,
}

impl Default for HlsS3Config {
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

impl std::fmt::Debug for HlsS3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HlsS3Config")
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
    pub public_rtmp_host: String,
    pub logging: LoggingConfig,
    pub gop_cache_size: u32,
    pub stream_timeout_seconds: u64,
    pub cleanup_check_interval_seconds: u64,
    pub pull_max_retries: u32,
    pub pull_initial_backoff_ms: u64,
    pub pull_max_backoff_ms: u64,
    pub max_flv_tag_size_bytes: usize,
    pub gop_cache_max_memory_mb: u64,
    pub hls_storage: HlsStorageConfig,
    pub flv_max_connection_duration_seconds: u64,
    pub flv_write_timeout_seconds: u64,
}

impl Default for LivestreamConfig {
    fn default() -> Self {
        Self {
            rtmp_port: 1935,
            public_rtmp_host: String::new(),
            logging: LoggingConfig::default(),
            gop_cache_size: 2,
            stream_timeout_seconds: 300,
            cleanup_check_interval_seconds: 60,
            pull_max_retries: 10,
            pull_initial_backoff_ms: 1000,
            pull_max_backoff_ms: 30_000,
            max_flv_tag_size_bytes: 10 * 1024 * 1024,
            gop_cache_max_memory_mb: 100,
            hls_storage: HlsStorageConfig::default(),
            flv_max_connection_duration_seconds: 86400,
            flv_write_timeout_seconds: 30,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FileStorageS3Config {
    pub endpoint: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket: String,
    pub region: String,
    pub base_path: String,
    pub public_base_url: Option<String>,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FileStorageDatabaseCompression {
    None,
    Lz4,
    #[default]
    Zstd,
}

impl std::str::FromStr for FileStorageDatabaseCompression {
    type Err = config::ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "lz4" => Ok(Self::Lz4),
            "zstd" => Ok(Self::Zstd),
            _ => Err(config::ConfigError::Message(format!(
                "database file storage compression '{value}' must be one of: none, lz4, zstd"
            ))),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FileStorageDatabaseConfig {
    pub compression: FileStorageDatabaseCompression,
    pub compression_min_size_bytes: i64,
    pub compression_min_savings_percent: u8,
}

impl Default for FileStorageDatabaseConfig {
    fn default() -> Self {
        Self {
            compression: FileStorageDatabaseCompression::Zstd,
            compression_min_size_bytes: 4096,
            compression_min_savings_percent: 10,
        }
    }
}

impl std::fmt::Debug for FileStorageDatabaseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStorageDatabaseConfig")
            .field("compression", &self.compression)
            .field(
                "compression_min_size_bytes",
                &self.compression_min_size_bytes,
            )
            .field(
                "compression_min_savings_percent",
                &self.compression_min_savings_percent,
            )
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileStorageBackendConfig {
    #[default]
    Disabled,
    Database(FileStorageDatabaseConfig),
    S3(FileStorageS3Config),
}

impl FileStorageBackendConfig {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Database(_) => "database",
            Self::S3(_) => "s3",
        }
    }

    #[must_use]
    pub const fn s3(&self) -> Option<&FileStorageS3Config> {
        match self {
            Self::S3(config) => Some(config),
            _ => None,
        }
    }

    #[must_use]
    pub const fn s3_mut(&mut self) -> Option<&mut FileStorageS3Config> {
        match self {
            Self::S3(config) => Some(config),
            _ => None,
        }
    }
}

impl std::fmt::Debug for FileStorageBackendConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => f
                .debug_struct("FileStorageBackendConfig::Disabled")
                .finish(),
            Self::Database(config) => f
                .debug_struct("FileStorageBackendConfig::Database")
                .field("database", config)
                .finish(),
            Self::S3(config) => f
                .debug_struct("FileStorageBackendConfig::S3")
                .field("s3", config)
                .finish(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FileStorageConfig {
    pub upload_token_secret: String,
    pub default_backend: String,
    pub chat_attachments_backend: String,
    pub user_avatars_backend: String,
    pub media_covers_backend: String,
    pub room_covers_backend: String,
    pub playlist_covers_backend: String,
    pub unreferenced_object_retention_seconds: u64,
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
            .field("backends", &"<redacted>")
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebAuthnConfig {
    pub enabled: bool,
    pub rp_id: String,
    pub rp_origin: String,
    pub rp_name: String,
    pub allowed_origins: Vec<String>,
    pub apple_app_ids: Vec<String>,
    pub android_apps: Vec<AndroidAppAssociationConfig>,
    pub allow_subdomains: bool,
    pub allow_any_port: bool,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AndroidAppAssociationConfig {
    pub package_name: String,
    pub sha256_cert_fingerprints: Vec<String>,
}

impl Default for WebAuthnConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rp_id: String::new(),
            rp_origin: String::new(),
            rp_name: "SyncTV".to_string(),
            allowed_origins: Vec::new(),
            apple_app_ids: Vec::new(),
            android_apps: Vec::new(),
            allow_subdomains: false,
            allow_any_port: false,
            timeout_seconds: 300,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalProviderHttpConfig {
    pub request_timeout_seconds: u64,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MediaProvidersConfig {
    pub alist: LocalProviderHttpConfig,
    pub bilibili: LocalProviderHttpConfig,
    pub emby: LocalProviderHttpConfig,
    pub cloudreve: LocalProviderHttpConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebRTCMode {
    SignalingOnly,
    PeerToPeer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebRTCConfig {
    pub mode: WebRTCMode,
    pub enable_builtin_stun: bool,
    pub stun_port: u16,
    pub stun_host: String,
    pub stun_external_addr: String,
    pub filter_private_ice_candidates: bool,
    pub logging: LoggingConfig,
}

impl Default for WebRTCConfig {
    fn default() -> Self {
        Self {
            mode: WebRTCMode::PeerToPeer,
            enable_builtin_stun: false,
            stun_port: 3478,
            stun_host: "0.0.0.0".to_string(),
            stun_external_addr: String::new(),
            filter_private_ice_candidates: false,
            logging: LoggingConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionLimitsConfig {
    pub max_per_user: usize,
    pub max_per_room: usize,
    pub max_total: usize,
    pub idle_timeout_seconds: u64,
    pub max_duration_seconds: u64,
    pub ws_message_rate_limit_per_second: u32,
}

impl Default for ConnectionLimitsConfig {
    fn default() -> Self {
        Self {
            max_per_user: 20,
            max_per_room: 2000,
            max_total: 100_000,
            idle_timeout_seconds: 300,
            max_duration_seconds: 86400,
            ws_message_rate_limit_per_second: 50,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BootstrapConfig {
    pub create_root_user: bool,
    pub root_username: String,
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
            errors.push("Root password is empty".to_string());
            return errors;
        }
        if pwd == "root" || is_known_dev_secret(pwd, &["Rootpasswd1234567890!", "DevRootPass12345"])
        {
            errors.push("Root password is set to default value 'root'".to_string());
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

impl std::fmt::Display for ClusterDiscoveryMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Redis => "redis",
            Self::Static => "static",
            Self::K8sDns => "k8s_dns",
        };
        f.write_str(value)
    }
}

impl std::str::FromStr for ClusterDiscoveryMode {
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

impl std::fmt::Display for ClusterLeaderElectionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Redis => "redis",
            Self::K8sLease => "k8s_lease",
        };
        f.write_str(value)
    }
}

impl std::str::FromStr for ClusterLeaderElectionMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "redis" => Ok(Self::Redis),
            "k8s_lease" => Ok(Self::K8sLease),
            _ => Err(format!("expected one of: {}", Self::ALLOWED_VALUES)),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClusterChannelConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub advertise_host: String,
    pub advertise_port: u16,
    pub logging: LoggingConfig,
    pub secret: String,
    pub critical_channel_capacity: usize,
    pub publish_channel_capacity: usize,
    pub discovery_mode: ClusterDiscoveryMode,
    pub leader_election_mode: ClusterLeaderElectionMode,
    pub peers: Vec<String>,
    pub catchup_window_secs: u64,
    pub stream_max_length: usize,
}

impl std::fmt::Debug for ClusterChannelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterChannelConfig")
            .field("enabled", &self.enabled)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("advertise_host", &self.advertise_host)
            .field("advertise_port", &self.advertise_port)
            .field("secret", &"<redacted>")
            .field("critical_channel_capacity", &self.critical_channel_capacity)
            .field("publish_channel_capacity", &self.publish_channel_capacity)
            .field("discovery_mode", &self.discovery_mode)
            .field("leader_election_mode", &self.leader_election_mode)
            .field("peers", &self.peers)
            .field("catchup_window_secs", &self.catchup_window_secs)
            .field("stream_max_length", &self.stream_max_length)
            .finish()
    }
}

impl Default for ClusterChannelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "0.0.0.0".to_string(),
            port: 50051,
            advertise_host: String::new(),
            advertise_port: 0,
            logging: LoggingConfig {
                level: "warn".to_string(),
                ..LoggingConfig::default()
            },
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BufferSizesConfig {
    pub websocket_outbound: usize,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    pub l1_capacity: u64,
    pub l1_ttl_seconds: u64,
    pub l2_ttl_seconds: u64,
    pub username_cache_capacity: u64,
    pub username_cache_ttl_seconds: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            l1_capacity: 5000,
            l1_ttl_seconds: 300,
            l2_ttl_seconds: 300,
            username_cache_capacity: 10_000,
            username_cache_ttl_seconds: 3600,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxySliceCacheConfig {
    pub enabled: bool,
    pub slice_size_bytes: usize,
    pub max_cache_size_bytes: u64,
    pub segment_ttl_seconds: u64,
    pub stale_max_age_seconds: u64,
    pub stale_while_revalidate: bool,
    pub file_backend_enabled: bool,
    pub file_cache_dir: String,
    pub eviction_interval_seconds: u64,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestRateLimitConfig {
    pub auth_max_requests: u32,
    pub auth_window_seconds: u64,
    pub write_max_requests: u32,
    pub write_window_seconds: u64,
    pub read_max_requests: u32,
    pub read_window_seconds: u64,
    pub media_max_requests: u32,
    pub media_window_seconds: u64,
    pub admin_max_requests: u32,
    pub admin_window_seconds: u64,
    pub streaming_max_requests: u32,
    pub streaming_window_seconds: u64,
    pub websocket_max_requests: u32,
    pub websocket_window_seconds: u64,
    pub scopes: HashMap<String, RateLimitScopeRule>,
}

impl Default for RequestRateLimitConfig {
    fn default() -> Self {
        Self {
            auth_max_requests: 5,
            auth_window_seconds: 60,
            write_max_requests: 120,
            write_window_seconds: 60,
            read_max_requests: 600,
            read_window_seconds: 60,
            media_max_requests: 120,
            media_window_seconds: 60,
            admin_max_requests: 180,
            admin_window_seconds: 60,
            streaming_max_requests: 1200,
            streaming_window_seconds: 60,
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
    pub max_requests: Option<u32>,
    pub window_seconds: Option<u64>,
    pub strategy: RateLimitScopeStrategy,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub logging: LoggingConfig,
    pub server: ServerConfig,
    pub health: HealthConfig,
    pub time: TimeConfig,
    pub security: SecurityConfig,
    pub data_dir: String,
    pub metrics: MetricsConfig,
    pub management: ManagementConfig,
    pub database: DatabaseConfig,
    pub redis: RedisConfig,
    pub jwt: JwtConfig,
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

impl std::fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppConfig")
            .field("logging", &self.logging)
            .field("server", &self.server)
            .field("health", &self.health)
            .field("time", &self.time)
            .field("security", &"<redacted>")
            .field("data_dir", &self.data_dir)
            .field("metrics", &self.metrics)
            .field("management", &self.management)
            .field("database", &"<redacted>")
            .field("redis", &self.redis)
            .field("jwt", &"<redacted>")
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

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            logging: LoggingConfig::default(),
            server: ServerConfig::default(),
            health: HealthConfig::default(),
            time: TimeConfig::default(),
            security: SecurityConfig::default(),
            data_dir: default_data_dir().display().to_string(),
            metrics: MetricsConfig::default(),
            management: ManagementConfig::default(),
            database: DatabaseConfig::default(),
            redis: RedisConfig::default(),
            jwt: JwtConfig::default(),
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

#[cfg(test)]
mod tests {
    use super::WebRTCConfig;

    #[test]
    fn default_webrtc_config_disables_builtin_stun() {
        assert!(!WebRTCConfig::default().enable_builtin_stun);
    }
}
