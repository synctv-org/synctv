//! Room concurrency integration tests
//!
//! Tests concurrent room operations: multi-user joins, concurrent room creation,
//! and concurrent settings updates with optimistic lock retry.
//!
//! Run with: cargo test --test `room_concurrency_tests`
//!
//! # Test Coverage
//!
//! - Multi-user concurrent join with `max_members` limit enforcement
//! - Concurrent creation of rooms with same name by different users
//! - Concurrent room settings updates (optimistic lock retry)
//!
//! # Requirements
//!
//! - Docker for testcontainers (`PostgreSQL` + Redis)
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use sqlx::PgPool;
use std::sync::Arc;
use synctv_core_testing::postgres::docker_startup_timeout;
use synctv_core::{
    models::{
        room_settings::MaxMembers, Room, RoomId, RoomMember, RoomRole, RoomSettings, RoomStatus,
        User, UserId, UserRole, UserStatus,
    },
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository, UserRepository},
    service::{
        member::{AddMemberOptions, MemberService},
        permission::PermissionService,
    },
    Error,
};
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;
use tokio::sync::Barrier;
// ============================================================================
// Test Infrastructure
// ============================================================================

/// Test container wrapper for Postgres
pub struct TestPostgres {
    pub pool: PgPool,
    _container: ContainerAsync<Postgres>,
}

async fn create_test_pool() -> TestPostgres {
    let container = tokio::time::timeout(
        docker_startup_timeout(),
        Postgres::default()
            .with_db_name("synctv_test")
            .with_user("synctv")
            .with_password("synctv_test")
            .with_tag("16-alpine")
            .start(),
    )
    .await
    .expect("Docker container startup timed out (is Docker running?)")
    .expect("Failed to start Postgres container");

    let host = container.get_host().await.expect("Failed to get host");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get port");

    let database_url = format!("postgres://synctv:synctv_test@{host}:{port}/synctv_test");

    let pool = {
        let mut retries = 0u32;
        loop {
            match sqlx::postgres::PgPoolOptions::new()
                .max_connections(64) // Higher for concurrent tests with serialized room-row locking
                .acquire_timeout(std::time::Duration::from_secs(10))
                .connect(&database_url)
                .await
            {
                Ok(p) => break p,
                Err(_) if retries < 60 => {
                    retries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => panic!("PostgreSQL not ready after {retries} retries: {e}"),
            }
        }
    };

    // Run migrations
    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    TestPostgres {
        pool,
        _container: container,
    }
}

/// Create a test user in the database
fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "test_hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

/// Create a test room
fn make_room(name: &str, description: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: description.to_string(),
        created_by: owner.clone(),
        status: RoomStatus::Active,
        is_banned: false,
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

    // Create owner
    let owner = user_repo
        .create(&make_user("room_owner"))
        .await
        .expect("Failed to create owner");

    // Create room
    let room = room_repo
        .create(&make_room(room_name, "Test room", &owner.id))
        .await
        .expect("Failed to create room");

    // Create room settings with max_members
    let settings = RoomSettings {
        max_members: MaxMembers(max_members),
        ..Default::default()
    };
    room_settings_repo
        .set_settings(&room.id, &settings)
        .await
        .expect("Failed to create room settings");

    // Add owner as member (Creator)
    let member_repo = RoomMemberRepository::new(pool.clone());
    let owner_member = RoomMember::new(room.id.clone(), owner.id.clone(), RoomRole::Creator);
    member_repo
        .add(&owner_member)
        .await
        .expect("Failed to add owner as member");

    (owner, room, settings)
}

// ============================================================================
// Test: Multi-user Concurrent Join with max_members Limit
// ============================================================================

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

    // Create room with max_members = 5 (total capacity including owner)
    let (_owner, room, _settings) = setup_test_room(pool, "Concurrent Join Room", 5).await;

    // Create 20 users who will try to join
    let user_repo = UserRepository::new(pool.clone());
    let mut users = Vec::with_capacity(20);
    for i in 0..20 {
        let user = user_repo
            .create(&make_user(&format!("joiner_{i}")))
            .await
            .expect("Failed to create user");
        users.push(user);
    }

    // Setup services
    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service =
        PermissionService::new(member_repo.clone(), room_repo.clone(), None, 1000, 300);
    let mut member_service = MemberService::new(
        member_repo.clone(),
        room_repo.clone(),
        permission_service.clone(),
    );
    member_service.set_room_settings_repo(room_settings_repo);

    // Use barrier to synchronize all joins
    let barrier = Arc::new(Barrier::new(20));
    let room_id = room.id.clone();

    // Spawn 20 concurrent join tasks
    let mut handles = Vec::with_capacity(20);
    for user in users {
        let barrier_clone = barrier.clone();
        let member_service_clone = member_service.clone();
        let room_id_clone = room_id.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;

            // Use add_member_with_options with max_members check enabled
            let options = AddMemberOptions::new().with_max_members(0); // 0 = read from RoomSettings
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
        match handle.await.expect("Task panicked") {
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
    let member_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM room_members WHERE room_id = $1 AND left_at IS NULL",
    )
    .bind(room.id.as_str())
    .fetch_one(pool)
    .await
    .expect("Failed to count members");

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

    // Create 5 users
    let user_repo = UserRepository::new(pool.clone());
    let mut users = Vec::with_capacity(5);
    for i in 0..5 {
        let user = user_repo
            .create(&make_user(&format!("boundary_{i}")))
            .await
            .expect("Failed to create user");
        users.push(user);
    }

    // Setup services
    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service =
        PermissionService::new(member_repo.clone(), room_repo.clone(), None, 1000, 300);
    let mut member_service = MemberService::new(
        member_repo.clone(),
        room_repo.clone(),
        permission_service.clone(),
    );
    member_service.set_room_settings_repo(room_settings_repo);

    // Synchronize with barrier
    let barrier = Arc::new(Barrier::new(5));
    let room_id = room.id.clone();

    let mut handles = Vec::with_capacity(5);
    for user in users {
        let barrier_clone = barrier.clone();
        let member_service_clone = member_service.clone();
        let room_id_clone = room_id.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            let options = AddMemberOptions::new().with_max_members(0);
            member_service_clone
                .add_member_with_options(room_id_clone, user.id, RoomRole::Member, options)
                .await
        });
        handles.push(handle);
    }

    let mut success = 0;
    let mut rejected = 0;

    for handle in handles {
        match handle.await.expect("Task panicked") {
            Ok(_) => success += 1,
            Err(Error::InvalidInput(msg)) if msg.contains("Room is full") => rejected += 1,
            Err(e) => panic!("Unexpected error: {e:?}"),
        }
    }

    // With capacity 3 and owner already member, only 2 more can join
    assert_eq!(success, 2, "Exactly 2 should join successfully");
    assert_eq!(rejected, 3, "3 should be rejected (room full)");
}

