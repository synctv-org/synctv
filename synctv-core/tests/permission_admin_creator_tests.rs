//! `PermissionService` integration tests for admin-or-creator checks.

#![allow(clippy::unwrap_used)]

mod permission_test_support;

use synctv_core::repository::UserRepository;
use synctv_core_testing::create_test_pool;

use permission_test_support::{make_room_service, make_user};

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_is_admin_or_creator_for_creator() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("isadmin_creator"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Is Admin Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    let perm_service = room_service.permission_service();
    let result = perm_service
        .is_admin_or_creator(&room.id, &creator.id)
        .await
        .unwrap();

    assert!(result);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_is_admin_or_creator_for_regular_member() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("isadmin2_creator"))
        .await
        .unwrap();
    let member = user_repo
        .create(&make_user("isadmin2_member"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Is Admin 2 Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id, member.id, None)
        .await
        .unwrap();

    let perm_service = room_service.permission_service();
    let result = perm_service
        .is_admin_or_creator(&room.id, &member.id)
        .await
        .unwrap();

    assert!(!result);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_is_admin_or_creator_for_non_member() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("isadmin3_creator"))
        .await
        .unwrap();
    let outsider = user_repo
        .create(&make_user("isadmin3_outsider"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Is Admin 3 Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .unwrap();

    let perm_service = room_service.permission_service();
    let result = perm_service
        .is_admin_or_creator(&room.id, &outsider.id)
        .await
        .unwrap();

    assert!(!result);
}
