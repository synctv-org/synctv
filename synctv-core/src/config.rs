use config::{Config as ConfigBuilder, ConfigError, Environment, File};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Application configuration
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub redis: RedisConfig,
    pub jwt: JwtConfig,
    pub logging: LoggingConfig,
    pub livestream: LivestreamConfig,
    pub oauth2: OAuth2Config,
    pub email: EmailConfig,
    pub media_providers: MediaProvidersConfig,
    pub webrtc: WebRTCConfig,
    pub connection_limits: ConnectionLimitsConfig,
    pub bootstrap: BootstrapConfig,
    pub cluster: ClusterChannelConfig,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("server", &self.server)
            .field("database", &"<redacted>")
            .field("redis", &self.redis)
            .field("jwt", &"<redacted>")
            .field("logging", &self.logging)
            .field("livestream", &self.livestream)
            .field("oauth2", &self.oauth2)
            .field("email", &"<redacted>")
            .field("media_providers", &self.media_providers)
            .field("webrtc", &self.webrtc)
            .field("connection_limits", &self.connection_limits)
            .field("bootstrap", &"<redacted>")
            .field("cluster", &self.cluster)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub grpc_port: u16,
    pub http_port: u16,
    pub enable_reflection: bool,
    /// Development mode enables relaxed security checks for local development.
    /// WARNING: Never enable in production!
    pub development_mode: bool,
    /// Enable the `/metrics` Prometheus endpoint.
    /// Independent of `development_mode` so metrics can be scraped in production.
    /// Defaults to `false`. In Kubernetes, set via Helm `metrics.enabled`.
    pub metrics_enabled: bool,
    /// Trusted proxy IP addresses/CIDRs for X-Forwarded-For validation.
    /// When set, X-Forwarded-For/X-Real-IP headers are only trusted from these addresses.
    /// Example: ["10.0.0.0/8", "192.168.0.0/16"] for internal load balancers.
    /// If empty, X-Forwarded-For headers are NOT trusted (socket address is used).
    pub trusted_proxies: Vec<String>,
    /// CORS allowed origins. In development mode, all origins are allowed.
    /// In production, this should be set to specific domains.
    /// Example: ["<https://app.example.com>", "<https://admin.example.com>"]
    pub cors_allowed_origins: Vec<String>,
    /// Shared secret for authenticating cluster gRPC calls between nodes.
    /// When set, all inter-node gRPC requests must include this secret in the
    /// `x-cluster-secret` metadata header. If empty, cluster endpoints are disabled.
    pub cluster_secret: String,
    /// Advertise host for cluster node registration.
    /// This is the address other nodes use to reach this instance.
    /// Reads from SYNCTV_SERVER_ADVERTISE_HOST env var. In Kubernetes, set this
    /// to the pod IP via the downward API (status.podIP).
    /// If empty, falls back to POD_IP env var, then to the system hostname.
    pub advertise_host: String,
    /// Maximum time in seconds to wait for active connections to drain during shutdown.
    /// Defaults to 30 seconds. Increase for deployments with many long-lived connections.
    pub shutdown_drain_timeout_seconds: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            grpc_port: 50051,
            http_port: 8080,
            enable_reflection: true,
            development_mode: false,
            metrics_enabled: false,
            trusted_proxies: Vec::new(),
            cors_allowed_origins: Vec::new(),
            cluster_secret: String::new(),
            advertise_host: String::new(),
            shutdown_drain_timeout_seconds: 30,
        }
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
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub connect_timeout_seconds: u64,
    pub idle_timeout_seconds: u64,
}

