use super::*;
use crate::slice_cache::backend::file_format::{
    cache_entry_deadline_millis, encode_header, FileEntryHeader, CACHE_FILE_MAGIC,
};
use crate::slice_cache::backend::file_index::{FileIndex, FileIndexEntry};
use bytes::Bytes;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::SystemTime;
use tempfile::TempDir;

/// Helper: create a `FileBackend` in a fresh temp directory.
async fn make_backend() -> (FileBackend, TempDir) {
    let tmp = TempDir::new().expect("create temp dir");
    let backend = FileBackend::new(tmp.path().to_path_buf(), (2, 2))
        .await
        .expect("create backend");
    (backend, tmp)
}

/// Helper: store an entry with sensible defaults.
async fn put_entry(backend: &FileBackend, key: &str, data: &[u8]) {
    let entry = StoredEntry::new(Bytes::from(data.to_vec()), Duration::from_mins(5));
    backend.put(key, entry).await.expect("put entry");
}

#[tokio::test]
async fn test_file_backend_put_get() {
    let (backend, _tmp) = make_backend().await;
    let data = b"hello cache world";
    let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    put_entry(&backend, key, data).await;

    let result = backend.get(key).await;
    assert!(result.is_some());
    assert_eq!(result.unwrap().data, Bytes::from(data.to_vec()));
}

#[test]
fn test_cache_entry_deadline_saturates() {
    assert_eq!(cache_entry_deadline_millis(1_000, 5), 6_000);
    assert_eq!(cache_entry_deadline_millis(u64::MAX - 1, 1), u64::MAX);
    assert_eq!(cache_entry_deadline_millis(0, u64::MAX), u64::MAX);
}

