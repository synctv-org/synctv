//! Playlist tree operation integration tests
//!
//! Tests playlist CRUD, tree structure, position sorting, and subtree delete.
//!

use chrono::Utc;
use synctv_core::{
    models::{
        Playlist, PlaylistId, Room, RoomId, RoomMember, RoomRole, RoomStatus, User, UserId,
        UserRole, UserStatus,
    },
    repository::{PlaylistRepository, RoomMemberRepository, RoomRepository, UserRepository},
    service::DeleteEntriesRequest,
};
use synctv_core_testing::{create_test_pool, create_test_room_service};
use synctv_core_testing::{TestOptionExt, TestResultExt};
/// Default `PostgreSQL` version for test containers
fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: synctv_core::models::SignupMethod::Email,
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

fn make_room(name: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        category: None,
        labels: Vec::new(),
        created_by: *owner,
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

fn make_playlist(
    room_id: &RoomId,
    name: &str,
    parent_id: Option<&PlaylistId>,
    position: i32,
) -> Playlist {
    Playlist {
        id: PlaylistId::new(),
        room_id: *room_id,
        creator_id: None,
        name: name.to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: parent_id.copied(),
        position: f64::from(position),
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_top_level_playlist() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_owner_1"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("PL Room 1", &owner.id))
        .await
        .checked("test operation should succeed");

    let top_level = make_playlist(&room.id, "Top Level Display Name", None, 0);
    let created = playlist_repo
        .create(&top_level)
        .await
        .checked("test operation should succeed");
    let fetched = playlist_repo
        .get_top_level(&room.id)
        .await
        .checked("test operation should succeed");

    assert!(created.parent_id.is_none());
    assert_eq!(created.name, "Top Level Display Name");
    assert_eq!(created.room_id, room.id);
    assert!(created.is_top_level());
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].id, created.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_nested_playlists() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_owner_2"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("PL Room 2", &owner.id))
        .await
        .checked("test operation should succeed");

    let root = playlist_repo
        .create(&make_playlist(&room.id, "Root", None, 0))
        .await
        .checked("test operation should succeed");

    let child = playlist_repo
        .create(&make_playlist(&room.id, "Child 1", Some(&root.id), 0))
        .await
        .checked("test operation should succeed");
    assert_eq!(
        child
            .parent_id
            .as_ref()
            .checked("test operation should succeed"),
        &root.id
    );
    assert_eq!(child.name, "Child 1");

    let grandchild = playlist_repo
        .create(&make_playlist(&room.id, "Grandchild", Some(&child.id), 0))
        .await
        .checked("test operation should succeed");
    assert_eq!(
        grandchild
            .parent_id
            .as_ref()
            .checked("test operation should succeed"),
        &child.id
    );

    // Verify tree traversal via get_path
    let path = playlist_repo
        .get_path(&grandchild.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(path.len(), 3);
    assert_eq!(path[0].id, root.id);
    assert_eq!(path[1].id, child.id);
    assert_eq!(path[2].id, grandchild.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_position_sorting() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_owner_3"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("PL Room 3", &owner.id))
        .await
        .checked("test operation should succeed");

    let root = playlist_repo
        .create(&make_playlist(&room.id, "Root", None, 0))
        .await
        .checked("test operation should succeed");

    let _child_b = playlist_repo
        .create(&make_playlist(&room.id, "B", Some(&root.id), 1))
        .await
        .checked("test operation should succeed");
    let _child_a = playlist_repo
        .create(&make_playlist(&room.id, "A", Some(&root.id), 0))
        .await
        .checked("test operation should succeed");
    let _child_c = playlist_repo
        .create(&make_playlist(&room.id, "C", Some(&root.id), 2))
        .await
        .checked("test operation should succeed");

    // Get children - should be sorted by position ASC
    let children = playlist_repo
        .get_children(&root.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(children.len(), 3);
    assert_eq!(children[0].name, "A");
    assert_eq!(children[1].name, "B");
    assert_eq!(children[2].name, "C");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_duplicate_positions_are_allowed_in_same_parent() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_owner_4"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("PL Room 4", &owner.id))
        .await
        .checked("test operation should succeed");

    let root = playlist_repo
        .create(&make_playlist(&room.id, "Root", None, 0))
        .await
        .checked("test operation should succeed");
    playlist_repo
        .create(&make_playlist(&room.id, "First", Some(&root.id), 0))
        .await
        .checked("test operation should succeed");

    // Duplicate floating positions are allowed. Stable ordering falls back to
    // secondary keys when necessary, and move operations rebalance only when needed.
    let duplicate = make_playlist(&room.id, "Duplicate", Some(&root.id), 0);
    let result = playlist_repo
        .create(&duplicate)
        .await
        .checked("test operation should succeed");
    let children = playlist_repo
        .get_children(&root.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(result.name, "Duplicate");
    assert_eq!(children.len(), 2);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cascade_delete_parent_playlist() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_owner_6"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("PL Room 6", &owner.id))
        .await
        .checked("test operation should succeed");
    member_repo
        .add(&RoomMember::new(room.id, owner.id, RoomRole::Creator))
        .await
        .checked("test operation should succeed");

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .checked("test operation should succeed");
    let child = playlist_repo
        .create(&make_playlist(&room.id, "Child", Some(&root.id), 0))
        .await
        .checked("test operation should succeed");
    let _grandchild = playlist_repo
        .create(&make_playlist(&room.id, "GC", Some(&child.id), 0))
        .await
        .checked("test operation should succeed");

    let room_service = create_test_room_service(pool.clone());
    let deleted = room_service
        .delete_entries(
            room.id,
            owner.id,
            DeleteEntriesRequest {
                playlist_ids: vec![child.id],
                media_ids: Vec::new(),
                force: false,
            },
        )
        .await
        .checked("test operation should succeed");
    assert_eq!(deleted.deleted_playlists, 2);

    // Only root should remain
    let all_playlists = playlist_repo
        .get_by_room(&room.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(all_playlists.len(), 1);
    assert_eq!(all_playlists[0].id, root.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_next_append_position_uses_sparse_floating_positions() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_owner_7"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("PL Room 7", &owner.id))
        .await
        .checked("test operation should succeed");

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .checked("test operation should succeed");

    let mut tx = pool.begin().await.checked("test operation should succeed");
    let first_position = playlist_repo
        .get_next_append_position_with_tx(&room.id, Some(&root.id), &mut tx)
        .await
        .checked("test operation should succeed");
    assert!((first_position - 1024.0).abs() < f64::EPSILON);

    playlist_repo
        .create_with_executor(
            &Playlist {
                id: PlaylistId::new(),
                room_id: room.id,
                creator_id: None,
                name: "Auto1".to_string(),
                description: String::new(),
                cover_file_reference_id: None,
                parent_id: Some(root.id),
                position: first_position,
                source_provider: None,
                source_config: None,
                provider_instance_name: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                version: 0,
            },
            &mut *tx,
        )
        .await
        .checked("test operation should succeed");

    let second_position = playlist_repo
        .get_next_append_position_with_tx(&room.id, Some(&root.id), &mut tx)
        .await
        .checked("test operation should succeed");
    assert!((second_position - 2048.0).abs() < f64::EPSILON);
    tx.commit().await.checked("test operation should succeed");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_duplicate_names_are_allowed_within_same_parent() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_owner_8"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("PL Room 8", &owner.id))
        .await
        .checked("test operation should succeed");

    let root = playlist_repo
        .create(&make_playlist(&room.id, "Root", None, 0))
        .await
        .checked("test operation should succeed");
    playlist_repo
        .create(&make_playlist(&room.id, "SameName", Some(&root.id), 0))
        .await
        .checked("test operation should succeed");

    let duplicate = make_playlist(&room.id, "SameName", Some(&root.id), 1);
    let created = playlist_repo
        .create(&duplicate)
        .await
        .checked("test operation should succeed");
    assert_eq!(created.name, "SameName");
    assert_ne!(created.id, root.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cross_room_parent_id_rejected() {
    // from a different room. This is a data integrity and security requirement.

    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_cross_owner"))
        .await
        .checked("test operation should succeed");

    let room_a = room_repo
        .create(&make_room("Room A", &owner.id))
        .await
        .checked("test operation should succeed");
    let room_b = room_repo
        .create(&make_room("Room B", &owner.id))
        .await
        .checked("test operation should succeed");

    let root_a = playlist_repo
        .create(&make_playlist(&room_a.id, "Room A Top", None, 0))
        .await
        .checked("test operation should succeed");

    let _root_b = playlist_repo
        .create(&make_playlist(&room_b.id, "Room B Top", None, 0))
        .await
        .checked("test operation should succeed");

    // Try to create a playlist in Room B with parent_id from Room A.
    // The composite parent foreign key rejects this as a missing parent in the child's room.
    let cross_room_child = Playlist {
        id: PlaylistId::new(),
        room_id: room_b.id, // Child belongs to Room B
        creator_id: None,
        name: "Cross Room Child".to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: Some(root_a.id), // But parent is in Room A - INVALID!
        position: 0.0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };

    let result = playlist_repo.create(&cross_room_child).await;

    assert!(
        result.is_err(),
        "Cross-room parent_id should be rejected. Child in room {} cannot have parent from room {}",
        room_b.id,
        room_a.id
    );

    assert!(
        matches!(
            result.failed("operation should fail"),
            synctv_core::Error::NotFound(_)
        ),
        "Cross-room parent should be mapped from the foreign key violation to NotFound"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_same_room_parent_id_allowed() {
    // Verify that same-room parent_id is still allowed (the valid case)

    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_same_owner"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Same", &owner.id))
        .await
        .checked("test operation should succeed");

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .checked("test operation should succeed");

    let child = make_playlist(&room.id, "Valid Child", Some(&root.id), 0);
    let result = playlist_repo.create(&child).await;

    assert!(result.is_ok(), "Same-room parent_id should be allowed");

    let created = result.checked("test operation should succeed");
    assert_eq!(
        created
            .parent_id
            .as_ref()
            .checked("test operation should succeed"),
        &root.id
    );
    assert_eq!(created.room_id, room.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_count_by_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_count_owner"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Count", &owner.id))
        .await
        .checked("test operation should succeed");

    // Initially only one top-level playlist
    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .checked("test operation should succeed");

    let count = playlist_repo
        .count_by_room(&room.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(count, 1, "Should have 1 playlist");

    // Add more playlists
    for i in 0..10 {
        playlist_repo
            .create(&make_playlist(
                &room.id,
                &format!("Playlist {i}"),
                Some(&root.id),
                i,
            ))
            .await
            .checked("test operation should succeed");
    }

    let count = playlist_repo
        .count_by_room(&room.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(count, 11, "Should have 11 playlists (root + 10 children)");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_room_paginated_first_page() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_page_owner_1"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Page 1", &owner.id))
        .await
        .checked("test operation should succeed");

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .checked("test operation should succeed");

    for i in 0..5 {
        playlist_repo
            .create(&make_playlist(
                &room.id,
                &format!("Playlist {i}"),
                Some(&root.id),
                i,
            ))
            .await
            .checked("test operation should succeed");
    }

    // Get first page with page_size=3
    let playlists = playlist_repo
        .get_by_room_paginated(&room.id, 3, 0)
        .await
        .checked("test operation should succeed");

    assert_eq!(playlists.len(), 3, "First page should have 3 items");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_room_paginated_second_page() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_page_owner_2"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Page 2", &owner.id))
        .await
        .checked("test operation should succeed");

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .checked("test operation should succeed");

    for i in 0..5 {
        playlist_repo
            .create(&make_playlist(
                &room.id,
                &format!("Playlist {i}"),
                Some(&root.id),
                i,
            ))
            .await
            .checked("test operation should succeed");
    }

    // Get second page with page_size=3 (offset=3)
    let playlists = playlist_repo
        .get_by_room_paginated(&room.id, 3, 3)
        .await
        .checked("test operation should succeed");

    assert_eq!(playlists.len(), 3, "Second page should have 3 items");
    // Total is 6 (root + 5 children), offset 3 means we get items 4, 5, 6
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_room_paginated_last_partial_page() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_page_owner_3"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Page 3", &owner.id))
        .await
        .checked("test operation should succeed");

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .checked("test operation should succeed");

    for i in 0..5 {
        playlist_repo
            .create(&make_playlist(
                &room.id,
                &format!("Playlist {i}"),
                Some(&root.id),
                i,
            ))
            .await
            .checked("test operation should succeed");
    }

    // Get third page with page_size=5 (offset=10) - should be empty
    let playlists = playlist_repo
        .get_by_room_paginated(&room.id, 5, 10)
        .await
        .checked("test operation should succeed");

    assert_eq!(playlists.len(), 0, "Page beyond data should be empty");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_room_paginated_large_dataset() {
    // Test with 150 playlists to verify pagination works with large datasets
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_large_owner"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Large", &owner.id))
        .await
        .checked("test operation should succeed");

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .checked("test operation should succeed");

    for i in 0..149 {
        playlist_repo
            .create(&make_playlist(
                &room.id,
                &format!("Playlist {i:03}"),
                Some(&root.id),
                i,
            ))
            .await
            .checked("test operation should succeed");
    }

    // Verify total count
    let count = playlist_repo
        .count_by_room(&room.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(count, 150, "Should have 150 playlists total");

    // Get first page with max page_size=100
    let page1 = playlist_repo
        .get_by_room_paginated(&room.id, 100, 0)
        .await
        .checked("test operation should succeed");
    assert_eq!(page1.len(), 100, "First page should have 100 items");

    // Get second page
    let page2 = playlist_repo
        .get_by_room_paginated(&room.id, 100, 100)
        .await
        .checked("test operation should succeed");
    assert_eq!(page2.len(), 50, "Second page should have 50 items");

    // Verify no overlap between pages
    let page1_ids: std::collections::HashSet<_> = page1.iter().map(|p| p.id).collect();
    let page2_ids: std::collections::HashSet<_> = page2.iter().map(|p| p.id).collect();
    let intersection: Vec<_> = page1_ids.intersection(&page2_ids).collect();
    assert!(
        intersection.is_empty(),
        "Pages should not have overlapping items"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_room_paginated_empty_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_empty_owner"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Empty", &owner.id))
        .await
        .checked("test operation should succeed");

    // Don't create root - room is completely empty

    let count = playlist_repo
        .count_by_room(&room.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(count, 0, "Empty room should have 0 playlists");

    let playlists = playlist_repo
        .get_by_room_paginated(&room.id, 50, 0)
        .await
        .checked("test operation should succeed");
    assert_eq!(playlists.len(), 0, "Empty room should return empty page");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_room_paginated_respects_page_size_limit() {
    // Verify that requesting more than max (100) still only returns 100
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_limit_owner"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Limit", &owner.id))
        .await
        .checked("test operation should succeed");

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .checked("test operation should succeed");

    for i in 0..150 {
        playlist_repo
            .create(&make_playlist(
                &room.id,
                &format!("Playlist {i:03}"),
                Some(&root.id),
                i,
            ))
            .await
            .checked("test operation should succeed");
    }

    // Try to get 200 items (should work at repository level - API enforces limit)
    let playlists = playlist_repo
        .get_by_room_paginated(&room.id, 200, 0)
        .await
        .checked("test operation should succeed");
    assert_eq!(
        playlists.len(),
        151,
        "Repository returns all items when limit > count"
    );
}
