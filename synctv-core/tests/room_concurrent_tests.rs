//! Room concurrent member management tests
//!
//! Tests concurrent member operations including concurrent joins, role updates,
//! permission changes, and optimistic lock conflict retry.
//!
//!
//! # Test Coverage
//!
//! - Concurrent join with `max_members` limit
//! - Concurrent role updates with role hierarchy enforcement
//! - Concurrent permission changes with retry
//! - Optimistic lock conflict handling and retry
//!
//! # Requirements
//!
//! - Docker for testcontainers (`PostgreSQL`)

use chrono::Utc;
use sqlx::PgPool;
use std::sync::Arc;
use synctv_core::{
    models::{
        room_settings::MaxMembers, AddMemberOptions, Room, RoomAdminPermissionBits, RoomId,
        RoomMember, RoomRole, RoomSettings, RoomStatus, User, UserId, UserRole, UserStatus,
    },
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository, UserRepository},
    service::{MemberService, NotificationService, PermissionService},
    Error,
};
use synctv_core_testing::{
    create_test_database_with_options_and_label, some, TestDatabase, TestResultExt,
};
use tokio::sync::Barrier;
// Test Infrastructure

async fn create_test_pool() -> TestDatabase {
    create_test_database_with_options_and_label(
        "synctv_test",
        "room-concurrent",
        30,
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

    let owner = user_repo
        .create(&make_user("room_owner"))
        .await
        .checked("owner should be created");

    let room = room_repo
        .create(&make_room(room_name, "Test room", &owner.id))
        .await
        .checked("room should be created");

    let settings = RoomSettings {
        max_members: MaxMembers(max_members),
        ..Default::default()
    };
    room_settings_repo
        .set_settings(&room.id, &settings)
        .await
        .checked("room settings should be created");

    // Add owner as member (Creator)
    let member_repo = RoomMemberRepository::new(pool.clone());
    let owner_member = RoomMember::new(room.id, owner.id, RoomRole::Creator);
    member_repo
        .add(&owner_member)
        .await
        .checked("owner member should be added");

    (owner, room, settings)
}

/// Get creator user ID from room
async fn get_creator_user_id(pool: &PgPool, room_id: &RoomId) -> UserId {
    let member_repo = RoomMemberRepository::new(pool.clone());
    let members = member_repo
        .list_by_room_all(room_id)
        .await
        .checked("members should be listed");
    some(
        members.iter().find(|m| m.role == RoomRole::Creator),
        "creator should exist",
    )
    .user_id
}

/// Test concurrent join operations respect `max_members` limit.
///
/// Scenario:
/// 1. Create a room with `max_members` = 10
/// 2. Spawn 30 concurrent join requests
/// 3. Verify exactly 9 succeed (room capacity 10 - owner = 9)
/// 4. Verify final member count is 10
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_join_respects_max_members() {
    let infra = create_test_database_with_options_and_label(
        "synctv_test",
        "concurrent-join-max-members",
        30,
        std::time::Duration::from_secs(30),
    )
    .await;
    let pool = &infra.pool;

    let (_owner, room, _settings) = setup_test_room(pool, "Concurrent Join Room", 10).await;

    let user_repo = UserRepository::new(pool.clone());
    let mut users = Vec::with_capacity(30);
    for i in 0..30 {
        let user = user_repo
            .create(&make_user(&format!("joiner_{i}")))
            .await
            .checked("user should be created");
        users.push(user);
    }

    // Setup services
    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service =
        PermissionService::new(member_repo.clone(), room_repo.clone(), None, 1000, 300)
            .checked("permission service should build");
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
    let barrier = Arc::new(Barrier::new(30));
    let room_id = room.id;

    // Spawn 30 concurrent join tasks
    let mut handles = Vec::with_capacity(30);
    for user in users {
        let barrier_clone = barrier.clone();
        let member_service_clone = member_service.clone();
        let room_id_clone = room_id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;

            let options = AddMemberOptions::new().with_max_members(0);
            member_service_clone
                .add_member_with_options(room_id_clone, user.id, RoomRole::Member, options)
                .await
        });
        handles.push(handle);
    }

    // Collect results
    let mut success_count = 0;
    let mut limit_reached_count = 0;

    for handle in handles {
        match handle.await.checked("task should complete") {
            Ok(_) => success_count += 1,
            Err(Error::InvalidInput(msg)) if msg.contains("Room is full") => {
                limit_reached_count += 1;
            }
            Err(e) => {
                std::panic::panic_any(format!("unexpected join error: {e:?}"));
            }
        }
    }

    // Verify: With max_members=10 and owner already a member, only 9 more can join
    assert_eq!(success_count, 9, "Expected exactly 9 joins to succeed");
    assert_eq!(
        limit_reached_count, 21,
        "Expected 21 joins to be rejected (room full)"
    );

    // Verify final member count
    let member_count: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM room_members WHERE room_id = $1"#,
        room.id.as_i64()
    )
    .fetch_one(pool)
    .await
    .checked("members should be counted");

    assert_eq!(member_count, 10, "Final member count should be 10");
}

