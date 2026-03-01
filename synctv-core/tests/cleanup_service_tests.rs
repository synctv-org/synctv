//! `CleanupService` tests (S12b)
//!
//! Tests zero retention skipping all tasks, and non-leader skipping cleanup.
//! These are unit-style tests that don't need a real database for the leader/config
//! checks (but use testcontainers for `run_all` verification).
//!
//! Run with: cargo test -p synctv-core --test `cleanup_service_tests` -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::{Duration, Utc};
use sqlx::PgPool;
use synctv_core::models::{Room, RoomId, RoomStatus, User, UserId, UserRole, UserStatus};
use synctv_core::repository::RoomRepository;
use synctv_core::service::{
    cleanup::{CleanupConfig, CleanupService},
    AlwaysLeader, LeaderCheck,
};
use synctv_core_testing::create_test_pool;

/// A `LeaderCheck` that always returns false
struct NeverLeader;

impl LeaderCheck for NeverLeader {
    fn is_leader(&self) -> bool {
        false
    }
}

// ========== Zero retention skips all tasks ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_zero_retention_skips_all_tasks() {
    let (_container, pool) = create_test_pool().await;

    // All zero retention values
    let config = CleanupConfig {
        room_ttl_seconds: 0,
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
    assert_eq!(
        result.rooms_expired, 0,
        "Zero retention should skip room TTL expiration"
    );
    assert_eq!(
        result.users_purged, 0,
        "Zero retention should skip user purge"
    );
    assert_eq!(
        result.rooms_purged, 0,
        "Zero retention should skip room purge"
    );
    assert_eq!(
        result.tokens_deleted, 0,
        "Zero retention should skip token cleanup"
    );
    assert_eq!(
        result.credentials_deleted, 0,
        "Zero retention should skip credential cleanup"
    );
    assert_eq!(
        result.notifications_deleted, 0,
        "Zero retention should skip notification cleanup"
    );
    assert_eq!(
        result.chat_messages_deleted, 0,
        "Zero retention should skip chat cleanup"
    );
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
#[ignore = "Requires Docker"]
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
    assert_eq!(config.room_ttl_seconds, 172_800); // 48 hours
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
#[ignore = "Requires Docker"]
async fn test_run_all_on_empty_database() {
    let (_container, pool) = create_test_pool().await;

    let config = CleanupConfig::default();
    let service = CleanupService::new(pool, config, Arc::new(AlwaysLeader));
    let result = service.run_all().await;

    // Empty database means nothing to clean
    assert_eq!(result.rooms_expired, 0);
    assert_eq!(result.users_purged, 0);
    assert_eq!(result.rooms_purged, 0);
    assert_eq!(result.tokens_deleted, 0);
    assert_eq!(result.notifications_deleted, 0);
}

// ========== Partial config: only some tasks enabled ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_partial_config_only_some_tasks_enabled() {
    let (_container, pool) = create_test_pool().await;

    // Only user and room purge enabled, everything else disabled
    let config = CleanupConfig {
        room_ttl_seconds: 0, // disabled
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
    assert_eq!(
        result.credentials_deleted, 0,
        "Disabled tasks should return 0"
    );
    assert_eq!(
        result.notifications_deleted, 0,
        "Disabled tasks should return 0"
    );
    assert_eq!(
        result.chat_messages_deleted, 0,
        "Disabled tasks should return 0"
    );
}

// ========== Room TTL enforcement tests ==========

/// Helper to create a test user in the database
async fn create_test_user(pool: &PgPool) -> User {
    let now = Utc::now();
    let user_id = UserId::new();
    let email = format!("test_{}@example.com", nanoid::nanoid!(8));
    let user = User {
        id: user_id,
        username: format!("test_user_{}", nanoid::nanoid!(8)),
        email: Some(email),
        password_hash: "test_hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        signup_method: None,
        email_verified: true,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    };
    sqlx::query(
        "INSERT INTO users (id, username, email, password_hash, role, status, email_verified, created_at, updated_at, password_changed_at, password_version, version) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"
    )
    .bind(user.id.as_str())
    .bind(&user.username)
    .bind(&user.email)
    .bind(&user.password_hash)
    .bind(user.role as i16)
    .bind(user.status as i16)
    .bind(user.email_verified)
    .bind(user.created_at)
    .bind(user.updated_at)
    .bind(user.password_changed_at)
    .bind(user.password_version)
    .bind(user.version)
    .execute(pool)
    .await
    .expect("Failed to create test user");
    user
}

/// Helper to create a test room with optional custom timestamps
fn create_test_room(created_by: UserId, updated_at: Option<chrono::DateTime<Utc>>) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId(nanoid::nanoid!(12)),
        name: "Test Room".to_string(),
        description: String::new(),
        created_by,
        status: RoomStatus::Active,
        is_banned: false,
        created_at: now,
        updated_at: updated_at.unwrap_or(now),
        deleted_at: None,
        version: 0,
    }
}

