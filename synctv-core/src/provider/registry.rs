// Provider Registry
//
// Factory-based registry for managing provider instances

use super::{MediaProvider, ProviderError};
use dashmap::DashMap;
use parking_lot::RwLock;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Provider factory function type
pub type ProviderFactory =
    Box<dyn Fn(&str, Value) -> Result<Arc<dyn MediaProvider>, ProviderError> + Send + Sync>;

/// Thread-safe provider registry for managing instances.
///
/// Uses factory pattern to create provider instances from configuration.
/// Each provider type registers a factory function.
///
/// Factories are behind a `parking_lot::RwLock` (registered at startup, rarely mutated).
/// Unlike `std::sync::RwLock`, `parking_lot`'s `RwLock` does not poison on panic.
/// Instances use `DashMap` for lock-free concurrent reads.
pub struct ProviderRegistry {
    /// Registered provider factories by type name
    factories: RwLock<HashMap<String, ProviderFactory>>,

    /// Created provider instances by `instance_id`
    instances: DashMap<String, Arc<dyn MediaProvider>>,
}

impl ProviderRegistry {
    /// Create new empty registry
    #[must_use]
    pub fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
            instances: DashMap::new(),
        }
    }

    /// Register a provider factory
    ///
    /// # Example
    /// ```text
    /// registry.register_factory("bilibili", Box::new(|instance_id, config| {
    ///     Ok(Arc::new(BilibiliProvider::new(instance_id, config)?))
    /// }));
    /// ```
    pub fn register_factory(&self, provider_type: &str, factory: ProviderFactory) {
        self.factories
            .write()
            .insert(provider_type.to_string(), factory);
    }

    /// Create and register a provider instance
    ///
    /// # Arguments
    /// - `provider_type`: Type of provider (e.g., "bilibili", "alist")
    /// - `instance_id`: Unique instance ID (e.g., "`bilibili_main`", "`alist_company`")
    /// - `config`: Provider-specific configuration
    ///
    /// # Example
    /// ```text
    /// let config = json!({
    ///     "base_url": "https://api.bilibili.com",
    ///     "cookies": "..."
    /// });
    /// registry.create_instance("bilibili", "bilibili_main", config)?;
    /// ```
    pub fn create_instance(
        &self,
        provider_type: &str,
        instance_id: &str,
        config: Value,
    ) -> Result<(), ProviderError> {
        let factories = self.factories.read();
        let factory = factories
            .get(provider_type)
            .ok_or_else(|| ProviderError::InstanceNotFound(provider_type.to_string()))?;

        let instance = factory(instance_id, config)?;
        drop(factories); // release read lock before inserting into DashMap
        self.instances.insert(instance_id.to_string(), instance);

        Ok(())
    }

    /// Get provider instance by ID
    ///
    /// # Example
    /// ```text
    /// let provider = registry.get_instance("bilibili_main")?;
    /// let result = provider.generate_playback(&ctx, &source_config).await?;
    /// ```
    #[must_use]
    pub fn get_instance(&self, instance_id: &str) -> Option<Arc<dyn MediaProvider>> {
        self.instances.get(instance_id).map(|r| r.value().clone())
    }

    /// List all registered instances
    #[must_use]
    pub fn list_instances(&self) -> Vec<String> {
        self.instances.iter().map(|r| r.key().clone()).collect()
    }

    /// Remove an instance
    pub fn remove_instance(&self, instance_id: &str) -> bool {
        self.instances.remove(instance_id).is_some()
    }

    // Note: Service/route registration is now handled via extension traits
    // in synctv-api layer. Use list_instances() to get all providers.
    // See:
    // - synctv-api/src/http/provider_extensions.rs
    // - synctv-api/src/grpc/provider_extensions.rs
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockProvider {}

    #[async_trait::async_trait]
    impl MediaProvider for MockProvider {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn generate_playback(
            &self,
            _ctx: &super::super::ProviderContext<'_>,
            _source_config: &Value,
        ) -> Result<super::super::PlaybackResult, ProviderError> {
            Ok(super::super::PlaybackResult {
                playback_infos: HashMap::new(),
                default_mode: "direct".to_string(),
                metadata: HashMap::new(),
            })
        }
    }

    #[test]
    fn test_registry_factory() {
        let registry = ProviderRegistry::new();

        // Register factory
        registry.register_factory(
            "mock",
            Box::new(|_instance_id, _config| {
                Ok(Arc::new(MockProvider {}))
            }),
        );

        // Create instance
        registry
            .create_instance("mock", "mock_main", serde_json::json!({}))
            .unwrap();

        // Get instance
        let provider = registry.get_instance("mock_main").unwrap();
        assert_eq!(provider.name(), "mock");
    }
}
