use super::*;
use crate::models::id::{MediaId, PlaylistId, RoomId, UserId};
use crate::models::{FromProviderParams, ProviderInstance};
use crate::repository::ProviderInstanceRepository;
use crate::test_helpers::{TestOptionExt, TestResultExt};
use sqlx::Execute;
use synctv_core_testing::create_test_pool;

async fn insert_test_provider_instance(pool: &PgPool, name: &str, provider: &str) {
    let now = chrono::Utc::now();
    let instance = ProviderInstance {
        name: name.to_string(),
        endpoint: "http://localhost:50051".to_string(),
        comment: Some("test provider instance".to_string()),
        jwt_secret: None,
        custom_ca: None,
        timeout: "10s".to_string(),
        tls: false,
        insecure_tls: false,
        providers: vec![provider.to_string()],
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    ProviderInstanceRepository::new(pool.clone())
        .create(&instance)
        .await
        .checked("operation should succeed");
}

/// Unit test: Media builder pattern
#[test]
fn test_media_from_provider() {
    let playlist_id = PlaylistId::new();
    let room_id = RoomId::new();
    let creator_id = UserId::new();

    let media = Media::from_provider_with_params(FromProviderParams {
        playlist_id: Some(playlist_id),
        room_id,
        creator_id: Some(creator_id),
        name: "Test Video".to_string(),
        description: String::new(),
        source_config: serde_json::json!({"url": "https://example.com/video.mp4"}),
        provider_name: "direct_url".to_string(),
        provider_instance_name: None,
        position: 0.0,
    });

    assert_eq!(media.name, "Test Video");
    assert!((media.position - 0.0).abs() < f64::EPSILON);
    assert_eq!(media.source_provider, "direct_url");
}

#[test]
fn test_push_media_scope_filters_treats_empty_provider_instance_as_default() {
    let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT m.id FROM media m");
    let query = MediaListQuery {
        provider_instance_name: Some("   ".to_string()),
        ..MediaListQuery::default()
    };
    let room_id = RoomId::expect_positive(123_456_678);

    MediaRepository::push_media_scope_filters(&mut builder, &room_id, None, &query)
        .checked("operation should succeed");

    let built = builder.build();
    assert!(built
        .sql()
        .as_str()
        .contains("NULLIF(m.provider_instance_name, '') IS NULL"));
}

fn media_order_by_sql(query: &MediaListQuery) -> String {
    let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("");
    MediaRepository::push_media_list_order_by(&mut builder, query);
    builder.sql().as_str().to_string()
}

#[test]
fn test_push_media_list_order_by_uses_static_sort_branches() {
    use crate::models::{MediaListSortBy, SortDirection};

    let mut query = MediaListQuery {
        sort_by: MediaListSortBy::Name,
        sort_direction: SortDirection::Desc,
        ..MediaListQuery::default()
    };
    assert_eq!(
        media_order_by_sql(&query),
        " ORDER BY m.name DESC, m.position DESC, m.id DESC"
    );

    query.sort_by = MediaListSortBy::ProviderInstanceName;
    query.sort_direction = SortDirection::Asc;
    assert_eq!(
        media_order_by_sql(&query),
        " ORDER BY NULLIF(m.provider_instance_name, '') ASC, m.name ASC, m.id ASC"
    );

    query.sort_by = MediaListSortBy::Position;
    query.sort_direction = SortDirection::Asc;
    assert_eq!(
        media_order_by_sql(&query),
        " ORDER BY m.position ASC, m.name ASC, m.id ASC"
    );
}

/// Unit test: `Media::from_direct_single_mode`
#[test]
fn test_media_from_direct_single_mode() {
    let playlist_id = PlaylistId::new();
    let room_id = RoomId::new();
    let creator_id = UserId::new();

    let playback_info = crate::models::media::PlaybackInfo::single_url(
        "https://example.com/video.mp4".to_string(),
        "1080P".to_string(),
    );

    let media = Media::from_direct_single_mode(
        Some(playlist_id),
        room_id,
        Some(creator_id),
        "Single Mode Video".to_string(),
        "direct",
        playback_info,
        5.0,
    )
    .checked("direct media should build");

    assert_eq!(media.name, "Single Mode Video");
    assert!((media.position - 5.0).abs() < f64::EPSILON);
    assert!(media.provider_instance_name.is_none());
    assert_eq!(
        media.source_config["url"],
        serde_json::json!("https://example.com/video.mp4")
    );
    assert!(media.source_config.get("playback_infos").is_none());
}

/// Unit test: `Media::from_direct_multimode`
#[test]
fn test_media_from_direct_multimode() {
    let playlist_id = PlaylistId::new();
    let room_id = RoomId::new();

    let mut playback_infos = std::collections::HashMap::new();
    playback_infos.insert(
        "direct".to_string(),
        crate::models::media::PlaybackInfo::single_url(
            "https://example.com/video.mp4".to_string(),
            "1080P".to_string(),
        ),
    );
    playback_infos.insert(
        "proxied".to_string(),
        crate::models::media::PlaybackInfo::single_url(
            "https://proxy.example.com/video.mp4".to_string(),
            "720P".to_string(),
        ),
    );

    let media = Media::from_direct_multimode(crate::models::DirectMultimodeParams {
        playlist_id: Some(playlist_id),
        room_id,
        creator_id: None,
        name: "Multimode Video".to_string(),
        playback_infos,
        default_mode: "direct".to_string(),
        position: 10.0,
    })
    .checked("direct multimode media should build");

    assert_eq!(media.name, "Multimode Video");
    assert!((media.position - 10.0).abs() < f64::EPSILON);
    assert!(media.provider_instance_name.is_none());
    assert_eq!(
        media.source_config["url"],
        serde_json::json!("https://example.com/video.mp4")
    );
    assert!(media.source_config.get("playback_infos").is_none());
    assert!(media.source_config.get("metadata").is_none());
}

/// Integration test: Create and get media
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_and_get_media() {
    use crate::repository::playlist::PlaylistRepository;
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    // Create owner and room
    let owner = UserFixture::new().with_username("media_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Media Test Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create playlist hierarchy (root + child with name)
    let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
        &playlist_repo,
        room.id,
        "Test Playlist",
    )
    .await;

    // Create media
    let media = Media::from_provider_with_params(FromProviderParams {
        playlist_id: Some(playlist.id),
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Test Video".to_string(),
        description: String::new(),
        source_config: serde_json::json!({"url": "https://example.com/video.mp4"}),
        provider_name: "direct_url".to_string(),
        provider_instance_name: None,
        position: 0.0,
    });

    let created = media_repo
        .create(&media)
        .await
        .checked("operation should succeed");
    assert_eq!(created.name, "Test Video");
    assert!((created.position - 0.0).abs() < f64::EPSILON);

    // Get by ID
    let fetched = media_repo
        .get_by_id(&created.id)
        .await
        .checked("operation should succeed");
    assert!(fetched.is_some());
    let fetched = fetched.checked("operation should succeed");
    assert_eq!(fetched.name, "Test Video");
}

/// Integration test: Update media
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_media() {
    use crate::repository::playlist::PlaylistRepository;
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    // Setup
    let owner = UserFixture::new()
        .with_username("media_update_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Media Update Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create playlist hierarchy (root + child with name)
    let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
        &playlist_repo,
        room.id,
        "Test Playlist",
    )
    .await;

    let media = Media::from_provider_with_params(FromProviderParams {
        playlist_id: Some(playlist.id),
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Original Name".to_string(),
        description: String::new(),
        source_config: serde_json::json!({}),
        provider_name: "direct_url".to_string(),
        provider_instance_name: None,
        position: 0.0,
    });
    let created = media_repo
        .create(&media)
        .await
        .checked("operation should succeed");

    // Update
    let mut updated = created.clone();
    updated.name = "Updated Name".to_string();
    updated.position = 5.0;
    updated.source_config = serde_json::json!({"url": "https://example.com/changed.mp4"});
    updated.provider_instance_name = Some("changed-instance".to_string());

    let result = media_repo
        .update(&updated)
        .await
        .checked("operation should succeed");
    assert_eq!(result.name, "Updated Name");
    assert!((result.position - 5.0).abs() < f64::EPSILON);
    assert_eq!(result.source_config, created.source_config);
    assert_eq!(
        result.provider_instance_name,
        created.provider_instance_name
    );
}

/// Integration test: Delete media
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_media() {
    use crate::repository::playlist::PlaylistRepository;
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    // Setup
    let owner = UserFixture::new()
        .with_username("media_delete_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Media Delete Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create playlist hierarchy (root + child with name)
    let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
        &playlist_repo,
        room.id,
        "Test Playlist",
    )
    .await;

    let media = Media::from_provider_with_params(FromProviderParams {
        playlist_id: Some(playlist.id),
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "To Delete".to_string(),
        description: String::new(),
        source_config: serde_json::json!({}),
        provider_name: "direct_url".to_string(),
        provider_instance_name: None,
        position: 0.0,
    });
    let created = media_repo
        .create(&media)
        .await
        .checked("operation should succeed");

    // Delete
    let deleted = media_repo
        .delete(&created.id)
        .await
        .checked("operation should succeed");
    assert!(deleted);

    // Verify deleted
    let fetched = media_repo
        .get_by_id(&created.id)
        .await
        .checked("operation should succeed");
    assert!(fetched.is_none());

    // Delete non-existent returns false
    let deleted_again = media_repo
        .delete(&created.id)
        .await
        .checked("operation should succeed");
    assert!(!deleted_again);
}

/// Integration test: empty/default provider-instance filter must match rows
/// stored as NULL in the database.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_filtered_by_scope_matches_default_provider_instance_name() {
    use crate::models::{MediaListQuery, MediaListSortBy};
    use crate::repository::playlist::PlaylistRepository;
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let owner = user_repo
        .create(
            &UserFixture::new()
                .with_username("media_default_instance_owner")
                .build(),
        )
        .await
        .checked("operation should succeed");

    let room = room_repo
        .create(
            &RoomFixture::new()
                .with_name("Media Default Instance Room")
                .with_owner(owner.id)
                .build(),
        )
        .await
        .checked("operation should succeed");

    let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
        &playlist_repo,
        room.id,
        "Default Instance Playlist",
    )
    .await;

    let default_media = Media::from_provider_with_params(FromProviderParams {
        playlist_id: Some(playlist.id),
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Default Backend".to_string(),
        description: String::new(),
        source_config: serde_json::json!({"url": "https://example.com/default.mp4"}),
        provider_name: "direct_url".to_string(),
        provider_instance_name: None,
        position: 0.0,
    });
    let explicit_media = Media::from_provider_with_params(FromProviderParams {
        playlist_id: Some(playlist.id),
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Explicit Backend".to_string(),
        description: String::new(),
        source_config: serde_json::json!({"url": "https://example.com/explicit.mp4"}),
        provider_name: "direct_url".to_string(),
        provider_instance_name: Some("direct_url_remote".to_string()),
        position: 1.0,
    });

    insert_test_provider_instance(&pool, "direct_url_remote", "direct_url").await;
    let created_default = media_repo
        .create(&default_media)
        .await
        .checked("operation should succeed");
    media_repo
        .create(&explicit_media)
        .await
        .checked("operation should succeed");

    let query = MediaListQuery {
        provider_instance_name: Some(String::new()),
        sort_by: MediaListSortBy::Position,
        ..MediaListQuery::default()
    };

    let count = media_repo
        .count_filtered_by_scope(&room.id, Some(&playlist.id), &query)
        .await
        .checked("operation should succeed");
    assert_eq!(count, 1);

    let rows = media_repo
        .list_filtered_by_scope(&room.id, Some(&playlist.id), &query, 50, 0)
        .await
        .checked("operation should succeed");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].media.id, created_default.id);
    assert!(rows[0].media.provider_instance_name.is_none());
}

