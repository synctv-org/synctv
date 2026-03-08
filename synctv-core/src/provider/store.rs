// Provider Store - key-value storage abstraction for provider caching and locking

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use moka::Expiry;
use parking_lot::Mutex;
use thiserror::Error;
use uuid::Uuid;

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

#[derive(Clone)]
struct LockValue {
    owner_token: String,
    ttl: Duration,
}

/// Moka `Expiry` for lock entries: each lock key expires after its stored TTL `Duration`.
struct LockEntryExpiry;

impl Expiry<String, LockValue> for LockEntryExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &LockValue,
        _current_time: std::time::Instant,
    ) -> Option<Duration> {
        Some(value.ttl)
    }
}

/// In-memory provider store backed by `moka::future::Cache` with per-entry TTL support.
///
/// Locks use a separate `moka::sync::Cache<String, Duration>` with per-entry TTL so that
/// leaked guards auto-expire rather than holding the lock forever.
/// A `Mutex` guards the check-and-insert to prevent TOCTOU races.
pub struct InMemoryProviderStore {
    cache: moka::future::Cache<String, TtlValue>,
    locks: moka::sync::Cache<String, LockValue>,
    lock_mutex: Mutex<()>,
}

impl InMemoryProviderStore {
    pub fn new(max_capacity: u64) -> Self {
        Self {
            cache: moka::future::Cache::builder()
                .max_capacity(max_capacity)
                .expire_after(PerEntryExpiry)
                .build(),
            locks: moka::sync::Cache::builder()
                .expire_after(LockEntryExpiry)
                .build(),
            lock_mutex: Mutex::new(()),
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

    async fn lock(&self, key: &str, ttl: Duration) -> Result<StoreLockGuard, StoreError> {
        // Atomic check-and-insert under mutex to prevent TOCTOU race.
        let _guard = self.lock_mutex.lock();
        if self.locks.contains_key(key) {
            return Err(StoreError::LockFailed(format!("key already locked: {key}")));
        }
        let owner_token = Uuid::new_v4().to_string();
        self.locks.insert(
            key.to_string(),
            LockValue {
                owner_token: owner_token.clone(),
                ttl,
            },
        );
        drop(_guard);

        let locks = self.locks.clone();
        let key_owned = key.to_string();
        let owner_token_owned = owner_token;
        Ok(StoreLockGuard::new(move || {
            let should_release = locks
                .get(&key_owned)
                .is_some_and(|value| value.owner_token == owner_token_owned);
            if should_release {
                locks.invalidate(&key_owned);
            }
        }))
    }
}

// ---------------------------------------------------------------------------
// RedisProviderStore
// ---------------------------------------------------------------------------

/// Redis-backed provider store with distributed locking via `SET NX EX`.
///
/// Uses a shared `Arc<RwLock<ConnectionManager>>` so that in Sentinel mode the
/// background health check can hot-swap the inner connection on failover and
/// this store automatically picks up the new master.
pub struct RedisProviderStore {
    shared_conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
}

impl RedisProviderStore {
    pub fn new(shared_conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>) -> Self {
        Self { shared_conn }
    }

    /// Get a cloned connection snapshot for the current operation.
    async fn conn(&self) -> redis::aio::ConnectionManager {
        self.shared_conn.read().await.clone()
    }
}

#[async_trait::async_trait]
impl ProviderStore for RedisProviderStore {
    async fn get_raw(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let result: Option<Vec<u8>> = redis::cmd("GET")
            .arg(key)
            .query_async(&mut self.conn().await)
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
            .query_async::<()>(&mut self.conn().await)
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), StoreError> {
        redis::cmd("DEL")
            .arg(key)
            .query_async::<()>(&mut self.conn().await)
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn lock(&self, key: &str, ttl: Duration) -> Result<StoreLockGuard, StoreError> {
        let ttl_secs = ttl.as_secs().max(1);
        let owner_token = Uuid::new_v4().to_string();
        for _ in 0..10 {
            let result: Option<String> = redis::cmd("SET")
                .arg(key)
                .arg(&owner_token)
                .arg("NX")
                .arg("EX")
                .arg(ttl_secs)
                .query_async(&mut self.conn().await)
                .await
                .map_err(|e| StoreError::Backend(e.to_string()))?;

            if result.is_some() {
                let key_owned = key.to_string();
                let shared = self.shared_conn.clone();
                let owner_token_owned = owner_token.clone();
                return Ok(StoreLockGuard::new(move || {
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        handle.spawn(async move {
                            let delete_if_owner = redis::Script::new(
                                r#"
                                if redis.call("GET", KEYS[1]) == ARGV[1] then
                                    return redis.call("DEL", KEYS[1])
                                end
                                return 0
                            "#,
                            );
                            let mut conn = shared.read().await.clone();
                            let _: Result<i32, _> = delete_if_owner
                                .key(&key_owned)
                                .arg(&owner_token_owned)
                                .invoke_async(&mut conn)
                                .await;
                        });
                    } else {
                        tracing::warn!(
                            key = %key_owned,
                            "Skipping Redis provider lock release because no Tokio runtime is available; lock will expire via TTL"
                        );
                    }
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
    pub const fn new(inner: S, prefix: String) -> Self {
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
// ProviderStoreRegistry — lazy, per-provider store creation
// ---------------------------------------------------------------------------

/// Registry that lazily creates and caches per-provider stores on first access.
///
/// No need to pre-register provider names — calling `load("some_new_provider")`
/// automatically creates a prefixed store backed by Redis (if available) or in-memory.
pub struct ProviderStoreRegistry {
    redis: Option<Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>,
    stores: Mutex<HashMap<String, Arc<dyn ProviderStore>>>,
}

impl ProviderStoreRegistry {
    /// Create a new registry, optionally backed by Redis (Sentinel-safe shared handle).
    pub fn new(redis: Option<Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>) -> Self {
        Self {
            redis,
            stores: Mutex::new(HashMap::new()),
        }
    }

    /// Get or lazily create a store for the given provider name.
    ///
    /// The store is prefixed with `synctv:provider:{name}` and cached for
    /// subsequent calls with the same name.
    pub fn load(&self, name: &str) -> Arc<dyn ProviderStore> {
        let mut stores = self.stores.lock();
        stores
            .entry(name.to_string())
            .or_insert_with(|| {
                let prefix = format!("synctv:provider:{name}");
                match &self.redis {
                    Some(shared_conn) => Arc::new(PrefixedProviderStore::new(
                        RedisProviderStore::new(shared_conn.clone()),
                        prefix,
                    )),
                    None => Arc::new(PrefixedProviderStore::new(
                        InMemoryProviderStore::new(10_000),
                        prefix,
                    )),
                }
            })
            .clone()
    }
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
    use redis::AsyncCommands;
    use std::sync::Arc;
    use testcontainers::{runners::AsyncRunner, ContainerAsync};
    use testcontainers_modules::redis::Redis;

    async fn start_redis() -> (
        ContainerAsync<Redis>,
        Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
    ) {
        let container = Redis::default()
            .start()
            .await
            .expect("Failed to start Redis container");
        let host = container
            .get_host()
            .await
            .expect("Failed to get Redis host");
        let port = container
            .get_host_port_ipv4(6379)
            .await
            .expect("Failed to get Redis port");
        let redis_url = format!("redis://{host}:{port}");
        let client =
            redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");
        let conn = client
            .get_connection_manager()
            .await
            .expect("Failed to create ConnectionManager");
        (container, Arc::new(tokio::sync::RwLock::new(conn)))
    }

    #[tokio::test]
    async fn test_in_memory_store_get_set() {
        let store = InMemoryProviderStore::new(100);
        assert!(store.get_raw("key1").await.unwrap().is_none());
        store
            .set_raw("key1", b"hello", Duration::from_mins(1))
            .await
            .unwrap();
        assert_eq!(store.get_raw("key1").await.unwrap().unwrap(), b"hello");
    }

    #[tokio::test]
    async fn test_in_memory_store_delete() {
        let store = InMemoryProviderStore::new(100);
        store
            .set_raw("key1", b"hello", Duration::from_mins(1))
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
    async fn test_in_memory_stale_guard_does_not_delete_new_owner_lock() {
        let store = InMemoryProviderStore::new(100);
        let first_guard = store.lock("mylock", Duration::from_secs(1)).await.unwrap();

        tokio::time::sleep(Duration::from_millis(1200)).await;

        let second_guard = store.lock("mylock", Duration::from_secs(10)).await.unwrap();
        drop(first_guard);

        assert!(
            store.lock("mylock", Duration::from_secs(10)).await.is_err(),
            "stale guard must not invalidate a new owner's lock"
        );

        drop(second_guard);
        assert!(store.lock("mylock", Duration::from_secs(10)).await.is_ok());
    }

    #[tokio::test]
    async fn test_prefixed_store() {
        let inner = InMemoryProviderStore::new(100);
        let store = PrefixedProviderStore::new(inner, "test:prefix".to_string());
        store
            .set_raw("key1", b"value", Duration::from_mins(1))
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
            .set("typed_key", &data, Duration::from_mins(1))
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

    #[tokio::test]
    async fn test_provider_store_registry_lazy_creation() {
        let registry = ProviderStoreRegistry::new(None);

        // First load creates the store
        let store1 = registry.load("bilibili");
        store1
            .set_raw("key1", b"value1", Duration::from_mins(1))
            .await
            .unwrap();

        // Second load returns the same (cached) store instance
        let store2 = registry.load("bilibili");
        assert_eq!(store2.get_raw("key1").await.unwrap().unwrap(), b"value1");

        // Different provider name creates a separate store
        let store3 = registry.load("emby");
        assert!(store3.get_raw("key1").await.unwrap().is_none());
    }

    #[test]
    fn test_provider_store_registry_any_name() {
        let registry = ProviderStoreRegistry::new(None);
        // Any arbitrary provider name works — no pre-registration needed
        let _store = registry.load("my_custom_provider");
        let _store2 = registry.load("another_one");
    }

    #[test]
    fn test_store_lock_guard_drop_without_runtime_does_not_panic() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let guard = StoreLockGuard::new(|| {
                let _ = tokio::runtime::Handle::try_current()
                    .expect("release callback should not assume runtime in this test");
            });
            drop(guard);
        }));

        assert!(
            result.is_err(),
            "control check: plain runtime-dependent callbacks still panic without protection"
        );

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let guard = StoreLockGuard::new(|| {
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async {});
                }
            });
            drop(guard);
        }));

        assert!(
            result.is_ok(),
            "StoreLockGuard drop path must not panic when runtime is unavailable"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_redis_lock_drop_does_not_delete_new_owner_lock() {
        let (_container, shared_conn) = start_redis().await;
        let store = RedisProviderStore::new(shared_conn.clone());
        let lock_key = "provider-lock-race";

        let first_guard = store.lock(lock_key, Duration::from_secs(1)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;

        let second_guard = store.lock(lock_key, Duration::from_secs(30)).await.unwrap();

        drop(first_guard);

        let mut conn = shared_conn.read().await.clone();
        let value: Option<String> = conn.get(lock_key).await.unwrap();
        assert!(
            value.is_some(),
            "dropping stale guard must not delete lock held by newer owner"
        );

        drop(second_guard);
    }
}
