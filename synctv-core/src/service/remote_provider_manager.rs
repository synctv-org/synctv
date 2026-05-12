// Remote Provider Manager
// Manages remote provider instances (gRPC connections).
// Supports both local (in-process) and remote (gRPC) provider instances.
// ## Multi-replica support
// Instead of maintaining a persistent channel map that is invisible across replicas,
// channels are created lazily on demand and cached with a TTL. When a provider
// operation is needed, the manager looks up the instance config from the DB and
// creates a channel if not already cached.
// Provider changes (add/update/delete/enable/disable) are broadcast via the
// shared durable cache invalidation stream so other replicas can invalidate
// their local channel cache even across restarts and transient disconnects.

use crate::cache::{CacheInvalidationRuntime, InvalidationMessage};
use crate::models::{validate_provider_instance_name, ProviderInstance, ProviderInstanceListQuery};
use crate::provider::provider_client::{validate_auth_secret, RemoteProviderConnection};
use crate::provider::{AlistProvider, BilibiliProvider, EmbyProvider, ProviderError};
use crate::repository::ProviderInstanceRepository;
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use synctv_common::ExecutionControl;
use tokio::task::JoinHandle;
#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
use tonic::transport::{Certificate, ClientTlsConfig};
use tonic::transport::{Channel, Endpoint, Uri};
use tonic_health::pb::{health_client::HealthClient, HealthCheckRequest};

#[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
fn apply_default_grpc_roots(mut tls_config: ClientTlsConfig) -> ClientTlsConfig {
    #[cfg(feature = "tls-webpki-roots")]
    {
        tls_config = tls_config.with_webpki_roots();
    }

    #[cfg(feature = "tls-native-roots")]
    {
        tls_config = tls_config.with_native_roots();
    }

    tls_config
}

/// Default channel cache TTL (5 minutes)
const CHANNEL_CACHE_TTL_SECS: u64 = 300;

/// Maximum number of cached channels
const MAX_CACHED_CHANNELS: u64 = 1_000;

/// Match the remote provider server's HTTP/2 frame budget for large provider
/// directory/listing responses.
const PROVIDER_GRPC_FRAME_SIZE_LIMIT: u32 = 4 * 1024 * 1024;

/// Remote Provider Manager
///
/// Manages remote provider instances (gRPC connections).
/// When no remote instance is found, providers fallback to singleton local clients.
///
/// ## Multi-replica architecture
///
/// - Channels are created lazily from DB config and cached with TTL via moka
/// - `get(name)` looks up the cached channel or creates one from DB on cache miss
/// - Provider mutations publish durable invalidation events through `CacheInvalidationService`
/// - A background subscriber listens for invalidation messages and evicts stale entries
pub struct RemoteProviderManager {
    /// Lazily-populated channel cache with TTL (indexed by instance name)
    channel_cache: Arc<moka::future::Cache<String, RemoteProviderConnection>>,

    /// Repository for database operations
    repository: Arc<ProviderInstanceRepository>,

    /// Shared durable invalidation bus for cross-replica cache invalidation.
    cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,

    /// Cancellation token for the provider invalidation listener.
    invalidation_cancel: tokio_util::sync::CancellationToken,

    /// Provider invalidation listener task handle.
    invalidation_listener_task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,

    /// Exact-host address overrides used by tests to route synthetic hostnames
    /// to in-process servers without weakening the production SSRF policy.
    address_overrides: Arc<HashMap<String, SocketAddr>>,
}

impl std::fmt::Debug for RemoteProviderManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteProviderManager")
            .field("invalidation_enabled", &self.cache_invalidation.is_some())
            .finish()
    }
}

