//! User service tests
//!
//! Tests user registration and login validation using testcontainers.
//!
//! Run Docker tests: cargo test --test `user_service_tests` -- --ignored
use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{CacheDomain, KeyBuilder, LocalVersionFenceStore, UsernameCache, VersionFenceStore},
    config::PasswordComplexityConfig,
    models::{
        Media, MediaId, MemberStatus, NotificationType, Playlist, PlaylistId, Room, RoomId,
        RoomMember, RoomStatus, SignupMethod, User, UserId, UserRole, UserStatus,
        OPAQUE_CIPHERSUITE_RISTRETTO255_SHA512_ARGON2ID, OPAQUE_SERVER_SETUP_VERSION,
    },
    repository::{
        MediaRepository, PlaylistRepository, RoomMemberRepository, RoomRepository,
        SettingsRepository, UserEmailRepository, UserPasswordRepository, UserRepository,
        WebAuthnCredentialRepository,
    },
    service::{
        auth::{BruteForceProtection, JwtService, OpaquePasswordService},
        local_passkey_session_store,
        permission::PermissionServiceRuntime,
        user::UserServiceRuntimeOptions,
        AccountRegistrationOutcome, AuthFactorMethod, AuthenticatedLogin,
        InMemoryTokenBlacklistStore, PasskeyService, PermissionService, SecurityPipeline,
        SecurityPipelineRuntime, SensitiveVerificationOutcome, SettingsRegistry, SettingsService,
        TokenAuthContext, UserService,
    },
    Config, Error,
};
use synctv_core_testing::{
    create_test_pool, opaque_login_user, opaque_login_user_with_challenge, opaque_register_user,
    opaque_register_user_with_client_ip, TestOptionExt, TestResultExt,
};
use tokio::sync::Barrier;

fn create_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-user-service-tests-long-enough-1234567890")
        .checked("test operation should succeed")
}

fn create_user_service(pool: &PgPool) -> UserService {
    create_user_service_with_runtime(pool, UserServiceRuntimeOptions::test_defaults())
}

fn create_user_service_with_runtime(
    pool: &PgPool,
    mut runtime: UserServiceRuntimeOptions,
) -> UserService {
    let jwt = create_jwt_service();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let password_config = PasswordComplexityConfig::default();
    let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    runtime.password_registration_policy_override =
        Some(synctv_core::service::RegistrationPolicy {
            enabled: true,
            need_review: false,
        });

    UserService::new_with_brute_force_service_and_runtime(
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
    )
}

