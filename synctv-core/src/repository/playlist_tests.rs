use super::*;
use crate::models::{
    AlistPlaylistSourceConfig, PlaylistSourceConfig, ProviderInstance, SourceProvider,
};
use crate::repository::ProviderInstanceRepository;
use crate::test_helpers::{TestOptionExt, TestResultExt};
use sqlx::Execute;
use synctv_core_testing::create_test_pool;

fn alist_playlist_source_config(path: impl Into<String>) -> PlaylistSourceConfig {
    PlaylistSourceConfig::Alist(AlistPlaylistSourceConfig {
        server_id: "alist-server".to_string(),
        path: path.into(),
        password: None,
    })
}

async fn insert_test_provider_instance(pool: &PgPool, name: &str, provider: &str) {
    let now = crate::SystemClock.now();
    let instance = ProviderInstance {
        name: name.to_string(),
        endpoint: "http://localhost:50051".to_string(),
        comment: Some("test provider instance".to_string()),
        jwt_secret: None,
        custom_ca: None,
        timeout: "10s".to_string(),
        tls: false,
        insecure_tls: false,
        providers: vec![provider
            .parse::<SourceProvider>()
            .checked("test provider should be known")],
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    ProviderInstanceRepository::new(pool.clone())
        .create(&instance)
        .await
        .checked("operation should succeed");
}

fn assert_position_eq(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < f64::EPSILON,
        "expected position {expected}, got {actual}"
    );
}

fn playlist_order_by_sql(query: &PlaylistListQuery) -> String {
    let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("");
    PlaylistRepository::push_playlist_list_order_by(&mut builder, query);
    builder.sql().as_str().to_string()
}

#[test]
fn test_push_playlist_list_order_by_uses_static_sort_branches() {
    use crate::models::{PlaylistListSortBy, SortDirection};

    let mut query = PlaylistListQuery {
        sort_by: PlaylistListSortBy::Name,
        sort_direction: SortDirection::Desc,
        ..PlaylistListQuery::default()
    };
    assert_eq!(
        playlist_order_by_sql(&query),
        " ORDER BY p.name DESC, p.position DESC, p.id DESC"
    );

    query.sort_by = PlaylistListSortBy::CreatedAt;
    query.sort_direction = SortDirection::Asc;
    assert_eq!(
        playlist_order_by_sql(&query),
        " ORDER BY p.created_at ASC, p.position ASC, p.id ASC"
    );

    query.sort_by = PlaylistListSortBy::Position;
    query.sort_direction = SortDirection::Asc;
    assert_eq!(
        playlist_order_by_sql(&query),
        " ORDER BY p.position ASC, p.name ASC, p.id ASC"
    );
}

#[test]
fn test_advisory_lock_key_deterministic() {
    let room_id = RoomId::expect_positive(80_001);
    let parent_id = PlaylistId::expect_positive(80_002);
    let key1 = PlaylistRepository::scope_lock_key(&room_id, Some(&parent_id));
    let key2 = PlaylistRepository::scope_lock_key(&room_id, Some(&parent_id));
    assert_eq!(key1, key2, "Lock key should be deterministic");
}

#[test]
fn test_advisory_lock_key_different() {
    let room1 = RoomId::expect_positive(80_003);
    let room2 = RoomId::expect_positive(80_004);
    let parent1 = PlaylistId::expect_positive(80_005);
    let parent2 = PlaylistId::expect_positive(80_006);
    let key_room1_parent1 = PlaylistRepository::scope_lock_key(&room1, Some(&parent1));
    let key_room1_parent2 = PlaylistRepository::scope_lock_key(&room1, Some(&parent2));
    let key_room2_parent1 = PlaylistRepository::scope_lock_key(&room2, Some(&parent1));
    let key_room2_none = PlaylistRepository::scope_lock_key(&room2, None);

    assert_ne!(key_room1_parent1, key_room2_parent1);
    assert_ne!(key_room1_parent1, key_room1_parent2);
    assert_ne!(key_room2_parent1, key_room2_none);
}

#[test]
fn test_advisory_lock_key_range() {
    let test_ids = [1, 42, 80_007, i64::from(i32::MAX), i64::MAX / 2];

    for id in test_ids {
        let room_id = RoomId::expect_positive(id);
        let parent_id = PlaylistId::expect_positive(id);
        let key = PlaylistRepository::scope_lock_key(&room_id, Some(&parent_id));
        assert!(key >= 0, "Lock key should be non-negative for id: {id}");
    }
}

