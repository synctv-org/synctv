//! User service tests
//!
//! Tests user registration and login validation using testcontainers.
//!
//! Run with: cargo test --test `user_service_tests`
//! Run Docker tests: cargo test --test `user_service_tests` -- --ignored
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        Media, MediaId, MemberStatus, Playlist, PlaylistId, Room, RoomId, RoomMember, RoomStatus,
        SignupMethod, User, UserId, UserRole, UserStatus,
    },
    repository::{
        MediaRepository, PlaylistRepository, RoomMemberRepository, RoomRepository, UserRepository,
    },
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        InMemoryTokenBlacklistStore, UserService,
    },
    Error,
};
use synctv_core_testing::create_test_pool;
use tokio::sync::Barrier;

fn create_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-user-service-tests-long-enough-1234567890").unwrap()
}

fn create_user_service(pool: PgPool) -> UserService {
    let jwt = create_jwt_service();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let password_config = PasswordComplexityConfig::default();
    let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    let mut svc = UserService::new(
        pool,
        jwt,
        username_cache,
        password_config,
        token_blacklist,
        key_builder,
        brute_force,
    );
    svc.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    svc
}

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@example.com")),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        signup_method: SignupMethod::Email,
        email_verified: true,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

fn make_room(name: &str, owner_id: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: String::new(),
        created_by: owner_id.clone(),
        status: RoomStatus::Active,
        is_banned: false,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
        last_activity_at: now,
    }
}

fn make_playlist(room_id: &RoomId, creator_id: &UserId, name: &str, position: i32) -> Playlist {
    let now = Utc::now();
    Playlist {
        id: PlaylistId::new(),
        room_id: room_id.clone(),
        creator_id: Some(creator_id.clone()),
        name: name.to_string(),
        parent_id: None,
        position: f64::from(position),
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: now,
        updated_at: now,
        version: 0,
    }
}

fn make_media(
    room_id: &RoomId,
    playlist_id: Option<&PlaylistId>,
    creator_id: &UserId,
    name: &str,
    position: i32,
) -> Media {
    let now = Utc::now();
    Media {
        id: MediaId::new(),
        playlist_id: playlist_id.cloned(),
        room_id: room_id.clone(),
        creator_id: Some(creator_id.clone()),
        name: name.to_string(),
        position: f64::from(position),
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/video.mp4"}),
        provider_instance_name: "direct_url".to_string(),
        added_at: now,
        updated_at: now,
        version: 0,
    }
}

// ============================================================================
// Integration tests (require Docker)
// ============================================================================