/// Integration test: Batch create media
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_batch() {
    use crate::repository::playlist::PlaylistRepository;
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    // Setup
    let owner = UserFixture::new().with_username("batch_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Batch Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create playlist hierarchy (root + child with name)
    let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
        &playlist_repo,
        room.id,
        "Batch Playlist",
    )
    .await;

    // Create batch
    let items: Vec<Media> = (0..5)
        .map(|i| {
            Media::from_provider_with_params(FromProviderParams {
                playlist_id: Some(playlist.id),
                room_id: room.id,
                creator_id: Some(owner.id),
                name: format!("Video {i}"),
                description: String::new(),
                source_config: serde_json::json!({"url": format!("https://example.com/{}.mp4", i)}),
                provider_name: "direct_url".to_string(),
                provider_instance_name: None,
                position: f64::from(i),
            })
        })
        .collect();

    let created = media_repo
        .create_batch(&items)
        .await
        .checked("operation should succeed");
    assert_eq!(created.len(), 5);

    // Verify all created
    let fetched = media_repo
        .get_by_playlist(&playlist.id)
        .await
        .checked("operation should succeed");
    assert_eq!(fetched.len(), 5);
}

#[test]
fn test_create_batch_chunk_too_large() {
    let err = MediaRepository::validate_create_batch_chunk_len(1001)
        .failed("oversized chunks should be rejected before building a query");

    match err {
        crate::Error::InvalidInput(message) => {
            assert!(
                message.contains("1000 row limit"),
                "unexpected message: {message}"
            );
            assert!(
                message.contains("12012 bind parameters"),
                "unexpected message: {message}"
            );
        }
        other => std::panic::panic_any(format!("expected invalid input error, got {other:?}")),
    }
}

