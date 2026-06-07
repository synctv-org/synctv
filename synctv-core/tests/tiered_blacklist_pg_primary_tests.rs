//! Integration tests for `TieredTokenBlacklistStore` durable persistence ordering.
//!
//! These tests guard the production invariant that the durable store is the
//! source of truth and Redis is only a cache / coordination layer.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use redis::AsyncCommands;
use synctv_core::service::{auth::token_blacklist::TieredTokenBlacklistStore, TokenBlacklistStore};
use synctv_core_testing::{redis_connection_manager, start_redis_with_client};
use tokio::sync::RwLock;

async fn start_redis() -> (synctv_core_testing::RedisContainer, redis::Client) {
    start_redis_with_client().await
}

#[derive(Clone, Debug)]
struct FailingDurableTokenBlacklistStore;

#[async_trait::async_trait]
impl TokenBlacklistStore for FailingDurableTokenBlacklistStore {
    async fn is_blacklisted_checked(&self, _key: &str) -> synctv_core::Result<bool> {
        Err(synctv_core::Error::ServiceUnavailable(
            "durable token blacklist unavailable".to_string(),
        ))
    }

    async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> synctv_core::Result<()> {
        Err(synctv_core::Error::ServiceUnavailable(
            "durable token blacklist unavailable".to_string(),
        ))
    }

    async fn blacklist_if_not_exists(
        &self,
        _key: &str,
        _ttl_secs: u64,
    ) -> synctv_core::Result<bool> {
        Err(synctv_core::Error::ServiceUnavailable(
            "durable token blacklist unavailable".to_string(),
        ))
    }

    async fn get_family_revoked_at_checked(&self, _key: &str) -> synctv_core::Result<Option<i64>> {
        Err(synctv_core::Error::ServiceUnavailable(
            "durable token blacklist unavailable".to_string(),
        ))
    }

    async fn set_family_revoked(
        &self,
        _key: &str,
        _timestamp: i64,
        _ttl_secs: u64,
    ) -> synctv_core::Result<()> {
        Err(synctv_core::Error::ServiceUnavailable(
            "durable token blacklist unavailable".to_string(),
        ))
    }
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
        FailingDurableTokenBlacklistStore,
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
