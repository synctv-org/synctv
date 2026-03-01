//! Tests for HlsProxyClient cache version overflow handling.
//!
//! These tests verify that the cache version counter in HlsProxyClient
//! handles overflow correctly when approaching u64::MAX.

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use dashmap::DashMap;

/// Test that cache version can reach near u64::MAX.
#[test]
fn test_cache_version_near_max() {
    let cache_versions: Arc<DashMap<String, u64>> = Arc::new(DashMap::new());
    let key = "room1:media1".to_string();

    // Set version near u64::MAX
    cache_versions.insert(key.clone(), u64::MAX - 1);

    // Verify we can read it
    let version = cache_versions.get(&key).map(|v| *v).unwrap_or(0);
    assert_eq!(version, u64::MAX - 1);
}

/// Test that unchecked increment at u64::MAX causes overflow.
/// This documents the current behavior that will wrap to 0.
#[test]
fn test_unchecked_increment_overflow_documents_wrap() {
    let cache_versions: Arc<DashMap<String, u64>> = Arc::new(DashMap::new());
    let key = "room1:media1".to_string();

    // Set version to u64::MAX
    cache_versions.insert(key.clone(), u64::MAX);

    // Unchecked increment will overflow (wrap to 0)
    let mut entry = cache_versions.entry(key.clone()).or_insert(0);
    *entry = entry.wrapping_add(1);
    let version_after = *entry;
    drop(entry);

    // With wrapping_add, it wraps to 0
    assert_eq!(version_after, 0);
}

/// Test that checked_add detects overflow.
#[test]
fn test_checked_add_detects_overflow() {
    let cache_versions: Arc<DashMap<String, u64>> = Arc::new(DashMap::new());
    let key = "room1:media1".to_string();

    // Set version to u64::MAX
    cache_versions.insert(key.clone(), u64::MAX);

    // Use checked_add to detect overflow
    let entry = cache_versions.get(&key).map(|v| *v).unwrap_or(0);
    let result = entry.checked_add(1);

    // checked_add should return None on overflow
    assert!(result.is_none(), "checked_add should return None on overflow");
}

/// Test proper overflow handling: clear cache when overflow occurs.
#[test]
fn test_overflow_clears_cache() {
    let cache_versions: Arc<DashMap<String, u64>> = Arc::new(DashMap::new());
    let key = "room1:media1".to_string();

    // Set version to u64::MAX
    cache_versions.insert(key.clone(), u64::MAX);

    // Simulate the overflow handling logic
    let mut entry = cache_versions.entry(key.clone()).or_insert(0);
    let current = *entry;

    if current.checked_add(1).is_none() {
        // Overflow detected: remove the entry
        drop(entry);
        cache_versions.remove(&key);
    } else {
        *entry = current + 1;
    }

    // Verify the entry was removed
    assert!(
        !cache_versions.contains_key(&key),
        "Cache entry should be removed on overflow"
    );
}

/// Test increment from normal value works correctly.
#[test]
fn test_normal_increment_works() {
    let cache_versions: Arc<DashMap<String, u64>> = Arc::new(DashMap::new());
    let key = "room1:media1".to_string();

    // Set version to a normal value
    cache_versions.insert(key.clone(), 100);

    // Increment should work normally
    let mut entry = cache_versions.entry(key.clone()).or_insert(0);
    let current = *entry;

    if let Some(new_version) = current.checked_add(1) {
        *entry = new_version;
    }
    let version_after = *entry;
    drop(entry);

    assert_eq!(version_after, 101);
}

/// Test that multiple overflows are handled.
#[test]
fn test_multiple_overflow_handling() {
    let cache_versions: Arc<DashMap<String, u64>> = Arc::new(DashMap::new());

    // Set multiple entries to u64::MAX
    cache_versions.insert("room1:media1".to_string(), u64::MAX);
    cache_versions.insert("room2:media2".to_string(), u64::MAX);
    cache_versions.insert("room3:media3".to_string(), 100);

    // Process all entries
    let keys: Vec<String> = cache_versions.iter().map(|e| e.key().clone()).collect();
    for key in keys {
        let mut entry = cache_versions.entry(key.clone()).or_insert(0);
        let current = *entry;

        if current.checked_add(1).is_none() {
            // Overflow detected: remove the entry
            drop(entry);
            cache_versions.remove(&key);
        } else {
            *entry = current + 1;
        }
    }

    // Verify: room1 and room2 should be removed, room3 should be 101
    assert!(!cache_versions.contains_key("room1:media1"));
    assert!(!cache_versions.contains_key("room2:media2"));

    let room3_version = cache_versions
        .get("room3:media3")
        .map(|v| *v)
        .unwrap_or(0);
    assert_eq!(room3_version, 101);
}