#[test]
fn test_push_playlist_scope_filters_treats_empty_provider_instance_as_default() {
    let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT p.id FROM playlists p");
    let query = PlaylistListQuery {
        provider_instance_name: Some("   ".to_string()),
        ..PlaylistListQuery::default()
    };
    let room_id = RoomId::expect_positive(80_008);

    PlaylistRepository::push_playlist_scope_filters(&mut builder, &room_id, None, &query)
        .checked("operation should succeed");

    let built = builder.build();
    assert!(built
        .sql()
        .as_str()
        .contains("NULLIF(p.provider_instance_name, '') IS NULL"));
}

/// Integration test: Create and get playlist by ID
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_and_get_by_id() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    // Create owner and room
    let owner = UserFixture::new().with_username("playlist_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Playlist Test Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create top-level playlist
    let playlist = PlaylistFixture::new()
        .with_room_id(room.id)
        .with_name("Top Level")
        .build();
    let created = playlist_repo
        .create(&playlist)
        .await
        .checked("operation should succeed");

    assert!(created.is_top_level());
    assert_position_eq(created.position, 0.0);

    // Get by ID
    let fetched = playlist_repo
        .get_by_id(&created.id)
        .await
        .checked("operation should succeed");
    assert!(fetched.is_some());
    let fetched = fetched.checked("operation should succeed");
    assert_eq!(fetched.id, created.id);
    assert!(fetched.is_top_level());
}

/// Integration test: Get top-level playlists for a room
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_top_level_playlists() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = UserFixture::new()
        .with_username("top_level_playlist_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Top Level Playlist Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    let top_level = PlaylistFixture::new()
        .with_room_id(room.id)
        .with_name("Top Level")
        .build();
    let created = playlist_repo
        .create(&top_level)
        .await
        .checked("operation should succeed");

    let fetched = playlist_repo
        .get_top_level(&room.id)
        .await
        .checked("operation should succeed");
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].id, created.id);
    assert!(fetched[0].is_top_level());
}

/// Integration test: blank provider instance name is stored as the default binding.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_normalizes_blank_provider_instance_name_to_default_binding() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = UserFixture::new()
        .with_username("playlist_default_provider_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Default Provider Playlist Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    let mut playlist = PlaylistFixture::new()
        .with_room_id(room.id)
        .with_name("Dynamic Default Provider")
        .build();
    playlist.source_provider = Some(SourceProvider::Alist);
    playlist.source_config = Some(alist_playlist_source_config("/movies"));
    playlist.provider_instance_name = Some("   ".to_string());

    let created = playlist_repo
        .create(&playlist)
        .await
        .checked("operation should succeed");
    assert!(created.provider_instance_name.is_none());

    let stored = sqlx::query_scalar!(
        "SELECT provider_instance_name FROM playlists WHERE id = $1",
        created.id.as_i64()
    )
    .fetch_one(&pool)
    .await
    .checked("operation should succeed");
    assert!(stored.is_none());

    let fetched = playlist_repo
        .get_by_id(&created.id)
        .await
        .checked("operation should succeed")
        .checked("operation should succeed");
    assert!(fetched.provider_instance_name.is_none());
}

/// Integration test: empty provider-instance filter matches default dynamic playlists.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_filtered_by_parent_matches_default_provider_instance_name() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = UserFixture::new()
        .with_username("playlist_default_provider_filter_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Default Provider Filter Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    let mut default_provider_playlist = PlaylistFixture::new()
        .with_room_id(room.id)
        .with_name("Default Provider Playlist")
        .with_creator(owner.id)
        .build();
    default_provider_playlist.source_provider = Some(SourceProvider::Alist);
    default_provider_playlist.source_config = Some(alist_playlist_source_config("/default"));
    default_provider_playlist.provider_instance_name = None;
    let default_provider_playlist = playlist_repo
        .create(&default_provider_playlist)
        .await
        .checked("operation should succeed");

    let mut explicit_provider_playlist = PlaylistFixture::new()
        .with_room_id(room.id)
        .with_name("Explicit Provider Playlist")
        .with_creator(owner.id)
        .build();
    explicit_provider_playlist.source_provider = Some(SourceProvider::Alist);
    explicit_provider_playlist.source_config = Some(alist_playlist_source_config("/explicit"));
    explicit_provider_playlist.provider_instance_name = Some("alist_home".to_string());
    insert_test_provider_instance(&pool, "alist_home", "alist").await;
    let _explicit_provider_playlist = playlist_repo
        .create(&explicit_provider_playlist)
        .await
        .checked("operation should succeed");

    let query = PlaylistListQuery {
        source_provider: Some(SourceProvider::Alist),
        provider_instance_name: Some(String::new()),
        dynamic_only: Some(true),
        ..PlaylistListQuery::default()
    };

    let total = playlist_repo
        .count_filtered_by_parent(&room.id, None, &query)
        .await
        .checked("operation should succeed");
    assert_eq!(total, 1);

    let rows = playlist_repo
        .list_filtered_by_parent(&room.id, None, &query, 50, 0)
        .await
        .checked("operation should succeed");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].playlist.id, default_provider_playlist.id);
    assert!(rows[0].playlist.provider_instance_name.is_none());
}

