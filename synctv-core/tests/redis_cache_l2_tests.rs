//! `RedisCacheL2` integration tests
//!
//! Tests `set_if_newer` (absent key, newer wins, older rejected, concurrent)
//! and namespaced prefix invalidation/index maintenance.
//!
#![allow(clippy::unwrap_used)]

use redis::AsyncCommands;
use synctv_core::cache::l2_backend::VersionedFenceRead;
use synctv_core::cache::{CacheL2Backend, RedisCacheL2};
use synctv_core_testing::start_redis as start_test_redis;

fn ts_millis(ts: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(ts)
        .expect("test timestamp should parse")
        .timestamp_millis()
}

fn assert_stored_name(stored: Option<String>, expected_name: &str, expected_ts: i64) {
    let value: serde_json::Value =
        serde_json::from_str(&stored.expect("cache value should exist")).unwrap();
    assert_eq!(value["name"], expected_name);
    assert_eq!(value["updated_at_ms"], expected_ts);
}

async fn start_redis() -> (
    synctv_core_testing::RedisContainer,
    redis::aio::ConnectionManager,
) {
    start_test_redis().await
}

// set_if_newer tests

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_newer_absent_key() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn));

    let key = "test:sin:absent";
    let json = r#"{"name":"alice","updated_at":"2024-01-01T12:00:00Z"}"#;
    let ts = ts_millis("2024-01-01T12:00:00Z");

    // Setting an absent key should succeed
    let was_set = l2.set_if_newer(key, json, 300, ts).await.unwrap();
    assert!(was_set, "set_if_newer should succeed for absent key");

    // Verify it was actually stored
    let stored = l2.get(key).await.unwrap();
    assert_stored_name(stored, "alice", ts);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_newer_newer_wins() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn));

    let key = "test:sin:newer_wins";
    let old_json = r#"{"name":"alice_old","updated_at":"2024-01-01T12:00:00Z"}"#;
    let old_ts = ts_millis("2024-01-01T12:00:00Z");
    let new_json = r#"{"name":"alice_new","updated_at":"2024-06-15T12:00:00Z"}"#;
    let new_ts = ts_millis("2024-06-15T12:00:00Z");

    // Set the old value first
    l2.set_if_newer(key, old_json, 300, old_ts).await.unwrap();

    // Set a newer value - should succeed
    let was_set = l2.set_if_newer(key, new_json, 300, new_ts).await.unwrap();
    assert!(was_set, "Newer value should overwrite older value");

    let stored = l2.get(key).await.unwrap();
    assert_stored_name(stored, "alice_new", new_ts);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_newer_older_rejected() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn));

    let key = "test:sin:older_rejected";
    let new_json = r#"{"name":"alice_new","updated_at":"2024-06-15T12:00:00Z"}"#;
    let new_ts = ts_millis("2024-06-15T12:00:00Z");
    let old_json = r#"{"name":"alice_old","updated_at":"2024-01-01T12:00:00Z"}"#;
    let old_ts = ts_millis("2024-01-01T12:00:00Z");

    // Set the newer value first
    l2.set_if_newer(key, new_json, 300, new_ts).await.unwrap();

    // Try to set an older value - should be rejected
    let was_set = l2.set_if_newer(key, old_json, 300, old_ts).await.unwrap();
    assert!(!was_set, "Older value should be rejected by set_if_newer");

    // Value should still be the newer one
    let stored = l2.get(key).await.unwrap();
    assert_stored_name(stored, "alice_new", new_ts);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_newer_rejects_older_value_after_normal_set() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn));

    let key = "test:sin:normal_set_then_older_rejected";
    let new_json = r#"{"name":"alice_new","updated_at":"2024-06-15T12:00:00Z"}"#;
    let new_ts = ts_millis("2024-06-15T12:00:00Z");
    let old_json = r#"{"name":"alice_old","updated_at":"2024-01-01T12:00:00Z"}"#;
    let old_ts = ts_millis("2024-01-01T12:00:00Z");

    l2.set(key, new_json, 300).await.unwrap();

    let was_set = l2.set_if_newer(key, old_json, 300, old_ts).await.unwrap();
    assert!(
        !was_set,
        "Older value should be rejected after a normal L2 set"
    );

    let stored = l2.get(key).await.unwrap();
    assert_stored_name(stored, "alice_new", new_ts);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_newer_rejects_existing_timestamped_value_without_numeric_epoch() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn.clone()));

    let key = "test:sin:missing_epoch_rejected";
    let existing_json = r#"{"name":"alice_existing","updated_at":"2024-06-15T12:00:00Z"}"#;
    let old_json = r#"{"name":"alice_old","updated_at":"2024-01-01T12:00:00Z"}"#;
    let old_ts = ts_millis("2024-01-01T12:00:00Z");

    let mut raw_conn = conn;
    raw_conn
        .set_ex::<_, _, ()>(key, existing_json, 300)
        .await
        .unwrap();

    let was_set = l2.set_if_newer(key, old_json, 300, old_ts).await.unwrap();
    assert!(
        !was_set,
        "Existing timestamped values without updated_at_ms must fail closed"
    );

    let stored = l2.get(key).await.unwrap().unwrap();
    assert!(
        stored.contains("alice_existing"),
        "non-normalized existing value should remain untouched; got {stored}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_newer_concurrent() {
    let (_container, conn) = start_redis().await;
    let l2 = std::sync::Arc::new(RedisCacheL2::from_runtime(synctv_core::direct_runtime(
        conn,
    )));

    let key = "test:sin:concurrent";

    // Spawn multiple concurrent set_if_newer with different timestamps
    let mut handles = Vec::new();
    for i in 0..10 {
        let l2_clone = l2.clone();
        let json = format!(
            r#"{{"name":"worker_{i}","updated_at":"2024-{:02}-15T12:00:00Z"}}"#,
            i + 1
        );
        let ts = ts_millis(&format!("2024-{:02}-15T12:00:00Z", i + 1));
        let k = key.to_string();
        handles.push(tokio::spawn(async move {
            l2_clone.set_if_newer(&k, &json, 300, ts).await
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_version_uses_domain_version_when_cache_version_missing() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn.clone()));

    let key = "test:siv:domain_version_fallback";
    let mut raw_conn = conn;
    raw_conn
        .set_ex::<_, _, ()>(key, r#"{"name":"existing","version":5}"#, 300)
        .await
        .unwrap();

    let stale = l2
        .set_if_version_at_least(key, r#"{"name":"stale","version":4}"#, 300, 4)
        .await
        .unwrap();
    assert!(
        !stale,
        "write older than the existing domain version should be rejected"
    );
    let stored = l2.get(key).await.unwrap().unwrap();
    assert!(
        stored.contains("existing"),
        "existing version-5 value should remain after stale write; got {stored}"
    );

    let fresh = l2
        .set_if_version_at_least(key, r#"{"name":"fresh","version":6}"#, 300, 6)
        .await
        .unwrap();
    assert!(
        fresh,
        "write newer than the existing domain version should be accepted"
    );
    let value: serde_json::Value = serde_json::from_str(&l2.get(key).await.unwrap().unwrap())
        .expect("stored value should be JSON");
    assert_eq!(value["name"], "fresh");
    assert_eq!(value["cache_version"], 6);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_version_overwrites_unversioned_json() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn.clone()));

    let key = "test:siv:unversioned_json";
    let mut raw_conn = conn;
    raw_conn
        .set_ex::<_, _, ()>(key, r#"{"name":"unversioned"}"#, 300)
        .await
        .unwrap();

    let was_set = l2
        .set_if_version_at_least(key, r#"{"name":"versioned","version":1}"#, 300, 1)
        .await
        .unwrap();
    assert!(
        was_set,
        "unversioned JSON must not permanently block versioned cache writes"
    );

    let value: serde_json::Value = serde_json::from_str(&l2.get(key).await.unwrap().unwrap())
        .expect("stored value should be JSON");
    assert_eq!(value["name"], "versioned");
    assert_eq!(value["cache_version"], 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_versioned_fence_read_uses_l1_when_version_is_current() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn.clone()));
    let mut raw_conn = conn;

    let fence_key = "test:vfr:l1:fence";
    let cache_key = "test:vfr:l1:value";
    raw_conn.set::<_, _, ()>(fence_key, 7_i64).await.unwrap();
    raw_conn
        .set_ex::<_, _, ()>(cache_key, r#"{"name":"l2","version":7}"#, 300)
        .await
        .unwrap();

    let decision = l2
        .read_versioned_with_l1_by_fence(fence_key, cache_key, 7)
        .await
        .unwrap();

    assert_eq!(decision, VersionedFenceRead::UseL1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_versioned_fence_read_returns_l2_when_l1_is_stale() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn.clone()));
    let mut raw_conn = conn;

    let fence_key = "test:vfr:l2:fence";
    let cache_key = "test:vfr:l2:value";
    raw_conn.set::<_, _, ()>(fence_key, 8_i64).await.unwrap();
    raw_conn
        .set_ex::<_, _, ()>(
            cache_key,
            r#"{"name":"fresh-l2","version":8,"cache_version":8}"#,
            300,
        )
        .await
        .unwrap();

    let decision = l2
        .read_versioned_with_l1_by_fence(fence_key, cache_key, 7)
        .await
        .unwrap();

    match decision {
        VersionedFenceRead::UseL2(json) => {
            let value: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(value["name"], "fresh-l2");
            assert_eq!(value["cache_version"], 8);
        }
        other => panic!("expected L2 decision, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_versioned_fence_l2_only_read_returns_payload_when_fresh() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn.clone()));
    let mut raw_conn = conn;

    let fence_key = "test:vfr:l2_only:fence";
    let cache_key = "test:vfr:l2_only:value";
    raw_conn.set::<_, _, ()>(fence_key, 9_i64).await.unwrap();
    raw_conn
        .set_ex::<_, _, ()>(cache_key, r#"{"name":"l2-only","version":9}"#, 300)
        .await
        .unwrap();

    let json = l2
        .read_versioned_l2_by_fence(fence_key, cache_key)
        .await
        .unwrap()
        .expect("fresh L2 payload should be returned");
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["name"], "l2-only");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_versioned_fence_read_fails_closed_when_write_pending() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn.clone()));
    let mut raw_conn = conn;

    let fence_key = "test:vfr:pending:fence";
    let pending_key = format!("{fence_key}:pending");
    let cache_key = "test:vfr:pending:value";
    raw_conn.set::<_, _, ()>(fence_key, 10_i64).await.unwrap();
    raw_conn
        .hset::<_, _, _, ()>(&pending_key, "version", 11_i64)
        .await
        .unwrap();
    raw_conn
        .set_ex::<_, _, ()>(cache_key, r#"{"name":"stale","version":11}"#, 300)
        .await
        .unwrap();

    let decision = l2
        .read_versioned_with_l1_by_fence(fence_key, cache_key, 11)
        .await
        .unwrap();
    assert_eq!(decision, VersionedFenceRead::DbFallback);

    let l2_only = l2
        .read_versioned_l2_by_fence(fence_key, cache_key)
        .await
        .unwrap();
    assert!(l2_only.is_none());
}

// delete_by_prefix tests

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_by_prefix_100_plus_keys() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn.clone()));

    // Insert 150 keys with the same prefix
    let prefix = "test:dbp:batch:";
    for i in 0..150 {
        let key = format!("{prefix}key_{i}");
        l2.set_scoped(prefix, &key, &format!("value_{i}"), 300)
            .await
            .unwrap();
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
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn.clone()));

    let prefix_a = "test:dbp:iso_a:";
    let prefix_b = "test:dbp:iso_b:";

    // Insert keys under both prefixes
    for i in 0..10 {
        l2.set_scoped(
            prefix_a,
            &format!("{prefix_a}key_{i}"),
            &format!("a_{i}"),
            300,
        )
        .await
        .unwrap();
        l2.set_scoped(
            prefix_b,
            &format!("{prefix_b}key_{i}"),
            &format!("b_{i}"),
            300,
        )
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_scoped_prunes_missing_namespace_index_member() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn.clone()));

    let prefix = "test:dbp:prune:";
    let missing_key = format!("{prefix}ghost");
    let index_key = format!("{prefix}__l2_index");
    let mut raw = conn;

    let _: () = redis::cmd("ZADD")
        .arg(&index_key)
        .arg(9_999_999_999_i64)
        .arg(&missing_key)
        .query_async(&mut raw)
        .await
        .unwrap();

    let result = l2.get_scoped(prefix, &missing_key).await.unwrap();
    assert!(result.is_none(), "missing key should still read as None");

    let members: Vec<String> = raw.zrange(&index_key, 0, -1).await.unwrap();
    assert!(
        members.is_empty(),
        "missing namespace index member should be pruned on scoped get"
    );
}
