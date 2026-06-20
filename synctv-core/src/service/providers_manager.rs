//! Providers Manager
//!
//! Manages all `MediaProvider` instances with singleton pattern.
//! Built-in local providers are created once at startup from explicit local
//! provider configuration.

use crate::config::{LocalProviderHttpConfig, MediaProvidersConfig};
use crate::models::normalize_provider_instance_name;
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
    ssrf_guard: &synctv_common::ssrf::SsrfGuard,
) -> std::result::Result<Option<reqwest::Client>, crate::Error> {
    fn parse_timeout_seconds(config: &Value, key: &str) -> Result<Option<u64>> {
        config
            .get(key)
            .map(|value| {
                value.as_u64().ok_or_else(|| {
                    crate::Error::InvalidInput(format!(
                        "{key} must be a positive integer number of seconds"
                    ))
                })
            })
            .transpose()
    }

    let request_timeout_seconds = parse_timeout_seconds(config, "request_timeout_seconds")?;
    let connect_timeout_seconds = parse_timeout_seconds(config, "connect_timeout_seconds")?;

    if request_timeout_seconds.is_none() && connect_timeout_seconds.is_none() {
        return Ok(None);
    }

    let defaults = LocalProviderHttpConfig::default();
    let request_timeout_seconds =
        request_timeout_seconds.unwrap_or(defaults.request_timeout_seconds);
    let connect_timeout_seconds =
        connect_timeout_seconds.unwrap_or(defaults.connect_timeout_seconds);

    if request_timeout_seconds == 0 {
        return Err(crate::Error::InvalidInput(
            "request_timeout_seconds must be greater than 0".to_string(),
        ));
    }
    if request_timeout_seconds > 300 {
        return Err(crate::Error::InvalidInput(
            "request_timeout_seconds should not exceed 300 seconds (5 minutes)".to_string(),
        ));
    }
    if connect_timeout_seconds == 0 {
        return Err(crate::Error::InvalidInput(
            "connect_timeout_seconds must be greater than 0".to_string(),
        ));
    }
    if connect_timeout_seconds > request_timeout_seconds {
        return Err(crate::Error::InvalidInput(
            "connect_timeout_seconds should not exceed request_timeout_seconds".to_string(),
        ));
    }

    let client = synctv_media_providers::provider_http_client_builder(ssrf_guard.clone())
        .request_timeout(std::time::Duration::from_secs(request_timeout_seconds))
        .read_timeout(std::time::Duration::from_secs(request_timeout_seconds))
        .connect_timeout(std::time::Duration::from_secs(connect_timeout_seconds))
        .build()
        .map_err(|e| {
            crate::Error::Internal(format!("Failed to build provider HTTP client: {e}"))
        })?;

    Ok(Some(client))
}

/// Factory function type for creating `MediaProvider` instances.
pub type MediaProviderFactory = Box<
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
    factories: HashMap<String, MediaProviderFactory>,

    /// Created `MediaProvider` instances (singleton per provider type)
    instances: Arc<RwLock<HashMap<String, Arc<dyn MediaProvider>>>>,
    /// Provider instance manager (for local/remote dispatch)
    instance_manager: Arc<RemoteProviderManager>,
    /// Default injected local provider clients used by provider instances
    /// when they do not specify a per-instance HTTP transport override.
    default_client_manager: Arc<ProviderClientManager>,
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
}

impl ProvidersManager {
    fn default_instance_id(provider_type: &str) -> String {
        provider_type.to_string()
    }

