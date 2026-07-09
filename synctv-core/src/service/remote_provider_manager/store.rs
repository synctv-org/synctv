use std::sync::Arc;

use crate::{
    models::{ProviderInstance, ProviderInstanceListQuery},
    repository::ProviderInstanceRepository,
};

use super::RemoteProviderManager;

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

#[derive(Debug)]
pub(crate) struct EmptyProviderInstanceStore;

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

pub(crate) fn empty_provider_instance_store() -> Arc<dyn ProviderInstanceStore> {
    Arc::new(EmptyProviderInstanceStore)
}

pub fn empty_provider_instance_manager() -> Arc<RemoteProviderManager> {
    Arc::new(RemoteProviderManager::new_with_store(
        empty_provider_instance_store(),
        None,
    ))
}
