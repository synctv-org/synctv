use super::*;
use bytes::Bytes;
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn make_entry(data: &[u8], ttl: Duration) -> StoredEntry {
    StoredEntry::new(Bytes::from(data.to_vec()), ttl)
}

fn default_backend() -> MemoryBackend {
    MemoryBackend::new(64 * 1024 * 1024, Duration::from_hours(1))
}

#[tokio::test]
async fn test_memory_backend_put_get() -> TestResult {
    let backend = default_backend();
    let entry = make_entry(b"hello world", Duration::from_mins(1));

    backend.put("k1", entry.clone()).await?;
    let got = backend.get("k1").await;
    assert_eq!(
        got.ok_or("k1 should be cached")?.data,
        Bytes::from_static(b"hello world")
    );
    Ok(())
}

#[tokio::test]
async fn test_memory_backend_get_miss() {
    let backend = default_backend();
    assert!(backend.get("nonexistent").await.is_none());
}

#[tokio::test]
async fn test_memory_backend_remove() -> TestResult {
    let backend = default_backend();
    backend
        .put("k1", make_entry(b"data", Duration::from_mins(1)))
        .await?;

    backend.remove("k1").await;
    assert!(backend.get("k1").await.is_none());
    Ok(())
}

#[tokio::test]
async fn test_memory_backend_get_presence() -> TestResult {
    let backend = default_backend();
    assert!(backend.get("k1").await.is_none());

    backend
        .put("k1", make_entry(b"x", Duration::from_mins(1)))
        .await?;
    backend.run_pending_tasks().await;
    assert!(backend.get("k1").await.is_some());
    Ok(())
}

#[tokio::test]
async fn test_memory_backend_current_size() -> TestResult {
    let backend = default_backend();
    assert_eq!(backend.current_size(), 0);

    backend
        .put("k1", make_entry(b"abcde", Duration::from_mins(1)))
        .await?;
    assert_eq!(backend.current_size(), 5);

    backend
        .put("k2", make_entry(b"12345678", Duration::from_mins(1)))
        .await?;
    assert_eq!(backend.current_size(), 13);

    backend.remove("k1").await;
    assert_eq!(backend.current_size(), 8);
    Ok(())
}

#[tokio::test]
async fn test_memory_backend_keys() -> TestResult {
    let backend = default_backend();
    backend
        .put("alpha", make_entry(b"1", Duration::from_mins(1)))
        .await?;
    backend
        .put("beta", make_entry(b"2", Duration::from_mins(1)))
        .await?;

    let mut keys = backend.keys().await;
    keys.sort();
    assert_eq!(keys, vec!["alpha".to_string(), "beta".to_string()]);
    Ok(())
}

#[tokio::test]
async fn test_memory_backend_evict_expired() -> TestResult {
    let backend = default_backend();

    backend
        .put("short", make_entry(b"gone", Duration::from_millis(10)))
        .await?;
    backend
        .put("long", make_entry(b"stays", Duration::from_hours(1)))
        .await?;

    tokio::time::sleep(Duration::from_millis(50)).await;

    let evicted = backend.evict_expired().await;
    assert_eq!(evicted, 1);
    assert!(backend.get("long").await.is_some());
    assert!(backend.get("short").await.is_none());
    Ok(())
}

