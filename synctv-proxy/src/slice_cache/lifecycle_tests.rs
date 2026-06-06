use super::*;
use std::time::Duration;

use bytes::Bytes;

use crate::slice_cache::backend::memory::MemoryBackend;
use crate::slice_cache::etag::StoredEntry;

/// Helper: create a memory backend wrapped in CacheBackend.
fn memory_backend() -> Arc<CacheBackend> {
    Arc::new(CacheBackend::Memory(MemoryBackend::new(
        64 * 1024 * 1024,
        Duration::from_hours(1),
    )))
}

/// Helper: config with a very short eviction interval for testing.
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
async fn test_lifecycle_evicts_expired() {
    let backend = memory_backend();

    // Insert an entry that is already expired.
    let expired_entry = StoredEntry {
        data: Bytes::from("old_data"),
        inserted_at: std::time::SystemTime::now() - Duration::from_mins(2),
        ttl: Duration::from_secs(1),
        last_accessed: std::time::SystemTime::now() - Duration::from_mins(2),
    };
    backend.put("expired_key", expired_entry).await.unwrap();

    // Insert a fresh entry.
    let fresh_entry = StoredEntry::new(Bytes::from("fresh"), Duration::from_hours(1));
    backend.put("fresh_key", fresh_entry).await.unwrap();

    // Verify both entries are retrievable before lifecycle starts.
    assert!(backend.get("expired_key").await.is_some());
    assert!(backend.get("fresh_key").await.is_some());

    // Start the lifecycle manager.
    let manager = CacheLifecycleManager::new(Arc::clone(&backend), fast_config());
    let cancel = manager.cancellation_token();
    let handle = manager.start();

    // Wait for at least one cycle.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Cancel and wait for shutdown.
    cancel.cancel();
    handle.await.unwrap();

    // The expired entry should have been evicted.
    assert!(backend.get("expired_key").await.is_none());
    // The fresh entry should still exist.
    assert!(backend.get("fresh_key").await.is_some());
}

#[tokio::test]
async fn test_lifecycle_watermark_eviction() {
    let backend = memory_backend();

    let config = SliceCacheConfig {
        eviction_interval: Duration::from_millis(50),
        max_cache_size: 500,  // 500 bytes max
        watermark_ratio: 0.5, // watermark at 250 bytes
        ..SliceCacheConfig::default()
    };

    // Insert entries totaling 400 bytes (above 250 watermark).
    for i in 0..4u8 {
        let entry = StoredEntry::new(Bytes::from(vec![i; 100]), Duration::from_hours(1));
        backend.put(&format!("key_{i}"), entry).await.unwrap();
        // Small sleep so last_accessed differs for LRU.
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    assert_eq!(backend.current_size(), 400);

    // Start the lifecycle manager.
    let manager = CacheLifecycleManager::new(Arc::clone(&backend), config);
    let cancel = manager.cancellation_token();
    let handle = manager.start();

    // Wait for eviction.
    tokio::time::sleep(Duration::from_millis(200)).await;

    cancel.cancel();
    handle.await.unwrap();

    // Size should be at or below the watermark (250).
    assert!(
        backend.current_size() <= 250,
        "Expected size <= 250, got {}",
        backend.current_size()
    );
}

#[tokio::test]
async fn test_lifecycle_cancellation() {
    let backend = memory_backend();
    let config = SliceCacheConfig {
        eviction_interval: Duration::from_hours(1), // Long interval
        ..SliceCacheConfig::default()
    };

    let manager = CacheLifecycleManager::new(Arc::clone(&backend), config);
    let cancel = manager.cancellation_token();
    let handle = manager.start();

    // Cancel immediately.
    cancel.cancel();

    // The task should exit promptly.
    let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
    assert!(
        result.is_ok(),
        "Lifecycle manager should have stopped within 2 seconds"
    );
    result.unwrap().unwrap();
}

#[tokio::test]
async fn test_lifecycle_no_eviction_when_under_watermark() {
    let backend = memory_backend();

    let config = SliceCacheConfig {
        eviction_interval: Duration::from_millis(50),
        max_cache_size: 10_000,
        watermark_ratio: 0.875,
        ..SliceCacheConfig::default()
    };

    // Insert a small entry well below the watermark.
    let entry = StoredEntry::new(Bytes::from("tiny"), Duration::from_hours(1));
    backend.put("k1", entry).await.unwrap();

    let manager = CacheLifecycleManager::new(Arc::clone(&backend), config);
    let cancel = manager.cancellation_token();
    let handle = manager.start();

    // Let a few cycles run.
    tokio::time::sleep(Duration::from_millis(200)).await;

    cancel.cancel();
    handle.await.unwrap();

    // The entry should still be there.
    assert!(backend.get("k1").await.is_some());
    assert_eq!(backend.current_size(), 4);
}

#[tokio::test]
async fn test_lifecycle_multiple_cycles() {
    let backend = memory_backend();
    let config = fast_config();

    let manager = CacheLifecycleManager::new(Arc::clone(&backend), config);
    let cancel = manager.cancellation_token();
    let handle = manager.start();

    // Insert an expired entry partway through.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let expired_entry = StoredEntry {
        data: Bytes::from("late_expired"),
        inserted_at: std::time::SystemTime::now() - Duration::from_mins(2),
        ttl: Duration::from_secs(1),
        last_accessed: std::time::SystemTime::now() - Duration::from_mins(2),
    };
    backend.put("late_key", expired_entry).await.unwrap();

    // Wait for another cycle to pick it up.
    tokio::time::sleep(Duration::from_millis(200)).await;

    cancel.cancel();
    handle.await.unwrap();

    assert!(backend.get("late_key").await.is_none());
}
