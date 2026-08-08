//! `OAuth2` Redis state store integration tests
//!
//! Tests the `RedisOAuthStateStore`: store, consume, TTL expiry,
//! and atomic single-use consumption under concurrency.
//!

use redis::AsyncCommands;
use std::sync::Arc;
use synctv_core::oauth2::OAuth2AuthorizationMode;
use synctv_core::service::{OAuth2Operation, OAuth2State, OAuthStateStore, RedisOAuthStateStore};
use synctv_core_testing::{ok, some, start_redis as start_test_redis, test_redis_key_prefix};
use tokio::sync::RwLock;

async fn start_redis() -> (
    synctv_core_testing::RedisContainer,
    redis::aio::ConnectionManager,
) {
    start_test_redis().await
}

fn prefixed_store(
    conn: redis::aio::ConnectionManager,
    label: &str,
) -> (
    RedisOAuthStateStore,
    Arc<RwLock<redis::aio::ConnectionManager>>,
    String,
) {
    let shared_conn = Arc::new(RwLock::new(conn));
    let key_prefix = test_redis_key_prefix(label);
    let store = RedisOAuthStateStore::from_runtime(
        synctv_core::shared_runtime(shared_conn.clone()),
        key_prefix.clone(),
    );
    (store, shared_conn, key_prefix)
}

