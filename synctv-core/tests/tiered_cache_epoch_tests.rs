//! `TieredCache` epoch guard tests
//!
//! Tests that concurrent `get()` vs `invalidate()` is correctly handled by the epoch
//! counter: when an invalidation arrives while a `SingleFlight` L2 fetch is in-flight,
//! the stale result is NOT written to L1.
//!
//! Uses a mock L2 backend with artificial delay to simulate the race condition.
//!

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::sync::Arc;
use synctv_core::cache::{CacheKey, CacheL2Backend, TieredCache, Timestamped, Versioned};
use synctv_core::Result;
use synctv_core_testing::{ok, some};

// Test types

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TestId(String);

impl Display for TestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl CacheKey for TestId {
    fn cache_key(&self) -> String {
        self.0.clone()
    }

    fn try_from_id(id: &str) -> Result<Self> {
        Ok(Self(id.to_string()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct TestValue {
    name: String,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl Timestamped for TestValue {
    fn updated_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.updated_at
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct VersionedTestValue {
    name: String,
    version: i64,
}

impl Versioned for VersionedTestValue {
    fn cache_version(&self) -> i64 {
        self.version
    }
}

// Mock L2 backend with artificial delay

struct DelayedL2 {
    store: tokio::sync::RwLock<std::collections::HashMap<String, String>>,
    get_delay: std::time::Duration,
}

impl DelayedL2 {
    fn new(get_delay: std::time::Duration) -> Self {
        Self {
            store: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            get_delay,
        }
    }
}

#[async_trait]
impl CacheL2Backend for DelayedL2 {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        // Read the value first, then simulate slow network transfer.
        // This models the real Redis scenario: the server reads the data
        // but the response takes time to arrive.
        let value = {
            let store = self.store.read().await;
            store.get(key).cloned()
        };
        tokio::time::sleep(self.get_delay).await;
        Ok(value)
    }

    async fn set(&self, key: &str, json: &str, _ttl_secs: u64) -> Result<()> {
        self.store
            .write()
            .await
            .insert(key.to_string(), json.to_string());

        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.store.write().await.remove(key);

        Ok(())
    }

    async fn get_batch(&self, keys: &[String]) -> Result<Vec<Option<String>>> {
        tokio::time::sleep(self.get_delay).await;
        let store = self.store.read().await;
        Ok(keys.iter().map(|k| store.get(k).cloned()).collect())
    }

    async fn set_if_newer(
        &self,
        key: &str,
        json: &str,
        ttl_secs: u64,
        _new_ts_millis: i64,
    ) -> Result<bool> {
        self.set(key, json, ttl_secs).await?;
        Ok(true)
    }

    async fn set_if_version_at_least(
        &self,
        key: &str,
        json: &str,
        ttl_secs: u64,
        _version: i64,
    ) -> Result<bool> {
        self.set(key, json, ttl_secs).await?;
        Ok(true)
    }

    async fn delete_by_prefix(&self, prefix: &str) -> Result<()> {
        self.store
            .write()
            .await
            .retain(|k, _| !k.starts_with(prefix));

        Ok(())
    }

    fn is_active(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn test_epoch_prevents_stale_l1_write() {
    let l2 = Arc::new(DelayedL2::new(std::time::Duration::from_millis(200)));

    // Pre-populate L2 with a stale value
    let stale_value = TestValue {
        name: "stale".to_string(),
        updated_at: chrono::Utc::now() - chrono::Duration::seconds(60),
    };
    let stale_json = ok(
        serde_json::to_string(&stale_value),
        "stale cache value should serialize",
    );
    ok(
        l2.set("test:epoch:k1", &stale_json, 300).await,
        "stale cache value should be written to L2",
    );

    let cache: TieredCache<TestId, TestValue> = TieredCache::new(
        l2.clone(),
        100,
        5,
        300,
        "test:epoch:".to_string(),
        "test_epoch".to_string(),
    );

    let key = TestId("k1".to_string());

    let cache_clone = cache.clone();
    let key_clone = key.clone();
    let get_handle = tokio::spawn(async move { cache_clone.get(&key_clone).await });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    ok(
        cache.invalidate(&key).await,
        "cache key should be invalidated while L2 fetch is in flight",
    );

    let result = ok(
        ok(get_handle.await, "in-flight cache get task should join"),
        "in-flight cache get should succeed",
    );

    // The get() should return the stale value from L2 (it was already in-flight)
    assert!(
        result.is_some(),
        "The in-flight fetch should still return the L2 value"
    );
    assert_eq!(
        some(result, "in-flight cache get should return L2 value").name,
        "stale"
    );

    // But L1 should NOT have been populated with the stale value because
    // the epoch changed during the fetch. A subsequent get() that only checks
    // L1 should miss.
    // Clear L2 to ensure we only check L1
    ok(
        l2.delete("test:epoch:k1").await,
        "stale cache value should be removed from L2",
    );

    // Since L2 is now empty, if L1 was populated with stale data, we'd get it back.
    // If epoch guard works, L1 should be empty and we get None.
    let l1_result = ok(
        cache.get(&key).await,
        "cache lookup after L2 delete should succeed",
    );
    assert!(
        l1_result.is_none(),
        "Stale value should NOT have been written to L1 due to epoch guard"
    );
}

#[tokio::test]
async fn test_l2_versioned_write_does_not_downgrade_newer_l1() {
    let l2 = Arc::new(DelayedL2::new(std::time::Duration::ZERO));
    let cache: TieredCache<TestId, VersionedTestValue> = TieredCache::new(
        l2,
        100,
        5,
        300,
        "test:versioned:".to_string(),
        "test_versioned".to_string(),
    );
    let key = TestId("k1".to_string());

    ok(
        cache
            .set_if_version_at_least(
                &key,
                VersionedTestValue {
                    name: "newer-local".to_string(),
                    version: 10,
                },
            )
            .await,
        "newer versioned cache value should be accepted",
    );

    let updated = ok(
        cache
            .set_if_version_at_least(
                &key,
                VersionedTestValue {
                    name: "older-reload".to_string(),
                    version: 9,
                },
            )
            .await,
        "older versioned cache value write should be evaluated",
    );

    assert!(
        !updated,
        "L1 should reject an older version even when L2 accepts the write"
    );
    let cached = some(
        cache.get_l1(&key).await,
        "newer L1 entry should remain cached",
    );
    assert_eq!(cached.version, 10);
    assert_eq!(cached.name, "newer-local");
}