    /// Create a new `ProvidersManager`
    pub fn new(instance_manager: Arc<RemoteProviderManager>) -> Result<Self> {
        Self::new_with_ssrf_guard(
            instance_manager,
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
    }

    /// Create a new manager with an explicit global SSRF guard.
    pub fn new_with_ssrf_guard(
        instance_manager: Arc<RemoteProviderManager>,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
    ) -> Result<Self> {
        let default_provider_http_client = synctv_media_providers::build_provider_http_client(
            ssrf_guard.clone(),
        )
        .map_err(|error| {
            crate::Error::Internal(format!("Failed to build provider HTTP client: {error}"))
        })?;
        Ok(Self::new_with_provider_http_client_and_ssrf_guard(
            instance_manager,
            default_provider_http_client,
            ssrf_guard,
        ))
    }

    /// Create a new manager with an explicit default local provider HTTP client.
    #[must_use]
    pub fn new_with_provider_http_client(
        instance_manager: Arc<RemoteProviderManager>,
        default_provider_http_client: reqwest::Client,
    ) -> Self {
        Self::new_with_provider_http_client_and_ssrf_guard(
            instance_manager,
            default_provider_http_client,
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
    }

    /// Create a new manager with explicit local provider transport and SSRF guard.
    #[must_use]
    pub fn new_with_provider_http_client_and_ssrf_guard(
        instance_manager: Arc<RemoteProviderManager>,
        default_provider_http_client: reqwest::Client,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
    ) -> Self {
        let default_client_manager = Arc::new(
            ProviderClientManager::new_with_provider_http_client(default_provider_http_client),
        );
        let mut manager = Self {
            factories: HashMap::new(),
            instances: Arc::new(RwLock::new(HashMap::new())),
            instance_manager,
            default_client_manager,
            ssrf_guard,
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
        let ssrf_guard = self.ssrf_guard.clone();

        // Helper to select client manager based on config
        let select_client_manager = |config: &Value, ssrf_guard: &synctv_common::ssrf::SsrfGuard| {
            provider_http_client_from_config(config, ssrf_guard)
                .map(|client_opt| {
                    client_opt.map(|client| {
                        Arc::new(ProviderClientManager::new_with_provider_http_client(client))
                    })
                })
        };

        // Alist factory
        let default_client_manager_alist = Arc::clone(&default_client_manager);
        let ssrf_guard_alist = ssrf_guard.clone();
        self.register_factory(
            AlistProvider::NAME,
            Box::new(move |_instance_id, config, instance_manager| {
                let client_manager = match select_client_manager(config, &ssrf_guard_alist)? {
                    Some(manager) => manager,
                    None => Arc::clone(&default_client_manager_alist),
                };
                Ok(Arc::new(AlistProvider::with_client_manager(
                    instance_manager,
                    client_manager,
                )))
            }),
        );

        // Bilibili factory
        let default_client_manager_bilibili = Arc::clone(&default_client_manager);
        let ssrf_guard_bilibili = ssrf_guard.clone();
        self.register_factory(
            BilibiliProvider::NAME,
            Box::new(move |_instance_id, config, instance_manager| {
                let client_manager = match select_client_manager(config, &ssrf_guard_bilibili)? {
                    Some(manager) => manager,
                    None => Arc::clone(&default_client_manager_bilibili),
                };
                Ok(Arc::new(BilibiliProvider::with_client_manager(
                    instance_manager,
                    client_manager,
                )))
            }),
        );

        // Emby factory
        let default_client_manager_emby = Arc::clone(&default_client_manager);
        let ssrf_guard_emby = ssrf_guard.clone();
        self.register_factory(
            EmbyProvider::NAME,
            Box::new(move |_instance_id, config, instance_manager| {
                let client_manager = match select_client_manager(config, &ssrf_guard_emby)? {
                    Some(manager) => manager,
                    None => Arc::clone(&default_client_manager_emby),
                };
                Ok(Arc::new(EmbyProvider::with_client_manager(
                    instance_manager,
                    client_manager,
                )))
            }),
        );

        // RTMP factory
        self.register_factory(
            RtmpProvider::NAME,
            Box::new(|_instance_id, _config, _instance_manager| Ok(Arc::new(RtmpProvider::new()))),
        );

        // DirectUrl factory
        let ssrf_guard = self.ssrf_guard.clone();
        self.register_factory(
            DirectUrlProvider::NAME,
            Box::new(move |_instance_id, _config, _instance_manager| {
                Ok(Arc::new(DirectUrlProvider::new_with_ssrf_guard(
                    ssrf_guard.clone(),
                )))
            }),
        );

        // LiveProxy factory
        let ssrf_guard = self.ssrf_guard.clone();
        self.register_factory(
            LiveProxyProvider::NAME,
            Box::new(move |_instance_id, _config, _instance_manager| {
                Ok(Arc::new(LiveProxyProvider::new_with_ssrf_guard(
                    ssrf_guard.clone(),
                )))
            }),
        );
    }

    /// Register a provider factory
    pub fn register_factory(&mut self, provider_type: &str, factory: MediaProviderFactory) {
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
                AlistProvider::NAME => serde_json::to_value(&config.alist),
                BilibiliProvider::NAME => serde_json::to_value(&config.bilibili),
                EmbyProvider::NAME => serde_json::to_value(&config.emby),
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

        let trimmed_instance = normalize_provider_instance_name(provider_instance_name);

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
    use crate::test_helpers::{TestOptionExt, TestResultExt};

    fn test_instance_manager() -> Arc<RemoteProviderManager> {
        crate::service::remote_provider_manager::empty_provider_instance_manager()
    }

    fn test_manager() -> ProvidersManager {
        ProvidersManager::new(test_instance_manager()).checked("providers manager should build")
    }

    #[tokio::test]
    async fn test_providers_manager_creation() {
        let manager = test_manager();

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
        let manager = test_manager();

        let types = manager.list_types();
        assert!(types.contains(&"alist".to_string()));
        assert!(types.contains(&"bilibili".to_string()));
        assert!(types.contains(&"live_proxy".to_string()));
        assert_eq!(types.len(), 6);
    }

    #[tokio::test]
    async fn test_provider_config_without_timeout() {
        let manager = test_manager();

        let config = serde_json::json!({});
        let provider = manager
            .create_provider("alist", "test_alist", &config)
            .await;
        assert!(provider.is_ok());

        let stored = manager.get("test_alist").await;
        assert!(stored.is_some());
    }

    #[tokio::test]
    async fn test_provider_config_with_timeout() {
        let manager = test_manager();

        let config = serde_json::json!({
            "request_timeout_seconds": 30,
            "connect_timeout_seconds": 10
        });
        let provider = manager
            .create_provider("alist", "test_alist_timeout", &config)
            .await;
        assert!(provider.is_ok());

        let stored = manager.get("test_alist_timeout").await;
        assert!(stored.is_some());
    }

    #[tokio::test]
    async fn test_bilibili_provider_config_with_timeout() {
        let manager = test_manager();

        let config = serde_json::json!({
            "request_timeout_seconds": 45,
            "connect_timeout_seconds": 10
        });
        let provider = manager
            .create_provider("bilibili", "test_bilibili_timeout", &config)
            .await;
        assert!(provider.is_ok());

        let stored = manager.get("test_bilibili_timeout").await;
        assert!(stored.is_some());
    }

    #[tokio::test]
    async fn test_emby_provider_config_with_timeout() {
        let manager = test_manager();

        let config = serde_json::json!({
            "request_timeout_seconds": 60,
            "connect_timeout_seconds": 10
        });
        let provider = manager
            .create_provider("emby", "test_emby_timeout", &config)
            .await;
        assert!(provider.is_ok());

        let stored = manager.get("test_emby_timeout").await;
        assert!(stored.is_some());
    }

    #[tokio::test]
    async fn test_provider_config_invalid_timeout_rejected() {
        let manager = test_manager();

        for (config, expected_message) in [
            (
                serde_json::json!({"request_timeout_seconds": "invalid"}),
                "request_timeout_seconds must be a positive integer number of seconds",
            ),
            (
                serde_json::json!({"connect_timeout_seconds": "invalid"}),
                "connect_timeout_seconds must be a positive integer number of seconds",
            ),
            (
                serde_json::json!({"request_timeout_seconds": 0}),
                "request_timeout_seconds must be greater than 0",
            ),
            (
                serde_json::json!({"request_timeout_seconds": 301}),
                "request_timeout_seconds should not exceed 300 seconds",
            ),
            (
                serde_json::json!({
                    "request_timeout_seconds": 10,
                    "connect_timeout_seconds": 11,
                }),
                "connect_timeout_seconds should not exceed request_timeout_seconds",
            ),
        ] {
            let Err(error) = manager
                .create_provider("alist", "test_alist_invalid", &config)
                .await
            else {
                std::panic::panic_any("invalid provider timeout config should fail fast");
            };
            assert!(
                error.to_string().contains(expected_message),
                "unexpected error for {config}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn test_new_with_provider_http_client_accepts_explicit_default_client() {
        let client = synctv_media_providers::provider_http_client_builder(
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .connect_timeout(std::time::Duration::from_secs(4))
        .request_timeout(std::time::Duration::from_secs(12))
        .build()
        .checked("provider HTTP client should build");

        let manager =
            ProvidersManager::new_with_provider_http_client(test_instance_manager(), client);

        assert!(manager.has_factory("alist"));
        assert!(manager.has_factory("bilibili"));
        assert!(manager.has_factory("emby"));
    }

    #[tokio::test]
    async fn test_default_provider_instances_use_injected_default_client_manager() {
        let instance_manager = test_instance_manager();
        let client = synctv_media_providers::provider_http_client_builder(
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .connect_timeout(std::time::Duration::from_secs(4))
        .request_timeout(std::time::Duration::from_secs(12))
        .build()
        .checked("provider HTTP client should build");

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
                .checked("default provider should be created");

            assert_eq!(
                provider.test_client_manager_marker(),
                Some(expected_marker),
                "default {provider_type} provider should reuse the injected default client manager",
            );
        }
    }

    #[tokio::test]
    async fn test_per_instance_timeout_override_keeps_dedicated_client_manager() {
        let instance_manager = test_instance_manager();
        let client = synctv_media_providers::provider_http_client_builder(
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .connect_timeout(std::time::Duration::from_secs(4))
        .request_timeout(std::time::Duration::from_secs(12))
        .build()
        .checked("provider HTTP client should build");

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
            .checked("provider with timeout override should be created");

        let actual_marker = provider
            .test_client_manager_marker()
            .checked("test provider should expose its client manager marker");
        assert_ne!(
            actual_marker, default_marker,
            "per-instance timeout overrides should build a dedicated client manager"
        );
    }

    #[tokio::test]
    async fn test_rtmp_provider_no_longer_requires_base_url() {
        let manager = test_manager();

        let config = serde_json::json!({});
        let provider = manager.create_provider("rtmp", "test_rtmp", &config).await;
        assert!(provider.is_ok());
    }

    #[tokio::test]
    async fn test_live_proxy_provider_no_longer_requires_base_url() {
        let manager = test_manager();

        let config = serde_json::json!({});
        let provider = manager
            .create_provider("live_proxy", "test_live_proxy", &config)
            .await;
        assert!(provider.is_ok());
    }

    #[tokio::test]
    async fn test_provider_lookup_by_instance_id() {
        let manager = test_manager();

        let config = serde_json::json!({});
        manager
            .create_provider("alist", "my_alist_instance", &config)
            .await
            .checked("provider should be created");

        let provider = manager.get("my_alist_instance").await;
        assert!(provider.is_some());
        assert_eq!(
            provider.checked("provider should be stored").name(),
            "alist"
        );

        let not_found = manager.get("nonexistent").await;
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_provider_lookup_by_type() {
        let manager = test_manager();

        let config = serde_json::json!({});
        manager
            .create_provider("alist", "alist", &config)
            .await
            .checked("provider should be created");

        let provider = manager.get_by_type("alist").await;
        assert!(provider.is_some());
        assert_eq!(
            provider.checked("provider should be stored").name(),
            "alist"
        );

        let not_found = manager.get_by_type("unknown").await;
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_resolve_provider_uses_default_for_missing_or_empty_instance() {
        let manager = test_manager();

        manager
            .create_provider("alist", "alist", &serde_json::json!({}))
            .await
            .checked("default provider should be created");

        let implicit_default = manager
            .resolve_provider("alist", None)
            .await
            .checked("implicit default provider should resolve");
        let empty_default = manager
            .resolve_provider("alist", Some("  "))
            .await
            .checked("empty default provider should resolve");

        assert_eq!(implicit_default.name(), "alist");
        assert_eq!(empty_default.name(), "alist");
        assert!(
            Arc::ptr_eq(&implicit_default, &empty_default),
            "None and empty provider_instance_name must resolve to the same default provider"
        );
    }

    #[tokio::test]
    async fn test_resolve_provider_uses_explicit_local_instance_and_checks_type() {
        let manager = test_manager();

        manager
            .create_provider("alist", "alist_alt", &serde_json::json!({}))
            .await
            .checked("explicit provider should be created");

        let provider = manager
            .resolve_provider("alist", Some(" alist_alt "))
            .await
            .checked("explicit provider should resolve");
        assert_eq!(provider.name(), "alist");

        let error = match manager.resolve_provider("emby", Some("alist_alt")).await {
            Ok(provider) => std::panic::panic_any(format!(
                "explicit local instance must match the requested provider type, got provider '{}'",
                provider.name()
            )),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("request declared 'emby'"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn test_provider_singleton_pattern() {
        let manager = test_manager();

        let config1 = serde_json::json!({"request_timeout_seconds": 10});
        let provider1 = manager
            .create_provider("alist", "alist_singleton", &config1)
            .await
            .checked("first provider should be created");
        assert_eq!(provider1.name(), "alist");

        let config2 = serde_json::json!({"request_timeout_seconds": 30});
        let provider2 = manager
            .create_provider("alist", "alist_singleton", &config2)
            .await
            .checked("second provider should be created");
        assert_eq!(provider2.name(), "alist");

        assert!(!Arc::ptr_eq(&provider1, &provider2));

        let stored = manager
            .get("alist_singleton")
            .await
            .checked("provider singleton should be stored");
        assert!(Arc::ptr_eq(&provider2, &stored));
    }

    #[tokio::test]
    async fn test_provider_list() {
        let manager = test_manager();

        let list = manager.list().await;
        assert!(list.is_empty());

        manager
            .create_provider("alist", "alist1", &serde_json::json!({}))
            .await
            .checked("alist provider should be created");
        manager
            .create_provider("bilibili", "bilibili1", &serde_json::json!({}))
            .await
            .checked("bilibili provider should be created");
        manager
            .create_provider("emby", "emby1", &serde_json::json!({}))
            .await
            .checked("emby provider should be created");

        let list = manager.list().await;
        assert_eq!(list.len(), 3);

        let names: Vec<&str> = list.iter().map(|p| p.name()).collect();
        assert!(names.contains(&"alist"));
        assert!(names.contains(&"bilibili"));
        assert!(names.contains(&"emby"));
    }

    #[tokio::test]
    async fn test_provider_remove() {
        let manager = test_manager();

        manager
            .create_provider("alist", "alist_remove", &serde_json::json!({}))
            .await
            .checked("provider should be created");

        assert!(manager.get("alist_remove").await.is_some());

        let removed = manager.remove("alist_remove").await;
        assert!(removed.is_some());

        assert!(manager.get("alist_remove").await.is_none());

        let not_found = manager.remove("nonexistent").await;
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_provider_factory_unknown_type() {
        let manager = test_manager();

        let config = serde_json::json!({});
        let result = manager
            .create_provider("unknown_type", "test", &config)
            .await;

        assert!(result.is_err());
        match result {
            Err(crate::Error::NotFound(msg)) => {
                assert!(msg.contains("Unknown provider type"));
            }
            _ => std::panic::panic_any("Expected NotFound error"),
        }
    }

    #[tokio::test]
    async fn test_concurrent_provider_creation() {
        let instance_manager = test_instance_manager();
        let manager = Arc::new(
            ProvidersManager::new(instance_manager).checked("providers manager should build"),
        );

        let mut handles = vec![];

        for i in 0..5 {
            let mgr = manager.clone();
            handles.push(tokio::spawn(async move {
                let instance_id = format!("alist_concurrent_{i}");
                mgr.create_provider("alist", &instance_id, &serde_json::json!({}))
                    .await
            }));
        }

        let results: Vec<_> = futures::future::join_all(handles).await;
        for result in results {
            assert!(result.checked("provider creation task should join").is_ok());
        }

        for i in 0..5 {
            let instance_id = format!("alist_concurrent_{i}");
            assert!(manager.get(&instance_id).await.is_some());
        }
    }

    #[tokio::test]
    async fn test_instance_manager_reference() {
        let instance_manager = test_instance_manager();
        let manager = ProvidersManager::new(instance_manager.clone())
            .checked("providers manager should build");

        let retrieved = manager.instance_manager();
        assert!(Arc::ptr_eq(&instance_manager, retrieved));
    }
}