fn make_state(instance_name: &str) -> OAuth2State {
    OAuth2State {
        instance_name: instance_name.to_string(),
        redirect_url: Some("/dashboard".to_string()),
        created_at: chrono::Utc::now(),
        operation: OAuth2Operation::Login,
        authorization_mode: OAuth2AuthorizationMode::Browser,
        target_user_id: None,
        pkce_verifier: format!("verifier_{instance_name}"),
        nonce: None,
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_oauth_state_store_and_consume() {
    let (_container, conn) = start_redis().await;
    let (store, _shared_conn, _key_prefix) = prefixed_store(conn, "oauth2-state-store-consume");

    let state = make_state("github");
    let ttl = std::time::Duration::from_mins(1);

    // Store
    ok(
        store.store("token_1", &state, ttl).await,
        "OAuth state should be stored",
    );

    // Consume
    let retrieved = ok(
        store.consume("token_1").await,
        "OAuth state should be consumed",
    );
    assert!(retrieved.is_some());
    let retrieved = some(
        retrieved,
        "OAuth state should exist before single-use consume",
    );
    assert_eq!(retrieved.instance_name, "github");
    assert_eq!(retrieved.pkce_verifier, "verifier_github");
    assert_eq!(retrieved.redirect_url.as_deref(), Some("/dashboard"));

    // Second consume should return None (token was deleted)
    let second = ok(
        store.consume("token_1").await,
        "second OAuth state consume should succeed",
    );
    assert!(
        second.is_none(),
        "Second consume should return None (single-use)"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_oauth_state_consume_is_atomic() {
    let (_container, conn) = start_redis().await;
    let (store, _shared_conn, _key_prefix) = prefixed_store(conn, "oauth2-state-atomic");
    let store = Arc::new(store);

    let state = make_state("atomic_test");
    let ttl = std::time::Duration::from_mins(1);
    ok(
        store.store("atomic_token", &state, ttl).await,
        "atomic OAuth state should be stored",
    );

    // Spawn 20 concurrent consumers
    let mut handles = Vec::new();
    for _ in 0..20 {
        let s = store.clone();
        handles.push(tokio::spawn(async move { s.consume("atomic_token").await }));
    }

    let mut success_count = 0;
    let mut none_count = 0;
    for h in handles {
        let result = ok(
            ok(h.await, "OAuth state consume task should join"),
            "OAuth state consume task should succeed",
        );
        match result {
            Some(state) => {
                assert_eq!(state.instance_name, "atomic_test");
                success_count += 1;
            }
            None => {
                none_count += 1;
            }
        }
    }

    assert_eq!(
        success_count, 1,
        "Exactly 1 out of 20 concurrent consumes must succeed"
    );
    assert_eq!(none_count, 19);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_oauth_state_ttl_expiry() {
    let (_container, conn) = start_redis().await;
    let (store, _shared_conn, _key_prefix) = prefixed_store(conn, "oauth2-state-ttl");

    let state = make_state("ttl_test");
    let ttl = std::time::Duration::from_secs(1);

    ok(
        store.store("ttl_token", &state, ttl).await,
        "TTL OAuth state should be stored",
    );

    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

    let result = ok(
        store.consume("ttl_token").await,
        "expired OAuth state consume should succeed",
    );
    assert!(result.is_none(), "Token should have expired after 1s TTL");
}

// Additional Tests for OAuth2 Concurrency and Error Handling

/// Test that concurrent state consumption with barrier synchronization
/// results in exactly one success (simulates real-world race conditions).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_oauth_state_concurrent_with_barrier() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Barrier;

    let (_container, conn) = start_redis().await;
    let (store, _shared_conn, _key_prefix) = prefixed_store(conn, "oauth2-state-barrier");
    let store = Arc::new(store);

    let state = make_state("barrier_test");
    let ttl = std::time::Duration::from_mins(1);
    ok(
        store.store("barrier_token", &state, ttl).await,
        "barrier OAuth state should be stored",
    );

    // Use barrier to maximize concurrency - all threads start at exactly the same time
    let barrier = Arc::new(Barrier::new(50));
    let success_count = Arc::new(AtomicUsize::new(0));
    let none_count = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..50 {
        let s = store.clone();
        let b = barrier.clone();
        let sc = success_count.clone();
        let nc = none_count.clone();

        handles.push(tokio::spawn(async move {
            b.wait().await;

            match s.consume("barrier_token").await {
                Ok(Some(state)) => {
                    assert_eq!(state.instance_name, "barrier_test");
                    sc.fetch_add(1, Ordering::SeqCst);
                }
                Ok(None) => {
                    nc.fetch_add(1, Ordering::SeqCst);
                }
                Err(_) => {
                    // Redis error - should not happen in this test
                }
            }
        }));
    }

    for h in handles {
        ok(h.await, "barrier OAuth state consume task should join");
    }

    assert_eq!(
        success_count.load(Ordering::SeqCst),
        1,
        "Exactly 1 out of 50 concurrent consumes must succeed"
    );
    assert_eq!(
        none_count.load(Ordering::SeqCst),
        49,
        "49 consumes must return None (already consumed)"
    );
}

/// Test that storing and consuming different state tokens works correctly
/// (no interference between tokens).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_oauth_state_multiple_tokens_isolated() {
    let (_container, conn) = start_redis().await;
    let (store, _shared_conn, _key_prefix) = prefixed_store(conn, "oauth2-state-multi");

    let ttl = std::time::Duration::from_mins(1);

    // Store multiple states
    for i in 0..10 {
        let state = OAuth2State {
            instance_name: format!("provider_{i}"),
            redirect_url: None,
            created_at: chrono::Utc::now(),
            operation: OAuth2Operation::Login,
            authorization_mode: OAuth2AuthorizationMode::Browser,
            target_user_id: None,
            pkce_verifier: format!("verifier_{i}"),
            nonce: None,
        };
        ok(
            store.store(&format!("token_{i}"), &state, ttl).await,
            &format!("OAuth state token_{i} should be stored"),
        );
    }

    // Consume in random order and verify each is isolated
    let order = [5, 2, 8, 1, 9, 0, 3, 7, 4, 6];
    for &i in &order {
        let result = ok(
            store.consume(&format!("token_{i}")).await,
            &format!("OAuth state token_{i} should be consumed"),
        );
        assert!(result.is_some(), "Token {i} should be found");
        let state = some(result, &format!("OAuth state token_{i} should exist"));
        assert_eq!(state.instance_name, format!("provider_{i}"));
        assert_eq!(state.pkce_verifier, format!("verifier_{i}"));
    }

    // All tokens should now be consumed
    for i in 0..10 {
        let result = ok(
            store.consume(&format!("token_{i}")).await,
            &format!("OAuth state token_{i} second consume should succeed"),
        );
        assert!(result.is_none(), "Token {i} should be already consumed");
    }
}

/// Test that expired state tokens (based on `created_at`) are correctly rejected
/// even if they somehow persist in Redis.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_oauth_state_created_at_expiry_check() {
    let (_container, conn) = start_redis().await;
    let (store, _shared_conn, _key_prefix) = prefixed_store(conn, "oauth2-state-created-at");

    let expired_time = chrono::Utc::now() - chrono::Duration::seconds(360);
    let state = OAuth2State {
        instance_name: "expired".to_string(),
        redirect_url: None,
        created_at: expired_time,
        operation: OAuth2Operation::Login,
        authorization_mode: OAuth2AuthorizationMode::Browser,
        target_user_id: None,
        pkce_verifier: "expired_verifier".to_string(),
        nonce: None,
    };

    // Store with long TTL (simulating Redis TTL not being enforced)
    let long_ttl = std::time::Duration::from_hours(1);
    ok(
        store.store("expired_by_created_at", &state, long_ttl).await,
        "created-at expired OAuth state should be stored",
    );

    // The state should be retrievable from Redis (TTL not expired)
    let result = ok(
        store.consume("expired_by_created_at").await,
        "created-at expired OAuth state should be consumed from store",
    );
    assert!(result.is_some(), "State should be in Redis");

    // But the service layer (consume_state) should reject it based on created_at
    // Note: This test validates the store layer; service layer expiry is tested elsewhere
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_oauth_state_store_uses_configured_key_prefix() {
    let (_container, conn) = start_redis().await;
    let shared_conn = Arc::new(RwLock::new(conn));
    let key_prefix = format!("{}tenant-a", test_redis_key_prefix("oauth2-state-prefixed"));
    let store = RedisOAuthStateStore::from_runtime(
        synctv_core::shared_runtime(shared_conn.clone()),
        key_prefix.clone(),
    );

    let state = make_state("prefixed");
    ok(
        store
            .store("prefixed_token", &state, std::time::Duration::from_mins(1))
            .await,
        "prefixed OAuth state should be stored",
    );

    let mut raw_conn = shared_conn.read().await.clone();

    let prefixed_exists: bool = ok(
        raw_conn
            .exists(format!("{key_prefix}:oauth2:state:prefixed_token"))
            .await,
        "prefixed key existence check should succeed",
    );
    assert!(
        prefixed_exists,
        "state must be stored under configured key prefix"
    );

    let unprefixed_exists: bool = ok(
        raw_conn.exists("oauth2:state:prefixed_token").await,
        "unprefixed key existence check should succeed",
    );
    assert!(
        !unprefixed_exists,
        "state must not leak into the global unprefixed namespace"
    );
}
