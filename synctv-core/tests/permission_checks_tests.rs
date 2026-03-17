//! `PermissionService` integration tests for batch checks and role checks.

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
async fn test_check_permissions_batch_all_present() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("batch_perm_creator"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Batch Perm Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let perm_service = room_service.permission_service();
    let result = perm_service
        .check_permissions(
            &room.id,
            &creator.id,
            &[
                PermissionBits::SEND_CHAT,
                PermissionBits::ADD_MOVIE,
                PermissionBits::DELETE_ROOM,
            ],
        )
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_permissions_batch_one_missing_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("batch_miss_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("batch_miss_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Batch Miss Room".to_string(),
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
    let result = perm_service
        .check_permissions(
            &room.id,
            &member.id,
            &[PermissionBits::SEND_CHAT, PermissionBits::DELETE_ROOM],
        )
        .await;

    assert!(result.is_err());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_role_creator_passes() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("checkrole_creator"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Check Role Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let perm_service = room_service.permission_service();
    let result = perm_service
        .check_role(&room.id, &creator.id, RoomRole::Creator)
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_role_member_not_creator_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("checkrole2_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("checkrole2_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Check Role 2 Room".to_string(),
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
    let result = perm_service
        .check_role(&room.id, &member.id, RoomRole::Creator)
        .await;

    assert!(result.is_err());
}
