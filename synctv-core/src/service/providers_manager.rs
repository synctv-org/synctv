//! Providers Manager
//!
//! Manages all `MediaProvider` instances with singleton pattern.
//! Built-in local providers are created once at startup from explicit local
//! provider configuration.

use crate::config::{LocalProviderHttpConfig, MediaProvidersConfig};
use crate::provider::{
    AlistProvider, BilibiliProvider, DirectUrlProvider, EmbyProvider, LiveProxyProvider,
    MediaProvider, ProviderClientManager, RtmpProvider,
};
use crate::service::RemoteProviderManager;
use crate::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

fn provider_http_client_from_config(
    config: &Value,
) -> std::result::Result<Option<reqwest::Client>, crate::Error> {
    let request_timeout_seconds = config
        .get("request_timeout_seconds")
        .and_then(serde_json::Value::as_u64);
    let connect_timeout_seconds = config
        .get("connect_timeout_seconds")
        .and_then(serde_json::Value::as_u64);

    if request_timeout_seconds.is_none() && connect_timeout_seconds.is_none() {
        return Ok(None);
    }

    let request_timeout_seconds = request_timeout_seconds
        .unwrap_or_else(|| LocalProviderHttpConfig::default().request_timeout_seconds);
    let connect_timeout_seconds = connect_timeout_seconds
        .unwrap_or_else(|| LocalProviderHttpConfig::default().connect_timeout_seconds);

    let client = synctv_common::http::SsrfSafeClientBuilder::provider()
        .request_timeout(std::time::Duration::from_secs(request_timeout_seconds))
        .read_timeout(std::time::Duration::from_secs(request_timeout_seconds))
        .connect_timeout(std::time::Duration::from_secs(connect_timeout_seconds))
        .build()
        .map_err(|e| {
            crate::Error::Internal(format!("Failed to build provider HTTP client: {e}"))
        })?;

    Ok(Some(client))
}

/// Factory function type for creating `MediaProvider` instances
pub type ProviderFactory = Box<
    dyn Fn(&str, &Value, Arc<RemoteProviderManager>) -> Result<Arc<dyn MediaProvider>>
        + Send
        + Sync,
>;

/// Providers Manager
///
/// Manages all `MediaProvider` instances using singleton pattern.
/// Each provider type has exactly one instance.
///
/// # Initialization Order
/// 1. Create `ProvidersManager` with `RemoteProviderManager`
/// 2. Load provider configurations from Config
/// 3. Create provider instances (singleton per type)
/// 4. Pass to synctv-api layer for route registration
///
/// # Architecture
/// ```text
/// ProvidersManager (synctv-core)
/// ├── Factories (registered for each provider type)
/// ├── Instances (singleton MediaProvider instances)
/// └── RemoteProviderManager (for local/remote dispatch)
///
/// synctv-api layer
/// ├── Gets provider instances from ProvidersManager
/// ├── Registers HTTP/gRPC routes for each provider
/// └── No hardcoded provider types
/// ```
pub struct ProvidersManager {
    /// Registered factory functions (`provider_type` → factory)
    factories: HashMap<String, ProviderFactory>,

    /// Created `MediaProvider` instances (singleton per provider type)
    instances: Arc<RwLock<HashMap<String, Arc<dyn MediaProvider>>>>,

    /// Provider instance manager (for local/remote dispatch)
    instance_manager: Arc<RemoteProviderManager>,
    /// Default injected local provider clients used by provider instances
    /// when they do not specify a per-instance HTTP transport override.
    default_client_manager: Arc<ProviderClientManager>,
}

impl ProvidersManager {
    fn default_instance_id(provider_type: &str) -> String {
        provider_type.to_string()
    }

    /// Create a new `ProvidersManager`
    #[must_use]
    pub fn new(instance_manager: Arc<RemoteProviderManager>) -> Self {
        let default_provider_http_client = synctv_common::http::build_provider_client()
            .expect("default provider HTTP client should build");
        Self::new_with_provider_http_client(instance_manager, default_provider_http_client)
    }

    /// Create a new manager with an explicit default local provider HTTP client.
    #[must_use]
    pub fn new_with_provider_http_client(
        instance_manager: Arc<RemoteProviderManager>,
        default_provider_http_client: reqwest::Client,
    ) -> Self {
        let default_client_manager = Arc::new(
            ProviderClientManager::new_with_provider_http_client(default_provider_http_client),
        );
        let mut manager = Self {
            factories: HashMap::new(),
            instances: Arc::new(RwLock::new(HashMap::new())),
            instance_manager,
            default_client_manager,
        };

        // Register all built-in providers
        manager.register_builtin_providers();

        manager
    }

