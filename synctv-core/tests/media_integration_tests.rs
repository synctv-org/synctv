//! Media CRUD integration tests
//! Tests media item creation, unique constraints, deletion, and playlist association.
//!
//! Run with: cargo test --test `media_integration_tests`
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use synctv_core::{
    models::{
        Media, MediaId, Playlist, PlaylistId, Room, RoomId, RoomStatus, User, UserId, UserRole,
        UserStatus,
    },
    repository::{MediaRepository, PlaylistRepository, RoomRepository, UserRepository},
};
use synctv_core_testing::{create_test_pool, TestContainer};

fn assert_f64_eq(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < f64::EPSILON,
        "expected {expected}, got {actual}"
    );
}

fn usize_to_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
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

struct TestContext {
    #[allow(dead_code)]
    container: TestContainer,
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

    let owner = user_repo
        .create(&make_user(&format!("media_owner_{suffix}")))
        .await
        .unwrap();
    let room = room_repo
        .create(&{
            let now = Utc::now();
            Room {
                id: RoomId::new(),
                name: format!("Media Room {suffix}"),
                description: String::new(),
                created_by: owner.id.clone(),
                status: RoomStatus::Active,
                is_banned: false,
                created_at: now,
                updated_at: now,
                deleted_at: None,
                version: 0,
                last_activity_at: now,
            }
        })
        .await
        .unwrap();