/// Integration test: Move media within a scope using anchor-based ordering.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_move_with_tx_reorders_scope() {
    use crate::repository::playlist::PlaylistRepository;
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    // Setup
    let owner = UserFixture::new().with_username("swap_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Swap Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create playlist hierarchy (root + child with name)
    let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
        &playlist_repo,
        room.id,
        "Swap Playlist",
    )
    .await;

    // Create two media items
    let media1 = Media::from_provider_with_params(FromProviderParams {
        playlist_id: Some(playlist.id),
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Video 1".to_string(),
        description: String::new(),
        source_config: serde_json::json!({}),
        provider_name: "direct_url".to_string(),
        provider_instance_name: None,
        position: 1024.0,
    });
    let media2 = Media::from_provider_with_params(FromProviderParams {
        playlist_id: Some(playlist.id),
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Video 2".to_string(),
        description: String::new(),
        source_config: serde_json::json!({}),
        provider_name: "direct_url".to_string(),
        provider_instance_name: None,
        position: 2048.0,
    });

    let created1 = media_repo
        .create(&media1)
        .await
        .checked("operation should succeed");
    let created2 = media_repo
        .create(&media2)
        .await
        .checked("operation should succeed");

    assert!((created1.position - 1024.0).abs() < f64::EPSILON);
    assert!((created2.position - 2048.0).abs() < f64::EPSILON);

    let mut tx = pool.begin().await.checked("operation should succeed");
    media_repo
        .move_with_tx(&room.id, &created2.id, Some(&created1.id), None, &mut tx)
        .await
        .checked("operation should succeed");
    tx.commit().await.checked("operation should succeed");

    // Verify ordering changed and only the moved item crossed the anchor.
    let fetched1 = media_repo
        .get_by_id(&created1.id)
        .await
        .checked("operation should succeed")
        .checked("operation should succeed");
    let fetched2 = media_repo
        .get_by_id(&created2.id)
        .await
        .checked("operation should succeed")
        .checked("operation should succeed");

    assert!(fetched2.position < fetched1.position);
}