/// Integration test: Get playlists by room
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_room() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = UserFixture::new()
        .with_username("room_playlist_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Room Playlist Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create top-level playlist
    let root = PlaylistFixture::new().with_room_id(room.id).build();
    let created_root = playlist_repo
        .create(&root)
        .await
        .checked("operation should succeed");

    // Create child playlists
    let child1 = PlaylistFixture::new_child(created_root.id)
        .with_room_id(room.id)
        .with_name("Child 1")
        .build();
    let created_child1 = playlist_repo
        .create(&child1)
        .await
        .checked("operation should succeed");

    let child2 = PlaylistFixture::new_child(created_root.id)
        .with_room_id(room.id)
        .with_name("Child 2")
        .build();
    let created_child2 = playlist_repo
        .create(&child2)
        .await
        .checked("operation should succeed");

    // Get all playlists for room
    let playlists = playlist_repo
        .get_by_room(&room.id)
        .await
        .checked("operation should succeed");
    assert_eq!(playlists.len(), 3);

    // Verify root comes first (NULLS FIRST in ORDER BY)
    assert!(playlists[0].is_top_level());
    assert_eq!(playlists[0].id, created_root.id);

    // Children should be sorted by position
    let child_ids: Vec<_> = playlists[1..].iter().map(|p| p.id).collect();
    assert!(child_ids.contains(&created_child1.id));
    assert!(child_ids.contains(&created_child2.id));
}

/// Integration test: Update playlist with optimistic locking
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_with_current_version() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = UserFixture::new()
        .with_username("update_playlist_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Update Playlist Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create root and child
    let root = PlaylistFixture::new().with_room_id(room.id).build();
    let created_root = playlist_repo
        .create(&root)
        .await
        .checked("operation should succeed");

    let child = PlaylistFixture::new_child(created_root.id)
        .with_room_id(room.id)
        .with_name("Original Name")
        .build();
    let created = playlist_repo
        .create(&child)
        .await
        .checked("operation should succeed");

    // Update playlist
    let mut updated = created.clone();
    updated.name = "Updated Name".to_string();
    updated.position = 5.0;
    updated.source_provider = Some(SourceProvider::Alist);
    updated.source_config = Some(alist_playlist_source_config("/changed"));
    updated.provider_instance_name = Some("changed-instance".to_string());

    let result = playlist_repo
        .update_with_version(&updated, created.version)
        .await
        .checked("operation should succeed");
    assert_eq!(result.name, "Updated Name");
    assert_position_eq(result.position, 5.0);
    assert_eq!(result.source_provider, created.source_provider);
    assert_eq!(result.source_config, created.source_config);
    assert_eq!(
        result.provider_instance_name,
        created.provider_instance_name
    );
    assert!(result.version > created.version); // Version should increment
}

/// Integration test: Update with version (optimistic locking)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_with_version() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = UserFixture::new()
        .with_username("version_playlist_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Version Playlist Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create root and child
    let root = PlaylistFixture::new().with_room_id(room.id).build();
    let created_root = playlist_repo
        .create(&root)
        .await
        .checked("operation should succeed");

    let child = PlaylistFixture::new_child(created_root.id)
        .with_room_id(room.id)
        .with_name("Test Playlist")
        .build();
    let created = playlist_repo
        .create(&child)
        .await
        .checked("operation should succeed");
    let original_version = created.version;

    // Update with correct version
    let mut updated = created.clone();
    updated.name = "Updated".to_string();
    let result = playlist_repo
        .update_with_version(&updated, original_version)
        .await
        .checked("operation should succeed");
    assert_eq!(result.name, "Updated");

    // Update with stale version should fail
    let mut stale = created.clone();
    stale.name = "Stale Update".to_string();
    let result = playlist_repo
        .update_with_version(&stale, original_version) // Old version
        .await;
    assert!(result.is_err());
    assert!(matches!(
        result.failed("operation should fail"),
        crate::Error::OptimisticLockConflict
    ));
}

