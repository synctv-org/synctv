//! Room concurrency integration tests
//!
//! Tests concurrent room operations: multi-user joins, concurrent room creation,
//! and concurrent settings updates with optimistic lock retry.
//!
//!
//! # Test Coverage
//!
//! - Multi-user concurrent join with `max_members` limit enforcement
//! - Concurrent creation of rooms with the same name by different users succeeds
//! - Concurrent repository-level inserts do not enforce room-name product policy
//! - Concurrent room settings updates (optimistic lock retry)
//!
//! # Requirements
//!
//! - Docker for testcontainers (`PostgreSQL` + Redis)

use chrono::Utc;
use sqlx::PgPool;
use std::sync::Arc;
use synctv_core::{
    models::{
        room_settings::MaxMembers, AddMemberOptions, Room, RoomId, RoomMember, RoomRole,
        RoomSettings, RoomStatus, User, UserId, UserRole, UserStatus,
    },
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository, UserRepository},
    service::{MemberService, NotificationService, PermissionService},
    Error,
};
use synctv_core_testing::{create_test_database_with_options_and_label, ok, TestDatabase};
use tokio::sync::Barrier;
// Test Infrastructure

async fn create_test_pool() -> TestDatabase {
    create_test_database_with_options_and_label(
        "synctv_test",
        "room-concurrency",
        64,
        std::time::Duration::from_secs(30),
    )
    .await
}

/// Create a test user in the database
fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

/// Create a test room
fn make_room(name: &str, description: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: description.to_string(),
        cover_file_reference_id: None,
        category: None,
        labels: Vec::new(),
        created_by: *owner,
        status: RoomStatus::Active,
        is_banned: false,
        is_public: true,
        closed_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
        last_activity_at: now,
    }
}

/// Setup test infrastructure with users and a room
async fn setup_test_room(
    pool: &PgPool,
    room_name: &str,
    max_members: u64,
) -> (User, Room, RoomSettings) {
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());

    let owner = ok(
        user_repo.create(&make_user("room_owner")).await,
        "room owner should be created",
    );

    let room = ok(
        room_repo
            .create(&make_room(room_name, "Test room", &owner.id))
            .await,
        "room should be created",
    );

    let settings = RoomSettings {
        max_members: MaxMembers(max_members),
        ..Default::default()
    };
    ok(
        room_settings_repo.set_settings(&room.id, &settings).await,
        "room settings should be created",
    );

    // Add owner as member (Creator)
    let member_repo = RoomMemberRepository::new(pool.clone());
    let owner_member = RoomMember::new(room.id, owner.id, RoomRole::Creator);
    ok(
        member_repo.add(&owner_member).await,
        "room owner member should be added",
    );

    (owner, room, settings)
}

fn permission_service(
    member_repo: RoomMemberRepository,
    room_repo: RoomRepository,
) -> PermissionService {
    ok(
        PermissionService::new(member_repo, room_repo, None, 1000, 300),
        "permission service should build",
    )
}

