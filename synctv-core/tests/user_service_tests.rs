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
    cache::{CacheDomain, KeyBuilder, LocalVersionFenceStore, UsernameCache, VersionFenceStore},
    config::PasswordComplexityConfig,
    models::{
        Media, MediaId, MemberStatus, NotificationType, Playlist, PlaylistId, Room, RoomId,
        RoomMember, RoomStatus, SignupMethod, User, UserId, UserRole, UserStatus,
    },
    repository::{
        MediaRepository, PlaylistRepository, RoomMemberRepository, RoomRepository,
        UserEmailRepository, UserRepository, WebAuthnCredentialRepository,
    },
    service::{
        auth::{BruteForceProtection, JwtService},
        local_passkey_session_store,
        permission::PermissionServiceRuntime,
        user::UserServiceRuntimeOptions,
        AuthFactorMethod, AuthenticatedLogin, InMemoryTokenBlacklistStore, PasskeyService,
        PermissionService, SecurityPipeline, SensitiveVerificationOutcome, TokenAuthContext,
        UserService,
    },
    Config, Error,
};
use synctv_core_testing::create_test_pool;
use tokio::sync::Barrier;

fn create_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-user-service-tests-long-enough-1234567890").unwrap()
}

fn create_user_service(pool: &PgPool) -> UserService {
    create_user_service_with_runtime(pool, UserServiceRuntimeOptions::default())
}

fn create_user_service_with_runtime(
    pool: &PgPool,
    runtime: UserServiceRuntimeOptions,
) -> UserService {
    let jwt = create_jwt_service();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let password_config = PasswordComplexityConfig::default();
    let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    let mut svc = UserService::new_with_brute_force_service_and_runtime(
        pool,
        synctv_core::service::user::UserServiceDependencies {
            jwt_service: jwt,
            username_cache,
            password_complexity: password_config,
            token_blacklist,
            key_builder,
            brute_force: Arc::new(brute_force),
        },
        runtime,
    );
    svc.enable_password_registration_for_tests();
    svc
}

fn create_user_service_with_security_pipeline(
    pool: &PgPool,
) -> (Arc<UserService>, JwtService, SecurityPipeline) {
    let jwt = create_jwt_service();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let password_config = PasswordComplexityConfig::default();
    let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    let mut service = UserService::new(
        pool,
        jwt.clone(),
        username_cache,
        password_config,
        Arc::clone(&token_blacklist),
        key_builder.clone(),
        brute_force,
    );
    service.enable_password_registration_for_tests();
    let service = Arc::new(service);
    let pipeline = SecurityPipeline::new(Arc::clone(&service))
        .with_token_blacklist(token_blacklist, key_builder);
    (service, jwt, pipeline)
}

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: SignupMethod::Email,
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

async fn password_verification_id(
    service: &UserService,
    user_id: &UserId,
    password: &str,
) -> String {
    let outcome = service
        .start_sensitive_operation_verification(user_id, None)
        .await
        .expect("start sensitive verification");
    let SensitiveVerificationOutcome::Pending(challenge) = outcome else {
        panic!("password verification should start with a pending challenge");
    };
    match service
        .finish_sensitive_operation_password_verification(&challenge.session_id, password, None, None)
        .await
        .expect("finish password verification")
    {
        SensitiveVerificationOutcome::Complete { verification_id } => verification_id,
        SensitiveVerificationOutcome::Pending(_) => {
            panic!("single-factor password verification should complete")
        }
    }
}