#[test]
fn test_file_index_total_size_saturates() {
    let index = FileIndex::new();
    index.insert(
        "huge".to_string(),
        FileIndexEntry {
            path: PathBuf::from("huge"),
            data_size: u64::MAX,
            inserted_at_millis: 0,
            ttl_secs: 1,
            last_accessed: AtomicU64::new(0),
        },
    );
    index.insert(
        "one".to_string(),
        FileIndexEntry {
            path: PathBuf::from("one"),
            data_size: 1,
            inserted_at_millis: 0,
            ttl_secs: 1,
            last_accessed: AtomicU64::new(0),
        },
    );

    assert_eq!(index.total_size(), u64::MAX);

    index.remove("huge");
    assert_eq!(index.total_size(), 1);
    index.remove("one");
    assert_eq!(index.total_size(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_file_index_total_size_matches_entries_after_concurrent_mutations() {
    let index = Arc::new(FileIndex::new());
    let mut handles = Vec::new();

    for worker in 0..32u64 {
        let index = Arc::clone(&index);
        handles.push(tokio::spawn(async move {
            for round in 0..500u64 {
                let key = format!("key_{worker}_{round}");
                index.insert(
                    key.clone(),
                    FileIndexEntry {
                        path: PathBuf::from(&key),
                        data_size: round + 1,
                        inserted_at_millis: 0,
                        ttl_secs: 1,
                        last_accessed: AtomicU64::new(0),
                    },
                );

                if round % 2 == 0 {
                    index.remove(&key);
                }
            }
        }));
    }

    for handle in handles {
        handle.await.expect("mutation task should finish");
    }

    let actual_size = index
        .entries
        .iter()
        .fold(0u64, |total, entry| total.saturating_add(entry.data_size));
    assert_eq!(index.total_size(), actual_size);
}

#[tokio::test]
async fn test_file_backend_get_missing() {
    let (backend, _tmp) = make_backend().await;
    let result = backend
        .get("nonexistent_key_00000000000000000000000000000000000000000000")
        .await;
    assert!(result.is_none());
}

#[tokio::test]
async fn test_file_backend_remove() {
    let (backend, _tmp) = make_backend().await;
    let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    put_entry(&backend, key, b"data").await;
    assert!(backend.contains(key).await);

    backend.remove(key).await;
    assert!(!backend.contains(key).await);

    let result = backend.get(key).await;
    assert!(result.is_none());
}

#[tokio::test]
async fn test_file_backend_remove_nonexistent() {
    let (backend, _tmp) = make_backend().await;
    backend
        .remove("does_not_exist_0000000000000000000000000000000000000000000000")
        .await;
}

#[tokio::test]
async fn test_file_backend_contains() {
    let (backend, _tmp) = make_backend().await;
    let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    assert!(!backend.contains(key).await);

    put_entry(&backend, key, b"data").await;
    assert!(backend.contains(key).await);
}

#[tokio::test]
async fn test_file_backend_directory_structure() {
    let (backend, tmp) = make_backend().await;
    let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    put_entry(&backend, key, b"payload").await;

    let expected_path = tmp.path().join("ab").join("cd").join(key);
    assert!(
        expected_path.exists(),
        "Expected cache file at {}",
        expected_path.display()
    );
}

#[tokio::test]
async fn test_file_backend_atomic_write() {
    let (backend, tmp) = make_backend().await;
    let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    put_entry(&backend, key, b"first").await;
    put_entry(&backend, key, b"second").await;

    let result = backend.get(key).await.expect("should exist");
    assert_eq!(result.data, Bytes::from("second"));

    let mut tmp_entries = fs::read_dir(tmp.path().join(".tmp"))
        .await
        .expect("read .tmp");
    let mut count = 0u64;
    while tmp_entries.next_entry().await.expect("entry").is_some() {
        count += 1;
    }
    assert_eq!(count, 0, "No orphaned temp files should remain");
}

#[tokio::test]
async fn test_file_backend_current_size() {
    let (backend, _tmp) = make_backend().await;
    assert_eq!(backend.current_size(), 0);

    put_entry(
        &backend,
        "aaaa0000000000000000000000000000000000000000000000000000000000aa",
        &[0u8; 100],
    )
    .await;
    assert_eq!(backend.current_size(), 100);

    put_entry(
        &backend,
        "bbbb0000000000000000000000000000000000000000000000000000000000bb",
        &[0u8; 200],
    )
    .await;
    assert_eq!(backend.current_size(), 300);

    backend
        .remove("aaaa0000000000000000000000000000000000000000000000000000000000aa")
        .await;
    assert_eq!(backend.current_size(), 200);
}

#[tokio::test]
async fn test_file_backend_entry_count() {
    let (backend, _tmp) = make_backend().await;
    assert_eq!(backend.entry_count(), 0);

    put_entry(
        &backend,
        "aaaa0000000000000000000000000000000000000000000000000000000000aa",
        b"a",
    )
    .await;
    put_entry(
        &backend,
        "bbbb0000000000000000000000000000000000000000000000000000000000bb",
        b"b",
    )
    .await;
    assert_eq!(backend.entry_count(), 2);
}

#[tokio::test]
async fn test_file_backend_evict_expired() {
    let (backend, _tmp) = make_backend().await;

    let past = SystemTime::now() - Duration::from_secs(10);
    let expired_entry = StoredEntry {
        data: Bytes::from("old_data"),
        inserted_at: past,
        ttl: Duration::from_secs(1),
        last_accessed: past,
    };
    backend
        .put(
            "expired_key_000000000000000000000000000000000000000000000000000000",
            expired_entry,
        )
        .await
        .expect("put expired");

    put_entry(
        &backend,
        "fresh_key_0000000000000000000000000000000000000000000000000000000000",
        b"fresh_data",
    )
    .await;

    assert_eq!(backend.entry_count(), 2);

    let evicted = backend.evict_expired().await;
    assert_eq!(evicted, 1);
    assert_eq!(backend.entry_count(), 1);
    assert!(
        backend
            .contains("fresh_key_0000000000000000000000000000000000000000000000000000000000")
            .await
    );
}

#[tokio::test]
async fn test_file_backend_evict_to_size() {
    let (backend, _tmp) = make_backend().await;

    let t1 = SystemTime::now() - Duration::from_secs(30);
    let t2 = SystemTime::now() - Duration::from_secs(20);
    let t3 = SystemTime::now() - Duration::from_secs(10);

    backend
        .put(
            "oldest_key_0000000000000000000000000000000000000000000000000000000",
            StoredEntry {
                data: Bytes::from(vec![0u8; 100]),
                inserted_at: t1,
                ttl: Duration::from_hours(1),
                last_accessed: t1,
            },
        )
        .await
        .expect("put oldest");

    backend
        .put(
            "middle_key_0000000000000000000000000000000000000000000000000000000",
            StoredEntry {
                data: Bytes::from(vec![0u8; 100]),
                inserted_at: t2,
                ttl: Duration::from_hours(1),
                last_accessed: t2,
            },
        )
        .await
        .expect("put middle");

    backend
        .put(
            "newest_key_0000000000000000000000000000000000000000000000000000000",
            StoredEntry {
                data: Bytes::from(vec![0u8; 100]),
                inserted_at: t3,
                ttl: Duration::from_hours(1),
                last_accessed: t3,
            },
        )
        .await
        .expect("put newest");

    assert_eq!(backend.current_size(), 300);

    let freed = backend.evict_to_size(150).await;
    assert!(
        freed >= 200,
        "Expected at least 200 bytes freed, got {freed}"
    );
    assert!(
        backend.current_size() <= 150,
        "Expected size <= 150, got {}",
        backend.current_size()
    );

    assert!(
        backend
            .contains("newest_key_0000000000000000000000000000000000000000000000000000000")
            .await
    );
}

#[tokio::test]
async fn test_file_backend_load_index() {
    let tmp = TempDir::new().expect("create temp dir");
    let cache_dir = tmp.path().to_path_buf();

    {
        let backend = FileBackend::new(cache_dir.clone(), (2, 2))
            .await
            .expect("create backend");

        put_entry(
            &backend,
            "aaaa0000000000000000000000000000000000000000000000000000000000aa",
            b"hello",
        )
        .await;
        put_entry(
            &backend,
            "bbbb0000000000000000000000000000000000000000000000000000000000bb",
            b"world",
        )
        .await;
    }

    let backend2 = FileBackend::new(cache_dir, (2, 2))
        .await
        .expect("create backend2");
    assert_eq!(backend2.entry_count(), 0, "Fresh backend has empty index");

    let result = backend2
        .load_index(Duration::from_hours(1))
        .await
        .expect("load index");

    assert_eq!(result.loaded, 2);
    assert_eq!(result.errors, 0);
    assert_eq!(result.deleted, 0);
    assert_eq!(result.total_bytes, 10);

    let entry = backend2
        .get("aaaa0000000000000000000000000000000000000000000000000000000000aa")
        .await
        .expect("should exist");
    assert_eq!(entry.data, Bytes::from("hello"));
}

#[tokio::test]
async fn test_file_backend_load_index_deletes_stale() {
    let tmp = TempDir::new().expect("create temp dir");
    let cache_dir = tmp.path().to_path_buf();

    {
        let backend = FileBackend::new(cache_dir.clone(), (2, 2))
            .await
            .expect("create backend");
        let past = SystemTime::now() - Duration::from_secs(100);
        let stale_entry = StoredEntry {
            data: Bytes::from("stale_data"),
            inserted_at: past,
            ttl: Duration::from_secs(1),
            last_accessed: past,
        };
        backend
            .put(
                "stale_key_0000000000000000000000000000000000000000000000000000000",
                stale_entry,
            )
            .await
            .expect("put stale");
    }

    let backend2 = FileBackend::new(cache_dir, (2, 2))
        .await
        .expect("create backend2");
    let result = backend2
        .load_index(Duration::from_secs(10))
        .await
        .expect("load index");

    assert_eq!(result.loaded, 0);
    assert_eq!(result.deleted, 1);
}

#[tokio::test]
async fn test_file_backend_corrupted_file_handled() {
    let tmp = TempDir::new().expect("create temp dir");
    let cache_dir = tmp.path().to_path_buf();

    let garbage_dir = cache_dir.join("ga").join("rb");
    fs::create_dir_all(&garbage_dir).await.expect("create dirs");
    let garbage_path =
        garbage_dir.join("garb0000000000000000000000000000000000000000000000000000000000");
    fs::write(&garbage_path, b"this is not a valid cache file")
        .await
        .expect("write garbage");

    fs::create_dir_all(cache_dir.join(".tmp"))
        .await
        .expect("create .tmp");

    let backend = FileBackend::new(cache_dir, (2, 2))
        .await
        .expect("create backend");
    let result = backend
        .load_index(Duration::from_hours(1))
        .await
        .expect("load index");

    assert_eq!(result.errors, 1);
    assert_eq!(result.loaded, 0);

    assert!(
        !garbage_path.exists(),
        "Corrupted file should have been deleted"
    );
}

#[tokio::test]
async fn test_file_backend_load_index_deletes_size_mismatch() {
    let tmp = TempDir::new().expect("create temp dir");
    let cache_dir = tmp.path().to_path_buf();
    let key = "aaaa0000000000000000000000000000000000000000000000000000000000aa";

    {
        let backend = FileBackend::new(cache_dir.clone(), (2, 2))
            .await
            .expect("create backend");
        put_entry(&backend, key, b"payload").await;
    }

    let cache_file = cache_dir.join("aa").join("aa").join(key);
    let mut bytes = fs::read(&cache_file).await.expect("read cache file");
    bytes.pop().expect("cache file should have data");
    fs::write(&cache_file, bytes)
        .await
        .expect("truncate cache file");

    let backend = FileBackend::new(cache_dir, (2, 2))
        .await
        .expect("create backend");
    let result = backend
        .load_index(Duration::from_hours(1))
        .await
        .expect("load index");

    assert_eq!(result.loaded, 0);
    assert_eq!(result.errors, 1);
    assert!(
        !cache_file.exists(),
        "size-mismatched file should be deleted"
    );
}

#[tokio::test]
async fn test_file_backend_load_index_deletes_invalid_timestamp() {
    let tmp = TempDir::new().expect("create temp dir");
    let cache_dir = tmp.path().to_path_buf();
    let key = "bbbb0000000000000000000000000000000000000000000000000000000000bb";
    let payload = b"payload";
    let cache_file = cache_dir.join("bb").join("bb").join(key);
    fs::create_dir_all(cache_file.parent().expect("cache file parent"))
        .await
        .expect("create cache dirs");
    fs::create_dir_all(cache_dir.join(".tmp"))
        .await
        .expect("create .tmp");

    let header = FileEntryHeader {
        key: key.to_string(),
        inserted_at_millis: u64::MAX,
        ttl_secs: 60,
        last_accessed_millis: 0,
        data_size: payload.len() as u64,
    };
    let header_bytes = encode_header(&header).expect("encode header");
    let mut file_bytes = Vec::new();
    file_bytes.extend_from_slice(CACHE_FILE_MAGIC);
    file_bytes.extend_from_slice(
        &u32::try_from(header_bytes.len())
            .expect("header len fits")
            .to_le_bytes(),
    );
    file_bytes.extend_from_slice(&header_bytes);
    file_bytes.extend_from_slice(payload);
    fs::write(&cache_file, file_bytes)
        .await
        .expect("write cache file");

    let backend = FileBackend::new(cache_dir, (2, 2))
        .await
        .expect("create backend");
    let result = backend
        .load_index(Duration::from_hours(1))
        .await
        .expect("load index");

    assert_eq!(result.loaded, 0);
    assert_eq!(result.errors, 1);
    assert!(
        !cache_file.exists(),
        "invalid timestamp file should be deleted"
    );
}

#[tokio::test]
async fn test_file_backend_load_index_deletes_key_path_mismatch() {
    let tmp = TempDir::new().expect("create temp dir");
    let cache_dir = tmp.path().to_path_buf();
    let key = "aaaa0000000000000000000000000000000000000000000000000000000000aa";
    let wrong_key = "bbbb0000000000000000000000000000000000000000000000000000000000bb";

    {
        let backend = FileBackend::new(cache_dir.clone(), (2, 2))
            .await
            .expect("create backend");
        put_entry(&backend, key, b"payload").await;
    }

    let original_path = cache_dir.join("aa").join("aa").join(key);
    let wrong_path = cache_dir.join("bb").join("bb").join(wrong_key);
    fs::create_dir_all(wrong_path.parent().expect("wrong path parent"))
        .await
        .expect("create wrong path parent");
    fs::rename(&original_path, &wrong_path)
        .await
        .expect("move cache file to wrong path");

    let backend = FileBackend::new(cache_dir, (2, 2))
        .await
        .expect("create backend");
    let result = backend
        .load_index(Duration::from_hours(1))
        .await
        .expect("load index");

    assert_eq!(result.loaded, 0);
    assert_eq!(result.errors, 1);
    assert!(
        !wrong_path.exists(),
        "key/path mismatched file should be deleted"
    );
}

#[tokio::test]
async fn test_file_backend_keys() {
    let (backend, _tmp) = make_backend().await;

    put_entry(
        &backend,
        "aaaa0000000000000000000000000000000000000000000000000000000000aa",
        b"data_a",
    )
    .await;
    put_entry(
        &backend,
        "bbbb0000000000000000000000000000000000000000000000000000000000bb",
        b"data_b",
    )
    .await;

    let mut keys = backend.keys().await;
    keys.sort();

    assert_eq!(keys.len(), 2);
    assert_eq!(
        keys[0],
        "aaaa0000000000000000000000000000000000000000000000000000000000aa"
    );
    assert_eq!(
        keys[1],
        "bbbb0000000000000000000000000000000000000000000000000000000000bb"
    );
}

#[tokio::test]
async fn test_file_backend_cleanup_temp_files() {
    let (backend, tmp) = make_backend().await;

    let tmp_dir = tmp.path().join(".tmp");
    let orphan = tmp_dir.join("tmp_orphaned_file");
    fs::write(&orphan, b"orphan data")
        .await
        .expect("write orphan");

    backend.cleanup_temp_files().await;

    assert!(orphan.exists(), "Fresh temp file should not be cleaned up");
}

#[tokio::test]
async fn test_file_backend_overwrite_updates_size() {
    let (backend, _tmp) = make_backend().await;
    let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    put_entry(&backend, key, &[0u8; 100]).await;
    assert_eq!(backend.current_size(), 100);

    put_entry(&backend, key, &[0u8; 50]).await;
    assert_eq!(backend.current_size(), 50);
}

#[tokio::test]
async fn test_file_backend_empty_data() {
    let (backend, _tmp) = make_backend().await;
    let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    put_entry(&backend, key, b"").await;
    let result = backend.get(key).await.expect("should exist");
    assert_eq!(result.data, Bytes::new());
    assert_eq!(backend.current_size(), 0);
}

#[tokio::test]
async fn test_file_backend_get_returns_stored_entry_fields() {
    let (backend, _tmp) = make_backend().await;
    let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    let ttl = Duration::from_mins(10);

    let entry = StoredEntry::new(Bytes::from("test data"), ttl);
    backend.put(key, entry).await.expect("put");

    let got = backend.get(key).await.expect("should exist");
    assert_eq!(got.data, Bytes::from("test data"));
    assert_eq!(got.ttl.as_secs(), 600);
    assert!(
        got.inserted_at.elapsed().unwrap_or_default() < Duration::from_secs(5),
        "inserted_at should be recent"
    );
}

#[tokio::test]
async fn test_file_backend_persist_access_times() {
    let tmp = TempDir::new().expect("create temp dir");
    let cache_dir = tmp.path().to_path_buf();

    let key_a = "aaaa0000000000000000000000000000000000000000000000000000000000aa";
    let key_b = "bbbb0000000000000000000000000000000000000000000000000000000000bb";

    {
        let backend = FileBackend::new(cache_dir.clone(), (2, 2))
            .await
            .expect("create backend");
        put_entry(&backend, key_a, b"data_a").await;
        put_entry(&backend, key_b, b"data_b").await;

        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = backend.get(key_b).await.expect("get key_b");

        backend.persist_access_times().await;
    }

    {
        let backend2 = FileBackend::new(cache_dir, (2, 2))
            .await
            .expect("create backend2");
        backend2
            .load_index(Duration::from_hours(1))
            .await
            .expect("load index");

        let entry_a = backend2.index.entries.get(key_a).expect("key_a in index");
        let entry_b = backend2.index.entries.get(key_b).expect("key_b in index");

        let accessed_a = entry_a.last_accessed.load(Ordering::Relaxed);
        let accessed_b = entry_b.last_accessed.load(Ordering::Relaxed);

        assert!(
            accessed_b > accessed_a,
            "key_b should have a more recent last_accessed than key_a \
             (key_a={accessed_a}, key_b={accessed_b})"
        );
    }
}

#[tokio::test]
async fn test_file_backend_persist_access_times_no_corruption() {
    let (backend, _tmp) = make_backend().await;
    let key = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    put_entry(&backend, key, b"important data").await;

    let _ = backend.get(key).await;

    backend.persist_access_times().await;

    let got = backend.get(key).await.expect("should still exist");
    assert_eq!(got.data, Bytes::from("important data"));
}