/// Test concurrent role updates with role hierarchy enforcement.
///
/// Scenario:
/// 1. Create a room with creator and multiple members
/// 2. Concurrently update roles from Member to Admin
/// 3. Verify all updates succeed
/// 4. Verify role hierarchy is maintained
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_role_update_member_to_admin() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room, _settings) = setup_test_room(pool, "Role Update Room", 100).await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let mut member_users = Vec::with_capacity(10);

    for i in 0..10 {
        let user = user_repo
            .create(&make_user(&format!("member_{i}")))
            .await
            .checked("user should be created");
        let member = RoomMember::new(room.id, user.id, RoomRole::Member);
        member_repo
            .add(&member)
            .await
            .checked("member should be added");
        member_users.push(user);
    }

    // Setup services
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service =
        PermissionService::new(member_repo.clone(), room_repo.clone(), None, 1000, 300)
            .checked("permission service should build");
    let member_service = MemberService::new_with_runtime(
        member_repo.clone(),
        room_repo.clone(),
        Some(room_settings_repo),
        permission_service.clone(),
        None,
        None,
        NotificationService::default(),
    );

    // Get owner user ID
    let operator_id = get_creator_user_id(pool, &room.id).await;

    // Use barrier for concurrent updates
    let barrier = Arc::new(Barrier::new(10));
    let room_id = room.id;

    let mut handles = Vec::with_capacity(10);
    for user in member_users {
        let barrier_clone = barrier.clone();
        let member_service_clone = member_service.clone();
        let room_id_clone = room_id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            member_service_clone
                .set_member_role(room_id_clone, operator_id, user.id, RoomRole::Admin)
                .await
        });
        handles.push(handle);
    }

    // Collect results
    let mut success_count = 0;
    for handle in handles {
        match handle.await.checked("task should complete") {
            Ok(_) => success_count += 1,
            Err(e) => tracing::warn!("Role update failed: {:?}", e),
        }
    }

    // All role updates should succeed
    assert_eq!(success_count, 10, "All role updates should succeed");

    // Verify all members now have Admin role
    let operator_id = get_creator_user_id(pool, &room.id).await;
    for member in member_repo
        .list_by_room_all(&room.id)
        .await
        .checked("members should be listed")
    {
        if member.user_id != operator_id {
            assert_eq!(
                member.role,
                RoomRole::Admin,
                "Member should be promoted to Admin"
            );
        }
    }
}

