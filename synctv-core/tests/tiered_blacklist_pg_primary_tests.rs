//! Integration tests for `TieredTokenBlacklistStore` persistence ordering.
//!
//! These tests guard the production invariant that PostgreSQL is the durable
//! source of truth and Redis is only a cache / coordination layer.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use redis::aio::ConnectionManager as RedisConnectionManager;
use redis::AsyncCommands;
use synctv_core::service::{PgTokenBlacklistStore, TieredTokenBlacklistStore, TokenBlacklistStore};
use synctv_core_testing::start_redis_with_client;
use tokio::sync::RwLock;

async fn start_redis() -> (synctv_core_testing::RedisContainer, redis::Client) {
    start_redis_with_client().await
}

fn unavailable_pg_store() -> PgTokenBlacklistStore {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(200))
        .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/invalid")
        .expect("connect_lazy should succeed for unavailable PG target");
    PgTokenBlacklistStore::new(pool)
}

#[tokio::test]
#[ignore = "Requires Docker"]
#[ignore = "Requires Docker (Redis testcontainer)"]
async fn test_blacklist_if_not_exists_requires_pg_success_before_redis_cache_write() {
    let (_container, redis_client) = start_redis().await;
    let shared_conn = Arc::new(RwLock::new(
        RedisConnectionManager::new(redis_client.clone())
            .await
            .expect("Failed to create Redis connection manager"),
    ));
    let key_prefix = "pg-primary:".to_string();
    let key = "jti:pg_primary_required";
    let redis_key = format!("{key_prefix}bl:{key}");

    let store = TieredTokenBlacklistStore::new(
        unavailable_pg_store(),
        Some(shared_conn),
        key_prefix.clone(),
    );

    let result = store.blacklist_if_not_exists(key, 3600).await;
    assert!(
        result.is_err(),
        "blacklist_if_not_exists must fail when PG persistence fails, even if Redis is healthy"
    );

    let mut verify_conn = RedisConnectionManager::new(redis_client)
        .await
        .expect("Failed to create Redis verification connection manager");
    let cached: Option<String> = verify_conn
        .get(&redis_key)
        .await
        .expect("Redis lookup should succeed");
    assert!(
        cached.is_none(),
        "Redis cache must not be populated when PG primary write fails"
    );
}
