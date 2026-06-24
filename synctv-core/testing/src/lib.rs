//! Shared test helpers for synctv-core tests
//!
//! This crate provides common test utilities to reduce code duplication
//! across integration tests.

use std::path::PathBuf;

pub mod assertions;
pub mod constants;
pub(crate) mod docker;
pub mod external_service;
pub mod postgres;
pub mod redis;
pub mod result;
pub mod rustfs;
pub mod services;
pub mod source_config;

pub(crate) fn test_temp_dir() -> PathBuf {
    let path = std::env::temp_dir();
    match std::fs::create_dir_all(&path) {
        Ok(()) => path,
        Err(error) => std::panic::panic_any(format!(
            "failed to create test temp dir {}: {error}",
            path.display()
        )),
    }
}

pub use external_service::{
    start_external_service, ExternalServiceContainer, ExternalServiceRequest,
};
pub use postgres::{
    connect_test_pool_url, create_test_database, create_test_database_url_with_label,
    create_test_database_with_db_and_label, create_test_database_with_options_and_label,
    create_test_pool, create_test_pool_with_db, create_test_pool_with_db_and_label,
    create_test_pool_with_options_and_label, ensure_audit_partition_for, ensure_chat_partition_for,
    ensure_notification_partition_for, postgres_connection_url_with_credentials, TestContainer,
    TestDatabase,
};
pub use redis::{
    redis_connection_manager, redis_multiplexed_connection, start_dedicated_redis,
    start_dedicated_redis_url_with_label, start_redis, start_redis_client_manager,
    start_redis_client_manager_with_label, start_redis_client_url_with_label, start_redis_handle,
    start_redis_url, start_redis_url_with_label, start_redis_with_client, test_redis_key_prefix,
    wait_for_redis_ready, RedisContainer,
};
pub use result::{err, ok, some, TestOptionExt, TestResultExt};
pub use rustfs::{
    start_rustfs, test_rustfs_base_path, test_rustfs_bucket_name, RustfsContainer, RustfsS3Config,
    RUSTFS_ACCESS_KEY, RUSTFS_REGION, RUSTFS_SECRET_KEY,
};
pub use services::{
    create_empty_provider_instance_manager, create_test_brute_force_protection_service,
    create_test_jwt_service, create_test_request_rate_limiter, create_test_room_service,
    create_test_token_blacklist_store_service, create_test_user_service, failing_redis_runtime,
    opaque_login_user, opaque_login_user_with_challenge, opaque_register_user,
    opaque_register_user_with_client_ip,
};
pub use source_config::{
    alist_directory_playlist_source_config, alist_file_media_source_config,
    bilibili_video_media_source_config, direct_url_media_source_config,
    direct_url_media_source_config_with_headers, live_proxy_pull_live_media_source_config,
    rtmp_managed_live_media_source_config,
};
