use super::*;
use std::time::Duration;

use bytes::Bytes;

use crate::slice_cache::backend::memory::MemoryBackend;
use crate::slice_cache::etag::StoredEntry;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn memory_backend() -> Arc<CacheBackend> {
    Arc::new(CacheBackend::Memory(MemoryBackend::new(
        64 * 1024 * 1024,
        Duration::from_hours(1),
    )))
}

fn fast_config() -> SliceCacheConfig {
    SliceCacheConfig {
        eviction_interval: Duration::from_millis(50),
        max_cache_size: 1024,
        watermark_ratio: 0.875,
        ..SliceCacheConfig::default()
    }
}

#[test]
fn watermark_bytes_clamps_invalid_ratios() {
    assert_eq!(watermark_bytes(1_000, f64::NAN), 1_000);
    assert_eq!(watermark_bytes(1_000, f64::INFINITY), 1_000);
    assert_eq!(watermark_bytes(1_000, -0.5), 0);
    assert_eq!(watermark_bytes(1_000, 0.0), 0);
    assert_eq!(watermark_bytes(1_000, 0.875), 875);
    assert_eq!(watermark_bytes(1_000, 1.0), 1_000);
    assert_eq!(watermark_bytes(1_000, 2.0), 1_000);
}

#[tokio::test]
async fn test_lifecycle_evicts_expired() -> TestResult {
    let backend = memory_backend();

    let expired_entry = StoredEntry {
        data: Bytes::from("old_data"),
        inserted_at: std::time::SystemTime::now() - Duration::from_mins(2),
        ttl: Duration::from_secs(1),
        last_accessed: std::time::SystemTime::now() - Duration::from_mins(2),
    };
    backend.put("expired_key", expired_entry).await?;

    let fresh_entry = StoredEntry::new(Bytes::from("fresh"), Duration::from_hours(1));
    backend.put("fresh_key", fresh_entry).await?;

    assert!(backend.get("expired_key").await.is_some());
    assert!(backend.get("fresh_key").await.is_some());

    let manager = CacheLifecycleManager::new(Arc::clone(&backend), fast_config());
    let cancel = manager.cancellation_token();
    let handle = manager.start();

    tokio::time::sleep(Duration::from_millis(200)).await;

    cancel.cancel();
    handle.await?;

    assert!(backend.get("expired_key").await.is_none());
    assert!(backend.get("fresh_key").await.is_some());
    Ok(())
}

#[tokio::test]
async fn test_lifecycle_watermark_eviction() -> TestResult {
    let backend = memory_backend();

    let config = SliceCacheConfig {
        eviction_interval: Duration::from_millis(50),
        max_cache_size: 500,
        watermark_ratio: 0.5,
        ..SliceCacheConfig::default()
    };

    for i in 0..4u8 {
        let entry = StoredEntry::new(Bytes::from(vec![i; 100]), Duration::from_hours(1));
        backend.put(&format!("key_{i}"), entry).await?;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    assert_eq!(backend.current_size(), 400);

    let manager = CacheLifecycleManager::new(Arc::clone(&backend), config);
    let cancel = manager.cancellation_token();
    let handle = manager.start();

    tokio::time::sleep(Duration::from_millis(200)).await;

    cancel.cancel();
    handle.await?;

    assert!(
        backend.current_size() <= 250,
        "Expected size <= 250, got {}",
        backend.current_size()
    );
    Ok(())
}

#[tokio::test]
async fn test_lifecycle_cancellation() -> TestResult {
    let backend = memory_backend();
    let config = SliceCacheConfig {
        eviction_interval: Duration::from_hours(1),
        ..SliceCacheConfig::default()
    };

    let manager = CacheLifecycleManager::new(Arc::clone(&backend), config);
    let cancel = manager.cancellation_token();
    let handle = manager.start();

    cancel.cancel();

    tokio::time::timeout(Duration::from_secs(2), handle).await??;
    Ok(())
}

#[tokio::test]
async fn test_lifecycle_no_eviction_when_under_watermark() -> TestResult {
    let backend = memory_backend();

    let config = SliceCacheConfig {
        eviction_interval: Duration::from_millis(50),
        max_cache_size: 10_000,
        watermark_ratio: 0.875,
        ..SliceCacheConfig::default()
    };

    let entry = StoredEntry::new(Bytes::from("tiny"), Duration::from_hours(1));
    backend.put("k1", entry).await?;

    let manager = CacheLifecycleManager::new(Arc::clone(&backend), config);
    let cancel = manager.cancellation_token();
    let handle = manager.start();

    tokio::time::sleep(Duration::from_millis(200)).await;

    cancel.cancel();
    handle.await?;

    assert!(backend.get("k1").await.is_some());
    assert_eq!(backend.current_size(), 4);
    Ok(())
}

#[tokio::test]
async fn test_lifecycle_multiple_cycles() -> TestResult {
    let backend = memory_backend();
    let config = fast_config();

    let manager = CacheLifecycleManager::new(Arc::clone(&backend), config);
    let cancel = manager.cancellation_token();
    let handle = manager.start();

    tokio::time::sleep(Duration::from_millis(100)).await;
    let expired_entry = StoredEntry {
        data: Bytes::from("late_expired"),
        inserted_at: std::time::SystemTime::now() - Duration::from_mins(2),
        ttl: Duration::from_secs(1),
        last_accessed: std::time::SystemTime::now() - Duration::from_mins(2),
    };
    backend.put("late_key", expired_entry).await?;

    tokio::time::sleep(Duration::from_millis(200)).await;

    cancel.cancel();
    handle.await?;

    assert!(backend.get("late_key").await.is_none());
    Ok(())
}