impl std::fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Mask password in database URL if present
        let masked_url = if let Some(at_pos) = self.url.find('@') {
            if let Some(colon_pos) = self.url[..at_pos].rfind(':') {
                let scheme_end = self.url.find("://").map(|p| p + 3).unwrap_or(0);
                if colon_pos > scheme_end {
                    // Has password - mask it
                    format!("{}:****@{}", &self.url[..colon_pos], &self.url[at_pos + 1..])
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
            .field("max_connections", &self.max_connections)
            .field("min_connections", &self.min_connections)
            .field("connect_timeout_seconds", &self.connect_timeout_seconds)
            .field("idle_timeout_seconds", &self.idle_timeout_seconds)
            .finish()
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "postgresql://synctv:synctv@localhost:5432/synctv".to_string(),
            max_connections: 20,
            min_connections: 5,
            connect_timeout_seconds: 10,
            idle_timeout_seconds: 600,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RedisConfig {
    pub url: String,
    pub pool_size: u32,
    pub connect_timeout_seconds: u64,
    pub key_prefix: String,
}

impl std::fmt::Debug for RedisConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Mask password in Redis URL if present (redis://:password@host or redis://user:password@host)
        let masked_url = if self.url.contains('@') {
            if let Some(at_pos) = self.url.find('@') {
                if let Some(colon_pos) = self.url[..at_pos].rfind(':') {
                    let scheme_end = self.url.find("://").map(|p| p + 3).unwrap_or(0);
                    if colon_pos >= scheme_end && colon_pos < at_pos {
                        // Has password - mask it
                        format!("{}:****@{}", &self.url[..colon_pos], &self.url[at_pos + 1..])
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

        f.debug_struct("RedisConfig")
            .field("url", &masked_url)
            .field("pool_size", &self.pool_size)
            .field("connect_timeout_seconds", &self.connect_timeout_seconds)
            .field("key_prefix", &self.key_prefix)
            .finish()
    }
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            url: "redis://localhost:6379".to_string(),
            pool_size: 10,
            connect_timeout_seconds: 5,
            key_prefix: "synctv:".to_string(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
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
            .field("access_token_duration_hours", &self.access_token_duration_hours)
            .field("refresh_token_duration_days", &self.refresh_token_duration_days)
            .field("guest_token_duration_hours", &self.guest_token_duration_hours)
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
pub struct LoggingConfig {
    pub level: String,
    pub format: String, // "json" or "pretty"
    pub file_path: Option<String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: "pretty".to_string(),
            file_path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LivestreamConfig {
    pub rtmp_port: u16,
    pub max_streams: u32,
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
}

impl Default for LivestreamConfig {
    fn default() -> Self {
        Self {
            rtmp_port: 1935,
            max_streams: 50,
            gop_cache_size: 2,
            stream_timeout_seconds: 300,
            cleanup_check_interval_seconds: 60,
            pull_max_retries: 10,
            pull_initial_backoff_ms: 1000,
            pull_max_backoff_ms: 30_000,
            max_flv_tag_size_bytes: 10 * 1024 * 1024,
            gop_cache_max_memory_mb: 100,
        }
    }
}

/// `OAuth2` configuration
///
/// Stores `OAuth2` provider configurations in the main config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth2Config {
    /// Provider configurations (e.g., github, google, logto1, logto2)
    #[serde(default)]
    pub providers: serde_json::Value,
    /// URL scheme for OAuth2 redirect URLs.
    /// Supported values: "http", "https"
    /// Default: "http" for backward compatibility.
    /// When behind a reverse proxy terminating TLS, set this to "https".
    #[serde(default = "default_redirect_scheme")]
    pub redirect_scheme: String,
}

fn default_redirect_scheme() -> String {
    "http".to_string()
}

impl Default for OAuth2Config {
    fn default() -> Self {
        Self {
            providers: serde_json::json!({}),
            redirect_scheme: default_redirect_scheme(),
        }
    }
}

/// Media providers configuration
///
/// Stores media provider configurations (Alist, Emby, Bilibili, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaProvidersConfig {
    /// Provider configurations (e.g., alist, emby, jellyfin, bilibili)
    #[serde(default)]
    pub providers: serde_json::Value,
}

impl Default for MediaProvidersConfig {
    fn default() -> Self {
        Self {
            providers: serde_json::json!({}),
        }
    }
}

/// WebRTC configuration for audio/video calls
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebRTCConfig {
    /// WebRTC operation mode
    pub mode: WebRTCMode,

    // STUN Configuration
    /// Enable built-in STUN server (powered by turn-rs)
    pub enable_builtin_stun: bool,
    /// STUN server port
    pub stun_port: u16,
    /// STUN server bind host
    pub stun_host: String,
    /// STUN server external address for reflexive candidates.
    /// In K8s/NAT environments, set this to the routable address
    /// (e.g., pod IP or service IP). If empty, falls back to
    /// advertise_host:stun_port.
    pub stun_external_addr: String,

    // SFU Configuration (for large rooms)
    /// Room size threshold to switch to SFU mode (only for Hybrid mode)
    pub sfu_threshold: usize,
    /// Enable Simulcast (multiple quality layers)
    pub enable_simulcast: bool,
    /// Maximum concurrent SFU rooms (0 = unlimited)
    pub max_sfu_rooms: usize,
    /// Maximum peers per SFU room
    pub max_peers_per_sfu_room: usize,
    /// Simulcast layers to use (e.g., ["high", "medium", "low"])
    pub simulcast_layers: Vec<String>,
    /// Maximum bitrate per peer in kbps (0 = unlimited)
    pub max_bitrate_per_peer: u32,
    /// Enable bandwidth estimation
    pub enable_bandwidth_estimation: bool,
}

/// WebRTC operation mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebRTCMode {
    /// Pure P2P mode (zero server cost)
    /// - Signaling only, no STUN/TURN/SFU
    /// - Best for: personal deployments
    /// - Connection success rate: ~70-75%
    SignalingOnly,

    /// P2P with STUN/TURN support (recommended for most deployments)
    /// - P2P connections with NAT traversal
    /// - STUN for reflexive candidates
    /// - TURN fallback for difficult NAT scenarios
    /// - Best for: small to medium deployments
    /// - Connection success rate: ~99%
    PeerToPeer,

    /// Hybrid mode (P2P for small rooms, SFU for large rooms)
    /// - Automatically switches based on room size
    /// - P2P for rooms < threshold
    /// - SFU for rooms >= threshold
    /// - Best for: flexible deployments with mixed room sizes
    /// - Optimal balance of cost and performance
    Hybrid,

    /// Pure SFU mode (enterprise grade)
    /// - All rooms use SFU regardless of size
    /// - Server receives and forwards all media streams
    /// - Best for: large scale deployments, recording, monitoring
    /// - Highest server cost, best quality and reliability
    #[serde(rename = "sfu")]
    SFU,
}

impl Default for WebRTCConfig {
    fn default() -> Self {
        Self {
            // Default to Hybrid mode (balanced)
            mode: WebRTCMode::Hybrid,

            // STUN enabled by default (powered by turn-rs)
            enable_builtin_stun: true,
            stun_port: 3478,
            stun_host: "0.0.0.0".to_string(),
            stun_external_addr: String::new(),

            // SFU configuration
            sfu_threshold: 5, // Switch to SFU for 5+ participants
            enable_simulcast: true,
            max_sfu_rooms: 0, // No limit by default
            max_peers_per_sfu_room: 50,
            simulcast_layers: vec![
                "high".to_string(),
                "medium".to_string(),
                "low".to_string(),
            ],
            max_bitrate_per_peer: 0, // No limit by default
            enable_bandwidth_estimation: true,
        }
    }
}

/// Email configuration for SMTP
#[derive(Clone, Serialize, Deserialize)]
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
        let mut builder = ConfigBuilder::builder();

        // Load config file if provided
        if let Some(path) = config_file {
            if Path::new(path).exists() {
                builder = builder.add_source(File::new(path, config::FileFormat::Yaml));
            }
        }

        // Override with environment variables (SYNCTV_JWT_SECRET, SYNCTV_DATABASE_URL, etc.)
        builder = builder.add_source(
            Environment::with_prefix("SYNCTV")
                .separator("_")
                .try_parsing(true),
        );

        let config = builder.build()?;
        config.try_deserialize()
    }

    /// Load from environment variables only (for Docker/K8s)
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::load(None)
    }

    /// Load from file path
    pub fn from_file(path: &str) -> Result<Self, ConfigError> {
        Self::load(Some(path))
    }

    /// Get database URL
    #[must_use] 
    pub fn database_url(&self) -> &str {
        &self.database.url
    }

    /// Get Redis URL
    #[must_use] 
    pub fn redis_url(&self) -> &str {
        &self.redis.url
    }

    /// Get gRPC address
    #[must_use] 
    pub fn grpc_address(&self) -> String {
        format!("{}:{}", self.server.host, self.server.grpc_port)
    }

    /// Get HTTP address
    #[must_use]
    pub fn http_address(&self) -> String {
        format!("{}:{}", self.server.host, self.server.http_port)
    }

    /// Resolve the advertise host for cluster node registration.
    ///
    /// Priority: `server.advertise_host` config > `POD_IP` env var > system hostname.
    /// This address must be routable from other nodes (never `0.0.0.0`).
    #[must_use]
    pub fn advertise_host(&self) -> String {
        // 1. Explicit config value (set via SYNCTV_SERVER_ADVERTISE_HOST)
        if !self.server.advertise_host.is_empty() {
            return self.server.advertise_host.clone();
        }

        // 2. POD_IP env var (set by Kubernetes downward API)
        if let Ok(pod_ip) = std::env::var("POD_IP") {
            if !pod_ip.is_empty() {
                return pod_ip;
            }
        }

        // 3. System hostname
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| self.server.host.clone())
    }

    /// Get the gRPC address advertised to other cluster nodes.
    #[must_use]
    pub fn advertise_grpc_address(&self) -> String {
        format!("{}:{}", self.advertise_host(), self.server.grpc_port)
    }

    /// Get the HTTP address advertised to other cluster nodes.
    #[must_use]
    pub fn advertise_http_address(&self) -> String {
        format!("{}:{}", self.advertise_host(), self.server.http_port)
    }

    /// Validate configuration at startup (fail fast on misconfigurations)
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        // Validate port numbers are in valid range (1-65535)
        let ports_to_check: &[(&str, u16)] = &[
            ("server.http_port", self.server.http_port),
            ("server.grpc_port", self.server.grpc_port),
            ("livestream.rtmp_port", self.livestream.rtmp_port),
        ];
        for (name, port) in ports_to_check {
            if *port == 0 {
                errors.push(format!("{name} must be between 1 and 65535, got 0"));
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
        if self.database.url.is_empty() {
            errors.push("database.url must not be empty".to_string());
        }

        // H-01: Dev mode guard - prevent development_mode on non-localhost addresses
        if self.server.development_mode {
            let host = self.server.host.as_str();
            let is_localhost = matches!(host, "127.0.0.1" | "localhost" | "::1");
            if !is_localhost {
                tracing::warn!(
                    "development_mode=true with non-localhost host '{}'. \
                     This is dangerous in production! Only bind to 127.0.0.1/localhost/::1 in dev mode.",
                    host
                );
                // 0.0.0.0 is commonly used in containers even for dev, so warn but don't error
                if host != "0.0.0.0" && host != "::" {
                    errors.push(format!(
                        "development_mode=true with non-localhost host '{}'. \
                         Set host to 127.0.0.1/localhost/::1 or disable development_mode",
                        host
                    ));
                }
            }
        }

        // Validate JWT secret (warn if using default)
        if self.jwt.secret.is_empty() {
            errors.push("JWT secret is empty".to_string());
        } else if self.jwt.secret == "change-me-in-production" {
            if self.server.development_mode {
                // Allow default secret in dev mode (just log a warning at startup)
                tracing::warn!("Using default JWT secret in development mode - do NOT use in production");
            } else {
                errors.push("JWT secret is set to default value 'change-me-in-production'. Set SYNCTV_JWT_SECRET environment variable or server.development_mode=true for local development".to_string());
            }
        }

        // Validate root credentials (only in production mode)
        if !self.server.development_mode
            && self.bootstrap.create_root_user {
                if self.bootstrap.root_password == "root" {
                    errors.push("Root password is set to default value 'root'. Set SYNCTV_BOOTSTRAP_ROOT_PASSWORD environment variable or server.development_mode=true for local development".to_string());
                }
                if self.bootstrap.root_username.len() < 3 {
                    errors.push("Root username must be at least 3 characters".to_string());
                }
                // H-02: Enforce 12-char minimum and complexity for root password in production
                let pwd = &self.bootstrap.root_password;
                if pwd.len() < 12 {
                    errors.push("Root password must be at least 12 characters in production mode".to_string());
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
            }

        // Validate port conflicts: RTMP != HTTP != gRPC (all three must differ)
        if self.server.grpc_port == self.server.http_port {
            errors.push(format!(
                "server.grpc_port ({}) and server.http_port ({}) must be different",
                self.server.grpc_port, self.server.http_port
            ));
        }
        if self.livestream.rtmp_port == self.server.http_port {
            errors.push(format!(
                "livestream.rtmp_port ({}) and server.http_port ({}) must be different",
                self.livestream.rtmp_port, self.server.http_port
            ));
        }
        if self.livestream.rtmp_port == self.server.grpc_port {
            errors.push(format!(
                "livestream.rtmp_port ({}) and server.grpc_port ({}) must be different",
                self.livestream.rtmp_port, self.server.grpc_port
            ));
        }

        // Validate trusted_proxies CIDR format
        for (i, proxy) in self.server.trusted_proxies.iter().enumerate() {
            // Each entry must be a valid CIDR notation or IP address
            if proxy.parse::<ipnet::IpNet>().is_err()
                && proxy.parse::<std::net::IpAddr>().is_err()
            {
                errors.push(format!(
                    "server.trusted_proxies[{i}] '{proxy}' is not a valid CIDR or IP address"
                ));
            }
        }

        // Warn about missing Redis in production (security features degrade)
        if !self.server.development_mode && self.redis.url.is_empty() {
            tracing::warn!("Redis is not configured in production mode — token blacklist and rate limiting will be DISABLED");
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

        // Validate livestream config
        if self.livestream.stream_timeout_seconds == 0 {
            errors.push("livestream.stream_timeout_seconds must be greater than 0".to_string());
        }
        if self.livestream.cleanup_check_interval_seconds == 0 {
            errors.push("livestream.cleanup_check_interval_seconds must be greater than 0".to_string());
        }

        // Validate email config (only when SMTP is configured)
        if !self.email.smtp_host.is_empty() {
            if self.email.smtp_port == 0 {
                errors.push("email.smtp_port must be between 1 and 65535 when smtp_host is set".to_string());
            }
            if self.email.from_email.is_empty() {
                errors.push("email.from_email must be set when smtp_host is configured".to_string());
            } else if !self.email.from_email.contains('@') || self.email.from_email.starts_with('@') || self.email.from_email.ends_with('@') {
                errors.push(format!(
                    "email.from_email '{}' is not a valid email address",
                    self.email.from_email
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
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
}

impl Default for ConnectionLimitsConfig {
    fn default() -> Self {
        Self {
            max_per_user: 5,
            max_per_room: 200,
            max_total: 10000,
            idle_timeout_seconds: 300, // 5 minutes
            max_duration_seconds: 86400, // 24 hours
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
            create_root_user: true,
            root_username: "root".to_string(),
            root_password: "root".to_string(),
        }
    }
}

/// Cluster channel capacity configuration
///
/// Controls the buffer sizes for internal channels used in cluster communication.
/// Larger values provide more resilience during traffic spikes but use more memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClusterChannelConfig {
    /// Capacity for the high-priority critical event channel.
    /// Critical events (KickPublisher, KickUser, PermissionChanged) are never dropped;
    /// when this channel is full, senders block until space is available.
    /// Default: 1000
    pub critical_channel_capacity: usize,

    /// Capacity for the normal-priority Redis publish channel.
    /// Normal events are dropped with a warning when this channel is full
    /// (e.g., during a prolonged Redis outage).
    /// Default: 10000
    pub publish_channel_capacity: usize,

    /// Discovery mode for cluster node registration.
    /// - "redis": Use Redis-based node registry (default, works everywhere)
    /// - "k8s_dns": Use Kubernetes headless service DNS for peer discovery
    ///   (requires HEADLESS_SERVICE_NAME and POD_NAMESPACE env vars)
    pub discovery_mode: String,
}

impl Default for ClusterChannelConfig {
    fn default() -> Self {
        Self {
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            discovery_mode: "redis".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::from_env().unwrap_or_else(|_| Config {
            server: ServerConfig::default(),
            database: DatabaseConfig::default(),
            redis: RedisConfig::default(),
            jwt: JwtConfig::default(),
            logging: LoggingConfig::default(),
            livestream: LivestreamConfig::default(),
            oauth2: OAuth2Config::default(),
            email: EmailConfig::default(),
            media_providers: MediaProvidersConfig::default(),
            webrtc: WebRTCConfig::default(),
            connection_limits: ConnectionLimitsConfig::default(),
            bootstrap: BootstrapConfig::default(),
            cluster: ClusterChannelConfig::default(),
        });

        assert!(!config.database_url().is_empty());
        assert!(!config.redis_url().is_empty());
        assert!(config.server.grpc_port > 0);
        assert!(config.server.http_port > 0);
        assert!(config.webrtc.enable_builtin_stun);
        assert!(config.bootstrap.create_root_user);
    }

    #[test]
    fn test_grpc_address() {
        let config = Config {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                grpc_port: 50051,
                http_port: 8080,
                enable_reflection: true,
                development_mode: false,
                metrics_enabled: false,
                trusted_proxies: Vec::new(),
                cors_allowed_origins: Vec::new(),
                cluster_secret: String::new(),
                advertise_host: String::new(),
                shutdown_drain_timeout_seconds: 30,
            },
            database: DatabaseConfig::default(),
            redis: RedisConfig::default(),
            jwt: JwtConfig::default(),
            logging: LoggingConfig::default(),
            livestream: LivestreamConfig::default(),
            oauth2: OAuth2Config::default(),
            email: EmailConfig::default(),
            media_providers: MediaProvidersConfig::default(),
            webrtc: WebRTCConfig::default(),
            connection_limits: ConnectionLimitsConfig::default(),
            bootstrap: BootstrapConfig::default(),
            cluster: ClusterChannelConfig::default(),
        };

        assert_eq!(config.grpc_address(), "127.0.0.1:50051");
        assert_eq!(config.http_address(), "127.0.0.1:8080");
    }

    /// Helper to create a valid production config for validation tests
    fn valid_prod_config() -> Config {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                grpc_port: 50051,
                http_port: 8080,
                development_mode: false,
                metrics_enabled: false,
                enable_reflection: false,
                trusted_proxies: Vec::new(),
                cors_allowed_origins: Vec::new(),
                cluster_secret: String::new(),
                advertise_host: String::new(),
                shutdown_drain_timeout_seconds: 30,
            },
            database: DatabaseConfig::default(),
            redis: RedisConfig::default(),
            jwt: JwtConfig {
                secret: "my-very-secret-production-key-that-is-long-enough".to_string(),
                ..JwtConfig::default()
            },
            logging: LoggingConfig::default(),
            livestream: LivestreamConfig::default(),
            oauth2: OAuth2Config::default(),
            email: EmailConfig::default(),
            media_providers: MediaProvidersConfig::default(),
            webrtc: WebRTCConfig::default(),
            connection_limits: ConnectionLimitsConfig::default(),
            bootstrap: BootstrapConfig {
                create_root_user: true,
                root_username: "admin".to_string(),
                root_password: "StrongPwd12345!".to_string(),
            },
            cluster: ClusterChannelConfig::default(),
        }
    }

    #[test]
    fn test_validate_valid_production_config() {
        let config = valid_prod_config();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_port_conflict_grpc_http() {
        let mut config = valid_prod_config();
        config.server.grpc_port = 8080;
        config.server.http_port = 8080;
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("grpc_port") && e.contains("http_port")));
    }

    #[test]
    fn test_validate_port_conflict_rtmp_http() {
        let mut config = valid_prod_config();
        config.livestream.rtmp_port = 8080;
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("rtmp_port") && e.contains("http_port")));
    }

    #[test]
    fn test_validate_port_conflict_rtmp_grpc() {
        let mut config = valid_prod_config();
        config.livestream.rtmp_port = 50051;
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("rtmp_port") && e.contains("grpc_port")));
    }

    #[test]
    fn test_validate_zero_port() {
        let mut config = valid_prod_config();
        config.server.http_port = 0;
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("http_port") && e.contains("0")));
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
    fn test_validate_default_jwt_secret_dev_mode_ok() {
        let mut config = valid_prod_config();
        config.server.development_mode = true;
        config.server.host = "127.0.0.1".to_string();
        config.jwt.secret = "change-me-in-production".to_string();
        // dev mode relaxes root password requirements, so use default
        config.bootstrap.create_root_user = false;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_default_root_password_production() {
        let mut config = valid_prod_config();
        config.bootstrap.root_password = "root".to_string();
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("Root password") && e.contains("default")));
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
        assert!(errors.iter().any(|e| e.contains("Root username") && e.contains("3")));
    }

    #[test]
    fn test_validate_db_pool_min_exceeds_max() {
        let mut config = valid_prod_config();
        config.database.min_connections = 30;
        config.database.max_connections = 10;
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("min_connections") && e.contains("max_connections")));
    }

    #[test]
    fn test_validate_db_pool_max_zero() {
        let mut config = valid_prod_config();
        config.database.max_connections = 0;
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("max_connections") && e.contains("greater than 0")));
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
        assert!(errors.iter().any(|e| e.contains("from_email") && e.contains("not a valid")));
    }

    #[test]
    fn test_validate_dev_mode_non_localhost_host() {
        let mut config = valid_prod_config();
        config.server.development_mode = true;
        config.server.host = "192.168.1.100".to_string();
        config.bootstrap.create_root_user = false;
        config.jwt.secret = "change-me-in-production".to_string();
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("development_mode") && e.contains("non-localhost")));
    }

    #[test]
    fn test_validate_dev_mode_0000_warns_but_no_error() {
        let mut config = valid_prod_config();
        config.server.development_mode = true;
        config.server.host = "0.0.0.0".to_string();
        config.bootstrap.create_root_user = false;
        config.jwt.secret = "change-me-in-production".to_string();
        // 0.0.0.0 should warn but NOT error (common in containers)
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_dev_mode_localhost_ok() {
        let mut config = valid_prod_config();
        config.server.development_mode = true;
        config.server.host = "127.0.0.1".to_string();
        config.bootstrap.create_root_user = false;
        config.jwt.secret = "change-me-in-production".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_livestream_zero_timeout() {
        let mut config = valid_prod_config();
        config.livestream.stream_timeout_seconds = 0;
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("stream_timeout_seconds")));
    }
}
