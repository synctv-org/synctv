//! OAuth2 Redis state store integration tests
//!
//! Tests the RedisOAuthStateStore: store, consume, TTL expiry,
//! and atomic single-use consumption under concurrency.
//!
//! Run with: cargo test --test oauth2_state_store_tests -- --nocapture

use std::sync::Arc;
use synctv_core::service::{OAuthStateStore, OAuth2State, RedisOAuthStateStore};
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

const REDIS_VERSION: &str = "7-alpine";

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, redis::aio::ConnectionManager) {
    let container = Redis::default()
        .with_tag(REDIS_VERSION)
        .start()
        .await
        .expect("Failed to start Redis");
    let port = container
        .get_host_port_ipv4(6379)
        .await
        .expect("Failed to get port");
    let redis_url = format!("redis://127.0.0.1:{}", port);
    let client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let conn = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to create connection manager");
    (container, conn)
}

fn make_state(instance_name: &str) -> OAuth2State {
    OAuth2State {
        instance_name: instance_name.to_string(),
        redirect_url: Some("/dashboard".to_string()),
        created_at: chrono::Utc::now(),
        bind_user_id: None,
        pkce_verifier: format!("verifier_{instance_name}"),
    }
}

#[tokio::test]
async fn test_redis_oauth_state_store_and_consume() {
    let (_container, conn) = start_redis().await;
    let store = RedisOAuthStateStore::new(conn);

    let state = make_state("github");
    let ttl = std::time::Duration::from_secs(60);

    // Store
    store.store("token_1", &state, ttl).await.unwrap();

    // Consume
    let retrieved = store.consume("token_1").await.unwrap();
    assert!(retrieved.is_some());
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.instance_name, "github");
    assert_eq!(retrieved.pkce_verifier, "verifier_github");
    assert_eq!(retrieved.redirect_url.as_deref(), Some("/dashboard"));

    // Second consume should return None (token was deleted)
    let second = store.consume("token_1").await.unwrap();
    assert!(
        second.is_none(),
        "Second consume should return None (single-use)"
    );
}

#[tokio::test]
async fn test_redis_oauth_state_consume_is_atomic() {
    let (_container, conn) = start_redis().await;
    let store = Arc::new(RedisOAuthStateStore::new(conn));

    let state = make_state("atomic_test");
    let ttl = std::time::Duration::from_secs(60);
    store.store("atomic_token", &state, ttl).await.unwrap();

    // Spawn 20 concurrent consumers
    let mut handles = Vec::new();
    for _ in 0..20 {
        let s = store.clone();
        handles.push(tokio::spawn(async move {
            s.consume("atomic_token").await
        }));
    }

    let mut success_count = 0;
    let mut none_count = 0;
    for h in handles {
        let result = h.await.unwrap().unwrap();
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
async fn test_redis_oauth_state_ttl_expiry() {
    let (_container, conn) = start_redis().await;
    let store = RedisOAuthStateStore::new(conn);

    let state = make_state("ttl_test");
    let ttl = std::time::Duration::from_secs(1);

    store.store("ttl_token", &state, ttl).await.unwrap();

    // Immediately available
    // (Don't consume -- just verify we can store with short TTL)

    // Wait for expiry
    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

    // Should be expired
    let result = store.consume("ttl_token").await.unwrap();
    assert!(
        result.is_none(),
        "Token should have expired after 1s TTL"
    );
}