/// Integration test: Count by playlist
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_count_by_playlist() {
    use crate::repository::playlist::PlaylistRepository;
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    // Setup
    let owner = UserFixture::new().with_username("count_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Count Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create playlist hierarchy (root + child with name)
    let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
        &playlist_repo,
        room.id,
        "Count Playlist",
    )
    .await;

    // Initially empty
    let count = media_repo
        .count_by_playlist(&playlist.id)
        .await
        .checked("operation should succeed");
    assert_eq!(count, 0);

    // Add 3 items
    for i in 0..3 {
        let media = Media::from_provider_with_params(FromProviderParams {
            playlist_id: Some(playlist.id),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: format!("Video {i}"),
            description: String::new(),
            source_config: serde_json::json!({}),
            provider_name: "direct_url".to_string(),
            provider_instance_name: None,
            position: f64::from(i),
        });
        media_repo
            .create(&media)
            .await
            .checked("operation should succeed");
    }

    let count = media_repo
        .count_by_playlist(&playlist.id)
        .await
        .checked("operation should succeed");
    assert_eq!(count, 3);
}

/// Integration test: Get playlist paginated
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_playlist_paginated() {
    use crate::repository::playlist::PlaylistRepository;
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    // Setup
    let owner = UserFixture::new().with_username("paginate_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Paginate Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create playlist hierarchy (root + child with name)
    let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
        &playlist_repo,
        room.id,
        "Paginate Playlist",
    )
    .await;

    // Create 15 items
    for i in 0..15 {
        let media = Media::from_provider_with_params(FromProviderParams {
            playlist_id: Some(playlist.id),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: format!("Video {i}"),
            description: String::new(),
            source_config: serde_json::json!({}),
            provider_name: "direct_url".to_string(),
            provider_instance_name: None,
            position: f64::from(i),
        });
        media_repo
            .create(&media)
            .await
            .checked("operation should succeed");
    }

    // Page 1 (limit 10, offset 0)
    let page1 = PageParams::new(Some(1), Some(10));
    let (items, total) = media_repo
        .get_playlist_paginated(&playlist.id, page1)
        .await
        .checked("operation should succeed");
    assert_eq!(items.len(), 10);
    assert_eq!(total, 15);

    // Page 2 (limit 10, offset 10)
    let page2 = PageParams::new(Some(2), Some(10));
    let (items, total) = media_repo
        .get_playlist_paginated(&playlist.id, page2)
        .await
        .checked("operation should succeed");
    assert_eq!(items.len(), 5);
    assert_eq!(total, 15);
}

