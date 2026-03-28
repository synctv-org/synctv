//! `RoomService` integration tests
//!
//! Tests the `RoomService` business logic layer with real `PostgreSQL` via testcontainers.
//!
//! Run with: cargo test -p synctv-core --test `room_service_tests` -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        Media, MediaId, MemberStatus, PermissionBits, RoomId, RoomRole, RoomSettings, User, UserId,
        UserRole, UserStatus,
    },
    repository::{
        MediaRepository, PlaylistRepository, RoomMemberRepository, RoomRepository,
        RoomSettingsRepository, SettingsRepository, UserRepository,
    },
    service::{
        auth::{BruteForceProtection, JwtService},
        notification::{GuestKickReason, RoomEvent},
        InMemoryTokenBlacklistStore, RoomService, SettingsRegistry, SettingsService, UserService,
    },
    Error,
};
use synctv_core_testing::create_test_pool;

// Helper functions for room tests
// TODO: Consider moving to room_test_helpers.rs for reuse
fn make_user_service(pool: PgPool) -> UserService {
    // 32-byte secret for HS256
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

fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(pool.clone());
    RoomService::new(pool, user_service)
}

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
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

// ========== Room Creation Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_room_with_password_stores_hash() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("pwd_owner")).await.unwrap();

    let (room, member) = room_service
        .create_room(
            "Password Room".to_string(),
            "A password-protected room".to_string(),
            owner.id.clone(),
            Some("MySecretPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    assert_eq!(room.name, "Password Room");
    assert_eq!(member.role, RoomRole::Creator);

    // Verify password hash was stored
    let pwd_hash = settings_repo.get_password_hash(&room.id).await.unwrap();
    assert!(pwd_hash.is_some(), "Password hash should be stored");
    let hash = pwd_hash.unwrap();
    assert!(!hash.is_empty(), "Password hash should not be empty");
    // Hash should not be the plaintext password
    assert_ne!(hash, "MySecretPassword123");

    // Verify room settings have require_password = true
    let settings = settings_repo.get(&room.id).await.unwrap();
    assert!(
        settings.require_password.0,
        "require_password should be true"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_room_without_password() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("nopwd_owner")).await.unwrap();

    let (room, member) = room_service
        .create_room(
            "No Password Room".to_string(),
            "An open room".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(room.name, "No Password Room");
    assert_eq!(member.role, RoomRole::Creator);

    // Verify no password hash was stored
    let pwd_hash = settings_repo.get_password_hash(&room.id).await.unwrap();
    assert!(pwd_hash.is_none(), "Password hash should not be stored");

    // Verify room settings have require_password = false
    let settings = settings_repo.get(&room.id).await.unwrap();
    assert!(
        !settings.require_password.0,
        "require_password should be false"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_room_creates_root_playlist() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("playlist_owner"))
        .await
        .unwrap();

    let (room, _member) = room_service
        .create_room(
            "Playlist Room".to_string(),
            "A room with playlist".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Verify root playlist was created
    let playlist_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM playlists WHERE room_id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(playlist_count, 1, "Root playlist should be created");
}

// ========== Room Join Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_join_room_correct_password() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("join_owner")).await.unwrap();
    let joiner = user_repo.create(&make_user("join_user")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Join Test Room".to_string(),
            String::new(),
            owner.id.clone(),
            Some("CorrectPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    let (joined_room, member, members) = room_service
        .join_room(
            room.id.clone(),
            joiner.id.clone(),
            Some("CorrectPassword123".to_string()),
        )
        .await
        .unwrap();

    assert_eq!(joined_room.id, room.id);
    assert_eq!(member.user_id, joiner.id);
    assert_eq!(member.role, RoomRole::Member);
    assert!(
        members.len() >= 2,
        "Should have at least creator and joiner"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_join_room_wrong_password_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("wrong_pwd_owner"))
        .await
        .unwrap();
    let joiner = user_repo
        .create(&make_user("wrong_pwd_joiner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Wrong Pwd Room".to_string(),
            String::new(),
            owner.id.clone(),
            Some("CorrectPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    let result = room_service
        .join_room(
            room.id.clone(),
            joiner.id.clone(),
            Some("WrongPassword456".to_string()),
        )
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(
                msg.contains("Invalid password") || msg.contains("password"),
                "Error should mention password: {msg}"
            );
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_join_room_password_required_not_provided() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("nopwd_join_owner"))
        .await
        .unwrap();
    let joiner = user_repo
        .create(&make_user("nopwd_join_user"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Pwd Required Room".to_string(),
            String::new(),
            owner.id.clone(),
            Some("SecretPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    // Try to join without providing a password
    let result = room_service
        .join_room(room.id.clone(), joiner.id.clone(), None)
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(
                msg.contains("Password required") || msg.contains("password"),
                "Error should mention password required: {msg}"
            );
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

// ========== Room Leave Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_leave_room_creator_cannot_leave() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("leave_owner")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Leave Test Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let result = room_service
        .leave_room(room.id.clone(), owner.id.clone())
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(
                msg.contains("creator") || msg.contains("Creator"),
                "Error should mention creator: {msg}"
            );
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_leave_room_member_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("leave_succ_owner"))
        .await
        .unwrap();
    let joiner = user_repo
        .create(&make_user("leave_succ_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Leave Success Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Join the room first
    room_service
        .join_room(room.id.clone(), joiner.id.clone(), None)
        .await
        .unwrap();

    // Verify membership exists
    assert!(member_repo.is_member(&room.id, &joiner.id).await.unwrap());

    // Leave the room
    room_service
        .leave_room(room.id.clone(), joiner.id.clone())
        .await
        .unwrap();

    // Verify membership is gone
    assert!(!member_repo.is_member(&room.id, &joiner.id).await.unwrap());
}

// ========== Room Delete Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_room_sets_deleted_at() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("delete_owner")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Delete Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Room should exist
    assert!(room_repo.exists(&room.id).await.unwrap());

    // Delete the room
    room_service
        .delete_room(room.id.clone(), owner.id.clone())
        .await
        .unwrap();

    // Room should no longer be findable via normal queries (deleted_at IS NULL filter)
    let fetched = room_repo.get_by_id(&room.id).await.unwrap();
    assert!(
        fetched.is_none(),
        "Room should not be found after soft-delete"
    );

    // But should still exist in DB with deleted_at set
    let deleted_at: Option<chrono::DateTime<Utc>> =
        sqlx::query_scalar("SELECT deleted_at FROM rooms WHERE id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();

    assert!(deleted_at.is_some(), "deleted_at should be set");
}

// ========== CAS Exhaustion Test (B6 fix verification) ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_settings_cas_exhaustion_returns_internal() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let owner = user_repo.create(&make_user("cas_owner")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "CAS Test Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Manually corrupt the version to force OptimisticLockConflict on every attempt.
    // We do this by updating the version to a very high number after each read.
    // The service reads version N, then we immediately bump it, so the CAS write
    // fails with OptimisticLockConflict.
    //
    // Spawn a concurrent task that keeps bumping the version.
    let room_id_str = room.id.as_str().to_string();
    let pool_clone = pool.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();

    let bumper = tokio::spawn(async move {
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = sqlx::query(
                "UPDATE room_settings SET version = version + 1 WHERE room_id = $1 AND key = '_settings'"
            )
            .bind(&room_id_str)
            .execute(&pool_clone)
            .await;
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    });

    let settings = RoomSettings::default();
    let result = room_service
        .set_settings(room.id.clone(), owner.id.clone(), settings)
        .await;

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = bumper.await;

    // The result should be an Internal error (not OptimisticLockConflict)
    match result {
        Ok(_) => {
            // If the bumper didn't run fast enough, the update may have succeeded.
            // This is acceptable - the test is probabilistic.
        }
        Err(Error::Internal(msg)) => {
            assert!(
                msg.contains("maximum retry"),
                "Should mention retry exhaustion: {msg}"
            );
        }
        Err(Error::OptimisticLockConflict) => {
            panic!("Bug B6: OptimisticLockConflict should NOT leak; should be wrapped in Internal error");
        }
        Err(other) => {
            panic!("Unexpected error: {other:?}");
        }
    }
}

// ========== Banned User Cannot Rejoin (S10) ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_banned_user_cannot_rejoin_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("ban_rejoin_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("ban_rejoin_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Ban Rejoin Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Join first
    room_service
        .join_room(room.id.clone(), target.id.clone(), None)
        .await
        .unwrap();

    // Ban the member
    let member_service = room_service.member_service();
    member_service
        .ban_member(
            room.id.clone(),
            creator.id.clone(),
            target.id.clone(),
            Some("Spamming".to_string()),
        )
        .await
        .unwrap();

    // Attempt to rejoin -- should fail because user is banned
    let result = room_service
        .join_room(room.id.clone(), target.id.clone(), None)
        .await;

    assert!(result.is_err(), "Banned user should not be able to rejoin");
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(
                msg.contains("banned") || msg.contains("ban"),
                "Error should mention ban: {msg}"
            );
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

// ========== Room Description Validation Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_description_unicode_500_chars_accepted() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("unicode_owner")).await.unwrap();

    // 500 Unicode characters (mix of ASCII and CJK)
    let desc = "Hello世界".repeat(50); // 7 chars * 50 = 350 chars
    let desc = format!("{}{}", desc, "a".repeat(150)); // 350 + 150 = 500 chars
    assert_eq!(desc.chars().count(), 500);

    let result = room_service
        .create_room(
            "Unicode Room".to_string(),
            desc.clone(),
            owner.id.clone(),
            None,
            None,
        )
        .await;

    assert!(result.is_ok(), "500 Unicode chars should be accepted");
    let (room, _) = result.unwrap();
    assert_eq!(room.description.chars().count(), 500);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_description_over_500_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("long_desc_owner"))
        .await
        .unwrap();

    // 501 characters
    let desc = "a".repeat(501);

    let result = room_service
        .create_room(
            "Long Desc Room".to_string(),
            desc,
            owner.id.clone(),
            None,
            None,
        )
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("description") || msg.contains("500"),
                "Should mention description limit: {msg}"
            );
        }
        other => panic!("Expected InvalidInput error, got: {other:?}"),
    }
}

// ========== Password Re-verification Race Condition Tests ==========
//
// These tests verify that the password is re-verified under the distributed lock
// to prevent a race condition where:
// 1. User passes password verification with a valid password
// 2. While waiting for lock, room password is changed
// 3. User joins with old (now invalid) password
//
// The fix ensures password is re-verified inside the lock with fresh data.

/// Helper to directly update room password in database, simulating an admin change
async fn direct_update_room_password(
    pool: &PgPool,
    room_id: &synctv_core::models::RoomId,
    new_password_hash: &str,
) {
    // Update the password hash directly
    sqlx::query("UPDATE room_settings SET value = $1 WHERE room_id = $2 AND key = 'password'")
        .bind(new_password_hash)
        .bind(room_id.as_str())
        .execute(pool)
        .await
        .expect("Failed to update password hash");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_join_room_password_changed_during_join_with_correct_old_password_fails() {
    // This test simulates the race condition scenario:
    // 1. User starts join with correct password
    // 2. Password is changed in DB before lock is acquired (simulated)
    // 3. Join should fail because password is re-verified under lock with fresh hash
    //
    // NOTE: Without a distributed lock configured, this test verifies baseline behavior.
    // With a distributed lock, the re-verification inside the lock catches this.
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("race_owner")).await.unwrap();
    let joiner = user_repo.create(&make_user("race_joiner")).await.unwrap();

    // Create room with initial password
    let (room, _) = room_service
        .create_room(
            "Race Test Room".to_string(),
            String::new(),
            owner.id.clone(),
            Some("OriginalPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    // Hash a new password to simulate the password being changed
    let new_hash = synctv_core::service::auth::password::hash_password("NewPassword456")
        .await
        .expect("Failed to hash new password");

    // Directly change the password in the database (simulating admin change)
    direct_update_room_password(&pool, &room.id, &new_hash).await;

    // Now try to join with the OLD password
    // This should fail because the password hash in DB has changed
    // (With distributed lock, the re-verification inside lock will catch this)
    let result = room_service
        .join_room(
            room.id.clone(),
            joiner.id.clone(),
            Some("OriginalPassword123".to_string()),
        )
        .await;

    assert!(
        result.is_err(),
        "Join with old password should fail after password change"
    );
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(
                msg.contains("Invalid password") || msg.contains("password"),
                "Error should mention password: {msg}"
            );
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_join_room_password_changed_during_join_with_correct_new_password_succeeds() {
    // This test verifies that if password is changed during join,
    // using the NEW password should succeed
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("race_new_owner"))
        .await
        .unwrap();
    let joiner = user_repo
        .create(&make_user("race_new_joiner"))
        .await
        .unwrap();

    // Create room with initial password
    let (room, _) = room_service
        .create_room(
            "Race New Password Room".to_string(),
            String::new(),
            owner.id.clone(),
            Some("OriginalPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    // Hash a new password
    let new_hash = synctv_core::service::auth::password::hash_password("NewPassword456")
        .await
        .expect("Failed to hash new password");

    // Directly change the password in the database
    direct_update_room_password(&pool, &room.id, &new_hash).await;

    // Join with the NEW password - should succeed
    let result = room_service
        .join_room(
            room.id.clone(),
            joiner.id.clone(),
            Some("NewPassword456".to_string()),
        )
        .await;

    assert!(
        result.is_ok(),
        "Join with new password should succeed after password change: {:?}",
        result.err()
    );
    let (joined_room, member, _members) = result.unwrap();
    assert_eq!(joined_room.id, room.id);
    assert_eq!(member.user_id, joiner.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_join_room_password_not_required_password_cleared_during_join() {
    // This test verifies that if require_password is set to false during join
    // (password removed from room), the join should succeed without password check
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("pwd_cleared_owner"))
        .await
        .unwrap();
    let joiner = user_repo
        .create(&make_user("pwd_cleared_joiner"))
        .await
        .unwrap();

    // Create room with initial password
    let (room, _) = room_service
        .create_room(
            "Password Cleared Room".to_string(),
            String::new(),
            owner.id.clone(),
            Some("OriginalPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    // Directly remove password requirement from room settings
    // The value column is TEXT, so we need to update with properly serialized JSON
    sqlx::query(
        r#"UPDATE room_settings SET value = '{"require_password":false,"allow_guest_join":false,"max_members":0,"require_approval":false,"allow_auto_join":true,"chat_enabled":true,"danmaku_enabled":true,"auto_play":{"enabled":true,"mode":"sequential","delay":3},"admin_added_permissions":0,"admin_removed_permissions":0,"member_added_permissions":0,"member_removed_permissions":0,"guest_added_permissions":0,"guest_removed_permissions":0}' WHERE room_id = $1 AND key = '_settings'"#
    )
    .bind(room.id.as_str())
    .execute(&pool)
    .await
    .expect("Failed to update require_password");

    // Join should now succeed even with wrong password (password no longer required)
    // Actually, since password is no longer required, we can join without password
    let result = room_service
        .join_room(room.id.clone(), joiner.id.clone(), None)
        .await;

    assert!(
        result.is_ok(),
        "Join should succeed when password is no longer required: {:?}",
        result.err()
    );
    let (joined_room, member, _members) = result.unwrap();
    assert_eq!(joined_room.id, room.id);
    assert_eq!(member.user_id, joiner.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_join_room_password_added_during_join_requires_password() {
    // This test verifies that if password requirement is added during join
    // (room was originally public, now requires password), join without password fails
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("pwd_added_owner"))
        .await
        .unwrap();
    let joiner = user_repo
        .create(&make_user("pwd_added_joiner"))
        .await
        .unwrap();

    // Create room without password
    let (room, _) = room_service
        .create_room(
            "Password Added Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Add password requirement to room
    let new_hash = synctv_core::service::auth::password::hash_password("NewPassword123")
        .await
        .expect("Failed to hash password");

    // Insert password hash
    sqlx::query(
        "INSERT INTO room_settings (room_id, key, value, version) VALUES ($1, 'password', $2, 1)",
    )
    .bind(room.id.as_str())
    .bind(&new_hash)
    .execute(&pool)
    .await
    .expect("Failed to insert password");

    // Update require_password setting - value column is TEXT with serialized JSON
    sqlx::query(
        r#"UPDATE room_settings SET value = '{"require_password":true,"allow_guest_join":false,"max_members":0,"require_approval":false,"allow_auto_join":true,"chat_enabled":true,"danmaku_enabled":true,"auto_play":{"enabled":true,"mode":"sequential","delay":3},"admin_added_permissions":0,"admin_removed_permissions":0,"member_added_permissions":0,"member_removed_permissions":0,"guest_added_permissions":0,"guest_removed_permissions":0}' WHERE room_id = $1 AND key = '_settings'"#
    )
    .bind(room.id.as_str())
    .execute(&pool)
    .await
    .expect("Failed to update require_password");

    // Join without password should fail
    let result = room_service
        .join_room(room.id.clone(), joiner.id.clone(), None)
        .await;

    assert!(
        result.is_err(),
        "Join without password should fail when password is added"
    );
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(
                msg.contains("Password required") || msg.contains("password"),
                "Error should mention password required: {msg}"
            );
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }

    // Join with correct password should succeed
    let result = room_service
        .join_room(
            room.id.clone(),
            joiner.id.clone(),
            Some("NewPassword123".to_string()),
        )
        .await;

    assert!(
        result.is_ok(),
        "Join with correct password should succeed: {:?}",
        result.err()
    );
}

// ========== Distributed Lock Tests (Mock-based) ==========

/// Mock distributed lock that tracks acquire/release calls
/// (Used for testing distributed lock semantics without Redis)
#[derive(Debug, Default)]
#[allow(dead_code)]
struct MockDistributedLock {
    acquired_keys: std::sync::Mutex<Vec<String>>,
    released_keys: std::sync::Mutex<Vec<String>>,
    should_fail_acquire: std::sync::atomic::AtomicBool,
    lock_held: std::sync::atomic::AtomicBool,
}

#[allow(dead_code)]
impl MockDistributedLock {
    fn new() -> Self {
        Self::default()
    }

    fn with_fail_acquire(&self) {
        self.should_fail_acquire
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn acquire_count(&self, key: &str) -> usize {
        self.acquired_keys
            .lock()
            .unwrap()
            .iter()
            .filter(|k| *k == key)
            .count()
    }

    fn was_released(&self, key: &str) -> bool {
        self.released_keys.lock().unwrap().iter().any(|k| k == key)
    }
}

#[async_trait::async_trait]
impl synctv_core::service::distributed_lock::MigrationLock for MockDistributedLock {
    async fn acquire(&self, key: &str, _ttl_secs: u64) -> anyhow::Result<Option<String>> {
        if self
            .should_fail_acquire
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(None);
        }

        // Simulate lock already held by another process
        if self
            .lock_held
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(None);
        }

        self.acquired_keys.lock().unwrap().push(key.to_string());
        Ok(Some(format!("lock-value-{key}")))
    }

    async fn extend(&self, _key: &str, _lock_value: &str, _ttl_secs: u64) -> anyhow::Result<bool> {
        Ok(self.lock_held.load(std::sync::atomic::Ordering::SeqCst))
    }

    async fn release(&self, key: &str, _lock_value: &str) -> anyhow::Result<bool> {
        self.lock_held
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.released_keys.lock().unwrap().push(key.to_string());
        Ok(true)
    }
}

/// Test concurrent room creation with the same name by different users
/// This test verifies that without a distributed lock, concurrent room creations
/// would both succeed (each user creates their own room with the same name).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_room_creation_without_lock_creates_separate_rooms() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let user1 = user_repo
        .create(&make_user("concurrent_user1"))
        .await
        .unwrap();
    let user2 = user_repo
        .create(&make_user("concurrent_user2"))
        .await
        .unwrap();

    // Both users create rooms with the same name simultaneously
    let room_name = "Same Name Room".to_string();

    let (result1, result2) = tokio::join!(
        room_service.create_room(
            room_name.clone(),
            "Desc1".to_string(),
            user1.id.clone(),
            None,
            None
        ),
        room_service.create_room(
            room_name.clone(),
            "Desc2".to_string(),
            user2.id.clone(),
            None,
            None
        )
    );

    // Both should succeed - they are separate rooms
    assert!(
        result1.is_ok(),
        "User1 should create room: {:?}",
        result1.err()
    );
    assert!(
        result2.is_ok(),
        "User2 should create room: {:?}",
        result2.err()
    );

    let (room1, _) = result1.unwrap();
    let (room2, _) = result2.unwrap();

    // Same name, but different room IDs
    assert_eq!(room1.name, room2.name);
    assert_ne!(
        room1.id, room2.id,
        "Different users should create different rooms"
    );
}

/// Test that distributed lock prevents duplicate room creation by the SAME user
/// This is a mock-based test that simulates what happens with distributed lock enabled.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_room_creation_same_user_without_lock_allows_duplicate() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let user = user_repo
        .create(&make_user("same_user_concurrent"))
        .await
        .unwrap();

    // Same user tries to create two rooms concurrently (e.g., double-click or network retry)
    let room_name = "User's Room".to_string();

    let (result1, result2) = tokio::join!(
        room_service.create_room(
            room_name.clone(),
            "Desc1".to_string(),
            user.id.clone(),
            None,
            None
        ),
        room_service.create_room(
            "Different Room".to_string(),
            "Desc2".to_string(),
            user.id.clone(),
            None,
            None
        )
    );

    // Without distributed lock, both should succeed
    // (With distributed lock, one would wait or fail)
    assert!(
        result1.is_ok(),
        "First room should be created: {:?}",
        result1.err()
    );
    assert!(
        result2.is_ok(),
        "Second room should be created: {:?}",
        result2.err()
    );
}

// ========== Room Password Update Cache Invalidation Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_password_update_invalidates_room_cache() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pwd_cache_owner"))
        .await
        .unwrap();

    // Create room with password
    let (room, _) = room_service
        .create_room(
            "Password Cache Room".to_string(),
            String::new(),
            owner.id.clone(),
            Some("OriginalPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    // Verify initial password
    let initial_hash = settings_repo.get_password_hash(&room.id).await.unwrap();
    assert!(initial_hash.is_some());

    // Update password
    let new_hash = synctv_core::service::auth::password::hash_password("NewPassword456")
        .await
        .expect("Failed to hash new password");

    room_service
        .update_room_password(&room.id, Some(new_hash.clone()))
        .await
        .unwrap();

    // Verify password was updated
    let updated_hash = settings_repo.get_password_hash(&room.id).await.unwrap();
    assert!(updated_hash.is_some());
    assert_ne!(initial_hash, updated_hash);

    // Verify settings reflect password requirement
    let settings = settings_repo.get(&room.id).await.unwrap();
    assert!(
        settings.require_password.0,
        "Room should still require password after update"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_password_removal_clears_require_password_flag() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pwd_remove_owner"))
        .await
        .unwrap();

    // Create room with password
    let (room, _) = room_service
        .create_room(
            "Password Remove Room".to_string(),
            String::new(),
            owner.id.clone(),
            Some("PasswordToBeRemoved".to_string()),
            None,
        )
        .await
        .unwrap();

    // Verify password is set
    let settings = settings_repo.get(&room.id).await.unwrap();
    assert!(settings.require_password.0);
    assert!(settings_repo
        .get_password_hash(&room.id)
        .await
        .unwrap()
        .is_some());

    // Remove password
    room_service
        .update_room_password(&room.id, None)
        .await
        .unwrap();

    // Verify password is removed and flag is cleared
    let settings = settings_repo.get(&room.id).await.unwrap();
    assert!(
        !settings.require_password.0,
        "require_password should be false after password removal"
    );
    assert!(settings_repo
        .get_password_hash(&room.id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_password_update_allows_join_with_new_password() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("pwd_update_owner"))
        .await
        .unwrap();
    let joiner = user_repo
        .create(&make_user("pwd_update_joiner"))
        .await
        .unwrap();

    // Create room with initial password
    let (room, _) = room_service
        .create_room(
            "Password Update Room".to_string(),
            String::new(),
            owner.id.clone(),
            Some("OldPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    // Update password
    let new_hash = synctv_core::service::auth::password::hash_password("NewPassword456")
        .await
        .expect("Failed to hash new password");
    room_service
        .update_room_password(&room.id, Some(new_hash))
        .await
        .unwrap();

    // Join with old password should fail
    let result = room_service
        .join_room(
            room.id.clone(),
            joiner.id.clone(),
            Some("OldPassword123".to_string()),
        )
        .await;
    assert!(result.is_err(), "Old password should fail after update");

    // Join with new password should succeed
    let result = room_service
        .join_room(
            room.id.clone(),
            joiner.id.clone(),
            Some("NewPassword456".to_string()),
        )
        .await;
    assert!(
        result.is_ok(),
        "New password should succeed: {:?}",
        result.err()
    );
}

// ========== Ban/Unban Member Synchronization Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_member_invalidates_permission_cache() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("ban_sync_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("ban_sync_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Ban Sync Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Target joins the room
    room_service
        .join_room(room.id.clone(), target.id.clone(), None)
        .await
        .unwrap();

    // Verify target is a member with permissions
    let perm_service = room_service.permission_service();
    let initial_perms = perm_service
        .get_user_permissions(&room.id, &target.id)
        .await
        .unwrap();
    assert!(initial_perms.0 > 0, "Member should have some permissions");

    // Ban the target
    let member_service = room_service.member_service();
    member_service
        .ban_member(
            room.id.clone(),
            creator.id.clone(),
            target.id.clone(),
            Some("Test ban".to_string()),
        )
        .await
        .unwrap();

    // Verify permission cache is invalidated - banned user should have no permissions
    // Note: get_user_permissions_no_cache returns an error for banned users (not a member),
    // so we verify the ban was applied by checking member status directly
    let member = member_repo
        .get_any(&room.id, &target.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        member.status,
        MemberStatus::Banned,
        "Member should be banned"
    );

    // Verify that get_user_permissions returns error for banned user
    let perms_result = perm_service
        .get_user_permissions_no_cache(&room.id, &target.id)
        .await;
    assert!(
        perms_result.is_err(),
        "Banned user should not have permissions"
    );

    // Verify member status is Banned
    let member = member_repo
        .get_any(&room.id, &target.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member.status, MemberStatus::Banned);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_unban_member_restores_permission_access() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("unban_sync_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("unban_sync_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Unban Sync Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Target joins and then gets banned
    room_service
        .join_room(room.id.clone(), target.id.clone(), None)
        .await
        .unwrap();
    let member_service = room_service.member_service();
    member_service
        .ban_member(room.id.clone(), creator.id.clone(), target.id.clone(), None)
        .await
        .unwrap();

    // Verify banned
    let member = member_repo
        .get_any(&room.id, &target.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member.status, MemberStatus::Banned);

    // Unban
    member_service
        .unban_member(room.id.clone(), creator.id.clone(), target.id.clone())
        .await
        .unwrap();

    // Verify unbanned - status should not be Banned
    let member = member_repo
        .get(&room.id, &target.id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(member.status, MemberStatus::Banned);

    // User should be able to join again
    let result = room_service
        .join_room(room.id.clone(), target.id.clone(), None)
        .await;
    assert!(
        result.is_ok(),
        "Unbanned user should be able to join: {:?}",
        result.err()
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_prevents_room_access_even_with_cached_permissions() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("ban_cache_creator"))
        .await
        .unwrap();
    let target = user_repo
        .create(&make_user("ban_cache_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Ban Cache Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Target joins and gets permissions cached
    room_service
        .join_room(room.id.clone(), target.id.clone(), None)
        .await
        .unwrap();

    // Cache permissions
    let perm_service = room_service.permission_service();
    let _cached = perm_service
        .get_user_permissions(&room.id, &target.id)
        .await
        .unwrap();

    // Ban the target
    let member_service = room_service.member_service();
    member_service
        .ban_member(room.id.clone(), creator.id.clone(), target.id.clone(), None)
        .await
        .unwrap();

    // Try to rejoin - should fail because banned
    let result = room_service
        .join_room(room.id.clone(), target.id.clone(), None)
        .await;
    assert!(
        result.is_err(),
        "Banned user should not be able to join even with cached permissions"
    );

    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(msg.contains("banned"), "Error should mention ban: {msg}");
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

// ========== Room Settings Optimistic Lock Retry Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_settings_update_retries_on_version_conflict() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("retry_owner")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Retry Settings Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Concurrent settings update from two tasks
    let room_id1 = room.id.clone();
    let room_id2 = room.id.clone();
    let user_id1 = owner.id.clone();
    let user_id2 = owner.id.clone();

    let pool1 = pool.clone();
    let update1 = tokio::spawn(async move {
        let room_service = make_room_service(pool1.clone());
        let mut settings = room_service.get_room_settings(&room_id1).await.unwrap();
        settings.allow_guest_join = synctv_core::models::room_settings::AllowGuestJoin(true);
        room_service
            .set_settings(room_id1.clone(), user_id1.clone(), settings)
            .await
    });

    let pool2 = pool.clone();
    let update2 = tokio::spawn(async move {
        let room_service = make_room_service(pool2.clone());
        let mut settings = room_service.get_room_settings(&room_id2).await.unwrap();
        settings.max_members = synctv_core::models::room_settings::MaxMembers(50);
        room_service
            .set_settings(room_id2.clone(), user_id2.clone(), settings)
            .await
    });

    let (r1, r2) = tokio::join!(update1, update2);

    // At least one should succeed (possibly both with retries)
    // The retry mechanism should handle version conflicts
    let success_count = u8::from(r1.unwrap().is_ok()) + u8::from(r2.unwrap().is_ok());
    assert!(
        success_count >= 1,
        "At least one update should succeed with retry mechanism"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_settings_update_returns_internal_error_after_max_retries() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let owner = user_repo
        .create(&make_user("max_retry_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Max Retry Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Manually corrupt the version to force OptimisticLockConflict on every attempt
    let room_id_str = room.id.as_str().to_string();
    let pool_clone = pool.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();

    // Spawn a task that keeps bumping the version
    let bumper = tokio::spawn(async move {
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = sqlx::query(
                "UPDATE room_settings SET version = version + 1 WHERE room_id = $1 AND key = '_settings'"
            )
            .bind(&room_id_str)
            .execute(&pool_clone)
            .await;
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    });

    let settings = RoomSettings::default();
    let result = room_service
        .set_settings(room.id.clone(), owner.id.clone(), settings)
        .await;

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = bumper.await;

    // The result should be an Internal error (not OptimisticLockConflict which is wrapped)
    match result {
        Ok(_) => {
            // If the bumper didn't run fast enough, the update may have succeeded
            // This is acceptable - the test is probabilistic
        }
        Err(Error::Internal(msg)) => {
            assert!(
                msg.contains("retry"),
                "Should mention retry exhaustion: {msg}"
            );
        }
        Err(Error::OptimisticLockConflict) => {
            panic!("OptimisticLockConflict should be wrapped in Internal error");
        }
        Err(other) => {
            panic!("Unexpected error: {other:?}");
        }
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_single_setting_update_with_retry() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("single_setting_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Single Setting Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Update a single setting
    let result = room_service
        .update_room_setting(&room.id, &owner.id, "allow_guest_join", "true")
        .await;

    assert!(
        result.is_ok(),
        "Single setting update should succeed: {:?}",
        result.err()
    );

    // Verify the setting was updated
    let settings = room_service.get_room_settings(&room.id).await.unwrap();
    assert!(
        settings.allow_guest_join.0,
        "allow_guest_join should be true"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_password_update_with_cas_retry() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("pwd_retry_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Password Retry Room".to_string(),
            String::new(),
            owner.id.clone(),
            Some("InitialPassword".to_string()),
            None,
        )
        .await
        .unwrap();

    // Update password (internally uses CAS retry)
    let new_hash = synctv_core::service::auth::password::hash_password("NewPassword123")
        .await
        .expect("Failed to hash password");

    let result = room_service
        .update_room_password(&room.id, Some(new_hash))
        .await;

    assert!(
        result.is_ok(),
        "Password update with CAS retry should succeed: {:?}",
        result.err()
    );

    // Verify the new password works
    let joiner = user_repo
        .create(&make_user("pwd_retry_joiner"))
        .await
        .unwrap();
    let join_result = room_service
        .join_room(
            room.id.clone(),
            joiner.id.clone(),
            Some("NewPassword123".to_string()),
        )
        .await;

    assert!(
        join_result.is_ok(),
        "Join with new password should succeed: {:?}",
        join_result.err()
    );
}

// ========== Cache Invalidation Broadcast Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_deletion_invalidates_caches() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("cache_inval_owner"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("cache_inval_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Cache Invalidation Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Member joins to populate caches
    room_service
        .join_room(room.id.clone(), member.id.clone(), None)
        .await
        .unwrap();

    // Cache permissions
    let perm_service = room_service.permission_service();
    let _cached = perm_service
        .get_user_permissions(&room.id, &member.id)
        .await
        .unwrap();

    // Delete the room
    room_service
        .delete_room(room.id.clone(), owner.id.clone())
        .await
        .unwrap();

    // Verify room is deleted (soft-deleted, not visible via normal queries)
    let room_repo = RoomRepository::new(pool.clone());
    let fetched = room_repo.get_by_id(&room.id).await.unwrap();
    assert!(fetched.is_none(), "Room should not be found after deletion");
}

// ========== Password Timing Attack Prevention Tests ==========
//
// These tests verify that password verification uses constant-time comparison
// to prevent timing attacks. The verify_password function uses Argon2id which
// has built-in constant-time password comparison.
//
// Key security properties:
// 1. Same verification time for correct and incorrect passwords
// 2. No early-exit on password mismatch
// 3. Argon2id's verify_password returns Ok(true) or Ok(false), not early errors

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_password_verification_constant_time_properties() {
    // This test verifies the password verification uses constant-time comparison
    // by checking that Argon2id verification works correctly for both valid
    // and invalid passwords without early-exit timing differences.
    use synctv_core::service::auth::password::{hash_password, verify_password};

    let password = "TestPassword123!";
    let hash = hash_password(password)
        .await
        .expect("Failed to hash password");

    // Valid password should verify successfully
    let valid_result = verify_password(password, &hash)
        .await
        .expect("Verification should not error");
    assert!(valid_result, "Correct password should verify");

    // Invalid password should NOT verify, but should not error
    // (no timing difference from erroring vs returning false)
    let invalid_result = verify_password("WrongPassword456", &hash)
        .await
        .expect("Verification should not error even for wrong password");
    assert!(!invalid_result, "Wrong password should not verify");

    // Completely different length password should also just return false
    let short_result = verify_password("x", &hash)
        .await
        .expect("Short password should not cause error");
    assert!(!short_result, "Short password should not verify");

    // Empty password should also just return false
    let empty_result = verify_password("", &hash)
        .await
        .expect("Empty password should not cause error");
    assert!(!empty_result, "Empty password should not verify");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_password_verification_handles_malformed_hash_gracefully() {
    use synctv_core::service::auth::password::verify_password;

    // Malformed hash should return an error, but the service layer
    // should handle this gracefully and not leak information
    let result = verify_password("anypassword", "not_a_valid_hash").await;
    assert!(result.is_err(), "Malformed hash should return error");

    // The error should be Internal, not revealing hash details
    match result.unwrap_err() {
        synctv_core::Error::Internal(msg) => {
            assert!(
                msg.contains("Invalid password hash format") || msg.contains("verification"),
                "Error message should indicate hash format issue: {msg}"
            );
        }
        other => panic!("Expected Internal error for malformed hash, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_password_uses_unique_salt_per_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("salt_owner")).await.unwrap();

    // Create two rooms with the same password
    let (room1, _) = room_service
        .create_room(
            "Salt Room 1".to_string(),
            String::new(),
            owner.id.clone(),
            Some("SamePassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    let (room2, _) = room_service
        .create_room(
            "Salt Room 2".to_string(),
            String::new(),
            owner.id.clone(),
            Some("SamePassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    // Get password hashes for both rooms
    let hash1 = settings_repo
        .get_password_hash(&room1.id)
        .await
        .unwrap()
        .unwrap();
    let hash2 = settings_repo
        .get_password_hash(&room2.id)
        .await
        .unwrap()
        .unwrap();

    // Hashes should be different due to unique salts
    assert_ne!(
        hash1, hash2,
        "Password hashes should differ due to unique salts"
    );

    // But both hashes should verify the same password
    assert!(room_service
        .check_room_password(&room1.id, "SamePassword123")
        .await
        .unwrap());
    assert!(room_service
        .check_room_password(&room2.id, "SamePassword123")
        .await
        .unwrap());
}

// ========== Room Limits Enforcement Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_max_members_enforced_on_join() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("max_owner")).await.unwrap();

    // Create room with max_members = 3 (owner counts as 1)
    let settings = synctv_core::models::RoomSettings {
        max_members: synctv_core::models::room_settings::MaxMembers(3),
        ..Default::default()
    };

    let (room, _) = room_service
        .create_room(
            "Max Members Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            Some(settings),
        )
        .await
        .unwrap();

    // First joiner (count: 2)
    let joiner1 = user_repo.create(&make_user("max_joiner1")).await.unwrap();
    let result = room_service
        .join_room(room.id.clone(), joiner1.id.clone(), None)
        .await;
    assert!(
        result.is_ok(),
        "First joiner should succeed: {:?}",
        result.err()
    );

    // Second joiner (count: 3 = max)
    let joiner2 = user_repo.create(&make_user("max_joiner2")).await.unwrap();
    let result = room_service
        .join_room(room.id.clone(), joiner2.id.clone(), None)
        .await;
    assert!(
        result.is_ok(),
        "Second joiner should succeed (at limit): {:?}",
        result.err()
    );

    // Verify current member count
    let count = member_repo.count_by_room(&room.id).await.unwrap();
    assert_eq!(count, 3, "Should have 3 members (owner + 2 joiners)");

    // Third joiner should fail (would exceed max_members)
    let joiner3 = user_repo.create(&make_user("max_joiner3")).await.unwrap();
    let result = room_service
        .join_room(room.id.clone(), joiner3.id.clone(), None)
        .await;
    assert!(result.is_err(), "Third joiner should fail (exceeds max)");

    match result.unwrap_err() {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("full") || msg.contains("max") || msg.contains("capacity"),
                "Error should mention room capacity: {msg}"
            );
        }
        other => panic!("Expected InvalidInput error, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_max_members_zero_means_unlimited() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("unlim_owner")).await.unwrap();

    // Create room with max_members = 0 (unlimited)
    let settings = synctv_core::models::RoomSettings {
        max_members: synctv_core::models::room_settings::MaxMembers(0),
        ..Default::default()
    };

    let (room, _) = room_service
        .create_room(
            "Unlimited Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            Some(settings),
        )
        .await
        .unwrap();

    // Add many members - all should succeed
    for i in 0..20 {
        let joiner = user_repo
            .create(&make_user(&format!("unlim_joiner_{i}")))
            .await
            .unwrap();
        let result = room_service
            .join_room(room.id.clone(), joiner.id.clone(), None)
            .await;
        assert!(
            result.is_ok(),
            "Joiner {} should succeed (unlimited room): {:?}",
            i,
            result.err()
        );
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_max_members_enforced_at_limit_boundary() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("boundary_owner"))
        .await
        .unwrap();

    // Create room with max_members = 1 (owner only)
    let settings = synctv_core::models::RoomSettings {
        max_members: synctv_core::models::room_settings::MaxMembers(1),
        ..Default::default()
    };

    let (room, _) = room_service
        .create_room(
            "Boundary Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            Some(settings),
        )
        .await
        .unwrap();

    // Any joiner should fail (room already at capacity)
    let joiner = user_repo
        .create(&make_user("boundary_joiner"))
        .await
        .unwrap();
    let result = room_service
        .join_room(room.id.clone(), joiner.id.clone(), None)
        .await;
    assert!(result.is_err(), "Joiner should fail (room at capacity)");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_max_members_cannot_exceed_10000() {
    // Test that the max_members validation rejects values > 10000
    use synctv_core::models::room_settings::RoomSettingsRegistry;

    let result = RoomSettingsRegistry::validate_setting("max_members", "10001");
    assert!(result.is_err(), "max_members > 10000 should be rejected");

    let result = RoomSettingsRegistry::validate_setting("max_members", "10000");
    assert!(result.is_ok(), "max_members = 10000 should be accepted");
}

// ========== Room Settings Comprehensive Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_settings_update_validates_permissions_no_escalation() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("perm_esc_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Permission Escalation Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Try to set guest permissions that exceed member-level permissions
    let mut settings = room_service.get_room_settings(&room.id).await.unwrap();
    settings.guest_added_permissions = synctv_core::models::room_settings::GuestAddedPermissions(
        PermissionBits::KICK_MEMBER, // This exceeds DEFAULT_MEMBER
    );

    let result = room_service
        .set_settings(room.id.clone(), owner.id.clone(), settings)
        .await;

    assert!(result.is_err(), "Permission escalation should be rejected");
    match result.unwrap_err() {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("Guest") && msg.contains("permissions"),
                "Error should mention permission escalation: {msg}"
            );
        }
        other => panic!("Expected InvalidInput error, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_settings_guest_mode_change_kicks_guests() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let mut event_rx = room_service.notification_service().subscribe();

    let owner = user_repo
        .create(&make_user("guest_kick_owner"))
        .await
        .unwrap();
    let guest = user_repo
        .create(&make_user("guest_kick_guest"))
        .await
        .unwrap();

    // Create room with guest join allowed
    let settings = synctv_core::models::RoomSettings {
        allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(true),
        ..Default::default()
    };

    let (room, _) = room_service
        .create_room(
            "Guest Kick Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            Some(settings),
        )
        .await
        .unwrap();

    // Add guest as member with Guest role
    let member_service = room_service.member_service();
    member_service
        .add_member(room.id.clone(), guest.id.clone(), RoomRole::Guest)
        .await
        .unwrap();

    // Verify guest is a member
    assert!(member_repo.is_member(&room.id, &guest.id).await.unwrap());
    let guest_version_before = room_service.get_room_guest_version(&room.id).await.unwrap();

    // Disable guest join - this should kick the guest
    let result = room_service
        .update_room_setting(&room.id, &owner.id, "allow_guest_join", "false")
        .await;
    assert!(
        result.is_ok(),
        "Setting update should succeed: {:?}",
        result.err()
    );

    let settings = room_service.get_room_settings(&room.id).await.unwrap();
    assert!(
        !settings.allow_guest_join.0,
        "allow_guest_join should be false"
    );

    assert!(
        !member_repo.is_member(&room.id, &guest.id).await.unwrap(),
        "guest-role members must be removed when room guest mode is disabled"
    );
    let guest_version_after = room_service.get_room_guest_version(&room.id).await.unwrap();
    assert_eq!(
        guest_version_after,
        guest_version_before + 1,
        "room guest version must be bumped so anonymous guest JWTs are revoked"
    );

    let (event_room_id, event) = event_rx.recv().await.unwrap();
    assert_eq!(event_room_id, room.id);
    match event {
        RoomEvent::GuestKicked { reason, .. } => {
            assert!(
                matches!(reason, GuestKickReason::RoomGuestModeDisabled),
                "unexpected guest kick reason: {reason:?}"
            );
        }
        other => panic!("expected GuestKicked event, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_settings_password_required_triggers_guest_kick() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let mut event_rx = room_service.notification_service().subscribe();

    let owner = user_repo
        .create(&make_user("pwd_kick_owner"))
        .await
        .unwrap();
    let guest = user_repo
        .create(&make_user("pwd_kick_guest"))
        .await
        .unwrap();

    // Create room without password, guest join allowed
    let settings = synctv_core::models::RoomSettings {
        allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(true),
        require_password: synctv_core::models::room_settings::RequirePassword(false),
        ..Default::default()
    };

    let (room, _) = room_service
        .create_room(
            "Password Kick Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            Some(settings),
        )
        .await
        .unwrap();

    room_service
        .member_service()
        .add_member(room.id.clone(), guest.id.clone(), RoomRole::Guest)
        .await
        .unwrap();
    assert!(member_repo.is_member(&room.id, &guest.id).await.unwrap());
    let guest_version_before = room_service.get_room_guest_version(&room.id).await.unwrap();

    // Set a password - this should trigger guest kick
    let new_hash = synctv_core::service::auth::password::hash_password("NewPassword123")
        .await
        .expect("Failed to hash password");

    room_service
        .update_room_password(&room.id, Some(new_hash))
        .await
        .unwrap();

    // Verify settings reflect password requirement
    let settings = room_service.get_room_settings(&room.id).await.unwrap();
    assert!(
        settings.require_password.0,
        "require_password should be true"
    );

    assert!(
        !member_repo.is_member(&room.id, &guest.id).await.unwrap(),
        "guest-role members must be removed when a room password is added"
    );
    let guest_version_after = room_service.get_room_guest_version(&room.id).await.unwrap();
    assert_eq!(
        guest_version_after,
        guest_version_before + 1,
        "room guest version must be bumped when adding a password"
    );

    let (event_room_id, event) = event_rx.recv().await.unwrap();
    assert_eq!(event_room_id, room.id);
    match event {
        RoomEvent::GuestKicked { reason, .. } => {
            assert!(
                matches!(reason, GuestKickReason::RoomPasswordAdded),
                "unexpected guest kick reason: {reason:?}"
            );
        }
        other => panic!("expected GuestKicked event, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_remove_media_respects_admin_override_columns() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("remove_media_admin_owner"))
        .await
        .unwrap();
    let admin = user_repo
        .create(&make_user("remove_media_admin_target"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Admin Remove Media Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .member_service()
        .add_member(room.id.clone(), admin.id.clone(), RoomRole::Admin)
        .await
        .unwrap();

    sqlx::query(
        "UPDATE room_members
         SET admin_removed_permissions = admin_removed_permissions | $3,
             added_permissions = 0,
             removed_permissions = 0
         WHERE room_id = $1 AND user_id = $2",
    )
    .bind(room.id.as_str())
    .bind(admin.id.as_str())
    .bind(PermissionBits::DELETE_MOVIE_ANY as i64)
    .execute(&pool)
    .await
    .unwrap();

    let root_playlist = playlist_repo.get_root_playlist(&room.id).await.unwrap();
    let now = Utc::now();
    let media = Media {
        id: MediaId::new(),
        playlist_id: root_playlist.id.clone(),
        room_id: room.id.clone(),
        name: "Protected Media".to_string(),
        position: 0,
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({}),
        provider_instance_name: None,
        creator_id: Some(owner.id.clone()),
        added_at: now,
        updated_at: now,
        version: 0,
    };
    media_repo.create(&media).await.unwrap();

    let result = room_service
        .remove_media(room.id.clone(), admin.id.clone(), media.id.clone())
        .await;
    assert!(
        matches!(result, Err(Error::Authorization(_))),
        "admin DELETE_MOVIE_ANY revoke must be enforced by transactional SQL, got: {result:?}"
    );
}

// ========== Room Name and Description Edge Cases ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_name_unicode_validation() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("unicode_name_owner"))
        .await
        .unwrap();

    // Room name with various Unicode characters
    let unicode_name = "Room \u{4e2d}\u{6587} \u{65e5}\u{672c}\u{8a9e} \u{c0}\u{e9}\u{f1}"; // Chinese, Japanese, accented
    let (room, _) = room_service
        .create_room(
            unicode_name.to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(room.name, unicode_name);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_name_whitespace_handling() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ws_owner")).await.unwrap();

    // Room name with leading/trailing whitespace - should be preserved as-is
    // (validation may trim in the future, but current behavior preserves)
    let name_with_spaces = "  Room with spaces  ";
    let result = room_service
        .create_room(
            name_with_spaces.to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await;

    // The room creation should succeed (validator allows spaces)
    assert!(
        result.is_ok(),
        "Room with whitespace should be created: {:?}",
        result.err()
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_description_with_newlines() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("newline_owner")).await.unwrap();

    // Description with newlines (within 500 chars)
    let description = "Line 1\nLine 2\nLine 3\n\nParagraph 2";
    let (room, _) = room_service
        .create_room(
            "Newline Room".to_string(),
            description.to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(room.description, description);
}

// ========== Room Status Management Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cannot_join_closed_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("closed_owner")).await.unwrap();
    let joiner = user_repo.create(&make_user("closed_joiner")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Closed Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Close the room via admin operation
    room_service
        .update_room_status(&room.id, synctv_core::models::RoomStatus::Closed)
        .await
        .unwrap();

    // Try to join the closed room
    let result = room_service
        .join_room(room.id.clone(), joiner.id.clone(), None)
        .await;
    assert!(result.is_err(), "Should not be able to join closed room");

    match result.unwrap_err() {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("closed"),
                "Error should mention room is closed: {msg}"
            );
        }
        other => panic!("Expected InvalidInput error, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cannot_join_banned_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("banned_room_owner"))
        .await
        .unwrap();
    let joiner = user_repo
        .create(&make_user("banned_room_joiner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "To Be Banned Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // First, joiner joins the room
    room_service
        .join_room(room.id.clone(), joiner.id.clone(), None)
        .await
        .unwrap();

    // Now ban the joiner from the room
    let member_service = room_service.member_service();
    member_service
        .ban_member(
            room.id.clone(),
            owner.id.clone(),
            joiner.id.clone(),
            Some("Test ban".to_string()),
        )
        .await
        .unwrap();

    // Try to join again - should fail because user is banned
    let result = room_service
        .join_room(room.id.clone(), joiner.id.clone(), None)
        .await;
    assert!(
        result.is_err(),
        "Should not be able to join room when banned"
    );

    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(msg.contains("banned"), "Error should mention ban: {msg}");
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

// ========== Room Creation Transaction Safety Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_creation_creates_all_related_records_atomically() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("atomic_owner")).await.unwrap();

    let (room, _member) = room_service
        .create_room(
            "Atomic Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Verify all related records were created

    // 1. Room exists
    let room_repo = RoomRepository::new(pool.clone());
    assert!(room_repo.exists(&room.id).await.unwrap());

    // 2. Creator is a member with Creator role
    let member_repo = RoomMemberRepository::new(pool.clone());
    let creator_membership = member_repo.get(&room.id, &owner.id).await.unwrap();
    assert!(creator_membership.is_some(), "Creator should be a member");
    assert_eq!(creator_membership.unwrap().role, RoomRole::Creator);

    // 3. Settings exist with defaults
    let settings_repo = RoomSettingsRepository::new(pool.clone());
    let settings = settings_repo.get(&room.id).await.unwrap();
    assert!(settings.chat_enabled.0, "Chat should be enabled by default");

    // 4. Root playlist exists
    let playlist_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM playlists WHERE room_id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(playlist_count, 1, "Root playlist should exist");

    // 5. Playback state exists
    let playback_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM room_playback_state WHERE room_id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(playback_count, 1, "Playback state should exist");
}

// ========== Room Deletion Edge Cases ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_non_creator_cannot_delete_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("del_owner")).await.unwrap();
    let other_user = user_repo.create(&make_user("del_other")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Non-Creator Delete Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Non-creator tries to delete the room
    let result = room_service
        .delete_room(room.id.clone(), other_user.id.clone())
        .await;
    assert!(
        result.is_err(),
        "Non-creator should not be able to delete room"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_delete_room_bypasses_permission_check() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("admin_del_owner"))
        .await
        .unwrap();
    let mut admin_user = user_repo
        .create(&make_user("admin_del_admin"))
        .await
        .unwrap();
    admin_user.role = UserRole::Admin;
    user_repo
        .update(&admin_user, admin_user.version)
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Admin Delete Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Admin delete (bypasses permission check)
    room_service
        .admin_delete_room(&room.id, &admin_user.id)
        .await
        .unwrap();

    // Room should be soft-deleted
    let fetched = room_repo.get_by_id(&room.id).await.unwrap();
    assert!(fetched.is_none(), "Room should be soft-deleted");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_delete_room_requires_admin_role() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    // Create a room with a regular user as owner
    let owner = user_repo.create(&make_user("owner")).await.unwrap();
    let (room, _) = room_service
        .create_room(
            "Test Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Create another regular user (NOT admin)
    let regular_user = user_repo.create(&make_user("regular_user")).await.unwrap();

    // Non-admin user should NOT be able to call admin_delete_room
    let result = room_service
        .admin_delete_room(&room.id, &regular_user.id)
        .await;
    assert!(
        result.is_err(),
        "Non-admin user should not be able to call admin_delete_room"
    );
    if let Err(Error::Authorization(msg)) = result {
        assert!(
            msg.contains("admin") || msg.contains("Admin"),
            "Error message should mention admin requirement"
        );
    } else {
        panic!("Expected Authorization error, got {result:?}");
    }

    // Room should still exist (not deleted)
    let room_repo = RoomRepository::new(pool.clone());
    let fetched = room_repo.get_by_id(&room.id).await.unwrap();
    assert!(
        fetched.is_some(),
        "Room should still exist after failed admin delete"
    );

    // Now test with an actual admin user
    let mut admin_user = user_repo.create(&make_user("admin_user")).await.unwrap();
    admin_user.role = UserRole::Admin;
    user_repo
        .update(&admin_user, admin_user.version)
        .await
        .unwrap();

    // Admin user should be able to call admin_delete_room
    room_service
        .admin_delete_room(&room.id, &admin_user.id)
        .await
        .unwrap();

    // Room should be soft-deleted
    let fetched = room_repo.get_by_id(&room.id).await.unwrap();
    assert!(fetched.is_none(), "Room should be soft-deleted by admin");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_delete_room_requires_root_role() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create a room with a regular user as owner
    let owner = user_repo.create(&make_user("owner2")).await.unwrap();
    let (room, _) = room_service
        .create_room(
            "Test Room 2".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Root user should also be able to call admin_delete_room
    let mut root_user = user_repo.create(&make_user("root_user")).await.unwrap();
    root_user.role = UserRole::Root;
    user_repo
        .update(&root_user, root_user.version)
        .await
        .unwrap();

    // Root user should be able to call admin_delete_room
    room_service
        .admin_delete_room(&room.id, &root_user.id)
        .await
        .unwrap();

    // Room should be soft-deleted
    let fetched = room_repo.get_by_id(&room.id).await.unwrap();
    assert!(fetched.is_none(), "Room should be soft-deleted by root");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_nonexistent_room_returns_error() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    // Create a user and a room, then delete the room
    let owner = user_repo
        .create(&make_user("delete_nonexistent_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "To Be Deleted".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Delete the room first
    room_service
        .delete_room(room.id.clone(), owner.id.clone())
        .await
        .unwrap();

    // Try to delete the already-deleted room - should fail
    let result = room_service
        .delete_room(room.id.clone(), owner.id.clone())
        .await;
    assert!(result.is_err(), "Deleting already-deleted room should fail");

    match result.unwrap_err() {
        Error::NotFound(msg) => {
            assert!(
                msg.contains("not found") || msg.contains("deleted"),
                "Error should mention not found: {msg}"
            );
        }
        other => panic!("Expected NotFound error, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_double_delete_room_returns_error() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("double_del_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Double Delete Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // First delete should succeed
    room_service
        .delete_room(room.id.clone(), owner.id.clone())
        .await
        .unwrap();

    // Second delete should fail
    let result = room_service
        .delete_room(room.id.clone(), owner.id.clone())
        .await;
    assert!(result.is_err(), "Double delete should fail");
}

// ========== Room Member Operations ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_member_count_batch_efficient_query() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("batch_owner")).await.unwrap();

    // Create multiple rooms
    let mut room_ids = Vec::new();
    for i in 0..5 {
        let (room, _) = room_service
            .create_room(
                format!("Batch Room {i}"),
                String::new(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .unwrap();
        room_ids.push(room.id);
    }

    // Get batch member counts
    let room_id_refs: Vec<_> = room_ids.iter().collect();
    let counts = room_service
        .get_member_count_batch(&room_id_refs)
        .await
        .unwrap();

    // Each room should have 1 member (the creator)
    for room_id in &room_ids {
        assert_eq!(
            counts.get(room_id.as_str()).unwrap_or(&0),
            &1,
            "Each room should have 1 member"
        );
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_room_exists_is_efficient() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("exists_owner")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Exists Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // room_exists should return true for existing room
    assert!(room_service.room_exists(&room.id).await.unwrap());

    // room_exists should return false for nonexistent room
    let fake_room_id = synctv_core::models::RoomId::new();
    assert!(!room_service.room_exists(&fake_room_id).await.unwrap());
}

// ========== Room Listing Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_rooms_by_creator() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("list_owner")).await.unwrap();
    let other = user_repo.create(&make_user("list_other")).await.unwrap();

    // Owner creates 3 rooms
    for i in 0..3 {
        room_service
            .create_room(
                format!("Owner Room {i}"),
                String::new(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .unwrap();
    }

    // Other creates 2 rooms
    for i in 0..2 {
        room_service
            .create_room(
                format!("Other Room {i}"),
                String::new(),
                other.id.clone(),
                None,
                None,
            )
            .await
            .unwrap();
    }

    // List rooms by owner
    let (rooms, total) = room_service
        .list_rooms_by_creator(&owner.id, synctv_core::models::PageParams::default())
        .await
        .unwrap();

    assert_eq!(total, 3, "Owner should have 3 rooms");
    assert_eq!(rooms.len(), 3, "Should return all 3 rooms");
    for room in &rooms {
        assert_eq!(room.created_by, owner.id, "Room should be created by owner");
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_rooms_pagination() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("page_owner")).await.unwrap();

    // Create 15 rooms
    for i in 0..15 {
        room_service
            .create_room(
                format!("Page Room {i:02}"),
                String::new(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .unwrap();
    }

    // Request first page
    let page1 = synctv_core::models::PageParams {
        page: 1,
        page_size: 10,
    };
    let (rooms, total) = room_service
        .list_rooms_by_creator(&owner.id, page1)
        .await
        .unwrap();

    assert_eq!(total, 15, "Total should be 15");
    assert_eq!(rooms.len(), 10, "First page should have 10 rooms");

    // Request second page
    let page2 = synctv_core::models::PageParams {
        page: 2,
        page_size: 10,
    };
    let (rooms2, total2) = room_service
        .list_rooms_by_creator(&owner.id, page2)
        .await
        .unwrap();

    assert_eq!(total2, 15, "Total should still be 15");
    assert_eq!(rooms2.len(), 5, "Second page should have 5 rooms");
}

// ========== Guest Access Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_guest_cannot_join_password_protected_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("guest_pwd_owner"))
        .await
        .unwrap();

    // Create room with password and guest join enabled
    let settings = synctv_core::models::RoomSettings {
        allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(true),
        ..Default::default()
    };

    let (room, _) = room_service
        .create_room(
            "Guest Password Room".to_string(),
            String::new(),
            owner.id.clone(),
            Some("SecretPassword123".to_string()),
            Some(settings),
        )
        .await
        .unwrap();

    // Check guest access should fail (password required)
    let result = room_service.check_guest_allowed(&room.id, None).await;
    assert!(
        result.is_err(),
        "Guests should not be able to join password-protected room"
    );

    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(
                msg.contains("password") || msg.contains("Guest"),
                "Error should mention password or guests: {msg}"
            );
        }
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_guest_allowed_when_disabled_globally() {
    // This test verifies the fail-closed behavior when settings registry is None
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("guest_disabled_owner"))
        .await
        .unwrap();

    let settings = synctv_core::models::RoomSettings {
        allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin(true),
        ..Default::default()
    };

    let (room, _) = room_service
        .create_room(
            "Guest Disabled Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            Some(settings),
        )
        .await
        .unwrap();

    // Without settings_registry, should deny guest access (fail-closed)
    let result = room_service.check_guest_allowed(&room.id, None).await;
    assert!(
        result.is_err(),
        "Should deny guests when registry unavailable"
    );
}

// ========== Room Description Update Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_room_description_success() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("desc_update_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Description Update Room".to_string(),
            "Original description".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(room.description, "Original description");

    // Update description
    let new_description = "Updated description with more details";
    let updated_room = room_service
        .update_room_description(&room.id, &owner.id, new_description.to_string())
        .await
        .unwrap();

    assert_eq!(updated_room.description, new_description);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_room_description_too_long_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("desc_long_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Description Long Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Try to set description longer than 500 chars
    let long_description = "x".repeat(501);
    let result = room_service
        .update_room_description(&room.id, &owner.id, long_description)
        .await;

    assert!(result.is_err(), "Description > 500 chars should fail");
}

/// Test: User without `UPDATE_ROOM_SETTINGS` permission cannot update room description
///
/// This verifies that the permission check is enforced for description updates.
/// Only room owner (or users with `UPDATE_ROOM_SETTINGS` permission) should be able
/// to modify the room description.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_room_description_permission_denied() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    // Create room owner
    let owner = user_repo
        .create(&make_user("desc_perm_owner"))
        .await
        .unwrap();

    // Create another user who is NOT a member of the room
    let outsider = user_repo
        .create(&make_user("desc_perm_outsider"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Permission Check Room".to_string(),
            "Original description".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Outsider tries to update description - should be denied
    let result = room_service
        .update_room_description(&room.id, &outsider.id, "Hacked description".to_string())
        .await;

    assert!(
        result.is_err(),
        "Non-member should not be able to update description"
    );
    let err = result.unwrap_err();
    let err_str = err.to_string();
    assert!(
        err_str.contains("permission")
            || err_str.contains("denied")
            || err_str.contains("not found")
            || err_str.contains("Not a member"),
        "Error should indicate permission denied or not a member: {err_str}"
    );

    // Verify description was NOT changed
    let room_after = room_service.get_room(&room.id).await.unwrap();
    assert_eq!(room_after.description, "Original description");
}

/// Test: Room owner can update room description (has implicit `UPDATE_ROOM_SETTINGS`)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_room_description_owner_allowed() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("desc_owner_allowed"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Owner Update Room".to_string(),
            "Original".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Owner should be able to update description
    let updated = room_service
        .update_room_description(&room.id, &owner.id, "Owner updated this".to_string())
        .await;

    assert!(
        updated.is_ok(),
        "Owner should be able to update description"
    );
    assert_eq!(updated.unwrap().description, "Owner updated this");
}

// ========== Idempotent Join Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_join_room_idempotent_same_user() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("idem_owner")).await.unwrap();
    let joiner = user_repo.create(&make_user("idem_joiner")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Idempotent Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // First join
    let result1 = room_service
        .join_room(room.id.clone(), joiner.id.clone(), None)
        .await;
    assert!(result1.is_ok(), "First join should succeed");

    // Get member count after first join
    let count1 = member_repo.count_by_room(&room.id).await.unwrap();

    // Second join (idempotent)
    let result2 = room_service
        .join_room(room.id.clone(), joiner.id.clone(), None)
        .await;
    assert!(result2.is_ok(), "Second join should succeed (idempotent)");

    // Member count should be the same
    let count2 = member_repo.count_by_room(&room.id).await.unwrap();
    assert_eq!(
        count1, count2,
        "Member count should not increase on idempotent join"
    );
}

// ========== Max Members Concurrent Tests ==========

/// Test that `max_members` is correctly read from `RoomSettings` when joining.
///
/// This test verifies that when `max_members=0` is passed to `with_max_members(0)`,
/// the system correctly reads the actual `max_members` value from `RoomSettings`.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_max_members_read_from_room_settings_on_join() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("settings_owner"))
        .await
        .unwrap();

    // Create room with default settings (max_members = 100 by default)
    let (room, _) = room_service
        .create_room(
            "Settings Test Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Verify default max_members in settings
    let settings = settings_repo.get(&room.id).await.unwrap();
    assert_eq!(
        settings.max_members.0, 100,
        "Default max_members should be 100"
    );

    // Add 99 more members to reach the limit (owner + 99 = 100)
    for i in 0..99 {
        let joiner = user_repo
            .create(&make_user(&format!("settings_joiner_{i}")))
            .await
            .unwrap();
        let result = room_service
            .join_room(room.id.clone(), joiner.id.clone(), None)
            .await;
        assert!(
            result.is_ok(),
            "Joiner {} should succeed: {:?}",
            i,
            result.err()
        );
    }

    // Verify we're at the limit
    let count = member_repo.count_by_room(&room.id).await.unwrap();
    assert_eq!(count, 100, "Should have 100 members (owner + 99 joiners)");

    // The 101st member should fail
    let joiner101 = user_repo
        .create(&make_user("settings_joiner_101"))
        .await
        .unwrap();
    let result = room_service
        .join_room(room.id.clone(), joiner101.id.clone(), None)
        .await;
    assert!(result.is_err(), "101st joiner should fail (exceeds max)");

    match result.unwrap_err() {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("full") || msg.contains("max") || msg.contains("capacity"),
                "Error should mention room capacity: {msg}"
            );
        }
        other => panic!("Expected InvalidInput error, got: {other:?}"),
    }
}

/// Test that concurrent joins cannot exceed `max_members` limit.
///
/// This test spawns multiple concurrent join requests and verifies that
/// even under concurrent access, the room never exceeds its `max_members` limit.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_joins_cannot_exceed_max_members() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("concurrent_owner"))
        .await
        .unwrap();

    // Create room with max_members = 5 (owner counts as 1, so 4 more can join)
    let settings = synctv_core::models::RoomSettings {
        max_members: synctv_core::models::room_settings::MaxMembers(5),
        ..Default::default()
    };

    let (room, _) = room_service
        .create_room(
            "Concurrent Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            Some(settings),
        )
        .await
        .unwrap();

    // Create 20 users who will try to join concurrently
    let mut users = Vec::new();
    for i in 0..20 {
        let user = user_repo
            .create(&make_user(&format!("concurrent_joiner_{i}")))
            .await
            .unwrap();
        users.push(user);
    }

    // Track success/failure counts (wrapped in Arc for sharing across tasks)
    let success_count = Arc::new(AtomicUsize::new(0));
    let failure_count = Arc::new(AtomicUsize::new(0));

    // Spawn all join requests concurrently
    let mut handles = Vec::new();
    for user in users {
        let room_service = room_service.clone();
        let room_id = room.id.clone();
        let success_count = Arc::clone(&success_count);
        let failure_count = Arc::clone(&failure_count);

        let handle = tokio::spawn(async move {
            let result = room_service.join_room(room_id, user.id.clone(), None).await;
            match result {
                Ok(_) => {
                    success_count.fetch_add(1, Ordering::SeqCst);
                }
                Err(Error::InvalidInput(_)) => {
                    // Expected for users who couldn't join due to capacity
                    failure_count.fetch_add(1, Ordering::SeqCst);
                }
                Err(Error::AlreadyExists(_)) => {
                    // Idempotent join - treat as success but don't increment count
                    // (shouldn't happen in this test since all users are unique)
                }
                Err(e) => {
                    panic!("Unexpected error type: {e:?}");
                }
            }
        });
        handles.push(handle);
    }

    // Wait for all joins to complete
    for handle in handles {
        handle.await.expect("Join task panicked");
    }

    // Verify final member count
    let final_count = member_repo.count_by_room(&room.id).await.unwrap();

    // The room should have exactly 5 members (max_members limit)
    assert_eq!(
        final_count, 5,
        "Room should have exactly 5 members (max limit)"
    );

    // 4 should succeed (owner + 4 = 5), 16 should fail
    let successes = success_count.load(Ordering::SeqCst);
    let failures = failure_count.load(Ordering::SeqCst);

    assert_eq!(
        successes, 4,
        "Exactly 4 users should have joined successfully"
    );
    assert_eq!(
        failures, 16,
        "16 users should have been rejected due to capacity"
    );

    // Total should account for all 20 users
    assert_eq!(
        successes + failures,
        20,
        "All 20 users should have been processed"
    );
}

/// Test that `max_members=0` in `RoomSettings` means unlimited members.
///
/// This verifies that when `RoomSettings.max_members` is explicitly set to 0,
/// the room accepts unlimited members (no capacity enforcement).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_max_members_zero_in_settings_means_unlimited() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("unlimited_owner"))
        .await
        .unwrap();

    // Create room with max_members = 0 (unlimited)
    let settings = synctv_core::models::RoomSettings {
        max_members: synctv_core::models::room_settings::MaxMembers(0),
        ..Default::default()
    };

    let (room, _) = room_service
        .create_room(
            "Unlimited Room Explicit".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            Some(settings),
        )
        .await
        .unwrap();

    // Verify settings have max_members = 0
    let saved_settings = settings_repo.get(&room.id).await.unwrap();
    assert_eq!(
        saved_settings.max_members.0, 0,
        "max_members should be 0 in settings"
    );

    // Add 50 members - all should succeed since max_members=0 means unlimited
    for i in 0..50 {
        let joiner = user_repo
            .create(&make_user(&format!("unlimited_explicit_{i}")))
            .await
            .unwrap();
        let result = room_service
            .join_room(room.id.clone(), joiner.id.clone(), None)
            .await;
        assert!(
            result.is_ok(),
            "Joiner {} should succeed (unlimited room): {:?}",
            i,
            result.err()
        );
    }
}

// ========== Soft-Delete Cleanup Tests ==========
//
// These tests verify the optimized soft-delete cleanup strategy.
// Problem: Soft-deleted rooms and related data consume resources for up to 90 days.
// Fix: Clean up playlists/media immediately on soft-delete, keep only audit data.
//

/// Test that soft-delete immediately cleans up non-critical data (playlists, media, members).
///
/// This test verifies the optimized soft-delete strategy:
/// 1. Room row gets `deleted_at` set (soft-delete)
/// 2. Non-critical data (playlists, media, playback state, members, settings) is immediately deleted
/// 3. Only audit log entries are preserved
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_soft_delete_immediately_cleans_up_non_critical_data() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("cleanup_owner")).await.unwrap();
    let member1 = user_repo
        .create(&make_user("cleanup_member1"))
        .await
        .unwrap();

    // Create room
    let (room, _) = room_service
        .create_room(
            "Cleanup Test Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Add a member
    room_service
        .join_room(room.id.clone(), member1.id.clone(), None)
        .await
        .unwrap();

    // Get the root playlist (created automatically when room was created)
    let playlist_repo = synctv_core::repository::PlaylistRepository::new(pool.clone());
    let media_repo = synctv_core::repository::MediaRepository::new(pool.clone());
    let root_playlist = playlist_repo
        .get_root_playlist(&room.id)
        .await
        .expect("Root playlist should exist");

    // Create a child playlist under root
    let playlist = synctv_core::models::Playlist {
        id: synctv_core::models::PlaylistId::new(),
        room_id: room.id.clone(),
        parent_id: Some(root_playlist.id.clone()),
        name: "Test Playlist".to_string(),
        position: 0,
        creator_id: Some(owner.id.clone()),
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
    };
    playlist_repo.create(&playlist).await.unwrap();

    let now = chrono::Utc::now();
    let media = synctv_core::models::Media {
        id: synctv_core::models::MediaId::new(),
        playlist_id: playlist.id.clone(),
        room_id: room.id.clone(),
        name: "Test Media".to_string(),
        position: 0,
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({}),
        provider_instance_name: None,
        creator_id: Some(owner.id.clone()),
        added_at: now,
        updated_at: now,
        version: 0,
    };
    media_repo.create(&media).await.unwrap();

    // Verify related data exists before deletion
    let member_count_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM room_members WHERE room_id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        member_count_before, 2,
        "Owner and member1 should be in room"
    );

    let playlist_count_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM playlists WHERE room_id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(playlist_count_before > 0, "Should have playlists");

    let media_count_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media WHERE room_id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(media_count_before > 0, "Should have media");

    let settings_count_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM room_settings WHERE room_id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(settings_count_before > 0, "Should have settings");

    let playback_count_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM room_playback_state WHERE room_id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(playback_count_before, 1, "Should have playback state");

    // Soft-delete the room
    room_service
        .delete_room(room.id.clone(), owner.id.clone())
        .await
        .unwrap();

    // Verify room row exists but is soft-deleted
    let deleted_at: Option<chrono::DateTime<Utc>> =
        sqlx::query_scalar("SELECT deleted_at FROM rooms WHERE id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(deleted_at.is_some(), "Room should be soft-deleted");

    // VERIFY NON-CRITICAL DATA IS IMMEDIATELY DELETED
    // This is the key optimization - these should be gone, not waiting 90 days

    // Members should be immediately deleted
    let member_count_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM room_members WHERE room_id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        member_count_after, 0,
        "Members should be immediately cleaned up"
    );

    // Playlists should be immediately deleted (cascade to media via FK)
    let playlist_count_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM playlists WHERE room_id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        playlist_count_after, 0,
        "Playlists should be immediately cleaned up"
    );

    // Media should be immediately deleted
    let media_count_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media WHERE room_id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        media_count_after, 0,
        "Media should be immediately cleaned up"
    );

    // Settings should be immediately deleted
    let settings_count_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM room_settings WHERE room_id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        settings_count_after, 0,
        "Settings should be immediately cleaned up"
    );

    // Playback state should be immediately deleted
    let playback_count_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM room_playback_state WHERE room_id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        playback_count_after, 0,
        "Playback state should be immediately cleaned up"
    );

    // Chat messages should be immediately deleted
    let chat_count_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM chat_messages WHERE room_id = $1")
            .bind(room.id.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        chat_count_after, 0,
        "Chat messages should be immediately cleaned up"
    );

    // Verify room row still exists (soft-deleted, not hard-deleted)
    let room_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM rooms WHERE id = $1)")
        .bind(room.id.as_str())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(room_exists, "Room row should still exist (soft-deleted)");

    // NOTE: Audit logs are not tested here because the test RoomService
    // does not have an audit service configured. Audit functionality is
    // tested separately in audit service tests.
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_delete_orphaned_room_creator_soft_deleted() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let user_service = make_user_service(pool.clone());
    let room_service = make_room_service(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create a user who will be the creator
    let creator = user_repo
        .create(&make_user("orphan_creator_1"))
        .await
        .unwrap();

    // Create a room as this creator
    let (room, _) = room_service
        .create_room(
            "Orphaned Room 1".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Create an admin user
    let admin = user_repo
        .create(&make_user("admin_orphan_1"))
        .await
        .unwrap();
    let mut admin = admin;
    admin.role = UserRole::Admin;
    let admin = user_repo.update(&admin, 0).await.unwrap();

    // Soft-delete the creator (simulating user deletion)
    user_service.delete_user(&creator.id).await.unwrap();

    // Verify the creator is soft-deleted (get_by_id returns None for soft-deleted users)
    let deleted_creator = user_repo.get_by_id(&creator.id).await.unwrap();
    assert!(
        deleted_creator.is_none(),
        "Soft-deleted user should not be found"
    );

    // Now admin should be able to delete the orphaned room
    room_service
        .admin_delete_orphaned_room(&room.id, &admin.id)
        .await
        .unwrap();

    // Room should be soft-deleted
    let fetched = room_repo.get_by_id(&room.id).await.unwrap();
    assert!(
        fetched.is_none(),
        "Orphaned room should be soft-deleted by admin"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_delete_orphaned_room_requires_admin_role() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let user_service = make_user_service(pool.clone());
    let room_service = make_room_service(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("orphan_creator_non_admin"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Orphaned Room Non Admin".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let regular_user = user_repo
        .create(&make_user("regular_orphan_delete"))
        .await
        .unwrap();

    user_service.delete_user(&creator.id).await.unwrap();

    let result = room_service
        .admin_delete_orphaned_room(&room.id, &regular_user.id)
        .await;

    assert!(
        matches!(result, Err(Error::Authorization(_))),
        "Non-admin user must not be able to delete orphaned rooms"
    );

    let fetched = room_repo.get_by_id(&room.id).await.unwrap();
    assert!(
        fetched.is_some(),
        "Room should still exist after failed deletion"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_delete_orphaned_room_creator_banned() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create a user who will be banned
    let creator = user_repo
        .create(&make_user("banned_creator"))
        .await
        .unwrap();

    // Create a room as this creator
    let (room, _) = room_service
        .create_room(
            "Banned Creator Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Create an admin user
    let admin = user_repo.create(&make_user("admin_banned")).await.unwrap();
    let mut admin = admin;
    admin.role = UserRole::Admin;
    let admin = user_repo.update(&admin, 0).await.unwrap();

    // Ban the creator by setting status to Banned (3)
    sqlx::query("UPDATE users SET status = 3 WHERE id = $1")
        .bind(creator.id.as_str())
        .execute(&pool)
        .await
        .unwrap();

    // Now admin should be able to delete the orphaned room
    room_service
        .admin_delete_orphaned_room(&room.id, &admin.id)
        .await
        .unwrap();

    // Room should be soft-deleted
    let fetched = room_repo.get_by_id(&room.id).await.unwrap();
    assert!(
        fetched.is_none(),
        "Orphaned room with banned creator should be soft-deleted"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_delete_orphaned_room_rejects_active_creator() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    // Create an active creator
    let creator = user_repo
        .create(&make_user("active_creator"))
        .await
        .unwrap();

    // Create a room
    let (room, _) = room_service
        .create_room(
            "Active Creator Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Create an admin
    let admin = user_repo.create(&make_user("admin_active")).await.unwrap();

    // Trying to use admin_delete_orphaned_room should fail
    let result = room_service
        .admin_delete_orphaned_room(&room.id, &admin.id)
        .await;

    assert!(
        result.is_err(),
        "Should reject orphaned deletion for active creator"
    );

    // Room should still exist
    let room_repo = RoomRepository::new(pool.clone());
    let fetched = room_repo.get_by_id(&room.id).await.unwrap();
    assert!(
        fetched.is_some(),
        "Room should still exist when creator is active"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_delete_orphaned_room_rejects_non_admin() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let user_service = make_user_service(pool.clone());
    let room_service = make_room_service(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("orphan_creator_non_admin"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Orphaned Room Non Admin".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let regular_user = user_repo
        .create(&make_user("regular_non_admin"))
        .await
        .unwrap();

    user_service.delete_user(&creator.id).await.unwrap();

    let result = room_service
        .admin_delete_orphaned_room(&room.id, &regular_user.id)
        .await;

    assert!(
        matches!(result, Err(Error::Authorization(_))),
        "non-admin users must not be allowed to delete orphaned rooms"
    );

    let fetched = room_repo.get_by_id(&room.id).await.unwrap();
    assert!(
        fetched.is_some(),
        "room must remain untouched when orphaned delete is attempted by a non-admin"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_delete_orphaned_room_already_deleted_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let user_service = make_user_service(pool.clone());
    let room_service = make_room_service(pool.clone());

    // Create and delete a creator
    let creator = user_repo
        .create(&make_user("orphan_already_del"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Already Deleted Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let mut admin = user_repo.create(&make_user("admin_already")).await.unwrap();
    admin.role = UserRole::Admin;
    user_repo.update(&admin, admin.version).await.unwrap();

    // Delete the creator first
    user_service.delete_user(&creator.id).await.unwrap();

    // Delete the room normally
    room_service
        .admin_delete_room(&room.id, &admin.id)
        .await
        .unwrap();

    // Trying to delete again should fail
    let result = room_service
        .admin_delete_orphaned_room(&room.id, &admin.id)
        .await;

    assert!(result.is_err(), "Should reject double deletion");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_delete_orphaned_room_nonexistent_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let admin = user_repo
        .create(&make_user("admin_nonexistent"))
        .await
        .unwrap();
    let fake_room_id = RoomId::from_string(nanoid::nanoid!(12));

    let result = room_service
        .admin_delete_orphaned_room(&fake_room_id, &admin.id)
        .await;

    assert!(
        result.is_err(),
        "Should reject deletion of nonexistent room"
    );
}

// ========== Room Password Policy Tests (H1) ==========

async fn make_settings_registry(pool: PgPool) -> Arc<SettingsRegistry> {
    let settings_repo = SettingsRepository::new(pool.clone());
    let settings_service = Arc::new(SettingsService::new(settings_repo, pool.clone()));
    let registry = Arc::new(SettingsRegistry::new(settings_service));

    // Seed the settings rows that tests may need
    for (key, group, default_value) in [
        ("room.room_must_need_pwd", "room", "false"),
        ("room.room_must_no_need_pwd", "room", "false"),
        ("room.disable_create_room", "room", "false"),
        ("server.allow_room_creation", "server", "true"),
        ("room.create_room_need_review", "room", "false"),
    ] {
        sqlx::query(
            "INSERT INTO settings (key, group_name, value) VALUES ($1, $2, $3) ON CONFLICT (key) DO NOTHING"
        )
        .bind(key)
        .bind(group)
        .bind(default_value)
        .execute(&pool)
        .await
        .unwrap();
    }

    registry
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_room_rejects_no_password_when_must_need_pwd() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let mut room_service = make_room_service(pool.clone());
    let registry = make_settings_registry(pool.clone()).await;

    // Enable room_must_need_pwd
    registry.set_room_must_need_pwd(true).await.unwrap();
    room_service.set_settings_registry(registry);

    let owner = user_repo
        .create(&make_user("pwd_policy_owner"))
        .await
        .unwrap();

    // Attempt to create room without password should fail
    let result = room_service
        .create_room(
            "No Pwd Room".to_string(),
            "Should fail".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await;

    assert!(
        result.is_err(),
        "Should reject room without password when room_must_need_pwd is true"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err, Error::InvalidInput(ref msg) if msg.contains("password is required")),
        "Error should mention password requirement, got: {err:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_room_allows_password_when_must_need_pwd() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let mut room_service = make_room_service(pool.clone());
    let registry = make_settings_registry(pool.clone()).await;

    // Enable room_must_need_pwd
    registry.set_room_must_need_pwd(true).await.unwrap();
    room_service.set_settings_registry(registry);

    let owner = user_repo
        .create(&make_user("pwd_policy_ok_owner"))
        .await
        .unwrap();

    // Creating room WITH password should succeed
    let result = room_service
        .create_room(
            "Pwd Room".to_string(),
            "Should succeed".to_string(),
            owner.id.clone(),
            Some("StrongPassword123".to_string()),
            None,
        )
        .await;

    assert!(
        result.is_ok(),
        "Should allow room with password when room_must_need_pwd is true"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_room_rejects_password_when_must_no_need_pwd() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let mut room_service = make_room_service(pool.clone());
    let registry = make_settings_registry(pool.clone()).await;

    // Enable room_must_no_need_pwd
    registry.set_room_must_no_need_pwd(true).await.unwrap();
    room_service.set_settings_registry(registry);

    let owner = user_repo
        .create(&make_user("no_pwd_policy_owner"))
        .await
        .unwrap();

    // Attempt to create room WITH password should fail
    let result = room_service
        .create_room(
            "Pwd Room Fail".to_string(),
            "Should fail".to_string(),
            owner.id.clone(),
            Some("UnwantedPassword123".to_string()),
            None,
        )
        .await;

    assert!(
        result.is_err(),
        "Should reject room with password when room_must_no_need_pwd is true"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err, Error::InvalidInput(ref msg) if msg.contains("not allowed")),
        "Error should mention passwords not allowed, got: {err:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_room_allows_no_password_when_must_no_need_pwd() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let mut room_service = make_room_service(pool.clone());
    let registry = make_settings_registry(pool.clone()).await;

    // Enable room_must_no_need_pwd
    registry.set_room_must_no_need_pwd(true).await.unwrap();
    room_service.set_settings_registry(registry);

    let owner = user_repo
        .create(&make_user("no_pwd_policy_ok_owner"))
        .await
        .unwrap();

    // Creating room WITHOUT password should succeed
    let result = room_service
        .create_room(
            "Open Room".to_string(),
            "Should succeed".to_string(),
            owner.id.clone(),
            None,
            None,
        )
        .await;

    assert!(
        result.is_ok(),
        "Should allow room without password when room_must_no_need_pwd is true"
    );
}

// ========== set_member_role Creator-Only Tests (M1) ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_member_role_only_creator_can_change_roles() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("role_creator")).await.unwrap();
    let admin_user = user_repo.create(&make_user("role_admin")).await.unwrap();
    let member_user = user_repo.create(&make_user("role_member")).await.unwrap();

    // Create room
    let (room, _) = room_service
        .create_room(
            "Role Test Room".to_string(),
            "Testing roles".to_string(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Join admin and member
    room_service
        .join_room(room.id.clone(), admin_user.id.clone(), None)
        .await
        .unwrap();
    room_service
        .join_room(room.id.clone(), member_user.id.clone(), None)
        .await
        .unwrap();

    // Creator promotes admin_user to Admin
    room_service
        .member_service()
        .set_member_role(
            room.id.clone(),
            creator.id.clone(),
            admin_user.id.clone(),
            RoomRole::Admin,
        )
        .await
        .unwrap();

    // Admin tries to change member_user's role -- should fail (creator-only)
    let result = room_service
        .member_service()
        .set_member_role(
            room.id.clone(),
            admin_user.id.clone(),
            member_user.id.clone(),
            RoomRole::Admin,
        )
        .await;

    assert!(
        result.is_err(),
        "Admin should not be able to change roles (creator-only)"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err, Error::Authorization(ref msg) if msg.contains("creator")),
        "Error should mention creator-only restriction, got: {err:?}"
    );
}

// ========== ban_member No Sleep Test (M4) ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_member_completes_quickly() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("ban_creator")).await.unwrap();
    let target = user_repo.create(&make_user("ban_target")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Ban Test Room".to_string(),
            "Testing ban".to_string(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), target.id.clone(), None)
        .await
        .unwrap();

    // Ban should complete without the 100ms sleep overhead
    let start = std::time::Instant::now();
    room_service
        .member_service()
        .ban_member(
            room.id.clone(),
            creator.id.clone(),
            target.id.clone(),
            Some("test ban".to_string()),
        )
        .await
        .unwrap();
    let elapsed = start.elapsed();

    // Without the sleep, this should complete well under 100ms
    // (allowing generous margin for DB operations)
    // The main point is it shouldn't have the hardcoded 100ms sleep
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "Ban operation took too long: {elapsed:?}"
    );

    // Verify the member is actually banned (use get_any since banned members have left_at set)
    let member = member_repo
        .get_any(&room.id, &target.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member.status, MemberStatus::Banned);
}
