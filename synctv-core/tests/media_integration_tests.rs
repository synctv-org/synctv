//! Media CRUD integration tests
//!
//! Tests media item creation, unique constraints, deletion, and playlist association.
//!
//! Run with: cargo test --test media_integration_tests

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
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

/// Default PostgreSQL version for test containers
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

    let owner = user_repo.create(&make_user(&format!("media_owner_{}", suffix))).await.unwrap();
    let room = room_repo.create(&{
        let now = Utc::now();
        Room {
            id: RoomId::new(),
            name: format!("Media Room {}", suffix),
            description: String::new(),
            created_by: owner.id.clone(),
            status: RoomStatus::Active,
            is_banned: false,
            created_at: now,
            updated_at: now,
            deleted_at: None,
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
    }
}

#[tokio::test]
async fn test_create_media_basic() {
    let ctx = setup_test_context("1").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let media = make_media(&ctx.root_playlist.id, &ctx.room.id, "test_video.mp4", 0);
    let created = media_repo.create(&media).await.unwrap();

    assert_eq!(created.name, "test_video.mp4");
    assert_eq!(created.position, 0);
    assert_eq!(created.source_provider, "direct_url");
    assert_eq!(created.playlist_id, ctx.root_playlist.id);
    assert_eq!(created.room_id, ctx.room.id);
}

#[tokio::test]
async fn test_media_get_by_id() {
    let ctx = setup_test_context("2").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let media = make_media(&ctx.root_playlist.id, &ctx.room.id, "get_me.mp4", 0);
    let created = media_repo.create(&media).await.unwrap();

    let fetched = media_repo.get_by_id(&created.id).await.unwrap();
    assert!(fetched.is_some());
    let fetched = fetched.unwrap();
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.name, "get_me.mp4");
}

#[tokio::test]
async fn test_media_update() {
    let ctx = setup_test_context("3").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let media = make_media(&ctx.root_playlist.id, &ctx.room.id, "original.mp4", 0);
    let created = media_repo.create(&media).await.unwrap();

    let mut updated_media = created.clone();
    updated_media.name = "renamed.mp4".to_string();
    let updated = media_repo.update(&updated_media).await.unwrap();
    assert_eq!(updated.name, "renamed.mp4");
}

#[tokio::test]
async fn test_media_delete() {
    let ctx = setup_test_context("4").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let media = make_media(&ctx.root_playlist.id, &ctx.room.id, "delete_me.mp4", 0);
    let created = media_repo.create(&media).await.unwrap();

    let deleted = media_repo.delete(&created.id).await.unwrap();
    assert!(deleted);

    let fetched = media_repo.get_by_id(&created.id).await.unwrap();
    assert!(fetched.is_none());

    // Double delete returns false
    let deleted_again = media_repo.delete(&created.id).await.unwrap();
    assert!(!deleted_again);
}

#[tokio::test]
async fn test_unique_media_name_constraint() {
    let ctx = setup_test_context("5").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    media_repo.create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "same_name.mp4", 0)).await.unwrap();

    // Try to create another media with the same name in the same playlist
    let duplicate = make_media(&ctx.root_playlist.id, &ctx.room.id, "same_name.mp4", 1);
    let result = media_repo.create(&duplicate).await;
    assert!(result.is_err(), "Duplicate media name in same playlist should fail");
}

#[tokio::test]
async fn test_unique_media_position_constraint() {
    let ctx = setup_test_context("6").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    media_repo.create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "first.mp4", 0)).await.unwrap();

    // Try to create another media at the same position in the same playlist
    let duplicate = make_media(&ctx.root_playlist.id, &ctx.room.id, "second.mp4", 0);
    let result = media_repo.create(&duplicate).await;
    assert!(result.is_err(), "Duplicate position in same playlist should fail");
}

#[tokio::test]
async fn test_media_get_by_playlist() {
    let ctx = setup_test_context("7").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    media_repo.create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "a.mp4", 0)).await.unwrap();
    media_repo.create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "b.mp4", 1)).await.unwrap();
    media_repo.create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "c.mp4", 2)).await.unwrap();

    let items = media_repo.get_by_playlist(&ctx.root_playlist.id).await.unwrap();
    assert_eq!(items.len(), 3);
    // Should be ordered by position ASC
    assert_eq!(items[0].name, "a.mp4");
    assert_eq!(items[1].name, "b.mp4");
    assert_eq!(items[2].name, "c.mp4");
}