/// Test that `max_members` limit is enforced under concurrent joins.
///
/// Scenario:
/// 1. Create a room with `max_members` = 5
/// 2. Spawn 20 concurrent join requests
/// 3. Verify exactly 5 succeed (room capacity limit)
/// 4. Verify final member count is 5 (including owner = 6 total)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_join_respects_max_members_limit() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room, _settings) = setup_test_room(pool, "Concurrent Join Room", 5).await;

    let user_repo = UserRepository::new(pool.clone());
    let mut users = Vec::with_capacity(20);
    for i in 0..20 {
        let user = ok(
            user_repo.create(&make_user(&format!("joiner_{i}"))).await,
            "joiner user should be created",
        );
        users.push(user);
    }

    // Setup services
    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service = permission_service(member_repo.clone(), room_repo.clone());
    let member_service = MemberService::new_with_runtime(
        member_repo.clone(),
        room_repo.clone(),
        Some(room_settings_repo),
        permission_service.clone(),
        None,
        None,
        NotificationService::default(),
    );

    // Use barrier to synchronize all joins
    let barrier = Arc::new(Barrier::new(20));
    let room_id = room.id;

    // Spawn 20 concurrent join tasks
    let mut handles = Vec::with_capacity(20);
    for user in users {
        let barrier_clone = barrier.clone();
        let member_service_clone = member_service.clone();
        let room_id_clone = room_id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;

            // Use add_member_with_options with max_members check enabled
            let options = AddMemberOptions::default().with_max_members(0); // 0 = read from RoomSettings
            member_service_clone
                .add_member_with_options(room_id_clone, user.id, RoomRole::Member, options)
                .await
        });
        handles.push(handle);
    }

    // Collect results
    let mut success_count = 0;
    let mut limit_reached_count = 0;
    let mut other_errors = 0;

    for handle in handles {
        match ok(handle.await, "join task should complete") {
            Ok(_) => success_count += 1,
            Err(Error::InvalidInput(msg)) if msg.contains("Room is full") => {
                limit_reached_count += 1;
            }
            Err(e) => {
                tracing::warn!("Unexpected error: {:?}", e);
                other_errors += 1;
            }
        }
    }

    // Verify: With max_members=5 and owner already a member, only 4 more can join
    // Total = 5 (owner + 4 joiners)
    assert_eq!(
        success_count, 4,
        "Expected exactly 4 joins to succeed (room capacity 5, owner already member)"
    );
    assert_eq!(
        limit_reached_count, 16,
        "Expected 16 joins to be rejected (room full)"
    );
    assert_eq!(other_errors, 0, "No unexpected errors should occur");

    // Verify final member count
    let member_count: i64 = ok(
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM room_members WHERE room_id = $1"#,
            room.id.as_i64()
        )
        .fetch_one(pool)
        .await,
        "room member count should be fetched",
    );

    assert_eq!(
        member_count, 5,
        "Final member count should be 5 (owner + 4 joiners)"
    );
}

/// Test concurrent joins that exceed capacity exactly at boundary.
///
/// Scenario:
/// 1. Create room with `max_members` = 3
/// 2. Owner is already a member (count = 1)
/// 3. 5 users concurrently try to join
/// 4. Exactly 2 should succeed, 3 should be rejected
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_join_boundary_condition() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    // Room with 3 member limit
    let (_owner, room, _settings) = setup_test_room(pool, "Boundary Room", 3).await;

    let user_repo = UserRepository::new(pool.clone());
    let mut users = Vec::with_capacity(5);
    for i in 0..5 {
        let user = ok(
            user_repo.create(&make_user(&format!("boundary_{i}"))).await,
            "boundary user should be created",
        );
        users.push(user);
    }

    // Setup services
    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service = permission_service(member_repo.clone(), room_repo.clone());
    let member_service = MemberService::new_with_runtime(
        member_repo.clone(),
        room_repo.clone(),
        Some(room_settings_repo),
        permission_service.clone(),
        None,
        None,
        NotificationService::default(),
    );

    // Synchronize with barrier
    let barrier = Arc::new(Barrier::new(5));
    let room_id = room.id;

    let mut handles = Vec::with_capacity(5);
    for user in users {
        let barrier_clone = barrier.clone();
        let member_service_clone = member_service.clone();
        let room_id_clone = room_id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            let options = AddMemberOptions::default().with_max_members(0);
            member_service_clone
                .add_member_with_options(room_id_clone, user.id, RoomRole::Member, options)
                .await
        });
        handles.push(handle);
    }

    let mut success = 0;
    let mut rejected = 0;

    for handle in handles {
        match ok(handle.await, "boundary join task should complete") {
            Ok(_) => success += 1,
            Err(Error::InvalidInput(msg)) if msg.contains("Room is full") => rejected += 1,
            Err(e) => std::panic::panic_any(format!("unexpected error: {e:?}")),
        }
    }

    // With capacity 3 and owner already member, only 2 more can join
    assert_eq!(success, 2, "Exactly 2 should join successfully");
    assert_eq!(rejected, 3, "3 should be rejected (room full)");
}

