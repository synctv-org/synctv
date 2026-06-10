//! Room settings cache (L1: Moka in-memory, L2: Redis)
//!
//! Room settings carry an optimistic-lock version. Strong reads compare that
//! version with the authoritative version fence before trusting either cache tier.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::cache::l2_backend::CacheL2Backend;
use crate::cache::tiered::{FenceReadResult, TieredCache, Versioned};
use crate::models::{RoomId, RoomSettings};
use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomSettingsSnapshot {
    pub settings: RoomSettings,
    pub version: i64,
}

impl Versioned for RoomSettingsSnapshot {
    fn cache_version(&self) -> i64 {
        self.version
    }
}

#[derive(Clone)]
pub struct RoomSettingsCache {
    inner: TieredCache<RoomId, RoomSettingsSnapshot>,
}

impl RoomSettingsCache {
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
            "room_settings".to_string(),
        );
        Self { inner }
    }

    pub async fn get(&self, room_id: &RoomId) -> Result<Option<RoomSettingsSnapshot>> {
        self.inner.get(room_id).await
    }

    pub async fn get_l1(&self, room_id: &RoomId) -> Option<RoomSettingsSnapshot> {
        self.inner.get_l1(room_id).await
    }

    pub async fn get_l2(&self, room_id: &RoomId) -> Result<Option<RoomSettingsSnapshot>> {
        self.inner.get_l2(room_id).await
    }

    pub async fn get_by_fence_key(
        &self,
        room_id: &RoomId,
        fence_key: &str,
    ) -> Result<FenceReadResult<RoomSettingsSnapshot>> {
        self.inner.get_by_fence_key(room_id, fence_key).await
    }

    pub async fn set(&self, room_id: &RoomId, snapshot: RoomSettingsSnapshot) -> Result<()> {
        self.inner.set(room_id, snapshot).await
    }

    pub async fn set_if_version_at_least(
        &self,
        room_id: &RoomId,
        snapshot: RoomSettingsSnapshot,
    ) -> Result<bool> {
        self.inner.set_if_version_at_least(room_id, snapshot).await
    }

    pub async fn invalidate(&self, room_id: &RoomId) -> Result<()> {
        self.inner.invalidate(room_id).await
    }

    pub async fn invalidate_by_id(&self, room_id: &str) -> Result<()> {
        self.inner.invalidate_by_id(room_id).await
    }

    pub fn clear_l1(&self) {
        self.inner.clear_l1();
    }

    #[must_use]
    pub fn entry_count(&self) -> u64 {
        self.inner.entry_count()
    }

    #[must_use]
    pub fn weighted_size(&self) -> u64 {
        self.inner.weighted_size()
    }

    pub async fn clear(&self) {
        self.inner.clear().await;
    }
}

impl std::fmt::Debug for RoomSettingsCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomSettingsCache")
            .field("inner", &self.inner)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheL2Backend;
    use crate::test_helpers::TestResultExt;
    use async_trait::async_trait;
    use parking_lot::Mutex;

    #[derive(Default)]
    struct RecordingL2 {
        set_if_version_keys: Mutex<Vec<String>>,
        delete_prefixes: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl CacheL2Backend for RecordingL2 {
        async fn get(&self, _key: &str) -> Result<Option<String>> {
            Ok(None)
        }

        async fn set(&self, _key: &str, _json: &str, _ttl_secs: u64) -> Result<()> {
            Ok(())
        }

        async fn delete(&self, _key: &str) -> Result<()> {
            Ok(())
        }

        async fn get_batch(&self, keys: &[String]) -> Result<Vec<Option<String>>> {
            Ok(vec![None; keys.len()])
        }

        async fn set_if_newer(
            &self,
            _key: &str,
            _json: &str,
            _ttl_secs: u64,
            _new_ts_millis: i64,
        ) -> Result<bool> {
            Ok(true)
        }

        async fn set_if_version_at_least(
            &self,
            key: &str,
            _json: &str,
            _ttl_secs: u64,
            _version: i64,
        ) -> Result<bool> {
            self.set_if_version_keys.lock().push(key.to_string());
            Ok(true)
        }

        async fn delete_by_prefix(&self, prefix: &str) -> Result<()> {
            self.delete_prefixes.lock().push(prefix.to_string());
            Ok(())
        }

        fn is_active(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn test_room_settings_l2_key_uses_configured_prefix() {
        let l2 = Arc::new(RecordingL2::default());
        let cache = RoomSettingsCache::new(
            l2.clone(),
            100,
            60,
            60,
            "tenant-a:room_settings:".to_string(),
        );
        let room_id = RoomId::expect_positive(42);

        cache
            .set_if_version_at_least(
                &room_id,
                RoomSettingsSnapshot {
                    settings: RoomSettings::default(),
                    version: 3,
                },
            )
            .await
            .checked("cache write should succeed");

        let keys = l2.set_if_version_keys.lock();
        assert_eq!(&*keys, &["tenant-a:room_settings:42".to_string()]);
    }

    #[tokio::test]
    async fn test_clear_deletes_l2_by_configured_prefix() {
        let l2 = Arc::new(RecordingL2::default());
        let cache = RoomSettingsCache::new(
            l2.clone(),
            100,
            60,
            60,
            "tenant-a:room_settings:".to_string(),
        );

        cache.clear().await;

        let prefixes = l2.delete_prefixes.lock();
        assert_eq!(&*prefixes, &["tenant-a:room_settings:".to_string()]);
    }
}