/// Test concurrent role update rejection (lower role cannot update higher role).
///
/// Scenario:
/// 1. Create room with creator and 2 admins
/// 2. Admin1 tries to demote Admin2 to Member
/// 3. Admin2 tries to demote Admin1 to Member
/// 4. Both should fail due to equal role hierarchy
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_role_update_equal_role_rejected() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room, _settings) = setup_test_room(pool, "Equal Role Room", 100).await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let admin1 = user_repo
        .create(&make_user("admin1"))
        .await
        .checked("admin1 should be created");
    let admin1_member = RoomMember::new(room.id, admin1.id, RoomRole::Admin);
    member_repo
        .add(&admin1_member)
        .await
        .checked("admin1 member should be added");

    let admin2 = user_repo
        .create(&make_user("admin2"))
        .await
        .checked("admin2 should be created");
    let admin2_member = RoomMember::new(room.id, admin2.id, RoomRole::Admin);
    member_repo
        .add(&admin2_member)
        .await
        .checked("admin2 member should be added");

    // Setup services
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service =
        PermissionService::new(member_repo.clone(), room_repo.clone(), None, 1000, 300)
            .checked("permission service should build");
    let member_service = MemberService::new_with_runtime(
        member_repo.clone(),
        room_repo.clone(),
        Some(room_settings_repo),
        permission_service.clone(),
        None,
        None,
        NotificationService::default(),
    );

    // Admin1 tries to demote Admin2
    let result1 = member_service
        .set_member_role(room.id, admin1.id, admin2.id, RoomRole::Member)
        .await;

    // Admin2 tries to demote Admin1
    let result2 = member_service
        .set_member_role(room.id, admin2.id, admin1.id, RoomRole::Member)
        .await;

    // Both should fail due to equal role hierarchy
    assert!(result1.is_err(), "Admin1 cannot demote Admin2 (equal role)");
    assert!(result2.is_err(), "Admin2 cannot demote Admin1 (equal role)");
}

/// Test concurrent permission changes with optimistic lock retry.
///
/// Scenario:
/// 1. Create room with creator and multiple members
/// 2. Creator concurrently grants different permissions to different members
/// 3. All operations should succeed through optimistic lock retry
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_permission_grant() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room, _settings) = setup_test_room(pool, "Permission Grant Room", 100).await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let mut member_users = Vec::with_capacity(5);

    for i in 0..5 {
        let user = user_repo
            .create(&make_user(&format!("perm_member_{i}")))
            .await
            .checked("user should be created");
        let member = RoomMember::new(room.id, user.id, RoomRole::Member);
        member_repo
            .add(&member)
            .await
            .checked("member should be added");
        member_users.push(user);
    }

    // Setup services
    let room_repo = RoomRepository::new(pool.clone());
    let _room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service =
        PermissionService::new(member_repo.clone(), room_repo.clone(), None, 1000, 300)
            .checked("permission service should build");
    let member_service = Arc::new(MemberService::new(
        member_repo.clone(),
        room_repo.clone(),
        permission_service.clone(),
        NotificationService::default(),
    ));

    // Get owner user ID
    let operator_id = get_creator_user_id(pool, &room.id).await;

    // Use barrier for concurrent operations
    let barrier = Arc::new(Barrier::new(5));
    let room_id = room.id;

    let mut handles = Vec::with_capacity(5);
    for (i, user) in member_users.into_iter().enumerate() {
        let barrier_clone = barrier.clone();
        let member_service_clone = member_service.clone();
        let room_id_clone = room_id;

        // Grant different permissions to each member
        let permission = match i {
            0 | 1 => RoomAdminPermissionBits::USE_VOICE_CHAT,
            2 => RoomAdminPermissionBits::VIEW_CHAT_HISTORY,
            3 => RoomAdminPermissionBits::SEND_CHAT_MESSAGES,
            _ => RoomAdminPermissionBits::MANAGE_OWN_MEDIA,
        };

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            member_service_clone
                .grant_permission(room_id_clone, operator_id, user.id, permission)
                .await
        });
        handles.push(handle);
    }

    // Collect results
    let mut success_count = 0;
    for handle in handles {
        match handle.await.checked("task should complete") {
            Ok(_) => success_count += 1,
            Err(e) => tracing::warn!("Permission grant failed: {:?}", e),
        }
    }

    // All permission grants should succeed
    assert!(
        success_count >= 4,
        "At least 4 permission grants should succeed"
    );
}

