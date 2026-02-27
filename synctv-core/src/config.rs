use config::{Config as ConfigBuilder, ConfigError, File};
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
    pub password_complexity: PasswordComplexityConfig,
    pub buffer_sizes: BufferSizesConfig,
    pub cache: CacheConfig,
    pub http_rate_limits: HttpRateLimitConfig,
    pub grpc_rate_limits: GrpcRateLimitConfig,
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
            .field("password_complexity", &self.password_complexity)
            .field("buffer_sizes", &self.buffer_sizes)
            .field("cache", &self.cache)
            .field("http_rate_limits", &self.http_rate_limits)
            .field("grpc_rate_limits", &self.grpc_rate_limits)
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
    /// Enable the `/metrics` Prometheus endpoint.
    /// Defaults to `false`. In Kubernetes, set via Helm `metrics.enabled`.
    pub metrics_enabled: bool,
    /// Trusted proxy IP addresses/CIDRs for X-Forwarded-For validation.
    /// When set, X-Forwarded-For/X-Real-IP headers are only trusted from these addresses.
    /// Example: ["10.0.0.0/8", "192.168.0.0/16"] for internal load balancers.
    /// If empty, X-Forwarded-For headers are NOT trusted (socket address is used).
    pub trusted_proxies: Vec<String>,
    /// CORS allowed origins. Must be set to specific domains.
    /// Example: ["<https://app.example.com>", "<https://admin.example.com>"]
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
    /// Disable the legacy `?token=<jwt>` WebSocket query parameter authentication.
    /// When true, only Authorization header and `?ticket=` are accepted for WebSocket auth.
    /// The `?token=` method is less secure because JWT tokens appear in server logs,
    /// browser history, and Referer headers. Defaults to true (disabled) for security;
    /// set to false only if you need backward compatibility with legacy clients.
    pub disable_ws_token_query: bool,
    /// Bearer token required to access the `/metrics` Prometheus endpoint.
    /// When set (non-empty), requests to `/metrics` must include an
    /// `Authorization: Bearer <token>` header with this value.
    /// When empty (default), the endpoint is open to anyone who can reach it —
    /// only enable metrics in this mode when the endpoint is network-restricted
    /// (e.g. inside a Kubernetes cluster with no external ingress).
    /// Set via `SYNCTV_SERVER_METRICS_BEARER_TOKEN` env var or config file.
    pub metrics_bearer_token: String,
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
            grpc_port: 50051,
            http_port: 8080,
            enable_reflection: true,
            metrics_enabled: false,
            trusted_proxies: Vec::new(),
            cors_allowed_origins: Vec::new(),
            cluster_secret: String::new(),
            advertise_host: String::new(),
            shutdown_drain_timeout_seconds: 30,
            disable_ws_token_query: true,
            metrics_bearer_token: String::new(),
            grpc_max_message_size_bytes: 16 * 1024 * 1024, // 16 MB default
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
            .field("max_lifetime_seconds", &self.max_lifetime_seconds)
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
            max_lifetime_seconds: 1800, // 30 minutes
        }
    }
}