async fn insert_trusted_email_identity(pool: &PgPool, user_id: &UserId, email: &str) {
    sqlx::query(
        r"
        INSERT INTO auth_email_identities (user_id, email, created_at, updated_at)
        VALUES ($1, $2, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
        ON CONFLICT (user_id)
        DO UPDATE SET email = EXCLUDED.email, updated_at = EXCLUDED.updated_at
        ",
    )
    .bind(user_id)
    .bind(email)
    .execute(pool)
    .await
    .expect("trusted email identity should be inserted");
}

async fn insert_dummy_passkey(pool: &PgPool, user_id: &UserId, credential_id: &[u8]) {
    sqlx::query(
        r"
        INSERT INTO auth_webauthn_credentials (
            user_id, credential_id, passkey, public_key, name
        )
        VALUES ($1, $2, '{}'::jsonb, '{}'::jsonb, 'test passkey')
        ",
    )
    .bind(user_id)
    .bind(credential_id)
    .execute(pool)
    .await
    .expect("insert dummy passkey");
}

async fn insert_oauth2_identity(
    pool: &PgPool,
    user_id: &UserId,
    provider_instance_name: &str,
    provider_user_id: &str,
) {
    sqlx::query(
        "INSERT INTO auth_oauth2_identities (
             provider_type, provider_instance_name, provider_user_id, user_id, username, email
         )
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(2_i16)
    .bind(provider_instance_name)
    .bind(provider_user_id)
    .bind(user_id)
    .bind(provider_user_id)
    .bind(format!("{provider_user_id}@example.com"))
    .execute(pool)
    .await
    .expect("oauth2 identity should be inserted");
}

fn make_passkey_service(pool: PgPool, user_service: Arc<UserService>) -> PasskeyService {
    let mut config = Config::default().webauthn;
    config.enabled = true;
    config.rp_id = "localhost".to_string();
    config.rp_origin = "http://localhost".to_string();
    PasskeyService::new(
        &config,
        WebAuthnCredentialRepository::new(pool),
        user_service,
        local_passkey_session_store(),
    )
    .expect("passkey service should build")
}

fn make_room(name: &str, owner_id: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        created_by: *owner_id,
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

fn make_playlist(room_id: &RoomId, creator_id: &UserId, name: &str, position: i32) -> Playlist {
    let now = Utc::now();
    Playlist {
        id: PlaylistId::new(),
        room_id: *room_id,
        creator_id: Some(*creator_id),
        name: name.to_string(),
        description: String::new(),
        cover_file_reference_id: None,
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
        playlist_id: playlist_id.copied(),
        room_id: *room_id,
        creator_id: Some(*creator_id),
        name: name.to_string(),
        description: String::new(),
        position: f64::from(position),
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/video.mp4"}),
        provider_instance_name: None,
        cover_file_reference_id: None,
        added_at: now,
        updated_at: now,
        version: 0,
    }
}

// Integration tests (require Docker)

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

// Validation tests (no Docker needed)

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

// Delete User Transaction Tests

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

    let user_id = user.id;

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
    let service = create_user_service(&pool);

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

    let user_id = user.id;

    // Use a barrier to synchronize both delete attempts
    let barrier = Arc::new(Barrier::new(2));
    let service1 = service.clone();
    let service2 = service.clone();
    let user_id1 = user_id;
    let user_id2 = user_id;
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
    let service = create_user_service(&pool);
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
            room_id: foreign_room.id,
            user_id: doomed_user.id,
            role: synctv_core::models::RoomRole::Member,
            status: MemberStatus::Active,
            added_permissions: 0,
            removed_permissions: 0,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            joined_at: Utc::now(),
            version: 0,
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
             (room_id, playing_media_id, playing_playlist_id, target, \"position\", speed, is_playing, updated_at, version)
         VALUES ($1, $2, NULL, ''::bytea, 12.5, 1.0, TRUE, NOW(), 0)",
    )
    .bind(foreign_room.id)
    .bind(foreign_media.id)
    .execute(&pool)
    .await
    .expect("create playback state");

    sqlx::query(
        "INSERT INTO auth_oauth2_identities (
             provider_type, provider_instance_name, provider_user_id, user_id, username, email
         )
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(2_i16)
    .bind("github")
    .bind("delete-owner-gh")
    .bind(doomed_user.id)
    .bind("delete_owner")
    .bind("delete_owner@example.com")
    .execute(&pool)
    .await
    .expect("create oauth2 mapping");

    sqlx::query(
        "INSERT INTO notifications (user_id, title, content, type, is_read, created_at, updated_at)
         VALUES ($1, $2, $3, $4, FALSE, NOW(), NOW())",
    )
    .bind(doomed_user.id)
    .bind("title")
    .bind("body")
    .bind(i16::from(NotificationType::SystemAnnouncement))
    .execute(&pool)
    .await
    .expect("create notification");

    sqlx::query(
        "INSERT INTO chat_messages (room_id, user_id, content, message_type, created_at)
         VALUES ($1, $2, $3, 1, NOW())",
    )
    .bind(foreign_room.id)
    .bind(doomed_user.id)
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
    assert_eq!(summary.deleted_room_ids, vec![owned_room.id]);
    assert_eq!(summary.membership_room_ids, vec![foreign_room.id]);
    assert_eq!(summary.modified_rooms.len(), 1);
    assert_eq!(summary.modified_rooms[0].room_id, foreign_room.id);
    assert_eq!(
        summary.modified_rooms[0].deleted_media_ids,
        vec![foreign_media.id]
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
    .bind(foreign_room.id)
    .fetch_one(&pool)
    .await
    .expect("query playback");
    assert_eq!(playback_row.0, None, "playing media must be cleared");
    assert_eq!(playback_row.1, None, "playing playlist must be cleared");
    assert!(!playback_row.2, "playback must be stopped");

    let oauth2_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM auth_oauth2_identities WHERE user_id = $1")
            .bind(doomed_user.id)
            .fetch_one(&pool)
            .await
            .expect("count oauth2 mappings");
    assert_eq!(oauth2_count, 0, "oauth2 mappings must be deleted");

    let notification_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1")
            .bind(doomed_user.id)
            .fetch_one(&pool)
            .await
            .expect("count notifications");
    assert_eq!(notification_count, 0, "notifications must be deleted");

    let chat_user_ids: Vec<Option<String>> =
        sqlx::query_scalar("SELECT user_id FROM chat_messages WHERE room_id = $1")
            .bind(foreign_room.id)
            .fetch_all(&pool)
            .await
            .expect("query chat messages");
    assert_eq!(
        chat_user_ids,
        vec![None],
        "chat author should be anonymized"
    );
}

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
    let version_fence: Arc<dyn VersionFenceStore> = Arc::new(LocalVersionFenceStore::new());
    let permission_service = PermissionService::new_with_runtime(
        RoomMemberRepository::new(pool.clone()),
        RoomRepository::new(pool.clone()),
        PermissionServiceRuntime {
            version_fence: Some(version_fence.clone()),
            ..PermissionServiceRuntime::default()
        },
    );
    let user_service = create_user_service_with_runtime(
        &pool,
        UserServiceRuntimeOptions {
            permission_service: Some(permission_service),
            ..UserServiceRuntimeOptions::default()
        },
    );
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("banned_owner")).await.unwrap();
    let member = user_repo
        .create(&make_user("banned_owner_member"))
        .await
        .unwrap();

    let owned_room = room_repo
        .create(&make_room("owner-room", &owner.id))
        .await
        .unwrap();

    room_member_repo
        .add(&RoomMember::new(
            owned_room.id,
            owner.id,
            synctv_core::models::RoomRole::Creator,
        ))
        .await
        .unwrap();
    room_member_repo
        .add(&RoomMember::new(
            owned_room.id,
            member.id,
            synctv_core::models::RoomRole::Member,
        ))
        .await
        .unwrap();

    user_service
        .ban_user_and_cleanup_memberships(&owner.id, None, None)
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
        "banning a room owner must remove other memberships from the owned room"
    );

    let member_fence = version_fence
        .current_version(&CacheDomain::Permission {
            room_id: owned_room.id,
            user_id: member.id,
        })
        .await
        .expect("member permission fence should be readable");
    assert!(
        member_fence.is_some(),
        "banning a room owner must commit permission fences for removed owned-room members"
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

async fn assert_update_user_rejects_direct_email_changes(pool: PgPool) {
    let service = create_user_service(&pool);
    let user_repo = UserRepository::new(pool.clone());
    let email_repo = UserEmailRepository::new(pool.clone());

    let created = user_repo
        .create(&make_user("email_update_guard_user"))
        .await
        .expect("create email signup user");
    let original_email = "email_update_guard_user@example.com";
    email_repo
        .create_for_user_with_executor(&created, Some(original_email), &pool)
        .await
        .expect("create original email identity");

    let mut profile_update = created.clone();
    profile_update.username = "email_update_guard_renamed".to_string();
    let updated = service
        .update_user(&profile_update, created.version)
        .await
        .expect("profile update should succeed");

    assert_eq!(updated.username, "email_update_guard_renamed");
    let unchanged_email = email_repo
        .get_email(&created.id)
        .await
        .expect("fetch unchanged email identity");
    assert_eq!(unchanged_email.as_deref(), Some(original_email));
}

async fn assert_email_bind_writes_email_only_after_confirm(pool: PgPool) {
    let service = create_user_service(&pool);
    let email_repo = UserEmailRepository::new(pool.clone());

    let original_email = "email_bind_flow_user@example.com";
    let (created, _, _) = service
        .register(
            "email_bind_flow_user".to_string(),
            Some(original_email.to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("create email bind flow user");
    let new_email = "email_bind_flow_new@example.com";

    let token = service
        .start_email_bind(&created.id, new_email)
        .await
        .expect("start email bind");

    let after_start = email_repo
        .get_email(&created.id)
        .await
        .expect("fetch email after bind start");
    assert_eq!(after_start.as_deref(), Some(original_email));

    let mismatch_result = service
        .confirm_email_bind(
            &created.id,
            "email_bind_flow_other@example.com",
            &token,
            &password_verification_id(&service, &created.id, "StrongPass1").await,
        )
        .await
        .expect_err("email mismatch must reject pending bind request");
    assert!(
        matches!(mismatch_result, Error::InvalidInput(_)),
        "expected InvalidInput for email mismatch"
    );

    let after_mismatch = email_repo
        .get_email(&created.id)
        .await
        .expect("fetch email after bind mismatch");
    assert_eq!(after_mismatch.as_deref(), Some(original_email));

    let updated = service
        .confirm_email_bind(
            &created.id,
            new_email,
            &token,
            &password_verification_id(&service, &created.id, "StrongPass1").await,
        )
        .await
        .expect("confirm email bind");
    assert_eq!(updated.id, created.id);
    let updated_email = email_repo
        .get_email(&created.id)
        .await
        .expect("fetch updated email");
    assert_eq!(updated_email.as_deref(), Some(new_email));

    let consumed_result = service
        .confirm_email_bind(
            &created.id,
            new_email,
            &token,
            &password_verification_id(&service, &created.id, "StrongPass1").await,
        )
        .await
        .expect_err("consumed bind token must be rejected");
    assert!(
        matches!(consumed_result, Error::InvalidInput(_)),
        "expected InvalidInput for consumed token"
    );
}

async fn assert_email_bind_rejects_taken_email(pool: PgPool) {
    let service = create_user_service(&pool);
    let user_repo = UserRepository::new(pool.clone());
    let email_repo = UserEmailRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("email_bind_taken_owner"))
        .await
        .expect("create owner user");
    let owner_email = "email_bind_taken_owner@example.com";
    email_repo
        .create_for_user_with_executor(&owner, Some(owner_email), &pool)
        .await
        .expect("create owner email identity");
    let requester = user_repo
        .create(&make_user("email_bind_taken_requester"))
        .await
        .expect("create requester user");

    let result = service
        .start_email_bind(&requester.id, owner_email)
        .await
        .expect_err("taken email must be rejected");
    assert!(
        matches!(result, Error::AlreadyExists(_)),
        "expected AlreadyExists for taken email"
    );
}

async fn assert_two_factor_requires_two_usable_methods(pool: PgPool) {
    let service = create_user_service(&pool);

    let (password_only, _, _) = service
        .register(
            "two_factor_password_only".to_string(),
            None,
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("create password-only user");
    let result = service
        .set_two_factor_enabled(&password_only.id, true)
        .await
        .expect_err("single-method users must not enable two-factor authentication");
    assert!(
        matches!(&result, Error::InvalidInput(message) if message.contains("requires at least two")),
        "expected InvalidInput for insufficient auth factors, got {result:?}"
    );

    let (email_and_password, _, _) = service
        .register(
            "two_factor_email_password".to_string(),
            Some("two_factor_email_password@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("create email+password user");
    let (preferences, factors) = service
        .set_two_factor_enabled(&email_and_password.id, true)
        .await
        .expect("email+password user can enable two-factor authentication");
    assert!(preferences.two_factor_enabled);
    assert!(factors.password);
    assert!(factors.email);
    assert_eq!(factors.eligible_count(), 2);
}

async fn assert_sensitive_verification_is_one_time(pool: PgPool) {
    let service = create_user_service(&pool);
    let (user, _, _) = service
        .register(
            "sensitive_verification_one_time".to_string(),
            Some("sensitive_verification_one_time@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("create user with password");

    let verification_id = password_verification_id(&service, &user.id, "StrongPass1").await;
    service
        .consume_sensitive_operation_verification(&user.id, &verification_id)
        .await
        .expect("first verification consumption should succeed");
    let reused = service
        .consume_sensitive_operation_verification(&user.id, &verification_id)
        .await
        .expect_err("verification id must be single-use");
    assert!(
        matches!(reused, Error::Authentication(_)),
        "expected Authentication for reused verification id, got {reused:?}"
    );
}

async fn assert_sensitive_password_verification_is_rate_limited(pool: PgPool) {
    let service = create_user_service(&pool);
    let (user, _, _) = service
        .register(
            "sensitive_verification_rate_limit".to_string(),
            Some("sensitive_verification_rate_limit@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("create user with password");
    let outcome = service
        .start_sensitive_operation_verification(&user.id, None)
        .await
        .expect("start sensitive verification");
    let SensitiveVerificationOutcome::Pending(challenge) = outcome else {
        panic!("password-sensitive verification should start with a challenge");
    };

    for _ in 0..5 {
        let result = service
            .finish_sensitive_operation_password_verification(
                &challenge.session_id,
                "WrongPass1",
                None,
                None,
            )
            .await;
        assert!(
            matches!(result, Err(Error::Authentication(_))),
            "wrong password should fail authentication"
        );
    }

    let locked = service
        .finish_sensitive_operation_password_verification(
            &challenge.session_id,
            "StrongPass1",
            None,
            None,
        )
        .await
        .expect_err("sensitive password verification should lock out after repeated failures");
    assert!(
        matches!(locked, Error::Authentication(ref message) if message.contains("Too many failed attempts")),
        "expected sensitive verification brute-force lockout, got {locked:?}"
    );
}

async fn assert_sensitive_verification_requires_two_local_factors_when_2fa_enabled(pool: PgPool) {
    let service = create_user_service(&pool);
    let email = "sensitive_verification_2fa@example.com";
    let (user, _, _) = service
        .register(
            "sensitive_verification_2fa".to_string(),
            Some(email.to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("create user with password and email");
    insert_trusted_email_identity(&pool, &user.id, email).await;
    service
        .set_two_factor_enabled(&user.id, true)
        .await
        .expect("password+email user can enable two-factor authentication");

    let outcome = service
        .start_sensitive_operation_verification(&user.id, None)
        .await
        .expect("start sensitive verification");
    let SensitiveVerificationOutcome::Pending(challenge) = outcome else {
        panic!("2FA-enabled sensitive verification should start with a pending challenge");
    };
    assert_eq!(challenge.required_count, 2);
    assert!(challenge
        .available_methods
        .contains(&AuthFactorMethod::Password));
    assert!(challenge
        .available_methods
        .contains(&AuthFactorMethod::Email));

    let pending = service
        .finish_sensitive_operation_password_verification(
            &challenge.session_id,
            "StrongPass1",
            None,
            None,
        )
        .await
        .expect("password factor should verify");
    let SensitiveVerificationOutcome::Pending(next_challenge) = pending else {
        panic!("2FA-enabled sensitive verification should require another factor");
    };
    assert_eq!(next_challenge.required_count, 2);
    assert_eq!(
        next_challenge.completed_methods,
        vec![AuthFactorMethod::Password]
    );
    assert!(next_challenge
        .available_methods
        .contains(&AuthFactorMethod::Email));

    let complete = service
        .finish_sensitive_operation_verified_method(
            &next_challenge.session_id,
            AuthFactorMethod::Email,
        )
        .await
        .expect("email factor should complete");
    let SensitiveVerificationOutcome::Complete { verification_id } = complete else {
        panic!("second factor should complete sensitive verification");
    };
    service
        .consume_sensitive_operation_verification(&user.id, &verification_id)
        .await
        .expect("completed two-factor verification should be consumable");
}

async fn assert_oauth2_session_sensitive_verification_requires_one_local_factor(pool: PgPool) {
    let service = create_user_service(&pool);
    let email = "sensitive_verification_oauth2@example.com";
    let (user, _, _) = service
        .register(
            "sensitive_verification_oauth2".to_string(),
            Some(email.to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("create user with password and email");
    insert_trusted_email_identity(&pool, &user.id, email).await;
    service
        .set_two_factor_enabled(&user.id, true)
        .await
        .expect("password+email user can enable two-factor authentication");

    let outcome = service
        .start_sensitive_operation_verification(&user.id, Some(TokenAuthContext::OAuth2))
        .await
        .expect("start OAuth2 sensitive verification");
    let SensitiveVerificationOutcome::Pending(challenge) = outcome else {
        panic!(
            "OAuth2-session sensitive verification should start with local factors when present"
        );
    };
    assert_eq!(challenge.required_count, 1);
    assert!(challenge
        .available_methods
        .contains(&AuthFactorMethod::Password));

    let complete = service
        .finish_sensitive_operation_password_verification(
            &challenge.session_id,
            "StrongPass1",
            None,
            None,
        )
        .await
        .expect("one local factor should complete OAuth2-session sensitive verification");
    let SensitiveVerificationOutcome::Complete { verification_id } = complete else {
        panic!("OAuth2-session sensitive verification should complete after one local factor");
    };
    service
        .consume_sensitive_operation_verification(&user.id, &verification_id)
        .await
        .expect("OAuth2-session verification should be consumable");
}

async fn assert_oauth2_only_session_can_bootstrap_first_local_factor(pool: PgPool) {
    let service = create_user_service(&pool);
    let user_repo = UserRepository::new(pool.clone());
    let user = user_repo
        .create(&User::new(
            "sensitive_verification_oauth2_only".to_string(),
            SignupMethod::OAuth2,
        ))
        .await
        .expect("create OAuth2-only user");
    insert_oauth2_identity(
        &pool,
        &user.id,
        "github",
        "sensitive-verification-oauth2-only",
    )
    .await;

    let outcome = service
        .start_sensitive_operation_verification(&user.id, Some(TokenAuthContext::OAuth2))
        .await
        .expect("OAuth2-only account should receive a bootstrap verification id");
    let SensitiveVerificationOutcome::Complete { verification_id } = outcome else {
        panic!("OAuth2-only bootstrap should complete from current OAuth2 session");
    };
    service
        .consume_sensitive_operation_verification(&user.id, &verification_id)
        .await
        .expect("OAuth2-only bootstrap verification should be consumable");
}

async fn assert_two_factor_blocks_deleting_required_passkey(pool: PgPool) {
    let user_service = Arc::new(create_user_service(&pool));
    let (user, _, _) = user_service
        .register(
            "two_factor_passkey_user".to_string(),
            None,
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("create password+passkey user");
    let credential_id = b"two-factor-required-passkey";
    insert_dummy_passkey(&pool, &user.id, credential_id).await;

    user_service
        .set_two_factor_enabled(&user.id, true)
        .await
        .expect("password+passkey user can enable two-factor authentication");

    let passkey_service = make_passkey_service(pool, user_service);
    let result = passkey_service
        .delete_credential(&user.id, credential_id)
        .await
        .expect_err("deleting the passkey would leave fewer than two auth methods");
    assert!(
        matches!(&result, Error::InvalidInput(message) if message.contains("remaining verification methods are insufficient")),
        "expected InvalidInput for deleting required passkey, got {result:?}"
    );
}

async fn assert_two_factor_blocks_single_factor_token_issuance(pool: PgPool) {
    let service = create_user_service(&pool);
    let (user, _, _) = service
        .register(
            "two_factor_login_blocked".to_string(),
            Some("two_factor_login_blocked@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("create user with password");

    insert_trusted_email_identity(&pool, &user.id, "two_factor_login_blocked@example.com").await;
    let refresh_token = match service
        .login(
            "two_factor_login_blocked".to_string(),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("single-factor login should work before 2FA is enabled")
    {
        AuthenticatedLogin::Complete { refresh_token, .. } => refresh_token,
        AuthenticatedLogin::MfaRequired { .. } => {
            panic!("2FA is disabled, login should be complete")
        }
    };

    service
        .set_two_factor_enabled(&user.id, true)
        .await
        .expect("password+verified email user can enable two-factor authentication");

    let login_result = service
        .login(
            "two_factor_login_blocked".to_string(),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("first factor should return an MFA challenge after 2FA is enabled");
    let AuthenticatedLogin::MfaRequired { challenge, .. } = login_result else {
        panic!("single-factor login must not issue tokens after 2FA is enabled");
    };
    assert!(
        challenge
            .available_methods
            .contains(&AuthFactorMethod::Email),
        "password first-factor login should expose email as a remaining factor"
    );
    assert!(
        !challenge
            .available_methods
            .contains(&AuthFactorMethod::Password),
        "same password factor must not be offered twice"
    );
    let mfa_refresh_token = match service
        .complete_mfa_session_with_control(
            &challenge.session_id,
            AuthFactorMethod::Email,
            None,
            None,
        )
        .await
        .expect("verified second factor should complete MFA login")
    {
        AuthenticatedLogin::Complete { refresh_token, .. } => refresh_token,
        AuthenticatedLogin::MfaRequired { .. } => {
            panic!("completed MFA must issue tokens")
        }
    };
    let (rotated_access, rotated_refresh) = service
        .refresh_token(mfa_refresh_token)
        .await
        .expect("refresh token issued after MFA should rotate successfully");
    assert!(!rotated_access.is_empty());
    assert!(!rotated_refresh.is_empty());

    let refresh_result = service
        .refresh_token(refresh_token)
        .await
        .expect_err("refresh token rotation must not issue tokens after 2FA is enabled");
    assert!(
        matches!(&refresh_result, Error::Authentication(message) if message.contains("Two-factor authentication is required")),
        "expected Authentication error requiring 2FA during refresh, got {refresh_result:?}"
    );
}

async fn assert_two_factor_access_token_context_is_enforced(pool: PgPool) {
    let (service, jwt, pipeline) = create_user_service_with_security_pipeline(&pool);
    let (user, old_access_token, old_refresh_token) = service
        .register(
            "two_factor_access_context".to_string(),
            Some("two_factor_access_context@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("create user with password");
    let old_access_token = old_access_token.expect("2FA-disabled registration issues access token");
    let old_refresh_token =
        old_refresh_token.expect("2FA-disabled registration issues refresh token");
    let old_access_claims = jwt
        .verify_access_token(&old_access_token)
        .expect("old access token should be syntactically valid");
    pipeline
        .check(&old_access_claims)
        .await
        .expect("single-factor access token should work before 2FA is enabled");

    insert_trusted_email_identity(&pool, &user.id, "two_factor_access_context@example.com").await;
    service
        .set_two_factor_enabled(&user.id, true)
        .await
        .expect("password+verified email user can enable two-factor authentication");

    let result = pipeline
        .check(&old_access_claims)
        .await
        .expect_err("old single-factor access token must be rejected while 2FA is enabled");
    assert!(
        matches!(&result, Error::Authentication(message) if message.contains("Two-factor authentication is required")),
        "expected old access token to require 2FA context, got {result:?}"
    );
    let refresh_result = service
        .refresh_token(old_refresh_token)
        .await
        .expect_err("old single-factor refresh token must also be rejected while 2FA is enabled");
    assert!(
        matches!(&refresh_result, Error::Authentication(message) if message.contains("Two-factor authentication is required")),
        "expected old refresh token to require 2FA context, got {refresh_result:?}"
    );

    let login_result = service
        .login(
            "two_factor_access_context".to_string(),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("password first factor should start MFA challenge");
    let AuthenticatedLogin::MfaRequired { challenge, .. } = login_result else {
        panic!("2FA-enabled password login should require email second factor");
    };
    let mfa_access_token = match service
        .complete_mfa_session_with_control(
            &challenge.session_id,
            AuthFactorMethod::Email,
            None,
            None,
        )
        .await
        .expect("verified email second factor should complete MFA login")
    {
        AuthenticatedLogin::Complete { access_token, .. } => access_token,
        AuthenticatedLogin::MfaRequired { .. } => {
            panic!("completed MFA must issue tokens")
        }
    };
    let mfa_access_claims = jwt
        .verify_access_token(&mfa_access_token)
        .expect("MFA access token should be syntactically valid");
    assert!(
        mfa_access_claims.satisfies_two_factor_requirement(),
        "MFA-completed token must carry a 2FA auth context"
    );
    pipeline
        .check(&mfa_access_claims)
        .await
        .expect("MFA access token should work while 2FA is enabled");

    insert_oauth2_identity(&pool, &user.id, "github", "oauth2-provider-user-id").await;
    let oauth_access_token = match service
        .login_oauth2(&user.id, "github", "oauth2-provider-user-id", None)
        .await
        .expect("OAuth2 login should stay independent from local 2FA")
    {
        AuthenticatedLogin::Complete { access_token, .. } => access_token,
        AuthenticatedLogin::MfaRequired { .. } => {
            panic!("OAuth2 login must not start a local MFA challenge")
        }
    };
    let oauth_access_claims = jwt
        .verify_access_token(&oauth_access_token)
        .expect("OAuth2 access token should be syntactically valid");
    assert!(
        oauth_access_claims.satisfies_two_factor_requirement(),
        "OAuth2 token must carry its independent auth context"
    );
    pipeline
        .check(&oauth_access_claims)
        .await
        .expect("OAuth2 access token should work while 2FA is enabled");

    service
        .set_two_factor_enabled(&user.id, false)
        .await
        .expect("2FA can be disabled once the caller has a valid strong context");
    pipeline
        .check(&old_access_claims)
        .await
        .expect("single-factor access token should work again after 2FA is disabled");
    pipeline
        .check(&mfa_access_claims)
        .await
        .expect("MFA access token should remain valid after 2FA is disabled");
}

async fn assert_two_factor_allows_oauth2_without_local_mfa(pool: PgPool) {
    let service = create_user_service(&pool);
    let (user, _, _) = service
        .register(
            "two_factor_oauth2_allowed".to_string(),
            Some("two_factor_oauth2_allowed@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("create user with password");

    insert_trusted_email_identity(&pool, &user.id, "two_factor_oauth2_allowed@example.com").await;
    service
        .set_two_factor_enabled(&user.id, true)
        .await
        .expect("password+verified email user can enable two-factor authentication");

    insert_oauth2_identity(&pool, &user.id, "github", "oauth2-provider-user-id-mfa").await;
    let (access_token, refresh_token) = match service
        .login_oauth2(&user.id, "github", "oauth2-provider-user-id-mfa", None)
        .await
        .expect("OAuth2 login should stay independent from local 2FA")
    {
        AuthenticatedLogin::Complete {
            access_token,
            refresh_token,
            ..
        } => (access_token, refresh_token),
        AuthenticatedLogin::MfaRequired { .. } => {
            panic!("OAuth2 login must not start a local MFA challenge")
        }
    };
    assert!(!access_token.is_empty());
    let (rotated_access, rotated_refresh) = service
        .refresh_token(refresh_token)
        .await
        .expect("OAuth2 refresh token should rotate for 2FA-enabled users");
    assert!(!rotated_access.is_empty());
    assert!(!rotated_refresh.is_empty());
}

async fn assert_refresh_token_rejects_unbound_oauth2_identity(pool: PgPool) {
    let service = create_user_service(&pool);
    let (user, _, _) = service
        .register(
            "oauth_refresh_binding".to_string(),
            Some("oauth_refresh_binding@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("create user with password");

    insert_oauth2_identity(&pool, &user.id, "github", "oauth-refresh-provider-user").await;
    let refresh_token = match service
        .login_oauth2(&user.id, "github", "oauth-refresh-provider-user", None)
        .await
        .expect("OAuth2 login should issue tokens")
    {
        AuthenticatedLogin::Complete { refresh_token, .. } => refresh_token,
        AuthenticatedLogin::MfaRequired { .. } => panic!("OAuth2 login should complete"),
    };

    sqlx::query(
        "DELETE FROM auth_oauth2_identities
         WHERE user_id = $1 AND provider_instance_name = $2 AND provider_user_id = $3",
    )
    .bind(user.id)
    .bind("github")
    .bind("oauth-refresh-provider-user")
    .execute(&pool)
    .await
    .expect("delete oauth2 identity");

    let result = service.refresh_token(refresh_token).await;
    assert!(
        matches!(result, Err(Error::Authentication(_))),
        "OAuth2-bound refresh token should be rejected after unlink"
    );
}

async fn assert_refresh_token_rejects_unbound_email_identity(pool: PgPool) {
    let service = create_user_service(&pool);
    let (user, _, _) = service
        .register(
            "email_refresh_binding".to_string(),
            Some("email_refresh_binding@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("create user with password");
    let refresh_token = match service
        .login_with_verified_email(&user.id, "email-refresh-binding", None)
        .await
        .expect("verified email login should issue tokens")
    {
        AuthenticatedLogin::Complete { refresh_token, .. } => refresh_token,
        AuthenticatedLogin::MfaRequired { .. } => panic!("email login should complete"),
    };

    sqlx::query("DELETE FROM auth_email_identities WHERE user_id = $1")
        .bind(user.id)
        .execute(&pool)
        .await
        .expect("delete email identity");

    let result = service.refresh_token(refresh_token).await;
    assert!(
        matches!(result, Err(Error::Authentication(_))),
        "Email-bound refresh token should be rejected after unlink"
    );
}

async fn assert_refresh_token_rejects_deleted_passkey_binding(pool: PgPool) {
    let service = create_user_service(&pool);
    let (user, _, _) = service
        .register(
            "passkey_refresh_binding".to_string(),
            Some("passkey_refresh_binding@example.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("create user with password");
    let credential_id = b"passkey-refresh-binding";
    insert_dummy_passkey(&pool, &user.id, credential_id).await;

    let refresh_token = match service
        .login_with_verified_external_credential_with_control(
            &user.id,
            credential_id,
            "passkey-refresh-binding",
            None,
            None,
        )
        .await
        .expect("passkey login should issue tokens")
    {
        AuthenticatedLogin::Complete { refresh_token, .. } => refresh_token,
        AuthenticatedLogin::MfaRequired { .. } => panic!("passkey login should complete"),
    };

    sqlx::query("DELETE FROM auth_webauthn_credentials WHERE user_id = $1 AND credential_id = $2")
        .bind(user.id)
        .bind(credential_id.as_slice())
        .execute(&pool)
        .await
        .expect("delete passkey credential");

    let result = service.refresh_token(refresh_token).await;
    assert!(
        matches!(result, Err(Error::Authentication(_))),
        "Passkey-bound refresh token should be rejected after credential deletion"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_user_service_registration_login_and_delete_flows() {
    let (_container, pool) = create_test_pool().await;

    let duplicate_username_service = create_user_service(&pool);
    assert_register_duplicate_username_error(&duplicate_username_service).await;

    let duplicate_email_service = create_user_service(&pool);
    assert_register_duplicate_email_error(&duplicate_email_service).await;

    let wrong_password_service = create_user_service(&pool);
    assert_login_wrong_password(&wrong_password_service).await;

    let delete_twice_service = create_user_service(&pool);
    assert_delete_user_already_deleted_returns_error(&delete_twice_service).await;

    assert_delete_user_removes_owned_resources_and_resets_foreign_room_playback(pool.clone()).await;

    assert_update_user_rejects_direct_email_changes(pool.clone()).await;
    assert_email_bind_writes_email_only_after_confirm(pool.clone()).await;
    assert_email_bind_rejects_taken_email(pool.clone()).await;

    assert_two_factor_requires_two_usable_methods(pool.clone()).await;
    assert_sensitive_verification_is_one_time(pool.clone()).await;
    assert_sensitive_password_verification_is_rate_limited(pool.clone()).await;
    assert_sensitive_verification_requires_two_local_factors_when_2fa_enabled(pool.clone()).await;
    assert_oauth2_session_sensitive_verification_requires_one_local_factor(pool.clone()).await;
    assert_oauth2_only_session_can_bootstrap_first_local_factor(pool.clone()).await;
    assert_two_factor_blocks_deleting_required_passkey(pool.clone()).await;
    assert_two_factor_blocks_single_factor_token_issuance(pool.clone()).await;
    assert_two_factor_access_token_context_is_enforced(pool.clone()).await;
    assert_two_factor_allows_oauth2_without_local_mfa(pool.clone()).await;
    assert_refresh_token_rejects_unbound_oauth2_identity(pool.clone()).await;
    assert_refresh_token_rejects_unbound_email_identity(pool.clone()).await;
    assert_refresh_token_rejects_deleted_passkey_binding(pool.clone()).await;

    assert_delete_user_concurrent_deletion_atomicity(pool).await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_user_service_registration_brute_force_flows() {
    let (_container, pool) = create_test_pool().await;

    let username_taken_service = create_user_service(&pool);
    assert_register_username_taken_no_brute_force_lockout(&username_taken_service).await;

    let validation_error_service = create_user_service(&pool);
    assert_register_validation_errors_trigger_brute_force_lockout(&validation_error_service).await;
}
