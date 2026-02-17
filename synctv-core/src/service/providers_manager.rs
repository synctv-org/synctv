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
        // Alist factory
        self.register_factory(
            "alist",
            Box::new(|_instance_id, _config, instance_manager| {
                Ok(Arc::new(AlistProvider::new(instance_manager)))
            }),
        );

        // Bilibili factory
        self.register_factory(
            "bilibili",
            Box::new(|_instance_id, _config, instance_manager| {
                Ok(Arc::new(BilibiliProvider::new(instance_manager)))
            }),
        );

        // Emby factory
        self.register_factory(
            "emby",
            Box::new(|_instance_id, _config, instance_manager| {
                Ok(Arc::new(EmbyProvider::new(instance_manager)))
            }),
        );

        // RTMP factory
        self.register_factory(
            "rtmp",
            Box::new(|_instance_id, config, _instance_manager| {
                let base_url = config
                    .get("base_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("http://localhost:8080");

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
            Box::new(|_instance_id, config, _instance_manager| {
                let base_url = config
                    .get("base_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("http://localhost:8080");

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
}