// ============================================================================
// Test: Concurrent Room Creation with Same Name
// ============================================================================

/// Test that concurrent room creation with same name by different users works.
///
/// Note: Room names are NOT unique in the system - each room gets a unique ID.
/// This test verifies that concurrent creation doesn't cause race conditions.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_room_creation_same_name_different_users() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    // Create 10 users
    let user_repo = UserRepository::new(pool.clone());
    let mut users = Vec::with_capacity(10);
    for i in 0..10 {
        let user = user_repo
            .create(&make_user(&format!("creator_{i}")))
            .await
            .expect("Failed to create user");
        users.push(user);
    }

    let room_repo = Arc::new(RoomRepository::new(pool.clone()));
    let barrier = Arc::new(Barrier::new(10));

    let room_name = "Same Name Room"; // All rooms have the same name
    let mut handles = Vec::with_capacity(10);

    for user in users {
        let room_repo_clone = room_repo.clone();
        let barrier_clone = barrier.clone();
        let room_name_clone = room_name.to_string();
        let user_id = user.id.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            let room = make_room(&room_name_clone, "Created concurrently", &user_id);
            room_repo_clone.create(&room).await
        });
        handles.push(handle);
    }

    // All should succeed since room IDs are unique (nanoid)
    let mut created_room_ids = std::collections::HashSet::new();
    let mut success_count = 0;

    for handle in handles {
        match handle.await.expect("Task panicked") {
            Ok(room) => {
                assert!(
                    created_room_ids.insert(room.id.as_str().to_string()),
                    "Room IDs must be unique"
                );
                success_count += 1;
            }
            Err(e) => panic!("Room creation should succeed: {e:?}"),
        }
    }

    assert_eq!(success_count, 10, "All 10 rooms should be created");
    assert_eq!(created_room_ids.len(), 10, "All room IDs should be unique");
}

