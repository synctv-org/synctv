// Remote Provider Manager
// Manages remote provider instances.
// Supports both local (in-process) and remote provider instances.
// ## Multi-replica support
// Instead of maintaining a persistent connection map that is invisible across replicas,
// connections are created lazily on demand and cached with a TTL. When a provider
// operation is needed, the manager looks up the instance config from the DB and
// creates a connection if not already cached.
// Provider changes (add/update/delete/enable/disable) are broadcast via the
// shared durable cache invalidation stream so other replicas can invalidate
// their local connection cache even across restarts and transient disconnects.

use crate::cache::CacheInvalidationRuntime;
use crate::provider::{AlistProvider, BilibiliProvider, EmbyProvider};
use crate::repository::ProviderInstanceRepository;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
pub use store::ProviderInstanceStore;
use synctv_media_providers::remote_transport::RemoteProviderConnection;
use tokio::task::JoinHandle;

mod health;
mod invalidation;
mod management;
mod queries;
mod resolution;
mod store;
mod transport;
mod validation;
pub use store::empty_provider_instance_manager;
#[cfg(test)]
pub(crate) use store::empty_provider_instance_store;

/// Default connection cache TTL (5 minutes)
const CONNECTION_CACHE_TTL_SECS: u64 = 300;

/// Maximum number of cached connections
const MAX_CACHED_CONNECTIONS: u64 = 1_000;

/// Remote Provider Manager
///
/// Manages remote provider instances.
/// Provider adapters use the local singleton only when no provider instance
/// name is requested. Explicit instance names are resolved through
/// `resolve_client_required(_with_context)` so missing, disabled, or
/// misconfigured remote instances fail closed instead of being masked by local
/// fallback.
///
/// ## Multi-replica architecture
///
/// - Connections are created lazily from DB config and cached with TTL via moka
/// - `get(name)` looks up the cached connection or creates one from DB on cache miss
/// - Provider mutations publish durable invalidation events through `CacheInvalidationService`
/// - A background subscriber listens for invalidation messages and evicts stale entries
pub struct RemoteProviderManager {
    /// Lazily-populated connection cache with TTL (indexed by instance name)
    connection_cache: Arc<moka::future::Cache<String, RemoteProviderConnection>>,

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

    /// Global SSRF policy for remote provider endpoints.
    ssrf_guard: synctv_common::ssrf::SsrfGuard,

    /// Whether remote provider clients should negotiate transport compression.
    transport_compression_enabled: bool,
}

pub struct RemoteProviderManagerOptions {
    pub address_overrides: HashMap<String, SocketAddr>,
    pub ssrf_guard: synctv_common::ssrf::SsrfGuard,
    pub transport_compression_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteProviderRuntimeStatus {
    pub available: bool,
    pub has_auth_secret: bool,
}

impl Default for RemoteProviderManagerOptions {
    fn default() -> Self {
        Self {
            address_overrides: HashMap::new(),
            ssrf_guard: synctv_common::ssrf::SsrfGuard::strict_policy(),
            transport_compression_enabled: true,
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
        let connection_cache = moka::future::Cache::builder()
            .max_capacity(MAX_CACHED_CONNECTIONS)
            .time_to_live(Duration::from_secs(CONNECTION_CACHE_TTL_SECS))
            .build();

        Self {
            connection_cache: Arc::new(connection_cache),
            repository,
            cache_invalidation,
            invalidation_cancel: tokio_util::sync::CancellationToken::new(),
            invalidation_listener_task: Arc::new(tokio::sync::Mutex::new(None)),
            address_overrides: Arc::new(options.address_overrides),
            ssrf_guard: options.ssrf_guard,
            transport_compression_enabled: options.transport_compression_enabled,
        }
    }

    /// Initialize manager by pre-warming the cache with all enabled instances from database.
    ///
    /// This is optional -- connections will be created lazily on demand even without
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
                    "Skipping remote connection pre-warm for local-only provider instance: {}",
                    config.name
                );
                continue;
            }

            Self::validate_config_with_ssrf_guard(&config, &self.ssrf_guard)?;

            match self.create_remote_connection(&config) {
                Ok(connection) => {
                    self.connection_cache
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
            }
        }

        tracing::info!(
            "Provider instance manager initialized: {} instances cached, {} failed",
            success_count,
            error_count
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests;
