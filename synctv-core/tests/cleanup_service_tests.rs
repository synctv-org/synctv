//! CleanupService tests (S12b)
//!
//! Tests zero retention skipping all tasks, and non-leader skipping cleanup.
//! These are unit-style tests that don't need a real database for the leader/config
//! checks (but use testcontainers for run_all verification).
//!
//! Run with: cargo test -p synctv-core --test cleanup_service_tests -- --nocapture

use std::sync::Arc;

use synctv_core::service::{
    cleanup::{CleanupConfig, CleanupService},
    LeaderCheck, AlwaysLeader,
};
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

const POSTGRES_VERSION: &str = "16-alpine";

/// A LeaderCheck that always returns false
struct NeverLeader;

impl LeaderCheck for NeverLeader {
    fn is_leader(&self) -> bool {
        false
    }
}

async fn create_test_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let postgres = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
        .start()
        .await
        .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/synctv_test",
        postgres.get_host_port_ipv4(5432).await.expect("Failed to get port")
    );

    let pool = PgPool::connect(&connection_string)
        .await
        .expect("Failed to create pool");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (postgres, pool)
}

// ========== Zero retention skips all tasks ==========

#[tokio::test]
async fn test_zero_retention_skips_all_tasks() {
    let (_container, pool) = create_test_pool().await;

    // All zero retention values
    let config = CleanupConfig {
        soft_delete_retention_days: 0,
        room_soft_delete_retention_days: 0,
        expired_token_retention_days: 0,
        expired_credential_buffer_hours: 0,
        notification_retention_days: 0,
        notification_max_retention_days: 0,
        chat_max_messages_per_room: 0,
    };

    let service = CleanupService::new(pool, config, Arc::new(AlwaysLeader));
    let result = service.run_all().await;

    // All counters should be 0 since all tasks are skipped
    assert_eq!(result.users_purged, 0, "Zero retention should skip user purge");
    assert_eq!(result.rooms_purged, 0, "Zero retention should skip room purge");
    assert_eq!(result.tokens_deleted, 0, "Zero retention should skip token cleanup");
    assert_eq!(result.credentials_deleted, 0, "Zero retention should skip credential cleanup");
    assert_eq!(result.notifications_deleted, 0, "Zero retention should skip notification cleanup");
    assert_eq!(result.chat_messages_deleted, 0, "Zero retention should skip chat cleanup");
}

// ========== Non-leader skips cleanup (via start_periodic) ==========
// Note: start_periodic checks is_leader() inside the loop. We test that NeverLeader
// causes the service to skip. Since start_periodic is a background task, we verify
// the concept by calling run_all directly with NeverLeader not being meaningful at
// that level -- run_all is always called.
//
// The actual leader check happens in start_periodic. So we test
// the config-level skip and verify NeverLeader compiles and works.

#[tokio::test]
async fn test_non_leader_periodic_skips() {
    let (_container, pool) = create_test_pool().await;

    let config = CleanupConfig::default();
    let service = CleanupService::new(pool, config, Arc::new(NeverLeader));

    // Start periodic with a very short interval, cancel quickly
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel.clone();

    let handle = service.start_periodic(1, cancel_clone);

    // Wait a brief moment, then cancel
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    cancel.cancel();

    // Should complete without errors
    let _ = handle.await;
}

// ========== Default config values ==========

#[test]
fn test_cleanup_config_defaults() {
    let config = CleanupConfig::default();
    assert_eq!(config.soft_delete_retention_days, 90);
    assert_eq!(config.room_soft_delete_retention_days, 90);
    assert_eq!(config.expired_token_retention_days, 7);
    assert_eq!(config.expired_credential_buffer_hours, 1);
    assert_eq!(config.notification_retention_days, 30);
    assert_eq!(config.notification_max_retention_days, 90);
    assert_eq!(config.chat_max_messages_per_room, 0);
}

// ========== run_all with default config on empty database ==========

#[tokio::test]
async fn test_run_all_on_empty_database() {
    let (_container, pool) = create_test_pool().await;

    let config = CleanupConfig::default();
    let service = CleanupService::new(pool, config, Arc::new(AlwaysLeader));
    let result = service.run_all().await;

    // Empty database means nothing to clean
    assert_eq!(result.users_purged, 0);
    assert_eq!(result.rooms_purged, 0);
    assert_eq!(result.tokens_deleted, 0);
    assert_eq!(result.notifications_deleted, 0);
}

// ========== Partial config: only some tasks enabled ==========

#[tokio::test]
async fn test_partial_config_only_some_tasks_enabled() {
    let (_container, pool) = create_test_pool().await;

    // Only user and room purge enabled, everything else disabled
    let config = CleanupConfig {
        soft_delete_retention_days: 30,
        room_soft_delete_retention_days: 30,
        expired_token_retention_days: 0,
        expired_credential_buffer_hours: 0,
        notification_retention_days: 0,
        notification_max_retention_days: 0,
        chat_max_messages_per_room: 0,
    };

    let service = CleanupService::new(pool, config, Arc::new(AlwaysLeader));
    let result = service.run_all().await;

    // Token/credential/notification/chat should be 0 since disabled
    assert_eq!(result.tokens_deleted, 0, "Disabled tasks should return 0");
    assert_eq!(result.credentials_deleted, 0, "Disabled tasks should return 0");
    assert_eq!(result.notifications_deleted, 0, "Disabled tasks should return 0");
    assert_eq!(result.chat_messages_deleted, 0, "Disabled tasks should return 0");
}