/// Integration test: Append helper returns sparse floating positions.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_next_append_position_with_tx() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = UserFixture::new()
        .with_username("position_playlist_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Position Playlist Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create root
    let root = PlaylistFixture::new().with_room_id(room.id).build();
    let created_root = playlist_repo
        .create(&root)
        .await
        .checked("operation should succeed");

    let mut tx = pool.begin().await.checked("operation should succeed");
    let next_pos = playlist_repo
        .get_next_append_position_with_tx(&room.id, Some(&created_root.id), &mut tx)
        .await
        .checked("operation should succeed");
    assert_position_eq(next_pos, 1024.0);

    // Create children with explicit positions
    for i in 0..3 {
        let child = PlaylistFixture::new_child(created_root.id)
            .with_room_id(room.id)
            .with_name(&format!("Child {i}"))
            .with_position((i + 1) * 1024)
            .build();
        playlist_repo
            .create_with_executor(&child, &mut *tx)
            .await
            .checked("operation should succeed");
    }

    // Next append position should continue the sparse sequence.
    let next_pos = playlist_repo
        .get_next_append_position_with_tx(&room.id, Some(&created_root.id), &mut tx)
        .await
        .checked("operation should succeed");
    assert_position_eq(next_pos, 4096.0);
    tx.commit().await.checked("operation should succeed");
}

/// Integration test: Get children
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_children() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = UserFixture::new()
        .with_username("children_playlist_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Children Playlist Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create root
    let root = PlaylistFixture::new().with_room_id(room.id).build();
    let created_root = playlist_repo
        .create(&root)
        .await
        .checked("operation should succeed");

    // Create 3 children
    for i in 0..3 {
        let child = PlaylistFixture::new_child(created_root.id)
            .with_room_id(room.id)
            .with_name(&format!("Child {i}"))
            .with_position(i)
            .build();
        playlist_repo
            .create(&child)
            .await
            .checked("operation should succeed");
    }

    // Get children
    let children = playlist_repo
        .get_children(&created_root.id)
        .await
        .checked("operation should succeed");
    assert_eq!(children.len(), 3);

    // Should be sorted by position
    let mut expected_position = 0.0;
    for child in &children {
        assert_position_eq(child.position, expected_position);
        expected_position += 1.0;
    }
}

/// Integration test: Get children paginated
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_children_paginated() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = UserFixture::new().with_username("paginated_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Paginated Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create root
    let root = PlaylistFixture::new().with_room_id(room.id).build();
    let created_root = playlist_repo
        .create(&root)
        .await
        .checked("operation should succeed");

    // Create 15 children
    for i in 0..15 {
        let child = PlaylistFixture::new_child(created_root.id)
            .with_room_id(room.id)
            .with_name(&format!("Child {i}"))
            .with_position(i)
            .build();
        playlist_repo
            .create(&child)
            .await
            .checked("operation should succeed");
    }

    // Page 1 (limit 10, offset 0)
    let page1 = playlist_repo
        .get_children_paginated(&created_root.id, 10, 0)
        .await
        .checked("operation should succeed");
    assert_eq!(page1.len(), 10);
    assert_eq!(page1[0].name, "Child 0");

    // Page 2 (limit 10, offset 10)
    let page2 = playlist_repo
        .get_children_paginated(&created_root.id, 10, 10)
        .await
        .checked("operation should succeed");
    assert_eq!(page2.len(), 5);
    assert_eq!(page2[0].name, "Child 10");
}

/// Integration test: Count children
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_count_children() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = UserFixture::new()
        .with_username("count_children_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Count Children Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create root
    let root = PlaylistFixture::new().with_room_id(room.id).build();
    let created_root = playlist_repo
        .create(&root)
        .await
        .checked("operation should succeed");

    // Initially 0 children
    let count = playlist_repo
        .count_children(&created_root.id)
        .await
        .checked("operation should succeed");
    assert_eq!(count, 0);

    // Create 5 children
    for i in 0..5 {
        let child = PlaylistFixture::new_child(created_root.id)
            .with_room_id(room.id)
            .with_name(&format!("Child {i}"))
            .build();
        playlist_repo
            .create(&child)
            .await
            .checked("operation should succeed");
    }

    let count = playlist_repo
        .count_children(&created_root.id)
        .await
        .checked("operation should succeed");
    assert_eq!(count, 5);
}