    let root_playlist = playlist_repo
        .create(&Playlist {
            id: PlaylistId::new(),
            room_id: room.id.clone(),
            creator_id: Some(owner.id.clone()),
            name: "Top Level".to_string(),
            parent_id: None,
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();

    TestContext {
        container,
        pool,
        owner,
        room,
        root_playlist,
    }
}

fn make_media(playlist_id: &PlaylistId, room_id: &RoomId, name: &str, position: i32) -> Media {
    Media {
        id: MediaId::new(),
        playlist_id: Some(playlist_id.clone()),
        room_id: room_id.clone(),
        creator_id: None,
        name: name.to_string(),
        position: f64::from(position),
        source_provider: "direct_url".to_string(),
        source_config: json!({"url": "https://example.com/video.mp4"}),
        provider_instance_name: "direct_url".to_string(),
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    }
}

fn make_room_root_media(room_id: &RoomId, name: &str, position: i32) -> Media {
    Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id: room_id.clone(),
        creator_id: None,
        name: name.to_string(),
        position: f64::from(position),
        source_provider: "direct_url".to_string(),
        source_config: json!({"url": "https://example.com/video.mp4"}),
        provider_instance_name: "direct_url".to_string(),
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_media_basic() {
    let ctx = setup_test_context("1").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let media = make_media(&ctx.root_playlist.id, &ctx.room.id, "test_video.mp4", 0);
    let created = media_repo.create(&media).await.unwrap();

    assert_eq!(created.name, "test_video.mp4");
    assert_f64_eq(created.position, 0.0);
    assert_eq!(created.source_provider, "direct_url");
    assert_eq!(created.playlist_id, Some(ctx.root_playlist.id));
    assert_eq!(created.room_id, ctx.room.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
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
#[ignore = "Requires Docker"]
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
#[ignore = "Requires Docker"]
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
#[ignore = "Requires Docker"]
async fn test_duplicate_media_names_are_allowed_in_same_playlist() {
    let ctx = setup_test_context("5").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    media_repo
        .create(&make_media(
            &ctx.root_playlist.id,
            &ctx.room.id,
            "same_name.mp4",
            0,
        ))
        .await
        .unwrap();

    let duplicate = make_media(&ctx.root_playlist.id, &ctx.room.id, "same_name.mp4", 1);
    let created = media_repo.create(&duplicate).await.unwrap();
    assert_eq!(created.name, "same_name.mp4");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_duplicate_media_positions_are_allowed() {
    let ctx = setup_test_context("6").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    media_repo
        .create(&make_media(
            &ctx.root_playlist.id,
            &ctx.room.id,
            "first.mp4",
            0,
        ))
        .await
        .unwrap();

    // Duplicate floating positions are allowed; ordering falls back to the
    // secondary sort key and move operations rebalance only when needed.
    let duplicate = make_media(&ctx.root_playlist.id, &ctx.room.id, "second.mp4", 0);
    let result = media_repo.create(&duplicate).await.unwrap();
    let items = media_repo
        .get_by_playlist(&ctx.root_playlist.id)
        .await
        .unwrap();
    assert_eq!(result.name, "second.mp4");
    assert_eq!(items.len(), 2);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_media_get_by_playlist() {
    let ctx = setup_test_context("7").await;
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
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "c.mp4", 2))
        .await
        .unwrap();

    let items = media_repo
        .get_by_playlist(&ctx.root_playlist.id)
        .await
        .unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].name, "a.mp4");
    assert_eq!(items[1].name, "b.mp4");
    assert_eq!(items[2].name, "c.mp4");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_media_can_exist_at_room_root_without_playlist() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("media_root_owner"))
        .await
        .unwrap();
    let room = room_repo
        .create(&{
            let now = Utc::now();
            Room {
                id: RoomId::new(),
                name: "Media Root Room".to_string(),
                description: String::new(),
                created_by: owner.id.clone(),
                status: RoomStatus::Active,
                is_banned: false,
                created_at: now,
                updated_at: now,
                deleted_at: None,
                version: 0,
                last_activity_at: now,
            }
        })
        .await
        .unwrap();

    let media_id = MediaId::new();
    sqlx::query(
        "INSERT INTO media (
            id, playlist_id, room_id, creator_id, name, position,
            source_provider, source_config, provider_instance_name, added_at, updated_at, version
        ) VALUES ($1, NULL, $2, $3, $4, $5, $6, $7, $8, NOW(), NOW(), 0)",
    )
    .bind(media_id.as_str())
    .bind(room.id.as_str())
    .bind(owner.id.as_str())
    .bind("root-media.mp4")
    .bind(0.0)
    .bind("direct_url")
    .bind(json!({"url": "https://example.com/root.mp4"}))
    .bind("direct_url")
    .execute(&pool)
    .await
    .unwrap();

    let fetched = media_repo.get_by_id(&media_id).await.unwrap().unwrap();
    assert!(fetched.playlist_id.is_none());

    let items = media_repo.get_room_root(&room.id).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, media_id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_media_cascade_delete_with_playlist() {
    let ctx = setup_test_context("8").await;
    let playlist_repo = PlaylistRepository::new(ctx.pool.clone());
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let child_playlist = playlist_repo
        .create(&Playlist {
            id: PlaylistId::new(),
            room_id: ctx.room.id.clone(),
            creator_id: None,
            name: "Child PL".to_string(),
            parent_id: Some(ctx.root_playlist.id.clone()),
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();

    // Add media to child playlist
    let media = media_repo
        .create(&make_media(
            &child_playlist.id,
            &ctx.room.id,
            "cascade_test.mp4",
            0,
        ))
        .await
        .unwrap();

    // Delete the child playlist - nested media should be explicitly deleted too.
    playlist_repo.delete(&child_playlist.id).await.unwrap();

    let fetched = media_repo.get_by_id(&media.id).await.unwrap();
    assert!(
        fetched.is_none(),
        "Media should be deleted when its playlist subtree is deleted"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_media_move_with_tx_reorders_scope() {
    let ctx = setup_test_context("9").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let m1 = media_repo
        .create(&make_media(
            &ctx.root_playlist.id,
            &ctx.room.id,
            "first.mp4",
            1024,
        ))
        .await
        .unwrap();
    let m2 = media_repo
        .create(&make_media(
            &ctx.root_playlist.id,
            &ctx.room.id,
            "second.mp4",
            2048,
        ))
        .await
        .unwrap();

    let mut tx = ctx.pool.begin().await.unwrap();
    media_repo
        .move_with_tx(&m2.id, Some(&m1.id), None, &mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let updated_m1 = media_repo.get_by_id(&m1.id).await.unwrap().unwrap();
    let updated_m2 = media_repo.get_by_id(&m2.id).await.unwrap().unwrap();

    assert!(updated_m2.position < updated_m1.position);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_media_count_by_playlist() {
    let ctx = setup_test_context("10").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    assert_eq!(
        media_repo
            .count_by_playlist(&ctx.root_playlist.id)
            .await
            .unwrap(),
        0
    );

    media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "a.mp4", 0))
        .await
        .unwrap();
    media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "b.mp4", 1))
        .await
        .unwrap();

    assert_eq!(
        media_repo
            .count_by_playlist(&ctx.root_playlist.id)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_media_batch_delete() {
    let ctx = setup_test_context("11").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let m1 = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "a.mp4", 0))
        .await
        .unwrap();
    let m2 = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "b.mp4", 1))
        .await
        .unwrap();
    let m3 = media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "c.mp4", 2))
        .await
        .unwrap();

