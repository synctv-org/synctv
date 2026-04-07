//! `PermissionService` integration tests for allow/deny overrides.

#![allow(clippy::unwrap_used)]

mod permission_test_support;

use synctv_core::{models::PermissionBits, repository::UserRepository};
use synctv_core_testing::create_test_pool;

use permission_test_support::{make_room_service, make_user};

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_allow_override_role_default() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("allow_creator")).await.unwrap();
    let member = user_repo.create(&make_user("allow_member")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Allow Override Room".to_string(),
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
    let perms_before = perm_service
        .get_user_permissions_no_cache(&room.id, &member.id)
        .await
        .unwrap();
    assert!(!perms_before.has(PermissionBits::BAN_MEMBER));

    room_service
        .member_service()
        .grant_permission(
            room.id.clone(),
            creator.id.clone(),
            member.id.clone(),
            PermissionBits::BAN_MEMBER,
        )
        .await
        .unwrap();

    let perms_after = perm_service
        .get_user_permissions_no_cache(&room.id, &member.id)
        .await
        .unwrap();
    assert!(perms_after.has(PermissionBits::BAN_MEMBER));
    assert!(perms_after.has(PermissionBits::SEND_CHAT));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deny_override_role_default() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("deny_creator")).await.unwrap();
    let member = user_repo.create(&make_user("deny_member")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Deny Override Room".to_string(),
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
    let perms_before = perm_service
        .get_user_permissions_no_cache(&room.id, &member.id)
        .await
        .unwrap();
    assert!(perms_before.has(PermissionBits::SEND_CHAT));

    room_service
        .member_service()
        .revoke_permission(
            room.id.clone(),
            creator.id.clone(),
            member.id.clone(),
            PermissionBits::SEND_CHAT,
        )
        .await
        .unwrap();

    let perms_after = perm_service
        .get_user_permissions_no_cache(&room.id, &member.id)
        .await
        .unwrap();
    assert!(!perms_after.has(PermissionBits::SEND_CHAT));
    assert!(perms_after.has(PermissionBits::ADD_MEDIA));
}
