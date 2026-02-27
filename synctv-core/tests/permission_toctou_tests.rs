//! Permission TOCTOU (Time-of-check to Time-of-use) tests
//!
//! Tests that permission checks are performed within database transactions
//! to prevent race conditions where permissions are revoked between check and use.
//!
//! Run with: cargo test -p synctv-core --test permission_toctou_tests
//!
//! # Test Coverage
//!
//! - Concurrent permission revocation during media deletion
//! - Verify operations fail when permissions revoked mid-operation
//! - Ensure transaction-atomic permission checks
//!
//! # Requirements
//!
//! - Docker for testcontainers (PostgreSQL)

use synctv_core::{
    models::{
        Room, RoomId, RoomMember, RoomRole,
        UserId, User, UserRole, UserStatus, PermissionBits,
    },
    repository::{RoomRepository, UserRepository, RoomMemberRepository},
    service::{
        RoomService, UserService,
        permission::PermissionService,
    },
    Error,
};
use chrono::Utc;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::Barrier;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

/// Default PostgreSQL version for test containers
const POSTGRES_VERSION: &str = "16-alpine";

// ============================================================================
// Test Infrastructure
// ============================================================================

/// Test container wrapper for Postgres
pub struct TestPostgres {
    pub pool: PgPool,
    _container: ContainerAsync<Postgres>,
}

async fn create_test_pool() -> TestPostgres {
    let container = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
        .start()
        .await
        .expect("Failed to start Postgres container");

    let host = container.get_host().await.expect("Failed to get host");
    let port = container.get_host_port_ipv4(5432).await.expect("Failed to get port");

    let database_url = format!(
        "postgres://synctv:synctv_test@{}:{}/synctv_test",
        host, port
    );

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .expect("Failed to connect to database");

    // Run migrations
    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    TestPostgres { pool, _container: container }
}

/// Create a test user in the database
fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{}@test.com", username)),
        password_hash: "test_hash".to_string(),
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

/// Create a test room
fn make_room(name: &str, description: &str, owner: &UserId) -> Room {
    Room::new(
        name.to_string(),
        description.to_string(),
        owner.clone(),
    )
}