/// Redis deployment mode
///
/// # Supported modes
///
/// - **Standalone** (default): Single Redis instance. Works with all features.
/// - **Sentinel**: Redis Sentinel for high availability. Automatic master
///   failover is NOT yet supported; a restart is required after failover.
///   A proper `SentinelClient` integration is planned.
///
/// # Why Redis Cluster is not supported
///
/// Redis Cluster mode is defined here for forward compatibility but is
/// **not currently supported** at runtime. Attempting to use it will fail
/// at startup with an error. The reasons are:
///
/// 1. **`ConnectionManager` does not support cluster-aware routing.**
///    The `redis` crate's `ConnectionManager` (used throughout for
///    multiplexed async connections) connects to a single Redis node.
///    Redis Cluster requires a `ClusterClient` that maintains connections
///    to all shard nodes and routes commands by key hash slot.
///
/// 2. **Lua scripts use multi-key operations.** The node registry and
///    leader election use Lua scripts that touch multiple Redis keys
///    (e.g., SCAN + GET). In Redis Cluster, all keys accessed in a
///    single script invocation must hash to the same slot. Refactoring
///    to use hash tags (`{prefix}:key`) is required.
///
/// 3. **Pub/Sub semantics differ.** In Redis Cluster, PUBLISH is
///    broadcast to all nodes, but SUBSCRIBE only receives messages
///    published on the connected node's shard. The current pub/sub
///    architecture assumes single-node semantics.
///
/// # Future plan
///
/// Redis Cluster support requires:
/// - Migrate from `ConnectionManager` to `ClusterConnection`
/// - Add hash tags to all Redis keys so related keys land on the same slot
/// - Test Pub/Sub behavior under cluster mode (may need sharded pub/sub
///   from Redis 7.0+)
/// - Validate Lua scripts work within single-slot constraints
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RedisDeploymentMode {
    #[default]
    Standalone,
    Sentinel,
    Cluster,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RedisConfig {
    pub url: String,
    pub connect_timeout_seconds: u64,
    pub key_prefix: String,
    /// Deployment mode: standalone (default), sentinel, or cluster
    pub deployment_mode: RedisDeploymentMode,
    /// Sentinel master name (required for sentinel mode)
    pub sentinel_master_name: Option<String>,
    /// Sentinel node addresses (required for sentinel mode)
    pub sentinel_addresses: Vec<String>,
    /// Cluster node addresses (required for cluster mode)
    pub cluster_nodes: Vec<String>,
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

        let masked_sentinel: Vec<String> = self.sentinel_addresses.iter().map(|u| mask_url(u)).collect();
        let masked_cluster: Vec<String> = self.cluster_nodes.iter().map(|u| mask_url(u)).collect();

        f.debug_struct("RedisConfig")
            .field("url", &masked_url)
            .field("connect_timeout_seconds", &self.connect_timeout_seconds)
            .field("key_prefix", &self.key_prefix)
            .field("deployment_mode", &self.deployment_mode)
            .field("sentinel_master_name", &self.sentinel_master_name)
            .field("sentinel_addresses", &masked_sentinel)
            .field("cluster_nodes", &masked_cluster)
            .finish()
    }
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            connect_timeout_seconds: 5,
            key_prefix: "synctv:".to_string(),
            deployment_mode: RedisDeploymentMode::Standalone,
            sentinel_master_name: None,
            sentinel_addresses: Vec::new(),
            cluster_nodes: Vec::new(),
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
    /// Whether HLS segment storage is on shared storage accessible by all replicas.
    ///
    /// In cluster mode (cluster.enabled=true or `cluster_secret` is set), HLS segments
    /// must be accessible from any replica. If this is false and cluster mode is
    /// enabled, a warning is logged at startup. Set to true when using NFS, shared
    /// volume mounts, or S3-compatible object storage for HLS segments.
    ///
    /// Default: false (local storage, single-node safe).
    pub hls_shared_storage: bool,
    /// Base path for HLS segment storage.
    ///
    /// Used for validation: paths that are obviously local-only (e.g. /tmp/)
    /// trigger a stronger warning in cluster mode even when `hls_shared_storage=true`.
    /// If empty, the default in-memory storage is used.
    pub hls_storage_path: String,
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
            gop_cache_size: 2,
            stream_timeout_seconds: 300,
            cleanup_check_interval_seconds: 60,
            pull_max_retries: 10,
            pull_initial_backoff_ms: 1000,
            pull_max_backoff_ms: 30_000,
            max_flv_tag_size_bytes: 10 * 1024 * 1024,
            gop_cache_max_memory_mb: 100,
            hls_memory_max_mb: 0,
            hls_shared_storage: false,
            hls_storage_path: String::new(),
            flv_max_connection_duration_seconds: 86400, // 24 hours
            flv_write_timeout_seconds: 30,
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
    /// URL scheme for `OAuth2` redirect URLs.
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
#[serde(default)]
pub struct MediaProvidersConfig {
    /// Provider configurations (e.g., alist, emby, jellyfin, bilibili)
    #[serde(default)]
    pub providers: serde_json::Value,

    /// **Production Enhancement (#23)**: Timeout for external provider HTTP requests (seconds)
    /// Prevents indefinite hanging on slow/unresponsive provider APIs (Bilibili, Alist, Emby).
    /// Default: 30 seconds. Set to 0 to disable timeout (not recommended in production).
    pub request_timeout_seconds: u64,

    /// **Production Enhancement (#23)**: Connection timeout for provider API connections (seconds)
    /// Limits time spent establishing TCP connections to external providers.
    /// Default: 10 seconds.
    pub connect_timeout_seconds: u64,
}

impl Default for MediaProvidersConfig {
    fn default() -> Self {
        Self {
            providers: serde_json::json!({}),
            request_timeout_seconds: 30,
            connect_timeout_seconds: 10,
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
    /// `advertise_host:stun_port`.
    pub stun_external_addr: String,

    // TURN Configuration
    /// TURN shared secret for generating time-limited HMAC-SHA1 credentials.
    /// When set, the server generates ephemeral credentials compatible with
    /// coturn's `use-auth-secret` mode (RFC 5389 long-term credentials).
    /// If empty, TURN credentials are taken from the dynamic settings registry as-is.
    pub turn_shared_secret: String,
    /// TURN server URLs to include in ICE server responses when `turn_shared_secret`
    /// is set. Example: `["turn:turn.example.com:3478", "turns:turn.example.com:5349"]`
    /// Ignored when `turn_shared_secret` is empty.
    pub turn_server_urls: Vec<String>,
    /// TURN credential time-to-live in seconds.
    /// After this duration, clients must refresh their ICE servers.
    /// Default: 86400 (24 hours). Set to 0 for credentials that never expire.
    pub turn_credential_ttl_seconds: u64,

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
    /// - Signaling only, no STUN/TURN
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
}

impl Default for WebRTCConfig {
    fn default() -> Self {
        Self {
            // Default to PeerToPeer mode (recommended for most deployments)
            mode: WebRTCMode::PeerToPeer,

            // STUN enabled by default (powered by turn-rs)
            enable_builtin_stun: true,
            stun_port: 3478,
            stun_host: "0.0.0.0".to_string(),
            stun_external_addr: String::new(),

            // TURN shared secret (empty = disabled, use static settings)
            turn_shared_secret: String::new(),
            turn_server_urls: Vec::new(),
            // TURN credentials expire after 24 hours by default
            turn_credential_ttl_seconds: 86400,

            filter_private_ice_candidates: true,
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

        let config = builder.build()?;
        let mut config: Self = config.try_deserialize()?;

        // Apply SYNCTV_* environment variable overrides (single underscore format).
        // We don't use the config crate's Environment source because its separator
        // cannot distinguish nesting from underscores within field names.
        // Instead, every SYNCTV_ env var is mapped explicitly here.
        config.apply_env_overrides();

        Ok(config)
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
    /// Priority: `server.advertise_host` config > `POD_IP` env var >
    /// first non-loopback IP > system hostname.
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

        // 3. Auto-detect first non-loopback IP (for non-K8s environments like
        //    Docker Compose, bare-metal, or VMs where hostname may not be routable)
        if let Some(ip) = Self::detect_non_loopback_ip() {
            return ip;
        }

        // 4. System hostname (last resort)
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| self.server.host.clone())
    }

    /// Detect the outbound IP address by performing a UDP connect to a public
    /// address. This triggers the OS to select the appropriate network interface
    /// without actually sending any packets.
    ///
    /// Returns `None` if the detection fails (e.g., no network available).
    fn detect_non_loopback_ip() -> Option<String> {
        // Connect a UDP socket to a well-known public IP (Google DNS).
        // No actual data is sent; the OS just selects the outbound interface.
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        socket.connect("8.8.8.8:53").ok()?;
        let local_addr = socket.local_addr().ok()?;
        let ip = local_addr.ip();

        if ip.is_loopback() || ip.is_unspecified() {
            return None;
        }

        Some(ip.to_string())
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

    /// Apply environment variable overrides using single-underscore format.
    ///
    /// Format: `SYNCTV_<SECTION>_<FIELD>=<value>`
    ///
    /// Examples:
    /// - `SYNCTV_SERVER_HOST=0.0.0.0`
    /// - `SYNCTV_DATABASE_URL=postgresql://...`
    /// - `SYNCTV_SERVER_ADVERTISE_HOST=10.0.0.1`
    fn apply_env_overrides(&mut self) {
        // -- Server --
        env_override_str("SYNCTV_SERVER_HOST", &mut self.server.host);
        env_override_parse("SYNCTV_SERVER_GRPC_PORT", &mut self.server.grpc_port);
        env_override_parse("SYNCTV_SERVER_HTTP_PORT", &mut self.server.http_port);
        env_override_bool("SYNCTV_SERVER_ENABLE_REFLECTION", &mut self.server.enable_reflection);
        env_override_bool("SYNCTV_SERVER_METRICS_ENABLED", &mut self.server.metrics_enabled);
        env_override_csv("SYNCTV_SERVER_TRUSTED_PROXIES", &mut self.server.trusted_proxies);
        env_override_json_or_csv("SYNCTV_SERVER_CORS_ALLOWED_ORIGINS", &mut self.server.cors_allowed_origins);
        env_override_str("SYNCTV_SERVER_CLUSTER_SECRET", &mut self.server.cluster_secret);
        env_override_str("SYNCTV_SERVER_ADVERTISE_HOST", &mut self.server.advertise_host);
        env_override_parse("SYNCTV_SERVER_SHUTDOWN_DRAIN_TIMEOUT_SECONDS", &mut self.server.shutdown_drain_timeout_seconds);
        env_override_bool("SYNCTV_SERVER_DISABLE_WS_TOKEN_QUERY", &mut self.server.disable_ws_token_query);
        env_override_str("SYNCTV_SERVER_METRICS_BEARER_TOKEN", &mut self.server.metrics_bearer_token);
        env_override_parse("SYNCTV_SERVER_GRPC_MAX_MESSAGE_SIZE_BYTES", &mut self.server.grpc_max_message_size_bytes);

        // -- Database --
        env_override_str("SYNCTV_DATABASE_URL", &mut self.database.url);
        env_override_parse("SYNCTV_DATABASE_MAX_CONNECTIONS", &mut self.database.max_connections);
        env_override_parse("SYNCTV_DATABASE_MIN_CONNECTIONS", &mut self.database.min_connections);
        env_override_parse("SYNCTV_DATABASE_CONNECT_TIMEOUT_SECONDS", &mut self.database.connect_timeout_seconds);
        env_override_parse("SYNCTV_DATABASE_IDLE_TIMEOUT_SECONDS", &mut self.database.idle_timeout_seconds);
        env_override_parse("SYNCTV_DATABASE_MAX_LIFETIME_SECONDS", &mut self.database.max_lifetime_seconds);

        // -- Redis --
        env_override_str("SYNCTV_REDIS_URL", &mut self.redis.url);
        env_override_parse("SYNCTV_REDIS_CONNECT_TIMEOUT_SECONDS", &mut self.redis.connect_timeout_seconds);
        env_override_str("SYNCTV_REDIS_KEY_PREFIX", &mut self.redis.key_prefix);
        if let Ok(val) = std::env::var("SYNCTV_REDIS_DEPLOYMENT_MODE") {
            match val.to_lowercase().as_str() {
                "standalone" => self.redis.deployment_mode = RedisDeploymentMode::Standalone,
                "sentinel" => self.redis.deployment_mode = RedisDeploymentMode::Sentinel,
                "cluster" => self.redis.deployment_mode = RedisDeploymentMode::Cluster,
                _ => {}
            }
        }
        env_override_opt_str("SYNCTV_REDIS_SENTINEL_MASTER_NAME", &mut self.redis.sentinel_master_name);
        env_override_csv("SYNCTV_REDIS_SENTINEL_ADDRESSES", &mut self.redis.sentinel_addresses);
        env_override_csv("SYNCTV_REDIS_CLUSTER_NODES", &mut self.redis.cluster_nodes);

        // -- JWT --
        env_override_str("SYNCTV_JWT_SECRET", &mut self.jwt.secret);
        env_override_parse("SYNCTV_JWT_ACCESS_TOKEN_DURATION_HOURS", &mut self.jwt.access_token_duration_hours);
        env_override_parse("SYNCTV_JWT_REFRESH_TOKEN_DURATION_DAYS", &mut self.jwt.refresh_token_duration_days);
        env_override_parse("SYNCTV_JWT_GUEST_TOKEN_DURATION_HOURS", &mut self.jwt.guest_token_duration_hours);
        env_override_parse("SYNCTV_JWT_CLOCK_SKEW_LEEWAY_SECS", &mut self.jwt.clock_skew_leeway_secs);

        // -- Logging --
        env_override_str("SYNCTV_LOGGING_LEVEL", &mut self.logging.level);
        env_override_str("SYNCTV_LOGGING_FORMAT", &mut self.logging.format);
        env_override_opt_str("SYNCTV_LOGGING_FILE_PATH", &mut self.logging.file_path);

        // -- Livestream --
        env_override_parse("SYNCTV_LIVESTREAM_RTMP_PORT", &mut self.livestream.rtmp_port);
        env_override_parse("SYNCTV_LIVESTREAM_GOP_CACHE_SIZE", &mut self.livestream.gop_cache_size);
        env_override_parse("SYNCTV_LIVESTREAM_STREAM_TIMEOUT_SECONDS", &mut self.livestream.stream_timeout_seconds);
        env_override_parse("SYNCTV_LIVESTREAM_CLEANUP_CHECK_INTERVAL_SECONDS", &mut self.livestream.cleanup_check_interval_seconds);
        env_override_parse("SYNCTV_LIVESTREAM_PULL_MAX_RETRIES", &mut self.livestream.pull_max_retries);
        env_override_parse("SYNCTV_LIVESTREAM_PULL_INITIAL_BACKOFF_MS", &mut self.livestream.pull_initial_backoff_ms);
        env_override_parse("SYNCTV_LIVESTREAM_PULL_MAX_BACKOFF_MS", &mut self.livestream.pull_max_backoff_ms);
        env_override_parse("SYNCTV_LIVESTREAM_MAX_FLV_TAG_SIZE_BYTES", &mut self.livestream.max_flv_tag_size_bytes);
        env_override_parse("SYNCTV_LIVESTREAM_GOP_CACHE_MAX_MEMORY_MB", &mut self.livestream.gop_cache_max_memory_mb);
        env_override_bool("SYNCTV_LIVESTREAM_HLS_SHARED_STORAGE", &mut self.livestream.hls_shared_storage);
        env_override_str("SYNCTV_LIVESTREAM_HLS_STORAGE_PATH", &mut self.livestream.hls_storage_path);

        // -- Email --
        env_override_str("SYNCTV_EMAIL_SMTP_HOST", &mut self.email.smtp_host);
        env_override_parse("SYNCTV_EMAIL_SMTP_PORT", &mut self.email.smtp_port);
        env_override_str("SYNCTV_EMAIL_SMTP_USERNAME", &mut self.email.smtp_username);
        env_override_str("SYNCTV_EMAIL_SMTP_PASSWORD", &mut self.email.smtp_password);
        env_override_str("SYNCTV_EMAIL_FROM_EMAIL", &mut self.email.from_email);
        env_override_str("SYNCTV_EMAIL_FROM_NAME", &mut self.email.from_name);
        env_override_bool("SYNCTV_EMAIL_USE_TLS", &mut self.email.use_tls);

        // -- OAuth2 --
        env_override_str("SYNCTV_OAUTH2_REDIRECT_SCHEME", &mut self.oauth2.redirect_scheme);

        // -- Media Providers --
        env_override_parse("SYNCTV_MEDIA_PROVIDERS_REQUEST_TIMEOUT_SECONDS", &mut self.media_providers.request_timeout_seconds);
        env_override_parse("SYNCTV_MEDIA_PROVIDERS_CONNECT_TIMEOUT_SECONDS", &mut self.media_providers.connect_timeout_seconds);

        // -- WebRTC --
        if let Ok(val) = std::env::var("SYNCTV_WEBRTC_MODE") {
            match val.to_lowercase().as_str() {
                "signaling_only" => self.webrtc.mode = WebRTCMode::SignalingOnly,
                "peer_to_peer" => self.webrtc.mode = WebRTCMode::PeerToPeer,
                _ => {}
            }
        }
        env_override_bool("SYNCTV_WEBRTC_ENABLE_BUILTIN_STUN", &mut self.webrtc.enable_builtin_stun);
        env_override_parse("SYNCTV_WEBRTC_STUN_PORT", &mut self.webrtc.stun_port);
        env_override_str("SYNCTV_WEBRTC_STUN_HOST", &mut self.webrtc.stun_host);
        env_override_str("SYNCTV_WEBRTC_STUN_EXTERNAL_ADDR", &mut self.webrtc.stun_external_addr);
        env_override_str("SYNCTV_WEBRTC_TURN_SHARED_SECRET", &mut self.webrtc.turn_shared_secret);
        env_override_json_or_csv("SYNCTV_WEBRTC_TURN_SERVER_URLS", &mut self.webrtc.turn_server_urls);
        env_override_parse("SYNCTV_WEBRTC_TURN_CREDENTIAL_TTL_SECONDS", &mut self.webrtc.turn_credential_ttl_seconds);
        env_override_bool("SYNCTV_WEBRTC_FILTER_PRIVATE_ICE_CANDIDATES", &mut self.webrtc.filter_private_ice_candidates);

        // -- Connection Limits --
        env_override_parse("SYNCTV_CONNECTION_LIMITS_MAX_PER_USER", &mut self.connection_limits.max_per_user);
        env_override_parse("SYNCTV_CONNECTION_LIMITS_MAX_PER_ROOM", &mut self.connection_limits.max_per_room);
        env_override_parse("SYNCTV_CONNECTION_LIMITS_MAX_TOTAL", &mut self.connection_limits.max_total);
        env_override_parse("SYNCTV_CONNECTION_LIMITS_IDLE_TIMEOUT_SECONDS", &mut self.connection_limits.idle_timeout_seconds);
        env_override_parse("SYNCTV_CONNECTION_LIMITS_MAX_DURATION_SECONDS", &mut self.connection_limits.max_duration_seconds);
        env_override_parse("SYNCTV_CONNECTION_LIMITS_WS_MESSAGE_RATE_LIMIT_PER_SECOND", &mut self.connection_limits.ws_message_rate_limit_per_second);

        // -- Bootstrap --
        env_override_bool("SYNCTV_BOOTSTRAP_CREATE_ROOT_USER", &mut self.bootstrap.create_root_user);
        env_override_str("SYNCTV_BOOTSTRAP_ROOT_USERNAME", &mut self.bootstrap.root_username);
        env_override_str("SYNCTV_BOOTSTRAP_ROOT_PASSWORD", &mut self.bootstrap.root_password);

        // -- Cluster --
        env_override_bool("SYNCTV_CLUSTER_ENABLED", &mut self.cluster.enabled);
        env_override_parse("SYNCTV_CLUSTER_CRITICAL_CHANNEL_CAPACITY", &mut self.cluster.critical_channel_capacity);
        env_override_parse("SYNCTV_CLUSTER_PUBLISH_CHANNEL_CAPACITY", &mut self.cluster.publish_channel_capacity);
        env_override_str("SYNCTV_CLUSTER_DISCOVERY_MODE", &mut self.cluster.discovery_mode);
        env_override_str("SYNCTV_CLUSTER_LEADER_ELECTION_MODE", &mut self.cluster.leader_election_mode);
        env_override_csv("SYNCTV_CLUSTER_PEERS", &mut self.cluster.peers);
        env_override_parse("SYNCTV_CLUSTER_CATCHUP_WINDOW_SECS", &mut self.cluster.catchup_window_secs);

        // -- Password Complexity --
        env_override_parse("SYNCTV_PASSWORD_COMPLEXITY_MIN_LENGTH", &mut self.password_complexity.min_length);
        env_override_bool("SYNCTV_PASSWORD_COMPLEXITY_REQUIRE_UPPERCASE", &mut self.password_complexity.require_uppercase);
        env_override_bool("SYNCTV_PASSWORD_COMPLEXITY_REQUIRE_LOWERCASE", &mut self.password_complexity.require_lowercase);
        env_override_bool("SYNCTV_PASSWORD_COMPLEXITY_REQUIRE_DIGIT", &mut self.password_complexity.require_digit);
        env_override_bool("SYNCTV_PASSWORD_COMPLEXITY_REQUIRE_SPECIAL", &mut self.password_complexity.require_special);
        env_override_parse("SYNCTV_PASSWORD_COMPLEXITY_MAX_REPEATED_CHARS", &mut self.password_complexity.max_repeated_chars);

        // -- Buffer Sizes --
        env_override_parse("SYNCTV_BUFFER_SIZES_WEBSOCKET_OUTBOUND", &mut self.buffer_sizes.websocket_outbound);
        env_override_parse("SYNCTV_BUFFER_SIZES_AUDIT_BUFFER", &mut self.buffer_sizes.audit_buffer);

        // -- Cache --
        env_override_parse("SYNCTV_CACHE_L1_CAPACITY", &mut self.cache.l1_capacity);
        env_override_parse("SYNCTV_CACHE_L1_TTL_SECONDS", &mut self.cache.l1_ttl_seconds);
        env_override_parse("SYNCTV_CACHE_L2_TTL_SECONDS", &mut self.cache.l2_ttl_seconds);
        env_override_parse("SYNCTV_CACHE_USERNAME_CACHE_CAPACITY", &mut self.cache.username_cache_capacity);
        env_override_parse("SYNCTV_CACHE_USERNAME_CACHE_TTL_SECONDS", &mut self.cache.username_cache_ttl_seconds);
        env_override_parse("SYNCTV_CACHE_PERMISSION_CACHE_CAPACITY", &mut self.cache.permission_cache_capacity);
        env_override_parse("SYNCTV_CACHE_PERMISSION_CACHE_TTL_SECONDS", &mut self.cache.permission_cache_ttl_seconds);

        // -- HTTP Rate Limits --
        env_override_parse("SYNCTV_HTTP_RATE_LIMITS_AUTH_MAX_REQUESTS", &mut self.http_rate_limits.auth_max_requests);
        env_override_parse("SYNCTV_HTTP_RATE_LIMITS_AUTH_WINDOW_SECONDS", &mut self.http_rate_limits.auth_window_seconds);
        env_override_parse("SYNCTV_HTTP_RATE_LIMITS_WRITE_MAX_REQUESTS", &mut self.http_rate_limits.write_max_requests);
        env_override_parse("SYNCTV_HTTP_RATE_LIMITS_WRITE_WINDOW_SECONDS", &mut self.http_rate_limits.write_window_seconds);
        env_override_parse("SYNCTV_HTTP_RATE_LIMITS_READ_MAX_REQUESTS", &mut self.http_rate_limits.read_max_requests);
        env_override_parse("SYNCTV_HTTP_RATE_LIMITS_READ_WINDOW_SECONDS", &mut self.http_rate_limits.read_window_seconds);
        env_override_parse("SYNCTV_HTTP_RATE_LIMITS_MEDIA_MAX_REQUESTS", &mut self.http_rate_limits.media_max_requests);
        env_override_parse("SYNCTV_HTTP_RATE_LIMITS_MEDIA_WINDOW_SECONDS", &mut self.http_rate_limits.media_window_seconds);
        env_override_parse("SYNCTV_HTTP_RATE_LIMITS_ADMIN_MAX_REQUESTS", &mut self.http_rate_limits.admin_max_requests);
        env_override_parse("SYNCTV_HTTP_RATE_LIMITS_ADMIN_WINDOW_SECONDS", &mut self.http_rate_limits.admin_window_seconds);
        env_override_parse("SYNCTV_HTTP_RATE_LIMITS_STREAMING_MAX_REQUESTS", &mut self.http_rate_limits.streaming_max_requests);
        env_override_parse("SYNCTV_HTTP_RATE_LIMITS_STREAMING_WINDOW_SECONDS", &mut self.http_rate_limits.streaming_window_seconds);
        env_override_parse("SYNCTV_HTTP_RATE_LIMITS_WEBSOCKET_MAX_REQUESTS", &mut self.http_rate_limits.websocket_max_requests);
        env_override_parse("SYNCTV_HTTP_RATE_LIMITS_WEBSOCKET_WINDOW_SECONDS", &mut self.http_rate_limits.websocket_window_seconds);

        // -- gRPC Rate Limits --
        env_override_parse("SYNCTV_GRPC_RATE_LIMITS_AUTH_MAX_REQUESTS", &mut self.grpc_rate_limits.auth_max_requests);
        env_override_parse("SYNCTV_GRPC_RATE_LIMITS_AUTH_WINDOW_SECONDS", &mut self.grpc_rate_limits.auth_window_seconds);
        env_override_parse("SYNCTV_GRPC_RATE_LIMITS_EMAIL_MAX_REQUESTS", &mut self.grpc_rate_limits.email_max_requests);
        env_override_parse("SYNCTV_GRPC_RATE_LIMITS_EMAIL_WINDOW_SECONDS", &mut self.grpc_rate_limits.email_window_seconds);
        env_override_parse("SYNCTV_GRPC_RATE_LIMITS_MEDIA_MAX_REQUESTS", &mut self.grpc_rate_limits.media_max_requests);
        env_override_parse("SYNCTV_GRPC_RATE_LIMITS_MEDIA_WINDOW_SECONDS", &mut self.grpc_rate_limits.media_window_seconds);
        env_override_parse("SYNCTV_GRPC_RATE_LIMITS_WRITE_MAX_REQUESTS", &mut self.grpc_rate_limits.write_max_requests);
        env_override_parse("SYNCTV_GRPC_RATE_LIMITS_WRITE_WINDOW_SECONDS", &mut self.grpc_rate_limits.write_window_seconds);
        env_override_parse("SYNCTV_GRPC_RATE_LIMITS_ADMIN_MAX_REQUESTS", &mut self.grpc_rate_limits.admin_max_requests);
        env_override_parse("SYNCTV_GRPC_RATE_LIMITS_ADMIN_WINDOW_SECONDS", &mut self.grpc_rate_limits.admin_window_seconds);
        env_override_parse("SYNCTV_GRPC_RATE_LIMITS_READ_MAX_REQUESTS", &mut self.grpc_rate_limits.read_max_requests);
        env_override_parse("SYNCTV_GRPC_RATE_LIMITS_READ_WINDOW_SECONDS", &mut self.grpc_rate_limits.read_window_seconds);
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

        // Validate JWT secret
        if self.jwt.secret.is_empty() {
            errors.push("JWT secret is empty".to_string());
        } else if self.jwt.secret == "change-me-in-production" {
            errors.push("JWT secret is set to default value 'change-me-in-production'. Set SYNCTV_JWT_SECRET environment variable".to_string());
        }

        // Validate root credentials
        if self.bootstrap.create_root_user {
                if self.bootstrap.root_password.is_empty() {
                    errors.push("Root password is empty. Set SYNCTV_BOOTSTRAP_ROOT_PASSWORD environment variable".to_string());
                } else if self.bootstrap.root_password == "root" {
                    errors.push("Root password is set to default value 'root'. Set SYNCTV_BOOTSTRAP_ROOT_PASSWORD environment variable".to_string());
                }
                if self.bootstrap.root_username.len() < 3 {
                    errors.push("Root username must be at least 3 characters".to_string());
                }
                // H-02: Enforce 12-char minimum and complexity for root password
                let pwd = &self.bootstrap.root_password;
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

        // Validate gRPC max message size (prevent OOM attacks)
        const MIN_GRPC_MESSAGE_SIZE: usize = 1024 * 1024; // 1 MB minimum
        const MAX_GRPC_MESSAGE_SIZE: usize = 1024 * 1024 * 1024; // 1 GB maximum
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

        // Validate trusted_proxies CIDR format
        // List of dangerous CIDR ranges that should never be trusted
        const DANGEROUS_CIDR_RANGES: &[&str] = &[
            "0.0.0.0/0",       // All IPv4 addresses
            "::/0",            // All IPv6 addresses
            "0.0.0.0/0,::/0",  // Combined IPv4+IPv6 (sometimes seen in configs)
        ];

        for (i, proxy) in self.server.trusted_proxies.iter().enumerate() {
            // Check for dangerous overly-broad CIDR ranges first
            let proxy_normalized = proxy.replace(' ', "").to_lowercase();
            for dangerous in DANGEROUS_CIDR_RANGES {
                if proxy_normalized == dangerous.replace(' ', "").to_lowercase() {
                    errors.push(format!(
                        "server.trusted_proxies[{i}] '{proxy}' is a dangerous configuration \
                         that trusts ALL IP addresses. This allows IP spoofing attacks via \
                         X-Forwarded-For headers. Use specific IP ranges (e.g., '10.0.0.0/8', \
                         '172.16.0.0/12', '192.168.0.0/16') for your trusted proxies/_load balancers."
                    ));
                    continue;
                }
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
            if proxy.parse::<ipnet::IpNet>().is_err()
                && proxy.parse::<std::net::IpAddr>().is_err()
            {
                errors.push(format!(
                    "server.trusted_proxies[{i}] '{proxy}' is not a valid CIDR or IP address"
                ));
            }
        }

        // Reject Redis Cluster mode (not yet supported)
        if self.redis.deployment_mode == RedisDeploymentMode::Cluster {
            errors.push(
                "Redis Cluster mode is not yet supported. \
                 Please use Standalone or Sentinel mode. \
                 See the RedisDeploymentMode documentation for details."
                    .to_string(),
            );
        }

        // Validate Redis Sentinel mode required fields
        if self.redis.deployment_mode == RedisDeploymentMode::Sentinel {
            if self.redis.sentinel_master_name.is_none()
                || self.redis.sentinel_master_name.as_ref().is_none_or(std::string::String::is_empty)
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

        // Validate Redis requirement based on deployment mode
        if self.redis.url.is_empty() {
            if self.cluster.enabled {
                errors.push(
                    "Redis is required when cluster mode is enabled (cluster.enabled=true). \
                     Configure redis.url or set SYNCTV_REDIS_URL."
                        .to_string(),
                );
            } else {
                tracing::info!(
                    "Redis is not configured — running in standalone mode with in-memory fallbacks. \
                     All features work, but data (rate limits, brute-force counters, token blacklist) \
                     will not persist across restarts."
                );
            }
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
        if self.connection_limits.idle_timeout_seconds > 0 && self.connection_limits.idle_timeout_seconds < 10 {
            errors.push("connection_limits.idle_timeout_seconds must be at least 10 seconds".to_string());
        }
        if self.connection_limits.ws_message_rate_limit_per_second == 0 {
            errors.push("connection_limits.ws_message_rate_limit_per_second must be greater than 0".to_string());
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

        // **Production Enhancement (#23)**: Validate media provider timeouts
        if self.media_providers.request_timeout_seconds > 300 {
            errors.push("media_providers.request_timeout_seconds should not exceed 300 seconds (5 minutes)".to_string());
        }
        if self.media_providers.connect_timeout_seconds == 0 {
            errors.push("media_providers.connect_timeout_seconds must be greater than 0".to_string());
        }
        if self.media_providers.connect_timeout_seconds > self.media_providers.request_timeout_seconds && self.media_providers.request_timeout_seconds > 0 {
            errors.push("media_providers.connect_timeout_seconds should not exceed request_timeout_seconds".to_string());
        }

        // Validate CORS origins
        if !self.server.cors_allowed_origins.is_empty() {
            for origin in &self.server.cors_allowed_origins {
                if origin == "*" {
                    errors.push("CORS wildcard '*' is not allowed. Specify exact origins.".to_string());
                    break;
                }
                // Validate origin format
                if !origin.starts_with("http://") && !origin.starts_with("https://") {
                    errors.push(format!(
                        "CORS origin '{origin}' must start with http:// or https://"
                    ));
                }
            }
        }

        // Validate: cluster mode requires Redis.
        // Redis is essential for cross-replica pub/sub, leader election, node registry,
        // distributed rate limiting, and brute-force protection. Running cluster mode
        // without Redis causes silent data loss and broken multi-replica coordination.
        let cluster_mode_active = self.cluster.enabled || !self.server.cluster_secret.is_empty();
        if cluster_mode_active && self.redis.url.is_empty() {
            errors.push(
                "Redis is required when cluster mode is enabled. \
                 Configure redis.url (or SYNCTV_REDIS_URL env var) before starting \
                 with cluster.enabled=true or server.cluster_secret set. \
                 Redis provides cross-replica pub/sub, leader election, node registry, \
                 and distributed rate limiting — all mandatory for multi-replica deployments."
                    .to_string(),
            );
        }

        // HLS shared storage validation in cluster mode.
        // In cluster mode, HLS segments must be on storage accessible by all replicas.
        // If segments are on local-only storage, clients will receive 404s for segments
        // served by a replica that did not create them.
        if cluster_mode_active {
            if self.livestream.hls_shared_storage {
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
                         shared storage (NFS, CSI volume) on every replica, otherwise HLS \
                         clients will receive 404 errors for segments on other replicas.",
                        path
                    );
                }
            } else {
                tracing::warn!(
                    "Cluster mode is enabled but livestream.hls_shared_storage is false. \
                     HLS segments stored on node-local storage will NOT be accessible from \
                     other replicas, causing 404 errors for clients. \
                     Use a shared volume (NFS, CSI), S3-compatible mount, or set \
                     livestream.hls_shared_storage=true once shared storage is configured."
                );
            }
        } else {
            // Single-node: warn about MemoryStorage for observability
            tracing::warn!(
                "The default HLS storage backend is MemoryStorage, which is node-local. \
                 HLS segments will NOT be shared across replicas if cluster mode is later enabled. \
                 For multi-replica HLS, configure shared FileStorage (NFS/CSI volume) or OssStorage."
            );
        }

        // Require cluster_secret when cluster mode is enabled (Issue #39).
        //
        // An empty `cluster_secret` means that ANY node claiming to be part of the
        // cluster can call inter-node gRPC endpoints without authentication.
        //
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

        // Warn if cors_allowed_origins is empty
        if self.server.cors_allowed_origins.is_empty() {
            tracing::warn!(
                "server.cors_allowed_origins is empty. \
                 CORS requests will be rejected. Set allowed origins."
            );
        }

        // SECURITY: Warn if metrics endpoint is enabled without authentication
        // The /metrics endpoint exposes sensitive operational data including:
        // - Request rates and latencies
        // - Internal service topology
        // - Database connection pool states
        // - Memory and CPU usage patterns
        // Without authentication, anyone who can reach the endpoint can access this data.
        if self.server.metrics_enabled && self.server.metrics_bearer_token.is_empty() {
            tracing::warn!(
                "SECURITY WARNING: server.metrics_enabled=true but server.metrics_bearer_token is empty. \
                 The /metrics endpoint is COMPLETELY OPEN to anyone who can reach it. \
                 This exposes sensitive operational data (request rates, internal topology, \
                 resource usage) that could aid attackers in reconnaissance. \
                 STRONGLY RECOMMENDED: Set SYNCTV_SERVER_METRICS_BEARER_TOKEN to a secure random value \
                 (e.g., output of `openssl rand -hex 32`). \
                 Only leave this empty if the /metrics endpoint is protected at the network level \
                 (e.g., Kubernetes NetworkPolicy restricting access to monitoring pods only)."
            );
        }


        // **WebRTC Issue (#21)**: Validate STUN external address.
        // In cluster/K8s/NAT environments (indicated by cluster_secret being set),
        // stun_external_addr is REQUIRED because pods sit behind NAT and the
        // default advertise_host (pod IP) is not reachable from the internet.
        // Without an explicit external address, STUN reflexive candidates will
        // contain internal IPs and WebRTC connections will fail.
        if self.webrtc.enable_builtin_stun
            && self.webrtc.stun_external_addr.is_empty() {
                if self.server.cluster_secret.is_empty() {
                    tracing::warn!(
                        "webrtc.enable_builtin_stun=true but stun_external_addr is not set. \
                         STUN server will advertise reflexive candidates using advertise_host. \
                         This may not work correctly behind NAT or load balancers. \
                         Set webrtc.stun_external_addr to the server's public IP:port."
                    );
                } else {
                    errors.push(
                        "webrtc.stun_external_addr must be set when running in cluster mode \
                         (cluster_secret is configured). In NAT/K8s environments, STUN needs \
                         the server's public IP:port to generate valid reflexive candidates. \
                         Example: webrtc.stun_external_addr = \"203.0.113.1:3478\""
                            .to_string(),
                    );
                }
            }

        // Validate OAuth2 state storage requirements.
        // In cluster mode, Redis is required for OAuth2 state storage because the
        // callback request may hit a different replica than the one that generated
        // the auth URL. In standalone mode, an in-memory state store is used.
        let oauth2_enabled = self.oauth2.providers.as_object()
            .is_some_and(|obj| !obj.is_empty());
        if oauth2_enabled && self.redis.url.is_empty() && self.cluster.enabled {
            errors.push(
                "OAuth2 requires Redis for state storage in cluster mode. \
                 Configure redis.url or disable OAuth2 by removing all oauth2.providers."
                    .to_string(),
            );
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

// --- Environment variable override helpers (single underscore format) ---

fn env_override_str(name: &str, target: &mut String) {
    if let Ok(val) = std::env::var(name) {
        *target = val;
    }
}

fn env_override_opt_str(name: &str, target: &mut Option<String>) {
    if let Ok(val) = std::env::var(name) {
        *target = Some(val);
    }
}

fn env_override_parse<T: std::str::FromStr>(name: &str, target: &mut T) {
    if let Ok(val) = std::env::var(name) {
        if let Ok(parsed) = val.parse() {
            *target = parsed;
        }
    }
}

fn env_override_bool(name: &str, target: &mut bool) {
    if let Ok(val) = std::env::var(name) {
        match val.to_lowercase().as_str() {
            "true" | "1" | "yes" => *target = true,
            "false" | "0" | "no" => *target = false,
            _ => {}
        }
    }
}

fn env_override_csv(name: &str, target: &mut Vec<String>) {
    if let Ok(val) = std::env::var(name) {
        *target = val
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
}

fn env_override_json_or_csv(name: &str, target: &mut Vec<String>) {
    if let Ok(val) = std::env::var(name) {
        // Try JSON array first (e.g., '["https://app.example.com"]')
        if let Ok(parsed) = serde_json::from_str::<Vec<String>>(&val) {
            *target = parsed;
        } else {
            // Fall back to comma-separated
            *target = val
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
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
            idle_timeout_seconds: 300, // 5 minutes
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

/// Cluster channel capacity configuration
///
/// Controls the buffer sizes for internal channels used in cluster communication.
/// Larger values provide more resilience during traffic spikes but use more memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClusterChannelConfig {
    /// Whether cluster mode is explicitly enabled.
    ///
    /// When true, Redis is **mandatory** and startup will fail (not warn) if
    /// `redis.url` is empty or unset. Cluster mode uses Redis for:
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
    /// - "redis": Use Redis-based node registry (default, works everywhere)
    /// - "`k8s_dns"`: Use Kubernetes headless service DNS for peer discovery
    ///   (requires `HEADLESS_SERVICE_NAME` and `POD_NAMESPACE` env vars).
    ///   NOTE: K8s DNS mode still requires Redis for health monitoring, load
    ///   balancing, and cluster pub/sub. DNS only supplements peer discovery
    ///   (faster detection of new pods). Without Redis, `k8s_dns` mode provides
    ///   DNS resolution only -- no `NodeRegistry`, `HealthMonitor`, or `LoadBalancer`.
    pub discovery_mode: String,

    /// Leader election mode for singleton operations.
    /// - "redis": Use Redis-based distributed locks (default, works everywhere)
    /// - "`k8s_lease"`: Use Kubernetes coordination.k8s.io/v1 Lease resource
    ///   (requires `POD_NAME` and `POD_NAMESPACE` env vars, RBAC permissions)
    pub leader_election_mode: String,

    /// Static peer addresses for non-K8s / non-Redis cluster discovery.
    /// When configured, each peer is periodically health-checked via gRPC
    /// and registered into the `NodeRegistry` if alive.
    /// Example: ["host1:50051", "host2:50051"]
    pub peers: Vec<String>,

    /// How far back (in seconds) to replay Redis Stream events when a new node
    /// first connects to the cluster.  Replaying recent history ensures that
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
            discovery_mode: "redis".to_string(),
            leader_election_mode: "redis".to_string(),
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

/// Cache layer configuration
///
/// Controls cache capacities and TTLs for the L1 (in-memory) and L2 (Redis) tiers.
/// All values have sensible defaults matching the previously hardcoded values.
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
            l1_ttl_seconds: 300,       // 5 minutes (was hardcoded as 5 min TTL)
            l2_ttl_seconds: 300,       // 5 minutes
            username_cache_capacity: 1000,
            username_cache_ttl_seconds: 3600,  // 1 hour
            permission_cache_capacity: 1000,
            permission_cache_ttl_seconds: 300,
        }
    }
}

/// HTTP API rate limit configuration for different endpoint categories.
///
/// This is separate from the domain-level `RateLimitConfig` in
/// `synctv_core::service::rate_limit` (which controls chat/danmaku rates).
/// This struct configures the per-category request rate limits applied by
/// the HTTP middleware layer.
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
            password_complexity: PasswordComplexityConfig::default(),
            buffer_sizes: BufferSizesConfig::default(),
            cache: CacheConfig::default(),
            http_rate_limits: HttpRateLimitConfig::default(),
            grpc_rate_limits: GrpcRateLimitConfig::default(),
        });

        assert!(!config.database_url().is_empty());
        // Redis URL defaults to empty (standalone mode without Redis)
        assert!(config.redis_url().is_empty());
        assert!(config.server.grpc_port > 0);
        assert!(config.server.http_port > 0);
        assert!(config.webrtc.enable_builtin_stun);
        // Default is false for security - operators must explicitly enable root creation
        assert!(!config.bootstrap.create_root_user);
        // Default gRPC message size limit is 16 MB
        assert_eq!(config.server.grpc_max_message_size_bytes, 16 * 1024 * 1024);
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
        assert!(errors.iter().any(|e| e.contains("grpc_max_message_size_bytes") && e.contains("1 MB")));

        // Invalid: above maximum
        config.server.grpc_max_message_size_bytes = 1024 * 1024 * 1024 + 1; // Just over 1 GB
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("grpc_max_message_size_bytes") && e.contains("1 GB")));
    }

    #[test]
    fn test_grpc_address() {
        let config = Config {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                grpc_port: 50051,
                http_port: 8080,
                enable_reflection: true,
                metrics_enabled: false,
                metrics_bearer_token: String::new(),
                grpc_max_message_size_bytes: 16 * 1024 * 1024,
                trusted_proxies: Vec::new(),
                cors_allowed_origins: Vec::new(),
                cluster_secret: String::new(),
                advertise_host: String::new(),
                shutdown_drain_timeout_seconds: 30,
                disable_ws_token_query: true,
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
            password_complexity: PasswordComplexityConfig::default(),
            buffer_sizes: BufferSizesConfig::default(),
            cache: CacheConfig::default(),
            http_rate_limits: HttpRateLimitConfig::default(),
            grpc_rate_limits: GrpcRateLimitConfig::default(),
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
                metrics_enabled: false,
                metrics_bearer_token: String::new(),
                grpc_max_message_size_bytes: 16 * 1024 * 1024,
                enable_reflection: false,
                trusted_proxies: Vec::new(),
                cors_allowed_origins: Vec::new(),
                // A non-empty cluster_secret is required whenever Redis is configured (Issue #39).
                // Tests that explicitly clear this field test the validation error path.
                cluster_secret: "test-cluster-secret-for-validation".to_string(),
                advertise_host: String::new(),
                shutdown_drain_timeout_seconds: 30,
                disable_ws_token_query: true,
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
            livestream: LivestreamConfig::default(),
            oauth2: OAuth2Config::default(),
            email: EmailConfig::default(),
            media_providers: MediaProvidersConfig::default(),
            webrtc: WebRTCConfig {
                // In cluster mode (cluster_secret is set), stun_external_addr is required.
                stun_external_addr: "203.0.113.1:3478".to_string(),
                ..WebRTCConfig::default()
            },
            connection_limits: ConnectionLimitsConfig::default(),
            bootstrap: BootstrapConfig {
                create_root_user: true,
                root_username: "admin".to_string(),
                root_password: "StrongPwd12345!".to_string(),
            },
            cluster: ClusterChannelConfig::default(),
            password_complexity: PasswordComplexityConfig::default(),
            buffer_sizes: BufferSizesConfig::default(),
            cache: CacheConfig::default(),
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
    fn test_validate_default_root_password() {
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
    fn test_validate_livestream_zero_timeout() {
        let mut config = valid_prod_config();
        config.livestream.stream_timeout_seconds = 0;
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("stream_timeout_seconds")));
    }

    #[test]
    fn test_validate_webrtc_p2p_mode_allowed_in_cluster() {
        let mut config = valid_prod_config();
        config.server.cluster_secret = "shared-secret-123".to_string();
        config.webrtc.mode = WebRTCMode::PeerToPeer;
        config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_webrtc_signaling_only_mode_allowed_in_cluster() {
        let mut config = valid_prod_config();
        config.server.cluster_secret = "shared-secret-123".to_string();
        config.webrtc.mode = WebRTCMode::SignalingOnly;
        config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_redis_cluster_mode_rejected() {
        let mut config = valid_prod_config();
        config.redis.deployment_mode = RedisDeploymentMode::Cluster;
        let errors = config.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("Cluster") && e.contains("not yet supported")));
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
            errors.iter().any(|e| e.contains("Redis is required when cluster mode is enabled")),
            "Expected cluster+no-redis error, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_validate_cluster_enabled_with_redis_ok() {
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        // valid_prod_config() includes redis.url, so this should pass
        // (assuming webrtc.stun_external_addr is set for cluster mode)
        config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_cluster_secret_without_redis_requires_redis() {
        let mut config = valid_prod_config();
        // cluster_secret set (cluster mode active) but no Redis
        config.server.cluster_secret = "shared-secret".to_string();
        config.redis.url = String::new();
        config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();
        let errors = config.validate().unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("Redis is required when cluster mode is enabled")),
            "Expected cluster+no-redis error, got: {:?}",
            errors
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
            errors.iter().any(|e| e.contains("cluster_secret must be set when cluster mode is enabled")),
            "Expected cluster_secret error, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_validate_standalone_redis_without_cluster_secret_ok() {
        // Standalone mode (cluster.enabled=false) with Redis but no cluster_secret → OK
        let mut config = valid_prod_config();
        config.cluster.enabled = false;
        config.server.cluster_secret = String::new();
        assert!(config.validate().is_ok(), "Expected Ok in standalone mode without cluster_secret");
    }

    #[test]
    fn test_validate_cluster_enabled_with_cluster_secret_ok() {
        // cluster.enabled=true + cluster_secret set → should pass
        let mut config = valid_prod_config();
        config.cluster.enabled = true;
        assert!(config.validate().is_ok(), "Expected Ok with cluster mode + cluster_secret set");
    }
}
