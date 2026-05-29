//! `RoomSettingsRepository` integration tests
//!
//! Tests: `set_settings_with_version` CAS (concurrent insert race, stale version -> `OptimisticLockConflict`),
//!        batch reads fail closed on invalid room settings JSON.
//!
//! Run with: cargo test -p synctv-core --test `room_settings_repository_tests`
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    models::{Room, RoomId, RoomSettings, RoomStatus, User, UserId, UserRole, UserStatus},
    repository::{RoomRepository, RoomSettingsRepository, UserRepository},
    Error,
};
use synctv_core_testing::create_test_pool;
fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
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
    }
}

fn make_room(name: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: String::new(),
        created_by: *owner,
        status: RoomStatus::Active,
        is_banned: false,
        closed_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
        last_activity_at: now,
    }
}

async fn setup_room(pool: &PgPool, username: &str, room_name: &str) -> (User, Room) {
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let user = user_repo.create(&make_user(username)).await.unwrap();
    let room = room_repo
        .create(&make_room(room_name, &user.id))
        .await
        .unwrap();
    (user, room)
}

// ─── CAS: insert with version=0 ─────────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_settings_with_version_initial_insert() {
    let (_container, pool) = create_test_pool().await;
    let settings_repo = RoomSettingsRepository::new(pool.clone());
    let (_user, room) = setup_room(&pool, "cas_user1", "cas_room1").await;

    let settings = RoomSettings::default();
    let new_version = settings_repo
        .set_settings_with_version(&room.id, &settings, 0)
        .await
        .unwrap();
    assert_eq!(new_version, 1);

    // Read back
    let (read_settings, version) = settings_repo.get_with_version(&room.id).await.unwrap();
    assert_eq!(version, 1);
    assert!(read_settings.chat_enabled.0); // default is true
}

// ─── CAS: concurrent insert race ────────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_settings_with_version_concurrent_insert_race() {
    let (_container, pool) = create_test_pool().await;
    let settings_repo = RoomSettingsRepository::new(pool.clone());
    let (_user, room) = setup_room(&pool, "cas_race_user", "cas_race_room").await;

    // First insert succeeds
    let settings = RoomSettings::default();
    let v1 = settings_repo
        .set_settings_with_version(&room.id, &settings, 0)
        .await
        .unwrap();
    assert_eq!(v1, 1);

    // Second insert with version=0 should fail (concurrent insert)
    let result = settings_repo
        .set_settings_with_version(&room.id, &settings, 0)
        .await;
    assert!(
        matches!(result, Err(Error::OptimisticLockConflict)),
        "Concurrent insert with version=0 should fail: {result:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_settings_with_exact_version_updates_existing_zero_version_row() {
    let (_container, pool) = create_test_pool().await;
    let settings_repo = RoomSettingsRepository::new(pool.clone());
    let (_user, room) = setup_room(&pool, "exact_zero_user", "exact_zero_room").await;

    sqlx::query!(
        "INSERT INTO room_settings (room_id, key, value, version)
         VALUES ($1, '_settings', $2, 0)",
        room.id as RoomId,
        serde_json::to_string(&RoomSettings::default()).unwrap(),
    )
    .execute(&pool)
    .await
    .unwrap();

    let changed = RoomSettings {
        chat_enabled: synctv_core::models::room_settings::ChatEnabled(false),
        ..Default::default()
    };
    let version = settings_repo
        .set_settings_with_exact_version(&room.id, &changed, 0, 1)
        .await
        .unwrap();
    assert_eq!(version, 1);

    let (stored, stored_version) = settings_repo.get_with_version(&room.id).await.unwrap();
    assert_eq!(stored_version, 1);
    assert!(!stored.chat_enabled.0);
}

// ─── CAS: stale version update ──────────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_settings_with_version_stale_update() {
    let (_container, pool) = create_test_pool().await;
    let settings_repo = RoomSettingsRepository::new(pool.clone());
    let (_user, room) = setup_room(&pool, "cas_stale_user", "cas_stale_room").await;

    let mut settings = RoomSettings::default();
    let v1 = settings_repo
        .set_settings_with_version(&room.id, &settings, 0)
        .await
        .unwrap();
    assert_eq!(v1, 1);

    // Update with correct version
    settings.chat_enabled = synctv_core::models::room_settings::ChatEnabled(false);
    let v2 = settings_repo
        .set_settings_with_version(&room.id, &settings, v1)
        .await
        .unwrap();
    assert_eq!(v2, 2);

    // Update with stale version (v1 instead of v2) should fail
    settings.chat_enabled = synctv_core::models::room_settings::ChatEnabled(true);
    let result = settings_repo
        .set_settings_with_version(&room.id, &settings, v1)
        .await;
    assert!(
        matches!(result, Err(Error::OptimisticLockConflict)),
        "Stale version update should fail: {result:?}"
    );

    // Verify the settings weren't changed
    let (read_settings, version) = settings_repo.get_with_version(&room.id).await.unwrap();
    assert_eq!(version, 2);
    assert!(
        !read_settings.chat_enabled.0,
        "Should still be false from v2 update"
    );
}

// ─── get_batch fail-closed JSON deserialization ──────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_batch_rejects_invalid_json() {
    let (_container, pool) = create_test_pool().await;
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    let (_user1, room1) = setup_room(&pool, "batch_user1", "batch_room1").await;
    let (_user2, room2) = setup_room(&pool, "batch_user2", "batch_room2").await;
    let (_user3, room3) = setup_room(&pool, "batch_user3", "batch_room3").await;

    // Insert valid settings for room1
    let settings = RoomSettings::default();
    settings_repo
        .set_settings(&room1.id, &settings)
        .await
        .unwrap();

    // Insert invalid JSON for room2 directly via SQL
    sqlx::query(
        r"INSERT INTO room_settings (room_id, key, value, version) VALUES ($1, '_settings', 'not valid json!!!', 1)"
    )
    .bind(room2.id)
    .execute(&pool)
    .await
    .unwrap();

    // Insert valid settings for room3
    settings_repo
        .set_settings(&room3.id, &settings)
        .await
        .unwrap();

    // get_batch must fail instead of silently dropping room2 and letting callers
    // cache defaults for a corrupted settings row.
    let room_ids = vec![room1.id, room2.id, room3.id];
    let error = settings_repo
        .get_batch(&room_ids)
        .await
        .expect_err("invalid settings JSON should fail batch reads");

    assert!(
        error
            .to_string()
            .contains("Failed to deserialize room settings"),
        "unexpected error: {error}"
    );
    assert!(
        error.to_string().contains(&room2.id.to_string()),
        "error should identify the corrupted room: {error}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_batch_empty_input() {
    let (_container, pool) = create_test_pool().await;
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    let result = settings_repo.get_batch(&[]).await.unwrap();
    assert!(result.is_empty());
}
