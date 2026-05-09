//! Integration tests for `TieredTokenBlacklistStore` persistence ordering.
//!
//! These tests guard the production invariant that PostgreSQL is the durable
//! source of truth and Redis is only a cache / coordination layer.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use redis::AsyncCommands;
use synctv_core::service::{
    auth::token_blacklist::{PgTokenBlacklistStore, TieredTokenBlacklistStore},
    TokenBlacklistStore,
};
use synctv_core_testing::{redis_connection_manager, start_redis_with_client};
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
async fn test_blacklist_if_not_exists_requires_pg_success_before_redis_cache_write() {
    let (_container, redis_client) = start_redis().await;
    let shared_conn = Arc::new(RwLock::new(redis_connection_manager(&redis_client).await));
    let key_prefix = "pg-primary:".to_string();
    let key = "jti:pg_primary_required";
    let redis_key = format!("{key_prefix}bl:{key}");

    let store = TieredTokenBlacklistStore::from_runtime(
        unavailable_pg_store(),
        synctv_core::shared_runtime_from_conn(Some(shared_conn)),
        key_prefix.clone(),
    );

    let result = store.blacklist_if_not_exists(key, 3600).await;
    assert!(
        result.is_err(),
        "blacklist_if_not_exists must fail when PG persistence fails, even if Redis is healthy"
    );

    let mut verify_conn = redis_connection_manager(&redis_client).await;
    let cached: Option<String> = verify_conn
        .get(&redis_key)
        .await
        .expect("Redis lookup should succeed");
    assert!(
        cached.is_none(),
        "Redis cache must not be populated when PG primary write fails"
    );
}
