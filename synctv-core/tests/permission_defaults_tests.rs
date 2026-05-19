//! `PermissionService` integration tests for role defaults.
//!
//! Tests permission checking with real `PostgreSQL` via testcontainers,
//! verifying that the three-layer permission system works end-to-end.

#![allow(clippy::unwrap_used)]

mod permission_test_support;

use synctv_core::{models::PermissionBits, repository::UserRepository};
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
            creator.id,
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
    assert!(perms.has(PermissionBits::KICK_MEMBER));
    assert!(perms.has(PermissionBits::KICK_MEMBER));
    assert!(perms.has(PermissionBits::SEND_CHAT));
    assert!(perms.has(PermissionBits::CREATE_MEDIA_RESOURCE));
    assert!(perms.has(PermissionBits::USE_WEBRTC));
}