/// Test that concurrent room creation with the same name by different users succeeds.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_room_creation_same_name_different_users() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let user_repo = UserRepository::new(pool.clone());
    let mut users = Vec::with_capacity(10);
    for i in 0..10 {
        let user = ok(
            user_repo.create(&make_user(&format!("creator_{i}"))).await,
            "creator user should be created",
        );
        users.push(user);
    }

    let room_repo = Arc::new(RoomRepository::new(pool.clone()));
    let barrier = Arc::new(Barrier::new(10));

    let room_name = "Same Name Room";
    let mut handles = Vec::with_capacity(10);

    for user in users {
        let room_repo_clone = room_repo.clone();
        let barrier_clone = barrier.clone();
        let room_name_clone = room_name.to_string();
        let user_id = user.id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            let room = make_room(&room_name_clone, "Created concurrently", &user_id);
            room_repo_clone.create(&room).await
        });
        handles.push(handle);
    }

    let mut success_count = 0;

    for handle in handles {
        match ok(handle.await, "room creation task should complete") {
            Ok(room) => {
                assert_eq!(room.name, room_name);
                success_count += 1;
            }
            Err(e) => std::panic::panic_any(format!("unexpected room creation error: {e:?}")),
        }
    }

    assert_eq!(success_count, 10, "All 10 rooms should be created");

    let persisted_count: i64 = ok(
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM rooms WHERE name = $1 AND deleted_at IS NULL"#,
            room_name
        )
        .fetch_one(pool)
        .await,
        "persisted room count should be fetched",
    );
    assert_eq!(persisted_count, 10, "All active rooms should persist");
}

/// Test that repository-level room creation does not enforce duplicate-name policy.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_room_creation_same_user_is_repository_allowed() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let user_repo = UserRepository::new(pool.clone());
    let user = ok(
        user_repo.create(&make_user("single_creator")).await,
        "single creator should be created",
    );

    let room_repo = Arc::new(RoomRepository::new(pool.clone()));
    let barrier = Arc::new(Barrier::new(5));
    let user_id = user.id;

    // Same user creates the same room 5 times concurrently.
    let mut handles = Vec::with_capacity(5);
    for _ in 0..5 {
        let room_repo_clone = room_repo.clone();
        let barrier_clone = barrier.clone();
        let user_id_clone = user_id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            let room = make_room("Repeated Room", "Repeated Description", &user_id_clone);
            room_repo_clone.create(&room).await
        });
        handles.push(handle);
    }

    let mut success_count = 0;
    for handle in handles {
        match ok(handle.await, "repeated room creation task should complete") {
            Ok(_) => success_count += 1,
            Err(e) => std::panic::panic_any(format!("unexpected room creation error: {e:?}")),
        }
    }

    assert_eq!(success_count, 5, "Repository should persist all rows");
    let persisted_count: i64 = ok(
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM rooms WHERE created_by = $1 AND name = $2 AND deleted_at IS NULL"#,
            user_id.as_i64(),
            "Repeated Room"
        )
        .fetch_one(pool)
        .await,
        "persisted repeated room count should be fetched",
    );
    assert_eq!(
        persisted_count, 5,
        "Room-name product policy belongs above the repository layer"
    );
}

