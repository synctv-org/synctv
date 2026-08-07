//! `PermissionService` integration tests for admin-or-creator checks.
mod permission_test_support;

use synctv_core::repository::UserRepository;
use synctv_core_testing::{create_test_pool, ok};

use permission_test_support::{make_room_service, make_user};

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_is_admin_or_creator_for_creator() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = ok(
        user_repo.create(&make_user("isadmin_creator")).await,
        "creator user should be created",
    );

    let (room, _) = ok(
        room_service
            .create_room(
                "Is Admin Room".to_string(),
                String::new(),
                creator.id,
                None,
                None,
            )
            .await,
        "room should be created for creator check",
    );

    let perm_service = room_service.permission_service();
    let result = ok(
        perm_service
            .is_admin_or_creator(&room.id, &creator.id)
            .await,
        "creator permission check should succeed",
    );

    assert!(result);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_is_admin_or_creator_for_regular_member() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = ok(
        user_repo.create(&make_user("isadmin2_creator")).await,
        "creator user should be created",
    );
    let member = ok(
        user_repo.create(&make_user("isadmin2_member")).await,
        "regular member user should be created",
    );

    let (room, _) = ok(
        room_service
            .create_room(
                "Is Admin 2 Room".to_string(),
                String::new(),
                creator.id,
                None,
                None,
            )
            .await,
        "room should be created for member check",
    );

    ok(
        room_service.join_room(room.id, member.id, None).await,
        "regular member should join room",
    );

    let perm_service = room_service.permission_service();
    let result = ok(
        perm_service.is_admin_or_creator(&room.id, &member.id).await,
        "member permission check should succeed",
    );

    assert!(!result);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_is_admin_or_creator_for_non_member() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = ok(
        user_repo.create(&make_user("isadmin3_creator")).await,
        "creator user should be created",
    );
    let outsider = ok(
        user_repo.create(&make_user("isadmin3_outsider")).await,
        "outsider user should be created",
    );

    let (room, _) = ok(
        room_service
            .create_room(
                "Is Admin 3 Room".to_string(),
                String::new(),
                creator.id,
                None,
                None,
            )
            .await,
        "room should be created for outsider check",
    );

    let perm_service = room_service.permission_service();
    let result = ok(
        perm_service
            .is_admin_or_creator(&room.id, &outsider.id)
            .await,
        "outsider permission check should succeed",
    );

    assert!(!result);
}
