//! Admin concurrency tests
//!
//! Tests concurrent admin operations including:
//! - Concurrent user ban operations
//! - Concurrent room ban operations
//! - Settings update concurrency with optimistic lock
//! - Optimistic lock conflict retry scenarios
//!
//! Run with: cargo test -p synctv-core --test `admin_concurrency_tests` -- --nocapture
//! Docker tests: cargo test -p synctv-core --test `admin_concurrency_tests` -- --ignored --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        Room, RoomId, RoomRole, RoomSettings, RoomStatus, User, UserId, UserRole, UserStatus,
    },
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository, UserRepository},
    service::{
        auth::{BruteForceProtection, JwtService},
        member::{AddMemberOptions, MemberService},
        permission::PermissionService,
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Error,
};
use synctv_core_testing::create_test_pool;
use tokio::sync::Barrier;
// ============================================================================
// Test Infrastructure
// ============================================================================

fn make_user_with_role(username: &str, role: UserRole) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
        role,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: None,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

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
    }
}

#[allow(dead_code)]
fn make_user_service(pool: PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);
    let password_complexity = PasswordComplexityConfig::default();
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new(
        pool,
        jwt_service,
        username_cache,
        password_complexity,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

#[allow(dead_code)]
fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(pool.clone());
    RoomService::new(pool, user_service)
}

async fn setup_test_room(pool: &PgPool, room_name: &str) -> (User, Room) {
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Use a unique owner name derived from the room name to avoid duplicate username conflicts
    let owner_name = format!("owner_{}", room_name.replace(' ', "_").to_lowercase());
    let owner = user_repo
        .create(&make_user_with_role(&owner_name, UserRole::User))
        .await
        .expect("Failed to create owner");

    let room = room_repo
        .create(&make_room(room_name, "Test room", &owner.id))
        .await
        .expect("Failed to create room");

    // Add owner as member
    let member_repo = RoomMemberRepository::new(pool.clone());
    let owner_member =
        synctv_core::models::RoomMember::new(room.id.clone(), owner.id.clone(), RoomRole::Creator);
    member_repo
        .add(&owner_member)
        .await
        .expect("Failed to add owner as member");

    (owner, room)
}

// ============================================================================
// Test: Concurrent user ban operations
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_user_ban_operations() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = Arc::new(UserRepository::new(pool.clone()));

    // Create multiple users to ban
    let mut users = Vec::with_capacity(10);
    for i in 0..10 {
        let user = user_repo
            .create(&make_user_with_role(
                &format!("concurrent_ban_{i}"),
                UserRole::User,
            ))
            .await
            .expect("Failed to create user");
        users.push(user);
    }

    let barrier = Arc::new(Barrier::new(10));

    // Concurrently ban all users
    let mut handles = Vec::with_capacity(10);
    for user in users {
        let repo_clone = user_repo.clone();
        let barrier_clone = barrier.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            repo_clone.update_status(&user.id, UserStatus::Banned).await
        });
        handles.push(handle);
    }

    // All bans should succeed
    let mut success_count = 0;
    for handle in handles {
        match handle.await.expect("Task panicked") {
            Ok(updated) => {
                assert_eq!(updated.status, UserStatus::Banned);
                success_count += 1;
            }
            Err(e) => panic!("Ban operation should succeed: {e:?}"),
        }
    }

    assert_eq!(success_count, 10, "All ban operations should succeed");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_ban_unban_same_user() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = Arc::new(UserRepository::new(pool.clone()));

    let user = user_repo
        .create(&make_user_with_role("ban_unban_user", UserRole::User))
        .await
        .expect("Failed to create user");

    // 5 tasks try to ban, 5 try to set active concurrently
    let barrier = Arc::new(Barrier::new(10));
    let user_id = user.id.clone();

    let mut handles = Vec::with_capacity(10);

    // 5 ban operations
    for _ in 0..5 {
        let repo_clone = user_repo.clone();
        let barrier_clone = barrier.clone();
        let uid = user_id.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            repo_clone.update_status(&uid, UserStatus::Banned).await
        });
        handles.push(handle);
    }

    // 5 active operations
    for _ in 0..5 {
        let repo_clone = user_repo.clone();
        let barrier_clone = barrier.clone();
        let uid = user_id.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            repo_clone.update_status(&uid, UserStatus::Active).await
        });
        handles.push(handle);
    }

    // All operations should succeed (last write wins)
    let mut success_count = 0;
    for handle in handles {
        match handle.await.expect("Task panicked") {
            Ok(_) => success_count += 1,
            Err(e) => panic!("Status operation should succeed: {e:?}"),
        }
    }

    assert_eq!(success_count, 10, "All operations should succeed");

    // Final state depends on which write was last
    let final_user = user_repo
        .get_by_id(&user_id)
        .await
        .expect("Query failed")
        .expect("User exists");
    assert!(
        final_user.status == UserStatus::Active || final_user.status == UserStatus::Banned,
        "Final status should be either Active or Banned"
    );
}

