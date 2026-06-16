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

use crate::cache::CacheInvalidationRuntime;
use crate::models::{ProviderInstance, ProviderInstanceListQuery};
use crate::provider::provider_client::{validate_auth_secret, RemoteProviderConnection};
use crate::provider::{AlistProvider, BilibiliProvider, EmbyProvider, ProviderError};
use crate::repository::ProviderInstanceRepository;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use synctv_common::ExecutionControl;
use tokio::task::JoinHandle;
use tonic::transport::Channel;
use tonic_health::pb::{health_client::HealthClient, HealthCheckRequest};

mod invalidation;
mod management;
mod transport;
mod validation;

/// Default channel cache TTL (5 minutes)
const CHANNEL_CACHE_TTL_SECS: u64 = 300;

/// Maximum number of cached channels
const MAX_CACHED_CHANNELS: u64 = 1_000;

#[async_trait::async_trait]
pub trait ProviderInstanceStore: Send + Sync + std::fmt::Debug {
    async fn get_all_enabled(&self) -> crate::Result<Vec<ProviderInstance>>;
    async fn get_all(&self) -> crate::Result<Vec<ProviderInstance>>;
    async fn get_by_name(&self, name: &str) -> crate::Result<Option<ProviderInstance>>;
    async fn list_with_total(
        &self,
        query: &ProviderInstanceListQuery,
    ) -> crate::Result<(Vec<ProviderInstance>, i64)>;
    async fn find_by_provider(&self, provider: &str) -> crate::Result<Vec<ProviderInstance>>;
    async fn create(&self, instance: &ProviderInstance) -> crate::Result<()>;
    async fn update(&self, instance: &ProviderInstance) -> crate::Result<()>;
    async fn delete(&self, name: &str) -> crate::Result<()>;
    async fn enable(&self, name: &str) -> crate::Result<()>;
    async fn disable(&self, name: &str) -> crate::Result<()>;
}

#[async_trait::async_trait]
impl ProviderInstanceStore for ProviderInstanceRepository {
    async fn get_all_enabled(&self) -> crate::Result<Vec<ProviderInstance>> {
        self.get_all_enabled().await
    }

    async fn get_all(&self) -> crate::Result<Vec<ProviderInstance>> {
        self.get_all().await
    }

    async fn get_by_name(&self, name: &str) -> crate::Result<Option<ProviderInstance>> {
        self.get_by_name(name).await
    }

    async fn list_with_total(
        &self,
        query: &ProviderInstanceListQuery,
    ) -> crate::Result<(Vec<ProviderInstance>, i64)> {
        self.list_with_total(query).await
    }

    async fn find_by_provider(&self, provider: &str) -> crate::Result<Vec<ProviderInstance>> {
        self.find_by_provider(provider).await
    }

    async fn create(&self, instance: &ProviderInstance) -> crate::Result<()> {
        self.create(instance).await
    }

    async fn update(&self, instance: &ProviderInstance) -> crate::Result<()> {
        self.update(instance).await
    }

    async fn delete(&self, name: &str) -> crate::Result<()> {
        self.delete(name).await
    }

    async fn enable(&self, name: &str) -> crate::Result<()> {
        self.enable(name).await
    }