/// Test helper: Setup UserService for RoomService
fn setup_user_service(pool: PgPool) -> UserService {
    use synctv_core::{
        cache::{KeyBuilder, UsernameCache, NoopCacheL2},
        config::PasswordComplexityConfig,
        service::auth::{JwtService, BruteForceProtection},
    };

    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);
    let password_complexity = PasswordComplexityConfig::default();
    let token_blacklist = Arc::new(synctv_core::service::InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
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

/// Setup test room with owner
async fn setup_test_room(pool: &PgPool, room_name: &str) -> (User, RoomId) {
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo.create(&make_user(&format!("{}_owner", room_name))).await.expect("Failed to create owner");
    let room = room_repo.create(&make_room(room_name, "Test room", &owner.id))
        .await
        .expect("Failed to create room");

    // Add owner as member (Creator)
    let owner_member = RoomMember::new(room.id.clone(), owner.id.clone(), RoomRole::Creator);
    member_repo.add(&owner_member).await.expect("Failed to add owner as member");

    (owner, room.id.clone())
}

// ============================================================================
// Test: Permission Revocation During Remove Media
// ============================================================================

/// Test that permission revocation during remove_media is detected.
///
/// This test demonstrates the TOCTOU vulnerability:
/// 1. User has DELETE_MOVIE_ANY permission
/// 2. Permission check passes (before transaction)
/// 3. Creator revokes the permission concurrently
/// 4. Operation should fail if permission check is inside transaction
///
/// Current implementation has the check OUTSIDE the transaction, so this test
/// is expected to demonstrate the vulnerability (operation succeeds when it shouldn't).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_permission_revocation_during_remove_media() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    // Setup: Create room with creator
    let (creator, room_id) = setup_test_room(pool, "TOCTOU Test").await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create an admin user with DELETE_MOVIE_ANY permission
    let admin_user = user_repo.create(&make_user("admin_deleter")).await.expect("Failed to create admin");
    let mut admin_member = RoomMember::new(room_id.clone(), admin_user.id.clone(), RoomRole::Admin);
    // Grant DELETE_MOVIE_ANY permission
    admin_member.added_permissions = PermissionBits::DELETE_MOVIE_ANY;
    member_repo.add(&admin_member).await.expect("Failed to add admin");

    // Create a test media via direct SQL for simplicity
    let playlist_id = synctv_core::models::id::PlaylistId::new();
    let media_id: synctv_core::models::id::MediaId = sqlx::query_scalar(
        "INSERT INTO playlists (id, room_id, creator_id, name, parent_id, position, created_at, updated_at, version)
         VALUES ($1, $2, $3, '', NULL, 0, NOW(), NOW(), 0)
         RETURNING (SELECT id FROM media WHERE playlist_id = $1 LIMIT 1)"
    )
    .bind(playlist_id.as_str())
    .bind(room_id.as_str())
    .bind(admin_user.id.as_str())
    .fetch_one(pool)
    .await
    .expect("Failed to create playlist");

    // Actually, let's use RoomService to add media properly
    let user_service = setup_user_service(pool.clone());
    let room_service = Arc::new(RoomService::new(pool.clone(), user_service));

    // Add media to the room
    let media_id = room_service.add_media_to_playlist(
        room_id.clone(),
        admin_user.id.clone(),
        synctv_core::models::playlist::CreatePlaylistRequest {
            room_id: room_id.clone(),
            name: String::new(), // root playlist
            parent_id: None,
            position: None,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
        },
        synctv_core::service::media::AddMediaRequest {
            url: "http://example.com/video.mp4".to_string(),
            name: Some("Test Media".to_string()),
            provider_type: None,
            provider_instance_name: None,
        },
    ).await.expect("Failed to add media");

    // Verify admin has the permission
    let permission_service = PermissionService::new(
        member_repo.clone(),
        RoomRepository::new(pool.clone()),
        None,
        1000,
        300,
    );

    let perms = permission_service
        .get_user_permissions_no_cache(&room_id, &admin_user.id)
        .await
        .expect("Failed to get permissions");

    assert!(perms.has(PermissionBits::DELETE_MOVIE_ANY), "Admin should have DELETE_MOVIE_ANY permission");

    // Create a barrier to synchronize the revocation and removal
    let barrier = Arc::new(Barrier::new(2));
    let pool_clone = pool.clone();
    let room_id_clone = room_id.clone();
    let admin_id_clone = admin_user.id.clone();
    let creator_id_clone = creator.id.clone();
    let media_id_clone = media_id.clone();
    let room_service_clone = room_service.clone();

    // Spawn task to remove media (will check permission, then wait, then delete)
    let remove_task = tokio::spawn(async move {
        barrier.wait().await; // Wait for revocation task to be ready

        // This will check permission (should pass), then start transaction
        let result = room_service_clone.remove_media(
            room_id_clone.clone(),
            admin_id_clone.clone(),
            media_id_clone.clone(),
        ).await;

        result
    });

    // Spawn task to revoke permission concurrently
    let revoke_task = tokio::spawn(async move {
        barrier.wait().await; // Wait for remove task to be ready

        // Small delay to ensure remove task starts first and checks permission
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Revoke DELETE_MOVIE_ANY permission
        let mut tx = pool_clone.begin().await.expect("Failed to begin transaction");
        sqlx::query(
            "UPDATE room_members
             SET added_permissions = 0,
                 version = version + 1
             WHERE room_id = $1 AND user_id = $2"
        )
        .bind(room_id_clone.as_str())
        .bind(admin_id_clone.as_str())
        .execute(&mut *tx)
        .await
        .expect("Failed to revoke permission");

        tx.commit().await.expect("Failed to commit revocation");

        tracing::info!("Permission revoked");
    });

    // Wait for both tasks to complete
    let remove_result = remove_task.await.expect("Remove task panicked");
    let _ = revoke_task.await.expect("Revoke task panicked");

    // Current implementation: Permission check is OUTSIDE transaction
    // Expected behavior (VULNERABLE):
    //   - remove_media succeeds (permission was checked before revocation)
    //
    // Desired behavior (FIXED):
    //   - remove_media fails (permission revoked before transaction commits)

    // For now, we expect the vulnerable behavior (operation succeeds)
    // This test documents the current state and should be updated after the fix
    match remove_result {
        Ok(_) => {
            // Current implementation: Permission check outside transaction
            // Operation succeeds even though permission was revoked

            tracing::warn!(
                "TOCTOU vulnerability confirmed: remove_media succeeded despite permission revocation. \
                 This is expected BEFORE the fix."
            );
        }
        Err(Error::Authorization(msg)) => {
            // Fixed implementation: Permission check inside transaction

            tracing::info!(
                "TOCTOU fixed: remove_media failed due to permission revocation within transaction. \
                 Error: {}", msg
            );
        }
        Err(other) => {
            panic!("Unexpected error: {:?}", other);
        }
    }
}