    // Batch delete first two
    let deleted_count = media_repo
        .delete_batch(&[m1.id.clone(), m2.id.clone()])
        .await
        .unwrap();
    assert_eq!(deleted_count, 2);

    // Only m3 should remain
    assert!(media_repo.get_by_id(&m1.id).await.unwrap().is_none());
    assert!(media_repo.get_by_id(&m2.id).await.unwrap().is_none());
    assert!(media_repo.get_by_id(&m3.id).await.unwrap().is_some());
}

// Append-position helper tests

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_next_append_position_with_tx_empty_playlist_returns_order_step() {
    let ctx = setup_test_context("next_pos_empty").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let mut tx = ctx.pool.begin().await.unwrap();
    let next_pos = media_repo
        .get_next_append_position_with_tx(&ctx.room.id, Some(&ctx.root_playlist.id), &mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_f64_eq(next_pos, 1024.0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_next_append_position_with_tx_existing_items() {
    let ctx = setup_test_context("next_pos_existing").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    media_repo
        .create(&make_media(&ctx.root_playlist.id, &ctx.room.id, "a.mp4", 0))
        .await
        .unwrap();
    media_repo
        .create(&make_media(
            &ctx.root_playlist.id,
            &ctx.room.id,
            "b.mp4",
            2048,
        ))
        .await
        .unwrap();
    media_repo
        .create(&make_media(
            &ctx.root_playlist.id,
            &ctx.room.id,
            "c.mp4",
            4096,
        ))
        .await
        .unwrap();

    let mut tx = ctx.pool.begin().await.unwrap();
    let next_pos = media_repo
        .get_next_append_position_with_tx(&ctx.room.id, Some(&ctx.root_playlist.id), &mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_f64_eq(next_pos, 5120.0);
}

// create_batch_chunked test

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_batch_chunked_inserts_all() {
    let ctx = setup_test_context("batch_create").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let items: Vec<Media> = (0..5)
        .map(|i| {
            make_media(
                &ctx.root_playlist.id,
                &ctx.room.id,
                &format!("batch_{i}.mp4"),
                i,
            )
        })
        .collect();

    let results = media_repo.create_batch(&items).await.unwrap();
    assert_eq!(results.len(), 5);

    let playlist_items = media_repo
        .get_by_playlist(&ctx.root_playlist.id)
        .await
        .unwrap();
    assert_eq!(playlist_items.len(), 5);
    for (i, item) in playlist_items.iter().enumerate() {
        assert_f64_eq(item.position, usize_to_f64(i));
    }
}

// count_by_playlists_batch tests

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_count_by_playlists_batch_multiple_playlists() {
    let ctx = setup_test_context("batch_count").await;
    let playlist_repo = PlaylistRepository::new(ctx.pool.clone());
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let pl2 = playlist_repo
        .create(&Playlist {
            id: PlaylistId::new(),
            room_id: ctx.room.id.clone(),
            creator_id: None,
            name: "Second PL".to_string(),
            parent_id: Some(ctx.root_playlist.id.clone()),
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();

    // Add 3 items to root, 2 to pl2
    for i in 0..3 {
        media_repo
            .create(&make_media(
                &ctx.root_playlist.id,
                &ctx.room.id,
                &format!("root_{i}.mp4"),
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
                &format!("pl2_{i}.mp4"),
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
#[ignore = "Requires Docker"]
async fn test_count_by_playlists_batch_empty_input() {
    let ctx = setup_test_context("batch_count_empty").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let counts = media_repo.count_by_playlists_batch(&[]).await.unwrap();
    assert!(counts.is_empty(), "Empty input should return empty map");
}

// get_by_ids test

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_ids_partial_returns_subset() {
    let ctx = setup_test_context("get_by_ids").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let m1 = media_repo
        .create(&make_media(
            &ctx.root_playlist.id,
            &ctx.room.id,
            "ids_a.mp4",
            0,
        ))
        .await
        .unwrap();
    let m2 = media_repo
        .create(&make_media(
            &ctx.root_playlist.id,
            &ctx.room.id,
            "ids_b.mp4",
            1,
        ))
        .await
        .unwrap();
    let _m3 = media_repo
        .create(&make_media(
            &ctx.root_playlist.id,
            &ctx.room.id,
            "ids_c.mp4",
            2,
        ))
        .await
        .unwrap();

    // Ask for only m1 and m2 (not m3), plus a nonexistent ID
    let nonexistent = MediaId::new();
    let results = media_repo
        .get_by_ids(&[m1.id.clone(), m2.id.clone(), nonexistent])
        .await
        .unwrap();

    assert_eq!(results.len(), 2, "Should return only the 2 existing items");
    let result_ids: std::collections::HashSet<String> =
        results.iter().map(|m| m.id.as_str().to_string()).collect();
    assert!(result_ids.contains(m1.id.as_str()));
    assert!(result_ids.contains(m2.id.as_str()));
}

// delete_by_playlist test

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_by_playlist_removes_all() {
    let ctx = setup_test_context("del_by_pl").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    media_repo
        .create(&make_media(
            &ctx.root_playlist.id,
            &ctx.room.id,
            "del_a.mp4",
            0,
        ))
        .await
        .unwrap();
    media_repo
        .create(&make_media(
            &ctx.root_playlist.id,
            &ctx.room.id,
            "del_b.mp4",
            1,
        ))
        .await
        .unwrap();
    media_repo
        .create(&make_media(
            &ctx.root_playlist.id,
            &ctx.room.id,
            "del_c.mp4",
            2,
        ))
        .await
        .unwrap();

    assert_eq!(
        media_repo
            .count_by_playlist(&ctx.root_playlist.id)
            .await
            .unwrap(),
        3
    );

    let deleted = media_repo
        .delete_playlist(&ctx.root_playlist.id)
        .await
        .unwrap();
    assert_eq!(deleted, 3);

    assert_eq!(
        media_repo
            .count_by_playlist(&ctx.root_playlist.id)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_room_root_only_removes_target_room_media() {
    let ctx = setup_test_context("del_root").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());
    let user_repo = UserRepository::new(ctx.pool.clone());
    let room_repo = RoomRepository::new(ctx.pool.clone());

    let other_owner = user_repo
        .create(&make_user("media_owner_del_root_other"))
        .await
        .unwrap();
    let other_room = room_repo
        .create(&{
            let now = Utc::now();
            Room {
                id: RoomId::new(),
                name: "Other root media room".to_string(),
                description: String::new(),
                created_by: other_owner.id.clone(),
                status: RoomStatus::Active,
                is_banned: false,
                created_at: now,
                updated_at: now,
                deleted_at: None,
                version: 0,
                last_activity_at: now,
            }
        })
        .await
        .unwrap();

    media_repo
        .create(&make_room_root_media(&ctx.room.id, "room-a-root-1.mp4", 0))
        .await
        .unwrap();
    media_repo
        .create(&make_room_root_media(&ctx.room.id, "room-a-root-2.mp4", 1))
        .await
        .unwrap();
    media_repo
        .create(&make_room_root_media(
            &other_room.id,
            "room-b-root-1.mp4",
            0,
        ))
        .await
        .unwrap();

    assert_eq!(media_repo.count_room_root(&ctx.room.id).await.unwrap(), 2);
    assert_eq!(media_repo.count_room_root(&other_room.id).await.unwrap(), 1);

    let deleted = media_repo.delete_room_root(&ctx.room.id).await.unwrap();
    assert_eq!(deleted, 2);

    assert_eq!(media_repo.count_room_root(&ctx.room.id).await.unwrap(), 0);
    assert_eq!(media_repo.count_room_root(&other_room.id).await.unwrap(), 1);
}

// Concurrent append-position assignment tests

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_add_to_empty_playlist_unique_positions() {
    let ctx = setup_test_context("concurrent_empty").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    let num_concurrent = 10;
    let mut handles = Vec::new();

    // Spawn 10 concurrent tasks, each adding media to the same empty playlist
    for i in 0..num_concurrent {
        let pool = ctx.pool.clone();
        let playlist_id = ctx.root_playlist.id.clone();
        let room_id = ctx.room.id.clone();
        let media_repo = media_repo.clone();

        let handle = tokio::spawn(async move {
            let mut tx = pool.begin().await.expect("Failed to begin transaction");

            let position = media_repo
                .get_next_append_position_with_tx(&room_id, Some(&playlist_id), &mut tx)
                .await
                .expect("Failed to get next position");

            let media = Media {
                id: MediaId::new(),
                playlist_id: Some(playlist_id.clone()),
                room_id: room_id.clone(),
                creator_id: None,
                name: format!("concurrent_{i}.mp4"),
                position,
                source_provider: "direct_url".to_string(),
                source_config: json!({"url": format!("https://example.com/video{}.mp4", i)}),
                provider_instance_name: "direct_url".to_string(),
                added_at: Utc::now(),
                updated_at: Utc::now(),
                version: 0,
            };

            let result = media_repo.create_with_executor(&media, &mut *tx).await;

            if let Err(ref e) = result {
                eprintln!("Task {i} failed to create media: {e:?}");
            }

            tx.commit().await.expect("Failed to commit transaction");

            result.map(|m| m.position)
        });

        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.expect("Task panicked"))
        .collect();

    // All tasks should succeed (no UNIQUE constraint violations)
    let successful_positions: Vec<f64> = results
        .into_iter()
        .filter_map(std::result::Result::ok)
        .collect();

    assert_eq!(
        successful_positions.len(),
        num_concurrent,
        "All {} concurrent adds should succeed, but only {} did",
        num_concurrent,
        successful_positions.len()
    );

    // All positions should be unique
    let mut sorted_positions = successful_positions.clone();
    sorted_positions.sort_by(f64::total_cmp);
    sorted_positions.dedup();
    assert_eq!(
        sorted_positions.len(),
        num_concurrent,
        "All positions should be unique, got duplicates: {successful_positions:?}"
    );

    let expected_positions: Vec<f64> = (1..=num_concurrent)
        .map(|n| usize_to_f64(n) * 1024.0)
        .collect();
    assert_eq!(
        sorted_positions.len(),
        expected_positions.len(),
        "Positions should follow sparse append ordering"
    );
    for (actual, expected) in sorted_positions.iter().zip(expected_positions) {
        assert_f64_eq(*actual, expected);
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_add_to_nonempty_playlist_unique_positions() {
    let ctx = setup_test_context("concurrent_nonempty").await;
    let media_repo = MediaRepository::new(ctx.pool.clone());

    // Pre-populate with 5 existing items
    for i in 0..5 {
        media_repo
            .create(&make_media(
                &ctx.root_playlist.id,
                &ctx.room.id,
                &format!("existing_{i}.mp4"),
                i,
            ))
            .await
            .unwrap();
    }

    let num_concurrent = 5;
    let mut handles = Vec::new();

    // Spawn 5 concurrent tasks adding more media
    for i in 0..num_concurrent {
        let pool = ctx.pool.clone();
        let playlist_id = ctx.root_playlist.id.clone();
        let room_id = ctx.room.id.clone();
        let media_repo = media_repo.clone();

        let handle = tokio::spawn(async move {
            let mut tx = pool.begin().await.expect("Failed to begin transaction");

            let position = media_repo
                .get_next_append_position_with_tx(&room_id, Some(&playlist_id), &mut tx)
                .await
                .expect("Failed to get next position");

            let media = Media {
                id: MediaId::new(),
                playlist_id: Some(playlist_id.clone()),
                room_id: room_id.clone(),
                creator_id: None,
                name: format!("new_{i}.mp4"),
                position,
                source_provider: "direct_url".to_string(),
                source_config: json!({"url": format!("https://example.com/new{}.mp4", i)}),
                provider_instance_name: "direct_url".to_string(),
                added_at: Utc::now(),
                updated_at: Utc::now(),
                version: 0,
            };

            let result = media_repo.create_with_executor(&media, &mut *tx).await;
            tx.commit().await.expect("Failed to commit transaction");

            result.map(|m| m.position)
        });

        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.expect("Task panicked"))
        .collect();

    let successful_positions: Vec<f64> = results
        .into_iter()
        .filter_map(std::result::Result::ok)
        .collect();

    assert_eq!(
        successful_positions.len(),
        num_concurrent,
        "All {num_concurrent} concurrent adds should succeed"
    );

    // All new positions should be unique and greater than the existing max.
    let mut sorted_positions = successful_positions;
    sorted_positions.sort_by(f64::total_cmp);
    sorted_positions.dedup();
    assert_eq!(sorted_positions.len(), num_concurrent);

    assert!(
        sorted_positions.iter().all(|&pos| pos > 4.0),
        "All new positions should append after the existing max position"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_media_rejects_cross_room_playlist_reference() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("cross_room_media_owner"))
        .await
        .unwrap();

    let room_a = room_repo
        .create(&{
            let now = Utc::now();
            Room {
                id: RoomId::new(),
                name: "Cross Room Media A".to_string(),
                description: String::new(),
                created_by: owner.id.clone(),
                status: RoomStatus::Active,
                is_banned: false,
                created_at: now,
                updated_at: now,
                deleted_at: None,
                version: 0,
                last_activity_at: now,
            }
        })
        .await
        .unwrap();
    let room_b = room_repo
        .create(&{
            let now = Utc::now();
            Room {
                id: RoomId::new(),
                name: "Cross Room Media B".to_string(),
                description: String::new(),
                created_by: owner.id.clone(),
                status: RoomStatus::Active,
                is_banned: false,
                created_at: now,
                updated_at: now,
                deleted_at: None,
                version: 0,
                last_activity_at: now,
            }
        })
        .await
        .unwrap();

    let playlist_b = playlist_repo
        .create(&Playlist {
            id: PlaylistId::new(),
            room_id: room_b.id.clone(),
            creator_id: Some(owner.id.clone()),
            name: "Room B Playlist".to_string(),
            parent_id: None,
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();

    let result = media_repo
        .create(&Media {
            id: MediaId::new(),
            playlist_id: Some(playlist_b.id.clone()),
            room_id: room_a.id.clone(),
            creator_id: None,
            name: "cross-room.mp4".to_string(),
            position: 0.0,
            source_provider: "direct_url".to_string(),
            source_config: json!({"url": "https://example.com/cross-room.mp4"}),
            provider_instance_name: "direct_url".to_string(),
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await;

    assert!(
        result.is_err(),
        "media must not be insertable when playlist_id belongs to a different room"
    );
}