// ============================================================================
// Test: Concurrent room ban operations
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_room_ban_operations() {
    let (_container, pool) = create_test_pool().await;
    let room_repo = Arc::new(RoomRepository::new(pool.clone()));

    // Create multiple rooms
    let mut room_ids = Vec::with_capacity(10);
    for i in 0..10 {
        let (_owner, room) = setup_test_room(&pool, &format!("Concurrent Ban Room {i}")).await;
        room_ids.push(room.id);
    }

    let barrier = Arc::new(Barrier::new(10));

    // Concurrently ban all rooms
    let mut handles = Vec::with_capacity(10);
    for room_id in room_ids {
        let repo_clone = room_repo.clone();
        let barrier_clone = barrier.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            repo_clone.update_ban_status(&room_id, true).await
        });
        handles.push(handle);
    }

    // All bans should succeed
    let mut success_count = 0;
    for handle in handles {
        match handle.await.expect("Task panicked") {
            Ok(updated) => {
                assert!(updated.is_banned);
                success_count += 1;
            }
            Err(e) => panic!("Room ban should succeed: {e:?}"),
        }
    }

    assert_eq!(success_count, 10, "All room ban operations should succeed");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_room_status_changes() {
    let (_container, pool) = create_test_pool().await;
    let room_repo = Arc::new(RoomRepository::new(pool.clone()));

    let (_owner, room) = setup_test_room(&pool, "Status Change Room").await;
    let room_id = room.id.clone();

    // 10 tasks try different status changes
    let barrier = Arc::new(Barrier::new(10));
    let statuses = vec![
        RoomStatus::Active,
        RoomStatus::Pending,
        RoomStatus::Closed,
        RoomStatus::Active,
        RoomStatus::Pending,
        RoomStatus::Closed,
        RoomStatus::Active,
        RoomStatus::Pending,
        RoomStatus::Closed,
        RoomStatus::Active,
    ];

    let mut handles = Vec::with_capacity(10);
    for status in statuses {
        let repo_clone = room_repo.clone();
        let barrier_clone = barrier.clone();
        let rid = room_id.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            repo_clone.update_status(&rid, status).await
        });
        handles.push(handle);
    }

    // All operations should succeed
    let mut success_count = 0;
    for handle in handles {
        match handle.await.expect("Task panicked") {
            Ok(_) => success_count += 1,
            Err(e) => panic!("Status change should succeed: {e:?}"),
        }
    }

    assert_eq!(success_count, 10, "All status operations should succeed");
}

