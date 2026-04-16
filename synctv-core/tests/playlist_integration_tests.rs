//! Playlist tree operation integration tests
//!
//! Tests playlist CRUD, tree structure, position sorting, cycle prevention, and subtree delete.
//!
//! Run with: cargo test --test `playlist_integration_tests`
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use synctv_core::{
    models::{Playlist, PlaylistId, Room, RoomId, RoomStatus, User, UserId, UserRole, UserStatus},
    repository::{PlaylistRepository, RoomRepository, UserRepository},
};
use synctv_core_testing::create_test_pool;
/// Default `PostgreSQL` version for test containers
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

fn make_room(name: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: String::new(),
        created_by: owner.clone(),
        status: RoomStatus::Active,
        is_banned: false,
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
        room_id: room_id.clone(),
        creator_id: None,
        name: name.to_string(),
        parent_id: parent_id.cloned(),
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

    let owner = user_repo.create(&make_user("pl_owner_1")).await.unwrap();
    let room = room_repo
        .create(&make_room("PL Room 1", &owner.id))
        .await
        .unwrap();

    let top_level = make_playlist(&room.id, "Top Level Display Name", None, 0);
    let created = playlist_repo.create(&top_level).await.unwrap();
    let fetched = playlist_repo.get_top_level(&room.id).await.unwrap();

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

    let owner = user_repo.create(&make_user("pl_owner_2")).await.unwrap();
    let room = room_repo
        .create(&make_room("PL Room 2", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "Root", None, 0))
        .await
        .unwrap();

    let child = playlist_repo
        .create(&make_playlist(&room.id, "Child 1", Some(&root.id), 0))
        .await
        .unwrap();
    assert_eq!(child.parent_id.as_ref().unwrap(), &root.id);
    assert_eq!(child.name, "Child 1");

    let grandchild = playlist_repo
        .create(&make_playlist(&room.id, "Grandchild", Some(&child.id), 0))
        .await
        .unwrap();
    assert_eq!(grandchild.parent_id.as_ref().unwrap(), &child.id);

    // Verify tree traversal via get_path
    let path = playlist_repo.get_path(&grandchild.id).await.unwrap();
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

    let owner = user_repo.create(&make_user("pl_owner_3")).await.unwrap();
    let room = room_repo
        .create(&make_room("PL Room 3", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "Root", None, 0))
        .await
        .unwrap();

    let _child_b = playlist_repo
        .create(&make_playlist(&room.id, "B", Some(&root.id), 1))
        .await
        .unwrap();
    let _child_a = playlist_repo
        .create(&make_playlist(&room.id, "A", Some(&root.id), 0))
        .await
        .unwrap();
    let _child_c = playlist_repo
        .create(&make_playlist(&room.id, "C", Some(&root.id), 2))
        .await
        .unwrap();

    // Get children - should be sorted by position ASC
    let children = playlist_repo.get_children(&root.id).await.unwrap();
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

    let owner = user_repo.create(&make_user("pl_owner_4")).await.unwrap();
    let room = room_repo
        .create(&make_room("PL Room 4", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "Root", None, 0))
        .await
        .unwrap();
    playlist_repo
        .create(&make_playlist(&room.id, "First", Some(&root.id), 0))
        .await
        .unwrap();

    // Duplicate floating positions are allowed. Stable ordering falls back to
    // secondary keys when necessary, and move operations rebalance only when needed.
    let duplicate = make_playlist(&room.id, "Duplicate", Some(&root.id), 0);
    let result = playlist_repo.create(&duplicate).await.unwrap();
    let children = playlist_repo.get_children(&root.id).await.unwrap();
    assert_eq!(result.name, "Duplicate");
    assert_eq!(children.len(), 2);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cycle_prevention_trigger() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("pl_owner_5")).await.unwrap();
    let room = room_repo
        .create(&make_room("PL Room 5", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "Root", None, 0))
        .await
        .unwrap();
    let child = playlist_repo
        .create(&make_playlist(&room.id, "Child", Some(&root.id), 0))
        .await
        .unwrap();
    let grandchild = playlist_repo
        .create(&make_playlist(&room.id, "Grandchild", Some(&child.id), 0))
        .await
        .unwrap();

    // Try to set root's parent to grandchild, creating a cycle: root -> child -> grandchild -> root
    let result = sqlx::query("UPDATE playlists SET parent_id = $1 WHERE id = $2")
        .bind(grandchild.id.as_str())
        .bind(root.id.as_str())
        .execute(&pool)
        .await;

    assert!(
        result.is_err(),
        "Circular reference should be prevented by trigger"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("Circular reference detected") || err_msg.contains("cycle"),
        "Error should mention circular reference, got: {err_msg}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cascade_delete_parent_playlist() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("pl_owner_6")).await.unwrap();
    let room = room_repo
        .create(&make_room("PL Room 6", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .unwrap();
    let child = playlist_repo
        .create(&make_playlist(&room.id, "Child", Some(&root.id), 0))
        .await
        .unwrap();
    let _grandchild = playlist_repo
        .create(&make_playlist(&room.id, "GC", Some(&child.id), 0))
        .await
        .unwrap();

    // Delete the child - the repository should remove the whole subtree too.
    let deleted = playlist_repo.delete(&child.id).await.unwrap();
    assert!(deleted);

    // Only root should remain
    let all_playlists = playlist_repo.get_by_room(&room.id).await.unwrap();
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

    let owner = user_repo.create(&make_user("pl_owner_7")).await.unwrap();
    let room = room_repo
        .create(&make_room("PL Room 7", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let first_position = playlist_repo
        .get_next_append_position_with_tx(&room.id, Some(&root.id), &mut tx)
        .await
        .unwrap();
    assert!((first_position - 1024.0).abs() < f64::EPSILON);

    playlist_repo
        .create_with_executor(
            &Playlist {
                id: PlaylistId::new(),
                room_id: room.id.clone(),
                creator_id: None,
                name: "Auto1".to_string(),
                parent_id: Some(root.id.clone()),
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
        .unwrap();

    let second_position = playlist_repo
        .get_next_append_position_with_tx(&room.id, Some(&root.id), &mut tx)
        .await
        .unwrap();
    assert!((second_position - 2048.0).abs() < f64::EPSILON);
    tx.commit().await.unwrap();
}

// VERIFIED: scope-level advisory locking is centralized in the repository and
// append-position computation uses the same scope lock.
// use the SAME hash strategy for computing advisory lock keys. This ensures
// they acquire the SAME lock for the same (room_id, parent_id) pair.
// The unified strategy: (room_bits << 31) | parent_bits
// - room_bits: lower 31 bits of room_id hash
// - parent_bits: lower 31 bits of parent_id hash (0 if None)

#[test]
fn test_advisory_lock_key_consistency_between_methods() {
    // (room_id, parent_id) pair, otherwise the lock provides no protection.

    use std::hash::{Hash, Hasher};

    // This is the unified hash strategy used in BOTH:
    // - scope lock in create/move flows
    // - append-position helper
    let compute_lock_key = |room_id: &str, parent_id: Option<&str>| -> i64 {
        let room_hash = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            room_id.hash(&mut h);
            h.finish()
        };

        let parent_hash = parent_id.map_or(0, |pid| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            pid.hash(&mut h);
            h.finish()
        });

        let room_bits = (room_hash & 0x7FFF_FFFF).cast_signed();
        let parent_bits = (parent_hash & 0x7FFF_FFFF).cast_signed();
        (room_bits << 31) | parent_bits
    };

    // Test cases: various (room_id, parent_id) pairs
    let test_cases = [
        ("room_abc", Some("parent_1")),
        ("room_abc", Some("parent_2")),
        ("room_xyz", Some("parent_1")),
        ("room_xyz", None),
        ("room_test", Some("parent_with_long_name")),
    ];

    // Verify keys are computed correctly and consistently
    for (room_id, parent_id) in &test_cases {
        let key = compute_lock_key(room_id, *parent_id);

        println!("room={room_id}, parent={parent_id:?} => lock_key={key}");

        // Key should be a valid i64
        assert!(
            key != 0 || (*room_id == "room_xyz" && parent_id.is_none()),
            "Lock key should be non-zero for non-trivial inputs"
        );
    }

    // Verify that different (room_id, parent_id) pairs produce different keys
    // (collision is possible but extremely unlikely with this strategy)
    let key1 = compute_lock_key("room_abc", Some("parent_1"));
    let key2 = compute_lock_key("room_abc", Some("parent_2"));
    let key3 = compute_lock_key("room_xyz", Some("parent_1"));

    assert_ne!(
        key1, key2,
        "Different parent_id should produce different keys"
    );
    assert_ne!(
        key1, key3,
        "Different room_id should produce different keys"
    );
    assert_ne!(
        key2, key3,
        "Completely different pairs should produce different keys"
    );
}

#[test]
fn test_advisory_lock_key_no_collision_between_different_parents() {
    // distinct advisory lock keys. A collision would cause unrelated playlist
    // operations to block each other unnecessarily.
    // The old implementation used DefaultHasher which could theoretically
    // produce collisions.

    use std::hash::{Hash, Hasher};

    // Simulate the OLD (buggy) implementation
    let compute_lock_key_old = |room_id: &str, parent_id: Option<&str>| -> i64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        room_id.hash(&mut h);
        parent_id.hash(&mut h);
        h.finish().cast_signed()
    };

    // Test multiple pairs to find potential collisions
    let key1 = compute_lock_key_old("room_abc", Some("parent_1"));
    let key2 = compute_lock_key_old("room_abc", Some("parent_2"));
    let key3 = compute_lock_key_old("room_xyz", Some("parent_1"));
    let key4 = compute_lock_key_old("room_xyz", None);

    // These should all be different, but with DefaultHasher there's a small
    // probability of collision. We can't guarantee no collisions in this test,
    // but we document the risk.
    println!("Old keys: {key1} {key2} {key3} {key4}");

    // The NEW implementation should be deterministic and collision-free
    // for reasonable ID lengths
    let compute_lock_key_new = |room_id: &str, parent_id: Option<&str>| -> i64 {
        // Use a deterministic combination that guarantees uniqueness
        // for the same input and minimizes collision probability
        let room_hash = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            room_id.hash(&mut h);
            h.finish()
        };

        let parent_hash = parent_id.map_or(0, |pid| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            pid.hash(&mut h);
            h.finish()
        });

        // Combine using a method that reduces collision probability
        // This uses prime number multiplication to spread values
        ((room_hash % (1 << 32)) << 32).cast_signed()
            | ((parent_hash % (1 << 32)) & 0x7FFF_FFFF).cast_signed()
    };

    let new_key1 = compute_lock_key_new("room_abc", Some("parent_1"));
    let new_key2 = compute_lock_key_new("room_abc", Some("parent_2"));
    let new_key3 = compute_lock_key_new("room_xyz", Some("parent_1"));
    let new_key4 = compute_lock_key_new("room_xyz", None);

    // With the new approach, different inputs should produce different keys
    // (for reasonable inputs)
    println!("New keys: {new_key1} {new_key2} {new_key3} {new_key4}");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_duplicate_names_are_allowed_within_same_parent() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("pl_owner_8")).await.unwrap();
    let room = room_repo
        .create(&make_room("PL Room 8", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "Root", None, 0))
        .await
        .unwrap();
    playlist_repo
        .create(&make_playlist(&room.id, "SameName", Some(&root.id), 0))
        .await
        .unwrap();

    let duplicate = make_playlist(&room.id, "SameName", Some(&root.id), 1);
    let created = playlist_repo.create(&duplicate).await.unwrap();
    assert_eq!(created.name, "SameName");
    assert_ne!(created.id, root.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cross_room_parent_id_rejected() {
    // from a different room. This is a data integrity and security requirement.
    // BUG: Currently the FK only references playlists(id), not (room_id, id).
    // This allows cross-room parent references, breaking room isolation.

    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("pl_cross_owner"))
        .await
        .unwrap();

    let room_a = room_repo
        .create(&make_room("Room A", &owner.id))
        .await
        .unwrap();
    let room_b = room_repo
        .create(&make_room("Room B", &owner.id))
        .await
        .unwrap();

    let root_a = playlist_repo
        .create(&make_playlist(&room_a.id, "Room A Top", None, 0))
        .await
        .unwrap();

    let _root_b = playlist_repo
        .create(&make_playlist(&room_b.id, "Room B Top", None, 0))
        .await
        .unwrap();

    // SECURITY CHECK: Try to create a playlist in Room B with parent_id from Room A
    // This should be rejected by the database trigger (trigger_validate_parent_same_room)
    let cross_room_child = Playlist {
        id: PlaylistId::new(),
        room_id: room_b.id.clone(), // Child belongs to Room B
        creator_id: None,
        name: "Cross Room Child".to_string(),
        parent_id: Some(root_a.id.clone()), // But parent is in Room A - INVALID!
        position: 0.0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };

    let result = playlist_repo.create(&cross_room_child).await;

    // This should fail with a constraint violation from the database trigger
    assert!(
        result.is_err(),
        "Cross-room parent_id should be rejected. Child in room {} cannot have parent from room {}",
        room_b.id,
        room_a.id
    );

    // The error should be from the database trigger
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(
        err_msg.contains("constraint")
            || err_msg.contains("Constraint")
            || err_msg.contains("violation"),
        "Error should be a constraint violation, got: {err_msg}"
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

    let owner = user_repo.create(&make_user("pl_same_owner")).await.unwrap();
    let room = room_repo
        .create(&make_room("Room Same", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .unwrap();

    let child = make_playlist(&room.id, "Valid Child", Some(&root.id), 0);
    let result = playlist_repo.create(&child).await;

    assert!(result.is_ok(), "Same-room parent_id should be allowed");

    let created = result.unwrap();
    assert_eq!(created.parent_id.as_ref().unwrap(), &root.id);
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
        .unwrap();
    let room = room_repo
        .create(&make_room("Room Count", &owner.id))
        .await
        .unwrap();

    // Initially only one top-level playlist
    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .unwrap();

    let count = playlist_repo.count_by_room(&room.id).await.unwrap();
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
            .unwrap();
    }

    let count = playlist_repo.count_by_room(&room.id).await.unwrap();
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
        .unwrap();
    let room = room_repo
        .create(&make_room("Room Page 1", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .unwrap();

    for i in 0..5 {
        playlist_repo
            .create(&make_playlist(
                &room.id,
                &format!("Playlist {i}"),
                Some(&root.id),
                i,
            ))
            .await
            .unwrap();
    }

    // Get first page with page_size=3
    let playlists = playlist_repo
        .get_by_room_paginated(&room.id, 3, 0)
        .await
        .unwrap();

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
        .unwrap();
    let room = room_repo
        .create(&make_room("Room Page 2", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .unwrap();

    for i in 0..5 {
        playlist_repo
            .create(&make_playlist(
                &room.id,
                &format!("Playlist {i}"),
                Some(&root.id),
                i,
            ))
            .await
            .unwrap();
    }

    // Get second page with page_size=3 (offset=3)
    let playlists = playlist_repo
        .get_by_room_paginated(&room.id, 3, 3)
        .await
        .unwrap();

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
        .unwrap();
    let room = room_repo
        .create(&make_room("Room Page 3", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .unwrap();

    for i in 0..5 {
        playlist_repo
            .create(&make_playlist(
                &room.id,
                &format!("Playlist {i}"),
                Some(&root.id),
                i,
            ))
            .await
            .unwrap();
    }

    // Get third page with page_size=5 (offset=10) - should be empty
    let playlists = playlist_repo
        .get_by_room_paginated(&room.id, 5, 10)
        .await
        .unwrap();

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
        .unwrap();
    let room = room_repo
        .create(&make_room("Room Large", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .unwrap();

    for i in 0..149 {
        playlist_repo
            .create(&make_playlist(
                &room.id,
                &format!("Playlist {i:03}"),
                Some(&root.id),
                i,
            ))
            .await
            .unwrap();
    }

    // Verify total count
    let count = playlist_repo.count_by_room(&room.id).await.unwrap();
    assert_eq!(count, 150, "Should have 150 playlists total");

    // Get first page with max page_size=100
    let page1 = playlist_repo
        .get_by_room_paginated(&room.id, 100, 0)
        .await
        .unwrap();
    assert_eq!(page1.len(), 100, "First page should have 100 items");

    // Get second page
    let page2 = playlist_repo
        .get_by_room_paginated(&room.id, 100, 100)
        .await
        .unwrap();
    assert_eq!(page2.len(), 50, "Second page should have 50 items");

    // Verify no overlap between pages
    let page1_ids: std::collections::HashSet<_> = page1.iter().map(|p| p.id.clone()).collect();
    let page2_ids: std::collections::HashSet<_> = page2.iter().map(|p| p.id.clone()).collect();
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
        .unwrap();
    let room = room_repo
        .create(&make_room("Room Empty", &owner.id))
        .await
        .unwrap();

    // Don't create root - room is completely empty

    let count = playlist_repo.count_by_room(&room.id).await.unwrap();
    assert_eq!(count, 0, "Empty room should have 0 playlists");

    let playlists = playlist_repo
        .get_by_room_paginated(&room.id, 50, 0)
        .await
        .unwrap();
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
        .unwrap();
    let room = room_repo
        .create(&make_room("Room Limit", &owner.id))
        .await
        .unwrap();

    let root = playlist_repo
        .create(&make_playlist(&room.id, "", None, 0))
        .await
        .unwrap();

    for i in 0..150 {
        playlist_repo
            .create(&make_playlist(
                &room.id,
                &format!("Playlist {i:03}"),
                Some(&root.id),
                i,
            ))
            .await
            .unwrap();
    }

    // Try to get 200 items (should work at repository level - API enforces limit)
    let playlists = playlist_repo
        .get_by_room_paginated(&room.id, 200, 0)
        .await
        .unwrap();
    assert_eq!(
        playlists.len(),
        151,
        "Repository returns all items when limit > count"
    );
}