async fn email_signup_registry(pool: &PgPool) -> Arc<SettingsRegistry> {
    let settings_service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool.clone(),
    ));
    let registry = Arc::new(SettingsRegistry::new(settings_service));
    registry
        .enable_email_signup
        .set(true)
        .await
        .checked("email signup setting should persist");
    registry
        .email_signup_need_review
        .set(false)
        .await
        .checked("email signup review setting should persist");
    registry
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

    let service = UserService::new_with_brute_force_service_and_runtime(
        pool,
        synctv_core::service::user::UserServiceDependencies {
            jwt_service: jwt.clone(),
            username_cache,
            password_complexity: password_config,
            token_blacklist: Arc::clone(&token_blacklist),
            key_builder: key_builder.clone(),
            brute_force: Arc::new(brute_force),
        },
        UserServiceRuntimeOptions {
            password_registration_policy_override: Some(synctv_core::service::RegistrationPolicy {
                enabled: true,
                need_review: false,
            }),
            ..UserServiceRuntimeOptions::test_defaults()
        },
    );
    let service = Arc::new(service);
    let pipeline = SecurityPipeline::new_with_runtime(
        Arc::clone(&service),
        SecurityPipelineRuntime {
            user_cache: None,
            token_blacklist,
            key_builder,
        },
    );
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_direct_password_registration_stores_opaque_credential() {
    let (_container, pool) = create_test_pool().await;
    let opaque_password_service = Arc::new(OpaquePasswordService::derive_from_secret(
        b"direct-password-registration-test",
    ));
    let service = create_user_service_with_runtime(
        &pool,
        UserServiceRuntimeOptions {
            opaque_password_service: Arc::clone(&opaque_password_service),
            ..UserServiceRuntimeOptions::test_defaults()
        },
    );

    let username = format!("direct_password_{}", synctv_common::snanoid!(8));
    let password = "StrongPass1";
    let email = format!("{username}@example.com");
    let outcome = service
        .register_with_direct_password_transport_with_control(
            username.clone(),
            Some(email),
            password.to_string(),
            None,
            None,
        )
        .await
        .checked("direct password registration should succeed");
    let AccountRegistrationOutcome::Registered {
        user,
        access_token,
        refresh_token,
        ..
    } = outcome
    else {
        std::panic::panic_any("direct password registration should complete without review");
    };

    assert_eq!(user.username, username.to_ascii_lowercase());
    assert!(!access_token.is_empty());
    assert!(!refresh_token.is_empty());

    let stored_credential = UserPasswordRepository::new(pool.clone())
        .get_opaque_credential(&user.id)
        .await
        .checked("password credential lookup should succeed")
        .checked("password credential should be stored");

    assert_eq!(
        stored_credential.record.ciphersuite,
        OPAQUE_CIPHERSUITE_RISTRETTO255_SHA512_ARGON2ID
    );
    assert_eq!(
        stored_credential.record.server_setup_version,
        OPAQUE_SERVER_SETUP_VERSION
    );
    assert!(opaque_password_service
        .verify_password(&stored_credential.record, password,)
        .checked("stored OPAQUE credential should verify"));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_direct_password_allows_opaque_login() {
    let (_container, pool) = create_test_pool().await;
    let opaque_password_service = Arc::new(OpaquePasswordService::derive_from_secret(
        b"set-direct-password-opaque-login-test",
    ));
    let service = create_user_service_with_runtime(
        &pool,
        UserServiceRuntimeOptions {
            opaque_password_service,
            ..UserServiceRuntimeOptions::test_defaults()
        },
    );

    let username = format!("reset_password_{}", synctv_common::snanoid!(8));
    let old_password = "StrongPass1";
    let new_password = "StrongerPass2";
    let (user, _, _) = opaque_register_user(&service, username.clone(), None, old_password)
        .await
        .checked("initial OPAQUE registration should succeed");
    let target_user_id = user.id;

    service
        .set_direct_password(&user.id, new_password)
        .await
        .checked("direct password reset should succeed");

    let login = opaque_login_user(&service, username, new_password)
        .await
        .checked("OPAQUE login should accept reset password");
    match login {
        AuthenticatedLogin::Complete {
            user,
            access_token,
            refresh_token,
            ..
        } => {
            assert_eq!(user.id, target_user_id);
            assert!(!access_token.is_empty());
            assert!(!refresh_token.is_empty());
        }
        AuthenticatedLogin::MfaRequired { .. } => {
            std::panic::panic_any("fresh reset-password user should not require MFA")
        }
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_email_registration_confirmation_stores_opaque_credential() {
    let (_container, pool) = create_test_pool().await;
    let opaque_password_service = Arc::new(OpaquePasswordService::derive_from_secret(
        b"email-registration-confirmation-test",
    ));
    let service = create_user_service_with_runtime(
        &pool,
        UserServiceRuntimeOptions {
            opaque_password_service: Arc::clone(&opaque_password_service),
            settings_registry: Some(email_signup_registry(&pool).await),
            ..UserServiceRuntimeOptions::test_defaults()
        },
    );

    let username = format!("email_registration_{}", synctv_common::snanoid!(8));
    let email = format!("{username}@example.com");
    let password = "StrongPass1";

    let token = service
        .create_email_registration_token_with_control(username.clone(), email.clone(), None, None)
        .await
        .checked("email registration token should be created");
    let outcome = service
        .complete_email_registration_with_direct_password_transport_with_control(
            &token,
            password.to_string(),
            None,
            None,
        )
        .await
        .checked("email registration confirmation should succeed");
    let AccountRegistrationOutcome::Registered {
        user,
        access_token,
        refresh_token,
        ..
    } = outcome
    else {
        std::panic::panic_any("email registration confirmation should complete without review");
    };

    assert_eq!(user.username, username.to_ascii_lowercase());
    assert!(!access_token.is_empty());
    assert!(!refresh_token.is_empty());

    let stored_credential = UserPasswordRepository::new(pool.clone())
        .get_opaque_credential(&user.id)
        .await
        .checked("password credential lookup should succeed")
        .checked("password credential should be stored");
    assert!(opaque_password_service
        .verify_password(&stored_credential.record, password,)
        .checked("stored OPAQUE credential should verify"));

    let reuse = service
        .complete_email_registration_with_direct_password_transport_with_control(
            &token,
            password.to_string(),
            None,
            None,
        )
        .await;
    assert!(matches!(reuse, Err(Error::InvalidInput(_))));
}

async fn password_verification_id(
    service: &UserService,
    user_id: &UserId,
    password: &str,
) -> String {
    let outcome = service
        .start_sensitive_operation_verification(user_id, None)
        .await
        .checked("start sensitive verification");
    let SensitiveVerificationOutcome::Pending(challenge) = outcome else {
        std::panic::panic_any("password verification should start with a pending challenge");
    };
    match service
        .finish_sensitive_operation_password_verification(
            &challenge.session_id,
            password,
            None,
            None,
        )
        .await
        .checked("finish password verification")
    {
        SensitiveVerificationOutcome::Complete { verification_id } => verification_id,
        SensitiveVerificationOutcome::Pending(_) => {
            std::panic::panic_any("single-factor password verification should complete")
        }
    }
}

async fn insert_trusted_email_identity(pool: &PgPool, user_id: &UserId, email: &str) {
    sqlx::query!(
        r"
        INSERT INTO auth_email_identities (user_id, email, created_at, updated_at)
        VALUES ($1, $2, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
        ON CONFLICT (user_id)
        DO UPDATE SET email = EXCLUDED.email, updated_at = EXCLUDED.updated_at
        ",
        user_id.as_i64(),
        email
    )
    .execute(pool)
    .await
    .checked("trusted email identity should be inserted");
}

async fn insert_test_passkey(pool: &PgPool, user_id: &UserId, credential_id: &[u8]) {
    sqlx::query!(
        r"
        INSERT INTO auth_webauthn_credentials (
            user_id, credential_id, passkey, public_key, name
        )
        VALUES ($1, $2, '{}'::jsonb, '{}'::jsonb, 'test passkey')
        ",
        user_id.as_i64(),
        credential_id
    )
    .execute(pool)
    .await
    .checked("insert test passkey");
}

async fn insert_oauth2_identity(
    pool: &PgPool,
    user_id: &UserId,
    provider_instance_name: &str,
    provider_user_id: &str,
) {
    sqlx::query!(
        "INSERT INTO auth_oauth2_identities (
             provider_type, provider_instance_name, provider_user_id, user_id, username, email
         )
         VALUES ($1, $2, $3, $4, $5, $6)",
        2_i16,
        provider_instance_name,
        provider_user_id,
        user_id.as_i64(),
        provider_user_id,
        &format!("{provider_user_id}@example.com")
    )
    .execute(pool)
    .await
    .checked("oauth2 identity should be inserted");
}

fn make_passkey_service(pool: PgPool, user_service: Arc<UserService>) -> PasskeyService {
    let mut config = Config::default().webauthn;
    config.enabled = true;
    config.rp_id = "localhost".to_string();
    config.rp_origin = "http://localhost".to_string();
    match PasskeyService::new(
        &config,
        WebAuthnCredentialRepository::new(pool),
        user_service,
        local_passkey_session_store(),
    ) {
        Ok(service) => service,
        Err(error) => std::panic::panic_any(format!("passkey service should build: {error:?}")),
    }
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
    let result = opaque_register_user(
        service,
        "unique_user_dup",
        Some("dup1@example.com".to_string()),
        "StrongPass1",
    )
    .await;
    assert!(
        result.is_ok(),
        "First registration should succeed: {result:?}"
    );

    // Register with same username, different email
    let result = opaque_register_user(
        service,
        "unique_user_dup",
        Some("dup2@example.com".to_string()),
        "StrongPass2",
    )
    .await;
    assert!(result.is_err(), "Duplicate username should be rejected");
}

async fn assert_register_duplicate_email_error(service: &UserService) {
    // Register first user
    let result = opaque_register_user(
        service,
        "email_dup_1",
        Some("same_email@example.com".to_string()),
        "StrongPass1",
    )
    .await;
    assert!(result.is_ok(), "First registration should succeed");

    // Register with different username, same email
    let result = opaque_register_user(
        service,
        "email_dup_2",
        Some("same_email@example.com".to_string()),
        "StrongPass2",
    )
    .await;
    assert!(result.is_err(), "Duplicate email should be rejected");
}

async fn assert_login_wrong_password(service: &UserService) {
    // Register a user
    opaque_register_user(
        service,
        "login_test_user",
        Some("login@example.com".to_string()),
        "CorrectPass1",
    )
    .await
    .checked("Registration should succeed");

    // Try to login with wrong password
    let result = opaque_login_user(service, "login_test_user", "WrongPass1").await;

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
    let (user, _, _) = opaque_register_user(
        service,
        "delete_test_user",
        Some("delete@example.com".to_string()),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed");

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
        Err(e) => std::panic::panic_any(format!("expected InvalidInput error, got: {e:?}")),
        Ok(()) => std::panic::panic_any("expected error, got Ok"),
    }
}

/// Test that concurrent `delete_user` calls maintain atomicity - only one should succeed
async fn assert_delete_user_concurrent_deletion_atomicity(pool: PgPool) {
    let service = create_user_service(&pool);

    // Register a user
    let (user, _, _) = opaque_register_user(
        &service,
        "concurrent_delete_user",
        Some("concurrent@example.com".to_string()),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed");

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

    let result1 = handle1.await.checked("Task 1 panicked");
    let result2 = handle2.await.checked("Task 2 panicked");

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
        .checked("Query should work");
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
        .checked("create doomed user");
    let foreign_owner = user_repo
        .create(&make_user("foreign_owner"))
        .await
        .checked("create foreign owner");
    let other_creator = user_repo
        .create(&make_user("other_creator"))
        .await
        .checked("create other creator");

    let owned_room = room_repo
        .create(&make_room("owned room", &doomed_user.id))
        .await
        .checked("create owned room");
    let foreign_room = room_repo
        .create(&make_room("foreign room", &foreign_owner.id))
        .await
        .checked("create foreign room");

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
        .checked("create foreign room membership");

    let owned_playlist = playlist_repo
        .create(&make_playlist(
            &owned_room.id,
            &doomed_user.id,
            "owned playlist",
            0,
        ))
        .await
        .checked("create playlist in owned room");
    let owned_media = media_repo
        .create(&make_media(
            &owned_room.id,
            Some(&owned_playlist.id),
            &doomed_user.id,
            "owned media",
            0,
        ))
        .await
        .checked("create media in owned room");

    let foreign_playlist = playlist_repo
        .create(&make_playlist(
            &foreign_room.id,
            &doomed_user.id,
            "foreign doomed playlist",
            0,
        ))
        .await
        .checked("create playlist in foreign room");
    let foreign_media = media_repo
        .create(&make_media(
            &foreign_room.id,
            Some(&foreign_playlist.id),
            &doomed_user.id,
            "foreign doomed media",
            0,
        ))
        .await
        .checked("create media in foreign room");

    let survivor_playlist = playlist_repo
        .create(&make_playlist(
            &foreign_room.id,
            &other_creator.id,
            "foreign survivor playlist",
            1,
        ))
        .await
        .checked("create surviving playlist");
    let survivor_media = media_repo
        .create(&make_media(
            &foreign_room.id,
            Some(&survivor_playlist.id),
            &other_creator.id,
            "foreign survivor media",
            0,
        ))
        .await
        .checked("create surviving media");

    let foreign_progress_id: i64 = sqlx::query_scalar!(
        r#"INSERT INTO room_playback_progress
             (room_id, media_id, playlist_id, target, target_hash, "position", version)
         VALUES ($1, $2, NULL, ''::bytea, $3, 12.5, 0)
         RETURNING id AS "id!""#,
        foreign_room.id.as_i64(),
        foreign_media.id.as_i64(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    )
    .fetch_one(&pool)
    .await
    .checked("create playback progress");

    sqlx::query!(
        "INSERT INTO room_playback_state
             (room_id, playing_media_id, playing_playlist_id, target, current_progress_id, speed, is_playing, updated_at, version)
         VALUES ($1, $2, NULL, ''::bytea, $3, 1.0, TRUE, NOW(), 0)",
        foreign_room.id.as_i64(),
        foreign_media.id.as_i64(),
        foreign_progress_id
    )
    .execute(&pool)
    .await
    .checked("create playback state");

    sqlx::query!(
        "INSERT INTO auth_oauth2_identities (
             provider_type, provider_instance_name, provider_user_id, user_id, username, email
         )
         VALUES ($1, $2, $3, $4, $5, $6)",
        2_i16,
        "github",
        "delete-owner-gh",
        doomed_user.id.as_i64(),
        "delete_owner",
        "delete_owner@example.com"
    )
    .execute(&pool)
    .await
    .checked("create oauth2 mapping");

    sqlx::query!(
        "INSERT INTO notifications (user_id, title, content, type, is_read, created_at, updated_at)
         VALUES ($1, $2, $3, $4, FALSE, NOW(), NOW())",
        doomed_user.id.as_i64(),
        "title",
        "body",
        i16::from(NotificationType::SystemAnnouncement)
    )
    .execute(&pool)
    .await
    .checked("create notification");

    sqlx::query!(
        "INSERT INTO chat_messages (room_id, user_id, content, message_type, created_at)
         VALUES ($1, $2, $3, 1, NOW())",
        foreign_room.id.as_i64(),
        doomed_user.id.as_i64(),
        "hello"
    )
    .execute(&pool)
    .await
    .checked("create chat message");

    let summary = service
        .delete_user_with_summary(&doomed_user.id)
        .await
        .checked("delete_user_with_summary should succeed");

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
            .checked("get user")
            .is_none(),
        "deleted user must no longer be visible"
    );
    assert!(
        room_repo
            .get_by_id(&owned_room.id)
            .await
            .checked("get owned room")
            .is_none(),
        "owned room must be soft-deleted"
    );
    assert!(
        room_repo
            .get_by_id(&foreign_room.id)
            .await
            .checked("get foreign room")
            .is_some(),
        "foreign room must survive"
    );

    assert!(
        playlist_repo
            .get_by_id(&owned_playlist.id)
            .await
            .checked("get owned playlist")
            .is_none(),
        "owned room playlist should be deleted"
    );
    assert!(
        media_repo
            .get_by_id(&owned_media.id)
            .await
            .checked("get owned media")
            .is_none(),
        "owned room media should be deleted"
    );
    assert!(
        playlist_repo
            .get_by_id(&foreign_playlist.id)
            .await
            .checked("get foreign playlist")
            .is_none(),
        "user-created playlist in foreign room should be deleted"
    );
    assert!(
        media_repo
            .get_by_id(&foreign_media.id)
            .await
            .checked("get foreign media")
            .is_none(),
        "user-created media in foreign room should be deleted"
    );
    assert!(
        playlist_repo
            .get_by_id(&survivor_playlist.id)
            .await
            .checked("get survivor playlist")
            .is_some(),
        "other users' playlists must survive"
    );
    assert!(
        media_repo
            .get_by_id(&survivor_media.id)
            .await
            .checked("get survivor media")
            .is_some(),
        "other users' media must survive"
    );

    let member_after = room_member_repo
        .get(&foreign_room.id, &doomed_user.id)
        .await
        .checked("get membership");
    assert!(
        member_after.is_none(),
        "deleted user must no longer be an active member of surviving rooms"
    );

    let playback_row = sqlx::query!(
        r#"SELECT playing_media_id AS "playing_media_id?: MediaId",
                  playing_playlist_id AS "playing_playlist_id?: PlaylistId",
                  is_playing AS "is_playing!"
         FROM room_playback_state
         WHERE room_id = $1"#,
        foreign_room.id.as_i64()
    )
    .fetch_one(&pool)
    .await
    .checked("query playback");
    assert_eq!(
        playback_row.playing_media_id, None,
        "playing media must be cleared"
    );
    assert_eq!(
        playback_row.playing_playlist_id, None,
        "playing playlist must be cleared"
    );
    assert!(!playback_row.is_playing, "playback must be stopped");

    let oauth2_count: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM auth_oauth2_identities WHERE user_id = $1"#,
        doomed_user.id.as_i64()
    )
    .fetch_one(&pool)
    .await
    .checked("count oauth2 mappings");
    assert_eq!(oauth2_count, 0, "oauth2 mappings must be deleted");

    let notification_count: i64 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM notifications WHERE user_id = $1"#,
        doomed_user.id.as_i64()
    )
    .fetch_one(&pool)
    .await
    .checked("count notifications");
    assert_eq!(notification_count, 0, "notifications must be deleted");

    let chat_user_ids: Vec<Option<UserId>> = sqlx::query_scalar!(
        r#"SELECT user_id AS "user_id?: UserId" FROM chat_messages WHERE room_id = $1"#,
        foreign_room.id.as_i64()
    )
    .fetch_all(&pool)
    .await
    .checked("query chat messages");
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
    let client_ip: std::net::IpAddr = "192.168.1.100"
        .parse()
        .checked("test operation should succeed");

    // Register first user
    opaque_register_user_with_client_ip(
        service,
        "existing_user_42",
        Some("existing_42@test.com".to_string()),
        "StrongPass1",
        Some(client_ip),
    )
    .await
    .checked("First registration should succeed");

    // Try to register with the same username multiple times (should fail with AlreadyExists)
    for _ in 0..5 {
        let result = opaque_register_user_with_client_ip(
            service,
            "existing_user_42",
            Some("different@test.com".to_string()),
            "StrongPass1",
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
    let result = opaque_register_user_with_client_ip(
        service,
        "new_unique_user_42",
        Some("new_42@test.com".to_string()),
        "StrongPass1",
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
            version_fence: version_fence.clone(),
            ..PermissionServiceRuntime::local_only()
        },
    )
    .checked("permission service should build");
    let user_service = create_user_service_with_runtime(
        &pool,
        UserServiceRuntimeOptions {
            permission_service: Some(permission_service),
            ..UserServiceRuntimeOptions::test_defaults()
        },
    );
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let room_member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("banned_owner"))
        .await
        .checked("test operation should succeed");
    let member = user_repo
        .create(&make_user("banned_owner_member"))
        .await
        .checked("test operation should succeed");

    let owned_room = room_repo
        .create(&make_room("owner-room", &owner.id))
        .await
        .checked("test operation should succeed");

    room_member_repo
        .add(&RoomMember::new(
            owned_room.id,
            owner.id,
            synctv_core::models::RoomRole::Creator,
        ))
        .await
        .checked("test operation should succeed");
    room_member_repo
        .add(&RoomMember::new(
            owned_room.id,
            member.id,
            synctv_core::models::RoomRole::Member,
        ))
        .await
        .checked("test operation should succeed");

    user_service
        .ban_user_and_cleanup_memberships(&owner.id, None, None)
        .await
        .checked("banning owner should succeed");

    let owner_membership = room_member_repo
        .get(&owned_room.id, &owner.id)
        .await
        .checked("owner membership lookup should succeed");
    assert!(
        owner_membership.is_none(),
        "banned owner must no longer be an active member of their owned room"
    );

    let member_membership = room_member_repo
        .get(&owned_room.id, &member.id)
        .await
        .checked("member membership lookup should succeed");
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
        .checked("member permission fence should be readable");
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
    let client_ip: std::net::IpAddr = "192.168.1.101"
        .parse()
        .checked("test operation should succeed");

    // The brute-force lockout thresholds are:
    // - 5 failures: 1 minute lockout
    // - 10 failures: 5 minute lockout
    // - 15+ failures: 15 minute lockout
    // We need to trigger at least 5 validation errors

    let mut validation_error_count = 0;
    for _ in 0..25 {
        let result = service
            .start_opaque_registration_with_control(
                "ab".to_string(),
                Some("test@example.com".to_string()),
                vec![1, 2, 3],
                Some(client_ip),
                None,
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
        .checked("create email signup user");
    let original_email = "email_update_guard_user@example.com";
    email_repo
        .create_for_user_with_executor(&created, Some(original_email), &pool)
        .await
        .checked("create original email identity");

    let mut profile_update = created.clone();
    profile_update.username = "email_update_guard_renamed".to_string();
    let updated = service
        .update_user(&profile_update, created.version)
        .await
        .checked("profile update should succeed");

    assert_eq!(updated.username, "email_update_guard_renamed");
    let unchanged_email = email_repo
        .get_email(&created.id)
        .await
        .checked("fetch unchanged email identity");
    assert_eq!(unchanged_email.as_deref(), Some(original_email));
}

async fn assert_email_bind_writes_email_only_after_confirm(pool: PgPool) {
    let service = create_user_service(&pool);
    let email_repo = UserEmailRepository::new(pool.clone());

    let original_email = "email_bind_flow_user@example.com";
    let (created, _, _) = opaque_register_user(
        &service,
        "email_bind_flow_user",
        Some(original_email.to_string()),
        "StrongPass1",
    )
    .await
    .checked("create email bind flow user");
    let new_email = "email_bind_flow_new@example.com";

    let token = service
        .start_email_bind(&created.id, new_email)
        .await
        .checked("start email bind");

    let after_start = email_repo
        .get_email(&created.id)
        .await
        .checked("fetch email after bind start");
    assert_eq!(after_start.as_deref(), Some(original_email));

    let mismatch_result = service
        .confirm_email_bind(
            &created.id,
            "email_bind_flow_other@example.com",
            &token,
            &password_verification_id(&service, &created.id, "StrongPass1").await,
        )
        .await
        .failed("email mismatch must reject pending bind request");
    assert!(
        matches!(mismatch_result, Error::InvalidInput(_)),
        "expected InvalidInput for email mismatch"
    );

    let after_mismatch = email_repo
        .get_email(&created.id)
        .await
        .checked("fetch email after bind mismatch");
    assert_eq!(after_mismatch.as_deref(), Some(original_email));

    let updated = service
        .confirm_email_bind(
            &created.id,
            new_email,
            &token,
            &password_verification_id(&service, &created.id, "StrongPass1").await,
        )
        .await
        .checked("confirm email bind");
    assert_eq!(updated.id, created.id);
    let updated_email = email_repo
        .get_email(&created.id)
        .await
        .checked("fetch updated email");
    assert_eq!(updated_email.as_deref(), Some(new_email));

    let consumed_result = service
        .confirm_email_bind(
            &created.id,
            new_email,
            &token,
            &password_verification_id(&service, &created.id, "StrongPass1").await,
        )
        .await
        .failed("consumed bind token must be rejected");
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
        .checked("create owner user");
    let owner_email = "email_bind_taken_owner@example.com";
    email_repo
        .create_for_user_with_executor(&owner, Some(owner_email), &pool)
        .await
        .checked("create owner email identity");
    let requester = user_repo
        .create(&make_user("email_bind_taken_requester"))
        .await
        .checked("create requester user");

    let result = service
        .start_email_bind(&requester.id, owner_email)
        .await
        .failed("taken email must be rejected");
    assert!(
        matches!(result, Error::AlreadyExists(_)),
        "expected AlreadyExists for taken email"
    );
}

async fn assert_two_factor_requires_two_usable_methods(pool: PgPool) {
    let service = create_user_service(&pool);

    let (password_only, _, _) =
        opaque_register_user(&service, "two_factor_password_only", None, "StrongPass1")
            .await
            .checked("create password-only user");
    let result = service
        .set_two_factor_enabled(&password_only.id, true)
        .await
        .failed("single-method users must not enable two-factor authentication");
    assert!(
        matches!(&result, Error::InvalidInput(message) if message.contains("requires at least two")),
        "expected InvalidInput for insufficient auth factors, got {result:?}"
    );

    let (email_and_password, _, _) = opaque_register_user(
        &service,
        "two_factor_email_password",
        Some("two_factor_email_password@example.com".to_string()),
        "StrongPass1",
    )
    .await
    .checked("create email+password user");
    let (preferences, factors) = service
        .set_two_factor_enabled(&email_and_password.id, true)
        .await
        .checked("email+password user can enable two-factor authentication");
    assert!(preferences.two_factor_enabled);
    assert!(factors.password);
    assert!(factors.email);
    assert_eq!(factors.eligible_count(), 2);
}

async fn assert_sensitive_verification_is_one_time(pool: PgPool) {
    let service = create_user_service(&pool);
    let (user, _, _) = opaque_register_user(
        &service,
        "sensitive_verification_one_time",
        Some("sensitive_verification_one_time@example.com".to_string()),
        "StrongPass1",
    )
    .await
    .checked("create user with password");

    let verification_id = password_verification_id(&service, &user.id, "StrongPass1").await;
    service
        .consume_sensitive_operation_verification(&user.id, &verification_id)
        .await
        .checked("first verification consumption should succeed");
    let reused = service
        .consume_sensitive_operation_verification(&user.id, &verification_id)
        .await
        .failed("verification id must be single-use");
    assert!(
        matches!(reused, Error::Authentication(_)),
        "expected Authentication for reused verification id, got {reused:?}"
    );
}

async fn assert_sensitive_password_verification_is_rate_limited(pool: PgPool) {
    let service = create_user_service(&pool);
    let (user, _, _) = opaque_register_user(
        &service,
        "sensitive_verification_rate_limit",
        Some("sensitive_verification_rate_limit@example.com".to_string()),
        "StrongPass1",
    )
    .await
    .checked("create user with password");
    let outcome = service
        .start_sensitive_operation_verification(&user.id, None)
        .await
        .checked("start sensitive verification");
    let SensitiveVerificationOutcome::Pending(challenge) = outcome else {
        std::panic::panic_any("password-sensitive verification should start with a challenge");
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
        .failed("sensitive password verification should lock out after repeated failures");
    assert!(
        matches!(locked, Error::Authentication(ref message) if message.contains("Too many failed attempts")),
        "expected sensitive verification brute-force lockout, got {locked:?}"
    );
}

async fn assert_sensitive_verification_requires_two_local_factors_when_2fa_enabled(pool: PgPool) {
    let service = create_user_service(&pool);
    let email = "sensitive_verification_2fa@example.com";
    let (user, _, _) = opaque_register_user(
        &service,
        "sensitive_verification_2fa",
        Some(email.to_string()),
        "StrongPass1",
    )
    .await
    .checked("create user with password and email");
    insert_trusted_email_identity(&pool, &user.id, email).await;
    service
        .set_two_factor_enabled(&user.id, true)
        .await
        .checked("password+email user can enable two-factor authentication");

    let outcome = service
        .start_sensitive_operation_verification(&user.id, None)
        .await
        .checked("start sensitive verification");
    let SensitiveVerificationOutcome::Pending(challenge) = outcome else {
        std::panic::panic_any(
            "2FA-enabled sensitive verification should start with a pending challenge",
        );
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
        .checked("password factor should verify");
    let SensitiveVerificationOutcome::Pending(next_challenge) = pending else {
        std::panic::panic_any("2FA-enabled sensitive verification should require another factor");
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
        .checked("email factor should complete");
    let SensitiveVerificationOutcome::Complete { verification_id } = complete else {
        std::panic::panic_any("second factor should complete sensitive verification");
    };
    service
        .consume_sensitive_operation_verification(&user.id, &verification_id)
        .await
        .checked("completed two-factor verification should be consumable");
}

async fn assert_oauth2_session_sensitive_verification_requires_one_local_factor(pool: PgPool) {
    let service = create_user_service(&pool);
    let email = "sensitive_verification_oauth2@example.com";
    let (user, _, _) = opaque_register_user(
        &service,
        "sensitive_verification_oauth2",
        Some(email.to_string()),
        "StrongPass1",
    )
    .await
    .checked("create user with password and email");
    insert_trusted_email_identity(&pool, &user.id, email).await;
    service
        .set_two_factor_enabled(&user.id, true)
        .await
        .checked("password+email user can enable two-factor authentication");

    let outcome = service
        .start_sensitive_operation_verification(&user.id, Some(TokenAuthContext::OAuth2))
        .await
        .checked("start OAuth2 sensitive verification");
    let SensitiveVerificationOutcome::Pending(challenge) = outcome else {
        std::panic::panic_any(
            "OAuth2-session sensitive verification should start with local factors when present",
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
        .checked("one local factor should complete OAuth2-session sensitive verification");
    let SensitiveVerificationOutcome::Complete { verification_id } = complete else {
        std::panic::panic_any(
            "OAuth2-session sensitive verification should complete after one local factor",
        );
    };
    service
        .consume_sensitive_operation_verification(&user.id, &verification_id)
        .await
        .checked("OAuth2-session verification should be consumable");
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
        .checked("create OAuth2-only user");
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
        .checked("OAuth2-only account should receive a bootstrap verification id");
    let SensitiveVerificationOutcome::Complete { verification_id } = outcome else {
        std::panic::panic_any("OAuth2-only bootstrap should complete from current OAuth2 session");
    };
    service
        .consume_sensitive_operation_verification(&user.id, &verification_id)
        .await
        .checked("OAuth2-only bootstrap verification should be consumable");
}

async fn assert_two_factor_blocks_deleting_required_passkey(pool: PgPool) {
    let user_service = Arc::new(create_user_service(&pool));
    let (user, _, _) = opaque_register_user(
        user_service.as_ref(),
        "two_factor_passkey_user",
        None,
        "StrongPass1",
    )
    .await
    .checked("create password+passkey user");
    let credential_id = b"two-factor-required-passkey";
    insert_test_passkey(&pool, &user.id, credential_id).await;

    user_service
        .set_two_factor_enabled(&user.id, true)
        .await
        .checked("password+passkey user can enable two-factor authentication");

    let passkey_service = make_passkey_service(pool, user_service);
    let result = passkey_service
        .delete_credential(&user.id, credential_id)
        .await
        .failed("deleting the passkey would leave fewer than two auth methods");
    assert!(
        matches!(&result, Error::InvalidInput(message) if message.contains("remaining verification methods are insufficient")),
        "expected InvalidInput for deleting required passkey, got {result:?}"
    );
}

async fn assert_two_factor_blocks_single_factor_token_issuance(pool: PgPool) {
    let service = create_user_service(&pool);
    let (user, _, _) = opaque_register_user(
        &service,
        "two_factor_login_blocked",
        Some("two_factor_login_blocked@example.com".to_string()),
        "StrongPass1",
    )
    .await
    .checked("create user with password");

    insert_trusted_email_identity(&pool, &user.id, "two_factor_login_blocked@example.com").await;
    let refresh_token = match opaque_login_user(&service, "two_factor_login_blocked", "StrongPass1")
        .await
        .checked("single-factor login should work before 2FA is enabled")
    {
        AuthenticatedLogin::Complete { refresh_token, .. } => refresh_token,
        AuthenticatedLogin::MfaRequired { .. } => {
            std::panic::panic_any("2FA is disabled, login should be complete")
        }
    };

    service
        .set_two_factor_enabled(&user.id, true)
        .await
        .checked("password+verified email user can enable two-factor authentication");

    let login_result =
        opaque_login_user_with_challenge(&service, "two_factor_login_blocked", "StrongPass1")
            .await
            .checked("first factor should return an MFA challenge after 2FA is enabled");
    let AuthenticatedLogin::MfaRequired { challenge, .. } = login_result else {
        std::panic::panic_any("single-factor login must not issue tokens after 2FA is enabled");
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
        .checked("verified second factor should complete MFA login")
    {
        AuthenticatedLogin::Complete { refresh_token, .. } => refresh_token,
        AuthenticatedLogin::MfaRequired { .. } => {
            std::panic::panic_any("completed MFA must issue tokens")
        }
    };
    let (rotated_access, rotated_refresh) = service
        .refresh_token(mfa_refresh_token)
        .await
        .checked("refresh token issued after MFA should rotate successfully");
    assert!(!rotated_access.is_empty());
    assert!(!rotated_refresh.is_empty());

    let refresh_result = service
        .refresh_token(refresh_token)
        .await
        .failed("refresh token rotation must not issue tokens after 2FA is enabled");
    assert!(
        matches!(&refresh_result, Error::Authentication(message) if message.contains("Two-factor authentication is required")),
        "expected Authentication error requiring 2FA during refresh, got {refresh_result:?}"
    );
}

async fn assert_two_factor_access_token_context_is_enforced(pool: PgPool) {
    let (service, jwt, pipeline) = create_user_service_with_security_pipeline(&pool);
    let (user, old_access_token, old_refresh_token) = opaque_register_user(
        service.as_ref(),
        "two_factor_access_context",
        Some("two_factor_access_context@example.com".to_string()),
        "StrongPass1",
    )
    .await
    .checked("create user with password");
    let old_access_token =
        old_access_token.checked("2FA-disabled registration issues access token");
    let old_refresh_token =
        old_refresh_token.checked("2FA-disabled registration issues refresh token");
    let old_access_claims = jwt
        .verify_access_token(&old_access_token)
        .checked("old access token should be syntactically valid");
    pipeline
        .check(&old_access_claims)
        .await
        .checked("single-factor access token should work before 2FA is enabled");

    insert_trusted_email_identity(&pool, &user.id, "two_factor_access_context@example.com").await;
    service
        .set_two_factor_enabled(&user.id, true)
        .await
        .checked("password+verified email user can enable two-factor authentication");

    let result = pipeline
        .check(&old_access_claims)
        .await
        .failed("old single-factor access token must be rejected while 2FA is enabled");
    assert!(
        matches!(&result, Error::Authentication(message) if message.contains("Two-factor authentication is required")),
        "expected old access token to require 2FA context, got {result:?}"
    );
    let refresh_result = service
        .refresh_token(old_refresh_token)
        .await
        .failed("old single-factor refresh token must also be rejected while 2FA is enabled");
    assert!(
        matches!(&refresh_result, Error::Authentication(message) if message.contains("Two-factor authentication is required")),
        "expected old refresh token to require 2FA context, got {refresh_result:?}"
    );

    let login_result = opaque_login_user_with_challenge(
        service.as_ref(),
        "two_factor_access_context",
        "StrongPass1",
    )
    .await
    .checked("password first factor should start MFA challenge");
    let AuthenticatedLogin::MfaRequired { challenge, .. } = login_result else {
        std::panic::panic_any("2FA-enabled password login should require email second factor");
    };
    let mfa_access_token = match service
        .complete_mfa_session_with_control(
            &challenge.session_id,
            AuthFactorMethod::Email,
            None,
            None,
        )
        .await
        .checked("verified email second factor should complete MFA login")
    {
        AuthenticatedLogin::Complete { access_token, .. } => access_token,
        AuthenticatedLogin::MfaRequired { .. } => {
            std::panic::panic_any("completed MFA must issue tokens")
        }
    };
    let mfa_access_claims = jwt
        .verify_access_token(&mfa_access_token)
        .checked("MFA access token should be syntactically valid");
    assert!(
        mfa_access_claims.satisfies_two_factor_requirement(),
        "MFA-completed token must carry a 2FA auth context"
    );
    pipeline
        .check(&mfa_access_claims)
        .await
        .checked("MFA access token should work while 2FA is enabled");

    insert_oauth2_identity(&pool, &user.id, "github", "oauth2-provider-user-id").await;
    let oauth_access_token = match service
        .login_oauth2(&user.id, "github", "oauth2-provider-user-id", None)
        .await
        .checked("OAuth2 login should stay independent from local 2FA")
    {
        AuthenticatedLogin::Complete { access_token, .. } => access_token,
        AuthenticatedLogin::MfaRequired { .. } => {
            std::panic::panic_any("OAuth2 login must not start a local MFA challenge")
        }
    };
    let oauth_access_claims = jwt
        .verify_access_token(&oauth_access_token)
        .checked("OAuth2 access token should be syntactically valid");
    assert!(
        oauth_access_claims.satisfies_two_factor_requirement(),
        "OAuth2 token must carry its independent auth context"
    );
    pipeline
        .check(&oauth_access_claims)
        .await
        .checked("OAuth2 access token should work while 2FA is enabled");

    service
        .set_two_factor_enabled(&user.id, false)
        .await
        .checked("2FA can be disabled once the caller has a valid strong context");
    pipeline
        .check(&old_access_claims)
        .await
        .checked("single-factor access token should work again after 2FA is disabled");
    pipeline
        .check(&mfa_access_claims)
        .await
        .checked("MFA access token should remain valid after 2FA is disabled");
}

async fn assert_two_factor_allows_oauth2_without_local_mfa(pool: PgPool) {
    let service = create_user_service(&pool);
    let (user, _, _) = opaque_register_user(
        &service,
        "two_factor_oauth2_allowed",
        Some("two_factor_oauth2_allowed@example.com".to_string()),
        "StrongPass1",
    )
    .await
    .checked("create user with password");

    insert_trusted_email_identity(&pool, &user.id, "two_factor_oauth2_allowed@example.com").await;
    service
        .set_two_factor_enabled(&user.id, true)
        .await
        .checked("password+verified email user can enable two-factor authentication");

    insert_oauth2_identity(&pool, &user.id, "github", "oauth2-provider-user-id-mfa").await;
    let (access_token, refresh_token) = match service
        .login_oauth2(&user.id, "github", "oauth2-provider-user-id-mfa", None)
        .await
        .checked("OAuth2 login should stay independent from local 2FA")
    {
        AuthenticatedLogin::Complete {
            access_token,
            refresh_token,
            ..
        } => (access_token, refresh_token),
        AuthenticatedLogin::MfaRequired { .. } => {
            std::panic::panic_any("OAuth2 login must not start a local MFA challenge")
        }
    };
    assert!(!access_token.is_empty());
    let (rotated_access, rotated_refresh) = service
        .refresh_token(refresh_token)
        .await
        .checked("OAuth2 refresh token should rotate for 2FA-enabled users");
    assert!(!rotated_access.is_empty());
    assert!(!rotated_refresh.is_empty());
}

async fn assert_refresh_token_rejects_unbound_oauth2_identity(pool: PgPool) {
    let service = create_user_service(&pool);
    let (user, _, _) = opaque_register_user(
        &service,
        "oauth_refresh_binding",
        Some("oauth_refresh_binding@example.com".to_string()),
        "StrongPass1",
    )
    .await
    .checked("create user with password");

    insert_oauth2_identity(&pool, &user.id, "github", "oauth-refresh-provider-user").await;
    let refresh_token = match service
        .login_oauth2(&user.id, "github", "oauth-refresh-provider-user", None)
        .await
        .checked("OAuth2 login should issue tokens")
    {
        AuthenticatedLogin::Complete { refresh_token, .. } => refresh_token,
        AuthenticatedLogin::MfaRequired { .. } => {
            std::panic::panic_any("OAuth2 login should complete")
        }
    };

    sqlx::query!(
        "DELETE FROM auth_oauth2_identities
         WHERE user_id = $1 AND provider_instance_name = $2 AND provider_user_id = $3",
        user.id.as_i64(),
        "github",
        "oauth-refresh-provider-user"
    )
    .execute(&pool)
    .await
    .checked("delete oauth2 identity");

    let result = service.refresh_token(refresh_token).await;
    assert!(
        matches!(result, Err(Error::Authentication(_))),
        "OAuth2-bound refresh token should be rejected after unlink"
    );
}

async fn assert_refresh_token_rejects_unbound_email_identity(pool: PgPool) {
    let service = create_user_service(&pool);
    let (user, _, _) = opaque_register_user(
        &service,
        "email_refresh_binding",
        Some("email_refresh_binding@example.com".to_string()),
        "StrongPass1",
    )
    .await
    .checked("create user with password");
    let refresh_token = match service
        .login_with_verified_email(&user.id, "email-refresh-binding", None)
        .await
        .checked("verified email login should issue tokens")
    {
        AuthenticatedLogin::Complete { refresh_token, .. } => refresh_token,
        AuthenticatedLogin::MfaRequired { .. } => {
            std::panic::panic_any("email login should complete")
        }
    };

    sqlx::query!(
        "DELETE FROM auth_email_identities WHERE user_id = $1",
        user.id.as_i64()
    )
    .execute(&pool)
    .await
    .checked("delete email identity");

    let result = service.refresh_token(refresh_token).await;
    assert!(
        matches!(result, Err(Error::Authentication(_))),
        "Email-bound refresh token should be rejected after unlink"
    );
}

async fn assert_refresh_token_rejects_deleted_passkey_binding(pool: PgPool) {
    let service = create_user_service(&pool);
    let (user, _, _) = opaque_register_user(
        &service,
        "passkey_refresh_binding",
        Some("passkey_refresh_binding@example.com".to_string()),
        "StrongPass1",
    )
    .await
    .checked("create user with password");
    let credential_id = b"passkey-refresh-binding";
    insert_test_passkey(&pool, &user.id, credential_id).await;

    let refresh_token = match service
        .login_with_verified_external_credential_with_control(
            &user.id,
            credential_id,
            "passkey-refresh-binding",
            None,
            None,
        )
        .await
        .checked("passkey login should issue tokens")
    {
        AuthenticatedLogin::Complete { refresh_token, .. } => refresh_token,
        AuthenticatedLogin::MfaRequired { .. } => {
            std::panic::panic_any("passkey login should complete")
        }
    };

    sqlx::query!(
        "DELETE FROM auth_webauthn_credentials WHERE user_id = $1 AND credential_id = $2",
        user.id.as_i64(),
        credential_id.as_slice()
    )
    .execute(&pool)
    .await
    .checked("delete passkey credential");

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
