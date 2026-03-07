// Remote Provider Manager
//
// Manages remote provider instances (gRPC connections).
// Supports both local (in-process) and remote (gRPC) provider instances.
//
// ## Multi-replica support
//
// Instead of maintaining a persistent channel map that is invisible across replicas,
// channels are created lazily on demand and cached with a TTL. When a provider
// operation is needed, the manager looks up the instance config from the DB and
// creates a channel if not already cached.
//
// Provider changes (add/update/delete/enable/disable) publish a Redis Pub/Sub
// notification so other replicas can invalidate their local cache.

use crate::models::ProviderInstance;
use crate::provider::ProviderError;
use crate::repository::ProviderInstanceRepository;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Uri};
use tonic_health::pb::{health_client::HealthClient, HealthCheckRequest};

/// Redis Pub/Sub channel for provider instance change notifications
const PROVIDER_CHANGE_CHANNEL: &str = "synctv:provider_instances:changes";

/// Default channel cache TTL (5 minutes)
const CHANNEL_CACHE_TTL_SECS: u64 = 300;

/// Maximum number of cached channels
const MAX_CACHED_CHANNELS: u64 = 1_000;

/// Remote Provider Manager
///
/// Manages remote provider instances (gRPC connections).
/// When no remote instance is found, providers fallback to singleton local clients.
///
/// ## Multi-replica architecture
///
/// - Channels are created lazily from DB config and cached with TTL via moka
/// - `get(name)` looks up the cached channel or creates one from DB on cache miss
/// - Provider mutations publish a Redis Pub/Sub notification for cross-replica invalidation
/// - A background subscriber listens for invalidation messages and evicts stale entries
pub struct RemoteProviderManager {
    /// Lazily-populated channel cache with TTL (indexed by instance name)
    channel_cache: Arc<moka::future::Cache<String, Channel>>,

    /// Repository for database operations
    repository: Arc<ProviderInstanceRepository>,

    /// Shared Redis connection handle that follows Sentinel failover.
    redis_conn: Option<Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>,

    /// Optional Redis client for creating Pub/Sub subscriptions
    /// (`ConnectionManager` cannot be used for subscriptions)
    redis_client: Option<redis::Client>,
}

impl std::fmt::Debug for RemoteProviderManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteProviderManager")
            .field("redis_enabled", &self.redis_client.is_some())
            .finish()
    }
}

impl RemoteProviderManager {
    /// Create a new `RemoteProviderManager`
    ///
    /// When `redis_conn` is provided, provider changes are published via Redis Pub/Sub
    /// so other replicas can invalidate their local cache. Without Redis, cache
    /// invalidation is local only (entries expire naturally via TTL).
    ///
    /// `redis_client` is needed to create a dedicated Pub/Sub subscription connection.
    #[must_use]
    pub fn new(
        repository: Arc<ProviderInstanceRepository>,
        redis_conn: Option<Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>,
        redis_client: Option<redis::Client>,
    ) -> Self {
        if redis_conn.is_none() {
            tracing::warn!(
                "RemoteProviderManager using local-only cache invalidation. \
                 For multi-replica setups, configure Redis for cross-replica sync."
            );
        }
        let channel_cache = moka::future::Cache::builder()
            .max_capacity(MAX_CACHED_CHANNELS)
            .time_to_live(Duration::from_secs(CHANNEL_CACHE_TTL_SECS))
            .build();

        Self {
            channel_cache: Arc::new(channel_cache),
            repository,
            redis_conn,
            redis_client,
        }
    }

    /// Initialize manager by pre-warming the cache with all enabled instances from database.
    ///
    /// This is optional -- channels will be created lazily on demand even without
    /// calling `init()`. However, pre-warming reduces latency for the first request
    /// to each provider.
    pub async fn init(&self) -> crate::Result<()> {
        tracing::info!("Initializing provider instance manager (pre-warming cache)");

        let configs = self.repository.get_all_enabled().await?;
        let mut success_count = 0;
        let mut error_count = 0;

        for config in configs {
            match Self::create_grpc_channel(&config).await {
                Ok(channel) => {
                    self.channel_cache
                        .insert(config.name.clone(), channel)
                        .await;
                    tracing::info!("Pre-warmed provider instance cache: {}", config.name);
                    success_count += 1;
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to pre-warm provider instance {}: {}",
                        config.name,
                        e
                    );
                    error_count += 1;
                }
            }
        }