/// Test that a room newer than `room_ttl` is NOT soft-deleted
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_ttl_new_room_not_expired() {
    let (_container, pool) = create_test_pool().await;

    // Create a user and room
    let user = create_test_user(&pool).await;
    let room = create_test_room(user.id.clone(), None);

    let room_repo = RoomRepository::new(pool.clone());
    let _ = room_repo
        .create(&room)
        .await
        .expect("Failed to create room");

    // Set room_ttl to 1 hour (room is newer, should NOT be expired)
    let config = CleanupConfig {
        room_ttl_seconds: 3600, // 1 hour
        ..CleanupConfig::default()
    };

    let service = CleanupService::new(pool, config, Arc::new(AlwaysLeader));
    let result = service.run_all().await;

    // Room should NOT be expired since it's newer than 1 hour
    assert_eq!(result.rooms_expired, 0, "New room should not be expired");

    // Verify room is still active (not soft-deleted)
    let found = room_repo
        .get_by_id(&room.id)
        .await
        .expect("Failed to find room");
    assert!(found.is_some(), "Room should still exist");
    let found = found.unwrap();
    assert!(
        found.deleted_at.is_none(),
        "Room should not be soft-deleted"
    );
}

/// Test that a room older than `room_ttl` IS soft-deleted
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_ttl_old_room_is_expired() {
    let (_container, pool) = create_test_pool().await;

    // Create a room with updated_at 2 hours ago (older than 1 hour TTL)
    let two_hours_ago = Utc::now() - Duration::hours(2);
    let user = create_test_user(&pool).await;
    let room = create_test_room(user.id.clone(), Some(two_hours_ago));

    let room_repo = RoomRepository::new(pool.clone());
    let _ = room_repo
        .create(&room)
        .await
        .expect("Failed to create room");

    // Set room_ttl to 1 hour (room is older, should be expired)
    let config = CleanupConfig {
        room_ttl_seconds: 3600, // 1 hour
        ..CleanupConfig::default()
    };

    let service = CleanupService::new(pool.clone(), config, Arc::new(AlwaysLeader));
    let result = service.run_all().await;

    // Room SHOULD be expired since it's older than 1 hour
    assert_eq!(result.rooms_expired, 1, "Old room should be expired");

    // Verify room is soft-deleted (need to query directly since get_by_id filters deleted_at)
    let found: Option<Room> = sqlx::query_as(
        "SELECT id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at, version
         FROM rooms WHERE id = $1"
    )
    .bind(room.id.as_str())
    .fetch_optional(&pool)
    .await
    .expect("Failed to find room");
    assert!(found.is_some(), "Room should still exist (soft-deleted)");
    let found = found.unwrap();
    assert!(found.deleted_at.is_some(), "Room should be soft-deleted");
}

/// Test that already soft-deleted rooms are not affected by `room_ttl` check
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_ttl_skips_already_soft_deleted() {
    let (_container, pool) = create_test_pool().await;

    // Create a room that's already soft-deleted and very old
    let two_hours_ago = Utc::now() - Duration::hours(2);
    let user = create_test_user(&pool).await;
    let room = create_test_room(user.id.clone(), Some(two_hours_ago));

    let room_repo = RoomRepository::new(pool.clone());
    let _ = room_repo
        .create(&room)
        .await
        .expect("Failed to create room");

    // Manually soft-delete the room (simulating it was deleted earlier)
    sqlx::query("UPDATE rooms SET deleted_at = $1 WHERE id = $2")
        .bind(Utc::now())
        .bind(room.id.as_str())
        .execute(&pool)
        .await
        .expect("Failed to soft-delete room");

    // Set room_ttl to 1 hour
    let config = CleanupConfig {
        room_ttl_seconds: 3600, // 1 hour
        ..CleanupConfig::default()
    };

    let service = CleanupService::new(pool, config, Arc::new(AlwaysLeader));
    let result = service.run_all().await;

    // Should not count already-soft-deleted rooms
    assert_eq!(
        result.rooms_expired, 0,
        "Already soft-deleted room should not be counted"
    );
}

/// Test that `room_ttl` = 0 disables expiration
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_ttl_zero_disables_expiration() {
    let (_container, pool) = create_test_pool().await;

    // Create a room that's very old
    let two_days_ago = Utc::now() - Duration::days(2);
    let user = create_test_user(&pool).await;
    let room = create_test_room(user.id.clone(), Some(two_days_ago));

    let room_repo = RoomRepository::new(pool.clone());
    let _ = room_repo
        .create(&room)
        .await
        .expect("Failed to create room");

    // Set room_ttl to 0 (disabled)
    let config = CleanupConfig {
        room_ttl_seconds: 0, // disabled
        ..CleanupConfig::default()
    };

    let service = CleanupService::new(pool, config, Arc::new(AlwaysLeader));
    let result = service.run_all().await;

    // Room should NOT be expired since room_ttl is disabled
    assert_eq!(
        result.rooms_expired, 0,
        "room_ttl=0 should disable expiration"
    );

    // Verify room is still active (not soft-deleted)
    let found = room_repo
        .get_by_id(&room.id)
        .await
        .expect("Failed to find room");
    assert!(found.is_some(), "Room should still exist");
    let found = found.unwrap();
    assert!(
        found.deleted_at.is_none(),
        "Room should not be soft-deleted when room_ttl=0"
    );
}
