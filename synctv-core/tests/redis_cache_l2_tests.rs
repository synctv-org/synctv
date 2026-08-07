//! `RedisCacheL2` integration tests
//!
//! Tests `set_if_newer` (absent key, newer wins, older rejected, concurrent)
//! and namespaced prefix invalidation/index maintenance.
//!

use redis::AsyncCommands;
use serde::Serialize;
use synctv_core::cache::l2_backend::VersionedFenceRead;
use synctv_core::cache::{CacheL2Backend, RedisCacheL2};
use synctv_core_testing::start_redis as start_test_redis;
use synctv_core_testing::{
    ok, some, timestamped_l2_envelope, unversioned_l2_envelope, versioned_l2_envelope,
    TestOptionExt, TestResultExt,
};

fn ts_millis(ts: &str) -> i64 {
    ok(
        chrono::DateTime::parse_from_rfc3339(ts),
        "test timestamp should parse",
    )
    .timestamp_millis()
}

fn assert_stored_name(stored: Option<String>, expected_name: &str, expected_ts: i64) {
    let stored = some(stored, "cache value should exist");
    let value: serde_json::Value = ok(serde_json::from_str(&stored), "stored value should be JSON");
    assert_eq!(value["payload"]["name"], expected_name);
    assert_eq!(value["updatedAtMs"], expected_ts);
}

fn json_value(json: &str) -> serde_json::Value {
    ok(serde_json::from_str(json), "stored value should be JSON")
}

#[derive(Serialize)]
struct CacheNamePayload<'a> {
    name: &'a str,
}

#[derive(Serialize)]
struct TimestampedCacheNamePayload<'a> {
    name: &'a str,
    updated_at: &'a str,
}

#[derive(Serialize)]
struct VersionedCacheNamePayload<'a> {
    name: &'a str,
    version: i64,
}

fn timestamped_payload(name: &str, updated_at: &str) -> (String, i64) {
    let updated_at_ms = ts_millis(updated_at);
    (
        timestamped_l2_envelope(
            TimestampedCacheNamePayload { name, updated_at },
            updated_at_ms,
        ),
        updated_at_ms,
    )
}