// ============================================================================
// Test: Concurrent settings updates with optimistic lock
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_global_settings_update_optimistic_lock() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user = user_repo
        .create(&make_user_with_role("settings_user", UserRole::User))
        .await
        .expect("Failed to create user");

    let barrier = Arc::new(Barrier::new(5));
    let user_id = user.id.clone();

    // 5 concurrent updates to the same user
    let mut handles = Vec::with_capacity(5);
    for i in 0..5 {
        let repo_clone = Arc::new(UserRepository::new(pool.clone()));
        let barrier_clone = barrier.clone();
        let uid = user_id.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;

            // Retry loop for optimistic lock conflicts
            let mut retries = 0;
            let max_retries = 3;
            loop {
                let current = repo_clone
                    .get_by_id(&uid)
                    .await
                    .expect("Query failed")
                    .expect("User exists");
                let mut updated = current.clone();
                updated.email = Some(format!("updated_{i}@test.com"));

                match repo_clone.update(&updated, current.version).await {
                    Ok(result) => break Ok(result),
                    Err(Error::OptimisticLockConflict) => {
                        retries += 1;
                        if retries >= max_retries {
                            break Err(Error::OptimisticLockConflict);
                        }
                        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
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
            Ok(_) => success_count += 1,
            Err(Error::OptimisticLockConflict) => conflict_count += 1,
            Err(e) => panic!("Unexpected error: {e:?}"),
        }
    }

    // With retry, most should succeed
    assert!(
        success_count >= 3,
        "At least 3 updates should succeed with retry"
    );

    tracing::info!(
        "Concurrent user updates: {} succeeded, {} failed after retries",
        success_count,
        conflict_count
    );
}

// ============================================================================
// Test: Optimistic lock conflict retry scenarios
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_optimistic_lock_conflict_detected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user = user_repo
        .create(&make_user_with_role("lock_conflict", UserRole::User))
        .await
        .expect("Failed to create user");

    // Read same user twice
    let read1 = user_repo.get_by_id(&user.id).await.unwrap().unwrap();
    let read2 = user_repo.get_by_id(&user.id).await.unwrap().unwrap();

    // First update succeeds
    let mut update1 = read1.clone();
    update1.role = UserRole::Admin;
    let result1 = user_repo.update(&update1, read1.version).await;
    assert!(result1.is_ok());

    // Second update with stale version fails
    let mut update2 = read2.clone();
    update2.role = UserRole::Admin;
    let result2 = user_repo.update(&update2, read2.version).await;

    assert!(
        matches!(result2, Err(Error::OptimisticLockConflict)),
        "Should detect optimistic lock conflict"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_optimistic_lock_retry_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = Arc::new(UserRepository::new(pool.clone()));

    let user = user_repo
        .create(&make_user_with_role("retry_success", UserRole::User))
        .await
        .expect("Failed to create user");

    let barrier = Arc::new(Barrier::new(2));
    let user_id = user.id.clone();

    // Task 1: Update without retry
    let repo1 = user_repo.clone();
    let barrier1 = barrier.clone();
    let uid1 = user_id.clone();

    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;
        let current = repo1.get_by_id(&uid1).await.unwrap().unwrap();
        let mut updated = current.clone();
        updated.role = UserRole::Admin;
        repo1.update(&updated, current.version).await
    });

    // Task 2: Update with retry
    let repo2 = user_repo.clone();
    let barrier2 = barrier.clone();
    let uid2 = user_id.clone();

    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;

        // Retry loop
        let mut retries = 0;
        let max_retries = 5;
        loop {
            let current = repo2.get_by_id(&uid2).await.unwrap().unwrap();
            let mut updated = current.clone();
            updated.email = Some("retried@test.com".to_string());

            match repo2.update(&updated, current.version).await {
                Ok(result) => break Ok(result),
                Err(Error::OptimisticLockConflict) => {
                    retries += 1;
                    if retries >= max_retries {
                        break Err(Error::OptimisticLockConflict);
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                }
                Err(e) => break Err(e),
            }
        }
    });

    let result1 = handle1.await.expect("Task 1 panicked");
    let result2 = handle2.await.expect("Task 2 panicked");

    // First should always succeed
    assert!(result1.is_ok(), "First update should succeed");

    // Second should succeed with retry
    assert!(
        result2.is_ok(),
        "Second update should succeed with retry: {result2:?}"
    );
}