    async fn disable(&self, name: &str) -> crate::Result<()> {
        self.disable(name).await
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct EmptyProviderInstanceStore;

#[cfg(test)]
#[async_trait::async_trait]
impl ProviderInstanceStore for EmptyProviderInstanceStore {
    async fn get_all_enabled(&self) -> crate::Result<Vec<ProviderInstance>> {
        Ok(Vec::new())
    }

    async fn get_all(&self) -> crate::Result<Vec<ProviderInstance>> {
        Ok(Vec::new())
    }

    async fn get_by_name(&self, _name: &str) -> crate::Result<Option<ProviderInstance>> {
        Ok(None)
    }

    async fn list_with_total(
        &self,
        _query: &ProviderInstanceListQuery,
    ) -> crate::Result<(Vec<ProviderInstance>, i64)> {
        Ok((Vec::new(), 0))
    }

    async fn find_by_provider(&self, _provider: &str) -> crate::Result<Vec<ProviderInstance>> {
        Ok(Vec::new())
    }

    async fn create(&self, _instance: &ProviderInstance) -> crate::Result<()> {
        Ok(())
    }

    async fn update(&self, _instance: &ProviderInstance) -> crate::Result<()> {
        Ok(())
    }

    async fn delete(&self, _name: &str) -> crate::Result<()> {
        Ok(())
    }

    async fn enable(&self, _name: &str) -> crate::Result<()> {
        Ok(())
    }

    async fn disable(&self, _name: &str) -> crate::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn empty_provider_instance_store() -> Arc<dyn ProviderInstanceStore> {
    Arc::new(EmptyProviderInstanceStore)
}

#[cfg(test)]
pub(crate) fn empty_provider_instance_manager() -> Arc<RemoteProviderManager> {
    Arc::new(RemoteProviderManager::new_with_store(
        empty_provider_instance_store(),
        None,
    ))
}

/// Remote Provider Manager
///
/// Manages remote provider instances (gRPC connections).
/// Provider adapters use the local singleton only when no provider instance
/// name is requested. Explicit instance names are resolved through
/// `resolve_client_required(_with_context)` so missing, disabled, or
/// misconfigured remote instances fail closed instead of being masked by local
/// fallback.
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
    repository: Arc<dyn ProviderInstanceStore>,

    /// Shared durable invalidation bus for cross-replica cache invalidation.
    cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,

    /// Cancellation token for the provider invalidation listener.
    invalidation_cancel: tokio_util::sync::CancellationToken,

    /// Provider invalidation listener task handle.
    invalidation_listener_task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,

    /// Exact-host address overrides used by tests to route synthetic hostnames
    /// to in-process servers without weakening the production SSRF policy.
    address_overrides: Arc<HashMap<String, SocketAddr>>,

    /// Global SSRF policy for remote provider gRPC endpoints.
    ssrf_guard: synctv_common::ssrf::SsrfGuard,

    /// Whether remote provider gRPC clients should negotiate gzip compression.
    grpc_compression_enabled: bool,
}

pub struct RemoteProviderManagerOptions {
    pub address_overrides: HashMap<String, SocketAddr>,
    pub ssrf_guard: synctv_common::ssrf::SsrfGuard,
    pub grpc_compression_enabled: bool,
}

impl Default for RemoteProviderManagerOptions {
    fn default() -> Self {
        Self {
            address_overrides: HashMap::new(),
            ssrf_guard: synctv_common::ssrf::SsrfGuard::strict_policy(),
            grpc_compression_enabled: true,
        }
    }
}

impl std::fmt::Debug for RemoteProviderManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteProviderManager")
            .field("invalidation_enabled", &self.cache_invalidation.is_some())
            .field("ssrf_enabled", &self.ssrf_guard.acl().is_some())
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
            crate::Error::RangeNotSatisfiable { total_size } => ProviderError::InvalidConfig(
                format!("Range not satisfiable: total size {total_size}"),
            ),
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
            crate::Error::Authorization(msg) => ProviderError::Internal(format!(
                "Unexpected authorization error while resolving provider instance: {msg}"
            )),
            crate::Error::NotFound(msg) => ProviderError::Internal(format!(
                "Unexpected not found error while resolving provider instance: {msg}"
            )),
            crate::Error::AlreadyExists(msg) => ProviderError::Internal(format!(
                "Unexpected already exists error while resolving provider instance: {msg}"
            )),
            crate::Error::Conflict(msg) => ProviderError::Internal(format!(
                "Unexpected conflict while resolving provider instance: {msg}"
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

                let channel = self.create_grpc_channel(&config).map_err(|error| {
                    LoadError::Provider(Self::map_remote_resolution_error(error))
                })?;
                let connection =
                    self.build_remote_connection(&config, channel)
                        .map_err(|error| {
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
        Self::new_with_store(repository, None)
    }

    /// Create a new `RemoteProviderManager` with the shared durable invalidation service.
    #[must_use]
    pub fn new_with_invalidation(
        repository: Arc<ProviderInstanceRepository>,
        cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
    ) -> Self {
        Self::new_with_store(repository, cache_invalidation)
    }

    #[must_use]
    pub fn new_with_store(
        store: Arc<dyn ProviderInstanceStore>,
        cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
    ) -> Self {
        Self::new_with_store_and_options(
            store,
            cache_invalidation,
            RemoteProviderManagerOptions::default(),
        )
    }

    #[must_use]
    pub fn new_with_address_overrides(
        repository: Arc<ProviderInstanceRepository>,
        cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
        address_overrides: HashMap<String, SocketAddr>,
    ) -> Self {
        Self::new_with_store_and_options(
            repository,
            cache_invalidation,
            RemoteProviderManagerOptions {
                address_overrides,
                ..RemoteProviderManagerOptions::default()
            },
        )
    }

    #[must_use]
    pub fn new_with_address_overrides_and_ssrf_guard(
        repository: Arc<ProviderInstanceRepository>,
        cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
        address_overrides: HashMap<String, SocketAddr>,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
    ) -> Self {
        Self::new_with_store_and_options(
            repository,
            cache_invalidation,
            RemoteProviderManagerOptions {
                address_overrides,
                ssrf_guard,
                ..RemoteProviderManagerOptions::default()
            },
        )
    }

    #[must_use]
    pub fn new_with_options(
        repository: Arc<ProviderInstanceRepository>,
        cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
        options: RemoteProviderManagerOptions,
    ) -> Self {
        Self::new_with_store_and_options(repository, cache_invalidation, options)
    }

    #[must_use]
    pub fn new_with_store_and_options(
        repository: Arc<dyn ProviderInstanceStore>,
        cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
        options: RemoteProviderManagerOptions,
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
            address_overrides: Arc::new(options.address_overrides),
            ssrf_guard: options.ssrf_guard,
            grpc_compression_enabled: options.grpc_compression_enabled,
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

            Self::validate_config_with_ssrf_guard(&config, &self.ssrf_guard)?;

            match self.create_grpc_channel(&config) {
                Ok(channel) => match self.build_remote_connection(&config, channel) {
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

    fn build_remote_connection(
        &self,
        config: &ProviderInstance,
        channel: Channel,
    ) -> crate::Result<RemoteProviderConnection> {
        let auth_secret = validate_auth_secret(Some(Self::required_auth_secret(config)?))
            .map_err(|e| crate::Error::InvalidInput(e.to_string()))?;
        Ok(RemoteProviderConnection::new_with_grpc_compression(
            channel,
            auth_secret,
            self.grpc_compression_enabled,
        ))
    }

    async fn build_management_validated_remote_connection_with_control(
        &self,
        config: &ProviderInstance,
        control: Option<&ExecutionControl>,
    ) -> crate::Result<RemoteProviderConnection> {
        let channel = self.create_grpc_channel(config)?;
        let connection = self.build_remote_connection(config, channel)?;
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

    /// Get a remote provider instance channel by name for best-effort probes.
    ///
    /// Checks the local moka cache first. On cache miss, loads the instance config
    /// from the database and creates a channel lazily. This ensures that provider
    /// instances added on other replicas are visible after the cache TTL expires
    /// (or immediately if a Redis invalidation notification was received).
    ///
    /// Returns:
    /// - `Some(channel)` if the instance exists and is enabled
    /// - `None` if not found, disabled, or temporarily unavailable
    pub async fn get(&self, name: &str) -> Option<RemoteProviderConnection> {
        match self.get_connection_result(name).await {
            Ok(connection) => connection,
            Err(err) => {
                tracing::error!(
                    "Failed to resolve remote provider instance '{}' for best-effort probe: {}",
                    name,
                    err
                );
                None
            }
        }
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
        self.list_instances_with_total(query)
            .await
            .map(|(instances, _)| instances)
    }

    pub async fn list_instances_with_total(
        &self,
        query: &ProviderInstanceListQuery,
    ) -> crate::Result<(Vec<ProviderInstance>, i64)> {
        self.repository
            .list_with_total(query)
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
mod tests;
