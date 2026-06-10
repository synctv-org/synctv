//! `PermissionService` integration tests for allow/deny overrides.

mod permission_test_support;

use synctv_core::{
    models::{
        room_settings::MemberRemovedPermissions, RoomMemberPermissionBits, RoomPermission,
        RoomSettings,
    },
    repository::UserRepository,
};
use synctv_core_testing::{create_test_pool, ok};

use permission_test_support::{make_room_service, make_user};

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_allow_override_role_default() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = ok(
        user_repo.create(&make_user("allow_creator")).await,
        "allow override creator should be created",
    );
    let member = ok(
        user_repo.create(&make_user("allow_member")).await,
        "allow override member should be created",
    );

    let settings = RoomSettings {
        member_removed_permissions: MemberRemovedPermissions(RoomMemberPermissionBits::USE_WEBRTC),
        ..RoomSettings::default()
    };
    let (room, _) = ok(
        room_service
            .create_room(
                "Allow Override Room".to_string(),
                String::new(),
                creator.id,
                None,
                Some(settings),
            )
            .await,
        "allow override room should be created",
    );

    ok(
        room_service.join_room(room.id, member.id, None).await,
        "allow override member should join room",
    );

    let perm_service = room_service.permission_service();
    let perms_before = ok(
        perm_service
            .get_user_permissions_no_cache(&room.id, &member.id)
            .await,
        "allow override permissions should load before grant",
    );
    assert!(!perms_before.has(RoomPermission::USE_WEBRTC));

    ok(
        room_service
            .member_service()
            .grant_permission(
                room.id,
                creator.id,
                member.id,
                synctv_core::models::RoomAdminPermissionBits::USE_WEBRTC,
            )
            .await,
        "allow override permission should be granted",
    );

    let perms_after = ok(
        perm_service
            .get_user_permissions_no_cache(&room.id, &member.id)
            .await,
        "allow override permissions should load after grant",
    );
    assert!(perms_after.has(RoomPermission::USE_WEBRTC));
    assert!(perms_after.has(RoomPermission::CHAT));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deny_override_role_default() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = ok(
        user_repo.create(&make_user("deny_creator")).await,
        "deny override creator should be created",
    );
    let member = ok(
        user_repo.create(&make_user("deny_member")).await,
        "deny override member should be created",
    );

    let (room, _) = ok(
        room_service
            .create_room(
                "Deny Override Room".to_string(),
                String::new(),
                creator.id,
                None,
                None,
            )
            .await,
        "deny override room should be created",
    );

    ok(
        room_service.join_room(room.id, member.id, None).await,
        "deny override member should join room",
    );

    let perm_service = room_service.permission_service();
    let perms_before = ok(
        perm_service
            .get_user_permissions_no_cache(&room.id, &member.id)
            .await,
        "deny override permissions should load before revoke",
    );
    assert!(perms_before.has(RoomPermission::CHAT));

    ok(
        room_service
            .member_service()
            .revoke_permission(
                room.id,
                creator.id,
                member.id,
                RoomMemberPermissionBits::CHAT,
            )
            .await,
        "deny override permission should be revoked",
    );

    let perms_after = ok(
        perm_service
            .get_user_permissions_no_cache(&room.id, &member.id)
            .await,
        "deny override permissions should load after revoke",
    );
    assert!(!perms_after.has(RoomPermission::CHAT));
    assert!(perms_after.has(RoomPermission::CREATE_MEDIA_RESOURCE));
}
