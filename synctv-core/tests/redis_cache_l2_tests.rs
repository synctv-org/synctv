//! `RedisCacheL2` integration tests
//!
//! Tests `set_if_newer` (absent key, newer wins, older rejected, concurrent)
//! and `delete_by_prefix` (100+ keys with SCAN pagination, prefix isolation).
//!
//! Run with: cargo test --test `redis_cache_l2_tests`
#![allow(clippy::unwrap_used)]

use synctv_core::cache::{CacheL2Backend, RedisCacheL2};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

async fn start_redis() -> (
    testcontainers::ContainerAsync<Redis>,
    redis::aio::ConnectionManager,
) {
    let container = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Redis::default().start(),
    )
    .await
    .expect("Docker container startup timed out (is Docker running?)")
    .expect("Failed to start Redis");
    let port = container
        .get_host_port_ipv4(6379)
        .await
        .expect("Failed to get port");
    let redis_url = format!("redis://127.0.0.1:{port}");
    let client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let conn = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to create connection manager");
    (container, conn)
}

// ============================================================================
// set_if_newer tests
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_newer_absent_key() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::new(conn);

    let key = "test:sin:absent";
    let json = r#"{"name":"alice","updated_at":"2024-01-01T12:00:00Z"}"#;
    let ts = "2024-01-01T12:00:00Z";

    // Setting an absent key should succeed
    let was_set = l2.set_if_newer(key, json, 300, ts).await.unwrap();
    assert!(was_set, "set_if_newer should succeed for absent key");

    // Verify it was actually stored
    let stored = l2.get(key).await.unwrap();
    assert_eq!(stored.as_deref(), Some(json));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_newer_newer_wins() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::new(conn);

    let key = "test:sin:newer_wins";
    let old_json = r#"{"name":"alice_old","updated_at":"2024-01-01T12:00:00Z"}"#;
    let old_ts = "2024-01-01T12:00:00Z";
    let new_json = r#"{"name":"alice_new","updated_at":"2024-06-15T12:00:00Z"}"#;
    let new_ts = "2024-06-15T12:00:00Z";

    // Set the old value first
    l2.set_if_newer(key, old_json, 300, old_ts).await.unwrap();

    // Set a newer value - should succeed
    let was_set = l2.set_if_newer(key, new_json, 300, new_ts).await.unwrap();
    assert!(was_set, "Newer value should overwrite older value");

    let stored = l2.get(key).await.unwrap();
    assert_eq!(stored.as_deref(), Some(new_json));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_newer_older_rejected() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::new(conn);

    let key = "test:sin:older_rejected";
    let new_json = r#"{"name":"alice_new","updated_at":"2024-06-15T12:00:00Z"}"#;
    let new_ts = "2024-06-15T12:00:00Z";
    let old_json = r#"{"name":"alice_old","updated_at":"2024-01-01T12:00:00Z"}"#;
    let old_ts = "2024-01-01T12:00:00Z";

    // Set the newer value first
    l2.set_if_newer(key, new_json, 300, new_ts).await.unwrap();

    // Try to set an older value - should be rejected
    let was_set = l2.set_if_newer(key, old_json, 300, old_ts).await.unwrap();
    assert!(!was_set, "Older value should be rejected by set_if_newer");

    // Value should still be the newer one
    let stored = l2.get(key).await.unwrap();
    assert_eq!(stored.as_deref(), Some(new_json));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_newer_concurrent() {
    let (_container, conn) = start_redis().await;
    let l2 = std::sync::Arc::new(RedisCacheL2::new(conn));

    let key = "test:sin:concurrent";

    // Spawn multiple concurrent set_if_newer with different timestamps
    let mut handles = Vec::new();
    for i in 0..10 {
        let l2_clone = l2.clone();
        let json = format!(
            r#"{{"name":"worker_{i}","updated_at":"2024-{:02}-15T12:00:00Z"}}"#,
            i + 1
        );
        let ts = format!("2024-{:02}-15T12:00:00Z", i + 1);
        let k = key.to_string();
        handles.push(tokio::spawn(async move {
            l2_clone.set_if_newer(&k, &json, 300, &ts).await
        }));
    }

    let _results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap().unwrap())
        .collect();

    // The final value should have the newest timestamp (month 10)
    let stored = l2.get(key).await.unwrap().unwrap();
    assert!(
        stored.contains("worker_9"),
        "Concurrent set_if_newer should resolve to the newest value; got: {stored}"
    );
}

// ============================================================================
// delete_by_prefix tests
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_by_prefix_100_plus_keys() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::new(conn.clone());

    // Insert 150 keys with the same prefix
    let prefix = "test:dbp:batch:";
    for i in 0..150 {
        let key = format!("{prefix}key_{i}");
        l2.set(&key, &format!("value_{i}"), 300).await.unwrap();
    }

    // Verify some keys exist
    let val = l2.get(&format!("{prefix}key_0")).await.unwrap();
    assert!(val.is_some(), "Key should exist before delete_by_prefix");

    // Delete by prefix
    l2.delete_by_prefix(prefix).await.unwrap();

    // All keys with this prefix should be gone
    for i in 0..150 {
        let key = format!("{prefix}key_{i}");
        let val = l2.get(&key).await.unwrap();
        assert!(
            val.is_none(),
            "Key {key} should have been deleted by delete_by_prefix"
        );
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_by_prefix_isolation() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::new(conn.clone());

    let prefix_a = "test:dbp:iso_a:";
    let prefix_b = "test:dbp:iso_b:";

    // Insert keys under both prefixes
    for i in 0..10 {
        l2.set(&format!("{prefix_a}key_{i}"), &format!("a_{i}"), 300)
            .await
            .unwrap();
        l2.set(&format!("{prefix_b}key_{i}"), &format!("b_{i}"), 300)
            .await
            .unwrap();
    }

    // Delete only prefix_a
    l2.delete_by_prefix(prefix_a).await.unwrap();

    // prefix_a keys should be gone
    for i in 0..10 {
        let val = l2.get(&format!("{prefix_a}key_{i}")).await.unwrap();
        assert!(val.is_none(), "prefix_a key should be deleted");
    }

    // prefix_b keys should still exist
    for i in 0..10 {
        let val = l2.get(&format!("{prefix_b}key_{i}")).await.unwrap();
        assert!(
            val.is_some(),
            "prefix_b key should NOT be deleted by prefix_a deletion"
        );
    }
}