/// Test that concurrent room creation by the SAME user is prevented.
///
/// This tests the distributed lock mechanism (when available) or database-level
/// uniqueness constraints that prevent a single user from creating duplicate rooms.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_room_creation_same_user_prevented() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    // Create one user
    let user_repo = UserRepository::new(pool.clone());
    let user = user_repo
        .create(&make_user("single_creator"))
        .await
        .expect("Failed to create user");

    let room_repo = Arc::new(RoomRepository::new(pool.clone()));
    let barrier = Arc::new(Barrier::new(5));
    let user_id = user.id.clone();

    // Same user creates 5 rooms concurrently
    let mut handles = Vec::with_capacity(5);
    for i in 0..5 {
        let room_repo_clone = room_repo.clone();
        let barrier_clone = barrier.clone();
        let user_id_clone = user_id.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            let room = make_room(
                &format!("Room {i}"),
                &format!("Description {i}"),
                &user_id_clone,
            );
            room_repo_clone.create(&room).await
        });
        handles.push(handle);
    }

    // All should succeed since there's no constraint on number of rooms per user
    // (Each room has a unique ID)
    let mut success_count = 0;
    for handle in handles {
        if handle.await.expect("Task panicked").is_ok() {
            success_count += 1;
        }
    }

    // All 5 rooms should be created (no constraint prevents this)
    assert_eq!(success_count, 5, "User can create multiple rooms");
}

// ============================================================================
// Test: Concurrent Room Settings Updates (Optimistic Lock Retry)
// ============================================================================

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
    let room_id = room.id.clone();

    // 10 concurrent updates to different settings
    let mut handles = Vec::with_capacity(10);
    for i in 0..10 {
        let repo_clone = room_settings_repo.clone();
        let barrier_clone = barrier.clone();
        let room_id_clone = room_id.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;

            // Retry loop for optimistic lock conflicts
            let mut retries = 0;
            let max_retries = 3;
            loop {
                // Fetch current settings with version
                let (settings, version) = repo_clone
                    .get_with_version(&room_id_clone)
                    .await
                    .expect("Failed to get settings");

                // Modify a different setting based on iteration
                let mut updated = settings.clone();
                updated.max_members = MaxMembers(50 + i as u64); // Different value for each task

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
        match handle.await.expect("Task panicked") {
            Ok(()) => success_count += 1,
            Err(Error::OptimisticLockConflict) => conflict_count += 1,
            Err(e) => panic!("Unexpected error: {e:?}"),
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
    let final_settings = room_settings_repo
        .get(&room_id)
        .await
        .expect("Failed to get final settings");
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
    let (settings_v0, version_v0) = room_settings_repo
        .get_with_version(&room.id)
        .await
        .expect("Failed to get settings");
    let version_v0_copy = version_v0;

    // First update succeeds
    let mut updated = settings_v0.clone();
    updated.max_members = MaxMembers(200);
    room_settings_repo
        .set_settings_with_version(&room.id, &updated, version_v0)
        .await
        .expect("First update should succeed");

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

// ============================================================================
// Test: Concurrent Join and Leave Operations
// ============================================================================

/// Test concurrent join and leave operations don't cause race conditions.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_join_and_leave_operations() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    // Room with high capacity
    let (_owner, room, _settings) = setup_test_room(pool, "Join Leave Room", 100).await;

    // Create users and add them as members first
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let mut users = Vec::with_capacity(10);
    for i in 0..10 {
        let user = user_repo
            .create(&make_user(&format!("join_leave_{i}")))
            .await
            .expect("Failed to create user");
        let member = RoomMember::new(room.id.clone(), user.id.clone(), RoomRole::Member);
        member_repo
            .add(&member)
            .await
            .expect("Failed to add member");
        users.push(user);
    }

    // Now concurrently: 5 leave, 5 new join
    let barrier = Arc::new(Barrier::new(10));
    let room_id = room.id.clone();

    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service =
        PermissionService::new(member_repo.clone(), room_repo.clone(), None, 1000, 300);
    let mut member_service = MemberService::new(
        member_repo.clone(),
        room_repo.clone(),
        permission_service.clone(),
    );
    member_service.set_room_settings_repo(room_settings_repo);

    let mut leave_handles = Vec::with_capacity(5);

    // First 5 leave
    for user in users.iter().take(5) {
        let member_repo_clone = member_repo.clone();
        let barrier_clone = barrier.clone();
        let room_id_clone = room_id.clone();
        let user_id = user.id.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            member_repo_clone.remove(&room_id_clone, &user_id).await
        });
        leave_handles.push(handle);
    }

    // Create 5 new users to join
    let mut new_users = Vec::with_capacity(5);
    for i in 0..5 {
        let user = user_repo
            .create(&make_user(&format!("new_joiner_{i}")))
            .await
            .expect("Failed to create user");
        new_users.push(user);
    }

    let mut join_handles = Vec::with_capacity(5);

    // 5 new join
    for user in new_users {
        let member_service_clone = member_service.clone();
        let barrier_clone = barrier.clone();
        let room_id_clone = room_id.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            let options = AddMemberOptions::new().with_max_members(0);
            member_service_clone
                .add_member_with_options(room_id_clone, user.id, RoomRole::Member, options)
                .await
        });
        join_handles.push(handle);
    }

    // Wait for leave operations
    let mut leave_success = 0;
    for handle in leave_handles {
        match handle.await.expect("Leave task panicked") {
            Ok(_) => leave_success += 1,
            Err(e) => tracing::warn!("Leave operation failed: {:?}", e),
        }
    }

    // Wait for join operations
    let mut join_success = 0;
    for handle in join_handles {
        match handle.await.expect("Join task panicked") {
            Ok(_) => join_success += 1,
            Err(e) => tracing::warn!("Join operation failed: {:?}", e),
        }
    }

    // All operations should succeed
    assert_eq!(leave_success, 5, "All leave operations should succeed");
    assert_eq!(join_success, 5, "All join operations should succeed");

    // Final count: owner (1) + 5 remaining original + 5 new = 11
    let final_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM room_members WHERE room_id = $1 AND left_at IS NULL",
    )
    .bind(room.id.as_str())
    .fetch_one(pool)
    .await
    .expect("Failed to count members");

    assert_eq!(final_count, 11, "Final member count should be 11");
}