/// Test optimistic lock retry on concurrent settings updates.
///
/// Scenario:
/// 1. Create room with settings
/// 2. Multiple tasks concurrently update settings
/// 3. Verify all updates succeed through retry mechanism
/// 4. Verify final state is consistent
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_settings_update_optimistic_lock_retry() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room, _settings) = setup_test_room(pool, "Settings Update Room", 100).await;

    let room_settings_repo = Arc::new(RoomSettingsRepository::new(pool.clone()));
    let barrier = Arc::new(Barrier::new(10));
    let room_id = room.id;

    // 10 concurrent updates to different settings
    let mut handles = Vec::with_capacity(10);
    for i in 0..10 {
        let repo_clone = room_settings_repo.clone();
        let barrier_clone = barrier.clone();
        let room_id_clone = room_id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;

            // Retry loop for optimistic lock conflicts
            let mut retries = 0;
            let max_retries = 3;
            loop {
                // Fetch current settings with version
                let (settings, version) = ok(
                    repo_clone.get_with_version(&room_id_clone).await,
                    "room settings should be fetched",
                );

                // Modify a different setting based on iteration
                let mut updated = settings.clone();
                updated.max_members = MaxMembers(50 + u64::try_from(i).unwrap_or_default());

                // Try to update with optimistic locking
                match repo_clone
                    .set_settings_with_version(&room_id_clone, &updated, version)
                    .await
                {
                    Ok(_) => break Ok(()),
                    Err(Error::OptimisticLockConflict) => {
                        retries += 1;
                        if retries >= max_retries {
                            break Err(Error::OptimisticLockConflict);
                        }
                        // Exponential backoff
                        tokio::time::sleep(tokio::time::Duration::from_millis(
                            5 * 2_u64.pow(retries),
                        ))
                        .await;
                    }
                    Err(e) => break Err(e),
                }
            }
        });
        handles.push(handle);
    }

    let mut success_count = 0;
    let mut conflict_count = 0;

    for handle in handles {
        match ok(handle.await, "settings update task should complete") {
            Ok(()) => success_count += 1,
            Err(Error::OptimisticLockConflict) => conflict_count += 1,
            Err(e) => std::panic::panic_any(format!("unexpected error: {e:?}")),
        }
    }

    // With retry mechanism, most or all should succeed
    assert!(
        success_count >= 3,
        "At least 3 updates should succeed with retry"
    );
    tracing::info!(
        "Concurrent settings updates: {} succeeded, {} failed after retries",
        success_count,
        conflict_count
    );

    // Verify final state is consistent
    let final_settings = ok(
        room_settings_repo.get(&room_id).await,
        "final room settings should be fetched",
    );
    assert!(
        final_settings.max_members.0 >= 50,
        "Final max_members should be updated"
    );
}

/// Test that stale version update is rejected.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_settings_update_stale_version_rejected() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room, _settings) = setup_test_room(pool, "Stale Version Room", 100).await;

    let room_settings_repo = RoomSettingsRepository::new(pool.clone());

    // Fetch settings twice with version (simulating two readers)
    let (settings_v0, version_v0) = ok(
        room_settings_repo.get_with_version(&room.id).await,
        "room settings should be fetched",
    );
    let version_v0_copy = version_v0;

    // First update succeeds
    let mut updated = settings_v0.clone();
    updated.max_members = MaxMembers(200);
    ok(
        room_settings_repo
            .set_settings_with_version(&room.id, &updated, version_v0)
            .await,
        "first room settings update should succeed",
    );

    // Second update with stale version should fail
    let mut stale_update = settings_v0.clone();
    stale_update.max_members = MaxMembers(300);
    let result = room_settings_repo
        .set_settings_with_version(&room.id, &stale_update, version_v0_copy)
        .await;

    assert!(
        matches!(result, Err(Error::OptimisticLockConflict)),
        "Update with stale version should fail with OptimisticLockConflict"
    );
}

