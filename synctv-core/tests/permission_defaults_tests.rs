//! `PermissionService` integration tests for role defaults.
//!
//! Tests permission checking with real `PostgreSQL` via testcontainers,
//! verifying that the three-layer permission system works end-to-end.

mod permission_test_support;

use synctv_core::{
    models::{RoomPermission, RoomPermissionSet},
    repository::UserRepository,
};
use synctv_core_testing::{create_test_pool, ok};

use permission_test_support::{make_room_service, make_user};

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_creator_has_all_permissions() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = ok(
        user_repo.create(&make_user("perm_creator")).await,
        "creator should be created",
    );

    let (room, _) = ok(
        room_service
            .create_room(
                "Perm Creator Room".to_string(),
                String::new(),
                creator.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );

    let perm_service = room_service.permission_service();
    let perms = ok(
        perm_service
            .get_user_permissions_no_cache(&room.id, &creator.id)
            .await,
        "creator permissions should load",
    );

    assert_eq!(
        perms.0,
        RoomPermissionSet::all().0,
        "Creator should have ALL permissions"
    );
    assert!(perms.has(RoomPermission::KICK_MEMBER));
    assert!(perms.has(RoomPermission::KICK_MEMBER));
    assert!(perms.has(RoomPermission::CHAT));
    assert!(perms.has(RoomPermission::CREATE_MEDIA_RESOURCE));
    assert!(perms.has(RoomPermission::USE_WEBRTC));
}
