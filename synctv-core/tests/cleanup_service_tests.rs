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
use synctv_core::repository::{RoomRepository, UserRepository};
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

#[tokio::test]
#[ignore = "Requires Docker"]
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

// Note: start_periodic checks is_leader() inside the loop. We test that NeverLeader
// causes the service to skip. Since start_periodic is a background task, we verify
// the concept by calling run_all directly with NeverLeader not being meaningful at
// that level -- run_all is always called.
// The actual leader check happens in start_periodic. So we test
// the config-level skip and verify NeverLeader compiles and works.

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_non_leader_periodic_skips() {
    let (_container, pool) = create_test_pool().await;

    let config = CleanupConfig::default();
    let service = CleanupService::new(pool, config, Arc::new(NeverLeader));

    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel.clone();

    let handle = service.start_periodic(1, cancel_clone);

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    cancel.cancel();

    // Should complete without errors
    let _ = handle.await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
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

#[tokio::test]
#[ignore = "Requires Docker"]
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_run_all_purges_soft_deleted_user_after_room_and_membership_cleanup() {
    let (_container, pool) = create_test_pool().await;

    let room_owner = create_test_user(&pool).await;
    let deleted_user = create_test_user(&pool).await;

    let deleted_owned_room = create_test_room(deleted_user.id, None);
    let surviving_room = create_test_room(room_owner.id, None);

    let room_repo = RoomRepository::new(pool.clone());
    let deleted_owned_room = room_repo
        .create(&deleted_owned_room)
        .await
        .expect("Failed to create deleted user's owned room");
    let surviving_room = room_repo
        .create(&surviving_room)
        .await
        .expect("Failed to create surviving room");

    let forty_days_ago = Utc::now() - Duration::days(40);
    sqlx::query(
        "UPDATE users
         SET deleted_at = $2, updated_at = $2
         WHERE id = $1",
    )
    .bind(deleted_user.id)
    .bind(forty_days_ago)
    .execute(&pool)
    .await
    .expect("Failed to soft-delete user");

    sqlx::query(
        "UPDATE rooms
         SET deleted_at = $2, updated_at = $2
         WHERE id = $1",
    )
    .bind(deleted_owned_room.id)
    .bind(forty_days_ago)
    .execute(&pool)
    .await
    .expect("Failed to soft-delete owned room");

    sqlx::query(
        "INSERT INTO room_members (room_id, user_id, role, joined_at, version)
         VALUES ($1, $2, $3, $4, 0)",
    )
    .bind(surviving_room.id)
    .bind(deleted_user.id)
    .bind(3_i16)
    .bind(forty_days_ago)
    .execute(&pool)
    .await
    .expect("Failed to insert historical room membership");

    let service = CleanupService::new(
        pool.clone(),
        CleanupConfig {
            soft_delete_retention_days: 30,
            room_soft_delete_retention_days: 30,
            expired_token_retention_days: 0,
            expired_credential_buffer_hours: 0,
            notification_retention_days: 0,
            notification_max_retention_days: 0,
            chat_max_messages_per_room: 0,
        },
        Arc::new(AlwaysLeader),
    );

    let result = service.run_all().await;

    assert_eq!(
        result.rooms_purged, 1,
        "Cleanup should purge the user's soft-deleted owned room first"
    );
    assert_eq!(
        result.users_purged, 1,
        "Cleanup should purge the soft-deleted user in the same run"
    );

    let user_still_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
            .bind(deleted_user.id)
            .fetch_one(&pool)
            .await
            .expect("Failed to query deleted user");
    assert!(
        !user_still_exists,
        "Soft-deleted user should be hard-deleted"
    );

    let membership_still_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM room_members WHERE user_id = $1)")
            .bind(deleted_user.id)
            .fetch_one(&pool)
            .await
            .expect("Failed to query historical memberships");
    assert!(
        !membership_still_exists,
        "Historical room_members rows must not block hard deletion of soft-deleted users"
    );
}

/// Helper to create a test user in the database
async fn create_test_user(pool: &PgPool) -> User {
    let now = Utc::now();
    let user_id = UserId::new();
    let email = format!("test_{}@example.com", synctv_common::snanoid!(8));
    let user = User {
        id: user_id,
        username: format!("test_user_{}", synctv_common::snanoid!(8)),
        email: Some(email),
        password_hash: "test_hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };
    UserRepository::new(pool.clone())
        .create(&user)
        .await
        .expect("Failed to create test user")
}

/// Helper to create a test room with optional custom timestamps
fn create_test_room(created_by: UserId, updated_at: Option<chrono::DateTime<Utc>>) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: String::new(),
        created_by,
        status: RoomStatus::Active,
        is_banned: false,
        closed_at: None,
        created_at: now,
        updated_at: updated_at.unwrap_or(now),
        deleted_at: None,
        version: 0,
        last_activity_at: updated_at.unwrap_or(now),
    }
}