    /// Get a reference to the provider instance manager
    #[must_use]
    pub const fn instance_manager(&self) -> &Arc<RemoteProviderManager> {
        &self.instance_manager
    }

    /// Register all built-in provider factories
    fn register_builtin_providers(&mut self) {
        let default_client_manager = Arc::clone(&self.default_client_manager);
        // Alist factory - reads local Alist config.
        self.register_factory(
            "alist",
            Box::new(move |_instance_id, config, instance_manager| {
                let provider = if let Some(client) = provider_http_client_from_config(config)? {
                    AlistProvider::with_client_manager(
                        instance_manager,
                        Arc::new(
                            crate::provider::ProviderClientManager::new_with_provider_http_client(
                                client,
                            ),
                        ),
                    )
                } else {
                    AlistProvider::with_client_manager(
                        instance_manager,
                        Arc::clone(&default_client_manager),
                    )
                };
                Ok(Arc::new(provider))
            }),
        );

        // Bilibili factory - reads local Bilibili config.
        let default_client_manager = Arc::clone(&self.default_client_manager);
        self.register_factory(
            "bilibili",
            Box::new(move |_instance_id, config, instance_manager| {
                let provider = if let Some(client) = provider_http_client_from_config(config)? {
                    BilibiliProvider::with_client_manager(
                        instance_manager,
                        Arc::new(
                            crate::provider::ProviderClientManager::new_with_provider_http_client(
                                client,
                            ),
                        ),
                    )
                } else {
                    BilibiliProvider::with_client_manager(
                        instance_manager,
                        Arc::clone(&default_client_manager),
                    )
                };
                Ok(Arc::new(provider))
            }),
        );

        // Emby factory - reads local Emby config.
        let default_client_manager = Arc::clone(&self.default_client_manager);
        self.register_factory(
            "emby",
            Box::new(move |_instance_id, config, instance_manager| {
                let provider = if let Some(client) = provider_http_client_from_config(config)? {
                    EmbyProvider::with_client_manager(
                        instance_manager,
                        Arc::new(
                            crate::provider::ProviderClientManager::new_with_provider_http_client(
                                client,
                            ),
                        ),
                    )
                } else {
                    EmbyProvider::with_client_manager(
                        instance_manager,
                        Arc::clone(&default_client_manager),
                    )
                };
                Ok(Arc::new(provider))
            }),
        );

        // RTMP factory
        self.register_factory(
            "rtmp",
            Box::new(|_instance_id, _config, _instance_manager| Ok(Arc::new(RtmpProvider::new()))),
        );

        // DirectUrl factory
        self.register_factory(
            "direct_url",
            Box::new(|_instance_id, _config, _instance_manager| {
                Ok(Arc::new(DirectUrlProvider::new()))
            }),
        );

        // LiveProxy factory
        self.register_factory(
            "live_proxy",
            Box::new(|_instance_id, _config, _instance_manager| {
                Ok(Arc::new(LiveProxyProvider::new()))
            }),
        );
    }

    /// Register a provider factory
    pub fn register_factory(&mut self, provider_type: &str, factory: ProviderFactory) {
        self.factories.insert(provider_type.to_string(), factory);
        tracing::debug!("Registered provider factory: {}", provider_type);
    }

    /// Create a provider instance (singleton per type)
    ///
    /// # Arguments
    /// * `provider_type` - Type of provider ("alist", "bilibili", etc.)
    /// * `instance_id` - Unique instance identifier
    /// * `config` - Provider configuration (JSON)
    pub async fn create_provider(
        &self,
        provider_type: &str,
        instance_id: &str,
        config: &Value,
    ) -> Result<Arc<dyn MediaProvider>> {
        let factory = self.factories.get(provider_type).ok_or_else(|| {
            crate::Error::NotFound(format!("Unknown provider type: {provider_type}"))
        })?;

        let provider = factory(instance_id, config, self.instance_manager.clone())?;

        // Store in instances map (singleton)
        self.instances
            .write()
            .await
            .insert(instance_id.to_string(), provider.clone());

        tracing::info!(
            "Created provider instance: {} (type: {})",
            instance_id,
            provider_type
        );

        Ok(provider)
    }