#[tokio::test]
async fn test_media_cascade_delete_with_playlist() {
    let ctx = setup_test_context("8").await;
    let playlist_repo = PlaylistRepository::new(ctx.pool.clone());
    let media_repo = MediaRepository::new(ctx.pool.clone());

    // Create a child playlist under root
    let child_playlist = playlist_repo.create(&Playlist {
        id: PlaylistId::new(),
        room_id: ctx.room.id.clone(),
        creator_id: None,
        name: "Child PL".to_string(),
        parent_id: Some(ctx.root_playlist.id.clone()),
        position: 0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }).await.unwrap();

    // Add media to child playlist
    let media = media_repo.create(&make_media(&child_playlist.id, &ctx.room.id, "cascade_test.mp4", 0)).await.unwrap();

    // Delete the child playlist - media should be cascade-deleted
    playlist_repo.delete(&child_playlist.id).await.unwrap();

    let fetched = media_repo.get_by_id(&media.id).await.unwrap();
    assert!(fetched.is_none(), "Media should be cascade-deleted when playlist is deleted");
}

#[tokio::test]
async fn test_media_swap_positions() {
    let ctx = setup_test_context("9").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let m1 = media_repo.create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "first.mp4", 0)).await.unwrap();
    let m2 = media_repo.create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "second.mp4", 1)).await.unwrap();

    media_repo.swap_positions(&m1.id, &m2.id).await.unwrap();

    let updated_m1 = media_repo.get_by_id(&m1.id).await.unwrap().unwrap();
    let updated_m2 = media_repo.get_by_id(&m2.id).await.unwrap().unwrap();

    assert_eq!(updated_m1.position, 1, "first should now be at position 1");
    assert_eq!(updated_m2.position, 0, "second should now be at position 0");
}

#[tokio::test]
async fn test_media_count_by_playlist() {
    let ctx = setup_test_context("10").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    assert_eq!(media_repo.count_by_playlist(&ctx.root_playlist.id).await.unwrap(), 0);

    media_repo.create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "a.mp4", 0)).await.unwrap();
    media_repo.create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "b.mp4", 1)).await.unwrap();

    assert_eq!(media_repo.count_by_playlist(&ctx.root_playlist.id).await.unwrap(), 2);
}

#[tokio::test]
async fn test_media_batch_delete() {
    let ctx = setup_test_context("11").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let m1 = media_repo.create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "a.mp4", 0)).await.unwrap();
    let m2 = media_repo.create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "b.mp4", 1)).await.unwrap();
    let m3 = media_repo.create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "c.mp4", 2)).await.unwrap();

    // Batch delete first two
    let deleted_count = media_repo.delete_batch(&[m1.id.clone(), m2.id.clone()]).await.unwrap();
    assert_eq!(deleted_count, 2);

    // Only m3 should remain
    assert!(media_repo.get_by_id(&m1.id).await.unwrap().is_none());
    assert!(media_repo.get_by_id(&m2.id).await.unwrap().is_none());
    assert!(media_repo.get_by_id(&m3.id).await.unwrap().is_some());
}

// ============================================================================
// Bug B1: get_next_position_with_tx tests
// ============================================================================

#[tokio::test]
async fn test_get_next_position_with_tx_empty_playlist_returns_zero() {
    let ctx = setup_test_context("next_pos_empty").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let mut tx = ctx.pool.begin().await.unwrap();
    let next_pos = media_repo
        .get_next_position_with_tx(&ctx.root_playlist.id, &mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(next_pos, 0, "Next position for an empty playlist should be 0");
}

#[tokio::test]
async fn test_get_next_position_with_tx_existing_items() {
    let ctx = setup_test_context("next_pos_existing").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "a.mp4", 0))
        .await
        .unwrap();
    media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "b.mp4", 1))
        .await
        .unwrap();
    media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "c.mp4", 5))
        .await
        .unwrap();

    let mut tx = ctx.pool.begin().await.unwrap();
    let next_pos = media_repo
        .get_next_position_with_tx(&ctx.root_playlist.id, &mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(next_pos, 6, "Next position should be max(position)+1 = 6");
}

