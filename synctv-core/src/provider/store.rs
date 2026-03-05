// Provider Store - key-value storage abstraction for provider caching and locking

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use moka::Expiry;
use parking_lot::Mutex;
use thiserror::Error;

/// Errors returned by provider store operations.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("backend error: {0}")]
    Backend(String),
    #[error("lock acquisition failed: {0}")]
    LockFailed(String),
}

/// RAII guard that runs a cleanup function on drop.
pub struct StoreLockGuard {
    release: Option<Box<dyn FnOnce() + Send>>,
}

impl StoreLockGuard {
    pub fn new(release: impl FnOnce() + Send + 'static) -> Self {
        Self {
            release: Some(Box::new(release)),
        }
    }

    pub fn noop() -> Self {
        Self { release: None }
    }
}

impl Drop for StoreLockGuard {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

/// Key-value store trait for provider caching and distributed locking.
#[async_trait::async_trait]
pub trait ProviderStore: Send + Sync {
    async fn get_raw(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError>;
    async fn set_raw(&self, key: &str, value: &[u8], ttl: Duration) -> Result<(), StoreError>;
    async fn delete(&self, key: &str) -> Result<(), StoreError>;
    async fn lock(&self, key: &str, ttl: Duration) -> Result<StoreLockGuard, StoreError>;
}

/// Extension trait providing typed (serde) convenience methods on top of `ProviderStore`.
#[async_trait::async_trait]
pub trait ProviderStoreExt: ProviderStore {
    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, StoreError> {
        match self.get_raw(key).await? {
            Some(bytes) => {
                let value = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Serialization(e.to_string()))?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    async fn set<T: serde::Serialize + Send + Sync>(
        &self,
        key: &str,
        value: &T,
        ttl: Duration,
    ) -> Result<(), StoreError> {
        let bytes =
            serde_json::to_vec(value).map_err(|e| StoreError::Serialization(e.to_string()))?;
        self.set_raw(key, &bytes, ttl).await
    }
}

impl<S: ProviderStore + ?Sized> ProviderStoreExt for S {}

// ---------------------------------------------------------------------------
// InMemoryProviderStore
// ---------------------------------------------------------------------------

/// Value wrapper that carries a per-entry TTL for moka's `Expiry` trait.
#[derive(Clone)]
struct TtlValue {
    data: Vec<u8>,
    ttl: Duration,
}

/// Moka `Expiry` implementation that uses the per-entry TTL stored in `TtlValue`.
struct PerEntryExpiry;

impl Expiry<String, TtlValue> for PerEntryExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &TtlValue,
        _current_time: std::time::Instant,
    ) -> Option<Duration> {
        Some(value.ttl)
    }
}

/// In-memory provider store backed by `moka::future::Cache` with per-entry TTL support.
pub struct InMemoryProviderStore {
    cache: moka::future::Cache<String, TtlValue>,
    locks: Arc<Mutex<HashSet<String>>>,
}

impl InMemoryProviderStore {
    pub fn new(max_capacity: u64) -> Self {
        Self {
            cache: moka::future::Cache::builder()
                .max_capacity(max_capacity)
                .expire_after(PerEntryExpiry)
                .build(),
            locks: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

#[async_trait::async_trait]
impl ProviderStore for InMemoryProviderStore {
    async fn get_raw(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.cache.get(key).await.map(|v| v.data))
    }

    async fn set_raw(&self, key: &str, value: &[u8], ttl: Duration) -> Result<(), StoreError> {
        self.cache
            .insert(
                key.to_string(),
                TtlValue {
                    data: value.to_vec(),
                    ttl,
                },
            )
            .await;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), StoreError> {
        self.cache.remove(key).await;
        Ok(())
    }

    async fn lock(&self, key: &str, _ttl: Duration) -> Result<StoreLockGuard, StoreError> {
        {
            let mut set = self.locks.lock();
            if !set.insert(key.to_string()) {
                return Err(StoreError::LockFailed(format!("key already locked: {key}")));
            }
        }
        let locks = Arc::clone(&self.locks);
        let key_owned = key.to_string();
        Ok(StoreLockGuard::new(move || {
            locks.lock().remove(&key_owned);
        }))
    }
}

// ---------------------------------------------------------------------------
// RedisProviderStore
// ---------------------------------------------------------------------------

/// Redis-backed provider store with distributed locking via `SET NX EX`.
pub struct RedisProviderStore {
    conn: redis::aio::ConnectionManager,
}

impl RedisProviderStore {
    pub fn new(conn: redis::aio::ConnectionManager) -> Self {
        Self { conn }
    }
}

#[async_trait::async_trait]
impl ProviderStore for RedisProviderStore {
    async fn get_raw(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let result: Option<Vec<u8>> = redis::cmd("GET")
            .arg(key)
            .query_async(&mut self.conn.clone())
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(result)
    }

    async fn set_raw(&self, key: &str, value: &[u8], ttl: Duration) -> Result<(), StoreError> {
        redis::cmd("SET")
            .arg(key)
            .arg(value)
            .arg("EX")
            .arg(ttl.as_secs().max(1))
            .query_async::<()>(&mut self.conn.clone())
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), StoreError> {
        redis::cmd("DEL")
            .arg(key)
            .query_async::<()>(&mut self.conn.clone())
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn lock(&self, key: &str, ttl: Duration) -> Result<StoreLockGuard, StoreError> {
        let ttl_secs = ttl.as_secs().max(1);
        for _ in 0..10 {
            // SET key 1 NX EX ttl returns OK (Some("OK")) on success, nil (None) if key exists
            let result: Option<String> = redis::cmd("SET")
                .arg(key)
                .arg(1)
                .arg("NX")
                .arg("EX")
                .arg(ttl_secs)
                .query_async(&mut self.conn.clone())
                .await
                .map_err(|e| StoreError::Backend(e.to_string()))?;

            if result.is_some() {
                let key_owned = key.to_string();
                let mut conn_clone = self.conn.clone();
                return Ok(StoreLockGuard::new(move || {
                    tokio::spawn(async move {
                        let _: Result<(), _> = redis::cmd("DEL")
                            .arg(&key_owned)
                            .query_async(&mut conn_clone)
                            .await;
                    });
                }));
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        Err(StoreError::LockFailed(format!(
            "failed to acquire lock for key: {key}"
        )))
    }
}

// ---------------------------------------------------------------------------
// PrefixedProviderStore
// ---------------------------------------------------------------------------

/// Wraps another `ProviderStore` and prepends a prefix to all keys.
pub struct PrefixedProviderStore<S> {
    inner: S,
    prefix: String,
}

impl<S> PrefixedProviderStore<S> {
    pub fn new(inner: S, prefix: String) -> Self {
        Self { inner, prefix }
    }

    fn prefixed_key(&self, key: &str) -> String {
        format!("{}:{}", self.prefix, key)
    }
}

#[async_trait::async_trait]
impl<S: ProviderStore> ProviderStore for PrefixedProviderStore<S> {
    async fn get_raw(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get_raw(&self.prefixed_key(key)).await
    }

    async fn set_raw(&self, key: &str, value: &[u8], ttl: Duration) -> Result<(), StoreError> {
        self.inner
            .set_raw(&self.prefixed_key(key), value, ttl)
            .await
    }

    async fn delete(&self, key: &str) -> Result<(), StoreError> {
        self.inner.delete(&self.prefixed_key(key)).await
    }

    async fn lock(&self, key: &str, ttl: Duration) -> Result<StoreLockGuard, StoreError> {
        self.inner.lock(&self.prefixed_key(key), ttl).await
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Create per-provider stores, using Redis when available, falling back to in-memory.
pub fn create_provider_stores(
    redis: Option<&redis::aio::ConnectionManager>,
) -> HashMap<&'static str, Arc<dyn ProviderStore>> {
    let providers = [
        "bilibili",
        "alist",
        "emby",
        "direct_url",
        "rtmp",
        "live_proxy",
    ];
    providers
        .iter()
        .map(|name| {
            let prefix = format!("synctv:provider:{name}");
            let store: Arc<dyn ProviderStore> = match redis {
                Some(conn) => Arc::new(PrefixedProviderStore::new(
                    RedisProviderStore::new(conn.clone()),
                    prefix,
                )),
                None => Arc::new(PrefixedProviderStore::new(
                    InMemoryProviderStore::new(10_000),
                    prefix,
                )),
            };
            (*name, store)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// VersionedPlayback
// ---------------------------------------------------------------------------

/// Cached playback result with a version tag and expiry timestamp.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VersionedPlayback {
    pub version: String,
    pub result: super::PlaybackResult,
    pub expires_at: i64,
}

impl VersionedPlayback {
    pub fn is_expired(&self) -> bool {
        chrono::Utc::now().timestamp() > self.expires_at
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_in_memory_store_get_set() {
        let store = InMemoryProviderStore::new(100);
        assert!(store.get_raw("key1").await.unwrap().is_none());
        store
            .set_raw("key1", b"hello", Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(store.get_raw("key1").await.unwrap().unwrap(), b"hello");
    }

    #[tokio::test]
    async fn test_in_memory_store_delete() {
        let store = InMemoryProviderStore::new(100);
        store
            .set_raw("key1", b"hello", Duration::from_secs(60))
            .await
            .unwrap();
        store.delete("key1").await.unwrap();
        assert!(store.get_raw("key1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_in_memory_store_lock() {
        let store = InMemoryProviderStore::new(100);
        let guard = store.lock("mylock", Duration::from_secs(10)).await.unwrap();
        // Second lock should fail
        assert!(store.lock("mylock", Duration::from_secs(10)).await.is_err());
        drop(guard);
        // After drop, should succeed
        assert!(store.lock("mylock", Duration::from_secs(10)).await.is_ok());
    }

    #[tokio::test]
    async fn test_prefixed_store() {
        let inner = InMemoryProviderStore::new(100);
        let store = PrefixedProviderStore::new(inner, "test:prefix".to_string());
        store
            .set_raw("key1", b"value", Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(store.get_raw("key1").await.unwrap().unwrap(), b"value");
    }

    #[tokio::test]
    async fn test_typed_get_set() {
        let store = InMemoryProviderStore::new(100);
        #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
        struct TestData {
            name: String,
            count: u32,
        }
        let data = TestData {
            name: "test".to_string(),
            count: 42,
        };
        store
            .set("typed_key", &data, Duration::from_secs(60))
            .await
            .unwrap();
        let retrieved: Option<TestData> = store.get("typed_key").await.unwrap();
        assert_eq!(retrieved.unwrap(), data);
    }

    #[tokio::test]
    async fn test_versioned_playback_expiry() {
        let vp = VersionedPlayback {
            version: "test123".to_string(),
            result: super::super::PlaybackResult {
                playback_infos: std::collections::HashMap::new(),
                default_mode: "direct".to_string(),
                metadata: std::collections::HashMap::new(),
            },
            expires_at: 0, // Already expired
        };
        assert!(vp.is_expired());

        let vp_future = VersionedPlayback {
            expires_at: chrono::Utc::now().timestamp() + 3600,
            ..vp
        };
        assert!(!vp_future.is_expired());
    }

    #[test]
    fn test_create_provider_stores_in_memory() {
        let stores = create_provider_stores(None);
        assert_eq!(stores.len(), 6);
        assert!(stores.contains_key("bilibili"));
        assert!(stores.contains_key("alist"));
        assert!(stores.contains_key("emby"));
        assert!(stores.contains_key("direct_url"));
        assert!(stores.contains_key("rtmp"));
        assert!(stores.contains_key("live_proxy"));
    }
}