impl RemoteProviderManager {
    const SUPPORTED_REMOTE_PROVIDERS: &'static [&'static str] = &[
        AlistProvider::NAME,
        EmbyProvider::NAME,
        BilibiliProvider::NAME,
    ];

    fn probe_execution_control(
        control: Option<&ExecutionControl>,
        timeout: Duration,
    ) -> ExecutionControl {
        let probe_deadline = std::time::Instant::now() + timeout;
        match control {
            Some(control) => {
                let deadline = control
                    .deadline()
                    .map_or(probe_deadline, |deadline| deadline.min(probe_deadline));
                ExecutionControl::from_parts(Some(deadline), control.cancellation_token())
            }
            None => ExecutionControl::from_timeout(Some(timeout)),
        }
    }

    fn provider_registry_unavailable(
        operation: &'static str,
        error: impl std::fmt::Display,
    ) -> crate::Error {
        tracing::warn!(operation, error = %error, "Provider registry unavailable");
        crate::Error::ServiceUnavailable(
            "Provider configuration service is temporarily unavailable.".to_string(),
        )
    }

    async fn run_with_control<T, F>(
        control: Option<&ExecutionControl>,
        future: F,
    ) -> crate::Result<T>
    where
        F: Future<Output = crate::Result<T>>,
    {
        match control {
            Some(control) => control.run(future).await.map_err(crate::Error::from)?,
            None => future.await,
        }
    }

    fn provider_connection_setup_error(
        message: &'static str,
        error: impl std::fmt::Display,
    ) -> crate::Error {
        tracing::error!(error = %error, "{message}");
        crate::Error::Internal(message.to_string())
    }

    fn map_remote_resolution_error(err: crate::Error) -> ProviderError {
        match err {
            crate::Error::InvalidInput(msg) => ProviderError::InvalidConfig(msg),
            crate::Error::Timeout(msg) => ProviderError::NetworkError(msg),
            crate::Error::ServiceUnavailable(msg) | crate::Error::RateLimited(msg) => {
                ProviderError::ApiError(msg)
            }
            crate::Error::Database(error) => {
                tracing::warn!(error = %error, "Provider configuration store unavailable");
                ProviderError::ApiError(
                    "Provider configuration service is temporarily unavailable.".to_string(),
                )
            }
            crate::Error::Redis(error) => {
                tracing::warn!(error = %error, "Provider configuration cache unavailable");
                ProviderError::ApiError(
                    "Provider configuration service is temporarily unavailable.".to_string(),
                )
            }
            crate::Error::Serialization(err) => {
                ProviderError::Internal(format!("Serialization error: {err}"))
            }
            crate::Error::Deserialization { context } => {
                ProviderError::Internal(format!("Deserialization error: {context}"))
            }
            crate::Error::Authentication(msg) => ProviderError::Internal(format!(
                "Unexpected authentication error while resolving provider instance: {msg}"
            )),
            crate::Error::EmailNotVerified => ProviderError::Internal(
                "Unexpected email verification error while resolving provider instance".to_string(),
            ),
            crate::Error::Authorization(msg) => ProviderError::Internal(format!(
                "Unexpected authorization error while resolving provider instance: {msg}"
            )),
            crate::Error::NotFound(msg) => ProviderError::Internal(format!(
                "Unexpected not found error while resolving provider instance: {msg}"
            )),
            crate::Error::AlreadyExists(msg) => ProviderError::Internal(format!(
                "Unexpected already exists error while resolving provider instance: {msg}"
            )),
            crate::Error::Internal(msg) => ProviderError::Internal(msg),
            crate::Error::OptimisticLockConflict => {
                ProviderError::Internal("Optimistic lock conflict".to_string())
            }
            crate::Error::LockConflict(msg) => ProviderError::Internal(format!(
                "Distributed lock conflict while resolving provider instance: {msg}"
            )),
        }
    }

    fn provider_error_from_ref(error: &ProviderError) -> ProviderError {
        match error {
            ProviderError::InvalidUrl(message) => ProviderError::InvalidUrl(message.clone()),
            ProviderError::InvalidConfig(message) => ProviderError::InvalidConfig(message.clone()),
            ProviderError::MissingField(message) => ProviderError::MissingField(message.clone()),
            ProviderError::NetworkError(message) => ProviderError::NetworkError(message.clone()),
            ProviderError::AuthRequired => ProviderError::AuthRequired,
            ProviderError::CredentialRequired => ProviderError::CredentialRequired,
            ProviderError::InvalidCredentialType => ProviderError::InvalidCredentialType,
            ProviderError::Authentication(message) => {
                ProviderError::Authentication(message.clone())
            }
            ProviderError::NotFound => ProviderError::NotFound,
            ProviderError::ApiError(message) => ProviderError::ApiError(message.clone()),
            ProviderError::UpstreamHttp { status, url } => ProviderError::UpstreamHttp {
                status: *status,
                url: url.clone(),
            },
            ProviderError::UnsupportedFormat(format) => {
                ProviderError::UnsupportedFormat(format.clone())
            }
            ProviderError::ParseError(message) => ProviderError::ParseError(message.clone()),
            ProviderError::MissingInstance => ProviderError::MissingInstance,
            ProviderError::InstanceNotFound(name) => ProviderError::InstanceNotFound(name.clone()),
            ProviderError::CredentialNotFound(message) => {
                ProviderError::CredentialNotFound(message.clone())
            }
            ProviderError::CredentialExpired(message) => {
                ProviderError::CredentialExpired(message.clone())
            }
            ProviderError::EncryptionRequired(provider) => {
                ProviderError::EncryptionRequired(provider)
            }
            ProviderError::RouteRegistrationFailed(message) => {
                ProviderError::RouteRegistrationFailed(message.clone())
            }
            ProviderError::Internal(message) => ProviderError::Internal(message.clone()),
            ProviderError::IoError(error) => {
                ProviderError::IoError(std::io::Error::new(error.kind(), error.to_string()))
            }
            ProviderError::JsonError(error) => ProviderError::JsonError(serde_json::Error::io(
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()),
            )),
        }
    }

    async fn get_connection_result(
        &self,
        name: &str,
    ) -> std::result::Result<Option<RemoteProviderConnection>, ProviderError> {
        enum LoadError {
            NotCacheable,
            Provider(ProviderError),
        }

        let loaded = self
            .channel_cache
            .try_get_with_by_ref(name, async {
                let Some(config) = self.repository.get_by_name(name).await.map_err(|error| {
                    LoadError::Provider(Self::map_remote_resolution_error(error))
                })?
                else {
                    return Err(LoadError::NotCacheable);
                };

                if !config.enabled || !Self::requires_remote_connection(&config) {
                    return Err(LoadError::NotCacheable);
                }

                let channel = self.create_grpc_channel(&config).await.map_err(|error| {
                    LoadError::Provider(Self::map_remote_resolution_error(error))
                })?;
                let connection =
                    Self::build_remote_connection(&config, channel).map_err(|error| {
                        LoadError::Provider(Self::map_remote_resolution_error(error))
                    })?;

                tracing::debug!(
                    "Lazily created and cached channel for instance '{}'",
                    config.name
                );
                Ok(connection)
            })
            .await;

        match loaded {
            Ok(connection) => Ok(Some(connection)),
            Err(error) => match Arc::try_unwrap(error) {
                Ok(LoadError::NotCacheable) => Ok(None),
                Ok(LoadError::Provider(error)) => Err(error),
                Err(error) => match error.as_ref() {
                    LoadError::NotCacheable => Ok(None),
                    LoadError::Provider(error) => Err(Self::provider_error_from_ref(error)),
                },
            },
        }
    }

    /// Create a new `RemoteProviderManager` without shared durable invalidation.
    #[must_use]
    pub fn new(repository: Arc<ProviderInstanceRepository>) -> Self {
        Self::new_with_invalidation(repository, None)
    }

    /// Create a new `RemoteProviderManager` with the shared durable invalidation service.
    #[must_use]
    pub fn new_with_invalidation(
        repository: Arc<ProviderInstanceRepository>,
        cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
    ) -> Self {
        Self::new_with_options(repository, cache_invalidation, HashMap::new())
    }

    #[must_use]
    pub fn new_with_address_overrides(
        repository: Arc<ProviderInstanceRepository>,
        cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
        address_overrides: HashMap<String, SocketAddr>,
    ) -> Self {
        Self::new_with_options(repository, cache_invalidation, address_overrides)
    }

    fn new_with_options(
        repository: Arc<ProviderInstanceRepository>,
        cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
        address_overrides: HashMap<String, SocketAddr>,
    ) -> Self {
        if cache_invalidation.is_none() {
            tracing::warn!(
                "RemoteProviderManager using local-only cache invalidation. \
                 For multi-replica setups, configure durable cache invalidation for cross-replica sync."
            );
        }
        let channel_cache = moka::future::Cache::builder()
            .max_capacity(MAX_CACHED_CHANNELS)
            .time_to_live(Duration::from_secs(CHANNEL_CACHE_TTL_SECS))
            .build();

        Self {
            channel_cache: Arc::new(channel_cache),
            repository,
            cache_invalidation,
            invalidation_cancel: tokio_util::sync::CancellationToken::new(),
            invalidation_listener_task: Arc::new(tokio::sync::Mutex::new(None)),
            address_overrides: Arc::new(address_overrides),
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
            if !Self::requires_remote_connection(&config) {
                tracing::debug!(
                    "Skipping remote channel pre-warm for local-only provider instance: {}",
                    config.name
                );
                continue;
            }

            Self::validate_config(&config)?;

            match self.create_grpc_channel(&config).await {
                Ok(channel) => match Self::build_remote_connection(&config, channel) {
                    Ok(connection) => {
                        self.channel_cache
                            .insert(config.name.clone(), connection)
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
                },
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

    /// Start the durable provider invalidation subscriber for cross-replica cache invalidation.
    ///
    /// Subscribes to the shared `CacheInvalidationService`, which is backed by
    /// Redis Streams in cluster mode and therefore replays pending invalidations
    /// after reconnect/restart.
    /// Returns immediately if cross-replica invalidation is not configured.
    pub async fn start_invalidation_listener(&self) -> crate::Result<()> {
        let Some(ref invalidation_service) = self.cache_invalidation else {
            tracing::debug!("No durable invalidation service configured, skipping listener");
            return Ok(());
        };

        let mut guard = self.invalidation_listener_task.lock().await;
        if guard.is_some() {
            tracing::debug!("Provider invalidation listener already running");
            return Ok(());
        }

        let cache = Arc::clone(&self.channel_cache);
        let cancel = self.invalidation_cancel.child_token();
        let mut receiver = invalidation_service.subscribe();

        // The shared invalidation service may already have consumed durable
        // stream entries before this manager attaches its local broadcast
        // receiver. Drop all cached channels now so the next access reloads the
        // latest DB state instead of serving a stale pre-listener snapshot.
        self.channel_cache.invalidate_all();

        let handle = crate::spawn::spawn_monitored("provider_invalidation_listener", async move {
            loop {
                tokio::select! {
                    () = cancel.cancelled() => {
                        tracing::info!("Provider invalidation listener shutting down");
                        break;
                    }
                    result = receiver.recv() => {
                        match result {
                            Ok(InvalidationMessage::ProviderInstance { instance_name }) => {
                                tracing::info!(
                                    "Received provider change notification for '{}', invalidating cache",
                                    instance_name
                                );
                                cache.invalidate(&instance_name).await;
                            }
                            Ok(_) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                tracing::warn!(
                                    "cache invalidation service closed provider invalidation subscription"
                                );
                                break;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                tracing::warn!(
                                    skipped,
                                    "Provider invalidation listener lagged; invalidating all cached provider channels"
                                );
                                cache.invalidate_all();
                            }
                        }
                    }
                }
            }
        });
        *guard = Some(handle);
        drop(guard);

        tracing::info!("Provider instance cache invalidation listener started (durable stream)");
        Ok(())
    }

    /// Cancel and join the provider invalidation listener.
    pub async fn shutdown(&self) {
        self.invalidation_cancel.cancel();

        let mut guard = self.invalidation_listener_task.lock().await;
        if let Some(handle) = guard.take() {
            let _ = handle.await;
        }
    }

    #[must_use]
    pub fn invalidation_listener_task(&self) -> Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>> {
        Arc::clone(&self.invalidation_listener_task)
    }

    #[must_use]
    pub fn invalidation_cancel_token(&self) -> tokio_util::sync::CancellationToken {
        self.invalidation_cancel.clone()
    }

    /// Publish a durable cache invalidation notification so other replicas
    /// evict the stale entry for `instance_name`.
    async fn notify_change(
        &self,
        operation: &'static str,
        instance_name: &str,
    ) -> crate::Result<()> {
        let Some(ref invalidation_service) = self.cache_invalidation else {
            return Ok(());
        };

        invalidation_service
            .invalidate_provider_instance(instance_name)
            .await
            .map_err(|e| {
                tracing::error!(
                    operation,
                    instance_name,
                    error = %e,
                    "Failed to publish provider change notification"
                );
                crate::Error::ServiceUnavailable(format!(
                    "Failed to publish provider invalidation for {operation} '{instance_name}': {e}"
                ))
            })
    }

    async fn restore_cached_connection(
        &self,
        instance_name: &str,
        previous_connection: Option<RemoteProviderConnection>,
    ) {
        if let Some(connection) = previous_connection {
            self.channel_cache
                .insert(instance_name.to_string(), connection)
                .await;
        } else {
            self.channel_cache.invalidate(instance_name).await;
        }
    }

    fn rollback_failure(
        operation: &'static str,
        instance_name: &str,
        notify_error: &crate::Error,
        rollback_error: &crate::Error,
    ) -> crate::Error {
        crate::Error::Internal(format!(
            "Failed to roll back provider instance {operation} for '{instance_name}' after invalidation publish failure. publish_error: {notify_error}; rollback_error: {rollback_error}"
        ))
    }

    /// Validate endpoint URL structure and apply the configured runtime SSRF
    /// policy to hostnames and IP literals.
    ///
    /// Only validates hostnames and IP literals statically. Does NOT resolve DNS,
    /// because DNS results can change between validation and connection (DNS rebinding),
    /// and VPN/proxy environments may return unexpected IPs for public hostnames.
    /// SyncTV's default policy is permissive for self-hosted/private deployments;
    /// strict private-address blocking only happens when the shared guard is strict.
    fn validate_endpoint_ssrf(endpoint: &str) -> crate::Result<()> {
        let url = url::Url::parse(endpoint).map_err(|e| {
            crate::Error::InvalidInput(format!("SSRF validation: invalid URL: {e}"))
        })?;

        match url.scheme() {
            "http" | "https" => {}
            scheme => {
                return Err(crate::Error::InvalidInput(format!(
                    "Remote provider endpoint scheme '{scheme}' is not supported; use http:// for plaintext gRPC, or https:// for TLS"
                )))
            }
        }

        let host = url.host_str().ok_or_else(|| {
            crate::Error::InvalidInput("SSRF validation: missing host".to_string())
        })?;

        let guard = synctv_common::ssrf::SsrfGuard::shared_default();

        // Check if the configured policy blocks the hostname itself.
        if guard.is_host_blocked(host) {
            return Err(crate::Error::InvalidInput(format!(
                "SSRF validation: host '{host}' is blocked (internal/reserved)"
            )));
        }

        // If the host is an IP address, check it directly against the configured policy.
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            if guard.is_ip_blocked(&ip) {
                return Err(crate::Error::InvalidInput(format!(
                    "SSRF validation: IP '{ip}' is blocked (internal/private)"
                )));
            }
        }
        // Note: For hostnames, we do NOT resolve DNS here. DNS results can change
        // between validation and connection; the gRPC transport applies the same
        // configured policy to resolved addresses at connection time.

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

    fn normalized_transport_endpoint(config: &ProviderInstance) -> crate::Result<String> {
        let url = url::Url::parse(&config.endpoint).map_err(|e| {
            crate::Error::InvalidInput(format!("Remote provider endpoint is invalid: {e}"))
        })?;

        let normalized_scheme = match url.scheme() {
            "http" => "http",
            "https" => "https",
            scheme => {
                return Err(crate::Error::InvalidInput(format!(
                    "Remote provider endpoint scheme '{scheme}' is not supported; use http:// for plaintext gRPC, or https:// for TLS"
                )))
            }
        };

        let host = url.host_str().ok_or_else(|| {
            crate::Error::InvalidInput("Remote provider endpoint is missing host".to_string())
        })?;
        let mut normalized = format!("{normalized_scheme}://{host}");
        if let Some(port) = url.port() {
            normalized.push(':');
            normalized.push_str(&port.to_string());
        }
        let path = url.path();
        if !path.is_empty() && path != "/" {
            normalized.push_str(path);
        }
        if let Some(query) = url.query() {
            normalized.push('?');
            normalized.push_str(query);
        }

        Ok(normalized)
    }

    /// Validate endpoint and timeout without creating or connecting a channel.
    fn validate_config(config: &ProviderInstance) -> crate::Result<()> {
        validate_provider_instance_name(&config.name).map_err(crate::Error::InvalidInput)?;
        config.parse_timeout().map_err(crate::Error::Internal)?;
        for provider in &config.providers {
            if !Self::is_supported_remote_provider(provider) {
                return Err(crate::Error::InvalidInput(format!(
                    "Remote provider instance '{}' declares unsupported provider '{}'; supported providers are: {}",
                    config.name,
                    provider,
                    Self::SUPPORTED_REMOTE_PROVIDERS.join(", ")
                )));
            }
        }
        if Self::requires_remote_connection(config) {
            Self::validate_endpoint_ssrf(&config.endpoint)?;
            let endpoint = url::Url::parse(&config.endpoint).map_err(|e| {
                crate::Error::InvalidInput(format!("Remote provider endpoint is invalid: {e}"))
            })?;

            match (endpoint.scheme(), config.tls) {
                ("https", false) => {
                    return Err(crate::Error::InvalidInput(format!(
                        "Remote provider endpoint '{}' requires tls=true to match its https:// scheme",
                        config.endpoint
                    )));
                }
                ("http", true) => {
                    return Err(crate::Error::InvalidInput(format!(
                        "Remote provider endpoint '{}' requires tls=false to match its {}:// scheme",
                        config.endpoint,
                        endpoint.scheme()
                    )));
                }
                _ => {}
            }

            if config.insecure_tls && !config.tls {
                return Err(crate::Error::InvalidInput(
                    "insecure_tls=true requires tls=true for remote provider instances".to_string(),
                ));
            }

            if config.custom_ca.is_some() && !config.tls {
                return Err(crate::Error::InvalidInput(
                    "custom_ca requires tls=true for remote provider instances".to_string(),
                ));
            }
            validate_auth_secret(Some(Self::required_auth_secret(config)?))
                .map_err(|e| crate::Error::InvalidInput(e.to_string()))?;
        }
        Ok(())
    }

    fn requires_remote_connection(config: &ProviderInstance) -> bool {
        config
            .providers
            .iter()
            .any(|provider| Self::is_supported_remote_provider(provider))
    }

    fn is_supported_remote_provider(provider: &str) -> bool {
        let trimmed = provider.trim();
        Self::SUPPORTED_REMOTE_PROVIDERS.contains(&trimmed)
    }

    fn required_auth_secret(config: &ProviderInstance) -> crate::Result<&str> {
        let secret = config.jwt_secret.as_deref().ok_or_else(|| {
            crate::Error::InvalidInput(format!(
                "Remote provider instance '{}' requires a non-empty jwt_secret",
                config.name
            ))
        })?;
        let trimmed = secret.trim();
        if trimmed.is_empty() {
            return Err(crate::Error::InvalidInput(format!(
                "Remote provider instance '{}' requires a non-empty jwt_secret",
                config.name
            )));
        }
        Ok(trimmed)
    }

    fn build_remote_connection(
        config: &ProviderInstance,
        channel: Channel,
    ) -> crate::Result<RemoteProviderConnection> {
        let auth_secret = validate_auth_secret(Some(Self::required_auth_secret(config)?))
            .map_err(|e| crate::Error::InvalidInput(e.to_string()))?;
        Ok(RemoteProviderConnection::new(channel, auth_secret))
    }

    async fn build_management_validated_remote_connection_with_control(
        &self,
        config: &ProviderInstance,
        control: Option<&ExecutionControl>,
    ) -> crate::Result<RemoteProviderConnection> {
        let channel = Box::pin(Self::run_with_control(
            control,
            self.create_grpc_channel(config),
        ))
        .await?;
        let connection = Self::build_remote_connection(config, channel)?;
        self.validate_management_connection_with_control(config, &connection, control)
            .await?;
        Ok(connection)
    }

    async fn validate_management_connection_with_control(
        &self,
        config: &ProviderInstance,
        connection: &RemoteProviderConnection,
        control: Option<&ExecutionControl>,
    ) -> crate::Result<()> {
        let mut client = HealthClient::new(connection.channel());
        let mut request = tonic::Request::new(HealthCheckRequest {
            service: String::new(),
        });
        let secret = connection.auth_secret().ok_or_else(|| {
            crate::Error::InvalidInput(format!(
                "Remote provider instance '{}' requires a non-empty jwt_secret for health checks",
                config.name
            ))
        })?;
        let metadata_value = secret.parse().map_err(|e| {
            crate::Error::InvalidInput(format!(
                "Remote provider instance '{}' jwt_secret must be valid ASCII gRPC metadata: {e}",
                config.name
            ))
        })?;
        request
            .metadata_mut()
            .insert("x-provider-secret", metadata_value);
        let timeout = Duration::from_secs(5);
        let control = Self::probe_execution_control(control, timeout);

        let response = control
            .run(client.check(request))
            .await
            .map_err(|err| match err {
                synctv_common::ExecutionControlError::DeadlineExceeded => {
                    crate::Error::InvalidInput(format!(
                        "Remote provider instance '{}' connectivity validation timed out after {}s",
                        config.name,
                        timeout.as_secs()
                    ))
                }
                other => crate::Error::from(other),
            })?
            .map_err(|status| {
                crate::Error::InvalidInput(format!(
                    "Remote provider instance '{}' health check failed: {status}",
                    config.name
                ))
            })?;

        let status = response.into_inner().status;
        if status != 1 {
            return Err(crate::Error::InvalidInput(format!(
                "Remote provider instance '{}' is not serving (health status: {status})",
                config.name
            )));
        }

        Ok(())
    }

    fn resolve_ssrf_validated_address(
        address_overrides: Arc<HashMap<String, SocketAddr>>,
        uri: &Uri,
        guard: &synctv_common::ssrf::SsrfGuard,
    ) -> impl std::future::Future<Output = std::io::Result<(String, std::net::SocketAddr)>> + Send
    {
        let uri = uri.clone();
        let guard = guard.clone();
        async move {
            let host = uri.host().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing host")
            })?;

            if let Some(address) = address_overrides.get(host).copied() {
                tracing::debug!(
                    host,
                    ip = %address.ip(),
                    port = address.port(),
                    "Connecting to remote provider via explicit test address override"
                );
                return Ok((host.to_string(), address));
            }

            if guard.is_host_blocked(host) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("SSRF validation: host '{host}' is blocked at connection time"),
                ));
            }

            let port = uri.port_u16().unwrap_or_else(|| {
                if uri.scheme_str() == Some("https") {
                    443
                } else {
                    80
                }
            });

            let mut resolved = tokio::net::lookup_host((host, port)).await?;
            let address = resolved.find(|addr| {
                let blocked = guard.is_ip_blocked(&addr.ip());
                if blocked {
                    tracing::warn!(
                        host,
                        ip = %addr.ip(),
                        "Blocked remote provider connection due to SSRF policy during DNS resolution"
                    );
                }
                !blocked
            });

            let address = address.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("SSRF validation: all resolved addresses for '{host}' are blocked"),
                )
            })?;

            tracing::debug!(
                host,
                ip = %address.ip(),
                port = address.port(),
                "Connecting to remote provider after SSRF DNS validation"
            );

            Ok((host.to_string(), address))
        }
    }

    /// Create a gRPC channel for the given provider instance
    ///
    /// Establishes gRPC connection with configured TLS settings, timeout, and middleware.
    async fn create_grpc_channel(&self, config: &ProviderInstance) -> crate::Result<Channel> {
        // Apply the configured SSRF policy to the endpoint before connecting.
        Self::validate_endpoint_ssrf(&config.endpoint)?;

        // Parse timeout
        let timeout = config.parse_timeout().map_err(crate::Error::Internal)?;

        // Create endpoint
        let transport_endpoint = Self::normalized_transport_endpoint(config)?;
        let endpoint = Endpoint::from_shared(transport_endpoint)
            .map_err(|error| {
                Self::provider_connection_setup_error(
                    "Remote provider endpoint configuration is invalid.",
                    error,
                )
            })?
            .timeout(timeout)
            .max_frame_size(PROVIDER_GRPC_FRAME_SIZE_LIMIT)
            .tcp_keepalive(Some(Duration::from_secs(30)))
            .http2_keep_alive_interval(Duration::from_secs(30))
            .keep_alive_timeout(Duration::from_secs(10));
        #[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
        let mut endpoint = endpoint;

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
                let channel = self.connect_insecure_tls(endpoint).await.map_err(|error| {
                    Self::provider_connection_setup_error(
                        "Remote provider TLS connection setup failed.",
                        error,
                    )
                })?;

                tracing::info!(
                    "Established insecure-TLS gRPC connection to {} (timeout: {:?})",
                    config.endpoint,
                    timeout,
                );

                return Ok(channel);
            }

            #[cfg(any(feature = "tls-webpki-roots", feature = "tls-native-roots"))]
            {
                let mut tls_config = ClientTlsConfig::new();

                // Use custom CA certificate if provided
                if let Some(ref ca_pem) = config.custom_ca {
                    let cert = Certificate::from_pem(ca_pem);
                    tls_config = tls_config.ca_certificate(cert);
                } else {
                    tls_config = apply_default_grpc_roots(tls_config);
                }

                endpoint = endpoint.tls_config(tls_config).map_err(|error| {
                    Self::provider_connection_setup_error(
                        "Remote provider TLS connection setup failed.",
                        error,
                    )
                })?;
            }

            #[cfg(not(any(feature = "tls-webpki-roots", feature = "tls-native-roots")))]
            {
                return Err(crate::Error::InvalidInput(
                    "Remote provider TLS requires a TLS root feature".to_string(),
                ));
            }
        }

        let guard = synctv_common::ssrf::SsrfGuard::shared_default();
        let address_overrides = Arc::clone(&self.address_overrides);
        let connector = tower::service_fn(move |uri: Uri| {
            let guard = guard.clone();
            let address_overrides = address_overrides.clone();
            async move {
                let (_, address) =
                    Self::resolve_ssrf_validated_address(address_overrides, &uri, &guard).await?;
                let stream = tokio::net::TcpStream::connect(address).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        });

        // Create lazy gRPC channel (connects on first use, not eagerly).
        // The custom connector re-validates DNS resolution at connection time to
        // block hostname-based SSRF and DNS rebinding.
        let channel = endpoint.connect_with_connector_lazy(connector);

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
        &self,
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

        let guard = synctv_common::ssrf::SsrfGuard::shared_default();
        let address_overrides = Arc::clone(&self.address_overrides);

        let tls_config = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth();

        let connector = tower::service_fn(move |uri: Uri| {
            let tls_config = tls_config.clone();
            let guard = guard.clone();
            let address_overrides = address_overrides.clone();
            async move {
                let (host, address) =
                    Self::resolve_ssrf_validated_address(address_overrides, &uri, &guard).await?;
                let tcp = tokio::net::TcpStream::connect(address).await?;
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
    pub async fn get(&self, name: &str) -> Option<RemoteProviderConnection> {
        match self.get_connection_result(name).await {
            Ok(connection) => connection,
            Err(err) => {
                tracing::error!(
                    "Failed to resolve remote provider instance '{}' for optional fallback path: {}",
                    name,
                    err
                );
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
        create_remote: impl FnOnce(RemoteProviderConnection) -> T,
        load_local: impl FnOnce() -> T,
    ) -> T {
        if let Some(name) = instance_name {
            if let Some(connection) = self.get(name).await {
                return create_remote(connection);
            }
        }
        load_local()
    }

    /// Resolve a provider client with a cooperative request context for remote calls.
    pub async fn resolve_client_with_context<T>(
        &self,
        instance_name: Option<&str>,
        request_context: Option<&crate::provider::ExecutionControl>,
        create_remote: impl FnOnce(RemoteProviderConnection) -> T,
        load_local: impl FnOnce() -> T,
    ) -> T {
        if let Some(name) = instance_name {
            if let Some(connection) = self.get(name).await {
                return create_remote(connection.with_request_context(request_context.cloned()));
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
    /// - `instance_name=Some(name)` and remote missing/disabled: return
    ///   [`ProviderError::InstanceNotFound`] instead of masking the issue by
    ///   falling back to the local singleton.
    /// - `instance_name=Some(name)` and remote resolution/config loading fails:
    ///   surface the underlying provider/config/internal error.
    pub async fn resolve_client_required<T>(
        &self,
        instance_name: Option<&str>,
        create_remote: impl FnOnce(RemoteProviderConnection) -> T,
        load_local: impl FnOnce() -> T,
    ) -> std::result::Result<T, ProviderError> {
        match instance_name {
            Some(name) => self
                .get_connection_result(name)
                .await?
                .map(create_remote)
                .ok_or_else(|| ProviderError::InstanceNotFound(name.to_string())),
            None => Ok(load_local()),
        }
    }

    /// Resolve a provider client without silent fallback and attach a cooperative request context.
    pub async fn resolve_client_required_with_context<T>(
        &self,
        instance_name: Option<&str>,
        request_context: Option<&crate::provider::ExecutionControl>,
        create_remote: impl FnOnce(RemoteProviderConnection) -> T,
        load_local: impl FnOnce() -> T,
    ) -> std::result::Result<T, ProviderError> {
        if let Some(request_context) = request_context {
            request_context
                .check_active()
                .map_err(|err| ProviderError::NetworkError(err.to_string()))?;
        }

        match instance_name {
            Some(name) => self
                .get_connection_result(name)
                .await?
                .map(|connection| {
                    if let Some(request_context) = request_context {
                        request_context
                            .check_active()
                            .map_err(|err| ProviderError::NetworkError(err.to_string()))?;
                    }
                    Ok::<T, ProviderError>(create_remote(
                        connection.with_request_context(request_context.cloned()),
                    ))
                })
                .transpose()?
                .ok_or_else(|| ProviderError::InstanceNotFound(name.to_string())),
            None => Ok(load_local()),
        }
    }

    /// List all remote instance names (from cache + DB)
    ///
    /// Returns the union of cached instances and enabled instances from the DB.
    pub async fn list(&self) -> crate::Result<Vec<String>> {
        self.repository
            .get_all_enabled()
            .await
            .map(|configs| configs.into_iter().map(|c| c.name).collect())
            .map_err(|e| Self::provider_registry_unavailable("list enabled instances", e))
    }

    /// Get all provider instances with full metadata
    pub async fn get_all_instances(&self) -> crate::Result<Vec<ProviderInstance>> {
        self.repository
            .get_all()
            .await
            .map_err(|e| Self::provider_registry_unavailable("get all instances", e))
    }

    pub async fn get_instance(&self, name: &str) -> crate::Result<Option<ProviderInstance>> {
        self.repository
            .get_by_name(name)
            .await
            .map_err(|e| Self::provider_registry_unavailable("get instance by name", e))
    }

    pub async fn list_instances(
        &self,
        query: &ProviderInstanceListQuery,
    ) -> crate::Result<Vec<ProviderInstance>> {
        self.repository
            .list(query)
            .await
            .map_err(|e| Self::provider_registry_unavailable("list instances", e))
    }

    pub async fn find_instances_by_provider(
        &self,
        provider: &str,
    ) -> crate::Result<Vec<ProviderInstance>> {
        self.repository
            .find_by_provider(provider)
            .await
            .map_err(|e| Self::provider_registry_unavailable("find instances by provider", e))
    }

    /// Add a new provider instance
    ///
    /// 1. Creates gRPC connection
    /// 2. Saves to database
    /// 3. Caches the channel locally
    /// 4. Notifies other replicas via Redis
    pub async fn add(&self, config: ProviderInstance) -> crate::Result<()> {
        Box::pin(self.add_with_control(config, None)).await
    }

    pub async fn add_with_control(
        &self,
        config: ProviderInstance,
        control: Option<&ExecutionControl>,
    ) -> crate::Result<()> {
        Self::validate_config(&config)?;

        let connection = if config.enabled && Self::requires_remote_connection(&config) {
            Some(
                Box::pin(
                    self.build_management_validated_remote_connection_with_control(
                        &config, control,
                    ),
                )
                .await?,
            )
        } else {
            None
        };

        // Save to database
        self.repository.create(&config).await?;

        if let Some(connection) = connection {
            self.channel_cache
                .insert(config.name.clone(), connection)
                .await;
        } else {
            self.channel_cache.invalidate(&config.name).await;
        }

        // Notify other replicas
        if let Err(notify_error) = self.notify_change("add", &config.name).await {
            if let Err(rollback_error) = self.repository.delete(&config.name).await {
                return Err(Self::rollback_failure(
                    "add",
                    &config.name,
                    &notify_error,
                    &rollback_error,
                ));
            }
            self.channel_cache.invalidate(&config.name).await;
            return Err(notify_error);
        }

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
        Box::pin(self.update_with_control(config, None)).await
    }

    pub async fn update_with_control(
        &self,
        config: ProviderInstance,
        control: Option<&ExecutionControl>,
    ) -> crate::Result<()> {
        let previous_config = self
            .repository
            .get_by_name(&config.name)
            .await?
            .ok_or_else(|| {
                crate::Error::NotFound(format!("Instance '{}' not found", config.name))
            })?;
        let previous_connection = self.channel_cache.get(&config.name).await;

        Self::validate_config(&config)?;

        let connection = if config.enabled && Self::requires_remote_connection(&config) {
            Some(
                Box::pin(
                    self.build_management_validated_remote_connection_with_control(
                        &config, control,
                    ),
                )
                .await?,
            )
        } else {
            None
        };

        // Update database
        self.repository.update(&config).await?;

        if let Some(connection) = connection {
            self.channel_cache
                .insert(config.name.clone(), connection)
                .await;
        } else {
            self.channel_cache.invalidate(&config.name).await;
        }

        // Notify other replicas
        if let Err(notify_error) = self.notify_change("update", &config.name).await {
            if let Err(rollback_error) = self.repository.update(&previous_config).await {
                return Err(Self::rollback_failure(
                    "update",
                    &config.name,
                    &notify_error,
                    &rollback_error,
                ));
            }
            self.restore_cached_connection(&config.name, previous_connection)
                .await;
            return Err(notify_error);
        }

        tracing::info!("Updated provider instance: {}", config.name);
        Ok(())
    }

    /// Delete a provider instance
    ///
    /// 1. Removes from database
    /// 2. Invalidates cached channel
    /// 3. Notifies other replicas via Redis
    pub async fn delete(&self, name: &str) -> crate::Result<()> {
        let previous_config = self
            .repository
            .get_by_name(name)
            .await?
            .ok_or_else(|| crate::Error::NotFound(format!("Instance '{name}' not found")))?;
        let previous_connection = self.channel_cache.get(name).await;

        // Remove from database
        self.repository.delete(name).await?;

        // Invalidate cache
        self.channel_cache.invalidate(name).await;

        // Notify other replicas
        if let Err(notify_error) = self.notify_change("delete", name).await {
            if let Err(rollback_error) = self.repository.create(&previous_config).await {
                return Err(Self::rollback_failure(
                    "delete",
                    name,
                    &notify_error,
                    &rollback_error,
                ));
            }
            self.restore_cached_connection(name, previous_connection)
                .await;
            return Err(notify_error);
        }

        tracing::info!("Deleted provider instance: {}", name);
        Ok(())
    }

    /// Enable a provider instance
    ///
    /// Loads config from DB, creates channel, caches it, and notifies replicas.
    pub async fn enable(&self, name: &str) -> crate::Result<()> {
        Box::pin(self.enable_with_control(name, None)).await
    }

    pub async fn enable_with_control(
        &self,
        name: &str,
        control: Option<&ExecutionControl>,
    ) -> crate::Result<()> {
        let mut config = self
            .repository
            .get_by_name(name)
            .await?
            .ok_or_else(|| crate::Error::NotFound(format!("Instance '{name}' not found")))?;
        let previous_connection = self.channel_cache.get(name).await;

        if config.enabled {
            if Self::requires_remote_connection(&config) {
                let connection = self
                    .build_management_validated_remote_connection_with_control(&config, control);
                let connection = Box::pin(connection).await?;
                self.channel_cache
                    .insert(config.name.clone(), connection)
                    .await;
            } else {
                self.channel_cache.invalidate(&config.name).await;
            }
            if let Err(notify_error) = self.notify_change("enable", name).await {
                self.restore_cached_connection(name, previous_connection)
                    .await;
                return Err(notify_error);
            }
            tracing::info!("Enabled provider instance: {}", name);
            return Ok(());
        }

        Self::validate_config(&config)?;

        config.enabled = true;
        if Self::requires_remote_connection(&config) {
            let connection =
                self.build_management_validated_remote_connection_with_control(&config, control);
            let connection = Box::pin(connection).await?;

            // Persist only after a valid channel can be constructed.
            self.repository.enable(name).await?;
            self.channel_cache
                .insert(config.name.clone(), connection)
                .await;
        } else {
            self.repository.enable(name).await?;
            self.channel_cache.invalidate(&config.name).await;
        }

        // Notify other replicas
        if let Err(notify_error) = self.notify_change("enable", name).await {
            if let Err(rollback_error) = self.repository.disable(name).await {
                return Err(Self::rollback_failure(
                    "enable",
                    name,
                    &notify_error,
                    &rollback_error,
                ));
            }
            self.restore_cached_connection(name, previous_connection)
                .await;
            return Err(notify_error);
        }

        tracing::info!("Enabled provider instance: {}", name);
        Ok(())
    }

    /// Disable a provider instance
    ///
    /// Invalidates cached channel and notifies replicas.
    pub async fn disable(&self, name: &str) -> crate::Result<()> {
        let previous_config = self
            .repository
            .get_by_name(name)
            .await?
            .ok_or_else(|| crate::Error::NotFound(format!("Instance '{name}' not found")))?;
        let previous_connection = self.channel_cache.get(name).await;

        // Update database
        self.repository.disable(name).await?;

        // Invalidate cache
        self.channel_cache.invalidate(name).await;

        // Notify other replicas
        if let Err(notify_error) = self.notify_change("disable", name).await {
            if let Err(rollback_error) = self.repository.enable(name).await {
                return Err(Self::rollback_failure(
                    "disable",
                    name,
                    &notify_error,
                    &rollback_error,
                ));
            }
            if previous_config.enabled {
                self.restore_cached_connection(name, previous_connection)
                    .await;
            } else {
                self.channel_cache.invalidate(name).await;
            }
            return Err(notify_error);
        }

        tracing::info!("Disabled provider instance: {}", name);
        Ok(())
    }

    /// Reconnect a provider instance atomically.
    ///
    /// Invalidates the cached channel and re-creates it from the current DB
    /// config. If the re-creation fails, the instance remains disabled so
    /// callers observe a consistent state rather than a half-connected instance.
    pub async fn reconnect(&self, name: &str) -> crate::Result<()> {
        Box::pin(self.reconnect_with_control(name, None)).await
    }

    pub async fn reconnect_with_control(
        &self,
        name: &str,
        control: Option<&ExecutionControl>,
    ) -> crate::Result<()> {
        let previous_connection = self.channel_cache.get(name).await;

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

        if !Self::requires_remote_connection(&config) {
            return Err(crate::Error::InvalidInput(format!(
                "Instance '{name}' is local-only and does not support remote reconnect"
            )));
        }

        let connection =
            self.build_management_validated_remote_connection_with_control(&config, control);
        let connection = Box::pin(connection).await?;
        self.channel_cache
            .insert(config.name.clone(), connection)
            .await;

        // Notify other replicas
        if let Err(notify_error) = self.notify_change("reconnect", name).await {
            self.restore_cached_connection(name, previous_connection)
                .await;
            return Err(notify_error);
        }

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
        let configs = match self.repository.get_all_enabled().await {
            Ok(configs) => configs,
            Err(e) => {
                tracing::error!("Failed to load instances for health check: {e}");
                return HashMap::new();
            }
        };

        self.health_check_instances(&configs).await
    }

    /// Health check a selected set of provider instances.
    ///
    /// This avoids probing every enabled instance when a caller only needs
    /// status for a filtered or paginated subset.
    pub async fn health_check_instances(
        &self,
        configs: &[ProviderInstance],
    ) -> HashMap<String, bool> {
        let mut results = HashMap::new();

        for config in configs {
            if !Self::requires_remote_connection(config) {
                continue;
            }

            if validate_auth_secret(config.jwt_secret.as_deref()).is_err() {
                tracing::warn!(
                    "Health check reporting provider instance '{}' unhealthy: missing or invalid jwt_secret for remote-capable configuration",
                    config.name
                );
                results.insert(config.name.clone(), false);
                continue;
            }

            let Some(connection) = self.get(&config.name).await else {
                results.insert(config.name.clone(), false);
                continue;
            };

            let is_healthy = self
                .check_instance_health(&config.name, config, &connection)
                .await;
            results.insert(config.name.clone(), is_healthy);
        }

        results
    }

    /// Check health of a single remote instance
    ///
    /// Calls gRPC Health Check RPC with 5-second timeout.
    async fn check_instance_health(
        &self,
        name: &str,
        config: &ProviderInstance,
        connection: &RemoteProviderConnection,
    ) -> bool {
        match self
            .validate_management_connection_with_control(config, connection, None)
            .await
        {
            Ok(()) => {
                tracing::debug!("Provider instance '{}' is healthy", name);
                true
            }
            Err(error) => {
                tracing::error!("Health check failed for instance '{}': {}", name, error);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheInvalidationService;
    use crate::models::ProviderInstance;
    use crate::repository::ProviderInstanceRepository;
    use chrono::Utc;

    fn remote_instance(endpoint: &str) -> ProviderInstance {
        ProviderInstance {
            name: "remote".to_string(),
            endpoint: endpoint.to_string(),
            comment: None,
            jwt_secret: Some("remote-provider-test-secret".to_string()),
            custom_ca: None,
            timeout: "5s".to_string(),
            tls: false,
            insecure_tls: false,
            providers: vec!["alist".to_string()],
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn start_invalidation_listener_does_not_wait_for_fake_readiness() {
        let pool = sqlx::PgPool::connect_lazy("postgresql://test")
            .expect("lazy pool should build without a live database");
        let repository = Arc::new(ProviderInstanceRepository::new(pool));
        let invalidation = Arc::new(CacheInvalidationService::new(
            "test-node".to_string(),
            "test:provider:invalidate".to_string(),
        ));
        let manager = RemoteProviderManager::new_with_invalidation(repository, Some(invalidation));

        let start = tokio::time::Instant::now();
        manager
            .start_invalidation_listener()
            .await
            .expect("listener should start");

        assert_eq!(
            tokio::time::Instant::now().duration_since(start),
            Duration::ZERO,
            "listener startup should not advance time via a fake readiness sleep"
        );

        manager.shutdown().await;
    }

    #[test]
    fn validate_config_accepts_http_endpoint_scheme() {
        let config = remote_instance("http://provider.example.com:50051");

        RemoteProviderManager::validate_config(&config)
            .expect("http:// endpoint should remain valid");
    }

    #[test]
    fn validate_config_rejects_invalid_provider_instance_name() {
        let mut config = remote_instance("http://provider.example.com:50051");
        config.name = "bad name".to_string();

        let err = RemoteProviderManager::validate_config(&config)
            .expect_err("provider instance names must match the core naming contract");

        assert!(
            err.to_string().contains("provider instance name"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_config_rejects_unsupported_provider_type() {
        let mut config = remote_instance("http://provider.example.com:50051");
        config.providers = vec!["custom_local".to_string()];

        let err = RemoteProviderManager::validate_config(&config)
            .expect_err("unsupported remote provider types must be rejected");

        assert!(
            err.to_string().contains("unsupported provider"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_config_rejects_http_endpoint_with_tls_enabled() {
        let mut config = remote_instance("http://provider.example.com:50051");
        config.tls = true;

        let err = RemoteProviderManager::validate_config(&config)
            .expect_err("plaintext http endpoints must require tls=false");

        assert!(
            err.to_string().contains("tls=false"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_config_rejects_https_endpoint_without_tls() {
        let config = remote_instance("https://provider.example.com:50051");

        let err = RemoteProviderManager::validate_config(&config)
            .expect_err("https endpoints must require tls=true");

        assert!(
            err.to_string().contains("tls=true"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_config_rejects_custom_ca_without_tls() {
        let mut config = remote_instance("http://provider.example.com:50051");
        config.custom_ca =
            Some("-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----".to_string());

        let err = RemoteProviderManager::validate_config(&config)
            .expect_err("custom CA must not be accepted for plaintext endpoints");

        assert!(
            err.to_string().contains("custom_ca requires tls=true"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_config_accepts_https_endpoint_with_tls() {
        let mut config = remote_instance("https://provider.example.com:50051");
        config.tls = true;

        RemoteProviderManager::validate_config(&config)
            .expect("https endpoint with tls=true should pass validation");
    }

    #[test]
    fn normalized_transport_endpoint_preserves_http() {
        let config = remote_instance("http://provider.example.com:50051");

        let normalized = RemoteProviderManager::normalized_transport_endpoint(&config)
            .expect("http:// endpoint should normalize to a tonic transport URL");

        assert_eq!(normalized, "http://provider.example.com:50051");
    }

    #[test]
    fn map_remote_resolution_error_hides_database_details() {
        let err = RemoteProviderManager::map_remote_resolution_error(crate::Error::Database(
            sqlx::Error::PoolTimedOut,
        ));

        assert!(matches!(
            err,
            ProviderError::ApiError(ref message)
                if message == "Provider configuration service is temporarily unavailable."
        ));
    }

    #[test]
    fn map_remote_resolution_error_hides_redis_details() {
        let err = RemoteProviderManager::map_remote_resolution_error(crate::Error::Redis(
            redis::RedisError::from((redis::ErrorKind::Io, "connection reset by peer")),
        ));

        assert!(matches!(
            err,
            ProviderError::ApiError(ref message)
                if message == "Provider configuration service is temporarily unavailable."
        ));
    }

    #[test]
    fn provider_connection_setup_error_hides_invalid_endpoint_details() {
        let err = RemoteProviderManager::provider_connection_setup_error(
            "Remote provider endpoint configuration is invalid.",
            "relative URL without a base",
        );

        assert!(matches!(
            err,
            crate::Error::Internal(ref message)
                if message == "Remote provider endpoint configuration is invalid."
        ));
    }

    #[test]
    fn provider_connection_setup_error_hides_tls_connect_details() {
        let err = RemoteProviderManager::provider_connection_setup_error(
            "Remote provider TLS connection setup failed.",
            "certificate verify failed",
        );

        assert!(matches!(
            err,
            crate::Error::Internal(ref message)
                if message == "Remote provider TLS connection setup failed."
        ));
    }

    #[test]
    fn probe_execution_control_preserves_tighter_parent_deadline_and_cancellation() {
        let cancellation = tokio_util::sync::CancellationToken::new();
        let parent_deadline = std::time::Instant::now() + Duration::from_secs(1);
        let parent = ExecutionControl::from_parts(Some(parent_deadline), cancellation.clone());

        let probe =
            RemoteProviderManager::probe_execution_control(Some(&parent), Duration::from_secs(5));

        assert_eq!(probe.deadline(), Some(parent_deadline));
        cancellation.cancel();
        assert!(matches!(
            probe.check_active(),
            Err(synctv_common::ExecutionControlError::Cancelled)
        ));
    }

    #[test]
    fn probe_execution_control_applies_probe_timeout_without_parent_control() {
        let probe = RemoteProviderManager::probe_execution_control(None, Duration::from_secs(5));

        let remaining = probe
            .remaining_timeout()
            .expect("probe without parent control should still have a deadline");
        assert!(remaining <= Duration::from_secs(5));
        assert!(remaining > Duration::ZERO);
    }
}