// ============================================================================
// Bug B2: swap_positions_no_constraint_violation test
// ============================================================================

#[tokio::test]
async fn test_swap_positions_no_constraint_violation() {
    let ctx = setup_test_context("swap_fix").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let m1 = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "swap_a.mp4", 0))
        .await
        .unwrap();
    let m2 = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "swap_b.mp4", 1))
        .await
        .unwrap();

    // This should NOT trigger a unique constraint violation
    media_repo.swap_positions(&m1.id, &m2.id).await.unwrap();

    let updated_m1 = media_repo.get_by_id(&m1.id).await.unwrap().unwrap();
    let updated_m2 = media_repo.get_by_id(&m2.id).await.unwrap().unwrap();

    assert_eq!(updated_m1.position, 1, "m1 should now be at position 1");
    assert_eq!(updated_m2.position, 0, "m2 should now be at position 0");
}

// ============================================================================
// Optimistic locking: update_if_unchanged tests
// ============================================================================

#[tokio::test]
async fn test_update_if_unchanged_succeeds_when_row_unchanged() {
    let ctx = setup_test_context("opt_lock_ok").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let m = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "opt.mp4", 0))
        .await
        .unwrap();

    let old_name = m.name.clone();
    let old_position = m.position;

    let mut updated = m.clone();
    updated.name = "opt_renamed.mp4".to_string();

    let result = media_repo
        .update_if_unchanged(&updated, &old_name, old_position)
        .await
        .unwrap();

    assert!(result.is_some(), "Update should succeed when row is unchanged");
    let result = result.unwrap();
    assert_eq!(result.name, "opt_renamed.mp4");
}

#[tokio::test]
async fn test_update_if_unchanged_returns_none_when_changed_concurrently() {
    let ctx = setup_test_context("opt_lock_conflict").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let m = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "conflict.mp4", 0))
        .await
        .unwrap();

    let old_name = m.name.clone();
    let old_position = m.position;

    // Simulate a concurrent update
    let mut concurrent = m.clone();
    concurrent.name = "concurrently_changed.mp4".to_string();
    media_repo.update(&concurrent).await.unwrap();

    // Now try update_if_unchanged with the stale old_name/old_position
    let mut my_update = m.clone();
    my_update.name = "my_change.mp4".to_string();

    let result = media_repo
        .update_if_unchanged(&my_update, &old_name, old_position)
        .await
        .unwrap();

    assert!(
        result.is_none(),
        "Update should return None when row has been changed concurrently"
    );

    // Verify the concurrent change is still in place
    let current = media_repo.get_by_id(&m.id).await.unwrap().unwrap();
    assert_eq!(current.name, "concurrently_changed.mp4");
}

// ============================================================================
// reorder_batch tests
// ============================================================================

#[tokio::test]
async fn test_reorder_batch_with_tx_swaps_positions() {
    let ctx = setup_test_context("reorder_swap").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let m1 = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "reorder_a.mp4", 0))
        .await
        .unwrap();
    let m2 = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "reorder_b.mp4", 1))
        .await
        .unwrap();

    // Swap positions [0,1] -> [1,0]
    media_repo
        .reorder_batch(&[(m1.id.clone(), 1), (m2.id.clone(), 0)])
        .await
        .unwrap();

    let updated_m1 = media_repo.get_by_id(&m1.id).await.unwrap().unwrap();
    let updated_m2 = media_repo.get_by_id(&m2.id).await.unwrap().unwrap();

    assert_eq!(updated_m1.position, 1);
    assert_eq!(updated_m2.position, 0);
}

#[tokio::test]
async fn test_reorder_batch_with_tx_empty_list_noop() {
    let ctx = setup_test_context("reorder_empty").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    // Empty reorder should succeed as a no-op
    media_repo.reorder_batch(&[]).await.unwrap();
}

// ============================================================================
// create_batch_chunked test
// ============================================================================