// ============================================================================
// Test: High concurrency stress test
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_high_concurrency_status_updates() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = Arc::new(UserRepository::new(pool.clone()));

    let user = user_repo
        .create(&make_user_with_role("stress_user", UserRole::User))
        .await
        .expect("Failed to create user");

    let barrier = Arc::new(Barrier::new(50));
    let user_id = user.id.clone();

    // 50 concurrent status updates
    let mut handles = Vec::with_capacity(50);
    for i in 0..50 {
        let repo_clone = user_repo.clone();
        let barrier_clone = barrier.clone();
        let uid = user_id.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            let status = if i % 2 == 0 {
                UserStatus::Banned
            } else {
                UserStatus::Active
            };
            repo_clone.update_status(&uid, status).await
        });
        handles.push(handle);
    }

    let mut success_count = 0;
    for handle in handles {
        match handle.await.expect("Task panicked") {
            Ok(_) => success_count += 1,
            Err(e) => tracing::warn!("Operation failed: {:?}", e),
        }
    }

    // All should succeed (no optimistic lock for status-only updates)
    assert_eq!(success_count, 50, "All status updates should succeed");
}

// ============================================================================
// Test: Concurrent room member operations with ban
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_room_ban_with_members_joining() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = Arc::new(RoomRepository::new(pool.clone()));

    let (_owner, room) = setup_test_room(&pool, "Ban With Join Room").await;

    // Create 10 users to join
    let mut users = Vec::with_capacity(10);
    for i in 0..10 {
        let user = user_repo
            .create(&make_user_with_role(
                &format!("join_ban_{i}"),
                UserRole::User,
            ))
            .await
            .expect("Failed to create user");
        users.push(user);
    }

    let barrier = Arc::new(Barrier::new(11)); // 10 joins + 1 ban

    // Setup member service
    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_repo_for_service = RoomRepository::new(pool.clone());
    let room_settings_repo = RoomSettingsRepository::new(pool.clone());
    let permission_service = PermissionService::new(
        member_repo.clone(),
        room_repo_for_service.clone(),
        None,
        1000,
        300,
    );
    let mut member_service = MemberService::new(
        member_repo.clone(),
        room_repo_for_service.clone(),
        permission_service.clone(),
    );
    member_service.set_room_settings_repo(room_settings_repo);

    let room_id = room.id.clone();

    // 10 join tasks
    let mut join_handles = Vec::with_capacity(10);
    for user in users {
        let ms = member_service.clone();
        let bc = barrier.clone();
        let rid = room_id.clone();

        let handle = tokio::spawn(async move {
            bc.wait().await;
            let options = AddMemberOptions::new().with_max_members(0);
            ms.add_member_with_options(rid, user.id, RoomRole::Member, options)
                .await
        });
        join_handles.push(handle);
    }

    // 1 ban task
    let rr = room_repo.clone();
    let bc = barrier.clone();
    let rid = room_id.clone();

    let ban_handle = tokio::spawn(async move {
        bc.wait().await;
        rr.update_ban_status(&rid, true).await
    });

    // Collect results for join handles
    let mut join_success = 0;
    let mut join_failed = 0;

    for handle in join_handles {
        match handle.await.expect("Task panicked") {
            Ok(_) => join_success += 1,
            Err(_) => join_failed += 1,
        }
    }

    // Collect result for ban handle
    let ban_success = ban_handle.await.expect("Task panicked").is_ok();

    // Ban should succeed
    assert!(ban_success, "Room ban should succeed");

    // Some joins may succeed before ban, some after (will fail on banned room)
    tracing::info!(
        "Join results: {} succeeded, {} failed, ban: {}",
        join_success,
        join_failed,
        ban_success
    );
}

