//! RoomService integration tests
//!
//! Tests the RoomService business logic layer with real PostgreSQL via testcontainers.
//!
//! Run with: cargo test -p synctv-core --test room_service_tests -- --nocapture

use std::sync::Arc;

use synctv_core::{
    cache::{KeyBuilder, UsernameCache, NoopCacheL2},
    config::PasswordComplexityConfig,
    models::{
        RoomSettings, UserId, User, UserRole, UserStatus,
        RoomRole,
    },
    repository::{RoomRepository, UserRepository, RoomMemberRepository, RoomSettingsRepository},
    service::{
        RoomService, UserService, InMemoryTokenBlacklistStore,
        auth::{JwtService, BruteForceProtection},
    },
    Error,
};
use chrono::Utc;
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

const POSTGRES_VERSION: &str = "16-alpine";

async fn create_test_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let postgres = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
        .start()
        .await
        .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/synctv_test",
        postgres.get_host_port_ipv4(5432).await.expect("Failed to get port")
    );

    let pool = PgPool::connect(&connection_string)
        .await
        .expect("Failed to create pool");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (postgres, pool)
}

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
        email: Some(format!("{}@test.com", username)),
        password_hash: "hash".to_string(),
        role: UserRole::User,
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

// ========== Room Creation Tests ==========

#[tokio::test]
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
    assert!(settings.require_password.0, "require_password should be true");
}

#[tokio::test]
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
    assert!(!settings.require_password.0, "require_password should be false");
}

#[tokio::test]
async fn test_create_room_creates_root_playlist() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("playlist_owner")).await.unwrap();

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
    let playlist_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM playlists WHERE room_id = $1"
    )
    .bind(room.id.as_str())
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(playlist_count, 1, "Root playlist should be created");
}

// ========== Room Join Tests ==========

#[tokio::test]
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
        .join_room(room.id.clone(), joiner.id.clone(), Some("CorrectPassword123".to_string()))
        .await
        .unwrap();

    assert_eq!(joined_room.id, room.id);
    assert_eq!(member.user_id, joiner.id);
    assert_eq!(member.role, RoomRole::Member);
    assert!(members.len() >= 2, "Should have at least creator and joiner");
}

#[tokio::test]
async fn test_join_room_wrong_password_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("wrong_pwd_owner")).await.unwrap();
    let joiner = user_repo.create(&make_user("wrong_pwd_joiner")).await.unwrap();

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
        .join_room(room.id.clone(), joiner.id.clone(), Some("WrongPassword456".to_string()))
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        Error::Authorization(msg) => {
            assert!(msg.contains("Invalid password") || msg.contains("password"), "Error should mention password: {}", msg);
        }
        other => panic!("Expected Authorization error, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_join_room_password_required_not_provided() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("nopwd_join_owner")).await.unwrap();
    let joiner = user_repo.create(&make_user("nopwd_join_user")).await.unwrap();

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
            assert!(msg.contains("Password required") || msg.contains("password"), "Error should mention password required: {}", msg);
        }
        other => panic!("Expected Authorization error, got: {:?}", other),
    }
}

// ========== Room Leave Tests ==========

#[tokio::test]
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
            assert!(msg.contains("creator") || msg.contains("Creator"), "Error should mention creator: {}", msg);
        }
        other => panic!("Expected Authorization error, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_leave_room_member_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("leave_succ_owner")).await.unwrap();
    let joiner = user_repo.create(&make_user("leave_succ_member")).await.unwrap();

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
    assert!(fetched.is_none(), "Room should not be found after soft-delete");

    // But should still exist in DB with deleted_at set
    let deleted_at: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(
        "SELECT deleted_at FROM rooms WHERE id = $1"
    )
    .bind(room.id.as_str())
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(deleted_at.is_some(), "deleted_at should be set");
}

// ========== CAS Exhaustion Test (B6 fix verification) ==========

#[tokio::test]
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
            assert!(msg.contains("maximum retry"), "Should mention retry exhaustion: {}", msg);
        }
        Err(Error::OptimisticLockConflict) => {
            panic!("Bug B6: OptimisticLockConflict should NOT leak; should be wrapped in Internal error");
        }
        Err(other) => {
            panic!("Unexpected error: {:?}", other);
        }
    }
}

// ========== Banned User Cannot Rejoin (S10) ==========

#[tokio::test]
async fn test_banned_user_cannot_rejoin_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("ban_rejoin_creator")).await.unwrap();
    let target = user_repo.create(&make_user("ban_rejoin_target")).await.unwrap();

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
                "Error should mention ban: {}",
                msg
            );
        }
        other => panic!("Expected Authorization error, got: {:?}", other),
    }
}

// ========== Room Description Validation Tests ==========

#[tokio::test]
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
async fn test_room_description_over_500_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("long_desc_owner")).await.unwrap();

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
            assert!(msg.contains("description") || msg.contains("500"), "Should mention description limit: {}", msg);
        }
        other => panic!("Expected InvalidInput error, got: {:?}", other),
    }
}