fn versioned_payload(name: &str, version: i64) -> String {
    versioned_l2_envelope(VersionedCacheNamePayload { name, version }, version)
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
    let (json, ts) = timestamped_payload("alice", "2024-01-01T12:00:00Z");

    // Setting an absent key should succeed
    let was_set = l2
        .set_if_newer(key, &json, 300, ts)
        .await
        .checked("test operation should succeed");
    assert!(was_set, "set_if_newer should succeed for absent key");

    // Verify it was actually stored
    let stored = l2.get(key).await.checked("test operation should succeed");
    assert_stored_name(stored, "alice", ts);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_newer_newer_wins() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn));

    let key = "test:sin:newer_wins";
    let (old_json, old_ts) = timestamped_payload("alice_old", "2024-01-01T12:00:00Z");
    let (new_json, new_ts) = timestamped_payload("alice_new", "2024-06-15T12:00:00Z");

    // Set the old value first
    l2.set_if_newer(key, &old_json, 300, old_ts)
        .await
        .checked("test operation should succeed");

    // Set a newer value - should succeed
    let was_set = l2
        .set_if_newer(key, &new_json, 300, new_ts)
        .await
        .checked("test operation should succeed");
    assert!(was_set, "Newer value should overwrite older value");

    let stored = l2.get(key).await.checked("test operation should succeed");
    assert_stored_name(stored, "alice_new", new_ts);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_newer_older_rejected() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn));

    let key = "test:sin:older_rejected";
    let (new_json, new_ts) = timestamped_payload("alice_new", "2024-06-15T12:00:00Z");
    let (old_json, old_ts) = timestamped_payload("alice_old", "2024-01-01T12:00:00Z");

    // Set the newer value first
    l2.set_if_newer(key, &new_json, 300, new_ts)
        .await
        .checked("test operation should succeed");

    // Try to set an older value - should be rejected
    let was_set = l2
        .set_if_newer(key, &old_json, 300, old_ts)
        .await
        .checked("test operation should succeed");
    assert!(!was_set, "Older value should be rejected by set_if_newer");

    // Value should still be the newer one
    let stored = l2.get(key).await.checked("test operation should succeed");
    assert_stored_name(stored, "alice_new", new_ts);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_newer_rejects_older_value_after_normal_set() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn));

    let key = "test:sin:normal_set_then_older_rejected";
    let (new_json, new_ts) = timestamped_payload("alice_new", "2024-06-15T12:00:00Z");
    let (old_json, old_ts) = timestamped_payload("alice_old", "2024-01-01T12:00:00Z");

    l2.set(key, &new_json, 300)
        .await
        .checked("test operation should succeed");

    let was_set = l2
        .set_if_newer(key, &old_json, 300, old_ts)
        .await
        .checked("test operation should succeed");
    assert!(
        !was_set,
        "Older value should be rejected after a normal L2 set"
    );

    let stored = l2.get(key).await.checked("test operation should succeed");
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
        .checked("existing value should be written");

    let was_set = l2
        .set_if_newer(key, old_json, 300, old_ts)
        .await
        .checked("older value should be rejected cleanly");
    assert!(
        !was_set,
        "Existing timestamped values without updatedAtMs must fail closed"
    );

    let stored = l2
        .get(key)
        .await
        .checked("cache value should be read")
        .checked("cache value should exist");
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
        let updated_at = format!("2024-{:02}-15T12:00:00Z", i + 1);
        let (json, ts) = timestamped_payload(&format!("worker_{i}"), &updated_at);
        let k = key.to_string();
        handles.push(tokio::spawn(async move {
            l2_clone.set_if_newer(&k, &json, 300, ts).await
        }));
    }

    let _results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| {
            r.checked("worker task should complete")
                .checked("worker write should succeed")
        })
        .collect();

    // The final value should have the newest timestamp (month 10)
    let stored = l2
        .get(key)
        .await
        .checked("cache value should be read")
        .checked("cache value should exist");
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
    let existing_json = versioned_payload("existing", 5);
    let mut raw_conn = conn;
    raw_conn
        .set_ex::<_, _, ()>(key, existing_json, 300)
        .await
        .checked("existing version should be written");

    let stale_json = versioned_payload("stale", 4);
    let stale = l2
        .set_if_version_at_least(key, &stale_json, 300, 4)
        .await
        .checked("stale version write should complete");
    assert!(
        !stale,
        "write older than the existing domain version should be rejected"
    );
    let stored = l2
        .get(key)
        .await
        .checked("cache value should be read")
        .checked("cache value should exist");
    assert!(
        stored.contains("existing"),
        "existing version-5 value should remain after stale write; got {stored}"
    );

    let fresh_json = versioned_payload("fresh", 6);
    let fresh = l2
        .set_if_version_at_least(key, &fresh_json, 300, 6)
        .await
        .checked("fresh version write should complete");
    assert!(
        fresh,
        "write newer than the existing domain version should be accepted"
    );
    let stored = l2
        .get(key)
        .await
        .checked("cache value should be read")
        .checked("cache value should exist");
    let value = json_value(&stored);
    assert_eq!(value["payload"]["name"], "fresh");
    assert_eq!(value["cacheVersion"], 6);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_if_version_overwrites_unversioned_json() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn.clone()));

    let key = "test:siv:unversioned_json";
    let mut raw_conn = conn;
    let unversioned_json = unversioned_l2_envelope(CacheNamePayload {
        name: "unversioned",
    });
    raw_conn
        .set_ex::<_, _, ()>(key, unversioned_json, 300)
        .await
        .checked("unversioned value should be written");

    let versioned_json = versioned_payload("versioned", 1);
    let was_set = l2
        .set_if_version_at_least(key, &versioned_json, 300, 1)
        .await
        .checked("versioned write should complete");
    assert!(
        was_set,
        "unversioned JSON must not permanently block versioned cache writes"
    );

    let stored = l2
        .get(key)
        .await
        .checked("cache value should be read")
        .checked("cache value should exist");
    let value = json_value(&stored);
    assert_eq!(value["payload"]["name"], "versioned");
    assert_eq!(value["cacheVersion"], 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_versioned_fence_read_uses_l1_when_version_is_current() {
    let (_container, conn) = start_redis().await;
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(conn.clone()));
    let mut raw_conn = conn;

    let fence_key = "test:vfr:l1:fence";
    let cache_key = "test:vfr:l1:value";
    raw_conn
        .set::<_, _, ()>(fence_key, 7_i64)
        .await
        .checked("fence version should be written");
    raw_conn
        .set_ex::<_, _, ()>(cache_key, versioned_payload("l2", 7), 300)
        .await
        .checked("cache payload should be written");

    let decision = l2
        .read_versioned_with_l1_by_fence(fence_key, cache_key, 7)
        .await
        .checked("versioned read should complete");

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
    raw_conn
        .set::<_, _, ()>(fence_key, 8_i64)
        .await
        .checked("fence version should be written");
    raw_conn
        .set_ex::<_, _, ()>(cache_key, versioned_payload("fresh-l2", 8), 300)
        .await
        .checked("cache payload should be written");

    let decision = l2
        .read_versioned_with_l1_by_fence(fence_key, cache_key, 7)
        .await
        .checked("versioned read should complete");

    match decision {
        VersionedFenceRead::UseL2(json) => {
            let value = json_value(&json);
            assert_eq!(value["payload"]["name"], "fresh-l2");
            assert_eq!(value["cacheVersion"], 8);
        }
        other => std::panic::panic_any(format!("expected L2 decision, got {other:?}")),
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
    raw_conn
        .set::<_, _, ()>(fence_key, 9_i64)
        .await
        .checked("fence version should be written");
    raw_conn
        .set_ex::<_, _, ()>(cache_key, versioned_payload("l2-only", 9), 300)
        .await
        .checked("cache payload should be written");

    let json = l2
        .read_versioned_l2_by_fence(fence_key, cache_key)
        .await
        .checked("versioned read should complete")
        .checked("fresh L2 payload should be returned");
    let value = json_value(&json);
    assert_eq!(value["payload"]["name"], "l2-only");
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
    raw_conn
        .set::<_, _, ()>(fence_key, 10_i64)
        .await
        .checked("fence version should be written");
    raw_conn
        .hset::<_, _, _, ()>(&pending_key, "version", 11_i64)
        .await
        .checked("pending version should be written");
    raw_conn
        .set_ex::<_, _, ()>(cache_key, versioned_payload("stale", 11), 300)
        .await
        .checked("cache payload should be written");

    let decision = l2
        .read_versioned_with_l1_by_fence(fence_key, cache_key, 11)
        .await
        .checked("versioned read should complete");
    assert_eq!(decision, VersionedFenceRead::DbFallback);

    let l2_only = l2
        .read_versioned_l2_by_fence(fence_key, cache_key)
        .await
        .checked("versioned L2 read should complete");
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
        let value = format!(r#"{{"value":"value_{i}"}}"#);
        l2.set_scoped(prefix, &key, &value, 300)
            .await
            .checked("test value should exist");
    }

    // Verify some keys exist
    let val = l2
        .get(&format!("{prefix}key_0"))
        .await
        .checked("test operation should succeed");
    assert!(val.is_some(), "Key should exist before delete_by_prefix");

    // Delete by prefix
    l2.delete_by_prefix(prefix)
        .await
        .checked("test operation should succeed");

    // All keys with this prefix should be gone
    for i in 0..150 {
        let key = format!("{prefix}key_{i}");
        let val = l2.get(&key).await.checked("test operation should succeed");
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
            &format!(r#"{{"value":"a_{i}"}}"#),
            300,
        )
        .await
        .checked("test value should exist");
        l2.set_scoped(
            prefix_b,
            &format!("{prefix_b}key_{i}"),
            &format!(r#"{{"value":"b_{i}"}}"#),
            300,
        )
        .await
        .checked("test value should exist");
    }

    // Delete only prefix_a
    l2.delete_by_prefix(prefix_a)
        .await
        .checked("test operation should succeed");

    // prefix_a keys should be gone
    for i in 0..10 {
        let val = l2
            .get(&format!("{prefix_a}key_{i}"))
            .await
            .checked("test operation should succeed");
        assert!(val.is_none(), "prefix_a key should be deleted");
    }

    // prefix_b keys should still exist
    for i in 0..10 {
        let val = l2
            .get(&format!("{prefix_b}key_{i}"))
            .await
            .checked("test operation should succeed");
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
        .checked("test value should exist");

    let result = l2
        .get_scoped(prefix, &missing_key)
        .await
        .checked("test operation should succeed");
    assert!(result.is_none(), "missing key should still read as None");

    let members: Vec<String> = raw
        .zrange(&index_key, 0, -1)
        .await
        .checked("test operation should succeed");
    assert!(
        members.is_empty(),
        "missing namespace index member should be pruned on scoped get"
    );
}