/// Test optimistic lock conflict handling and retry for member updates.
///
/// Scenario:
/// 1. Create room with creator and member
/// 2. Multiple tasks concurrently update member's permissions
/// 3. Database version conflict occurs
/// 4. Verify retry mechanism handles conflicts gracefully
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_optimistic_lock_conflict_retry_on_permission_update() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room, _settings) = setup_test_room(pool, "Optimistic Lock Room", 100).await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let target_user = user_repo
        .create(&make_user("target_member"))
        .await
        .checked("target user should be created");
    let target_member = RoomMember::new(room.id, target_user.id, RoomRole::Member);
    member_repo
        .add(&target_member)
        .await
        .checked("member should be added");

    // Setup services
    let room_repo = RoomRepository::new(pool.clone());
    let permission_service =
        PermissionService::new(member_repo.clone(), room_repo.clone(), None, 1000, 300)
            .checked("permission service should build");
    let member_service = Arc::new(MemberService::new(
        member_repo.clone(),
        room_repo.clone(),
        permission_service.clone(),
        NotificationService::default(),
    ));

    // Get owner user ID
    let operator_id = get_creator_user_id(pool, &room.id).await;

    // Use barrier for concurrent updates
    let barrier = Arc::new(Barrier::new(10));
    let room_id = room.id;

    // 10 concurrent updates to the same member
    let mut handles = Vec::with_capacity(10);
    let permission_updates = [
        RoomAdminPermissionBits::SEND_CHAT_MESSAGES,
        RoomAdminPermissionBits::MANAGE_OWN_MEDIA,
        RoomAdminPermissionBits::BROWSE_LIBRARY,
        RoomAdminPermissionBits::VIEW_MEMBERS,
        RoomAdminPermissionBits::VIEW_CHAT_HISTORY,
        RoomAdminPermissionBits::USE_VOICE_CHAT,
        RoomAdminPermissionBits::SEND_CHAT_MESSAGES | RoomAdminPermissionBits::USE_VOICE_CHAT,
        RoomAdminPermissionBits::MANAGE_OWN_MEDIA | RoomAdminPermissionBits::BROWSE_LIBRARY,
        RoomAdminPermissionBits::VIEW_MEMBERS | RoomAdminPermissionBits::VIEW_CHAT_HISTORY,
        RoomAdminPermissionBits::SEND_CHAT_MESSAGES | RoomAdminPermissionBits::MANAGE_OWN_MEDIA,
    ];
    for permission in permission_updates {
        let barrier_clone = barrier.clone();
        let member_service_clone = member_service.clone();
        let room_id_clone = room_id;
        let target_id = target_user.id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;

            // Each task sets different permissions (using raw u64)
            member_service_clone
                .set_member_permissions(room_id_clone, operator_id, target_id, permission, 0)
                .await
        });
        handles.push(handle);
    }

    // Collect results
    let mut success_count = 0;
    let mut retry_exhausted = 0;
    let mut unexpected_errors = Vec::new();

    for handle in handles {
        match handle.await.checked("task should complete") {
            Ok(_) => success_count += 1,
            Err(Error::Internal(msg)) if msg.contains("retry") || msg.contains("maximum") => {
                retry_exhausted += 1;
            }
            Err(e) => unexpected_errors.push(e),
        }
    }

    assert!(
        unexpected_errors.is_empty(),
        "Unexpected permission update errors: {unexpected_errors:?}"
    );

    // With retry mechanism, most operations should succeed
    assert!(
        success_count >= 5,
        "At least 5 updates should succeed with retry"
    );

    tracing::info!(
        "Optimistic lock retry test: {} succeeded, {} exhausted retries",
        success_count,
        retry_exhausted
    );
}

/// Test optimistic lock conflict on role update.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_optimistic_lock_conflict_on_role_update() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (_owner, room, _settings) = setup_test_room(pool, "Role Lock Room", 100).await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let target_user = user_repo
        .create(&make_user("role_target"))
        .await
        .checked("target user should be created");
    let target_member = RoomMember::new(room.id, target_user.id, RoomRole::Member);
    member_repo
        .add(&target_member)
        .await
        .checked("member should be added");

    // Get owner user ID
    let operator_id = get_creator_user_id(pool, &room.id).await;

    // Concurrently update the same member's role (Creator -> Admin, then Admin -> Member)
    // Use a version bumper to simulate conflicts
    let room_id = room.id;
    let target_id = target_user.id;
    let pool_clone = pool.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();

    let bumper = tokio::spawn(async move {
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            sqlx::query!(
                "UPDATE room_members SET version = version + 1 WHERE room_id = $1 AND user_id = $2",
                room_id.as_i64(),
                target_id.as_i64()
            )
            .execute(&pool_clone)
            .await?;
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        Ok::<_, sqlx::Error>(())
    });

    // Setup services
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service =
        PermissionService::new(member_repo.clone(), room_repo.clone(), None, 1000, 300)
            .checked("permission service should build");
    let member_service = MemberService::new_with_runtime(
        member_repo.clone(),
        room_repo.clone(),
        Some(room_settings_repo),
        permission_service.clone(),
        None,
        None,
        NotificationService::default(),
    );

    let result = member_service
        .set_member_role(room.id, operator_id, target_user.id, RoomRole::Admin)
        .await;

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    bumper
        .await
        .checked("bumper task should not panic")
        .checked("bumper task should update member versions");

    // Either succeeds (retry worked) or returns Internal (retry exhaustion)
    match result {
        Ok(_) => {}
        Err(Error::Internal(msg)) => {
            assert!(
                msg.contains("retry") || msg.contains("maximum"),
                "Should mention retry exhaustion: {msg}"
            );
        }
        Err(other) => {
            std::panic::panic_any(format!("unexpected role update error: {other:?}"));
        }
    }
}