/// Integration test: Delete batch
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_batch() {
    use crate::repository::playlist::PlaylistRepository;
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    // Setup
    let owner = UserFixture::new()
        .with_username("batch_delete_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Batch Delete Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create playlist hierarchy (root + child with name)
    let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
        &playlist_repo,
        room.id,
        "Batch Delete Playlist",
    )
    .await;

    // Create 5 items
    let mut ids: Vec<MediaId> = Vec::new();
    for i in 0..5 {
        let media = Media::from_provider_with_params(FromProviderParams {
            playlist_id: Some(playlist.id),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: format!("Video {i}"),
            description: String::new(),
            source_config: serde_json::json!({}),
            provider_name: "direct_url".to_string(),
            provider_instance_name: None,
            position: f64::from(i),
        });
        let created = media_repo
            .create(&media)
            .await
            .checked("operation should succeed");
        ids.push(created.id);
    }

    // Delete 3 items
    let deleted = media_repo
        .delete_batch(&ids[0..3])
        .await
        .checked("operation should succeed");
    assert_eq!(deleted, 3);

    // Verify remaining
    let remaining = media_repo
        .get_by_playlist(&playlist.id)
        .await
        .checked("operation should succeed");
    assert_eq!(remaining.len(), 2);
}

/// Integration test: Get by IDs
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_ids() {
    use crate::repository::playlist::PlaylistRepository;
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    // Setup
    let owner = UserFixture::new().with_username("get_ids_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Get IDs Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create playlist hierarchy (root + child with name)
    let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
        &playlist_repo,
        room.id,
        "Get IDs Playlist",
    )
    .await;

    // Create 3 items
    let mut ids: Vec<MediaId> = Vec::new();
    for i in 0..3 {
        let media = Media::from_provider_with_params(FromProviderParams {
            playlist_id: Some(playlist.id),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: format!("Video {i}"),
            description: String::new(),
            source_config: serde_json::json!({}),
            provider_name: "direct_url".to_string(),
            provider_instance_name: None,
            position: f64::from(i),
        });
        let created = media_repo
            .create(&media)
            .await
            .checked("operation should succeed");
        ids.push(created.id);
    }

    // Get by IDs
    let fetched = media_repo
        .get_by_ids(&ids)
        .await
        .checked("operation should succeed");
    assert_eq!(fetched.len(), 3);

    // Get with non-existent ID
    let mut mixed_ids = ids.clone();
    mixed_ids.push(MediaId::new());
    let fetched = media_repo
        .get_by_ids(&mixed_ids)
        .await
        .checked("operation should succeed");
    assert_eq!(fetched.len(), 3); // Only existing ones returned

    // Empty IDs returns empty
    let fetched = media_repo
        .get_by_ids(&[])
        .await
        .checked("operation should succeed");
    assert!(fetched.is_empty());
}