#[tokio::test]
async fn test_memory_backend_evict_to_size() -> TestResult {
    let backend = default_backend();

    for i in 0..5u8 {
        let data = vec![i; 100];
        backend
            .put(&format!("k{i}"), make_entry(&data, Duration::from_hours(1)))
            .await?;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(backend.current_size(), 500);

    let freed = backend.evict_to_size(250).await;
    assert!(
        freed >= 200,
        "Expected at least 200 bytes freed, got {freed}"
    );
    assert!(
        backend.current_size() <= 250,
        "Expected size <= 250, got {}",
        backend.current_size()
    );
    Ok(())
}

#[tokio::test]
async fn test_memory_backend_evict_to_size_already_under() -> TestResult {
    let backend = default_backend();
    backend
        .put("k1", make_entry(b"small", Duration::from_mins(1)))
        .await?;

    let freed = backend.evict_to_size(1_000_000).await;
    assert_eq!(freed, 0);
    assert!(backend.get("k1").await.is_some());
    Ok(())
}

#[tokio::test]
async fn test_memory_backend_replace_updates_size() -> TestResult {
    let backend = default_backend();

    backend
        .put("k1", make_entry(b"short", Duration::from_mins(1)))
        .await?;
    assert_eq!(backend.current_size(), 5);

    backend
        .put(
            "k1",
            make_entry(b"a much longer value", Duration::from_mins(1)),
        )
        .await?;
    assert_eq!(backend.current_size(), 19);

    backend
        .put("k1", make_entry(b"tiny", Duration::from_mins(1)))
        .await?;
    assert_eq!(backend.current_size(), 4);
    Ok(())
}

#[tokio::test]
async fn test_memory_backend_keys_only_returns_live_entries() -> TestResult {
    let backend = MemoryBackend::new(150, Duration::from_hours(1));

    for i in 0..5u8 {
        let data = vec![i; 50];
        backend
            .put(&format!("k{i}"), make_entry(&data, Duration::from_hours(1)))
            .await?;
    }

    backend.run_pending_tasks().await;

    let entry_count = backend.entry_count();
    assert!(
        entry_count <= 3,
        "Expected moka to evict some entries, but entry_count = {entry_count}"
    );

    let keys = backend.keys().await;
    assert_eq!(keys.len() as u64, entry_count);
    assert!(
        futures::future::join_all(keys.iter().map(|key| backend.get(key)))
            .await
            .into_iter()
            .all(|entry| entry.is_some())
    );
    Ok(())
}

#[tokio::test]
async fn test_memory_backend_replace_size_tracking_no_double_subtract() -> TestResult {
    let backend = MemoryBackend::new(500, Duration::from_hours(1));

    for i in 0..4u8 {
        backend
            .put(
                &format!("filler_{i}"),
                make_entry(&[i; 100], Duration::from_hours(1)),
            )
            .await?;
    }
    backend.run_pending_tasks().await;

    backend
        .put("target", make_entry(&[0u8; 80], Duration::from_hours(1)))
        .await?;
    backend.run_pending_tasks().await;
    let size_before_replace = backend.current_size();

    backend
        .put("target", make_entry(&[1u8; 80], Duration::from_hours(1)))
        .await?;
    backend.run_pending_tasks().await;

    let size_after_replace = backend.current_size();
    assert_eq!(
        size_before_replace, size_after_replace,
        "Size should be unchanged after same-size replace (before={size_before_replace}, after={size_after_replace})"
    );
    Ok(())
}

#[tokio::test]
async fn test_memory_backend_evict_to_size_lru_ordering_not_affected_by_get() -> TestResult {
    let backend = default_backend();

    for i in 0..5u8 {
        let data = vec![i; 100];
        backend
            .put(&format!("k{i}"), make_entry(&data, Duration::from_hours(1)))
            .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(backend.current_size(), 500);

    let freed = backend.evict_to_size(400).await;
    assert_eq!(freed, 100, "Should have freed exactly 100 bytes (k0)");
    assert_eq!(backend.current_size(), 400);
    assert!(
        backend.get("k0").await.is_none(),
        "k0 (oldest) should have been evicted"
    );
    assert!(backend.get("k1").await.is_some(), "k1 should still exist");
    assert!(
        backend.get("k4").await.is_some(),
        "k4 (newest) should still exist"
    );
    Ok(())
}

#[tokio::test]
async fn test_memory_backend_evict_to_size_respects_last_accessed_order() -> TestResult {
    let backend = default_backend();

    backend
        .put("k0", make_entry(&[0u8; 100], Duration::from_hours(1)))
        .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;

    backend
        .put("k1", make_entry(&[1u8; 100], Duration::from_hours(1)))
        .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;

    backend
        .put("k2", make_entry(&[2u8; 100], Duration::from_hours(1)))
        .await?;

    assert_eq!(backend.current_size(), 300);

    assert!(
        backend.get("k0").await.is_some(),
        "k0 should exist before refreshing access time"
    );

    let freed = backend.evict_to_size(200).await;
    assert_eq!(freed, 100, "Should have freed exactly 100 bytes");
    assert_eq!(backend.current_size(), 200);

    assert!(
        backend.get("k1").await.is_none(),
        "k1 should have been evicted (oldest access time)"
    );
    assert!(
        backend.get("k0").await.is_some(),
        "k0 should still exist (recently accessed)"
    );
    assert!(backend.get("k2").await.is_some(), "k2 should still exist");
    Ok(())
}

#[tokio::test]
async fn test_memory_backend_size_returns_to_zero() -> TestResult {
    let backend = default_backend();

    for round in 0..3u8 {
        let key = format!("k{round}");
        backend
            .put(&key, make_entry(&[round; 50], Duration::from_mins(1)))
            .await?;
        backend
            .put(&key, make_entry(&[round; 100], Duration::from_mins(1)))
            .await?;
    }
    backend.run_pending_tasks().await;
    assert_eq!(backend.current_size(), 300);

    for round in 0..3u8 {
        backend.remove(&format!("k{round}")).await;
    }
    backend.run_pending_tasks().await;

    assert_eq!(
        backend.current_size(),
        0,
        "After removing all entries, total_bytes should be 0"
    );
    Ok(())
}