    /// Create missing built-in default provider instances.
    ///
    /// Default instances are addressable by provider type name directly, e.g.
    /// `direct_url`, `alist`, `emby`.
    pub async fn create_builtin_defaults(&self) -> Result<usize> {
        self.create_builtin_defaults_with_config(&MediaProvidersConfig::default())
            .await
    }

    /// Create missing built-in default provider instances using explicit local
    /// provider config from static configuration.
    pub async fn create_builtin_defaults_with_config(
        &self,
        config: &MediaProvidersConfig,
    ) -> Result<usize> {
        let mut provider_types = self.list_types();
        provider_types.sort();

        let mut created = 0;

        for provider_type in provider_types {
            let instance_id = Self::default_instance_id(&provider_type);
            if self.get(&instance_id).await.is_some() {
                continue;
            }

            let provider_config = match provider_type.as_str() {
                "alist" => serde_json::to_value(&config.alist),
                "bilibili" => serde_json::to_value(&config.bilibili),
                "emby" => serde_json::to_value(&config.emby),
                _ => Ok(serde_json::json!({})),
            }
            .map_err(|e| {
                crate::Error::Internal(format!(
                    "Failed to serialize local provider config for '{provider_type}': {e}"
                ))
            })?;

            self.create_provider(&provider_type, &instance_id, &provider_config)
                .await?;
            created += 1;
        }

        Ok(created)
    }

    /// Get a provider instance by ID
    pub async fn get(&self, instance_id: &str) -> Option<Arc<dyn MediaProvider>> {
        self.instances.read().await.get(instance_id).cloned()
    }

    /// Resolve a provider by requested type and optional bound instance name.
    ///
    /// Bound instance names may refer either to a locally configured provider alias
    /// stored in `instances` or to a dynamic remote provider instance managed by
    /// `RemoteProviderManager`.
    pub async fn resolve_provider(
        &self,
        provider_type: &str,
        provider_instance_name: Option<&str>,
    ) -> Result<Arc<dyn MediaProvider>> {
        let trimmed_provider = provider_type.trim();
        if trimmed_provider.is_empty() {
            return Err(crate::Error::InvalidInput(
                "provider_type is required".to_string(),
            ));
        }

        let trimmed_instance = provider_instance_name
            .map(str::trim)
            .filter(|value| !value.is_empty());

        if let Some(instance_name) = trimmed_instance {
            if let Some(provider) = self.get(instance_name).await {
                if provider.name() != trimmed_provider {
                    return Err(crate::Error::InvalidInput(format!(
                        "Provider instance '{instance_name}' is type '{}' but request declared '{trimmed_provider}'",
                        provider.name()
                    )));
                }
                return Ok(provider);
            }

            let Some(instance) = self.instance_manager.get_instance(instance_name).await? else {
                return Err(crate::Error::NotFound(format!(
                    "Provider instance not found: {instance_name}"
                )));
            };

            if !instance.enabled {
                return Err(crate::Error::NotFound(format!(
                    "Provider instance not found: {instance_name}"
                )));
            }

            if !instance.supports_provider(trimmed_provider) {
                return Err(crate::Error::InvalidInput(format!(
                    "Provider instance '{instance_name}' does not support provider '{trimmed_provider}'"
                )));
            }
        }

        self.get_by_type(trimmed_provider).await.ok_or_else(|| {
            crate::Error::NotFound(format!("Provider not found: {trimmed_provider}"))
        })
    }

    /// Get provider by type (returns default instance)
    pub async fn get_by_type(&self, provider_type: &str) -> Option<Arc<dyn MediaProvider>> {
        let instance_id = Self::default_instance_id(provider_type);
        self.get(&instance_id).await
    }

    /// List all provider instances
    pub async fn list(&self) -> Vec<Arc<dyn MediaProvider>> {
        self.instances.read().await.values().cloned().collect()
    }

    /// Remove a provider instance
    pub async fn remove(&self, instance_id: &str) -> Option<Arc<dyn MediaProvider>> {
        self.instances.write().await.remove(instance_id)
    }

    /// Check if a provider type is registered
    #[must_use]
    pub fn has_factory(&self, provider_type: &str) -> bool {
        self.factories.contains_key(provider_type)
    }

    /// List all registered provider types
    #[must_use]
    pub fn list_types(&self) -> Vec<String> {
        self.factories.keys().cloned().collect()
    }