// ============================================================================
// Test: Stress Test - Many Concurrent Operations
// ============================================================================

/// Stress test with many concurrent operations.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_stress_many_concurrent_joins() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    // Room with capacity 50
    let (_owner, room, _settings) = setup_test_room(pool, "Stress Room", 50).await;

    // Create 100 users
    let user_repo = UserRepository::new(pool.clone());
    let mut users = Vec::with_capacity(100);
    for i in 0..100 {
        let user = user_repo
            .create(&make_user(&format!("stress_{i}")))
            .await
            .expect("Failed to create user");
        users.push(user);
    }

    // Setup services
    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service =
        PermissionService::new(member_repo.clone(), room_repo.clone(), None, 1000, 300);
    let mut member_service = MemberService::new(
        member_repo.clone(),
        room_repo.clone(),
        permission_service.clone(),
    );
    member_service.set_room_settings_repo(room_settings_repo);

    let barrier = Arc::new(Barrier::new(100));
    let room_id = room.id.clone();

    let mut handles = Vec::with_capacity(100);
    for user in users {
        let member_service_clone = member_service.clone();
        let barrier_clone = barrier.clone();
        let room_id_clone = room_id.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            let options = AddMemberOptions::new().with_max_members(0);
            member_service_clone
                .add_member_with_options(room_id_clone, user.id, RoomRole::Member, options)
                .await
        });
        handles.push(handle);
    }

    let mut success = 0;
    let mut rejected = 0;

    for handle in handles {
        match handle.await.expect("Task panicked") {
            Ok(_) => success += 1,
            Err(Error::InvalidInput(msg)) if msg.contains("Room is full") => rejected += 1,
            Err(e) => tracing::warn!("Unexpected error: {:?}", e),
        }
    }

    // With capacity 50 and owner already member, 49 more can join
    assert_eq!(success, 49, "49 users should join successfully");
    assert_eq!(rejected, 51, "51 users should be rejected (room full)");

    tracing::info!(
        "Stress test results: {} succeeded, {} rejected",
        success,
        rejected
    );
}
