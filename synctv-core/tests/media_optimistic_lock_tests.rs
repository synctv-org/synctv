//! Media optimistic locking integration tests
//!
//! Tests for version-based optimistic locking in MediaRepository.
//!
//! Run with: cargo test --test media_optimistic_lock_tests

use synctv_core_testing::{create_test_pool, create_test_jwt_service};
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::redis::Redis;
use synctv_core::{
    models::{
        Room, RoomId, RoomStatus, UserId, User, UserRole, UserStatus,
        Playlist, PlaylistId, Media, MediaId,
    },
    repository::{RoomRepository, UserRepository, PlaylistRepository, MediaRepository},
};
use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
/// Default PostgreSQL version for test containers
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

struct TestContext {
    _container: ContainerAsync<Postgres>,
    pool: PgPool,
    #[allow(dead_code)]
    owner: User,
    room: Room,
    root_playlist: Playlist,
}

async fn setup_test_context(suffix: &str) -> TestContext {
    let (container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user(&format!("optlock_owner_{}", suffix))).await.unwrap();
    let room = room_repo.create(&{
        let now = Utc::now();
        Room {
            id: RoomId::new(),
            name: format!("OptLock Room {}", suffix),
            description: String::new(),
            created_by: owner.id.clone(),
            status: RoomStatus::Active,
            is_banned: false,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 0,
        }
    }).await.unwrap();

    let root_playlist = playlist_repo.create(&Playlist {
        id: PlaylistId::new(),
        room_id: room.id.clone(),
        creator_id: Some(owner.id.clone()),
        name: String::new(),
        parent_id: None,
        position: 0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    }).await.unwrap();

    TestContext {
        _container: container,
        pool,
        owner,
        room,
        root_playlist,
    }
}

fn make_media(playlist_id: &PlaylistId, room_id: &RoomId, name: &str, position: i32) -> Media {
    Media {
        id: MediaId::new(),
        playlist_id: playlist_id.clone(),
        room_id: room_id.clone(),
        creator_id: None,
        name: name.to_string(),
        position,
        source_provider: "direct_url".to_string(),
        source_config: json!({"url": "https://example.com/video.mp4"}),
        provider_instance_name: None,
        added_at: Utc::now(),
        version: 0,
    }
}

// ============================================================================
// Optimistic Locking Tests with version field
// ============================================================================

/// Test: update_with_version should succeed when version matches
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_with_version_succeeds_when_version_matches() {
    let ctx = setup_test_context("v1").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    // Create a media item
    let media = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "version_test.mp4", 0))
        .await
        .unwrap();

    // Initial version should be 0
    assert_eq!(media.version, 0, "New media should have version 0");

    // Update with matching version should succeed
    let mut updated = media.clone();
    updated.name = "version_test_updated.mp4".to_string();

    let result = media_repo
        .update_with_version(&updated, media.version)
        .await
        .unwrap();

    assert!(result.is_some(), "Update should succeed when version matches");
    let result = result.unwrap();
    assert_eq!(result.name, "version_test_updated.mp4");
    assert_eq!(result.version, 1, "Version should be incremented to 1");
}

/// Test: update_with_version should return OptimisticLockConflict error when version mismatch
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_with_version_conflict_when_version_mismatch() {
    let ctx = setup_test_context("v2").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    // Create a media item
    let media = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "conflict_test.mp4", 0))
        .await
        .unwrap();

    // First update succeeds, version becomes 1
    let mut first_update = media.clone();
    first_update.name = "first_update.mp4".to_string();
    let first_result = media_repo
        .update_with_version(&first_update, 0)
        .await
        .unwrap()
        .expect("First update should succeed");
    assert_eq!(first_result.version, 1);

    // Try to update with stale version 0 - should fail with conflict
    let mut stale_update = media.clone();
    stale_update.name = "stale_update.mp4".to_string();
    let result = media_repo
        .update_with_version(&stale_update, 0)  // Using old version 0, but DB has 1
        .await
        .unwrap();

    assert!(result.is_none(), "Update with stale version should return None");

    // Verify the data wasn't corrupted
    let current = media_repo.get_by_id(&media.id).await.unwrap().unwrap();
    assert_eq!(current.name, "first_update.mp4", "Name should be from first update");
    assert_eq!(current.version, 1, "Version should still be 1");
}