// ============================================================================
// Test: Concurrent room settings updates
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_room_settings_update() {
    let (_container, pool) = create_test_pool().await;

    let (_owner, room) = setup_test_room(&pool, "Settings Update Room").await;

    // Initialize room settings
    let room_settings_repo = Arc::new(RoomSettingsRepository::new(pool.clone()));
    room_settings_repo
        .set_settings(&room.id, &RoomSettings::default())
        .await
        .expect("Failed to create settings");

    let barrier = Arc::new(Barrier::new(10));
    let room_id = room.id.clone();

    // 10 concurrent settings updates
    let mut handles = Vec::with_capacity(10);
    for i in 0..10 {
        let repo = room_settings_repo.clone();
        let bc = barrier.clone();
        let rid = room_id.clone();

        let handle = tokio::spawn(async move {
            bc.wait().await;

            // Retry loop for optimistic lock with exponential backoff + jitter
            let mut retries = 0u32;
            let max_retries = 10;
            loop {
                let (settings, version) = repo
                    .get_with_version(&rid)
                    .await
                    .expect("Failed to get settings");
                let mut updated = settings.clone();
                updated.max_members = synctv_core::models::room_settings::MaxMembers(50 + i as u64);

                match repo
                    .set_settings_with_version(&rid, &updated, version)
                    .await
                {
                    Ok(_) => break Ok(()),
                    Err(Error::OptimisticLockConflict) => {
                        retries += 1;
                        if retries >= max_retries {
                            break Err(Error::OptimisticLockConflict);
                        }
                        let base_ms = 10u64 * (1u64 << retries.min(5));
                        let jitter = (i as u64 * 7 + u64::from(retries) * 3) % base_ms;
                        tokio::time::sleep(tokio::time::Duration::from_millis(base_ms + jitter))
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

    // With retry + backoff, most should succeed (at least half)
    assert!(
        success_count >= 5,
        "At least 5 settings updates should succeed with retry, got {success_count}"
    );

    tracing::info!(
        "Concurrent settings updates: {} succeeded, {} failed after retries",
        success_count,
        conflict_count
    );
}

// ============================================================================
// Test: Concurrent user role updates with validation
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_role_updates_same_user() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = Arc::new(UserRepository::new(pool.clone()));

    let user = user_repo
        .create(&make_user_with_role("role_update_target", UserRole::User))
        .await
        .expect("Failed to create user");

    let barrier = Arc::new(Barrier::new(5));
    let user_id = user.id.clone();

    // 5 concurrent role updates
    let mut handles = Vec::with_capacity(5);
    for _ in 0..5 {
        let repo = user_repo.clone();
        let bc = barrier.clone();
        let uid = user_id.clone();

        let handle = tokio::spawn(async move {
            bc.wait().await;

            // Retry loop
            let mut retries = 0;
            let max_retries = 3;
            loop {
                let current = repo.get_by_id(&uid).await.unwrap().unwrap();
                let mut updated = current.clone();

                // Toggle between User and Admin
                updated.role = if current.role == UserRole::User {
                    UserRole::Admin
                } else {
                    UserRole::User
                };

                match repo.update(&updated, current.version).await {
                    Ok(result) => break Ok(result),
                    Err(Error::OptimisticLockConflict) => {
                        retries += 1;
                        if retries >= max_retries {
                            break Err(Error::OptimisticLockConflict);
                        }
                        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    }
                    Err(e) => break Err(e),
                }
            }
        });
        handles.push(handle);
    }

    let mut success_count = 0;

    for handle in handles {
        match handle.await.expect("Task panicked") {
            Ok(_) => success_count += 1,
            Err(Error::OptimisticLockConflict) => {}
            Err(e) => panic!("Unexpected error: {e:?}"),
        }
    }

    // At least some should succeed
    assert!(success_count >= 3, "At least 3 role updates should succeed");

    // Verify final state is valid
    let final_user = user_repo.get_by_id(&user_id).await.unwrap().unwrap();
    assert!(
        final_user.role == UserRole::User || final_user.role == UserRole::Admin,
        "Final role should be valid"
    );
}
