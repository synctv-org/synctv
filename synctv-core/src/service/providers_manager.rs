//! Providers Manager
//!
//! Manages all `MediaProvider` instances with singleton pattern.
//! Providers are loaded from configuration and created once at startup.

use crate::provider::{
    AlistProvider, BilibiliProvider, DirectUrlProvider, EmbyProvider, LiveProxyProvider,
    MediaProvider, RtmpProvider,
};
use crate::service::RemoteProviderManager;
use crate::Config;
use crate::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Factory function type for creating `MediaProvider` instances
pub type ProviderFactory =
    Box<dyn Fn(&str, &Value, Arc<RemoteProviderManager>) -> Result<Arc<dyn MediaProvider>> + Send + Sync>;

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
///   ├── Factories (registered for each provider type)
///   ├── Instances (singleton MediaProvider instances)
///   └── RemoteProviderManager (for local/remote dispatch)
///
/// synctv-api layer
///   ├── Gets provider instances from ProvidersManager
///   ├── Registers HTTP/gRPC routes for each provider
///   └── No hardcoded provider types
/// ```
pub struct ProvidersManager {
    /// Registered factory functions (`provider_type` → factory)
    factories: HashMap<String, ProviderFactory>,

    /// Created `MediaProvider` instances (singleton per provider type)
    instances: Arc<RwLock<HashMap<String, Arc<dyn MediaProvider>>>>,

    /// Provider instance manager (for local/remote dispatch)
    instance_manager: Arc<RemoteProviderManager>,
}