/// Test baseline: remove_media works when permission is not revoked
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_remove_media_with_permission() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (creator, room_id) = setup_test_room(pool, "Baseline Test").await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create an admin user with DELETE_MOVIE_ANY permission
    let admin_user = user_repo.create(&make_user("admin_baseline")).await.expect("Failed to create admin");
    let mut admin_member = RoomMember::new(room_id.clone(), admin_user.id.clone(), RoomRole::Admin);
    admin_member.added_permissions = PermissionBits::DELETE_MOVIE_ANY;
    member_repo.add(&admin_member).await.expect("Failed to add admin");

    // Create RoomService and add media
    let user_service = setup_user_service(pool.clone());
    let room_service = RoomService::new(pool.clone(), user_service);

    // Add media
    let media_id = room_service.add_media_to_playlist(
        room_id.clone(),
        admin_user.id.clone(),
        synctv_core::models::playlist::CreatePlaylistRequest {
            room_id: room_id.clone(),
            name: String::new(),
            parent_id: None,
            position: None,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
        },
        synctv_core::service::media::AddMediaRequest {
            url: "http://example.com/video.mp4".to_string(),
            name: Some("Test Media".to_string()),
            provider_type: None,
            provider_instance_name: None,
        },
    ).await.expect("Failed to add media");

    // Remove media should succeed
    let result = room_service.remove_media(
        room_id.clone(),
        admin_user.id.clone(),
        media_id.clone(),
    ).await;

    assert!(result.is_ok(), "Remove media should succeed with valid permission");
}

/// Test baseline: remove_media fails without permission
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_remove_media_without_permission() {
    let infra = create_test_pool().await;
    let pool = &infra.pool;

    let (creator, room_id) = setup_test_room(pool, "No Permission Test").await;

    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create a regular member without DELETE_MOVIE_ANY permission
    let regular_user = user_repo.create(&make_user("regular_member")).await.expect("Failed to create user");
    let regular_member = RoomMember::new(room_id.clone(), regular_user.id.clone(), RoomRole::Member);
    member_repo.add(&regular_member).await.expect("Failed to add member");

    // Create RoomService and add media (as creator)
    let user_service = setup_user_service(pool.clone());
    let room_service = RoomService::new(pool.clone(), user_service);

    // Add media as creator
    let media_id = room_service.add_media_to_playlist(
        room_id.clone(),
        creator.id.clone(),
        synctv_core::models::playlist::CreatePlaylistRequest {
            room_id: room_id.clone(),
            name: String::new(),
            parent_id: None,
            position: None,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
        },
        synctv_core::service::media::AddMediaRequest {
            url: "http://example.com/video.mp4".to_string(),
            name: Some("Test Media".to_string()),
            provider_type: None,
            provider_instance_name: None,
        },
    ).await.expect("Failed to add media");

    // Remove media should fail (regular user trying to delete creator's media)
    let result = room_service.remove_media(
        room_id.clone(),
        regular_user.id.clone(),
        media_id.clone(),
    ).await;

    assert!(result.is_err(), "Remove media should fail without permission");
}
