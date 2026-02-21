//! TieredCache epoch guard tests
//!
//! Tests that concurrent get() vs invalidate() is correctly handled by the epoch
//! counter: when an invalidation arrives while a SingleFlight L2 fetch is in-flight,
//! the stale result is NOT written to L1.
//!
//! Uses a mock L2 backend with artificial delay to simulate the race condition.
//!
//! Run with: cargo test --test tiered_cache_epoch_tests

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::sync::Arc;
use synctv_core::cache::{CacheKey, CacheL2Backend, TieredCache, Timestamped};
use synctv_core::Result;

// ============================================================================
// Test types
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TestId(String);

impl Display for TestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl CacheKey for TestId {
    fn as_str(&self) -> &str {
        &self.0
    }
    fn from_id(id: &str) -> Self {
        Self(id.to_string())
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

// ============================================================================
// Mock L2 backend with artificial delay
// ============================================================================

struct DelayedL2 {
    /// Values stored in the mock L2
    store: tokio::sync::RwLock<std::collections::HashMap<String, String>>,
    /// How long to delay on get()
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
        let mut store = self.store.write().await;
        store.insert(key.to_string(), json.to_string());
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let mut store = self.store.write().await;
        store.remove(key);
        Ok(())
    }

    async fn delete_with_retry(&self, key: &str, _max_retries: u32, _cache_type: &str) -> Result<()> {
        self.delete(key).await
    }

    async fn get_batch(&self, keys: &[String]) -> Result<Vec<Option<String>>> {
        tokio::time::sleep(self.get_delay).await;
        let store = self.store.read().await;
        Ok(keys.iter().map(|k| store.get(k).cloned()).collect())
    }

    async fn set_if_newer(&self, key: &str, json: &str, ttl_secs: u64, _new_ts_iso: &str) -> Result<bool> {
        self.set(key, json, ttl_secs).await?;
        Ok(true)
    }

    async fn delete_by_prefix(&self, prefix: &str) -> Result<()> {
        let mut store = self.store.write().await;
        store.retain(|k, _| !k.starts_with(prefix));
        Ok(())
    }

    fn is_active(&self) -> bool {
        true
    }

    fn backend_name(&self) -> &'static str {
        "delayed_mock"
    }
}

#[tokio::test]
async fn test_epoch_prevents_stale_l1_write() {
    // Create a mock L2 with a 200ms delay on get()
    let l2 = Arc::new(DelayedL2::new(std::time::Duration::from_millis(200)));

    // Pre-populate L2 with a stale value
    let stale_value = TestValue {
        name: "stale".to_string(),
        updated_at: chrono::Utc::now() - chrono::Duration::seconds(60),
    };
    let stale_json = serde_json::to_string(&stale_value).unwrap();
    l2.set("test:epoch:k1", &stale_json, 300).await.unwrap();

    // Create cache with this delayed L2 backend
    let cache: TieredCache<TestId, TestValue> = TieredCache::new(
        l2.clone(),
        100,
        5,
        300,
        "test:epoch:".to_string(),
        "test_epoch".to_string(),
    )
    .unwrap();

    let key = TestId("k1".to_string());

    // Start a get() that will take ~200ms (due to delayed L2)
    let cache_clone = cache.clone();
    let key_clone = key.clone();
    let get_handle = tokio::spawn(async move {
        cache_clone.get(&key_clone).await
    });

    // Wait a bit for the get() to be in-flight, then invalidate
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    cache.invalidate(&key).await.unwrap();

    // Wait for the get() to finish
    let result = get_handle.await.unwrap().unwrap();

    // The get() should return the stale value from L2 (it was already in-flight)
    assert!(result.is_some(), "The in-flight fetch should still return the L2 value");
    assert_eq!(result.unwrap().name, "stale");

    // But L1 should NOT have been populated with the stale value because
    // the epoch changed during the fetch. A subsequent get() that only checks
    // L1 should miss.
    // Clear L2 to ensure we only check L1
    l2.delete("test:epoch:k1").await.unwrap();

    // Since L2 is now empty, if L1 was populated with stale data, we'd get it back.
    // If epoch guard works, L1 should be empty and we get None.
    let l1_result = cache.get(&key).await.unwrap();
    assert!(
        l1_result.is_none(),
        "Stale value should NOT have been written to L1 due to epoch guard"
    );
}