async fn assert_register_duplicate_username_error(service: &UserService) {
    // Register first user
    let result = service
        .register(
            "unique_user_dup".to_string(),
            Some("dup1@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await;
    assert!(
        result.is_ok(),
        "First registration should succeed: {result:?}"
    );

    // Register with same username, different email
    let result = service
        .register(
            "unique_user_dup".to_string(),
            Some("dup2@example.com".to_string()),
            "StrongPass2".to_string(),
            None,
        )
        .await;
    assert!(result.is_err(), "Duplicate username should be rejected");
}

async fn assert_register_duplicate_email_error(service: &UserService) {
    // Register first user
    let result = service
        .register(
            "email_dup_1".to_string(),
            Some("same_email@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await;
    assert!(result.is_ok(), "First registration should succeed");

    // Register with different username, same email
    let result = service
        .register(
            "email_dup_2".to_string(),
            Some("same_email@example.com".to_string()),
            "StrongPass2".to_string(),
            None,
        )
        .await;
    assert!(result.is_err(), "Duplicate email should be rejected");
}

async fn assert_login_wrong_password(service: &UserService) {
    // Register a user
    service
        .register(
            "login_test_user".to_string(),
            Some("login@example.com".to_string()),
            "CorrectPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    // Try to login with wrong password
    let result = service
        .login(
            "login_test_user".to_string(),
            "WrongPass1".to_string(),
            None,
        )
        .await;

    assert!(result.is_err(), "Login with wrong password should fail");
}

// ============================================================================
// Validation tests (no Docker needed)
// ============================================================================

#[test]
fn test_username_validation() {
    let validator = synctv_core::validation::UsernameValidator::new();

    assert!(validator.validate("good_user").is_ok());
    assert!(validator.validate("ab").is_err()); // too short
    assert!(validator.validate("user@name").is_err()); // invalid chars
}

#[test]
fn test_password_validation() {
    let validator = synctv_core::validation::PasswordValidator::from_config(
        &PasswordComplexityConfig::default(),
    );

    assert!(validator.validate("StrongPass1").is_ok());
    assert!(validator.validate("weak").is_err());
    assert!(validator.validate("nouppercase1").is_err());
}

// ============================================================================
// Delete User Transaction Tests
// ============================================================================

async fn assert_delete_user_already_deleted_returns_error(service: &UserService) {
    // Register a user
    let (user, _, _) = service
        .register(
            "delete_test_user".to_string(),
            Some("delete@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    let user_id = user.id.clone();

    // First delete should succeed
    let result = service.delete_user(&user_id).await;
    assert!(result.is_ok(), "First delete should succeed: {result:?}");

    // Second delete should fail with "already deleted" error
    let result = service.delete_user(&user_id).await;
    assert!(result.is_err(), "Second delete should fail");
    match result {
        Err(Error::InvalidInput(msg)) => {
            assert!(
                msg.contains("already deleted"),
                "Error message should mention 'already deleted': {msg}"
            );
        }
        Err(e) => panic!("Expected InvalidInput error, got: {e:?}"),
        Ok(()) => panic!("Expected error, got Ok"),
    }
}

/// Test that concurrent `delete_user` calls maintain atomicity - only one should succeed
async fn assert_delete_user_concurrent_deletion_atomicity(pool: PgPool) {
    let service = create_user_service(pool.clone());

    // Register a user
    let (user, _, _) = service
        .register(
            "concurrent_delete_user".to_string(),
            Some("concurrent@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    let user_id = user.id.clone();

    // Use a barrier to synchronize both delete attempts
    let barrier = Arc::new(Barrier::new(2));
    let service1 = service.clone();
    let service2 = service.clone();
    let user_id1 = user_id.clone();
    let user_id2 = user_id.clone();
    let barrier1 = barrier.clone();
    let barrier2 = barrier.clone();

    let handle1 = tokio::spawn(async move {
        barrier1.wait().await;
        service1.delete_user(&user_id1).await
    });

    let handle2 = tokio::spawn(async move {
        barrier2.wait().await;
        service2.delete_user(&user_id2).await
    });

    let result1 = handle1.await.expect("Task 1 panicked");
    let result2 = handle2.await.expect("Task 2 panicked");

    // Exactly one of the two should succeed
    let success_count = [result1.is_ok(), result2.is_ok()]
        .iter()
        .filter(|&&x| x)
        .count();
    assert_eq!(
        success_count, 1,
        "Exactly one delete should succeed, but got {success_count} successes. Results: {result1:?}, {result2:?}"
    );

    // Verify user is deleted in the database
    let user_repo = UserRepository::new(pool);
    let user_after = user_repo
        .get_by_id(&user_id)
        .await
        .expect("Query should work");
    assert!(
        user_after.is_none(),
        "User should be soft-deleted (not found via get_by_id)"
    );
}

async fn assert_delete_user_removes_owned_resources_and_resets_foreign_room_playback(pool: PgPool) {
    let service = create_user_service(pool.clone());
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_member_repo = RoomMemberRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let doomed_user = user_repo
        .create(&make_user("delete_owner"))
        .await
        .expect("create doomed user");
    let foreign_owner = user_repo
        .create(&make_user("foreign_owner"))
        .await
        .expect("create foreign owner");
    let other_creator = user_repo
        .create(&make_user("other_creator"))
        .await
        .expect("create other creator");

    let owned_room = room_repo
        .create(&make_room("owned room", &doomed_user.id))
        .await
        .expect("create owned room");
    let foreign_room = room_repo
        .create(&make_room("foreign room", &foreign_owner.id))
        .await
        .expect("create foreign room");

    room_member_repo
        .add(&RoomMember {
            room_id: foreign_room.id.clone(),
            user_id: doomed_user.id.clone(),
            role: synctv_core::models::RoomRole::Member,
            status: MemberStatus::Active,
            added_permissions: 0,
            removed_permissions: 0,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            joined_at: Utc::now(),
            left_at: None,
            version: 0,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
        })
        .await
        .expect("create foreign room membership");

    let owned_playlist = playlist_repo
        .create(&make_playlist(
            &owned_room.id,
            &doomed_user.id,
            "owned playlist",
            0,
        ))
        .await
        .expect("create playlist in owned room");
    let owned_media = media_repo
        .create(&make_media(
            &owned_room.id,
            Some(&owned_playlist.id),
            &doomed_user.id,
            "owned media",
            0,
        ))
        .await
        .expect("create media in owned room");

    let foreign_playlist = playlist_repo
        .create(&make_playlist(
            &foreign_room.id,
            &doomed_user.id,
            "foreign doomed playlist",
            0,
        ))
        .await
        .expect("create playlist in foreign room");
    let foreign_media = media_repo
        .create(&make_media(
            &foreign_room.id,
            Some(&foreign_playlist.id),
            &doomed_user.id,
            "foreign doomed media",
            0,
        ))
        .await
        .expect("create media in foreign room");

    let survivor_playlist = playlist_repo
        .create(&make_playlist(
            &foreign_room.id,
            &other_creator.id,
            "foreign survivor playlist",
            1,
        ))
        .await
        .expect("create surviving playlist");
    let survivor_media = media_repo
        .create(&make_media(
            &foreign_room.id,
            Some(&survivor_playlist.id),
            &other_creator.id,
            "foreign survivor media",
            0,
        ))
        .await
        .expect("create surviving media");

    sqlx::query(
        "INSERT INTO room_playback_state
             (room_id, playing_media_id, playing_playlist_id, target, \"current_time\", speed, is_playing, updated_at, version)
         VALUES ($1, $2, NULL, ''::bytea, 12.5, 1.0, TRUE, NOW(), 0)",
    )
    .bind(foreign_room.id.as_str())
    .bind(foreign_media.id.as_str())
    .execute(&pool)
    .await
    .expect("create playback state");

    sqlx::query(
        "INSERT INTO oauth2_clients (id, provider, provider_user_id, user_id, username, email)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(synctv_common::snanoid!(12))
    .bind("github")
    .bind("delete-owner-gh")
    .bind(doomed_user.id.as_str())
    .bind("delete_owner")
    .bind("delete_owner@example.com")
    .execute(&pool)
    .await
    .expect("create oauth2 mapping");

    sqlx::query(
        "INSERT INTO notifications (user_id, title, content, type, is_read, created_at, updated_at)
         VALUES ($1, $2, $3, $4, FALSE, NOW(), NOW())",
    )
    .bind(doomed_user.id.as_str())
    .bind("title")
    .bind("body")
    .bind("system")
    .execute(&pool)
    .await
    .expect("create notification");

    sqlx::query(
        "INSERT INTO chat_messages (id, room_id, user_id, content, message_type, created_at)
         VALUES ($1, $2, $3, $4, 1, NOW())",
    )
    .bind(synctv_common::snanoid!(12))
    .bind(foreign_room.id.as_str())
    .bind(doomed_user.id.as_str())
    .bind("hello")
    .execute(&pool)
    .await
    .expect("create chat message");

    let summary = service
        .delete_user_with_summary(&doomed_user.id)
        .await
        .expect("delete_user_with_summary should succeed");

    assert_eq!(summary.user_id, doomed_user.id);
    assert_eq!(summary.username, doomed_user.username);
    assert_eq!(summary.deleted_room_ids, vec![owned_room.id.clone()]);
    assert_eq!(summary.membership_room_ids, vec![foreign_room.id.clone()]);
    assert_eq!(summary.modified_rooms.len(), 1);
    assert_eq!(summary.modified_rooms[0].room_id, foreign_room.id);
    assert_eq!(
        summary.modified_rooms[0].deleted_media_ids,
        vec![foreign_media.id.clone()]
    );
    assert!(
        summary.modified_rooms[0].playback_reset,
        "deleting the currently playing foreign media must reset playback"
    );

    assert!(
        user_repo
            .get_by_id(&doomed_user.id)
            .await
            .expect("get user")
            .is_none(),
        "deleted user must no longer be visible"
    );
    assert!(
        room_repo
            .get_by_id(&owned_room.id)
            .await
            .expect("get owned room")
            .is_none(),
        "owned room must be soft-deleted"
    );
    assert!(
        room_repo
            .get_by_id(&foreign_room.id)
            .await
            .expect("get foreign room")
            .is_some(),
        "foreign room must survive"
    );

    assert!(
        playlist_repo
            .get_by_id(&owned_playlist.id)
            .await
            .expect("get owned playlist")
            .is_none(),
        "owned room playlist should be deleted"
    );
    assert!(
        media_repo
            .get_by_id(&owned_media.id)
            .await
            .expect("get owned media")
            .is_none(),
        "owned room media should be deleted"
    );
    assert!(
        playlist_repo
            .get_by_id(&foreign_playlist.id)
            .await
            .expect("get foreign playlist")
            .is_none(),
        "user-created playlist in foreign room should be deleted"
    );
    assert!(
        media_repo
            .get_by_id(&foreign_media.id)
            .await
            .expect("get foreign media")
            .is_none(),
        "user-created media in foreign room should be deleted"
    );
    assert!(
        playlist_repo
            .get_by_id(&survivor_playlist.id)
            .await
            .expect("get survivor playlist")
            .is_some(),
        "other users' playlists must survive"
    );
    assert!(
        media_repo
            .get_by_id(&survivor_media.id)
            .await
            .expect("get survivor media")
            .is_some(),
        "other users' media must survive"
    );

    let member_after = room_member_repo
        .get(&foreign_room.id, &doomed_user.id)
        .await
        .expect("get membership");
    assert!(
        member_after.is_none(),
        "deleted user must no longer be an active member of surviving rooms"
    );

    let playback_row = sqlx::query_as::<_, (Option<String>, Option<String>, bool)>(
        "SELECT playing_media_id, playing_playlist_id, is_playing
         FROM room_playback_state
         WHERE room_id = $1",
    )
    .bind(foreign_room.id.as_str())
    .fetch_one(&pool)
    .await
    .expect("query playback");
    assert_eq!(playback_row.0, None, "playing media must be cleared");
    assert_eq!(playback_row.1, None, "playing playlist must be cleared");
    assert!(!playback_row.2, "playback must be stopped");

    let oauth2_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM oauth2_clients WHERE user_id = $1")
            .bind(doomed_user.id.as_str())
            .fetch_one(&pool)
            .await
            .expect("count oauth2 mappings");
    assert_eq!(oauth2_count, 0, "oauth2 mappings must be deleted");

    let notification_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1")
            .bind(doomed_user.id.as_str())
            .fetch_one(&pool)
            .await
            .expect("count notifications");
    assert_eq!(notification_count, 0, "notifications must be deleted");

    let chat_user_ids: Vec<Option<String>> =
        sqlx::query_scalar("SELECT user_id FROM chat_messages WHERE room_id = $1")
            .bind(foreign_room.id.as_str())
            .fetch_all(&pool)
            .await
            .expect("query chat messages");
    assert_eq!(
        chat_user_ids,
        vec![None],
        "chat author should be anonymized"
    );
}

// ============================================================================
// Registration brute-force lockout tests (Task #42)
// ============================================================================

/// Test that "username taken" errors do NOT count against IP brute-force lockout.
///
/// Scenario: User tries to register with a username that already exists.
/// This should fail with `AlreadyExists`, but should NOT lock out the IP
/// because it's not a security threat - just an unfortunate choice of username.
async fn assert_register_username_taken_no_brute_force_lockout(service: &UserService) {
    let client_ip: std::net::IpAddr = "192.168.1.100".parse().unwrap();

    // Register first user
    service
        .register(
            "existing_user_42".to_string(),
            Some("existing_42@test.com".to_string()),
            "StrongPass1".to_string(),
            Some(client_ip),
        )
        .await
        .expect("First registration should succeed");

    // Try to register with the same username multiple times (should fail with AlreadyExists)
    for _ in 0..5 {
        let result = service
            .register(
                "existing_user_42".to_string(),
                Some("different@test.com".to_string()),
                "StrongPass1".to_string(),
                Some(client_ip),
            )
            .await;

        // Should fail with AlreadyExists
        assert!(
            matches!(result, Err(Error::AlreadyExists(_))),
            "Should fail with AlreadyExists"
        );

        // IMPORTANT: Should NOT be RateLimited even after many attempts
        assert!(
            !matches!(result, Err(Error::RateLimited(_))),
            "Username taken errors should NOT trigger brute-force lockout"
        );
    }

    // Now try with a DIFFERENT username - should succeed (IP not locked)
    let result = service
        .register(
            "new_unique_user_42".to_string(),
            Some("new_42@test.com".to_string()),
            "StrongPass1".to_string(),
            Some(client_ip),
        )
        .await;

    assert!(
        result.is_ok(),
        "Should be able to register with new username - IP should NOT be locked out by 'username taken' errors: {:?}",
        result.err()
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_user_cleans_up_owned_room_memberships() {
    let (_container, pool) = create_test_pool().await;
    let user_service = create_user_service(pool.clone());
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("banned_owner")).await.unwrap();
    let member = user_repo.create(&make_user("banned_owner_member")).await.unwrap();

    let owned_room = room_repo
        .create(&make_room("owner-room", &owner.id))
        .await
        .unwrap();

    room_member_repo
        .add(&RoomMember::new(
            owned_room.id.clone(),
            owner.id.clone(),
            synctv_core::models::RoomRole::Creator,
        ))
        .await
        .unwrap();
    room_member_repo
        .add(&RoomMember::new(
            owned_room.id.clone(),
            member.id.clone(),
            synctv_core::models::RoomRole::Member,
        ))
        .await
        .unwrap();

    user_service
        .ban_user_and_cleanup_memberships(&owner.id)
        .await
        .expect("banning owner should succeed");

    let owner_membership = room_member_repo
        .get(&owned_room.id, &owner.id)
        .await
        .expect("owner membership lookup should succeed");
    assert!(
        owner_membership.is_none(),
        "banned owner must no longer be an active member of their owned room"
    );

    let member_membership = room_member_repo
        .get(&owned_room.id, &member.id)
        .await
        .expect("member membership lookup should succeed");
    assert!(
        member_membership.is_none(),
        "owned room members must be removed when the owner is banned"
    );
}

/// Test that validation errors DO count against IP brute-force lockout.
///
/// Scenario: Attacker sends malformed registration requests (validation errors).
/// These should count against the IP lockout because they indicate automated attacks.
async fn assert_register_validation_errors_trigger_brute_force_lockout(service: &UserService) {
    let client_ip: std::net::IpAddr = "192.168.1.101".parse().unwrap();

    // The brute-force lockout thresholds are:
    // - 5 failures: 1 minute lockout
    // - 10 failures: 5 minute lockout
    // - 15+ failures: 15 minute lockout
    // We need to trigger at least 5 validation errors

    // Send multiple registrations with invalid usernames (too short)
    let mut validation_error_count = 0;
    for _ in 0..25 {
        let result = service
            .register(
                "ab".to_string(), // Too short - validation error
                Some("test@example.com".to_string()),
                "StrongPass1".to_string(),
                Some(client_ip),
            )
            .await;

        match &result {
            Err(Error::InvalidInput(_)) => {
                validation_error_count += 1;
            }
            Err(Error::RateLimited(_)) => {
                // Expected - IP should be locked out after enough validation errors
                break;
            }
            _ => {}
        }
    }

    assert!(
        validation_error_count >= 5,
        "Should have had at least 5 validation errors before lockout, got {validation_error_count}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_user_service_registration_login_and_delete_flows() {
    let (_container, pool) = create_test_pool().await;

    let duplicate_username_service = create_user_service(pool.clone());
    assert_register_duplicate_username_error(&duplicate_username_service).await;

    let duplicate_email_service = create_user_service(pool.clone());
    assert_register_duplicate_email_error(&duplicate_email_service).await;

    let wrong_password_service = create_user_service(pool.clone());
    assert_login_wrong_password(&wrong_password_service).await;

    let delete_twice_service = create_user_service(pool.clone());
    assert_delete_user_already_deleted_returns_error(&delete_twice_service).await;

    assert_delete_user_removes_owned_resources_and_resets_foreign_room_playback(pool.clone()).await;

    assert_delete_user_concurrent_deletion_atomicity(pool).await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_user_service_registration_brute_force_flows() {
    let (_container, pool) = create_test_pool().await;

    let username_taken_service = create_user_service(pool.clone());
    assert_register_username_taken_no_brute_force_lockout(&username_taken_service).await;

    let validation_error_service = create_user_service(pool);
    assert_register_validation_errors_trigger_brute_force_lockout(&validation_error_service).await;
}
