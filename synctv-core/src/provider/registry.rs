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

    // ========== Task #27: Registry Additional Tests ==========

    #[test]
    fn test_factory_registration() {
        // Test that factories can be registered and replaced
        let registry = ProviderRegistry::new();

        // Register first factory
        registry.register_factory(
            "mock",
            Box::new(|_instance_id, _config| {
                Ok(Arc::new(MockProvider {}))
            }),
        );

        // Create instance with first factory
        registry
            .create_instance("mock", "test1", serde_json::json!({}))
            .unwrap();
        let provider = registry.get_instance("test1").unwrap();
        assert_eq!(provider.name(), "mock");

        // Register new factory (replaces old one)
        registry.register_factory(
            "mock",
            Box::new(|_instance_id, _config| {
                // This factory would create a different instance
                // but for this test we just verify registration doesn't panic
                Ok(Arc::new(MockProvider {}))
            }),
        );

        // Create instance with new factory
        registry
            .create_instance("mock", "test2", serde_json::json!({}))
            .unwrap();
        let provider2 = registry.get_instance("test2").unwrap();
        assert_eq!(provider2.name(), "mock");
    }

    #[test]
    fn test_multiple_provider_types() {
        // Test registry with multiple provider types
        let registry = ProviderRegistry::new();

        // Register multiple factories
        registry.register_factory(
            "mock1",
            Box::new(|_instance_id, _config| Ok(Arc::new(MockProvider {}))),
        );
        registry.register_factory(
            "mock2",
            Box::new(|_instance_id, _config| Ok(Arc::new(MockProvider {}))),
        );

        // Create instances of different types
        registry
            .create_instance("mock1", "instance1", serde_json::json!({}))
            .unwrap();
        registry
            .create_instance("mock2", "instance2", serde_json::json!({}))
            .unwrap();

        // Verify both exist
        assert!(registry.get_instance("instance1").is_some());
        assert!(registry.get_instance("instance2").is_some());

        // Verify list contains both
        let instances = registry.list_instances();
        assert_eq!(instances.len(), 2);
    }

    #[test]
    fn test_instance_id_uniqueness() {
        // Test that instance_id must be unique
        let registry = ProviderRegistry::new();
        registry.register_factory(
            "mock",
            Box::new(|_instance_id, _config| Ok(Arc::new(MockProvider {}))),
        );

        // Create first instance
        registry
            .create_instance("mock", "duplicate", serde_json::json!({}))
            .unwrap();

        // Create second instance with same ID (should replace in DashMap)
        registry
            .create_instance("mock", "duplicate", serde_json::json!({}))
            .unwrap();

        // Only one instance should exist
        let instances = registry.list_instances();
        assert_eq!(instances.len(), 1);
        assert!(instances.contains(&"duplicate".to_string()));
    }

    #[test]
    fn test_default_registry() {
        // Test that Default trait works
        let registry = ProviderRegistry::default();

        // Should be empty
        assert!(registry.list_instances().is_empty());

        // Should be usable
        registry.register_factory(
            "mock",
            Box::new(|_instance_id, _config| Ok(Arc::new(MockProvider {}))),
        );
        registry
            .create_instance("mock", "test", serde_json::json!({}))
            .unwrap();

        assert!(registry.get_instance("test").is_some());
    }

    #[test]
    fn test_factory_config_parsing() {
        // Test that factory receives and can parse config
        let registry = ProviderRegistry::new();

        // Register factory that validates config
        registry.register_factory(
            "config_test",
            Box::new(|_instance_id, config| {
                // Verify config contains expected field
                let value = config
                    .get("test_field")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ProviderError::InvalidConfig("test_field missing".to_string())
                    })?;

                assert_eq!(value, "test_value");
                Ok(Arc::new(MockProvider {}))
            }),
        );

        // Valid config
        let result = registry.create_instance(
            "config_test",
            "test",
            serde_json::json!({"test_field": "test_value"}),
        );
        assert!(result.is_ok());

        // Invalid config (missing field)
        let result = registry.create_instance(
            "config_test",
            "test2",
            serde_json::json!({"other_field": "value"}),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_concurrent_factory_registration() {
        // Test that concurrent factory registration is safe
        use std::thread;

        let registry = std::sync::Arc::new(ProviderRegistry::new());
        let mut handles = vec![];

        // Spawn multiple threads registering factories
        for i in 0..10 {
            let reg = registry.clone();
            handles.push(thread::spawn(move || {
                reg.register_factory(
                    &format!("factory_{}", i),
                    Box::new(|_instance_id, _config| Ok(Arc::new(MockProvider {}))),
                );
            }));
        }

        // All should complete without panic
        for handle in handles {
            handle.join().unwrap();
        }

        // Create instances with each factory
        for i in 0..10 {
            let factory_name = format!("factory_{}", i);
            registry
                .create_instance(&factory_name, &format!("inst_{}", i), serde_json::json!({}))
                .unwrap();
        }

        // Verify all instances exist
        let instances = registry.list_instances();
        assert_eq!(instances.len(), 10);
    }

    #[test]
    fn test_get_nonexistent_instance() {
        // Test getting nonexistent instance returns None
        let registry = ProviderRegistry::new();

        let result = registry.get_instance("nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn test_remove_and_recreate() {
        // Test removing and recreating an instance
        let registry = ProviderRegistry::new();
        registry.register_factory(
            "mock",
            Box::new(|_instance_id, _config| Ok(Arc::new(MockProvider {}))),
        );

        // Create instance
        registry
            .create_instance("mock", "recreate_test", serde_json::json!({}))
            .unwrap();
        assert!(registry.get_instance("recreate_test").is_some());

        // Remove it
        assert!(registry.remove_instance("recreate_test"));
        assert!(registry.get_instance("recreate_test").is_none());

        // Recreate with same ID
        registry
            .create_instance("mock", "recreate_test", serde_json::json!({}))
            .unwrap();
        assert!(registry.get_instance("recreate_test").is_some());
    }

    #[tokio::test]
    async fn test_provider_trait_send_sync() {
        // Test that provider instances can be sent across threads
        use std::thread;

        let registry = std::sync::Arc::new(ProviderRegistry::new());
        registry.register_factory(
            "mock",
            Box::new(|_instance_id, _config| Ok(Arc::new(MockProvider {}))),
        );

        registry
            .create_instance("mock", "thread_test", serde_json::json!({}))
            .unwrap();

        // Get instance in main thread
        let provider = registry.get_instance("thread_test").unwrap();

        // Spawn thread and use provider there
        let handle = thread::spawn(move || {
            // Provider should be usable in different thread
            assert_eq!(provider.name(), "mock");
        });

        handle.join().unwrap();
    }
}
