use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn test_cache() -> anyhow::Result<SliceCache> {
    SliceCache::new(SliceCacheConfig::default())
}

#[test]
fn cleanup_stale_meta_evicts_oldest_entries() -> TestResult {
    let cache = test_cache()?;
    let now = std::time::SystemTime::now();

    for index in 0_u64..6 {
        cache.meta.insert(
            format!("meta-{index}"),
            CachedResourceMeta {
                etag: None,
                last_modified: None,
                total_size: None,
                supports_ranges: false,
                content_type: None,
                content_encoding: None,
                validated_at: now,
                last_accessed: now + Duration::from_secs(index),
            },
        );
    }

    cache.cleanup_stale_meta_with_limit(4);

    assert_eq!(cache.meta_count(), 2);
    assert!(!cache.meta.contains_key("meta-0"));
    assert!(!cache.meta.contains_key("meta-1"));
    assert!(!cache.meta.contains_key("meta-2"));
    assert!(!cache.meta.contains_key("meta-3"));
    assert!(cache.meta.contains_key("meta-4"));
    assert!(cache.meta.contains_key("meta-5"));
    Ok(())
}

#[test]
fn cleanup_stale_meta_skips_when_within_limit() -> TestResult {
    let cache = test_cache()?;
    let now = std::time::SystemTime::now();

    for index in 0..4 {
        cache.meta.insert(
            format!("meta-{index}"),
            CachedResourceMeta {
                etag: None,
                last_modified: None,
                total_size: None,
                supports_ranges: false,
                content_type: None,
                content_encoding: None,
                validated_at: now,
                last_accessed: now,
            },
        );
    }

    cache.cleanup_stale_meta_with_limit(4);

    assert_eq!(cache.meta_count(), 4);
    Ok(())
}

#[test]
fn put_resource_meta_applies_metadata_cap() -> TestResult {
    let cache = test_cache()?;
    let now = std::time::SystemTime::now();

    for index in 0_u64..6 {
        cache.put_resource_meta_by_key_with_limit(
            format!("meta-{index}"),
            CachedResourceMeta {
                etag: None,
                last_modified: None,
                total_size: None,
                supports_ranges: false,
                content_type: None,
                content_encoding: None,
                validated_at: now,
                last_accessed: now + Duration::from_secs(index),
            },
            4,
        );
    }

    assert_eq!(cache.meta_count(), 3);
    assert!(!cache.meta.contains_key("meta-0"));
    assert!(!cache.meta.contains_key("meta-1"));
    assert!(!cache.meta.contains_key("meta-2"));
    assert!(cache.meta.contains_key("meta-3"));
    assert!(cache.meta.contains_key("meta-4"));
    assert!(cache.meta.contains_key("meta-5"));
    assert!(cache.meta_count() <= 4);
    Ok(())
}

#[tokio::test]
async fn stats_reports_backend_and_runtime_counters() -> TestResult {
    let cache = test_cache()?;
    cache
        .backend
        .put(
            "slice-1",
            StoredEntry::new(Bytes::from_static(b"cached"), Duration::from_mins(1)),
        )
        .await?;
    cache.meta.insert(
        "meta-1".to_string(),
        CachedResourceMeta {
            etag: Some("etag".to_string()),
            last_modified: None,
            total_size: Some(6),
            supports_ranges: true,
            content_type: Some("video/mp2t".to_string()),
            content_encoding: None,
            validated_at: std::time::SystemTime::now(),
            last_accessed: std::time::SystemTime::now(),
        },
    );
    cache.updating_keys.insert("slice-1".to_string());
    cache
        .locks
        .insert("slice-1".to_string(), Arc::new(Mutex::new(())));

    let stats = cache.stats();

    assert!(stats.engine_enabled);
    assert_eq!(stats.backend, "memory");
    assert_eq!(stats.file_cache_dir, None);
    assert_eq!(stats.current_size_bytes, 6);
    assert_eq!(stats.entry_count, 1);
    assert_eq!(stats.metadata_entries, 1);
    assert_eq!(stats.updating_entries, 1);
    assert_eq!(stats.lock_count, 1);
    assert!(stats.usage_ratio > 0.0);
    Ok(())
}

#[tokio::test]
async fn purge_all_removes_entries_and_runtime_metadata() -> TestResult {
    let cache = test_cache()?;
    cache
        .backend
        .put(
            "slice-1",
            StoredEntry::new(Bytes::from_static(b"cached"), Duration::from_mins(1)),
        )
        .await?;
    cache.meta.insert(
        "meta-1".to_string(),
        CachedResourceMeta {
            etag: None,
            last_modified: None,
            total_size: None,
            supports_ranges: false,
            content_type: None,
            content_encoding: None,
            validated_at: std::time::SystemTime::now(),
            last_accessed: std::time::SystemTime::now(),
        },
    );
    cache.updating_keys.insert("slice-1".to_string());
    cache
        .locks
        .insert("slice-1".to_string(), Arc::new(Mutex::new(())));

    let result = cache.purge_all().await;

    assert_eq!(result.removed_entries, 1);
    assert_eq!(result.freed_bytes, 6);
    assert_eq!(cache.backend.entry_count(), 0);
    assert_eq!(cache.meta_count(), 0);
    assert!(cache.updating_keys.is_empty());
    assert!(cache.locks.is_empty());
    Ok(())
}

#[tokio::test]
async fn evict_expired_entries_removes_expired_backend_entries() -> TestResult {
    let cache = test_cache()?;
    cache
        .backend
        .put(
            "expired-slice",
            StoredEntry::new(Bytes::from_static(b"old"), Duration::ZERO),
        )
        .await?;

    let removed = cache.evict_expired_entries().await;

    assert_eq!(removed, 1);
    assert_eq!(cache.backend.entry_count(), 0);
    Ok(())
}