/// Test: Concurrent updates should detect conflict
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_updates_detect_conflict() {
    let ctx = setup_test_context("v3").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    // Create a media item
    let media = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "concurrent.mp4", 0))
        .await
        .unwrap();

    // Simulate two clients reading the same data
    let client1_media = media.clone();
    let client2_media = media.clone();

    // Client 1 updates first with version 0 -> succeeds, version becomes 1
    let mut c1_update = client1_media.clone();
    c1_update.name = "client1_update.mp4".to_string();
    c1_update.source_config = json!({"url": "https://example.com/c1.mp4"});
    let c1_result = media_repo
        .update_with_version(&c1_update, 0)
        .await
        .unwrap()
        .expect("Client 1 update should succeed");
    assert_eq!(c1_result.version, 1);

    // Client 2 tries to update with same version 0 -> should fail
    // because version is now 1
    let mut c2_update = client2_media.clone();
    c2_update.name = "client2_update.mp4".to_string();
    c2_update.source_config = json!({"url": "https://example.com/c2.mp4"});
    let c2_result = media_repo
        .update_with_version(&c2_update, 0)  // Stale version!
        .await
        .unwrap();

    assert!(c2_result.is_none(), "Client 2 update should fail with version conflict");

    // Verify only client1's changes persisted
    let current = media_repo.get_by_id(&media.id).await.unwrap().unwrap();
    assert_eq!(current.name, "client1_update.mp4");
    assert_eq!(current.source_config["url"], "https://example.com/c1.mp4");
    assert_eq!(current.version, 1);
}

/// Test: Multiple sequential updates increment version correctly
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_sequential_updates_increment_version() {
    let ctx = setup_test_context("v4").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    // Create a media item
    let mut media = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "sequential.mp4", 0))
        .await
        .unwrap();
    assert_eq!(media.version, 0);

    // Update 1: version 0 -> 1
    media.name = "update1.mp4".to_string();
    media = media_repo
        .update_with_version(&media, 0)
        .await
        .unwrap()
        .expect("Update 1 should succeed");
    assert_eq!(media.version, 1);

    // Update 2: version 1 -> 2
    media.name = "update2.mp4".to_string();
    media = media_repo
        .update_with_version(&media, 1)
        .await
        .unwrap()
        .expect("Update 2 should succeed");
    assert_eq!(media.version, 2);

    // Update 3: version 2 -> 3
    media.name = "update3.mp4".to_string();
    media = media_repo
        .update_with_version(&media, 2)
        .await
        .unwrap()
        .expect("Update 3 should succeed");
    assert_eq!(media.version, 3);

    // Stale update with version 1 should fail (current is 3)
    let mut stale = media.clone();
    stale.name = "stale.mp4".to_string();
    let result = media_repo
        .update_with_version(&stale, 1)
        .await
        .unwrap();
    assert!(result.is_none(), "Stale update should fail");

    // Correct update with version 3 should succeed
    media.name = "update4.mp4".to_string();
    media = media_repo
        .update_with_version(&media, 3)
        .await
        .unwrap()
        .expect("Update 4 should succeed");
    assert_eq!(media.version, 4);
}

/// Test: update_with_version on source_config changes
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_source_config_with_version() {
    let ctx = setup_test_context("v5").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let media = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "config_test.mp4", 0))
        .await
        .unwrap();

    // Update source_config with correct version
    let mut updated = media.clone();
    updated.source_config = json!({
        "playback_infos": {
            "direct": {
                "urls": [{"name": "1080P", "url": "https://example.com/new.mp4"}]
            }
        },
        "default_mode": "direct"
    });

    let result = media_repo
        .update_with_version(&updated, 0)
        .await
        .unwrap()
        .expect("Source config update should succeed");

    assert_eq!(result.version, 1);
    assert!(result.source_config.get("playback_infos").is_some());

    // Concurrent update with old version should fail
    let mut stale = media.clone();
    stale.source_config = json!({"stale": true});
    let stale_result = media_repo
        .update_with_version(&stale, 0)
        .await
        .unwrap();
    assert!(stale_result.is_none(), "Stale source_config update should fail");
}

/// Test: Non-existent media returns None
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_with_version_nonexistent_media() {
    let ctx = setup_test_context("v6").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let fake_media = make_media(&ctx.root_playlist.id, &ctx.room.id, "nonexistent.mp4", 0);

    let result = media_repo
        .update_with_version(&fake_media, 0)
        .await
        .unwrap();

    assert!(result.is_none(), "Update on non-existent media should return None");
}

/// Test: Verify version is returned in all read operations
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_version_returned_in_read_operations() {
    let ctx = setup_test_context("v7").await;
    let pool = ctx.pool.clone();
    let media_repo = MediaRepository::new(pool);

    // Create and update a media item
    let mut media = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "read_ops.mp4", 0))
        .await
        .unwrap();
    assert_eq!(media.version, 0);

    // Update to version 1
    media.name = "read_ops_v1.mp4".to_string();
    media = media_repo
        .update_with_version(&media, 0)
        .await
        .unwrap()
        .expect("Update should succeed");

    // get_by_id should return correct version
    let by_id = media_repo.get_by_id(&media.id).await.unwrap().unwrap();
    assert_eq!(by_id.version, 1, "get_by_id should return version 1");

    // get_by_playlist should return correct version
    let by_playlist = media_repo.get_by_playlist(&ctx.root_playlist.id).await.unwrap();
    assert_eq!(by_playlist.len(), 1);
    assert_eq!(by_playlist[0].version, 1, "get_by_playlist should return version 1");

    // get_by_ids should return correct version
    let by_ids = media_repo.get_by_ids(&[media.id.clone()]).await.unwrap();
    assert_eq!(by_ids.len(), 1);
    assert_eq!(by_ids[0].version, 1, "get_by_ids should return version 1");
}