/// Test concurrent join and leave operations don't cause race conditions.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_join_and_leave_operations() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    // Room with high capacity
    let (_owner, room, _settings) = setup_test_room(pool, "Join Leave Room", 100).await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let mut users = Vec::with_capacity(10);
    for i in 0..10 {
        let user = ok(
            user_repo
                .create(&make_user(&format!("join_leave_{i}")))
                .await,
            "join/leave user should be created",
        );
        let member = RoomMember::new(room.id, user.id, RoomRole::Member);
        ok(member_repo.add(&member).await, "member should be added");
        users.push(user);
    }

    // Now concurrently: 5 leave, 5 new join
    let barrier = Arc::new(Barrier::new(10));
    let room_id = room.id;

    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service = permission_service(member_repo.clone(), room_repo.clone());
    let member_service = MemberService::new_with_runtime(
        member_repo.clone(),
        room_repo.clone(),
        Some(room_settings_repo),
        permission_service.clone(),
        None,
        None,
        NotificationService::default(),
    );

    let mut leave_handles = Vec::with_capacity(5);

    // First 5 leave
    for user in users.iter().take(5) {
        let member_repo_clone = member_repo.clone();
        let barrier_clone = barrier.clone();
        let room_id_clone = room_id;
        let user_id = user.id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            member_repo_clone.remove(&room_id_clone, &user_id).await
        });
        leave_handles.push(handle);
    }

    let mut new_users = Vec::with_capacity(5);
    for i in 0..5 {
        let user = ok(
            user_repo
                .create(&make_user(&format!("new_joiner_{i}")))
                .await,
            "new joiner should be created",
        );
        new_users.push(user);
    }

    let mut join_handles = Vec::with_capacity(5);

    // 5 new join
    for user in new_users {
        let member_service_clone = member_service.clone();
        let barrier_clone = barrier.clone();
        let room_id_clone = room_id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            let options = AddMemberOptions::default().with_max_members(0);
            member_service_clone
                .add_member_with_options(room_id_clone, user.id, RoomRole::Member, options)
                .await
        });
        join_handles.push(handle);
    }

    let mut leave_success = 0;
    for handle in leave_handles {
        match ok(handle.await, "leave task should complete") {
            Ok(_) => leave_success += 1,
            Err(e) => tracing::warn!("Leave operation failed: {:?}", e),
        }
    }

    let mut join_success = 0;
    for handle in join_handles {
        match ok(handle.await, "join task should complete") {
            Ok(_) => join_success += 1,
            Err(e) => tracing::warn!("Join operation failed: {:?}", e),
        }
    }

    // All operations should succeed
    assert_eq!(leave_success, 5, "All leave operations should succeed");
    assert_eq!(join_success, 5, "All join operations should succeed");

    // Final count: owner (1) + 5 remaining original + 5 new = 11
    let final_count: i64 = ok(
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM room_members WHERE room_id = $1"#,
            room.id.as_i64()
        )
        .fetch_one(pool)
        .await,
        "final member count should be fetched",
    );

    assert_eq!(final_count, 11, "Final member count should be 11");
}

/// Concurrent joins must still respect the room capacity limit.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_joins_respect_room_capacity() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    // Room with capacity 50
    let (_owner, room, _settings) = setup_test_room(pool, "Concurrent Join Room", 50).await;

    let user_repo = UserRepository::new(pool.clone());
    let mut users = Vec::with_capacity(100);
    for i in 0..100 {
        let user = ok(
            user_repo
                .create(&make_user(&format!("concurrent_join_{i}")))
                .await,
            "concurrent join user should be created",
        );
        users.push(user);
    }

    // Setup services
    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service = permission_service(member_repo.clone(), room_repo.clone());
    let member_service = MemberService::new_with_runtime(
        member_repo.clone(),
        room_repo.clone(),
        Some(room_settings_repo),
        permission_service.clone(),
        None,
        None,
        NotificationService::default(),
    );

    let barrier = Arc::new(Barrier::new(100));
    let room_id = room.id;

    let mut handles = Vec::with_capacity(100);
    for user in users {
        let member_service_clone = member_service.clone();
        let barrier_clone = barrier.clone();
        let room_id_clone = room_id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            let options = AddMemberOptions::default().with_max_members(0);
            member_service_clone
                .add_member_with_options(room_id_clone, user.id, RoomRole::Member, options)
                .await
        });
        handles.push(handle);
    }

    let mut success = 0;
    let mut rejected = 0;

    for handle in handles {
        match ok(handle.await, "capacity join task should complete") {
            Ok(_) => success += 1,
            Err(Error::InvalidInput(msg)) if msg.contains("Room is full") => rejected += 1,
            Err(e) => tracing::warn!("Unexpected error: {:?}", e),
        }
    }

    // With capacity 50 and owner already member, 49 more can join
    assert_eq!(success, 49, "49 users should join successfully");
    assert_eq!(rejected, 51, "51 users should be rejected (room full)");

    tracing::info!(
        "Concurrent join results: {} succeeded, {} rejected",
        success,
        rejected
    );
}
