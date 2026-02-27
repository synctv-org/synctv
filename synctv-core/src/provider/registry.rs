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

    // ========== Dynamic Operation Tests (Task #71) ==========

    #[test]
    fn test_concurrent_create_same_instance_id() {
        // Test that concurrent creation of same-named instances doesn't cause data races
        use std::thread;

        let registry = Arc::new(ProviderRegistry::new());
        registry.register_factory(
            "mock",
            Box::new(|_instance_id, _config| {
                Ok(Arc::new(MockProvider {}))
            }),
        );

        let mut handles = vec![];

        // Spawn 10 threads all trying to create the same instance
        for _ in 0..10 {
            let reg = Arc::clone(&registry);
            handles.push(thread::spawn(move || {
                reg.create_instance("mock", "mock_concurrent", serde_json::json!({}))
            }));
        }

        // All should succeed (last write wins in DashMap)
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        for result in results {
            assert!(result.is_ok(), "Concurrent create should succeed (last write wins)");
        }

        // Verify instance exists
        let provider = registry.get_instance("mock_concurrent");
        assert!(provider.is_some(), "Instance should exist after concurrent creation");
    }

    #[test]
    fn test_remove_instance_in_use() {
        // Test removing an instance that's currently being used
        let registry = ProviderRegistry::new();
        registry.register_factory(
            "mock",
            Box::new(|_instance_id, _config| {
                Ok(Arc::new(MockProvider {}))
            }),
        );

        // Create instance
        registry
            .create_instance("mock", "mock_in_use", serde_json::json!({}))
            .unwrap();

        // Get a reference to the instance (simulating "in use")
        let provider_ref = registry.get_instance("mock_in_use").unwrap();

        // Remove the instance
        let removed = registry.remove_instance("mock_in_use");
        assert!(removed, "Instance should be removed");

        // The old reference should still be valid (Arc keeps it alive)
        assert_eq!(provider_ref.name(), "mock");

        // But new lookups should fail
        let not_found = registry.get_instance("mock_in_use");
        assert!(not_found.is_none(), "Removed instance should not be found");
    }

    #[test]
    fn test_factory_error_handling() {
        // Test that factory function errors are properly propagated
        use super::ProviderError;

        let registry = ProviderRegistry::new();
        registry.register_factory(
            "failing",
            Box::new(|_instance_id, _config| {
                Err(ProviderError::InvalidConfig("Intentional failure for testing".to_string()))
            }),
        );

        // Try to create instance with failing factory
        let result = registry.create_instance("failing", "fail_instance", serde_json::json!({}));

        assert!(result.is_err(), "Factory error should propagate");
        match result {
            Err(ProviderError::InvalidConfig(msg)) => {
                assert!(msg.contains("Intentional failure"), "Error message should be preserved");
            }
            _ => panic!("Expected InvalidConfig error"),
        }

        // Instance should not exist in registry
        let not_found = registry.get_instance("fail_instance");
        assert!(not_found.is_none(), "Failed instance should not be registered");
    }

    #[test]
    fn test_create_unknown_provider_type() {
        // Test creating instance of unregistered provider type
        let registry = ProviderRegistry::new();

        let result = registry.create_instance("unknown", "unknown_instance", serde_json::json!({}));

        assert!(result.is_err(), "Unknown provider type should fail");
        match result {
            Err(ProviderError::InstanceNotFound(name)) => {
                assert_eq!(name, "unknown", "Error should mention the provider type");
            }
            _ => panic!("Expected InstanceNotFound error"),
        }
    }

    #[test]
    fn test_remove_nonexistent_instance() {
        // Test removing an instance that doesn't exist
        let registry = ProviderRegistry::new();

        let removed = registry.remove_instance("nonexistent");
        assert!(!removed, "Removing nonexistent instance should return false");
    }

    #[test]
    fn test_list_instances() {
        // Test listing all registered instances
        let registry = ProviderRegistry::new();
        registry.register_factory(
            "mock",
            Box::new(|_instance_id, _config| {
                Ok(Arc::new(MockProvider {}))
            }),
        );

        // Initially empty
        assert!(registry.list_instances().is_empty());

        // Create multiple instances
        registry.create_instance("mock", "inst1", serde_json::json!({})).unwrap();
        registry.create_instance("mock", "inst2", serde_json::json!({})).unwrap();
        registry.create_instance("mock", "inst3", serde_json::json!({})).unwrap();

        let instances = registry.list_instances();
        assert_eq!(instances.len(), 3);
        assert!(instances.contains(&"inst1".to_string()));
        assert!(instances.contains(&"inst2".to_string()));
        assert!(instances.contains(&"inst3".to_string()));
    }
}
