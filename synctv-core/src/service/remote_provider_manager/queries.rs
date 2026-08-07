use super::RemoteProviderManager;
use crate::models::{ProviderInstance, ProviderInstanceListQuery};

impl RemoteProviderManager {
    fn provider_registry_unavailable(
        operation: &'static str,
        error: impl std::fmt::Display,
    ) -> crate::Error {
        tracing::warn!(operation, error = %error, "Provider registry unavailable");
        crate::Error::ServiceUnavailable(
            "Provider configuration service is temporarily unavailable.".to_string(),
        )
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
}