/// Integration test: Count by room
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_count_by_room() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = UserFixture::new().with_username("count_room_owner").build();
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

    // Create root
    let root = PlaylistFixture::new().with_room_id(room.id).build();
    let created_root = playlist_repo
        .create(&root)
        .await
        .checked("operation should succeed");

    // Initially 1 (just root)
    let count = playlist_repo
        .count_by_room(&room.id)
        .await
        .checked("operation should succeed");
    assert_eq!(count, 1);

    // Create children
    for i in 0..3 {
        let child = PlaylistFixture::new_child(created_root.id)
            .with_room_id(room.id)
            .with_name(&format!("Child {i}"))
            .build();
        playlist_repo
            .create(&child)
            .await
            .checked("operation should succeed");
    }

    let count = playlist_repo
        .count_by_room(&room.id)
        .await
        .checked("operation should succeed");
    assert_eq!(count, 4); // root + 3 children
}

/// Integration test: Get path (breadcrumb)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_path() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = UserFixture::new()
        .with_username("path_playlist_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Path Playlist Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create root -> child -> grandchild
    let root = PlaylistFixture::new().with_room_id(room.id).build();
    let created_root = playlist_repo
        .create(&root)
        .await
        .checked("operation should succeed");

    let child = PlaylistFixture::new_child(created_root.id)
        .with_room_id(room.id)
        .with_name("Child")
        .build();
    let created_child = playlist_repo
        .create(&child)
        .await
        .checked("operation should succeed");

    let grandchild = PlaylistFixture::new_child(created_child.id)
        .with_room_id(room.id)
        .with_name("Grandchild")
        .build();
    let created_grandchild = playlist_repo
        .create(&grandchild)
        .await
        .checked("operation should succeed");

    // Get path from grandchild
    let path = playlist_repo
        .get_path(&created_grandchild.id)
        .await
        .checked("operation should succeed");

    assert_eq!(path.len(), 3);
    // Should be ordered from root to leaf
    assert!(path[0].is_top_level());
    assert_eq!(path[1].id, created_child.id);
    assert_eq!(path[2].id, created_grandchild.id);
}

/// Integration test: Get by room paginated
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_room_paginated() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = UserFixture::new()
        .with_username("room_paginated_owner")
        .build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Room Paginated Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create root
    let root = PlaylistFixture::new().with_room_id(room.id).build();
    let created_root = playlist_repo
        .create(&root)
        .await
        .checked("operation should succeed");

    // Create 15 children
    for i in 0..15 {
        let child = PlaylistFixture::new_child(created_root.id)
            .with_room_id(room.id)
            .with_name(&format!("Child {i}"))
            .with_position(i)
            .build();
        playlist_repo
            .create(&child)
            .await
            .checked("operation should succeed");
    }

    // Total 16 playlists (root + 15 children)
    let count = playlist_repo
        .count_by_room(&room.id)
        .await
        .checked("operation should succeed");
    assert_eq!(count, 16);

    // Page 1 (limit 10, offset 0)
    let page1 = playlist_repo
        .get_by_room_paginated(&room.id, 10, 0)
        .await
        .checked("operation should succeed");
    assert_eq!(page1.len(), 10);

    // Page 2 (limit 10, offset 10)
    let page2 = playlist_repo
        .get_by_room_paginated(&room.id, 10, 10)
        .await
        .checked("operation should succeed");
    assert_eq!(page2.len(), 6);
}

/// Integration test: Create with executor preserves explicit sparse positions.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_with_executor_preserves_position() {
    use crate::repository::room::RoomRepository;
    use crate::repository::user::UserRepository;
    use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = UserFixture::new().with_username("executor_owner").build();
    let owner = user_repo
        .create(&owner)
        .await
        .checked("operation should succeed");

    let room = RoomFixture::new()
        .with_name("Executor Room")
        .with_owner(owner.id)
        .build();
    let room = room_repo
        .create(&room)
        .await
        .checked("operation should succeed");

    // Create root
    let root = PlaylistFixture::new().with_room_id(room.id).build();
    let created_root = playlist_repo
        .create(&root)
        .await
        .checked("operation should succeed");

    let child_explicit = PlaylistFixture::new_child(created_root.id)
        .with_room_id(room.id)
        .with_name("Explicit Child")
        .with_position(2048)
        .build();

    let result = playlist_repo
        .create_with_executor(&child_explicit, &pool)
        .await;
    let created = result.checked("create with executor should succeed");
    assert_position_eq(created.position, 2048.0);
}