        tracing::info!(
            "Provider instance manager initialized: {} instances cached, {} failed",
            success_count,
            error_count
        );

        Ok(())
    }

    /// Start the Redis Pub/Sub subscriber for cross-replica cache invalidation.
    ///
    /// Spawns a background task that subscribes to the provider change channel
    /// using a dedicated Pub/Sub connection. All replicas receive every
    /// invalidation message (broadcast semantics).
    /// Returns immediately if Redis is not configured.
    pub async fn start_invalidation_listener(&self) -> crate::Result<()> {
        let Some(ref client) = self.redis_client else {
            tracing::debug!("No Redis configured, skipping invalidation listener");
            return Ok(());
        };

        let client = client.clone();

        let cache = Arc::clone(&self.channel_cache);

        crate::spawn::spawn_monitored("provider_invalidation_listener", async move {
            loop {
                match Self::run_pubsub_listener(&client, &cache).await {
                    Ok(()) => break, // clean shutdown (shouldn't happen)
                    Err(e) => {
                        tracing::warn!(
                            "Provider invalidation subscriber error: {e}. Reconnecting in 5s."
                        );
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });

        tracing::info!("Provider instance cache invalidation listener started (Pub/Sub)");
        Ok(())
    }

    /// Internal Pub/Sub listener loop. Returns Err on connection failure so
    /// the caller can reconnect.
    async fn run_pubsub_listener(
        client: &redis::Client,
        cache: &moka::future::Cache<String, Channel>,
    ) -> crate::Result<()> {
        use futures::StreamExt;

        let mut pubsub = client.get_async_pubsub().await.map_err(|e| {
            crate::Error::Internal(format!("Failed to get Pub/Sub connection: {e}"))
        })?;

        pubsub
            .subscribe(PROVIDER_CHANGE_CHANNEL)
            .await
            .map_err(|e| crate::Error::Internal(format!("Failed to subscribe: {e}")))?;

        let mut stream = pubsub.on_message();

        while let Some(msg) = stream.next().await {
            let instance_name: String = match msg.get_payload() {
                Ok(name) => name,
                Err(e) => {
                    tracing::warn!("Invalid payload in provider change message: {e}");
                    continue;
                }
            };

            tracing::info!(
                "Received provider change notification for '{}', invalidating cache",
                instance_name
            );
            cache.invalidate(&instance_name).await;
        }

        // Stream ended unexpectedly
        Err(crate::Error::Internal("Pub/Sub stream ended".to_string()))
    }

    /// Publish a cache invalidation notification to Redis so other replicas
    /// evict the stale entry for `instance_name`.
    async fn notify_change(&self, instance_name: &str) {
        let Some(ref conn_handle) = self.redis_conn else {
            return;
        };

        let mut conn = conn_handle.read().await.clone();

        // PUBLISH to the channel -- all subscribed replicas receive the message
        let result: Result<(), redis::RedisError> = redis::cmd("PUBLISH")
            .arg(PROVIDER_CHANGE_CHANNEL)
            .arg(instance_name)
            .query_async(&mut conn)
            .await;

        if let Err(e) = result {
            tracing::warn!(
                "Failed to publish provider change notification for '{}': {e}",
                instance_name
            );
        }
    }

    /// Validate that an endpoint URL does not target internal/private IP addresses
    /// or reserved hostnames (SSRF protection).
    ///
    /// Only validates hostnames and IP literals statically. Does NOT resolve DNS,
    /// because DNS results can change between validation and connection (DNS rebinding),
    /// and VPN/proxy environments may return unexpected IPs for public hostnames.
    fn validate_endpoint_ssrf(endpoint: &str) -> crate::Result<()> {
        let url = url::Url::parse(endpoint).map_err(|e| {
            crate::Error::InvalidInput(format!("SSRF validation: invalid URL: {e}"))
        })?;

        let host = url.host_str().ok_or_else(|| {
            crate::Error::InvalidInput("SSRF validation: missing host".to_string())
        })?;

        let guard = synctv_common::ssrf::SsrfGuard::default_policy();

        // Check if the hostname itself is blocked (e.g., "localhost", metadata endpoints)
        if guard.is_host_blocked(host) {
            return Err(crate::Error::InvalidInput(format!(
                "SSRF validation: host '{host}' is blocked (internal/reserved)"
            )));
        }

        // If the host is an IP address, check it directly against blocklist
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            if guard.is_ip_blocked(&ip) {
                return Err(crate::Error::InvalidInput(format!(
                    "SSRF validation: IP '{ip}' is blocked (internal/private)"
                )));
            }
        }
        // Note: For hostnames, we do NOT resolve DNS here. DNS-based SSRF checking
        // is unreliable (DNS rebinding, VPN interception). The gRPC transport layer
        // provides the actual connection-time SSRF protection.

        // Validate port range
        if let Some(port) = url.port() {
            if port == 0 {
                return Err(crate::Error::InvalidInput(
                    "SSRF validation: port 0 is not valid".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Create a gRPC channel for the given provider instance
    ///
    /// Establishes gRPC connection with configured TLS settings, timeout, and middleware.
    async fn create_grpc_channel(config: &ProviderInstance) -> crate::Result<Channel> {
        // SSRF validation: block internal/private IPs and reserved hostnames
        Self::validate_endpoint_ssrf(&config.endpoint)?;

        // Parse timeout
        let timeout = config.parse_timeout().map_err(crate::Error::Internal)?;

        // Create endpoint
        let mut endpoint = Endpoint::from_shared(config.endpoint.clone())
            .map_err(|e| crate::Error::Internal(format!("Invalid endpoint: {e}")))?
            .timeout(timeout)
            .tcp_keepalive(Some(Duration::from_secs(30)))
            .http2_keep_alive_interval(Duration::from_secs(30))
            .keep_alive_timeout(Duration::from_secs(10));

        // Configure TLS if enabled
        if config.tls {
            if config.insecure_tls {
                // Skip certificate verification (UNSAFE, development/testing only)
                tracing::warn!(
                    "Instance '{}' configured with insecure TLS (skips certificate verification)",
                    config.name
                );

                // Build a custom connector that skips TLS certificate verification.
                // tonic's ClientTlsConfig doesn't expose this, so we build a raw
                // rustls ClientConfig with a no-op verifier and wrap it in a
                // tower::Service<Uri> that tonic can use.
                let channel = Self::connect_insecure_tls(endpoint).await.map_err(|e| {
                    crate::Error::Internal(format!("insecure TLS connect failed: {e}"))
                })?;

                tracing::info!(
                    "Established insecure-TLS gRPC connection to {} (timeout: {:?})",
                    config.endpoint,
                    timeout,
                );

                return Ok(channel);
            }

            let mut tls_config = ClientTlsConfig::new();

            // Use custom CA certificate if provided
            if let Some(ref ca_pem) = config.custom_ca {
                let cert = Certificate::from_pem(ca_pem);
                tls_config = tls_config.ca_certificate(cert);
            } else {
                // Use system CA certificates
                tls_config = tls_config.with_native_roots();
            }

            endpoint = endpoint
                .tls_config(tls_config)
                .map_err(|e| crate::Error::Internal(format!("TLS config error: {e}")))?;
        }

        // Create lazy gRPC channel (connects on first use, not eagerly)
        // Lazy connection allows storing the channel even if the remote server
        // is temporarily unavailable. Health checks detect unreachable servers.
        let channel = endpoint.connect_lazy();

        tracing::info!(
            "Established gRPC connection to {} (timeout: {:?}, TLS: {})",
            config.endpoint,
            timeout,
            config.tls
        );

        Ok(channel)
    }

    /// Connect to a gRPC endpoint with TLS certificate verification disabled.
    ///
    /// This builds a custom `tower::Service<Uri>` connector that uses a rustls
    /// `ClientConfig` with a no-op certificate verifier. Only for dev/testing.
    async fn connect_insecure_tls(
        endpoint: Endpoint,
    ) -> Result<Channel, Box<dyn std::error::Error + Send + Sync>> {
        use rustls::client::danger::{
            HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
        };
        use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
        use rustls::{ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme};

        /// A certificate verifier that accepts any server certificate.
        #[derive(Debug)]
        struct NoVerifier;

        impl ServerCertVerifier for NoVerifier {
            fn verify_server_cert(
                &self,
                _end_entity: &CertificateDer<'_>,
                _intermediates: &[CertificateDer<'_>],
                _server_name: &ServerName<'_>,
                _ocsp_response: &[u8],
                _now: UnixTime,
            ) -> Result<ServerCertVerified, TlsError> {
                Ok(ServerCertVerified::assertion())
            }

            fn verify_tls12_signature(
                &self,
                _message: &[u8],
                _cert: &CertificateDer<'_>,
                _dss: &DigitallySignedStruct,
            ) -> Result<HandshakeSignatureValid, TlsError> {
                Ok(HandshakeSignatureValid::assertion())
            }

            fn verify_tls13_signature(
                &self,
                _message: &[u8],
                _cert: &CertificateDer<'_>,
                _dss: &DigitallySignedStruct,
            ) -> Result<HandshakeSignatureValid, TlsError> {
                Ok(HandshakeSignatureValid::assertion())
            }

            fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
                vec![
                    SignatureScheme::RSA_PKCS1_SHA256,
                    SignatureScheme::RSA_PKCS1_SHA384,
                    SignatureScheme::RSA_PKCS1_SHA512,
                    SignatureScheme::ECDSA_NISTP256_SHA256,
                    SignatureScheme::ECDSA_NISTP384_SHA384,
                    SignatureScheme::ECDSA_NISTP521_SHA512,
                    SignatureScheme::RSA_PSS_SHA256,
                    SignatureScheme::RSA_PSS_SHA384,
                    SignatureScheme::RSA_PSS_SHA512,
                    SignatureScheme::ED25519,
                    SignatureScheme::ED448,
                ]
            }
        }

        let provider = rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));

        let tls_config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("TLS protocol version error: {e}"))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth();

        let connector = tower::service_fn(move |uri: Uri| {
            let tls_config = tls_config.clone();
            async move {
                let host = uri.host().unwrap_or("localhost").to_string();
                let port = uri.port_u16().unwrap_or(443);
                let tcp = tokio::net::TcpStream::connect((host.as_str(), port)).await?;
                let server_name = rustls::pki_types::ServerName::try_from(host)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
                let tls = tokio_rustls::TlsConnector::from(Arc::new(tls_config));
                let stream = tls.connect(server_name, tcp).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        });

        let channel = endpoint.connect_with_connector(connector).await?;
        Ok(channel)
    }

    /// Get a remote provider instance channel by name.
    ///
    /// Checks the local moka cache first. On cache miss, loads the instance config
    /// from the database and creates a channel lazily. This ensures that provider
    /// instances added on other replicas are visible after the cache TTL expires
    /// (or immediately if a Redis invalidation notification was received).
    ///
    /// Returns:
    /// - `Some(channel)` if the instance exists and is enabled
    /// - `None` if not found or disabled (caller should fallback to singleton local client)
    pub async fn get(&self, name: &str) -> Option<Channel> {
        // Fast path: check cache
        if let Some(channel) = self.channel_cache.get(name).await {
            return Some(channel);
        }

        // Cache miss: try to load from database and create channel lazily
        match self.repository.get_by_name(name).await {
            Ok(Some(config)) if config.enabled => match Self::create_grpc_channel(&config).await {
                Ok(channel) => {
                    self.channel_cache
                        .insert(name.to_string(), channel.clone())
                        .await;
                    tracing::debug!("Lazily created and cached channel for instance '{}'", name);
                    Some(channel)
                }
                Err(e) => {
                    tracing::error!("Failed to create channel for instance '{}': {}", name, e);
                    None
                }
            },
            Ok(_) => {
                // Instance not found or disabled
                None
            }
            Err(e) => {
                tracing::error!("Failed to load provider instance '{}' from DB: {}", name, e);
                None
            }
        }
    }

    /// Resolve a provider client: try remote instance first, fallback to local.
    ///
    /// Encapsulates the common pattern used by all provider adapters:
    /// 1. If `instance_name` is provided and a remote channel exists, call `create_remote`
    /// 2. Otherwise, call `load_local`
    pub async fn resolve_client<T>(
        &self,
        instance_name: Option<&str>,
        create_remote: impl FnOnce(Channel) -> T,
        load_local: impl FnOnce() -> T,
    ) -> T {
        if let Some(name) = instance_name {
            if let Some(channel) = self.get(name).await {
                return create_remote(channel);
            }
        }
        load_local()
    }

    /// Resolve a provider client without silently falling back when an explicit
    /// remote instance name was requested.
    ///
    /// Semantics:
    /// - `instance_name=None`: use the local singleton client.
    /// - `instance_name=Some(name)` and remote exists: use remote client.
    /// - `instance_name=Some(name)` and remote missing/disabled/unreachable:
    ///   return [`ProviderError::InstanceNotFound`] instead of masking the issue
    ///   by falling back to the local singleton.
    pub async fn resolve_client_required<T>(
        &self,
        instance_name: Option<&str>,
        create_remote: impl FnOnce(Channel) -> T,
        load_local: impl FnOnce() -> T,
    ) -> std::result::Result<T, ProviderError> {
        match instance_name {
            Some(name) => self
                .get(name)
                .await
                .map(create_remote)
                .ok_or_else(|| ProviderError::InstanceNotFound(name.to_string())),
            None => Ok(load_local()),
        }
    }

    /// List all remote instance names (from cache + DB)
    ///
    /// Returns the union of cached instances and enabled instances from the DB.
    pub async fn list(&self) -> Vec<String> {
        // Get all enabled instances from DB for a complete picture
        match self.repository.get_all_enabled().await {
            Ok(configs) => configs.into_iter().map(|c| c.name).collect(),
            Err(e) => {
                tracing::warn!("Failed to list provider instances from DB: {e}, using cache only");
                // Fallback to cache keys (moka doesn't expose keys directly, so return empty)
                Vec::new()
            }
        }
    }

    /// Get all provider instances with full metadata
    pub async fn get_all_instances(&self) -> crate::Result<Vec<ProviderInstance>> {
        self.repository
            .get_all()
            .await
            .map_err(|e| crate::Error::Internal(format!("{e}")))
    }

    /// Add a new provider instance
    ///
    /// 1. Creates gRPC connection
    /// 2. Saves to database
    /// 3. Caches the channel locally
    /// 4. Notifies other replicas via Redis
    pub async fn add(&self, config: ProviderInstance) -> crate::Result<()> {
        // Check DB for existing instance (not just local cache)
        if let Ok(Some(_)) = self.repository.get_by_name(&config.name).await {
            return Err(crate::Error::AlreadyExists(format!(
                "Instance '{}' already exists",
                config.name
            )));
        }

        // Create gRPC connection
        let channel = Self::create_grpc_channel(&config).await?;

        // Save to database
        self.repository.create(&config).await?;

        // Cache locally
        self.channel_cache
            .insert(config.name.clone(), channel)
            .await;

        // Notify other replicas
        self.notify_change(&config.name).await;

        tracing::info!("Added provider instance: {}", config.name);
        Ok(())
    }

    /// Update an existing provider instance
    ///
    /// 1. Creates new gRPC connection
    /// 2. Updates database
    /// 3. Replaces cached channel
    /// 4. Notifies other replicas via Redis
    pub async fn update(&self, config: ProviderInstance) -> crate::Result<()> {
        // Create new gRPC connection
        let channel = Self::create_grpc_channel(&config).await?;

        // Update database
        self.repository.update(&config).await?;

        // Replace cached channel
        self.channel_cache
            .insert(config.name.clone(), channel)
            .await;

        // Notify other replicas
        self.notify_change(&config.name).await;

        tracing::info!("Updated provider instance: {}", config.name);
        Ok(())
    }

    /// Delete a provider instance
    ///
    /// 1. Removes from database
    /// 2. Invalidates cached channel
    /// 3. Notifies other replicas via Redis
    pub async fn delete(&self, name: &str) -> crate::Result<()> {
        // Remove from database
        self.repository.delete(name).await?;

        // Invalidate cache
        self.channel_cache.invalidate(name).await;

        // Notify other replicas
        self.notify_change(name).await;

        tracing::info!("Deleted provider instance: {}", name);
        Ok(())
    }

    /// Enable a provider instance
    ///
    /// Loads config from DB, creates channel, caches it, and notifies replicas.
    pub async fn enable(&self, name: &str) -> crate::Result<()> {
        // Update database
        self.repository.enable(name).await?;

        // Reload instance from database and create channel
        let config = self
            .repository
            .get_by_name(name)
            .await?
            .ok_or_else(|| crate::Error::NotFound(format!("Instance '{name}' not found")))?;

        let channel = Self::create_grpc_channel(&config).await?;
        self.channel_cache
            .insert(config.name.clone(), channel)
            .await;

        // Notify other replicas
        self.notify_change(name).await;

        tracing::info!("Enabled provider instance: {}", name);
        Ok(())
    }

    /// Disable a provider instance
    ///
    /// Invalidates cached channel and notifies replicas.
    pub async fn disable(&self, name: &str) -> crate::Result<()> {
        // Update database
        self.repository.disable(name).await?;

        // Invalidate cache
        self.channel_cache.invalidate(name).await;

        // Notify other replicas
        self.notify_change(name).await;

        tracing::info!("Disabled provider instance: {}", name);
        Ok(())
    }

    /// Reconnect a provider instance atomically.
    ///
    /// Invalidates the cached channel and re-creates it from the current DB
    /// config. If the re-creation fails, the instance remains disabled so
    /// callers observe a consistent state rather than a half-connected instance.
    pub async fn reconnect(&self, name: &str) -> crate::Result<()> {
        // Invalidate the cached channel first
        self.channel_cache.invalidate(name).await;

        // Reload config from DB and create a fresh channel
        let config = self
            .repository
            .get_by_name(name)
            .await?
            .ok_or_else(|| crate::Error::NotFound(format!("Instance '{name}' not found")))?;

        if !config.enabled {
            return Err(crate::Error::InvalidInput(format!(
                "Instance '{name}' is disabled; enable it before reconnecting"
            )));
        }

        let channel = Self::create_grpc_channel(&config).await?;
        self.channel_cache
            .insert(config.name.clone(), channel)
            .await;

        // Notify other replicas
        self.notify_change(name).await;

        tracing::info!("Reconnected provider instance: {}", name);
        Ok(())
    }

    /// Health check all remote instances
    ///
    /// Returns a map of instance name to health status.
    /// Uses gRPC Health Check protocol with 5-second timeout per instance.
    ///
    /// Loads the full list from DB to check all instances, not just cached ones.
    pub async fn health_check(&self) -> HashMap<String, bool> {
        let mut results = HashMap::new();

        // Get all enabled instances from DB for complete health check
        let configs = match self.repository.get_all_enabled().await {
            Ok(configs) => configs,
            Err(e) => {
                tracing::error!("Failed to load instances for health check: {e}");
                return results;
            }
        };

        for config in configs {
            // Try to get channel from cache or create it
            let channel = match self.get(&config.name).await {
                Some(ch) => ch,
                None => continue,
            };

            let is_healthy = self.check_instance_health(&config.name, channel).await;
            results.insert(config.name, is_healthy);
        }

        results
    }

    /// Check health of a single remote instance
    ///
    /// Calls gRPC Health Check RPC with 5-second timeout.
    async fn check_instance_health(&self, name: &str, channel: Channel) -> bool {
        // Create health check client
        let mut client = HealthClient::new(channel);

        // Create health check request (empty service name checks overall health)
        let request = tonic::Request::new(HealthCheckRequest {
            service: String::new(), // Empty string = overall server health
        });

        // Set timeout for health check (5 seconds)
        let timeout = Duration::from_secs(5);

        match tokio::time::timeout(timeout, client.check(request)).await {
            Ok(Ok(response)) => {
                // Check if status is SERVING (1)
                let status = response.into_inner().status;
                let is_serving = status == 1; // tonic_health::ServingStatus::Serving

                if is_serving {
                    tracing::debug!("Provider instance '{}' is healthy", name);
                } else {
                    tracing::warn!(
                        "Provider instance '{}' is not serving (status: {})",
                        name,
                        status
                    );
                }

                is_serving
            }
            Ok(Err(e)) => {
                tracing::error!("Health check failed for instance '{}': {}", name, e);
                false
            }
            Err(_) => {
                tracing::error!("Health check timeout for instance '{}' (5s)", name);
                false
            }
        }
    }
}
