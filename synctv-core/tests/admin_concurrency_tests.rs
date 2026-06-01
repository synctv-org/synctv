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
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        AddMemberOptions, Room, RoomId, RoomRole, RoomSettings, RoomStatus, User, UserId, UserRole,
        UserStatus,
    },
    repository::{RoomMemberRepository, RoomRepository, RoomSettingsRepository, UserRepository},
    service::{
        auth::{BruteForceProtection, JwtService},
        member::MemberService,
        permission::PermissionService,
        InMemoryTokenBlacklistStore, NotificationService, RoomService, UserService,
    },
    Error,
};
use synctv_core_testing::create_test_pool;
use tokio::sync::Barrier;
// Test Infrastructure

fn make_user_with_role(username: &str, role: UserRole) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role,
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

fn make_room(name: &str, description: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: description.to_string(),
        cover_file_reference_id: None,
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

#[allow(dead_code)]
fn make_user_service(pool: &PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
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
    let user_service = make_user_service(&pool);
    
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
    let owner_member = synctv_core::models::RoomMember::new(room.id, owner.id, RoomRole::Creator);
    member_repo
        .add(&owner_member)
        .await
        .expect("Failed to add owner as member");

    (owner, room)
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_user_ban_operations() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = Arc::new(UserRepository::new(pool.clone()));

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
            repo_clone
                .ban(&user.id, None, Some("concurrent test".to_string()))
                .await
        });
        handles.push(handle);
    }

    // All bans should succeed
    let mut success_count = 0;
    for handle in handles {
        match handle.await.expect("Task panicked") {
            Ok(updated) => {
                assert_eq!(updated.status, UserStatus::Banned);
                assert!(user_repo.is_banned(&updated.id).await.unwrap());
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

    // 5 tasks try to ban, 5 try to revoke the active ban concurrently.
    let barrier = Arc::new(Barrier::new(10));
    let user_id = user.id;

    let mut handles = Vec::with_capacity(10);

    // 5 ban operations
    for _ in 0..5 {
        let repo_clone = user_repo.clone();
        let barrier_clone = barrier.clone();
        let uid = user_id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            repo_clone
                .ban(&uid, None, Some("concurrent test".to_string()))
                .await
        });
        handles.push(handle);
    }

    // 5 active operations
    for _ in 0..5 {
        let repo_clone = user_repo.clone();
        let barrier_clone = barrier.clone();
        let uid = user_id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            repo_clone.unban(&uid).await
        });
        handles.push(handle);
    }

    let mut success_count = 0;
    for handle in handles {
        match handle.await.expect("Task panicked") {
            Ok(_) => success_count += 1,
            Err(Error::AlreadyExists(_) | Error::NotFound(_)) => {}
            Err(e) => panic!("Ban operation should not fail unexpectedly: {e:?}"),
        }
    }

    assert!(
        success_count > 0,
        "At least one ban/unban operation should succeed"
    );

    let final_user = user_repo
        .get_by_id(&user_id)
        .await
        .expect("Query failed")
        .expect("User exists");
    assert_eq!(
        final_user.status == UserStatus::Banned,
        user_repo.is_banned(&user_id).await.unwrap(),
        "derived user status must stay consistent with active user_bans"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_room_ban_operations() {
    let (_container, pool) = create_test_pool().await;
    let room_repo = Arc::new(RoomRepository::new(pool.clone()));

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
    let room_id = room.id;

    // 10 tasks try different status changes
    let barrier = Arc::new(Barrier::new(10));
    let statuses = vec![
        RoomStatus::Active,
        RoomStatus::Closed,
        RoomStatus::Closed,
        RoomStatus::Active,
        RoomStatus::Closed,
        RoomStatus::Closed,
        RoomStatus::Active,
        RoomStatus::Closed,
        RoomStatus::Closed,
        RoomStatus::Active,
    ];

    let mut handles = Vec::with_capacity(10);
    for status in statuses {
        let repo_clone = room_repo.clone();
        let barrier_clone = barrier.clone();
        let rid = room_id;

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
    let user_id = user.id;

    // 5 concurrent updates to the same user
    let mut handles = Vec::with_capacity(5);
    for i in 0..5 {
        let repo_clone = Arc::new(UserRepository::new(pool.clone()));
        let barrier_clone = barrier.clone();
        let uid = user_id;

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
                updated.username = format!("updated_user_{i}");

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
    let user_id = user.id;

    // Task 1: Update without retry
    let repo1 = user_repo.clone();
    let barrier1 = barrier.clone();
    let uid1 = user_id;

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
    let uid2 = user_id;

    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;

        // Retry loop
        let mut retries = 0;
        let max_retries = 5;
        loop {
            let current = repo2.get_by_id(&uid2).await.unwrap().unwrap();
            let mut updated = current.clone();
            updated.username = "retried_user".to_string();

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

    // Task 2 (with retry) should always succeed regardless of race order
    assert!(
        result2.is_ok(),
        "Update with retry should succeed: {result2:?}"
    );

    // Task 1 (no retry) may or may not succeed depending on timing —
    // if Task 2's first attempt commits before Task 1, Task 1 gets an
    // OptimisticLockConflict with no retry to recover.
    assert!(
        result1.is_ok() || matches!(result1, Err(Error::OptimisticLockConflict)),
        "Task without retry should either succeed or get OptimisticLockConflict: {result1:?}"
    );
}

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
    let user_id = user.id;

    // 50 concurrent ban/unban attempts should not corrupt account facts.
    let mut handles = Vec::with_capacity(50);
    for i in 0..50 {
        let repo_clone = user_repo.clone();
        let barrier_clone = barrier.clone();
        let uid = user_id;

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;
            if i % 2 == 0 {
                repo_clone
                    .ban(&uid, None, Some("stress test".to_string()))
                    .await
            } else {
                repo_clone.unban(&uid).await
            }
        });
        handles.push(handle);
    }

    let mut success_count = 0;
    for handle in handles {
        match handle.await.expect("Task panicked") {
            Ok(_) => success_count += 1,
            Err(Error::NotFound(_)) => {}
            Err(e) => tracing::warn!("Operation failed: {:?}", e),
        }
    }

    assert!(
        success_count > 0,
        "At least one ban-state operation should succeed"
    );
    let final_user = user_repo
        .get_by_id(&user_id)
        .await
        .expect("Query failed")
        .expect("User exists");
    assert_eq!(
        final_user.status == UserStatus::Banned,
        user_repo.is_banned(&user_id).await.unwrap(),
        "derived user status must stay consistent with active user_bans"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_room_ban_with_members_joining() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = Arc::new(RoomRepository::new(pool.clone()));

    let (_owner, room) = setup_test_room(&pool, "Ban With Join Room").await;

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
        NotificationService::default(),
    );
    member_service.set_room_settings_repo(room_settings_repo);

    let room_id = room.id;

    // 10 join tasks
    let mut join_handles = Vec::with_capacity(10);
    for user in users {
        let ms = member_service.clone();
        let bc = barrier.clone();
        let rid = room_id;

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
    let rid = room_id;

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
    let room_id = room.id;

    // 10 concurrent settings updates
    let mut handles = Vec::with_capacity(10);
    for i in 0..10 {
        let repo = room_settings_repo.clone();
        let bc = barrier.clone();
        let rid = room_id;

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
                updated.max_members = synctv_core::models::room_settings::MaxMembers(
                    50 + u64::try_from(i).unwrap_or_default(),
                );

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
                        let jitter = (u64::try_from(i).unwrap_or_default() * 7
                            + u64::from(retries) * 3)
                            % base_ms;
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
    let user_id = user.id;

    // 5 concurrent role updates
    let mut handles = Vec::with_capacity(5);
    for _ in 0..5 {
        let repo = user_repo.clone();
        let bc = barrier.clone();
        let uid = user_id;

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