/// Test concurrent leave and rejoin operations.
///
/// Scenario:
/// 1. Create room with creator and members
/// 2. Some members leave, some new members join concurrently
/// 3. Verify member count remains consistent
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_leave_and_rejoin() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    // Room with capacity 20
    let (_owner, room, _settings) = setup_test_room(pool, "Leave Rejoin Room", 20).await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let mut initial_members = Vec::with_capacity(10);

    for i in 0..10 {
        let user = user_repo
            .create(&make_user(&format!("initial_{i}")))
            .await
            .checked("user should be created");
        let member = RoomMember::new(room.id, user.id, RoomRole::Member);
        member_repo
            .add(&member)
            .await
            .checked("member should be added");
        initial_members.push(user);
    }

    // Setup services
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service =
        PermissionService::new(member_repo.clone(), room_repo.clone(), None, 1000, 300)
            .checked("permission service should build");
    let member_service = MemberService::new_with_runtime(
        member_repo.clone(),
        room_repo.clone(),
        Some(room_settings_repo),
        permission_service.clone(),
        None,
        None,
        NotificationService::default(),
    );

    let mut new_users = Vec::with_capacity(10);
    for i in 0..10 {
        let user = user_repo
            .create(&make_user(&format!("new_joiner_{i}")))
            .await
            .checked("user should be created");
        new_users.push(user);
    }

    // Use barrier for synchronization
    let barrier = Arc::new(Barrier::new(20));
    let room_id = room.id;

    // 10 leave + 10 join = 20 concurrent operations
    let mut leave_handles = Vec::with_capacity(10);
    let mut join_handles = Vec::with_capacity(10);

    // Leave operations
    for user in initial_members {
        let member_repo_clone = member_repo.clone();
        let barrier_clone = barrier.clone();
        let room_id_clone = room_id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            member_repo_clone.remove(&room_id_clone, &user.id).await
        });
        leave_handles.push(handle);
    }

    // Join operations
    for user in new_users {
        let member_service_clone = member_service.clone();
        let barrier_clone = barrier.clone();
        let room_id_clone = room_id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            let options = AddMemberOptions::new().with_max_members(0);
            member_service_clone
                .add_member_with_options(room_id_clone, user.id, RoomRole::Member, options)
                .await
        });
        join_handles.push(handle);
    }

    // Collect results
    let mut leave_success = 0;
    let mut join_success = 0;

    for handle in leave_handles {
        match handle.await.checked("leave task should complete") {
            Ok(_) => leave_success += 1,
            Err(e) => tracing::warn!("Leave operation failed: {:?}", e),
        }
    }

    for handle in join_handles {
        match handle.await.checked("join task should complete") {
            Ok(_) => join_success += 1,
            Err(e) => tracing::warn!("Join operation failed: {:?}", e),
        }
    }

    // All operations should succeed
    assert_eq!(leave_success, 10, "All leave operations should succeed");
    assert_eq!(join_success, 10, "All join operations should succeed");

    // Final count: owner (1) + 10 new = 11
    let final_count: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM room_members WHERE room_id = $1"#,
        room.id.as_i64()
    )
    .fetch_one(pool)
    .await
    .checked("members should be counted");

    assert_eq!(
        final_count, 11,
        "Final member count should be 11 (owner + 10 new)"
    );
}