    #[cfg(test)]
    fn default_client_manager_marker(&self) -> usize {
        self.default_client_manager.marker()
    }
}

impl std::fmt::Debug for ProvidersManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let instances_count = match self.instances.try_read() {
            Ok(guard) => Some(guard.len()),
            Err(_) => None,
        };
        f.debug_struct("ProvidersManager")
            .field("factories_count", &self.factories.len())
            .field("instances_count", &instances_count)
            .field("instance_manager", &self.instance_manager)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::ProviderInstanceRepository;
    use sqlx::PgPool;

    #[tokio::test]
    async fn test_providers_manager_creation() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        // Check that built-in providers are registered
        assert!(manager.has_factory("alist"));
        assert!(manager.has_factory("bilibili"));
        assert!(manager.has_factory("emby"));
        assert!(manager.has_factory("rtmp"));
        assert!(manager.has_factory("direct_url"));
        assert!(manager.has_factory("live_proxy"));
        assert!(!manager.has_factory("unknown"));
    }

    #[tokio::test]
    async fn test_list_provider_types() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        let types = manager.list_types();
        assert!(types.contains(&"alist".to_string()));
        assert!(types.contains(&"bilibili".to_string()));
        assert!(types.contains(&"live_proxy".to_string()));
        assert_eq!(types.len(), 6); // alist, bilibili, emby, rtmp, direct_url, live_proxy
    }

    #[tokio::test]
    async fn test_provider_config_without_timeout() {
        // Provider creation should use defaults when timeout config is omitted.
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create provider with empty config (no timeout)
        let config = serde_json::json!({});
        let provider = manager
            .create_provider("alist", "test_alist", &config)
            .await;
        assert!(provider.is_ok());

        // Verify the provider was stored
        let stored = manager.get("test_alist").await;
        assert!(stored.is_some());
    }

    #[tokio::test]
    async fn test_provider_config_with_timeout() {
        // Test that provider accepts per-instance HTTP timeout overrides.
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create provider with Alist-specific timeout config.
        let config = serde_json::json!({
            "request_timeout_seconds": 30,
            "connect_timeout_seconds": 10
        });
        let provider = manager
            .create_provider("alist", "test_alist_timeout", &config)
            .await;
        assert!(provider.is_ok());

        // Verify the provider was stored
        let stored = manager.get("test_alist_timeout").await;
        assert!(stored.is_some());
    }

    #[tokio::test]
    async fn test_bilibili_provider_config_with_timeout() {
        // Test that Bilibili provider accepts per-instance HTTP timeout overrides.
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create provider with Bilibili-specific timeout config.
        let config = serde_json::json!({
            "request_timeout_seconds": 45,
            "connect_timeout_seconds": 10
        });
        let provider = manager
            .create_provider("bilibili", "test_bilibili_timeout", &config)
            .await;
        assert!(provider.is_ok());

        // Verify the provider was stored
        let stored = manager.get("test_bilibili_timeout").await;
        assert!(stored.is_some());
    }

    #[tokio::test]
    async fn test_emby_provider_config_with_timeout() {
        // Test that Emby provider accepts per-instance HTTP timeout overrides.
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create provider with Emby-specific timeout config.
        let config = serde_json::json!({
            "request_timeout_seconds": 60,
            "connect_timeout_seconds": 10
        });
        let provider = manager
            .create_provider("emby", "test_emby_timeout", &config)
            .await;
        assert!(provider.is_ok());

        // Verify the provider was stored
        let stored = manager.get("test_emby_timeout").await;
        assert!(stored.is_some());
    }

    #[tokio::test]
    async fn test_provider_config_invalid_timeout_ignored() {
        // Test that invalid timeout values are gracefully handled
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create provider with invalid timeout type (string instead of number).
        let config = serde_json::json!({
            "request_timeout_seconds": "invalid"
        });
        let provider = manager
            .create_provider("alist", "test_alist_invalid", &config)
            .await;
        // Should still succeed, just ignore the invalid timeout
        assert!(provider.is_ok());
    }

    #[tokio::test]
    async fn test_new_with_provider_http_client_accepts_explicit_default_client() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let client = synctv_common::http::SsrfSafeClientBuilder::provider()
            .connect_timeout(std::time::Duration::from_secs(4))
            .request_timeout(std::time::Duration::from_secs(12))
            .build()
            .unwrap();

        let manager = ProvidersManager::new_with_provider_http_client(instance_manager, client);

        assert!(manager.has_factory("alist"));
        assert!(manager.has_factory("bilibili"));
        assert!(manager.has_factory("emby"));
    }

    #[tokio::test]
    async fn test_default_provider_instances_use_injected_default_client_manager() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let client = synctv_common::http::SsrfSafeClientBuilder::provider()
            .connect_timeout(std::time::Duration::from_secs(4))
            .request_timeout(std::time::Duration::from_secs(12))
            .build()
            .unwrap();

        let manager = ProvidersManager::new_with_provider_http_client(instance_manager, client);
        let expected_marker = manager.default_client_manager_marker();

        for provider_type in ["alist", "bilibili", "emby"] {
            let provider = manager
                .create_provider(
                    provider_type,
                    &format!("{provider_type}_default"),
                    &serde_json::json!({}),
                )
                .await
                .unwrap();

            assert_eq!(
                provider.test_client_manager_marker(),
                Some(expected_marker),
                "default {provider_type} provider should reuse the injected default client manager",
            );
        }
    }

    #[tokio::test]
    async fn test_per_instance_timeout_override_keeps_dedicated_client_manager() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let client = synctv_common::http::SsrfSafeClientBuilder::provider()
            .connect_timeout(std::time::Duration::from_secs(4))
            .request_timeout(std::time::Duration::from_secs(12))
            .build()
            .unwrap();

        let manager = ProvidersManager::new_with_provider_http_client(instance_manager, client);
        let default_marker = manager.default_client_manager_marker();

        let provider = manager
            .create_provider(
                "alist",
                "alist_override",
                &serde_json::json!({
                    "request_timeout_seconds": 30,
                    "connect_timeout_seconds": 4
                }),
            )
            .await
            .unwrap();

        let actual_marker = provider
            .test_client_manager_marker()
            .expect("test provider should expose its client manager marker");
        assert_ne!(
            actual_marker, default_marker,
            "per-instance timeout overrides should build a dedicated client manager"
        );
    }

    #[tokio::test]
    async fn test_rtmp_provider_no_longer_requires_base_url() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        let config = serde_json::json!({});
        let provider = manager.create_provider("rtmp", "test_rtmp", &config).await;
        assert!(provider.is_ok());
    }

    #[tokio::test]
    async fn test_live_proxy_provider_no_longer_requires_base_url() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        let config = serde_json::json!({});
        let provider = manager
            .create_provider("live_proxy", "test_live_proxy", &config)
            .await;
        assert!(provider.is_ok());
    }

    #[tokio::test]
    async fn test_provider_lookup_by_instance_id() {
        // Test getting providers by instance ID
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create a provider
        let config = serde_json::json!({});
        manager
            .create_provider("alist", "my_alist_instance", &config)
            .await
            .unwrap();

        // Get by instance ID
        let provider = manager.get("my_alist_instance").await;
        assert!(provider.is_some());
        assert_eq!(provider.unwrap().name(), "alist");

        // Get nonexistent instance
        let not_found = manager.get("nonexistent").await;
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_provider_lookup_by_type() {
        // Test getting providers by type (returns default instance)
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create default instance
        let config = serde_json::json!({});
        manager
            .create_provider("alist", "alist", &config)
            .await
            .unwrap();

        // Get by type
        let provider = manager.get_by_type("alist").await;
        assert!(provider.is_some());
        assert_eq!(provider.unwrap().name(), "alist");

        // Get unknown type
        let not_found = manager.get_by_type("unknown").await;
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_resolve_provider_uses_default_for_missing_or_empty_instance() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        manager
            .create_provider("alist", "alist", &serde_json::json!({}))
            .await
            .unwrap();

        let implicit_default = manager.resolve_provider("alist", None).await.unwrap();
        let empty_default = manager.resolve_provider("alist", Some("  ")).await.unwrap();

        assert_eq!(implicit_default.name(), "alist");
        assert_eq!(empty_default.name(), "alist");
        assert!(
            Arc::ptr_eq(&implicit_default, &empty_default),
            "None and empty provider_instance_name must resolve to the same default provider"
        );
    }

    #[tokio::test]
    async fn test_resolve_provider_uses_explicit_local_instance_and_checks_type() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        manager
            .create_provider("alist", "alist_alt", &serde_json::json!({}))
            .await
            .unwrap();

        let provider = manager
            .resolve_provider("alist", Some(" alist_alt "))
            .await
            .unwrap();
        assert_eq!(provider.name(), "alist");

        let error = match manager.resolve_provider("emby", Some("alist_alt")).await {
            Ok(provider) => panic!(
                "explicit local instance must match the requested provider type, got provider '{}'",
                provider.name()
            ),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("request declared 'emby'"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn test_provider_singleton_pattern() {
        // Test singleton pattern - creating provider with same instance_id replaces previous
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create first instance
        let config1 = serde_json::json!({"request_timeout_seconds": 10});
        let provider1 = manager
            .create_provider("alist", "alist_singleton", &config1)
            .await
            .unwrap();
        assert_eq!(provider1.name(), "alist");

        // Create second instance with same ID (should replace)
        let config2 = serde_json::json!({"request_timeout_seconds": 30});
        let provider2 = manager
            .create_provider("alist", "alist_singleton", &config2)
            .await
            .unwrap();
        assert_eq!(provider2.name(), "alist");

        // Both Arcs point to different instances (second replaced first in map)
        // but first is still valid via Arc
        assert!(!Arc::ptr_eq(&provider1, &provider2));

        // The manager now returns the second instance
        let stored = manager.get("alist_singleton").await.unwrap();
        assert!(Arc::ptr_eq(&provider2, &stored));
    }

    #[tokio::test]
    async fn test_provider_list() {
        // Test listing all provider instances
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        // Initially empty
        let list = manager.list().await;
        assert!(list.is_empty());

        // Create multiple providers
        manager
            .create_provider("alist", "alist1", &serde_json::json!({}))
            .await
            .unwrap();
        manager
            .create_provider("bilibili", "bilibili1", &serde_json::json!({}))
            .await
            .unwrap();
        manager
            .create_provider("emby", "emby1", &serde_json::json!({}))
            .await
            .unwrap();

        // List all
        let list = manager.list().await;
        assert_eq!(list.len(), 3);

        // Verify names
        let names: Vec<&str> = list.iter().map(|p| p.name()).collect();
        assert!(names.contains(&"alist"));
        assert!(names.contains(&"bilibili"));
        assert!(names.contains(&"emby"));
    }

    #[tokio::test]
    async fn test_provider_remove() {
        // Test removing provider instances
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create provider
        manager
            .create_provider("alist", "alist_remove", &serde_json::json!({}))
            .await
            .unwrap();

        // Verify it exists
        assert!(manager.get("alist_remove").await.is_some());

        // Remove it
        let removed = manager.remove("alist_remove").await;
        assert!(removed.is_some());

        // Verify it's gone
        assert!(manager.get("alist_remove").await.is_none());

        // Remove nonexistent
        let not_found = manager.remove("nonexistent").await;
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_provider_factory_unknown_type() {
        // Test creating provider with unknown type
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager);

        // Try to create unknown provider type
        let config = serde_json::json!({});
        let result = manager
            .create_provider("unknown_type", "test", &config)
            .await;

        assert!(result.is_err());
        match result {
            Err(crate::Error::NotFound(msg)) => {
                assert!(msg.contains("Unknown provider type"));
            }
            _ => panic!("Expected NotFound error"),
        }
    }

    #[tokio::test]
    async fn test_concurrent_provider_creation() {
        // Test concurrent provider creation doesn't cause races
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = Arc::new(ProvidersManager::new(instance_manager));

        // Spawn multiple tasks creating different providers concurrently
        let mut handles = vec![];

        for i in 0..5 {
            let mgr = manager.clone();
            handles.push(tokio::spawn(async move {
                let instance_id = format!("alist_concurrent_{i}");
                mgr.create_provider("alist", &instance_id, &serde_json::json!({}))
                    .await
            }));
        }

        // All should succeed
        let results: Vec<_> = futures::future::join_all(handles).await;
        for result in results {
            assert!(result.is_ok());
            assert!(result.unwrap().is_ok());
        }

        // Verify all instances exist
        for i in 0..5 {
            let instance_id = format!("alist_concurrent_{i}");
            assert!(manager.get(&instance_id).await.is_some());
        }
    }

    #[tokio::test]
    async fn test_instance_manager_reference() {
        // Test that ProvidersManager holds a reference to RemoteProviderManager
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(repo, None));
        let manager = ProvidersManager::new(instance_manager.clone());

        // Verify instance_manager is accessible
        let retrieved = manager.instance_manager();
        assert!(Arc::ptr_eq(&instance_manager, retrieved));
    }
}