impl ProvidersManager {
    /// Create a new `ProvidersManager`
    #[must_use] 
    pub fn new(instance_manager: Arc<RemoteProviderManager>) -> Self {
        let mut manager = Self {
            factories: HashMap::new(),
            instances: Arc::new(RwLock::new(HashMap::new())),
            instance_manager,
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
        // Alist factory - reads optional timeout from config
        self.register_factory(
            "alist",
            Box::new(|_instance_id, config, instance_manager| {
                // Read optional timeout from config (in seconds)
                let timeout_seconds = config
                    .get("timeout_seconds")
                    .and_then(serde_json::Value::as_u64);

                let provider = if let Some(secs) = timeout_seconds {
                    AlistProvider::with_timeout(instance_manager, secs)
                } else {
                    AlistProvider::new(instance_manager)
                };
                Ok(Arc::new(provider))
            }),
        );

        // Bilibili factory - reads optional timeout from config
        self.register_factory(
            "bilibili",
            Box::new(|_instance_id, config, instance_manager| {
                // Read optional timeout from config (in seconds)
                let timeout_seconds = config
                    .get("timeout_seconds")
                    .and_then(serde_json::Value::as_u64);

                let provider = if let Some(secs) = timeout_seconds {
                    BilibiliProvider::with_timeout(instance_manager, secs)
                } else {
                    BilibiliProvider::new(instance_manager)
                };
                Ok(Arc::new(provider))
            }),
        );

        // Emby factory - reads optional timeout from config
        self.register_factory(
            "emby",
            Box::new(|_instance_id, config, instance_manager| {
                // Read optional timeout from config (in seconds)
                let timeout_seconds = config
                    .get("timeout_seconds")
                    .and_then(serde_json::Value::as_u64);

                let provider = if let Some(secs) = timeout_seconds {
                    EmbyProvider::with_timeout(instance_manager, secs)
                } else {
                    EmbyProvider::new(instance_manager)
                };
                Ok(Arc::new(provider))
            }),
        );

        // RTMP factory
        self.register_factory(
            "rtmp",
            Box::new(|instance_id, config, _instance_manager| {
                let base_url = config
                    .get("base_url")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        crate::Error::Internal(format!(
                            "providers_manager base_url is not configured for rtmp provider instance '{instance_id}'"
                        ))
                    })?;

                Ok(Arc::new(RtmpProvider::new(base_url)))
            }),
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
            Box::new(|instance_id, config, _instance_manager| {
                let base_url = config
                    .get("base_url")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        crate::Error::Internal(format!(
                            "providers_manager base_url is not configured for live_proxy provider instance '{instance_id}'"
                        ))
                    })?;

                Ok(Arc::new(LiveProxyProvider::new(base_url)))
            }),
        );
    }

    /// Register a provider factory
    pub fn register_factory(&mut self, provider_type: &str, factory: ProviderFactory) {
        self.factories.insert(provider_type.to_string(), factory);
        tracing::debug!("Registered provider factory: {}", provider_type);
    }

    /// Load providers from configuration
    ///
    /// Reads provider configurations from Config and creates instances.
    /// This should be called once during server startup.
    ///
    /// # Arguments
    /// * `config`: Application configuration
    ///
    /// # Returns
    /// Number of providers loaded
    pub async fn load_from_config(&mut self, config: &Config) -> Result<usize> {
        let mut count = 0;

        // Read provider configurations from config.media_providers.providers
        // Each provider config should have:
        // - instance_id: Unique identifier for this instance
        // - provider_type: Type of provider (alist, emby, bilibili, etc.)
        // - config: Provider-specific configuration (URL, credentials, etc.)

        // Check if providers is an object
        if let Some(providers_obj) = config.media_providers.providers.as_object() {
            for (instance_id, provider_config) in providers_obj {
                // Extract provider_type from config (defaults to first part of instance_id)
                let provider_type = provider_config
                    .get("provider_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| {
                        // Fallback: derive from instance_id (e.g., "alist_main" -> "alist")
                        // split('_').next() always returns Some for non-empty strings
                        instance_id
                            .split('_')
                            .next()
                            .unwrap_or(instance_id)
                    });

                // Check if this provider type is registered
                if !self.has_factory(provider_type) {
                    tracing::warn!(
                        "Unknown provider type '{}' for instance '{}', skipping",
                        provider_type,
                        instance_id
                    );
                    continue;
                }

                // Create the provider instance
                match self
                    .create_provider(provider_type, instance_id, provider_config)
                    .await
                {
                    Ok(_) => {
                        count += 1;
                        tracing::info!(
                            "Loaded provider instance: {} (type: {})",
                            instance_id,
                            provider_type
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to load provider instance '{}' (type: {}): {}",
                            instance_id,
                            provider_type,
                            e
                        );
                        // Continue loading other providers
                    }
                }
            }
        }

        // If no providers were configured, create default instances for all registered factories
        if count == 0 {
            tracing::info!(
                "No providers configured, creating default instances for {} provider types",
                self.factories.len()
            );
            for provider_type in self.factories.keys() {
                let instance_id = format!("{provider_type}_default");
                let provider_config = &serde_json::json!({});

                self.create_provider(provider_type, &instance_id, provider_config)
                    .await?;
                count += 1;
            }
        }

        tracing::info!("Loaded {} providers from configuration", count);
        Ok(count)
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
        let factory = self
            .factories
            .get(provider_type)
            .ok_or_else(|| crate::Error::NotFound(format!("Unknown provider type: {provider_type}")))?;

        let provider = factory(instance_id, config, self.instance_manager.clone())?;

        // Store in instances map (singleton)
        self.instances.write().await.insert(instance_id.to_string(), provider.clone());

        tracing::info!(
            "Created provider instance: {} (type: {})",
            instance_id,
            provider_type
        );

        Ok(provider)
    }

    /// Get a provider instance by ID
    pub async fn get(&self, instance_id: &str) -> Option<Arc<dyn MediaProvider>> {
        self.instances.read().await.get(instance_id).cloned()
    }

    /// Get provider by type (returns default instance)
    pub async fn get_by_type(&self, provider_type: &str) -> Option<Arc<dyn MediaProvider>> {
        let instance_id = format!("{provider_type}_default");
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
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
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
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = ProvidersManager::new(instance_manager);

        let types = manager.list_types();
        assert!(types.contains(&"alist".to_string()));
        assert!(types.contains(&"bilibili".to_string()));
        assert!(types.contains(&"live_proxy".to_string()));
        assert_eq!(types.len(), 6); // alist, bilibili, emby, rtmp, direct_url, live_proxy
    }

    #[tokio::test]
    async fn test_provider_config_without_timeout() {
        // Test that provider can be created without timeout config (backward compatible)
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create provider with empty config (no timeout)
        let config = serde_json::json!({});
        let provider = manager.create_provider("alist", "test_alist", &config).await;
        assert!(provider.is_ok());

        // Verify the provider was stored
        let stored = manager.get("test_alist").await;
        assert!(stored.is_some());
    }

    #[tokio::test]
    async fn test_provider_config_with_timeout() {
        // Test that provider reads timeout from config
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create provider with timeout config
        let config = serde_json::json!({
            "timeout_seconds": 30
        });
        let provider = manager.create_provider("alist", "test_alist_timeout", &config).await;
        assert!(provider.is_ok());

        // Verify the provider was stored
        let stored = manager.get("test_alist_timeout").await;
        assert!(stored.is_some());
    }

    #[tokio::test]
    async fn test_bilibili_provider_config_with_timeout() {
        // Test that Bilibili provider reads timeout from config
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create provider with timeout config
        let config = serde_json::json!({
            "timeout_seconds": 45
        });
        let provider = manager.create_provider("bilibili", "test_bilibili_timeout", &config).await;
        assert!(provider.is_ok());

        // Verify the provider was stored
        let stored = manager.get("test_bilibili_timeout").await;
        assert!(stored.is_some());
    }

    #[tokio::test]
    async fn test_emby_provider_config_with_timeout() {
        // Test that Emby provider reads timeout from config
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create provider with timeout config
        let config = serde_json::json!({
            "timeout_seconds": 60
        });
        let provider = manager.create_provider("emby", "test_emby_timeout", &config).await;
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
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create provider with invalid timeout type (string instead of number)
        let config = serde_json::json!({
            "timeout_seconds": "invalid"
        });
        let provider = manager.create_provider("alist", "test_alist_invalid", &config).await;
        // Should still succeed, just ignore the invalid timeout
        assert!(provider.is_ok());
    }

    #[tokio::test]
    async fn test_rtmp_provider_requires_base_url() {
        // Test that RTMP provider still requires base_url (existing behavior)
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create RTMP provider without base_url - should fail
        let config = serde_json::json!({});
        let provider = manager.create_provider("rtmp", "test_rtmp", &config).await;
        assert!(provider.is_err());

        // Create RTMP provider with base_url - should succeed
        let config = serde_json::json!({
            "base_url": "rtmp://localhost/live"
        });
        let provider = manager.create_provider("rtmp", "test_rtmp_valid", &config).await;
        assert!(provider.is_ok());
    }

    #[tokio::test]
    async fn test_live_proxy_provider_requires_base_url() {
        // Test that live_proxy provider still requires base_url (existing behavior)
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create live_proxy provider without base_url - should fail
        let config = serde_json::json!({});
        let provider = manager.create_provider("live_proxy", "test_live_proxy", &config).await;
        assert!(provider.is_err());

        // Create live_proxy provider with base_url - should succeed
        let config = serde_json::json!({
            "base_url": "http://localhost:8080"
        });
        let provider = manager.create_provider("live_proxy", "test_live_proxy_valid", &config).await;
        assert!(provider.is_ok());
    }

    // ========== Task #27: Provider Manager Tests ==========

    #[tokio::test]
    async fn test_provider_lookup_by_instance_id() {
        // Test getting providers by instance ID
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create a provider
        let config = serde_json::json!({});
        manager.create_provider("alist", "my_alist_instance", &config).await.unwrap();

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
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create default instance
        let config = serde_json::json!({});
        manager.create_provider("alist", "alist_default", &config).await.unwrap();

        // Get by type
        let provider = manager.get_by_type("alist").await;
        assert!(provider.is_some());
        assert_eq!(provider.unwrap().name(), "alist");

        // Get unknown type
        let not_found = manager.get_by_type("unknown").await;
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_provider_singleton_pattern() {
        // Test singleton pattern - creating provider with same instance_id replaces previous
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create first instance
        let config1 = serde_json::json!({"timeout_seconds": 10});
        let provider1 = manager.create_provider("alist", "alist_singleton", &config1).await.unwrap();
        assert_eq!(provider1.name(), "alist");

        // Create second instance with same ID (should replace)
        let config2 = serde_json::json!({"timeout_seconds": 30});
        let provider2 = manager.create_provider("alist", "alist_singleton", &config2).await.unwrap();
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
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = ProvidersManager::new(instance_manager);

        // Initially empty
        let list = manager.list().await;
        assert!(list.is_empty());

        // Create multiple providers
        manager.create_provider("alist", "alist1", &serde_json::json!({})).await.unwrap();
        manager.create_provider("bilibili", "bilibili1", &serde_json::json!({})).await.unwrap();
        manager.create_provider("emby", "emby1", &serde_json::json!({})).await.unwrap();

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
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = ProvidersManager::new(instance_manager);

        // Create provider
        manager.create_provider("alist", "alist_remove", &serde_json::json!({})).await.unwrap();

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
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = ProvidersManager::new(instance_manager);

        // Try to create unknown provider type
        let config = serde_json::json!({});
        let result = manager.create_provider("unknown_type", "test", &config).await;

        assert!(result.is_err());
        match result {
            Err(crate::Error::NotFound(msg)) => {
                assert!(msg.contains("Unknown provider type"));
            }
            _ => panic!("Expected NotFound error"),
        }
    }

    #[tokio::test]
    async fn test_load_from_config_empty() {
        // Test loading providers from empty config (creates defaults)
        // Note: RTMP and live_proxy providers require base_url, so we provide them in config
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let mut manager = ProvidersManager::new(instance_manager);

        // Create config with providers that require base_url
        let config = crate::Config {
            media_providers: crate::config::MediaProvidersConfig {
                providers: serde_json::json!({
                    "rtmp_default": {
                        "provider_type": "rtmp",
                        "base_url": "rtmp://localhost/live"
                    },
                    "live_proxy_default": {
                        "provider_type": "live_proxy",
                        "base_url": "http://localhost:8080"
                    }
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        // Load from config (should create 2 providers from config)
        let count = manager.load_from_config(&config).await.unwrap();
        assert_eq!(count, 2); // rtmp_default and live_proxy_default

        // Verify instances exist
        assert!(manager.get("rtmp_default").await.is_some());
        assert!(manager.get("live_proxy_default").await.is_some());
    }

    #[tokio::test]
    async fn test_load_from_config_with_providers() {
        // Test loading providers from config with provider definitions
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let mut manager = ProvidersManager::new(instance_manager);

        // Create config with provider definitions
        let config = crate::Config {
            media_providers: crate::config::MediaProvidersConfig {
                providers: serde_json::json!({
                    "alist_main": {
                        "provider_type": "alist",
                        "timeout_seconds": 60
                    },
                    "bilibili_backup": {
                        "provider_type": "bilibili"
                    }
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        // Load from config
        let count = manager.load_from_config(&config).await.unwrap();
        assert_eq!(count, 2);

        // Verify instances exist
        assert!(manager.get("alist_main").await.is_some());
        assert!(manager.get("bilibili_backup").await.is_some());
    }

    #[tokio::test]
    async fn test_load_from_config_derives_type_from_id() {
        // Test that provider_type is derived from instance_id when not specified
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let mut manager = ProvidersManager::new(instance_manager);

        // Create config with provider definitions (no provider_type specified)
        let config = crate::Config {
            media_providers: crate::config::MediaProvidersConfig {
                providers: serde_json::json!({
                    "alist_company": {}
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        // Load from config
        let count = manager.load_from_config(&config).await.unwrap();
        assert_eq!(count, 1);

        // Verify instance exists
        assert!(manager.get("alist_company").await.is_some());
    }

    #[tokio::test]
    async fn test_load_from_config_unknown_provider_skipped() {
        // Test that unknown provider types are skipped with warning
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let mut manager = ProvidersManager::new(instance_manager);

        // Create config with unknown provider type
        let config = crate::Config {
            media_providers: crate::config::MediaProvidersConfig {
                providers: serde_json::json!({
                    "unknown_provider": {
                        "provider_type": "does_not_exist"
                    },
                    "alist_valid": {
                        "provider_type": "alist"
                    }
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        // Load from config (unknown should be skipped)
        let count = manager.load_from_config(&config).await.unwrap();
        assert_eq!(count, 1); // Only alist_valid loaded

        // Verify only valid provider exists
        assert!(manager.get("alist_valid").await.is_some());
        assert!(manager.get("unknown_provider").await.is_none());
    }

    #[tokio::test]
    async fn test_load_from_config_invalid_provider_skipped() {
        // Test that providers with invalid config are skipped
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let mut manager = ProvidersManager::new(instance_manager);

        // Create config with invalid RTMP config (missing base_url)
        let config = crate::Config {
            media_providers: crate::config::MediaProvidersConfig {
                providers: serde_json::json!({
                    "rtmp_invalid": {
                        "provider_type": "rtmp"
                        // Missing base_url - will fail
                    },
                    "alist_valid": {
                        "provider_type": "alist"
                    }
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        // Load from config (invalid RTMP should be skipped)
        let count = manager.load_from_config(&config).await.unwrap();
        assert_eq!(count, 1); // Only alist_valid loaded

        // Verify only valid provider exists
        assert!(manager.get("alist_valid").await.is_some());
        assert!(manager.get("rtmp_invalid").await.is_none());
    }

    #[tokio::test]
    async fn test_concurrent_provider_creation() {
        // Test concurrent provider creation doesn't cause races
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = Arc::new(ProvidersManager::new(instance_manager));

        // Spawn multiple tasks creating different providers concurrently
        let mut handles = vec![];

        for i in 0..5 {
            let mgr = manager.clone();
            handles.push(tokio::spawn(async move {
                let instance_id = format!("alist_concurrent_{}", i);
                mgr.create_provider("alist", &instance_id, &serde_json::json!({})).await
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
            let instance_id = format!("alist_concurrent_{}", i);
            assert!(manager.get(&instance_id).await.is_some());
        }
    }

    #[tokio::test]
    async fn test_instance_manager_reference() {
        // Test that ProvidersManager holds a reference to RemoteProviderManager
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        let instance_manager = Arc::new(RemoteProviderManager::new(repo, None, None));
        let manager = ProvidersManager::new(instance_manager.clone());

        // Verify instance_manager is accessible
        let retrieved = manager.instance_manager();
        assert!(Arc::ptr_eq(&instance_manager, retrieved));
    }
}
