//! Runtime settings cache (L1: Moka in-memory, L2: Redis).
//!
//! Runtime settings carry an optimistic-lock version. Strong reads compare the
//! cached setting version with the authoritative runtime-setting version fence
//! before trusting either cache tier.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::cache::l2_backend::CacheL2Backend;
use crate::cache::tiered::{CacheKey, FenceReadResult, TieredCache, Versioned};
use crate::models::settings::SettingsGroup;
use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuntimeSettingKey(String);

impl RuntimeSettingKey {
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RuntimeSettingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl CacheKey for RuntimeSettingKey {
    fn cache_key(&self) -> String {
        self.0.clone()
    }

    fn try_from_id(id: &str) -> Result<Self> {
        if id.is_empty() || id.contains(':') {
            return Err(crate::Error::InvalidInput(format!(
                "Invalid runtime setting cache key: {id}"
            )));
        }
        Ok(Self::new(id))
    }
}

impl Versioned for SettingsGroup {
    fn cache_version(&self) -> i64 {
        i64::from(self.version)
    }
}

#[derive(Clone)]
pub struct RuntimeSettingsCache {
    inner: TieredCache<RuntimeSettingKey, SettingsGroup>,
}

impl RuntimeSettingsCache {
    pub fn new(
        l2: Arc<dyn CacheL2Backend>,
        l1_max_capacity: u64,
        l1_ttl_seconds: u64,
        l2_ttl_seconds: u64,
        key_prefix: String,
    ) -> Self {
        let inner = TieredCache::new(
            l2,
            l1_max_capacity,
            l1_ttl_seconds,
            l2_ttl_seconds,
            key_prefix,
            "runtime_setting".to_string(),
        );
        Self { inner }
    }

    pub async fn get_l1(&self, key: &RuntimeSettingKey) -> Option<SettingsGroup> {
        self.inner.get_l1(key).await
    }

    pub async fn get_l2(&self, key: &RuntimeSettingKey) -> Result<Option<SettingsGroup>> {
        self.inner.get_l2(key).await
    }

    pub async fn get_by_fence_key(
        &self,
        key: &RuntimeSettingKey,
        fence_key: &str,
    ) -> Result<FenceReadResult<SettingsGroup>> {
        self.inner.get_by_fence_key(key, fence_key).await
    }

    pub async fn set_if_version_at_least(
        &self,
        key: &RuntimeSettingKey,
        setting: SettingsGroup,
    ) -> Result<bool> {
        self.inner.set_if_version_at_least(key, setting).await
    }

    pub async fn invalidate(&self, key: &RuntimeSettingKey) -> Result<()> {
        self.inner.invalidate(key).await
    }

    pub async fn clear(&self) {
        self.inner.clear().await;
    }
}

impl fmt::Debug for RuntimeSettingsCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeSettingsCache")
            .field("inner", &self.inner)
            .finish()
    }
}