#[tokio::test]
async fn test_create_batch_chunked_inserts_all() {
    let ctx = setup_test_context("batch_create").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let items: Vec<Media> = (0..5)
        .map(|i| make_media(&ctx.root_playlist.id, &ctx.room.id, &format!("batch_{}.mp4", i), i))
        .collect();

    let results = media_repo.create_batch(&items).await.unwrap();
    assert_eq!(results.len(), 5);

    let playlist_items = media_repo.get_by_playlist(&ctx.root_playlist.id).await.unwrap();
    assert_eq!(playlist_items.len(), 5);
    for (i, item) in playlist_items.iter().enumerate() {
        assert_eq!(item.position, i as i32);
    }
}

// ============================================================================
// count_by_playlists_batch tests
// ============================================================================

#[tokio::test]
async fn test_count_by_playlists_batch_multiple_playlists() {
    let ctx = setup_test_context("batch_count").await;
    let playlist_repo = PlaylistRepository::new(ctx.pool.clone());
    let media_repo = MediaRepository::new(ctx.pool.clone());

    // Create a second playlist
    let pl2 = playlist_repo
        .create(&Playlist {
            id: PlaylistId::new(),
            room_id: ctx.room.id.clone(),
            creator_id: None,
            name: "Second PL".to_string(),
            parent_id: Some(ctx.root_playlist.id.clone()),
            position: 0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
        .await
        .unwrap();

    // Add 3 items to root, 2 to pl2
    for i in 0..3 {
        media_repo
            .create(&make_media(
                &ctx.root_playlist.id,
                &ctx.room.id,
                &format!("root_{}.mp4", i),
                i,
            ))
            .await
            .unwrap();
    }
    for i in 0..2 {
        media_repo
            .create(&make_media(
                &pl2.id,
                &ctx.room.id,
                &format!("pl2_{}.mp4", i),
                i,
            ))
            .await
            .unwrap();
    }

    let counts = media_repo
        .count_by_playlists_batch(&[ctx.root_playlist.id.as_str(), pl2.id.as_str()])
        .await
        .unwrap();

    assert_eq!(
        counts.get(ctx.root_playlist.id.as_str().trim()),
        Some(&3),
        "Root playlist should have 3 items"
    );
    assert_eq!(
        counts.get(pl2.id.as_str().trim()),
        Some(&2),
        "Second playlist should have 2 items"
    );
}

#[tokio::test]
async fn test_count_by_playlists_batch_empty_input() {
    let ctx = setup_test_context("batch_count_empty").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let counts = media_repo.count_by_playlists_batch(&[]).await.unwrap();
    assert!(counts.is_empty(), "Empty input should return empty map");
}

// ============================================================================
// get_by_ids test
// ============================================================================

#[tokio::test]
async fn test_get_by_ids_partial_returns_subset() {
    let ctx = setup_test_context("get_by_ids").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let m1 = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "ids_a.mp4", 0))
        .await
        .unwrap();
    let m2 = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "ids_b.mp4", 1))
        .await
        .unwrap();
    let _m3 = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "ids_c.mp4", 2))
        .await
        .unwrap();

    // Ask for only m1 and m2 (not m3), plus a nonexistent ID
    let nonexistent = MediaId::new();
    let results = media_repo
        .get_by_ids(&[m1.id.clone(), m2.id.clone(), nonexistent])
        .await
        .unwrap();

    assert_eq!(results.len(), 2, "Should return only the 2 existing items");
    let result_ids: std::collections::HashSet<String> = results
        .iter()
        .map(|m| m.id.as_str().to_string())
        .collect();
    assert!(result_ids.contains(m1.id.as_str()));
    assert!(result_ids.contains(m2.id.as_str()));
}

// ============================================================================
// delete_by_playlist test
// ============================================================================

#[tokio::test]
async fn test_delete_by_playlist_removes_all() {
    let ctx = setup_test_context("del_by_pl").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "del_a.mp4", 0))
        .await
        .unwrap();
    media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "del_b.mp4", 1))
        .await
        .unwrap();
    media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "del_c.mp4", 2))
        .await
        .unwrap();

    assert_eq!(media_repo.count_by_playlist(&ctx.root_playlist.id).await.unwrap(), 3);

    let deleted = media_repo
        .delete_by_playlist(&ctx.root_playlist.id)
        .await
        .unwrap();
    assert_eq!(deleted, 3);

    assert_eq!(media_repo.count_by_playlist(&ctx.root_playlist.id).await.unwrap(), 0);
}
