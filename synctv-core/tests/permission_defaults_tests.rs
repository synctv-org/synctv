//! `PermissionService` integration tests for role defaults.
//!
//! Tests permission checking with real `PostgreSQL` via testcontainers,
//! verifying that the three-layer permission system works end-to-end.

#![allow(clippy::unwrap_used)]

mod permission_test_support;

use synctv_core::{
    models::{PermissionBits, RoomRole},
    repository::UserRepository,
};
use synctv_core_testing::create_test_pool;

use permission_test_support::{make_room_service, make_user};

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_creator_has_all_permissions() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("perm_creator")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Perm Creator Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let perm_service = room_service.permission_service();
    let perms = perm_service
        .get_user_permissions_no_cache(&room.id, &creator.id)
        .await
        .unwrap();

    assert_eq!(
        perms.0,
        PermissionBits::ALL,
        "Creator should have ALL permissions"
    );
    assert!(perms.has(PermissionBits::DELETE_ROOM));
    assert!(perms.has(PermissionBits::BAN_MEMBER));
    assert!(perms.has(PermissionBits::KICK_USER));
    assert!(perms.has(PermissionBits::SEND_CHAT));
    assert!(perms.has(PermissionBits::ADD_MOVIE));
    assert!(perms.has(PermissionBits::MANAGE_ADMIN));
    assert!(perms.has(PermissionBits::EXPORT_DATA));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_default_bits() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("admin_perm_creator"))
        .await
        .unwrap();
    let admin = user_repo
        .create(&make_user("admin_perm_user"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Admin Perm Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), admin.id.clone(), None)
        .await
        .unwrap();

    room_service
        .member_service()
        .set_member_role(
            room.id.clone(),
            creator.id.clone(),
            admin.id.clone(),
            RoomRole::Admin,
        )
        .await
        .unwrap();

    let perm_service = room_service.permission_service();
    let perms = perm_service
        .get_user_permissions_no_cache(&room.id, &admin.id)
        .await
        .unwrap();

    assert!(perms.has(PermissionBits::BAN_MEMBER));
    assert!(perms.has(PermissionBits::KICK_USER));
    assert!(perms.has(PermissionBits::SEND_CHAT));
    assert_ne!(
        perms.0,
        PermissionBits::ALL,
        "Admin should not have ALL permissions"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_member_default_bits() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("member_perm_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("member_perm_user"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Member Perm Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), member.id.clone(), None)
        .await
        .unwrap();

    let perm_service = room_service.permission_service();
    let perms = perm_service
        .get_user_permissions_no_cache(&room.id, &member.id)
        .await
        .unwrap();

    assert!(perms.has(PermissionBits::SEND_CHAT));
    assert!(perms.has(PermissionBits::ADD_MOVIE));
    assert!(perms.has(PermissionBits::VIEW_PLAYLIST));
    assert!(!perms.has(PermissionBits::BAN_MEMBER));
    assert!(!perms.has(PermissionBits::DELETE_ROOM));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_guest_default_no_permissions() {
    let (_container, pool) = create_test_pool().await;
    let room_service = make_room_service(pool);

    let perm_service = room_service.permission_service();
    let settings = synctv_core::models::RoomSettings::default();
    let perms = perm_service.calculate_role_default_permissions(&RoomRole::Guest, &settings);

    assert!(perms.has(PermissionBits::VIEW_PLAYLIST));
    assert!(!perms.has(PermissionBits::SEND_CHAT));
    assert!(!perms.has(PermissionBits::ADD_MOVIE));
    assert!(!perms.has(PermissionBits::BAN_MEMBER));
    assert!(!perms.has(PermissionBits::DELETE_ROOM));
}
