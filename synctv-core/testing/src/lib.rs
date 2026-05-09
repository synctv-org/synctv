#![allow(clippy::unwrap_used)]
//! Shared test helpers for synctv-core tests
//!
//! This crate provides common test utilities to reduce code duplication
//! across integration tests.

use std::path::PathBuf;

pub mod assertions;
pub mod constants;
pub mod fixtures;
pub mod postgres;
pub mod redis;
pub mod services;

pub(crate) fn test_temp_dir() -> PathBuf {
    let path = std::env::temp_dir();
    std::fs::create_dir_all(&path)
        .unwrap_or_else(|e| panic!("failed to create test temp dir {}: {e}", path.display()));
    path
}

// Re-export commonly used items
pub use fixtures::{TestRoom, TestUser};
pub use postgres::{
    connect_test_pool_url, create_test_database, create_test_database_url_with_label,
    create_test_database_with_db_and_label, create_test_database_with_options_and_label,
    create_test_pool, create_test_pool_with_db, create_test_pool_with_db_and_label,
    create_test_pool_with_options_and_label, postgres_connection_url_with_credentials,
    TestContainer, TestDatabase,
};
pub use redis::{
    redis_connection_manager, redis_multiplexed_connection, start_dedicated_redis,
    start_dedicated_redis_url_with_label, start_redis, start_redis_client_manager,
    start_redis_client_manager_with_label, start_redis_client_url_with_label, start_redis_handle,
    start_redis_url, start_redis_url_with_label, start_redis_with_client, test_redis_key_prefix,
    wait_for_redis_ready, RedisConnectionHandle, RedisConnectionManager, RedisContainer,
};
pub use services::{
    create_test_attempt_tracker, create_test_brute_force_protection,
    create_test_brute_force_protection_service, create_test_jwt_service,
    create_test_jwt_service_with_secret, create_test_request_rate_limiter,
    create_test_streaming_publish_key_service, create_test_token_blacklist_store,
    create_test_token_blacklist_store_service, create_test_websocket_ticket_service,
};
